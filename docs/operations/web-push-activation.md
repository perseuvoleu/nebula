# Web Push activation on findl

Scope: STEP2 only, based on `d07c37dd9fa4b3ff82f1b2c8d8d635e96f1d520e`.
Preview identity and Vela are separate work. No browser runtime is installed on findl.

## Release policy and rollback

The verified rollback source is `947493351c490e1fa31ac98f1316d809a17d2acd`, at
`/home/andrei/.local/share/nebula/web-releases/947493351c490e1fa31ac98f1316d809a17d2acd`.
The base unit is `/home/andrei/.config/systemd/user/nebula-web.service`; the deployment
override is `nebula-web.service.d/90-durable-release.conf` beside it. Preserve both and
`/home/andrei/.config/nebula-web.env` before activation. Never deploy from a worktree.

Private backups and capped evidence live in this task's git-ignored
`.scratch/web-push-activation/` (0700, backup files 0600). The saved `unit.backup`,
`override.backup` and `env.backup` restore the verified STEP1 release, not the deleted
historical checkout. To roll back from this task checkout:

```bash
install -m 600 .scratch/web-push-activation/unit.backup ~/.config/systemd/user/nebula-web.service
install -m 600 .scratch/web-push-activation/override.backup ~/.config/systemd/user/nebula-web.service.d/90-durable-release.conf
install -m 600 .scratch/web-push-activation/env.backup ~/.config/nebula-web.env
systemctl --user daemon-reload
systemctl --user restart nebula-web.service
```

Retain the VAPID key and subscription store across rollback. Do not regenerate keys or
restore an older subscription store. Recheck service cwd, trusted HTTPS assets and
anonymous/authenticated API behavior. Do not restart agents, daemon, proxy or containers.

## Review and verification contract

Browser `PushSubscription.toJSON()` includes `expirationTime`; the gateway accepts it
and persists only endpoint/keys. Destinations are restricted to the known Apple, Google
and Mozilla provider hosts on HTTPS port 443; DNS and TLS for those providers remain
trusted. No arbitrary hosts, alternate ports, credentials, control characters or redirects
are accepted. The HTTP transport ignores proxy and netrc environment credentials.
Subscriptions are atomically replaced using a private temporary file. New store directories
are 0700 and files 0600; existing shared parent directories are not chmodded by the app.

Authentication and exact Origin checks apply before push API writes. The web bearer token
never enters the push payload; CLI subprocess environments strip NEBULA/MC/PI values.
Payloads contain the agent name/status and a session route; those appear on the lock screen.
The daemon-backed `agent list` lifecycle is the state source. An initial snapshot does not
notify; subsequent transitions to finished/needs_feedback do. Five-second polling can miss
states shorter than a poll interval; sending is best effort with no durable retry queue.
Expired (404/410) subscriptions are removed. No registered devices means no polling.
Service-worker click routes stay on the app origin; optional badging failure cannot block
navigation. Browser and physical-device behavior still need runtime verification.

Run backend tests using the dedicated venv and frontend Node tests, typecheck and build
serially with `nice -n 15 taskset -c 0`, Node heap capped at 1536 MiB. Capture bounded logs.
Export committed `web/` source into a new durable per-SHA directory, build there, verify
source hashes, and make the release read-only. Test the real candidate over loopback with
synthetic token/key and isolated store before promotion. Verify missing dependencies fail
closed, absent key disables push, authenticated `/api/push` returns a valid public key,
HTTPS assets match the release, key stability across restart and process/container continuity.
Mocks are never evidence of device delivery.

## Runtime layout

Dedicated venv: `/home/andrei/.local/share/nebula/web-push-venv` (private root).
Dependencies are pinned in `web/requirements.txt`. On this host ensurepip is absent;
PyPA's official get-pip bootstrap was run only inside the dedicated venv. No system Python
packages changed. A persistent key is provisioned only if absent, under the private
`/home/andrei/.config/nebula-web/` directory. Never print keys, tokens or subscription URLs.
The only required new environment entry is `NEBULA_WEB_VAPID_PRIVATE_KEY`; existing token,
projects, origin, DB, preview/global binary, flags and hardening must remain unchanged.

## Physical acceptance (pending)

On iOS 16.4+, install `https://code.possu.dev` through Safari's Add to Home Screen. Open
that installed PWA, log in, tap **Activează notificările** (Activate Notifications), and
grant permission. Close the app, then arrange a controlled agent finish and needs_feedback
transition. The user must confirm both physical notification receipt and correct session
navigation after tapping it. No subscription or OS permission is created on the user's behalf.
Desktop browser tests are also pending until actually run on a machine with a browser.

Primary references checked September 14, 2026 (Context7 unavailable in this session):
- [WebKit iOS 16.4 Web Push](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados/)
- [MDN subscription JSON](https://developer.mozilla.org/en-US/docs/Web/API/PushSubscription/toJSON)
- [pywebpush 2.3.0 source](https://github.com/web-push-libs/pywebpush/blob/2.3.0/pywebpush/__init__.py)
- [PyPA supported bootstrap](https://pip.pypa.io/en/stable/installation/)
