//! TS-004: presets repository over `presets`/`preset_members`/
//! `preset_applications`/`preset_application_members`, plus the applied-ratio
//! query the presets screen needs (TS-022 consumes these functions).

mod common;

use at_db::Database;
use at_db::presets::{
    ApplicationMemberSpec, NewApplication, NewPreset, PresetError, PresetTargetKind,
    application_members, applied_ratio, create_preset, delete_preset, list_presets, preset_members,
    record_application, remove_member, remove_members_for_item, rename_preset, set_member,
};
use at_model::{ItemId, ItemKind, Scope};
use rusqlite::params;

use common::{ALL_VERSIONS, open_with};

fn item_id() -> ItemId {
    ItemId::new()
}

fn insert_item(database: &Database, id: &ItemId, slug: &str) {
    database
        .connection()
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) \
             VALUES (?1, 'skill', 'user', ?2, ?2)",
            params![id.to_string(), slug],
        )
        .expect("item inserts");
}

fn preset<'a>(name: &'a str, scope: Scope, description: &'a str) -> NewPreset<'a> {
    NewPreset {
        id: None,
        name,
        description: Some(description),
        scope,
        is_builtin: false,
    }
}

#[test]
fn duplicate_names_are_refused_per_scope_case_insensitively() {
    let mut database = open_with(ALL_VERSIONS);

    let first = create_preset(
        database.connection_mut(),
        &preset("Frontend", Scope::User, "ui"),
    )
    .expect("first preset");
    assert!(first.starts_with("prst_"));
    let second = create_preset(
        database.connection_mut(),
        &preset("Tools", Scope::User, "cli"),
    )
    .expect("second preset");

    // Same scope, different case: the UNIQUE(name COLLATE NOCASE, scope)
    // constraint becomes a typed error before SQLite sees it.
    let error = create_preset(
        database.connection_mut(),
        &preset("frontend", Scope::User, "dupe"),
    )
    .expect_err("duplicate name refused");
    match error {
        PresetError::DuplicatePresetName { name, scope } => {
            assert_eq!(name, "frontend");
            assert_eq!(scope, Scope::User);
        }
        other => panic!("expected DuplicatePresetName, got {other:?}"),
    }

    // Same name in another scope is a different preset.
    create_preset(
        database.connection_mut(),
        &preset("FRONTEND", Scope::Project, "project copy"),
    )
    .expect("other scope allowed");

    // Renaming a preset onto a sibling's name in the same scope is refused.
    let error = rename_preset(database.connection_mut(), &second, "fRoNtEnD")
        .expect_err("rename collision refused");
    assert!(matches!(error, PresetError::DuplicatePresetName { .. }));

    // Renaming to the same name (any case) is a no-op success.
    assert!(
        rename_preset(database.connection_mut(), &first, "FRONTEND").expect("same-name rename")
    );
    let presets = list_presets(database.connection()).expect("list");
    assert_eq!(presets.len(), 3);
    let renamed = presets
        .iter()
        .find(|record| record.id == first)
        .expect("first preset listed");
    assert_eq!(renamed.name, "FRONTEND");
}

#[test]
fn applied_ratio_counts_existing_members_and_reports_completion() {
    let mut database = open_with(ALL_VERSIONS);

    // Seven skills: the first two will be enabled, five not.
    let mut skills = Vec::new();
    for index in 0..7 {
        let id = item_id();
        insert_item(&database, &id, &format!("skill-{index}"));
        skills.push(id);
    }
    let doomed = skills[6];

    let preset_id = create_preset(
        database.connection_mut(),
        &preset("Frontend", Scope::User, "seven skills"),
    )
    .expect("preset");
    for (index, skill) in skills.iter().enumerate() {
        set_member(
            database.connection(),
            &preset_id,
            skill,
            ItemKind::Skill,
            index < 2,
        )
        .expect("member");
    }

    let ratio = applied_ratio(
        database.connection(),
        &preset_id,
        PresetTargetKind::Agent,
        "claude",
    )
    .expect("ratio");
    assert_eq!(ratio.total, 7);
    assert_eq!(ratio.enabled, 2);
    assert!(!ratio.complete, "2 of 7 is not fully applied");
    assert_eq!(ratio.preset_id, preset_id);
    assert_eq!(ratio.target_kind, PresetTargetKind::Agent);
    assert_eq!(ratio.target_id, "claude");
    assert!(ratio.members.iter().all(|member| member.exists));

    // Member-level booleans and item metadata come back via the join.
    let members = preset_members(database.connection(), &preset_id).expect("members");
    assert_eq!(members.len(), 7);
    assert!(members[0].enabled);
    assert!(!members[6].enabled);
    assert!(members[0].exists);
    assert_eq!(members[0].item_name.as_deref(), Some("skill-0"));
    assert_eq!(members[0].item_scope, Some(Scope::User));
    assert_eq!(members[0].kind, ItemKind::Skill);

    // Enable the rest: the ratio reports complete.
    for skill in &skills {
        set_member(
            database.connection(),
            &preset_id,
            skill,
            ItemKind::Skill,
            true,
        )
        .expect("enable");
    }
    let ratio = applied_ratio(
        database.connection(),
        &preset_id,
        PresetTargetKind::Project,
        "/repo",
    )
    .expect("ratio");
    assert_eq!(ratio.enabled, 7);
    assert!(ratio.complete);

    // The explicit cascade the item-delete flow runs removes membership rows,
    // and the denominator drops with them.
    assert_eq!(
        remove_members_for_item(database.connection(), &doomed).expect("cascade"),
        1
    );
    let members = preset_members(database.connection(), &preset_id).expect("members");
    assert_eq!(members.len(), 6);
    let ratio = applied_ratio(
        database.connection(),
        &preset_id,
        PresetTargetKind::Agent,
        "claude",
    )
    .expect("ratio");
    assert_eq!(ratio.total, 6);
    assert!(ratio.complete, "6 of 6 remaining is complete");
}

