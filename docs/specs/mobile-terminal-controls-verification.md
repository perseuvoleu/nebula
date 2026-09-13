# Mobile terminal controls — verification and handoff

Source: [mobile-terminal-controls.md](mobile-terminal-controls.md).
Base: `a579456f1db6a8a530d58c5bdde566947f03fe70`; branch: `mobile-terminal-controls`.

## Implemented behavior

- Seven explicit, touch-sized terminal keys. One acknowledged PTY write per click, including standalone Enter; no composer submission, automatic trust choice, keyboard interception or mutation retry.
- Session deletion uses a captured ID, scoped again in the CLI and daemon. Confirmation names the session, project, checkout and ID and warns about stopping its process. Successful deletion invalidates stale list requests, removes its tabs and leaves another selection alone.
- Checkout removal uses a new conservative daemon operation, not legacy deletion or a Git fallback. Preview and confirmation both validate ID/path/branch. Primary and main/master are protected. Every agent row (including archived), terminal tab, dirty/untracked/ignored file, or HEAD not merged into local main/master blocks removal. No baseline also refuses. The branch is **always preserved**, including branches represented by `for_branch` rows. No branch-delete or force flow is exposed. Removal is one plain Git worktree removal, with no legacy unlock/retry or pruning fallback; locked checkouts stay locked.
- Session arrival is serialized with checkout operations. The safe path never stops user sessions before a failed Git removal. Read/scrollback paths remain unchanged.

## Compatibility and deployment (NOT performed)

The installed CLI was inspected: `agent send` pastes text then always writes Enter, `agent delete` selects a mutable name, and legacy `worktree delete` can stop sessions before Git refuses removal. None is a safe substitute for these new controls.

This change adds `agent key <ID> <KEY>`, `agent delete-id <ID>`, `worktree delete-safe [--check] <ID> <BRANCH> <PATH>`, and worktree IDs in list output. Protocol is **29** (was 28). The gateway must use this CLI, and the owning daemon must be upgraded; remote relays/owning hosts must also speak 29. Old installations fail closed; old worktree lists disable removal. Terminal keys use standard legacy VT escape sequences, not full physical/Kitty keyboard emulation.

Deployment requires an operator-approved CLI/daemon rollout and gateway/static app update. A daemon upgrade/restart affects live sessions: schedule it explicitly, never restart merely to try the web buttons. No deployment, real daemon restart, main merge, push, synchronization or collection was done here. Only isolated test daemons/processes were started/stopped. This worktree and worker remain intact.

The separate uncommitted yellow-name animation work was not present in this base and is not claimed deployed. The separate mobile-send running-status report was not changed; key delivery acknowledgement means only a PTY write, not an agent lifecycle/status transition.

## Verification on findl

Bootstrap ran first: `bash ~/.agents/skills/nebula-subagents/wt-bootstrap.sh`. It does not discover the nested `web/app/package.json`; dependencies were installed there with the frozen lockfile, `--prefer-offline --ignore-scripts`, not copied. No browser binaries/system packages were installed.

| Command | Result |
| --- | --- |
| `cargo check -p nebula --no-default-features` | Passed |
| `cargo test -p nebula --no-default-features --test e2e_pty mobile_ -- --test-threads=1` | 2 passed: real isolated CLI/RPC/PTYS, exact key bytes, renamed-ID deletion/project scope, safe checkout refusals/removal/branch preservation, locked checkout retained (verified red/green) |
| `cargo test --workspace --no-default-features -- --test-threads=1` | Not green: 12 CLI unit tests passed; PTY suite 32 passed / 1 failed, then Cargo stopped |
| `cargo test --workspace --no-default-features --lib -- --test-threads=1` | Remaining libraries: core 12, daemon 151, TUI 542 passed |
| `cargo test -p nebula --no-default-features --test e2e_tui -- --test-threads=1` | 7 passed / 1 failed |
| `cd web && python3 -m unittest test_server test_pi_history` | 33 passed; key/deletion gateway tests demonstrated red before gateway implementation |
| `pnpm --dir web/app test:unit` | 5 passed |
| `pnpm --dir web/app build` | Typecheck/build passed; existing >500 kB bundle warning |
| `pnpm --dir web/app lint` | No errors; five warnings in existing effect/export regions |
| `pnpm --dir web/app exec playwright test --list terminal-controls.spec.ts` | Five browser scenarios loaded/listed, **not executed** |
| `git diff --check` | Passed |

Failures outside the new scenarios:

- `worktree_new_seeds_node_modules_when_lockfile_matches`: expected seeded fixture file absent. Existing seeding invokes macOS `cp -c`, unavailable on this Linux host. No production seeding change made.
- `tui_pull_request_row_leads_the_links_group`: timed out waiting for `can't be deleted`. No changes to that PR-row behavior; baseline reproduction was not performed, so this is not asserted to be a proven pre-existing failure.

Default Ghostty build is unverified: zig is absent. Rustfmt is also absent; no toolchain components were installed. The complete workspace suite is not claimed green. Browser tests have not demonstrated runtime red/green here; that is explicitly pending local verification.

## Local browser / phone verification — pending

On a laptop with this checkout, dependencies and its approved Playwright Chromium already available:

```sh
pnpm --dir web/app build
pnpm --dir web/app test:e2e terminal-controls.spec.ts
# Regression pass, including existing conversation, composer and scrollback tests:
pnpm --dir web/app test:e2e
```

These commands start the real gateway on `127.0.0.1:8790`, with a fake CLI, synthetic token/data and no real Nebula sessions. Five added scenarios cover mobile key dispatch/errors/pending state, composer independence, deletion confirmation/cancel/failure/success, rename/selection changes, active-tab removal, protected/occupied/dirty checkouts and checkout refresh. Browser runtime, physical-phone touch/focus/scroll behavior and actual remote relay delivery remain pending. Do not use a real trust prompt or user checkout for verification.

## Changed files

- Specification/handoff: `docs/specs/mobile-terminal-controls.md`, `docs/specs/mobile-terminal-controls-verification.md`.
- CLI/RPC: `crates/nebula/src/main.rs`, `crates/nebula-core/src/protocol.rs`, `crates/nebula-tui/src/ipc.rs`, `crates/nebula-tui/src/lib.rs`.
- Daemon safety/dispatch: `crates/nebula-daemon/src/git.rs`, `crates/nebula-daemon/src/registry.rs`, `crates/nebula-daemon/src/server.rs`.
- Gateway: `web/server.py`.
- UI: `web/app/src/App.tsx`, `web/app/src/lib/types.ts`, `web/app/src/components/TerminalView.tsx`, `web/app/src/components/SessionList.tsx`, `web/app/src/components/CheckoutPanel.tsx`, `web/app/src/components/DeleteSessionButton.tsx`, `web/app/src/components/DeleteCheckoutButton.tsx`.
- Tests/fixtures: `crates/nebula/tests/e2e_pty.rs`, `web/test_server.py`, `web/e2e/terminal-controls.spec.ts`, `web/e2e/fake_nebula.py`, `web/e2e/serve.py`.

## Review

Sole-worker self-review against the saved specification and repository instructions (no review subagents spawned). Standards review kept existing auth/origin/subprocess boundaries and avoided animation regions/new dependencies. Spec review caught and addressed stale selection callbacks, name ambiguity on ID-only actions, existing browser locator collisions, session-arrival races during safe removal, and the legacy helper's silent unlock/retry (a failing isolated regression drove its replacement in the safe path). Remaining qualifications are the conservative branch-preserving removal policy, absent browser/default-Ghostty verification, and the two full-suite failures above.
