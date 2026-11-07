//! `agentuna drift` and `agentuna backups` (TC-031) through the real binary.
//!
//! Every case runs in a temporary AgenTuna home, so nothing reads or writes the
//! operator's agent configuration, Keychain or network.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use at_db::Database;
use at_db::deployments::{DeploymentSpec, upsert_deployment};
use at_db::drift::{NewDriftEvent, record_drift};
use at_db::items::{AdoptedItemSpec, upsert_adopted_item};
use at_db::versions::{NewVersion, record_skill_version};
use at_model::{Agent, AgentId, ItemId, ItemKind, ItemState, Scope};
use at_store::hash_skill_folder;
use at_store::layout::{Slug, StoreLayout};
use serde_json::{Value, json};
use support::{Sandbox, agentuna, exit_code, stdout};

const SKILL_SLUG: &str = "example-skill";
const STORE_SKILL_MD: &str = "# store version\n";
const DISK_SKILL_MD: &str = "# edited on disk\n";
const DISK_NOTE: &str = "a note the store does not have\n";

fn envelope(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).expect("--json writes one envelope object to stdout")
}

/// Creates a healthy index, which the read commands open; `doctor` does not create it.
fn with_index() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    sandbox
}

#[test]
fn drift_list_json_reads_an_empty_index() {
    let sandbox = with_index();
    let output = agentuna(&sandbox, &["drift", "list", "--json"]);
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "drift list failed: {body}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["data"]["events"], json!([]));
}

#[test]
fn drift_list_without_an_index_is_not_found() {
    let sandbox = Sandbox::new();
    let output = agentuna(&sandbox, &["drift", "list", "--json"]);
    let body = envelope(&output);
    assert_ne!(exit_code(&output), 0);
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["error"]["code"], json!("not_found"));
}

#[test]
fn drift_resolutions_refuse_to_write_without_yes() {
    let sandbox = with_index();
    for verb in ["adopt", "restore"] {
        let output = agentuna(&sandbox, &["drift", verb, "7", "--json"]);
        let body = envelope(&output);
        assert_eq!(body["ok"], json!(false), "drift {verb}: {body}");
        assert_eq!(
            body["error"]["code"],
            json!("confirmation_required"),
            "drift {verb}: {body}"
        );
        assert_eq!(exit_code(&output), 9, "drift {verb}: {body}");
    }
}

#[test]
fn drift_resolutions_refuse_read_only_before_the_event_lookup() {
    let sandbox = with_index();
    for verb in ["adopt", "restore"] {
        let output = agentuna(
            &sandbox,
            &["drift", verb, "1", "--yes", "--read-only", "--json"],
        );
        let body = envelope(&output);
        assert_eq!(
            body["error"]["code"],
            json!("locked"),
            "drift {verb} in read-only mode: {body}"
        );
        assert_eq!(
            exit_code(&output),
            5,
            "drift {verb} in read-only mode: {body}"
        );
        assert_ne!(
            body["error"]["code"],
            json!("not_found"),
            "the write guard must come before the event lookup"
        );
    }
}

#[test]
fn drift_dry_run_reports_an_unknown_event_as_not_found() {
    let sandbox = with_index();
    for verb in ["adopt", "restore"] {
        let output = agentuna(&sandbox, &["drift", verb, "7", "--dry-run", "--json"]);
        let body = envelope(&output);
        assert_eq!(
            body["error"]["code"],
            json!("not_found"),
            "drift {verb}: {body}"
        );
    }
}

#[test]
fn drift_write_for_an_unknown_event_is_not_found() {
    let sandbox = with_index();
    let output = agentuna(&sandbox, &["drift", "adopt", "7", "--yes", "--json"]);
    let body = envelope(&output);
    assert_eq!(body["error"]["code"], json!("not_found"), "{body}");
}

#[test]
fn backups_list_json_reads_an_empty_catalogue() {
    let sandbox = with_index();
    let output = agentuna(&sandbox, &["backups", "list", "--json"]);
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "backups list failed: {body}");
    assert_eq!(body["data"]["backups"], json!([]));
}

#[test]
fn backups_restore_refuses_to_write_without_yes() {
    let sandbox = with_index();
    let output = agentuna(&sandbox, &["backups", "restore", "bkp_missing", "--json"]);
    let body = envelope(&output);
    assert_eq!(
        body["error"]["code"],
        json!("confirmation_required"),
        "{body}"
    );
    assert_eq!(exit_code(&output), 9, "{body}");
}

