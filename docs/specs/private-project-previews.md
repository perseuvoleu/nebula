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
