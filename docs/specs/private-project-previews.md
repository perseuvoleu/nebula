# Architecture — Private project previews

Status: user agreed to the product direction; implementation and deployment pending.

## Problem & goals

The owner should ask an agent on findl, including through the mobile UI at
`code.possu.dev`, to serve a selected project worktree at a stable project domain
such as `nebula.possu.dev`. The application runs on findl; inspection happens in
the owner's desktop or mobile browser without copying the checkout locally.
Applications start explicitly through the CLI, not on incoming browser requests.
Access is private to the owner; shared/public links are outside the first version.

## Approaches considered

- Agent-started previews: chosen; explicit lifecycle and straightforward failures.
- Wake-on-request: deferred; requires additional lifecycle and waiting-page logic.
- Separate server utility versus native CLI: native `nebula preview` chosen because
  Nebula already identifies projects, checkouts and agent sessions.

## Recommended approach

Add a host-local CLI preview capability on findl. Reuse the existing Traefik ingress
and HTTPS infrastructure; leave `code.possu.dev` and production routes untouched.
A project has a stable domain and at most one active target worktree. An agent can
build, run, inspect and stop its preview independently of the mobile connection or
local desktop daemon. Existing mobile agent messaging is the control path; no new
mobile shell API is needed.

The following command surface is proposed, not currently implemented:

```
nebula preview up
nebula preview status
nebula preview logs
nebula preview restart
nebula preview down
```

## Key decisions

- **Identity:** project + host owns the domain; a preview target records canonical
  checkout identity, branch/commit, owning agent, managed services and lifecycle state.
  Branch name alone is insufficient to identify a worktree. Uncommitted work may
  be served and should be reported as dirty, not represented as an immutable build.
- **Lifecycle:** explicit start/stop first. Closing the browser must not stop the
  preview. Automatic expiry is a possible follow-up, not a decided timeout policy.
  Stop preserves test data; deletion requires a separate explicit operation.
- **Agent capability:** support project-specific build/start commands, frontend,
  backend and test dependencies, bounded log inspection and restart. Command
  configuration is executable code under the server user, not untrusted data.
- **Concurrency:** serialize mutations per project, refuse implicit takeover, and
  require an explicit replacement operation when another agent owns the preview.
  Never switch or reset another agent's checkout. Validate candidate health before
  switching the route; retain the previous healthy target when promotion fails.
- **Privilege:** use a narrowly scoped route-management boundary, not arbitrary
  root shell commands or arbitrary proxy configuration supplied by an agent.
  Avoid unrelated Docker/production operations. CLI guardrails alone do not sandbox
  agents: the current findl SSH user has passwordless sudo. Genuine isolation would
  require separate OS identities/permissions; do not claim that the CLI removes
  existing authority.
- **Privacy:** authenticate before serving any preview content, API or WebSocket
  handshake. TLS alone is not authentication. Internal service ports must not
  provide an externally reachable bypass. Use test secrets/data, not production.
- **Authentication separation:** never pass the mobile agent-control token to
  preview applications or use a broadly scoped sibling-domain control cookie.
  The exact preview authentication mechanism remains to be selected and tested.
- **Runtime:** process supervision must survive CLI exit and browser disconnect;
  cleanup must target only managed preview processes. Select concrete supervision,
  state storage and service recipe format during implementation design.

## Existing infrastructure verified

Read-only inspection of findl found Traefik v3.6.7 and Dokploy v0.30.6.
`/etc/dokploy/traefik/dynamic/nebula-web.yml` routes `code.possu.dev` over HTTPS
with the `letsencrypt` resolver to a private bridge at `172.18.0.1:8766`.
The host has `nebula-web-bridge.socket` and a user `nebula-web.service`.
Production application and database containers also run on this host: preserve them.

`web/README.md` documents the current mobile gateway's sessionStorage bearer-token
authentication. It is application-specific and does not automatically protect
sibling preview domains. DNS availability for new project domains is not verified.

## Missing pieces

