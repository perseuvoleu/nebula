# Nebula Mobile — agent-control UX and real Pi conversations

Status: ready for implementation, authorized by the user in the originating conversation.
Priority: replace the rough mobile terminal viewer with a reliable, polished agent-control application.
Scope: one authenticated owner, existing host-owned Nebula sessions, Pi structured conversations first.
UI language: Romanian. Specification language: English.
Explicit visual requirement from the owner: an exceptionally clean black-and-white, monochrome UI, with excellent UX and highly responsive behavior. This overrides the purple-accent appearance of the MVP.

## Problem Statement

The owner uses Nebula to manage coding agents across projects and wants to control the agents running on their server from an installed phone PWA, even while their laptop is closed. The deployed MVP works, but it is a small vanilla-JavaScript terminal-output viewer, not a real conversation interface. The owner explicitly requested a substantially better UX/UI and suggested Astro and the Vercel AI SDK or similar chat tooling. After documentation research, the proposed direction was React, TypeScript, Vite, shadcn/ui and AI Elements, with a Nebula-backed transport rather than a new model-calling application.

The current application has a project selector, session buttons, a periodically replaced block of terminal text, and a prompt textarea. It lacks durable structured chat history, message identity, deliberate mobile navigation, and a trustworthy representation of delivery and reconnection. Terminal snapshots contain screen chrome and do not provide reliable user/assistant message boundaries. Wrapping snapshots in message bubbles would misrepresent them as chat and is not an acceptable solution.

Two production incidents must guide the replacement:

- Only one project was initially visible because the gateway's explicit project allowlist contained one entry. The current deployment now enables four existing server projects. A redesign must not reset that configuration or silently confuse server projects with laptop-only projects.
- Selecting an existing session previously left its output at “Se încarcă…” because a JavaScript variable shadowed the sessions response before initialization. Gateway tests and JavaScript syntax checks passed; only exercising a real selected-session UI exposed the defect. A regression test now covers selection, repeated updates and stale responses. The replacement must be validated through actual browser interaction, not only a successful build or screenshots of empty states.

The server daemon, PTYs, agent authentication and checkouts are already valuable state. A UI migration must not restart all agents, change their working directories, replace their conversations, or force the owner to abandon the terminal workflow.

## Solution

Build a single-owner, mobile-first agent-control PWA with a deliberate desktop layout and a structured Pi conversation view. Keep the existing server as the execution host. Reuse its Nebula sessions and CLI authentication; do not start a parallel LLM agent behind the owner's back.

Use React + TypeScript + Vite for the interactive application, shadcn/ui for accessible application primitives, and selected AI Elements components for conversation presentation. Retain the existing private gateway as the starting integration boundary. Use AI SDK UI only where it genuinely simplifies consumption of Nebula/Pi events via a custom transport. Neither Vercel hosting nor AI Gateway/model-provider credentials are required. Astro is not selected for this predominantly interactive application; it can remain a future choice for a separate documentation/marketing site.

Present Pi conversations as real messages from an authoritative structured source. Keep terminal output as an explicitly labelled alternate view and the initial fallback for other agent kinds. Do not invent message boundaries, token streaming, tool results, delivery acknowledgements, or completion states from terminal screen diffs.

The implementation starts with a bounded Pi integration spike, then proceeds in vertical slices. A polished UI with a working, correctly labelled terminal fallback is useful, but the full feature is not complete until a real existing Pi session and a new Pi turn can be represented as structured messages without damaging the session. If live partial-text events require an extension available only to newly started sessions, document the distinction and support completed-message refresh for existing sessions; do not claim unsupported token streaming.

## User Stories

