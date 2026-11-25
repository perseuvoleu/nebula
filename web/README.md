# Nebula Mobile (single-user)

A Romanian, mobile-first, black-and-white PWA for controlling the agents that run on the
machine hosting the gateway. React + TypeScript + Vite frontend (`web/app`, shadcn/ui on
Radix, the AI Elements conversation primitive), Python 3.10+ standard-library gateway
(`web/server.py`), no daemon changes. Closing the app never stops server agents.

## What works

- Persistent weekly Codex and Claude account quota navbar on every authenticated screen.
  Tap for reset/freshness/errors; unavailable and stale values are explicit. Sources, caching,
  limitations and private preview instructions: [Weekly account quotas](USAGE.md).
- **Rulări serverless** (`/serverless`, link in the sidebar): build/test runs on Cloudflare
  Containers — repo, ref, status, cache, instance, start, duration (live while running), upper-bound
  cost; tap a row for its steps. Polled every 5 s while visible. The gateway reads the Worker's
  `/runs` with `NEBULA_SERVERLESS_URL` and `NEBULA_SERVERLESS_TOKEN` (the token never reaches the
  browser); without both, the section says it is not configured.

- Explicitly enabled projects (`NEBULA_WEB_PROJECTS`), active sessions with kind, checkout,
  status and feedback requests, existing checkouts, local search.
- **Pi sessions as real conversations**: user/assistant messages, tool activity (collapsible,
  bounded, full output on demand), safe Markdown with copyable code blocks, older-history
  paging, honest "agent working" state. History comes from Pi's persisted session file mapped
  by the native session id Nebula stored — never from terminal screen text. Details and
  limits: `docs/specs/mobile-chat-redesign-pi-source.md`.
- **Claude sessions use their saved transcript** when Nebula has recorded the native session id.
  The initial page contains at most 30 user/assistant text messages (120,000 characters per page,
  40,000 per message). Scrolling upward loads older pages while preserving the reading position;
  polling reads appended records and retains pages already loaded. This is chronological saved
  main-session text, including retained history before compaction, not reconstructed model context.
  Thinking, tool payloads, injected metadata, system records and sidechain/team entries are hidden.
  Raw records larger than 2 MiB become omission notices without parsing their payloads.
  Tool activity and interactive controls remain available through **Arată terminalul**. Claude files
  are resolved only by the stored UUID in the selected checkout under `~/.claude/projects`;
  `NEBULA_WEB_CLAUDE_DIR` (or `CLAUDE_CONFIG_DIR`) overrides the Claude config directory.
- Codex/Cursor agents and Pi/Claude sessions without persisted history show an
  explicitly labelled terminal snapshot, not reconstructed chat. The default **Lectură** view
  renders its Markdown with reflowing prose; **Terminal brut** preserves the terminal text for
  logs, diffs and interactive prompts. Snapshot wrapping can already have split words or
  formatting, so this is not a substitute for native structured history. It starts with 250 lines;
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
  Optional standards-based Web Push alerts when an agent finishes or needs feedback, including
  while the app is closed. Tapping an alert opens that session; logging out unsubscribes the device.
  iPhone: Safari → Share → Add to Home Screen. A visible “Instalează pe telefon” guide is
  available on login and in session navigation; standalone mode, touch icon and HTTPS are supported.
  The installed app may require the same token again; physical iPhone installation/keyboard checks
  remain manual.
- Bearer token in browser **sessionStorage** only, cleared on logout together with drafts and
  subscriptions. No token in URLs, cookies or caches.

Not included: live token streaming (see the Pi source note), archive/restart, file
access, shell creation/endpoints, offline actions, multi-user.

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

Web Push is optional. To enable it, install its pinned protocol dependencies in a
venv and generate one persistent VAPID key (never commit the key):

```bash
python3 -m venv ~/.local/share/nebula/web-venv
~/.local/share/nebula/web-venv/bin/pip install -r web/requirements.txt
umask 077
mkdir -p ~/.config/nebula-web && chmod 700 ~/.config/nebula-web
cd ~/.config/nebula-web
# Only when no private key already exists; never rotate an existing key.
test -e private_key.pem || ~/.local/share/nebula/web-venv/bin/vapid --gen
export NEBULA_WEB_VAPID_PRIVATE_KEY="$HOME/.config/nebula-web/private_key.pem"
# Run server.py with the venv's Python. HTTPS is required on the phone.
```

