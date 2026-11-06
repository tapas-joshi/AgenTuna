//! TS-002: skill version history and operation linkage over `item_versions`.

mod common;

use std::fs;

use at_db::ops::{
    FileAction, NewOperation, OperationActor, OperationKind, OperationStatus, TouchedFile,
};
use at_db::versions::{
    NewVersion, mark_version_seen, record_skill_version, version_content, versions_for,
    versions_for_item,
};
use at_db::{Database, DbError};
use at_model::{ItemId, Scope};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use common::{ALL_VERSIONS, open_with};

fn item_id() -> ItemId {
    ItemId::new()
}

/// Insert one `items` row directly; the repository under test only reads it.
fn insert_item(connection: &Connection, id: &ItemId, scope: &str) {
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) \
             VALUES (?1, 'skill', ?2, 'demo', 'Demo')",
            params![id.to_string(), scope],
        )
        .expect("item inserts");
}

/// A real skill folder on disk hashed the way production callers hash it.
fn skill_folder_hash(root: &TempDir, bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let dir = root.path().join("skills").join("demo");
    fs::create_dir_all(dir.join("scripts")).expect("skill dirs");
    fs::write(dir.join("SKILL.md"), bytes).expect("SKILL.md");
    fs::write(dir.join("scripts").join("run.sh"), b"#!/bin/sh\necho hi\n").expect("script");
    let mut hasher = Sha256::new();
    hasher.update(b"skills/demo/SKILL.md\0");
    hasher.update(bytes);
    hasher.update(b"skills/demo/scripts/run.sh\0");
    hasher.update(b"#!/bin/sh\necho hi\n");
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

#[test]
fn every_write_records_one_row_and_the_hash_tracks_every_byte() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let id = item_id();
    insert_item(connection, &id, "user");

    let temp = TempDir::new().expect("temp dir");
    let hash_v1 = skill_folder_hash(&temp, b"---\nname: demo\ndescription: v1\n---\nbody v1\n");

    record_skill_version(
        connection,
        &NewVersion::new(&id, Scope::User, &hash_v1).with_path("skills/demo"),
    )
    .expect("first version records");

    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM item_versions WHERE item_id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .expect("count versions");
    assert_eq!(count, 1, "one successful write leaves exactly one row");

    // Any byte change anywhere in the folder changes the tree hash, hence the
    // stored content_hash — here a one-byte edit to SKILL.md.
    let hash_v2 = skill_folder_hash(&temp, b"---\nname: demo\ndescription: v2\n---\nbody v1\n");
    assert_ne!(hash_v1, hash_v2, "a single byte changes the folder hash");

    record_skill_version(
        connection,
        &NewVersion::new(&id, Scope::User, &hash_v2).with_path("skills/demo"),
    )
    .expect("second version records");

    let hashes: Vec<String> = connection
        .prepare("SELECT content_hash FROM item_versions WHERE item_id = ?1 ORDER BY id")
        .expect("prepare")
        .query_map([id.to_string()], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(hashes, vec![hash_v1, hash_v2.clone()]);

    // Re-writing identical content upserts the same row instead of duplicating.
    record_skill_version(
        connection,
        &NewVersion::new(&id, Scope::User, &hash_v2).with_path("skills/demo"),
    )
    .expect("repeat write records");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM item_versions WHERE item_id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .expect("count versions");
    assert_eq!(count, 2, "UNIQUE(item_id, content_hash) dedupes rewrites");
}

#[test]
fn versions_for_is_newest_first_and_joins_operation_kind_actor_and_file_count() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let id = item_id();
    insert_item(connection, &id, "project");

    // Two scan-captured versions with explicit timestamps for ordering.
    for (hash, captured) in [
        ("hash-old", "2026-01-01T00:00:00.000Z"),
        ("hash-new", "2026-06-01T00:00:00.000Z"),
    ] {
        connection
            .execute(
                "INSERT INTO item_versions (item_id, scope, content_hash, captured_at) \
                 VALUES (?1, 'project', ?2, ?3)",
                params![id.to_string(), hash, captured],
            )
            .expect("version inserts");
    }

    // One operation-produced version, newest.
    let operation = at_db::ops::record(
        connection,
        &NewOperation::new(OperationKind::EditSkill, OperationActor::Ui)
            .with_scope(Scope::Project)
            .with_summary("edit demo"),
        &[
            TouchedFile::new("/store/skills/demo/SKILL.md", FileAction::Modified)
                .with_hashes(Some("h0"), Some("hash-op")),
            TouchedFile::new("/store/skills/demo/scripts/x.sh", FileAction::Created)
                .with_hashes(None, Some("h1")),
        ],
        OperationStatus::Succeeded,
        None,
    )
    .expect("operation records");
    record_skill_version(
        connection,
        &NewVersion::new(&id, Scope::Project, "hash-op").with_operation(&operation.to_string()),
    )
    .expect("operation version records");

    let versions = versions_for(connection, &id, Scope::Project).expect("versions_for");
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].content_hash, "hash-op", "newest first");
    assert_eq!(versions[1].content_hash, "hash-new");
    assert_eq!(versions[2].content_hash, "hash-old");

    let newest = &versions[0];
    assert_eq!(newest.operation_kind, Some(OperationKind::EditSkill));
    assert_eq!(newest.actor, Some(OperationActor::Ui));
    assert_eq!(newest.operation_status, Some(OperationStatus::Succeeded));
    assert_eq!(newest.operation_file_count, 2);
    let op_id = operation.to_string();
    assert_eq!(newest.operation_id.as_deref(), Some(op_id.as_str()));

    assert_eq!(
        versions[1].operation_kind, None,
        "scan rows join to nothing"
    );
    assert_eq!(versions[1].operation_file_count, 0);

    // Scope filter: the same item has no versions at user scope.
    assert!(
        versions_for(connection, &id, Scope::User)
            .expect("versions_for user")
            .is_empty()
    );
    // Cross-scope listing sees all three.
    assert_eq!(
        versions_for_item(connection, &id)
            .expect("all versions")
            .len(),
        3
    );
}

