# Nebula Memory

Work log written by the `nebula-memory` skill. Newest first. Read this before starting a task; append
after finishing one. See `.claude/skills/nebula-memory/SKILL.md` for the entry format and the rules
about what is worth recording.

> **Provenance.** Everything dated 2026-08-04 through 2026-08-24 is a backfill, reconstructed on
> 2026-08-24 from the 152 session transcripts in `~/.claude/projects/…-nebula/`, `git log`, and the
> verified notes in that project's `memory/` directory. Prompts are quoted from the transcripts, and
> every file and symbol named below was confirmed to still exist. The **Did** lines are grounded in
> commits and code; where a session's outcome could not be verified it was left out rather than guessed.
> Entries written from here on are first-hand. The ~300-line pruning rule in the skill applies to
> ongoing appends — this backfill is deliberately over it.

## Entries

### Vim-Style Aliases In The ⌘K Command Palette — 2026-08-25

**Asked:** "cand dau cmmd k as vrea sa le pot da din comenzi ca vim simplificate gen sa scriu a sa mi
faca agent de la branch ul existent, sau aw, agent de la worktree, sau w worktree nou, toate
combinatiile astea" — later "nu vad nimic diferit in cmmd k" (the list of suggestions had been given
but nothing implemented; implemented on that follow-up).

