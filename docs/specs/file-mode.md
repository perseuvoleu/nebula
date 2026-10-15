# File mode

Status: ready-for-agent. GitHub Issues is disabled on this fork, so this committed document is the
spec and its tickets (the `docs/specs/mobile-chat-redesign.md` precedent).

## Problem Statement

While working in nebula I want to browse and read a project's files the way an editor's file
explorer allows: a tree I can filter on one side, the code on the other, the whole screen given
to it. Today the `b` tree browser does most of this, but it is a centered modal with the
sidebars still visible around it, its tree sits on the left, and I can reach it only through a
key I must remember.

## Solution

**File mode** is the existing tree browser, reshaped:

- It takes the whole terminal: the Projects and Worktrees panels and the attached pane are hidden.
- Code on the left (preview, or the embedded editor), file tree with its filter on the right.
- A breadcrumb on the top line names the checkout and the selected path
  (`nebula › crates › nebula-tui › ui.rs`), with a `✕` that leaves file mode.
- A clickable **Files** button in the sidebar header, beside the `⎇ <branch> ▾` primary-branch
  dropdown, opens file mode on the selected project's primary checkout.
- Leaving file mode (Esc, `✕`) restores exactly the layout that was on screen before.

Browsing is preview-first, as today: selecting a file shows it syntax-highlighted; Enter opens it
in the configured editor embedded in the code pane; quitting the editor returns to the preview.

## User Stories

1. As a nebula user, I want a Files button next to the primary branch control, so that I can open the project's files with one click.
2. As a nebula user, I want the Files button to open the primary checkout of the selected project, so that its target does not depend on which worktree row is selected.
3. As a nebula user, I want `b` to keep opening file mode on the selected row's checkout or `(branch)` row, so that keyboard flows keep working.
4. As a nebula user, I want file mode to take the whole terminal, so that the code has as much room as possible.
5. As a nebula user, I want the Projects and Worktrees panels hidden in file mode, so that nothing competes with the files.
6. As a nebula user, I want the file tree on the right, so that the layout matches the editors I use.
7. As a nebula user, I want the code on the left, so that it starts where I read.
8. As a nebula user, I want the filter box above the tree, so that typing narrows the tree as it does today.
9. As a nebula user, I want a breadcrumb of the selected path on the top line, so that I know where I am.
10. As a nebula user, I want a `✕` on the top line, so that I can leave file mode with the mouse.
11. As a nebula user, I want Esc to leave file mode, so that I can leave with the keyboard.
12. As a nebula user, I want leaving file mode to restore the previous layout, selection and attached pane, so that I continue exactly where I was.
13. As a nebula user, I want selecting a file to preview it with syntax highlighting and line numbers, so that reading is fast and safe.
14. As a nebula user, I want Enter on a file to open it in my editor inside the code pane, so that I can edit without leaving file mode.
15. As a nebula user, I want quitting the editor to bring the preview back, so that I keep browsing.
16. As a nebula user, I want the tree/code splitter to stay draggable, so that I can size the tree.
17. As a nebula user, I want mouse clicks on tree rows to keep working at their new position, so that the swap does not break the mouse.
18. As a nebula user on a project that lives only on findl, I want the Files button to tell me the files live on findl, as `b` does, so that the refusal is consistent.

## Implementation Decisions

- File mode is the existing `Overlay::Tree` / `TreeBrowser`, not a new view. No new module.
- The overlay renders over the full frame instead of the 92%×90% centered rect. Being an overlay,
  closing it already restores the prior layout; no layout state is saved or restored.
- Layout: top line = breadcrumb + `✕` (a hit target that closes the overlay); below it the code
  pane on the left and the tree panel (filter row + rows) on the right. `files_width` keeps
  meaning the tree panel's width; the splitter is now the tree panel's left border, and the
  drag math flips accordingly.
- The written-back rects (`area`, `list_area`, `preview_area`) keep their meaning, so row
  hit-testing, paging and the embedded editor's size follow the new positions unchanged.
- The breadcrumb replaces the tree title's branch label; the match count (`(n)` / filtered
  count) stays on the tree panel's title. The embedded editor's "— editing" state stays visible.
- The Files button is drawn in the sidebar header by the code that draws the `⎇ <branch> ▾`
  dropdown, left of it, as a new `HitTarget`. Its click opens the tree browser on the selected
  project's primary checkout through the same opener `b` uses, with the same refusals (remote
  twin, no files). Label: `≡ Files`, shrinking to `≡` when the header is narrow; it is hidden
  when there is no primary checkout (same condition as the dropdown).
- Keys inside file mode do not change. Esc closes it (unless the embedded editor has focus).
- README's `b` entry and keys table describe file mode and the button.

## Testing Decisions

- Test through the app's public seam, as the existing tree-browser tests do: drive `App` with
  key presses and mouse clicks, draw into a ratatui `TestBackend`, and assert on the rendered
  buffer text and the overlay state. Prior art: `t_opens_tree_browser_folds_dirs_and_filters_hierarchies`,
  `tree_browser_renders_tree_and_preview_panes`, `embedded_editor_takes_over_the_preview_pane`,
  and the click tests that locate a `HitTarget` in `app.hits` (e.g. the `CheckoutBranch` one).
- Assert behaviour: the tree's rows render right of the code; no sidebar panel title appears
  while the overlay is open; the breadcrumb shows the selected path; clicking `✕` or Esc clears
  the overlay; clicking Files opens the overlay on the primary checkout even with a linked
  worktree row selected.
- Update existing tree tests only where they asserted the old title text or left/right position.

## Out of Scope

- A native (non-vim) editor; always-on editor on selection; editing a `(branch)` row's files.
- File mode for checkouts on findl.
- Files buttons on individual worktree rows.
- Git status decorations, file icons, rename/create/delete in the tree.

## Tickets

### 01: Full-screen file mode with tree on the right

**Blocked by:** None — can start immediately.

**What to build:** `b` opens the tree browser over the whole terminal, sidebars hidden, code
pane left, tree + filter right, a breadcrumb and `✕` on the top line. Esc or `✕` returns to the
previous layout. Splitter drag, row clicks and the embedded editor work in the new positions.

Acceptance criteria:
- [ ] With the overlay open, the frame shows no Projects/Worktrees panel content.
- [ ] Tree rows render to the right of the preview text.
- [ ] The top line shows the breadcrumb of the selected node and a `✕`.
- [ ] Clicking `✕` and pressing Esc each close the overlay.
- [ ] Clicking a tree row selects it; dragging the tree's left border resizes the tree.
- [ ] Enter on a file embeds the editor in the left pane at that pane's size.
- [ ] README's `b` description matches.

Verification: `T01-a` render test (sidebar text absent, tree x > preview x, breadcrumb present);
`T01-b` `✕` click and Esc close the overlay; existing tree tests updated and green;
`cargo test -p nebula-tui` green; manual look in a fresh `ng`.

### 02: Files button in the sidebar header

**Blocked by:** 01.

**What to build:** a `≡ Files` button beside `⎇ <branch> ▾` that opens file mode on the
selected project's primary checkout; refusals match `b`.

Acceptance criteria:
- [ ] The button renders beside the primary branch dropdown, shrinking to `≡` when narrow.
- [ ] Clicking it opens file mode rooted at the primary checkout, even with a linked worktree row selected.
- [ ] On a remote-only project it flashes the same message `b` does.
- [ ] README's keys/mouse docs mention the button.

Verification: `T02-a` click test locating the new `HitTarget` in `app.hits`, asserting the
overlay's source is the primary checkout with a linked worktree selected; `cargo test -p nebula-tui` green.
