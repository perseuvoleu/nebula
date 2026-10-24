# Spawned sessions

A session that a lead session spawns (`nebula agent new` run inside the lead) records the lead as its `spawned_by` and is drawn under the lead in the sidebar, joined by a `└`/`├` guide. Sessions the user starts are drawn at the top level.

## Sub-features

- `spawn-record`: `agent list` shows the lead's id in the worker's `spawned_by`; a session started from outside any session has `spawned_by: null`.
- `spawn-nest`: the worktree row in the sidebar draws the worker one level under its lead.

## How to get to it (user POV)

- A lead agent runs `nebula agent new --worktree <branch> --name <name>` in its own shell.
- The user opens the TUI on the project and looks at the WORKTREES & BRANCHES column.

## Driving it with vn

Preconditions:

- The baseline from the README holds.

- **Start the lead.** Run `vn nebula agent new --project repo --worktree main --name Lead`. Stdout is one JSON object with `"name":"Lead"`.
- **Open the sidebar.** Run `vn tui`, `vn wait Lead 20` and `vn shot before-spawn`. The row under `main ⌂ primary` reads `● claude Lead`.
- **Lead spawns a worker.** Run `vn nebula agent send Lead --project repo 'nebula agent new --worktree main --name Worker'`. The lead's shell runs the scratch `nebula` with its own `NEBULA_AGENT_ID`.
- **See it nest.** Run `vn wait '└ ● claude Worker' 20`, `vn shot after-spawn` and `grep -F '└ ● claude Worker' "$NV_EVIDENCE/after-spawn.txt"`. The grep prints the line under `● claude Lead`; a worker drawn at the top level makes the wait fail.
- **Record.** Run `vn nebula agent list --project repo > "$NV_EVIDENCE/agents.json"`. Worker's `spawned_by` equals Lead's `id`; Lead's `spawned_by` is `null`.

## Gotchas

- Commit 492c963 first drew these sessions under an `AGENTS` header; 3a35893 replaced the header with nesting under the lead. The current proof is the `└` line, and no `AGENTS` text appears.
- `vn nebula agent new` run from your own shell is a user spawn (`spawned_by: null`), because `vn` strips the caller's `NEBULA_AGENT_ID`. Only a command sent into the lead proves this feature.
- `Worker` also appears in the SESSIONS column on the left; wait on and read the line under `Lead` in the middle column.
