# Branch intent stays on primary

Branch actions use the project's primary checkout on the selected host. They never
create or implicitly enter a linked worktree. Explicit worktree creation and
explicit existing-checkout selectors retain their separate-directory behavior.

## Session listing

A session records a branch (`Agent::branch`, `TerminalTab::branch`): the one
it was started on, updated when the session itself moves (its process enters
another checkout, or it is the mover of a primary switch). A session with a live process is listed under the
checkout its process runs in, and shows `⚠ expects <branch>` when that checkout
is on another branch. A session with no live process is listed under its
branch: the checkout holding that branch, else the `(branch)` row.

## Execution boundary

`ResolveBranchCheckout` and `CheckoutPrimary` share `checkout_primary_locked`.
The caller holds the lifecycle mutex; the helper takes the execution write lock.
It refuses, before stopping anything and in this order: foreign-host anchors,
a branch that exists neither here nor on origin, a branch parked in another
checkout, uncommitted tracked changes on primary
(`primary has N uncommitted files on "A"; commit them before switching to "B"`),
and an untracked file the target branch would overwrite. There is no stash;
untracked files the target does not overwrite stay in primary across the switch. A primary already on the requested branch is reused without
moving files.

Every live session on primary other than the mover, whatever its status, then
refuses the switch unless the request carries `stop_busy`. The error starts
with `BUSY_SESSIONS_REFUSAL` and lists every session that would stop:
`primary has sessions on "A"; switching to "B" stops fix (agent, running), dev (terminal)`.
Warm PTYs are stopped without asking.

A confirmed switch stamps branch A on any stopped row that had none, hangs up
each PTY (SIGKILL after the grace period, as archive and delete do), and waits
for each session's own process to exit before running `git checkout`. A process
still present at the deadline refuses the switch with
`<session> still running; primary stays on "A"`. Stopped rows keep their CLI
session id and recorded branch, and agents read disconnected. Each is listed
under its recorded branch: A, unless it already recorded another one C (it was
showing `⚠ expects C`). Attaching one once primary is back on its branch resumes
its conversation; attaching it before then is refused with
`session expects branch "<its branch>"`. Only the mover keeps
running and follows B. Parked checkouts and historical session rows are never
removed by resolution.

Known limits: descendants that detach from the session (`setsid`, `nohup`) are
not stopped. A stop that outlives the deadline (a process surviving SIGKILL for
about 2 s) is dropped by a later prewarm, warm reap or idle reap, and a retry no
longer waits for it.

## Front doors

- Plain `nebula switch` always requests primary, including when a linked checkout
  holds the branch. `--worktree` explicitly permits linked reuse/creation.
- `agent new --worktree primary` / `root` and existing checkout selectors keep
  their meaning. A missing selector interpreted as a branch uses guarded primary
  resolution, not worktree creation.
- TUI branch-agent, branch-terminal, quick/split and host-selection actions use
  primary resolution. Selecting an existing physical checkout remains explicit
  checkout intent. Physical linked rows and branch-picker entries read `(wt)`,
  even when historical `for_branch` metadata is true.
- `nebula switch` and `nebula worktree checkout` take `--stop-busy` to confirm
  stopping the listed sessions; without it the refusal ends with
  `rerun with --stop-busy to stop them and switch`. `agent new --worktree
  <branch>` has no such flag; its refusal names the `nebula switch … --stop-busy`
  command to run first.
- In the TUI, `Enter` on a `(branch)` row and its menu's "Switch primary to this
  branch" move primary to that branch. Attaching a session listed under a
  `(branch)` row moves primary first, then resumes it. A sessions refusal opens
  the "Stop sessions and switch?" confirm; confirming resends the request with
  `stop_busy`. Other refusals show as the flash message.
- `g`, `f`, `F` and `b` on a `(branch)` row read the branch tip from git
  (`git_diff::Source::Revision`) and never check it out. A `(branch)` row owned
  by a remote twin refuses them with `<branch> is a branch on <host> — open it
  there`. The diff has no
  working-tree side, and `Enter` on a hit in `f`, `F` or `b` opens a read-only
  copy of the file.
- Switch emits/injects a destination only after success and availability checks.
  Remote branch switches perform no local Git mutation; the owning daemon resolves
  or fetches the branch. Remote checkout paths are checked over read-only SSH,
  and are never injected as `cd` into the CLI machine's terminal.

## Compatibility and scope

Protocol version remains **29**. The one wire change is `CheckoutPrimary.stop_busy`,
skipped when false, so an unconfirmed request is byte-identical to the old one;
a confirmed request sent to an older daemon fails to decode and that daemon drops
the connection. CLI/TUI changes take effect when those processes launch the
installed binary. The primary resolver requires a user-approved daemon restart
window on each execution host, including findl. Installing a binary does not
update a running daemon.

Collect defaults, checkout deletion semantics, and ghost-row cleanup are unchanged.
In particular, `(wt)` is a physical label, not a guarantee about legacy deletion
behavior; `for_branch` deletion policy is a separate follow-up.

## Regression seams

Real-Git daemon and CLI/PTY fixtures verify primary HEAD, branch-specific files,
actual agent/terminal cwd, unchanged checkout counts, dirty and untracked
refusals, the sessions refusal and confirmed stop, resume on return,
parked-branch refusal, explicit worktree reuse/creation, unavailable destinations,
and expected-branch races. TUI event/render tests cover menu routing, the stop
confirm, branch-row listing and actions, and physical labels. A local transport
fixture projects a remote snapshot while retaining a real owning daemon/Git
repository; only its SSH transport is substituted.
