# Pi native-source spike (mobile chat redesign, slice 1)

Inspected on the server: pi-coding-agent **0.84.4** (`~/.local/share/pi-node/current/...`),
session format **v3**, Nebula CLI/daemon **0.6.0**. Note: the running daemon reports IPC protocol **28** while every
committed source here (branch, main, origin/main, the primary checkout) says 27, so the installed
binary was built from newer source; a CLI built from this branch cannot talk to that daemon.
Sources: installed `docs/session-format.md`, `docs/sessions.md`, `docs/extensions.md`,
`README.md` (message queue), `dist/core/session-manager.js`; Nebula
`crates/nebula-daemon/src/hooks/nebula_pi.ts`, `status.rs`, `registry.rs`,
`crates/nebula-tui/src/ipc.rs`. No private transcript content was copied anywhere.

## Answers

1. **Mapping.** Nebula's managed Pi extension posts `ctx.sessionManager.getSessionId()` on
   `session_start` and on every `before_agent_start`; the daemon stores it on the agent row
   (`Agent.session_id`, SQLite column `agents.claude_session_id`, reused for every kind). Pi
   persists the session at `<agent dir>/sessions/--<cwd with / replaced by ->--/<stamp>_<id>.jsonl`,
   where `<cwd>` is the worktree path. The gateway resolves **agent id → stored native id → exactly
   one file in the checkout's session directory whose header `id` equals the stored id**. Two Pi
   sessions in one checkout therefore differ by the UUID in the filename; zero or several matches,
   symlinks escaping the directory, malformed ids and header mismatches all fail closed. The
   installed `nebula agent show` JSON omits the stored id, so the gateway reads the field when the
   CLI provides it and otherwise falls back to a **read-only** (`mode=ro`) SQLite lookup guarded by
   a schema check (`NEBULA_WEB_DB`). A CLI-only addition exposing `session_id` is part of this
   branch (verified to build without zig; it could not be exercised against the live daemon
   because of the protocol mismatch above). The DB fallback is the promotion path today.
   Real check: every non-archived Pi agent on the host mapped to one file with matching header id
   and cwd; one archived pre-extension agent has no stored id and reports "unavailable".
2. **Persisted history.** Complete entries only: `message` (user, assistant with text/thinking/
   toolCall blocks, toolResult, bashExecution, custom), `custom_message` (with `display`),
   `custom`, `compaction` (`retainedTail` or `firstKeptEntryId`), `branch_summary`,
   `model_change`, `thinking_level_change`, `label`, `session_info`. No partial deltas are written.
   The first write happens only once the first assistant message exists (`_persist` buffers before
   that), so a brand-new session has no file until its first reply.
3. **Reading without touching the agent.** The file is append-only JSONL written with
   `appendFileSync`; Pi itself loads a file by taking the last entry as the leaf. The gateway reads
   bytes from a cursor offset, parses only complete lines (a partial trailing line is left for the
   next poll), and rebuilds the root→leaf path. Nothing attaches, resumes, resizes or restarts.
4. **Live partial text.** `message_update`, `tool_execution_update` etc. exist only inside the
   Pi process for extensions. Nebula's managed extension (`nebula-agent-state.ts`, rewritten from
   the daemon binary on every spawn) forwards lifecycle only. Exposing token deltas needs a new
   managed extension plus a daemon rebuild/restart, and it would apply only to sessions spawned
   afterwards. Out of scope for this candidate: the UI shows the daemon's `running` status as
   "agent working" and refreshes completed messages (each assistant turn and tool result is
   persisted when it completes, so multi-turn progress is visible without token streaming).
5. **Sending while busy.** `nebula agent send` writes the text to the PTY (bracketed paste when
   multiline) then `\r`. Pi's TUI treats Enter during a run as a **steering** message delivered
   after the current turn's tool calls (`steeringMode` one-at-a-time by default). The UI reports
   "accepted by Nebula" and then waits to observe the user message in the persisted history; it
   never invents its own queue.
6. **Session changes.** `/new`, `/resume`, `/fork` emit `session_start` again; the extension
   re-reports the new id and the daemon replaces it, so the gateway's identity cache (5 s) and the
   cursor generation (= native id) reset the conversation. `/tree` branches append entries whose
   `parentId` is an older node: the cursor detects the jump and returns a full reset. Compaction
   entries also force a reset and are shown as a notice. A missing/unavailable file returns an
   explicit unsupported reason, never an empty conversation.
7. **Smallest implementation.** `web/pi_history.py` (locate, tolerant JSONL reader, active-path
   normalization, cursor delta/reset, bounded tool output) plus gateway endpoints
   `/api/conversation`, `/api/conversation/updates`, `/api/conversation/tool` and an
   attempt-tagged `/api/send`. Bounded polling replaces SSE for now: without live events the
   authoritative data changes only per completed message, the stdlib server holds no thread per
   client, and the existing Traefik route needs no streaming changes. Existing daemon and agent
   CLIs are untouched.

## Contract summary

- Capabilities: `structured`, `reason`, `liveText: false` (+ reason), `tools`, `send`, `terminal`.
- Message: `{id (native entry id), role: user|assistant|tool|notice, timestamp, parts[]}`;
  parts are `text`, `image` (placeholder), `tool {callId, name, args, state, output, truncated}`.
  Thinking blocks and `display: false` custom messages are never emitted.
- Cursor: `{generation: native id, offset: bytes, leaf: entry id}`; updates answer
  `none | delta {messages, toolResults} | reset {snapshot}`.
- History paging: `limit` (≤ 500) and `before=<entry id>`; tool output above 6 000 characters is
  truncated inline and served in full (≤ 200 000) by `/api/conversation/tool`.