1. As the owner, I want to open an HTTPS URL or installed PWA on my phone, so that I can control server agents without opening my laptop.
2. As the owner, I want to authenticate using the existing private access token, so that I do not need to recreate credentials during the UI migration.
3. As the owner, I want a clear explanation of the token and connection errors, so that I do not confuse it with a Cloudflare or model-provider token.
4. As the owner, I want logout to clear credentials and sensitive in-memory UI state, so that someone reopening the app cannot read the previous conversation.
5. As the owner, I want all explicitly enabled server projects to be visible, so that my other projects do not disappear because of a hard-coded initial project.
6. As the owner, I want the execution host to be clear, so that I know a project or agent lives on the server rather than my Mac.
7. As the owner, I want to search or filter the visible projects and sessions locally, so that a longer list remains usable without a complex global search service.
8. As the owner, I want each session to show its name, agent kind, checkout/branch and status, so that I choose the correct conversation.
9. As the owner, I want sessions with feedback requests to be easy to recognize, so that I notice agents waiting for my input.
10. As the owner, I want empty-project, loading, failed and disconnected states to be different, so that a network failure does not look like lost sessions.
11. As a phone user, I want to open a session into a full-screen conversation and return to the session list, so that navigation is not a squeezed desktop sidebar.
12. As a desktop user, I want persistent project/session navigation beside the conversation, so that switching tasks is fast.
13. As the owner, I want refresh/back navigation to restore a valid selected project/session without losing my place unnecessarily, so that the app behaves predictably.
14. As the owner, I want a session renamed by its agent to keep the same conversation selected, so that names are not mistaken for identities.
15. As the owner, I want rapid project/session switching to discard stale responses, so that another agent's output never appears in the current conversation.
16. As a Pi user, I want my user messages and assistant replies displayed as separate messages, so that I can follow the actual conversation rather than terminal chrome.
17. As a Pi user, I want previously existing session history loaded from its authoritative source, so that using the web app does not require a new conversation.
18. As a Pi user, I want messages sent from the terminal to appear in the web conversation, so that the terminal and phone remain views of the same session.
19. As a Pi user, I want a message submitted from the phone to reach that same running agent, so that I am not paying for or controlling a duplicate agent.
20. As a Pi user, I want new genuine response data to appear while available, so that I can follow progress without manual reloads.
21. As a Pi user, I want an honest “agent working” state when partial text is unavailable, so that lack of token-level updates is not mistaken for a frozen app.
22. As the owner, I want streamed message fragments to update one message rather than create duplicates, so that the conversation stays readable.
23. As the owner, I want Markdown, lists, links and code blocks rendered clearly, so that technical answers are useful on a phone.
24. As the owner, I want to copy an answer or code block with one action, so that I do not need to select long terminal text.
25. As the owner, I want tool activity grouped separately and collapsible, so that progress does not overwhelm the final answer.
26. As the owner, I want tool errors and large tool outputs handled safely, so that a long build log does not freeze the phone or hide a failure.
27. As the owner, I want hidden application/system context excluded from ordinary message bubbles, so that metadata is not mistaken for something I typed.
28. As the owner, I want a visible switch to terminal output when structured chat is unsupported, so that Claude, Codex and Cursor sessions remain usable.
29. As the owner, I want terminal mode labelled as terminal output rather than chat history, so that the application does not imply completeness it cannot provide.
30. As a phone user, I want the composer reachable above the virtual keyboard with safe-area spacing, so that I can type and send comfortably.
31. As the owner, I want multiline editing and predictable keyboard shortcuts on desktop, so that Enter behavior does not accidentally submit an unfinished instruction.
32. As the owner, I want a separate draft for each session, so that switching conversations does not erase or cross-send my instructions.
33. As the owner, I want my unsent draft retained on transient failure, so that I can recover without retyping it.
34. As the owner, I want clear sending, accepted, waiting, streaming and failed/uncertain-delivery indicators, so that I know what actually happened.
35. As the owner, I want repeated taps on Send to avoid duplicate in-flight submissions, so that the agent is not instructed twice accidentally.
36. As the owner, I want a timeout after a possible acceptance to warn me before retrying, so that reconnection does not automatically execute my instruction again.
37. As the owner, I want background/resume and network recovery to restore the current conversation, so that locking the phone does not lose messages.
38. As the owner, I want reconnection to fetch or replay only the necessary state without duplicate messages, so that repeated mobile disconnections remain harmless.
39. As the owner, I want auto-scroll to follow new output only when I am already near the bottom, so that reading an earlier answer is not interrupted.
40. As the owner, I want an explicit “new messages / jump to latest” control when scrolled up, so that I can return to the live turn deliberately.
41. As the owner, I want to start a supported agent on a selected existing checkout with an initial task, so that I can begin work without the terminal.
42. As the owner, I want to see existing agents on that checkout before starting another, so that I avoid conflicting writers on the same branch.
43. As the owner, I want invalid or unavailable checkouts and agent kinds rejected safely, so that a stale picker cannot launch work in the wrong place.
44. As the owner, I want the newly created session selected once its identity is known, so that I can follow its first response immediately.
45. As the owner, I want the PWA to survive an app update without losing the server session, so that UI deployments do not interrupt agent work.
46. As the owner, I want offline mode to say that server actions are unavailable, so that cached UI is not mistaken for a live connection.
47. As the owner, I want conversation data and access tokens excluded from service-worker caches, so that the public app shell does not persist sensitive content.
48. As a keyboard or assistive-technology user, I want labelled controls, focus restoration and announced status changes, so that the app is usable without a mouse.
49. As the owner, I want a coherent black-and-white monochrome theme and restrained motion, so that the app feels exceptionally clean rather than assembled from demos.
50. As the owner, I want my current token, enabled projects and existing sessions preserved after deployment, so that improving the UI does not break access.
51. As the owner, I want the same carefully composed experience on a small phone, tablet and wide desktop, so that responsive behavior is not just a few emergency CSS breakpoints.
52. As the owner, I want hierarchy expressed through typography, spacing, weight and subtle grayscale surfaces instead of decorative color, so that content and actions remain the focus.
53. As the owner, I want text, controls and navigation to respond immediately even while the agent is streaming, so that the app feels fast rather than blocked by its rendering.

