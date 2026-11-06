//! Acceptance tests for `mcp.json`: canonical document, schema gate, unknown keys, backups.

use std::fs;

use at_fs::{BackupPolicy, ExpectedState};
use at_model::{EnvValue, McpServerDef, McpTransport, Scope};
use at_store::layout::{StoreLayout, init_store};
use at_store::mcp_store::{McpStore, McpStoreError, expected_state};
use serde_json::{Value, json};
use tempfile::TempDir;

fn initialized() -> (TempDir, StoreLayout) {
    let dir = TempDir::new().expect("temp dir");
    let layout = StoreLayout::new(Scope::User, dir.path().join("store"));
    init_store(Scope::User, layout.root()).expect("init");
    (dir, layout)
}

fn demo_def() -> McpServerDef {
    McpServerDef {
        display_name: "Demo Server".to_owned(),
        description: Some("A test server".to_owned()),
        transport: McpTransport::Stdio {
            command: "npx".to_owned(),
            args: vec!["-y".to_owned(), "@demo/server".to_owned()],
            cwd: None,
            env: [("API_KEY".to_owned(), EnvValue::Literal("test".to_owned()))]
                .into_iter()
                .collect(),
        },
        extra: Default::default(),
    }
}

#[test]
fn missing_file_loads_empty_and_does_not_create() {
    let (_dir, layout) = initialized();
    let path = layout.mcp_file();
    fs::remove_file(&path).expect("remove mcp.json");
    assert!(!path.exists());

    let loaded = McpStore::load(&layout).expect("load");
    assert!(loaded.store.is_empty());
    assert!(loaded.absent);
    assert!(!path.exists(), "load must not create the file");
}

#[test]
fn save_writes_only_schema_version_and_servers() {
    let (_dir, layout) = initialized();
    let path = layout.mcp_file();
    fs::remove_file(&path).expect("remove mcp.json");

    let mut store = McpStore::empty();
    store.set("demo", demo_def());
    store
        .save(&layout, ExpectedState::Absent, BackupPolicy::None)
        .expect("save");

    let text = fs::read_to_string(&path).expect("read");
    let value: Value = serde_json::from_str(&text).expect("json");
    let object = value.as_object().expect("object");
    assert_eq!(object.len(), 2, "only schema_version and servers");
    assert!(object.contains_key("schema_version"));
    assert!(object.contains_key("servers"));
    for forbidden in ["id", "item_id", "kind", "scope", "path", "hash"] {
        assert!(
            !object.contains_key(forbidden),
            "forbidden key: {forbidden}"
        );
    }
    // No identity keys inside server entries either.
    let servers = object["servers"].as_object().expect("servers");
    for (slug, def) in servers {
        let def_obj = def.as_object().expect("def object");
        for forbidden in ["id", "item_id", "kind", "scope", "path", "hash"] {
            assert!(
                !def_obj.contains_key(forbidden),
                "server {slug} carries forbidden key {forbidden}"
            );
        }
    }
}

#[test]
fn newer_schema_version_is_rejected_without_touching_file() {
    let (_dir, layout) = initialized();
    let path = layout.mcp_file();
    let bytes = br#"{"schema_version":2,"servers":{}}"#;
    fs::write(&path, bytes).expect("write");
    let before_mtime = fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    let before_hash = at_fs::sha256_hex(bytes);

    let error = McpStore::load(&layout).expect_err("schema 2 rejected");
    assert!(matches!(
        error,
        McpStoreError::UnsupportedStoreVersion { .. }
    ));

    let after_mtime = fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    assert_eq!(before_mtime, after_mtime, "mtime unchanged");
    let after_bytes = fs::read(&path).expect("read");
    assert_eq!(
        at_fs::sha256_hex(&after_bytes),
        before_hash,
        "hash unchanged"
    );
}