The notification permission prompt appears only after tapping “Activează notificările”. On
an iPhone or iPad, Web Push requires iOS/iPadOS 16.4+ and the PWA installed on the Home Screen.
Subscriptions are stored mode `0600` in `~/.local/share/nebula/web-push.json`; override with
`NEBULA_WEB_PUSH_STORE`. `NEBULA_WEB_VAPID_SUBJECT` may override the default HTTPS origin.
Supported destinations are `web.push.apple.com`, `fcm.googleapis.com`, and
`updates.push.services.mozilla.com`, over HTTPS port 443. Redirects and environment
proxy/netrc credentials are disabled. Other push providers require a reviewed allowlist update.
The gateway polls enabled projects every five seconds only while at least one device is subscribed.

`NEBULA_WEB_NOTIFY_TOKEN` (optional) lets a server-to-server caller notify every subscribed
device: `POST /api/notify` with `Authorization: Bearer <NEBULA_WEB_NOTIFY_TOKEN>`,
`Content-Type: application/json` and `{"title": "1–100 chars", "body": "1–300 chars", "tag":
"optional, 1–64 chars"}` replies `{"sent": <devices delivered to>}`, or 503 without a VAPID key.
That token opens only this endpoint and the main token does not open it; no `Origin` is needed,
but a browser `Origin` other than `NEBULA_WEB_ORIGIN` is refused. Unset or empty, the endpoint is 401.

Open `http://127.0.0.1:8765` and enter the token. The gateway listens **only on 127.0.0.1**
and refuses to start without a built app. `NEBULA_WEB_PORT`, `NEBULA_WEB_BINARY` (absolute
Nebula binary) and `NEBULA_WEB_PI_AGENT_DIR` (default `~/.pi/agent`) are optional.

The allowlist is configuration, not discovery. On findl use local project names
(`nebula`, not `nebula@findl`).

### Board

`NEBULA_WEB_BOARD_PROJECTS` turns the board on: comma-separated project names, or `*` for
every project. Home cards (no project) are always included. The gateway adds this scope to every
board, card and decision read (`--project=…` or `--all`, plus `--include-home`), whatever the
client asks, and checks every card write against a scoped read first. Unset, the board endpoints
answer 503.

Endpoints: `GET /api/board?since&awaySince` (one `nebula todo board` call per poll),
`GET /api/cards/<ref>`, `GET /api/decisions?query`, `POST /api/cards`,
`POST /api/cards/<ref>/{move,edit,note,archive,accept,dismiss,block,unblock,run,approve,open-code}`
and `POST /api/decisions`. The web publishes decisions only, never a host path. The board CLI's
exit codes map to HTTP: 3 (board host unreachable, nothing sent) → 503, 4 (outcome unknown) → 504,
5 (not found) → 404, 6 (conflict) → 409, anything else → 502. CLI stderr never reaches the client.