#[test]
fn a_dangling_member_row_drops_out_of_the_ratio_denominator() {
    let mut database = open_with(ALL_VERSIONS);

    let skill = item_id();
    insert_item(&database, &skill, "gone");
    let preset_id = create_preset(
        database.connection_mut(),
        &preset("Solo", Scope::User, "one skill"),
    )
    .expect("preset");
    set_member(
        database.connection(),
        &preset_id,
        &skill,
        ItemKind::Skill,
        true,
    )
    .expect("member");

    // Hard-delete the item row without the cascade call: preset_members.item_id
    // has no FK (TF-017), so the row dangles — the ratio must not count it.
    database
        .connection()
        .execute("DELETE FROM items WHERE id = ?1", [skill.to_string()])
        .expect("item deletes");

    let members = preset_members(database.connection(), &preset_id).expect("members");
    assert_eq!(members.len(), 1, "the member row itself is kept");
    assert!(!members[0].exists);
    assert_eq!(members[0].item_name, None);
    assert_eq!(members[0].item_scope, None);

    let ratio = applied_ratio(
        database.connection(),
        &preset_id,
        PresetTargetKind::Agent,
        "codex",
    )
    .expect("ratio");
    assert_eq!(ratio.total, 0, "non-existent items are excluded");
    assert_eq!(ratio.enabled, 0);
    assert!(!ratio.complete, "an empty preset is never complete");
    assert_eq!(ratio.members.len(), 1);
    assert!(!ratio.members[0].exists);
}

#[test]
fn applications_record_member_prior_state_in_insertion_order() {
    let mut database = open_with(ALL_VERSIONS);

    let preset_id = create_preset(
        database.connection_mut(),
        &preset("Frontend", Scope::User, "apply me"),
    )
    .expect("preset");
    let mut skills = Vec::new();
    let mut skill_strings = Vec::new();
    for index in 0..3 {
        let id = item_id();
        insert_item(&database, &id, &format!("s-{index}"));
        skill_strings.push(id.to_string());
        skills.push(id);
    }
    for skill in &skills {
        set_member(
            database.connection(),
            &preset_id,
            skill,
            ItemKind::Skill,
            true,
        )
        .expect("member");
    }

    let application_id = record_application(
        database.connection_mut(),
        &NewApplication {
            preset_id: &preset_id,
            target_kind: PresetTargetKind::Agent,
            target_id: "omp",
            operation_id: None,
            members: vec![
                ApplicationMemberSpec {
                    item_id: &skill_strings[0],
                    prior_enabled: Some(true),
                },
                ApplicationMemberSpec {
                    item_id: &skill_strings[1],
                    prior_enabled: Some(false),
                },
                ApplicationMemberSpec {
                    item_id: &skill_strings[2],
                    prior_enabled: None,
                },
            ],
            member_count: None,
        },
    )
    .expect("application records");

    let members = application_members(database.connection(), application_id).expect("members");
    assert_eq!(members.len(), 3);
    // Insertion order is the order the apply flow chose; prior_enabled is the
    // state unapply restores, NULL meaning the item did not exist.
    assert_eq!(members[0].item_id, skill_strings[0]);
    assert_eq!(members[1].item_id, skill_strings[1]);
    assert_eq!(members[2].item_id, skill_strings[2]);
    assert_eq!(members[0].prior_enabled, Some(true));
    assert_eq!(members[1].prior_enabled, Some(false));
    assert_eq!(members[2].prior_enabled, None);
    assert!(members[0].id < members[1].id && members[1].id < members[2].id);

    // member_count defaults to the member rows written.
    let member_count: i64 = database
        .connection()
        .query_row(
            "SELECT member_count FROM preset_applications WHERE id = ?1",
            [application_id],
            |row| row.get(0),
        )
        .expect("application row");
    assert_eq!(member_count, 3);
}

