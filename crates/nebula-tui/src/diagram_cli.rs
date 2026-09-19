//! `nebula diagram …`: the CLI over [`crate::diagrams`]. Everything is plain
//! file work on the calling checkout's `.scratch/diagrams/`, so it needs no
//! daemon — the daemon is only asked, best-effort, for the calling session's
//! name when a new diagram records its owner.

use crate::diagrams::{self, Meta, Template};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

pub enum DiagramOp {
    New {
        slug: String,
        title: Option<String>,
        tags: Vec<String>,
        decision: bool,
        force: bool,
    },
    Add {
        file: PathBuf,
        slug: Option<String>,
        force: bool,
    },
    List {
        query: Option<String>,
        json: bool,
    },
    Open {
        slug: Option<String>,
    },
    Rm {
        slug: String,
    },
    Path,
    Decisions {
        slug: String,
        json: bool,
    },
}

/// How long a request from `nebula diagram open` stays valid: long enough
/// for a TUI's poll to see it, short enough that one left behind with no
/// TUI running never pops a viewer open later.
pub const OPEN_REQUEST_TTL_MS: i64 = 10_000;

/// Where `nebula diagram open` leaves its request for a running TUI — a
/// file in the runtime dir, so opening needs no daemon protocol.
pub fn open_request_path() -> PathBuf {
    nebula_core::paths::runtime_dir().join("diagram-open.json")
}

/// A pending open request: consumed (deleted) by the first TUI to see it.
/// Returns the checkout and the slug to show (None = the newest).
pub fn take_open_request(path: &Path, now_ms: i64) -> Option<(PathBuf, Option<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path);
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let age = now_ms - value["ts"].as_i64()?;
    if !(0..=OPEN_REQUEST_TTL_MS).contains(&age) {
        return None;
    }
    let checkout = PathBuf::from(value["checkout"].as_str()?);
    let slug = value["slug"]
        .as_str()
        .filter(|s| diagrams::valid_slug(s))
        .map(str::to_owned);
    checkout.is_dir().then_some((checkout, slug))
}

fn check_slug(slug: &str) -> Result<()> {
    if !diagrams::valid_slug(slug) {
        bail!("invalid slug \"{slug}\" — lowercase letters, digits and hyphens only ([a-z0-9-]+, at most 64)");
    }
    Ok(())
}

fn existing(checkout: &Path, slug: &str) -> Result<PathBuf> {
    check_slug(slug)?;
    let path = diagrams::file(checkout, slug);
    if !path.is_file() {
        bail!("no diagram \"{slug}\" in {} — see `nebula diagram list`", diagrams::dir(checkout).display());
    }
    Ok(path)
}

fn age(mtime: std::time::SystemTime) -> String {
    let ms = mtime.elapsed().map_or(0, |d| d.as_millis() as i64);
    crate::hosts::ago_label(ms)
}

pub fn run(op: DiagramOp, owner: Meta) -> Result<()> {
    let cwd = std::env::current_dir().context("resolve current directory")?;
    run_in(&diagrams::checkout_root(&cwd), op, owner)
}

