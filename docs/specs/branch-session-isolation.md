# Independent branch sessions

> Superseded for branch actions by [branch-primary-intent.md](branch-primary-intent.md):
> branch actions use guarded primary resolution and never create a linked worktree.

## Problem Statement

A user wants to jump quickly between branches and open another agent without changing what existing agents are working on. Today the UI remembers the branch a conversation started on, but multiple sessions can share one physical checkout. Opening a session on a branch without a checkout automatically switches the primary checkout. Agents still appear under their original branches while reading and writing the newly checked-out branch. A confirmation for busy agents does not provide isolation; a finished but live agent can resume later.

The reported example has an agent on web-terminal-choice in the primary checkout and a request for a new session on main. The new session must not switch the old agent's files to main. Branch labels must not imply isolation that does not actually exist.

## Solution

Resolve a real checkout for each requested branch on the selected host. Reuse an existing checkout of that branch; otherwise create a managed linked worktree for the existing branch automatically. Never switch an occupied or unoccupied primary checkout merely to navigate or start a session. Bind sessions to their actual host-owned checkout. Switching the viewed branch/session is a UI action, not a Git checkout operation.

Different branches have independent working directories. Sessions intentionally created on the same branch and host may share that branch's checkout. Worktree provisioning is handled by Nebula; users should not need to create or name task worktrees just to have independent branch sessions. Existing explicit task-worktree workflows remain available.

## User Stories

1. As a developer, I want to open a main session while a feature agent works, so that neither agent changes the other's branch or files.
2. As a developer, I want to select another branch without checking it out over the current directory, so that navigation is safe and instant.
3. As a developer, I want a missing branch checkout provisioned automatically when I start a session, so that I do not need manual Git worktree commands.
4. As a developer, I want an existing branch checkout reused, so that repeated sessions do not create duplicate directories.
5. As a developer, I want multiple agents on the same branch to share its checkout intentionally, so that same-branch collaboration remains available.
6. As a developer, I want running, waiting, fresh and finished-but-live sessions protected equally from automatic branch switches, so that status does not determine filesystem safety.
7. As a developer, I want my agent's subsequent reads, edits and commits to stay on its branch after I navigate elsewhere, so that sidebar placement matches actual execution.
8. As a developer, I want terminal tabs and split/new-agent actions to honor the same selected checkout, so that keyboard entry points do not bypass isolation.
9. As a developer, I want a resumed or restarted session to use its correct branch checkout or explain a safe recovery requirement, so that an old conversation cannot silently continue on unrelated files.
10. As a developer, I want local and findl sessions resolved independently by host and checkout path, so that identical branch names do not mix execution destinations.
11. As a developer, I want concurrent creation requests for one branch to converge on one checkout, so that racing UI actions do not fail or create duplicates.
12. As a developer, I want uncommitted changes in other checkouts preserved, so that opening a new branch session is non-destructive.
13. As a developer, I want actionable errors for invalid branches, unavailable hosts and checkout creation failures, so that a failed request never falls back to the wrong branch.
14. As a developer, I want existing explicitly created worktrees preserved and reused where appropriate, so that this behavior does not disrupt existing workflows.
15. As a developer, I want the sidebar and breadcrumb to tell the truth about the agent's actual checkout, so that historical branch metadata cannot hide a mismatch.
16. As a developer, I want explicitly changing a checkout's branch to be separate from navigation/new-session actions and guarded when other sessions depend on it, so that intentional Git operations cannot silently undermine session affinity.
17. As a developer, I want auto-provisioned checkout setup to use Nebula's existing environment/bootstrap behavior, so that the new session is as usable as an ordinary managed worktree.
18. As a developer, I want managed branch checkouts to remain after session completion, so that returning to them is fast and no uncommitted work is unexpectedly deleted.

## Implementation Decisions

