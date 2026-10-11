//! The RAG database: one SQLite file (`rag.db` beside `nebula.db`) holding
//! the workspace tree, documents, their chunks, a sqlite-vec `vec0` index
//! and an FTS5 index. Litestream replicates the file as a whole, which is
//! why everything lives in one DB (see docs/specs/rag.md).

use anyhow::{bail, Context, Result};
use rusqlite::{ffi, Connection, TransactionBehavior};
use std::path::{Path, PathBuf};

/// Embedding model the index is built with. Changing it means re-embedding
/// every chunk, so the id is stored in `meta` and checked on open.
pub const MODEL: &str = "@cf/baai/bge-m3";
pub const DIMS: usize = 1024;

pub fn db_path() -> PathBuf {
    nebula_core::paths::data_dir().join("rag.db")
}

/// Ordered migrations; `PRAGMA user_version` is the count applied.
const MIGRATIONS: &[&str] = &[SCHEMA, OBSERVATION_FIELDS, BACKFILL_LEGACY_OBSERVATIONS];

const SCHEMA: &str = r#"
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE workspaces(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    created INTEGER NOT NULL
);
-- Repos whose origin/main markdown is indexed into apps/<name>/docs.
CREATE TABLE apps(
    name TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    remote TEXT,
    branch TEXT NOT NULL DEFAULT 'main',
    last_commit TEXT,
    synced_at INTEGER,
    created INTEGER NOT NULL
);
-- kind: doc (repo markdown), note, file, url. Notes are soft-deleted
-- (deleted_at, purged after 30 days); their chunks leave the indexes at
-- deletion so search never has to post-filter.
CREATE TABLE documents(
    id INTEGER PRIMARY KEY,
    ws_id INTEGER NOT NULL REFERENCES workspaces(id),
    kind TEXT NOT NULL CHECK (kind IN ('doc', 'note', 'file', 'url')),
    app TEXT,
    path TEXT,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    blob TEXT,
    source TEXT,
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE UNIQUE INDEX documents_ws_path ON documents(ws_id, path)
    WHERE path IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX documents_app ON documents(app, path);
CREATE TABLE chunks(
    id INTEGER PRIMARY KEY,
    doc_id INTEGER NOT NULL REFERENCES documents(id),
    ws_id INTEGER NOT NULL,
    ord INTEGER NOT NULL,
    heading TEXT NOT NULL,
    text TEXT NOT NULL
);
CREATE INDEX chunks_doc ON chunks(doc_id);
CREATE VIRTUAL TABLE chunk_vec USING vec0(
    embedding float[1024] distance_metric=cosine,
    ws_id integer
);
CREATE VIRTUAL TABLE chunk_fts USING fts5(text, tokenize='porter unicode61');
-- Workers AI neurons spent per UTC day (the free allocation is 10k/day).
CREATE TABLE embed_usage(
    day TEXT PRIMARY KEY,
    neurons REAL NOT NULL,
    tokens INTEGER NOT NULL
);
INSERT INTO workspaces(path, created) VALUES ('shared', unixepoch());
"#;

const OBSERVATION_FIELDS: &str = r#"
ALTER TABLE documents ADD COLUMN category TEXT CHECK (
    category IS NULL
    OR (category IN ('correction', 'rework', 'search', 'skill', 'tool') AND kind = 'note')
);
ALTER TABLE documents ADD COLUMN status TEXT CHECK (
    status IN ('open', 'done', 'dismissed') AND (status IS NULL) = (category IS NULL)
);
ALTER TABLE documents ADD COLUMN closed_at INTEGER CHECK (
    (closed_at IS NULL) = (status IS NULL OR status = 'open')
);
ALTER TABLE documents ADD COLUMN resolution TEXT CHECK (
    resolution IS NULL OR closed_at IS NOT NULL
);
"#;

const BACKFILL_LEGACY_OBSERVATIONS: &str = r#"
UPDATE documents SET status = 'open', category = CASE
        WHEN substr(title, 1, instr(title, ': ') - 1)
             IN ('correction', 'rework', 'search', 'skill', 'tool')
        THEN substr(title, 1, instr(title, ': ') - 1)
        ELSE 'tool' END
    WHERE kind = 'note' AND ws_id IN (
        SELECT id FROM workspaces WHERE path = 'observations' OR path LIKE '%/observations'
    );
"#;

/// Register sqlite-vec on this connection only. The spike used
/// `sqlite3_auto_extension`, which is process-wide; the nebula binary also
/// links the daemon's own SQLite, so scope it to the RAG connection.
fn register_vec(conn: &Connection) -> Result<()> {
    type Init = unsafe extern "C" fn(
        *mut ffi::sqlite3,
        *mut *mut std::os::raw::c_char,
        *const ffi::sqlite3_api_routines,
    ) -> i32;
    // SAFETY: sqlite3_vec_init has the loadable-extension entry signature;
    // the handle is live for the call and a null api pointer is accepted by
    // statically linked builds (SQLITE_CORE).
    let rc = unsafe {
        let init =
            std::mem::transmute::<*const (), Init>(sqlite_vec::sqlite3_vec_init as *const ());
        init(conn.handle(), std::ptr::null_mut(), std::ptr::null())
    };
    if rc != ffi::SQLITE_OK {
        bail!("sqlite-vec init failed ({rc})");
    }
    Ok(())
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    setup(conn)
}

#[cfg(test)]
pub fn open_memory() -> Result<Connection> {
    setup(Connection::open_in_memory()?)
}

fn setup(mut conn: Connection) -> Result<Connection> {
    register_vec(&conn)?;
    // Default cache/mmap on purpose: the 4 GiB mmap variant cost ~1.2 GiB RSS.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 10000;",
    )?;
    migrate(&mut conn, MIGRATIONS)?;
    check_model(&conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let applied = |c: &Connection| -> Result<usize> {
        let applied = c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
        if applied > migrations.len() {
            bail!(
                "rag.db schema v{applied} is newer than this nebula (v{}) — upgrade nebula",
                migrations.len()
            );
        }
        Ok(applied)
    };
    if applied(conn)? == migrations.len() {
        return Ok(());
    }
    // The write lock comes first and the version is read again under it:
    // another process opening the database now may have migrated it since.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (i, sql) in migrations.iter().enumerate().skip(applied(&tx)?) {
        tx.execute_batch(sql)
            .with_context(|| format!("rag migration {}", i + 1))?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
    }
    tx.commit()?;
    Ok(())
}

