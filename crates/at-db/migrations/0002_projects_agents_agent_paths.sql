-- Migration v2: project registry, detected agents, and their configuration paths.
--
-- TF-011 owns this migration. The `projects` table also carries the registry
-- columns TU-011 and TU-027 read (`source_root`, `last_used_at`, `hidden`), per
-- CONTRACT decision 15: each schema migration creates its tables with every
-- column other slices read, and no other story writes DDL.
--
-- `last_used_at` is the name TU-011 and the CLI use; `last_opened_at` is the name
-- TF-011 states. They hold the same instant: every repository write sets both,
-- and both start NULL until a project is added or selected.
--
-- `path` is stored canonicalised (symlinks resolved) by the repository and
-- compared case-insensitively: macOS volumes are case-insensitive by default,
-- so `/Users/me/Code/App` and `/Users/me/Code/app` must resolve to one row.
-- The unique index therefore uses the column's NOCASE collation.
CREATE TABLE projects (
    id             TEXT PRIMARY KEY,
    name           TEXT,
    path           TEXT NOT NULL COLLATE NOCASE,
    source_root    TEXT,
    config_hash    TEXT,
    last_opened_at TEXT,
    last_used_at   TEXT,
    hidden         INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1)),
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    is_global      INTEGER NOT NULL DEFAULT 0 CHECK (is_global IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX idx_projects_path ON projects(path);
CREATE INDEX idx_projects_last_used_at ON projects(last_used_at DESC);

-- One row per agent AgenTuna knows about (installed or merely configured).
CREATE TABLE agents (
    id            TEXT PRIMARY KEY,
    display_name  TEXT,
    kind          TEXT,
    version       TEXT,
    installed     INTEGER NOT NULL DEFAULT 0 CHECK (installed IN (0, 1)),
    managed       INTEGER NOT NULL DEFAULT 1 CHECK (managed IN (0, 1)),
    detected_at   TEXT,
    settings_json TEXT
) STRICT;

CREATE INDEX idx_agents_installed ON agents(installed);

-- Every filesystem location an agent reads or writes, per scope. `"exists"` is
-- quoted because EXISTS is a keyword in SQLite; the column stays 0 (not
-- deleted) after the path disappears so detection history survives.
CREATE TABLE agent_paths (
    id          INTEGER PRIMARY KEY,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    scope       TEXT,
    kind        TEXT CHECK (kind IN ('skills_dir', 'mcp_file', 'rules_file', 'settings_file', 'managed_dir', 'other')),
    path        TEXT,
    format      TEXT CHECK (format IN ('json', 'jsonc', 'toml', 'markdown', 'dir', 'yaml')),
    is_primary  INTEGER CHECK (is_primary IN (0, 1)),
    "exists"    INTEGER CHECK ("exists" IN (0, 1)),
    verified    INTEGER NOT NULL DEFAULT 0 CHECK (verified IN (0, 1)),
    is_override INTEGER NOT NULL DEFAULT 0 CHECK (is_override IN (0, 1)),
    UNIQUE (agent_id, scope, kind, path)
) STRICT;

CREATE INDEX idx_agent_paths_agent_id ON agent_paths(agent_id);
CREATE INDEX idx_agent_paths_path ON agent_paths(path);
CREATE INDEX idx_agent_paths_scope ON agent_paths(scope);
