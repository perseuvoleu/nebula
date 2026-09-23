//! The RAG database: one SQLite file (`rag.db` beside `nebula.db`) holding
//! the workspace tree, documents, their chunks, a sqlite-vec `vec0` index
//! and an FTS5 index. Litestream replicates the file as a whole, which is
//! why everything lives in one DB (see docs/specs/rag.md).

use anyhow::{bail, Context, Result};
use rusqlite::{ffi, Connection};
use std::path::{Path, PathBuf};

/// Embedding model the index is built with. Changing it means re-embedding
/// every chunk, so the id is stored in `meta` and checked on open.
pub const MODEL: &str = "@cf/baai/bge-m3";
pub const DIMS: usize = 1024;

pub fn db_path() -> PathBuf {
    nebula_core::paths::data_dir().join("rag.db")
}

/// Ordered migrations; `PRAGMA user_version` is the count applied.
const MIGRATIONS: &[&str] = &[r#"
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
"#];

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
    migrate(&mut conn)?;
    check_model(&conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let applied = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
    if applied > MIGRATIONS.len() {
        bail!(
            "rag.db schema v{applied} is newer than this nebula (v{}) — upgrade nebula",
            MIGRATIONS.len()
        );
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("rag migration {}", i + 1))?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

fn check_model(conn: &Connection) -> Result<()> {
    let stored: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'model'", [], |r| {
            r.get(0)
        })
        .ok();
    match stored {
        None => {
            conn.execute("INSERT INTO meta(key, value) VALUES ('model', ?1)", [MODEL])?;
        }
        Some(m) if m != MODEL => {
            bail!(
                "rag.db was embedded with {m}, this nebula uses {MODEL}: re-embedding is required"
            )
        }
        Some(_) => {}
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
}