`NEBULA_WEB_ARTIFACTS` (absolute directory, the board daemon's `data_dir/artifacts`) lets the
gateway serve artifact files. Each artifact row of a card read gets a `src` of the form
`/a/<expiry>.<HMAC>/<id>/<entry>`: a grant valid for 30–60 minutes, signed with a key that is
random per gateway process, so a restart ends every open grant. `GET /a/…` needs no bearer
(a sandboxed iframe cannot send one), runs no CLI call, refuses symlinks and traversal with 404,
and answers `Content-Security-Policy: sandbox allow-scripts` with nosniff and a MIME type from
the file extension.

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

The real lane runs the same gateway against an isolated daemon built from this checkout, through
the real CLI and its JSON (no fake), and publishes a Playwright reporter run as an artifact:

```bash
heavy .agents/skills/verify-nebula/vn build                        # target/verify-nebula/debug/nebula
pnpm --dir web/app test:e2e board.spec.ts board-artifacts.spec.ts  # a reporter run with ≥ 12 screenshots
pnpm --dir web/app test:e2e --project real                         # 127.0.0.1:8791
```

`web/e2e/serve.py --real` starts the daemon under `$REAL_NEBULA_STATE` (default `/tmp/nre-<checkout hash>`)
with `board = "local"` and the replay agent as its agent command, then the gateway with
`NEBULA_WEB_BINARY=web/e2e/real_nebula.sh`. The gateway strips every `NEBULA_*` variable from its CLI calls,
so the launcher re-exports `NEBULA_RUNTIME_DIR` and `NEBULA_DATA_DIR` from `REAL_NEBULA_STATE` and sets
`NEBULA_RAG_LOCAL=0 NEBULA_RAG_HOST=nebula-real-e2e.invalid`, so a decision never reaches the real RAG.
Any gateway aimed at a non-default daemon needs such a launcher. `REAL_NEBULA_BIN` picks another binary,
`REAL_E2E_RUN` another reporter run (this lane's own: `.scratch/real-e2e/e2e-run`).

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

The live service runs from a read-only, committed release under
`~/.local/share/nebula/web-releases/<sha>`, never from a disposable worktree.
See [Web Push activation and rollback](../docs/operations/web-push-activation.md) for the
current deployment, and [the durable web recovery runbook](../docs/operations/web-service-recovery.md)
for the earlier baseline and recovery evidence.
Build and validate a new committed release before promotion. Preserve existing unit hardening
and environment entries; change only deployment paths and necessary push configuration.
Restart only `nebula-web.service`; never restart the Nebula daemon, bridge or proxy.

## Deploy on leaseweb

leaseweb serves code.possu.dev from the same per-SHA release layout under
`~/.local/share/nebula/web-releases/<sha>`, run by the venv `~/.local/share/nebula/web-venv`
(`web/requirements.txt`) and the `nebula-web.service` user unit with a
`nebula-web.service.d/90-durable-release.conf` override naming the release. The gateway listens
on 127.0.0.1:8765 only. Since 2026-11-24 the `cloudflared-leaseweb` tunnel carries
code.possu.dev to it behind Cloudflare Access, and no inbound port opens. `NEBULA_WEB_PROJECTS` lists only projects
leaseweb's daemon knows. `NEBULA_WEB_BINARY` points at `web-releases/client-<sha>/bin/nebula`,
built from the same commit.

**Rollback:** findl's `nebula-web.service` is stopped and disabled, still pinned by its drop-in to
release 3b6189c. On findl, run `systemctl --user enable --now nebula-web` (enable keeps it up
across a reboot) and check that
`curl -s -o /dev/null -w '%{http_code}' -H 'Host: code.possu.dev' http://127.0.0.1:8765/` prints
200. Only then repoint the code.possu.dev public hostname in Cloudflare back to findl's tunnel:
service `https://127.0.0.1:443` (Traefik), origin server name `code.possu.dev`, No TLS Verify
on, HTTP/2 origin on. The leaseweb route it replaces is `http://127.0.0.1:8765`.

**The token grants agent control as the server's OS user.** Project allowlisting is an API
scope, not an OS sandbox. Rotate a leaked token in the env file and restart only `nebula-web`.

## Board rollout (protocol 33, migration 29)

Order: leaseweb, then the laptops, then findl. `nebula kill` stops every session on its host, so
each one waits for the user's go. Build every host from the same commit `<sha>` (the merged
agent-home release on main). Facts measured 2026-11-25: leaseweb runs `~/.local/bin/nebula`
0.6.0 (b7dfd64), has no zig and no `config.json`; findl runs a hand-built `~/.local/bin/nebula`
that has `rag` and no zig (never `nebula upgrade` there: the published build drops `rag`); the
laptop runs `~/.cargo/bin/nebula`. A daemon migrated to 29 cannot be read by an older binary, so
every host keeps a copy of its database from before the upgrade.

### 1. leaseweb becomes the board host

Any CLI call starts a daemon when none runs, the gateway's polls included, so the gateway stops
first and the database is copied online before the kill. `backup` below is
`python3 -c 'import sqlite3,sys; sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True).backup(sqlite3.connect(sys.argv[2]))'`
(leaseweb and findl have no `sqlite3` binary).

```bash
git -C ~/nebula fetch && git -C ~/nebula worktree add ~/nebula-release-33 <sha>
cp -p ~/.local/bin/nebula ~/.local/bin/nebula.pre-33
cd ~/nebula-release-33 && heavy make install-novt PREFIX=$HOME/.local/bin
systemctl --user stop nebula-web
backup ~/.local/share/nebula/nebula.db ~/.local/share/nebula/nebula.db.pre-33
printf '{"board": "local"}\n' > ~/.local/share/nebula/config.json
nebula kill                                                       # user's go
nebula todo board --json | head -c 300                            # starts the new daemon; answers
mkdir -p -m 700 ~/.local/share/nebula/artifacts
```

Projects: the board host serves every project findl's `NEBULA_WEB_PROJECTS` listed (14 on
2026-11-25; leaseweb's daemon knew 4 of them). Each missing one is cloned under
`~/Projects/<name>` from the origin findl uses, which needs the same deploy-key alias in leaseweb's
`~/.ssh/config` that findl's has. A checkout with no origin is copied through the laptop
(`scp -3 -r findl:<dir> leaseweb:Projects/`), since findl and leaseweb cannot ssh to each other.
Then `nebula add ~/Projects/<name>` for each.

Re-add the legacy rows: `nebula artifact add https://vela.possu.dev --project vela-hub-fork
--title vela` (the findl link); the laptop note only if the user wants it.

Gateway: a release at `<sha>` built the way ticket 01 did (`git archive` on the laptop, then
`prep-leaseweb.sh <sha>` on leaseweb, which also builds `web-releases/client-<sha>/bin/nebula`),
then in `~/.config/nebula-web.env` (keep a copy as `nebula-web.env.bak-pre-33`):

```bash
NEBULA_WEB_PROJECTS=<findl's 14>
NEBULA_WEB_BOARD_PROJECTS=<the same 14>
NEBULA_WEB_ARTIFACTS=/home/<user>/.local/share/nebula/artifacts
NEBULA_WEB_BINARY=/home/<user>/.local/share/nebula/web-releases/client-<sha>/bin/nebula
```

Point `nebula-web.service.d/90-durable-release.conf` at `web-releases/<sha>`, then
`systemctl --user daemon-reload && systemctl --user start nebula-web`. Check:
`nebula --version` names `<sha>`; `curl -s -H 'Host: code.possu.dev' -H "Authorization: Bearer $T"
http://127.0.0.1:8765/api/board` answers 200 with `cards`.

Decisions are copied to the RAG on findl over ssh. leaseweb → findl ssh fails today on host key
verification, so the copy retries until leaseweb trusts findl (an `ssh findl true` that exits 0).

### 2. The laptops

Quit every `ng` window first: a TUI restarts the daemon it lost.

```bash
D="$HOME/Library/Application Support/dev.nebula.nebula"
cp -p ~/.cargo/bin/nebula ~/.cargo/bin/nebula.pre-33
git -C ~/Projects/nebula worktree add ~/nebula-release-33 <sha>
cd ~/nebula-release-33 && heavy make install
sqlite3 "$D/nebula.db" ".backup '$D/nebula.db.pre-33'"
python3 -c 'import json,sys; p=sys.argv[1]; c=json.load(open(p)); c["board"]="leaseweb"; json.dump(c, open(p,"w"), indent=2)' "$D/config.json"
nebula kill                                                           # user's go
nebula todo add --project nebula Rollout check                        # prints #<n>@leaseweb
```

The board link is `ssh leaseweb` in BatchMode, so `ssh -o BatchMode=yes leaseweb true` must exit 0
first (it does from the main laptop). Run on the laptop, approve and open in code from the web
need the reverse: Remote Login on the laptop and a `Host <laptop>` entry with its `User` in
leaseweb's `~/.ssh/config`, checked with `ssh leaseweb ssh -o BatchMode=yes <laptop> true`.

### 3. findl

findl cannot reach leaseweb on port 22 (its public and its tailnet address both time out,
measured 2026-11-25), so `board = "leaseweb"` waits for a path (a Tailscale ACL or Tailscale
SSH, plus a `Host leaseweb` entry). Then, building beside the dirty `~/nebula` checkout:

```bash
git -C ~/nebula fetch && git -C ~/nebula worktree add ~/nebula-release-33 <sha>
cp -p ~/.local/bin/nebula ~/.local/bin/nebula.pre-33
cd ~/nebula-release-33 && heavy make install-novt PREFIX=$HOME/.local/bin && nebula rag --help >/dev/null
backup ~/.local/share/nebula/nebula.db ~/.local/share/nebula/nebula.db.pre-33
python3 -c 'import json,sys; p=sys.argv[1]; c=json.load(open(p)); c["board"]="leaseweb"; json.dump(c, open(p,"w"), indent=2)' ~/.local/share/nebula/config.json
nebula kill                                                           # user's go
nebula todo board --json | head -c 300
```

### Rollback

Per host, newest first: on leaseweb stop `nebula-web` first; `nebula kill`, restore the binary
(`mv nebula.pre-33 nebula` in its directory), delete `nebula.db-wal` and `nebula.db-shm`, move
`nebula.db.pre-33` over `nebula.db`, delete the `board` key from
`config.json` (leaseweb: delete the file), then start the daemon (`nebula agent list --all`). On
leaseweb also restore `nebula-web.env.bak-pre-33`, point the drop-in back at release
1df2ca59b7aaea1fc536811c6dacc04c1b17b69f, `systemctl --user daemon-reload` and start `nebula-web`. Cards, artifacts and
decisions made after the upgrade are lost with the database copy.

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

To verify frontend changes without replacing a running checkout's `web/static`:

```bash
pnpm --dir web/app typecheck
pnpm --dir web/app exec vite build --outDir ../../.scratch/mobile-reading-static
E2E_STATIC_DIR="$PWD/.scratch/mobile-reading-static" pnpm --dir web/app test:e2e
```