## Implementation Decisions

### 1. Product and module boundaries

- The domain vocabulary is: host, project, checkout/worktree, branch, agent session, agent kind, native agent session identity, conversation, message, turn, tool activity, terminal snapshot, and delivery attempt.
- A branch name or session title is a display attribute, not identity. A browser route and state key must include the host/project context and stable Nebula session identity.
- The frontend is an application client. The gateway owns authentication, enabled-project scope, safe Nebula operations, structured-history normalization and stream/reconnect semantics. The daemon remains the owner of PTYs, processes, native session association, status and checkouts.
- Prefer one externally testable gateway boundary consumed by a real browser. A narrow Pi structured-data adapter is justified for native session interpretation; do not create a general multi-provider plugin framework before a second implementation needs one.
- Preserve the existing public control capabilities while migrating. A new UI must not silently remove other supported agent kinds or their terminal fallback.
- Start with the existing gateway implementation. Change backend technology only if the Pi spike demonstrates a concrete need; document why. Introducing React does not require a new server runtime, an extra database service or a Rust daemon rewrite.

### 2. Frontend stack and design direction

- React, TypeScript and Vite are the selected app foundation. Use current mutually compatible stable package versions and commit the dependency lockfile.
- Use shadcn/ui primitives selectively for navigation, dialogs/sheets, selectors, menus and accessible controls. Use AI Elements conversation/message/composer primitives where appropriate; adapt their styles and behavior to Nebula instead of pasting a generic chatbot demo.
- AI Elements is the presentation component layer; AI SDK UI is the chat-state/transport layer. Do not confuse either with provider calls, model hosting or Vercel deployment.
- There is no requirement for Astro, Next.js, Vercel hosting, AI Gateway, Supabase, Redis or a new cloud service. Avoid dependencies whose only benefit is speculative future flexibility.
- **Visual acceptance is a first-class requirement, not optional polish.** The owner explicitly wants an exceptionally clean, black-and-white interface. Use true or near-black page backgrounds, slightly lifted black/charcoal surfaces only where needed, white primary text and subdued neutral-gray secondary text/borders. Do not retain the MVP's purple accent. No colored gradients, glows, glassmorphism, saturated decorative surfaces or oversized marketing hero.
- Establish a small coherent set of design tokens for black/white/grayscale surfaces, typography, spacing, radii, focus and motion. Use a crisp readable sans-serif UI and monospace only for code/terminal content. Prefer typographic hierarchy, alignment and whitespace to unnecessary cards, borders or badges. Keep prose at a readable maximum line length on wide screens rather than stretching text edge to edge.
- White primary actions with black labels, understated secondary/ghost actions, clear monochrome selection and high-contrast keyboard focus should form one consistent hierarchy. Status and errors must remain understandable through text and icons rather than color alone; neutral treatments are the default. No theme switcher or light theme is required.
- Responsive design must be intentional at narrow phones (320/360/390 CSS pixels), larger phones, tablets, ordinary desktop and wide desktop (up to at least 1920 CSS pixels), as well as short landscape viewports. Navigation progressively collapses into a sheet/list flow; the conversation always retains usable width. Avoid hard-coded viewport layouts that place the send action below the keyboard or require horizontal document scrolling.
- The application must feel responsive as well as resize correctly: immediate input/selection feedback, bounded rendering work during streams, no remount of the entire conversation per token, no unbounded session/history list rendering, and no blocking full-history fetch on every update. Use reduced, purposeful transitions that respect reduced-motion preferences. Loading states should reserve layout and avoid flashing/reflowing the conversation.
- Prepare a concise design overview and screenshot set showing login, project/session list, a nonempty conversation, a tool expansion, a long code block and a network/error state at phone and desktop widths. Use synthetic data for shareable screenshots. A visually polished empty state alone does not satisfy the requirement.
- The default desktop composition is navigation plus conversation, with tool details and terminal output as secondary surfaces. Narrow screens use list-to-conversation navigation, not horizontally overflowing desktop panels.
- Establish clear component states: loading, empty, ready, submitting, reconnecting, unavailable, permission failure and fatal error. Show actionable errors without rendering stack traces or raw server secrets.
- A development-only visual fixture can represent long conversations, code, tool failures and empty states. It must be deterministic, clearly separated from real data, and never masquerade as a working live integration.

