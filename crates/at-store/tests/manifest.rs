//! Acceptance tests for `store.json`: schema, ids, hashing and atomic persistence.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use at_fs::{BackupPolicy, ExpectedState, FsError};
use at_model::{AgentId, ItemId, ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::manifest::{
    ContentHash, DisabledAgents, FieldProblem, HashError, ItemSource, MANIFEST_MAX_BYTES,
    MANIFEST_MAX_NAME_CHARS, MANIFEST_MAX_SOURCE_FIELD_CHARS, ManifestError, ManifestItem,
    PathTrustError, SKILL_HASH_MAX_FILE_BYTES, SourceKind, StoreManifest, StorePath,
    ValidatedSkillRoot, hash_bytes, hash_mcp_entry, hash_rule_bytes, hash_skill_folder,
    hash_skill_folder_resolved, mcp_identity_key, resolve_item_path,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn initialized(sandbox: &TempDir) -> StoreLayout {
    let layout = StoreLayout::new(Scope::User, sandbox.path().join("store"));
    layout.init().expect("store initializes");
    layout
}

fn skill_item(id: ItemId, slug: &str, hash: &str) -> ManifestItem {
    ManifestItem::new(
        id,
        ItemKind::Skill,
        Slug::parse(slug).expect("slug"),
        StorePath::parse(&format!("skills/{slug}")).expect("path"),
        ContentHash::parse(hash).expect("hash"),
    )
}

fn zero_hash() -> String {
    "0".repeat(64)
}

fn fixed_time() -> OffsetDateTime {
    OffsetDateTime::parse("2026-11-05T00:00:00Z", &Rfc3339).expect("timestamp")
}

fn to_value(manifest: &StoreManifest) -> Value {
    serde_json::to_value(manifest).expect("manifest serializes")
}

fn from_value(value: Value) -> StoreManifest {
    serde_json::from_value(value).expect("manifest deserializes")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("copy directory");
    for entry in fs::read_dir(from).expect("read source") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

#[test]
fn skill_hash_ignores_mtime_and_copy_but_not_a_byte() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let skill = sandbox.path().join("demo");
    fs::create_dir_all(skill.join("assets")).expect("skill folder");
    fs::write(skill.join("SKILL.md"), b"---\nname: demo\n---\nbody\n").expect("SKILL.md");
    fs::write(skill.join("assets/logo.bin"), [0_u8, 1, 2, 255]).expect("asset");

    let first = hash_skill_folder(&skill).expect("hash");
    assert_eq!(
        first.as_str(),
        hash_skill_folder(&skill).expect("stable hash").as_str()
    );

    let copy = sandbox.path().join("copied");
    copy_tree(&skill, &copy);
    assert_eq!(
        hash_skill_folder(&copy).expect("copy hash"),
        first,
        "a byte-identical copy hashes the same"
    );

    let handle = fs::OpenOptions::new()
        .write(true)
        .open(skill.join("SKILL.md"))
        .expect("open");
    handle
        .set_modified(SystemTime::now() + Duration::from_secs(3600))
        .expect("touch mtime");
    assert_eq!(
        hash_skill_folder(&skill).expect("hash after touch"),
        first,
        "mtimes do not enter the tree hash"
    );

    fs::write(skill.join("SKILL.md"), b"---\nname: demo\n---\nbody!\n").expect("edit");
    assert_ne!(
        hash_skill_folder(&skill).expect("hash after edit"),
        first,
        "editing one byte changes the tree hash"
    );
}

#[test]
fn skill_hash_ignores_frontmatter_order_and_crlf_conversion() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let skill = sandbox.path().join("demo");
    fs::create_dir_all(&skill).expect("skill folder");
    fs::write(
        skill.join("SKILL.md"),
        b"---\nname: demo\ndescription: A demo\nlicense: MIT\n---\n# body\n",
    )
    .expect("write LF frontmatter");
    let first = hash_skill_folder(&skill).expect("first hash");

    fs::write(
        skill.join("SKILL.md"),
        b"---\r\nlicense: MIT\r\ndescription: A demo\r\nname: demo\r\n---\r\n# body\r\n",
    )
    .expect("write reordered CRLF frontmatter");
    assert_eq!(
        hash_skill_folder(&skill).expect("hash normalized frontmatter"),
        first
    );
}

#[test]
fn mcp_hash_ignores_key_order_and_whitespace_but_not_values() {
    let first: Value = serde_json::from_str(
        r#"{"command":"node","args":["server.js"],"env":{"TOKEN":"${GITHUB_TOKEN}","MODE":"dev"},"meta":{"z":1,"a":2}}"#,
    )
    .expect("entry");
    let reordered: Value = serde_json::from_str(
        "{\n  \"meta\": {\"a\": 2, \"z\": 1},\n  \"env\": {\"MODE\": \"dev\", \"TOKEN\": \"${GITHUB_TOKEN}\"},\n  \"args\": [\"server.js\"],\n  \"command\": \"node\"\n}",
    )
    .expect("entry");
    assert_eq!(
        hash_mcp_entry(&first).expect("hash"),
        hash_mcp_entry(&reordered).expect("hash"),
        "key order and whitespace do not matter"
    );

    let mut changed = first.clone();
    changed["env"]["MODE"] = json!("prod");
    assert_ne!(
        hash_mcp_entry(&changed).expect("hash"),
        hash_mcp_entry(&first).expect("hash"),
        "a changed env value changes the hash"
    );

    let mut literal = first.clone();
    literal["env"]["TOKEN"] = json!("literal-token");
    assert_ne!(
        hash_mcp_entry(&literal).expect("hash"),
        hash_mcp_entry(&first).expect("hash"),
        "${{VAR}} references stay literal"
    );

    assert!(mcp_identity_key(&first).is_none());
    assert_eq!(mcp_identity_key(&json!({ "id": "itm_x" })), Some("id"));
}

#[test]
fn rule_hash_ignores_crlf_but_not_content() {
    let lf = b"# Rules\n\nUse tabs.\n";
    let crlf = b"# Rules\r\n\r\nUse tabs.\r\n";
    assert_eq!(hash_rule_bytes(lf), hash_rule_bytes(crlf));
    assert_ne!(
        hash_rule_bytes(lf),
        hash_rule_bytes(b"# Rules\n\nUse spaces.\n")
    );
    assert_ne!(
        hash_rule_bytes(crlf),
        hash_rule_bytes(b"# Rules\r\n\r\nUse tabs.\r\n\r\n"),
        "trailing content still counts"
    );
}

#[test]
fn resaving_the_initial_store_is_byte_identical() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let original = fs::read(layout.manifest_path()).expect("store.json");
    let loaded = StoreManifest::load(&layout).expect("load");
    loaded
        .manifest
        .save(
            &layout,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save");
    assert_eq!(
        fs::read(layout.manifest_path()).expect("store.json"),
        original,
        "load then save of an unchanged manifest reproduces the bytes"
    );
}

#[test]
fn saves_are_byte_identical_and_readers_never_see_a_partial_manifest() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let manifest_path = layout.manifest_path();

    let loaded = StoreManifest::load(&layout).expect("load");
    let mut manifest = loaded.manifest.clone();
    let id = ItemId::new();
    manifest
        .insert_item(skill_item(id, "alpha", &zero_hash()))
        .expect("insert");
    let first_save = manifest
        .save(
            &layout,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save");
    let bytes = fs::read(&manifest_path).expect("read");

    StoreManifest::load(&layout)
        .expect("reload")
        .manifest
        .save(
            &layout,
            ExpectedState::Hash(first_save.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("second save");
    assert_eq!(
        fs::read(&manifest_path).expect("read"),
        bytes,
        "a second save of the unchanged manifest writes identical bytes"
    );

    let started = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let started = Arc::clone(&started);
        let stop = Arc::clone(&stop);
        let path = manifest_path.clone();
        std::thread::spawn(move || {
            let mut reads = 0_usize;
            started.store(true, Ordering::Release);
            loop {
                let observed = fs::read(&path).expect("read while writing");
                let value: Value = serde_json::from_slice(&observed)
                    .expect("readers never see a partial manifest");
                assert_eq!(value["schema_version"], 1);
                let items = value["items"].as_array().expect("items array");
                assert_eq!(items.len(), 1);
                let slug = items[0]["slug"].as_str().expect("slug");
                assert!(slug == "alpha" || slug == "alpha-renamed");
                assert_eq!(items[0]["hash"].as_str().map(str::len), Some(64));
                reads += 1;
                if stop.load(Ordering::Acquire) {
                    break;
                }
            }
            reads
        })
    };
    while !started.load(Ordering::Acquire) {
        std::hint::spin_loop();
    }

    let mut current = StoreManifest::load(&layout).expect("load for writer");
    for iteration in 0..25 {
        let existing = current.manifest.item(id).expect("item present").clone();
        let (slug, path) = if iteration % 2 == 0 {
            ("alpha-renamed", "skills/alpha-renamed")
        } else {
            ("alpha", "skills/alpha")
        };
        current
            .manifest
            .update_item(ManifestItem {
                slug: Slug::parse(slug).expect("slug"),
                path: StorePath::parse(path).expect("path"),
                ..existing
            })
            .expect("rename");
        let outcome = current
            .manifest
            .save(
                &layout,
                ExpectedState::Hash(current.hash.to_string()),
                BackupPolicy::None,
            )
            .expect("save");
        current.hash = outcome.hash;
    }
    stop.store(true, Ordering::Release);
    let reads = reader.join().expect("reader thread");
    assert!(reads > 0, "the reader observed at least one manifest");
}

#[test]
fn ids_survive_rename_and_scope_move_and_are_never_reissued() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let user = initialized(&sandbox);
    let project = StoreLayout::new(Scope::Project, sandbox.path().join("project/.agentuna"));
    project.init().expect("project store");

    let loaded = StoreManifest::load(&user).expect("load");
    let mut manifest = loaded.manifest;
    let id = ItemId::new();
    manifest
        .insert_item(skill_item(id, "before", &"a".repeat(64)))
        .expect("insert");
    manifest
        .disable(id, DisabledAgents::All)
        .expect("tombstone");
    manifest
        .save(
            &user,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save");

    let mut loaded = StoreManifest::load(&user).expect("load");
    assert_eq!(loaded.manifest.item(id).expect("item").id, id);
    let previous = loaded
        .manifest
        .update_item(ManifestItem {
            slug: Slug::parse("after").expect("slug"),
            path: StorePath::parse("skills/after").expect("path"),
            ..loaded.manifest.item(id).expect("item").clone()
        })
        .expect("rename");
    assert_eq!(previous.id, id, "rename keeps the id");
    let outcome = loaded
        .manifest
        .save(
            &user,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save rename");
    let mut user_manifest = StoreManifest::load(&user).expect("load renamed");
    let renamed = user_manifest.manifest.item(id).expect("item").clone();
    assert_eq!(renamed.slug.as_str(), "after");
    assert_eq!(renamed.id, id);

    // Move to another scope: remove here (the tombstone goes with it), insert there with the
    // same id.
    assert!(user_manifest.manifest.remove_item(id).is_some());
    assert!(
        user_manifest.manifest.remove_item(id).is_none(),
        "a second removal finds nothing"
    );
    user_manifest
        .manifest
        .save(
            &user,
            ExpectedState::Hash(user_manifest.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save removal");
    let mut project_manifest = StoreManifest::load(&project).expect("load project");
    project_manifest
        .manifest
        .insert_item(renamed.clone())
        .expect("insert in project");
    project_manifest
        .manifest
        .save(
            &project,
            ExpectedState::Hash(project_manifest.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save project");

    let project_reloaded = StoreManifest::load(&project).expect("reload project");
    assert_eq!(project_reloaded.manifest.item(id).expect("item").id, id);
    assert_eq!(
        project_reloaded.manifest.item(id).expect("item").slug,
        renamed.slug
    );
    let user_reloaded = StoreManifest::load(&user).expect("reload user");
    assert!(user_reloaded.manifest.item(id).is_none());
    assert!(
        user_reloaded
            .manifest
            .items()
            .iter()
            .all(|item| item.id != id),
        "the moved id is absent from the old scope"
    );

    // A deleted id is never reissued: a new item gets a new identity.
    let mut user_manifest = user_reloaded;
    let fresh = ItemId::new();
    assert_ne!(fresh, id);
    user_manifest
        .manifest
        .insert_item(skill_item(fresh, "fresh", &"b".repeat(64)))
        .expect("insert fresh");
    user_manifest
        .manifest
        .save(
            &user,
            ExpectedState::Hash(user_manifest.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save fresh");
    let final_manifest = StoreManifest::load(&user).expect("reload").manifest;
    assert!(final_manifest.item(id).is_none());
    assert!(final_manifest.item(fresh).is_some());
    let _ = outcome;
}

#[test]
fn store_json_is_the_only_item_manifest() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let mcp_before = fs::read(layout.mcp_file()).expect("mcp.json");
    let mcp_value: Value = serde_json::from_slice(&mcp_before).expect("mcp.json");
    assert!(mcp_value.get("servers").is_some());
    assert!(mcp_identity_key(&mcp_value).is_none());

    let entry = json!({ "command": "node", "env": { "TOKEN": "${TOKEN}" } });
    let loaded = StoreManifest::load(&layout).expect("load");
    let mut manifest = loaded.manifest;
    manifest
        .insert_item(ManifestItem::new(
            ItemId::new(),
            ItemKind::McpServer,
            Slug::parse("server").expect("slug"),
            StorePath::parse("mcp.json").expect("path"),
            hash_mcp_entry(&entry).expect("hash"),
        ))
        .expect("insert mcp item");
    manifest
        .save(
            &layout,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save");

    assert_eq!(
        fs::read(layout.mcp_file()).expect("mcp.json"),
        mcp_before,
        "saving the manifest never rewrites mcp.json"
    );
    let text = fs::read_to_string(layout.manifest_path()).expect("store.json");
    assert!(text.contains("\"kind\": \"mcp_server\""));
    let value: Value = serde_json::from_str(&text).expect("store.json");
    assert_eq!(value["items"].as_array().expect("items").len(), 1);
    assert!(value["items"][0].get("id").is_some());
    assert!(value.get("servers").is_none());
    assert!(!text.contains("servers"));
}

#[test]
fn invalid_tombstones_are_rejected_with_a_field_error() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = StoreLayout::new(Scope::Global, sandbox.path().join("store"));
    layout.init().expect("store initializes");
    let loaded = StoreManifest::load(&layout).expect("load");
    let mut manifest = loaded.manifest;
    let id = ItemId::new();
    manifest
        .insert_item(skill_item(id, "alpha", &zero_hash()))
        .expect("insert");
    let outcome = manifest
        .save(
            &layout,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save");
    let before = fs::read(layout.manifest_path()).expect("store.json");
    let base = to_value(&manifest);

    let save_invalid = |value: Value| -> ManifestError {
        from_value(value)
            .save(
                &layout,
                ExpectedState::Hash(outcome.hash.to_string()),
                BackupPolicy::None,
            )
            .expect_err("invalid manifest must be rejected")
    };

    let mut value = base.clone();
    value["disabled"] =
        json!([{ "item_id": ItemId::new().to_string(), "kind": "skill", "agents": null }]);
    let ManifestError::Validation(field) = save_invalid(value) else {
        panic!("expected a field error");
    };
    assert_eq!(field.field, "disabled[0].item_id");
    assert!(matches!(field.problem, FieldProblem::MissingItem { .. }));

    let mut value = base.clone();
    value["disabled"] = json!([{ "item_id": id.to_string(), "kind": "rule", "agents": null }]);
    let ManifestError::Validation(field) = save_invalid(value) else {
        panic!("expected a field error");
    };
    assert_eq!(field.field, "disabled[0].kind");
    assert!(matches!(
        field.problem,
        FieldProblem::KindMismatch {
            expected: ItemKind::Skill,
            found: ItemKind::Rule
        }
    ));

    let mut value = base.clone();
    value["disabled"] = json!([{ "item_id": id.to_string(), "kind": "skill", "agents": [] }]);
    let ManifestError::Validation(field) = save_invalid(value) else {
        panic!("expected a field error");
    };
    assert_eq!(field.field, "disabled[0].agents");
    assert!(matches!(field.problem, FieldProblem::EmptyAgents));

    let mut value = base.clone();
    value["disabled"] = json!([
        { "item_id": id.to_string(), "kind": "skill", "agents": null },
        { "item_id": id.to_string(), "kind": "skill", "agents": ["codex"] }
    ]);
    let ManifestError::Validation(field) = save_invalid(value) else {
        panic!("expected a field error");
    };
    assert_eq!(field.field, "disabled[1].item_id");
    assert!(matches!(
        field.problem,
        FieldProblem::DuplicateTombstone { .. }
    ));

    let mut value = base;
    value["disabled"] =
        json!([{ "item_id": id.to_string(), "kind": "skill", "agents": ["codex", "codex"] }]);
    let ManifestError::Validation(field) = save_invalid(value) else {
        panic!("expected a field error");
    };
    assert_eq!(field.field, "disabled[0].agents");
    assert!(matches!(field.problem, FieldProblem::DuplicateAgent { .. }));

    assert_eq!(
        fs::read(layout.manifest_path()).expect("store.json"),
        before,
        "a rejected save writes nothing"
    );

    assert!(
        manifest
            .disable(ItemId::new(), DisabledAgents::All)
            .is_err()
    );
    assert!(
        manifest
            .disable(id, DisabledAgents::Only(Vec::new()))
            .is_err()
    );
    assert!(
        manifest
            .disable(
                id,
                DisabledAgents::Only(vec![AgentId::Codex, AgentId::Codex])
            )
            .is_err()
    );
}

#[test]
fn valid_tombstones_round_trip_and_null_disables_every_agent() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let loaded = StoreManifest::load(&layout).expect("load");
    let mut manifest = loaded.manifest;
    let id = ItemId::new();
    manifest
        .insert_item(skill_item(id, "alpha", &zero_hash()))
        .expect("insert");
    let outcome = manifest
        .save(
            &layout,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save item");

    manifest.disable(id, DisabledAgents::All).expect("disable");
    assert_eq!(
        manifest.disabled_for(id).expect("tombstone").agents,
        DisabledAgents::All
    );
    let outcome = manifest
        .save(
            &layout,
            ExpectedState::Hash(outcome.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save tombstone");
    let loaded = StoreManifest::load(&layout).expect("load tombstone");
    let entry = loaded.manifest.disabled_for(id).expect("tombstone");
    assert_eq!(entry.kind, ItemKind::Skill);
    assert_eq!(entry.agents, DisabledAgents::All);
    assert!(
        entry.agents.contains(&AgentId::Codex),
        "null disables every agent"
    );
    let value: Value =
        serde_json::from_slice(&fs::read(layout.manifest_path()).expect("read")).expect("json");
    assert!(value["disabled"][0]["agents"].is_null());

    let mut manifest = loaded.manifest;
    manifest
        .disable(id, DisabledAgents::Only(vec![AgentId::Codex]))
        .expect("per-agent disable");
    let per_agent = manifest
        .save(
            &layout,
            ExpectedState::Hash(outcome.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save per-agent tombstone");
    let loaded = StoreManifest::load(&layout).expect("load per-agent tombstone");
    assert_eq!(
        loaded.manifest.disabled_for(id).expect("tombstone").agents,
        DisabledAgents::Only(vec![AgentId::Codex])
    );
    let value: Value =
        serde_json::from_slice(&fs::read(layout.manifest_path()).expect("read")).expect("json");
    assert_eq!(value["disabled"][0]["agents"], json!(["codex"]));

    let mut manifest = loaded.manifest;
    assert!(manifest.enable(id));
    assert!(!manifest.enable(id));
    manifest
        .save(
            &layout,
            ExpectedState::Hash(per_agent.hash.to_string()),
            BackupPolicy::None,
        )
        .expect("save enable");
    let value: Value =
        serde_json::from_slice(&fs::read(layout.manifest_path()).expect("read")).expect("json");
    assert_eq!(value["disabled"], json!([]));
    assert_eq!(value["items"].as_array().expect("items").len(), 1);
}

#[test]
fn duplicate_tombstones_collapse_on_load_but_reject_a_save() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let loaded = StoreManifest::load(&layout).expect("load");
    let mut manifest = loaded.manifest;
    let id = ItemId::new();
    manifest
        .insert_item(skill_item(id, "alpha", &zero_hash()))
        .expect("insert");
    let mut value = to_value(&manifest);
    value["disabled"] = json!([
        { "item_id": id.to_string(), "kind": "skill", "agents": null },
        { "item_id": id.to_string(), "kind": "skill", "agents": null }
    ]);
    let bytes = serde_json::to_vec(&value).expect("bytes");

    let parsed = StoreManifest::parse(&bytes).expect("duplicates collapse on load");
    assert_eq!(parsed.disabled().len(), 1);
    let canonical: Value =
        serde_json::from_slice(&parsed.to_bytes().expect("bytes")).expect("json");
    assert_eq!(canonical["disabled"].as_array().expect("array").len(), 1);

    let ManifestError::Validation(field) = from_value(value)
        .save(
            &layout,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect_err("duplicate tombstones must not be written")
    else {
        panic!("expected a field error");
    };
    assert_eq!(field.field, "disabled[1].item_id");
    assert!(matches!(
        field.problem,
        FieldProblem::DuplicateTombstone { .. }
    ));

    let mut value = canonical;
    value["disabled"] =
        json!([{ "item_id": id.to_string(), "kind": "skill", "agents": ["codex", "codex"] }]);
    let bytes = serde_json::to_vec(&value).expect("bytes");
    let error = StoreManifest::parse(&bytes).expect_err("duplicate agents are invalid");
    assert!(matches!(error, ManifestError::Validation(_)));
}

#[test]
fn manifest_paths_reject_traversal_and_symlink_escapes() {
    for bad in [
        "",
        "/etc/passwd",
        "../outside",
        "skills/../outside",
        "skills//demo",
        "./skills/demo",
        "skills/",
        "C:\\skills\\demo",
        "skills/de\0mo",
    ] {
        assert!(StorePath::parse(bad).is_err(), "`{bad}` must be rejected");
    }
    assert!(StorePath::parse("skills/demo").is_ok());
    assert!(StorePath::parse("rules/nested/AGENTS.md").is_ok());

    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let loaded = StoreManifest::load(&layout).expect("load");
    let mut value = to_value(&loaded.manifest);
    value["items"] = json!([{
        "id": ItemId::new().to_string(),
        "kind": "skill",
        "slug": "demo",
        "path": "skills/../outside",
        "hash": zero_hash(),
    }]);
    let text = serde_json::to_vec(&value).expect("bytes");
    assert!(
        matches!(StoreManifest::parse(&text), Err(ManifestError::Json(_))),
        "a traversal path is rejected while parsing"
    );

    let outside = tempfile::tempdir().expect("outside");
    let skill = layout.skills_dir().join("demo");
    fs::create_dir_all(&skill).expect("skill folder");
    std::os::unix::fs::symlink(outside.path(), skill.join("escape")).expect("symlink");
    let error = hash_skill_folder(&skill).expect_err("symlink inside a tree");
    assert!(matches!(error, HashError::Symlink { .. }));

    fs::remove_dir_all(&skill).expect("remove skill");
    let escape = layout.skills_dir().join("escape");
    std::os::unix::fs::symlink(outside.path(), &escape).expect("symlink");
    let path = StorePath::parse("skills/escape").expect("path");
    let error = resolve_item_path(layout.root(), &path).expect_err("symlinked component");
    assert!(matches!(error, PathTrustError::Symlink { .. }));

    let missing = StorePath::parse("skills/not-created").expect("path");
    let resolved = resolve_item_path(layout.root(), &missing).expect("missing tail resolves");
    assert!(!resolved.exists());
    assert!(
        resolved.starts_with(fs::canonicalize(layout.root()).expect("canonical root")),
        "the resolved path stays inside the store root"
    );
}

#[test]
fn manifest_and_skill_content_bounds_are_enforced() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let oversized = vec![b' '; (MANIFEST_MAX_BYTES + 1) as usize];
    fs::write(layout.manifest_path(), &oversized).expect("oversized manifest");
    let error = StoreManifest::load(&layout).expect_err("oversized manifest");
    assert!(matches!(error, ManifestError::TooLarge { .. }));
    assert!(!error.is_missing());

    let skill = sandbox.path().join("big-skill");
    fs::create_dir(&skill).expect("skill folder");
    let file = fs::File::create(skill.join("asset.bin")).expect("asset");
    file.set_len(SKILL_HASH_MAX_FILE_BYTES + 1)
        .expect("sparse file");
    let error = hash_skill_folder(&skill).expect_err("oversized asset");
    assert!(matches!(error, HashError::FileTooLarge { .. }));
}

#[test]
fn unknown_schema_and_scope_mismatch_are_refused() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let loaded = StoreManifest::load(&layout).expect("load");

    let mut value = to_value(&loaded.manifest);
    value["schema_version"] = json!(99);
    let text = serde_json::to_vec(&value).expect("bytes");
    assert!(matches!(
        StoreManifest::parse(&text),
        Err(ManifestError::UnknownSchema { found: 99, .. })
    ));

    let project = StoreManifest::new(
        Scope::Project,
        at_store::layout::StoreId::from_str("0190e7a0-1234-7abc-8def-0123456789ac").expect("id"),
    );
    fs::write(
        layout.manifest_path(),
        serde_json::to_vec(&project).expect("bytes"),
    )
    .expect("write project manifest");
    let error = StoreManifest::load(&layout).expect_err("scope mismatch");
    assert!(matches!(
        error,
        ManifestError::ScopeMismatch {
            expected: Scope::User,
            found: Scope::Project
        }
    ));
}

#[test]
fn an_external_edit_between_load_and_save_is_refused() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let loaded = StoreManifest::load(&layout).expect("load");
    let mut external = fs::read(layout.manifest_path()).expect("store.json");
    external.extend_from_slice(b"\n");
    fs::write(layout.manifest_path(), &external).expect("external edit");

    let error = loaded
        .manifest
        .save(
            &layout,
            ExpectedState::Hash(loaded.hash.to_string()),
            BackupPolicy::None,
        )
        .expect_err("stale save");
    let ManifestError::Write { source, .. } = error else {
        panic!("expected the pipeline error");
    };
    assert!(matches!(source, FsError::ConcurrentModification { .. }));
    assert_eq!(
        fs::read(layout.manifest_path()).expect("store.json"),
        external,
        "the external edit is preserved"
    );
}

#[test]
fn canonical_bytes_have_fixed_key_order_and_sorting() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let loaded = StoreManifest::load(&layout).expect("load");
    let mut manifest = StoreManifest::new(Scope::User, loaded.manifest.store_id());
    let skill_id = ItemId::new();
    let mcp_id = ItemId::new();
    manifest
        .insert_item(
            ManifestItem::new(
                mcp_id,
                ItemKind::McpServer,
                Slug::parse("zeta").expect("slug"),
                StorePath::parse("mcp.json").expect("path"),
                ContentHash::parse(&"1".repeat(64)).expect("hash"),
            )
            .with_name("Zeta"),
        )
        .expect("insert mcp first");
    manifest
        .insert_item(skill_item(skill_id, "alpha", &zero_hash()))
        .expect("insert skill second");
    manifest
        .disable(skill_id, DisabledAgents::All)
        .expect("tombstone");
    let manifest = manifest.with_updated_at(fixed_time());
    let bytes = manifest.to_bytes().expect("canonical bytes");

    let template = include_str!("fixtures/store-manifest.json");
    let expected = template
        .replace("<STORE>", &manifest.store_id().to_string())
        .replace("<SKILL>", &skill_id.to_string())
        .replace("<MCP>", &mcp_id.to_string())
        .replace("<ZERO>", &zero_hash())
        .replace("<ONE>", &"1".repeat(64));
    assert_eq!(String::from_utf8(bytes).expect("utf-8"), expected);

    let reloaded = StoreManifest::parse(expected.as_bytes()).expect("round trip");
    assert_eq!(reloaded, manifest);
}

#[test]
fn store_layout_init_and_default_manifest_agree() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = StoreLayout::new(Scope::Project, sandbox.path().join("project"));
    let outcome = init_store(Scope::Project, sandbox.path().join("project")).expect("init");
    let manifest = StoreManifest::load(&layout).expect("load");
    assert_eq!(manifest.manifest.store_id(), outcome.store_id);
    assert_eq!(manifest.manifest.scope(), Scope::Project);
    assert_eq!(manifest.manifest.schema_version(), 1);
    assert!(manifest.manifest.items().is_empty());
    assert!(manifest.manifest.disabled().is_empty());
}

#[test]
fn duplicate_items_and_invalid_fields_are_refused() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let layout = initialized(&sandbox);
    let mut manifest = StoreManifest::load(&layout).expect("load").manifest;
    let id = ItemId::new();
    manifest
        .insert_item(skill_item(id, "alpha", &zero_hash()))
        .expect("insert");

    let error = manifest
        .insert_item(skill_item(id, "beta", &zero_hash()))
        .expect_err("duplicate id");
    assert_eq!(error.field, "items[1].id");
    assert!(matches!(
        error.problem,
        FieldProblem::DuplicateItemId { .. }
    ));

    let error = manifest
        .insert_item(skill_item(ItemId::new(), "alpha", &zero_hash()))
        .expect_err("duplicate slug");
    assert_eq!(error.field, "items[1].slug");
    assert!(matches!(error.problem, FieldProblem::DuplicateSlug { .. }));

    let wrong_path = ManifestItem::new(
        ItemId::new(),
        ItemKind::Skill,
        Slug::parse("gamma").expect("slug"),
        StorePath::parse("rules/gamma.md").expect("path"),
        ContentHash::parse(&zero_hash()).expect("hash"),
    );
    let error = manifest
        .insert_item(wrong_path)
        .expect_err("path for the wrong kind");
    assert_eq!(error.field, "items[1].path");
    assert!(matches!(
        error.problem,
        FieldProblem::PathKindMismatch { .. }
    ));

    let unnamed = ManifestItem::new(
        ItemId::new(),
        ItemKind::Skill,
        Slug::parse("delta").expect("slug"),
        StorePath::parse("skills/delta").expect("path"),
        ContentHash::parse(&zero_hash()).expect("hash"),
    )
    .with_name("   ");
    let error = manifest.insert_item(unnamed).expect_err("empty name");
    assert_eq!(error.field, "items[1].name");
    assert!(matches!(error.problem, FieldProblem::EmptyName));

    let long_name = ManifestItem::new(
        ItemId::new(),
        ItemKind::Skill,
        Slug::parse("epsilon").expect("slug"),
        StorePath::parse("skills/epsilon").expect("path"),
        ContentHash::parse(&zero_hash()).expect("hash"),
    )
    .with_name("x".repeat(MANIFEST_MAX_NAME_CHARS + 1));
    let error = manifest.insert_item(long_name).expect_err("long name");
    assert_eq!(error.field, "items[1].name");
    assert!(matches!(error.problem, FieldProblem::NameTooLong { .. }));

    let mut source = ItemSource::new(SourceKind::Git);
    source.url = Some("u".repeat(MANIFEST_MAX_SOURCE_FIELD_CHARS + 1));
    let long_source = ManifestItem::new(
        ItemId::new(),
        ItemKind::Skill,
        Slug::parse("zeta").expect("slug"),
        StorePath::parse("skills/zeta").expect("path"),
        ContentHash::parse(&zero_hash()).expect("hash"),
    )
    .with_source(source);
    let error = manifest
        .insert_item(long_source)
        .expect_err("long source field");
    assert_eq!(error.field, "items[1].source.url");
    assert!(matches!(
        error.problem,
        FieldProblem::SourceFieldTooLong { .. }
    ));

    assert_eq!(manifest.items().len(), 1, "rejected inserts change nothing");
}

#[test]
fn content_hashes_reject_malformed_text() {
    assert!(ContentHash::parse(&"A".repeat(64)).is_err());
    assert!(ContentHash::parse(&"g".repeat(64)).is_err());
    assert!(ContentHash::parse("abc").is_err());
    assert!(ContentHash::parse(&zero_hash()).is_ok());
    assert_eq!(
        hash_bytes(b""),
        ContentHash::from_bytes(b""),
        "hashing is deterministic"
    );
}

#[test]
fn a_manifest_cannot_be_saved_into_another_scope() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let user = initialized(&sandbox);
    let before = fs::read(user.manifest_path()).expect("store.json");
    let project = StoreManifest::new(Scope::Project, at_store::layout::StoreId::new());
    let error = project
        .save(
            &user,
            ExpectedState::Hash(hash_bytes(&before).to_string()),
            BackupPolicy::None,
        )
        .expect_err("scope mismatch");
    assert!(matches!(
        error,
        ManifestError::ScopeMismatch {
            expected: Scope::User,
            found: Scope::Project
        }
    ));
    assert_eq!(
        fs::read(user.manifest_path()).expect("store.json"),
        before,
        "the wrong-scope save writes nothing"
    );
}

#[test]
fn a_validated_link_root_hashes_the_resolved_folder_only() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let home = sandbox.path().join("home");
    let library = home.join(".skills-manager/skills/demo");
    fs::create_dir_all(library.join("references")).expect("library folder");
    fs::write(
        library.join("SKILL.md"),
        "---\nname: demo\ndescription: A manager-installed skill\n---\n",
    )
    .expect("document");
    fs::write(library.join("references/guide.md"), "# Guide\n").expect("guide");
    let link = home.join(".claude/skills/demo");
    fs::create_dir_all(link.parent().expect("link parent")).expect("agent root");
    std::os::unix::fs::symlink(&library, &link).expect("link");

    // The entry itself stays refused: this is the guard the manager layout trips.
    let error = hash_skill_folder(&link).expect_err("a link is not a folder");
    assert!(
        matches!(error, HashError::Symlink { relative: None, .. }),
        "{error}"
    );

    // The resolved folder is the content, so both entry points agree on it.
    let resolved = fs::canonicalize(&link).expect("canonical link");
    assert_eq!(
        resolved,
        fs::canonicalize(&library).expect("canonical library")
    );
    let canonical_home = fs::canonicalize(&home).expect("canonical home");
    let validated = ValidatedSkillRoot::new(&resolved, &[canonical_home.as_path()])
        .expect("the resolved folder is inside the injected home");
    assert_eq!(validated.path(), resolved.as_path());
    let from_root = hash_skill_folder_resolved(&validated).expect("resolved hash");
    assert_eq!(
        from_root,
        hash_skill_folder(&library).expect("library hash"),
        "the resolved folder hashes exactly like the real folder"
    );

    // A link inside the tree is still refused, and the error names the entry not the folder.
    let asset = library.join("references/guide.md");
    std::os::unix::fs::symlink(&asset, library.join("references/alias.md")).expect("inner link");
    let error = hash_skill_folder_resolved(&validated).expect_err("inner link refused");
    match error {
        HashError::Symlink { relative, .. } => assert_eq!(
            relative.as_deref(),
            Some(Path::new("references/alias.md")),
            "the entry inside the folder is named"
        ),
        other => panic!("expected a symlink error, got {other}"),
    }
    assert_eq!(
        error_text(&hash_skill_folder_resolved(&validated)),
        "symlink inside skill folder: references/alias.md"
    );

    // A file is not a skill folder, resolved or not.
    let file_root = ValidatedSkillRoot::new(&asset, &[canonical_home.as_path()])
        .expect_err("a file is not a folder");
    assert!(
        matches!(file_root, HashError::NotADirectory { .. }),
        "{file_root}"
    );
}

/// A validated root swapped for a symlink after validation is refused, never followed.
#[cfg(unix)]
#[test]
fn a_resolved_root_swapped_for_a_symlink_is_refused_at_use() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let home = sandbox.path().join("home");
    let library = home.join(".skills-manager/skills/demo");
    fs::create_dir_all(&library).expect("library folder");
    fs::write(library.join("SKILL.md"), "# demo\n").expect("document");
    let canonical_home = fs::canonicalize(&home).expect("canonical home");
    let resolved = fs::canonicalize(&library).expect("canonical library");
    let validated =
        ValidatedSkillRoot::new(&resolved, &[canonical_home.as_path()]).expect("resolved folder");

    // The reviewed target is swapped for an outside symlink before the content is read.
    let outside = sandbox.path().join("outside");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("SKILL.md"), "# outside\n").expect("outside document");
    fs::remove_dir_all(&library).expect("remove the reviewed folder");
    std::os::unix::fs::symlink(&outside, &library).expect("outside link");

    let error = hash_skill_folder_resolved(&validated).expect_err("the swap is refused");
    assert!(
        matches!(
            error,
            HashError::Symlink { relative: None, .. } | HashError::NotCanonical { .. }
        ),
        "a root that is now a symlink must be refused, not followed: {error}"
    );
}

/// A resolved root outside the allowed roots is refused when it is validated.
#[cfg(unix)]
#[test]
fn a_resolved_root_outside_the_allowed_roots_is_refused() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let home = sandbox.path().join("home");
    let outside = sandbox.path().join("outside-skill");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&outside).expect("outside skill");
    fs::write(outside.join("SKILL.md"), "# outside\n").expect("document");
    let canonical_home = fs::canonicalize(&home).expect("canonical home");
    let canonical_outside = fs::canonicalize(&outside).expect("canonical outside");

    let error = ValidatedSkillRoot::new(&canonical_outside, &[canonical_home.as_path()])
        .expect_err("outside the allowed root");
    assert!(
        matches!(error, HashError::OutsideAllowedRoots { .. }),
        "{error}"
    );

    // No allowed roots at all is a refusal, not an implicit “anything goes”.
    let error = ValidatedSkillRoot::new(&canonical_outside, &[]).expect_err("no allowed roots");
    assert!(
        matches!(error, HashError::OutsideAllowedRoots { .. }),
        "{error}"
    );
}

/// A root reached through a symlinked component is not canonical and is refused.
#[cfg(unix)]
#[test]
fn a_non_canonical_resolved_root_is_refused() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let home = sandbox.path().join("home");
    let real = home.join("real");
    let library = real.join("demo");
    fs::create_dir_all(&library).expect("library folder");
    fs::write(library.join("SKILL.md"), "# demo\n").expect("document");
    let alias = home.join("alias");
    std::os::unix::fs::symlink(&real, &alias).expect("parent alias");
    let through_alias = alias.join("demo");
    let canonical_home = fs::canonicalize(&home).expect("canonical home");

    let error = ValidatedSkillRoot::new(&through_alias, &[canonical_home.as_path()])
        .expect_err("a symlinked parent is not canonical");
    assert!(matches!(error, HashError::NotCanonical { .. }), "{error}");

    // The link entry itself is never a content root.
    let canonical_real = fs::canonicalize(&real).expect("canonical real");
    let error = ValidatedSkillRoot::new(&alias, &[canonical_real.as_path()])
        .expect_err("the root is a symlink");
    assert!(
        matches!(
            error,
            HashError::Symlink { relative: None, .. } | HashError::NotCanonical { .. }
        ),
        "{error}"
    );
}

/// Renders whichever error a hashing attempt returned.
fn error_text(result: &Result<ContentHash, HashError>) -> String {
    match result {
        Ok(hash) => hash.to_string(),
        Err(error) => error.to_string(),
    }
}
