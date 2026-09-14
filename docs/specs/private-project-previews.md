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
idempotent. Publisher temporary/pending files use non-YAML suffixes and are not
Traefik config; the only router file is `nebula-preview-<project>.yaml`.

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
   `/etc/dokploy/traefik/dynamic/nebula-preview-nebula.yaml`; it does not edit
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
  In a reviewed emergency, removing only `nebula-preview-<project>.yaml` still
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

## Follow-up deployment and registration contract — 2026-09-14

The owner subsequently authorized preview-only installation and configured
wildcard DNS. `nebula.possu.dev` resolves to `159.195.250.202`; no AAAA was reported.
No DNS changes are made by this implementation. This section supersedes the
original deployment-pending authorization, but not the security requirements.

The reviewed `6318611` candidate is installed separately at
`/opt/nebula-preview/nebula`; its SHA-256 is the value in the verification record
above. `/usr/local/libexec/nebula-preview-publish` remains the fixed root-owned
publisher wrapper. Credential provisioning refused existing files and generated
48 random bytes encoded as a URL-safe password in process memory; plaintext was
written only to `/etc/nebula/preview-owner.initial-password`, root:root 0600.
The username is `owner`. The hash file `/etc/nebula/preview-owner.htpasswd` and
policy `/etc/nebula/previews.json` are root:andrei 0640. The plaintext is for the
orchestrator's secure retrieval into the owner's password manager; never copy it
into chat, logs, documentation, command arguments or the fixture repository.

The live policy uses namespace `findl`, project `nebula`, domain
`nebula.possu.dev`, and gateway slots `172.18.0.1:28171`/`:28172`. Both gateway and
Python fixture use the previously verified image digests. Project identity is
**the disposable synthetic repository**, not the production Nebula checkout:
`/home/andrei/nebula-worktrees/private-project-previews/.scratch/private-preview-implementation/live-fixture/.git`.
Its `/` page identifies the synthetic fixture and provides a browser API/WS check;
`/api/status` reports test-only observations and `/ws` echoes a test message.
No control application, control token, production environment or database is used.

### Registration remains fail-closed

There is no `preview register` command and no arbitrary new-project autonomy.
Wildcard DNS answers cannot distinguish an unallocated name from an explicit
record pointing to the same server. The available ingress inventory is not an
authoritative inventory of every DNS name. Do not infer permission to claim a
sibling name from a successful DNS lookup or an absent current HTTP route.

The minimal accepted future contract is a **root-reserved exact hostname pool**:

1. The DNS owner first supplies an authoritative inventory/reservation and
   explicitly allocates exact names for previews. Only single-label children of
   `possu.dev` are eligible; apex, `code.possu.dev`, existing explicit records and
   every non-preview ingress domain are reserved. Pool provisioning is separate
   from runtime registration; no DNS writes or credentials are accepted by agents.
2. A fixed root-owned helper, policy and secure parent directories enforce a
   global allocation lock. Agents can assign an already approved pool entry to
   a canonical repository identity; domain and identity are immutable afterward.
   Git identity discovery runs as the invoking user. Root never executes Git,
   hooks, recipes, or caller-supplied executables on a writable checkout.
3. The helper checks effective HTTP and TCP routing across file, Docker and Swarm
   providers. Exact Host rules, HostRegexp, HostSNI and catchalls all participate;
   missing provider state or unknown/ambiguous overlap fails closed. An allocation
   never overwrites or reassigns a non-preview route. The domain pool is not
   derived automatically from DNS answers.
4. Fixed root configuration owns the gateway digest, bind address, authentication
   path and port range. Agents supply none of these. The global lock reserves a
   unique A/B port pair across projects and checks existing publications/listeners;
   runtime bind failures remain errors, never permission to take another service's
   port. Allocation updates policy atomically in its root-owned directory and
   records identity/domain/ports together. There is no agent reassign/delete verb.
5. Test the public registration interface in private namespaces before installing:
   concurrent allocations, duplicate domain/repository/port, missing pool entry,
   apex/control/nested-domain injection, symlink/raced identity paths, conflicting
   HTTP/TCP/regex routes, unavailable inventory, atomic crash recovery and
   preservation of existing assignments. Request a fresh security review.

No new pool has been authorized or provisioned in this deployment, so no
registration extension is installed. The explicitly approved `nebula.possu.dev`
fixture does not authorize allocating other wildcard siblings.

### Agent invocation and mobile integration

Use the dedicated executable directly; the existing global `nebula` has no
preview command and does not delegate to the candidate:

```bash
/opt/nebula-preview/nebula preview --project nebula status
/opt/nebula-preview/nebula preview --project nebula logs --service web --lines 100
/opt/nebula-preview/nebula preview --project nebula restart \
  --owner 01M2FR05N1FE4DZV7JPMHPY4QM
```

A different agent must use its own stable owner ID and explicitly add `--replace`
for takeover. The fixture is the only registered preview project. Do not point
its policy at a real application until its recipe, prepared dependencies, test
data and identity transition have been reviewed while the fixture is stopped.

Read-only inspection of `ce4d20c` found that the mobile gateway's `/api/preview`
executes `preview --project <project> status` through its configured Nebula binary
(`NEBULA_WEB_BINARY`, default `nebula`). Its separate CLI change pins a URL using
existing daemon link messages after promotion. That commit is not included in
the installed `6318611` candidate, so this deployment does not auto-pin a link.
The synthetic checkout is also not registered in the host daemon's checkout list.

For mobile integration, the orchestrator must deploy the reviewed web code/assets
and arrange a preview-capable CLI invocation in a separately authorized web
service update window. Do not replace the global binary or blindly point all
mobile commands at an older candidate. A safe additive adapter can dispatch only
first argument `preview` to `/opt/nebula-preview/nebula`, and all other arguments
to the existing absolute global CLI, preserving argv without shell evaluation.
Point `NEBULA_WEB_BINARY` at that adapter only after review/testing, and deploy the
web changes with the orchestrator's approved service procedure. This adapter and
web deployment are proposed, not installed here; no web or daemon restart occurs.

### Deployment checks and shared-directory boundary

Before-state identities, route-file hashes, and verification logs are recorded
under `.scratch/private-preview-implementation/deployment/`. The initial publisher
attempt correctly refused `/etc/dokploy` because that shared ancestor was
root:root **0777**; `/etc/dokploy/traefik` and `dynamic` were already root:root0755.
No preview route was written by that refused attempt. Direct gateway anonymous
401, wrong Host404 and authenticated API checks passed while HTTPS still had the
unconfigured self-signed default certificate.

The orchestrator explicitly approved the only shared-infrastructure fix:
**nonrecursive** `chmod 0755 /etc/dokploy`, preserving owner and contents. It was
applied with root:root ownership and inode 521989 unchanged; the before/after
record is `deployment/parent-permission.json` under the scratch output directory. Observed Dokploy and Traefik processes run as host UID0
without user-namespace remapping; nonroot Grafana/Prometheus mounts are read-only
nested files/directories and retain traversal with 0755. No external nonroot
writer was identified, but this audit cannot prove one does not exist. The
publisher security check must not be weakened to accommodate the old permissions.

Live verification command (reads the dedicated credential only into memory):

```bash
sudo -n python3 scripts/verify-live-private-preview.py
```

This exercises verified public HTTPS, anonymous and invalid-credential HTML/API/WS
denial, direct gate authentication, owner HTML/API, Host/Origin rejection,
credential stripping and a real authenticated WebSocket echo. The initial run
failed at certificate verification before any credential read, as expected with
no published route. Server execution remains distinct from laptop/browser and
physical-mobile verification.

### Actual file-provider defect and durable first-publication recovery

Live deployment exposed a defect missed by the synthetic publisher fixture:
Traefik v3.6.7 ignores `.json` files in its watched directory. While the original
publisher's JSON file existed, the effective API contained no corresponding
router, service or middleware and no certificate request was initiated. Its
failed rollback correctly retained the pending marker and candidate when TLS
could not verify withdrawal.

The publisher now uses the fixed `nebula-preview-<project>.yaml` filename with
JSON-serialized bytes (valid YAML). Status, atomic promotion, rollback and down
all use that same filename. Existing legacy `.json` entries, including symlinks,
refuse operations with an explicit diagnostic; only metadata `NotFound` means
absence. Other metadata errors propagate. No old file is silently deleted or
converted by the helper. Before updating an already used installation, inspect
any legacy file and its effective routing; retire only the verified preview
artifact. The refused live deployment had no remaining legacy route file, so its
existing pending marker and candidate state were retained unchanged.

First publication now has a fixed **120-second** verified-adoption window for
ACME issuance. Replacement, status, rollback and withdrawal retain their
15-second windows; individual requests remain bounded and always verify TLS.
There is no user-supplied timeout or bootstrap route API. The candidate must pass
backend readiness and gateway fail-closed checks before the initial route write.
If issuance exceeds the window, preserve uncertainty and investigate ACME/DNS
before retrying; do not loop blindly or disable TLS.