### 3. Pi integration spike — mandatory first vertical slice

The worker must inspect the installed Pi documentation and version, its session persistence model, its extension/event interfaces, and Nebula's existing Pi integration before choosing the source strategy. The earlier discussion verified that Nebula's managed Pi integration reports busy/idle lifecycle and native session identity; it does not currently supply a full structured conversation to the web gateway. Nebula stores native session identity internally, but the inspected agent-show CLI output does not expose it. This is a discovery input, not permission to guess a session by title or choose the newest transcript in a directory.

The spike must answer and record:

1. How is a stable Nebula session mapped to the correct native Pi session and exact history source, including two Pi sessions in the same checkout?
2. Which structured history is persisted and at what point: complete turns, tool events, partial deltas, hidden/custom entries, branches, compaction and parent relationships?
3. How can completed history be read for an already-running session without attaching, resizing, resuming, restarting, or stealing the agent's stdin?
4. Which documented events can expose genuine partial text and tool progress? Can existing sessions provide them, or is a new/reloaded extension required?
5. How are messages submitted while the agent is busy? Which queue/steering semantics are actually supported by the installed integration? The UI must reflect supported behavior, not invent its own execution queue.
6. What happens when the Pi session switches, branches, resets, compacts, rotates or becomes temporarily unavailable?
7. What is the smallest implementation that gives reliable read/history and send behavior while leaving the existing daemon and agent CLIs intact?

Use read-only inspection of a real session only as necessary. Do not copy private conversation contents into documentation, issues, test fixtures or logs. Use synthetic/redacted fixtures for tests.

Preferred progression: read authoritative persisted structured messages for existing Pi sessions; add genuine event streaming through a documented, additive integration if available. If persisted data updates only after a message/turn completes, display working status and completed-message updates honestly until live events exist. Do not simulate streaming by slowly replaying a complete terminal snapshot.

If additional read-only CLI exposure of an already stored native identity is sufficient, prefer that over a wire-protocol redesign or direct writes to the daemon database. A database read, if considered, requires explicit rationale, read-only access and schema/version safeguards; it is not the default architecture. A managed extension that Nebula rewrites must not be edited in-place and treated as a durable fix.

If changes require replacing the running daemon or restarting real agents, finish the source implementation and tests, document the activation window, and stop before that production cutover. The worker must not kill sessions to complete a demo.

### 4. Conversation and identity contract

- Every normalized conversation has a stable Nebula session identity, agent kind, capability state, native-session association/generation and history/update cursor.
- Capabilities distinguish structured history, live partial text, tool activity, send and terminal fallback. An unsupported capability has an explicit reason; unsupported is not an empty successful conversation.
- Every displayed message has stable identity, role, ordered content parts and available timestamp/provenance. Preserve native identifiers where possible; deterministic derived identifiers must remain stable across refresh and replay.
- Normalize supported text and tool activity. Unknown native entry types are handled deliberately without crashing the conversation or accidentally displaying hidden context. Never expose credentials, system prompts or hidden internal reasoning as a default UI feature.
- Tool calls/results are correlated by a stable tool invocation identity. Bound large arguments/results and provide a controlled expansion/load mechanism; large logs must not be fully duplicated into every poll.
- Active native history branches and compaction are respected. Do not flatten abandoned branches into the active conversation as though all messages were sent sequentially. If exact current-branch state is unavailable, report that limitation rather than silently selecting the last file entry.
- Message identity and delivery-attempt identity are separate. A local pending message must reconcile with its authoritative source without producing one optimistic bubble and a second historical bubble for the same submission.
- A native conversation change invalidates/replaces the relevant cursor and snapshot deliberately; old deltas must not be appended to a new session generation.
- History access is bounded and supports progressive loading for long conversations. Client-controlled filesystem paths are never accepted as a history selector.

### 5. Gateway API and stream contract

