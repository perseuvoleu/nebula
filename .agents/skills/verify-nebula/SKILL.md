---
name: verify-nebula
description: "Drive a real, isolated nebula (own daemon, socket, data dir) and its TUI in tmux, and capture text screenshots as proof. Use to prove a daemon, CLI or sidebar change works the way a user sees it, before reporting it done."
---

# Verify nebula

`vn` (this folder) runs a nebula of its own: a daemon built from this checkout, on its own socket and data dir, driven through the real CLI and the real TUI in a tmux pty. The user's daemon and sessions stay untouched, so `nebula kill` and `make install` are never part of a verification run.

Run it from the checkout root as `.agents/skills/verify-nebula/vn <command>`, every build and test command through `heavy` (`~/Desktop/perseu-stack/bin/heavy`). A step with a `Done:` line is finished when that line holds.

## 1. Build

`heavy .agents/skills/verify-nebula/vn build` builds the debug binary into `target/verify-nebula/` (zig 0.15 keg on PATH, as the Makefile does). It is its own build tree, so it never waits on the lock of the shared `target/` build. Done: `Finished` and `target/verify-nebula/debug/nebula --version` names this checkout's HEAD.

## 2. Launch

`vn up` makes the state dir (`/tmp/nv-<checkout hash>`), a committed git repo at `<state>/repo`, starts the daemon from the scratch binary, and adds the repo as project `repo`. Ready means `nebula agent list` answers through the new socket within 2 s, not just that the socket exists; `up` gives up after 10 s. `up` on a live instance is a no-op unless the binary's sha256 changed since that `up`, which restarts it; on a dead one it starts over from an empty state dir. A launch that fails or is interrupted stops its daemon and removes the state dir. `up` and `down` refuse a state dir without the `.vn-state` marker `up` writes. Done: `vn: up at <state> (daemon <pid>, project repo)`.

Every process `vn` starts gets this environment, which is what keeps it off the user's nebula:

| Variable | Value | Why (source) |
|---|---|---|
| `NEBULA_RUNTIME_DIR` | `<state>/rt` | socket, pidfile and inboxes (`crates/nebula-core/src/paths.rs:7`) |
| `NEBULA_DATA_DIR` | `<state>/data` | `nebula.db`, `config.json` and the logs (`paths.rs:60`, `paths.rs:83`) |
| `NEBULA_AGENT_CMD` | `/bin/sh` | agent sessions run a shell, not a model, and the daemon skips the CLI trust writes into `~/.claude.json` / `~/.codex` (`crates/nebula-daemon/src/registry.rs:128`, `registry.rs:3465`) |
| `CODEX_HOME` | `<state>/codex` | a codex spawn writes its hooks there even with the override (`crates/nebula-daemon/src/hooks/installer.rs:338`) |
| `PI_CODING_AGENT_DIR` | `<state>/pi` | a pi spawn writes its extension there (`installer.rs:573`) |
| `SHELL` | `/bin/sh` | terminal tabs and the login-shell probe (`registry.rs:4023`) |
| `PATH` | `target/verify-nebula/debug` first | a session's shell running `nebula …` reaches the scratch binary and so the scratch daemon |
| `NEBULA_AGENT_ID`, `NEBULA_API_URL`, `NEBULA_API_TOKEN` | unset | inherited from the session running `vn`, they would make the scratch CLI act as that session (`crates/nebula-tui/src/ipc.rs:173`, `ipc.rs:1878`) |
| `TMUX`, `NO_COLOR`, `FORCE_COLOR` | unset; `TERM=xterm-256color` | a clean terminal for the TUI |

`NV_STATE` overrides the state dir, `NV_EVIDENCE` the evidence dir, `NV_BIN` the binary. The state dir must be short enough for `<state>/rt/daemon.sock` to fit the ~104-byte Unix socket path limit, and must be a real directory: `vn` refuses a link, since a linked state dir could point at another instance whose pids `down` would kill. One checkout owns one state dir; two checkouts get two.

## 3. Doctor

`vn doctor` is read-only and answers "is this instance worth driving?": state dir, binary and its build id, daemon pid running the scratch binary, the daemon started from a binary with the current sha256 (a rebuild at the same commit keeps the build id), socket, daemon build id equal to the binary's, `agent list` answering, and whether the TUI runs. Run it first, and again whenever anything looks off. Done: every line `ok` (exit 0); a `FAIL` line names what to fix.

## 4. Drive

- `vn nebula <args>` runs the scratch CLI against the instance, with cwd `<state>/repo`. Pass `--project repo` to agent and worktree commands: outside a session there is no default project.
- `vn tui` starts the TUI in tmux (160×45) as `nebula open <state>/repo`, which is what `ng` runs; `ng` itself opens a Ghostty window, which nothing can drive. `vn tui <nebula args>` starts it with other arguments.
- `vn keys <tmux keys>` types into the TUI (`vn keys j`, `vn keys Enter`, `vn keys C-q`).
- `vn wait <text> [secs]` blocks until the text is on screen (default 10 s) and fails with the screen dump otherwise. Wait on text, never on a sleep.
- A session's PTY runs `/bin/sh`, so `vn nebula agent send <name> --project repo '<command>'` runs a shell command inside that session, with its own `NEBULA_AGENT_ID`. That is how a lead session spawns a worker the way a real agent does.

The maintained recipes live in [`features/README.md`](features/README.md): read the index, then the feature file the change touches. A proof covers every entry point its feature file lists.

## 5. Evidence

`vn shot <name>` writes the TUI screen as text to `<evidence>/<name>.txt` (default `.scratch/verify-nebula/`, kept out of git) and prints it. Set `NV_EVIDENCE=.scratch/verify-nebula/<ticket>` for a run so its files stay together. A proof holds:

- a shot before and after the user action, not only the final screen;
- the CLI's own record of the side effect (`vn nebula agent list --project repo`, `vn nebula worktree list --project repo`) saved beside the shots;
- the real user path: the CLI, keys in the TUI, or a command typed into a session. Writes into `<state>/data` or a hand-crafted IPC frame prove nothing about what a user sees.

## 6. Cleanup

`vn down` kills the tmux server on `<state>/tmux.sock`, then the daemon whose pid `up` recorded (only when that pid still runs the scratch binary with this state dir's `NEBULA_RUNTIME_DIR`, so another instance from the same binary is never touched) and every process under it, and removes the state dir. Evidence stays. Running it twice is safe. Run it after every failed attempt too, so a broken run strands no daemon. Done: `vn: down; evidence kept in <evidence>`.

## Helper check

`heavy .agents/skills/verify-nebula/test-vn.sh` drives `vn` through up, doctor, spawns, the TUI, a shot and down, and asserts the isolation and the cleanup. Run it after changing `vn`; it needs a built binary.
