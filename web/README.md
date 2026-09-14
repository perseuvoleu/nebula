# Nebula Mobile (single-user)

A Romanian, mobile-first, black-and-white PWA for controlling the agents that run on the
machine hosting the gateway. React + TypeScript + Vite frontend (`web/app`, shadcn/ui on
Radix, the AI Elements conversation primitive), Python 3.10+ standard-library gateway
(`web/server.py`), no daemon changes. Closing the app never stops server agents.

## What works

- Explicitly enabled projects (`NEBULA_WEB_PROJECTS`), active sessions with kind, checkout,
  status and feedback requests, existing checkouts, local search.
- **Pi sessions as real conversations**: user/assistant messages, tool activity (collapsible,
  bounded, full output on demand), safe Markdown with copyable code blocks, older-history
  paging, honest "agent working" state. History comes from Pi's persisted session file mapped
  by the native session id Nebula stored — never from terminal screen text. Details and
  limits: `docs/specs/mobile-chat-redesign-pi-source.md`.
- Other agent kinds (Claude, Codex, Cursor) and Pi sessions without persisted history show an
  explicitly labelled terminal snapshot, not reconstructed chat. It starts with 250 lines;
  scrolling to the top (or “Output mai vechi”) expands up to 10,000 retained lines while
  preserving the reading position. Older discarded terminal output cannot be recovered.
- Pi history loads older pages as you reach the top; polling does not evict pages already read.
- Sidebar branch/worktree selector and creation dialog: the selected session's checkout is the
  default base. New names use lowercase letters, digits and hyphens. Worktree mode creates an
  isolated directory; Branch mode switches the primary and refuses open agent sessions or dirty
  files there. Creation is never automatically retried. Only existing, enabled-project bases are
  accepted; read-only Git preflight and normal Nebula CLI commands run on the gateway host.
- Horizontal session tabs above conversations, yellow names for working sessions, white/black
  outgoing messages and composer, hidden scrollbar chrome (scrolling remains enabled). In terminal
  mode, sends from this browser are shown separately, not inferred from terminal text.
- Send a prompt with a delivery attempt id: `sending → accepted → observed in history`, or
  `uncertain` (timeout/lost acknowledgement) / `failed`, always keeping the text. Nothing is
  ever retried automatically. Repeated taps cannot double-send.
- Start pi, Claude, Codex or Cursor on an existing checkout; existing agents on that checkout
  are shown first; the new session is selected by id.
- Project and selected-session preview links appear only when the dedicated preview CLI reports
  `running` with a valid policy domain. Both open the project's currently published preview;
  selecting a session does not publish or switch its checkout. Status is polled every five seconds
  after each response; stopped, malformed, unconfigured and failed responses hide the links.
  The gateway reads status only and never reads preview credential files or returns CLI diagnostics.
- Phone: session list → full-screen conversation → back; desktop: persistent sidebar.
  URL carries `/p/<project>/s/<session id>` (no secrets). Per-session drafts in memory.
- Offline/resume: cursor-based resync without duplicate messages; stale state is labelled.
- Installable app shell; the service worker caches only public assets, never `/api/*`.
  iPhone: Safari → Share → Add to Home Screen. A visible “Instalează pe telefon” guide is
  available on login and in session navigation; standalone mode, touch icon and HTTPS are supported.
  The installed app may require the same token again; physical iPhone installation/keyboard checks
  remain manual.
- Bearer token in browser **sessionStorage** only, cleared on logout together with drafts and
  subscriptions. No token in URLs, cookies or caches.

Not included: live token streaming (see the Pi source note), archive/delete/restart, file
access, shell creation/endpoints, notifications, offline actions, multi-user.

## Build and run locally

```bash
pnpm --dir web/app install --frozen-lockfile
pnpm --dir web/app build            # tsc + vite → web/static (git-ignored)
export NEBULA_WEB_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
export NEBULA_WEB_PROJECTS='nebula' # comma-separated exact Nebula project selectors
export NEBULA_WEB_ORIGIN='http://127.0.0.1:8765'
export NEBULA_WEB_DB="$HOME/.local/share/nebula/nebula.db"   # see "Native identity"
python3 web/server.py
```