A pending first publication on an uncertified domain can prevent ordinary `up`
from reconciling HTTPS absence. After fixing its cause, an operator can recover
by scoped re-publication of the **recorded healthy candidate**, using the fixed
installed helper:

```bash
sudo -n /usr/local/libexec/nebula-preview-publish \
  <approved-project> <recorded-a-or-b> <recorded-generation>
```

Read the nonsecret generation from `preview status`/recorded state; do not invent
one. The helper rechecks the exact candidate's authentication gate, publishes
under the same project lock, and verifies trusted TLS +401+generation. It cannot
select an arbitrary host, port, path or proxy fragment. Preserve state/pending
markers until verification succeeds. Then normal owner-authorized `up` can
reconcile the candidate and refresh it; alternatively scoped `down` verifies
withdrawal and stops the recorded candidate, and a subsequent `up` starts cleanly
with preserved test data and the issued certificate. Never edit the lifecycle
journal merely to suppress uncertainty.

The new regression fixture runs the actual extracted Traefik v3.6.7 binary in a
private mount/network namespace, with disposable trusted certificates and an
unreachable local ACME endpoint (no public CA requests). It failed against
`6318611`: no route adoption within 12 seconds. It passes with the fixed candidate,
including refusal/preservation of a legacy file, A/B adoption via the actual file
provider, and verified withdrawal. The synthetic publisher suite remains useful
for delayed adoption, concurrency and uncertain rollback but is not a parser test.

```bash
python3 scripts/test-preview-traefik.py \
  .scratch/private-preview-implementation/nebula-preview-yaml-candidate \
  .scratch/private-preview-implementation/traefik-v3.6.7
```

The test binary was copied from the existing `traefik:v3.6.7` image through one
explicitly named temporary container, which was removed afterward. No production
container or Traefik process was restarted.

