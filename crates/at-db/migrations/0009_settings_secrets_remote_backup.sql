-- Migration v9: typed settings, secret references, and remote backup state.
--
-- TF-018 owns this migration. `settings` is authoritative for user preferences
-- and is exported as JSON only for CLI parity. `secret_refs` and
-- `secret_ref_uses` hold references only: names, Keychain service/account and
-- timestamps. There is deliberately no column that can hold a secret value, no
-- note column, and TM-010 will not add one.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE INDEX idx_settings_updated_at ON settings(updated_at);

CREATE TABLE secret_refs (
    name             TEXT PRIMARY KEY,
    keychain_service TEXT,
    account          TEXT,
    created_at       TEXT,
    rotated_at       TEXT,
    last_read_ok_at  TEXT
) STRICT;

-- Which item/scope uses which secret. Removing the last use leaves the ref row
-- in place while the Keychain item exists (TM-010 owns that lifecycle).
CREATE TABLE secret_ref_uses (
    id          INTEGER PRIMARY KEY,
    secret_name TEXT NOT NULL REFERENCES secret_refs(name) ON DELETE CASCADE,
    item_id     TEXT,
    scope       TEXT,
    UNIQUE (secret_name, item_id, scope)
) STRICT;

CREATE INDEX idx_secret_ref_uses_secret_name ON secret_ref_uses(secret_name);
CREATE INDEX idx_secret_ref_uses_item_id ON secret_ref_uses(item_id);

-- Singleton row: the remote backup configuration always exists, starting
-- unconfigured, and the CHECK/PRIMARY KEY reject a second row.
CREATE TABLE remote_backup_state (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    repo_url     TEXT,
    branch       TEXT,
    last_push_at TEXT,
    last_pull_at TEXT,
    last_commit  TEXT,
    status       TEXT NOT NULL DEFAULT 'unconfigured' CHECK (status IN ('unconfigured', 'idle', 'syncing', 'error')),
    error_kind   TEXT
) STRICT;

INSERT INTO remote_backup_state (id) VALUES (1);
