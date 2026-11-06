-- Migration v7: operation history, the files each operation touched, and the
-- backup catalogue.
--
-- TF-016 owns this migration. These three tables are the only non-rebuildable
-- state in at-db (history and the backup catalogue), which is why the database
-- file itself is backed up on a schedule and before migrations.
--
-- `operations.created_at` mirrors `started_at` at insert time. TF-016 names the
-- column `started_at`; TA-042 and the history screen read `created_at`, so the
-- column exists here with a default (CONTRACT decision 15). Rows are
-- append-only: there is no `updated_at` column and no delete trigger.
CREATE TABLE operations (
    id             TEXT PRIMARY KEY,
    kind           TEXT,
    scope          TEXT,
    actor          TEXT,
    status         TEXT CHECK (status IN ('running', 'succeeded', 'failed', 'interrupted', 'undone')),
    summary        TEXT,
    error_kind     TEXT,
    started_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at    TEXT,
    correlation_id TEXT,
    undo_of        TEXT
) STRICT;

CREATE INDEX idx_operations_started_at ON operations(started_at DESC);
CREATE INDEX idx_operations_kind_status ON operations(kind, status);
CREATE INDEX idx_operations_correlation_id ON operations(correlation_id);

-- One row per file an operation created, changed, linked, moved or deleted.
-- `action = 'linked'` rows carry before_hash NULL: nothing existed before the
-- link, and after_hash is the canonical content hash.
CREATE TABLE operation_files (
    id           INTEGER PRIMARY KEY,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    path         TEXT,
    action       TEXT CHECK (action IN ('created', 'modified', 'deleted', 'linked', 'moved')),
    before_hash  TEXT,
    after_hash   TEXT,
    backup_id    TEXT,
    bytes_before INTEGER,
    bytes_after  INTEGER
) STRICT;

CREATE INDEX idx_operation_files_operation_id ON operation_files(operation_id);
CREATE INDEX idx_operation_files_path ON operation_files(path);

-- The backup catalogue. `operation_id` deliberately has no foreign key: backups
-- outlive the operation row that created them. `backup_path` is globally
-- unique; lookups go by original path newest first.
CREATE TABLE backups (
    id            TEXT PRIMARY KEY,
    operation_id  TEXT,
    original_path TEXT,
    backup_path   TEXT UNIQUE,
    content_hash  TEXT,
    size          INTEGER,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    pinned        INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    expires_at    TEXT,
    restore_count INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX idx_backups_original_path ON backups(original_path, created_at DESC);
CREATE INDEX idx_backups_expires_at ON backups(expires_at);
CREATE INDEX idx_backups_content_hash ON backups(content_hash);