#[test]
fn version_content_returns_the_stored_file_map_for_diffing() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let id = item_id();
    insert_item(connection, &id, "user");

    let operation = at_db::ops::record(
        connection,
        &NewOperation::new(OperationKind::Adopt, OperationActor::Cli),
        &[
            TouchedFile::new("/store/skills/demo/SKILL.md", FileAction::Created)
                .with_hashes(None, Some("sha-skill"))
                .with_sizes(0, 120),
            TouchedFile::new(
                "/store/skills/demo/references/notes.md",
                FileAction::Created,
            )
            .with_hashes(None, Some("sha-notes")),
        ],
        OperationStatus::Succeeded,
        None,
    )
    .expect("operation records");
    record_skill_version(
        connection,
        &NewVersion::new(&id, Scope::User, "hash-content").with_operation(&operation.to_string()),
    )
    .expect("version records");

    // A scan-captured version with no operation has an empty file map.
    record_skill_version(connection, &NewVersion::new(&id, Scope::User, "hash-scan"))
        .expect("scan version records");

    let version_id: i64 = connection
        .query_row(
            "SELECT id FROM item_versions WHERE content_hash = 'hash-content'",
            [],
            |row| row.get(0),
        )
        .expect("version id");

    let content = version_content(connection, version_id)
        .expect("version_content")
        .expect("version exists");
    assert_eq!(content.version.content_hash, "hash-content");
    assert_eq!(content.version.operation_kind, Some(OperationKind::Adopt));
    assert_eq!(content.files.len(), 2);
    assert_eq!(
        content.files[0].path.as_deref(),
        Some("/store/skills/demo/SKILL.md")
    );
    assert_eq!(content.files[0].action, Some(FileAction::Created));
    assert_eq!(content.files[0].after_hash.as_deref(), Some("sha-skill"));
    assert_eq!(content.files[0].bytes_after, Some(120));
    assert_eq!(content.files[1].after_hash.as_deref(), Some("sha-notes"));

    let scan_id: i64 = connection
        .query_row(
            "SELECT id FROM item_versions WHERE content_hash = 'hash-scan'",
            [],
            |row| row.get(0),
        )
        .expect("scan version id");
    let scan = version_content(connection, scan_id)
        .expect("version_content")
        .expect("scan version exists");
    assert!(scan.files.is_empty(), "no operation means no file map");
    assert_eq!(scan.version.operation_id, None);

    assert!(
        version_content(connection, 9_999_999)
            .expect("unknown id")
            .is_none(),
        "unknown version ids return None"
    );
}