CLI operations, project runtime recipes, supervised process ownership, persistent
preview state, port allocation, scoped route publishing, owner authentication and
new-domain DNS/TLS provisioning. No changes have been deployed for this feature.

## Spikes & experiments

Before deploying a real project, test one disposable private preview:

- Anonymous HTTP, API and WebSocket requests are denied, including alternate/direct
  ingress paths; the owner's desktop and mobile browsers can authenticate.
- Frontend hot reload, API routing and WebSockets work behind the authentication gate.
- A failed candidate leaves the previous preview available; concurrent takeover is
  refused; stopping the preview leaves agent sessions and unrelated services intact.
- Preview remains available after closing the CLI/mobile connection.

Server health checks are not browser verification. Browser tests run on the laptop;
findl has no browser and must not receive one without explicit approval. Report
physical-mobile verification separately from desktop mobile emulation.

## Open questions

- Preview-only authentication implementation and credential provisioning.
- Runtime recipe and supervision choice; per-project service/resource limits.
- Domain allowlist/DNS ownership and safe privileged route-helper integration.
- First project/worktree for the end-to-end spike.

Remote deployment must follow the hybrid-work runbook and must not push protected
main/master refs or restart the host Nebula daemon and its active agent sessions.

---

# Implementation contract (v1)

The original architecture above is retained verbatim. Deployment remains gated on
orchestrator review; no live DNS, ingress, firewall, sudoers, service restart or
global binary replacement is part of implementation.

## Decisions and public test seams

- Native host-local `preview` command, no daemon RPC or session lifecycle changes.
- Root-owned host policy maps a short project name to its canonical Git common
  directory, exact domain, and two fixed gateway ports. Recipe `.nebula-preview.json`
  belongs to the selected worktree. Explicit `--owner` records agent ownership;
  `--replace` is required for another owner or checkout, even if the runtime is stale.
- Docker restart policies supervise containers after CLI exit and host reboot.
  Per-candidate internal networks have no externally published application ports.
  Only a trusted Nginx gateway publishes a fixed policy port on the private bridge;
  disposable fixtures bind loopback. No production network or environment inheritance.
- Preview-only Nginx Basic Auth gates every path and WebSocket upgrade. Exact Host
  matching; credentials are stripped upstream. The root publisher emits a dedicated
  Traefik HTTPS router; Nginx is the sole mandatory authentication gate.
  Basic Auth reserves `Authorization`: bearer-auth applications are unsupported.
  Browser credential caching/logout and physical-mobile HMR need laptop verification.
- Runtime source is a private snapshot of Git tracked and nonignored files, excluding
  dotenv files and `.git`, `.scratch`, dependency/build directories. Symlinks are
  rejected. No dotenv bootstrap, host env, arbitrary host volumes or Docker socket
  is passed into services. Recipes and Dockerfiles are executable agent code, not
  an OS sandbox; existing passwordless sudo/Docker authority remains unchanged.
- Candidate health precedes atomic route publication. Durable transaction state and
  the published slot reconcile an interrupted promotion on the next command.
  Old runtime survives failed candidates. Project locks refuse concurrent mutations.
  Scoped cleanup uses exact names and ownership labels; volumes survive `down`.
- Scoped publisher accepts only project + `a|b|down|status`, reads a fixed root policy,
  and derives all paths/hosts/ports itself. No caller-supplied proxy fragments or paths.
  Offline fixture mode writes an artifact under state only and never invokes publisher.
- Tests use the real CLI, disposable Docker services and HTTP/WebSocket boundary:
  fail-closed configuration/injection/symlink rejection; lifecycle and restart after
  CLI exit; failed promotion rollback; ownership/takeover and concurrent lock;
  interrupted/stale runtime recovery; scoped stop/data preservation; anonymous,
  invalid-credential and authenticated HTML/API/WS; direct-path authentication and
  network isolation. No production env/DB and no browser installed on findl.

Implementation details, exact verification and deployment/rollback steps follow
as slices are completed.

