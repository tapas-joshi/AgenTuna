//! TS-003: source and update-tracking repository over `sources`.

mod common;

use at_db::Database;
use at_db::ops::OperationActor;
use at_db::settings::{self, SettingsKey};
use at_db::sources::{
    AutoUpdate, CheckOutcome, SourceKind, SourceSpec, archive_source, effective_auto_update,
    get_source, record_update_check, set_auto_update, set_ignored_sha, set_installed_commit,
    set_local_modified, source_for, source_history, update_candidates, upsert_source,
};
use at_model::ItemId;
use rusqlite::{Connection, params};

use common::{ALL_VERSIONS, open_with};

fn item_id() -> ItemId {
    ItemId::new()
}

fn insert_item(connection: &Connection, id: &ItemId, scope: &str) {
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) \
             VALUES (?1, 'skill', ?2, 'demo', 'Demo')",
            params![id.to_string(), scope],
        )
        .expect("item inserts");
}

fn git_spec<'a>(item: &'a ItemId, url: &'a str, commit: &'a str) -> SourceSpec<'a> {
    SourceSpec {
        item_id: item,
        kind: SourceKind::Git,
        url: Some(url),
        subpath: Some("skills/demo"),
        commit_sha: Some(commit),
        git_ref: Some("main"),
        version: None,
        content_hash: Some("hash-imported"),
        marketplace_id: None,
        install_count: None,
        etag: None,
        license: None,
        auto_update: None,
    }
}

#[test]
fn import_writes_one_row_and_reimport_updates_it_in_place() {
    let mut database = open_with(ALL_VERSIONS);
    let id = item_id();
    insert_item(database.connection(), &id, "user");

    let first = upsert_source(
        database.connection_mut(),
        &git_spec(&id, "https://example.test/skills", "aaa111"),
    )
    .expect("first import");
    let second = upsert_source(
        database.connection_mut(),
        &git_spec(&id, "https://example.test/skills-v2", "bbb222"),
    )
    .expect("re-import");

    assert_eq!(first, second, "re-import updates the same row in place");

    let rows: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sources WHERE item_id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .expect("count sources");
    assert_eq!(rows, 1, "exactly one source row per item");

    let source = source_for(database.connection(), &id)
        .expect("source_for")
        .expect("live row");
    assert_eq!(source.id, first);
    assert_eq!(
        source.url.as_deref(),
        Some("https://example.test/skills-v2")
    );
    assert_eq!(source.commit_sha.as_deref(), Some("bbb222"));
    assert_eq!(source.kind, SourceKind::Git);
    assert!(!source.local_modified);
    assert!(!source.update_available);
    assert!(source.deleted_at.is_none());
    assert!(source.installed_at.is_some());
    assert_eq!(source.auto_update, None, "no override was set");
}

#[test]
fn reimport_preserves_the_auto_update_override_unless_specified() {
    let mut database = open_with(ALL_VERSIONS);
    let id = item_id();
    insert_item(database.connection(), &id, "user");

    let mut spec = git_spec(&id, "https://example.test/skills", "aaa111");
    spec.auto_update = Some(AutoUpdate::On);
    let source_id = upsert_source(database.connection_mut(), &spec).expect("import");

    // A re-import that says nothing about auto-update keeps the override.
    upsert_source(
        database.connection_mut(),
        &git_spec(&id, "https://example.test/skills", "bbb222"),
    )
    .expect("re-import");
    let source = get_source(database.connection(), &source_id)
        .expect("get_source")
        .expect("row");
    assert_eq!(source.auto_update, Some(AutoUpdate::On));

    // An explicit override replaces it.
    let mut spec = git_spec(&id, "https://example.test/skills", "ccc333");
    spec.auto_update = Some(AutoUpdate::Off);
    upsert_source(database.connection_mut(), &spec).expect("re-import");
    let source = get_source(database.connection(), &source_id)
        .expect("get_source")
        .expect("row");
    assert_eq!(source.auto_update, Some(AutoUpdate::Off));
}

/// Upserts a source for `item` and records an update check when `remote` is
/// given. Returns the source row id.
fn upsert_with_check(
    database: &mut Database,
    item: &ItemId,
    kind: SourceKind,
    commit: &str,
    remote: Option<&str>,
) -> String {
    let mut spec = git_spec(item, "https://example.test/skills", commit);
    spec.kind = kind;
    spec.marketplace_id = (kind == SourceKind::Marketplace).then_some("mkt_1");
    let source_id = upsert_source(database.connection_mut(), &spec).expect("source upserts");
    if let Some(remote) = remote {
        record_update_check(
            database.connection_mut(),
            &source_id,
            &CheckOutcome {
                remote_sha: Some(remote),
                checked_at: Some("2026-11-05T10:00:00.000Z"),
                update_available: remote != commit,
                ..CheckOutcome::default()
            },
        )
        .expect("check records");
    }
    source_id
}

