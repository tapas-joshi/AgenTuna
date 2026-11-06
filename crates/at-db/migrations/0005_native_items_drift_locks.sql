-- Migration v5: items found only in an agent's own folders, explicit locks, and
-- the drift history.
--
-- TF-014 owns this migration. `native_items` and `locks` are rebuildable (a scan
-- re-derives the first, store lock files the second); `drift_events` is history
-- and survives every rescan.
--
-- `path` is the natural key of a native item: one filesystem location is one
-- row. A vanished item keeps its row and becomes `status = 'missing'` instead of
-- being deleted, so the next scan can recognise it again. `adopted_item_id` has
-- no foreign key on purpose: the TF-014 acceptance keeps native rows after the
-- canonical item is force-deleted, leaving the adoption reference dangling.
CREATE TABLE native_items (
    id              TEXT PRIMARY KEY,
    agent_id        TEXT NOT NULL,
    scope           TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    kind            TEXT NOT NULL CHECK (kind IN ('skill', 'mcp_server', 'rule')),
    name            TEXT NOT NULL,
    path            TEXT NOT NULL UNIQUE,
    content_hash    TEXT,
    first_seen_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_seen_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    adopted_item_id TEXT,
    status          TEXT NOT NULL DEFAULT 'new' CHECK (status IN ('new', 'adopted', 'ignored', 'missing'))
) STRICT;

CREATE INDEX idx_native_items_agent_status ON native_items(agent_id, status);
CREATE INDEX idx_native_items_kind ON native_items(kind);
CREATE INDEX idx_native_items_last_seen_at ON native_items(last_seen_at);

-- A lock pins an item to the definition at one scope, blocking every lower
-- scope. UNIQUE(item_id, scope) both prevents a duplicate lock and serves the
-- per-item lookup, so no extra index on item_id is created. There is no foreign
-- key to `items`: a lock row is written by the engine and a dangling lock must
-- fail resolution visibly rather than disappear with a rebuild.
CREATE TABLE locks (
    id        INTEGER PRIMARY KEY,
    item_id   TEXT NOT NULL,
    scope     TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    reason    TEXT,
    locked_by TEXT,
    locked_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (item_id, scope)
) STRICT;

-- One row per detected external edit. `resolution` stays NULL while the event is
-- open; resolving sets `resolved_at` and one of adopt/restore/ignore, and may
-- reference the backup taken during the resolution. History is never rebuilt
-- from files, so no foreign keys tie it to rows that a rebuild replaces.
CREATE TABLE drift_events (
    id            INTEGER PRIMARY KEY,
    item_id       TEXT,
    agent_id      TEXT NOT NULL,
    scope         TEXT NOT NULL CHECK (scope IN ('global', 'user', 'project', 'local')),
    path          TEXT NOT NULL,
    expected_hash TEXT,
    actual_hash   TEXT,
    detected_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    resolved_at   TEXT,
    resolution    TEXT CHECK (resolution IN ('adopt', 'restore', 'ignore')),
    backup_id     TEXT
) STRICT;

-- (item_id, detected_at DESC) answers "unresolved events for this item, newest
-- first"; idx_drift_events_open adds the resolved filter so that query never
-- walks resolved history.
CREATE INDEX idx_drift_events_item_detected ON drift_events(item_id, detected_at DESC);
CREATE INDEX idx_drift_events_open ON drift_events(item_id, resolved_at, detected_at DESC);
CREATE INDEX idx_drift_events_resolved_at ON drift_events(resolved_at);
