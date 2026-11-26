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
    , kind TEXT NOT NULL DEFAULT 'claude', pinned INTEGER NOT NULL DEFAULT 0, model TEXT, effort TEXT, archived_at INTEGER NOT NULL DEFAULT 0, auto_title_pending INTEGER NOT NULL DEFAULT 0, orchestrator INTEGER NOT NULL DEFAULT 0);
CREATE TABLE links (
      id          TEXT PRIMARY KEY,
      worktree_id TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
      url         TEXT NOT NULL,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL
    );
CREATE TABLE "notes" (
      id          TEXT PRIMARY KEY,
      project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
      worktree_id TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
      text        TEXT NOT NULL,
      done        INTEGER NOT NULL DEFAULT 0,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL,
      CHECK ((project_id IS NULL) <> (worktree_id IS NULL))
    );
CREATE TABLE pr_seen (
      url      TEXT PRIMARY KEY,
      marker   TEXT NOT NULL,
      seen_at  INTEGER NOT NULL
    );
CREATE TABLE "projects" (
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
CREATE TABLE workspaces (
      id          TEXT PRIMARY KEY,
      name        TEXT NOT NULL UNIQUE,
      active      INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL
    );
INSERT INTO "workspaces" VALUES('default','default',1,0);
CREATE TABLE worktrees (
      id          TEXT PRIMARY KEY,
      project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
      path        TEXT NOT NULL,
      branch      TEXT NOT NULL,
      is_main     INTEGER NOT NULL DEFAULT 0,
      sort_order  INTEGER NOT NULL DEFAULT 0,
      created_at  INTEGER NOT NULL, pinned INTEGER NOT NULL DEFAULT 0, created_from TEXT,
      UNIQUE (project_id, path)
    );
CREATE UNIQUE INDEX projects_workspace_repo ON projects (COALESCE(workspace_id, 'default'), repo_path);