#[test]
fn backups_restore_reports_unknown_ids_as_not_found() {
    let sandbox = with_index();
    let dry = agentuna(
        &sandbox,
        &["backups", "restore", "bkp_missing", "--dry-run", "--json"],
    );
    assert_eq!(envelope(&dry)["error"]["code"], json!("not_found"));
    let write = agentuna(
        &sandbox,
        &["backups", "restore", "bkp_missing", "--yes", "--json"],
    );
    assert_eq!(envelope(&write)["error"]["code"], json!("not_found"));
}

/// One drifted skill: the store and the agent folder disagree, and an open drift event
/// records the on-disk edit.
struct Drifted {
    event_id: i64,
    store_skill: PathBuf,
    disk_skill: PathBuf,
    /// Hash the store content had before the edit, as the drift detector records it.
    store_hash: String,
    /// The same folder as the change-set writer hashes it when it journals a backup.
    store_node_hash: String,
}

/// Writes one skill folder, with `note` added or removed.
fn write_skill(folder: &Path, skill_md: &str, note: Option<&str>) {
    fs::create_dir_all(folder).expect("create the skill folder");
    fs::write(folder.join("SKILL.md"), skill_md).expect("write SKILL.md");
    let notes = folder.join("notes.md");
    match note {
        Some(bytes) => fs::write(&notes, bytes).expect("write notes.md"),
        None => {
            if notes.exists() {
                fs::remove_file(&notes).expect("remove notes.md");
            }
        }
    }
}

fn skill_hash(folder: &Path) -> String {
    hash_skill_folder(folder)
        .expect("hash the skill folder")
        .to_string()
}

/// Seeds a user-scope skill whose deployment drifted, then closes the index so the spawned
/// binary sees a consistent file.
fn seed_drifted_skill(sandbox: &Sandbox) -> Drifted {
    sandbox.healthy();
    // The change-set writer resolves absolute destinations through the canonical authorized
    // root, so seed the same canonical paths the binary will record.
    let home = sandbox.path("home");
    fs::create_dir_all(home.join(".claude/skills")).expect("create the agent skill folder");
    let home = fs::canonicalize(&home).expect("canonical home");
    let user_store = fs::canonicalize(sandbox.user_store()).expect("canonical user store");
    let store_skill = StoreLayout::new(Scope::User, &user_store)
        .skill_dir(&Slug::parse(SKILL_SLUG).expect("valid slug"));
    let disk_skill = home.join(".claude/skills").join(SKILL_SLUG);
    write_skill(&store_skill, STORE_SKILL_MD, None);
    write_skill(&disk_skill, DISK_SKILL_MD, Some(DISK_NOTE));
    let store_hash = skill_hash(&store_skill);
    let disk_hash = skill_hash(&disk_skill);
    let store_node_hash = at_fs::changeset::hash_node(&store_skill)
        .expect("hash the store skill folder")
        .expect("the store skill exists");

    let mut database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
    let connection = database.connection_mut();
    at_db::agents::upsert_agent(
        connection,
        &Agent {
            id: AgentId::ClaudeCode,
            display_name: "Claude Code".to_owned(),
            detected: true,
            version: None,
        },
    )
    .expect("record the agent");
    let item_id = {
        let transaction = connection
            .transaction()
            .expect("begin the item transaction");
        let item_id = upsert_adopted_item(
            &transaction,
            &AdoptedItemSpec {
                proposed_id: ItemId::new(),
                kind: ItemKind::Skill,
                scope: Scope::User,
                project_id: None,
                slug: SKILL_SLUG,
                name: SKILL_SLUG,
                source_path: path_text(&store_skill),
                content_hash: Some(store_hash.as_str()),
            },
        )
        .expect("record the item");
        transaction.commit().expect("commit the item");
        item_id
    };
    upsert_deployment(
        connection,
        &DeploymentSpec::applied(
            &item_id,
            &AgentId::ClaudeCode,
            Scope::User,
            ItemState::Linked,
            Some("copy"),
            Some(path_text(&disk_skill)),
            at_db::deployments::DeploymentHashes::in_sync(store_hash.as_str()),
        ),
    )
    .expect("record the deployment");
    record_skill_version(
        connection,
        &NewVersion::new(&item_id, Scope::User, &store_hash),
    )
    .expect("record the store version");
    let item_text = item_id.to_string();
    let event_id = record_drift(
        connection,
        &NewDriftEvent::new(&AgentId::ClaudeCode, Scope::User, path_text(&disk_skill))
            .for_item(&item_text)
            .with_hashes(Some(&store_hash), Some(&disk_hash)),
    )
    .expect("record the drift event");
    drop(database);

    Drifted {
        event_id,
        store_skill,
        disk_skill,
        store_hash,
        store_node_hash,
    }
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("sandbox paths are UTF-8")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn drift_list_json_reports_open_events_without_writing() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let db_path = sandbox.paths().db_path().to_path_buf();
    let index_before = fs::read(&db_path).expect("read the index");

    let output = agentuna(&sandbox, &["drift", "list", "--json"]);
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "drift list failed: {body}");
    let events = body["data"]["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1, "{body}");
    let event = &events[0];
    assert_eq!(event["id"], json!(drifted.event_id));
    assert_eq!(event["path"], json!(path_text(&drifted.disk_skill)));
    assert_eq!(event["scope"], json!("user"));
    assert_eq!(event["agent_id"], json!("claude_code"));
    assert_eq!(event["expected_hash"], json!(drifted.store_hash));
    assert!(event["actual_hash"].is_string(), "{body}");
    assert_eq!(event["open"], json!(true));

    assert_eq!(
        fs::read(&db_path).expect("re-read the index"),
        index_before,
        "`drift list` wrote the index"
    );
}

