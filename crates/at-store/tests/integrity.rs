//! TC-005 store-integrity acceptance tests in isolated temporary stores.
use std::fs;

use at_fs::BackupPolicy;
use at_model::{ItemId, ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath, hash_skill_folder};
use at_store::store_io::{StoreAccess, load_store, save_store};
use at_store::verify_store;

#[test]
fn verify_reports_hash_drift_orphans_and_duplicate_names() {
    let dir = tempfile::tempdir().expect("temp dir");
    let layout = StoreLayout::new(Scope::User, dir.path().join("store"));
    layout.init().expect("store init");
    let first = layout.skills_dir().join("alpha");
    let second = layout.skills_dir().join("beta");
    fs::create_dir_all(&first).expect("first dir");
    fs::create_dir_all(&second).expect("second dir");
    fs::write(
        first.join("SKILL.md"),
        "---\nname: shared\ndescription: fixture\n---\n",
    )
    .expect("first skill");
    fs::write(
        second.join("SKILL.md"),
        "---\nname: shared\ndescription: fixture\n---\n",
    )
    .expect("second skill");
    let loaded = load_store(&layout, StoreAccess::ReadWrite).expect("load initialized store");
    let mut manifest = StoreManifest::from_header(&loaded.manifest.header());
    for (slug, folder) in [("alpha", first.as_path()), ("beta", second.as_path())] {
        manifest
            .insert_item(ManifestItem::new(
                ItemId::new(),
                ItemKind::Skill,
                Slug::parse(slug).expect("slug"),
                StorePath::parse(&format!("skills/{slug}")).expect("path"),
                hash_skill_folder(folder).expect("skill hash"),
            ))
            .expect("insert");
    }
    save_store(
        &layout,
        &manifest,
        &loaded.manifest_hash,
        StoreAccess::ReadWrite,
        BackupPolicy::None,
    )
    .expect("save manifest");
    fs::write(first.join("asset.txt"), "modified").expect("modify skill");
    fs::create_dir_all(layout.skills_dir().join("orphan")).expect("orphan dir");
    fs::write(layout.skills_dir().join("orphan/SKILL.md"), "orphan").expect("orphan skill");

    let report = verify_store(&layout, StoreAccess::ReadWrite).expect("verify store");
    assert!(report.has_issues());
    assert_eq!(report.duplicate_names.len(), 1);
    assert_eq!(report.duplicate_names[0].paths.len(), 2);
    assert!(
        report
            .snapshot
            .modified()
            .any(|item| item.item.slug.as_str() == "alpha")
    );
    assert!(
        report
            .snapshot
            .orphans
            .iter()
            .any(|item| item.slug == "orphan")
    );
}
