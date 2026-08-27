# Nebula

Rust workspace: `nebula` (CLI + e2e tests), `nebula-tui`, `nebula-daemon`, `nebula-core`, vendored
`vt100`. `make install` builds and installs the release binary; the daemon side of a change needs
`nebula kill` (stops ALL sessions) before it takes effect — TUI-only changes just need a new `ng`.

Repo skills live in `.claude/skills/` (shared by every agent kind — claude, codex, cursor, pi).
Invoke one only when its trigger matches; don't preload them.
