# Agent CLI

`nebula agent` is how a lead (or the user) spawns a session on a worktree, types into it and reads its screen without attaching.

## Sub-features

- `agent-new`: `agent new` creates a session and prints its JSON row.
- `agent-send`: `agent send` types a line into the session and submits it.
- `agent-read`: `agent read` prints the session's screen as text, rendered by the daemon.

## How to get to it (user POV)

- Run `nebula agent new|send|read … --project <project>` in a terminal or inside a session.

## Driving it with vn

Preconditions:

- The baseline from the README holds.

- **Spawn.** Run `vn nebula agent new --project repo --worktree main --name Echo`. Stdout is one JSON object with `"name":"Echo"` and `"kind":"claude"`.
- **Send.** Run `vn nebula agent send Echo --project repo 'echo hello-from-echo'`. Stdout is `{"name":"Echo","sent":"echo hello-from-echo",…}`.
- **Read.** Run `vn nebula agent read Echo --project repo --lines 6 > "$NV_EVIDENCE/echo-screen.txt"`. The file holds the typed command and the line `hello-from-echo` below it.

## Gotchas

- The session runs `/bin/sh` (`NEBULA_AGENT_CMD`), so `kind` is only a label here and model behaviour is out of reach; prove the plumbing, not the agent.
- `--prompt` on `agent new` is submitted once the session counts as ready; with the shell stand-in, `agent send` is the reliable way to type.
- `agent read` right after `send` can miss output the shell has not printed yet; re-read until the expected line is there.