Use the existing authenticated gateway as the public seam. Existing project/session/checkout listing and safe agent send/create operations remain available while the new frontend is introduced. The worker may version or add endpoints where necessary but must document compatibility and avoid accidental mixed-client breakage.

The contract must cover these operations, independent of final route spelling:

- List explicitly enabled projects and enough host identity for correct UI scope.
- List active sessions and checkouts for one authorized project with stable identities and existing status metadata.
- Read conversation capabilities and an authoritative, bounded message snapshot with a resume cursor.
- Subscribe to or fetch updates for a selected session, with event identity/ordering, heartbeat or connection liveness, explicit reset/resync and an error path.
- Submit one prompt to one resolved authorized session, with an identifiable delivery attempt and truthful acceptance/unknown-delivery semantics.
- Create one session on one existing authorized checkout and return, or safely resolve, its stable identity without guessing from a title that the agent may immediately change.
- Read the bounded terminal snapshot as an independent alternate view.

An SSE stream over authenticated fetch is a good default for server-to-client events, because the existing token is in an Authorization header. Native EventSource cannot simply be given that header; do not move the token into a URL as a workaround. A custom AI SDK ChatTransport or compatible UI message stream may adapt these events. It must not require replacing the host agent with an AI SDK model generation loop.

A short, bounded polling fallback is acceptable where native update events are unavailable, but the browser must consume stable message snapshots/deltas, not screen-text heuristics. Avoid spawning a CLI per token, scanning all history files on every poll, or holding a Python subprocess slot for the life of an SSE connection. Reconnection must work without an unbounded thread/subprocess leak. Preserve finite timeouts and concurrency limits for control operations.

If using AI SDK UI, verify the current transport/stream protocol with Context7. Account explicitly for an already-existing conversation, messages arriving from the terminal without a preceding web send, and reconnection when no AI SDK-initiated request exists. Do not assume the default request/response chatbot example models those cases. If SDK lifecycle assumptions add more state than they remove, retain AI Elements and implement the smaller typed conversation controller; record this justified decision rather than adding a second inconsistent history store.

### 6. Sending, status and reconnection semantics

- Input submission is explicit; disable duplicate in-flight submission for the same attempt. Empty or oversized input is rejected consistently on client and server.
- The authoritative agent status and the web connection state are separate. A disconnected browser does not imply that the agent stopped.
- Distinguish local submission, gateway/CLI acceptance, native user-message observation, agent working, partial reply, completed reply and failure. Do not label a prompt “delivered” only because it was optimistically rendered.
- Never automatically replay mutating requests after offline/resume or a timeout. If the existing command may have succeeded before its acknowledgement was lost, show “delivery uncertain” and preserve the draft/attempt for user inspection.
- The MVP does not promise exactly-once execution. If the worker introduces an idempotency mechanism, its crash boundary and reconciliation must be explicit and tested; do not add an in-memory map and claim durable exactly-once behavior.
- Web and terminal-originated messages converge into one authoritative conversation. Submission correlation cannot be based only on equal text, since the owner may intentionally repeat the same instruction.
- Changing selected sessions cancels or ignores old requests/subscriptions and restores the appropriate draft. Logout tears down subscriptions and prevents late responses from repopulating sensitive state.
- On return from background or connectivity recovery, restore current state from an authoritative snapshot or cursor and resume updates without duplicate tool/message entries. Native session identity changes require resync.
- Show stale content as stale if connectivity is lost. Keep error/uncertain-delivery notices visible until resolved rather than letting a generic “connected” poll message immediately overwrite them.

### 7. Navigation, composer and accessibility details

- Preserve selected project/session through refresh using non-secret identifiers in a URL or equivalent state. Validate restored selection against the current allowlist and live sessions. Never include bearer tokens in navigation state or URLs.
- On mobile, back returns to the session list and focus is predictable. New-session selection shows existing checkout activity before confirmation.
- The composer stays usable with the virtual keyboard and device safe areas; no horizontal document overflow at a typical narrow phone width. Textareas grow within bounds rather than pushing all navigation offscreen.
- Desktop keyboard behavior is documented and consistent: Enter submits when appropriate, Shift+Enter inserts a newline, composition/IME events do not accidentally submit. Mobile keyboards retain an obvious send button.
- Drafts survive session switches and transient failures in the current authenticated browser session. Durable disk persistence of message drafts/history is not required; do not silently add sensitive persistent storage.
- Only follow output automatically while the owner is already near the latest message. Use a jump-to-latest affordance otherwise; prepend-history loading preserves the reading position.
- Code blocks may scroll internally and have copy controls. Message content never causes page-wide overflow. Render links safely and prevent executable HTML/unsafe URLs.
- Meet accessible contrast and keyboard focus expectations; target WCAG AA for the changed UI. Use touch targets of about 44 CSS pixels for primary controls, meaningful labels, reduced-motion support and restrained live announcements rather than reading every token aloud.