#[test]
fn a_soft_deleted_skill_keeps_its_version_rows() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let id = item_id();
    insert_item(connection, &id, "user");
    record_skill_version(connection, &NewVersion::new(&id, Scope::User, "hash-keep"))
        .expect("version records");

    // TS-014 deletes by marking the item missing, never DELETE — the versions
    // are the audit trail and must survive.
    connection
        .execute(
            "UPDATE items SET state = 'missing' WHERE id = ?1",
            [id.to_string()],
        )
        .expect("mark missing");

    let versions = versions_for_item(connection, &id).expect("versions after delete");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].content_hash, "hash-keep");
}

#[test]
fn rescans_refresh_last_seen_without_losing_operation_linkage() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let id = item_id();
    insert_item(connection, &id, "user");

    let operation = at_db::ops::record_completed(
        connection,
        OperationKind::Promote,
        OperationActor::Ui,
        Some("promote demo"),
    )
    .expect("operation records");
    record_skill_version(
        connection,
        &NewVersion::new(&id, Scope::User, "hash-stable").with_operation(&operation.to_string()),
    )
    .expect("version records");

    // A rebuild-from-files rescan re-derives the same content hash and marks
    // it seen; it must not duplicate the row or drop the operation link.
    assert!(
        mark_version_seen(
            connection,
            &id,
            "hash-stable",
            Some("2026-11-05T12:00:00.000Z")
        )
        .expect("mark seen")
    );
    record_skill_version(
        connection,
        &NewVersion::new(&id, Scope::User, "hash-stable"),
    )
    .expect("rescan records");

    let versions = versions_for_item(connection, &id).expect("versions");
    assert_eq!(versions.len(), 1, "rescan adds no row");
    assert_eq!(
        versions[0].operation_id.as_deref(),
        Some(operation.to_string().as_str()),
        "the operation link survives a scan with no operation"
    );
    assert_eq!(versions[0].operation_kind, Some(OperationKind::Promote));
    assert_eq!(
        versions[0].last_seen_at.as_deref(),
        Some("2026-11-05T12:00:00.000Z")
    );
}

#[test]
fn opening_an_older_database_upgrades_without_losing_history() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("agentuna.db");

    let id = item_id();
    {
        // A database that only knows migrations through v7 (operations).
        let set = at_db::Migrations::sparse(
            &(1..=7)
                .map(|version| at_db::Migration::new(version, common::sql_for(version)))
                .collect::<Vec<_>>(),
        )
        .expect("migration set");
        let database = Database::open_file_with(&path, set).expect("open old schema");
        let connection = database.connection();
        insert_item(connection, &id, "user");
        record_skill_version(
            connection,
            &NewVersion::new(&id, Scope::User, "hash-before-upgrade"),
        )
        .expect("version records");
        at_db::ops::record_completed(
            connection,
            OperationKind::EditSkill,
            OperationActor::Cli,
            None,
        )
        .expect("operation records");
    }

    // Reopen with every shipped migration: v8 and v9 apply additively.
    let database = Database::open_file(&path).expect("upgrade opens");
    let connection = database.connection();
    assert_eq!(database.schema_version().expect("schema version"), 9);

    let versions = versions_for_item(connection, &id).expect("versions after upgrade");
    assert_eq!(versions.len(), 1, "history survives the upgrade");
    assert_eq!(versions[0].content_hash, "hash-before-upgrade");
}

#[test]
fn recording_a_version_for_a_missing_item_is_a_constraint_error() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let ghost = item_id();

    let error = record_skill_version(
        connection,
        &NewVersion::new(&ghost, Scope::User, "hash-ghost"),
    )
    .expect_err("foreign key refuses the version");
    assert!(
        matches!(error, DbError::Constraint { .. }),
        "expected a constraint error, got {error:?}"
    );
}