**Did:** Commit `e857abb`. Palette rows in `open_command_palette` (event_loop.rs) are labeled
`"<alias> · <name>"` and `ContextMenu::apply_filter` (app.rs) pins the row whose label's
`split_once(" · ")` alias equals the trimmed query — a convention on the label, deliberately NOT a
new MenuItem field (MenuItem is literal-constructed everywhere). Aliases after the `3e24d77`
refinement ("vreau sa am a care e default la primary, sa am si acc,ac,ap… ab o sa ma puna sa aleg
branch"): `a` = session picker straight on the PRIMARY checkout, `acc`/`ac`/`acu`/`ap` = kind-fixed
(Claude/Codex/Cursor/Pi) name prompt on primary via `MenuAction::NewAgentOfKind` rows (prewarm
fires), `ab` = the branch-pick flow, plus aw/w/o/t/tn/s/e/g/n/td/st/ws/p/r. After `34fedbd`
("nu vreau sa mi mai ceara nume de agent" + "sa dau t si sa mi puna ala de cmmd t"): the kind-fixed
spellings skip the name prompt too — `PaletteCommand::NewAgentNow` goes straight to `create_agent`
with an empty name (generated default + auto-title, agent renames itself from its first prompt) —
and `t` opens the ⌘T floating quick terminal (`toggle_quick_terminal`); the plain terminal tab
moved to `tn`.
The `ab` flow (branch-FIRST, mirror of the orchestrator's kind-first order):
`PaletteCommand::NewAgentOnBranch` → `open_agent_branch_picker` ("Agent branch") → rows
`MenuAction::AgentOnBranch { project, branch }` → existing checkout opens `open_new_agent_picker`,
missing one sends CreateWorktree with new `PendingIntent::PickAgentOnCreatedWorktree` whose Ack opens
the picker. `PaletteCommand::NewTerminal` → `create_terminal_for_context`. Branch listing factored
into `project_local_branches` (shared with `open_branch_picker`). Tests:
`command_palette_alias_ab_runs_agent_on_branch`, `…alias_a_opens_the_session_picker_on_primary`,
`…alias_ac_starts_a_codex_agent_on_primary`,
`agent_on_branch_without_checkout_creates_worktree_then_opens_picker`; 461 nebula-tui + workspace
green; `make install` run.

Follow-up in the same session ("da nu poti da tu reload la asta fara sa inchid sa dau ng iar?"):
commit `dfa581a` added `r · Reload nebula` — `PaletteCommand::ReloadUi` sets
`App.pending_reload` + `should_quit`, `run_app` now returns `Handoff::{None,Ssh,Reload}` (replacing
the `Option<HostEntry>` hosts-picker return), and main.rs's `run_tui_and_handoff` execs
`std::env::current_exe()` with the original argv over the running process — same window, selection
restored via the SaveUiState-on-quit path, daemon untouched. e2e
`tui_reloads_in_place_from_the_palette` proves the pid survives exec and the reloaded UI answers.

**Gotchas:**
- The pin is REQUIRED, not cosmetic: plain "a" fuzzy-matches nearly every command, and "agent"
  matches both agent rows with the on-branch row winning on original order — the old
  `command_palette_new_agent_in_worktree…` test had to switch to typing the "aw" alias.
- Any test asserting palette labels asserts the alias prefix now ("w · New worktree…", not
  "New worktree…").
- Re-exec works because `make install` replaces the file via cp-to-temp + mv: the launch path
  (target/release/nebula for `ng`, the ~/.cargo/bin path otherwise) holds the NEW build while the
  running image is never touched — `current_exe()` re-resolves to fresh code.

### Orchestrator Rows Trail A Harness Badge (claude/codex/cursor/pi) — 2026-08-25

**Asked:** "aici sub agenti vreau sa scrie daca e cursor pi codex etc" (screenshot of the SESSIONS
panel — which already shows the kind; the only agent rows missing it were the ORCHESTRATORS section).

**Did:** Commit `021d864`. ui.rs orchestrator rows now read `name ◆ branch kind` — the tuple grew
`a.kind.as_str()` and a dim trailing `kind_label`. Priority mirrors the sessions panel: the harness
badge ALWAYS renders; when the panel is too narrow to keep the name ≥ `MIN_SESSION_NAME_W` (8), the
BRANCH label drops whole (never clipped).

**Gotchas:**
- First attempt dropped the KIND on narrow panels — but the default worktrees-panel width is narrow,
  so the badge the user asked for would never have shown; branch is the right thing to sacrifice.
- Two screen tests pin this row's exact text (`orchestrators_group_tops…` at default width →
  "orchest" + "◆ claude"; `orchestrators_on_branch…` at `app.panel_widths[1] = 40` →
  "boss ◆ feature claude") — any change to the row's badge order or drop rule lands in both.

### TUI Auto-Reconnects After The Daemon Dies (Frozen/Truncated Screen Diagnosis) — 2026-08-25

**Asked:** "eu cand deschid ceva fereastra se face resize la nebula si dupa terminalul imi ramane cu
textul trunk pe jumate, ce putem face" + "si mi se si inchide daemon ul cand face asta".

**Did:** Diagnosis first: the daemon never crashed — `state/daemon.log` (in
`~/Library/Application Support/dev.nebula.nebula/`) showed only clean "shutdown requested by client"
exits, and the ONLY senders of `ClientRequest::Shutdown` are `nebula kill` and `nebula upgrade`'s
`shutdown_if_idle` — so "daemon-ul se închide" was explicit kills, not the resize. The real bug: the
TUI had **no reconnect at all** — after `ConnState::Disconnected` it dropped every outbound frame
forever, the terminal pane froze on the last parsed screen, and a window resize then ran
`sync_pty_size` → `parser.screen_mut().set_size()` on the dead stream, truncating the frozen lines
mid-width ("text trunchiat pe jumate"). Fix in `main_loop` (event_loop.rs): a `next_reconnect`
deadline armed on both disconnect sites, a select arm (gated on Disconnected) that awaits
`ipc::connect_or_spawn()` (respawns the daemon when nothing listens), replaces `*channels =
ipc::split_connection(conn)`, re-sends `Subscribe`, and re-attaches `app.term` via `attach()` — whose
Attach carries the current pane size and the daemon's `resize_with_jiggle` forces a full repaint,
healing the truncation too. e2e regression `tui_reconnects_after_the_daemon_is_killed`
(e2e_tui.rs): external `nebula kill` → "✗ disconnected" appears → disappears on its own. 456
nebula-tui + full workspace green; `make install` run.

**Gotchas:**
- The `channels.rx.recv()` select arm MUST be gated `if app.conn == ConnState::Connected`: a closed
  mpsc yields `None` instantly, and ungated it spins the loop hot the entire time the daemon is down.
- Snapshot handling already anticipated reconnects (`app.open_at.take()` — "Reconnect snapshots find
  None here"), and Attach already jiggle-resizes — the ONLY missing piece was the re-dial.
- `cargo test` runs can leak an isolated e2e daemon (`target/debug/nebula daemon --foreground` with a
  socket under `/var/folders/…/.tmpXXXX/rt/`); it holds no user state — identify via
  `lsof -p <pid> | grep daemon.sock` before assuming it's the real one.

### Orchestrator UX Split From Shell Terminals, "root" → "primary" — 2026-08-25

**Asked:** "Implement the orchestrator UX simplification… Make orchestrator agents and shell terminals
clearly separate concepts, and make the orchestrator creation flow visibly different from normal session
creation. Orchestrators may run on any local branch, including develop. 'root' means the original/primary
checkout, not main branch; use 'primary checkout' in user-facing text… retaining compatibility for the
CLI selector 'root'."

**Did:** Orchestrator picker (`open_agent_picker`, event_loop.rs) lost its "Terminal (shell)" row — the
session picker keeps it — which killed the whole terminal-on-branch detour: `MenuAction::PickBranch` is
deleted, `BranchSpawn` and `DeferredSpawn` (app.rs) are now structs (agent-only, no `Terminal` variant),
and `spawn_on_branch`/the `SpawnOnCreatedWorktree` Ack arm lost their terminal branches.
`PromptKind::NewAgentOnBranch` merged into `PromptKind::NewAgent { target: SpawnTarget }`
(`SpawnTarget::Worktree | Branch{project,branch}`, app.rs) — one open_prompt arm, one submit arm. The
prompt is role-correct: title "New orchestrator [on <branch>]", label "orchestrator name (empty =
orchestrator-N)"; branch picker title is now "Orchestrator branch". Default orchestrator numbering is
first-free **project-wide**: `App::default_orchestrator_name` (app.rs, used by `create_agent` and the
prompt label) and `ipc.rs::default_agent_name` now takes `taken: &[String]` (agent_new passes
project-wide orchestrator names). User-facing "⌂ root" badge → "⌂ primary" everywhere (ui.rs
`ROOT_BADGE`, both branch pickers, worktree pickers, e2e_tui.rs); CLI `--worktree` accepts
"primary" alongside the kept "root"; root-only claims fixed in entities.rs, main.rs docs, ipc.rs, and
`ORCHESTRATOR_INSTRUCTION` (hooks/mod.rs: "stay in your own checkout… may run on any branch"). No
protocol change. 456 nebula-tui + full workspace green; built on top of the uncommitted ⌘T
quick-terminal work without touching it (its 4 tests still pass). Deliberately did NOT introduce an
`AgentRole` enum — the bool converts at CreateAgent anyway and the swap would churn ~30 test sites for
three if/elses.

**Gotchas:**
- The Esc-restores-prewarm arm in the prompt handler must match only
  `SpawnTarget::Worktree` — a branch target never fired a prewarm (nothing to warm in a
  not-yet-created checkout), so restoring one would prewarm a worktree the user never picked.
- `e2e_pty::move_agent_respawns_live_session_in_target_worktree` flaked once under the full
  parallel workspace run and passed alone and on suite rerun — pre-existing PTY-load flake, not
  related to picker/label changes.
- Tests that need a multi-branch repo for the picker: two commits in the same second sort
  unpredictably under `--sort=-committerdate`, so assert membership (`labels.contains`), not order,
  for branches without a pinned `GIT_COMMITTER_DATE`.

### ⌘T Floating Quick Terminal On The Current Branch — 2026-08-25

**Asked:** "vreau cand dau cmmd t sa mi deschida o fereastrea ( cum e la cmmd +k ) doar ca sa fie
putin mai mare si sa am terminam de la branch ul la care sunt automat fara sa aleg"

**Did:** New `Action::QuickTerminal` (keymap.rs, id `quick_terminal`, defaults `cmd+t, ctrl+t`).
`toggle_quick_terminal`/`close_quick_terminal`/`quick_terminal_worktree` (event_loop.rs, next to
`create_terminal_for_context`): ⌘T reuses-or-creates one dedicated shell tab named `"quick"` per
worktree (`CreateTerminal { name: Some("quick") }` + the existing `PendingIntent::AttachCreated`),
attaches it, and sets `App.quick_term` — `ui.rs::draw_quick_terminal` then renders the attached
screen in a `centered_rect_pct(72, 72)` floating window (above panels, below overlays/vim), writing
`app.term_area` back to its inner rect so the post-draw `sync_pty_size` sizes the PTY to the window.
`App.quick_term_restore` remembers what ⌘T displaced (attached sref + focus + lock) and the second
⌘T/^q/outside-click restores it. In the locked-terminal SUPER intercept ⌘T toggles (locked in an
agent → quick shell on that agent's worktree); the other SUPER arms close the window first, except
⌘W which deliberately targets the quick shell itself. `~/.config/ghostty/nebula`: the old ⌘T
chain-macro (worktrees + new-worktree dialog) was **replaced** by `super+t=csi:116;9u`. Four
regression tests; 453 nebula-tui + full workspace green; `make install` run (TUI-only change, old
daemon fine — `CreateTerminal.name` already existed).

**Gotchas:**
- The daemon lazily respawns dead shells in `ensure_session` on Attach, so reusing the per-worktree
  `"quick"` tab needs no aliveness check and survives restarts with the same tab identity.
- While the window is up, `draw_terminal` must NOT render the attached screen in the pane behind it
  (the PTY is window-sized — pane render is garbage) nor write `term_area`/push its hit rect: the
  window owns both. The pane shows a "quick terminal open" hint instead.
- `hit_at` is first-match, and the panel/splitter rects are pushed during the panel draw — the
  floating window's `HitTarget::TerminalPane` rect must go in with `app.hits.insert(0, …)`.
- `detach_if_attached` clears `quick_term` too — a quick shell whose session/worktree is deleted
  would otherwise leave an empty floating box with no owner.

### Global Command Palette On ⇧P / ⌘K — 2026-08-24

**Asked:** "un shortcut care sa-mi deschida o fereastra de unde pot face orice comanda — ex: deschid
direct un worktree si dupa ma intreaba branch-ul si modelul, sau deschid direct un orchestrator, sau
deschid un agent intr-un worktree existent, sau caut o sesiune; un fuzzy principal care cauta comanda,
dau Enter si ma arunca in urmatorul nivel, si tot asa."

**Did:** Commit `87372f7` on `command-palette` (chord later moved to ⌘K in a follow-up commit on the
same branch). New `Action::CommandPalette` (keymap.rs, id
`command_palette`, defaults `cmd+k, shift+p` — shift+p was free; the plain chord satisfies
`every_action_ships_with_a_reachable_chord`). `open_command_palette` (event_loop.rs, next to
`open_new_agent_picker`) builds a **filterable ContextMenu** titled "Commands" — no new overlay type;
fuzzy + Enter-chaining came free from `MenuFilter`. Rows reuse existing `MenuAction`s where they exist
(`NewWorktree`, `NewOrchestrator`, `AddProject`) plus new `MenuAction::Command(PaletteCommand)`
(app.rs): `NewAgentInWorktree` → new `open_worktree_picker_for_agent` (filterable "In worktree" picker
whose rows are plain `MenuAction::NewAgent(worktree)` → the normal kind → model/effort → name picker),
`SearchSessions`/`SearchEverything` → the two `Palette` flavors, and `GitDiff`/`Notes`/`Todos`/
`Settings`/`Workspaces` calling the same `open_*` fns their hotkeys call. Creation rows act on
`selected_project()` falling back to the first project row, and are simply omitted with no project.
Also in the locked-terminal SUPER intercept (the ⌘N/⌘D pattern), and
`~/.config/ghostty/nebula` now maps `keybind = super+k=csi:107;9u` (107 = k, 9 = super) — this
**replaced** the old `super+k=text:S` session-search mapping, and the interim
`super+shift+p=csi:112;10u` line was removed; SessionPalette keeps its `shift+s` default and its
command-palette row ("Search sessions"). ⌘P stays the terminal full-screen toggle (`zoom`). Four
regression tests; 449 nebula-tui + full workspace green.

**Gotchas:**
- kitty CSI-u mods: super-only = **9** (1 + super 8), shift+super = 10; the overlay's `csi:107;9u`
  delivers ⌘K as a real `Char('k')+SUPER` chord. `KeyChord::from_event` canonicalizes shifted
  spellings, so `Char('P')+SHIFT` and lowercase-with-mods forms land on the same chord.
- `run_menu_action` clears `app.overlay` **before** dispatching, so a `MenuAction` arm may open the
  next overlay directly — that's the whole chaining mechanism; no state machine needed.
- The `Action::CommandPalette` arm is deliberately NOT gated on `app.focus != Focus::Terminal` (unlike
  `Action::Palette`): an unlocked pane forwards nothing, and the locked path never reaches the global
  dispatch — it goes through the SUPER intercept.
- `ContextMenu::is_workspace_picker()` steals n/r/d only when the menu has `OpenWorkspace` rows, so the
  command palette (which has none) types those letters into its filter as expected.

### `nebula agent wait` — Orchestrators Block Instead Of Sleep-Polling — 2026-08-24

**Asked:** "Add a blocking wait verb to the nebula CLI so orchestrator sessions can wait on their
workers instead of hand-rolling sleep loops, and teach orchestrators to use it." (Supersedes the
earlier 'Orchestrators Must Stay In A Polling Loop Until Workers Settle' guidance from another
branch: the sleep+poll `nebula agent list` loop is now the fallback, `nebula agent wait` the
primary.)

**Did:** `nebula agent wait [<name>...] [--timeout <secs>] [--project <name>]` — `agent_wait` in
`crates/nebula-tui/src/ipc.rs`, dispatched from `crates/nebula/src/main.rs`'s `AgentCommand::Wait`
(default timeout 600s). With names it blocks until each named worker settles; without, until every
unarchived non-orchestrator worker of the project (self excluded via `NEBULA_AGENT_ID`) does.
Settled = not Fresh and not Running — Fresh counts as pending on purpose, so waiting right after
`agent new --prompt` doesn't return before the worker's first turn starts. Prints the settled rows
as JSON in `agent list`'s exact shape; nonzero exit naming the still-running workers on timeout.
No new protocol variant: it holds the `Subscribe` connection open past the snapshot and consumes
`StatusChanged`/`EntityUpserted`/`EntityRemoved` deltas, so no `PROTOCOL_VERSION` bump and it works
against an already-running daemon. `ORCHESTRATOR_INSTRUCTION` (`crates/nebula-daemon/src/hooks/mod.rs`)
now teaches: after delegating run `nebula agent wait`, do NOT end the turn or hand-roll sleep loops;
on needs_feedback, surface what the worker is blocked on. Tests:
`orchestrator_instruction_teaches_blocking_wait` (hooks) and
`agent_wait_cli_blocks_until_worker_settles` (`crates/nebula/tests/e2e_pty.rs`) — the latter drives
the real CLI binary against an isolated daemon and moves status with the OSC `9;4` progress bytes.

**Gotchas:**
- The installer's `Bash(nebula agent:*)` allow rule (`hooks/installer.rs:113`) is a prefix match and
  already covers the new verb — verified, no installer change needed.
- The e2e recipe for settling a worker without any hook: attach and have the PTY shell
  `printf '\033]9;4;3;\007'` (→ running) then `…9;4;0;…` (→ finished) — same as
  `pty_progress_sequence_drives_status_without_any_hook`. No race in the test: wait's snapshot
  covers a status flip that lands before the CLI subscribes.

### Branch Picker Fuzzy Search, Base-Branch Pick In Manual Worktree Creation — 2026-08-24

**Asked:** "Add fuzzy search to the TUI branch picker, and offer the same branch picker in the manual
worktree-creation flow… while the picker is open, typing filters the branch list with crate::fuzzy::rank…
after the name step let the user pick the base branch… (default = current behavior, so Enter-Enter stays
fast)."

**Did:** Commit `85753de` on `branch-picker-fuzzy-search`. `ContextMenu` grew `filter:
Option<MenuFilter>` (app.rs) — `Some` opts a picker into type-to-filter: `filterable()` snapshots the
full item list into `MenuFilter.all`, and `apply_filter` rebuilds `items` via `crate::fuzzy::rank`
best-first with `hover = 0`, so draw/click/Enter code needed no changes (they already operate on
`items`). Key handler (event_loop.rs `Overlay::Menu` arm): Char appends to the query (guarded on
`filter.is_some()`, so plain menus keep j/k and the workspace picker's n/r/d), Backspace pops, first Esc
clears a non-empty query. ui.rs shows ` /{query}▏ ` as a bottom border title and a dim "no match" row
when the filter empties the list. The `From branch` picker is filterable; `PromptKind::NewWorktree`
submit now computes branch+base as before but chains into new `open_base_branch_picker` ("Base branch",
filterable, hover on the default base) whose rows carry new `MenuAction::CreateWorktreeFrom { project,
branch, base }` → `CreateWorktree` with `PendingIntent::SelectCreatedWorktree`. e2e `create_worktree`
helper gained the extra `wait_for_text("Base branch")` + Enter. 617 workspace tests green.

**Gotchas:**
- Filtering by rewriting `menu.items` (keeping the full list in `MenuFilter.all`) is what kept the
  derived-`scroll_offset` draw/click lockstep untouched — no parallel "filtered indices" bookkeeping.
- An empty filtered list makes three sites panic-prone: Enter's `items[hover]` (→ `.get`), Down's
  `items.len() - 1` (→ `saturating_sub`), and Right's `items[hover]` (→ `.get`). The wheel arms were
  already saturating.
- Five event_loop tests + the e2e helper submit the worktree name prompt and expected an immediate
  `CreateWorktree`; each needed one extra Enter for the base-picker step.
- When nothing is selected in the project, the old implicit base was `None` (daemon uses the root HEAD)
  — the picker hovers the root branch row instead of inventing a "default" row; a detached default
  (`detached @ <hash>` stripped to the hash) isn't in the branch list, so it gets a leading
  "<hash> (selected)" row to keep Enter-Enter faithful.

### Cmd+P Toggles The Attached Terminal Full-Screen — 2026-08-24

**Asked:** "Implement the user's requested Ctrl+Q terminal full-screen toggle in Nebula…" Corrected
mid-task to "muta pe cmmd + p", then clarified: "indifrente dde unde se da cmmd + p vreau sa fac
full/sau inapoi la ala cu temrinal/agent".

**Did:** `Action::Zoom` became `ToggleTerminalFullscreen` while retaining config id `zoom`; its defaults
are now `cmd+p, z`. `toggle_terminal_fullscreen` in `crates/nebula-tui/src/event_loop.rs` drives both
global-panel and locked-terminal paths: attached sessions collapse/focus/lock, a second press expands
and unlocks to Sessions, and no attachment flashes instead of showing an empty pane. Help/footer labels
now describe the toggle and preserve `^q` as an advertised escape. Three focused regressions cover
panel → full-screen → back, locked terminal → full-screen, and no attachment; all 442 `nebula-tui`
tests passed. The local `~/.config/ghostty/nebula` overlay now sends `super+p=csi:112;9u`.

**Gotchas:**
- The Ghostty overlay previously mapped `super+p=text:p`, stripping SUPER so panels ran Pin and locked
  terminals forwarded `p` to the child. Cmd shortcuts that must work while locked need kitty CSI-u.
- Terminal.app still cannot deliver Cmd+P. `z` remains the stock-terminal entry fallback, while the
  hardwired Ctrl+Q and configured Ctrl+]/Ctrl+Esc/Ctrl+Left bindings remain safe exits.

### Orchestrators Must Stay In A Polling Loop Until Workers Settle — 2026-08-24

**Asked:** "adauga undeva in memorie in nebula ca daca esti orchestrator tre sa ramai in loop, gen sa
ti dau cu timeout sleep ca sa verifici mereu" + "ceva de genul are herdr, cauta intai".

**Did:** Recorded as a standing working rule (no code changed). **If you are a nebula orchestrator
session, do not end your turn after delegating** — a finished turn means nobody watches the workers.
Stay in a blocking poll loop instead: `sleep <seconds>` then `nebula agent list`, repeated until every
delegated worker leaves `running` (→ `finished` or `needs_feedback`), then report/act. Use generous
sleeps (30–60s; the Bash timeout allows up to 600s per call) so the loop is cheap. On
`needs_feedback`, surface what the worker is blocked on to the user instead of waiting forever.

**Gotchas:**
- This mirrors herdr's model, where waiting is first-class and blocking: `herdr agent prompt …
  --wait --timeout 120000` and `herdr wait agent-status <pane> --status done --timeout 120000`
  (`~/.claude/skills/herdr/SKILL.md`, also `~/Desktop/herdr/skills/herdr/SKILL.md`). Nebula has no
  `nebula agent wait` verb — until one exists, `sleep` + `nebula agent list` is the substitute.
- Herdr's rule "inspect before waiting" applies here too: check `nebula agent list` once before the
  first sleep — a worker can finish faster than your first interval.

### E2E Tests Updated For The Split Orchestrator/Worktree Focus — 2026-08-24

**Asked:** "Fix the 3 failing e2e tests in crates/nebula/tests/e2e_tui.rs: tui_note_modal_crud_and_badge,
tui_projects_worktrees_agents_navigation, tui_pull_request_row_leads_the_links_group… Update the tests to
drive the new two-section focus model… rather than changing the app behavior."

**Did:** Commit `3566f2c` on `fix-e2e-focus-regression`. Added `FOOTER_ORCHESTRATORS` ("n: new
orchestrator", from ui.rs's Orchestrators footer arm) and, at every Enter-on-a-project site, wait for it
then Tab to reach Worktrees; the nav test's Tab cycle grew the extra Orchestrators stop. e2e suite and
full workspace green.

**Gotchas:**
- The focus cycle is now Projects → Orchestrators → Worktrees → Sessions → Terminal (`event_loop.rs`
  ~875); Enter on a project lands on Orchestrators, and any e2e script that then presses Enter again
  hits `Action::Activate`'s Orchestrators arm — on an empty section that's the "+ new orchestrator"
  placeholder and opens the picker, not the Sessions drill the old flow expected. Tab (or `j` past the
  section's last row) is the way down to Worktrees.
- `←` (FocusLeft) still maps Sessions → Worktrees and Worktrees → Projects, so the tests' existing
  back-navigation needed no changes — only the forward Enter/Tab paths.

### Orchestrators And Worktrees Have Independent Focus — 2026-08-24

**Asked:** "secitunile astea ar treb sa fie independete cand dau click pe una sau alt se se faca doar pe cea care dau click albastra si sa aiba propriile n, ctrl n etc"

**Did:** `Focus::Orchestrators` is now separate from `Focus::Worktrees` (`app.rs`), and `ui.rs` styles only the active half's header, rows, and focus tint with the accent color. Mouse clicks and keyboard traversal activate the corresponding half while retaining both independent cursors. `Action::New` and `Action::NewAgent` route `n`, `ctrl+n`, and `cmd+n` by the active half: orchestrator picker above, worktree prompt below. Full `nebula-tui` suite: 430 tests green.

**Gotchas:**
- `sel_orchestrator` used to double as both cursor and section-focus state (`None` meant WORKTREES). With a real `Focus::Orchestrators`, both cursors must remain populated while inactive; creation and styling must branch on `app.focus`, not cursor presence.
- Adding a focus enum variant requires auditing every focus cycle, mouse/context-menu dispatch, footer/render branch, and exhaustive match; this is why the change spans `app.rs`, `event_loop.rs`, `keymap.rs`, and `ui.rs` rather than being only a color tweak.
### Branch-Aware Orchestrator Creation With A Scrolling Branch Picker — 2026-08-24

**Asked:** "cand sunt pe orchestrator si dau n / ctrl+n vreau sa pot sa aleg si terminal (ca la
sesiuni) dar in plus vreau sa pot sa pun de la ce branch sa plece, o lista scrolabila ordonata dupa
branch-urile cele mai noi (nu worktrees) si sa fie focus pe primul" + "in lista de orchestratori
trebuie sa zica si pe ce branch e".

**Did:** Commit `7efafac` on `orchestrator-branch-picker`. Orchestrator flow is now kind →
model/effort → **branch** → name: new `crates/nebula-tui/src/branches.rs::local_branches` (git
`for-each-ref --sort=-committerdate refs/heads/`), `open_branch_picker`/`spawn_on_branch` in
event_loop.rs, `MenuAction::PickBranch`/`SpawnOnBranch` + `BranchSpawn`, `PromptKind::NewAgentOnBranch`,
and `PendingIntent::SpawnOnCreatedWorktree(DeferredSpawn)` (name prompt first, worktree create on
submit, session create on the worktree's Ack). Orchestrator picker gained the "Terminal (shell)" row
(routed through the branch step); `new_agent_shortcut` (⌘N/^N) opens the orchestrator picker when
`in_orchestrator_section()`. ContextMenu draws/clicks through a derived `scroll_offset(visible)`
(no stored state — ui.rs draw and the mouse handler recompute the same value; wheel moves hover).
`project_orchestrators()` (app.rs) now spans every worktree of the project, orchestrator rows render
their worktree's branch dim after the ◆ badge, and daemon `set_agent_orchestrator` (registry.rs)
dropped the root-checkout refusal. No PROTOCOL_VERSION bump. 611 workspace tests green.

**Gotchas:**
- **No protocol change was needed for "check out an existing branch":** `git.rs::add_worktree`
  already falls back from `-b <branch>` to a plain `git worktree add <path> <branch>` when the branch
  exists — so `CreateWorktree { branch: <existing>, base: None }` checks the existing branch out. The
  TUI must still prefer a worktree that already has the branch (git refuses double-checkout).
- The relaxation of `SetAgentOrchestrator` is daemon-side behavior: a running old daemon still
  refuses promotion off-root until `nebula kill` + relaunch. Everything else in this feature is
  TUI-only and works against the old daemon (v25 handshake unchanged).
- ContextMenu never scrolled before — items past the frame were silently clipped and clicks used
  `row - area.y - 1` raw. Any scroll scheme must keep draw and click in lockstep; deriving the offset
  from `hover` alone avoids adding a field to the widely-literal-constructed `ContextMenu`.
- In the orchestrator pill row the name-truncation math must subtract 3 (selection rail `▌` + the
  2-cell status dot), not the 2 the neighboring worktree rows use — with 2 the trailing branch label
  loses its last character. Panels have fixed default widths, so screen tests should assert a name
  prefix + "◆ <branch>", not the full string, even on a 160-col backend.
- Prewarm in the branch flow fires only once the branch resolves to an existing worktree
  (`spawn_on_branch`); a not-yet-created checkout has nothing to warm, and the Esc-restores-prewarm
  arm only matches `PromptKind::NewAgent`, which is correct for that reason.
- Test committer-date ordering with `GIT_COMMITTER_DATE` env on the commit — two commits in the same
  second sort unpredictably under `--sort=-committerdate`.

### Claude Spawns In Yolo Mode Too — 2026-08-24

**Asked:** "vreau cand se face un agent claude/codex sa fie ca in herdr, cu yolo mode ambele"

**Did:** `agent_spawn_command` in `crates/nebula-daemon/src/registry.rs` now appends
`--dangerously-skip-permissions` for `AgentKind::Claude` on every spawn (fresh and resume), matching
codex's `--yolo` and cursor's `--force`. Both spawn-shape tests updated; `make install` run (daemon
restart pending on the user's `nebula kill`).

**Gotchas:**
- Herdr has no yolo machinery of its own — its manifests only *detect* permission prompts; agents run
  yolo there because the launch command carries the flag. So "like herdr" here just means "spawn with
  the skip-permissions flag".
- The flag composes fine with `--resume <sid>` and `--model/--effort`; it goes right after the resume
  args, same slot as codex's `--yolo`.

### Six Feature Branches Merged Onto merge-train — 2026-08-24

**Asked:** "Merge these five branches into merge-train, one at a time, in this order:
searchable-session-names, cmd-d-diff, attention-queue, macos-notifications, session-message-preview…
resolve them so BOTH features survive… After all five are merged, run 'cargo test --workspace'…"
Follow-up: "Merge the branch todo-notes into merge-train… expect conflicts with the already-merged
work — resolve so both sides survive."

**Did:** Landed on main as `e26f173` (via the `merge-train` worktree branch; 601 workspace tests green
on main after). All five merged (`1e2e3b2`, `546b83a`, `d4926e4`, `beb4683`, `b9cdb18`), 593 workspace tests
green. Despite all branches touching event_loop.rs/keymap.rs/ui.rs/app.rs, every code file auto-merged —
the only conflict in all four conflicted merges was `.claude/MEMORY.md`'s entries section (resolved by
keeping every entry). Then `todo-notes` merged (`5c2abe7`): one code conflict, the `nebula_core` import
list at the top of `event_loop.rs` (HEAD added `AgentStatus`, todos added `TodoId, TodoOwner` — union),
plus the usual MEMORY.md entries clash. 601 workspace tests green after.

**Gotchas:**
- `cmd-d-diff`'s code commit `bc8d58e` is byte-identical to main's HEAD `8bee485`
  (`git diff 8bee485 bc8d58e -- crates/` is empty) — the same work landed on main separately. The merge
  contributes only the MEMORY.md entry; don't hunt for a second Cmd+D implementation.
- Concurrent same-day branches conflict on MEMORY.md every time, since each appends at the same spot
  under `## Entries`. Resolution is mechanical: keep both blocks, drop the markers.
- The feared double `PROTOCOL_VERSION` bump never materialized: base and merge-train HEAD were both 24
  and only todo-notes bumped to 25, so git auto-merged a single correct bump. Sqlite migrations also
  stayed sequential (todos = migration 20). Verify with `git show <ref>:crates/nebula-core/src/protocol.rs`
  against the merge-base before hand-editing anything.
- Deleting merged worktrees with raw `git worktree remove` leaves ghost rows in nebula's Worktrees
  panel — the daemon tracks worktrees in its own sqlite and only forgets them via its delete flow
  (`d` in the TUI; there is no `nebula worktree delete` CLI verb). Harmless: the daemon's
  `git.rs::remove_worktree` tolerates an already-gone checkout, so `d` on a ghost row succeeds.
- The final `git merge merge-train` in the root checkout conflicted only on `.claude/MEMORY.md`
  (yolo-mode entry vs merge-train entries — keep both); `registry.rs` auto-merged cleanly despite both
  sides touching `agent_spawn_command`.
- `e2e_tui::tui_projects_worktrees_agents_navigation` passes now — the long-red `FOOTER_TERMINAL_LOCKED`
  assertion was fixed on main in `67ba923`; earlier entries calling it "still unfixed" are stale.

### Orchestrator-Delegated Sessions Get Task-Derived Names — 2026-08-24

**Asked:** "when a project orchestrator creates a worktree and/or creates agent sessions on that
worktree through the injected Nebula orchestration CLI, it should be able to assign meaningful names
derived from the delegated session/task so those worktrees and sessions are easy to find via search."

**Did:** Commit `802aaea`. `title_from_prompt` in `crates/nebula-tui/src/branch_name.rs` (first
non-empty line, filler words dropped, first 4 words Title Cased) feeds `default_agent_name` in
`ipc.rs::agent_new`: an unnamed `--prompt` spawn is now named after its task instead of `agent-N`.
`--name` on `nebula agent new` takes multiple words unquoted (`num_args = 1..`, joined in `main.rs`),
and `ORCHESTRATOR_INSTRUCTION` (`hooks/mod.rs`) now teaches naming worktrees/sessions after the
delegated task. Worktree naming needed no code — `worktree new <name>` was already free-text→branch,
and the palettes already search `{project}/{branch}/{name}`.

**Gotchas:**
- Deliberate: a prompt-derived name does NOT clear `auto_title` — only an explicit `--name` counts as
  an assignment, so the worker may still retitle itself from the same prompt. Don't "fix" that as a bug.
- This also changes the manual flow `nebula agent new --prompt … ` (no `--name`): humans get the
  derived title too, same code path — intended, not creep.
- `hooks::tests::orchestrator_instruction_teaches_task_derived_naming` pins prose substrings
  (`--name`, `search`, `derived from --prompt`) — copyediting the brief means updating it.
### ⌘D Opens The Diff Viewer From Anywhere — 2026-08-24

**Asked:** "Cmd+D should open the git diff viewer for the currently selected worktree regardless of
which Nebula panel has focus, and it must also work while an agent terminal is input-locked… update
the Ghostty nebula overlay so Cmd+D reaches Nebula and overrides Ghostty's default split shortcut…
Keep plain g working."

**Did:** `bc8d58e`. `Action::GitDiff` defaults grew `cmd+d` next to `g` (`keymap.rs`), a `GitDiff`
arm joined the locked-terminal SUPER intercept in `event_loop.rs` (unlock → Sessions →
`open_diff_view`, same shape as ⌘N/⌘E/⌘W), and `~/.config/ghostty/nebula` got
`super+d=csi:100;9u` — which by existing overrides Ghostty's default `new_split:right`, no unbind
needed. `GitDiff` was already dispatched from the Global scope table, so panel-independence needed
no dispatch change — only the chord and the locked-mode intercept. Tests:
`cmd_d_opens_the_diff_viewer_from_any_panel`, `cmd_d_inside_a_locked_session_opens_the_diff_viewer`,
`plain_d_inside_a_locked_session_still_reaches_the_child`.

**Gotchas:**
- Adding a second default chord to `GitDiff` broke two hardcoded label assertions in
  `event_loop.rs` settings tests: one expecting `"g"` (now `"g ⌘d"`) and one expecting `"—"` after
  `g` is stolen (now `"⌘d"` — the ⌘ chord stays). Same class of breakage the ⌘E entry warned about;
  it applies to *any* test asserting a keymap label, not just the duplicate-chord one.
- The `csi:<codepoint>;9u` recipe from the ⌘W entry worked unchanged (`d` = 100). Real repo test
  fixtures already exist: `test_repo` + `seed_repo_tree` in `event_loop.rs` tests build a git repo
  a diff test can actually open.
### Space Jumps To The Oldest Session Needing Feedback — 2026-08-24

**Asked:** (spec-driven task, `SPEC-attention-queue.md`) One key (default `space`) that jumps straight
to the session that has been waiting on the user the longest, attaching its terminal — "Today the user
visually scans red status dots across projects; with this they just hit the key and answer sessions one
by one."

**Did:** New `Action::NextAttention` (`keymap.rs`, id `next_attention`, NAVIGATE group, default
`space` — free, no chord collision). `next_attention` in `event_loop.rs` (above `jump_to_target`) picks
the unarchived `NeedsFeedback` agent in the open workspace with the smallest `status_changed_at` (0 =
oldest) and calls `jump_to_target(app, PaletteTarget::Session(id), true, out)`; empty queue flashes
"nothing needs your feedback". Three regression tests
(`space_jumps_to_the_oldest_session_needing_feedback`, `space_with_nothing_blocked_flashes_and_stays_put`,
`space_reaches_a_blocked_orchestrator`).

**Gotchas:**
- `jump_to_target`'s `PaletteTarget::Session` arm was broken for orchestrators all along: they are
  excluded from `visible_session_rows`, so a palette pick of an orchestrator fell into the
  "session no longer exists" fallback instead of attaching. Fixed in that arm — an orchestrator pick now
  lands `app.sel_orchestrator = Some(i)` (its `project_orchestrators()` index) and attaches. This fixes
  `/` and ⌘K orchestrator picks too, not just the new key.
- Inside `jump_to_target` the `attach: bool` parameter shadows the free `attach()` fn in the value
  namespace — call it as `self::attach(…)` there.
### macOS Notifications When An Unfocused Window Needs You — 2026-08-24

**Asked:** "Implement the feature specified in SPEC-macos-notifications.md" — post a macOS notification
("nebula — <project>/<agent> needs feedback") when the window is unfocused and an agent flips to
needs-feedback or finishes a run.

**Did:** Focus tracking via crossterm `EnableFocusChange` in `setup_terminal`/`restore_terminal` +
`Event::FocusGained/FocusLost` → `App.window_focused` (default `true`; terminals that never report focus
stay silent). The gate is the pure `should_notify(prev, next, focused)` in `event_loop.rs` (any →
NeedsFeedback, Running → Finished), called from the `ServerEvent::StatusChanged` handler;
`notify_status_change` rate-limits 30s per agent via `App.notified_at: HashMap<AgentId, Instant>`,
resolves `<project>/<name>` through `app.tree`, and `post_notification` spawns fire-and-forget
`osascript -e 'display notification …'` (quotes/backslashes escaped). Config toggle `notifications: bool`
(default on) with a settings row in the Sessions tab, following the `animations` pattern end to end
(save_to / value_label / cycle / apply_setting_at).

**Gotchas:**
- `post_notification` is inert under `cfg!(test)` as well as off-macOS — without that, running the suite
  on this Mac posts real notification-center toasts. The tests' observable seam is the `notified_at` map.
- In the `StatusChanged` handler, compute `should_notify(a.status, …)` *before* overwriting `a.status`,
  and call `notify_status_change(app, …)` only after the `iter_mut` borrow ends — it needs `&mut App`.
### Last-Message Preview Under The Selected Session Row — 2026-08-24

**Asked:** (spec-driven task, SPEC-session-message-preview.md) "When a Claude session row is selected in
the SESSIONS panel, show a dim one-line sub-row under it with the agent's last message (truncated) — so
the user learns 'what does this agent want from me' without attaching."

**Did:** New `crates/nebula-tui/src/transcript.rs`: `transcript_path` (cwd → `~/.claude/projects/<slug>/
<sid>.jsonl`) and `last_assistant_text` (tail-reads the last 64 KB, scans lines in reverse for the newest
`type=="assistant"` turn with non-empty text blocks, collapses whitespace). Cache is
`App.preview: Option<SessionPreview>` (agent + transcript mtime + text) refreshed by a debounced
`pending_preview` (`schedule_preview`/`fire_pending_preview` in `event_loop.rs`, the `pending_prewarm`
idiom; `StatusChanged` on the selected agent re-arms it). `ui.rs::draw_sessions` renders the sub-line
under the selected agent row only, copying the worktree panel's `created_from` sub-line (selection fill +
`▌` rail), with the virtual-row layout, scroll bookkeeping, and the row's hit rect all one line taller.