/// `owner` is the calling session (empty outside one); only `new` uses it.
pub fn run_in(checkout: &Path, op: DiagramOp, owner: Meta) -> Result<()> {
    let folder = diagrams::dir(checkout);
    match op {
        DiagramOp::Path => println!("{}", folder.display()),
        DiagramOp::New { slug, title, tags, decision, force } => {
            check_slug(&slug)?;
            let path = diagrams::file(checkout, &slug);
            if path.exists() && !force {
                bail!(
                    "{} already exists — pick another slug, or pass --force to overwrite it",
                    path.display()
                );
            }
            let kind = if decision { Template::Decision } else { Template::Plain };
            let meta = Meta { tags, ..owner };
            let title = title.unwrap_or_else(|| slug.clone());
            std::fs::create_dir_all(&folder)
                .with_context(|| format!("create {}", folder.display()))?;
            std::fs::write(&path, diagrams::render_template(kind, &slug, &title, &meta))
                .with_context(|| format!("write {}", path.display()))?;
            println!("{}", path.display());
        }
        DiagramOp::Add { file, slug, force } => {
            let html = std::fs::read_to_string(&file)
                .with_context(|| format!("read {}", file.display()))?;
            let warnings = diagrams::validate(&html)
                .map_err(|why| anyhow::anyhow!("{}: {why}", file.display()))?;
            let slug = slug.unwrap_or_else(|| {
                diagrams::slugify(&file.file_stem().unwrap_or_default().to_string_lossy())
            });
            check_slug(&slug)?;
            let path = diagrams::file(checkout, &slug);
            if path.exists() && !force {
                bail!(
                    "{} already exists — pass --slug <other>, or --force to overwrite it",
                    path.display()
                );
            }
            for warning in warnings {
                eprintln!("warning: {warning}");
            }
            std::fs::create_dir_all(&folder)
                .with_context(|| format!("create {}", folder.display()))?;
            std::fs::write(&path, html).with_context(|| format!("write {}", path.display()))?;
            println!("{}", path.display());
        }
        DiagramOp::List { query, json } => {
            let all = diagrams::scan(checkout);
            let hits = diagrams::search(query.as_deref().unwrap_or_default(), &all);
            if json {
                let rows: Vec<_> = hits
                    .iter()
                    .map(|&i| {
                        let d = &all[i];
                        let mtime_ms = d
                            .mtime
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |t| t.as_millis() as i64);
                        json!({
                            "slug": d.slug,
                            "title": d.title,
                            "tags": d.meta.tags,
                            "session": d.meta.session,
                            "agent": d.meta.agent,
                            "path": d.path.display().to_string(),
                            "mtime_ms": mtime_ms,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else if hits.is_empty() {
                match query {
                    Some(q) => println!("no diagram matches \"{q}\" in {}", folder.display()),
                    None => println!(
                        "no diagrams in {} — create one with `nebula diagram new <slug>`",
                        folder.display()
                    ),
                }
            } else {
                for &i in &hits {
                    let d = &all[i];
                    let tags = if d.meta.tags.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", d.meta.tags.join(", "))
                    };
                    println!("{:<24} {}  ({}){tags}", d.slug, d.title, age(d.mtime));
                }
            }
        }
        DiagramOp::Open { slug } => {
            if let Some(slug) = &slug {
                existing(checkout, slug)?;
            } else if diagrams::scan(checkout).is_empty() {
                bail!("no diagrams in {} — create one with `nebula diagram new <slug>`", folder.display());
            }
            let request = json!({
                "checkout": checkout.display().to_string(),
                "slug": slug,
                "ts": crate::app::now_ms(),
            });
            let path = open_request_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Write-then-rename: the TUI never reads half a request.
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, request.to_string())?;
            std::fs::rename(&tmp, &path)?;
            println!(
                "asked the running nebula TUI to show {} — with none open, press ⌘X in nebula",
                slug.as_deref().unwrap_or("the newest diagram")
            );
        }
        DiagramOp::Rm { slug } => {
            let path = existing(checkout, &slug)?;
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            // Its decisions log goes with it; usually there is none.
            let _ = std::fs::remove_file(diagrams::decisions_file(checkout, &slug));
            println!("removed {}", path.display());
        }
        DiagramOp::Decisions { slug, json } => {
            existing(checkout, &slug)?;
            let rows = diagrams::read_decisions(checkout, &slug);
            if json {
                println!("{}", serde_json::Value::Array(rows));
            } else if rows.is_empty() {
                println!("no decisions recorded for \"{slug}\"");
            } else {
                for row in rows {
                    let text = |key: &str| row[key].as_str().unwrap_or_default().to_owned();
                    let note = match row["note"].as_str() {
                        Some(note) => format!(" — note: {note}"),
                        None => String::new(),
                    };
                    println!(
                        "{}  \"{}\" = \"{}\" ({}){note}",
                        row["ts"].as_i64().unwrap_or_default(),
                        text("id"),
                        text("choice"),
                        text("label"),
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(slug: &str, force: bool) -> DiagramOp {
        DiagramOp::New {
            slug: slug.into(),
            title: Some("My title".into()),
            tags: vec!["t1".into()],
            decision: false,
            force,
        }
    }

    #[test]
    fn new_writes_the_template_with_owner_meta_and_refuses_to_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = Meta {
            tags: vec![],
            session: Some("worker".into()),
            agent: Some("01ID".into()),
        };
        run_in(tmp.path(), new("flow", false), owner).unwrap();
        let listed = diagrams::scan(tmp.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "My title");
        assert_eq!(listed[0].meta.tags, ["t1"]);
        assert_eq!(listed[0].meta.session.as_deref(), Some("worker"));
        assert_eq!(listed[0].meta.agent.as_deref(), Some("01ID"));

        let path = diagrams::file(tmp.path(), "flow");
        std::fs::write(&path, "edited by the agent").unwrap();
        let refused = run_in(tmp.path(), new("flow", false), Meta::default()).unwrap_err();
        assert!(refused.to_string().contains("--force"), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited by the agent");
        run_in(tmp.path(), new("flow", true), Meta::default()).unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "edited by the agent");

        assert!(run_in(tmp.path(), new("Bad Slug", false), Meta::default()).is_err());
        assert!(run_in(tmp.path(), new("../escape", false), Meta::default()).is_err());
    }

    #[test]
    fn add_validates_copies_and_derives_the_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("Auth Flow.html");
        std::fs::write(&source, "<html><title>Auth</title></html>").unwrap();
        let add = |file: &Path, slug: Option<&str>| DiagramOp::Add {
            file: file.to_path_buf(),
            slug: slug.map(str::to_owned),
            force: false,
        };
        run_in(tmp.path(), add(&source, None), Meta::default()).unwrap();
        assert!(diagrams::file(tmp.path(), "auth-flow").is_file());
        assert!(run_in(tmp.path(), add(&source, None), Meta::default()).is_err(), "no overwrite");
        run_in(tmp.path(), add(&source, Some("second")), Meta::default()).unwrap();

        let untitled = tmp.path().join("untitled.html");
        std::fs::write(&untitled, "<html><body>x</body></html>").unwrap();
        assert!(run_in(tmp.path(), add(&untitled, None), Meta::default()).is_err());
        assert_eq!(diagrams::scan(tmp.path()).len(), 2);
    }

    #[test]
    fn rm_takes_the_diagram_and_its_decisions_log() {
        let tmp = tempfile::tempdir().unwrap();
        run_in(tmp.path(), new("pick", false), Meta::default()).unwrap();
        std::fs::write(diagrams::decisions_file(tmp.path(), "pick"), "{}\n").unwrap();
        run_in(tmp.path(), DiagramOp::Rm { slug: "pick".into() }, Meta::default()).unwrap();
        assert!(!diagrams::file(tmp.path(), "pick").exists());
        assert!(!diagrams::decisions_file(tmp.path(), "pick").exists());
        assert!(run_in(tmp.path(), DiagramOp::Rm { slug: "pick".into() }, Meta::default()).is_err());
    }

    #[test]
    fn an_open_request_is_consumed_once_and_expires() {
        let tmp = tempfile::tempdir().unwrap();
        let request = tmp.path().join("diagram-open.json");
        let write = |slug: serde_json::Value, ts: i64| {
            let body = json!({"checkout": tmp.path().display().to_string(), "slug": slug, "ts": ts});
            std::fs::write(&request, body.to_string()).unwrap();
        };
        write(json!("flow"), 1_000);
        assert_eq!(
            take_open_request(&request, 1_500),
            Some((tmp.path().to_path_buf(), Some("flow".into())))
        );
        assert_eq!(take_open_request(&request, 1_500), None, "consumed");

        write(json!(null), 1_000);
        assert_eq!(take_open_request(&request, 2_000), Some((tmp.path().to_path_buf(), None)));

        write(json!("flow"), 1_000);
        assert_eq!(take_open_request(&request, 1_000 + OPEN_REQUEST_TTL_MS + 1), None, "stale");
        assert!(!request.exists(), "a stale request is still cleaned up");

        // A slug that could never be a diagram falls back to the newest.
        write(json!("../../etc/passwd"), 1_000);
        assert_eq!(take_open_request(&request, 1_000), Some((tmp.path().to_path_buf(), None)));

        std::fs::write(&request, "not json").unwrap();
        assert_eq!(take_open_request(&request, 1_000), None);
    }
}
