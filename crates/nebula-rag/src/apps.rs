//! Apps: repos on the RAG host whose `origin/<branch>` markdown is indexed
//! into `apps/<name>/docs`. The git remote maps a checkout (anywhere, the
//! laptop included) to its app, which sets the default search scope.

use crate::db::now;
use crate::{ws, Rag};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Serialize, Clone)]
pub struct App {
    pub name: String,
    pub repo: String,
    pub remote: Option<String>,
    pub branch: String,
    pub last_commit: Option<String>,
    pub synced_at: Option<i64>,
}

/// `host/owner/repo`, lowercased, from any git remote spelling
/// (`git@github.com:o/r.git`, `https://github.com/o/r`, `ssh://git@…`).
pub fn remote_key(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let u = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
    let u = u.split_once('@').map(|(_, r)| r).unwrap_or(u);
    // scp-like `host:owner/repo`; a `host:port/…` URL keeps working since
    // only the first `:` becomes a slash.
    u.replacen(':', "/", 1).to_lowercase()
}

pub fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("run git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The origin URL of the checkout at `dir`, if it is one.
pub fn origin_of(dir: &Path) -> Option<String> {
    git(dir, &["remote", "get-url", "origin"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl Rag {
    pub fn apps(&self) -> Result<Vec<App>> {
        let mut st = self.conn.prepare(
            "SELECT name, repo, remote, branch, last_commit, synced_at FROM apps ORDER BY name",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(App {
                    name: r.get(0)?,
                    repo: r.get(1)?,
                    remote: r.get(2)?,
                    branch: r.get(3)?,
                    last_commit: r.get(4)?,
                    synced_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn app(&self, name: &str) -> Result<App> {
        self.apps()?
            .into_iter()
            .find(|a| a.name == name)
            .ok_or_else(|| anyhow!("no app {name} — see `nebula rag app list`"))
    }

    /// The app a git remote belongs to: a registered remote first, then an
    /// app named like the repo.
    pub fn app_for_remote(&self, url: &str) -> Result<Option<String>> {
        let key = remote_key(url);
        let apps = self.apps()?;
        if let Some(a) = apps
            .iter()
            .find(|a| a.remote.as_deref().map(remote_key).as_deref() == Some(key.as_str()))
        {
            return Ok(Some(a.name.clone()));
        }
        let repo = key.rsplit('/').next().unwrap_or_default();
        Ok(apps.into_iter().find(|a| a.name == repo).map(|a| a.name))
    }

    /// Register `name` for the checkout at `repo` (on this host) and create
    /// `apps/<name>/{docs,notes}`. The name gets the workspace duplicate
    /// guard; re-adding an existing app updates its repo.
    pub fn app_add(
        &self,
        name: &str,
        repo: &Path,
        branch: Option<&str>,
        force: bool,
    ) -> Result<App> {
        let name = ws::normalize(name)?
            .filter(|n| !n.contains('/'))
            .ok_or_else(|| anyhow!("app name is one path segment ([a-z0-9._-])"))?;
        let repo = std::fs::canonicalize(repo)
            .with_context(|| format!("repo {} not found", repo.display()))?;
        git(&repo, &["rev-parse", "--git-dir"])
            .with_context(|| format!("{} is not a git checkout", repo.display()))?;
        let remote = origin_of(&repo);
        let branch = match branch {
            Some(b) => b.to_string(),
            None => git(
                &repo,
                &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            )
            .ok()
            .and_then(|s| s.trim().strip_prefix("origin/").map(str::to_string))
            .unwrap_or_else(|| "main".into()),
        };
        let exists: Option<String> = self
            .conn
            .query_row("SELECT name FROM apps WHERE name = ?1", [&name], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            self.ws_create(&format!("apps/{name}"), force)?;
        }
        self.ws_ensure(&format!("apps/{name}/docs"))?;
        self.ws_ensure(&format!("apps/{name}/notes"))?;
        self.conn.execute(
            "INSERT INTO apps(name, repo, remote, branch, created) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name) DO UPDATE SET repo = ?2, remote = ?3, branch = ?4",
            params![name, repo.to_string_lossy(), remote, branch, now()],
        )?;
        self.app(&name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_spellings_share_a_key() {
        let k = "github.com/perseuvoleu/nebula";
        assert_eq!(remote_key("git@github.com:perseuvoleu/nebula.git"), k);
        assert_eq!(remote_key("https://github.com/PerseuVoleu/nebula"), k);
        assert_eq!(
            remote_key("ssh://git@github.com/perseuvoleu/nebula.git/"),
            "github.com/perseuvoleu/nebula"
        );
    }
}
