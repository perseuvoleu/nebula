//! `nebula ssh HOST [PATH]`: open nebula on a remote machine.
//!
//! Execs `ssh -t HOST <cmd>` so the remote TUI renders in this terminal. The
//! remote command installs nebula via the published install script when it
//! isn't on the remote PATH, then launches it.
//!
//! Quoting: sshd hands the command string to the user's login shell, which
//! may be bash, zsh, or fish. The script below is a fixed constant with no
//! single quotes, backslashes, or newlines, wrapped once in '...'; user input
//! (install URL, start dir) is passed only as positional parameters, each
//! POSIX-single-quoted. csh/tcsh login shells are the one unsupported case.

use anyhow::{bail, Context, Result};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Runs under `sh -c` on the remote: $1 = install URL, $2 = start dir
/// (optional; defaults to the remote $HOME).
const REMOTE_SCRIPT: &str = concat!(
    // install.sh's default NEBULA_INSTALL_DIR — non-login shells won't have it.
    "export PATH=\"$HOME/.local/bin:$PATH\"; ",
    "if ! command -v nebula >/dev/null 2>&1; then ",
    "command -v curl >/dev/null 2>&1 || { ",
    "echo \"nebula ssh: curl is required on the remote to install nebula\" >&2; exit 127; }; ",
    "echo \"nebula not found on remote; installing...\" >&2; ",
    "curl -fsSL \"$1\" | sh || exit 1; ",
    "fi; ",
    // A start dir spelled `~/x` arrives quoted (the local shell may or may
    // not have expanded it); expand the tilde here, where $HOME is the
    // remote one.
    "d=\"${2:-$HOME}\"; case \"$d\" in \"~\"*) d=\"$HOME${d#\"~\"}\";; esac; ",
    "cd -- \"$d\" || exit 1; ",
    "exec nebula"
);

pub fn run_ssh(host: &str, path: Option<&str>) -> Result<()> {
    // `nebula ssh vela ~/app`: the local shell expands `~` to *this*
    // machine's home before we see it. A path under the local home almost
    // certainly meant the remote home — spell it `~/…` again so the far
    // side expands it against its own.
    let remapped = path.map(|p| remap_home(p, std::env::var("HOME").ok().as_deref()));
    let path = remapped.as_deref();
    // Remember the destination for the TUI's `h` picker. Before the exec on
    // purpose (there is no after); a host that fails to connect still lists,
    // and `d` can drop it.
    nebula_tui::hosts::record(host, path);
    let cmd = remote_command(&crate::upgrade::install_url(), path);
    // exec: ssh owns the tty from here and its exit status propagates
    // natively. Only returns on failure.
    let err = Command::new("ssh").args(["-t", "--", host, &cmd]).exec();
    if err.kind() == std::io::ErrorKind::NotFound {
        bail!("ssh not found on PATH — nebula ssh requires the OpenSSH client");
    }
    Err(err).context("failed to exec ssh")
}

/// `/Users/me/app` → `~/app` when `/Users/me` is the local home.
fn remap_home(path: &str, home: Option<&str>) -> String {
    match home.filter(|h| !h.is_empty()) {
        Some(h) if path == h => "~".to_string(),
        Some(h) if path.starts_with(&format!("{h}/")) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

fn remote_command(install_url: &str, path: Option<&str>) -> String {
    let mut cmd = format!(
        "sh -c '{}' nebula-ssh {}",
        REMOTE_SCRIPT,
        shell_single_quote(install_url)
    );
    if let Some(path) = path {
        cmd.push(' ');
        cmd.push_str(&shell_single_quote(path));
    }
    cmd
}

/// POSIX-quote for a remote shell: `it's` -> `'it'\''s'`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/install.sh";

    #[test]
    fn script_survives_single_quoting() {
        // The whole scheme rests on the script needing no escaping inside
        // '...' under any login shell.
        assert!(!REMOTE_SCRIPT.contains('\''));
        assert!(!REMOTE_SCRIPT.contains('\\'));
        assert!(!REMOTE_SCRIPT.contains('\n'));
    }

    #[test]
    fn local_home_paths_become_remote_tilde() {
        assert_eq!(remap_home("/Users/me/app", Some("/Users/me")), "~/app");
        assert_eq!(remap_home("/Users/me", Some("/Users/me")), "~");
        assert_eq!(remap_home("/srv/app", Some("/Users/me")), "/srv/app");
        assert_eq!(
            remap_home("/Users/meow/x", Some("/Users/me")),
            "/Users/meow/x"
        );
        assert_eq!(remap_home("/Users/me/app", None), "/Users/me/app");
    }

    #[test]
    fn remote_script_expands_a_tilde_start_dir() {
        // The tilde survives quoting and is expanded against the remote
        // $HOME by the script itself.
        assert!(REMOTE_SCRIPT.contains("case \"$d\" in \"~\"*)"));
        let cmd = remote_command(URL, Some("~/app"));
        assert!(cmd.ends_with("'~/app'"));
    }

    #[test]
    fn no_path_defaults_to_remote_home() {
        let cmd = remote_command(URL, None);
        assert!(cmd.ends_with("nebula-ssh 'https://example.com/install.sh'"));
        assert!(cmd.contains("${2:-$HOME}"));
    }

    #[test]
    fn path_is_quoted() {
        let cmd = remote_command(URL, Some("/srv/my repo"));
        assert!(cmd.ends_with("'/srv/my repo'"));
    }

    #[test]
    fn path_with_single_quote_is_escaped() {
        let cmd = remote_command(URL, Some("/tmp/it's here"));
        assert!(cmd.ends_with("'/tmp/it'\\''s here'"));
    }

    #[test]
    fn single_quote_edge_cases() {
        assert_eq!(shell_single_quote(""), "''");
        assert_eq!(shell_single_quote("plain"), "'plain'");
        assert_eq!(shell_single_quote("'''"), "''\\'''\\'''\\'''");
    }
}