#[test]
fn drift_list_detects_an_external_edit_from_the_deployment_baselines() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    // The state a machine has when only the CLI ever ran: deployments are recorded, but no
    // watcher has produced an event yet.
    {
        let database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
        database
            .connection()
            .execute("DELETE FROM drift_events", [])
            .expect("clear recorded events");
    }

    let output = agentuna(&sandbox, &["drift", "list", "--json"]);
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "drift list failed: {body}");
    assert_eq!(body["data"]["refreshed"], json!(true), "{body}");
    let events = body["data"]["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1, "the external edit is detected: {body}");
    assert_eq!(events[0]["path"], json!(path_text(&drifted.disk_skill)));
    assert_eq!(events[0]["scope"], json!("user"));
    assert_eq!(events[0]["agent_id"], json!("claude_code"));
    assert_eq!(events[0]["expected_hash"], json!(drifted.store_hash));
    assert_eq!(read(&drifted.disk_skill.join("SKILL.md")), DISK_SKILL_MD);

    // A second listing must not open a duplicate event for the same path.
    let again = agentuna(&sandbox, &["drift", "list", "--json"]);
    let again = envelope(&again);
    assert_eq!(
        again["data"]["events"].as_array().expect("events").len(),
        1,
        "detection is idempotent: {again}"
    );
}

#[test]
fn drift_list_refresh_fails_when_detection_cannot_run() {
    let sandbox = Sandbox::new();
    seed_drifted_skill(&sandbox);
    let output = agentuna(
        &sandbox,
        &["drift", "list", "--refresh", "--read-only", "--json"],
    );
    let body = envelope(&output);
    assert_eq!(body["error"]["code"], json!("usage"), "{body}");
}

#[test]
fn drift_list_narrows_to_the_requested_scope() {
    let sandbox = Sandbox::new();
    seed_drifted_skill(&sandbox);
    let output = agentuna(&sandbox, &["drift", "list", "--scope", "project", "--json"]);
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "drift list failed: {body}");
    assert_eq!(body["data"]["events"], json!([]));
}

#[test]
fn drift_adopt_takes_the_disk_edit_into_the_store() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let output = agentuna(
        &sandbox,
        &[
            "drift",
            "adopt",
            &drifted.event_id.to_string(),
            "--yes",
            "--json",
        ],
    );
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "drift adopt failed: {body}");
    assert_eq!(body["data"]["action"], json!("adopt"));
    assert_eq!(body["data"]["event_id"], json!(drifted.event_id));
    assert!(body["data"]["operation_id"].is_string(), "{body}");
    assert_eq!(read(&drifted.store_skill.join("SKILL.md")), DISK_SKILL_MD);
    assert_eq!(read(&drifted.store_skill.join("notes.md")), DISK_NOTE);

    let listed = agentuna(&sandbox, &["drift", "list", "--json"]);
    assert_eq!(envelope(&listed)["data"]["events"], json!([]));
}

