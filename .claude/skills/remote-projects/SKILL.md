---
name: remote-projects
description: "Working on a remote host (findl or any ssh destination) through nebula: adding a remote checkout, starting sessions there from the local TUI or CLI, what the pink @host badge means, `nebula remote <host>` for status/sync/upgrade/clean, and the session lifecycle over the ssh tunnel. Trigger: \"remote\", \"pe server\", \"findl\", \"@findl\", `nebula remote`, a session or checkout that lives on another machine, or hooks/status not arriving from a remote session."
user-invocable: true
---

A *remote project* is a git checkout on another machine that the **local** daemon drives over ssh.
`nebula add findl:~/repo` (or `host:/path` in the `n` add-project prompt) registers it; the row shows as
`repo` with a pink `@findl` badge. Nothing about it is a second nebula: the local daemon spawns the
agent CLI on the host through `ssh -t`, reverse-forwards its hook receiver through that same
connection, and runs every git command for the panels over ssh. The host needs `nebula`, the agent
CLIs and their logins installed (`nebula remote <host> upgrade` keeps nebula current there).

## Where things live

| Thing | Where | Reached how |
|---|---|---|
| Session row, status, title, PTY | local daemon | the TUI, `nebula agent … --project repo@host` |
| The CLI process, its conversation, the checkout | the host | ssh from the local daemon |
| Status hooks, `nebula rename`, session id | posted on the host → tunnel → local daemon | `-R port:127.0.0.1:port` on the spawn's ssh |
| Skills / global agent config | the host's home | `nebula-sync-skills` / `nebula remote <host> sync` |

The host's own daemon (what `nebula ssh host` uses) never sees these sessions, and sessions started
there never show locally. Treat the two as separate worlds that happen to share a checkout.

## Twins: local ⇄ remote in one list

A local project and a remote one **with the same name** in the same workspace are twins, and the
Projects panel shows them as **one row** (the local one): the remote project is absorbed — its
remote-only checkouts join the worktree list with the badge, its sessions join the session lists —
and only resurfaces as its own row if the local project is removed. Every session picker offers
the other side (`Run on findl ▸` in `n`; flat `Claude on findl` … `Terminal
on findl` rows in ⌘T/⌘D), and a worktree's session list and tab bar include the twin's sessions
(primary pairs with primary, other worktrees pair by branch), each wearing the badge. So a session
started "on findl" from the local `nebula` row appears right there — do not go looking for it under
`nebula @findl`, though it is listed there too.

On the command line two projects share the name, so say which: `--project nebula@findl` is the
remote twin, `--project nebula` the local one.

```bash
nebula agent new --project nebula@findl --kind pi --worktree root --name fix login
nebula agent read "fix login" --project nebula@findl
nebula agent send "fix login" "run the tests" --project nebula@findl
```

## Lifecycle (verified)

- **Archive / ⌘W / delete here** ends the CLI process on the host at once.
- **Stopping the local daemon** (`nebula kill`) ends every remote session's process.
- **A dropped ssh** (laptop sleep, network) ends the process; the next attach respawns it with the
  CLI's own resume (`claude --resume <id>`, `codex resume`, `pi --session`), so the conversation
  continues. The id comes from the SessionStart hook through the tunnel.
- Processes that outlive their tunnel are orphans: `nebula remote <host> status` counts them,
  `nebula remote <host> clean` kills them. Only processes carrying a `NEBULA_AGENT_ID` this daemon no
  longer owns are touched; anything started by hand or by the host's daemon is left alone.

## `nebula remote <host>`

```
status    nebula + daemon on the host, this daemon's live sessions there, orphan count
sessions  every session there, archived and the host daemon's own included
watch     `sessions`, live, every 2s
sync      skills (nebula-sync-skills) + `git pull --ff-only` on every remote checkout
upgrade   `nebula upgrade` on the host (restarts only its daemon)
clean     kill orphaned agent processes (see above)
```

## What does not work remotely, on purpose

The editor, file finder, tree browser and `gh` are local tools; on a remote checkout they flash
"lives on <host>" instead of opening a path that isn't here. Use a `t` shell tab on the remote row
and edit there. Diff, branches, grep, worktree creation and PR links all work — they are git.

## When status is stuck on `fresh`

The hook tunnel is the usual suspect. Check, in order:

1. `nebula remote <host> status` — is nebula there ≥ 0.4.1 (has `nebula hooks install`)? Older
   hosts print `could not install … status hooks` at the top of the pane and never report.
2. The local daemon is on a build ≥ 0.4.1 too (`nebula --version` vs the running daemon; a
   stale daemon needs `nebula kill` — it stops ALL sessions, so ask first).
3. From the host: `curl -m 3 http://127.0.0.1:<port>/` where `<port>` is the local daemon's hook
   port (`hook receiver listening port=…` in the daemon log). A timeout means the reverse forward
   is dead: ssh multiplexing (`ControlMaster`) hijacking it was the v0.4.0 bug; spawns now force
   `ControlPath=none`.

## Security notes

The per-run hook token rides the ssh command line and is readable via `ps` by other accounts on
the host; it only allows status posts and `nebula rename` for your sessions. Never copy personal ssh
keys to the host — a per-repo deploy key (`gh repo deploy-key add --allow-write`) is the pattern
used for findl.
