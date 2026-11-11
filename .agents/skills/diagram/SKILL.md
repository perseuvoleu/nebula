---
name: diagram
description: "Trigger: any diagram request — flowchart, sequence, state, ER, architecture — or a decision the user should make by clicking an option. Output: a self-contained HTML page via `nebula diagram`, shown inside the TUI (⌘X)."
user-invocable: true
---

# Nebula

Diagrams are self-contained HTML files in **one folder**: `<checkout>/.scratch/diagrams/<slug>.html`.
The viewer and the CLI read nothing else, so never put a diagram anywhere else and never print
raw Mermaid or ASCII art to the terminal instead.

## Steps

1. `nebula diagram new <slug> --title "Short title" [--tag t]...` — writes the template, prints
   the absolute path. Slug: `[a-z0-9-]+`. It refuses to overwrite: pick another slug, or pass
   `--force` when you really mean to replace yours.
2. Edit that file: replace the sample `<svg>`/cards with the real diagram. Keep the head.
3. `nebula diagram open <slug>` — asks the user's running TUI to show it. Then tell the user in
   one line that it is open and that **⌘X** reopens the newest diagram any time
   (`[` `]` switch, `/` searches, Esc closes).

Updating a diagram = editing the same file; the open viewer reloads it when it changes.
Other verbs: `nebula diagram list [query] [--json]`, `add <file.html> [--slug s]` (import an
existing page), `rm <slug>`, `path`.

## Rules

1. **One file.** Inline all CSS, JS and SVG. No second file, no images on disk.
2. **No CDN, no network.** The viewer blocks every non-local request, so a CDN script (Mermaid
   included) renders nothing. Draw with HTML/CSS and inline `<svg>`; there is no bundled Mermaid.
3. **No hardcoded colours.** Use the template's CSS variables (`--bg`, `--surface`, `--edge`,
   `--text`, `--muted`, `--accent`, `--ok`, `--warn`, `--err`, `--info`) so pages match the TUI.
4. **`<title>` is required** — it is what the user sees in the list and searches by. Keep the
   `<meta name="nebula-diagram">` line too: it holds the tags and marks you as the owner.
5. **One concern per diagram.** Several small pages beat one wall; the user switches with `[` `]`.
6. The page is about 1200×800 and scrolls; mouse, wheel and keys reach it (except `[`, `]`, `/`,
   Esc, which the viewer keeps).

## Decision pages

When the user has to choose between options, do not write the options out in the terminal. Render
them as a page and let the user click:

1. `nebula diagram new <slug> --decision --title "The question"` — option cards (title,
   explanation, pros/cons, a "choose" button) plus a free-text note.
2. Edit it: one `<article class="option" data-choice="…">` per option, with the **real
   explanation and trade-offs on the page** — the terminal only gets a one-line "pick one in ⌘X".
   `data-decision` on `<main>` is the id you get back.
3. `nebula diagram open <slug>`, tell the user to choose there, and **end your turn**.
4. The choice arrives as your next prompt, in this exact form:
   `[nebula diagram] <slug>: decision "<id>" = "<choice>" (<label>) — note: <note>. Decisions file: <path>`
   Treat it as the user's answer. History: `nebula diagram decisions <slug> [--json]`.
5. Answer by rendering the next page — edit the same slug, or create a new one; the open viewer
   reloads the page, or moves to your newer one, by itself. Carry on with the work only once
   nothing is left to decide.

Custom pages can call `window.nebula.decide({id, choice, label, note})` themselves (limits: id and
choice 64 chars, label 200, note 2000). Only those four strings reach you. The prompt is delivered
only while your session is alive on the same checkout; otherwise the choice is just saved to the
decisions file, so read that file if you were restarted.
