//! Checkouts that live on another machine, reached over ssh.
//!
//! A project added as `host:/path` (`nebula add findl:/srv/app`) is a
//! *remote project*: its rows carry the ssh destination, and its checkout
//! paths mean "on that host". Everything that would touch such a checkout —
//! git for the panels and the daemon's worktree ops, the agent CLI, a
//! shell tab — runs through `ssh host …` instead.
//!
//! **Path → host registry.** Dozens of helpers on both sides of the socket
//! only know a checkout by its path (`git -C root …`). Rather than thread a
//! host through every one of them, this module keeps a process-wide map
//! from checkout path to host, filled from the tree (daemon: the store;
//! TUI: the snapshot), and the shared `git_command` consults it. A path
//! with no entry is local — the default, and the whole pre-remote
//! behaviour. Lookup is longest-prefix, so files and subdirectories under a
//! registered checkout resolve to its host too.
//!
//! **Quoting.** sshd hands the remote command to the user's login shell
//! (bash, zsh, fish…), so every argument that crosses is POSIX
//! single-quoted; see `ssh_quote`. csh-family login shells are unsupported,
//! same as `nebula ssh`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

fn registry() -> &'static RwLock<HashMap<PathBuf, String>> {
    static REG: OnceLock<RwLock<HashMap<PathBuf, String>>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Record that `path` (a checkout root or worktree) lives on `host`.
pub fn register(path: &Path, host: &str) {
    registry()
        .write()
        .unwrap()
        .insert(path.to_path_buf(), host.to_string());
}

/// Replace the whole map — the tree-sync entry point.
pub fn replace_all(entries: impl IntoIterator<Item = (PathBuf, String)>) {
    let mut reg = registry().write().unwrap();
    reg.clear();
    reg.extend(entries);
}

/// The host owning `path`, by longest registered prefix. None = local.
pub fn host_for(path: &Path) -> Option<String> {
    let reg = registry().read().unwrap();
    path.ancestors().find_map(|p| reg.get(p)).cloned()
}

/// Whether `path` is a remote checkout (or lives under one).
pub fn is_remote(path: &Path) -> bool {
    host_for(path).is_some()
}

/// `host:/absolute/path` or `host:~/path` — the `nebula add` spelling for a
/// remote checkout. The host part may not contain a slash (so a local path
/// like `dir:with:colons/x` is left alone) and the path must be rooted at
/// `/` or `~` (so `C:`-style and `a:b` strings never match).
pub fn parse_spec(input: &str) -> Option<(String, String)> {
    let (host, path) = input.trim().split_once(':')?;
    if host.is_empty() || host.contains('/') || host.contains(char::is_whitespace) {
        return None;
    }
    if !(path.starts_with('/') || path.starts_with('~')) {
        return None;
    }
    Some((host.to_string(), path.to_string()))
}

/// POSIX single-quote for a remote shell: `it's` → `'it'\''s'`.
pub fn ssh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// One shell-ready command line from words, each quoted.
pub fn join_quoted<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    parts
        .into_iter()
        .map(ssh_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Options every non-interactive ssh hop uses: no password prompts (a
/// daemon has no tty to answer them on), a bounded connect wait.
pub const SSH_BATCH_OPTS: [&str; 4] = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"];

/// `program` + argv for `git -C root args…`, run wherever `root` lives:
/// locally as-is, or as one quoted command line under `ssh host`.
pub fn git_command(root: &Path, args: &[&str]) -> (String, Vec<String>) {
    let root_str = root.to_string_lossy();
    match host_for(root) {
        None => {
            let mut argv = vec!["-C".to_string(), root_str.into_owned()];
            argv.extend(args.iter().map(|a| a.to_string()));
            ("git".into(), argv)
        }
        Some(host) => {
            let mut words = vec!["git", "-C", &root_str];
            words.extend(args.iter().copied());
            let mut argv: Vec<String> = SSH_BATCH_OPTS.iter().map(|s| s.to_string()).collect();
            argv.extend(["--".to_string(), host, join_quoted(words)]);
            ("ssh".into(), argv)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec_accepts_host_colon_rooted_path() {
        assert_eq!(
            parse_spec("findl:/srv/app"),
            Some(("findl".into(), "/srv/app".into()))
        );
        assert_eq!(
            parse_spec(" me@box:~/code "),
            Some(("me@box".into(), "~/code".into()))
        );
        assert_eq!(parse_spec("/local/path"), None);
        assert_eq!(parse_spec("a:b"), None, "relative after colon");
        assert_eq!(parse_spec("dir/with:colon/x"), None, "slash in host");
        assert_eq!(parse_spec(":/x"), None);
        assert_eq!(parse_spec("~/plain"), None);
    }

    #[test]
    fn ssh_quote_escapes_single_quotes() {
        assert_eq!(ssh_quote("plain"), "'plain'");
        assert_eq!(ssh_quote("it's"), "'it'\\''s'");
        assert_eq!(join_quoted(["a b", "c"]), "'a b' 'c'");
    }

    #[test]
    fn git_command_is_local_without_a_host() {
        // A path nobody registered: plain git.
        let root = Path::new("/definitely/not/registered");
        let (prog, argv) = git_command(root, &["status", "-z"]);
        assert_eq!(prog, "git");
        assert_eq!(
            argv,
            vec!["-C", "/definitely/not/registered", "status", "-z"]
        );
    }

    #[test]
    fn git_command_hops_over_ssh_for_registered_prefixes() {
        register(Path::new("/remote/repo"), "findl");
        let (prog, argv) = git_command(Path::new("/remote/repo/sub dir"), &["log", "-1"]);
        assert_eq!(prog, "ssh");
        assert_eq!(
            argv,
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                "findl",
                "'git' '-C' '/remote/repo/sub dir' 'log' '-1'"
            ]
        );
        assert_eq!(
            host_for(Path::new("/remote")),
            None,
            "parent is not covered"
        );
        assert!(is_remote(Path::new("/remote/repo")));
    }
}
