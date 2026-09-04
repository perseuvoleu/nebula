//! `.env` files: the git-ignored configuration a checkout needs to run and
//! that no clone, worktree or remote checkout gets on its own. Nebula
//! carries them along: from a primary checkout into each new worktree,
//! and from the laptop's checkout to a remote twin (`nebula add host:…`,
//! `nebula remote <host> sync`).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Is this repo-relative path an env file worth carrying: `.env` or
/// `.env.<anything>` anywhere in the tree, outside dependency dirs.
pub fn is_env_path(rel: &Path) -> bool {
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name != ".env" && !name.starts_with(".env.") {
        return false;
    }
    !rel.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("node_modules" | ".git" | "vendor" | "target" | "dist" | "build")
        )
    })
}

/// Pick the env files out of a `git ls-files -z --others --ignored
/// --exclude-standard` listing (NUL-separated, repo-relative).
pub fn env_paths_from_listing(listing: &[u8]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = listing
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
        .filter(|p| is_env_path(p))
        .collect();
    out.sort();
    out
}

/// The git-ignored env files of a local checkout, repo-relative.
pub fn list_local(repo: &Path) -> Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
        ])
        .output()
        .context("run git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(env_paths_from_listing(&out.stdout))
}

/// What a push to a host did: files sent, and files left alone because the
/// host already had them (only when not overwriting).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub sent: Vec<PathBuf>,
    pub kept: Vec<PathBuf>,
}

/// Copy `files` (repo-relative, from `local_repo`) into `remote_repo` on
/// `host`, one tar stream over one ssh hop. Without `overwrite`, files the
/// host already has are kept — its `.env` may hold that machine's own
/// values — and reported instead.
pub fn push(
    local_repo: &Path,
    host: &str,
    remote_repo: &Path,
    files: &[PathBuf],
    overwrite: bool,
) -> Result<Pushed> {
    if files.is_empty() {
        return Ok(Pushed::default());
    }
    let remote = remote_repo.to_string_lossy();
    let mut ssh_base: Vec<String> = super::remote::SSH_BATCH_OPTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    ssh_base.push("--".into());
    ssh_base.push(host.to_string());

    let (send, kept): (Vec<PathBuf>, Vec<PathBuf>) = if overwrite {
        (files.to_vec(), Vec::new())
    } else {
        // Ask the host which ones it already has.
        let probe = format!(
            "cd {} && for f in {}; do [ -e \"$f\" ] && printf '%s\\n' \"$f\"; done; true",
            super::remote::ssh_quote(&remote),
            super::remote::join_quoted(files.iter().map(|f| f.to_str().unwrap_or(""))),
        );
        let out = Command::new("ssh")
            .args(&ssh_base)
            .arg(&probe)
            .output()
            .context("run ssh")?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let existing: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(PathBuf::from)
            .collect();
        files.iter().cloned().partition(|f| !existing.contains(f))
    };
    if send.is_empty() {
        return Ok(Pushed { sent: send, kept });
    }
    let mut tar = Command::new("tar")
        .arg("-C")
        .arg(local_repo)
        .arg("-cf")
        .arg("-")
        .args(&send)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run tar")?;
    let stream = tar.stdout.take().expect("piped");
    let extract = format!(
        "mkdir -p {r} && tar -C {r} -xf -",
        r = super::remote::ssh_quote(&remote)
    );
    let ssh = Command::new("ssh")
        .args(&ssh_base)
        .arg(&extract)
        .stdin(stream)
        .output()
        .context("run ssh")?;
    let tar = tar.wait_with_output().context("tar")?;
    if !tar.status.success() {
        bail!("tar: {}", String::from_utf8_lossy(&tar.stderr).trim());
    }
    if !ssh.status.success() {
        bail!("{}", String::from_utf8_lossy(&ssh.stderr).trim());
    }
    Ok(Pushed { sent: send, kept })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_files_are_picked_out_of_the_ignored_listing() {
        let listing = b".env\0.env.local\0.envrc\0apps/web/.env.production\0node_modules/x/.env\0build/.env\0.env.example\0";
        let paths = env_paths_from_listing(listing);
        let got: Vec<&str> = paths.iter().map(|p| p.to_str().unwrap()).collect();
        // `.envrc` is direnv, not an env file; dependency dirs are skipped;
        // an ignored `.env.example` is still an env file (versioned ones
        // never show up here anyway).
        assert_eq!(
            got,
            vec![
                ".env",
                ".env.example",
                ".env.local",
                "apps/web/.env.production"
            ]
        );
    }
}