**Gotchas:**
- The transcript slug flattens **every** non-alphanumeric char of the cwd to `-`, not just `/` — verified
  in `~/.claude/projects/`, where `/Users/andrei/.herdr-worktrees/…` lands as `-Users-andrei--herdr-…`.
- `schedule_preview` must not re-arm once a read answered for the selected agent (even a "no transcript"
  answer), or the loop wakes and stats the file every 250 ms forever; `StatusChanged` is the explicit
  re-arm for "the turn ended, the line is stale". The early-return for already-armed-same-agent is what
  keeps that StatusChanged re-arm from being cancelled by the pre-draw scheduler.
- Sessions-panel pills stack on a `PILL_H`(=2) stride but `SessionEntry::height()` is 3 (pads overlap):
  the sub-line takes over the selected pill's bottom pad row, so layout advances `PILL_H + 1` for that
  row and `content_h`/cursor-visibility use an `entry_h` that adds the extra line — using bare
  `e.height()` there under-scrolls when the selected row is last.
### First-Class Todos With Per-Todo Notes — 2026-08-24

**Asked:** "Implementează un sistem first-class de TODO-uri în Nebula, separat de Notes existente.
Cerința utilizatorului: TODO-uri scoped per proiect și/sau worktree, afișate într-un UI navigabil
sus/jos; Enter pe TODO să deschidă/selecteze detaliul lui, unde utilizatorul poate adăuga și vedea
Notes copil sub acel TODO. … Agenții trebuie să poată accesa și modifica TODO-urile și notele lor
printr-un CLI clar (list/add/done/reopen și comenzi pentru notes asociate…) …"

