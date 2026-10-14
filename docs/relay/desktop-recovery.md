# Desktop relay recovery

## Contract

The remote daemon owns sessions and agents. Recovery replaces only the local
relay's transport. It must never recreate sessions, replay input or mutations,
restart daemons, or interpret an idle terminal as an unavailable host.

The desktop route is TUI → local daemon → SSH byte transport → host daemon.
`remote status` uses a separate path and is not evidence that this transport
makes progress. The mobile web UI is outside this change.

## Failure evidence

A fragmented incoming session-read reply was interrupted by outbound input.
The old select loop dropped `read_frame` after consuming a header/payload prefix,
then interpreted subsequent bytes as a new header. The transport stayed open
while reads stopped completing. The regression uses a duplex byte transport and
acknowledges fragment consumption, without sleeps to arrange the interleaving.

The isolated patch from remote commit b2b89c0e24e5f7103dd4880db03cd35bdf2e6095
was applied to this protocol-28 base; the remote branch was not merged.

## Transport behavior

- Keep the same frame-read future across outbound traffic. The existing 4 MiB
  codec frame limit remains unchanged.
- Bound handshake and writes at 10 seconds. A timeout abandons the entire pipe;
  a cancelled partial write is never resumed on that connection.
- Probe the application using the existing read-only Hello/HelloOk exchange,
  with one outstanding probe and 10 seconds to answer. Terminal output is not
  a heartbeat. Probe deadlines have priority over queued traffic.
- Bound pending caller requests independently of probes. An unanswered request
  expires after 30 seconds, checked on the next probe deadline. At most 256
  caller RPCs and 256 outbound frames can be pending.
- Serialize admission against teardown. Teardown closes admission, discards the
  old queue, and returns an explicit unknown-outcome error for pending RPCs.
  A new handshake admits new requests. Request IDs remain monotonic across
  reconnects, and each old pipe is dropped completely.
- Reconnect with the existing bounded 2–30 second backoff. Subscribe again;
  reconcile the host snapshot and reattach surviving watchers with full replay.
- Never await a local client's queue in the relay. If delivery cannot preserve
  ordered bytes, cancel that local connection. Its desktop client reconnects,
  subscribes, and requests a new screen; other clients continue independently.

## Canonical protocol-29 compatibility (2026-09-15)

Recovery base: 649d06f on protocol-28 a579456. The candidate ports only
Rust prerequisites from 64a8981 (parent a579456) and 3f22f10, retaining recovery.
64a8981 supplies the complete schema/handlers used by local-protocol-upgrade
b726d85: InputAcknowledged, CheckWorktreeRemoval and DeleteAgentScoped.
Their PTY acknowledgement, project validation and conservative worktree-removal
safeguards are required; unrelated web controls and f5968b2 are excluded.

Read-only inspection of findl source 3c6621e shows another schema also numbered
29, introduced by 3f22f10 and preserved by merge 15f2d68. Its branch-safety
implementation is required too: resolve a physical host checkout, preserve
branch intent through creation, and refuse unsafe primary movement/resume.
The candidate targets this newer schema as a matched TUI/CLI and daemon pair.

| Wire surface | b726d85 | Target: findl 3c6621e |
| --- | --- | --- |
| CreateAgent | 8 positional fields | 9; expected_branch after worktree |
| CreateTerminal | req_id, worktree, name | req_id, worktree, expected_branch, name |
| ResolveBranchCheckout | Absent | req_id, project, branch |
| CheckoutPrimary | Existing wire fields | Same fields, stricter checkout safeguards |
| Hello / HelloOk | 29, no capabilities | 29, no capabilities |

`codec.rs` uses `rmp_serde::to_vec`: variant names are tagged, but fields are
positional. A local diagnostic compiled from both complete protocol definitions
confirmed CreateTerminal fails in both directions: old → new reports invalid
length 3 (expected 4); new → old reports LengthMismatch(3). Source copies and
results are in ignored `.scratch/p29-wire-audit`. This prevents treating the
number 29 or version 0.6.0 alone as proof of compatibility.

Fresh bounded read-only Hello checks returned Incompatible(29) to Hello 28 and
HelloOk(29) to Hello 29 on both local `/tmp/nebula-501/daemon.sock` (PID 71248)
and findl `/run/user/1000/nebula/daemon.sock` (PID 1894421). Binary string
inspection found ResolveBranchCheckout/expected_branch only in findl's running
executable, supporting the source-dialect difference without claiming a complete
running-source attestation. No remote builds, writes or synchronization occurred.

