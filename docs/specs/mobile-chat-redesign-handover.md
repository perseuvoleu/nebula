# Mobile chat redesign — candidate handover

Branch `mobile-chat-redesign` (worktree on findl). Canonical spec:
`mobile-chat-redesign.md`; Pi source proof: `mobile-chat-redesign-pi-source.md`;
operations, promotion and rollback: `web/README.md`.

## Local integration verification — 2026-09-13

The remote handover below is the original candidate report. During integration with the
local TUI/collect changes, the laptop ran the previously pending browser checks:

- 24/24 Playwright E2E tests passed against the real gateway and synthetic native-agent data.
- 27/27 Python tests and 5/5 TypeScript unit tests passed; production build/typecheck passed.
- Rust: 705 library tests (core 12, daemon 151, TUI 542), plus 31 PTY and 8 TUI E2E tests passed.
- Browser verification exposed and fixed an invalid-login error lost by premature auth-state
  publication and a session-switch race mounting the next composer with the previous draft.
- E2E fixtures now reset native-agent state per test; selectors identify actual controls and
  exact messages; the branch-reset test really branches instead of merely appending.
- A long hostname no longer squeezes the project picker below a usable width.
- Screenshots were reviewed for phone and desktop monochrome layouts. Artifacts live under
  `.scratch/mobile-chat-e2e/`, not tracked source directories. Physical iOS/Android keyboard,
  installation and background checks remain pending. Source integration is not live deployment.
- Local tooling used Node 24.20 and pnpm 10.34; `--ignore-workspace` isolated dependency
  installation from the laptop's unrelated parent pnpm workspace. Rust tests use their normal
  isolated temporary directories (placing non-repository fixtures inside this repo breaks
  ancestor-discovery assumptions); command logs remain in project scratch.

## What is delivered

| Slice | Content | Status |
| --- | --- | --- |
| 1 Native-source proof | `web/pi_history.py` adapter, gateway conversation contract, read-only real-session proof | done, tested |
| 2 Application foundation + 3 Pi conversation + 4 resilience | `web/app` React/TS/Vite monochrome app, drafts, attempts, cursor polling, fallback, SW | done; browser acceptance pending |
| CLI identity exposure | `nebula agent show/list` JSON gains `session_id`; no-VT build wiring fixed (`default-features = false` on the workspace dep) | builds without zig on findl; cannot talk to the live daemon (protocol 28 vs 27), so unproven live |
| 5 Verification/handover | Python suites, node unit tests, Playwright suite + harness, docs | browser part pending |

## Verified on findl (no browser available)

- `python3 -m unittest discover -s web`: 26 tests (gateway auth/scope/control/static/fallback
  semantics, Pi adapter mapping/normalization/replay/paging, DB fallback).
- `pnpm --dir web/app typecheck && build`: clean; `test:unit`: 5 node tests (attempt
  reconciliation, tool-result merge, routes).
- Real read-only proof through the actual gateway with `NEBULA_WEB_DB`: the owner's live Pi
  session on project `nebula` returned 10 structured messages with 4 tool activities and a
  `none` update on its cursor; the Claude session reported terminal-only; second project
  listed. No content was logged.
- Harness smoke through the real gateway with the fake CLI: two projects, two Pi sessions in
  one checkout, hidden context absent, truncated/full tool output, send → delta with running
  status, create → resolved id, unsupported reasons for Claude and for a Pi session without
  identity.

## Original browser handoff (completed locally above; physical phone still pending)

`pnpm --dir web/app exec playwright install chromium` once, then `pnpm --dir web/app test:e2e`
on the collected checkout. Expected coverage and screenshot set are described in
`web/README.md` (Verification). Until then the following are **not verified**: rendered
layout at 320/390/tablet/desktop/landscape, composer/keyboard behaviour, jump-to-latest,
service-worker registration in a real browser, visual black/white acceptance, physical
iOS/Android install/background/keyboard. Visual inspection of the candidate can also use the
loopback preview + SSH tunnel recipe in `web/README.md`.

## Decisions to be aware of

- Bounded polling (1.5–2.5 s, cursor-based, cheap no-change answers) instead of SSE: no live
  events exist for existing Pi sessions, the stdlib server holds no thread per client, and
  the Traefik route needs no change. Revisit together with a streaming extension.
- Custom typed conversation controller instead of AI SDK UI `useChat` (existing
  conversation, terminal-originated messages, cursor resume). AI Elements is used for the
  conversation/stick-to-bottom primitive; Markdown is react-markdown + remark-gfm (no raw
  HTML, monochrome code blocks) rather than streamdown/shiki.
- Native identity: CLI field first, read-only SQLite fallback (`NEBULA_WEB_DB`) so promotion
  does not require installing a new binary. `agents.claude_session_id` is the stored column.
- Session-file leaf = last entry, exactly like Pi's own loader. Compaction and branch jumps
  reset the cursor and are shown as notices; pre-compaction history stays visible.

## Design overview (monochrome)

- Tokens: true-black page, `oklch(0.13)` cards, `0.18–0.22` hover/selection surfaces, white
  primary action with black label, `0.72` gray secondary text, `0.26` borders, single dark
  theme (`color-scheme: dark`), Geist (self-hosted) for UI, monospace only for code/tool/terminal.
- Hierarchy through weight and spacing: user messages as gray pills right-aligned, assistant
  text unboxed at a 72ch measure, tool activity as collapsed rows with a text state label,
  notices dashed. No color-coded status: text + a pulsing dot for "working".
- Composition: ≥768px sidebar (18/20rem) + conversation; below, list → full-screen
  conversation with back; project selector and host badge in both; new-agent dialog;
  composer pinned above the keyboard with safe-area padding; jump-to-latest pill only when
  scrolled up; reduced-motion honoured globally.
- Screenshots: produced by the Playwright suite (`.scratch/mobile-chat-e2e/screenshots/`) on
  the laptop during integration; none were captured on findl.

## Review outcome

Two-axis review (standards + spec) was run on the diff; fixed: session-list poll surviving a
project switch, controller creation under StrictMode, stale checkout in the new-agent dialog,
clock-sensitive attempt reconciliation, never-relaxing poll cadence, transient gateway
failures presented as "unsupported", title-based selection of new sessions, offline sends
labelled "uncertain", copy hidden on touch, unbounded message list, prepend scroll anchoring,
native ids leaking to the browser, and the uncertain-delivery e2e premise. Accepted as
documented: `web/test_app.cjs` (3 vanilla-JS polling tests for the deleted `app.js`) is
replaced by `web/e2e/conversation.spec.ts` (selection, stale response, rename), which is
pending execution; every `/updates` poll still runs one `agent list` subprocess (status and
rename need it), bounded by the 4-slot semaphore.

## Remaining issues / risks

- Live partial text is not available (documented). Pi persists a new session only after its
  first assistant reply.
- `agent send` still selects by name; the CLI rename race documented for the MVP remains.
- `agent show` costs one subprocess per selected session every 5 s (identity cache TTL);
  acceptable at single-user scale.
- Bundle: 498 kB JS (156 kB gzip), fonts self-hosted; no lazy chunks yet.
- The Rust change touches only JSON output and Cargo feature wiring; the full Ghostty build
  and Rust test suite were not run on findl (no zig).
