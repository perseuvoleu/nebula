use anyhow::{ensure, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};

// Refuse symlinks in every existing component, not only at the leaf. Private
// directories prevent another OS user from swapping entries between operations.
pub(super) fn check_path(path: &Path) -> Result<()> {
    ensure!(
        !path.to_string_lossy().contains([',', ':', '\n', '\r']),
        "unsafe preview mount path"
    );
    ensure!(path.is_absolute(), "preview path must be absolute");
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        ensure!(
            !matches!(part, std::path::Component::ParentDir),
            "parent traversal in preview path"
        );
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(!meta.file_type().is_symlink(), "symlink in preview path"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn read_private(path: &Path, root_owned: bool) -> Result<Vec<u8>> {
    check_path(path)?;
    if root_owned {
        for parent in path.parent().context("missing preview parent")?.ancestors() {
            let metadata = fs::metadata(parent)?;
            ensure!(
                metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
                "privileged preview files require root-controlled parents"
            );
        }
    }
    let parent = open_directory(path.parent().context("missing preview parent")?)?;
    let mut file = open_at(
        &parent,
        path.file_name().context("missing preview filename")?,
        false,
    )?;
    let meta = file.metadata()?;
    let uid = if root_owned {
        0
    } else {
        unsafe { libc::geteuid() }
    };
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.uid() == uid && meta.mode() & 0o027 == 0,
        "preview file must be owner-controlled, private and not hard-linked"
    );
    ensure!(meta.len() <= 1024 * 1024, "preview file too large");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn directory(path: &Path) -> Result<()> {
    check_path(path)?;
    if !path.exists() {
        // Parent must already exist: do not silently create uncontrolled ancestors.
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "preview state directory must be owned by this user and mode 0700"
    );
    Ok(())
}

pub(super) fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    check_path(path)?;
    if path.exists() {
        read_private(path, false)?;
    }
    let parent = path.parent().context("missing parent")?;
    let temporary = parent.join(format!(
        ".preview-{}-{}.tmp",
        std::process::id(),
        ulid::Ulid::generate()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn lock(path: &Path) -> Result<File> {
    check_path(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "unsafe preview lock"
    );
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another preview operation is in progress"
    );
    Ok(file)
}

// Walk through directory descriptors. O_NOFOLLOW on each component prevents
// raced leaf AND parent symlink substitution; the opened file is then copied,
// never reopened by path. O_NONBLOCK prevents a raced FIFO from hanging us.
fn open_at(parent: &File, component: &std::ffi::OsStr, directory: bool) -> Result<File> {
    use std::os::{fd::FromRawFd, unix::ffi::OsStrExt};
    let name = std::ffi::CString::new(component.as_bytes())?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

pub(super) fn open_directory(path: &Path) -> Result<File> {
    ensure!(path.is_absolute(), "source directory must be absolute");
    let mut directory = File::open("/")?;
    for part in path.components() {
        match part {
            std::path::Component::RootDir => (),
            std::path::Component::Normal(name) => directory = open_at(&directory, name, true)?,
            _ => anyhow::bail!("unsafe source directory"),
        }
    }
    Ok(directory)
}

pub(super) fn open_source(root: &File, relative: &Path) -> Result<File> {
    let mut directory = root.try_clone()?;
    let parts: Vec<_> = relative.components().collect();
    for (index, part) in parts.iter().enumerate() {
        let std::path::Component::Normal(name) = part else {
            anyhow::bail!("unsafe source path");
        };
        let last = index + 1 == parts.len();
        let file = open_at(&directory, name, !last)
            .context("preview source contains a symlink or unreadable component")?;
        if last {
            let metadata = file.metadata()?;
            ensure!(
                metadata.is_file() && metadata.nlink() == 1,
                "preview source must be a regular, non-linked file"
            );
            return Ok(file);
        }
        directory = file;
    }
    anyhow::bail!("empty source path")
}
