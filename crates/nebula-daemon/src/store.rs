//! SQLite persistence. Write volume is trivial (entity CRUD + status
//! changes), so a mutex-guarded connection is sufficient — no ORM, no
//! connection pool.

use anyhow::{Context, Result};
use nebula_core::board::Role;
use nebula_core::DispatchId;
use nebula_core::{
    Agent, AgentId, AgentKind, AgentStatus, PrSeen, Project, ProjectId, TerminalId, TerminalTab,
    TodoId, Workspace, WorkspaceId, Worktree, WorktreeId, DEFAULT_WORKSPACE_ID,
};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    "
    CREATE TABLE projects (
      id          TEXT PRIMARY KEY,
      name        TEXT NOT NULL,
      repo_path   TEXT NOT NULL UNIQUE,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL
    );
    CREATE TABLE worktrees (
      id          TEXT PRIMARY KEY,
      project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
      path        TEXT NOT NULL,
      branch      TEXT NOT NULL,
      is_main     INTEGER NOT NULL DEFAULT 0,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL,
      UNIQUE (project_id, path)
    );
    CREATE TABLE agents (
      id                TEXT PRIMARY KEY,
      worktree_id       TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
      name              TEXT NOT NULL,
      status            TEXT NOT NULL DEFAULT 'fresh',
      archived          INTEGER NOT NULL DEFAULT 0,
      claude_session_id TEXT,
      sort_order        INTEGER NOT NULL DEFAULT 0,
      created_at        INTEGER NOT NULL,
      status_changed_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE terminals (
      id          TEXT PRIMARY KEY,
      worktree_id TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
      name        TEXT NOT NULL DEFAULT 'shell',
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL
    );
    CREATE TABLE ui_state (
      id    INTEGER PRIMARY KEY CHECK (id = 1),
      json  TEXT NOT NULL
    );
    ",
    // 2: project group dividers
    "
    ALTER TABLE projects ADD COLUMN divider_after INTEGER NOT NULL DEFAULT 0;
    ",
    // 3: divider labels
    "
    ALTER TABLE projects ADD COLUMN divider_label TEXT;
    ",
    // 4: agent kind (claude | codex); claude_session_id doubles as the
    // resume id for whichever kind the agent runs.
    "
    ALTER TABLE agents ADD COLUMN kind TEXT NOT NULL DEFAULT 'claude';
    ",
    // 5: pinned agents (their own group in the sessions list)
    "
    ALTER TABLE agents ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
    ",
    // 6: pinned worktrees (their own group in the worktrees list)
    "
    ALTER TABLE worktrees ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
    ",
    // 7: the leading divider — drawn above the whole list, owned by the
    // first project
    "
    ALTER TABLE projects ADD COLUMN divider_before INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE projects ADD COLUMN divider_before_label TEXT;
    ",
    // 8: per-worktree todo notes
    "
    CREATE TABLE todos (
      id          TEXT PRIMARY KEY,
      worktree_id TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
      text        TEXT NOT NULL,
      done        INTEGER NOT NULL DEFAULT 0,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL
    );
    ",
    // 9: per-agent model/effort launch options (NULL = CLI default)
    "
    ALTER TABLE agents ADD COLUMN model TEXT;
    ALTER TABLE agents ADD COLUMN effort TEXT;
    ",
    // 10: todos gain a project scope — exactly one of project_id /
    // worktree_id is set. Table rebuild: SQLite can't relax the old
    // NOT NULL worktree_id in place. Existing rows stay worktree-owned.
    "
    CREATE TABLE todos_new (
      id          TEXT PRIMARY KEY,
      project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
      worktree_id TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
      text        TEXT NOT NULL,
      done        INTEGER NOT NULL DEFAULT 0,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL,
      CHECK ((project_id IS NULL) <> (worktree_id IS NULL))
    );
    INSERT INTO todos_new (id, worktree_id, text, done, sort_order, created_at)
      SELECT id, worktree_id, text, done, sort_order, created_at FROM todos;
    DROP TABLE todos;
    ALTER TABLE todos_new RENAME TO todos;
    ",
    // 11: when the agent was archived (orders the ARCHIVED group
    // newest-first; 0 for rows archived before this migration)
    "
    ALTER TABLE agents ADD COLUMN archived_at INTEGER NOT NULL DEFAULT 0;
    ",
    // 12: sessions created with the generated default name await one
    // agent-driven auto-title (`nebula rename` from inside the CLI);
    // cleared by the first rename, user- or agent-made. Daemon-internal —
    // never leaves the store, so pre-existing rows defaulting to 0 simply
    // keep their names.
    "
    ALTER TABLE agents ADD COLUMN auto_title_pending INTEGER NOT NULL DEFAULT 0;
    ",
    // 13: workspaces — named project groups, exactly one open (`active`) at
    // a time. Every install gets the built-in 'default' workspace and all
    // pre-existing projects move into it. The new projects column stays
    // nullable (SQLite forbids a non-NULL default on an added REFERENCES
    // column); reads COALESCE to 'default'.
    "
    CREATE TABLE workspaces (
      id          TEXT PRIMARY KEY,
      name        TEXT NOT NULL UNIQUE,
      active      INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL
    );
    INSERT INTO workspaces (id, name, active, created_at) VALUES ('default', 'default', 1, 0);
    ALTER TABLE projects ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
    UPDATE projects SET workspace_id = 'default';
    ",
    // 14: workspaces are free-form groupings, so the same repo may be added
    // to any number of them — uniqueness moves from a global repo_path
    // constraint to (workspace, repo_path). Table rebuild: SQLite can't
    // drop the inline UNIQUE. Runs with foreign keys off (see migrate())
    // so the DROP doesn't cascade into worktrees/agents/terminals/todos.
    "
    CREATE TABLE projects_new (
      id          TEXT PRIMARY KEY,
      name        TEXT NOT NULL,
      repo_path   TEXT NOT NULL,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL,
      divider_after INTEGER NOT NULL DEFAULT 0,
      divider_label TEXT,
      divider_before INTEGER NOT NULL DEFAULT 0,
      divider_before_label TEXT,
      workspace_id TEXT REFERENCES workspaces(id)
    );
    INSERT INTO projects_new (id, name, repo_path, sort_order, created_at, divider_after, divider_label, divider_before, divider_before_label, workspace_id)
      SELECT id, name, repo_path, sort_order, created_at, divider_after, divider_label, divider_before, divider_before_label, workspace_id FROM projects;
    DROP TABLE projects;
    ALTER TABLE projects_new RENAME TO projects;
    CREATE UNIQUE INDEX projects_workspace_repo ON projects (COALESCE(workspace_id, 'default'), repo_path);
    ",
    // 15: todos are now "notes" everywhere — rename the table to match.
    "
    ALTER TABLE todos RENAME TO notes;
    ",
    // 16: per-worktree links — pull requests, tickets, docs. Worktree-only
    // (unlike notes): a link describes the branch's work, and a project's
    // links would be the same for every checkout.
    "
    CREATE TABLE links (
      id          TEXT PRIMARY KEY,
      worktree_id TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
      url         TEXT NOT NULL,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL
    );
    ",
    // 17: how far the user has read into a pull request's conversation.
    // Keyed by URL rather than worktree — the PR is the thing that grows
    // comments, it outlives the checkout, and the same one can be pinned to
    // more than one of them.
    "
    CREATE TABLE pr_seen (
      url      TEXT PRIMARY KEY,
      marker   TEXT NOT NULL,
      seen_at  INTEGER NOT NULL
    );
    ",
    // 18: source branch or commit recorded when Nebula creates a worktree.
    // NULL means root checkout or an externally adopted worktree.
    "
    ALTER TABLE worktrees ADD COLUMN created_from TEXT;
    ",
    // 19: project-level orchestrator agents (spawned pinned, listed in
    // their own group, taught the nebula CLI verbs).
    "
    ALTER TABLE agents ADD COLUMN orchestrator INTEGER NOT NULL DEFAULT 0;
    ",
    // 20: first-class todos — a task list scoped to a project or one
    // worktree, separate from notes; each todo holds its own child notes.
    // (The `todos` name is free again: migration 15 renamed the old table
    // to `notes`.) Notes gain a third owner — a todo — so their table is
    // rebuilt to relax the two-way CHECK into exactly-one-of-three. Runs
    // with foreign keys off (see migrate()) so the DROP doesn't cascade.
    "
    CREATE TABLE todos (
      id          TEXT PRIMARY KEY,
      project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
      worktree_id TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
      text        TEXT NOT NULL,
      done        INTEGER NOT NULL DEFAULT 0,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL,
      CHECK ((project_id IS NULL) <> (worktree_id IS NULL))
    );
    CREATE TABLE notes_new (
      id          TEXT PRIMARY KEY,
      project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
      worktree_id TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
      todo_id     TEXT REFERENCES todos(id) ON DELETE CASCADE,
      text        TEXT NOT NULL,
      done        INTEGER NOT NULL DEFAULT 0,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL,
      CHECK ((project_id IS NOT NULL) + (worktree_id IS NOT NULL) + (todo_id IS NOT NULL) = 1)
    );
    INSERT INTO notes_new (id, project_id, worktree_id, text, done, sort_order, created_at)
      SELECT id, project_id, worktree_id, text, done, sort_order, created_at FROM notes;
    DROP TABLE notes;
    ALTER TABLE notes_new RENAME TO notes;
    ",
    // 21: checkouts created for a pre-existing branch (a session spawned
    // on a branch row) keep presenting as branches in the panel.
    "
    ALTER TABLE worktrees ADD COLUMN for_branch INTEGER NOT NULL DEFAULT 0;
    ",
    // 22: status for agent CLIs run by hand inside a shell tab (hook
    // events keyed by the terminal's `term:<id>` env). NULL = nothing has
    // reported; cleared whenever the tab's PTY dies or the daemon boots.
    "
    ALTER TABLE terminals ADD COLUMN status TEXT;
    ALTER TABLE terminals ADD COLUMN status_changed_at INTEGER NOT NULL DEFAULT 0;
    ",
    // 23: remote projects — the ssh destination whose filesystem the
    // project's paths belong to. NULL = this machine.
    "
    ALTER TABLE projects ADD COLUMN host TEXT;
    ",
    // 24: a remote project's checkouts and sessions live in the host's own
    // daemon and are mirrored live, never stored here; rows the earlier
    // ssh-spawn model wrote under host projects are dropped (agents and
    // terminals cascade with their worktrees).
    "
    DELETE FROM worktrees WHERE project_id IN (SELECT id FROM projects WHERE host IS NOT NULL);
    ",
    // 25: the branch a session was spawned on. The panel lists a session
    // under that branch even after its checkout switches to another one,
    // so a `nebula switch` on the primary leaves the chats where they
    // were. Existing rows are pinned to their checkout's branch as of the
    // upgrade — the first switch afterwards already leaves them behind;
    // NULL (only rows a newer client mirrors in) follows the checkout.
    "
    ALTER TABLE agents ADD COLUMN branch TEXT;
    ALTER TABLE terminals ADD COLUMN branch TEXT;
    UPDATE agents SET branch = (SELECT w.branch FROM worktrees w WHERE w.id = agents.worktree_id);
    UPDATE terminals SET branch = (SELECT w.branch FROM worktrees w WHERE w.id = terminals.worktree_id);
    ",
    // 26: the session spawned as its checkout's writer (`agent new --writer`)
    "
    ALTER TABLE agents ADD COLUMN writer INTEGER NOT NULL DEFAULT 0;
    ",
    // 27: the session whose `nebula agent new` spawned this one
    "
    ALTER TABLE agents ADD COLUMN spawned_by TEXT;
    ",
    // 28: the session spawned with Code Mode off (`agent new --no-code-mode`)
    "
    ALTER TABLE agents ADD COLUMN code_mode_off INTEGER NOT NULL DEFAULT 0;
    ",
    // 29: the board. Card columns replace the legacy todo owner, notes become
    // a card's log, and the board tables and the agents' card link appear.
    // Legacy rows convert in place: todos keep their ids (done → done, else
    // backlog), project/worktree notes become backlog cards, todo-owned
    // notes become log entries, and links become kind=link artifacts; then
    // the owner columns and the links table go. The author of a converted
    // row is this host's user, read from temp.migration_env (see migrate()).
    // Every write to a board table bumps board_meta.version through a
    // trigger, so no write path can forget the poll counter.
    "
    CREATE TABLE todos_new (
      id               TEXT PRIMARY KEY,
      number           INTEGER NOT NULL UNIQUE,
      title            TEXT NOT NULL,
      stage            TEXT NOT NULL CHECK (stage IN ('backlog', 'next', 'doing', 'review', 'done')),
      project          TEXT,
      checkout         TEXT,
      created_by       TEXT NOT NULL,
      proposed         INTEGER NOT NULL DEFAULT 0,
      sort_order       INTEGER NOT NULL DEFAULT 0,
      created_at       INTEGER NOT NULL,
      stage_changed_at INTEGER,
      archived_at      INTEGER
    );
    CREATE TEMP VIEW legacy_checkouts AS
      SELECT w.id, p.name AS project,
             json_object('host', COALESCE(p.host, e.host), 'branch', w.branch, 'path', w.path,
                         'worktree', CASE WHEN w.is_main THEN json('false') ELSE json('true') END) AS checkout
      FROM worktrees w JOIN projects p ON p.id = w.project_id, temp.migration_env e;
    CREATE TEMP VIEW legacy_author AS
      SELECT json_object('host', host, 'agent', NULL, 'name', user) AS author FROM temp.migration_env;
    INSERT INTO todos_new (id, number, title, stage, project, checkout, created_by, sort_order, created_at, stage_changed_at)
      SELECT c.id, ROW_NUMBER() OVER (ORDER BY c.created_at, c.id), c.text, c.stage,
             COALESCE(p.name, w.project), w.checkout, a.author, c.sort_order, c.created_at, c.created_at
      FROM (
        SELECT id, project_id, worktree_id, text, CASE WHEN done THEN 'done' ELSE 'backlog' END AS stage, sort_order, created_at FROM todos
        UNION ALL
        SELECT id, project_id, worktree_id, text, 'backlog', sort_order, created_at FROM notes WHERE todo_id IS NULL
      ) c
      LEFT JOIN projects p ON p.id = c.project_id
      LEFT JOIN legacy_checkouts w ON w.id = c.worktree_id, legacy_author a;
    CREATE TABLE notes_new (
      id         TEXT PRIMARY KEY,
      todo_id    TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
      text       TEXT NOT NULL,
      author     TEXT NOT NULL,
      sort_order INTEGER NOT NULL DEFAULT 0,
      created_at INTEGER NOT NULL
    );
    INSERT INTO notes_new (id, todo_id, text, author, sort_order, created_at)
      SELECT n.id, n.todo_id, n.text, a.author, n.sort_order, n.created_at
      FROM notes n, legacy_author a WHERE n.todo_id IS NOT NULL;
    CREATE TABLE artifacts (
      id           TEXT PRIMARY KEY,
      kind         TEXT NOT NULL,
      title        TEXT NOT NULL,
      storage_host TEXT,
      entry        TEXT,
      url          TEXT,
      body         TEXT,
      card_id      TEXT REFERENCES todos(id) ON DELETE SET NULL,
      project      TEXT,
      session      TEXT,
      checkout     TEXT,
      meta         TEXT NOT NULL DEFAULT '{}',
      created_at   INTEGER NOT NULL,
      removed_at   INTEGER,
      CHECK ((storage_host IS NULL) = (entry IS NULL)),
      CHECK ((entry IS NOT NULL) + (url IS NOT NULL) + (body IS NOT NULL) = 1)
    );
    INSERT INTO artifacts (id, kind, title, url, project, checkout, created_at)
      SELECT l.id, 'link', l.url, l.url, w.project, w.checkout, l.created_at
      FROM links l JOIN legacy_checkouts w ON w.id = l.worktree_id;
    DROP VIEW legacy_checkouts;
    DROP VIEW legacy_author;
    DROP TABLE links;
    DROP TABLE notes;
    DROP TABLE todos;
    ALTER TABLE notes_new RENAME TO notes;
    ALTER TABLE todos_new RENAME TO todos;
    ALTER TABLE agents ADD COLUMN card_id TEXT;
    ALTER TABLE agents ADD COLUMN role TEXT;
    ALTER TABLE agents ADD COLUMN dispatch_id TEXT;
    CREATE TABLE card_blocks (
      card_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
      by_id   TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
      PRIMARY KEY (card_id, by_id),
      CHECK (card_id <> by_id)
    );
    CREATE TABLE board_sessions (
      agent_id          TEXT PRIMARY KEY,
      host              TEXT NOT NULL,
      card_id           TEXT NOT NULL,
      role              TEXT CHECK (role IN ('worker', 'review', 'fix', 'verify', 'research')),
      kind              TEXT NOT NULL,
      name              TEXT NOT NULL,
      parent            TEXT,
      status            TEXT NOT NULL,
      status_changed_at INTEGER NOT NULL DEFAULT 0,
      writer            INTEGER NOT NULL DEFAULT 0,
      alive             INTEGER NOT NULL DEFAULT 0,
      archived          INTEGER NOT NULL DEFAULT 0,
      branch            TEXT,
      dispatch_id       TEXT,
      reported_at       INTEGER NOT NULL
    );
    CREATE TABLE board_hosts (
      host         TEXT PRIMARY KEY,
      writers      INTEGER NOT NULL DEFAULT 0,
      writer_slots INTEGER,
      checkouts    TEXT NOT NULL DEFAULT '[]',
      reported_at  INTEGER NOT NULL
    );
    CREATE TABLE dispatches (
      id         TEXT PRIMARY KEY,
      card_id    TEXT NOT NULL,
      host       TEXT NOT NULL,
      role       TEXT NOT NULL CHECK (role IN ('worker', 'review', 'fix', 'verify', 'research')),
      kind       TEXT NOT NULL,
      worktree   TEXT NOT NULL,
      state      TEXT NOT NULL CHECK (state IN ('pending', 'started', 'failed')),
      agent_id   TEXT,
      error      TEXT,
      created_at INTEGER NOT NULL
    );
    CREATE TABLE board_meta (
      id       INTEGER PRIMARY KEY CHECK (id = 1),
      board_id TEXT NOT NULL,
      version  INTEGER NOT NULL DEFAULT 0
    );
    INSERT INTO board_meta (id, board_id) VALUES (1, lower(hex(randomblob(16))));
    CREATE TRIGGER todos_board_insert AFTER INSERT ON todos BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER todos_board_update AFTER UPDATE ON todos BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER todos_board_delete AFTER DELETE ON todos BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER notes_board_insert AFTER INSERT ON notes BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER notes_board_update AFTER UPDATE ON notes BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER notes_board_delete AFTER DELETE ON notes BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER card_blocks_board_insert AFTER INSERT ON card_blocks BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER card_blocks_board_update AFTER UPDATE ON card_blocks BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER card_blocks_board_delete AFTER DELETE ON card_blocks BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER artifacts_board_insert AFTER INSERT ON artifacts BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER artifacts_board_update AFTER UPDATE ON artifacts BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER artifacts_board_delete AFTER DELETE ON artifacts BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER board_sessions_board_insert AFTER INSERT ON board_sessions BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER board_sessions_board_update AFTER UPDATE ON board_sessions BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER board_sessions_board_delete AFTER DELETE ON board_sessions BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER board_hosts_board_insert AFTER INSERT ON board_hosts BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER board_hosts_board_update AFTER UPDATE ON board_hosts BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER board_hosts_board_delete AFTER DELETE ON board_hosts BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER dispatches_board_insert AFTER INSERT ON dispatches BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER dispatches_board_update AFTER UPDATE ON dispatches BEGIN UPDATE board_meta SET version = version + 1; END;
    CREATE TRIGGER dispatches_board_delete AFTER DELETE ON dispatches BEGIN UPDATE board_meta SET version = version + 1; END;
    ",
];