- A branch reference is not a working directory. Session execution identity is host plus checkout identity/path; remembered conversation branch metadata is not proof of physical isolation.
- New-session resolution is host-scoped and branch-aware: reuse the selected host's checkout of the requested branch, including primary; otherwise create a linked checkout of that EXISTING branch. Never create an unintended task branch, detach HEAD, force duplicate branch checkouts, or switch primary as a fallback.
- Keep branch creation, explicit task worktree creation and explicit checkout movement as distinguishable intents. Creating a new session on an existing branch must not invoke the primary-switch path.
- Selecting a branch for viewing does not mutate Git. Provisioning is deferred until an action actually needs an execution checkout.
- Reuse the daemon's existing checkout lifecycle, serialization and reconciliation where possible. Concurrent resolution must be atomic/idempotent per host/project/branch. Report success only once a usable checkout is registered and available to spawn the requested session.
- New agent, terminal, split and equivalent CLI/remote request paths must preserve explicit execution destination. Existing `--worktree primary` still means that actual checkout; do not silently reinterpret explicit checkout selectors as historical branch names.
- Keep sessions bound to physical checkouts in the UI. A historical branch mismatch on an existing live process must be surfaced honestly rather than cosmetically grouping it into another checkout. Do not claim that changing a database row moves a running process's cwd.
- For legacy sessions whose recorded branch differs from their actual checkout, do not kill, move or restart processes automatically. Provide a safe explicit recovery/error path. A stopped session being resumed must resolve/validate its execution branch before spawning; preserve conversation identity where supported and fail clearly where safe resume cannot be guaranteed.
- Explicit checkout-switch operations remain deliberate operations. Refuse a move that would invalidate other live sessions' checkout affinity; finished-but-live agents and terminal tabs count. Do not add a force escape that bypasses isolation. A sole caller explicitly moving itself may retain existing supported semantics.
- Preserve host ownership: same-named branches on local and findl are separate physical destinations. Never resolve a remote spawn into a local checkout or create a local counterpart merely because a twin exists.
- Preserve dirty work, existing worktrees, branch ancestry metadata and established setup behavior. Automatically hosting an existing branch must not make normal cleanup more destructive or implicitly delete its branch. No background garbage collection is introduced.
- Prefer existing protocols/data structures. Add schema or protocol changes only when necessary, with compatible persisted-state handling and explicit daemon version/restart reporting.

## Testing Decisions

- The primary acceptance seam is an existing daemon or CLI integration fixture with disposable real Git repositories, supplemented by existing TUI event-loop tests to verify user intent routing. Test external behavior, not helper implementation shape or labels alone.
- Core red/green regression: primary on feature A with session A bound to it; request a new main session; assert primary's HEAD/path and feature-specific file content are unchanged, the new session uses a different checkout on main, and both sessions' execution directories see their own branch content. Where the fixture supports real shell sessions, perform writes/commits from both and assert each reaches only its own branch. Otherwise add the highest practical session-execution seam and state its limits.
- Repeat with session A running, waiting and finished-but-live. Terminal sessions must receive equivalent protection.
- Repeat branch-session creation and race two requests: reuse one checkout and produce correctly bound sessions without duplicate branch checkout errors.
- Verify selection alone issues no Git mutation, while agent/terminal/split creation on an unmaterialized branch uses resolution rather than CheckoutPrimary.
- Verify reuse of primary when it is already on the requested branch and reuse of an explicit existing linked worktree. Same-branch sessions may share it.
- Verify preservation of uncommitted files, rejection of invalid branch/create failures without wrong-branch fallback, and host-scoped local/remote resolution.
- Cover persisted legacy branch/path mismatch, live-process handling, stopped-session resume, and truthful sidebar grouping. No test should treat historical grouping alone as proof of isolation.
- Cover explicit switch refusal when another live session depends on the checkout, including finished agents and shells, without killing processes.
- Existing tests that encode automatic primary checkout or misleading historical session grouping must be updated to the new contract, not simply deleted to get a green suite.
- Run focused regressions first, then relevant workspace tests/build with the supported Ghostty/Zig setup or clearly report the no-default-features fallback. Live TUI/manual verification is separate from server-only automated checks; do not claim a daemon deployment from a source build.

## Out of Scope

- A private checkout for every agent on the same branch; same-branch sharing remains intentional.
- Automatic sync, merge, publish, collection, deployment or branch pushes across hosts.
- Automatically moving running processes, restarting existing sessions, rewriting conversation history or cleaning up old worktrees.
- A new sandbox engine, Git implementation, browser UI redesign or generalized worktree framework.
- Automatic garbage collection of managed branch checkouts.

## Further Notes

The user explicitly approved branch-level independence for better developer experience and requested implementation on findl's main checkout. This is a change to the current product contract: checkout switching should not be a side effect of selecting a branch or opening a session on it.

The spec was synthesized from the conversation using to-spec. No tracker/triage integration is configured, so it is delivered as canonical repository documentation rather than a published ready-for-agent issue. Test seams are proposed above; no separate user confirmation of those seams was obtained before this requested immediate delegation.

Do not push main/master. Leave implementation commits on findl, report them, and wait for explicit integration/collection instructions. Daemon-affecting changes need a later user-approved restart window; restarting ends sessions.