References: [Traefik file provider formats](https://doc.traefik.io/traefik/providers/file/)
and [version-matched directory extension filter](https://github.com/traefik/traefik/blob/v3.6.7/pkg/provider/file/file.go#L379).

### Final deployed state and evidence

**Live synthetic fixture:** <https://nebula.possu.dev>. The final CLI status is
`running`, active slot A, generation `01m2fxe49e0jjnn1jv9cdb9dm4`, owner
`01M2FR05N1FE4DZV7JPMHPY4QM`; candidate, previous and observation_error are null.
The synthetic repository is clean at `f9c3e3eaafd2a0ededd862f9fdc36d904fe0cf53`.
Its local branch is named `master`; it is an isolated fixture repository, no
protected ref was pushed, and the implementation branch remains
`private-project-previews`. This is not a deployed Nebula control application.

The installed binary is Nebula `0.6.0` with the reviewed YAML publisher correction:

- Executable: `/opt/nebula-preview/nebula`, root:root 0755.
- SHA-256: `990f090207dfd474e09cea541ff8e45f40e2c947598ddc080e0fb99e2b80e61e`.
- Side-by-side artifact: `.scratch/private-preview-implementation/nebula-preview-yaml-candidate`.
- The prior candidate is retained at `/opt/nebula-preview/nebula.6318611` for
  provenance only. Do not restore its ignored-JSON publisher while a YAML route
  exists; stop/withdraw with the fixed binary first.
- Fixed helper unchanged at `/usr/local/libexec/nebula-preview-publish`, root0755.
- Policy/authentication files retain the modes and paths documented above;
  plaintext retrieval remains only `/etc/nebula/preview-owner.initial-password`.

The single effective preview router is
`nebula-preview-findl-nebula@file`, exact `Host(nebula.possu.dev)` on `websecure`,
backed by `http://172.18.0.1:28171`. Its file is
`/etc/dokploy/traefik/dynamic/nebula-preview-nebula.yaml`, root:root 0600.
The pending marker is absent. Trusted TLS and generation matching passed; no
self-signed acceptance or TLS-verification bypass was used. ACME issuance
succeeded within the longer first-publication window.

The retained uncertain candidate was first republished through the fixed helper.
The authenticated live verifier passed, then scoped `down` reported `stopped`
and the public domain returned verified HTTPS404. A subsequent ordinary `up`
created the final active A slot with preserved test data; the full live verifier
passed again. The fixture remains running for later laptop verification.

Final topology:

| Resource | Identity / boundary |
| --- | --- |
| Gateway | `nebula-preview-live_findl_nebula_a-gate`, container `752e8cc016c4`; pinned Nginx digest; UID1000; only published port `172.18.0.1:28171 -> 8080` |
| App | `nebula-preview-live_findl_nebula_a-web`, container `0a72cfe5f00d`; pinned Python digest; UID1000; **no published ports** |
| Backend network | `nebula-preview-live_findl_nebula_a-back`, internal, IPv4/IPv6 gateway mode `isolated`; only app and gateway |
| Edge network | `nebula-preview-live_findl_nebula_a-edge`; only gateway; no production network attachment |
| App data | `nebula-preview-live_findl_nebula_a_web_data`; test-only writable volume |
| Secrets | Only gateway has read-only `/owner.htpasswd`; app has sanitized read-only `/app` and its data volume, no auth/control credential mount |
| Supervision | Both containers `unless-stopped`; existing resource caps remain in force |

Verification after the correction:

- `cargo test -p nebula --no-default-features --test preview_cli -j 2`: **6 passed**.
- `cargo test --workspace --no-default-features --lib --bins -j 2 -- --test-threads=2`:
  **717 passed**. No additional PTY/TUI end-to-end run; the earlier baseline failure
  and prohibited `nebula kill` teardown exclusions remain documented above.
- `scripts/test-preview-traefik.py`: **passed** actual v3.6.7 parser/watch,
  legacy-file refusal, A/B promotion and TLS withdrawal. The old binary failed
  this same real-provider acceptance before the implementation change.
- `scripts/test-preview-publisher.py`: **all 5 acceptance groups passed** with the
  new extension and initial120s window, including failed adoption, concurrency,
  crash recovery and uncertain rollback-to-None/down on503.
- `sudo -n python3 scripts/verify-live-private-preview.py`: **all 3 groups passed**
  twice against real HTTPS: anonymous/invalid-credential HTML/API/WS denial,
  authenticated HTML/API/header stripping/Host/Origin/snapshot checks, and
  authenticated same-origin WebSocket upgrade/echo. The direct gate's anonymous
  and invalid-credential paths were also checked. This fixed live verifier assumes
  the initial A gateway port28171; update the fixture verifier if deliberately
  changing slots rather than silently testing a retained old gateway.
- One disposable outsider container on Docker's default bridge could not connect
  to the isolated backend port8080 or public-IP direct port28171; its request to
  the private gateway received401. It was removed by exact name. This is a
  **same-host network-boundary test**, not an external Internet IPv4/IPv6 test.
- Targeted rustfmt, Python compilation and `git diff --check` passed.

Before/after evidence is in `.scratch/private-preview-implementation/deployment/`:
`before.json`, `after.json`, `parent-permission.json`, `topology.json`,
`status-final.json`, `real-traefik-red.log`, `real-traefik-green.log`,
`yaml-cli-tests.log`, `yaml-workspace-tests.log`, `yaml-publisher-tests.log`,
`live-verification-final.log`, `live-down.log`, `withdrawal.log`, and
`direct-paths.log`. Sensitive infrastructure metadata files are root0600; no
credential values were logged. Exact IDs/images/start times/restart counts/status
of **all 14 preexisting containers** (13 running plus one exited migration)
matched before/after. All **4 preexisting route/config file hashes** matched;
only ACME certificate storage changed as expected. The independent reviewer also
confirmed the **25 preexisting effective routers** were unchanged.

Traefik container `fbd3dab1ce9e` retained its identity/start time; the mobile web
service retained PID1176299 and its activation timestamp; the bridge socket's
activation identity also matched. `https://code.possu.dev/` still returned
trusted HTTPS200. No global CLI replacement, daemon/web/production restart,
firewall/sudoers change, DNS edit, main-worktree edit, merge, push or sync occurred.
The only shared-permission change was the explicitly approved parent chmod above.

The security, Standards and Spec rereviews found no remaining actionable concern
in the narrow fix. Registration remains **deferred**, with no allocated pool or
new command. Mobile pin integration `ce4d20c` remains **undeployed by this worker**.
Desktop/browser HMR and physical-phone checks await the laptop; external-network
IPv4/IPv6 checks remain separate. Basic Auth/Bearer incompatibility, frozen
snapshots, test-only dependencies/data and Docker-operation cancellation limits
remain unchanged.

Rollback of this deployed fixture is the tested scoped command:

```bash
/opt/nebula-preview/nebula preview --project nebula down \
  --owner 01M2FR05N1FE4DZV7JPMHPY4QM
```

An orchestrating agent taking ownership uses its own ID with explicit `--replace`.
Require stopped status and trusted HTTPS404 with no generation markers before
removing any managed runtime. Data is retained; credential removal/rotation and
resource deletion remain separate explicit operations. Preserve the hardened
shared parent permissions; do not restore world-writable mode as routine rollback.
The owner's wildcard DNS and all production routes remain untouched.