#[test]
fn malformed_json_reports_line_and_column() {
    let (_dir, layout) = initialized();
    let path = layout.mcp_file();
    fs::write(&path, b"{\"schema_version\":1,\"servers\":{\"demo\":").expect("write");

    let error = McpStore::load(&layout).expect_err("malformed json");
    let message = error.to_string();
    assert!(message.contains("line"), "error names line: {message}");
    assert!(message.contains("column"), "error names column: {message}");

    // No .tmp file left behind.
    let parent = path.parent().expect("parent");
    for entry in fs::read_dir(parent).expect("read dir") {
        let name = entry.expect("entry").file_name();
        let name = name.to_string_lossy();
        assert!(!name.contains(".tmp"), "tmp file left: {name}");
    }
}

#[test]
fn unknown_top_level_key_survives_save() {
    let (_dir, layout) = initialized();
    let path = layout.mcp_file();
    let original = br#"{
  "schema_version": 1,
  "servers": {},
  "$schema": "https://example.com/mcp.schema.json",
  "custom_extension": {"nested": true}
}
"#;
    fs::write(&path, original).expect("write");

    let loaded = McpStore::load(&layout).expect("load");
    assert!(!loaded.absent);
    let expected = expected_state(&loaded);
    let mut store = loaded.store;
    store.set("demo", demo_def());
    store
        .save(&layout, expected, BackupPolicy::None)
        .expect("save");

    let text = fs::read_to_string(&path).expect("read");
    let value: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(
        value["$schema"],
        json!("https://example.com/mcp.schema.json"),
        "$schema key preserved"
    );
    assert_eq!(
        value["custom_extension"],
        json!({"nested": true}),
        "custom_extension key preserved"
    );
}

#[test]
fn save_produces_backup_entry() {
    let (dir, layout) = initialized();
    let path = layout.mcp_file();
    let original = br#"{"schema_version":1,"servers":{}}"#;
    fs::write(&path, original).expect("write");

    let loaded = McpStore::load(&layout).expect("load");
    let expected = expected_state(&loaded);
    let mut store = loaded.store;
    store.set("demo", demo_def());

    let backup_dir = dir.path().join("backups/op1");
    fs::create_dir_all(&backup_dir).expect("backup dir");
    let outcome = store
        .save(
            &layout,
            expected,
            BackupPolicy::OperationFolder(backup_dir.clone()),
        )
        .expect("save");

    assert!(outcome.write.backup_path.is_some(), "backup was taken");
    let backup_path = outcome.write.backup_path.expect("backup path");
    assert!(backup_path.exists(), "backup file exists");
    let backup_bytes = fs::read(&backup_path).expect("read backup");
    assert_eq!(backup_bytes, original, "backup holds original bytes");
}

#[test]
fn servers_sorted_by_slug() {
    let (_dir, layout) = initialized();
    let path = layout.mcp_file();
    fs::remove_file(&path).expect("remove");

    let mut store = McpStore::empty();
    store.set("zebra", demo_def());
    store.set("alpha", demo_def());
    store.set("middle", demo_def());
    store
        .save(&layout, ExpectedState::Absent, BackupPolicy::None)
        .expect("save");

    let text = fs::read_to_string(&path).expect("read");
    let alpha_pos = text.find("\"alpha\"").expect("alpha");
    let middle_pos = text.find("\"middle\"").expect("middle");
    let zebra_pos = text.find("\"zebra\"").expect("zebra");
    assert!(alpha_pos < middle_pos, "alpha before middle");
    assert!(middle_pos < zebra_pos, "middle before zebra");
}

#[test]
fn concurrent_save_is_refused() {
    let (_dir, layout) = initialized();
    let path = layout.mcp_file();
    fs::write(&path, br#"{"schema_version":1,"servers":{}}"#).expect("write");

    let loaded = McpStore::load(&layout).expect("load");
    // External edit between load and save.
    fs::write(&path, br#"{"schema_version":1,"servers":{"external":{}}}"#).expect("external");

    let expected = expected_state(&loaded);
    let mut store = loaded.store;
    store.set("demo", demo_def());
    let error = store
        .save(&layout, expected, BackupPolicy::None)
        .expect_err("concurrent save refused");
    assert!(matches!(error, McpStoreError::Write { .. }));

    // External bytes are untouched.
    let text = fs::read_to_string(&path).expect("read");
    assert!(text.contains("\"external\""), "external edit preserved");
}
