---
name: diagram
description: "Trigger: any diagram request — flowchart, sequence, state, ER, architecture — or a decision the user should make by clicking an option. In a nebula session: a self-contained HTML page via `nebula diagram`, shown inside the TUI (⌘X). In a Mission Control session (MC_* env vars present): Mermaid in MC's diagram viewer."
user-invocable: true
---

Pick the branch by environment, then follow only that branch:

- `MC_API_URL` / `MC_TASK_ID` set → **Mission Control** (below, unchanged).
- Otherwise → **Nebula**: an HTML page in the checkout's diagrams folder, shown in the TUI.

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

# Mission Control

Mission Control agent sessions receive env vars automatically:

- `MC_API_URL` — loopback API base (e.g. `http://127.0.0.1:54321`)
- `MC_API_TOKEN` — bearer token for that API
- `MC_TASK_ID` — the active task/session id
- `MC_THEME` — Mission Control UI theme: `dark` or `light`

**Do not print Mermaid to the user and stop.** POST it to Mission Control so the app opens an interactive viewer modal. The viewer applies Mission Control theme colors automatically — your job is to avoid fighting that styling.

## When to use

- User asks for a diagram, flowchart, sequence diagram, architecture map, or state chart
- You are explaining a multi-step system and a diagram would land faster than prose
- You already drafted Mermaid and would otherwise paste it in the terminal

Skip when Mission Control env vars are missing — use the Nebula branch above.

## API contract

```http
POST $MC_API_URL/api/diagram?taskId=$MC_TASK_ID
Authorization: Bearer $MC_API_TOKEN
Content-Type: application/json

{
  "source": "<mermaid source>",
  "title": "Optional title shown in the modal",
  "format": "mermaid",
  "theme": "dark"
}
```

`theme` is optional metadata — pass `"$MC_THEME"` when set so Mission Control knows which UI theme was active. The viewer still re-renders using the live app theme; you do **not** need to bake colors into `source` for MC.

Success (`200`): `{ "ok": true, "id": "<uuid>" }` — Mission Control opens the viewer automatically.

Common errors:

- `400` — missing/invalid body, empty `source`, or `taskId` query param missing
- `401` — bad or missing bearer token
- `404` — unknown `taskId`

## Theme rules (critical)

Mission Control runs in dark mode by default. Diagrams fail when agents embed colors that disappear on dark backgrounds (dark gray boxes, faint borders, `#111` fills).

1. **Do not override Mermaid theme in `source`.** Never use `%%{init: {'theme':'dark'}}%%`, `%%{init: {'theme':'base', 'themeVariables': ...}}%%`, YAML frontmatter theme blocks, or `classDef` / `style` with hardcoded hex/rgb fills.
2. **Use plain Mermaid syntax.** Let Mission Control's viewer apply accent, surface, text, and border tokens from the active UI theme.
3. **Read `$MC_THEME`.** When it is `dark`, avoid near-black node/participant fills. When it is `light`, avoid near-white fills on white backgrounds.
4. **Sequence diagrams:** use default participant boxes — do not set actor background colors inline. Example:

```mermaid
sequenceDiagram
  participant U as User
  participant MC as Mission Control
  U->>MC: POST /api/diagram
  MC-->>U: Open themed viewer
```

5. **Flowcharts:** prefer unstyled nodes (`A[Step]`, `B{Decision?}`) over `A["Step"]:::darkClass`.

## Rules

1. **Always send valid Mermaid** in `source`. No markdown fences — raw diagram text only.
2. **Keep diagrams focused.** Prefer one concern per diagram; split large maps into multiple POSTs with distinct titles.
3. **Set a short `title`** when the diagram has a clear name (e.g. "Auth flow", "Worktree lifecycle").
4. **Pass `"theme": "$MC_THEME"`** in the JSON body when `$MC_THEME` is set.
5. **Tell the user briefly** that the diagram opened in Mission Control — don't repeat the full source unless they ask.
6. **On HTTP failure**, show the error and paste the Mermaid source as a fallback.
7. **Verify env vars before POSTing.** If `MC_API_URL`, `MC_API_TOKEN`, or `MC_TASK_ID` is empty, tell the user the session is not running inside Mission Control.

## Examples

### Minimal POST (preferred — no jq required)

```bash
curl -sS -X POST "$MC_API_URL/api/diagram?taskId=$MC_TASK_ID" \
  -H "Authorization: Bearer $MC_API_TOKEN" \
  -H "Content-Type: application/json" \
  -d @- <<EOF
{
  "source": "flowchart LR\n  A[Draft Mermaid] --> B[POST /api/diagram]\n  B --> C[Modal opens]",
  "title": "Diagram pipeline",
  "format": "mermaid",
  "theme": "${MC_THEME:-dark}"
}
EOF
```

### Sequence diagram

```bash
curl -sS -X POST "$MC_API_URL/api/diagram?taskId=$MC_TASK_ID" \
  -H "Authorization: Bearer $MC_API_TOKEN" \
  -H "Content-Type: application/json" \
  -d "$(jq -n \
    --arg source 'sequenceDiagram
  participant U as User
  participant MC as Mission Control
  participant A as Agent CLI
  U->>MC: Start session
  MC->>A: Spawn PTY + env
  A->>MC: POST /api/diagram
  MC-->>U: Open diagram modal' \
    --arg title 'Session diagram flow' \
    --arg theme "${MC_THEME:-dark}" \
    '{source:$source,title:$title,format:"mermaid",theme:$theme}')"
```

### Flowchart (minimal)

```bash
curl -sS -X POST "$MC_API_URL/api/diagram?taskId=$MC_TASK_ID" \
  -H "Authorization: Bearer $MC_API_TOKEN" \
  -H "Content-Type: application/json" \
  -d "$(jq -n \
    --arg source 'flowchart LR
  A[Draft Mermaid] --> B[POST /api/diagram]
  B --> C[Modal opens]' \
    --arg title 'Diagram pipeline' \
    --arg theme "${MC_THEME:-dark}" \
    '{source:$source,title:$title,format:"mermaid",theme:$theme}')"
```

## Mermaid tips

- Prefer `flowchart TD/LR`, `sequenceDiagram`, `stateDiagram-v2`, and `erDiagram` — they render reliably.
- Quote node labels with special characters: `A["Step (retry)"]`
- Avoid `click` directives and HTML labels — the viewer runs in strict mode.
- Avoid inline colors — Mission Control themes diagrams to match the app.
