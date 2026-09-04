---
name: remote-projects
description: "Working on a remote host (findl or any ssh destination) through nebula: adding a remote checkout, starting sessions there from the local TUI or CLI, what the pink @host badge means, how the relay keeps sessions alive on the host with the laptop closed, `nebula remote <host>` for status/sync/upgrade/restart. Trigger: \"remote\", \"pe server\", \"findl\", \"@findl\", `nebula remote`, a session or checkout that lives on another machine, or remote rows missing/stale."
user-invocable: true
---

A *remote project* is a git checkout on another machine. `nebula add findl:~/repo` (or `host:/path` in
the `n` add-project prompt) registers it as an **anchor**; everything else about it — worktrees,
sessions, PTYs, status hooks — belongs to the **host's own nebula daemon**, which the local daemon
mirrors through a relay. The row shows as `repo` with a pink `@findl` badge (absorbed into the local
project of the same name when one exists). The host needs `nebula`, the agent CLIs and their logins
installed; `nebula remote <host> upgrade` keeps nebula current there.

## The host is findl. Always.

Remote work goes to **findl** (ssh alias in `~/.ssh/config`, key on disk, no touch needed). Do not
install nebula on, or anchor projects to, any other server unless the user names it explicitly for
that purpose — other hosts in `~/.ssh/config` (vela, hetzner, …) are production or client machines.
A project that should also run remotely is **cloned on findl under the same directory name as the
local checkout** (so the rows merge into one), with its own read-write deploy key there
(`gh repo deploy-key add … --allow-write`; one key per repo, aliased in findl's `~/.ssh/config`),
then anchored with `nebula add findl:~/<name>`. Existing on findl: `nebula`, `vela-hub-fork`.

## The model (why it behaves the way it does)

| Thing | Where it lives | Reached how |
|---|---|---|
| Session row, status, title, PTY, conversation, checkout | the host daemon | mirrored here by the relay |
| Status hooks, `nebula rename`, session id | the host daemon's own loopback | native on the host, nothing tunnelled |
| The relay link | one `ssh host nebula proxy` per host, from the local daemon | reconnects on its own with backoff |
| Skills / global agent config | the host's home | `nebula-sync-skills` / `nebula remote <host> sync` |

So: **closing the laptop, sleeping, losing the network, or `nebula kill` locally never ends a remote
session** — the agent keeps working on the host. Coming back, the relay re-subscribes and re-attaches
every session a pane was on, which repaints from the live screen. The one thing that ends host sessions
is `nebula remote <host> restart` (a `nebula kill` there), and the host's own idle reaper.

Ids are the host daemon's, used verbatim; a request naming a mirrored id is forwarded to the host. The
local anchor row is hidden; the host's project row (stamped with the host) stands in for it, and the
anchor's path is rewritten to the host's spelling of the repo root the first time it connects.

## Twins: local ⇄ remote in one list

A local project and a remote one **with the same name** in the same workspace are twins: one row
in Projects, the remote-only checkouts join the worktree list with the badge, remote sessions join
the session lists and tabs. Every session picker offers the other side (`Run on findl ▸` in `n`;
flat `Claude on findl` … rows in ⌘T/⌘D). On the command line two projects share the name, so say
which: `--project nebula@findl` is the remote twin, `--project nebula` the local one.

```bash
nebula agent new --project nebula@findl --kind pi --worktree root --name fix login
nebula agent read "fix login" --project nebula@findl
nebula agent send "fix login" "run the tests" --project nebula@findl
```

## `nebula remote <host>`

```
status    nebula + daemon on the host, the sessions there (live; archived as a count)
sessions  every session there, archived included
watch     `sessions`, live, every 2s
sync      skills (nebula-sync-skills) + `git pull --ff-only` on every remote checkout
upgrade   `nebula upgrade` on the host; its daemon keeps the old build until `restart`
restart   `nebula kill` on the host — ends every session there
```

## What does not work remotely, on purpose

The editor, file finder, tree browser and `gh` are local tools; on a remote checkout they flash
"lives on <host>" instead of opening a path that isn't here. Use a `t` shell tab on the remote row
and edit there. Diff, branches, grep, worktree creation and PR links all work — they are git, run
on the host over ssh.

## When remote rows are missing or stale

The relay link is the usual suspect. In order:

1. `nebula remote <host> status` — nebula there ≥ 0.5.0 (has `nebula proxy`)? `daemon: running`?
2. The local daemon log (`daemon.log` in the state dir): `relay link up host=…` after boot or
   after `nebula add host:…`; a `relay link down` line carries ssh's own complaint (host key,
   BatchMode refusing a password, protocol mismatch → upgrade one side).
3. `ssh host 'nebula proxy' </dev/null` by hand: it should sit silently (Ctrl-C) rather than error.
4. A protocol mismatch (`Incompatible`) means the two nebulas disagree on the wire format:
   `nebula remote <host> upgrade` then `restart`, or upgrade locally.

## Security notes

Never copy personal ssh keys to the host — a per-repo deploy key (`gh repo deploy-key add
--allow-write`) is the pattern used for findl. The relay runs under your ssh identity and BatchMode:
it never answers prompts, so a host that needs a password simply stays unreachable.