## Recipe shape

A `.nebula-preview.json` in the checkout describes test services. Commands are
argv arrays (use an explicit shell if needed); unknown fields fail closed. A
service uses an existing image or builds the named Dockerfile from the sanitized
snapshot with `docker build --network=none`. Dockerfile services run their built
image filesystem, preserving compiled artifacts under `/app`; use `COPY . /app`
or the appropriate build stages in the Dockerfile. Prepared-image services mount
the snapshot read-only at `/app`, so put prepared dependencies outside that path
and configure their lookup explicitly. Provision dependency-bearing images
separately: package installs needing outbound access cannot run on the isolated
runtime network. No host environment is inherited. `environment` is a required
map of **test-only** settings, never production credentials. Recipes containing
secrets must be untracked and mode 0600; source snapshots omit the recipe itself.

```json
{
  "services": {
    "web": {
      "image": "your-prepared-preview-image:reviewed",
      "dockerfile": null,
      "command": ["npm", "run", "dev", "--", "--host", "0.0.0.0"],
      "environment": {"NODE_ENV": "development"},
      "port": 3000,
      "health": {"kind": "http", "path": "/"},
      "user": "1000:1000",
      "data": {"cache": "/app/.cache"}
    }
  },
  "routes": {"/": "web"},
  "health_timeout_seconds": 60
}
```

`routes` maps literal path prefixes to services (for example `/api/` to `api`).
All paths retain their original URI; the application handles its own prefix.
Each service has a nonzero port and an explicit health probe: either
`{"kind":"http","path":"/health"}` (strict 2xx, no redirects) or
`{"kind":"command","argv":["pg_isready","-U","preview"]}`. Command probes run
inside that service with its explicit environment; its image must provide
`timeout`. Credentials belong in the environment, never in the command array.
At most eight services and sixteen route prefixes are accepted. Every service is
checked. Resource limits are currently fixed per service: one CPU, 1 GiB RAM and
256 processes; the gateway gets half a CPU, 128 MiB and 64 processes.

Writable named data volumes are per project, slot and service. New volumes start
empty with the service's numeric owner; Docker image-to-volume copying is disabled
to prevent hidden seed data and ownership changes. A failed candidate
cannot mutate the active slot's data. Slot data survives `down`; the next use of
that slot reuses its test data, without copying data from the other slot. There is
no production migration or shared-data contract. Image build and runtime logs
must never print credentials; `logs` is explicit and bounded, and stdout/stderr
may contain application-provided sensitive text despite CLI redaction of config.

The source snapshot records checkout path, Git common directory, branch, commit,
and dirty state. It is a point-in-time working copy, not an immutable Git build;
concurrent editing while snapshotting is not transactional. HMR transport is
supported, but subsequent checkout edits require `restart` to refresh the snapshot.
Live host source mounts are deliberately outside v1 to avoid seeded env exposure.

## CLI and persisted state

```
/opt/nebula-preview/nebula preview up --owner <stable-agent-id>
/opt/nebula-preview/nebula preview --project nebula status
/opt/nebula-preview/nebula preview --project nebula logs --service web --lines 100
/opt/nebula-preview/nebula preview --project nebula logs --candidate --service web
/opt/nebula-preview/nebula preview --project nebula restart --owner <stable-agent-id>
/opt/nebula-preview/nebula preview --project nebula down --owner <stable-agent-id>
```

Use `up --checkout /absolute/worktree --owner <id> --replace` for explicit
replacement of another checkout/owner. `restart --replace` takes ownership of the
recorded checkout; it never selects the caller's other checkout. `--project` is
inferred only when exactly one policy entry matches the caller's canonical Git
common directory. No daemon lookup or new protocol is needed. The stable ID is
an ownership guardrail supplied by the agent, not a cryptographically verified
identity. Server-user sudo access is unchanged.