#[test]
fn update_candidates_returns_only_git_and_marketplace_rows_with_a_new_remote() {
    let mut database = open_with(ALL_VERSIONS);

    let slugs = [
        "candidate-git",
        "unchanged-git",
        "candidate-market",
        "ignored-git",
        "folder-src",
        "unchecked-git",
        "deleted-git",
    ];
    let mut ids = Vec::new();
    for slug in slugs {
        let id = item_id();
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name) \
                 VALUES (?1, 'skill', 'user', ?2, ?2)",
                params![id.to_string(), slug],
            )
            .expect("item inserts");
        ids.push(id);
    }
    let [
        git_new,
        git_same,
        market_new,
        git_ignored,
        folder,
        git_unchecked,
        git_deleted,
    ] = ids.try_into().expect("seven ids");

    upsert_with_check(&mut database, &git_new, SourceKind::Git, "aaa", Some("bbb"));
    upsert_with_check(
        &mut database,
        &git_same,
        SourceKind::Git,
        "aaa",
        Some("aaa"),
    );
    upsert_with_check(
        &mut database,
        &market_new,
        SourceKind::Marketplace,
        "v1",
        Some("v2"),
    );
    let ignored_id = upsert_with_check(
        &mut database,
        &git_ignored,
        SourceKind::Git,
        "aaa",
        Some("bbb"),
    );
    set_ignored_sha(database.connection(), &ignored_id, Some("bbb")).expect("ignore records");
    upsert_with_check(&mut database, &folder, SourceKind::Folder, "", None);
    upsert_with_check(&mut database, &git_unchecked, SourceKind::Git, "aaa", None);
    let deleted_id = upsert_with_check(
        &mut database,
        &git_deleted,
        SourceKind::Git,
        "aaa",
        Some("bbb"),
    );
    archive_source(
        database.connection(),
        &deleted_id,
        Some("2026-11-05T11:00:00.000Z"),
    )
    .expect("archive");

    let candidates = update_candidates(database.connection()).expect("update_candidates");
    let candidate_items: Vec<String> = candidates
        .iter()
        .filter_map(|row| row.item_id.clone())
        .collect();
    assert_eq!(
        candidate_items,
        vec![git_new.to_string(), market_new.to_string()],
        "only live git/marketplace rows whose remote differs and is not ignored"
    );
    assert!(
        candidates
            .iter()
            .all(|row| matches!(row.kind, SourceKind::Git | SourceKind::Marketplace))
    );
}

#[test]
fn effective_auto_update_resolves_override_against_the_global_setting() {
    let mut database = open_with(ALL_VERSIONS);

    let inherit = item_id();
    let on = item_id();
    let off = item_id();
    for (id, slug) in [(&inherit, "inherit"), (&on, "on-item"), (&off, "off-item")] {
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name) \
                 VALUES (?1, 'skill', 'user', ?2, ?2)",
                params![id.to_string(), slug],
            )
            .expect("item inserts");
    }

    // Global setting off (the default): inherit and missing sources are off.
    assert!(!effective_auto_update(database.connection(), &inherit).expect("inherit off"));
    assert!(!effective_auto_update(database.connection(), &ItemId::new()).expect("no source off"));

    // Per-item overrides win over the global flag.
    let on_id = upsert_source(
        database.connection_mut(),
        &git_spec(&on, "https://example.test/s", "c1"),
    )
    .expect("on import");
    set_auto_update(database.connection(), &on_id, AutoUpdate::On).expect("set on");
    assert!(effective_auto_update(database.connection(), &on).expect("on wins"));

    let off_id = upsert_source(
        database.connection_mut(),
        &git_spec(&off, "https://example.test/s", "c1"),
    )
    .expect("off import");
    set_auto_update(database.connection(), &off_id, AutoUpdate::Off).expect("set off");
    assert!(!effective_auto_update(database.connection(), &off).expect("off wins"));

    // Global on: inherit follows it, explicit overrides still win.
    settings::set_auto_update_skills(database.connection_mut(), true, OperationActor::Ui)
        .expect("global on");
    assert!(
        effective_auto_update(database.connection(), &inherit).expect("inherit follows global")
    );
    assert!(effective_auto_update(database.connection(), &on).expect("on still on"));
    assert!(!effective_auto_update(database.connection(), &off).expect("off still off"));

    // Sanity: the setting round-trips through the canonical key.
    let loaded = settings::load(database.connection()).expect("settings load");
    assert!(loaded.auto_update_skills);
    assert_eq!(
        SettingsKey::AutoUpdateSkills.stored_key(),
        "auto_update_skills"
    );
}

