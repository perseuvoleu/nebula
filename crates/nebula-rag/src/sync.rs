//! Docs sync: index each app's markdown as committed on `origin/<branch>`,
//! read with `git ls-tree` / `git cat-file` after a `git fetch` — never the
//! working tree, which on findl may hold other agents' uncommitted work.
//!
//! Incremental: files changed between the last fully indexed commit and the
//! new one are re-indexed (one transaction each); a file whose blob is
//! already indexed is skipped. `last_commit` only advances when a run
//! finishes, so a run stopped by the daily neuron cap resumes next time
//! without re-embedding what it already did.

use crate::apps::{git, host_projects, App};
use crate::db::now;
use crate::embed::is_cap;
use crate::ignore::Ignore;
use crate::store::{Kind, NewDoc};
use crate::Rag;
use anyhow::{bail, Context, Result};
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// Markdown files larger than this are skipped as generated.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
const EXTENSIONS: &[&str] = &["md", "markdown", "mdx"];
/// Always skipped, on top of the repo's .gitignore and .ragignore.
const DEFAULT_IGNORE: &str = "node_modules/\n/vendor/\nthird_party/\n";
const IGNORE_FILES: &[&str] = &[".gitignore", ".ragignore"];

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub commit: String,
    pub indexed: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub up_to_date: bool,
}

struct Entry {
    blob: String,
    size: u64,
}

pub fn run(rag: &mut Rag, app: Option<&str>, full: bool) -> Result<()> {
    let apps = match app {
        Some(a) => vec![rag.app(a)?],
        None => {
            match host_projects(&nebula_core::paths::db_path())
                .and_then(|projects| rag.register_checkouts(&projects))
            {
                Ok(added) => {
                    for name in added {
                        println!("{name}: registered from the nebula project list");
                    }
                }
                Err(e) => eprintln!("project registration skipped: {e:#}"),
            }
            rag.apps()?
        }
    };
    let purged = rag.purge()?;
    if purged > 0 {
        println!("purged {purged} note(s) deleted over 30 days ago");
    }
    if apps.is_empty() {
        println!("no apps (nebula rag app add <name> --repo <path>)");
    }
    let mut failed = 0;
    for a in &apps {
        match sync_app(rag, a, full) {
            Ok(r) if r.up_to_date => println!("{}: up to date ({})", a.name, short(&r.commit)),
            Ok(r) => println!(
                "{}: {} → {} indexed, {} removed, {} unchanged, {} skipped (big/binary)",
                a.name,
                short(&r.commit),
                r.indexed,
                r.removed,
                r.unchanged,
                r.skipped
            ),
            Err(e) if is_cap(&e) => {
                println!(
                    "{}: paused — {e}. The next sync resumes where this one stopped.",
                    a.name
                );
                break;
            }
            Err(e) => {
                eprintln!("{}: sync failed: {e:#}", a.name);
                failed += 1;
            }
        }
    }
    println!(
        "neurons used today (UTC): {:.1} of the {:.0} cap",
        rag.neurons_today()?,
        rag.neuron_cap
    );
    if failed > 0 {
        bail!("{failed} app(s) failed to sync");
    }
    Ok(())
}

fn short(c: &str) -> &str {
    &c[..c.len().min(10)]
}

