---
name: hybrid-work
description: "Working one project from the laptop and from findl at the same time, with agents on both sides: where to spawn a worker (local vs findl), the one-writer-per-branch rule across machines, handing a task to a remote worker and collecting its result with `nebula collect`, reading files that live on the host. Trigger: \"remote worker\", \"findl worker\", \"handoff\", \"continuă pe server\", \"adu ce a făcut agentul de pe findl\", sync between machines, the `[nebula] This project has a twin on <host>` prompt line, or a `nebula sync` / `nebula collect` refusal."
user-invocable: true
---

A project with a **twin** (the same-named project anchored on findl — see the remote-projects
skill) is one repo with two checkouts that nothing keeps in step. `nebula sync` is the verb that
does; this runbook says when to use it and how to place work so the two sides never write the same
branch. The prompt line `[nebula] This project has a twin on findl (…)` means this runbook applies.
`nebula sync --help` and `nebula collect --help` are the source of truth for flags.
This tracked file is the canonical runbook; from any local project read
`/Users/andrei/Desktop/nebula/.claude/skills/hybrid-work/SKILL.md`.

## 1. Placement rule

Decide where a worker runs before spawning it:

| Spawn on **findl** when | Spawn **locally** when |
|---|---|
| the task outlives a laptop session (long test suites, migrations, a job that must survive the lid closing) | the task needs the user's editor, browser or ⌘I file viewer on the result |
| the build is heavy and the laptop is doing other work | it is a quick edit the user wants to watch |
| the user says "pe server", "pe findl", "continuă pe server" | the checkout is dirty here and the changes are the task's input |

Preflight, either way:

```bash
nebula sync --status          # both sides: branch, head, dirty, ahead, sessions, plan
nebula agent list --project <name>@findl
```

One run of `nebula sync` is one project, one branch, one pair of checkouts: the local checkout
you name (or the one you are in) and the twin's checkout of the **same branch**. Nothing else on
the host is touched. The explicit form, from anywhere:

```bash
nebula sync --project <name> --worktree <branch|dir|root> [--to findl|--from findl] [--no-skills] --dry-run
```

Completion criterion: each planned worker has a side, and `--status` shows the branch it will use
exists on that side (`nebula worktree list --project <name>@findl` otherwise).

## 2. Ownership rule

**One writer per branch across both machines.** A branch with a running session on findl is the
remote's; a branch with a running session here is the laptop's. A second writer — on either side —
gets a worktree with a **task-named** branch (`nebula worktree new <task> --from <branch>
[--project <name>@findl]`), never the same branch on the other machine. Reviewers may read
anywhere. The prompt line's `(n running sessions)` per checkout is the ownership map; `nebula
agent list --project <name>@findl` is the live one.

Completion criterion: no branch has a planned writer on both sides.

## 3. Handoff out (laptop → findl)

```bash
git add <explicit paths> && git commit -m "…"       # sync moves commits, never a dirty tree
nebula sync --to findl                              # push → origin → findl fast-forwards; .env*, .scratch/, skills travel too
nebula agent new --project <name>@findl --kind <kind> --worktree <branch-or-root> \
  --name <Short Name> --prompt "$(cat <prompt-file>)"
