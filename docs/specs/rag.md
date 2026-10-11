# Architecture — Nebula RAG

Status: design agreed (2026-09-23); v1 implemented on branch `rag` (crate `nebula-rag`),
not yet deployed — see "Implementation notes" at the end.
Research backing the storage/embedding choice: `.scratch/research/rag-storage-backends.md`.

## Problem & goals

Agents (claude, codex, cursor, pi) running in nebula sessions need durable, searchable
knowledge that is not in the code: decisions and their reasons, procedures, fixes after
hard bugs, general know-how, and the markdown docs of every project — including projects
other than the one they are working in. Knowledge is scoped both globally and per app.
It lives on findl, is backed up to Cloudflare R2, and is reached through the nebula CLI.

The service is retrieval only (embed + search). Agents do the generation themselves.

## Non-goals (v1)

- **No source code indexing.** Agents search code better with `rg` and direct reads;
  semantic search misses exact identifiers and a code index is the costliest part to
  keep fresh. Revisit only if cross-repo code search proves missing.
- No HTTP API for apps, no MCP server, no multi-tenant app users. Both HTTP and MCP
  are later thin layers over the same library.
- No LLM answer generation.
- No offline cache on the laptop.

## Approaches considered

- Storage: SQLite + sqlite-vec + FTS5 (chosen) vs Postgres + pgvector, Qdrant,
  LanceDB, Cloudflare Vectorize + D1. Postgres is the heaviest on a host already in
  swap; wal-g hangs against R2 and pgBackRest has R2 429s. Qdrant costs ~1 GB RAM and
  backs up by snapshot only (runner-up if volume grows past brute-force range).
  LanceDB is pre-1.0 with near-daily betas. Vectorize ruled out: data stays on findl.
- Embeddings: Workers AI `@cf/baai/bge-m3` (chosen after the eval spike) vs
  `@cf/qwen/qwen3-embedding-0.6b` (research favourite, lost on the real bilingual
  corpus), Voyage, OpenAI, Gemini, Jina. Both Workers AI models ≈ 1,075 neurons/1M
  tokens, inside the 10k neurons/day free allocation. Paid fallback `voyage-4-lite`.
- Separate repo vs nebula crate: inside nebula (Rust), reusing its findl transport,
  install and release.
- Agent interface: skill + CLI (chosen) vs MCP. MCP only adds value for shell-less
  clients (Claude Desktop, mobile) and costs per-agent config and permanent context.

## Recommended approach

A `rag` library module in the nebula workspace plus a `nebula rag` subcommand. On
findl it opens the SQLite database directly; on the laptop it runs
`ssh findl nebula rag …` (same pattern as `nebula remote`, no daemon protocol change,
SSH key is the auth). No long-running RAG server. Litestream replicates the database
to R2 continuously.

```
laptop agent ──Bash──► nebula rag search … ──ssh──► findl: nebula rag search …
                                                        │
                           Workers AI (REST) ◄──embed───┤
                                                        ▼
                         ~/.local/share/nebula/rag.db (SQLite + sqlite-vec + FTS5)
                                                        │ Litestream (systemd)
                                                        ▼
                                              r2://nebula-rag/<path>/
```

### Proposed command surface (not implemented)

```
nebula rag search "<query>" [--ws <path>[,<path>]] [-k 5] [--json]
nebula rag add "<text>" --ws <path> [--title …]        # note; dedups
nebula rag add <file|url> --ws <path>                   # external doc
nebula rag ws list | create <path> | rm <path>
nebula rag note list [--ws] | show <id> | edit <id> | rm <id> | mv <id> <ws>
nebula rag app add <name> --repo <path-on-findl>
nebula rag sync [app]
```

## Key decisions

- **Workspaces are hierarchical paths**, one physical index filtered by path prefix.
  Searching a node includes its whole subtree.
  ```
  /
  ├── shared            # "general": conventions, know-how, diverse/…
  └── apps/<name>
      ├── docs          # auto, from the repo's markdown on origin/main
      └── notes         # manual / agent-written
  ```
  Default scope inside a repo: `apps/<repo>` + `shared`; `--ws /` searches everything.
  Repo → app mapping is stored in the DB and detected from the git remote name.
- **Workspace creation guards against duplicates:** `ws list` shows the tree with
  document counts; `ws create` refuses names close to existing ones and suggests them.
