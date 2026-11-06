-- Migration v8: presets, their members, and the history of preset applications.
--
-- TF-017 owns this migration. `presets` and `preset_members` are rebuildable
-- from `presets.json`; `preset_applications` and `preset_application_members`
-- are history and must survive a rebuild.
--
-- Name uniqueness is case-insensitive per scope. The TF-017 text writes
-- `UNIQUE(name, scope COLLATE NOCASE)`, but a COLLATE suffix binds to the column
-- just before it, so that spelling constrains `scope` (always lowercase anyway)
-- and would accept both `Frontend` and `frontend`. The acceptance requires the
-- pair to fail, so the collation is applied to `name`.
CREATE TABLE presets (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    scope       TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    is_builtin  INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1)),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (name COLLATE NOCASE, scope)
) STRICT;

-- Membership. `enabled = 0` is an explicit opt-out (applying the preset leaves
-- that member off), which is different from not being a member at all. The
-- UNIQUE constraint also serves lookups by preset, so no separate preset_id
-- index is created.
CREATE TABLE preset_members (
    id        INTEGER PRIMARY KEY,
    preset_id TEXT NOT NULL REFERENCES presets(id) ON DELETE CASCADE,
    item_id   TEXT NOT NULL,
    kind      TEXT NOT NULL CHECK (kind IN ('skill', 'mcp_server', 'rule')),
    enabled   INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    UNIQUE (preset_id, item_id)
) STRICT;

-- One row per applied preset. `preset_id` deliberately has no foreign key:
-- deleting or rebuilding a preset keeps the application history.
CREATE TABLE preset_applications (
    id           INTEGER PRIMARY KEY,
    preset_id    TEXT NOT NULL,
    target_kind  TEXT NOT NULL CHECK (target_kind IN ('agent', 'project')),
    target_id    TEXT NOT NULL,
    applied_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    operation_id TEXT,
    member_count INTEGER
) STRICT;

CREATE INDEX idx_preset_applications_preset_id ON preset_applications(preset_id);
CREATE INDEX idx_preset_applications_target ON preset_applications(target_kind, target_id);

-- The prior state of each member, so unapply restores exactly what was there.
-- `prior_enabled` is NULL when the item did not exist before the apply. The
-- UNIQUE constraint also serves lookups by application, so no separate
-- application_id index is created.
CREATE TABLE preset_application_members (
    id             INTEGER PRIMARY KEY,
    application_id INTEGER NOT NULL REFERENCES preset_applications(id) ON DELETE CASCADE,
    item_id        TEXT NOT NULL,
    prior_enabled  INTEGER CHECK (prior_enabled IN (0, 1)),
    UNIQUE (application_id, item_id)
) STRICT;