### 8. Security and privacy invariants

- This remains a single-owner server-control application, not multi-tenant SaaS. Token possession permits consequential agent instructions under the server user's permissions.
- Preserve existing token configuration and the expanded four-project allowlist. Project allowlisting scopes the gateway API; it is not an OS sandbox and must not be presented as one.
- The daemon is never exposed publicly. Gateway/bridge traffic stays private, with same-origin HTTPS through the existing reverse proxy.
- Preserve constant-time token checks or an equivalent well-reviewed authentication boundary, exact allowed Host/Origin checks, no permissive cross-origin access, bounded input and safe subprocess argument boundaries without a shell.
- Keep credentials out of URLs, process arguments, logs, tracked files, screenshots and browser caches. Do not copy the production token into frontend build variables or fixtures.
- Keep the token in session-scoped storage unless a separately justified authentication migration is explicitly approved. Existing authenticated access must not become public to simplify stream handling.
- Markdown, tool output, terminal output, titles and paths are untrusted display data. Sanitize/render safely; do not enable raw executable HTML. Retain a restrictive CSP and adjust it intentionally for the actual framework/build, not with broad unsafe exceptions.
- Resolve history by authorized session identity, not user-supplied absolute paths, arbitrary transcript search, file traversal or unchecked symlinks. Fail closed on ambiguous native mapping or duplicate CLI names.
- Logout clears credentials, sensitive client state and active subscriptions. The service worker only caches public application assets; API, histories, tool outputs and mutations are never cached or queued offline.
- Keep operational resource limits on the proxy and gateway. Any long-lived streaming change must be reviewed against existing buffering/timeout middleware rather than assumed to work behind the current proxy unchanged.

### 9. Deployment and migration

- Keep the established server-hosted arrangement: user-owned gateway service, existing Nebula daemon and CLI credentials, HTTPS through the existing Traefik deployment. A new Dokploy application/container is not part of this task.
- Implement and verify in a separate task checkout. The active web deployment serves its checkout directly; editing that directory is a production change. Do not develop in it.
- Build the new frontend into production assets, with a reproducible locked dependency install and documented start/build/test commands. Never serve a development server publicly.
- Document how the gateway serves the new assets, API routes and client navigation fallback without leaking filesystem contents or allowing arbitrary static-file reads. Content-hashed assets and a coherent app-shell update strategy should avoid mixed old/new builds.
- Preserve the existing hostname, private network bridge, project configuration, token and session data. No DNS, firewall or unrelated reverse-proxy changes are needed for visual development.
- Test a candidate on a separate loopback port with an exact matching development origin and synthetic credentials. Do not turn off production authentication to run the preview.
- Provide an explicit rollback to the current working web application and retain the existing tests until equivalent behavioral coverage is demonstrated. Do not remove tests simply because the implementation language/framework changed.
- If an SSE endpoint is introduced, check response buffering and long-lived transport behavior on that route. Do not disable protection for unrelated routes or applications.
- Do not restart the daemon or real agent sessions. Restarting only the web service at an approved cutover may be acceptable, but a completed implementation is not proof that deployment occurred. The worker delivers a verified candidate and cutover instructions; the orchestrator owns live promotion.

### 10. Delivery slices and completion gates

1. **Native-source proof:** demonstrate correct Pi association, existing structured history and one synthetic/new-turn update through the gateway contract; record source/version, capability limits and mapping safeguards. No cosmetic-only completion claim.
2. **Application foundation:** typed React shell, preserved authentication and project scope, responsive navigation, checkout/session selection, composer and error states using real gateway responses.
3. **Real Pi conversation:** normalized history, message identity, tools, live or honestly labelled completed-message updates, safe Markdown and terminal alternate view.
4. **Resilience:** pending-send reconciliation, uncertain-delivery UX, stale response protection, foreground/offline recovery, scrolling, long-history bounds and capability fallback.
5. **Verification and handover:** locked build, focused automated tests, real browser smoke checks, security/deployment review, screenshots with synthetic data, migration/rollback instructions and explicit remaining gaps.