State is below `<state_dir>/live/<namespace>/<project>/`; fixture state is below
`<state_dir>/offline/<namespace>/<project>/`. Docker names also contain a fixed
`live` or `offline` component, so a custom fixture policy cannot target live names.
Only one CLI operation holds a project's lock. A separate root publisher lock
serializes direct helper calls. Bounded Docker child processes retain the CLI
lock through an inherited standard descriptor if the CLI is killed: a retry may
briefly report an operation in progress until that child finishes. This bounds
client subprocess lifetime; it does not prove an already-submitted Docker daemon
operation completed or was canceled. After a Docker timeout or interrupted
mutation, inspect the exact managed resources and let the daemon settle before
recovery; do not treat a killed CLI as proof that no late resource can appear. Docker calls
are capped at 45 seconds, health exec at 5 seconds, and builds at 600 seconds,
with a five-second forced-termination grace. The state directory and files are respectively
0700 and 0600. Root policy/auth files may be 0640 with the runtime user's group;
no other users may write them or their root-controlled parent directories.

`status` reports `stopped`, `running`, `stale`, `recovering`, `orphaned_route` or
`unknown`,
canonical target identity, dirty state, current/previous generation and owner.
`observation_error` records an unavailable publisher or Docker observation while
preserving target identity. `unknown` means no recorded active target and an
observation failure; it must not be interpreted as stopped. Mutations still
require successful verification and never use this diagnostic fallback.
Running means the route matches and managed containers are running; it is not
continuous application health monitoring. `up`/`restart` check all recipe health
probes before publication. A failed startup stays available through
`logs --candidate`; the next authorized mutation cleans it up. A failed route
promotion preserves the active slot. If live route adoption or rollback cannot
be verified, the candidate and old slot remain recorded and running until an
operation can establish the route state. Never infer adoption from a file alone.

`down` first withdraws and verifies the route, then stops exact managed container
names only after checking their ownership labels. It retains test volumes. It
can withdraw a broken pending candidate without first proving that candidate's
adoption. An uncertain withdrawal must leave runtimes intact for recovery.
The publisher requires HTTPS 404 with neither generation marker to acknowledge
withdrawal; a 502/503 is not acknowledgment. First-time unconfigured `down` is
idempotent. Publisher temporary/pending files use non-JSON suffixes and are not
Traefik config; the only router file is `nebula-preview-<project>.json`.

Docker `unless-stopped` intentionally resumes active and retained previous
containers after a host reboot. This is not a boot-time refresh of the checkout.
Failed candidates can also resume until reconciled. Disable schedulers, queue
workers and production integrations in preview recipes: a retained old slot
keeps its complete application alive, not just established WebSockets. The next
`up` retires that old slot; `down` stops both. There is no automatic expiration,
drain deadline, background reconciliation or automatic failover in v1.

## Side-by-side candidate and server verification

All output goes under the project's git-ignored `.scratch/`; bootstrap already
ran before implementation. Do not install over the global Nebula binary.

```bash
CARGO_TARGET_DIR=.scratch/private-preview-implementation/target \
  cargo test -p nebula --no-default-features --test preview_cli -j 2
python3 scripts/test-private-previews.py \
  .scratch/private-preview-implementation/target/debug/nebula
python3 scripts/test-preview-publisher.py \
  .scratch/private-preview-implementation/target/debug/nebula
```

Run serially. The runtime fixture needs existing `nginx:1.28-alpine` and
`python:3.13-alpine` images, Docker 28+ and passwordless `sudo docker`; these two
fixture images were fetched during implementation. It uses random resource
names, loopback ports 28171/28172, test-only credentials and a synthetic app. No
production images, environments, database connections or daemon operations are
used. It removes only its exact fixture container/network/volume names.

The publisher fixture uses `sudo unshare --mount --net --fork`: `/etc` is a
private tmpfs, the candidate is copied to a root-owned path within that tmpfs,
and test listeners run on a new namespace's loopback. It exercises the actual
publisher against an external HTTP/2/TLS proxy fixture with a disposable trusted
certificate, including delayed adoption, concurrent publisher calls, uncertain
rollback (including rollback to no route), crash recovery and withdrawal. It neither mounts over live host
configuration nor shares the host's TCP listeners. This verifies the publisher
contract; it is not a claim of end-to-end live Traefik deployment.

