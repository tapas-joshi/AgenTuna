-- Migration v4: the deployment matrix (item x agent x scope).
--
-- TF-013 owns this migration. One row is the state of one item for one agent at
-- one scope; (item_id, agent_id, scope, COALESCE(project_id, '')) is unique, so
-- toggling an item off and on again updates the row instead of inserting a
-- second one.
--
-- `mode` values are the stored deployment modes, not at-model's wire names:
-- at-model::LinkMode serialises as `link`/`copy`, while this column and
-- at-fs::StoredLinkMode store `symlink`/`copy`. `junction` is the Windows
-- directory-junction fallback from TF-038 (at-fs::MODE_JUNCTION); `native` and
-- `none` describe rows that are not materialised by AgenTuna at all.
--
-- `operation_id` records the operation that caused the current state. It has no
-- foreign key: rows are rebuilt from disk (TF-019) and the operation may already
-- have been pruned from history.
CREATE TABLE deployments (
    id               INTEGER PRIMARY KEY,
    item_id          TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    agent_id         TEXT NOT NULL,
    scope            TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    project_id       TEXT,
    state            TEXT NOT NULL CHECK (state IN ('linked', 'off', 'native', 'conflict', 'drifted')),
    mode             TEXT CHECK (mode IN ('symlink', 'junction', 'copy', 'native', 'none')),
    target_path      TEXT,
    link_target      TEXT,
    store_hash       TEXT,
    target_hash      TEXT,
    baseline_hash    TEXT,
    sync_label       TEXT CHECK (sync_label IN ('in_sync', 'project_only', 'library_newer', 'project_newer')),
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_verified_at TEXT,
    operation_id     TEXT
) STRICT;

-- The matrix cell. COALESCE(project_id, '') is required for the same reason as
-- on `items`: NULLs are distinct in a SQLite unique index.
CREATE UNIQUE INDEX idx_deployments_matrix ON deployments(item_id, agent_id, scope, COALESCE(project_id, ''));
CREATE INDEX idx_deployments_agent_scope ON deployments(agent_id, scope);
CREATE INDEX idx_deployments_project_id ON deployments(project_id);
CREATE INDEX idx_deployments_target_path ON deployments(target_path);
CREATE INDEX idx_deployments_state ON deployments(state);
CREATE INDEX idx_deployments_sync_label ON deployments(sync_label);
-- No separate deployments(item_id) index: the matrix index above leads with
-- item_id and serves those lookups.