- **Content in v1:** notes, `/shared`, repo markdown docs (`.md`, specs, ADRs,
  READMEs), plus explicit `rag add <file|url>`. Respect `.gitignore`, add optional
  `.ragignore`, skip binaries and large generated files.
- **Docs indexing reads `origin/main` after `git fetch`** via `git ls-tree`/`git show`,
  never the working tree — findl checkouts may hold other agents' uncommitted work.
  Incremental: diff the last indexed commit against the new one; a file re-index is
  one transaction (delete by path, insert).
- **Freshness:** timer every ~10 min (fetch + diff + re-index) plus manual
  `nebula rag sync [app]`. No sync inside search, no laptop push hook in v1.
- **Agents write notes proactively under strict rules** (decision + reason, fix after
  a hard bug, non-obvious procedure; nothing derivable from code or git). Writes
  dedup semantically: similarity above ~0.9 updates the existing note. Agents list the
  notes they saved at the end of the session.
- **Note lifecycle:** each note has `created`, `updated`, `source` (session/agent).
  Search results show age. Agents verify a note before relying on it and fix or delete
  it when outdated. Deletion is soft (`deleted_at`), purged after 30 days.
- **Search:** dense vector search (sqlite-vec `vec0`, brute force, exact) is the default
  ranking; top 5. FTS5 (`porter unicode61`) is kept as an exact-term mode (quoted
  query / `--exact`), **not** fused by default: equal-weight RRF lowered Hit@5 and
  hurt Romanian queries in the eval spike. Revisit fusion only with a tuned dev set.
- **Workspace filter:** one `vec0` table with an integer `ws_id` metadata column;
  resolve the requested path(s) to the deduplicated set of subtree workspace ids and
  query `embedding MATCH ? AND k = ? AND ws_id IN (...)`; `/` → unfiltered. Apply the
  same set to FTS. Never post-filter a global top-k. `partition key` is rejected
  (returns per-partition top-k, not a global ranking). Default SQLite memory settings
  (the 4 GiB mmap + 512 MiB cache variant cost ~1.2 GiB RSS at 100k).
- **Output per hit:** workspace, source (`nebula/docs/x.md#Heading` or `note:<id>`),
  score, age, chunk text.
- **Chunking:** markdown split on headings, max ~400 tokens; each chunk carries its
  heading chain (`README > Install > zig`) as context.
- **Embeddings:** Workers AI `@cf/baai/bge-m3` via REST, 1024 dims, same `{text}`
  input for documents and queries (no instruction prefix). The model id is stored with
  the index; changing it requires re-embedding.
- **Secrets:** Cloudflare API token and R2 keys in
  `~/.local/share/nebula/rag.env` on findl, mode 600, never in the repo.
- **Backup:** Litestream as a systemd unit on findl, version pinned (0.5.17),
  replicating to a single R2 prefix. Only one replicator may ever write that prefix.
- **Failure mode:** findl unreachable → clear error. On findl, `nebula rag` runs
  locally without ssh.
- **Runtime on findl:** systemd **user** units (linger is on; same pattern as
  `nebula-web.service`), installed by `nebula rag setup`:
  `nebula-rag-sync.timer` (every 10 min → `nebula rag sync`) and
  `nebula-rag-litestream.service` (`EnvironmentFile=rag.env`). Not inside the nebula
  daemon: backup must survive daemon crashes, and RAG updates must not need
  `nebula kill`.
- **Agent skill is global**, not in this repo: `~/.agents/skills/rag/SKILL.md`, symlinked
  into `~/.claude/skills`, `~/.codex/skills`, `~/.cursor/skills`, `~/.pi/agent/skills`,
  synced to findl with `nss`. Plus one always-on line in each agent's global
  instructions (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, pi, cursor):
  > RAG: before non-trivial work run `nebula rag search`; after a decision, hard fix or
  > non-obvious procedure, save it (see `rag` skill).
  A nebula end-of-session hook nudge is deferred until saves prove too rare.

### Skill rules (agreed text)