Each slice should be reviewable and committed with explicit paths. Do not mix unrelated Rust/TUI/sync/collect changes from the primary checkout into this feature. A material architecture change discovered during the spike is recorded in this specification and escalated if it would require new credentials, services, a daemon restart or a different product scope.

## Testing Decisions

### Public seams and test budget

Prefer the highest useful seam: a real browser consuming the real gateway, with a deterministic fixture substituted only at the external Nebula/Pi process or native-history boundary. Supplement this with a small adapter fixture suite for native-format edge cases that would be awkward to drive through a browser. Retain the existing gateway security/CLI tests as a regression baseline.

A good test checks user-visible behavior and external contracts, uses stable semantic selectors/roles, and fails if the original selected-session bug returns. Avoid tests tied to component internals, CSS class names, polling implementation or renamed local variables. A UI build, a mocked empty dashboard and syntax checks alone are insufficient. Synthetic fixtures must not contain copied private transcripts or tokens.

### Required automated behavior matrix

| Boundary | Required behavior |
| --- | --- |
| Gateway/auth | Missing/invalid token denied; wrong Host/Origin denied; enabled projects allowed; disabled project/session/history denied; no token or conversation cache leakage. |
| Gateway/control | Valid send/create uses argument-safe CLI control; input bounds enforced; unknown/ambiguous session or checkout fails closed; subprocess timeout preserves uncertain-delivery semantics. |
| Pi adapter | Two native sessions in one checkout map correctly; stable history IDs; user/assistant/tool entries normalize; hidden and unknown entries do not become bogus chat; absent native identity reports unavailable. |
| Pi adapter/replay | Partial trailing records, incremental append, compaction/branch/native-session change and replay cursor resets are handled without cross-session leakage or duplicated messages. |
| Browser/selection | Login → choose a non-default project → select an existing session → actual structured reply is rendered, not “loading”; session rename keeps selection and an old session's delayed response cannot overwrite a new selection. |
| Browser/conversation | Initial history plus a new user message, incremental/complete assistant update and tool completion appear once with correct roles and order; messages originating outside the web UI are also observable. |
| Browser/send | One tap sends once; repeated tap cannot duplicate in-flight work; confirmed versus uncertain delivery is distinguishable; failure preserves draft; reconnection does not resend mutations. |
| Browser/reconnect | Background/offline/resume loads authoritative state without duplicate messages, preserves the current draft and reading position, and does not let late responses repopulate UI after logout. |
| Browser/fallback | Unsupported agent shows explicit terminal mode and still supports existing safe controls; absent structured history is not reported as an empty successful chat. |
| Browser/mobile | 320/390-pixel phone, tablet and desktop layouts; short landscape viewport; safe-area/composer layout, list/back navigation, long code block, scroll-follow versus jump-to-latest, keyboard focus and no horizontal overflow. |
| PWA/security | App shell install/update works; API and POST requests are not cached/replayed; logout clears credentials; rendered untrusted HTML/links do not execute. |

This is a minimum risk-based budget, not an instruction to create redundant tests at every layer. Group scenarios through the same realistic flows where practical. Start critical regression cases red before fixing/implementing them and keep expected results independent from production calculations.

### Non-test verification

- Typecheck the frontend and run a reproducible production build, plus any gateway check appropriate to the chosen implementation.
- Run the retained gateway regression suite and replacement/retained selected-session regression.
- Exercise the built UI in a browser with a mobile viewport and desktop viewport, including a nonempty real or synthetic Pi conversation and a second project.
- Read-only verify mapping/history against a real existing Pi session when safe, without logging the contents. Send/create verification uses a dedicated disposable test session on an isolated checkout, never the owner's ongoing agent.
- If an instrumented test agent is spawned, identify it explicitly and clean up only that test session after preserving the verification results. Do not kill real agents or the daemon.
- Check actual proxy streaming behavior on a private preview if the implementation requires streaming. Report separately what is proved by fixtures, by a real session, by a desktop browser emulating mobile, and by a physical phone.
- A physical iOS/Android install/background/keyboard check may need the owner. If unavailable, list it as unverified, not passed.
- Review visual output against the owner's black/white requirement at phone, tablet and desktop sizes. Check real content density, long names, multiple projects, code/tool output, loading/error states, 200% zoom and reduced motion. Reject decorative purple carry-over, cramped navigation, accidental horizontal page scrolling, hidden send buttons, low-contrast gray text or jumpy streaming layout. Inspect responsiveness while typing during updates, not just still screenshots.
- Run one heavy build/test command at a time; use focused suites during work and one appropriate full feature suite at the end. A frontend-only change should not trigger an unrelated whole Rust/Ghostty rebuild.
- Review the completed diff for correctness, security and unnecessary abstractions, then report source commit(s), actual commands/results and remaining limitations.