Open `http://127.0.0.1:8765` and enter the token. The gateway listens **only on 127.0.0.1**
and refuses to start without a built app. `NEBULA_WEB_PORT`, `NEBULA_WEB_BINARY` (absolute
Nebula binary) and `NEBULA_WEB_PI_AGENT_DIR` (default `~/.pi/agent`) are optional.

The allowlist is configuration, not discovery. On findl use local project names
(`nebula`, not `nebula@findl`).

### Native identity (Pi mapping)

The gateway needs the native Pi session id Nebula stored for an agent. It reads the
`session_id` field of `nebula agent show` when the installed CLI provides it (added on this
branch) and otherwise, when `NEBULA_WEB_DB` points at the daemon database, performs a
**read-only** SQLite lookup (`mode=ro`, schema-checked, id-addressed). Without either source,
Pi sessions report "identity not recorded" and fall back to the terminal view. Sessions
started before Nebula's Pi extension existed have no stored id.

## Verification

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s web -v   # gateway + Pi adapter
pnpm --dir web/app typecheck && pnpm --dir web/app build
pnpm --dir web/app test:unit                                       # node --test, no browser
pnpm --dir web/app test:e2e                                        # Playwright, needs a browser
```

The Playwright suite (`web/e2e`) starts the **real gateway** on `127.0.0.1:8790` against a
fake `nebula` CLI and synthetic Pi session files (`web/e2e/serve.py`). It covers login →
second project → nonempty structured conversation, rename/stale-response protection,
terminal-originated messages, tool completion, single send / uncertain / failed delivery,
drafts, creation, offline/resume, logout, service-worker and unsafe-Markdown invariants, and
phone/tablet/desktop/landscape layouts with screenshots in `.scratch/mobile-chat-e2e/screenshots/`. Run it on a
machine with a browser (`pnpm --dir web/app exec playwright install chromium` once).
**findl has no browser: those checks stay pending until run from the laptop.**

To inspect a candidate visually from the laptop without touching production, start a
preview on findl on a loopback port with a synthetic token and tunnel it:

```bash
# on findl (any port that is not 8765): synthetic data
E2E_PORT=8790 python3 web/e2e/serve.py
# or real projects, read-only Pi history, still a throwaway token
NEBULA_WEB_TOKEN=<synthetic 32+ chars> NEBULA_WEB_PROJECTS=nebula,logistica,scutulv2,vela-hub-fork \
NEBULA_WEB_PORT=8799 NEBULA_WEB_ORIGIN=http://127.0.0.1:8799 NEBULA_WEB_BINARY=$HOME/.local/bin/nebula \
NEBULA_WEB_DB=$HOME/.local/share/nebula/nebula.db python3 web/server.py
# on the laptop
ssh -N -L 8799:127.0.0.1:8799 findl   # then open http://127.0.0.1:8799
```

The origin must be exactly the tunnelled URL; no production token or port is involved.

## Deploy on findl (promotion checklist)

The live service runs from a read-only, per-commit release outside Git worktrees. See
[the durable web deployment runbook](../docs/operations/web-service-recovery.md) for the
active baseline, verification evidence, path-only systemd configuration and valid rollback.
Build and validate a new committed release before promotion; preserve the existing environment.
Restart only `nebula-web.service`, never the Nebula daemon or the bridge/proxy.

**The token grants agent control as the server's OS user.** Project allowlisting is an API
scope, not an OS sandbox. Rotate a leaked token in the env file and restart only `nebula-web`.

## Delivery semantics and limitations

POSTs are never retried or queued. A timeout or lost connection can happen *after* the daemon
accepted a message or created a session; the UI shows "livrare incertă", keeps the text and
asks you to check history before resending. There is no exactly-once guarantee. The CLI still
selects sessions by name, so the gateway resolves names right before each operation and
refuses duplicates; a concurrent rename remains a CLI-level race. Pi writes its session file
only after the first assistant reply, so a brand-new session shows no persisted messages
until then. Sends while Pi is busy become Pi *steering* messages (delivered after the current
turn's tool calls). Partial assistant text is not available for existing sessions: the UI
shows the daemon's working state and complete messages as Pi persists them.

Rendering never uses raw HTML; links are restricted to http(s)/mailto. A maximum of four CLI
subprocesses run concurrently with 20-second timeouts; Nebula/MC/Pi identity variables and
web secrets are stripped from subprocess environments. Access logs contain no paths, tokens
or content.
