# Nebula verification map

The maintained recipes for proving nebula's user-facing behaviour with `vn`. Read this index, then the feature file the change touches.

## Baseline preconditions

- `heavy .agents/skills/verify-nebula/vn build` finished on the code under test.
- `vn up` printed `up at`, and `vn doctor` exits 0.
- `NV_EVIDENCE=.scratch/verify-nebula/<ticket>` is exported for the run.
- The instance holds project `repo` with branch `main` and no sessions; start from `vn down && vn up` when an earlier run left state behind.

## Driving conventions

- CLI steps run as `vn nebula … --project repo`; `vn` already isolates their environment.
- TUI steps run through `vn tui`, `vn keys` and `vn wait`. Wait on on-screen text before each next key.
- The sidebar is 21 columns wide and truncates long names with `…`: give sessions and worktrees short names, and wait on a prefix that fits.
- Session PTYs run `/bin/sh`, so a session "does" something by having a shell command sent into it (`vn nebula agent send <name> --project repo '<command>'`).

## Proof

- One `vn shot` before the action and one after it.
- The CLI's JSON for the side effect, saved beside the shots (`… > "$NV_EVIDENCE/<name>.json"`).
- An entry point the run could not reach is reported with the command tried and the unmet precondition, never as verified through another path.

## Features

- [Spawned sessions](spawned-sessions.md): a session spawned by another nests under its lead in the sidebar.
- [Worktrees](worktrees.md): creating a worktree from the CLI or the TUI and seeing its row.
- [Agent CLI](agent-cli.md): spawning, steering and reading a session from the command line.
