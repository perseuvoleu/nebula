---
name: hybrid-work
description: "Working one project from the laptop and from findl at the same time, with agents on both sides: where to spawn a worker (local vs findl), the one-writer-per-branch rule across machines, handing a task to a remote worker and collecting its result with `nebula sync`, reading files that live on the host. Trigger: \"remote worker\", \"findl worker\", \"handoff\", \"continuă pe server\", \"adu ce a făcut agentul de pe findl\", sync between machines, the `[nebula] This project has a twin on <host>` prompt line, or a `nebula sync` refusal."
user-invocable: true
---

A project with a **twin** (the same-named project anchored on findl — see the remote-projects
skill) is one repo with two checkouts that nothing keeps in step. `nebula sync` is the verb that
does; this runbook says when to use it and how to place work so the two sides never write the same
branch. The prompt line `[nebula] This project has a twin on findl (…)` means this runbook applies.
`nebula sync --help` is the source of truth for flags.

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

```bash
nebula agent wait --project <name>@findl [<names>…]   # blocks until the remote workers settle
nebula sync --from findl                              # findl pushes → origin → this checkout fast-forwards; its .scratch/ notes come along
git log --oneline -5                                  # verify from the commits, not from the summary
```

A bare `nebula sync` does both directions when only one side is ahead; use `--from` when the
laptop has unrelated uncommitted work you do not want touched (it still refuses if this side is
dirty and would receive commits).

Completion criterion: the remote commits are on this checkout's branch and the remote session is
finished or archived.

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
| `the checkout on findl has uncommitted changes` | tell the session there to commit (`nebula agent send <name> … --project <name>@findl`), or `ssh findl 'git -C <path> stash'` |
| `this checkout has uncommitted changes` | commit or stash here, explicit paths only |
| `both sides have commits the other lacks` | someone wrote the same branch on both sides (rule 2 was broken). `nebula sync --rebase` rebases **this** side onto findl's commits; the host's history is never rewritten. Or merge by hand |
| `--to would need the other direction` / `--from …` | run the direction it names, or a bare `nebula sync` |
| `… has no `origin` remote` / `git fetch origin failed` | commits travel through origin; fix the remote or the deploy key on that side |

`--dry-run` prints the plan without running it when a refusal or a move is surprising.

Completion criterion: after acting, `nebula sync --status` shows `plan.steps` without a `refusal`.