pub fn sync_app(rag: &mut Rag, app: &App, full: bool) -> Result<Report> {
    let repo = Path::new(&app.repo);
    git(repo, &["fetch", "-q", "origin", &app.branch]).context("git fetch")?;
    let commit = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{}^{{commit}}", app.branch),
        ],
    )?
    .trim()
    .to_string();
    if !full && app.last_commit.as_deref() == Some(commit.as_str()) {
        return Ok(Report {
            commit,
            up_to_date: true,
            ..Default::default()
        });
    }

    let mut ignore = Ignore::new();
    ignore.add(DEFAULT_IGNORE);
    for f in IGNORE_FILES {
        if let Ok(text) = git(repo, &["show", &format!("{commit}:{f}")]) {
            ignore.add(&text);
        }
    }
    let eligible = list_markdown(repo, &commit, &ignore)?;
    let indexed = indexed_blobs(rag, &app.name)?;

    // Diff from the last complete sync when possible; otherwise (first run,
    // --full, rewritten history, ignore rules changed) reconcile everything.
    let changed = match (&app.last_commit, full) {
        (Some(last), false) => changed_paths(repo, last, &commit)
            .ok()
            .filter(|paths| !paths.iter().any(|p| IGNORE_FILES.contains(&p.as_str()))),
        _ => None,
    };
    let candidates: BTreeSet<String> = match changed {
        Some(paths) => paths.into_iter().collect(),
        None => eligible.keys().chain(indexed.keys()).cloned().collect(),
    };

    let docs_ws = rag.ws_ensure(&format!("apps/{}/docs", app.name))?;
    let mut report = Report {
        commit: commit.clone(),
        ..Default::default()
    };
    for path in candidates {
        match eligible.get(&path) {
            Some(e) if indexed.get(&path).map(|(_, b)| b) == Some(&e.blob) => report.unchanged += 1,
            Some(e) if e.size > MAX_FILE_BYTES => {
                report.skipped += 1;
                report.removed += remove(rag, &indexed, &path)?;
            }
            Some(e) => match read_text(repo, &e.blob)? {
                Some(body) => {
                    let label = format!("{}/{path}", app.name);
                    rag.index_doc(
                        &NewDoc {
                            ws_id: docs_ws,
                            kind: Kind::Doc,
                            app: Some(&app.name),
                            path: Some(&path),
                            title: &path,
                            body: &body,
                            blob: Some(&e.blob),
                            source: Some(&format!("git:{}", short(&commit))),
                        },
                        &label,
                    )?;
                    report.indexed += 1;
                }
                None => {
                    report.skipped += 1;
                    report.removed += remove(rag, &indexed, &path)?;
                }
            },
            None => report.removed += remove(rag, &indexed, &path)?,
        }
    }
    rag.conn.execute(
        "UPDATE apps SET last_commit = ?2, synced_at = ?3 WHERE name = ?1",
        params![app.name, commit, now()],
    )?;
    Ok(report)
}

fn remove(rag: &mut Rag, indexed: &BTreeMap<String, (i64, String)>, path: &str) -> Result<usize> {
    match indexed.get(path) {
        Some((id, _)) => {
            rag.delete_doc(*id)?;
            Ok(1)
        }
        None => Ok(0),
    }
}

/// Markdown blobs at `commit` that pass the ignore rules: path → blob.
fn list_markdown(repo: &Path, commit: &str, ignore: &Ignore) -> Result<BTreeMap<String, Entry>> {
    let out = git(repo, &["ls-tree", "-r", "-z", "--long", commit])?;
    let mut map = BTreeMap::new();
    for rec in out.split('\0').filter(|r| !r.is_empty()) {
        // `<mode> SP <type> SP <sha> SP+ <size> TAB <path>`
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let f: Vec<&str> = meta.split_whitespace().collect();
        if f.len() != 4 || f[1] != "blob" || f[0] == "120000" {
            continue;
        }
        let ext = path
            .rsplit_once('.')
            .map(|(_, e)| e.to_lowercase())
            .unwrap_or_default();
        if !EXTENSIONS.contains(&ext.as_str()) || ignore.is_ignored(path) {
            continue;
        }
        map.insert(
            path.to_string(),
            Entry {
                blob: f[2].to_string(),
                size: f[3].parse().unwrap_or(u64::MAX),
            },
        );
    }
    Ok(map)
}

/// Live repo docs of the app: path → (doc id, blob).
fn indexed_blobs(rag: &Rag, app: &str) -> Result<BTreeMap<String, (i64, String)>> {
    let mut st = rag.conn.prepare(
        "SELECT path, id, coalesce(blob, '') FROM documents WHERE kind = 'doc' AND app = ?1 AND deleted_at IS NULL",
    )?;
    let rows = st
        .query_map([app], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn changed_paths(repo: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    git(repo, &["cat-file", "-e", &format!("{from}^{{commit}}")])?;
    let out = git(
        repo,
        &["diff", "--name-only", "--no-renames", "-z", from, to],
    )?;
    Ok(out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

/// The blob as UTF-8 text, or `None` when it is binary or not UTF-8.
fn read_text(repo: &Path, blob: &str) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "blob", blob])
        .output()
        .context("run git cat-file")?;
    if !out.status.success() {
        bail!(
            "git cat-file {blob}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if out.stdout.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(out.stdout).ok())
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
