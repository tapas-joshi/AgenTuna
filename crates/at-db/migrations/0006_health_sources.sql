-- Migration v6: MCP health-check history and item sources/update tracking.
--
-- TF-015 owns this migration. `health_checks` is history owned by the TM-009
-- retention task (newest 200 rows per item); it is never rebuilt from files.
-- `sources` is rebuildable from store metadata.
--
-- `checked_at` is unix milliseconds, as TM-009 states for the row semantics.
-- Every other timestamp in this schema is RFC 3339 text; this one column is the
-- exception because the retention window and the "newest per server" window
-- function compare integers. TM-009 writes the value, so there is no default.
--
-- A failed check stores `error_code` and `error_text_redacted` (never the raw
-- text) and leaves `tools_count` NULL; an ok check stores `latency_ms` and
-- `tools_count`; a skipped_offline check stores none of them.
CREATE TABLE health_checks (
    id                  INTEGER PRIMARY KEY,
    item_id             TEXT,
    scope               TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    project_id          TEXT,
    agent_id            TEXT,
    server_name         TEXT NOT NULL,
    transport           TEXT CHECK (transport IN ('stdio', 'http', 'sse')),
    checked_at          INTEGER NOT NULL,
    status              TEXT NOT NULL CHECK (status IN ('ok', 'failed', 'unknown', 'skipped_offline')),
    trigger             TEXT NOT NULL CHECK (trigger IN ('manual', 'scheduled', 'startup')),
    latency_ms          INTEGER,
    tools_count         INTEGER,
    error_code          TEXT,
    error_text_redacted TEXT
) STRICT;

CREATE INDEX idx_health_checks_item_checked ON health_checks(item_id, checked_at DESC);
CREATE INDEX idx_health_checks_checked_at ON health_checks(checked_at);
CREATE INDEX idx_health_checks_server_checked ON health_checks(server_name, checked_at DESC);
CREATE INDEX idx_health_checks_status ON health_checks(status);

-- Where an item came from and what the remote said when it was last checked.
--
-- `kind = 'folder'` stores its directory path in `url`: the TF-015 column list
-- has a single location column and no separate `path`. `git_ref` is the ref the
-- user pinned (branch/tag), `commit_sha` the resolved commit, `remote_sha` the
-- upstream head at `last_checked_at`, and `ignored_sha` the commit the user
-- dismissed so it stops showing as an update.
--
-- Defaults follow the TF-015 acceptance: `etag`, `ignored_sha` and `auto_update`
-- start NULL (auto_update: 0 off, 1 on, 2 inherit; TS-003), `local_modified`
-- starts 0, and a soft delete sets `deleted_at` while `content_hash` remains.
-- `update_available` is NOT NULL DEFAULT 0 so a fresh row never reads as
-- unknown; TS-001 renders it as a badge.
CREATE TABLE sources (
    id               TEXT PRIMARY KEY,
    item_id          TEXT REFERENCES items(id) ON DELETE SET NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('git', 'library', 'local', 'folder', 'marketplace', 'zip')),
    url              TEXT,
    subpath          TEXT,
    commit_sha       TEXT,
    git_ref          TEXT,
    version          TEXT,
    content_hash     TEXT,
    marketplace_id   TEXT,
    install_count    INTEGER,
    etag             TEXT,
    remote_sha       TEXT,
    ignored_sha      TEXT,
    local_modified   INTEGER NOT NULL DEFAULT 0 CHECK (local_modified IN (0, 1)),
    auto_update      INTEGER CHECK (auto_update IN (0, 1, 2)),
    deleted_at       TEXT,
    installed_at     TEXT,
    last_checked_at  TEXT,
    update_available INTEGER NOT NULL DEFAULT 0 CHECK (update_available IN (0, 1)),
    license          TEXT
) STRICT;

CREATE INDEX idx_sources_item_id ON sources(item_id);
CREATE INDEX idx_sources_url ON sources(url);
CREATE INDEX idx_sources_kind ON sources(kind);
CREATE INDEX idx_sources_marketplace_id ON sources(marketplace_id);
CREATE INDEX idx_sources_deleted_at ON sources(deleted_at);