fn check_model(conn: &Connection) -> Result<()> {
    let stored = || -> Option<String> {
        conn.query_row("SELECT value FROM meta WHERE key = 'model'", [], |r| {
            r.get(0)
        })
        .ok()
    };
    let model = match stored() {
        Some(m) => m,
        None => {
            // OR IGNORE, then read back: another first open may have won.
            conn.execute(
                "INSERT OR IGNORE INTO meta(key, value) VALUES ('model', ?1)",
                [MODEL],
            )?;
            stored().context("rag.db has no model row after writing it")?
        }
    };
    if model != MODEL {
        bail!(
            "rag.db was embedded with {model}, this nebula uses {MODEL}: re-embedding is required"
        )
    }
    Ok(())
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn vec_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_with_vec_and_fts_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rag.db");
        drop(open(&path).unwrap());
        let conn = open(&path).unwrap();
        let v: String = conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "v0.1.9");
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM workspaces WHERE path = 'shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn refuses_an_index_built_with_another_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rag.db");
        let conn = open(&path).unwrap();
        conn.execute("UPDATE meta SET value = 'other' WHERE key = 'model'", [])
            .unwrap();
        drop(conn);
        let err = open(&path).unwrap_err().to_string();
        assert!(err.contains("re-embedding"), "{err}");
    }

    #[test]
    fn simultaneous_first_opens_after_an_upgrade_all_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rag.db");
        let mut v1 = Connection::open(&path).unwrap();
        register_vec(&v1).unwrap();
        v1.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        migrate(&mut v1, &MIGRATIONS[..1]).unwrap();
        drop(v1);

        let start = std::sync::Barrier::new(8);
        let errors: Vec<String> = std::thread::scope(|s| {
            let opens: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        open(&path).map(drop).map_err(|e| format!("{e:#}"))
                    })
                })
                .collect();
            opens
                .into_iter()
                .filter_map(|t| t.join().unwrap().err())
                .collect()
        });
        assert_eq!(errors, Vec::<String>::new());
        let columns: i64 = open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM pragma_table_info('documents') WHERE name = 'category'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1);
    }

    #[test]
    fn notes_in_observations_workspaces_become_open_observations() {
        use crate::obs::{Category, ObsFilter, Status};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rag.db");
        let mut v1 = Connection::open(&path).unwrap();
        register_vec(&v1).unwrap();
        migrate(&mut v1, &MIGRATIONS[..1]).unwrap();
        v1.execute_batch(
            "INSERT INTO workspaces(id, path, created) VALUES
                 (10, 'apps/x/observations', 0), (11, 'shared/observations', 0), (12, 'apps/x/notes', 0);
             INSERT INTO documents(id, ws_id, kind, title, body, created, updated) VALUES
                 (1, 10, 'note', 'rework: tests rewritten twice', 'b1', 100, 100),
                 (2, 11, 'note', 'the hop dropped a quote', 'b2', 200, 200),
                 (3, 12, 'note', 'rework: a plain note', 'b3', 300, 300);",
        )
        .unwrap();
        drop(v1);

        let rag = crate::Rag::with_conn(
            open(&path).unwrap(),
            Box::new(crate::embed::fake::FakeEmbedder::default()),
        );
        let got: Vec<_> = rag
            .obs_list(&ObsFilter::default())
            .unwrap()
            .into_iter()
            .map(|o| (o.id, o.app, o.category, o.status, o.title))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    2,
                    "shared".to_string(),
                    Category::Tool,
                    Status::Open,
                    "the hop dropped a quote".to_string()
                ),
                (
                    1,
                    "x".to_string(),
                    Category::Rework,
                    Status::Open,
                    "rework: tests rewritten twice".to_string()
                ),
            ]
        );
        assert_eq!(
            rag.obs(3).unwrap_err().to_string(),
            "note:3 is a note, not an observation"
        );
        let titles: Vec<String> = rag
            .notes(None, false)
            .unwrap()
            .into_iter()
            .map(|n| n.title)
            .collect();
        assert_eq!(
            titles,
            vec![
                "rework: a plain note",
                "the hop dropped a quote",
                "rework: tests rewritten twice"
            ]
        );
    }
}
