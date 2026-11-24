//! Artifacts: the storing daemon's copy of a published tree, and the
//! board's artifact rows.

use crate::board::{
    cursor, in_scope, json_column, now_ms, optional_json, page_offset, paged, resolve,
};
use anyhow::{bail, Context, Result};
use nebula_core::board::{
    entry_inside, Artifact, ArtifactContent, BoardScope, CardRef, Page, Refusal,
};
use nebula_core::{paths, ArtifactId, TodoId};
use nix::dir::Dir;
use nix::errno::Errno;
use nix::fcntl::{openat, OFlag};
use nix::sys::stat::{fstat, Mode, SFlag};
use rusqlite::{params, OptionalExtension, Row, Transaction};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

fn max_bytes() -> u64 {
    std::env::var("NEBULA_ARTIFACT_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64 << 20)
}

fn store_dir() -> PathBuf {
    paths::data_dir().join("artifacts")
}

pub fn store(id: &ArtifactId, kind: &str, source: &Path) -> Result<ArtifactContent> {
    if ulid::Ulid::from_string(id.as_str()).is_err() {
        bail!("artifact id {id} is not a ULID");
    }
    if !source.is_absolute() {
        bail!("{} is not an absolute path", source.display());
    }
    let root = store_dir();
    let dest = root.join(id.as_str());
    std::fs::create_dir_all(&root)?;
    let tmp = root.join(format!(".tmp-{id}"));
    let _ = std::fs::remove_dir_all(&tmp);
    let entry = copy_tree(source, &tmp).and_then(|(files, is_dir)| {
        let entry = entry_of(kind, is_dir, &tmp, &files)?;
        if !dest.exists() {
            std::fs::rename(&tmp, &dest)?;
        } else if !same_tree(&tmp, &dest, &files)? {
            bail!(Refusal::Conflict.message(format!(
                "artifact {id} is already stored with other content"
            )));
        }
        Ok(entry)
    });
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(ArtifactContent::Files {
        host: nebula_core::host::hostname(),
        entry: entry?,
    })
}

pub fn locate(id: &ArtifactId, entry: &str) -> Result<PathBuf> {
    if ulid::Ulid::from_string(id.as_str()).is_err() {
        bail!("artifact id {id} is not a ULID");
    }
    if !entry_inside(entry) {
        bail!("entry {entry:?} is not a relative path inside the stored tree");
    }
    let tree = store_dir()
        .join(id.as_str())
        .canonicalize()
        .with_context(|| format!("artifact {id} is not stored on this host"))?;
    let path = tree
        .join(entry)
        .canonicalize()
        .with_context(|| format!("artifact {id} has no {entry}"))?;
    if !path.starts_with(&tree) || !path.is_file() {
        bail!("{entry} is not a file inside artifact {id}");
    }
    Ok(path)
}

pub fn discard(id: &ArtifactId) -> Result<()> {
    if ulid::Ulid::from_string(id.as_str()).is_err() {
        bail!("artifact id {id} is not a ULID");
    }
    match std::fs::remove_dir_all(store_dir().join(id.as_str())) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn same_tree(staged: &Path, stored: &Path, files: &BTreeSet<String>) -> Result<bool> {
    if stored_files(stored)? != *files {
        return Ok(false);
    }
    for file in files {
        if std::fs::read(staged.join(file))? != std::fs::read(stored.join(file))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn entry_of(kind: &str, is_dir: bool, dir: &Path, files: &BTreeSet<String>) -> Result<String> {
    if kind == "e2e-run" {
        let result = std::fs::read_to_string(dir.join("result.json"))
            .context("an e2e-run needs a result.json at its root")?;
        nebula_core::board::validate_e2e_run(&result, files).map_err(anyhow::Error::msg)?;
        return Ok("result.json".into());
    }
    match files.len() {
        0 => bail!("nothing to publish: the tree holds no files"),
        _ if is_dir && matches!(kind, "recap" | "diagram" | "prototype") => {
            if files.contains("index.html") {
                Ok("index.html".into())
            } else {
                bail!("a {kind} directory needs an index.html at its root")
            }
        }
        1 => Ok(files.first().cloned().unwrap_or_default()),
        n => bail!("a {kind} artifact is one file, not {n}"),
    }
}

struct Budget {
    used: u64,
    cap: u64,
}

#[derive(PartialEq)]
enum Node {
    Dir,
    File,
    Other,
}

fn node(fd: &OwnedFd) -> Result<Node> {
    let format = SFlag::from_bits_truncate(fstat(fd)?.st_mode) & SFlag::S_IFMT;
    Ok(match format {
        SFlag::S_IFDIR => Node::Dir,
        SFlag::S_IFREG => Node::File,
        _ => Node::Other,
    })
}

fn open_at(dir: impl AsFd, name: &OsStr, shown: &str) -> Result<OwnedFd> {
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
    openat(dir, name, flags, Mode::empty()).map_err(|e| match e {
        Errno::ELOOP => anyhow::anyhow!("{shown} is a symlink; only regular files are published"),
        e => anyhow::anyhow!("cannot open {shown}: {e}"),
    })
}

fn open_source(source: &Path) -> Result<OwnedFd> {
    let mut fd = OwnedFd::from(std::fs::File::open("/")?);
    let mut shown = PathBuf::from("/");
    for part in source.components() {
        let (Component::Normal(_) | Component::ParentDir) = part else {
            continue;
        };
        shown.push(part);
        fd = open_at(&fd, part.as_os_str(), &shown.display().to_string())?;
    }
    Ok(fd)
}

fn copy_tree(source: &Path, dest: &Path) -> Result<(BTreeSet<String>, bool)> {
    let fd = open_source(source)?;
    let kind = node(&fd)?;
    std::fs::create_dir(dest)?;
    let mut budget = Budget {
        used: 0,
        cap: max_bytes(),
    };
    let mut files = BTreeSet::new();
    match kind {
        Node::Dir => copy_dir(fd, dest, "", &mut budget, &mut files)?,
        Node::File => {
            let name = utf8_name(source.file_name().unwrap_or_default(), source)?;
            copy_file(fd, &dest.join(&name), &mut budget)?;
            files.insert(name);
        }
        Node::Other => bail!("{} is not a regular file or directory", source.display()),
    }
    Ok((files, kind == Node::Dir))
}

fn utf8_name(name: &OsStr, path: &Path) -> Result<String> {
    name.to_str()
        .map(str::to_owned)
        .with_context(|| format!("{} is not a UTF-8 name", path.display()))
}

fn copy_dir(
    fd: OwnedFd,
    dest: &Path,
    rel: &str,
    budget: &mut Budget,
    files: &mut BTreeSet<String>,
) -> Result<()> {
    let mut dir = Dir::from_fd(fd)?;
    let names = dir
        .iter()
        .map(|entry| entry.map(|e| OsStr::from_bytes(e.file_name().to_bytes()).to_owned()))
        .collect::<nix::Result<Vec<_>>>()?;
    for name in names.iter().filter(|n| *n != "." && *n != "..") {
        let text = utf8_name(name, Path::new(name))?;
        let rel = if rel.is_empty() {
            text.clone()
        } else {
            format!("{rel}/{text}")
        };
        let child = open_at(&dir, name, &rel)?;
        match node(&child)? {
            Node::Dir => {
                std::fs::create_dir(dest.join(&text))?;
                copy_dir(child, &dest.join(&text), &rel, budget, files)?;
            }
            Node::File => {
                copy_file(child, &dest.join(&text), budget)?;
                files.insert(rel);
            }
            Node::Other => bail!("{rel} is not a regular file"),
        }
    }
    Ok(())
}

fn copy_file(fd: OwnedFd, dest: &Path, budget: &mut Budget) -> Result<()> {
    let input = std::fs::File::from(fd);
    let mut output = std::fs::File::create_new(dest)?;
    let left = budget.cap - budget.used;
    let copied = std::io::copy(&mut input.take(left + 1), &mut output)?;
    if copied > left {
        bail!("the tree is over the {}-byte artifact cap", budget.cap);
    }
    budget.used += copied;
    Ok(())
}

fn stored_files(dir: &Path) -> Result<BTreeSet<String>> {
    fn walk(dir: &Path, rel: &str, files: &mut BTreeSet<String>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = utf8_name(&entry.file_name(), &entry.path())?;
            let rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), &rel, files)?;
            } else {
                files.insert(rel);
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    walk(dir, "", &mut files)?;
    Ok(files)
}

const ARTIFACT_COLUMNS: &str = "id, kind, title, storage_host, entry, url, body, card_id, project, session, checkout, meta, created_at, removed_at";

fn artifact_from_row(r: &Row) -> rusqlite::Result<Artifact> {
    let content = match (
        r.get::<_, Option<String>>(3)?,
        r.get::<_, Option<String>>(4)?,
        r.get::<_, Option<String>>(5)?,
        r.get::<_, Option<String>>(6)?,
    ) {
        (Some(host), Some(entry), _, _) => ArtifactContent::Files { host, entry },
        (_, _, Some(url), _) => ArtifactContent::Url { url },
        (_, _, _, body) => ArtifactContent::Body {
            body: body.unwrap_or_default(),
        },
    };
    Ok(Artifact {
        id: ArtifactId(r.get(0)?),
        kind: r.get(1)?,
        title: r.get(2)?,
        card: r.get::<_, Option<String>>(7)?.map(TodoId),
        project: r.get(8)?,
        session: optional_json(r, 9)?,
        checkout: optional_json(r, 10)?,
        content,
        meta: json_column(r, 11)?,
        created_at: r.get(12)?,
        removed_at: r.get(13)?,
    })
}

fn by_id(tx: &Transaction, id: &ArtifactId) -> Result<Option<Artifact>> {
    Ok(tx
        .query_row(
            &format!("SELECT {ARTIFACT_COLUMNS} FROM artifacts WHERE id = ?1"),
            params![id.as_str()],
            artifact_from_row,
        )
        .optional()?)
}

fn not_found(id: &ArtifactId) -> anyhow::Error {
    anyhow::anyhow!(Refusal::NotFound.message(format!("no artifact {id}")))
}

pub(crate) fn of_card(tx: &Transaction, card: &TodoId) -> Result<Vec<Artifact>> {
    Ok(tx
        .prepare(&format!(
            "SELECT {ARTIFACT_COLUMNS} FROM artifacts WHERE card_id = ?1 AND removed_at IS NULL ORDER BY created_at"
        ))?
        .query_map(params![card.as_str()], artifact_from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn publish(tx: &Transaction, artifact: Artifact) -> Result<Artifact> {
    if let Some(known) = by_id(tx, &artifact.id)? {
        return Ok(known);
    }
    if let Some(card) = &artifact.card {
        resolve(tx, &CardRef::Id(card.clone()))?;
    }
    if let ArtifactContent::Files { entry, .. } = &artifact.content {
        if !entry_inside(entry) {
            bail!("entry {entry:?} is not a relative path inside the stored tree");
        }
    }
    let (host, entry, url, body) = match &artifact.content {
        ArtifactContent::Files { host, entry } => (Some(host), Some(entry), None, None),
        ArtifactContent::Url { url } => (None, None, Some(url), None),
        ArtifactContent::Body { body } => (None, None, None, Some(body)),
    };
    tx.execute(
        "INSERT INTO artifacts (id, kind, title, storage_host, entry, url, body, card_id, project, session, checkout, meta, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            artifact.id.as_str(),
            artifact.kind,
            artifact.title,
            host,
            entry,
            url,
            body,
            artifact.card.as_ref().map(TodoId::as_str),
            artifact.project,
            artifact.session.as_ref().map(serde_json::to_string).transpose()?,
            artifact.checkout.as_ref().map(serde_json::to_string).transpose()?,
            serde_json::to_string(&artifact.meta)?,
            artifact.created_at,
        ],
    )?;
    by_id(tx, &artifact.id)?.ok_or_else(|| not_found(&artifact.id))
}

pub(crate) fn remove(tx: &Transaction, id: &ArtifactId) -> Result<Artifact> {
    tx.execute(
        "UPDATE artifacts SET removed_at = ?2 WHERE id = ?1 AND removed_at IS NULL",
        params![id.as_str(), now_ms()],
    )?;
    by_id(tx, id)?.ok_or_else(|| not_found(id))
}

pub(crate) fn show(tx: &Transaction, scope: &BoardScope, id: &ArtifactId) -> Result<Artifact> {
    by_id(tx, id)?
        .filter(|a| in_scope(scope, a.project.as_deref()))
        .ok_or_else(|| not_found(id))
}

pub(crate) fn list(
    tx: &Transaction,
    scope: &BoardScope,
    card: Option<&CardRef>,
    kind: Option<&str>,
    query: Option<&str>,
    page: Option<&str>,
) -> Result<Page<Artifact>> {
    let version = cursor(tx)?;
    let offset = page_offset(page, &version)?;
    let card = card.map(|c| resolve(tx, c)).transpose()?;
    let query = query.map(str::to_lowercase);
    let items = tx
        .prepare(&format!(
            "SELECT {ARTIFACT_COLUMNS} FROM artifacts WHERE removed_at IS NULL ORDER BY created_at, id"
        ))?
        .query_map([], artifact_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|a| in_scope(scope, a.project.as_deref()))
        .filter(|a| card.is_none() || a.card == card)
        .filter(|a| kind.is_none_or(|k| a.kind == k))
        .filter(|a| {
            query.as_deref().is_none_or(|q| {
                let body = match &a.content {
                    ArtifactContent::Body { body } => body.as_str(),
                    _ => "",
                };
                a.title.to_lowercase().contains(q) || body.to_lowercase().contains(q)
            })
        })
        .collect();
    let (items, next) = paged(items, offset, &version)?;
    Ok(Page { items, next })
}
