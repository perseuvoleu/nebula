# Nebula Mobile (single-user MVP)

A Romanian, mobile-first PWA for controlling agents on the machine running the
web gateway. Python 3.10+ standard library only; no Node build or changes to the
Rust daemon. The browser polls every 2.5 seconds while visible and reconnects
when brought back to the foreground. Closing it does not stop server agents.

## What works

- Explicitly enabled projects, active sessions, kinds/statuses, existing checkouts.
- Last 250 lines of rendered terminal output (plain text, **not** parsed chat history).
- Send a prompt; start pi, Claude, Codex or Cursor on an existing checkout.
- Installable app shell, safe-area layout, per-session in-memory message drafts.
- A bearer token held in browser **sessionStorage**, cleared on logout. Re-enter it
  if the browser discards the session. No token in URLs, cookies or service-worker cache.

No agent archive/delete/restart, file access, shell endpoint, automatic worktree
creation, notifications, offline actions, multi-user permissions or chat parsing.
The server must already have Nebula, registered projects, agent CLIs and their logins.
A session already running on a checkout is shown in the creation picker: avoid
starting concurrent writers on the same branch.

## Local start

Run from the repository root under the same OS user as the Nebula daemon:

```bash
export NEBULA_WEB_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
export NEBULA_WEB_PROJECTS='nebula' # comma-separated exact Nebula project selectors
export NEBULA_WEB_ORIGIN='http://127.0.0.1:8765'
python3 web/server.py
```

Open `http://127.0.0.1:8765` and enter the generated token from your shell environment.
Keep it private. The server never prints it. The gateway listens **only on 127.0.0.1**.
`NEBULA_WEB_PORT` changes the default port; `NEBULA_WEB_BINARY` can specify the
absolute installed Nebula binary. Configure the origin with no trailing slash.

The allowlist is deliberately configuration, not auto-discovery: do not expose all
registered projects by accident. On findl use local project names, e.g. `nebula`,
not the laptop's `nebula@findl` remote selector.

## Deploy on findl

1. Move the committed implementation using the repository's `nebula sync` workflow.
   Resolve dirty-checkout/ownership refusals with the owner first; do not overwrite
   the checkout or restart the daemon. This gateway does not need a daemon restart.
2. Choose a dedicated HTTPS hostname and route it to findl using the existing
   reverse proxy. Forward to `127.0.0.1:8765`, **preserving the public Host and Origin**.
   If the proxy is containerized, its loopback is not the host loopback: use an
   explicitly reviewed host-network/private forwarding setup, not a public bind.
3. Keep the Python gateway private behind the proxy (it is not an Internet-facing
   production HTTP server). Configure proxy request body limits (100 KB), connection
   limits, timeouts (at least 30 seconds upstream), authentication rate limiting,
   HTTPS redirects and HSTS. Do not cache `/api/*` or log Authorization headers.
   Prefer an additional identity gate or private VPN for a server with valuable credentials.
4. Create `~/.config/nebula-web.env` with permissions `0600`:

   ```ini
   NEBULA_WEB_TOKEN=<random token generated with secrets.token_urlsafe(32)>
   NEBULA_WEB_PROJECTS=nebula
   NEBULA_WEB_ORIGIN=https://<chosen-hostname>
   NEBULA_WEB_BINARY=/absolute/path/from-command-v-nebula
   ```

5. Install `nebula-web.service` below as a **user service**, adjusting its working
   directory if necessary. Ensure its PATH includes installed agent binaries.

   ```bash
   mkdir -p ~/.config/systemd/user
   cp web/nebula-web.service ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now nebula-web
   journalctl --user -u nebula-web
   ```

   The user manager must survive logout (check `loginctl show-user "$USER" -p Linger`;
   arrange lingering with the server administrator if not already enabled).
6. On the phone, open the HTTPS URL, authenticate, then use Safari → Share →
   Add to Home Screen, or the browser's Install App action on Android.

**The token grants agent control as the server's OS user, not read-only access.**
Project allowlisting is an API scope, not an OS sandbox: an instructed agent may
access anything its OS user can. Rotate a leaked token in the env file and restart
only `nebula-web`; do not run `nebula kill`. Never share this app as a multi-user service.

## Delivery semantics and limitations

POSTs are never retried or queued automatically. A timeout/disconnect can happen
*after* the daemon accepts a message or creates a session. The draft remains and a
warning tells you to inspect sessions/output before retrying; there is no exactly-once
guarantee. Starting an agent gives it a unique initial `Mobile …` name; it may rename
itself afterwards. The UI identifies sessions by id, but the current Nebula CLI
selects by name: the gateway resolves names immediately before each operation and
refuses duplicates (including archived duplicates). A concurrent rename can still
make an operation fail, and name reuse between lookup and command is a remaining
CLI-level race. A future native id-addressed daemon API should remove this seam.

Output and titles are rendered with `textContent`, never HTML. A maximum of four
CLI subprocesses runs concurrently, with 20-second timeouts. The server strips
inherited Nebula/MC/Pi identity variables and web secrets from subprocess environments.
Polling is paused in the background. The service worker caches only the public app
shell, never outputs or messages. Offline mode offers no agent functionality.

## Verification

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s web -v
node --check web/static/app.js
node --check web/static/sw.js
node --test web/test_app.cjs
```

Tests exercise HTTP authentication, exact Host/Origin checks, project scope,
request validation, cache policy, static assets, session ambiguity, subprocess
argument boundaries, environment stripping and checkout selector resolution.
They mock mutating CLI operations: no real agents are spawned or sent prompts.
The JavaScript regression tests execute the actual app script with a minimal DOM
and mocked API, checking selected-session output, repeated polling and stale-response
suppression. They cover the `data` shadowing bug that previously left output stuck
on “Se încarcă…” before the browser could even request `/api/output`.

Before production use, verify on real iOS/Android: install, login/logout, switch
sessions while polling, background/resume, loss of network during send, creation
on an idle checkout and token rotation. Syntax/HTTP tests do not replace those
browser/device checks.
