# Weekly account quotas

`GET /api/usage` requires the normal web bearer token and Host/Origin validation. It is
project-independent (handled before project validation) and returns `codex` and `claude`.
Each entry contains `remainingPercent`, `resetAt`, `updatedAt`, `status`, and a safe `reason`
code. Timestamps are Unix seconds. Status is `ok`, `stale`, `unavailable`, or `loading`.
Missing readings have null numbers; zero remaining is a real value. No credentials, account
identifiers, raw provider responses, or CLI diagnostics enter the response.

The authenticated app mounts one quota navbar above the existing navigation, across the
sessions list, conversation, terminal and desktop. Tap the row for reset times (browser
local timezone), last successful check and errors. `vechi` explicitly marks stale readings.
These are subscription account quotas, unrelated to selected sessions, tokens or costs.

## Sources and configuration

- **Codex:** the installed `codex app-server` stdio protocol. The gateway completes
  `initialize` → `initialized`, reads `account/read` with `refreshToken: false`, then
  `account/rateLimits/read`. Only subscription accounts are accepted; no threads, turns,
  inference, login/logout or reset calls are sent. The CLI owns authentication; the gateway
  does not parse Codex credentials. It inherits the host's `HOME` / `CODEX_HOME`, with
  gateway secrets and agent identity variables stripped, and starts in the host home directory.
  Ensure `codex` is on the service PATH. The process is terminated/reaped after each read,
  with a 12-second protocol deadline and a bounded response buffer.
- Select a window only when `windowDurationMins == 10080`, whether primary or secondary.
  If a nonempty `rateLimitsByLimitId` map is present, use its explicit `codex` account bucket.
  Other model buckets are never summed or arbitrarily selected. Ambiguous/model-only or
  absent weekly data is unavailable. Used percentages must be finite numbers; remaining
  is `100 - used`, clamped to 0–100. Missing/invalid reset times invalidate the reading.
- **Claude:** HTTPS `GET https://api.anthropic.com/api/oauth/usage`, with
  `anthropic-beta: oauth-2025-04-20`. Reads only `seven_day.utilization` (used percentage)
  and its ISO `resets_at`. This endpoint is **undocumented**, may change, and has reported
  429 failures. This is a practical idle-polling fallback, not an official stable API.
  On Linux the existing `CLAUDE_CONFIG_DIR/.credentials.json` is read in memory (default
  `~/.claude/.credentials.json`). Only an existing unexpired `claudeAiOauth` access token
  is used. No credential copies, refresh-token rotation, auth changes or billing APIs.
  Transport uses a fixed HTTPS host with certificate verification, no proxies/netrc or
  redirects, a 10-second request deadline (socket shutdown interrupts slow headers/bodies)
  and bounded response size. OS DNS resolution follows the host resolver timeout before
  socket cancellation is available.
- Claude Code's documented statusline `rate_limits.seven_day` is available from v2.1.251,
  but only after session API responses, with no guaranteed idle freshness. We do **not**
  install a bridge or change an existing statusline/configuration.

Missing CLI, missing/expired/non-subscription credentials, failed provider calls, ambiguous
buckets and absent windows are honest per-provider unavailable states. They never prevent
using sessions or the other provider. The required `Gateway` keyword `usage_service=None` explicitly
disables reads; `server.py` wires production sources. Synthetic e2e previews set `NEBULA_WEB_USAGE=0` to disable reads; standalone servers enable
reads by default.

## Refresh and retention

There are at most two in-memory cache entries per gateway process. Refreshes are shared
across projects, browsers and callers; concurrent requests reuse current values while one
refresh is in flight. Provider reads run independently, in parallel, on demand. Successful
reads and ordinary failures are cached for five minutes. 429s back off 15, 30, then 60 minutes;
Claude Retry-After can extend this up to 24 hours. No background polling without API demand.

