# Branch intent stays on primary

Branch actions use the project's primary checkout on the selected host. They never
create or implicitly enter a linked worktree. Explicit worktree creation and
explicit existing-checkout selectors retain their separate-directory behavior.

## Execution boundary

`ResolveBranchCheckout` and `CheckoutPrimary` share `checkout_primary_locked`.
The caller holds the lifecycle mutex; the helper takes the execution write lock.
It rejects foreign-host anchors, live primary occupants (including finished agents,
shells and warm PTYs), conflicting edits, and branches parked in another checkout.
A primary already on the requested branch may be reused without moving files.
Parked checkouts and historical session rows are never removed by resolution.

Warm PTYs remain protected and expire through the existing 15-minute reaper;
resolution does not kill them to make a switch succeed. Expected-branch validation
at agent/terminal creation continues to reject a switch between resolve and spawn.

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
- Switch emits/injects a destination only after success and availability checks.
  Remote branch switches perform no local Git mutation; the owning daemon resolves
  or fetches the branch. Remote checkout paths are checked over read-only SSH,
  and are never injected as `cd` into the CLI machine's terminal.

## Compatibility and scope

Protocol version remains **29**; no wire fields change. CLI/TUI changes take effect
when those processes launch the installed binary. The primary resolver requires a
user-approved daemon restart window on each execution host, including findl.
Installing a binary does not update a running daemon.

Collect defaults, checkout deletion semantics, and ghost-row cleanup are unchanged.
In particular, `(wt)` is a physical label, not a guarantee about legacy deletion
behavior; `for_branch` deletion policy is a separate follow-up.

## Regression seams

Real-Git daemon and CLI/PTY fixtures verify primary HEAD, branch-specific files,
actual agent/terminal cwd, unchanged checkout counts, dirty/live/warm refusals,
parked-branch refusal, explicit worktree reuse/creation, unavailable destinations,
and expected-branch races. TUI event/render tests cover menu routing and physical
labels. A local transport fixture projects a remote snapshot while retaining a real
owning daemon/Git repository; only its SSH transport is substituted.