**Activation constraint:** older local TUI/CLI clients use incompatible creation
frames despite a successful Hello 29. Future activation must coordinate matching
clients and daemon; do not use the new candidate CLI to mutate the old default
daemon. Candidate tests use isolated state/sockets. Source tests and a release
build cannot prove live remote-pane recovery. Installation, integration and
activation remain separate approvals; nothing is installed or live by preparation.

## Protocol-29 verification (2026-09-15)

The isolated Rust port includes the daemon semantics and client branch-intent
paths from 64a8981 and 3f22f10, without web/mobile UI or preview dependencies.
Recovery transport, watchdogs, replay and client isolation remain intact.
Required integration adjustments:

- Add explicit expected_branch to recovery test requests.
- Use shared execution guards for create/move/reparent/prewarm and exclusive
  execution for safe removal, deletion and primary switching. Lifecycle locks
  precede exclusive execution locks. Keeping the old try-lock on ordinary
  reconciliation's mutex made moves fail and dropped hook reparenting; the
  execution guard preserves removal safety without that contention.
- Shorten the imported race-test proxy directory to `px` for macOS socket limits.
- Restore displaced public API documentation from the prerequisite patches.

| Verification | Result |
| --- | --- |
| Fixed findl MessagePack fixtures: all four new requests, CreateAgent and CreateTerminal | 1 test passed (6 fixtures; meaning and exact bytes) |
| Literal Hello 29, repeated probes, Hello 28 rejection | 1 PTY test passed |
| Existing relay recovery tests | 13 passed |
| CLI unit tests | 12 passed |
| PTY/daemon integration | 36 passed |
| Desktop terminal E2E | 8 passed |
| Core unit tests | 12 passed |
| Daemon unit tests | 164 passed |
| TUI unit tests | 546 passed across workspace run and fixture rerun |
| Documentation tests | No failures |

Total: 779 unique tests (the handshake is included in PTY tests; relay tests
are included in daemon unit tests). The first full workspace invocation had
three PTY failures and the known enclosing-checkout TUI fixture failure. The
socket-path and mutex integration fixes above resolved all three PTY failures;
the complete PTY and daemon targets then passed. The TUI fixture passed with
ordinary filesystem fixtures in `/tmp`; Nebula data/runtime paths stayed in
ignored project scratch. No product change was made to repository discovery.

Red controls: protocol-28 source rejected InputAcknowledged; b726d85's older
protocol-29 source decoded the new CreateAgent name as AgentKind and failed;
a temporary protocol-28 constant made the literal Hello-29 test receive
Incompatible(28). Each source control was restored before green verification.
These are local serialization/fixture proofs, not live pane recovery evidence.

Commands (run sequentially, never concurrently):

```sh
export CARGO_BUILD_JOBS=2
export NEBULA_DATA_DIR="$PWD/.scratch/d"
export NEBULA_RUNTIME_DIR="$PWD/.scratch/r"
export TMPDIR="$PWD/.scratch/t"
export GIT_CEILING_DIRECTORIES="$PWD/.scratch/t"
cargo test -p nebula-core --test protocol29 -- --test-threads=1
cargo test -p nebula --no-default-features --test e2e_pty protocol29_handshake -- --test-threads=1
cargo test -p nebula-daemon --lib relay::tests -- --test-threads=1
cargo test --workspace --no-default-features --no-fail-fast -- --test-threads=1
# After resolving integration failures:
cargo test -p nebula --no-default-features --test e2e_pty branch_session_cli_rejects_a_checkout_switched_after_resolution -- --test-threads=1
cargo test -p nebula --no-default-features --test e2e_pty -- --test-threads=1
cargo test -p nebula-daemon --lib -- --test-threads=1
TMPDIR=/tmp cargo test -p nebula-tui --no-default-features --lib open_at_lands_on_the_owning_worktree_or_adds_the_repo -- --test-threads=1
```

Logs: ignored `.scratch/p29-wire-audit/` (`red-*.log`, `green-*.log`,
`focused-relay.log`, `workspace.log`, `final-*.log`). Read-only standards and
spec reviews were completed; the findings above were addressed and rechecked.

## Release candidate and remaining activation steps

Built successfully with default Ghostty support, Zig 0.15.2 and two Cargo jobs
in 1m 42s:

```sh
PATH="/opt/homebrew/opt/zig@0.15/bin:$PATH" CARGO_BUILD_JOBS=2 \
NEBULA_DATA_DIR="$PWD/.scratch/d" NEBULA_RUNTIME_DIR="$PWD/.scratch/r" \
cargo build --release -p nebula --target-dir target/relay-protocol29-candidate
```

Candidate absolute path:
`/Users/andrei/Desktop/nebula-worktrees/relay-auto-recovery/target/relay-protocol29-candidate/release/nebula`

SHA-256: `195a36762320a3f63e3f919ac74536dac042f9626f3337e6b65369d44fda356b`.
The release executable also passed repeated literal Hello 29 and rejected Hello
28 in its own temporary `.scratch/p29-*/{d,r}` state/runtime directories. Only
that owned fixture daemon was shut down; neither live daemon was changed.
Smoke script/output and release build log are under `.scratch/p29-wire-audit/`.

**Not installed and not live.** Next steps require separate approval:

1. Review and integrate the candidate into primary while preserving its unrelated
   Local/Remote visibility edits in app.rs, ui.rs and event_loop.rs.
2. Plan installation and a safe local daemon activation window, coordinating
   matching new CLI/TUI clients. Old local clients' Hello 29 is insufficient.
   Verify the deployed findl schema/build before activation; no remote upgrade
   or restart is included in this preparation.
3. After approved activation, verify the actual desktop remote pane, reconnect
   behavior and surviving findl session identity. A tested artifact does not
   prove that the currently live remote pane recovered.

## Historical protocol-28 compatibility and activation

Source base: a579456, protocol 28. Version string 0.6.0 is not a build identity.
Read-only socket handshakes on 2026-09-15 returned:

| Endpoint | Hello 28 | Hello 29 | PID from HelloOk |
| --- | --- | --- | --- |
| Local default socket | Incompatible(29) | HelloOk(29) | 71248 |
| findl `/run/user/1000/nebula/daemon.sock` | Incompatible(29) | HelloOk(29) | 1894421 |

The protocol-28 candidate cannot be activated alongside these protocol-29
processes. Do not switch or alter the separate local-protocol-upgrade checkout.
Review and port the isolated recovery changes to an explicitly approved current
protocol-29 base before deployment. Do not just change the protocol constant.

Build into this worktree's candidate target directory. No `make install`,
default-socket test daemon, process kill, SSH relay kill, push, sync, or collect
is part of preparation. Test daemons, if needed, require both NEBULA_RUNTIME_DIR
and NEBULA_DATA_DIR under ignored `.scratch`.

Installation and daemon activation are distinct approvals. Installing a binary
alone does not replace the running daemon. Any later daemon restart requires a
separately approved safe window because it ends sessions. Source tests and a
candidate build do not prove that the live laptop's pane has recovered.

## Desktop behavior

Remote primary, split, and floating terminal panes retain a host-specific
connecting/reconnecting/unavailable notice until Scrollback arrives. Attach
waits retry at 10-second intervals, at most three attempts per recovery cycle;
Ctrl+R while the notice is visible starts a new safe attach attempt. Repeated
offline notices do not reset this budget. Retry sends no input, prompt, create,
or restart request. On host recovery, the new snapshot starts a fresh attach
cycle. Fresh replay replaces the parser even if sequence numbers happen to match.

Status is carried in `Error { req_id: None, message: "nebula relay: <JSON>" }`,
with `host` and `connected` fields. This is deliberately an existing wire
variant. Older desktop builds display the message as a flash; the new desktop
recognizes only this prefix. Unrecognized errors retain their normal behavior.
This notice does not provide recovery UX to an old desktop executable.

The normal desktop IPC reader already owns a dedicated read task. The hidden
`_raw-attach` client had the same cancellable-read pattern and now retains the
future across input/resize ticks. `create_worktree_path` used to cancel every
250 ms and resume on the same stream; it now owns one complete read loop under
the final deadline and returns an error on timeout, abandoning that connection.
These paths do not automatically repeat the mutation.

## Regression evidence