#[test]
fn drift_restore_writes_the_store_copy_over_the_disk_file() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let output = agentuna(
        &sandbox,
        &[
            "drift",
            "restore",
            &drifted.event_id.to_string(),
            "--yes",
            "--json",
        ],
    );
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "drift restore failed: {body}");
    assert_eq!(body["data"]["action"], json!("restore"));
    assert_eq!(read(&drifted.disk_skill.join("SKILL.md")), STORE_SKILL_MD);
    assert!(
        !drifted.disk_skill.join("notes.md").exists(),
        "restore must remove content the store does not have"
    );
    assert_eq!(
        read(&drifted.store_skill.join("SKILL.md")),
        STORE_SKILL_MD,
        "restore must not rewrite the store"
    );
}

#[test]
fn drift_resolution_refuses_a_file_that_changed_again() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    write_skill(&drifted.disk_skill, "# edited twice\n", Some(DISK_NOTE));
    let output = agentuna(
        &sandbox,
        &[
            "drift",
            "adopt",
            &drifted.event_id.to_string(),
            "--yes",
            "--json",
        ],
    );
    let body = envelope(&output);
    assert_eq!(
        body["error"]["code"],
        json!("drift_changed_again"),
        "a second edit must be refused: {body}"
    );
    assert_eq!(
        read(&drifted.store_skill.join("SKILL.md")),
        STORE_SKILL_MD,
        "the store must stay untouched"
    );
    assert_eq!(exit_code(&output), 6);
}

#[test]
fn backups_list_json_shows_the_backup_a_resolution_took() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let adopted = agentuna(
        &sandbox,
        &[
            "drift",
            "adopt",
            &drifted.event_id.to_string(),
            "--yes",
            "--json",
        ],
    );
    assert_eq!(exit_code(&adopted), 0, "drift adopt failed: {adopted:?}");

    let output = agentuna(&sandbox, &["backups", "list", "--json"]);
    let body = envelope(&output);
    assert_eq!(exit_code(&output), 0, "backups list failed: {body}");
    let backups = body["data"]["backups"].as_array().expect("backups array");
    let saved = backups
        .iter()
        .find(|backup| backup["original_path"] == json!(path_text(&drifted.store_skill)))
        .unwrap_or_else(|| panic!("no backup of the store skill in {body}"));
    assert_eq!(saved["content_hash"], json!(drifted.store_node_hash));
    assert!(saved["id"].is_string(), "{body}");
}

#[test]
fn backups_restore_puts_the_saved_store_copy_back() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let adopted = agentuna(
        &sandbox,
        &[
            "drift",
            "adopt",
            &drifted.event_id.to_string(),
            "--yes",
            "--json",
        ],
    );
    assert_eq!(exit_code(&adopted), 0, "drift adopt failed: {adopted:?}");

    let listed = agentuna(&sandbox, &["backups", "list", "--json"]);
    let body = envelope(&listed);
    let backup_id = body["data"]["backups"]
        .as_array()
        .expect("backups array")
        .iter()
        .find(|backup| backup["original_path"] == json!(path_text(&drifted.store_skill)))
        .and_then(|backup| backup["id"].as_str())
        .unwrap_or_else(|| panic!("no backup of the store skill in {body}"))
        .to_owned();

    let restored = agentuna(
        &sandbox,
        &["backups", "restore", &backup_id, "--yes", "--json"],
    );
    let body = envelope(&restored);
    assert_eq!(exit_code(&restored), 0, "backups restore failed: {body}");
    assert_eq!(body["data"]["backup_id"], json!(backup_id));
    assert_eq!(read(&drifted.store_skill.join("SKILL.md")), STORE_SKILL_MD);
    assert!(
        !drifted.store_skill.join("notes.md").exists(),
        "the saved copy has no notes.md"
    );
}

#[test]
fn backups_list_filters_by_original_path() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let adopted = agentuna(
        &sandbox,
        &[
            "drift",
            "adopt",
            &drifted.event_id.to_string(),
            "--yes",
            "--json",
        ],
    );
    assert_eq!(exit_code(&adopted), 0);

    let matched = agentuna(
        &sandbox,
        &[
            "backups",
            "list",
            "--path",
            path_text(&drifted.store_skill),
            "--json",
        ],
    );
    let body = envelope(&matched);
    assert_eq!(exit_code(&matched), 0, "{body}");
    assert_eq!(
        body["data"]["backups"]
            .as_array()
            .expect("backups array")
            .len(),
        1,
        "{body}"
    );

    let unmatched = agentuna(
        &sandbox,
        &[
            "backups",
            "list",
            "--path",
            path_text(&drifted.disk_skill),
            "--json",
        ],
    );
    let body = envelope(&unmatched);
    assert_eq!(exit_code(&unmatched), 0, "{body}");
    assert_eq!(body["data"]["backups"], json!([]));
}

