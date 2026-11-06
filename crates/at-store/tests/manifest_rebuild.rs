//! TC-005 manifest rebuild acceptance tests in isolated temporary stores.
use std::fs;

use at_model::{ItemId, ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath, hash_skill_folder};
use at_store::manifest_rebuild::{apply_manifest_rebuild, plan_manifest_rebuild};
use at_store::store_io::{StoreAccess, load_store, save_store};
use serde_json::json;
use tempfile::TempDir;

fn store() -> (TempDir, StoreLayout) {
    let temp = tempfile::tempdir().expect("temporary root");
    let layout = StoreLayout::new(Scope::User, temp.path().join("store"));
    layout.init().expect("initialize store");
    (temp, layout)
}

fn skill(layout: &StoreLayout, folder: &str, name: &str) {
    let path = layout.skills_dir().join(folder);
    fs::create_dir_all(&path).expect("skill folder");
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: fixture\n---\n\n# {name}\n"),
    )
    .expect("skill file");
}

#[test]
fn rebuild_of_unchanged_store_preserves_item_id() {
    let (_temp, layout) = store();
    skill(&layout, "alpha", "Alpha");
    let loaded = load_store(&layout, StoreAccess::ReadWrite).expect("load");
    let id = ItemId::new();
    let hash = hash_skill_folder(&layout.skills_dir().join("alpha")).expect("hash");
    let mut manifest = StoreManifest::from_header(&loaded.manifest.header());
    manifest
        .insert_item(
            ManifestItem::new(
                id,
                ItemKind::Skill,
                Slug::parse("alpha").expect("slug"),
                StorePath::parse("skills/alpha").expect("path"),
                hash,
            )
            .with_name("Alpha"),
        )
        .expect("insert");
    save_store(
        &layout,
        &manifest,
        &loaded.manifest_hash,
        StoreAccess::ReadWrite,
        at_fs::BackupPolicy::None,
    )
    .expect("save");

    let plan = plan_manifest_rebuild(&layout).expect("plan rebuild");
    assert_eq!(plan.report().matched, 1);
    let token = plan.token().clone();
    let report = apply_manifest_rebuild(&layout, &plan, &token).expect("apply rebuild");
    assert_eq!(report.matched, 1);
    let rebuilt = load_store(&layout, StoreAccess::ReadWrite).expect("rebuilt load");
    assert_eq!(rebuilt.manifest.schema_version(), 1);
    assert_eq!(rebuilt.manifest.items()[0].id, id);
}

#[test]
fn rebuild_creates_loadable_manifest_when_store_json_is_missing() {
    let (_temp, layout) = store();
    skill(&layout, "alpha", "alpha");
    fs::write(layout.rules_dir().join("guide.md"), "# Guide\n").expect("rule");
    fs::write(
        layout.mcp_file(),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "servers": {
                "sample": {
                    "display_name": "Sample",
                    "description": null,
                    "transport": {
                        "type": "stdio",
                        "command": "tool",
                        "args": [],
                        "cwd": null,
                        "env": {}
                    }
                }
            }
        }))
        .expect("MCP serialization"),
    )
    .expect("MCP file");
    fs::remove_file(layout.manifest_path()).expect("delete manifest");
    let plan = plan_manifest_rebuild(&layout).expect("plan missing manifest");
    assert_eq!(plan.report().added, 3);
    let token = plan.token().clone();
    apply_manifest_rebuild(&layout, &plan, &token).expect("apply missing manifest rebuild");
    let rebuilt = load_store(&layout, StoreAccess::ReadWrite).expect("load reconstructed manifest");
    assert_eq!(rebuilt.manifest.schema_version(), 1);
    assert_eq!(rebuilt.items.len(), 3);
}

#[test]
fn corrupt_manifest_is_backed_up_and_duplicate_name_loser_stays_orphaned() {
    let (_temp, layout) = store();
    skill(&layout, "alpha", "shared");
    skill(&layout, "zeta", "shared");
    assert_eq!(
        at_store::parse_skill_doc(
            &fs::read(layout.skills_dir().join("alpha/SKILL.md")).expect("skill bytes")
        )
        .expect("valid skill doc")
        .name,
        "shared"
    );
    fs::write(layout.manifest_path(), b"{corrupt bytes").expect("corrupt manifest");

    let plan = plan_manifest_rebuild(&layout).expect("plan");
    assert_eq!(plan.report().skipped, 1);
    assert_eq!(plan.report().skipped_entries[0].path, "skills/zeta");
    let token = plan.token().clone();
    apply_manifest_rebuild(&layout, &plan, &token).expect("apply");
    let rebuilt = load_store(&layout, StoreAccess::ReadWrite).expect("load rebuilt manifest");
    assert_eq!(rebuilt.manifest.items().len(), 1);
    assert!(rebuilt.orphans.iter().any(|orphan| orphan.slug == "zeta"));
    let backed_up_bytes = fs::read_dir(layout.backups_dir())
        .expect("backup directory")
        .flatten()
        .filter_map(|entry| fs::read(entry.path()).ok())
        .any(|bytes| bytes == b"{corrupt bytes");
    assert!(backed_up_bytes, "corrupt bytes remain recoverable");
}

#[test]
fn rebuild_skips_missing_skill_markdown_without_touching_content() {
    let (_temp, layout) = store();
    fs::create_dir_all(layout.skills_dir().join("broken")).expect("folder");
    let plan = plan_manifest_rebuild(&layout).expect("plan");
    assert_eq!(plan.report().skipped, 1);
    assert_eq!(plan.report().skipped_entries[0].reason, "missing SKILL.md");
}