```markdown
## When to search
- Before non-trivial work in a project: `nebula rag search "<what you're about to do>"`
  (default scope: this app + /shared). Also when the user references past decisions
  ("cum am făcut X", "de ce am ales Y") or another project.
- Treat hits as leads, not truth: check age, verify against code before relying on it.
  If a note is wrong or stale, fix it (`note edit`) or delete it (`note rm`).

## When to save (proactively, no need to ask)
Save only if a future agent would otherwise waste time or repeat a mistake:
- A decision + its reason ("chose X over Y because Z").
- A fix after a hard bug: symptom → root cause → fix.
- A non-obvious procedure: deploy steps, setup, recovery, a command that works.
- A user preference or constraint about how work is done in a project.
- An external fact that was costly to find (API quirk, pricing, limit).

## Never save
- Anything derivable from code, git log, README, or the spec itself.
- Session progress, TODOs, temporary state.
- Secrets, tokens, credentials, personal data.
- Guesses — only verified facts.

## Where
- Project-specific → `apps/<app>/notes`. Cross-project know-how → `shared/...`.
- Before creating a workspace, `nebula rag ws list`; reuse an existing node.
- One fact per note, with a clear title; write it so it's understandable with no context.

## Report
At the end of the task, list what you saved: `saved: <title> → <ws> (note:<id>)`.
```

## Existing infrastructure verified

- CLI is clap derive (`crates/nebula/src/main.rs`); subcommand logic lives in
  `crates/nebula-tui/src/*_cli.rs` (pattern: `remote_cli.rs`, `diagram_cli.rs`).
- `nebula remote` already shells `ssh <host> …` in BatchMode.
- `rusqlite` 0.40 (bundled, FTS5 included), `axum` 0.8, `tokio`, `serde` present.
  The `sqlite-vec` crate links against bundled rusqlite. ~~New dependency: an HTTP client
  (`reqwest`) for Workers AI.~~ **Implemented instead:** Workers AI is called by shelling
  out to `curl` (the pattern `nebula upgrade` already uses), config and token passed on
  curl's stdin (`-K -`), never argv. Reason: the workspace has no TLS stack, and reqwest
  would add rustls/ring or openssl for one POST per batch.
- Data dir on findl: `~/.local/share/nebula/` (next to `nebula.db`).
- findl: 8 cores, 15 GiB RAM with swap in use, no GPU, 255 GB free disk.

## Provisioned (2026-09-23)