| Test / behavior | Red observation | Green condition |
| --- | --- | --- |
| Fragmented bidirectional session reads | Original pump timed out after interleaved input | Six header/payload splits preserve reply and request ID |
| Silent handshake | Open transport waited forever | Handshake deadline rejects it |
| SSH alive, application silent | No application timeout | One unanswered Hello probe ends connection |
| Blocked writer | Deadline-disabled negative control hung | Entire transport abandoned at write deadline |
| Slow subscriber | Full queue blocked other output | Healthy frames ordered; lagging socket receives cancellation |
| Full request queue | Caller waited for capacity | Explicit not-sent error |
| EOF with queued actions | Mutation left pending | Unknown-outcome error; no actions cross three connections |
| Responsive probes, stuck RPC | Mutation pending forever | Independent request deadline |
| Reconnect/full replay/idle | Reattach-disabled negative control missed Attach | Same session restored three times; idle probe periods remain healthy |
| Continuous output | Read-biased negative control starved input | Input progresses while output remains readable |
| Desktop recovery | No connecting notice | Host notice, safe Ctrl+R, screen restored by Scrollback |
| Repeated offline notices | Counter-reset negative control hid unavailable | Exhausted retry state remains visible, including floating pane |

Negative controls temporarily changed only this worktree and were restored before
verification. The fragmented-read red run used the original pre-fix select loop.
Tests use real framing and transport code with in-memory byte streams, stores,
and client-delivery boundaries. No source test is a live laptop recovery claim.

## Integration note

Primary main has independent uncommitted Local/Remote visibility controls in
`app.rs`, `ui.rs`, and `event_loop.rs`. This branch does not contain or modify
those changes. Preserve them explicitly when integrating these overlapping files;
do not replace them wholesale from this worktree.

## Historical protocol-28 verification commands

From this worktree, keep Git fixture discovery below `.scratch/t`; otherwise
Git legitimately discovers the enclosing Nebula checkout and the non-repository
fixtures stop testing what they intend. The TUI harness now honors TMPDIR and
uses short names to remain below macOS's Unix socket path limit.

```sh
mkdir -p .scratch/t .scratch/d .scratch/r
GIT_CEILING_DIRECTORIES="$PWD/.scratch/t" \
TMPDIR="$PWD/.scratch/t" \
NEBULA_DATA_DIR="$PWD/.scratch/d" \
NEBULA_RUNTIME_DIR="$PWD/.scratch/r" \
CARGO_BUILD_JOBS=2 \
cargo test --workspace --no-default-features --no-fail-fast -- --test-threads=1

PATH="/opt/homebrew/opt/zig@0.15/bin:$PATH" CARGO_BUILD_JOBS=2 \
cargo build --release -p nebula --target-dir target/relay-candidate
```

The first workspace attempt failed before tests because of a wrong TMPDIR path.
A subsequent E2E attempt passed 29/31; the two non-repository-fixture failures
were resolved by the Git ceiling above (all three focused add-project cases
then passed). Neither required a product behavior change.

The initial subscribed snapshot now has its own 30-second deadline, even when
Hello probes succeed. Its regression failed without that deadline and passes
with it. Graceful local-client closure also has a total 10-second drain limit,
in addition to per-write limits and immediate cancellation on relay overflow.

## Historical protocol-28 verification results (2026-09-15)

- CLI unit tests: 12 passed.
- Daemon PTY/relay E2E tests: 31 passed.
- Desktop terminal E2E tests: 8 passed.
- Core unit tests: 12 passed.
- Daemon unit tests: 162 passed, including 13 relay tests (11 new regressions).
- TUI unit tests: 544 passed, including two new desktop recovery regressions.
- Total unique tests: 769. Documentation tests: no failures.

The workspace invocation passed every target except one TUI filesystem fixture:
`open_at_lands_on_the_owning_worktree_or_adds_the_repo` walks filesystem ancestors
itself, bypassing Git's ceiling, and therefore sees the enclosing checkout.
Rerunning the whole TUI unit target with ordinary filesystem fixtures outside
the checkout passed all 544 tests; Nebula state, socket and log paths remained
under this worktree's ignored scratch directory. No product change was made to
that existing fixture's repository-discovery behavior.

```sh
TMPDIR=/tmp NEBULA_DATA_DIR="$PWD/.scratch/d" \
NEBULA_RUNTIME_DIR="$PWD/.scratch/r" CARGO_BUILD_JOBS=2 \
cargo test -p nebula-tui --no-default-features --lib -- --test-threads=1
```

Two read-only review passes checked standards and spec. Findings about read/write
fairness, repeated offline notices, floating-pane visibility, initial snapshot
progress, and total client drain duration were addressed before the final suite.
