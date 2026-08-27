# Nebula

Rust workspace: `nebula` (CLI + e2e tests), `nebula-tui`, `nebula-daemon`, `nebula-core`, vendored
`vt100`. `make install` builds and installs the release binary; the daemon side of a change needs
`nebula kill` (stops ALL sessions) before it takes effect — TUI-only changes just need a new `ng`.

## Skills

Shared by every agent kind (claude, codex, cursor, pi). Open one only when its trigger matches:

- `.claude/skills/release/SKILL.md` — trigger: "release", "ship it", a new version, or questions
  about the release process.
- `.claude/skills/recall/SKILL.md` — Mission Control sessions only (MC_* env vars present):
  saving a durable project fact, or code questions via the graph_* MCP tools.
- `.claude/skills/diagram/SKILL.md` — Mission Control sessions only: diagram requests, rendered
  as Mermaid in MC's viewer.