## Deployment plan — execute only after orchestrator review

1. Review the implementation, exact test results and this runbook. Choose the
   first **disposable** project/checkout and test data, domain, policy namespace,
   two free ports and resource budget. Confirm Docker 28+ and its isolated bridge
   mode. Review that project's recipe/Dockerfile and prepared images; reject
   production credentials, workers and connections. Do not seed a production
   `.env`. `git rev-parse --path-format=absolute --git-common-dir` provides the
   policy identity; do not copy a branch name into this field.
2. DNS operation: add the selected preview domain's A record pointing to findl
   (`159.195.250.202` in the read-only inspection). Verify its authoritative DNS
   and absence/correctness of AAAA before use. The inspected
   `preview-test.possu.dev` was NXDOMAIN; the apex points elsewhere. Neither is
   evidence that a new domain already works. Preserve `code.possu.dev` unchanged.
3. Provision a distinct owner credential using a password-manager-generated
   random password of at least 32 characters. Do not reuse the mobile control
   token. On findl, the following prompts interactively; the plaintext password
   appears in neither argv nor shell history. The tested APR1 file format relies
   on the password's high entropy. Keep the credential in the owner's password
   manager and enter it only at the intended HTTPS preview domain.

   ```bash
   sudo install -d -o root -g root -m 0755 /etc/nebula
   sudo sh -c 'umask 077; { printf "owner:"; openssl passwd -apr1; } > /etc/nebula/preview-owner.htpasswd'
   sudo chown root:andrei /etc/nebula/preview-owner.htpasswd
   sudo chmod 0640 /etc/nebula/preview-owner.htpasswd
   ```

4. Install the reviewed candidate into a **separate root-owned path** and the
   scoped wrapper. Every parent must remain root-controlled; the publisher
   refuses a user-writable executable or policy path. No daemon/service restart
   or global binary replacement is required.

   ```bash
   sudo install -d -o root -g root -m 0755 /opt/nebula-preview /usr/local/libexec
   sudo install -o root -g root -m 0755 \
     .scratch/private-preview-implementation/nebula-preview-candidate \
     /opt/nebula-preview/nebula
   sudo install -o root -g root -m 0755 scripts/preview/nebula-preview-publish \
     /usr/local/libexec/nebula-preview-publish
   install -d -m 0700 /home/andrei/.local/state/nebula-previews
   ```

5. Write `/etc/nebula/previews.json` as root:andrei, mode 0640, using the example
   below **after** replacing/confirming identity, domain, ports and image digest.
   No custom output directory or upstream URL is accepted by the publisher.
   Production CLI policy is fixed to this path; custom policies require
   `--offline`, which restricts gateway binding to loopback and never calls the
   privileged publisher. There is no sudoers change in this implementation;
   andrei already has NOPASSWD ALL. A later least-privilege OS design must address
   Docker authority as well as the helper.

   ```json
   {
     "namespace": "findl",
     "state_dir": "/home/andrei/.local/state/nebula-previews",
     "bind_address": "172.18.0.1",
     "gateway_image": "nginx@sha256:a8b39bd9cf0f83869a2162827a0caf6137ddf759d50a171451b335cecc87d236",
     "projects": {
       "nebula": {
         "git_common_dir": "/home/andrei/nebula/.git",
         "domain": "nebula.possu.dev",
         "ports": [28171, 28172],
         "auth_file": "/etc/nebula/preview-owner.htpasswd"
       }
     }
   }
   ```

6. Verify Traefik's existing `websecure` entrypoint, `letsencrypt` resolver and
   watched dynamic directory. The publisher adds only
   `/etc/dokploy/traefik/dynamic/nebula-preview-nebula.json`; it does not edit
   `nebula-web.yml`, reuse its middleware, or restart Traefik. Confirm Traefik can
   reach the authenticated gateway's approved bridge address/port. A private
   address bind is not an interface ACL: authentication is mandatory on direct
   gateway requests too. Do not infer external reachability from listener output;
   existing nftables Dokploy guards protect unrelated management ports.
