# Mobile All sessions and recent activity

Implemented on `mobile-all-recent-sessions`, based on `3c6621e`.

## Ordering and activity contract

Both the individual-project list and All (including conversation tabs) use:

1. `running` and `needs_feedback` together, newest status activity first.
2. `finished` sessions in the existing persistent unread/blue set, newest first.
3. All remaining states, newest first.

Feedback-blocked sessions belong to group 1 because the TUI's existing
`is_active_status` definition includes `Running | NeedsFeedback`: they need
attention even though they are not producing output. `fresh`, `terminated`,
`disconnected`, read `finished`, and unknown statuses belong to group 3.
PTY liveness is never used as a proxy for active work.

The authoritative activity field is the daemon's persisted `Agent.status_changed_at`
(epoch milliseconds, set by `Store::set_agent_status` and carried on
`ServerEvent::StatusChanged.changed_at`). The CLI `agent list` now exposes this
existing field, just as `agent show` already did. The gateway already preserves
public row fields; no gateway implementation, daemon protocol, database migration,
or daemon restart is required.

This is **last lifecycle/status activity**, not every terminal byte, title edit,
creation time, native transcript mtime, polling time, or the TUI's synthetic
"active now" sort key. Polling unchanged sessions leaves their timestamps and
order unchanged. Missing, zero, negative or invalid timestamps are unknown and
sort after known positive timestamps within the same group. Ties use the stable
lexical `project/id` identity, independent of input order and session names.
Old CLIs remain usable with this unknown-time fallback; meaningful recency requires
releasing the updated CLI used by the gateway.

Each list row also shows the elapsed age of this same status timestamp beneath its
metadata: “acum câteva secunde”, “acum 2 min”, “acum 1 h”, “acum 1 zi” / “acum 2 zile”.
Minutes/hours/days are floored; future timestamps from clock skew clamp to zero age.
Unknown timestamps display “Activitate necunoscută”. Existing list refreshes update
the labels without additional requests or per-row timers. The age is also exposed
as an accessible description, including on unread rows with explicit labels.

## All, identity, access and errors

All is a virtual UI filter (`/?view=all`), not a daemon/Git project. A selected
session keeps its real route, for example `/p/beta/s/id?view=all`. The query retains
the aggregate context across reload/back navigation without requiring a new
server route. A real project literally named `All` is still distinguishable from
the virtual filter (whose selector value is empty).

The authenticated `/api/projects` allowlist is the only source of aggregate scopes.
Each existing project-scoped sessions endpoint is requested once per refresh,
serially. Polling remains at five seconds after completion and pauses while hidden;
filter changes cancel outstanding requests. There is no per-row activity/history
fetch and no `project=All` API request. Each gateway represents one host; within it,
React keys, selection, tabs, deletion and unread state use real project plus session
ID. All rows and tabs display the source project. Sending and creation use the real
conversation/target project, never a title or the aggregate filter.

Archived rows retain the existing hidden behavior. Search and the existing 200-row
list limit remain; ordering happens before that limit. All's new-agent button
focuses the existing project picker and asks for an explicit real target. Choosing
it opens the existing checkout/agent creation dialog. There is no implicit first
project creation target in All.

A failed project is explicitly named in the list error. Successful projects still
appear; failed project rows are omitted rather than substituted or shown as current.
An unavailable project's unread state is preserved until it is fetched successfully.
All-failed/offline states are distinguishable from an empty list. A partial failure
does not declare an already-open session deleted.

## Unread semantics

The existing `nebula-unseen-finished` localStorage format and reconciliation rules
remain unchanged. Merely visiting a project or All marks no sessions read. Only
opening the actual scoped session clears its blue state; identical IDs in another
project remain independent. An observed non-finished → finished transition makes a
session unread again. As before, a first-ever finished snapshot does not fabricate
an unread completion, and transitions entirely missed between polls cannot be
reconstructed from the stored status history. Failed projects are not reconciled
as empty projects.

## Verification and handoff

Automated seams: real CLI/isolated daemon lifecycle events, real HTTP gateway with a
synthetic external CLI, Node list ordering/aggregate fetching/route encoding/existing
unread reconciliation. Browser specs use synthetic harness data only.

Run serially from the worktree (one heavy command at a time):

```sh
CARGO_BUILD_JOBS=1 cargo test -p nebula --no-default-features --test e2e_pty agent_list_exposes_persistent_status_activity -- --test-threads=1
PYTHONPATH=target/mobile-all-python python3 -m unittest discover -s web -p 'test_*.py'
pnpm --dir web/app test:unit
pnpm --dir web/app typecheck
pnpm --dir web/app build
```

Completed checks on findl:

- CLI lifecycle regression: red because all four listed timestamps were absent;
  green after exposing `status_changed_at` (1 test).
- Node ordering/aggregate modules and All route: initial red, then green. Final
  full Node suite: 15/15. The pre-existing route assertion was updated to include
  the explicit `all: false` context after it identified the changed route contract.
- Full backend suite: 44/44. The first run lacked optional `py_vapid`; locked Python
  dependencies were installed only into ignored `target/mobile-all-python`, then
  the suite passed. No system packages were installed.
- Frontend typecheck and production build passed. Build reports its bundle-size
  advisory (main JS about 519 kB); no build error.
- Playwright `--list` discovers all 3 new scenarios; this does not execute them.
- Standards review: two findings addressed (accessible project identity for blue
  rows; browser assertions tied to DOM/classes). Spec review: accessible project
  identity finding addressed. Both read-only reviewers confirmed no remaining
  findings in their rechecks.

Logs are git-ignored `.scratch/mobile-all-*.log`. The scoped Python dependencies
are needed only when the current Python installation lacks the locked web runtime.

Browser and physical iPhone verification is **pending**, not proven by Node,
typecheck or build. No browser runtime/system dependency is installed on findl.
On a local verification checkout with dependencies and Playwright Chromium already
available, run:

```sh
pnpm --dir web/app build
pnpm --dir web/app exec playwright test ../e2e/all-sessions.spec.ts --workers=1
pnpm --dir web/app exec playwright test --workers=1
```

The existing Playwright configuration starts only its loopback fake-CLI harness.
On an iPhone against an authorized staging/release candidate: choose All; verify
source project labels and readable priority order; open equal-named sessions in two
projects; send synthetic text and confirm the correct destination; switch tabs,
return to All, reload/relaunch the PWA; confirm blue clears only for the opened
session and returns on a later observed completion. Use Agent nou in All, select a
real project, verify its checkout before creating. Check search, portrait/landscape,
keyboard visibility, offline/partial failure recovery, and that no list action
selects a session unexpectedly.

Deployment remains pending. The live PWA uses immutable durable releases, so this
source worktree does not change the live application. Publish through the normal
release procedure only after integration and separate deployment authorization.
No deployment, daemon/relay/web restart, global binary install, container change,
push, sync or collect is part of this task.

Integration: `web/server.py`, `web/test_server.py`, `web/e2e/fake_nebula.py`, and
`web/e2e/send.spec.ts` are untouched; the primary mobile autonaming/notification
changes in those files were not copied. Timestamp exposure is one additional field
in `crates/nebula-tui/src/ipc.rs`. If the relay worker also touches that file, retain
both independent hunks. This worktree remains intact on its assigned branch.
