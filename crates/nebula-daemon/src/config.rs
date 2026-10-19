//! User settings, read from `paths::config_path()` (JSON). Loaded fresh at
//! each use so edits apply without restarting the daemon. A missing file or
//! unknown fields fall back to defaults; a malformed file is logged and
//! ignored rather than failing the operation that read it.

use crate::dispatcher::Limits;
use nebula_core::AgentKind;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Run `git init` after AddProject creates a missing directory.
    pub git_init_on_create: bool,
    /// Pre-spawn a worktree's dead shells when the user's selection rests
    /// on it, so attaching shows an already-booted screen instead of a
    /// booting shell. Costs idle shell processes for tabs the user may
    /// never open.
    pub prewarm_sessions: bool,
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
    /// attach or prewarm (agents resume their conversation). Malformed
    /// values fall back to the 5m default.
    pub session_idle_timeout: String,
    /// Memory admission for heavy commands and agent sessions.
    pub heavy: HeavyConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            git_init_on_create: true,
            prewarm_sessions: true,
            seed_node_modules: true,
            session_idle_timeout: DEFAULT_SESSION_IDLE_TIMEOUT.into(),
            heavy: HeavyConfig::default(),
        }
    }
}

/// A negative or non-finite value falls back to its default.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HeavyConfig {
    /// What running heavy commands and sessions may hold together.
    pub budget_gib: f64,
    /// Free memory admission never dips below; on macOS it applies to free swap.
    pub floor_gib: f64,
    /// Linux memory PSI `some avg10` (percent) above which nothing is admitted.
    pub psi_some_avg10_max: f64,
    /// Reservation for a heavy command that names no size.
    pub command_reservation_gib: f64,
    pub session_reservation_gib: SessionReservations,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SessionReservations {
    pub claude: f64,
    pub codex: f64,
    pub cursor: f64,
    pub pi: f64,
}

impl Default for HeavyConfig {
    fn default() -> Self {
        Self {
            budget_gib: if cfg!(target_os = "macos") { 6.0 } else { 7.0 },
            floor_gib: 2.0,
            psi_some_avg10_max: 10.0,
            command_reservation_gib: 4.0,
            session_reservation_gib: SessionReservations::default(),
        }
    }
}

impl Default for SessionReservations {
    fn default() -> Self {
        Self {
            claude: 1.0,
            codex: 1.0,
            cursor: 1.0,
            pi: 0.5,
        }
    }
}

impl HeavyConfig {
    pub fn limits(&self) -> Limits {
        let d = Self::default();
        Limits {
            budget_bytes: gib_bytes(valid_or(self.budget_gib, d.budget_gib)),
            floor_bytes: gib_bytes(valid_or(self.floor_gib, d.floor_gib)),
            psi_some_avg10_max: valid_or(self.psi_some_avg10_max, d.psi_some_avg10_max),
        }
    }

    pub fn command_reservation_bytes(&self) -> u64 {
        let d = Self::default();
        gib_bytes(valid_or(
            self.command_reservation_gib,
            d.command_reservation_gib,
        ))
    }

    pub fn session_reservation_bytes(&self, kind: AgentKind) -> u64 {
        let (r, d) = (
            &self.session_reservation_gib,
            SessionReservations::default(),
        );
        let (gib, default) = match kind {
            AgentKind::Claude => (r.claude, d.claude),
            AgentKind::Codex => (r.codex, d.codex),
            AgentKind::Cursor => (r.cursor, d.cursor),
            AgentKind::Pi => (r.pi, d.pi),
        };
        gib_bytes(valid_or(gib, default))
    }
}

fn valid_or(value: f64, default: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        default
    }
}

fn gib_bytes(gib: f64) -> u64 {
    (gib * (1u64 << 30) as f64) as u64
}

/// Fallback for `session_idle_timeout` when the value is malformed.
pub const DEFAULT_SESSION_IDLE_TIMEOUT: &str = "5m";

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
    fn a_config_naming_the_retired_agent_prewarm_still_loads() {
        let cfg: Config =
            serde_json::from_str(r#"{"prewarm_agents": true, "git_init_on_create": false}"#)
                .unwrap();
        assert!(!cfg.git_init_on_create);
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
    fn defaults_enable_session_prewarm_and_allow_opt_out() {
        assert!(Config::default().prewarm_sessions);
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert!(cfg.prewarm_sessions);
        let cfg: Config = serde_json::from_str(r#"{"prewarm_sessions": false}"#).unwrap();
        assert!(!cfg.prewarm_sessions);
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