/// A project's twin on a host, as the hook pointer needs it: the host, the
/// project name there, and each mirrored checkout's branch with the
/// unarchived sessions on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwinPointer {
    pub host: String,
    pub name: String,
    pub checkouts: Vec<(String, usize)>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

pub type TreeRows = (Vec<Project>, Vec<Worktree>, Vec<Agent>, Vec<TerminalTab>);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // Rebuild-style migrations DROP a parent table (14 rebuilds
        // projects); with enforcement on, the DROP's implicit delete would
        // cascade into every child table. Standard SQLite rebuild procedure:
        // foreign keys off for the migration window, back on after. (On a
        // migration error the connection is abandoned with Store::open's
        // failure, so the early return never leaks a live FK-off handle.)
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        // Migration 29 converts legacy rows into cards and artifacts whose
        // author and checkout name this host and user, values SQL cannot read.
        conn.execute_batch(
            "CREATE TEMP TABLE migration_env (host TEXT NOT NULL, user TEXT NOT NULL)",
        )?;
        conn.execute(
            "INSERT INTO temp.migration_env (host, user) VALUES (?1, ?2)",
            params![
                nebula_core::host::hostname(),
                std::env::var("USER").unwrap_or_else(|_| "user".into())
            ],
        )?;
        for (i, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            conn.execute_batch(&format!(
                "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                i + 1
            ))
            .with_context(|| format!("migration {}", i + 1))?;
        }
        conn.execute_batch("DROP TABLE temp.migration_env")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Migration 29 is static SQL, so it can only mint a hex board id;
        // the board cursor wants a ULID like every other id.
        conn.execute(
            "UPDATE board_meta SET board_id = ?1 WHERE length(board_id) <> 26",
            params![ulid::Ulid::generate().to_string()],
        )?;
        Ok(())
    }

    /// One board op as one immediate transaction: its reads see a single
    /// version and its writes bump it together.
    pub fn board_transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ---- workspaces ----

    pub fn insert_workspace(&self, w: &Workspace) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO workspaces (id, name, active, created_at) VALUES (?1, ?2, 0, ?3)",
            params![w.id.as_str(), w.name, now_ms()],
        )?;
        Ok(())
    }

    pub fn rename_workspace(&self, id: &WorkspaceId, name: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE workspaces SET name = ?2 WHERE id = ?1",
            params![id.as_str(), name],
        )?;
        Ok(())
    }

    pub fn delete_workspace(&self, id: &WorkspaceId) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM workspaces WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    /// Every workspace, oldest first (the 'default' one leads — it is
    /// created at time 0 by the migration).
    pub fn load_workspaces(&self) -> Result<Vec<Workspace>> {
        let conn = self.conn.lock().unwrap();
        let workspaces = conn
            .prepare("SELECT id, name FROM workspaces ORDER BY created_at, id")?
            .query_map([], |r| {
                Ok(Workspace {
                    id: WorkspaceId(r.get(0)?),
                    name: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(workspaces)
    }

    pub fn get_workspace(&self, id: &WorkspaceId) -> Result<Option<Workspace>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, name FROM workspaces WHERE id = ?1")?;
        let mut rows = stmt.query(params![id.as_str()])?;
        Ok(rows.next()?.map(|r| Workspace {
            id: WorkspaceId(r.get::<_, String>(0).unwrap()),
            name: r.get(1).unwrap(),
        }))
    }

    pub fn workspace_by_name(&self, name: &str) -> Result<Option<WorkspaceId>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM workspaces WHERE name = ?1")?;
        let mut rows = stmt.query(params![name])?;
        Ok(rows
            .next()?
            .map(|r| WorkspaceId(r.get::<_, String>(0).unwrap())))
    }

    /// The open workspace. Falls back to 'default' if no row is flagged
    /// (never expected — the migration flags it and switches keep exactly
    /// one flag set).
    pub fn active_workspace_id(&self) -> Result<WorkspaceId> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM workspaces WHERE active = 1 LIMIT 1")?;
        let mut rows = stmt.query([])?;
        Ok(rows
            .next()?
            .map(|r| WorkspaceId(r.get::<_, String>(0).unwrap()))
            .unwrap_or_default())
    }

    pub fn set_active_workspace(&self, id: &WorkspaceId) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE workspaces SET active = (id = ?1)",
            params![id.as_str()],
        )?;
        Ok(())
    }

    pub fn count_workspace_projects(&self, id: &WorkspaceId) -> Result<i64> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM projects WHERE COALESCE(workspace_id, ?2) = ?1",
            params![id.as_str(), DEFAULT_WORKSPACE_ID],
            |r| r.get(0),
        )?)
    }

    pub fn count_workspaces(&self) -> Result<i64> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))?)
    }

    // ---- projects ----

    pub fn insert_project(&self, p: &Project) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO projects (id, name, workspace_id, repo_path, sort_order, divider_after, divider_label, divider_before, divider_before_label, created_at, host) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![p.id.as_str(), p.name, p.workspace_id.as_str(), p.repo_path.to_string_lossy(), p.sort_order, p.divider_after as i64, p.divider_label, p.divider_before as i64, p.divider_before_label, now_ms(), p.host],
        )?;
        Ok(())
    }

    /// Sort slot for a newly added project: after everything else.
    pub fn next_project_sort_order(&self) -> Result<i64> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT COALESCE(MAX(sort_order) + 1, 0) FROM projects",
            [],
            |r| r.get(0),
        )?)
    }

    /// Rewrite a remote anchor's path to the toplevel its host resolved
    /// (`host:~/repo/sub` → the repo root), so later mirrors match by path.
    pub fn set_project_repo_path(&self, id: &ProjectId, path: &Path) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE projects SET repo_path = ?2 WHERE id = ?1",
            params![id.as_str(), path.to_string_lossy()],
        )?;
        Ok(())
    }

    /// Persist a project's list position: sort order plus both dividers.
    pub fn set_project_position(&self, p: &Project) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE projects SET sort_order = ?2, divider_after = ?3, divider_label = ?4, divider_before = ?5, divider_before_label = ?6 WHERE id = ?1",
            params![
                p.id.as_str(),
                p.sort_order,
                p.divider_after as i64,
                p.divider_label,
                p.divider_before as i64,
                p.divider_before_label
            ],
        )?;
        Ok(())
    }

    pub fn delete_project(&self, id: &ProjectId) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM projects WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    /// The project row for `path` within one workspace. Repo paths may
    /// repeat across workspaces (a workspace is just a grouping), so path
    /// lookups are always workspace-scoped.
    pub fn project_in_workspace(
        &self,
        path: &Path,
        workspace: &WorkspaceId,
    ) -> Result<Option<ProjectId>> {
        self.project_in_workspace_on(path, workspace, None)
    }

    /// Same lookup for a remote checkout: the path is only a duplicate on
    /// the same host (`/srv/app` here and `/srv/app` on findl are two
    /// different repositories).
    pub fn project_in_workspace_on(
        &self,
        path: &Path,
        workspace: &WorkspaceId,
        host: Option<&str>,
    ) -> Result<Option<ProjectId>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id FROM projects WHERE repo_path = ?1 AND COALESCE(workspace_id, ?3) = ?2 AND host IS ?4",
        )?;
        let mut rows = stmt.query(params![
            path.to_string_lossy(),
            workspace.as_str(),
            DEFAULT_WORKSPACE_ID,
            host
        ])?;
        Ok(rows
            .next()?
            .map(|r| ProjectId(r.get::<_, String>(0).unwrap())))
    }

    // ---- worktrees ----

    pub fn insert_worktree(&self, w: &Worktree) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO worktrees (id, project_id, path, branch, is_main, pinned, sort_order, created_from, for_branch, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                w.id.as_str(),
                w.project_id.as_str(),
                w.path.to_string_lossy(),
                w.branch,
                w.is_main as i64,
                w.pinned as i64,
                w.sort_order,
                w.created_from,
                w.for_branch as i64,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn delete_worktree(&self, id: &WorktreeId) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM worktrees WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    pub fn update_worktree_branch(&self, id: &WorktreeId, branch: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE worktrees SET branch = ?2 WHERE id = ?1",
            params![id.as_str(), branch],
        )?;
        Ok(())
    }

    pub fn set_worktree_created_from(&self, id: &WorktreeId, base: Option<&str>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE worktrees SET created_from = ?2 WHERE id = ?1",
            params![id.as_str(), base],
        )?;
        Ok(())
    }

    pub fn set_worktree_for_branch(&self, id: &WorktreeId, for_branch: bool) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE worktrees SET for_branch = ?2 WHERE id = ?1",
            params![id.as_str(), for_branch as i64],
        )?;
        Ok(())
    }

    pub fn set_worktree_pinned(&self, id: &WorktreeId, pinned: bool) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE worktrees SET pinned = ?2 WHERE id = ?1",
            params![id.as_str(), pinned as i64],
        )?;
        Ok(())
    }

    // ---- agents ----

    pub fn insert_agent(&self, a: &Agent) -> Result<()> {
        self.insert_agent_with_auto_title(a, false)
    }

    /// `auto_title` marks the row as awaiting one agent-driven title
    /// (`nebula rename` from inside the CLI). The flag is store-internal:
    /// clients never see it, they only observe the eventual rename.
    pub fn insert_agent_with_auto_title(&self, a: &Agent, auto_title: bool) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO agents (id, worktree_id, name, status, archived, archived_at, pinned, kind, claude_session_id, sort_order, created_at, status_changed_at, model, effort, auto_title_pending, branch, writer, spawned_by, code_mode_off, card_id, role, dispatch_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                a.id.as_str(),
                a.worktree_id.as_str(),
                a.name,
                a.status.as_str(),
                a.archived as i64,
                a.archived_at,
                a.pinned as i64,
                a.kind.as_str(),
                a.session_id,
                a.sort_order,
                now_ms(),
                a.status_changed_at,
                a.model,
                a.effort,
                auto_title as i64,
                a.branch,
                a.writer as i64,
                a.spawned_by.as_ref().map(AgentId::as_str),
                a.code_mode_off as i64,
                a.card_id.as_ref().map(TodoId::as_str),
                a.role.map(|r| r.as_str()),
                a.dispatch_id.as_ref().map(DispatchId::as_str)
            ],
        )?;
        Ok(())
    }

    /// User rename: always applies, and retires any pending auto-title so a
    /// late agent attempt can't clobber the user's choice.
    pub fn rename_agent(&self, id: &AgentId, name: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE agents SET name = ?2, auto_title_pending = 0 WHERE id = ?1",
            params![id.as_str(), name],
        )?;
        Ok(())
    }

    /// Agent rename: applies only while the auto-title is still pending
    /// (single atomic conditional update — concurrent attempts can't both
    /// win). Returns whether the rename was applied.
    pub fn rename_agent_if_auto_pending(&self, id: &AgentId, name: &str) -> Result<bool> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE agents SET name = ?2, auto_title_pending = 0 WHERE id = ?1 AND auto_title_pending = 1",
            params![id.as_str(), name],
        )?;
        Ok(changed == 1)
    }

    /// Whether the session still awaits its agent-driven auto-title (drives
    /// the hook server's decision to inject the titling instruction).
    pub fn agent_auto_title_pending(&self, id: &AgentId) -> Result<bool> {
        let pending: Option<i64> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT auto_title_pending FROM agents WHERE id = ?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })?;
        Ok(pending == Some(1))
    }

    pub fn set_agent_worktree(&self, id: &AgentId, worktree_id: &WorktreeId) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE agents SET worktree_id = ?2 WHERE id = ?1",
            params![id.as_str(), worktree_id.as_str()],
        )?;
        Ok(())
    }

    pub fn set_terminal_worktree(&self, id: &TerminalId, worktree_id: &WorktreeId) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE terminals SET worktree_id = ?2 WHERE id = ?1",
            params![id.as_str(), worktree_id.as_str()],
        )?;
        Ok(())
    }

    /// Record the branch a session is listed under (see `Agent::branch`).
    pub fn set_agent_branch(&self, id: &AgentId, branch: Option<&str>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE agents SET branch = ?2 WHERE id = ?1",
            params![id.as_str(), branch],
        )?;
        Ok(())
    }

    pub fn set_terminal_branch(&self, id: &TerminalId, branch: Option<&str>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE terminals SET branch = ?2 WHERE id = ?1",
            params![id.as_str(), branch],
        )?;
        Ok(())
    }

    pub fn set_agent_archived(&self, id: &AgentId, archived: bool) -> Result<()> {
        // Stamp the archive time (cleared on unarchive) so the TUI can
        // order the ARCHIVED group newest-first.
        // Archiving drops the writer mark for good, so an unarchive never
        // brings back a second writer beside the one that took the slot.
        let archived_at = if archived { now_ms() } else { 0 };
        self.conn.lock().unwrap().execute(
            "UPDATE agents SET archived = ?2, archived_at = ?3, writer = writer AND NOT ?2 WHERE id = ?1",
            params![id.as_str(), archived as i64, archived_at],
        )?;
        Ok(())
    }

    /// Unarchived, unpinned agents idle (finished, fresh or disconnected)
    /// since before `cutoff_ms`. A 0 stamp means the age is unknown, so
    /// those rows are left alone.
    pub fn stale_idle_agent_ids(&self, cutoff_ms: i64) -> Result<Vec<AgentId>> {
        let conn = self.conn.lock().unwrap();
        let ids = conn
            .prepare(
                "SELECT id FROM agents WHERE archived = 0 AND pinned = 0
                 AND status IN ('finished', 'fresh', 'disconnected')
                 AND status_changed_at > 0 AND status_changed_at < ?1",
            )?
            .query_map(params![cutoff_ms], |r| Ok(AgentId(r.get(0)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    pub fn set_agent_pinned(&self, id: &AgentId, pinned: bool) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE agents SET pinned = ?2 WHERE id = ?1",
            params![id.as_str(), pinned as i64],
        )?;
        Ok(())
    }

    /// Returns the epoch-ms stamp written to `status_changed_at`, so the
    /// caller can broadcast the exact same timestamp it persisted.
    pub fn set_agent_status(&self, id: &AgentId, status: AgentStatus) -> Result<i64> {
        let stamp = now_ms();
        self.conn.lock().unwrap().execute(
            "UPDATE agents SET status = ?2, status_changed_at = ?3 WHERE id = ?1",
            params![id.as_str(), status.as_str(), stamp],
        )?;
        Ok(stamp)
    }

    pub fn set_agent_session_id(&self, id: &AgentId, session_id: Option<&str>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE agents SET claude_session_id = ?2 WHERE id = ?1",
            params![id.as_str(), session_id],
        )?;
        Ok(())
    }

    pub fn delete_agent(&self, id: &AgentId) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM agents WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    /// Boot sweep: agents whose PTYs died with the previous daemon.
    pub fn sweep_disconnected(&self) -> Result<Vec<AgentId>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id FROM agents WHERE status IN ('running', 'needs_feedback')")?;
        let ids: Vec<AgentId> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .map(AgentId)
            .collect();
        drop(stmt);
        conn.execute(
            "UPDATE agents SET status = 'disconnected', status_changed_at = ?1 WHERE status IN ('running', 'needs_feedback')",
            params![now_ms()],
        )?;
        Ok(ids)
    }

    // ---- terminals ----

    pub fn insert_terminal(&self, t: &TerminalTab) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO terminals (id, worktree_id, name, sort_order, created_at, branch) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                t.id.as_str(),
                t.worktree_id.as_str(),
                t.name,
                t.sort_order,
                now_ms(),
                t.branch
            ],
        )?;
        Ok(())
    }

    pub fn rename_terminal(&self, id: &TerminalId, name: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE terminals SET name = ?2 WHERE id = ?1",
            params![id.as_str(), name],
        )?;
        Ok(())
    }

    pub fn delete_terminal(&self, id: &TerminalId) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM terminals WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    /// Unarchived sibling sessions on the agent's worktree that have run
    /// (any status but `fresh` — a fresh session has produced no changes
    /// yet). Drives the hook-side shared-checkout pointer: foreign
    /// modifications in the diff are likely a sibling's, and the agent
    /// should check `nebula agent list` before acting on them. Unknown
    /// agents count as zero.
    pub fn active_sibling_count_for_agent(&self, id: &AgentId) -> Result<usize> {
        let count: i64 = self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM agents b, agents a
             WHERE a.id = ?1 AND b.worktree_id = a.worktree_id
               AND b.id != a.id AND b.archived = 0 AND b.status != 'fresh'",
            params![id.as_str()],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    /// The agent's project's twin on a host — the same-named project of
    /// the same workspace with a host — with the host's mirrored checkouts
    /// and the unarchived sessions on each. Drives the hook-side twin
    /// pointer from what the relay already mirrored: no git, no ssh. None
    /// for unknown agents, remote agents (the host daemon has no view of
    /// the laptop) and projects without a twin.
    pub fn twin_for_agent(&self, id: &AgentId) -> Result<Option<TwinPointer>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT t.host, t.name, w.branch,
                    (SELECT COUNT(*) FROM agents b WHERE b.worktree_id = w.id AND b.archived = 0)
             FROM agents a
             JOIN worktrees aw ON aw.id = a.worktree_id
             JOIN projects p ON p.id = aw.project_id AND p.host IS NULL
             JOIN projects t ON t.name = p.name AND t.host IS NOT NULL
                  AND COALESCE(t.workspace_id, 'default') = COALESCE(p.workspace_id, 'default')
             LEFT JOIN worktrees w ON w.project_id = t.id
             WHERE a.id = ?1
             ORDER BY t.host, w.is_main DESC, w.branch",
        )?;
        let rows = stmt.query_map(params![id.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut twin: Option<TwinPointer> = None;
        for row in rows {
            let (host, name, branch, sessions) = row?;
            let t = twin.get_or_insert_with(|| TwinPointer {
                host: host.clone(),
                name,
                checkouts: Vec::new(),
            });
            // One host per pointer; a second twin host is out of scope.
            if t.host != host {
                continue;
            }
            if let Some(branch) = branch {
                t.checkouts.push((branch, sessions.max(0) as usize));
            }
        }
        Ok(twin)
    }

    // ---- pull-request read marks ----

    /// Remember that this pull request's conversation has been read up to
    /// `marker`. Idempotent, and an empty marker is a real answer: it says
    /// the PR was opened while nobody had posted on it yet.
    pub fn mark_pr_seen(&self, url: &str, marker: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO pr_seen (url, marker, seen_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(url) DO UPDATE SET marker = excluded.marker, seen_at = excluded.seen_at",
            params![url, marker, now_ms()],
        )?;
        Ok(())
    }

    pub fn load_pr_seen(&self) -> Result<Vec<PrSeen>> {
        let conn = self.conn.lock().unwrap();
        let seen = conn
            .prepare("SELECT url, marker FROM pr_seen")?
            .query_map([], |r| {
                Ok(PrSeen {
                    url: r.get(0)?,
                    marker: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(seen)
    }

    // ---- point lookups ----

    pub fn get_project(&self, id: &ProjectId) -> Result<Option<Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, repo_path, sort_order, divider_after, divider_label, divider_before, divider_before_label, COALESCE(workspace_id, 'default'), host FROM projects WHERE id = ?1")?;
        let mut rows = stmt.query(params![id.as_str()])?;
        Ok(rows.next()?.map(|r| Project {
            id: ProjectId(r.get::<_, String>(0).unwrap()),
            name: r.get(1).unwrap(),
            repo_path: PathBuf::from(r.get::<_, String>(2).unwrap()),
            sort_order: r.get(3).unwrap(),
            divider_after: r.get::<_, i64>(4).unwrap() != 0,
            divider_label: r.get(5).unwrap(),
            divider_before: r.get::<_, i64>(6).unwrap() != 0,
            divider_before_label: r.get(7).unwrap(),
            workspace_id: WorkspaceId(r.get::<_, String>(8).unwrap()),
            host: r.get(9).unwrap(),
        }))
    }

    pub fn get_worktree(&self, id: &WorktreeId) -> Result<Option<Worktree>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, path, branch, is_main, pinned, sort_order, created_from, for_branch FROM worktrees WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.as_str()])?;
        Ok(rows.next()?.map(|r| Worktree {
            id: WorktreeId(r.get::<_, String>(0).unwrap()),
            project_id: ProjectId(r.get::<_, String>(1).unwrap()),
            path: PathBuf::from(r.get::<_, String>(2).unwrap()),
            branch: r.get(3).unwrap(),
            is_main: r.get::<_, i64>(4).unwrap() != 0,
            created_from: r.get(7).unwrap(),
            pinned: r.get::<_, i64>(5).unwrap() != 0,
            for_branch: r.get::<_, i64>(8).unwrap() != 0,
            sort_order: r.get(6).unwrap(),
        }))
    }

    /// Return the original checkout path for a project, if it is registered.
    pub fn get_primary_worktree_path(&self, project_id: &ProjectId) -> Result<Option<PathBuf>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT path FROM worktrees WHERE project_id = ?1 AND is_main = 1 LIMIT 1")?;
        let mut rows = stmt.query(params![project_id.as_str()])?;
        Ok(rows
            .next()?
            .map(|row| PathBuf::from(row.get::<_, String>(0).unwrap())))
    }

    pub fn get_agent(&self, id: &AgentId) -> Result<Option<Agent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, worktree_id, name, status, archived, pinned, kind, claude_session_id, sort_order, status_changed_at, model, effort, archived_at, branch, writer, spawned_by, code_mode_off, card_id, role, dispatch_id FROM agents WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.as_str()])?;
        Ok(rows.next()?.map(|r| Agent {
            id: AgentId(r.get::<_, String>(0).unwrap()),
            worktree_id: WorktreeId(r.get::<_, String>(1).unwrap()),
            name: r.get(2).unwrap(),
            status: AgentStatus::parse(&r.get::<_, String>(3).unwrap())
                .unwrap_or(AgentStatus::Fresh),
            archived: r.get::<_, i64>(4).unwrap() != 0,
            pinned: r.get::<_, i64>(5).unwrap() != 0,
            kind: AgentKind::parse(&r.get::<_, String>(6).unwrap()).unwrap_or_default(),
            session_id: r.get(7).unwrap(),
            sort_order: r.get(8).unwrap(),
            status_changed_at: r.get(9).unwrap(),
            model: r.get(10).unwrap(),
            effort: r.get(11).unwrap(),
            archived_at: r.get(12).unwrap(),
            alive: false,
            branch: r.get(13).unwrap(),
            writer: r.get::<_, i64>(14).unwrap() != 0,
            spawned_by: r.get::<_, Option<String>>(15).unwrap().map(AgentId),
            card_id: r.get::<_, Option<String>>(17).unwrap().map(TodoId),
            role: r
                .get::<_, Option<String>>(18)
                .unwrap()
                .and_then(|r| Role::parse(&r)),
            dispatch_id: r.get::<_, Option<String>>(19).unwrap().map(DispatchId),
            code_mode_off: r.get::<_, i64>(16).unwrap() != 0,
        }))
    }

    pub fn get_terminal(&self, id: &TerminalId) -> Result<Option<TerminalTab>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, worktree_id, name, sort_order, status, status_changed_at, branch FROM terminals WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.as_str()])?;
        Ok(rows.next()?.map(|r| TerminalTab {
            id: TerminalId(r.get::<_, String>(0).unwrap()),
            worktree_id: WorktreeId(r.get::<_, String>(1).unwrap()),
            name: r.get(2).unwrap(),
            sort_order: r.get(3).unwrap(),
            alive: false,
            busy: false,
            status: r
                .get::<_, Option<String>>(4)
                .unwrap()
                .and_then(|s| AgentStatus::parse(&s)),
            status_changed_at: r.get(5).unwrap(),
            branch: r.get(6).unwrap(),
        }))
    }

    /// Set (or clear, with None) the hook-driven status of a shell tab.
    /// Returns the epoch-ms stamp written, so the caller can broadcast the
    /// exact same timestamp it persisted.
    pub fn set_terminal_status(&self, id: &TerminalId, status: Option<AgentStatus>) -> Result<i64> {
        let stamp = now_ms();
        self.conn.lock().unwrap().execute(
            "UPDATE terminals SET status = ?2, status_changed_at = ?3 WHERE id = ?1",
            params![id.as_str(), status.map(|s| s.as_str()), stamp],
        )?;
        Ok(stamp)
    }

    /// Boot sweep: every persisted terminal status describes a CLI whose
    /// PTY died with the previous daemon — clear them all.
    pub fn sweep_terminal_statuses(&self) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE terminals SET status = NULL WHERE status IS NOT NULL",
            [],
        )?;
        Ok(())
    }

    pub fn count_terminals(&self, worktree_id: &WorktreeId) -> Result<i64> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM terminals WHERE worktree_id = ?1",
            params![worktree_id.as_str()],
            |r| r.get(0),
        )?)
    }

    // ---- whole tree ----

    pub fn load_tree(&self) -> Result<TreeRows> {
        let conn = self.conn.lock().unwrap();

        let projects = conn
            .prepare("SELECT id, name, repo_path, sort_order, divider_after, divider_label, divider_before, divider_before_label, COALESCE(workspace_id, 'default'), host FROM projects ORDER BY sort_order, created_at")?
            .query_map([], |r| {
                Ok(Project {
                    id: ProjectId(r.get(0)?),
                    name: r.get(1)?,
                    repo_path: PathBuf::from(r.get::<_, String>(2)?),
                    sort_order: r.get(3)?,
                    divider_after: r.get::<_, i64>(4)? != 0,
                    divider_label: r.get(5)?,
                    divider_before: r.get::<_, i64>(6)? != 0,
                    divider_before_label: r.get(7)?,
                    workspace_id: WorkspaceId(r.get(8)?),
                    host: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let worktrees = conn
            .prepare("SELECT id, project_id, path, branch, is_main, pinned, sort_order, created_from, for_branch FROM worktrees ORDER BY is_main DESC, sort_order, created_at")?
            .query_map([], |r| {
                Ok(Worktree {
                    id: WorktreeId(r.get(0)?),
                    project_id: ProjectId(r.get(1)?),
                    path: PathBuf::from(r.get::<_, String>(2)?),
                    branch: r.get(3)?,
                    is_main: r.get::<_, i64>(4)? != 0,
                    created_from: r.get(7)?,
                    pinned: r.get::<_, i64>(5)? != 0,
                    for_branch: r.get::<_, i64>(8)? != 0,
                    sort_order: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let agents = conn
            .prepare("SELECT id, worktree_id, name, status, archived, pinned, kind, claude_session_id, sort_order, status_changed_at, model, effort, archived_at, branch, writer, spawned_by, code_mode_off, card_id, role, dispatch_id FROM agents ORDER BY sort_order, created_at")?
            .query_map([], |r| {
                Ok(Agent {
                    id: AgentId(r.get(0)?),
                    worktree_id: WorktreeId(r.get(1)?),
                    name: r.get(2)?,
                    status: AgentStatus::parse(&r.get::<_, String>(3)?).unwrap_or(AgentStatus::Fresh),
                    archived: r.get::<_, i64>(4)? != 0,
                    pinned: r.get::<_, i64>(5)? != 0,
                    kind: AgentKind::parse(&r.get::<_, String>(6)?).unwrap_or_default(),
                    session_id: r.get(7)?,
                    sort_order: r.get(8)?,
                    status_changed_at: r.get(9)?,
                    model: r.get(10)?,
                    effort: r.get(11)?,
                    archived_at: r.get(12)?,
                    alive: false,
                    branch: r.get(13)?,
                    writer: r.get::<_, i64>(14)? != 0,
                    spawned_by: r.get::<_, Option<String>>(15)?.map(AgentId),
                    card_id: r.get::<_, Option<String>>(17)?.map(TodoId),
                    role: r.get::<_, Option<String>>(18)?.and_then(|r| Role::parse(&r)),
                    dispatch_id: r.get::<_, Option<String>>(19)?.map(DispatchId),
                    code_mode_off: r.get::<_, i64>(16)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let terminals = conn
            .prepare("SELECT id, worktree_id, name, sort_order, status, status_changed_at, branch FROM terminals ORDER BY sort_order, created_at")?
            .query_map([], |r| {
                Ok(TerminalTab {
                    id: TerminalId(r.get(0)?),
                    worktree_id: WorktreeId(r.get(1)?),
                    name: r.get(2)?,
                    sort_order: r.get(3)?,
                    alive: false,
                    busy: false,
                    status: r.get::<_, Option<String>>(4)?.and_then(|s| AgentStatus::parse(&s)),
                    status_changed_at: r.get(5)?,
                    branch: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok((projects, worktrees, agents, terminals))
    }

    // ---- ui state ----

    pub fn save_ui_state(&self, json: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO ui_state (id, json) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![json],
        )?;
        Ok(())
    }

    pub fn load_ui_state(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT json FROM ui_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0).unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_tree() {
        let store = Store::open_in_memory().unwrap();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId::generate(),
            name: "demo".into(),
            repo_path: "/tmp/demo".into(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        store.insert_project(&project).unwrap();
        let worktree = Worktree {
            id: WorktreeId::generate(),
            project_id: project.id.clone(),
            path: "/tmp/demo-feature".into(),
            branch: "feature".into(),
            is_main: false,
            created_from: Some("main".into()),
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        store.insert_worktree(&worktree).unwrap();
        let agent = Agent {
            id: AgentId::generate(),
            worktree_id: worktree.id.clone(),
            name: "agent-1".into(),
            status: AgentStatus::Running,
            archived: false,
            archived_at: 0,
            pinned: false,
            kind: AgentKind::Claude,
            model: Some("opus".into()),
            effort: Some("high".into()),
            session_id: Some("sess-123".into()),
            sort_order: 0,
            status_changed_at: 0,
            alive: false,
            branch: None,
            spawned_by: Some(AgentId("lead".into())),
            writer: true,
            card_id: None,
            role: None,
            dispatch_id: None,
            code_mode_off: false,
        };
        store.insert_agent(&agent).unwrap();
        let codex_agent = Agent {
            id: AgentId::generate(),
            worktree_id: worktree.id.clone(),
            name: "agent-2".into(),
            status: AgentStatus::Fresh,
            archived: false,
            archived_at: 0,
            pinned: false,
            kind: AgentKind::Codex,
            model: None,
            effort: None,
            session_id: None,
            sort_order: 1,
            status_changed_at: 0,
            alive: false,
            branch: None,
            spawned_by: None,
            writer: false,
            card_id: None,
            role: None,
            dispatch_id: None,
            code_mode_off: false,
        };
        store.insert_agent(&codex_agent).unwrap();
        let cursor_agent = Agent {
            id: AgentId::generate(),
            worktree_id: worktree.id.clone(),
            name: "agent-3".into(),
            status: AgentStatus::Fresh,
            archived: false,
            archived_at: 0,
            pinned: false,
            kind: AgentKind::Cursor,
            model: None,
            effort: None,
            session_id: None,
            sort_order: 2,
            status_changed_at: 0,
            alive: false,
            branch: None,
            spawned_by: None,
            writer: false,
            card_id: None,
            role: None,
            dispatch_id: None,
            code_mode_off: false,
        };
        store.insert_agent(&cursor_agent).unwrap();

        let (projects, worktrees, agents, _terms) = store.load_tree().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].created_from.as_deref(), Some("main"));
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[0].status, AgentStatus::Running);
        assert_eq!(agents[0].kind, AgentKind::Claude);
        assert_eq!(agents[0].session_id.as_deref(), Some("sess-123"));
        assert_eq!(agents[0].model.as_deref(), Some("opus"));
        assert_eq!(agents[0].effort.as_deref(), Some("high"));
        assert!(agents[0].writer);
        assert!(!agents[1].writer);
        assert_eq!(agents[0].spawned_by, Some(AgentId("lead".into())));
        assert_eq!(agents[1].spawned_by, None);
        assert_eq!(agents[1].kind, AgentKind::Codex);
        assert_eq!(agents[1].model, None);
        assert_eq!(agents[2].kind, AgentKind::Cursor);
    }

    /// Read marks are keyed by PR URL and outlive the worktree they were
    /// noticed on, so they live in their own table with no foreign key: no
    /// row here is ever cascaded away by a checkout being deleted.
    #[test]
    fn pr_seen_marks_roundtrip_and_overwrite() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.load_pr_seen().unwrap().is_empty());

        let url = "https://github.com/o/r/pull/7";
        store.mark_pr_seen(url, "2024-04-25T19:55:42Z").unwrap();
        let seen = store.load_pr_seen().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].url, url);
        assert_eq!(seen[0].marker, "2024-04-25T19:55:42Z");

        // Opening it again moves the mark rather than adding a second row.
        store.mark_pr_seen(url, "2024-04-27T09:00:00Z").unwrap();
        let seen = store.load_pr_seen().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].marker, "2024-04-27T09:00:00Z");

        // An empty marker is a real answer: opened, nobody had posted yet.
        store.mark_pr_seen(url, "").unwrap();
        assert_eq!(store.load_pr_seen().unwrap()[0].marker, "");
    }

    /// Real upgrade path: a v12 database (pre-workspaces) gains the
    /// 'default' workspace, marked open, with every existing project in it.
    /// Real upgrade path: sessions from before branch tracking are pinned
    /// to the branch their checkout held at upgrade time, so the first
    /// switch afterwards already leaves them where they were.
    #[test]
    fn migration_25_pins_existing_sessions_to_their_checkout_branch() {
        let path =
            std::env::temp_dir().join(format!("nebula-mig25-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            for (i, migration) in MIGRATIONS.iter().take(24).enumerate() {
                conn.execute_batch(&format!(
                    "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                    i + 1
                ))
                .unwrap();
            }
            conn.execute_batch(
                "INSERT INTO projects (id, workspace_id, name, repo_path, sort_order, created_at) VALUES ('p1', 'default', 'p', '/tmp/p', 0, 0);
                 INSERT INTO worktrees (id, project_id, path, branch, is_main, created_at) VALUES ('w1', 'p1', '/tmp/p', 'main', 1, 0);
                 INSERT INTO agents (id, worktree_id, name, created_at) VALUES ('a1', 'w1', 'agent', 0);
                 INSERT INTO terminals (id, worktree_id, name, created_at) VALUES ('t1', 'w1', 'shell', 0);",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let (_, _, agents, terminals) = store.load_tree().unwrap();
        assert_eq!(agents[0].branch.as_deref(), Some("main"));
        assert_eq!(agents[0].spawned_by, None);
        assert_eq!(terminals[0].branch.as_deref(), Some("main"));
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    /// Real upgrade path: sessions from before `agent new --no-code-mode`
    /// keep Code Mode as the user's settings have it, and a flagged session
    /// reads back flagged.
    #[test]
    fn migration_28_leaves_existing_sessions_in_code_mode_and_keeps_the_flag() {
        let path =
            std::env::temp_dir().join(format!("nebula-mig28-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            for (i, migration) in MIGRATIONS.iter().take(27).enumerate() {
                conn.execute_batch(&format!(
                    "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                    i + 1
                ))
                .unwrap();
            }
            conn.execute_batch(
                "INSERT INTO projects (id, workspace_id, name, repo_path, sort_order, created_at) VALUES ('p1', 'default', 'p', '/tmp/p', 0, 0);
                 INSERT INTO worktrees (id, project_id, path, branch, is_main, created_at) VALUES ('w1', 'p1', '/tmp/p', 'main', 1, 0);
                 INSERT INTO agents (id, worktree_id, name, created_at) VALUES ('a1', 'w1', 'legacy', 0);",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let mut flagged = store.get_agent(&AgentId("a1".into())).unwrap().unwrap();
        assert!(!flagged.code_mode_off);
        flagged.id = AgentId("a2".into());
        flagged.name = "browser".into();
        flagged.code_mode_off = true;
        store.insert_agent(&flagged).unwrap();
        let (_, _, agents, _) = store.load_tree().unwrap();
        let by_name = |name: &str| {
            agents
                .iter()
                .find(|a| a.name == name)
                .unwrap()
                .code_mode_off
        };
        assert!(!by_name("legacy"));
        assert!(by_name("browser"));
        assert!(
            store
                .get_agent(&AgentId("a2".into()))
                .unwrap()
                .unwrap()
                .code_mode_off
        );
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Migration 29 leaves the board schema: the board tables and columns,
    /// no legacy owner columns or links table, and every write to a board
    /// table bumps board_meta.version.
    #[test]
    fn migrate_v28_opens_as_v29() {
        let path =
            std::env::temp_dir().join(format!("nebula-mig29-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            for (i, migration) in MIGRATIONS.iter().take(28).enumerate() {
                conn.execute_batch(&format!(
                    "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                    i + 1
                ))
                .unwrap();
            }
            conn.execute_batch(
                "INSERT INTO projects (id, workspace_id, name, repo_path, sort_order, created_at) VALUES ('p1', 'default', 'p', '/tmp/p', 0, 0);
                 INSERT INTO worktrees (id, project_id, path, branch, is_main, created_at) VALUES ('w1', 'p1', '/tmp/p', 'main', 1, 0);
                 INSERT INTO agents (id, worktree_id, name, created_at) VALUES ('a1', 'w1', 'legacy', 0);
                 INSERT INTO notes (id, project_id, text, created_at) VALUES ('n1', 'p1', 'Lift utilities done', 0);
                 INSERT INTO todos (id, project_id, text, created_at) VALUES ('t1', 'p1', 'ship it', 0);
                 INSERT INTO todos (id, worktree_id, text, created_at) VALUES ('t2', 'w1', 'review it', 0);
                 INSERT INTO notes (id, todo_id, text, created_at) VALUES ('n2', 't1', 'child', 0);
                 INSERT INTO links (id, worktree_id, url, created_at) VALUES ('l1', 'w1', 'https://vela.possu.dev', 0);",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert!(store.get_agent(&AgentId("a1".into())).unwrap().is_some());

        let conn = store.conn.lock().unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 29);
        let expected: &[(&str, &[&str])] = &[
            ("agents", &["card_id", "role", "dispatch_id"]),
            (
                "todos",
                &[
                    "number",
                    "title",
                    "stage",
                    "project",
                    "checkout",
                    "created_by",
                    "proposed",
                    "stage_changed_at",
                    "archived_at",
                ],
            ),
            ("notes", &["author", "todo_id"]),
            ("card_blocks", &["card_id", "by_id"]),
            (
                "artifacts",
                &[
                    "id",
                    "kind",
                    "title",
                    "storage_host",
                    "entry",
                    "url",
                    "body",
                    "card_id",
                    "project",
                    "session",
                    "checkout",
                    "meta",
                    "created_at",
                    "removed_at",
                ],
            ),
            (
                "board_sessions",
                &[
                    "agent_id",
                    "host",
                    "card_id",
                    "role",
                    "kind",
                    "name",
                    "parent",
                    "status",
                    "status_changed_at",
                    "writer",
                    "alive",
                    "archived",
                    "branch",
                    "dispatch_id",
                    "reported_at",
                ],
            ),
            (
                "board_hosts",
                &[
                    "host",
                    "writers",
                    "writer_slots",
                    "checkouts",
                    "reported_at",
                ],
            ),
            (
                "dispatches",
                &[
                    "id",
                    "card_id",
                    "host",
                    "role",
                    "kind",
                    "worktree",
                    "state",
                    "agent_id",
                    "error",
                    "created_at",
                ],
            ),
            ("board_meta", &["board_id", "version"]),
        ];
        for (table, wanted) in expected {
            let have = columns(&conn, table);
            for column in *wanted {
                assert!(
                    have.iter().any(|c| c == column),
                    "{table}.{column}: {have:?}"
                );
            }
        }
        for (table, gone) in [
            ("todos", ["project_id", "worktree_id"]),
            ("notes", ["project_id", "worktree_id"]),
        ] {
            let have = columns(&conn, table);
            assert!(
                gone.iter().all(|c| !have.iter().any(|h| h == c)),
                "{table}: {have:?}"
            );
        }
        assert!(
            columns(&conn, "links").is_empty(),
            "the links table is gone"
        );

        let board_version = || -> i64 {
            conn.query_row("SELECT version FROM board_meta", [], |r| r.get(0))
                .unwrap()
        };
        let board_id: String = conn
            .query_row("SELECT board_id FROM board_meta", [], |r| r.get(0))
            .unwrap();
        assert!(
            ulid::Ulid::from_string(&board_id).is_ok(),
            "board_id {board_id} is not a ULID"
        );
        let writes: &[(&str, [&str; 3])] = &[
            ("todos", [
                "INSERT INTO todos (id, number, title, stage, created_by, created_at) VALUES ('t9', 9, 'x', 'backlog', '{}', 0)",
                "UPDATE todos SET stage = 'next' WHERE id = 't9'",
                "DELETE FROM todos WHERE id = 't9'",
            ]),
            ("notes", [
                "INSERT INTO notes (id, todo_id, text, author, created_at) VALUES ('n9', 't1', 'x', '{}', 0)",
                "UPDATE notes SET author = '{}' WHERE id = 'n9'",
                "DELETE FROM notes WHERE id = 'n9'",
            ]),
            ("card_blocks", [
                "INSERT INTO card_blocks (card_id, by_id) VALUES ('t1', 't2')",
                "UPDATE card_blocks SET card_id = 't2', by_id = 't1'",
                "DELETE FROM card_blocks",
            ]),
            ("artifacts", [
                "INSERT INTO artifacts (id, kind, title, url, created_at) VALUES ('a9', 'link', 'x', 'https://x', 0)",
                "UPDATE artifacts SET title = 'y' WHERE id = 'a9'",
                "DELETE FROM artifacts WHERE id = 'a9'",
            ]),
            ("board_sessions", [
                "INSERT INTO board_sessions (agent_id, host, card_id, kind, name, status, reported_at) VALUES ('s9', 'h', 't1', 'codex', 'w', 'running', 0)",
                "UPDATE board_sessions SET status = 'finished' WHERE agent_id = 's9'",
                "DELETE FROM board_sessions WHERE agent_id = 's9'",
            ]),
            ("board_hosts", [
                "INSERT INTO board_hosts (host, reported_at) VALUES ('h', 0)",
                "UPDATE board_hosts SET writers = 1 WHERE host = 'h'",
                "DELETE FROM board_hosts WHERE host = 'h'",
            ]),
            ("dispatches", [
                "INSERT INTO dispatches (id, card_id, host, role, kind, worktree, state, created_at) VALUES ('d9', 't1', 'h', 'worker', 'codex', 'new', 'pending', 0)",
                "UPDATE dispatches SET state = 'started' WHERE id = 'd9'",
                "DELETE FROM dispatches WHERE id = 'd9'",
            ]),
        ];
        for (table, statements) in writes {
            for sql in statements {
                let before = board_version();
                conn.execute(sql, []).unwrap();
                assert_eq!(board_version(), before + 1, "{table}: {sql}");
            }
        }
        for card in [
            "INSERT INTO todos (id, created_at, number, title, stage, created_by) VALUES ('c1', 0, 11, 'home card', 'backlog', '{}')",
            "INSERT INTO todos (id, created_at, number, title, stage, project, created_by) VALUES ('c2', 0, 12, 'remote card', 'next', 'vela', '{}')",
        ] {
            conn.execute(card, [])
                .unwrap_or_else(|e| panic!("{card}: {e}"));
        }
        for refused in [
            "INSERT INTO todos (id, number, title, created_by, created_at) VALUES ('t8', 8, 'x', '{}', 0)",
            "INSERT INTO todos (id, number, title, stage, created_by, created_at) VALUES ('t8', 11, 'x', 'backlog', '{}', 0)",
            "INSERT INTO notes (id, text, author, created_at) VALUES ('n8', 'x', '{}', 0)",
            "INSERT INTO artifacts (id, kind, title, url, body, created_at) VALUES ('a8', 'link', 'x', 'https://x', 'b', 0)",
            "INSERT INTO artifacts (id, kind, title, storage_host, created_at) VALUES ('a8', 'recap', 'x', 'h', 0)",
            "INSERT INTO artifacts (id, kind, title, created_at) VALUES ('a8', 'recap', 'x', 0)",
            "INSERT INTO card_blocks (card_id, by_id) VALUES ('t1', 't1')",
            "UPDATE todos SET stage = 'later' WHERE id = 't1'",
        ] {
            assert!(conn.execute(refused, []).is_err(), "{refused}");
        }
        drop(conn);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[test]
    fn migration_13_moves_existing_projects_into_default_workspace() {
        let path =
            std::env::temp_dir().join(format!("nebula-mig13-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            for (i, migration) in MIGRATIONS.iter().take(12).enumerate() {
                conn.execute_batch(&format!(
                    "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                    i + 1
                ))
                .unwrap();
            }
            conn.execute_batch(
                "INSERT INTO projects (id, name, repo_path, sort_order, created_at) VALUES ('p1', 'p', '/tmp/p', 0, 0);",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let workspaces = store.load_workspaces().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].id.as_str(), DEFAULT_WORKSPACE_ID);
        assert_eq!(workspaces[0].name, "default");
        assert_eq!(
            store.active_workspace_id().unwrap().as_str(),
            DEFAULT_WORKSPACE_ID
        );
        let (projects, _, _, _) = store.load_tree().unwrap();
        assert_eq!(projects[0].workspace_id.as_str(), DEFAULT_WORKSPACE_ID);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    /// Real upgrade path: a v13 database (global UNIQUE on repo_path) is
    /// rebuilt so the same repo can live in several workspaces. The rebuild
    /// drops the old projects table — child rows must survive it.
    #[test]
    fn migration_14_scopes_repo_uniqueness_to_workspace() {
        let path =
            std::env::temp_dir().join(format!("nebula-mig14-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            for (i, migration) in MIGRATIONS.iter().take(13).enumerate() {
                conn.execute_batch(&format!(
                    "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                    i + 1
                ))
                .unwrap();
            }
            conn.execute_batch(
                "INSERT INTO projects (id, name, repo_path, sort_order, created_at, workspace_id) VALUES ('p1', 'p', '/tmp/p', 0, 0, 'default');
                 INSERT INTO worktrees (id, project_id, path, branch, is_main, sort_order, created_at) VALUES ('w1', 'p1', '/tmp/p', 'main', 1, 0, 0);
                 INSERT INTO agents (id, worktree_id, name, created_at) VALUES ('a1', 'w1', 'agent', 0);",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let (projects, worktrees, agents, _) = store.load_tree().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(worktrees.len(), 1, "worktrees must survive the rebuild");
        assert_eq!(agents.len(), 1, "agents must survive the rebuild");

        // The same repo is now welcome in a second workspace…
        store
            .insert_workspace(&Workspace {
                id: WorkspaceId("w2".into()),
                name: "second".into(),
            })
            .unwrap();
        let dup = |id: &str, workspace: &str| Project {
            id: ProjectId(id.into()),
            name: "p".into(),
            workspace_id: WorkspaceId(workspace.into()),
            repo_path: PathBuf::from("/tmp/p"),
            sort_order: 1,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        store.insert_project(&dup("p2", "w2")).unwrap();
        // …but still refused twice in the same one.
        assert!(store.insert_project(&dup("p3", "default")).is_err());

        // Path lookups resolve per workspace.
        assert_eq!(
            store
                .project_in_workspace(Path::new("/tmp/p"), &WorkspaceId("w2".into()))
                .unwrap(),
            Some(ProjectId("p2".into()))
        );
        assert_eq!(
            store
                .project_in_workspace(Path::new("/tmp/p"), &WorkspaceId("empty".into()))
                .unwrap(),
            None
        );
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[test]
    fn workspace_crud_and_active_flag() {
        let store = Store::open_in_memory().unwrap();
        // The migration seeds the open 'default' workspace.
        let workspaces = store.load_workspaces().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].name, "default");
        assert_eq!(
            store.active_workspace_id().unwrap().as_str(),
            DEFAULT_WORKSPACE_ID
        );

        let client = Workspace {
            id: WorkspaceId("ws-client".into()),
            name: "client".into(),
        };
        store.insert_workspace(&client).unwrap();
        assert_eq!(store.count_workspaces().unwrap(), 2);
        assert_eq!(
            store.workspace_by_name("client").unwrap(),
            Some(client.id.clone())
        );
        // UNIQUE name: a duplicate insert errors.
        assert!(store
            .insert_workspace(&Workspace {
                id: WorkspaceId("ws-dup".into()),
                name: "client".into(),
            })
            .is_err());

        // Exactly one open workspace at a time.
        store.set_active_workspace(&client.id).unwrap();
        assert_eq!(store.active_workspace_id().unwrap(), client.id);
        store
            .set_active_workspace(&WorkspaceId(DEFAULT_WORKSPACE_ID.into()))
            .unwrap();
        assert_eq!(
            store.active_workspace_id().unwrap().as_str(),
            DEFAULT_WORKSPACE_ID
        );

        store.rename_workspace(&client.id, "acme").unwrap();
        assert_eq!(
            store.get_workspace(&client.id).unwrap().unwrap().name,
            "acme"
        );
        assert_eq!(store.workspace_by_name("client").unwrap(), None);

        // Projects count per workspace; inserts land where they say.
        let project = Project {
            workspace_id: client.id.clone(),
            id: ProjectId::generate(),
            name: "demo".into(),
            repo_path: "/tmp/demo".into(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        store.insert_project(&project).unwrap();
        assert_eq!(store.count_workspace_projects(&client.id).unwrap(), 1);
        assert_eq!(
            store
                .count_workspace_projects(&WorkspaceId(DEFAULT_WORKSPACE_ID.into()))
                .unwrap(),
            0
        );
        let (projects, _, _, _) = store.load_tree().unwrap();
        assert_eq!(projects[0].workspace_id, client.id);

        // The FK keeps a populated workspace undeletable; empty it first.
        assert!(store.delete_workspace(&client.id).is_err());
        store.delete_project(&project.id).unwrap();
        store.delete_workspace(&client.id).unwrap();
        assert_eq!(store.count_workspaces().unwrap(), 1);
    }

    #[test]
    fn auto_title_pending_lifecycle() {
        let store = Store::open_in_memory().unwrap();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId::generate(),
            name: "p".into(),
            repo_path: "/tmp/p".into(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        store.insert_project(&project).unwrap();
        let wt = Worktree {
            id: WorktreeId::generate(),
            project_id: project.id.clone(),
            path: "/tmp/p".into(),
            branch: "main".into(),
            is_main: true,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        store.insert_worktree(&wt).unwrap();
        let agent = |id: &str| Agent {
            id: AgentId(id.into()),
            worktree_id: wt.id.clone(),
            name: "agent-1".into(),
            status: AgentStatus::Fresh,
            archived: false,
            archived_at: 0,
            pinned: false,
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            session_id: None,
            sort_order: 0,
            status_changed_at: 0,
            alive: false,
            branch: None,
            spawned_by: None,
            writer: false,
            card_id: None,
            role: None,
            dispatch_id: None,
            code_mode_off: false,
        };

        // Default-named session: pending until the agent titles it, and the
        // conditional rename fires exactly once.
        store
            .insert_agent_with_auto_title(&agent("a1"), true)
            .unwrap();
        let id = AgentId("a1".into());
        assert!(store.agent_auto_title_pending(&id).unwrap());
        assert!(store
            .rename_agent_if_auto_pending(&id, "Fix Login Redirect")
            .unwrap());
        assert!(!store.agent_auto_title_pending(&id).unwrap());
        assert!(!store
            .rename_agent_if_auto_pending(&id, "Second Attempt")
            .unwrap());
        assert_eq!(
            store.get_agent(&id).unwrap().unwrap().name,
            "Fix Login Redirect"
        );

        // A user rename retires the pending flag so a late agent attempt
        // can't clobber the user's choice.
        store
            .insert_agent_with_auto_title(&agent("a2"), true)
            .unwrap();
        let id = AgentId("a2".into());
        store.rename_agent(&id, "my session").unwrap();
        assert!(!store.agent_auto_title_pending(&id).unwrap());
        assert!(!store.rename_agent_if_auto_pending(&id, "Nope").unwrap());
        assert_eq!(store.get_agent(&id).unwrap().unwrap().name, "my session");

        // Custom-named sessions (plain insert) never pend; unknown ids
        // report not-pending instead of erroring.
        store.insert_agent(&agent("a3")).unwrap();
        assert!(!store
            .agent_auto_title_pending(&AgentId("a3".into()))
            .unwrap());
        assert!(!store
            .agent_auto_title_pending(&AgentId("ghost".into()))
            .unwrap());
    }

    #[test]
    fn cascade_delete_project_removes_children() {
        let store = Store::open_in_memory().unwrap();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId::generate(),
            name: "demo".into(),
            repo_path: "/tmp/demo".into(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        store.insert_project(&project).unwrap();
        let worktree = Worktree {
            id: WorktreeId::generate(),
            project_id: project.id.clone(),
            path: "/tmp/demo".into(),
            branch: "main".into(),
            is_main: true,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        store.insert_worktree(&worktree).unwrap();
        store
            .insert_terminal(&TerminalTab {
                id: TerminalId::generate(),
                worktree_id: worktree.id.clone(),
                name: "shell".into(),
                sort_order: 0,
                alive: false,
                branch: None,
                busy: false,
                status: None,
                status_changed_at: 0,
            })
            .unwrap();

        store.delete_project(&project.id).unwrap();
        let (projects, worktrees, _agents, terminals) = store.load_tree().unwrap();
        assert!(projects.is_empty());
        assert!(worktrees.is_empty());
        assert!(terminals.is_empty());
    }

    /// A shell tab's hook-driven status (a `claude` run by hand inside it)
    /// persists, clears, and is wiped wholesale by the boot sweep.
    #[test]
    fn terminal_status_persists_clears_and_boot_sweeps() {
        let store = Store::open_in_memory().unwrap();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId::generate(),
            name: "p".into(),
            repo_path: "/tmp/p".into(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        store.insert_project(&project).unwrap();
        let worktree = Worktree {
            id: WorktreeId::generate(),
            project_id: project.id.clone(),
            path: "/tmp/p".into(),
            branch: "main".into(),
            is_main: true,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        store.insert_worktree(&worktree).unwrap();
        let tid = TerminalId::generate();
        store
            .insert_terminal(&TerminalTab {
                id: tid.clone(),
                worktree_id: worktree.id.clone(),
                name: "shell".into(),
                sort_order: 0,
                alive: false,
                branch: None,
                busy: false,
                status: None,
                status_changed_at: 0,
            })
            .unwrap();
        assert_eq!(store.get_terminal(&tid).unwrap().unwrap().status, None);

        let stamp = store
            .set_terminal_status(&tid, Some(AgentStatus::NeedsFeedback))
            .unwrap();
        let term = store.get_terminal(&tid).unwrap().unwrap();
        assert_eq!(term.status, Some(AgentStatus::NeedsFeedback));
        assert_eq!(term.status_changed_at, stamp);
        let (_, _, _, terminals) = store.load_tree().unwrap();
        assert_eq!(
            terminals[0].status,
            Some(AgentStatus::NeedsFeedback),
            "load_tree carries the status too"
        );

        store.set_terminal_status(&tid, None).unwrap();
        assert_eq!(store.get_terminal(&tid).unwrap().unwrap().status, None);

        store
            .set_terminal_status(&tid, Some(AgentStatus::Running))
            .unwrap();
        store.sweep_terminal_statuses().unwrap();
        assert_eq!(
            store.get_terminal(&tid).unwrap().unwrap().status,
            None,
            "boot sweep clears every terminal status"
        );
    }

    #[test]
    fn sweep_disconnected_only_hits_live_statuses() {
        let store = Store::open_in_memory().unwrap();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId::generate(),
            name: "p".into(),
            repo_path: "/tmp/p".into(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        store.insert_project(&project).unwrap();
        let wt = Worktree {
            id: WorktreeId::generate(),
            project_id: project.id.clone(),
            path: "/tmp/p".into(),
            branch: "main".into(),
            is_main: true,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        store.insert_worktree(&wt).unwrap();
        for (name, status) in [
            ("a", AgentStatus::Running),
            ("b", AgentStatus::Finished),
            ("c", AgentStatus::NeedsFeedback),
        ] {
            store
                .insert_agent(&Agent {
                    id: AgentId(format!("agent-{name}")),
                    worktree_id: wt.id.clone(),
                    name: name.into(),
                    status,
                    archived: false,
                    archived_at: 0,
                    pinned: false,
                    kind: AgentKind::Claude,
                    model: None,
                    effort: None,
                    session_id: None,
                    sort_order: 0,
                    status_changed_at: 0,
                    alive: false,
                    branch: None,
                    spawned_by: None,
                    writer: false,
                    card_id: None,
                    role: None,
                    dispatch_id: None,
                    code_mode_off: false,
                })
                .unwrap();
        }
        let swept = store.sweep_disconnected().unwrap();
        assert_eq!(swept.len(), 2);
        let (_, _, agents, _) = store.load_tree().unwrap();
        assert_eq!(
            agents
                .iter()
                .filter(|a| a.status == AgentStatus::Disconnected)
                .count(),
            2
        );
        assert_eq!(
            agents
                .iter()
                .filter(|a| a.status == AgentStatus::Finished)
                .count(),
            1
        );
    }
}