Last-good values survive failures in memory and are labelled stale with the original success
and reset timestamps. Passing a reset time never restores 100%. Restarts discard the cache.
The UI requests every five minutes after completion, pauses while hidden/offline, revalidates
on wake, and aborts/cleans up on logout or unmount. An initial concurrent `loading` response
is checked again after 15 seconds, through the same server cache. Display age updates locally
without additional provider calls. The existing service worker excludes **all** `/api/*`;
HTTP responses and browser fetches use `no-store`.

## Verification on findl, 2026-09-15

Base/live release: `f29ca56d2b7de250d36b4749d1e050175f623739`. Server versions checked:
Codex CLI **0.153.2**, Claude Code **2.1.272**. Both real read-only provider requests succeeded:
Codex **15% remaining**, Claude **64% remaining**, both with future resets. These are samples,
not current promises. No raw responses, credentials or account IDs were logged. The server
Claude settings had no statusline; no settings were changed.

Automated checks (serial, `nice -n 15 taskset -c 0`, Node heap 1536 MiB):

```bash
PYTHONDONTWRITEBYTECODE=1 ~/.local/share/nebula/web-push-venv/bin/python -m unittest discover -s web -v
pnpm --dir web/app test:unit
pnpm --dir web/app typecheck
pnpm --dir web/app build
```

Results: **56 Python tests and 21 frontend tests passed**; standalone TypeScript check and
production build passed. The build reports its existing large-chunk advisory. Standards and
Spec reviews have no remaining findings after the explicit dependency and slow-response
deadline fixes.

Backend tests exercise synthetic weekly data, invalid/missing windows, API-key/missing
accounts, a real subprocess handshake and timeout, Claude credential expiry/redirect/429
handling, global auth/no-store, cache/backoff/reset retention and concurrent readers.
Frontend tests exercise real component server rendering (loading labels and accessible trigger),
normalization/labels, hidden/offline polling, wake single-flight,
disposal and immediate stale marking on connection loss. Existing focused web tests are
included. Core added paths were observed failing before implementation; additional failure
and concurrency regression cases were added after their implementation.

**Browser rendering and physical-phone verification remain pending.** Builds and Node tests
are not visual evidence. No browser or browser-system dependencies were installed on findl.

## Private visual preview (no production promotion)

On findl, from this worktree after build, run the gateway on an unused loopback port using
only a synthetic preview token (not the production credential):

```bash
NEBULA_WEB_TOKEN=weekly-preview-synthetic-only-2026 \
NEBULA_WEB_PROJECTS=nebula,logistica,scutulv2,vela-hub-fork \
NEBULA_WEB_PORT=8799 NEBULA_WEB_ORIGIN=http://127.0.0.1:8799 \
NEBULA_WEB_BINARY="$HOME/.local/bin/nebula" \
NEBULA_WEB_DB="$HOME/.local/share/nebula/nebula.db" \
"$HOME/.local/share/nebula/web-push-venv/bin/python" web/server.py
```

On the laptop (browser only; do not sync or collect code):

```bash
ssh -N -L 8799:127.0.0.1:8799 findl
```

Open `http://127.0.0.1:8799` and enter the synthetic token shown above. Check 320/375px widths,
desktop, sessions across projects, Pi conversation, terminal, navigation controls, scrolling,
keyboard/touch detail dialog, offline/wake, stale/unavailable labels and logout. This preview
can control real sessions; use it for inspection, without sending prompts or changing agents.
Stop only this foreground preview process when finished. Physical phone checks need a private
reachable origin arranged separately; no infrastructure changes are part of this task.

Production promotion requires follow-up approval. Follow the existing release export/build/
seal procedure in `docs/operations/web-service-recovery.md` and
`docs/operations/web-push-activation.md`; preserve the active release and all existing service
configuration. No Nebula daemon/agent restart, laptop integration or main/master push.

Primary references checked (Context7 unavailable):

- [Codex app-server protocol](https://developers.openai.com/codex/app-server)
- [Codex account schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/account.rs)
- [Claude statusline usage](https://code.claude.com/docs/en/statusline#rate-limit-usage)
- [Claude OAuth usage 429 report](https://github.com/anthropics/claude-code/issues/31021)
