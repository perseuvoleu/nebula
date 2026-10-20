//! User settings, read from `paths::config_path()` (JSON). Loaded fresh at
//! each use so edits apply without restarting the daemon. A missing file or
//! unknown fields fall back to defaults; a malformed file is logged and
//! ignored rather than failing the operation that read it.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Run `git init` after AddProject creates a missing directory.
    pub git_init_on_create: bool,
    /// Pre-spawn agent CLIs while the user is still naming the session so
    /// creation feels instant. Costs one idle CLI process per warm slot.
    pub prewarm_agents: bool,
    /// Clone a matching primary checkout's top-level `node_modules` into new
    /// worktrees with APFS copy-on-write. Failures are ignored and Nebula
    /// never runs a package-manager install.
    pub seed_node_modules: bool,
    /// Kill idle session PTYs in worktrees no client is looking at once
    /// they've gone unwatched this long: "1m" | "5m" | "15m" | "30m" | "1h"
    /// ("off" disables reaping entirely; any `<n>s`/`<n>m`/`<n>h` works).
    /// Bounds what prewarming and walked-away-from sessions cost. Pinned
    /// agents, running or feedback-waiting agents, and terminals with a
    /// command running are spared; a reaped session revives on the next
    /// attach (agents resume their conversation). Malformed values fall
    /// back to the 5m default.
    pub session_idle_timeout: String,
    /// Archive unpinned agents that have sat finished, fresh or
    /// disconnected this long: "24h" by default, "off" disables, and any
    /// `<n>s`/`<n>m`/`<n>h` works. Running and feedback-waiting agents and
    /// sessions a client is attached to are never archived. Malformed
    /// values fall back to the default.
    pub auto_archive_after: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            git_init_on_create: true,
            prewarm_agents: true,
            seed_node_modules: true,
            session_idle_timeout: DEFAULT_SESSION_IDLE_TIMEOUT.into(),
            auto_archive_after: DEFAULT_AUTO_ARCHIVE_AFTER.into(),
        }
    }
}

/// Fallback for `session_idle_timeout` when the value is malformed.
pub const DEFAULT_SESSION_IDLE_TIMEOUT: &str = "5m";

/// Fallback for `auto_archive_after` when the value is malformed.
pub const DEFAULT_AUTO_ARCHIVE_AFTER: &str = "24h";

impl Config {
    pub fn load() -> Self {
        let path = nebula_core::paths::config_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_else(|err| {
            tracing::warn!("ignoring malformed {}: {err}", path.display());
            Self::default()
        })
    }

    /// `session_idle_timeout` parsed to a duration; None = reaping disabled.
    pub fn session_idle_timeout(&self) -> Option<std::time::Duration> {
        parse_timeout(&self.session_idle_timeout)
            .unwrap_or_else(|| parse_timeout(DEFAULT_SESSION_IDLE_TIMEOUT).expect("default parses"))
    }

    /// `auto_archive_after` parsed to a duration; None = auto-archive off.
    pub fn auto_archive_after(&self) -> Option<std::time::Duration> {
        parse_timeout(&self.auto_archive_after)
            .unwrap_or_else(|| parse_timeout(DEFAULT_AUTO_ARCHIVE_AFTER).expect("default parses"))
    }
}

/// "off"/"0" → Some(None); "<n>s"/"<n>m"/"<n>h" → Some(Some(d));
/// malformed → None (caller falls back to the default).
#[allow(clippy::option_option)]
fn parse_timeout(s: &str) -> Option<Option<std::time::Duration>> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("off") || s == "0" {
        return Some(None);
    }
    let (digits, unit_secs) = match s.strip_suffix(['s', 'S']) {
        Some(d) => (d, 1),
        None => match s.strip_suffix(['m', 'M']) {
            Some(d) => (d, 60),
            None => (s.strip_suffix(['h', 'H'])?, 3_600),
        },
    };
    let n: u64 = digits.trim().parse().ok()?;
    if n == 0 {
        return Some(None);
    }
    Some(Some(std::time::Duration::from_secs(n * unit_secs)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_enable_git_init() {
        assert!(Config::default().git_init_on_create);
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert!(cfg.git_init_on_create);
        let cfg: Config = serde_json::from_str(r#"{"git_init_on_create": false}"#).unwrap();
        assert!(!cfg.git_init_on_create);
    }

    #[test]
    fn defaults_enable_prewarm_and_allow_opt_out() {
        assert!(Config::default().prewarm_agents);
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert!(cfg.prewarm_agents);
        let cfg: Config = serde_json::from_str(r#"{"prewarm_agents": false}"#).unwrap();
        assert!(!cfg.prewarm_agents);
    }

    #[test]
    fn session_idle_timeout_parses_and_falls_back() {
        use std::time::Duration;
        let timeout = |v: &str| {
            let cfg: Config =
                serde_json::from_str(&format!(r#"{{"session_idle_timeout": "{v}"}}"#)).unwrap();
            cfg.session_idle_timeout()
        };
        assert_eq!(timeout("1m"), Some(Duration::from_secs(60)));
        assert_eq!(timeout("5m"), Some(Duration::from_secs(300)));
        assert_eq!(timeout("15m"), Some(Duration::from_secs(900)));
        assert_eq!(timeout("30m"), Some(Duration::from_secs(1800)));
        assert_eq!(timeout("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(
            timeout("2s"),
            Some(Duration::from_secs(2)),
            "seconds for tests"
        );
        assert_eq!(timeout("off"), None);
        assert_eq!(timeout("0"), None);
        // Malformed values fall back to the default, not to disabled.
        assert_eq!(timeout("soon"), Some(Duration::from_secs(300)));
        assert_eq!(
            Config::default().session_idle_timeout(),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn auto_archive_after_defaults_to_a_day_and_parses() {
        use std::time::Duration;
        let after = |v: &str| {
            let cfg: Config =
                serde_json::from_str(&format!(r#"{{"auto_archive_after": "{v}"}}"#)).unwrap();
            cfg.auto_archive_after()
        };
        let day = Some(Duration::from_secs(24 * 3600));
        assert_eq!(
            serde_json::from_str::<Config>("{}")
                .unwrap()
                .auto_archive_after(),
            day
        );
        assert_eq!(after("12h"), Some(Duration::from_secs(12 * 3600)));
        assert_eq!(after("off"), None);
        assert_eq!(after("soon"), day, "malformed falls back to the default");
    }

    #[test]
    fn retired_prewarm_sessions_key_still_loads() {
        let cfg: Config =
            serde_json::from_str(r#"{"prewarm_sessions": true, "prewarm_agents": false}"#).unwrap();
        assert!(!cfg.prewarm_agents);
    }

    #[test]
    fn defaults_enable_node_modules_seeding_and_allow_opt_out() {
        assert!(Config::default().seed_node_modules);
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert!(cfg.seed_node_modules);
        let cfg: Config = serde_json::from_str(r#"{"seed_node_modules": false}"#).unwrap();
        assert!(!cfg.seed_node_modules);
    }
}