#[test]
fn removing_a_member_and_deleting_a_preset_behave() {
    let mut database = open_with(ALL_VERSIONS);

    let skill = item_id();
    insert_item(&database, &skill, "solo");
    let preset_id = create_preset(
        database.connection_mut(),
        &preset("Doomed", Scope::User, "delete me"),
    )
    .expect("preset");
    set_member(
        database.connection(),
        &preset_id,
        &skill,
        ItemKind::Skill,
        true,
    )
    .expect("member");

    // set_member is an upsert: toggling flips enabled without duplicating.
    set_member(
        database.connection(),
        &preset_id,
        &skill,
        ItemKind::Skill,
        false,
    )
    .expect("toggle");
    let members = preset_members(database.connection(), &preset_id).expect("members");
    assert_eq!(members.len(), 1);
    assert!(!members[0].enabled);

    assert!(remove_member(database.connection(), &preset_id, &skill).expect("remove"));
    assert!(
        !remove_member(database.connection(), &preset_id, &skill).expect("remove again"),
        "removing a non-member reports false"
    );
    assert!(
        preset_members(database.connection(), &preset_id)
            .expect("members")
            .is_empty()
    );

    // Deleting the preset removes members but keeps application history.
    set_member(
        database.connection(),
        &preset_id,
        &skill,
        ItemKind::Skill,
        true,
    )
    .expect("re-add");
    let skill_string = skill.to_string();
    let application_id = record_application(
        database.connection_mut(),
        &NewApplication {
            preset_id: &preset_id,
            target_kind: PresetTargetKind::Agent,
            target_id: "claude",
            operation_id: None,
            members: vec![ApplicationMemberSpec {
                item_id: &skill_string,
                prior_enabled: None,
            }],
            member_count: None,
        },
    )
    .expect("application records");

    assert!(delete_preset(database.connection(), &preset_id).expect("delete"));
    assert!(
        list_presets(database.connection())
            .expect("list")
            .is_empty()
    );
    assert!(
        preset_members(database.connection(), &preset_id)
            .expect("members")
            .is_empty()
    );
    // preset_applications.preset_id has no FK: history survives the delete.
    assert_eq!(
        application_members(database.connection(), application_id)
            .expect("history members")
            .len(),
        1
    );
}

#[test]
fn this_story_adds_no_tables_or_columns_to_the_tf017_schema() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    let mut tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("table names");
    tables.retain(|name| name != "sqlite_sequence");
    assert_eq!(
        tables,
        vec![
            "agent_paths",
            "agents",
            "backups",
            "deployments",
            "drift_events",
            "health_checks",
            "item_versions",
            "items",
            "locks",
            "native_items",
            "operation_files",
            "operations",
            "preset_application_members",
            "preset_applications",
            "preset_members",
            "presets",
            "projects",
            "remote_backup_state",
            "schema_version",
            "secret_ref_uses",
            "secret_refs",
            "settings",
            "sources",
        ],
        "the shipped schema has exactly these tables"
    );

    // The four preset tables carry exactly the TF-017 columns — no ALTERs.
    for (table, columns) in [
        (
            "presets",
            vec![
                "created_at",
                "description",
                "id",
                "is_builtin",
                "name",
                "scope",
                "updated_at",
            ],
        ),
        (
            "preset_members",
            vec!["enabled", "id", "item_id", "kind", "preset_id"],
        ),
        (
            "preset_applications",
            vec![
                "applied_at",
                "id",
                "member_count",
                "operation_id",
                "preset_id",
                "target_id",
                "target_kind",
            ],
        ),
        (
            "preset_application_members",
            vec!["application_id", "id", "item_id", "prior_enabled"],
        ),
    ] {
        let mut found: Vec<String> = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("prepare pragma")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("pragma query")
            .collect::<Result<_, _>>()
            .expect("pragma rows");
        found.sort();
        let mut expected = columns;
        expected.sort_unstable();
        assert_eq!(found, expected, "{table} columns changed");
    }
}
