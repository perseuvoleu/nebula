//! The RAG library: workspaces, documents and notes, indexing and search
//! over one `rag.db`. Network work (embedding) always happens before a
//! write transaction opens; each document is replaced in one transaction.

use crate::chunk::{anchor_of, chunk_markdown, Chunk};
use crate::db::{self, now, vec_bytes};
use crate::embed::{estimate_neurons, CapReached, Embedder, BATCH};
use crate::ws;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;

/// Cosine similarity at which a new note updates an existing one instead.
pub const DEDUP_SIMILARITY: f64 = 0.9;
/// Soft-deleted notes are purged after this long.
pub const PURGE_AFTER_SECS: i64 = 30 * 86_400;
/// Default share of the 10k neurons/day free allocation that indexing may
/// spend; the rest is headroom for searches. `NEBULA_RAG_NEURON_CAP`
/// overrides.
pub const DEFAULT_NEURON_CAP: f64 = 8_000.0;

pub struct Rag {
    pub conn: Connection,
    embedder: Box<dyn Embedder>,
    pub neuron_cap: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Doc,
    Note,
    File,
    Url,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Doc => "doc",
            Kind::Note => "note",
            Kind::File => "file",
            Kind::Url => "url",
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct Hit {
    pub ws: String,
    pub source: String,
    pub score: f64,
    pub updated: i64,
    pub kind: String,
    pub doc_id: i64,
    pub title: String,
    pub heading: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct WsRow {
    pub path: String,
    /// Live documents (all kinds) directly in this workspace.
    pub docs: i64,
    pub notes: i64,
}

#[derive(Debug, Serialize)]
pub struct Note {
    pub id: i64,
    pub ws: String,
    pub title: String,
    pub body: String,
    pub source: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, PartialEq)]
pub enum NoteAdded {
    Created(i64),
    /// An existing note was this similar and was updated in place.
    Updated(i64, f64),
}

/// A document to (re)index.
pub struct NewDoc<'a> {
    pub ws_id: i64,
    pub kind: Kind,
    pub app: Option<&'a str>,
    pub path: Option<&'a str>,
    pub title: &'a str,
    pub body: &'a str,
    pub blob: Option<&'a str>,
    pub source: Option<&'a str>,
}

impl Rag {
    pub fn open(path: &std::path::Path, embedder: Box<dyn Embedder>) -> Result<Self> {
        Ok(Self::with_conn(db::open(path)?, embedder))
    }

    pub fn with_conn(conn: Connection, embedder: Box<dyn Embedder>) -> Self {
        let neuron_cap = std::env::var("NEBULA_RAG_NEURON_CAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_NEURON_CAP);
        Self {
            conn,
            embedder,
            neuron_cap,
        }
    }

    // ---- embedding budget ----

    pub fn neurons_today(&self) -> Result<f64> {
        Ok(self
            .conn
            .query_row(
                "SELECT neurons FROM embed_usage WHERE day = ?1",
                [utc_day(now())],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0.0))
    }

    /// Embed `texts`, recording the neurons spent. `capped` calls (indexing)
    /// refuse up front when the estimate would cross the daily cap; queries
    /// are tiny and always go through.
    pub fn embed(&self, texts: &[String], capped: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if capped {
            let used = self.neurons_today()?;
            let est = estimate_neurons(texts);
            if used + est > self.neuron_cap {
                return Err(CapReached(format!(
                    "{used:.0} of {:.0} neurons used today (UTC), this needs ~{est:.1}",
                    self.neuron_cap
                ))
                .into());
            }
        }
        // Batch here so each batch's cost is recorded as soon as it is
        // spent, even when a later batch fails.
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let e = self.embedder.embed(batch)?;
            // No cost in the response: count the (high) estimate rather
            // than zero, or the cap could never trigger.
            let neurons = if e.neurons > 0.0 {
                e.neurons
            } else {
                estimate_neurons(batch)
            };
            self.conn.execute(
                "INSERT INTO embed_usage(day, neurons, tokens) VALUES (?1, ?2, ?3)
                 ON CONFLICT(day) DO UPDATE SET neurons = neurons + ?2, tokens = tokens + ?3",
                params![utc_day(now()), neurons, e.tokens as i64],
            )?;
            vectors.extend(e.vectors);
        }
        Ok(vectors)
    }

