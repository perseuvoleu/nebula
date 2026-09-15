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

## Compatibility and activation

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

## Verification commands

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

## Verification results (2026-09-15)

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