**Did:** `Todo`/`TodoOwner`/`TodoId` (entities.rs, ids.rs), child notes as `NoteOwner::Todo(TodoId)` —
notes CRUD/protocol reused wholesale for them. Migration 20 (store.rs): new `todos` table + a `notes`
rebuild adding `todo_id`; PROTOCOL_VERSION 24→25 with `CreateTodo`/`UpdateTodo`/`SetTodoDone`/
`DeleteTodo` and `Snapshot.todos`. Store fns mirror notes (`open_todo_count_for_agent` drives a new
`todos_instruction` in hooks/mod.rs, composing with the notes one; installer adds
`Bash(nebula todo:*)`). TUI: `Action::Todos` on `shift+e` (next to Notes' `e`), `Overlay::Todos`
(app.rs `TodoView` — `detail: Option<TodoId>` is the drill-in mode, `note_selected` its own cursor;
event_loop.rs handler, ui.rs draw with a pinned bold todo header and the panels' ✎/✓ badge for child
notes), "Todos" rows in the project/worktree context menus, click-on-selected-row opens detail (the
settings idiom). CLI `nebula todo list|add|done|reopen|show|note|note-done` (ipc.rs `run_todo`) with
`run_notes`' exact target resolution. Tests: store CRUD/cascade/count/migration-20, hook injection,
4 event_loop modal tests; verified live against an isolated daemon + tmux TUI capture.

**Gotchas:**
- The old `notes` CHECK (`(project_id IS NULL) <> (worktree_id IS NULL)`) can't take a third owner
  column — migration 20 rebuilds the table with `(a IS NOT NULL)+(b IS NOT NULL)+(c IS NOT NULL)=1`
  (FK-off rebuild window, the migration-14 procedure). The `todos` table NAME is free again only
  because migration 15 renamed the original one to `notes` — a fresh DB replays both, which reads
  odd but is correct.
- Extending `NoteOwner` fans out further than the store: exhaustive matches in event_loop.rs
  (`apply_removal` — project/worktree removal must prune todos FIRST and then notes via the collected
  todo ids, a two-step cascade — plus the notes-modal owner-gone check) and `open_notes_for_owner`.
  Grep `NoteOwner::` before touching that enum again.
- `open_note_count_for_agent` needed no change to exclude todo-owned child notes: its
  `(n.worktree_id = w.id OR n.project_id = w.project_id)` is false when both are NULL. Asserted in
  `user_prompt_submit_injects_todos_instruction_while_open` so nobody "fixes" it into counting them.
- The Projects footer at 140 cols was ALREADY over budget in the disconnected state; adding
  "⇧E: todos" pushed "m: menu" off-screen and failed `splash_footer_lists_only_keys_that_work`.
  Reclaimed width by folding "e/⇧E: notes/todos" and dropping "-: divider" from that footer (still in
  the m menu, help, and the divider row's own footer). Any new footer verb needs this width math.
- `shift+t` was taken (NewTerminal's second default) — todos went to `shift+e`, deliberately adjacent
  to Notes' `e`.
- zsh in the Bash tool does not word-split `T="tmux -L sock"; $T cmd` — spell tmux invocations out in
  the screenshot harness.
- Protocol/entity change ⇒ the running daemon must be restarted (`nebula kill`, kills live sessions)
  before a rebuilt TUI/CLI can talk to it; the v24/v25 handshake refusal is clean and says so.

### ⌘W Closes The Selected Session — 2026-08-24

**Asked:** "ok e ok cand dau cmmd w si s pe un agent vreau sa mi inchida acea sesiune" — followed by "si
daca dau tab imi muta pana la agent e ok dar dupa daca nu sunt in agent sa scriu tab ul ar treb sa fie
circular sa se duca iar la projects etc". Earlier in the session: does copy-on-select "da alarma UI"?
(answer: only the footer flash in `copy_selection`, no popup).

**Did:** New `Action::CloseSession` (`keymap.rs`, id `close_session`, defaults `cmd+w, ctrl+w`, SESSIONS
group). `close_session_shortcut`/`close_session` in `event_loop.rs` (next to `archive_agent`): an agent
is archived (PTY released, `u` restores — no confirm), a shell terminal gets the existing close-confirm
dialog, links flash. Also fires inside a locked terminal via the ⌘-intercept block (the ⌘N pattern):
closes the *attached* session, unlocks, focus → Sessions. Three regression tests
(`cmd_w_closes_the_selected_agent_session`, `cmd_w_inside_a_locked_session_closes_it_and_returns_to_panels`,
`tab_cycles_back_to_projects_from_an_unlocked_pane`).

**Gotchas:**
- "cmmd +w inca nu inchide acea sesiune, de ce?" had two stacked causes, neither in the new code:
  (a) the running nebula (16:06) predated the built binary (16:11) — check `ps -o lstart= -p <pid>`
  against the binary's mtime before debugging; (b) the `ng` Ghostty overlay mapped
  `super+w=text:w`, so ⌘W arrived as plain `w` = Workspaces. Fixed to `super+w=csi:119;9u`
  (kitty-encoded cmd+w, mods 9 = super+1), which also reaches the locked-session SUPER intercept.
  **Any ⌘ chord that must arrive AS ⌘ in the ng window needs a `csi:<code>;9u` mapping, not `text:`.**
  Also: `ghostty +validate-config` and `+show-config` print nothing and exit 1 in the agent sandbox
  even on an empty file — validate config changes by relaunching, not via the CLI here.
- The Tab ask needed **no code**: `Action::FocusNext` has wrapped Terminal → Projects since the initial
  commit, and Tab-focusing the pane never locks input (only Enter/click do). Proven by running the new
  tab test against unmodified HEAD. If the user still sees Tab stop at the agent, the running binary is
  stale — `make` + restart (the `~/.cargo/bin` symlink gotcha).
- The shared tree flipped under this task twice: `cargo test -p nebula-tui event_loop` showed 77 failures
  and later `worktree_panel_len` missing from `app.rs` — all from another agent's in-flight orchestrator
  work. Isolating the diff in a `git worktree add <scratchpad>/wt-check HEAD` and re-applying only these
  edits proved it green (204 event_loop + 18 keymap); the main tree later caught up and passed too.
- Copy-on-select in the terminal pane already exists (`finish_selection` → `pbcopy` on mouse-up,
  double-click word via `select_word_at`) — the Herdr-style ask from this session was already built;
  what nebula does NOT have is text selection outside the terminal pane (lists, diff, notes).

### Pi As A Fourth Agent Kind, Via A Managed Extension — 2026-08-24

**Asked:** "adauga si optiune pentru PI cand faci un agent" — then "vezi ca si herdr poate integra, am
herdr pe desktop citeste" (herdr on the Desktop already integrates pi; read it).

**Did:** `AgentKind::Pi` end to end. Spawn shapes in `registry.rs::agent_spawn_command`: fresh `pi`,
resume `pi --session <sid>` (pi's `--resume` is an interactive picker, not an id flag), model
`--model`, effort maps to `--thinking` (off…max, see `PI_EFFORTS` in `nebula-tui/src/config.rs`); no
skip-permissions flag — pi has no permission prompts. Status/injection: pi speaks neither hooks
dialect — it has a TypeScript extension API — so `hooks/nebula_pi.ts` (installed by
`installer::install_pi_extension` into `$PI_CODING_AGENT_DIR`-or-`~/.pi/agent` under
`extensions/nebula-agent-state.ts`, global so one install covers all worktrees) maps `session_start`
→ SessionStart, `before_agent_start` → UserPromptSubmit, `agent_settled` (gated on `ctx.isIdle()`) →
Stop, POSTed to a new `/api/hooks/pi` route on the **injectable** path: the extension parses the
response's `hookSpecificOutput.additionalContext` and returns it as `{message: {customType, content,
display: false}}` from `before_agent_start`, so pi gets auto-title/notes/orchestrator like
claude/codex. Picker row, kind_label, settings rows (PiModel/PiEffort), and tests updated. Full
lifecycle verified against a fake hook server driving real `pi` 0.83.0 in tmux: SessionStart with
real session_id → UserPromptSubmit → Stop, and the injected message didn't fault the turn.

**Gotchas:**
- Herdr's own pi integration (`~/Desktop/herdr/src/integration/assets/pi/herdr-agent-state.ts`) is
  the reference for pi's extension API: `pi.on("session_start"|"agent_start"|"agent_settled")`,
  `ctx.sessionManager.getSessionId()/getSessionFile()`, `ctx.isIdle()`, and the `ctx.mode !== "tui"`
  gate (RPC mode lies with `hasUI=true`). Event/result types live in
  `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/types.d.ts`.
- `before_agent_start` (not `agent_start`) is the UserPromptSubmit analog — it fires after prompt
  submit, its async handler is awaited before the loop starts, and its return value is the only
  context-injection channel (`BeforeAgentStartEventResult.message`).
- `install_pi_extension` refuses when `~/.pi/agent` doesn't exist (pi never ran) instead of
  conjuring a config tree; the spawn degrades to "no status", same as other hook-install failures.
- When smoke-testing pi headlessly: `pi < /dev/null` under `script` never enters TUI mode (no tty on
  stdin) and the extension stays silent — drive it in tmux like the cursor recipe.

### Project Orchestrators: CLI Verbs, Injected Cheat-Sheet, Own Panel Group — 2026-08-24

**Asked:** "as vrea cumva sa am si niste agenti principali sub un proiect care sa faca ei worktree uri
etc sau sa managerieze sesiuni, cum face si herdr… si vreau sa fie si un ux/ui bun sa nu mai adaugam un
rand nou" — refined to "asta ar treb imaprtita sus orchestratori si jos worktrees, si dupa sa faca
skills pentru panels etc ca la herdr", with "ok dar sa pot sa fac si ca acum wt etc" (manual flows stay).

**Did:** Herdr-style control surface + a first-class role. `Agent.orchestrator` (entities.rs, sqlite
migration 19, `PROTOCOL_VERSION` 22→23); `CreateAgent` grew `orchestrator` + `prompt` — the prompt rides
the CLI's positional argv on fresh spawns only (`agent_spawn_command`, resume drops it; prompt also
forces the cold path since a warm CLI can't take argv). New CLI verbs in `nebula-tui/src/ipc.rs`:
`nebula worktree new <name> [--from] [--project]`, `nebula agent new [--worktree|--orchestrator]
[--kind/--model/--effort/--name/--prompt]`, `nebula agent list [--all]` (JSON) — target resolved from
`NEBULA_AGENT_ID` via one Subscribe/Snapshot, `--project <name>` overrides. Orchestrators spawn pinned
on the root checkout, get `ORCHESTRATOR_INSTRUCTION` injected on every UserPromptSubmit
(`hooks/mod.rs`, composes with auto-title + notes via `context_injection`), and Claude gets
`Bash(nebula worktree:*)`/`Bash(nebula agent:*)` allow rules. UI: the Worktrees panel now shows an
ORCHESTRATORS group on top — `sel_worktree` indexes orchestrator rows + worktrees
(`worktree_rows`-family helpers in app.rs: `selected_worktree_index`, `worktree_row_index`,
`worktree_panel_len`), Enter on an orchestrator attaches+locks, sessions panel excludes them, project
context menu grew "New orchestrator" (`PendingIntent::AttachCreatedOrchestrator`). Verified end-to-end
against an isolated daemon: orchestrator → worktree → worker → list, all correct.

**Gotchas:**
- Every `app.sel_worktree = visible_worktrees().position(...)` site had to move to `worktree_row_index`
  (offset by `orchestrator_row_count`) — grep for `sel_worktree` before adding any new row group.
- Two agents worked this tree simultaneously all afternoon; the notes feature and this one merged
  cleanly file-by-file, but their `context_injection(&[String])` refactor landed while my edit of the
  same function was mid-flight — re-read before every edit paid off.
- `worktree new`'s created-path can arrive as an upsert AFTER the Ack — the CLI now keeps reading up to
  2s for it; without that the printed `path` is null.
- The e2e_tui boot helper waited for the splash text ("create your first project") — the splash removal
  broke all 6; they now wait for "no projects yet". Also fixed the long-red
  `FOOTER_TERMINAL_LOCKED`: the footer spells it `^q: panels`, not `Ctrl+q: panels`.
- Protocol bumps now fail clean: the v22 daemon + v23 client handshake yields "run `nebula kill` and
  relaunch" instead of a silent connection drop. Still: any entity/protocol change ⇒ restart the daemon
  (user approved killing its one live session, twice today).
- Follow-up ("nu vad in ui tab ul de orchestratori… impartita sectiunea a 2-a in 2 bucati pe
  verticala", then "vreau sa am select separat pe fiecare… cand dau n sa imi aleaga", then "imparti pe
  jumate sectiunile"): the column is permanently split at `inner.height/2` — top half ORCHESTRATORS
  (the `draw_column` title doubles as its header; a selectable "+ new orchestrator" placeholder row
  when empty), bottom half a "WORKTREES · n" header + the old list. Selection is **two stacked
  cursors**: new `App.sel_orchestrator: Option<usize>` (Some = cursor in the section) while
  `sel_worktree` KEPT its worktrees-only meaning — the first attempt offset `sel_worktree` by the
  section length and broke ~30 tests that set `sel_worktree = 0`; the two-cursor model broke none.
  j/k walk both sections as one list (`move_selection` has a dedicated Worktrees arm); `n` creates by
  section (orchestrator above, worktree prompt below); Enter attaches an orchestrator / creates on the
  placeholder. Every site assigning `sel_worktree` must also clear `sel_orchestrator` — grep both
  before touching panel selection. New `HitTarget::Orchestrator(usize)` for clicks.
- "am dat un nou orchestrator… si tot nu apare": clicking "+ new orchestrator" only *selected* the row
  (list-click semantics), creation was Enter/`n` — the user reasonably expected the click to create.
  A mouse click on the placeholder now calls `new_orchestrator` directly. Rule: a row that *reads* as
  a button ("+ …") must act on click. Diagnosed by checking the store first — `sqlite3 …/nebula.db
  "select name, orchestrator from agents"` showed no orchestrator row, so no request had ever fired,
  and `nebula agent new --orchestrator --project <name>` against the live daemon proved the whole
  daemon path fine.
- Second report ("am facut dar e aratat tot la sesiuni") was the user creating a *normal* session via
  the sessions picker and expecting it under ORCHESTRATORS — their mental model is "session on main =
  orchestrator". Accommodated: `SetAgentOrchestrator` request (PROTOCOL_VERSION 24) — session context
  menu "Make orchestrator" (daemon refuses off-root with a clear error; promotion pins, demotion
  unpins), right-click on an orchestrator row offers Attach/Rename/"Demote to session"/Delete. The
  row hops panels live via its upsert. Lesson: when users invent a flow, add a bridge from their flow
  to the feature instead of only documenting the intended one.
- Third report ("tot asa mi-l pune in sesiune"): DB forensics showed every new row was a *named*
  picker creation ("dada", "test") — the user's muscle memory is ⌘S/n + type a name; the orchestrator
  flows were never touched. Added `nebula agent promote|demote <name> [--project]` (CLI twin of the
  menu item) and promoted their "test" live. New full-path regression test
  `clicking_the_orchestrator_placeholder_creates_one` (draw → hit rect → synthetic MouseEvent): note
  it must click the row's CENTER — the leftmost cell of any panel row belongs to the splitter grab
  zone (`hits` first-match), which is pre-existing behavior, not a bug.
- "cand dau n acolo vreau… sa aleg nume… si cu ce claude pi etc ca la sesiuni": orchestrator creation
  now runs the full session flow — `open_agent_picker(app, worktree, orchestrator)` (title "New
  orchestrator", no shell-terminal row) → model/effort submenus → name prompt. The flag threads
  through `MenuAction::NewAgentOfKind`, `PromptKind::NewAgent`, and `create_agent` (which picks
  `AttachCreatedOrchestrator` and numbers default names `orchestrator-N`). The instant-create
  `new_orchestrator` fn is gone — every entry point (n, Enter/click on placeholder, project menu)
  opens the picker. Palette items for orchestrators read "{project}/{name} ◆ orchestrator" so the
  label is fuzzy-searchable ("orch" narrows to them) in both `/` and ⌘K.

### Agents Can Read And Work The Notes, ⌘E Opens Them Anywhere — 2026-08-24

**Asked:** "vezi ca mai am todo cu notes per proiect, si sa le poata accesa si agentul, vreau sa dau un
shortcut si sa mi apar fereasra" (per-project notes already existed — the asks were agent access and a
shortcut that opens the notes window).

**Did:** New `nebula notes [list|add [--worktree]|done <n>]` CLI: `run_notes` in
`crates/nebula-tui/src/ipc.rs` subscribes for one Snapshot and resolves the target from
`NEBULA_AGENT_ID` (agent → worktree → project), falling back to cwd (deepest worktree, else project by
`repo_path` prefix); list shows the project's notes then the worktree's as one numbered list, and
`done <n>` indexes into that. Agents learn it exists via a UserPromptSubmit injection
(`notes_instruction` in `hooks/mod.rs`) that fires only while `open_note_count_for_agent` (`store.rs`)
counts undone notes, plus a `Bash(nebula notes:*)` allow rule in `hooks/installer.rs`. The injection
branch now builds a `parts: Vec<String>` joined by `context_injection` (auto-title + orchestrator +
notes compose). `Action::Notes` gained a `cmd+e` default and the locked-terminal ⌘-intercept in
`event_loop.rs` (the ⌘N one) now also opens the note view.

**Gotchas:**
- No protocol change was needed — `Subscribe`, `CreateNote`, `SetNoteDone` already existed, so old
  daemon/new CLI stay MessagePack-compatible. Prefer composing existing requests over new variants.
- Another agent's orchestrator work was mid-flight in the same files the whole time; their
  `ORCHESTRATOR_INSTRUCTION` merged into my `parts` vec cleanly, but `cargo test -p nebula-daemon`
  was left uncompilable by *their* `registry.rs` tests (`agent_spawn_command` grew a 6th `prompt`
  arg). The new `open_note_count_for_agent` store test is written but couldn't run; the SQL was
  verified directly against a synthetic sqlite3 schema, and the CLI end-to-end against an isolated
  daemon (`NEBULA_RUNTIME_DIR=/tmp/<short>`, `NEBULA_AGENT_CMD=/bin/cat`).
- `event_loop.rs::a_duplicate_chord_warns_before_it_is_taken` hardcodes Notes' default-chord label —
  adding a second default ("e ⌘e") breaks it. Any new default chord on an existing action needs that
  test's labels updated.

**Asked:** "cand dau ng sa dewchid nebula ar treb sa mi deschida direct in sesiunea aia ca la herdr, si
sa fie mai usor de cautat foldere" — then mid-task: "nu mai vreau prima interfata, si mereu sa ramana in
stadiul ala" (never show the first-run splash; always open on the panels).

**Did:** New `nebula open [dir]` subcommand (`crates/nebula/src/main.rs`, handoff refactored into
`run_tui_and_handoff`) threads `open_at` through `run_tui`/`run_app` into `App.open_at`; the first
Snapshot calls `land_open_at` (`event_loop.rs`) — deepest worktree containing the dir wins, else project
by `repo_path` prefix, else `AddProject` with new `PendingIntent::SelectAddedProject` (+
`select_project_when_seen`, same when-seen idiom as worktrees). `~/.local/bin/ng` now passes
`open "${1:-$PWD}"` (with an `ng --plain` escape hatch). The first-run splash is gone:
`App::splash_showing` is now only the N-key preview; an empty workspace draws the normal panels, keeps
the first-run guidance footer (`ui.rs` footer arm now fires on `!has_visible_projects()` too), and the
`Action::Workspaces` terminal-focus guard also opens on an empty tree. `completion::list_dirs` matching
is now fuzzy (`fuzzy_rank`: prefix < substring < subsequence, case-insensitive, best-first); Tab
completion stays bash-prefix.

**Gotchas:**
- `ng` runs `open -na Ghostty.app … -e nebula`, which launches via launchd — **the caller's cwd never
  reaches nebula**. Any "open at cwd" behavior must pass the directory as an explicit argument.
- Six tests encoded splash-on-empty-tree; the cheap fix that avoided rewriting the footer assertions was
  keeping the guidance-footer arm alive for `!has_visible_projects()` rather than deleting it with the
  splash.
- `land_open_at` flashes instead of sending `AddProject` when no ancestor has `.git` — otherwise a bare
  `ng` from a non-repo dir would surface a daemon error on every launch.
- Follow-up ("cand caut un proiect nu merge cu fuzzy… sa caut mai rpd decat sa ma duc prin foldere"):
  in-level fuzzy wasn't enough — the Add-project prompt now deep-scans. `completion::scan_parent`
  (depth 3, 25k-dir budget, hidden + `DEEP_SKIP` pruned, **git repos are leaves**) is cached per typed
  parent on `PromptDialog.deep`/`deep_parent`; `filter_deep` narrows per keystroke (basename rank,
  repos first, shallow first). A dotted partial still browses shallow — that's the hidden-dirs opt-in.
  Slashed entry names ("Desktop/nebula") flow through `hovered_path`/`dive` untouched. The listing
  highlight in `ui.rs` used to underline the first `partial.len()` chars (a prefix assumption);
  `completion::match_positions` now computes the real matched positions on the *truncated* name.
- Bug report ("am cautat nebula desktop dar ma intreaba daca il creeaza pe desktop"): Enter on the
  input row treated the fuzzy query as a literal path and hit the create-directory confirm. The Enter
  arm in `event_loop.rs` now falls back to `prompt.dirs.first()` when the typed text doesn't exist as
  a path — creating a new dir on purpose still works because a fresh name matches nothing. Queries
  also split on whitespace now (`filter_deep` requires every token to match basename-or-path), so
  "nebula desktop" finds `Desktop/nebula` in either word order.
- "Selectez un proiect, dau Enter si nu se intampla nimic" + empty panels was **not** a prompt bug: the
  footer said `✗ disconnected · daemon connection lost`. A daemon from an older build (it predated the
  in-flight `created_from` field on `Worktree`) can't decode/encode against a freshly rebuilt TUI —
  MessagePack structs are positional, so any entity-field change severs the connection, the Snapshot
  never arrives, and every keypress goes nowhere. Diagnose with `nebula _stale-daemon-note` (prints only
  when the running daemon's build differs; `/tmp/nebula-501/daemon.build` holds the id); fix is
  `nebula kill` + relaunch. **Check what dies first**: `pgrep -lP <daemon-pid>` — this time it held one
  live claude session, and the user okayed killing it.
- "cmmd k ar treb sa caute printre sesiuni": ⌘K used to be `text:/` in the Ghostty overlay (the
  everything-palette). New `Action::SessionPalette` (`shift+s`) opens `Palette::sessions` —
  `sessions_only` on the `Palette` struct filters `build_palette_items` to agent rows and forces
  `enter_attaches` — and the overlay mapped `super+k=text:S`. `/` still searches everything.
  (**Stale since 2026-08-24:** ⌘K now opens the *command* palette via `super+k=csi:107;9u`; session
  search is `shift+s` or the palette's "Search sessions" row.) Remember: plain-text Ghostty ⌘-keybinds
  are just text injections; use kitty `csi:<code>;9u` when the chord must survive a locked terminal.
- The `ng` Ghostty window opened at a small/restored size: the overlay `~/.config/ghostty/nebula` now
  sets `maximize = true` + `window-save-state = never` (they must sit *after* its `config-file = config`
  include to win). `ghostty +validate-config --config-file=…` is the cheap check — it flags unknown
  fields, so a clean exit proves the keys are real.

### Cmd+N Spawns A New Agent From Anywhere — 2026-08-24

**Asked:** "pe nebula vreau cmmd n sa mi fac nou agent mai bine" (cmd+n should create a new agent, better
than the panel-dependent `n`).

**Did:** New `Action::NewAgent` in `crates/nebula-tui/src/keymap.rs` (id `new_agent`, SESSIONS group,
defaults `cmd+n, ctrl+n`) opening the new-session picker for the selected worktree from any panel via
`new_agent_shortcut` in `event_loop.rs`. It also fires inside an input-locked terminal — a guard in the
locked branch (next to the ^q hatch) intercepts it, unlocks, and opens the picker.

**Gotchas:**
- The locked-terminal intercept is gated on `chord.mods.contains(SUPER)` on purpose: if a user rebinds
  `new_agent` to a plain or ctrl chord, intercepting it while locked would steal keys the child app
  owns (typing `N`, readline's ^n). Keep that guard if the intercept ever grows.
- `keymap::tests::every_action_ships_with_a_reachable_chord` forbids an action whose only default is a
  ⌘ chord (host_warning marks all ⌘ as Blocked — Terminal.app swallows them). Hence the `ctrl+n`
  fallback. First try was `shift+n`, which collides with `splash`.
- Verified in a scratch worktree at HEAD: the shared tree was mid-flight with another agent's
  `created_from` field on `Worktree`, breaking `cargo test` for unrelated reasons.

### Shift+G Opens The Repo's Git Host, Released As v0.3.0 — 2026-08-24

**Asked:** "is there a release skill in this repo?", then "commit and push and do another release", then
"make a skill called release which kicks in and does these similar steps the next time someone asks".

**Did:** Released **v0.3.0** — `c553409`, tag pushed, all four binaries attached. Feature commit
`b00ce46` adds `crates/nebula-tui/src/remote.rs` (`repo_url`, `web_url`) plus `open_repo_in_browser`
in `event_loop.rs`, bound to `Action::OpenRepo` / `shift+g`. `ef56fca` checks in `CLAUDE.md`,
`.claude/MEMORY.md`, and the new `.claude/skills/release/SKILL.md`.

**Gotchas:**
- **Another agent was editing the same tree the entire time**, mid-way through a `--workspace` feature:
  `protocol.rs`, `registry.rs`, `server.rs`, `app.rs`, `ipc.rs`, `main.rs`, `e2e_pty.rs` all turned
  modified while this task ran. It bit three separate ways — (a) `git add` on `event_loop.rs` captured
  **66 lines when the reviewed change was 56**, silently dragging in their
  `run_app(workspace: Option<String>)`; (b) the shared index was **reset out from under a staged
  commit**, so `git commit` answered "no changes added to commit"; (c) a `git worktree add` under the
  scratchpad was **pruned away while in use**. What worked: do the whole release in a private worktree
  on its own branch and `git push origin <branch>:main`. **Never `git add` in the shared tree.**
- Local `main` stays behind `origin/main` after that push — it is checked out and dirty, so it can't be
  fast-forwarded. Say so explicitly; the next `git pull` has to reconcile.
- `e2e_tui::tui_projects_worktrees_agents_navigation` **failed at `origin/main` too**:
  `FOOTER_TERMINAL_LOCKED = "Ctrl+q: panels"` (`crates/nebula/tests/e2e_tui.rs:29`) while the footer now
  renders `^q: panels`. Introduced by `87d2b24`, shipped red in v0.2.0, fixed on main in `67ba923`
  (passes as of 2026-08-24). Always re-run a failing test against `origin/main` before blaming your own diff.
- `.github/workflows/release.yml` publishes with `generate_release_notes: true`, which is a bare commit
  list, not a changelog. `gh release edit vX.Y.Z --notes "…"` afterwards is the step that makes it one.

### Project Memory System — 2026-08-24

**Asked:** "update claude.md to invoke a skill called nebula-memory which has instructions on how an
agent should summarize the original request, how we fixed or implemnted it, and any gotchya you ran
into along the way. update the claude.md to instruct agents to read the memory.md file that the skill
updates …" — then: "go through all previous sessions for this project and invoke the nebula-memory
skill starting with oldest last so we can document how we grew this project."

**Did:** Created `CLAUDE.md` (none existed — only an empty `CLAUDE.local.md`), the
`.claude/skills/nebula-memory/` skill, and this file. Backfilled the entries below.

**Gotchas:**
- Real user prompts are recoverable from the transcripts by filtering `type=="user"` **and**
  `promptSource=="typed"` **and** `origin.kind=="human"`. Without that filter you get 8544 tool-result
  records instead of 258 prompts.
- ~12 sessions in this project's transcript dir are not nebula work at all — they are Cartastrophe game
  sessions and one-off test prompts that happened to run from this cwd. Filter by content, not by directory.

### Sessions Ordered By Last Interaction — 2026-08-24

**Asked:** "order the sessions by last interaction date, also display a time last interacted next to the
session title to right but left of harness name, so the workflow is a session runs goes to top of list,
if anything else iteracts it would go top. when displaying the last interaction time just show '23m ago…'"
Follow-up: "commit and push, then release with good change log with detials on what changed, make release
skill when done to follow these steps." Related earlier ask (2c58d9c1): running / awaiting-feedback
sessions always pin to the top of the Recent list.

**Did:** Sessions sort by last-interaction timestamp with a relative age label; released as `c340baf`
(v0.2.0).

### Rebindable Hotkeys And Settings Tabs — 2026-08-24

**Asked:** "in the settings add a top tabs which a user can use arrows or tabs to navigate though.
challenge my prompt, pick the best user experience. make good tab categories for where to put settings.
now I need you to add in a setting for hotkeys, allow a user to customize ANY HOTKEY in the application…"

**Did:** New `crates/nebula-tui/src/keymap.rs` holds the rebindable key table; settings overlay grew
tabs. Landed in `87d2b24` alongside the cancel-status fix.

**Gotchas:**
- The user explicitly invited pushback ("challenge my prompt") — this is a standing preference on UX
  asks, not a one-off.

### Worktree Names With Spaces, Random Branch Names — 2026-08-24

**Asked:** "when I create a worktree name, allow a user to type in spaces in the worktree name but you
must convert the spaces to hyphens. also allow a user to just enter on the branch which will pick a
random branch name using three words combined such as yellow-fox-jumps <adj>-<noun>-<verb>"

**Did:** Added `crates/nebula-tui/src/branch_name.rs` for the `<adj>-<noun>-<verb>` generator; the
worktree name field slugifies spaces to hyphens.

### PR Links And New-Comment Counts — 2026-08-23 → 08-24

**Asked:** "I noticed that one of my sessions created a pull request but that link was not auto detected,
I think when I switch to a worktree you should run a background process to check if any pull request are
open and show them as links…" Then: "if possible, track how many NEW comments were added since the last
click on a pull request link, it would be nice to see when others have left comments…"

**Did:** `crates/nebula-tui/src/pull_request.rs` plus a `pr_seen` read-marker map on `App`
(`app.rs:1718`). Links pin to a worktree; commit `44bd270`.

**Gotchas:**
- `gh pr view --json comments,reviews`: `comments[]` has **`viewerDidAuthor`**, `reviews[]` does **not** —
  telling your own reviews apart needs `gh api user --jq .login`. Inline per-line review comments aren't
  exposed as a `--json` field at all; counting review submissions is the cheap approximation.
- Both timestamps are RFC 3339 UTC, which sorts **lexicographically in chronological order**. `pr_seen`
  stores the newest stamp seen at open time, so "newer than X" is a string compare — no clock, no date
  parsing, and no `chrono`/`time` dependency added to a deliberately dep-light workspace. Empty string
  works as the sentinel because every real stamp sorts above it.

### Cancelling Claude Left The Status Stuck — 2026-08-23

**Asked:** "I noticed that when I cancel Claude code, it never actually changed the status back to green
from that yellow animation. Can you debug and fix this?"

**Did:** Added `crates/nebula-daemon/src/pty/progress.rs`, which scans the PTY byte stream for OSC 9;4
progress edges; the pump emits `PtyEvent::Progress` and `status.rs` treats "progress cleared" as a
synthetic `Stop` (same subagent-drain bookkeeping), but only from Running/NeedsFeedback.

**Gotchas:**
- Esc-cancelling a Claude turn fires **no hook at all**. `Stop` is documented not to run on user
  interrupt, and the `idle_prompt` Notification that normally rescues a hookless turn end is suppressed
  because Claude gates it on 60s quiet **AND** the user not having touched the keyboard — pressing Esc
  *is* touching it. Verified against Claude Code 2.1.241 with a `pty.fork` harness; only
  `UserPromptSubmit` then `SessionEnd` ever fired.
- The window **title** is unusable as a busy/idle signal — during a permission prompt it shows idle (`✳`)
  while the OSC 9;4 progress state correctly stays busy (`3`). Trust the progress state, never the title,
  or you will green out an agent that is waiting on the user.
- Codex and cursor-agent emit no OSC 9;4 at all, so this path is inert for them.

### Shared Working Tree Is Raced By Other Sessions — 2026-08-23

**Asked:** (no prompt — surfaced mid-task) A `git stash push -m hotkey-wip` + pop cycle from **another**
Claude session reverted and then restored every uncommitted file mid-edit, and the pop left three
duplicated `activity:` fields in `event_loop.rs` test fixtures.

**Did:** Nothing to commit — recorded as a working rule.

**Gotchas:**
- The user runs nebula's own agents against this repo, so the main tree is routinely mid-refactor from
  someone else. A `cargo check`/`cargo test` failure often has nothing to do with your change — check
  whether the failing symbols belong to unrelated in-flight work before blaming your own edit.
- Re-verify your edits are still on disk after any unexplained state change. Never `git stash pop` or
  `git checkout` the shared tree on your own judgment.
- A self-contained new module can be checked in isolation with `rustc --test --edition 2021 <file>` when
  the crate as a whole won't build.

### MIT License And Dependency Audit — 2026-08-23

**Asked:** "change to MIT license" — then, separately: "is https://ratatui.rs/ used on this project? what
third party lib do we use?" and "verify we are on the latest version of all of these, and also verify they
are all MIT license or able to be used on this MIT tui I'm making."

**Did:** Added `LICENSE` (MIT) and audited workspace dependency licenses.

### Releases So The Installer Stops Falling Back To Cargo — 2026-08-22

**Asked:** "no prebuilt binary for this platform yet — falling back to cargo... fix. also update readme to
walk user how to use this"

**Did:** Cut real GitHub releases with binaries (`bcaa104`, then `4ddcc7e` v0.1.1, `0c178e2` v0.1.2) so
`install.sh` finds an artifact instead of building from source.

**Gotchas:**
- Two `gh` accounts are logged in. `webdevcody` is the admin; `codyseibert` has only READ on
  `AgentSystemLabs/nebula` and fails write calls with "must be a collaborator (createPullRequest)".
  **As of 2026-08-24 `webdevcody` is the active account** (it was `codyseibert` on 08-22, so check
  rather than assume): `gh auth status`, and `gh auth switch --hostname github.com --user webdevcody`
  if it has drifted back. `git push` is unaffected either way: it goes over SSH, not the gh token.

### Codex Hooks Moved To ~/.codex — 2026-08-22

**Asked:** (follow-on from the Aug 14 codex work — codex sessions still weren't reporting status)

**Did:** `22f1b24` moved codex's hooks to `$CODEX_HOME/hooks.json` and started trusting `idle_prompt`.

**Gotchas:**
- Codex gates hooks behind a trust modal keyed by the **hook file's absolute path**, recorded in
  `~/.codex/config.toml` under `[hooks.state."<abs path>:<snake_case event>:<group idx>:<hook idx>"]` as
  `trusted_hash = "sha256:…"` — **not** a plain sha256 of the command string, so don't try to precompute
  it. A project-local `.codex/hooks.json` therefore re-prompts in every new worktree, and an unanswered
  prompt means the hooks never run at all. `$CODEX_HOME/hooks.json` is a stable path → one approval
  covers everything.
- Codex discards raw stdout from hooks. Context injection only works through
  `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"…"}}`. Claude Code
  accepts that same envelope, so one response body serves both.
- `codex exec` **does** run hooks once trusted, so it's a fast harness — but it can't answer the trust
  modal, so grant trust first with one interactive run.

### Real Line Editing In Typed Fields — 2026-08-22

**Asked:** (session ran on branch `fixing-input-ux`, merged as PR #1)

**Did:** `cd07baa` gave every typed field real terminal line-editing.

### Workspaces And The o/t/e Hotkey Remap — 2026-08-21

**Asked:** "add the ability to do a nebula workspace add <name> and then later nebula workspace open
<workspace_name>, then all projects will scoped to that workspace. make sure the / fuzzy find doesn't
search over all workspaces. also include a workspace list and workspace delete and workspace rename…"
Separately, on keys: "right now I often press o to open a new project accidently and that opens the
notes… on the nebula landing screen… my first instinct was to press o to open a new project" →
"change the new terminal hotkey to t, and change the todos to instead just be e hotkey for not(e)s,
refactor the language so instead of it being todos it's just notes."

**Did:** `77a87ca` (workspaces, respawn moved agents, o/t/b remap) and `4bea626` (todos → notes, ssh host
picker, note badge glyph).

**Gotchas:**
- A workspace is **just a grouping of projects** — the same project may belong to several. An early
  version refused to add a project that already existed in another workspace; the user rejected that
  ("we should be able to add any projects to any workspaces").
- The user twice asked for the key-combo hints to be rendered at the bottom of a modal rather than behind
  submenus ("nah I'd rather it just show r and d in the bottom of the workspace panel like we do for the
  notes, we should need all these sub menus"). Follow the notes-modal pattern for any new modal.

### e2e Daemon-Boot Failures Have Two Different Causes — 2026-08-21 → 08-23

**Asked:** (no prompt — both surfaced while verifying other work)

**Did:** Nothing to commit. Both are environmental, and telling them apart saves hours.

**Gotchas:**
- **Cold-exec flake.** All 16 `e2e_pty` tests fail with `daemon socket never appeared`. First exec of a
  freshly relinked `target/debug/nebula` can stall for seconds on macOS signature validation, so the test
  panics at its 5s deadline, `TempDir` drop deletes the runtime dir, and the late daemon logs
  `FATAL bind …/daemon.sock: No such file or directory`. Fingerprint: orphaned
  `$TMPDIR/.tmp*/data/state/daemon.log` files. **Just rerun** — it passes clean the second time.
- **Orphaned daemons.** Same generic error, but **no `daemon.log` is written at all** and reruns don't
  help — a test that passes in the full suite fails alone, seemingly at random. Cause: dozens of stray
  `nebula daemon --foreground` processes from past runs, each holding watchers/fds. Check with
  `ps aux | grep -c "[n]ebula daemon"`; anything in the dozens means orphans.
- Reaping orphans is safe **except for the live one** — read `/tmp/nebula-501/daemon.pid` (or
  `$NEBULA_RUNTIME_DIR/daemon.pid`) and exclude it, or you kill the nebula session you are running inside.
  Ask before bulk-killing: it's the user's machine and other live sessions may be in play.
- **`kill` on those orphans is refused by the auto-mode permission classifier** (2026-08-24), even
  filtered to processes older than six hours. Don't burn turns retrying it — instead prove the failure
  is environmental by re-running the same test against `origin/main` in a scratch worktree, and report
  the orphan count to the user so they can reap them.

### Restyle, Focus Wash, And The Screenshot Harness — 2026-08-20 → 08-21

**Asked:** A run of visual passes: "would it be possible to space out the items in the projects worktrees
and sessions lists? like to make them feel like larger buttons, also visual hieachy…", "when a list panel
is in focus, render a themed gradient that comes up from the bottom, but very subtle…" → "the bottom focus
gradient looks like shit... let's think of a differnt indicator… maybe just make the entire panel a very
lightly colored (like 10% opactiy) theme color", and "when a session is running (when it's yellow status
or red), make the text animate with colors… it should be a sweeping animation."

**Did:** `d704da7` (borderless columns, raised-fill selection, quiet chrome) plus the animation pass, with
a settings toggle to disable animations for CPU.

**Gotchas (recipe for screenshotting the TUI with demo data):**
- Isolate with `NEBULA_RUNTIME_DIR=/tmp/<short>` (SUN_LEN!) and `NEBULA_DATA_DIR=<scratch>/demo/data`.
  Never touch the real daemon — and note the daemon **detaches and outlives the tmux server**, so
  `kill $(cat $NEBULA_RUNTIME_DIR/daemon.pid)` when done.
- **Set `NEBULA_AGENT_CMD` even if you never create an agent** — the warm-slot prewarm launches a real
  `claude` on its own (shows as "1 agent · ~600MB" with zero agent rows in the DB). `/bin/cat` works.
- **One Bash call per drive**: the sandbox kills the private tmux server when the tool call ends, so
  new-session, send-keys, captures and kill-server must all happen in a single call. Send one key per
  call with 0.3–1s sleeps — batched keystrokes concatenate into the name prompt.
- `tmux capture-pane -epN` — **without `-N`** tmux trims trailing styled spaces and any background fill on
  the rightmost pane silently vanishes from the capture.
- Color and animation checks don't need PNGs: `capture-pane -ep` keeps SGR escapes; decode with
  `LC_ALL=C sed 's/\x1b\[/¶/g'` and grep for `38;5;N`, capturing 2–3 frames ~350ms apart to prove motion.
- Chrome headless gets SIGKILLed on this Mac and charmbracelet freeze wrecks the cell grid — use a small
  pillow grid renderer instead.

### Sessions Auto-Rename Themselves — 2026-08-20

**Asked:** "add some type of hook into nebula and ability for claude to automatically rename the session,
update the system prompt to use the skill to tell nebula to rename the session after the initial prompt
was submitted, we should be able to creat a title between 3-4 words that describe the ask of the promp…"

**Did:** A `UserPromptSubmit` hook injects an instruction telling the agent to run `nebula rename <title>`.
Later extended to codex ("it doesn't seem lke when I send a prompt to codex it updates the session title…
look into how we do it for claude code and replicate that behavior").

**Gotchas:**
- This is why every session in this repo issues a `nebula rename` before doing anything. It is injected
  context, not something the user typed — don't mistake it for part of the request.

### Cursor's Hooks Are Not Claude-Shaped — 2026-08-20

**Asked:** "cursor doesn't seem to update the status of the wortree or sessions when it is running, debug
and fix, verify it has hooks, if not, then setup some type of skill that is injected to cursor as a system
prompt or something so that it knows how to phone home to nebula to update the status"

**Did:** `install_cursor_hooks` in `hooks/installer.rs:260` is its own writer (plus a migration purge of
nebula groups under every key), and the installer maps cursor event names onto Claude-equivalent
`hookEvent` query values so `parse_event` stays single-dialect. `HookPayload` in `hooks/mod.rs` grew
aliases.

**Gotchas:**
- The installer originally assumed "same hooks JSON shape across all three CLIs". Cursor **silently
  ignored** the PascalCase Claude-shaped groups, so no status ever phoned home — no error, just nothing.
- Cursor's dialect: camelCase events (`sessionStart`, `beforeSubmitPrompt`, `stop`, `subagentStart/Stop`,
  `sessionEnd`), **flat** `{"command": …}` entries (no nested `hooks` array, no `type`), and a required
  top-level `"version": 1`. Hooks must print `{"continue": true}` to stdout or gating events degrade.
- Payloads carry `session_id` == `conversation_id` (the `--resume` chatId), have **no `cwd`** (use
  `workspace_roots[0]`), and subagent hooks use `subagent_id`, not `agent_id`.
- `beforeSubmitPrompt` and `stop` fire **only in interactive TUI mode**. A `-p` print-mode test fires only
  sessionStart / tool hooks / afterAgentThought / sessionEnd — **never conclude hooks are broken from a
  `-p` test**. To drive one interactively: pipe timed keystrokes through
  `script -q /dev/null cursor-agent --force --trust`.

### Idle Session Reaping And Metrics — 2026-08-20

**Asked:** "right now when a user opens a session, it takes some time I think for nebula to connect maybe
to the server and actually show the terminal... can we find a way to prefetch these connections…" → "add
logic to auto suspend or kill claude sessions that are not in focus…" → then the user pushed back on their
own idea: "I'm concerned now because some claude sessions might have schedules or long running jobs and I
don't want them killed.... is the latest change potentially breaking that requirement?" → "ok for now
never reap pinned sessions, also make this entire reap process a setting configurstion to just turn it
off." Alongside: "add some type of metrics modal which will show the overal usage of nebula combined with
all the other terminals open, including memory usage for individual and overall."

**Did:** `e11f838` — idle reaping, metrics tracking, memory stats in the footer.

**Gotchas:**
- **Pinned sessions are never reaped**, and reaping is switchable off entirely. That constraint came from
  the user realizing mid-feature that agents may be running long jobs — treat it as load-bearing.

### The Daemon Needs Its Own Session, Not Just A Process Group — 2026-08-20

**Asked:** "sometimes nebula will enter this state when I try to start a new claude terminal, it just
keeps writing strange tokens and the entire app is broken basically, I can't interact, it just happened
in a previous session I tried to open"

**Did:** `4502575`. `spawn_daemon` in `crates/nebula-tui/src/ipc.rs` now calls `setsid()` in `pre_exec`
instead of only creating a new process group, so the daemon holds **no controlling terminal** and nothing
it spawns can reach the user's terminal through `/dev/tty`. The `zsh -l -i -c "command -v claude"` CLI
probe in `nebula-daemon/src/registry.rs` also `setsid()`s (so even a `--foreground` daemon can't have the
probe shell steal a tty) and gained `.kill_on_drop(true)` — previously a hung probe leaked the child
forever when the 5s timeout dropped the future.

**Gotchas:**
- The garbage tokens were a **shell job-control fight over the controlling terminal**, not a rendering or
  vt100 bug. A new process group is not enough; it must be a new *session*.
- With no controlling tty, zsh's `/dev/tty` open fails and it skips job-control init entirely — that's the
  mechanism, and it's why the fix is one call in the right place.

### `zsh: killed` Is A Stale Code Signature, Not A Rust Bug — 2026-08-20

**Asked:** "debug why when I run nebula if fails … `nebula upgrade` → `zsh: killed nebula upgrade` …
`nebula` → `zsh: killed nebula`" Same thread: "nebula fails when I try to run it, give me hte proper
commands I should run locally to use the latest built version" → "make that into a single script and maybe
a makefile" → "rename kill-server to just kill, do that everywhere kill-server is too verbose."

**Did:** Added the `Makefile` for the local dev loop and renamed `kill-server` → `kill`.

**Gotchas:**
- The crash report says `SIGKILL (Code Signature Invalid)` / `Taskgated Invalid Signature` **even though
  `codesign -vv ~/.cargo/bin/nebula` reports valid on disk**. Cause: `cargo install --path` rewrote the
  binary **in place (same inode)** while the kernel held a cached signing blob for that vnode, so every
  later exec was killed.
- Fix is to refresh the inode, not the code:
  `cp ~/.cargo/bin/nebula ~/.cargo/bin/nebula.new && mv -f ~/.cargo/bin/nebula.new ~/.cargo/bin/nebula`.
  Identical bytes on a fresh inode exec fine.
- Confirm before debugging anything else: `~/Library/Logs/DiagnosticReports/nebula-*.ips`.
- A lingering `nebula daemon` from the old inode keeps running **old code**. `nebula kill` is the user's
  call — it stops live sessions.

### In-TUI File Tooling — 2026-08-19

**Asked:** Four asks in one evening: "when a user presses f show a fuzzy file finder…", "add the ability
for a user to press a hotkey to show a find in files search, basically it should run grep over the code
base… when a user presses enter it should show a vim terminal to allow editing that file, that vim
terminal must be a modal inside this app", "when claude code prints file paths, I want to be able to do a
option click… to actually open that file directly inside a file viewer (vim) inside nebula", and "add a
hotkey for t which shows a full tree browser modal with a view of the file content on the right…" →
refined to "in the file preview, it should be syntax highlighted, also when I select the file, it
shouldn't open a new vim modal, the right panel should just focus and let editing with vim."

**Did:** `998901f` (file finder, grep overlay, path links, in-TUI editor via
`crates/nebula-tui/src/vim_term.rs`) and `7ebc264` (tree browser with live filter and syntax preview).
Later `6787999` numbered the lines in file previews but not directory listings. The editor command is
configurable — the user asked for neovim support explicitly.

### Crash Logging — 2026-08-19

**Asked:** "make sure all errors in nebula are logged into a .log file somewhere so that I can debug when
it crashes. so far i've seen nebula randomly close out and crash twice now when trying to create a new
claude session, but I'm not sure how to debug"

**Did:** `71e62c7` — panic logging for both the TUI and the daemon.

**Gotchas:**
- Worth knowing that the "random crashes on new claude session" the user was chasing here were most likely
  the two separate problems diagnosed the next day: the stale code signature and the controlling-terminal
  fight. Crash logging is what made both findable.

### nebula ssh And Remote Hosts — 2026-08-19 → 08-21

**Asked:** "add a way for someone to launch nebula from the cli into a remote ssh. assume ssh keys already
allow access to the remachine. so something like nebula ssh HOST and when we get into the machine it
should install nebula if it doesn't already exist on the machine (remote exec of a script)…" Later: "add a
built in way so that nebula remembers the hosts you've recently done `nebula ssh` with so that a user can
press h to view all the hosts…"

**Did:** `8ddad36` (remote hosts, user config with settings overlay, fuzzy diff filtering) and the host
picker in `4bea626`.

**Gotchas:**
- The user also had to enable inbound ssh on this laptop to test it, and explicitly asked to confirm it
  was **local-network only, nothing from the public internet**. Don't widen that.

### Sessions Re-Home Into The Worktree They Create — 2026-08-18 → 08-24

**Asked:** "sometimes I'll be on the main root worktree and I'll start a session, and inside that session
I'll prompt it to do the work inside a worktree, which claude or codex will then create the worktree. if
possible, when this happens I want to move the session out of that main worktree root and move it to…"
Later, twice more: "there is a strange bug where … after I manually move a session to that work tree, at
some point in the future that original session seems to switch back to whatever worktree it originally
was…" and "the session takes a while before it is moved into the worktree… is there a way to make
automatically move…"

**Did:** `7570387` re-homes an agent row by hook-reported cwd. The cwd probe is the
`("PostToolUse", Some("Bash|EnterWorktree|ExitWorktree"))` matcher in `hooks/installer.rs`.

**Gotchas:**
- Claude uses its own **EnterWorktree** tool, not `git worktree add`. That creates a **locked** worktree
  at `<repo>/.claude/worktrees/<name>` on branch `worktree-<name>`.
- A Bash `cd` to a directory **outside the session's workspace root is silently reset** ("Shell cwd was
  reset to …") and the hook cwd never changes. So nebula's own sibling layout
  (`<repo>/../<repo>-worktrees/<branch>`, `git.rs` `worktree_dir`) is unreachable by cwd-following — only
  checkouts *inside* the repo re-home.
- Before the `EnterWorktree` matcher existed, the row only moved at the turn's `Stop` — measured **~34s
  late**, which is exactly the "takes a while" the user reported.
- **Hooks are snapshotted at session start**, so any hook-set change only reaches newly spawned sessions.

### Cmd+P Never Reaches The Agent In Terminal.app — 2026-08-18

**Asked:** "when I try command + p in a claude session, it just pastes the pi character and recommends I
run /setup-terminal which I already have, can you figure out if maybe command + p is not properly being
sent to the claude session? this is inside a terminal.app I'm running nebula. this works perfectly fine…"

**Did:** No code change — diagnosed as not-a-nebula-bug and gave remedies.

**Gotchas:**
- Terminal.app **never encodes Cmd into pty bytes** (⌘P is File→Print at the menu layer). The press
  arrives as Option+P's character `π`. Nebula's chain was verified sound end to end: kitty probe in
  `event_loop.rs` setup_terminal → legacy encoder swallows SUPER (`keys.rs` `encode_legacy`) → kitty
  re-encode would have sent `\x1b[112;9u`.
- Agent PTYs get `TERM=xterm-256color` (`pty/mod.rs`) but inherit the **daemon's** `TERM_PROGRAM`, so
  `/terminal-setup` run inside nebula detects whatever terminal the daemon was first spawned from, not
  the one currently attached.
- Remedy given: `/model` opens the same picker, or bind `ctrl+p` → `chat:modelPicker` in
  `~/.claude/keybindings.json`.

### Wheel Scrollback Vs Claude's Alt Screen — 2026-08-18 → 08-21

**Asked:** "when I scroll on my mouse wheel know (or track pad), it doesn't seem to scroll back in the
terminal session output, it instead just switches my previous entered prompts in the input" — and again
later: "…it instead it says 'Scroll wheel is sending arrow keys · use PgUp/PgDn to scroll' and it just
keeps showing previous prompts I'm using, how do I fix that"

**Did:** `handle_mouse` in `event_loop.rs` (see `mouse_protocol_mode` at `event_loop.rs:5199`) now
forwards a real SGR wheel report (`\x1b[<64;col;rowM` / 65) at the 1-based pane cell whenever
`screen.mouse_protocol_mode() != None`; arrow synthesis remains only for mouseless alt-screen apps
(plain vim/less).

**Gotchas:**
- Claude Code 2.1.x renders its main UI on the alternate screen and enables mouse tracking
  `?1000h ?1002h ?1003h ?1006h` **in the same write as** `?1049h`, so a vt100 replay sees both or neither.
- The old arrow-synthesis fallback is what triggered Claude's own `arrow-burst` detector and that warning
  banner. Check the child's mouse protocol mode in the vendored vt100 before assuming arrows are right.

### Optimistic Worktree Deletes And Stale Locks — 2026-08-18

**Asked:** "add some type of background task for deleting worktrees, I notice when i try to delete a
worktree, it often freezes up for a bit until it finally removes the worktree, I'd like it to do
optimistic client updates for when it's deleted and rollback if it fails…" Plus: "I'm trying to delete a
worktree and it says 'cannot remove a locked working tree, lock reason: claude session
menu-enable-level'. when I try to delete a worktree, it should force kill and remove any locked sessions…"

**Did:** `d214366` — deletes are optimistic with rollback, and stale session locks are force-unlocked.

**Gotchas:**
- The lock is not nebula's; Claude's EnterWorktree creates locked worktrees, so `git worktree remove`
  refuses until the lock is cleared.

### Codex And Cursor As Agent Kinds — 2026-08-14 → 08-15

**Asked:** "add support for codex as well, so when a try to load up a new session using the n hotkey, show
a modal that let's me pick codex or claude, make sure the codex setup has the proper hooks or whatever
else instlaled like we do in claude so that the status indicators can properly reflect the state of th…"
Then: "also add support for cursor cli as a session option" and "run codex with --yolo mode on codex
sessions, same with cursor if it has a type of yolo flag see how we do it on mission-control."

**Did:** `AgentKind` + a picker modal (`5092684`, `986f505`), cursor-agent as a third kind (`f5ed97d`),
permissions always skipped for both (`89f9860`).

**Gotchas:**
- `claude` takes `--model <alias>` and `--effort <low|…|max>`; `codex` takes `-m/--model` but effort only
  via `-c model_reasoning_effort=<…>`; `cursor-agent` has no model/effort knobs. Pick lists are hardcoded
  in `crates/nebula-tui/src/config.rs` (`CLAUDE_MODELS`, `CODEX_MODELS`) — "default" always means
  "pass no flag".
- Cursor has no PermissionRequest hook and nebula runs `cursor-agent --force`, so cursor agents report
  busy/idle but **never** needs-feedback. That is expected, not a bug.

### Vendored vt100 So Codex Scrollback Works — 2026-08-14

**Asked:** "scrolling back using codex doesn't work, but claude works fine, debug and fix"

**Did:** Vendored vt100 0.15.2 into `vendor/vt100` with a one-line semantic change and wired it via
`[patch.crates-io]` in the root `Cargo.toml`, so both `nebula-tui` and `tui-term` pick it up
(`d1d1a50`). Two regression tests in `app.rs` — one replays a codex-style region scroll, and it also
fails if anyone drops the `[patch.crates-io]` wiring.

**Gotchas:**
- The bug was in the parser, not in nebula's scroll handling. Codex is a ratatui **inline-viewport** app:
  it inserts history by setting a top-anchored DECSTBM scroll region (`ESC[1;{viewport_top}r`) and
  scrolling inside it. Stock vt100 0.15.2 **discards** any line scrolled out while a scroll region is
  active (`grid.rs`, `scroll_up`), so codex's scrollback stayed empty. Real terminals keep top-anchored
  region scrolls — which is why codex scrolls fine *outside* nebula.
- `vendor/vt100` is a **patched fork**. Do not upgrade or re-vendor it without re-applying this change.
- Full-screen apps are unaffected: the alternate screen's grid is created with zero scrollback capacity.

### Agents Spawn Through A Login Shell — 2026-08-14

**Asked:** "it seems like new sessions don't use my ~/.zshrc, verify the do on load"

**Did:** `1344cd6` — agents and terminals spawn through a login shell.

**Gotchas:**
- This wrap is why `NEBULA_AGENT_CMD` also has to *skip* it: without that, `~/.zprofile` resets PATH and
  the **real** `claude` CLI launches instead of a test stub.

### Terminals Removed, Then Brought Back — 2026-08-09 → 08-20

**Asked:** "remove the terminal section from the session list, I decided I don't care about terminals as
we can just use claude code to run terminal commands directly" — reversed 11 days later: "add a way to
create a new terminal already in the pwd of the worktree or root, figure out a good key binding for this
as cmd + t will open a new ghostty terminal if I'm using ghostty to run nebula" (`c318eedb`).

**Did:** Removed, then re-added on its own hotkey (`t` after the Aug 21 remap).

**Gotchas:**
- Recorded because the removal reads like a settled decision in the Aug 9 history and is **not** one.
  Don't cite it as precedent.

### Worktree Watcher And Selection Memory — 2026-08-05

**Asked:** "verify we have some type of directory watcher on .worktrees or the github worktrees so that
when a new worktree is created from an agent or manually it'll update the worktrees list automatically.
right now i created a worktree and it did not show up in that list until i restarted nebula" — then:
"change of plans, we should remember the last agent that was selected for that project so that if i
switch between projects it'll automatically just show the last selected worktree & agent…"

**Did:** `91c29c0` (auto-sync + selection restore) and `02bb5a3` (refresh branches on external checkouts).

### Project Dividers And Shift+J/K Reordering — 2026-08-05

**Asked:** "add a way to put dividers between projects, also a way to hold shift and move projects up and
down in regards to their order in the list so that I can group projects together" — then, after the first
attempt only swapped neighbours: "when I do shift j and k, it doesn't seem to move projects under
dividers, it just swaps projects, you must treat a divider as something I can move a project under or
above separate" and, escalating, "I should be able to move a project into any fucking divider I want."

**Did:** `98dc681` — reordering treats dividers as real positions, and dividers are labelable and movable.

**Gotchas:**
- Shift+↑/↓ is **undeliverable in Terminal.app**: `keyMappings.plist` has entries for `$F702`/`$F703`
  (Shift+←/→) but **none** for `$F700`/`$F701`, so Terminal drops the shift and sends a plain arrow.
  Shift+J/K works everywhere because crossterm tags uppercase chars with SHIFT.
- "Move" has to mean move-across-groups, not swap-with-neighbour. The first implementation satisfied the
  literal words and not the request.

### Install Script And The Org Slug — 2026-08-05

**Asked:** "if I wanted to provide one command for anyone to install or update this cli tool, what's the
best way? a .sh script in the repo? I don't want to use some third party registery at this point" →
"do the curl approach and put in the readme" → "why did you make the readme say webdevcody,,, this is part
of the agentsystemlabs org"

**Did:** `install.sh` + README one-liner (`95ac3da`), then `nebula upgrade` (`1c87c06`).

**Gotchas:**
- The repo slug is **`AgentSystemLabs/nebula`**, never `webdevcody/<repo>`. It is hardcoded in
  `install.sh` (`REPO=`) and the README. Assume other repos under `~/Workspace/AgentSystemLabs/` are
  org repos too.

### iTerm Swallowed Option+Delete — 2026-08-05

**Asked:** "when I have a session focused, option + delete doesn't seem to work to backspace by words when
I have nebula opened in iterm, fix"

**Did:** Fixed outside the codebase — set left Option → Esc+ in iTerm's Default profile.

**Gotchas:**
- iTerm2 3.5.10 in kitty mode only reports Option as the alt modifier when the profile's Option key is
  **Esc+** (`Option Key Sends` = 2). With "Normal" (the user's old setting) Option+Delete arrives as a
  plain Backspace and word-delete silently breaks.
- iTerm must **not** be running when editing its plist or it clobbers the write on quit. Its quit-confirm
  dialog can't be dismissed via osascript without accessibility permission — SIGTERM works and skips the
  pref flush.

### The Focus-Key Odyssey → Ctrl+Q — 2026-08-04

**Asked:** "make cmd arrow change focus of the panels, require an enter of the session panel to focus lock
into it" — which turned into a long elimination, punctuated by "I'm not even using ghostty you fuck" and
ended by "fuck it go back to control + q, also shift drag doesn't do shit. fix it".

**Did:** Ctrl+Q is the unlock/escape hatch. Fallbacks kept: Ctrl+] / Ctrl+Esc / Ctrl+←. Shift-drag was
replaced with app-side plain drag-selection in the terminal pane (REVERSED overlay for highlight, text via
vt100 `contents_between`, `pbcopy` on mouse-up).

**Gotchas:**
- **The user runs Terminal.app**, not Ghostty, despite Ghostty being installed. Terminal.app fails the
  kitty-keyboard probe, so Cmd-modified keys and Ctrl+Esc never reach the app there.
- Everything else was eliminated for a reason: Cmd+arrows (no kitty protocol), Ctrl+arrows (Mission
  Control), Ctrl+Esc / Option+Esc (undeliverable), Ctrl+]: vetoed on feel, double-Esc: implemented then
  reverted because Claude Code owns Esc, Shift+arrows and Ctrl+G/T: Claude Code binds them. This was
  superseded on 2026-08-24: Ctrl+Q remains a hardwired unlock hatch, while Cmd+P toggles full-screen.
- crossterm collapses a same-read `\x1b\x1b` pair into **one** Esc event (escaped-escape rule), which is
  what made double-Esc unworkable.
- "Shift+drag selects text" is a lie in Terminal.app — there's no mouse-reporting bypass there, unlike
  Ghostty/iTerm.
- The user runs `nebula` via a `~/.cargo/bin` symlink to `target/release` — **rebuild release and restart
  the TUI** before testing keybinding changes, or you are testing a stale process.

### Bootstrap: Daemon/TUI Split — 2026-08-04

**Asked:** "I want to build out a cli tool which is performant, uses very little memory, but kind of acts
like a multi plexer to allow creating new terminal windows (similar to ghostty). the main things I need to
include, like the peak user experience I'm going for is. left side panel for project, then if you c…"

**Did:** `47037e8`. Cargo workspace `crates/{nebula-core,nebula-daemon,nebula-tui,nebula}` shipping one
binary. A detached tmux-style daemon owns the PTYs (portable-pty, 1MB byte-ring scrollback with seq
numbers); the TUI attaches over a unix socket with length-prefixed MessagePack (`nebula-core/src/codec.rs`).

**Gotchas (locked decisions — user-approved, don't relitigate):**
- **No server-side VT grid.** Attach replays the ring into the client's vt100 parser plus a SIGWINCH
  resize-jiggle.
- **tui-term is a renderer only**, kept behind `nebula-tui/src/ui.rs` as a swap point.
- **Status comes from agent hooks, not MCP** — MCP was proven unreliable in ../mission-control. Managed
  hooks are merged into the worktree's settings and curl a loopback axum server with a per-boot bearer
  token. Keep the logic in the pure `AgentStatusMachine` (`nebula-daemon/src/status.rs`, unit-tested with
  injected clocks) and **never trust a bare `Stop`**.
- Kitty keyboard protocol passthrough (`nebula-daemon/src/pty/kitty.rs`) is what makes Cmd/Option combos
  and Shift+Enter reach Claude Code at all.
- **Unix socket paths must stay short** — SUN_LEN is ~104 bytes, so a long `NEBULA_RUNTIME_DIR` breaks
  `bind()`. This bites the test harnesses and the screenshot harness constantly.
- Ideas were borrowed from ../mission-control, but **all code is written fresh** — that was a hard user
  requirement.
