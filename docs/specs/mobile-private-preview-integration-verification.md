# Main integration verification — 2026-09-14

## Integration and conflict decisions

The in-progress merge of `mobile-web-deploy` tip `9474933` into main
`3f22f10` was completed as `15f2d68`. Its merge base was `f5968b2`.
No merge was aborted, and all work stayed in the primary checkout on main.
The initial index/worktree contained only this merge's changes; the agent
registry was inspected before resolving them.

Nine files had conflicts:

- `crates/nebula-core/src/protocol.rs`: keep both branch resolution and safe
  removal requests; retain protocol 29.
- `crates/nebula-daemon/src/registry.rs`: retain main's execution read guards
  for session actions. Safe removal takes `worktree_ops` followed by the
  execution write guard, matching existing destructive checkout operations.
  The ops lock also excludes mobile's row-reparent paths during removal.
- `crates/nebula-daemon/src/server.rs`: dispatch both requests.
- `crates/nebula/src/main.rs`: retain safe removal and main's guarded explicit
  checkout help/behavior.
- `crates/nebula/tests/e2e_pty.rs`: retain both sets of regressions. All tests
  from both source tips survive: 35 combined PTY tests.
- `web/app/src/App.tsx`, `web/app/src/components/ConversationView.tsx`,
  `web/server.py`, `web/test_server.py`: use the mobile tip's stricter optional
  preview discovery and regression coverage, alongside deletion controls.
  Preview errors hide stale links, project switches cannot reuse another
  project's link, and CLI diagnostics are not exposed by preview discovery.

The subsequent merge of `ba9a7f1` applied cleanly. Its direct parent
`6318611` was already an ancestor of main; the merge adds only the five-file
publisher follow-up, without replaying the original preview implementation.
It retains YAML route publication, legacy JSON refusal, and bounded verified
adoption/rollback/withdrawal. Earlier live deployment records in the imported
private-preview spec are historical evidence, not actions taken by this integration.

Read-only Standards and Spec reviewers found no blocking integration defects.
No new tests or weakened assertions were introduced during conflict resolution.

## Verification

All heavy build/test commands ran serially, with Cargo builds limited to two
jobs for test commands. Only disposable fixture daemons were used by PTY tests.

| Command | Result |
| --- | --- |
| `cargo check -p nebula --no-default-features` | Passed |
| `cargo test -p nebula --no-default-features --test e2e_pty mobile_ -j 2 -- --test-threads=1` | 2 passed |
| `cargo test -p nebula --no-default-features --test e2e_pty branch_session -j 2 -- --test-threads=1` | 2 passed |
| `cd web && python3 -m unittest test_server test_pi_history` | 37 passed |
| `pnpm --dir web/app build` | Typecheck and production build passed; bundle-size warning |
| `pnpm --dir web/app test:unit` | 6 passed |
| `pnpm --dir web/app lint` | Passed with 6 warnings in existing effect/export regions |
| `pnpm --dir web/app exec playwright test --list terminal-controls.spec.ts` | 5 scenarios listed; browser execution pending |
| `cargo test -p nebula --no-default-features --test preview_cli -j 2 -- --test-threads=1` | 6 passed |
| `python3 scripts/test-preview-traefik.py target/debug/nebula /home/andrei/nebula-worktrees/private-project-previews/.scratch/private-preview-implementation/traefik-v3.6.7` | Passed in private mount/network namespaces using the existing Traefik binary |
| `python3 scripts/test-preview-publisher.py target/debug/nebula` | All 5 acceptance groups passed, including the 120-second first-publication timeout, rollback, concurrency, crash recovery, and uncertain withdrawal |
| `cargo test --workspace --no-default-features --lib --bins --test e2e_pty --test preview_cli --no-fail-fast -j 2 -- --test-threads=1` | 761 passed, 1 failed: CLI 12, core 12, daemon 153, TUI library 544, preview CLI 6; PTY 34 passed / 1 failed |
| `cargo test --workspace --no-default-features --doc -j 2` | Passed; no documentation test cases |
| `rustfmt --edition 2021 --check crates/nebula/src/preview/ingress.rs` | Passed |
| Python compilation of gateway/tests and the three publisher/verifier scripts | Passed; live verifier was not executed |
| `git diff --check` / `git diff --cached --check` | Passed |

The sole broader-suite failure was
`worktree_new_seeds_node_modules_when_lockfile_matches`: the seeded fixture
file was absent. The unchanged seeding implementation at both pre-integration
main `3f22f10` and the merged tree invokes macOS `cp -Rc`; GNU `cp` on this
host rejects `-c`. A disposable direct invocation reproduced that rejection.
The failure was already recorded in both source verification reports. No
seeding behavior or test assertion was changed to hide it. The workspace
suite is therefore not claimed fully green.

Command logs are local `/tmp/nebula-integration-*.log` artifacts.

## Rollout boundaries

- No push, deployment, installation, daemon restart, `nebula kill`, session
  archival, or worktree creation/deletion was performed. Fixture Git repositories
  and processes are disposable test resources, not user checkouts/sessions.
- The default Ghostty build remains unverified because Zig is unavailable.
- TUI end-to-end tests are excluded because their teardown invokes
  `nebula kill`, explicitly forbidden for this task. Browser/physical-phone,
  remote relay, and external IPv4/IPv6 verification remain pending.
- A later coordinated CLI/daemon/relay and gateway/static rollout is required
  for mobile controls. Protocol 29 alone does not make either partial source
  branch's daemon support the other branch's newly added requests.
- Mobile preview discovery requires the explicit `NEBULA_WEB_PREVIEW_BINARY`
  executable configuration. Source integration does not configure a live gateway.
- Legacy preview `.json` artifacts intentionally block publisher operations;
  any retirement requires a separate explicit operational action. No live
  artifacts or credentials were inspected or changed here.

## Changed files relative to pre-integration main

- `crates/nebula-core/src/protocol.rs`
- `crates/nebula-daemon/src/git.rs`
- `crates/nebula-daemon/src/registry.rs`
- `crates/nebula-daemon/src/server.rs`
- `crates/nebula-tui/src/ipc.rs`
- `crates/nebula-tui/src/lib.rs`
- `crates/nebula/src/main.rs`
- `crates/nebula/src/preview/ingress.rs`
- `crates/nebula/tests/e2e_pty.rs`
- `docs/specs/mobile-private-preview-integration-verification.md`
- `docs/specs/mobile-terminal-controls-verification.md`
- `docs/specs/mobile-terminal-controls.md`
- `docs/specs/private-project-previews.md`
- `scripts/test-preview-publisher.py`
- `scripts/test-preview-traefik.py`
- `scripts/verify-live-private-preview.py`
- `web/README.md`
- `web/app/src/App.tsx`
- `web/app/src/components/CheckoutPanel.tsx`
- `web/app/src/components/ConversationView.tsx`
- `web/app/src/components/DeleteCheckoutButton.tsx`
- `web/app/src/components/DeleteSessionButton.tsx`
- `web/app/src/components/Login.tsx`
- `web/app/src/components/SessionList.tsx`
- `web/app/src/components/TerminalView.tsx`
- `web/app/src/lib/types.ts`
- `web/e2e/fake_nebula.py`
- `web/e2e/serve.py`
- `web/e2e/terminal-controls.spec.ts`
- `web/server.py`
- `web/test_server.py`