    // ---- workspaces ----

    pub fn ws_id(&self, path: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM workspaces WHERE path = ?1", [path], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn ws_path(&self, id: i64) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT path FROM workspaces WHERE id = ?1", [id], |r| {
                r.get(0)
            })?)
    }

    /// Existing workspace by (normalized) path, or an error naming `ws list`.
    pub fn ws_require(&self, path: &str) -> Result<i64> {
        let norm =
            ws::normalize(path)?.ok_or_else(|| anyhow!("`/` is not a workspace to write to"))?;
        self.ws_id(&norm)?.ok_or_else(|| {
            anyhow!(
                "no workspace {norm} — see `nebula rag ws list`, or `nebula rag ws create {norm}`"
            )
        })
    }

    /// Create `path` and any missing parents, without the duplicate guard
    /// (used for the fixed `apps/<name>/{docs,notes}` layout).
    pub fn ws_ensure(&self, path: &str) -> Result<i64> {
        let norm = ws::normalize(path)?.ok_or_else(|| anyhow!("`/` always exists"))?;
        let mut id = 0;
        let mut acc = String::new();
        for seg in norm.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            self.conn.execute(
                "INSERT OR IGNORE INTO workspaces(path, created) VALUES (?1, ?2)",
                params![acc, now()],
            )?;
            id = self.ws_id(&acc)?.expect("just inserted");
        }
        Ok(id)
    }

    /// Workspaces whose leaf is close to `path`'s among its would-be
    /// siblings — what `ws create` refuses over.
    pub fn ws_near_duplicates(&self, path: &str) -> Result<Vec<String>> {
        let parent = ws::parent(path);
        let leaf = ws::leaf(path);
        Ok(self
            .ws_all()?
            .into_iter()
            .filter(|p| p != path && ws::parent(p) == parent && ws::similar(ws::leaf(p), leaf))
            .collect())
    }

    pub fn ws_create(&self, path: &str, force: bool) -> Result<(i64, bool)> {
        let norm = ws::normalize(path)?.ok_or_else(|| anyhow!("`/` always exists"))?;
        if let Some(id) = self.ws_id(&norm)? {
            return Ok((id, false));
        }
        if !force {
            let near = self.ws_near_duplicates(&norm)?;
            if !near.is_empty() {
                bail!(
                    "{norm} looks like an existing workspace: {} — reuse it, or pass --force",
                    near.join(", ")
                );
            }
        }
        Ok((self.ws_ensure(&norm)?, true))
    }

    pub fn ws_all(&self) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT path FROM workspaces ORDER BY path")?;
        let rows = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn ws_list(&self) -> Result<Vec<WsRow>> {
        let mut st = self.conn.prepare(
            "SELECT w.path,
                    (SELECT count(*) FROM documents d WHERE d.ws_id = w.id AND d.deleted_at IS NULL),
                    (SELECT count(*) FROM documents d WHERE d.ws_id = w.id AND d.deleted_at IS NULL AND d.kind = 'note')
             FROM workspaces w ORDER BY w.path",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(WsRow {
                    path: r.get(0)?,
                    docs: r.get(1)?,
                    notes: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Remove a workspace. A non-empty subtree (children or documents,
    /// including soft-deleted notes) needs `force`, which deletes it all.
    pub fn ws_rm(&mut self, path: &str, force: bool) -> Result<usize> {
        let norm = ws::normalize(path)?.ok_or_else(|| anyhow!("refusing to remove `/`"))?;
        self.ws_require(&norm)?;
        let ids = self
            .subtree_ids(std::slice::from_ref(&norm))?
            .unwrap_or_default();
        let docs = self.docs_in(&ids)?;
        if !force && (ids.len() > 1 || !docs.is_empty()) {
            bail!(
                "{norm} holds {} workspace(s) below it and {} document(s) — pass --force to delete them all",
                ids.len() - 1,
                docs.len()
            );
        }
        let tx = self.conn.transaction()?;
        for d in &docs {
            delete_chunks(&tx, *d)?;
            tx.execute("DELETE FROM documents WHERE id = ?1", [d])?;
        }
        for id in &ids {
            // Removing apps/<name> unregisters the app, or sync would
            // recreate it.
            let path: String =
                tx.query_row("SELECT path FROM workspaces WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            if let Some(app) = path.strip_prefix("apps/").filter(|a| !a.contains('/')) {
                tx.execute("DELETE FROM apps WHERE name = ?1", [app])?;
            }
            tx.execute("DELETE FROM workspaces WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(docs.len())
    }

    fn docs_in(&self, ws_ids: &[i64]) -> Result<Vec<i64>> {
        if ws_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT id FROM documents WHERE ws_id IN ({})",
            id_list(ws_ids)
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Deduplicated workspace ids of the subtrees under `paths`. `None`
    /// means unfiltered (`/` among them); unknown paths contribute nothing.
    pub fn subtree_ids(&self, paths: &[String]) -> Result<Option<Vec<i64>>> {
        let mut roots = Vec::new();
        for p in paths {
            match ws::normalize(p)? {
                None => return Ok(None),
                Some(n) => roots.push(n),
            }
        }
        let mut st = self.conn.prepare("SELECT id, path FROM workspaces")?;
        let mut ids: Vec<i64> = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(_, path)| roots.iter().any(|root| ws::in_subtree(path, root)))
            .map(|(id, _)| id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(Some(ids))
    }

    // ---- indexing ----

    /// Chunks of a document's body, labelled for their heading chain.
    pub fn chunks_for(label: &str, body: &str) -> Vec<Chunk> {
        chunk_markdown(label, body)
    }

    /// Insert or replace a document by (workspace, path) and index its
    /// chunks with the given vectors, in one transaction.
    pub fn put_doc(&mut self, doc: &NewDoc, chunks: &[Chunk], vectors: &[Vec<f32>]) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let existing: Option<(i64, String)> = match doc.path {
            Some(p) => tx
                .query_row(
                    "SELECT id, kind FROM documents WHERE ws_id = ?1 AND path = ?2 AND deleted_at IS NULL",
                    params![doc.ws_id, p],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?,
            None => None,
        };
        if let Some((id, kind)) = &existing {
            if kind != doc.kind.as_str() {
                bail!(
                    "{} is already a {kind} in this workspace ({kind}:{id}); pick another workspace or name",
                    doc.path.unwrap_or_default()
                );
            }
        }
        let t = now();
        let id = match existing {
            Some((id, _)) => {
                tx.execute(
                    "UPDATE documents SET kind = ?2, app = ?3, title = ?4, body = ?5, blob = ?6,
                     source = coalesce(?7, source), updated = ?8 WHERE id = ?1",
                    params![
                        id,
                        doc.kind.as_str(),
                        doc.app,
                        doc.title,
                        doc.body,
                        doc.blob,
                        doc.source,
                        t
                    ],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO documents(ws_id, kind, app, path, title, body, blob, source, created, updated)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                    params![doc.ws_id, doc.kind.as_str(), doc.app, doc.path, doc.title, doc.body, doc.blob, doc.source, t],
                )?;
                tx.last_insert_rowid()
            }
        };
        replace_chunks(&tx, id, doc.ws_id, chunks, vectors)?;
        tx.commit()?;
        Ok(id)
    }

    /// Chunk, embed (capped) and store a document.
    pub fn index_doc(&mut self, doc: &NewDoc, label: &str) -> Result<i64> {
        let chunks = Self::chunks_for(label, doc.body);
        let texts: Vec<String> = chunks.iter().map(Chunk::indexed).collect();
        let vectors = self.embed(&texts, true)?;
        self.put_doc(doc, &chunks, &vectors)
    }

    pub fn delete_doc(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        delete_chunks(&tx, id)?;
        tx.execute("DELETE FROM documents WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    // ---- notes ----

    /// Save a note in `ws_id`. Unless `dedup` is off, a live note in the
    /// same workspace at ≥ 0.9 similarity is updated instead (the new text
    /// replaces it; its id and created date stay).
    pub fn add_note(
        &mut self,
        ws_id: i64,
        title: Option<&str>,
        text: &str,
        source: Option<&str>,
        dedup: bool,
    ) -> Result<NoteAdded> {
        let text = text.trim();
        if text.is_empty() {
            bail!("empty note");
        }
        let title = title
            .map(str::to_string)
            .unwrap_or_else(|| default_title(text));
        let chunks = Self::chunks_for(&title, text);
        let texts: Vec<String> = chunks.iter().map(Chunk::indexed).collect();
        let vectors = self.embed(&texts, true)?;
        let similar = if dedup {
            self.nearest_note(ws_id, &vectors[0])?
                .filter(|(_, sim)| *sim >= DEDUP_SIMILARITY)
        } else {
            None
        };
        let tx = self.conn.transaction()?;
        let t = now();
        let result = match similar {
            Some((id, sim)) => {
                tx.execute(
                    "UPDATE documents SET title = ?2, body = ?3, source = coalesce(?4, source), updated = ?5 WHERE id = ?1",
                    params![id, title, text, source, t],
                )?;
                replace_chunks(&tx, id, ws_id, &chunks, &vectors)?;
                NoteAdded::Updated(id, sim)
            }
            None => {
                tx.execute(
                    "INSERT INTO documents(ws_id, kind, title, body, source, created, updated)
                     VALUES (?1, 'note', ?2, ?3, ?4, ?5, ?5)",
                    params![ws_id, title, text, source, t],
                )?;
                let id = tx.last_insert_rowid();
                replace_chunks(&tx, id, ws_id, &chunks, &vectors)?;
                NoteAdded::Created(id)
            }
        };
        tx.commit()?;
        Ok(result)
    }

    /// The live note in `ws_id` closest to `v`, with its cosine similarity.
    fn nearest_note(&self, ws_id: i64, v: &[f32]) -> Result<Option<(i64, f64)>> {
        let mut st = self.conn.prepare(
            "SELECT d.id, v.distance FROM chunk_vec v
             JOIN chunks c ON c.id = v.rowid JOIN documents d ON d.id = c.doc_id
             WHERE v.embedding MATCH ?1 AND v.k = 10 AND v.ws_id = ?2
               AND d.kind = 'note' AND d.deleted_at IS NULL
             ORDER BY v.distance LIMIT 1",
        )?;
        Ok(st
            .query_row(params![vec_bytes(v), ws_id], |r| {
                Ok((r.get::<_, i64>(0)?, 1.0 - r.get::<_, f64>(1)?))
            })
            .optional()?)
    }

    pub fn note(&self, id: i64) -> Result<Note> {
        self.conn
            .query_row(
                "SELECT d.id, w.path, d.title, d.body, d.source, d.created, d.updated, d.deleted_at
                 FROM documents d JOIN workspaces w ON w.id = d.ws_id
                 WHERE d.id = ?1 AND d.kind = 'note'",
                [id],
                note_row,
            )
            .optional()?
            .ok_or_else(|| anyhow!("no note:{id}"))
    }

    pub fn notes(&self, scope: Option<&[i64]>, deleted: bool) -> Result<Vec<Note>> {
        let filter = match scope {
            None => String::new(),
            Some(ids) => format!("AND d.ws_id IN ({})", id_list(ids)),
        };
        let del = if deleted { "IS NOT NULL" } else { "IS NULL" };
        let sql = format!(
            "SELECT d.id, w.path, d.title, d.body, d.source, d.created, d.updated, d.deleted_at
             FROM documents d JOIN workspaces w ON w.id = d.ws_id
             WHERE d.kind = 'note' AND d.deleted_at {del} {filter}
             ORDER BY d.updated DESC"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map([], note_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Replace a note's title and/or text and re-embed it.
    pub fn note_edit(
        &mut self,
        id: i64,
        title: Option<&str>,
        text: Option<&str>,
        source: Option<&str>,
    ) -> Result<()> {
        let n = self.note(id)?;
        if n.deleted_at.is_some() {
            bail!("note:{id} is deleted");
        }
        let body = text.map(str::trim).unwrap_or(&n.body).to_string();
        if body.is_empty() {
            bail!("empty note");
        }
        let title = title.map(str::to_string).unwrap_or(n.title);
        let ws_id = self.ws_require(&n.ws)?;
        let chunks = Self::chunks_for(&title, &body);
        let texts: Vec<String> = chunks.iter().map(Chunk::indexed).collect();
        let vectors = self.embed(&texts, true)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE documents SET title = ?2, body = ?3, source = coalesce(?4, source), updated = ?5 WHERE id = ?1",
            params![id, title, body, source, now()],
        )?;
        replace_chunks(&tx, id, ws_id, &chunks, &vectors)?;
        tx.commit()?;
        Ok(())
    }

    /// Soft delete: the note leaves the indexes now, its row stays for
    /// `PURGE_AFTER_SECS`.
    pub fn note_rm(&mut self, id: i64) -> Result<()> {
        let n = self.note(id)?;
        if n.deleted_at.is_some() {
            bail!("note:{id} is already deleted");
        }
        let tx = self.conn.transaction()?;
        delete_chunks(&tx, id)?;
        tx.execute(
            "UPDATE documents SET deleted_at = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn note_mv(&mut self, id: i64, ws_path: &str) -> Result<String> {
        let n = self.note(id)?;
        if n.deleted_at.is_some() {
            bail!("note:{id} is deleted");
        }
        let ws_id = self.ws_require(ws_path)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE documents SET ws_id = ?2, updated = ?3 WHERE id = ?1",
            params![id, ws_id, now()],
        )?;
        tx.execute(
            "UPDATE chunks SET ws_id = ?2 WHERE doc_id = ?1",
            params![id, ws_id],
        )?;
        tx.execute(
            "UPDATE chunk_vec SET ws_id = ?2 WHERE rowid IN (SELECT id FROM chunks WHERE doc_id = ?1)",
            params![id, ws_id],
        )?;
        tx.commit()?;
        self.ws_path(ws_id)
    }

    /// Hard-delete notes soft-deleted more than 30 days ago.
    pub fn purge(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM documents WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [now() - PURGE_AFTER_SECS],
        )?)
    }

    // ---- search ----

    /// Dense top-k over the scope (`None` = everything). The workspace
    /// filter runs inside the KNN (`ws_id IN (...)`), never as a
    /// post-filter of a global top-k.
    pub fn search(&self, query: &str, scope: Option<&[i64]>, k: usize) -> Result<Vec<Hit>> {
        if scope.is_some_and(|s| s.is_empty()) || k == 0 {
            return Ok(Vec::new());
        }
        let v = self
            .embed(&[query.to_string()], false)?
            .pop()
            .context("no query vector")?;
        let filter = match scope {
            None => String::new(),
            Some(ids) => format!("AND ws_id IN ({})", id_list(ids)),
        };
        let sql = format!(
            "SELECT rowid, distance FROM chunk_vec WHERE embedding MATCH ?1 AND k = ?2 {filter} ORDER BY distance"
        );
        let mut st = self.conn.prepare(&sql)?;
        let ranked: Vec<(i64, f64)> = st
            .query_map(params![vec_bytes(&v), k as i64], |r| {
                Ok((r.get(0)?, 1.0 - r.get::<_, f64>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        ranked
            .into_iter()
            .map(|(id, score)| self.hit(id, score))
            .collect()
    }

    /// FTS5 exact-term mode. A query wrapped in double quotes is one phrase;
    /// otherwise every word must appear. Score is -bm25 (higher is better).
    pub fn search_exact(&self, query: &str, scope: Option<&[i64]>, k: usize) -> Result<Vec<Hit>> {
        if scope.is_some_and(|s| s.is_empty()) || k == 0 {
            return Ok(Vec::new());
        }
        let Some(expr) = fts_expr(query) else {
            return Ok(Vec::new());
        };
        let filter = match scope {
            None => String::new(),
            Some(ids) => format!("AND c.ws_id IN ({})", id_list(ids)),
        };
        let sql = format!(
            "SELECT f.rowid, -bm25(chunk_fts) FROM chunk_fts f JOIN chunks c ON c.id = f.rowid
             WHERE chunk_fts MATCH ?1 {filter} ORDER BY bm25(chunk_fts) LIMIT ?2"
        );
        let mut st = self.conn.prepare(&sql)?;
        let ranked: Vec<(i64, f64)> = st
            .query_map(params![expr, k as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        ranked
            .into_iter()
            .map(|(id, score)| self.hit(id, score))
            .collect()
    }

    fn hit(&self, chunk_id: i64, score: f64) -> Result<Hit> {
        self.conn
            .query_row(
                "SELECT w.path, d.kind, d.app, d.path, d.id, d.title, d.updated, c.heading, c.text
                 FROM chunks c JOIN documents d ON d.id = c.doc_id JOIN workspaces w ON w.id = c.ws_id
                 WHERE c.id = ?1",
                [chunk_id],
                |r| {
                    let kind: String = r.get(1)?;
                    let app: Option<String> = r.get(2)?;
                    let path: Option<String> = r.get(3)?;
                    let doc_id: i64 = r.get(4)?;
                    let heading: String = r.get(7)?;
                    let anchor = anchor_of(&heading).map(|a| format!("#{a}")).unwrap_or_default();
                    let source = match (kind.as_str(), app, path) {
                        ("note", ..) => format!("note:{doc_id}"),
                        ("doc", Some(app), Some(path)) => format!("{app}/{path}{anchor}"),
                        (_, _, Some(path)) => format!("{path}{anchor}"),
                        _ => format!("{kind}:{doc_id}"),
                    };
                    Ok(Hit {
                        ws: r.get(0)?,
                        source,
                        score,
                        updated: r.get(6)?,
                        kind,
                        doc_id,
                        title: r.get(5)?,
                        heading,
                        text: r.get(8)?,
                    })
                },
            )
            .with_context(|| format!("chunk {chunk_id}"))
    }
}

fn note_row(r: &rusqlite::Row) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        ws: r.get(1)?,
        title: r.get(2)?,
        body: r.get(3)?,
        source: r.get(4)?,
        created: r.get(5)?,
        updated: r.get(6)?,
        deleted_at: r.get(7)?,
    })
}

fn replace_chunks(
    tx: &Transaction,
    doc_id: i64,
    ws_id: i64,
    chunks: &[Chunk],
    vectors: &[Vec<f32>],
) -> Result<()> {
    if chunks.len() != vectors.len() {
        bail!("{} chunks but {} vectors", chunks.len(), vectors.len());
    }
    delete_chunks(tx, doc_id)?;
    let mut ic = tx.prepare_cached(
        "INSERT INTO chunks(doc_id, ws_id, ord, heading, text) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut iv =
        tx.prepare_cached("INSERT INTO chunk_vec(rowid, embedding, ws_id) VALUES (?1, ?2, ?3)")?;
    let mut ifts = tx.prepare_cached("INSERT INTO chunk_fts(rowid, text) VALUES (?1, ?2)")?;
    for (i, (c, v)) in chunks.iter().zip(vectors).enumerate() {
        ic.execute(params![doc_id, ws_id, i as i64, c.heading, c.text])?;
        let id = tx.last_insert_rowid();
        iv.execute(params![id, vec_bytes(v), ws_id])?;
        ifts.execute(params![id, c.indexed()])?;
    }
    Ok(())
}

fn delete_chunks(tx: &Transaction, doc_id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM chunk_vec WHERE rowid IN (SELECT id FROM chunks WHERE doc_id = ?1)",
        [doc_id],
    )?;
    tx.execute(
        "DELETE FROM chunk_fts WHERE rowid IN (SELECT id FROM chunks WHERE doc_id = ?1)",
        [doc_id],
    )?;
    tx.execute("DELETE FROM chunks WHERE doc_id = ?1", [doc_id])?;
    Ok(())
}

/// Integer ids from the DB, so interpolation is safe (vec0 metadata `IN`
/// takes a literal list).
fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// FTS5 expression from user text: quoted → one phrase, else every word.
/// Words are quoted so FTS operators in the input stay literal.
pub fn fts_expr(query: &str) -> Option<String> {
    let q = query.trim();
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    if q.len() >= 2 && q.starts_with('"') && q.ends_with('"') {
        let inner = q[1..q.len() - 1].trim();
        return (!inner.is_empty()).then(|| quote(inner));
    }
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .filter(|w| !w.is_empty())
        .map(quote)
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn default_title(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("note");
    let line = line.trim().trim_start_matches('#').trim();
    if line.chars().count() > 80 {
        line.chars().take(79).collect::<String>() + "…"
    } else {
        line.to_string()
    }
}

/// `YYYY-MM-DD` (UTC) — the Workers AI allocation resets at 00:00 UTC.
pub fn utc_day(secs: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `3d`, `5h`, `now` — the age shown next to every hit.
pub fn age(updated: i64) -> String {
    let s = (now() - updated).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=5_183_999 => format!("{}d", s / 86_400),
        _ => format!("{}mo", s / 2_592_000),
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