7. Put the test-only recipe in the selected checkout, chmod it 0600, and run the
   installed candidate's `preview up --owner <agent-id>`. It creates private test
   runtime containers and writes the scoped route only after backend and gateway
   checks. The publisher probes HTTPS locally using `curl --resolve`, verifies
   TLS normally, disables curl config/proxies, and polls for the exact anonymous
   401 + generation. Initial certificate issuance may exceed the bounded poll;
   a verified rollback is safe to retry. No DNS/TLS verification is skipped.
8. Orchestrator on laptop: verify anonymous HTML/API/WebSocket denial, owner
   login, API and actual HMR/WebSocket behavior in a desktop browser. Repeat on
   a physical mobile browser through `code.possu.dev`-originated agent control
   and the new preview domain. Record physical-mobile results separately from
   desktop mobile emulation. Test direct gateway/backend paths from an external
   IPv4/IPv6 vantage; backend ports must remain unavailable and any reachable
   gateway path must still challenge. No browser is installed on findl.

## Rollback, credentials and cleanup

- Routine rollback after a bad build is automatic: the previous target remains
  routed until the candidate passes. If publication fails, the prior route is
  restored and verified; uncertainty preserves both slots. For an operator stop,
  run the candidate's scoped `preview --project <name> down --owner <id>` (add
  `--replace` only for deliberate takeover). Confirm `status` is stopped and the
  HTTPS domain no longer returns preview generation headers. Never use
  `nebula kill`, remote restart, Docker prune or a production service restart.
- If publisher verification is uncertain, inspect the scoped route and logs,
  restore HTTPS/Traefik reachability, then retry `down` or the intended lifecycle
  operation. Do not remove containers merely because an on-disk route looks old.
  In a reviewed emergency, removing only `nebula-preview-<project>.json` still
  requires verifying live withdrawal before deleting its managed runtime.
- To revoke/rotate the owner credential immediately, `down` first, replace the
  auth file using the provisioning operation, then `up`. File bind mounts retain
  the old inode across atomic replacement; a new gateway is required. A simple
  `restart` retains an old slot temporarily and is not complete credential
  revocation. Browser Basic Auth caching/logout remains browser-dependent.
- `down` deliberately preserves test data volumes and local snapshots/config.
  Image builds and snapshots are not garbage-collected in v1. After an explicit
  data-deletion decision, remove only the named project's offline/live slot
  volumes/images and private state while stopped; there is no broad cleanup
  command. Retaining snapshots means disk use needs operator monitoring.
- To remove the feature after `down` and verified withdrawal, remove only the
  preview policy/auth files, dedicated wrapper/binary and selected preview DNS
  record. Preserve the global Nebula binary, daemon sessions, existing Traefik
  config and all production application/database resources.

## Remaining verification limits

No live DNS/auth provisioning, persistent ingress installation, production route
change, daemon restart or global binary replacement was performed. Server tests
cannot establish browser credential caching, application-specific HMR behavior,
physical-mobile usability or external IPv4/IPv6 firewall behavior. Those remain
deployment gates. App Bearer Authorization, live checkout mounts, arbitrary host
process recipes, outbound runtime networking, production data, submodules and
cross-host preview control are outside this v1.


## Primary implementation references