#[test]
fn previews_report_the_resolution_without_writing() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let preview = agentuna(
        &sandbox,
        &[
            "drift",
            "adopt",
            &drifted.event_id.to_string(),
            "--dry-run",
            "--json",
        ],
    );
    let body = envelope(&preview);
    assert_eq!(exit_code(&preview), 0, "{body}");
    assert_eq!(body["data"]["dry_run"], json!(true));
    assert_eq!(body["data"]["action"], json!("adopt"));
    assert_eq!(body["data"]["event"]["id"], json!(drifted.event_id));
    assert_eq!(read(&drifted.store_skill.join("SKILL.md")), STORE_SKILL_MD);
    let listed = agentuna(&sandbox, &["drift", "list", "--json"]);
    assert_eq!(
        envelope(&listed)["data"]["events"].as_array().map(Vec::len),
        Some(1),
        "a preview must not resolve the event"
    );

    // Resolve for real so the second preview has a backup to inspect.
    let adopted = agentuna(
        &sandbox,
        &[
            "drift",
            "adopt",
            &drifted.event_id.to_string(),
            "--yes",
            "--json",
        ],
    );
    assert_eq!(exit_code(&adopted), 0, "drift adopt failed: {adopted:?}");
    let listed = agentuna(&sandbox, &["backups", "list", "--json"]);
    let body = envelope(&listed);
    let backup_id = body["data"]["backups"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["id"].as_str())
        .unwrap_or_else(|| panic!("no backup in {body}"))
        .to_owned();
    let store_after_adopt = fs::read(drifted.store_skill.join("SKILL.md")).expect("read the store");

    let preview = agentuna(
        &sandbox,
        &["backups", "restore", &backup_id, "--dry-run", "--json"],
    );
    let body = envelope(&preview);
    assert_eq!(exit_code(&preview), 0, "{body}");
    assert_eq!(body["data"]["dry_run"], json!(true));
    assert_eq!(body["data"]["backup"]["id"], json!(backup_id));
    assert_eq!(
        fs::read(drifted.store_skill.join("SKILL.md")).expect("re-read the store"),
        store_after_adopt,
        "a preview must not restore anything"
    );
}

#[test]
fn drift_resolutions_change_nothing_without_yes() {
    let sandbox = Sandbox::new();
    let drifted = seed_drifted_skill(&sandbox);
    let store_before = fs::read(drifted.store_skill.join("SKILL.md")).expect("read the store");
    let disk_before = fs::read(drifted.disk_skill.join("SKILL.md")).expect("read the disk copy");
    for verb in ["adopt", "restore"] {
        let output = agentuna(
            &sandbox,
            &["drift", verb, &drifted.event_id.to_string(), "--json"],
        );
        let body = envelope(&output);
        assert_eq!(
            body["error"]["code"],
            json!("confirmation_required"),
            "drift {verb}: {body}"
        );
        assert_eq!(exit_code(&output), 9, "drift {verb}: {body}");
    }
    assert_eq!(
        fs::read(drifted.store_skill.join("SKILL.md")).expect("re-read the store"),
        store_before,
        "an unconfirmed resolution must not write the store"
    );
    assert_eq!(
        fs::read(drifted.disk_skill.join("SKILL.md")).expect("re-read the disk copy"),
        disk_before,
        "an unconfirmed resolution must not write the agent folder"
    );
    let listed = agentuna(&sandbox, &["drift", "list", "--json"]);
    assert_eq!(
        envelope(&listed)["data"]["events"].as_array().map(Vec::len),
        Some(1),
        "the event must stay open"
    );
}

#[test]
fn drift_resolutions_without_an_event_id_say_where_ids_come_from() {
    let sandbox = with_index();
    for verb in ["adopt", "restore"] {
        let output = agentuna(&sandbox, &["drift", verb, "--json"]);
        assert_eq!(
            exit_code(&output),
            2,
            "a missing event id stays a usage error: {}",
            stdout(&output)
        );
        let body = envelope(&output);
        assert_eq!(body["error"]["code"], "usage");
        let hint = body["error"]["hint"].as_str().expect("hint");
        assert!(
            hint.contains("agentuna drift list"),
            "the hint names the listing command: {hint}"
        );
        assert!(
            hint.contains(&format!("agentuna drift {verb} 1 --dry-run")),
            "the hint shows a concrete preview command: {hint}"
        );
    }
}
