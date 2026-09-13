# Specification: Mobile terminal controls and safe session/checkout deletion

## Intent
The web/mobile Nebula UI currently renders terminal output read-only. A newly launched Pi can stop on 'Trust project folder?' with arrows/Enter options; users cannot navigate or confirm it from their phone. Users also need deletion controls for sessions and worktrees/branches in the web UI.

## Terminal input
- Add touch-sized accessible controls for Up, Down, Left, Right, Enter, Escape and Tab to the terminal view. Real keyboard forwarding can be added if straightforward, but must not intercept typing in the composer or other inputs.
- Send the actual key to the correct host/project/session PTY via an existing supported Nebula CLI/RPC path; inspect current CLI help and daemon implementation rather than guessing escape handling. Do not send a textual prompt or append an extra Enter for arrow keys. Enter must also work with an empty composer.
- An authenticated, validated gateway endpoint should use a fixed key allowlist and stable session identity. Preserve existing auth/origin protections, use bounded subprocess calls, avoid shell interpolation. No automatic retries of mutating requests. Disable controls while a request is pending, show delivery/error state without pretending a failed input succeeded.
- Trust/permission choices remain EXPLICIT user actions. Never automatically accept a trust prompt, select a parent directory, or bypass security prompts. These controls should work for generic interactive CLI dialogs, not just Pi trust.
- If target is disconnected/deleted/unsupported, show a clear error. Reading scrollback and scrolling must continue to work.

## Delete session
- Add a discoverable delete action in session list/details, usable on touch as well as desktop, not nested invalidly inside an existing button.
- Confirmation identifies the exact session and warns that a live process will stop. Cancel is side-effect-free. Use supported Nebula deletion semantics, not merely a UI filter or filesystem deletion.
- Validate stable identity server-side, scoped to the selected project. Do not target a different session after rename or selection changes.
- After success refresh list/tabs and move away from the deleted conversation to a sensible remaining session or empty state. Failures preserve UI/session selection and are actionable. Do not retry automatically.

## Delete worktree/branch
- Add a delete action next to the selected checkout/branch in the existing checkout UI. Clearly distinguish removing a worktree, deleting a branch, and stopping sessions. Inspect installed Nebula semantics and implement only supported safe operations; do not invent destructive git fallbacks.
- Show exact checkout path/branch and affected sessions before confirmation. Surface dirty state, unmerged commits, active sessions and what will be removed. Unsafe cases must refuse by default; if an explicit force flow is implemented it needs a separate clearly labelled confirmation, not silent force.
- NEVER permit deletion of the primary checkout or main/master through this web flow. Do not delete a branch currently checked out elsewhere. Preserve unrelated worktrees and sessions. Do not use deletion to free a branch for primary checkout.
- Refresh checkout/session state after success and select a valid remaining checkout. Backend validation must enforce protections independently of UI.

## Tests and acceptance
- Gateway tests assert auth, key allowlist, exact input dispatch (including standalone Enter), unknown/deleted session handling, errors, no automatic mutation retries, deletion identity/project scope, protected primary/main/master refusal and safe dirty/unmerged handling.
- Add UI/e2e coverage for key controls, error display, delete confirmation/cancel/success, selection refresh and protected checkout cases.
- On findl run builds/typecheck and non-browser tests only. findl has no approved browser runtime: DO NOT install/download browser binaries or system packages. Prepare exact browser verification commands or a private authenticated preview for local verification; report browser/phone verification as pending unless actually done on laptop.
- Do not stop real user sessions, delete real user branches/worktrees, confirm real trust prompts, or restart daemons to test; use fixtures/isolated test data.

## Scope boundaries and deployment
- Another local task has uncommitted yellow working-name animation changes in SessionList, SessionTabs, ConversationView, index.css and conversation e2e tests. Those are NOT in your remote base; do not claim they are deployed, and avoid unnecessary changes to those regions. Mobile-send running status was reported separately but is not a requirement to fix blindly in this task; report any relevant finding.
- Do not deploy, restart/kill daemons, sync or collect checkouts, or merge into main. Never push to main/master (including implicit pushes). Commit only on mobile-terminal-controls; no push is needed.
- Completion report: tracked spec path, commit SHA, changed files, tests run with results, pending browser/phone checks, and any daemon/CLI compatibility or deployment steps required. Leave worktree and session intact.
