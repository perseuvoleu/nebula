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

## Protected branches: explicit approval required

**Merge or push to `main` / `master` only with Andrei's explicit approval for that operation.**
Without approval, use a task branch and offer a PR. Approval in the current conversation
counts: a direct request to merge/push the named work into `main`, or confirmation such as
“ai aprobarea mea, fă direct” after that operation was described, authorizes it. Proceed after
preflight without asking for the same approval again. Approval is scoped to that work and
resolved destination, not to unrelated projects, branches or future operations.

Before changing a protected branch or publishing commits:

1. Resolve the checkout, source commits, destination remote and destination ref. Protection
   follows `refs/heads/main` / `refs/heads/master`, not the currently checked-out branch; it
   applies to local merges, direct pushes, PR/API merges and implicit pushes through
   sync/collect or wrappers on either machine. An unknown destination is a blocker.
2. For a protected destination, state the resolved operation and identify Andrei's approval
   in the conversation or handoff. If approval is absent or its scope is unclear, ask before
   writing. Carry that same scope into delegated worker prompts.
3. Verify checkout ownership and cleanliness, inspect the commits being integrated, and
   preserve other sessions' work. Use an explicit refspec for pushes. Normal approval does
   not authorize force-push, history rewriting, branch deletion, `--all` / `--mirror`, or
   bypassing repository access controls; those require separate, specific authorization.
4. Report the resulting local/remote SHA and any remaining blocker. Read/fetch operations
   need no protected-branch approval.

Completion criterion: each protected-branch write has explicit, scoped approval and a
verified destination; every other destination is an explicit task/feature branch. A needed
sync does not itself supply approval. The hybrid-work skill still governs transfer need
and one-writer ownership.

## Application-local Nebula artifacts

For preview/setup work and before staging application files, follow
[Local Nebula artifacts — outside application Git](../hybrid-work/SKILL.md#local-nebula-artifacts--outside-application-git).
Keep `.nebula-preview.json` and other `.nebula*` configuration ignored and local to the
checkout; transfer needed recipes as ignored files, not commits or recipe-only merges.
Tell delegated remote workers the same rule. The runbook covers safe untracking and
preserving each host's copy when synchronizing a cleanup commit.

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
in Projects. Every local and remote checkout retains its own row and id, including primary
checkouts and matching worktree branches. Twins show `@local` / `@findl` badges; identity is host
plus absolute checkout path. Sessions and tabs stay associated with their host-owned checkout.
Every session picker offers the other side (`Run on findl ▸` in `n`;
flat `Claude on findl` … rows in ⌘T/⌘D). On the command line two projects share the name, so say
which: `--project nebula@findl` is the remote twin, `--project nebula` the local one.

A new session on an existing branch uses the selected host's primary checkout, switched under
`nebula switch`'s rules (refused on uncommitted changes; live sessions there stop only after
confirmation); it never creates a linked worktree. `--worktree primary` explicitly selects the
actual primary checkout; identical branch names on another host do not redirect execution.
Protocol 29 requires matching client and host daemons; schedule any restart with the user
because restarting ends sessions.

```bash
nebula agent new --project nebula@findl --kind pi --worktree root --name fix login
nebula agent read "fix login" --project nebula@findl
nebula agent send "fix login" "run the tests" --project nebula@findl
```

## `nebula remote <host>`

```
status    nebula + daemon on the host, the sessions there (live; archived as a count), and drift:
          `binary: <host sha> vs local <sha> (same|differs|unknown)` from `nebula --version`,
          `skills: in sync | N files differ` from `nebula-sync-skills --check` (content checksums)
sessions  every session there, archived included
watch     `sessions`, live, every 2s
sync      skills (nebula-sync-skills) + `git pull --ff-only` on every remote checkout + the
          local project's `.env*` files to its checkout on the host (existing ones kept;
          `--force-env` overwrites). New worktrees, local or remote, get the primary's `.env*`s
          copied in automatically; `nebula add host:…` pushes them once when a local twin exists.
upgrade   `nebula upgrade` on the host; its daemon keeps the old build until `restart`
restart   `nebula kill` on the host — ends every session there
```

`nebula remote <host> sync` is the **host-wide sweep** for the user. The **per-project verb** for
agents is `nebula sync` (run from a local checkout, no host argument needed): it reports both
sides (`--status`, JSON), moves commits the safe way through origin in either direction, carries
`.env*`, `.scratch/` and skills, and refuses with the fix when moving would lose work. The
hybrid-work skill is its runbook. For remote → local handoff, use `nebula collect <selector>`
with `--branch` for the primary or `--worktree` for a separate directory; consult `nebula collect
--help` for current flags. Collection preserves both rows and all remote sessions, and refuses
unsafe ownership/history rather than replacing a checkout. The canonical local runbook is
`/Users/andrei/Desktop/nebula/.claude/skills/hybrid-work/SKILL.md`, reachable from other projects.

## What does not work remotely, on purpose

The editor, file finder, tree browser and `gh` are local tools; on a remote checkout they flash
"lives on <host>" instead of opening a path that isn't here. Use a `t` shell tab on the remote row
and edit there. Diff, branches, grep, worktree creation and PR links all work — they are git, run
on the host over ssh.

## Browser verification runs locally

**findl has no browser.** Plan browser-dependent work (UI inspection, screenshots, Playwright /
Puppeteer browser tests, interactive web login) on the laptop, not on the server. A test runner
being installed does not mean a browser runtime is available. Install/download a browser or its
system dependencies on findl only with explicit user approval.

Remote workers can implement, build, run non-browser tests and prepare a private preview. Inspect
that preview from the local browser through an SSH tunnel or an already configured private route;
keep authentication intact and the preview off the public internet. This needs no mirrored
checkout or automatic sync. Transfer code only if the local verification actually needs it,
following the hybrid-work skill's on-demand rule.

Include this constraint in remote worker handoffs. If no local browser check can be performed,
report it as pending with the exact preview/test instructions; a build, HTTP check or DOM-only
test is not proof of browser behavior. Physical-phone checks are separate again.

Completion criterion: report where each verification ran and distinguish browser-verified UI
from server-only checks and pending manual checks.

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