#[test]
fn archive_sets_deleted_at_and_never_deletes_the_row() {
    let mut database = open_with(ALL_VERSIONS);
    let id = item_id();
    insert_item(database.connection(), &id, "user");

    let source_id = upsert_source(
        database.connection_mut(),
        &git_spec(&id, "https://example.test/skills", "aaa111"),
    )
    .expect("import");

    assert!(
        archive_source(
            database.connection(),
            &source_id,
            Some("2026-11-05T12:00:00.000Z"),
        )
        .expect("archive")
    );
    // Archiving twice is a no-op, not an error.
    assert!(
        !archive_source(database.connection(), &source_id, None).expect("re-archive"),
        "already-archived rows are not re-archived"
    );

    assert!(
        source_for(database.connection(), &id)
            .expect("source_for")
            .is_none()
    );
    let history = source_history(database.connection(), &id).expect("source_history");
    assert_eq!(history.len(), 1, "the row survives for origin history");
    assert_eq!(
        history[0].deleted_at.as_deref(),
        Some("2026-11-05T12:00:00.000Z")
    );

    // A re-import revives the same row instead of duplicating it.
    let revived = upsert_source(
        database.connection_mut(),
        &git_spec(&id, "https://example.test/skills", "bbb222"),
    )
    .expect("re-import");
    assert_eq!(revived, source_id);
    let source = source_for(database.connection(), &id)
        .expect("source_for")
        .expect("live again");
    assert!(source.deleted_at.is_none());
    assert_eq!(
        source_history(database.connection(), &id)
            .expect("history")
            .len(),
        1
    );
}

#[test]
fn update_check_and_installed_commit_round_trip() {
    let mut database = open_with(ALL_VERSIONS);
    let id = item_id();
    insert_item(database.connection(), &id, "user");
    let source_id = upsert_source(
        database.connection_mut(),
        &git_spec(&id, "https://example.test/skills", "aaa111"),
    )
    .expect("import");

    assert!(
        record_update_check(
            database.connection(),
            &source_id,
            &CheckOutcome {
                remote_sha: Some("bbb222"),
                etag: Some("\"etag-1\""),
                checked_at: Some("2026-11-05T10:00:00.000Z"),
                update_available: true,
            },
        )
        .expect("check records")
    );
    let source = get_source(database.connection(), &source_id)
        .expect("get")
        .expect("row");
    assert_eq!(source.remote_sha.as_deref(), Some("bbb222"));
    assert_eq!(source.etag.as_deref(), Some("\"etag-1\""));
    assert!(source.update_available);
    assert_eq!(
        source.last_checked_at.as_deref(),
        Some("2026-11-05T10:00:00.000Z")
    );

    // local_modified is the flag auto-update reads to skip diverged content.
    assert!(set_local_modified(database.connection(), &source_id, true).expect("modified"));
    let source = get_source(database.connection(), &source_id)
        .expect("get")
        .expect("row");
    assert!(source.local_modified);

    // Applying the update bumps the installed commit and clears the badge.
    assert!(
        set_installed_commit(database.connection(), &source_id, "bbb222", Some("hash-v2"))
            .expect("commit bumps")
    );
    let source = get_source(database.connection(), &source_id)
        .expect("get")
        .expect("row");
    assert_eq!(source.commit_sha.as_deref(), Some("bbb222"));
    assert_eq!(source.content_hash.as_deref(), Some("hash-v2"));
    assert!(!source.update_available);
}

#[test]
fn every_source_kind_wire_name_round_trips() {
    for kind in SourceKind::ALL {
        let parsed: SourceKind = kind
            .as_str()
            .parse()
            .unwrap_or_else(|_| panic!("{} must parse", kind.as_str()));
        assert_eq!(parsed, *kind);
    }
    assert!("tarball".parse::<SourceKind>().is_err());
    assert_eq!(AutoUpdate::Inherit.as_i64(), 2);
    assert_eq!(AutoUpdate::from_i64(2), Some(AutoUpdate::Inherit));
    assert_eq!(AutoUpdate::from_i64(3), None);
}

#[test]
fn per_skill_auto_update_tri_state_survives_a_database_reopen() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("index.sqlite");
    let rows = {
        let mut database = Database::open_file(&path).expect("file database");
        let mut rows = Vec::new();
        for (index, mode) in [AutoUpdate::Off, AutoUpdate::Inherit, AutoUpdate::On]
            .into_iter()
            .enumerate()
        {
            let id = item_id();
            database
                .connection()
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name) \
                     VALUES (?1, 'skill', 'user', ?2, ?2)",
                    params![id.to_string(), format!("mode-{index}")],
                )
                .expect("item inserts");
            let source = upsert_source(
                database.connection_mut(),
                &git_spec(&id, "https://example.test/skills", "aaa111"),
            )
            .expect("source row");
            set_auto_update(database.connection(), &source, mode).expect("set override");
            rows.push((id, source, mode));
        }
        rows
    };

    let reopened = Database::open_file(&path).expect("reopen database");
    for (item, source, mode) in rows {
        let row = get_source(reopened.connection(), &source)
            .expect("read source")
            .expect("source survives reopen");
        assert_eq!(
            row.auto_update,
            Some(mode),
            "stored tri-state survives restart"
        );
        // `auto_update_skills` defaults to off, so only an explicit On is effective.
        assert_eq!(
            effective_auto_update(reopened.connection(), &item).expect("effective mode"),
            mode == AutoUpdate::On
        );
    }
    assert!(
        !settings::load(reopened.connection())
            .expect("settings")
            .auto_update_skills,
        "the global default stays off"
    );
}
