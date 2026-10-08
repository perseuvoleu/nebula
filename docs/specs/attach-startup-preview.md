# Session attach: useful first frames (protocol 29)

## Behavior

- A newly mounted pane with no visible text shows a dim `Starting <kind>…`
  hint. An empty Scrollback response does **not** mean the child is ready.
  The hint disappears on the first visible text. Actual CLI boot time is unchanged.
- Warm same-size attachments reuse the existing bounded parser cache and request
  a sequence delta. Normal remote `connecting` notices must not obscure cached
  content. A known relay outage still shows recovery controls.
- Cold remote attachments (main or split pane) first request a compact, plain-text
  `ReadSession` preview. The preview is dim, labeled `Resuming <kind>…`, and never
  fed into either VT parser. Full Attach follows the response, an error, or a
  150 ms deadline, whichever wins. Sending both requests together is ineffective:
  the asynchronous text read otherwise queues behind the large ring replay.
- Full Scrollback replaces the preview, including when the authoritative screen
  is blank. Output from an older attachment is ignored while the preview request
  is pending. Late preview responses cannot overwrite a different attachment or
  a resized pane. Preview errors are best effort, including after fallback.
- Local attaches keep their existing direct path; compact preview overhead is
  not added to already-fast local replays. Both Ghostty and vt100 continue receiving
  the complete authoritative byte stream. No ring truncation or sequence tricks.

## Why

Optimized live measurements found ~20 ms for a local idle Pi's existing screen,
~2 ms for a shell, and ~540 ms for a full 1 MiB findl replay. The compact remote
preview painted in 72–111 ms; warm cached frames painted in ~2 ms without waiting
for the remote response. These are programmatic attach-to-TestBackend timings,
not physical mouse-to-display latency. Full remote replay is intentionally still
transferred and can finish later because preview adds a bounded round trip.

A Pi session without a live PTY returned an empty replay in 18.5 ms and took
2.20 s to produce text. The startup hint fixes the black interval, not CLI boot.
Detailed observations and reproducible temporary probes are kept in the ignored
`.scratch/attach-perf/` directory. Debug Ghostty timings differ drastically from
release and must not be used to characterize the installed release binary.

## Persistence evaluation (not implemented)

The current daemon stores raw output only in `PtySession::ring` in memory. Idle
reaping and explicit kills remove the session; natural exit removes the registered
Arc; clean shutdown drains sessions and kills them. The database stores agent
resume IDs and metadata, not a terminal screen. `ReadSession` requires a live PTY
and renders its ring on demand. There is no last-screen file to recover after a
previous daemon restart.

A future disk preview cache can be implemented without altering protocol encoding,
but must define stale-screen semantics explicitly rather than silently changing
`ReadSession` from “live screen” to potentially old output. Safe lifecycle design:

1. Save a bounded **plain-text visible screen**, its source dimensions, capture time,
   host/session identity, and app kind before idle eviction and clean shutdown.
   On natural exit, save only if the registry still points at that same PTY, so an
   old exit watcher cannot overwrite a replacement session's preview. Avoid writing
   termination/alternate-screen teardown escapes over the useful last frame.
2. Use private files (directory 0700, files 0600), atomic replacement, bounded total
   bytes and retention; delete entries when sessions/projects are deleted. Terminal
   output can contain secrets: this creates a new persistent-data surface.
3. Never replay these bytes as authoritative VT state. Display only dim text marked
   as last-known/resuming, invalidate on actual size changes, and replace on live
   output. Do not import old sequence numbers into the new daemon/PTY lifetime.
4. Choose an explicit stale-preview request/metadata contract. A new protocol event
   is cleanest and requires approval; overloading existing `SessionText` text with
   a marker is wire-compatible but changes CLI/agent-read semantics and is not
   appropriate as an unnoticed fallback. A local-only disk reader would not solve
   remote sessions and would couple the TUI to daemon storage.

This change deliberately implements the immediate-starting-hint minimum instead
of introducing persistence semantics. It cannot reconstruct screens already lost
in a prior restart. A future persistence implementation needs daemon deployment
and takes effect only for screens captured by the new daemon.

## Deferred optimized snapshot protocol

An ANSI rendered screen cannot simply replace Scrollback's ring bytes: existing
clients compute the next raw stream sequence as `base_seq + data.len()`. A
synthesized screen has a different length and omits parser history/modes. An
explicit snapshot event needs its own raw-stream end sequence, dimensions, modes,
and history policy. Merely changing the bytes without these semantics is unsafe.

## Verification and rollout

Frame tests cover startup through empty replay, preview → authoritative screen,
late old replay, warm cache visibility and first-resized-frame invalidation. PTY
e2e tests cover an agent whose first output is gated, and a silent preview server
that requires the actual TUI timer to issue Attach and suppress a late error.

TUI-only, protocol stays 29. A new `ng` uses it; no daemon restart is required.