- R2 bucket `nebula-rag` (dedicated, account `84857d495e0414de227d5f080d7ba088`), empty.
- `findl:~/.local/share/nebula/rag.env` (mode 600): `CF_ACCOUNT_ID`, `CF_AI_TOKEN`
  (Workers AI), `R2_BUCKET`, `R2_ENDPOINT`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`,
  `R2_API_TOKEN`. R2 token scoped to `nebula-rag` only. Verified from findl: R2
  PUT/GET/DELETE and a real 1024-dim embedding. Tokens passed through chat once —
  roll them after setup if that matters.
- Workers AI works on the Workers Free plan (10k neurons/day free, embeddings not
  paid-only); beyond the daily allocation calls fail until 00:00 UTC. Bulk indexing
  must batch, cap daily usage, and resume the next day.

## Spikes & experiments

- **Litestream → R2 — passed** (v0.5.17 on findl, details in the spike's RESULTS.md):
  config needs only `type: s3`, bucket, path, `endpoint: ${R2_ENDPOINT}`,
  `region: auto`, keys (R2 auto-detected). First upload ~1.6 s, full restore of 21 MB
  in 2.7 s with identical data; `-timestamp` point-in-time restore works; ~70–75 MB RSS.
  Gotchas: idle monitors issue ~660 LIST/h (≈475k Class A/month — fits the free 1M, and
  is a reason for **one DB**, not one per workspace); each restart uploads a full
  snapshot; per-txn restore points only for 5 min, then 30 s / 5 m / 1 h; ~2× DB disk
  during bulk writes; `restore` needs `-f` to overwrite; logs print the endpoint URL.
- **sqlite-vec + FTS5 — passed** (`.scratch/rag/rag-spike/RESULTS.md`): sqlite-vec 0.1.9
  + rusqlite 0.40.2 bundled (SQLite 3.53.2, FTS5) builds and runs on macOS arm64 and
  findl x86_64; registered via `sqlite3_auto_extension`. findl, 30k × 1024, default
  scope (app + shared): vec0 metadata filter 57 / 69 ms (median / p95), unfiltered
  79 / 88 ms, FTS5 top-50 24 / 41 ms; peak RSS ~13 MiB; replacing a 20-chunk source
  68 ms. At 100k the default scope is 185 ms — fine for v1 volume, Qdrant if it grows.
  Synthetic data, warm single process; embedding call + ssh come on top.
- **Embedding quality — bge-m3 wins** (`.scratch/rag/embed-eval/RESULTS.md`, 483 real
  markdown chunks, 40 EN/RO queries): Hit@5 / MRR@10 — bge-m3 0.85 / 0.69; qwen3 with
  API default instruction 0.75 / 0.60; qwen3 custom instruction 0.50 / 0.34; BM25 0.40 /
  0.35; RRF(bge-m3 + BM25) 0.80 / 0.63. Romanian Hit@5: bge-m3 0.85 vs qwen3 0.62.
  Directional only (noisy labels, partly sparse score caches). Re-run on the production
  corpus with clean labels before calling the model final. Spike used ~408k tokens /
  ~440 neurons.

## Implementation notes (v1, 2026-09-23)

Where the code settles what the design left open, or deviates from it:

- **Crate:** `crates/nebula-rag` (library + its clap `Args`, `cli.rs`), dispatched from
  `crates/nebula/src/main.rs` as `Rag(nebula_rag::cli::RagArgs)`. Not a
  `nebula-tui/src/rag_cli.rs`: the TUI crate stays free of SQLite/sqlite-vec, and the
  command surface is big enough that mirroring it as an `Op` enum in main.rs would only
  duplicate it.
- **HTTP:** `curl` subprocess instead of reqwest (see "Existing infrastructure verified").
- **sqlite-vec registration** is per connection (`sqlite3_vec_init` on the handle), not
  `sqlite3_auto_extension`: the nebula binary also links the daemon's SQLite.
- **Schema:** notes are `documents` rows with `kind = 'note'` (kinds: doc, note, file,
  url) sharing the chunk/vector/FTS pipeline; ids are `note:<id>` / document ids. Soft
  delete removes a note's chunks from the indexes at once (row kept 30 days), so search
  never post-filters. `PRAGMA user_version` numbers the migrations.
- **Neuron cap:** indexing stops at 8,000 neurons/UTC day (`NEBULA_RAG_NEURON_CAP`);
  searches are never capped. Cost comes from the response's `meta.neurons` (estimate
  ≈3 chars/token when absent). A stopped sync does not advance `last_commit`; the next
  run re-diffs and skips blobs already indexed.
- **Dedup** compares a new note with live notes in the **same workspace** only, on its
  first chunk's vector (notes are one fact). The same fact in another app is kept.
- **Sync:** branch = `origin/HEAD` of the checkout at `app add` (else `main`; `--branch`
  overrides). Only the repo-root `.gitignore` and `.ragignore` are read (nested ignore
  files are not); `node_modules/`, `/vendor/`, `third_party/` are always skipped;
  `.md/.markdown/.mdx` up to 256 KiB, NUL-containing or non-UTF-8 files skipped.
  `apps/<app>/docs` is owned by sync: `rag add <file|url>` needs an explicit `--ws` and
  refuses that workspace.
- **Local vs ssh:** local when `NEBULA_RAG_LOCAL=1`, the hostname equals the RAG host
  (`NEBULA_RAG_HOST`, default `findl`), or `rag.env` exists here. A local file given to
  `add` travels over ssh stdin. `note edit` over ssh needs `--text` or piped stdin.
- **Default scope:** `apps/<app>` + `shared`, with the app from the registered remote or,
  failing that, the repo name; outside any git checkout, `/`.
- **Backup:** `nebula rag setup` writes the units and `litestream.yml` (replica
  `<prefix>/rag.db`, default prefix `prod`) and downloads Litestream 0.5.17 to
  `<data_dir>/bin` (sha256 pinned); `--enable` is a separate, explicit step.

## Open questions

None blocking. Follow-ups: re-evaluate the model on the real `origin/main` corpus with
independently written labels; corpus has conflicting docs (e.g. release vs protected
branch rules) — search results show source + age, authority is the agent's call.

Remote deployment must follow the hybrid-work runbook and must not restart the host
nebula daemon or its active agent sessions without asking.
