-- Migration v1: the migration runner's own bookkeeping table.
--
-- This is deliberately the only table v1 creates. Domain tables (projects,
-- agents, items, deployments, ...) arrive in migrations v2..v9, each owned by
-- TF-011..TF-018. Never edit this file once released: the runner records its
-- SHA-256 and refuses to open a database whose recorded hash no longer matches.
CREATE TABLE IF NOT EXISTS schema_version (
    version    INTEGER PRIMARY KEY NOT NULL,
    applied_at TEXT NOT NULL,
    sha256     TEXT NOT NULL
) STRICT;
