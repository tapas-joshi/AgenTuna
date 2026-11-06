-- Migration v3: canonical items and their content-version history.
--
-- TF-012 owns this migration. `items` is the index projection of the AgenTuna
-- store: one row per managed skill, MCP server or rule at one scope. A row's
-- identity is the opaque `itm_` UUIDv7 (TF-002), never kind/scope/path, so a
-- rename or a move between scopes updates the row instead of replacing it.
--
-- Uniqueness is the expression index below rather than a table constraint:
-- `project_id` is NULL for every global, user and local item, and SQLite treats
-- NULLs as distinct, so `UNIQUE (kind, scope, project_id, slug)` would allow
-- duplicates. COALESCE(project_id, '') makes the NULL case comparable.
--
-- `source_id` references `sources`, which migration v6 creates. SQLite resolves
-- foreign keys when rows are written, not at CREATE TABLE time, so the forward
-- reference is valid while v4..v5 apply (no item rows are written on the way).
--
-- `link_target` and `managed` come from TA-010's `RuleSource` and are read back
-- by TA-043's `rules_for_scope`: a rule file that is a symlink records what it
-- points at (so a symlink and its target collapse to one row), and `managed`
-- says whether AgenTuna owns the file. The scan sets both; the default claims
-- no ownership, which is the safe answer when a caller does not say.
--
-- `state` is deliberately unconstrained text: TF-012 does not enumerate the
-- values and TA-043 writes `missing` when a discovered file disappears. The
-- deployment baseline hash lives on `deployments.baseline_hash` (v4), not here.
CREATE TABLE items (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL CHECK (kind IN ('skill', 'mcp_server', 'rule')),
    scope           TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    project_id      TEXT,
    slug            TEXT NOT NULL,
    name            TEXT NOT NULL,
    source_path     TEXT,
    link_target     TEXT,
    content_hash    TEXT,
    state           TEXT,
    managed         INTEGER NOT NULL DEFAULT 0 CHECK (managed IN (0, 1)),
    source_id       TEXT REFERENCES sources(id) ON DELETE SET NULL,
    enabled_default INTEGER CHECK (enabled_default IN (0, 1)),
    tags_json       TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_scanned_at TEXT
) STRICT;

-- The item identity: one row per (kind, scope, project, slug). Moving an item
-- between scopes or renaming its slug therefore fails here loudly instead of
-- silently forking an identity.
CREATE UNIQUE INDEX idx_items_identity ON items(kind, scope, COALESCE(project_id, ''), slug);
CREATE INDEX idx_items_kind_scope ON items(kind, scope);
CREATE INDEX idx_items_project_id ON items(project_id);
CREATE INDEX idx_items_name ON items(name COLLATE NOCASE);
CREATE INDEX idx_items_source_id ON items(source_id);
CREATE INDEX idx_items_content_hash ON items(content_hash);

-- Content versions of an item, one row per distinct content hash.
--
-- `content_hash` is the SHA-256 of the canonicalised content (for a skill, the
-- hash over sorted relative paths plus their bytes; see TS-002). `size` and
-- `mtime` describe the scanned copy; `mtime` is unix milliseconds.
--
-- `last_seen_at` exists because TA-043 reads "last_seen" from this table: a
-- rescan of unchanged content writes no new version row (UNIQUE below) and only
-- refreshes `last_seen_at`. `operation_id` links the write that produced the
-- version (TS-002); it has no foreign key because `operations` arrives in v7 and
-- version history outlives the operation rows.
--
-- Both foreign keys cascade from `items`. A repository that upserts an item by
-- its natural key must therefore use `INSERT ... ON CONFLICT (...) DO UPDATE`
-- rather than `INSERT OR REPLACE`: REPLACE deletes the conflicting row first,
-- and that delete would cascade this history away.
CREATE TABLE item_versions (
    id            INTEGER PRIMARY KEY,
    item_id       TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    scope         TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    content_hash  TEXT NOT NULL,
    path          TEXT,
    size          INTEGER,
    mtime         INTEGER,
    captured_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    version_label TEXT,
    operation_id  TEXT,
    last_seen_at  TEXT,
    UNIQUE (item_id, content_hash)
) STRICT;

CREATE INDEX idx_item_versions_item_captured ON item_versions(item_id, captured_at DESC);
