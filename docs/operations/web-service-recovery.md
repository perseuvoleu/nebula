# Durable web service deployment on findl

Recovered on 2026-09-14 from a running process whose working directory was
`/home/andrei/nebula-worktrees/mobile-web-deploy (deleted)`. This repair deliberately
retains baseline `947493351c490e1fa31ac98f1316d809a17d2acd`; it does not deploy primary
`d07c37d`, activate push/VAPID, or repair preview identity.

## Active layout

- Release: `/home/andrei/.local/share/nebula/web-releases/947493351c490e1fa31ac98f1316d809a17d2acd`
- Base unit: `/home/andrei/.config/systemd/user/nebula-web.service`
- Override: `/home/andrei/.config/systemd/user/nebula-web.service.d/90-durable-release.conf`
- Existing environment file: `/home/andrei/.config/nebula-web.env`

The override contains only:

```ini
[Service]
WorkingDirectory=/home/andrei/.local/share/nebula/web-releases/947493351c490e1fa31ac98f1316d809a17d2acd
ExecStart=
ExecStart=/usr/bin/python3 /home/andrei/.local/share/nebula/web-releases/947493351c490e1fa31ac98f1316d809a17d2acd/web/server.py
```

The base unit still records historical worktree paths, so retain the override. All other
unit settings and environment entries are inherited unchanged, including token, origin,
projects, database, global/preview binaries, ports and hardening. Neither bridge nor proxy
configuration changed. The deployment uses an explicit release path, with no mutable
`current` symlink. Do not edit or delete an active or rollback release.

The release contains only committed `web/` source exported with `git archive`, generated
frontend output and build metadata. No `.env*` files were copied. Every tracked source file
was compared byte-for-byte with the commit. `release.json` records the full commit and
SHA-256 hashes of files. Build-only `node_modules` was removed; release files/directories
were made owner-readable, executable where required, and non-writable. This is a durable
artifact, not scratch or a worktree. A future deployment must create a separate release,
never update this one in place or source it from an uncommitted checkout.

## Verification performed on findl

Resources before build: 10,887 MiB available RAM, 260 GiB available disk. Build commands
ran serially with `nice -n 15 taskset -c 0`; Node heap was capped at 1,536 MiB.
Node 22.23.2 and pnpm 10.14.0 were used.

1. `pnpm --dir <release>/web/app install --frozen-lockfile --child-concurrency=1 --network-concurrency=2`: passed.
2. `pnpm --dir <release>/web/app build`: TypeScript and Vite passed.
3. `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s <release>/web -v`: 37 tests passed.
4. `pnpm --dir <release>/web/app test:unit`: 6 tests passed.
5. Started the real release server on an unused loopback port with a random synthetic token
   and loopback origin, otherwise retaining the existing process environment. Checked HTML,
   JS and CSS against release bytes, four anonymous sensitive API denials (401), authenticated
   projects and sessions for all four configured projects. Repeated after making release read-only.
6. Prepared a standalone rollback unit pointing to the verified release before restart.
   Installed the override, ran user `daemon-reload`, inspected resolved paths, then restarted
   only `nebula-web.service`. Service became active with PID 2004996; `/proc/2004996/cwd`
   resolved to the existing release directory. This proves startup from the committed artifact.
7. HTTPS `https://code.possu.dev` returned HTML and both actual JS/CSS assets, byte-identical
   to the release. Anonymous projects/sessions/output/preview requests returned 401.
   Authenticated projects and sessions reads passed (four projects; session counts 4, 0, 2, 4).
   The existing token was read only into verification process memory from `/proc/<pid>/environ`
   and sent only to the exact configured HTTPS origin, with redirects and proxy environment
   disabled, normal TLS verification, and no secret values in argv or output.
8. Hashed comparison of the complete before/after process environment matched, excluding
   systemd invocation IDs, execution PID and journal stream metadata.
9. All 21 original running containers retained their full IDs and start timestamps. All
   tracked Pi/Claude/Codex processes and both long-running Nebula processes retained PID/start
   identity. One additional short-lived Nebula PID exited between snapshots; no daemon or
   agent restart command was issued.

`/api/preview?project=nebula` returned `{"url":null,"phase":null}`. The previously reported
preview identity mismatch is a separate known issue; this response alone does not diagnose
it. No identity or preview policy was changed. Desktop browser verification remains with
the orchestrator; no browser was installed on findl.

## Evidence and rollback

Private evidence and backups are in the project’s git-ignored
`.scratch/web-service-recovery/` (bootstrap links `.scratch` to the primary project).
Directory mode is 0700 and backups are 0600. Do not print backup contents: they can contain
credentials. Logs are byte-capped at 192 KiB each. Files include `install.log`, `build.log`,
`server-tests.log`, `unit-tests.log`, `candidate-check.json`, before/after continuity JSON,
`after-summary.txt`, the original user units and environment backup, and
`rollback-valid.service`.

The original unit backup references a deleted worktree and **is not a runnable rollback**.
`rollback-valid.service` preserves the original base unit with only its two deployment paths
corrected to the verified baseline release. If the override must be undone, from this task
checkout, restore that valid unit and remove only this task's override:

```bash
install -m 600 .scratch/web-service-recovery/rollback-valid.service "$HOME/.config/systemd/user/nebula-web.service"
rm "$HOME/.config/systemd/user/nebula-web.service.d/90-durable-release.conf"
systemctl --user daemon-reload
systemctl --user restart nebula-web.service
```

Then repeat the active-state, existing cwd, static-asset, anonymous-denial and authenticated
read checks. The environment file needs no restoration or rotation. This rollback keeps the
same verified application baseline; for a future application promotion retain this release
and repoint the path-only override to it. Never restore the deleted worktree path. Do not
restart the daemon, bridge, proxy, containers or host, and do not sync/collect or push protected
branches as part of this procedure. Push activation is a separately coordinated next step.