## Out of Scope

- Replacing Nebula's daemon, TUI, agent execution model or installed provider authentication.
- Direct model-provider chat sessions that bypass Nebula, paid AI Gateway requirements or a second agent orchestration engine.
- Structured conversation adapters for all agent kinds in the first implementation. Pi is first; other supported agents retain terminal fallback.
- Full terminal emulation in the browser, arbitrary shell endpoints, file editing/explorer, repository mutation APIs or a general filesystem browser.
- Automatic cloning/syncing of laptop-only projects to the server, remote-host setup beyond the existing host, or making all server directories visible.
- Creating or deleting worktrees through this UI, session deletion/restarts, arbitrary permission approvals, arbitrary tool execution, or unsupported stop/regenerate controls copied from a chatbot template.
- Multi-user accounts, role-based access control, billing, organization/workspace administration or a new identity-provider migration.
- Persistent offline conversation storage, offline execution queues, push notifications, voice input, file attachments and screenshot uploads.
- A public marketing/docs site, Astro migration, Vercel hosting or a mandatory Dokploy app conversion.
- Production daemon upgrades/restarts, unsupervised deployment to the active checkout, DNS/Cloudflare changes or modifications to unrelated production services.
- Storing or displaying hidden reasoning/system prompts as a feature, and copying real private transcripts into the repository.

## Further Notes

### Current implementation facts

The starting point is the deployed single-user Python-standard-library gateway with static vanilla-JavaScript assets and an app-shell service worker. It shells out safely to Nebula's existing list/read/send/new/worktree commands. Session output is a bounded terminal snapshot updated approximately every 2.5 seconds while visible, not structured conversation history. The UI currently supports Pi, Claude, Codex and Cursor creation on existing checkouts. Bearer authentication is session-scoped in the browser; control operations check origin, project allowlist and input bounds. The gateway resolves a Nebula id to a CLI name and refuses ambiguous names; a native rename/reuse race remains and must not be worsened.

The existing regression baseline is 14 Python gateway/HTTP tests and 3 JavaScript selected-session polling tests. A real browser check also verified login, project selection, checkout picker, a nonempty live session's terminal output, service-worker readiness, mobile-width layout and logout. No physical-phone acceptance has yet been claimed. Previously the test browser was an old Playwright package with a matching cached Chromium; its incompatibility with newer Chrome was a tooling issue, not an application fix to preserve.

Use the handoff note for environment-specific paths and operational commands. It intentionally stays separate from this portable specification. The primary checkouts contain unrelated in-progress work and the current web deployment has its own checkout. Neither is this task's development directory.

### Documentation research already performed through Context7

- Astro supports React integration and hydrating a root React application. Separate islands do not automatically share React context. This supports feasibility, but not a reason to add Astro to a largely interactive control panel: https://docs.astro.build/en/guides/integrations-guide/react/ and https://docs.astro.build/en/recipes/sharing-state-islands/.
- AI SDK UI supports custom transports and reconnect endpoints. A custom ChatTransport can return a stream of UI message chunks; an HTTP transport can consume the documented UI-message stream protocol. Neither requires a new model call, but its lifecycle must fit externally running agents: https://ai-sdk.dev/docs/ai-sdk-ui/transport and https://ai-sdk.dev/docs/ai-sdk-ui/stream-protocol.
- AI Elements provides React conversation, message/Markdown, prompt input and tool-related components built on shadcn/ui: https://elements.ai-sdk.dev/.
- Context7 library identifiers used were `/withastro/docs`, `/vercel/ai`, and `/vercel/ai-elements`. Refresh documentation for the exact installed versions before implementation; do not blindly copy model-calling examples.
- The installed Pi docs and real native session schema are the authority for the integration spike. The existence of RPC/SDK/extension APIs does not mean an existing TUI process can be attached to or restarted safely as an RPC process.

### Tracker and authorization

The user requested a detailed `/to-spec` and direct implementation by Claude Fable 5.1 at medium effort. This repository is a fork with GitHub Issues disabled; a `ready-for-agent` label exists, but issue publication is currently unavailable. This committed document is the canonical implementation contract. Do not enable repository features, change visibility, or publish operational details to a different tracker merely to work around that setting. The owner can configure tracker publication separately with `/setup-matt-pocock-skills`.