- [Docker isolated gateway mode and port publishing](https://docs.docker.com/engine/network/port-publishing/)
- [Nginx Basic Auth](https://nginx.org/en/docs/http/ngx_http_auth_basic_module.html)
- [Nginx WebSocket proxying](https://nginx.org/en/docs/http/websocket.html)
- [Nginx proxy header configuration](https://nginx.org/en/docs/http/ngx_http_proxy_module.html)

Context7 was unavailable in this session; these official sources were consulted
for the network and proxy contract. The bootstrap completed before implementation.
The Rust toolchain lacked rustfmt afterward; only its formatter component was
added for formatting the new Rust files, without changing repository dependencies
outside Cargo's normal build or replacing any installed Nebula binary.

## Implementation verification record — 2026-09-14

The reviewed implementation is on `private-project-previews`, based on
`f5968b2c138a84637f0f442a0b3a819c9e5afcb9`. The implement skill's Standards and
Spec reviews and the independent security rereview have no remaining concrete
findings. Deployment remains pending the gates above.

Commands ran serially with
`CARGO_TARGET_DIR=.scratch/private-preview-implementation/target`:

- `cargo test -p nebula --no-default-features --test preview_cli -j 2`:
  **6 passed**, including policy injection, control-domain reuse and state symlinks.
- `cargo test --workspace --no-default-features --lib --bins -j 2 -- --test-threads=2`:
  **717 passed** (CLI 12, core 12, daemon 151, TUI 542).
- `cargo test --workspace --no-default-features --lib --bins --test preview_cli --test e2e_pty -j 2 -- --test-threads=2`:
  CLI units passed, then PTY **30 passed, 1 failed**. The existing
  `worktree_new_seeds_node_modules_when_lockfile_matches` fails because unchanged
  `crates/nebula-daemon/src/git.rs` uses macOS `cp -Rc`; GNU cp rejects `-c`.
  The same source is present at the base commit and the unsupported flag was
  independently reproduced. The separate unit command above ran the remaining
  workspace units after that failure stopped the combined command.
- `python3 scripts/test-private-previews.py <candidate>`:
  **9 acceptance groups passed** against real disposable Docker/Nginx services:
  persistence, all-path authentication/WebSockets/Origin, direct backend denial,
  failed promotion, symlink rejection, ownership/concurrency/crash recovery,
  stale state and retained data, down with broken pending publication, and
  Dockerfile artifacts/API routing/command health/distinct data volumes/scoped stop.
- `python3 scripts/test-preview-publisher.py <built-binary>`:
  **5 acceptance groups passed** against the actual root publisher in private
  namespaces: HTTP/2 TLS adoption, verified rollback/concurrency, uncertain
  rollback, killed-publisher recovery, and 503 refusal for rollback-to-None/down.
- Targeted `rustfmt --check`, Python `py_compile`, and `git diff --check` passed.

The eight existing `e2e_tui` tests were excluded because their teardown invokes
`nebula kill`, expressly prohibited for this task. Default Ghostty features were
not built. No browser was installed or run. The live Traefik/DNS/external
IPv4/IPv6 and desktop/physical-mobile checks remain pending; isolated server
acceptance is not a substitute for those checks.

The side-by-side binary is
`.scratch/private-preview-implementation/nebula-preview-candidate`, copied from
this build without modifying the global installation. Its version/help commands
passed. SHA-256:
`d2f9a80ddb582e8d1ad42e52c973be6a690e3598486ee13b2ca4792dc92cac6c`.
Logs, including the reproduced baseline failure, are under
`.scratch/private-preview-implementation/`. Fixture cleanup removes exact owned
resources, and no production service or existing agent session was stopped.
Client timeouts and inherited locks bound subprocess lifetime; they cannot prove
that already-submitted Docker daemon operations completed or were canceled.

Changed paths:

- `Cargo.lock`
- `crates/nebula/Cargo.toml`
- `crates/nebula/src/main.rs`
- `crates/nebula/src/preview/config.rs`
- `crates/nebula/src/preview/files.rs`
- `crates/nebula/src/preview/ingress.rs`
- `crates/nebula/src/preview/mod.rs`
- `crates/nebula/src/preview/runtime.rs`
- `crates/nebula/tests/preview_cli.rs`
- `docs/specs/private-project-previews.md`
- `scripts/fixtures/preview_server.py`
- `scripts/preview/nebula-preview-publish`
- `scripts/test-preview-publisher.py`
- `scripts/test-private-previews.py`