```

The prompt to the remote worker states the branch it owns and where the spec is (`.scratch/…`
arrives with the sync). It must not run `nebula sync` itself against the laptop: the laptop is not
reachable from findl; syncing is the orchestrator's job.

Completion criterion: `nebula sync --status` shows equal heads and the remote session's row exists.

## 4. Collect back (findl → laptop)

Check `nebula collect --help` on the binary you will use. A source checkout selector can be a
branch, checkout id, directory name or absolute remote path. Resolve ambiguity with its id/path.
Choose the destination explicitly when the user's wording specifies one:

```bash
nebula collect <remote-branch> --branch --project <name> --dry-run
nebula collect <remote-branch> --branch --project <name>
nebula collect <remote-worktree> --worktree --project <name> --dry-run
nebula collect <remote-worktree> --worktree --project <name>
```

**Branch means the local primary directory; worktree means a separate directory.** With neither
mode flag, collect follows the source's primary/linked-checkout mode. The configured twin decides
the host; multiple twins require `--host`. `--agent` identifies the sole owning session; it does
not override ambiguous writers. Use `nebula worktree list --project <name>@findl` to resolve it.

Collect checks ownership on the source daemon. Busy or feedback-blocked agents refuse; wait for
the owner to settle and rerun. For dirty work, the sole finished live owner receives an explicit-file
commit-and-stop instruction. The source daemon guards its status revision and acknowledges only a
new prompt submission and completed turn. A stale finished dot cannot complete the handoff.
Clean, settled sources need no commit prompt. Timeout or remaining dirty files refuse with the
next action; inspect the owner with `nebula agent read` before retrying.

The JSON report carries source/destination host, absolute path, existing checkout id, SHA,
mode, steps and refusal. Dry-run does not prompt, fetch, create directories or change refs;
its history assessment is conservative when a remote cannot prove ancestry without fetching.
Actual collection reuses sync's remote push → origin → local fast-forward. Repeating it reuses
the destination. It preserves the remote checkout, its row and all sessions.

A shared/dirty local primary, parked branch conflict, ambiguous writers, local history ahead of or
diverged from the source all require explicit resolution. Follow the refusal; do not substitute
manual push/pull, stash, reset, detach, force-push or checkout/session deletion chains.

New local worktrees get missing env files from the **local primary only**. Collect imports
committed history, not remote env, scratch directories, skills, dependencies or build output.
Run the project's normal local setup afterward when needed; collect runs no bootstrap/install scripts.

Local and remote checkout identities are **host + checkout path**, never branch alone. Both
rows remain under the project with `@local` / `@findl` badges, and sessions stay with their host.
Select the local row for local file tools; remote actions continue to target findl.

This implementation adds guarded daemon RPCs (protocol 28). A newly built CLI alone is insufficient:
both local and remote daemons must eventually run the compatible build. Report that need and wait
for a user-approved safe session window; restarting a daemon ends its sessions. Do not claim a
feature is installed merely because source or build verification passed.

Completion criterion: the report has no refusal, local and remote SHAs match, and both checkout
identities and remote sessions remain present. A newly created local row appears on daemon git
reconciliation; its host/path identity is usable immediately from the report.

## 5. Reading files on findl

The editor, `f`, `b` and ⌘I are local tools; on a remote row they flash "lives on findl". Read
instead:

```bash
nebula agent read <name> --project <name>@findl --lines 120   # a remote session's screen
ssh findl cat ~/<name>/<path>                                  # any file in the host checkout
ssh findl 'git -C ~/<name> diff --stat'                        # what a remote worker changed
nebula sync --from findl                                       # bring .scratch/ notes over, then read locally
```

## 6. `nebula sync` refusals and what to do

Every refusal names the fix; the shapes:

| Refusal | Do |
|---|---|
| `findl is unreachable` | `nebula remote findl status`; offline laptop or relay down — retry later, `--status` still prints JSON with `reachable: false` |
| `the checkout on findl is on X, not Y` | the twin has no checkout of your branch: `nebula worktree new … --project <name>@findl` or `nebula switch` there |
| `… has no checkout of <branch>` | same: create it on findl first |
| `the checkout on findl has uncommitted changes` | ask the owning session to finish explicit-file commits; use collect for remote → local handoff |
| `this checkout has uncommitted changes` | finish explicit-file commits here, preserving other agents’ work |
| `both sides have commits the other lacks` | stop collection and review ownership/history with the user; choose an explicit integration plan |
| `--to would need the other direction` / `--from …` | run the direction it names, or a bare `nebula sync` |
| `… has no `origin` remote` / `git fetch origin failed` | commits travel through origin; fix the remote or the deploy key on that side |

`--dry-run` prints the plan without running it when a refusal or a move is surprising.

Completion criterion: after acting, `nebula sync --status` shows `plan.steps` without a `refusal`.
