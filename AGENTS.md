# Nebula

Rust workspace: `nebula` (CLI + e2e tests), `nebula-tui`, `nebula-daemon`, `nebula-core`, vendored
`vt100`. `make install` builds and installs the release binary; the daemon side of a change needs
`nebula kill` (stops ALL sessions) before it takes effect — TUI-only changes just need a new `ng`.

## Ghostty pane engine

The attached pane is drawn by Ghostty's VT engine (`libghostty-vt`, wrapped in
`crates/nebula-tui/src/ghostty_pane.rs`). It is a default cargo feature, so
**building nebula needs zig 0.15.2** (`brew install zig@0.15`; the Makefile
puts the unlinked keg on PATH, CI installs it via `mlugg/setup-zig`). The
first build git-clones the ghostty sources, so it is slow and needs network.

Escape hatches, both of which fall back to vt100 + tui-term: `NEBULA_GHOSTTY=0`
at runtime, and `make build-novt` / `--no-default-features` at build time
(what `install.sh` uses when zig is missing).

vt100 is still fed every byte in parallel, because links, selection,
mouse modes and pane sizing read `vt100::Screen`. Porting those (plus
`daemon/pty/render.rs`) is what it takes to drop vt100 and tui-term entirely.

## Skills

Shared by every agent kind (claude, codex, cursor, pi). Open one only when its trigger matches:

- `.claude/skills/release/SKILL.md` — trigger: "release", "ship it", a new version, or questions
  about the release process.
- `.claude/skills/remote-projects/SKILL.md` — trigger: "remote", "pe server", findl / `@host`,
  `nebula remote`, sessions or checkouts on another machine, remote status stuck on fresh.
- `.claude/skills/sync-skills/SKILL.md` — trigger: "sync skills", updating skills on the server /
  findl, `nss`, or a remote agent missing a skill.
- `.claude/skills/recall/SKILL.md` — Mission Control sessions only (MC_* env vars present):
  saving a durable project fact, or code questions via the graph_* MCP tools.
- `.claude/skills/diagram/SKILL.md` — Mission Control sessions only: diagram requests, rendered
  as Mermaid in MC's viewer.
