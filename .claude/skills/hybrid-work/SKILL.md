---
name: hybrid-work
description: "Choose local vs findl execution and enforce one writer per branch. Sync only for a needed cross-machine transfer or an explicit request, not for every worktree. Trigger: remote worker, handoff, continuing work on the other machine, collecting remote results, the twin prompt line (ownership context only), or a nebula sync / collect refusal."
user-invocable: true
---

A project with a **twin** (the same-named project anchored on findl — see the remote-projects
skill) can be worked on from either machine. **Single-machine work is the default: create and
use a checkout only where the task runs.** A twin is available infrastructure, not a requirement
to mirror every branch/worktree. The twin prompt line is ownership context, not a sync request.

Use `nebula sync` / `nebula collect` when the current task actually needs commits or files on
the other machine (continuation, local review/integration, deployment), or the user explicitly
requests synchronization. Merely having sessions on both machines is not enough: independent
tasks may stay independent. An explicit local-only / remote-only / no-sync instruction takes
precedence; report a missing input rather than silently crossing that boundary.

Before any transfer that can push commits, apply [Protected branches](../remote-projects/SKILL.md#protected-branches-never-push-main-or-master): `main` / `master` are never push destinations,
including implicit pushes performed by sync/collect. Pass this guardrail to delegated workers.

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

Preflight the **chosen side**: use `nebula worktree list` and `nebula agent list` for its project
(`--project <name>` locally, `--project <name>@findl` remotely) as needed to resolve the target
and its owner. A local-only task needs no remote connection unless ownership of its branch is
in question. A remote-only task needs no local worktree. Create a missing task checkout only on
the execution side, from an appropriate base already available there.

| Task | Transfer decision |
|---|---|
| New local worktree, local implementation | Keep local; no sync/status sweep or remote counterpart |
| Remote worker with its required base and inputs already on findl | Create/use the remote checkout; pass the prompt directly; no local counterpart |
| Remote worker needs code/spec files that exist only locally | Transfer those required inputs; use §3 for committed checkout handoff |
| Work continues on the other machine, or its results are needed there | Transfer the relevant checkout when needed; §3 outward, §4 back |
| Read a remote status, diff, output or file | Read in place (§5); no sync or collect |
| User explicitly asks for sync | Inspect the named scope, preview and perform the safe transfer |
| User asks only for sync status or a dry-run | Report that scope; no transfer or counterpart creation |

When a checkout transfer is needed, or the user asks for its status/preview, inspect that scope
with `nebula sync --status` and/or `--dry-run`, using `--project` and `--worktree` to avoid an
unrelated primary checkout. One sync
pairs that local checkout with the host checkout of the **same branch**; the rest are untouched.
Create a receiving counterpart only for this actual transfer, not to satisfy a routine preflight.
A missing, dirty or unreachable unused twin is not a blocker for single-machine work.

Completion criterion: each worker has an execution side, an existing checkout there, its required
inputs and a safe owner. Equal heads and two checkouts are required only for a transfer that
actually needs them.

## 2. Ownership rule

**One writer per branch across both machines.** A branch with a running session on findl is the
remote's; a branch with a running session here is the laptop's. A second writer — on either side —
gets a worktree with a **task-named** branch (`nebula worktree new <task> --from <branch>
[--project <name>@findl]`), never the same branch on the other machine. Reviewers may read
anywhere. The prompt line's `(n running sessions)` per checkout is the ownership map; `nebula
agent list --project <name>@findl` is the live one.

Check the other side's live session list when reusing a branch that may already be owned there;
this ownership check does not authorize synchronization. A new task branch is created only on
its writer's side unless §1 identifies a concrete transfer need.

Completion criterion: no branch has a planned writer on both sides; read-only inspection needs
neither a mirror nor a sync.

## 3. Handoff out (laptop → findl)

Use this sequence only when the worker needs a local checkout's changes on findl. Starting a
remote worker by itself is not a handoff: if the inputs are already there, spawn directly on
findl. A text-only prompt can be passed to `agent new --prompt` without mirroring a checkout.

For a committed checkout transfer, first resolve the source and receiving checkout from §1,
check `nebula sync --project <name> --worktree <branch-or-root> --status`, then:

```bash
git add <explicit paths> && git commit -m "…"       # from the source; transfer commits, never a dirty tree
nebula sync --project <name> --worktree <branch-or-root> --to findl
nebula agent new --project <name>@findl --kind <kind> --worktree <branch-or-root> \
  --name <Short Name> --prompt "$(cat <prompt-file>)"
```

The prompt states the owned branch and how to find the required inputs: committed spec, prompt
text, or scratch files included in this transfer. Keep canonical specifications in tracked docs.
The worker does not run `nebula sync` against the laptop; any needed transfer is the
orchestrator's job. A completed handoff is not a standing instruction to sync every later commit.

Completion criterion for this transfer: scoped `--status` shows equal heads and the remote
session's row exists. For a direct remote spawn, verify only its checkout, inputs and session.

## 4. Collect back (findl → laptop)

Collect only when the user asks for the result locally or a concrete next step needs it here.
A remote worker finishing is not itself a collection trigger: report its branch, commits and
status, and leave remote-only work there. Reading the result in place is often sufficient (§5).

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
```

Read-only inspection needs no local counterpart. If actual local files become necessary,
choose the scoped transfer from §1 instead of using sync merely as a file reader.

## 6. `nebula sync` refusals and what to do

First confirm §1 still requires the transfer. If sync was run out of habit for single-machine
work, stop that sync path and continue on the chosen side; do not create counterparts, clean
unrelated dirty checkouts or move commits just to make an unnecessary preflight green.
For a needed transfer, every refusal names the fix; the shapes:

| Refusal | Do |
|---|---|
| `findl is unreachable` | `nebula remote findl status`; offline laptop or relay down — retry later, `--status` still prints JSON with `reachable: false` |
| `the checkout on findl is on X, not Y` | resolve/create the receiving checkout for this needed transfer using Nebula's checkout/worktree commands; preserve unrelated branches |
| `… has no checkout of <branch>` | create that receiving checkout only if the transfer needs it; otherwise keep work single-sided |
| `the checkout on findl has uncommitted changes` | ask the owning session to finish explicit-file commits; use collect for remote → local handoff |
| `this checkout has uncommitted changes` | finish explicit-file commits here, preserving other agents’ work |
| `both sides have commits the other lacks` | stop collection and review ownership/history with the user; choose an explicit integration plan |
| `--to would need the other direction` / `--from …` | run the direction it names, or a bare `nebula sync` |
| `… has no `origin` remote` / `git fetch origin failed` | commits travel through origin; fix the remote or the deploy key on that side |

`--dry-run` prints the plan without running it when a refusal or a move is surprising.

Completion criterion for a needed transfer: scoped `nebula sync --status` shows `plan.steps`
without a `refusal`. For an unnecessary sync attempt: no transfer or counterpart creation was
performed, and work can continue on its chosen side.
