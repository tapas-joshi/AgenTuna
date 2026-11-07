//! Generic operation undo integration coverage.
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use at_db::{Database, ops};
use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder};
use at_migrate::apply::{UndoOperationRequest, UndoOutcome, undo_operation};
use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind};
#[test]
fn undo_of_linked_directory_restores_original_tree_from_directory_backup() {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let store = root.join("store");
    let backups = root.join("backups");
    let native = root.join("native/skill");
    let canonical = store.join("skills/skill");
    fs::create_dir_all(native.join("assets")).expect("native tree");
    fs::create_dir_all(&canonical).expect("canonical tree");
    fs::create_dir_all(&backups).expect("backup root");
    fs::write(native.join("SKILL.md"), b"native skill\n").expect("native document");
    fs::write(native.join("assets/logo.svg"), b"<svg/>\n").expect("native asset");
    fs::write(canonical.join("SKILL.md"), b"native skill\n").expect("canonical document");
    let native_hash = at_fs::changeset::hash_node(&native)
        .expect("hash native tree")
        .expect("native tree exists");
    let database = Database::open_in_memory().expect("database");
    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: &store,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let set = ChangeSet {
        id: "replace-native".to_owned(),
        preview_id: "preview".to_owned(),
        operation_kind: OperationKind::parse("migrate").expect("operation kind"),
        scope: Some(at_model::Scope::User),
        project_id: None,
        summary: "Replace native skill".to_owned(),
        ops: vec![
            FileOp::Delete {
                path: native.clone(),
                expected: ExpectedState::hash(&native_hash).expect("expected native tree"),
            },
            FileOp::Symlink {
                link_path: native.clone(),
                target: canonical.clone(),
                mode: at_model::DeployMode::Link,
            },
        ],
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    };
    let roots = ApplyRoots::new(vec![root.clone()]).expect("authorized root");
    let applied = at_fs::apply_with_roots(&set, &mut context, &roots).expect("apply replacement");
    assert!(
        fs::symlink_metadata(&native)
            .expect("replacement link")
            .file_type()
            .is_symlink()
    );
    let detail = ops::operation_detail(database.connection(), &applied.operation_id)
        .expect("original operation history")
        .expect("original operation exists");
    let original_row = detail.files.first().expect("deleted directory row");
    let backup_id = original_row
        .backup_id
        .as_deref()
        .expect("directory backup id");
    let backup = at_db::backups::backup_by_id(database.connection(), backup_id)
        .expect("backup lookup")
        .expect("directory backup row");
    let backup_path = backup
        .backup_path
        .as_deref()
        .expect("directory backup path");
    assert!(
        std::path::Path::new(backup_path).is_dir(),
        "directory backup: {backup_path}"
    );

    let result = undo_operation(
        UndoOperationRequest {
            connection: database.connection(),
            store_root: &store,
            backup_root: &backups,
            authorized_roots: &roots,
            actor: ops::OperationActor::Ui,
            force: false,
        },
        &applied.operation_id,
    )
    .expect("undo replacement");
    assert!(matches!(result, UndoOutcome::Restored { .. }));
    assert_eq!(
        fs::read(native.join("SKILL.md")).expect("restored document"),
        b"native skill\n"
    );
    assert_eq!(
        fs::read(native.join("assets/logo.svg")).expect("restored asset"),
        b"<svg/>\n"
    );
    assert!(
        !fs::symlink_metadata(&native)
            .expect("restored real directory")
            .file_type()
            .is_symlink()
    );
}
use tempfile::TempDir;

#[test]
fn undo_of_created_file_removes_it_and_records_a_linked_undo_operation() {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let backups = root.join("backups");
    let store = root.join("store");
    fs::create_dir_all(&store).expect("store root");
    fs::create_dir_all(&backups).expect("backup root");
    let target = root.join("native/item.txt");
    let database = Database::open_in_memory().expect("database");
    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: &store,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let change_set = ChangeSet {
        id: "create-file".to_owned(),
        preview_id: "preview".to_owned(),
        operation_kind: OperationKind::parse("migrate").expect("operation kind"),
        scope: Some(at_model::Scope::User),
        project_id: None,
        summary: "Create fixture file".to_owned(),
        ops: vec![FileOp::Write {
            path: target.clone(),
            bytes: b"created by operation".to_vec(),
            expected: ExpectedState::Absent,
            follow_symlink: false,
            backup: true,
        }],
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    };
    let roots = ApplyRoots::new(vec![root.clone()]).expect("authorized root");
    let applied =
        at_fs::apply_with_roots(&change_set, &mut context, &roots).expect("apply operation");
    assert_eq!(
        fs::read(&target).expect("created file"),
        b"created by operation"
    );

    let outcome = undo_operation(
        UndoOperationRequest {
            connection: database.connection(),
            store_root: &store,
            backup_root: &backups,
            authorized_roots: &roots,
            actor: ops::OperationActor::Ui,
            force: false,
        },
        &applied.operation_id,
    )
    .expect("undo operation");
    let undo_id = match outcome {
        UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
        other => panic!("expected completed undo, got {other:?}"),
    };
    assert!(!target.exists());
    let undo = ops::operation_detail(database.connection(), &undo_id)
        .expect("undo history")
        .expect("undo operation row");
    assert_eq!(
        undo.operation.undo_of.as_deref(),
        Some(applied.operation_id.as_str())
    );
}

// ---------------------------------------------------------------------------
// TC-010: every operation kind, migrate metadata cleanup, window, idempotency.
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use at_migrate::apply::{ApplyPlanContext, apply_plan};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{BuildPlanRequest, MigrationStrategy, build_plan};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};
use at_model::{AgentId, ItemId, ItemKind, Scope};

struct MigrateFixture {
    _temp: TempDir,
    root: PathBuf,
    store: PathBuf,
    backups: PathBuf,
    native: PathBuf,
    database: Database,
    native_hash: String,
    store_manifest_bytes: Vec<u8>,
}

fn migrate_fixture() -> MigrateFixture {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let store = root.join("store");
    at_store::layout::init_store(Scope::User, &store).expect("initialised store");
    let backups = root.join("backups");
    fs::create_dir_all(&backups).expect("backup root");
    let native = root.join("native/skills/sample-skill");
    fs::create_dir_all(&native).expect("native tree");
    fs::write(native.join("SKILL.md"), b"# Sample\nbody\n").expect("native document");
    let native_hash = at_fs::changeset::hash_node(&native)
        .expect("hash native tree")
        .expect("native tree");
    let store_manifest_bytes = fs::read(store.join("store.json")).expect("manifest bytes");
    let database = Database::open_in_memory().expect("database");
    MigrateFixture {
        _temp: temp,
        root,
        store,
        backups,
        native,
        database,
        native_hash,
        store_manifest_bytes,
    }
}

fn migrate_plan(fixture: &MigrateFixture) -> at_migrate::plan::MigrationPlan {
    plan_for(
        fixture,
        vec![skill_item(
            &fixture.native,
            "sample-skill",
            &[AgentId::ClaudeCode],
        )],
        &BTreeSet::new(),
    )
}

fn apply_migrate(fixture: &mut MigrateFixture) -> at_migrate::apply::MigrationApplyOutcome {
    let plan = migrate_plan(fixture);
    apply_plan_for(fixture, &plan)
}

fn undo_request<'a>(
    fixture: &'a MigrateFixture,
    roots: &'a ApplyRoots,
) -> UndoOperationRequest<'a> {
    UndoOperationRequest {
        connection: fixture.database.connection(),
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: roots,
        actor: ops::OperationActor::Ui,
        force: false,
    }
}

fn tree_hash(path: &Path) -> String {
    at_fs::changeset::hash_node(path)
        .expect("hash tree")
        .expect("tree exists")
}

fn skill_item(path: &Path, name: &str, agents: &[AgentId]) -> ScanItem {
    let content_hash = at_fs::changeset::hash_node(path)
        .expect("content hash")
        .expect("native skill exists");
    let normalized_hash = at_store::manifest::hash_skill_folder(path)
        .expect("normalized hash")
        .to_string();
    ScanItem {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        agents: agents.iter().cloned().collect(),
        path: path.to_path_buf(),
        scope: Scope::User,
        content_hash: Some(content_hash),
        normalized_hash: Some(normalized_hash),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    }
}

fn plan_for(
    fixture: &MigrateFixture,
    items: Vec<ScanItem>,
    copy_agents: &BTreeSet<AgentId>,
) -> at_migrate::plan::MigrationPlan {
    let selected_paths = items
        .iter()
        .map(|item| item.path.clone())
        .collect::<BTreeSet<_>>();
    let scan = ScanReport {
        items,
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts(&scan);
    build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected_paths,
        store_root: fixture.store.clone(),
        copy_agents,
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: fixture.backups.clone(),
    })
    .expect("migration plan")
}

/// [`plan_for`] for one receiving scope and store, so a test can plan a multi-scope selection.
fn plan_for_scope(
    fixture: &MigrateFixture,
    items: Vec<ScanItem>,
    scope: Scope,
    store_root: &Path,
) -> at_migrate::plan::MigrationPlan {
    let selected_paths = items
        .iter()
        .map(|item| item.path.clone())
        .collect::<BTreeSet<_>>();
    let scan = ScanReport {
        items,
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts(&scan);
    build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected_paths,
        store_root: store_root.to_path_buf(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope,
        backup_root: fixture.backups.clone(),
    })
    .expect("migration plan")
}

fn apply_plan_for(
    fixture: &mut MigrateFixture,
    plan: &at_migrate::plan::MigrationPlan,
) -> at_migrate::apply::MigrationApplyOutcome {
    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let mut context = ApplyPlanContext {
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &roots,
        database: &mut fixture.database,
        project_id: None,
    };
    apply_plan(plan, &mut context).expect("apply migration")
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .expect("deployed path")
        .file_type()
        .is_symlink()
}

#[test]
fn undo_of_a_migrate_operation_restores_originals_and_removes_metadata() {
    let mut fixture = migrate_fixture();
    let applied = apply_migrate(&mut fixture);
    let store_item = fixture.store.join("skills/sample-skill");
    assert!(store_item.join("SKILL.md").is_file(), "store item created");
    assert!(
        fs::symlink_metadata(&fixture.native)
            .expect("link")
            .file_type()
            .is_symlink(),
        "original replaced by a link"
    );
    let item_id = at_db::items::item_id_by_identity(
        fixture.database.connection(),
        ItemKind::Skill,
        Scope::User,
        None,
        "sample-skill",
    )
    .expect("item lookup")
    .expect("item row written");
    assert_eq!(
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments")
            .len(),
        1
    );
    let native_text = fixture.native.to_string_lossy().into_owned();
    assert_eq!(
        at_db::native::native_item_by_path(fixture.database.connection(), &native_text)
            .expect("native lookup")
            .expect("native row")
            .status,
        at_db::native::NativeStatus::Adopted
    );

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo migration");
    let undo_id = match outcome {
        UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
        other => panic!("expected completed undo, got {other:?}"),
    };

    // Files: the original tree is back, the store item and its link are gone.
    assert_eq!(tree_hash(&fixture.native), fixture.native_hash);
    assert!(
        !fs::symlink_metadata(&fixture.native)
            .expect("restored tree")
            .file_type()
            .is_symlink()
    );
    assert!(!store_item.exists(), "store item removed");
    // The manifest is merged, not restored byte-for-byte (a later install may share the file):
    // the migrated record is gone and nothing else reappeared.
    let manifest = at_store::manifest::StoreManifest::load(&at_store::layout::StoreLayout::new(
        Scope::User,
        &fixture.store,
    ))
    .expect("manifest")
    .manifest;
    assert!(
        manifest
            .item_by_slug(
                ItemKind::Skill,
                &at_store::layout::Slug::parse("sample-skill").expect("slug")
            )
            .is_none(),
        "the migrated item record is removed"
    );
    let before =
        at_store::manifest::StoreManifest::parse(&fixture.store_manifest_bytes).expect("before");
    assert_eq!(
        manifest
            .items()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        before
            .items()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        "the merged manifest holds exactly the pre-migration records"
    );

    // Metadata: deployments, native adoption and the item row are gone.
    assert!(
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments")
            .is_empty()
    );
    let native = at_db::native::native_item_by_path(fixture.database.connection(), &native_text)
        .expect("native lookup")
        .expect("native row survives");
    assert_eq!(native.status, at_db::native::NativeStatus::New);
    assert!(native.adopted_item_id.is_none());
    assert!(
        at_db::items::item_id_by_identity(
            fixture.database.connection(),
            ItemKind::Skill,
            Scope::User,
            None,
            "sample-skill"
        )
        .expect("item lookup")
        .is_none(),
        "item row removed with the store files"
    );

    // The undo is a single Undo record linked to the original.
    let undo = ops::operation_detail(fixture.database.connection(), &undo_id)
        .expect("undo history")
        .expect("undo operation row");
    assert_eq!(undo.operation.kind, Some(ops::OperationKind::Undo));
    assert_eq!(
        undo.operation.undo_of.as_deref(),
        Some(applied.operation_id.as_str())
    );
    assert!(!undo.files.is_empty());

    // A second undo refuses.
    assert_eq!(
        undo_operation(undo_request(&fixture, &roots), &applied.operation_id).expect("second undo"),
        UndoOutcome::AlreadyUndone
    );

    // The first undo is itself undoable: the migrated state comes back.
    let redo = undo_operation(undo_request(&fixture, &roots), &undo_id).expect("undo the undo");
    assert!(matches!(redo, UndoOutcome::Restored { .. }));
    assert_eq!(tree_hash(&store_item), fixture.native_hash);
    assert!(
        fs::symlink_metadata(&fixture.native)
            .expect("link again")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn undo_of_migration_with_sibling_skills_restores_each_native_tree() {
    let mut fixture = migrate_fixture();
    let second = fixture
        .native
        .parent()
        .expect("native skills parent")
        .join("second-skill");
    fs::create_dir_all(&second).expect("second native tree");
    fs::write(second.join("SKILL.md"), b"# Secondary\nbody\n").expect("second document");
    let originals = [
        (fixture.native.clone(), b"# Sample\nbody\n".to_vec()),
        (second.clone(), b"# Secondary\nbody\n".to_vec()),
    ];
    let items = originals
        .iter()
        .map(|(path, _)| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("skill name")
                .to_owned();
            let content_hash = at_fs::changeset::hash_node(path)
                .expect("content hash")
                .expect("native skill exists");
            let normalized_hash = at_store::manifest::hash_skill_folder(path)
                .expect("normalized hash")
                .to_string();
            ScanItem {
                kind: ItemKind::Skill,
                name,
                agents: BTreeSet::from([AgentId::ClaudeCode]),
                path: path.clone(),
                scope: Scope::User,
                content_hash: Some(content_hash),
                normalized_hash: Some(normalized_hash),
                parse_status: ParseStatus::Parsed,
                store_link: false,
                ownership: Ownership::NotInAgenTuna,
            }
        })
        .collect::<Vec<_>>();
    let scan = ScanReport {
        items,
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts(&scan);
    let selected_paths = originals
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    let plan = build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected_paths,
        store_root: fixture.store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: fixture.backups.clone(),
    })
    .expect("migration plan");
    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let applied = {
        let mut context = ApplyPlanContext {
            store_root: &fixture.store,
            backup_root: &fixture.backups,
            authorized_roots: &roots,
            database: &mut fixture.database,
            project_id: None,
        };
        apply_plan(&plan, &mut context).expect("apply migration")
    };

    let store_items = [
        fixture.store.join("skills/sample-skill"),
        fixture.store.join("skills/second-skill"),
    ];
    for ((native, _), store_item) in originals.iter().zip(&store_items) {
        assert!(store_item.join("SKILL.md").is_file());
        assert!(
            fs::symlink_metadata(native)
                .expect("migrated native link")
                .file_type()
                .is_symlink()
        );
    }
    let result = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo migration");
    assert!(matches!(result, UndoOutcome::Restored { .. }));

    for ((native, bytes), store_item) in originals.iter().zip(&store_items) {
        assert_eq!(
            fs::read(native.join("SKILL.md")).expect("restored document"),
            *bytes
        );
        assert!(
            !fs::symlink_metadata(native)
                .expect("restored native directory")
                .file_type()
                .is_symlink()
        );
        assert!(!store_item.exists(), "store item removed");
    }
    let merged = at_store::manifest::StoreManifest::load(&at_store::layout::StoreLayout::new(
        Scope::User,
        &fixture.store,
    ))
    .expect("manifest")
    .manifest;
    let before = at_store::manifest::StoreManifest::parse(&fixture.store_manifest_bytes)
        .expect("pre-migration manifest");
    assert_eq!(
        merged
            .items()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        before
            .items()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        "the merged manifest holds exactly the pre-migration records"
    );
}

#[test]
fn undo_of_a_copy_strategy_migration_restores_the_original_and_removes_metadata() {
    let mut fixture = migrate_fixture();
    let copies = BTreeSet::from([AgentId::ClaudeCode]);
    let plan = plan_for(
        &fixture,
        vec![skill_item(
            &fixture.native,
            "sample-skill",
            &[AgentId::ClaudeCode],
        )],
        &copies,
    );
    let applied = apply_plan_for(&mut fixture, &plan);
    let store_item = fixture.store.join("skills/sample-skill");
    assert!(store_item.join("SKILL.md").is_file(), "store item created");
    assert!(
        !is_symlink(&fixture.native),
        "a copy deployment leaves a real directory at the native path"
    );
    assert_eq!(
        fs::read(fixture.native.join("SKILL.md")).expect("copied document"),
        b"# Sample\nbody\n",
        "the copy holds the original content (plus its copy manifest)"
    );
    let item_id = at_db::items::item_id_by_identity(
        fixture.database.connection(),
        ItemKind::Skill,
        Scope::User,
        None,
        "sample-skill",
    )
    .expect("item lookup")
    .expect("item row written");
    let deployments =
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments");
    assert_eq!(deployments.len(), 1, "one deployment row");
    assert_eq!(
        deployments[0].link_mode,
        Some(at_model::LinkMode::Copy),
        "the plan deployed a copy, not a link"
    );

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let readiness =
        at_migrate::undo::undo_readiness(fixture.database.connection(), &applied.operation_id)
            .expect("undo readiness");
    assert!(readiness.known, "the operation is known");
    assert!(!readiness.already_undone && !readiness.expired);
    assert!(
        readiness.conflicted_paths.is_empty(),
        "the check must agree with the undo on a deployment the operation owns: {readiness:?}"
    );
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo migration");
    let restored = matches!(outcome, UndoOutcome::Restored { .. });
    assert!(restored, "expected a completed undo, got {outcome:?}");

    assert_eq!(
        tree_hash(&fixture.native),
        fixture.native_hash,
        "the original tree is back byte-identically"
    );
    assert!(
        !is_symlink(&fixture.native),
        "the original directory is back"
    );
    assert!(!store_item.exists(), "store item removed");
    assert!(
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments")
            .is_empty(),
        "the copy deployment row is gone with the store item"
    );
    assert!(
        at_db::items::item_id_by_identity(
            fixture.database.connection(),
            ItemKind::Skill,
            Scope::User,
            None,
            "sample-skill",
        )
        .expect("item lookup")
        .is_none(),
        "item row removed with the store files"
    );
}

#[test]
fn undo_check_and_undo_agree_when_the_original_bytes_are_already_back() {
    let mut fixture = migrate_fixture();
    let applied = apply_migrate(&mut fixture);
    let store_item = fixture.store.join("skills/sample-skill");
    // The user replaces the deployment link with their own copy of the original bytes.
    fs::remove_file(&fixture.native).expect("remove the deployment link");
    fs::create_dir_all(&fixture.native).expect("native directory");
    fs::write(fixture.native.join("SKILL.md"), b"# Sample\nbody\n").expect("restored document");
    assert_eq!(tree_hash(&fixture.native), fixture.native_hash);

    let readiness =
        at_migrate::undo::undo_readiness(fixture.database.connection(), &applied.operation_id)
            .expect("undo readiness");
    assert!(
        readiness.conflicted_paths.is_empty(),
        "the end state already holds, so the check reports no conflict: {readiness:?}"
    );

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo migration");
    let restored = matches!(outcome, UndoOutcome::Restored { .. });
    assert!(restored, "expected a completed undo, got {outcome:?}");
    assert_eq!(
        tree_hash(&fixture.native),
        fixture.native_hash,
        "the restored bytes stay as they are"
    );
    assert_eq!(
        fs::read(fixture.native.join("SKILL.md")).expect("kept document"),
        b"# Sample\nbody\n"
    );
    assert!(!store_item.exists(), "the store item still goes");
}

#[test]
fn undo_of_a_nested_directory_skill_restores_every_nested_file() {
    let mut fixture = migrate_fixture();
    fs::create_dir_all(fixture.native.join("assets/icons")).expect("nested directory");
    fs::write(fixture.native.join("assets/icons/logo.svg"), b"<svg/>\n").expect("nested asset");
    fs::create_dir_all(fixture.native.join("docs")).expect("docs directory");
    fs::write(fixture.native.join("docs/guide.md"), b"# Guide\n").expect("nested document");
    fs::create_dir_all(fixture.native.join("empty")).expect("empty directory");
    let native_hash = tree_hash(&fixture.native);
    let plan = plan_for(
        &fixture,
        vec![skill_item(
            &fixture.native,
            "sample-skill",
            &[AgentId::ClaudeCode],
        )],
        &BTreeSet::new(),
    );
    let applied = apply_plan_for(&mut fixture, &plan);
    assert!(
        is_symlink(&fixture.native),
        "the nested tree is replaced by a link"
    );

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo migration");
    let restored = matches!(outcome, UndoOutcome::Restored { .. });
    assert!(restored, "expected a completed undo, got {outcome:?}");

    assert_eq!(
        tree_hash(&fixture.native),
        native_hash,
        "the restored tree is byte-identical, nested entries included"
    );
    assert_eq!(
        fs::read(fixture.native.join("assets/icons/logo.svg")).expect("restored asset"),
        b"<svg/>\n"
    );
    assert_eq!(
        fs::read(fixture.native.join("docs/guide.md")).expect("restored document"),
        b"# Guide\n"
    );
    assert!(
        fixture.native.join("empty").is_dir(),
        "empty nested directories are restored too"
    );
}

#[test]
fn undo_of_a_mixed_link_and_copy_migration_restores_each_native_tree() {
    let mut fixture = migrate_fixture();
    let second = fixture
        .native
        .parent()
        .expect("native skills parent")
        .join("second-skill");
    fs::create_dir_all(&second).expect("second native tree");
    fs::write(second.join("SKILL.md"), b"# Secondary\nbody\n").expect("second document");
    let first_hash = tree_hash(&fixture.native);
    let second_hash = tree_hash(&second);
    // Only Claude Code gets a copy; an item also deployed to Codex must stay a link.
    let copies = BTreeSet::from([AgentId::ClaudeCode]);
    let plan = plan_for(
        &fixture,
        vec![
            skill_item(&fixture.native, "sample-skill", &[AgentId::ClaudeCode]),
            skill_item(
                &second,
                "second-skill",
                &[AgentId::ClaudeCode, AgentId::Codex],
            ),
        ],
        &copies,
    );
    let applied = apply_plan_for(&mut fixture, &plan);
    assert!(
        !is_symlink(&fixture.native),
        "a copy-only item stays a real directory"
    );
    assert!(
        is_symlink(&second),
        "an item shared with another agent stays a link"
    );

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo mixed migration");
    let restored = matches!(outcome, UndoOutcome::Restored { .. });
    assert!(restored, "expected a completed undo, got {outcome:?}");

    assert_eq!(
        tree_hash(&fixture.native),
        first_hash,
        "the copied item's original is back byte-identically"
    );
    assert_eq!(
        tree_hash(&second),
        second_hash,
        "the linked item's original is back byte-identically"
    );
    assert!(
        !is_symlink(&fixture.native) && !is_symlink(&second),
        "both native paths are plain directories again"
    );
    assert!(!fixture.store.join("skills/sample-skill").exists());
    assert!(!fixture.store.join("skills/second-skill").exists());
}

#[test]
fn undo_refuses_a_foreign_directory_at_the_original_path_without_writing() {
    let mut fixture = migrate_fixture();
    let applied = apply_migrate(&mut fixture);
    let store_item = fixture.store.join("skills/sample-skill");
    let store_hash = tree_hash(&store_item);
    let item_id = at_db::items::item_id_by_identity(
        fixture.database.connection(),
        ItemKind::Skill,
        Scope::User,
        None,
        "sample-skill",
    )
    .expect("item lookup")
    .expect("item row written");
    // The user removes the deployment link and writes their own tree there.
    fs::remove_file(&fixture.native).expect("remove the deployment link");
    fs::create_dir_all(fixture.native.join("mine")).expect("user directory");
    fs::write(fixture.native.join("SKILL.md"), b"# User rewrite\n").expect("user document");
    let foreign_hash = tree_hash(&fixture.native);

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo attempt");
    match outcome {
        UndoOutcome::Conflict { paths } => assert_eq!(
            paths,
            vec![fixture.native.clone()],
            "the refusal names the foreign path"
        ),
        other => panic!("expected a conflict refusal, got {other:?}"),
    }

    assert_eq!(
        tree_hash(&fixture.native),
        foreign_hash,
        "the foreign tree is untouched"
    );
    assert_eq!(
        fs::read(fixture.native.join("SKILL.md")).expect("user document kept"),
        b"# User rewrite\n"
    );
    assert!(fixture.native.join("mine").is_dir());
    assert_eq!(
        tree_hash(&store_item),
        store_hash,
        "the store item is untouched"
    );
    assert_eq!(
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments")
            .len(),
        1,
        "the deployment row survives a refused undo"
    );
    let native = at_db::native::native_item_by_path(
        fixture.database.connection(),
        &fixture.native.to_string_lossy(),
    )
    .expect("native lookup")
    .expect("native row survives");
    assert_eq!(native.status, at_db::native::NativeStatus::Adopted);
    assert_eq!(native.adopted_item_id, Some(item_id.to_string()));
    let undo_rows: i64 = fixture
        .database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE undo_of = ?1",
            [applied.operation_id.as_str()],
            |row| row.get(0),
        )
        .expect("undo history");
    assert_eq!(undo_rows, 0, "a refused undo writes no operation row");
}

#[test]
fn failed_undo_is_not_reported_as_already_undone_and_can_be_retried() {
    struct FailChangeSetOperation;
    impl at_fs::failpoint::FailInjector for FailChangeSetOperation {
        fn fault(
            &self,
            point: at_fs::failpoint::FaultPoint,
            _: &Path,
        ) -> Option<at_fs::failpoint::Fault> {
            (point == at_fs::failpoint::FaultPoint::ChangeSetOperation)
                .then_some(at_fs::failpoint::Fault::Fail(5))
        }
    }

    let mut fixture = migrate_fixture();
    let applied = apply_migrate(&mut fixture);
    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let error =
        at_fs::failpoint::with_injector(std::sync::Arc::new(FailChangeSetOperation), || {
            undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        })
        .expect_err("injected undo failure");
    assert!(error.to_string().contains("ChangeSet operation 0"));
    let readiness =
        at_migrate::undo::undo_readiness(fixture.database.connection(), &applied.operation_id)
            .expect("undo readiness");
    assert!(!readiness.already_undone);

    let retry = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("retry failed undo");
    assert!(matches!(retry, UndoOutcome::Restored { .. }));
    assert_eq!(tree_hash(&fixture.native), fixture.native_hash);
}

#[test]
fn undo_of_a_drift_adopt_or_sync_push_restores_recorded_bytes_with_its_own_backup() {
    for kind in ["drift_adopt", "sync_push"] {
        let temp = TempDir::new().expect("temporary root");
        let root = fs::canonicalize(temp.path()).expect("canonical root");
        let store = root.join("store");
        let backups = root.join("backups");
        fs::create_dir_all(&store).expect("store root");
        fs::create_dir_all(&backups).expect("backup root");
        let target = store.join("rules/team-rules.md");
        fs::create_dir_all(target.parent().expect("parent")).expect("rules dir");
        fs::write(&target, b"before drift\n").expect("seed rule");
        let database = Database::open_in_memory().expect("database");
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let mut context = ApplyContext {
            store_root: &store,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let set = ChangeSet {
            id: format!("{kind}-operation"),
            preview_id: "preview".to_owned(),
            operation_kind: OperationKind::parse(kind).expect("operation kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: format!("{kind} wrote the rule"),
            ops: vec![FileOp::Write {
                path: target.clone(),
                bytes: b"adopted external bytes\n".to_vec(),
                expected: ExpectedState::hash(at_fs::sha256_hex(b"before drift\n"))
                    .expect("expected hash"),
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        };
        let roots = ApplyRoots::new([root.clone()]).expect("authorized root");
        let applied = at_fs::apply_with_roots(&set, &mut context, &roots).expect("apply operation");
        assert_eq!(
            fs::read(&target).expect("written bytes"),
            b"adopted external bytes\n"
        );

        let outcome = undo_operation(
            UndoOperationRequest {
                connection: database.connection(),
                store_root: &store,
                backup_root: &backups,
                authorized_roots: &roots,
                actor: ops::OperationActor::Cli,
                force: false,
            },
            &applied.operation_id,
        )
        .expect("undo operation");
        let undo_id = match outcome {
            UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
            other => panic!("expected completed undo for {kind}, got {other:?}"),
        };
        assert_eq!(
            fs::read(&target).expect("restored bytes"),
            b"before drift\n",
            "{kind} restores the recorded bytes"
        );
        let own_backups = at_db::backups::backups_for_operation(database.connection(), &undo_id)
            .expect("undo backups");
        assert_eq!(own_backups.len(), 1, "{kind} undo creates its own backup");
        let blob = own_backups[0].backup_path.as_deref().expect("backup path");
        assert_eq!(
            fs::read(blob).expect("backup bytes"),
            b"adopted external bytes\n",
            "{kind} undo backs up the bytes it replaces"
        );
    }
}

#[test]
fn undo_window_is_thirty_days_evaluated_in_utc() {
    for (age_days, expected) in [(29_u64, None), (31_u64, Some(UndoOutcome::Expired))] {
        let temp = TempDir::new().expect("temporary root");
        let root = fs::canonicalize(temp.path()).expect("canonical root");
        let store = root.join("store");
        let backups = root.join("backups");
        fs::create_dir_all(&store).expect("store root");
        fs::create_dir_all(&backups).expect("backup root");
        let target = root.join("native/item.txt");
        let database = Database::open_in_memory().expect("database");
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let mut context = ApplyContext {
            store_root: &store,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let set = ChangeSet {
            id: format!("window-{age_days}"),
            preview_id: "preview".to_owned(),
            operation_kind: OperationKind::parse("deploy").expect("operation kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: "window fixture".to_owned(),
            ops: vec![FileOp::Write {
                path: target.clone(),
                bytes: b"deployed\n".to_vec(),
                expected: ExpectedState::Absent,
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        };
        let roots = ApplyRoots::new([root.clone()]).expect("authorized root");
        let applied = at_fs::apply_with_roots(&set, &mut context, &roots).expect("apply operation");
        database
            .connection()
            .execute(
                "UPDATE operations SET started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2) \
                  WHERE id = ?1",
                rusqlite::params![applied.operation_id, format!("-{age_days} days")],
            )
            .expect("age the operation");
        let outcome = undo_operation(
            UndoOperationRequest {
                connection: database.connection(),
                store_root: &store,
                backup_root: &backups,
                authorized_roots: &roots,
                actor: ops::OperationActor::Ui,
                force: false,
            },
            &applied.operation_id,
        )
        .expect("undo window check");
        match expected {
            None => {
                assert!(
                    matches!(outcome, UndoOutcome::Restored { .. }),
                    "day {age_days} is inside the window, got {outcome:?}"
                );
                assert!(!target.exists(), "day {age_days} undo removed the file");
            }
            Some(expected) => {
                assert_eq!(outcome, expected, "day {age_days} is outside the window");
                assert_eq!(
                    fs::read(&target).expect("file kept"),
                    b"deployed\n",
                    "an expired undo writes nothing"
                );
            }
        }
    }
}

#[test]
fn undo_of_an_operation_without_file_rows_is_recorded_and_idempotent() {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let store = root.join("store");
    let backups = root.join("backups");
    fs::create_dir_all(&store).expect("store root");
    fs::create_dir_all(&backups).expect("backup root");
    let database = Database::open_in_memory().expect("database");
    let operation = ops::record_completed(
        database.connection(),
        ops::OperationKind::Scan,
        ops::OperationActor::Cli,
        Some("scanned agents"),
    )
    .expect("record scan operation");
    let roots = ApplyRoots::new([root.clone()]).expect("authorized root");
    let outcome = undo_operation(
        UndoOperationRequest {
            connection: database.connection(),
            store_root: &store,
            backup_root: &backups,
            authorized_roots: &roots,
            actor: ops::OperationActor::Ui,
            force: false,
        },
        operation.as_str(),
    )
    .expect("undo scan");
    let undo_id = match outcome {
        UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
        other => panic!("expected recorded undo, got {other:?}"),
    };
    let undo = ops::operation_detail(database.connection(), &undo_id)
        .expect("undo history")
        .expect("undo row");
    assert_eq!(undo.operation.kind, Some(ops::OperationKind::Undo));
    assert!(undo.files.is_empty());
    assert_eq!(
        undo_operation(
            UndoOperationRequest {
                connection: database.connection(),
                store_root: &store,
                backup_root: &backups,
                authorized_roots: &roots,
                actor: ops::OperationActor::Ui,
                force: false,
            },
            operation.as_str(),
        )
        .expect("second undo"),
        UndoOutcome::AlreadyUndone
    );
    let _ = ItemId::new();
}

#[test]
fn undo_of_a_migrate_operation_keeps_an_item_installed_afterwards() {
    let mut fixture = migrate_fixture();
    let applied = apply_migrate(&mut fixture);
    let layout = at_store::layout::StoreLayout::new(Scope::User, &fixture.store);
    let later_dir = fixture.store.join("skills/later-skill");
    fs::create_dir_all(&later_dir).expect("later skill folder");
    fs::write(later_dir.join("SKILL.md"), b"# later\n").expect("later skill document");
    let later_hash = at_store::manifest::hash_skill_folder(&later_dir)
        .expect("later hash")
        .to_string();
    let later_id = ItemId::new();
    let mut manifest = at_store::manifest::StoreManifest::load(&layout)
        .expect("manifest")
        .manifest;
    manifest
        .insert_item(
            at_store::manifest::ManifestItem::new(
                later_id,
                ItemKind::Skill,
                at_store::layout::Slug::parse("later-skill").expect("slug"),
                at_store::manifest::StorePath::parse("skills/later-skill").expect("path"),
                at_store::manifest::ContentHash::parse(&later_hash).expect("hash"),
            )
            .with_name("later-skill"),
        )
        .expect("record the later item");
    fs::write(
        layout.manifest_path(),
        manifest.to_bytes().expect("manifest bytes"),
    )
    .expect("write the shared manifest");

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo the migration");

    assert!(
        matches!(outcome, UndoOutcome::Restored { .. }),
        "a later install must not block the undo: {outcome:?}"
    );
    let merged = at_store::manifest::StoreManifest::load(&layout)
        .expect("merged manifest")
        .manifest;
    assert!(
        merged.item(later_id).is_some(),
        "the later install survives the undo"
    );
    assert!(
        merged
            .item_by_slug(
                ItemKind::Skill,
                &at_store::layout::Slug::parse("sample-skill").expect("slug")
            )
            .is_none(),
        "the migrated record is removed"
    );
    assert_eq!(
        fs::read(later_dir.join("SKILL.md")).expect("later skill content"),
        b"# later\n"
    );
    assert_eq!(tree_hash(&fixture.native), fixture.native_hash);
}

#[test]
fn undo_of_a_migrate_operation_recreates_the_tree_when_the_link_was_removed() {
    let mut fixture = migrate_fixture();
    let applied = apply_migrate(&mut fixture);
    fs::remove_file(&fixture.native).expect("the user removes the managed link");
    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");

    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo the migration");

    assert!(
        matches!(outcome, UndoOutcome::Restored { .. }),
        "a removed managed link must not be a stale target: {outcome:?}"
    );
    assert_eq!(
        tree_hash(&fixture.native),
        fixture.native_hash,
        "the original native tree comes back from the operation backup"
    );
    assert!(
        !fs::symlink_metadata(&fixture.native)
            .expect("restored tree")
            .file_type()
            .is_symlink(),
        "the link is not re-created"
    );
    assert!(
        !fixture.store.join("skills/sample-skill").exists(),
        "the store item is removed"
    );
}

/// A selection that spans scopes is applied as one operation that writes several stores' shared
/// documents. Undo has to merge each of them entry by entry, not restore the whole file from the
/// operation backup, or a later unrelated change in the other store is silently lost.
fn multi_scope_migration(fixture: &mut MigrateFixture) -> (PathBuf, PathBuf, String) {
    let global_store = fixture.root.join("global-store");
    at_store::layout::init_store(Scope::Global, &global_store).expect("global store");
    let global_native = fixture.root.join("native/global/other-skill");
    fs::create_dir_all(&global_native).expect("global native tree");
    fs::write(global_native.join("SKILL.md"), b"# Other\nbody\n").expect("global document");

    let user_plan = plan_for_scope(
        fixture,
        vec![skill_item(
            &fixture.native,
            "sample-skill",
            &[AgentId::ClaudeCode],
        )],
        Scope::User,
        &fixture.store,
    );
    let global_plan = plan_for_scope(
        fixture,
        vec![skill_item(
            &global_native,
            "other-skill",
            &[AgentId::ClaudeCode],
        )],
        Scope::Global,
        &global_store,
    );

    let mut stores = at_migrate::plan::ScopeStores::new();
    stores.insert(Scope::User, fixture.store.clone());
    stores.insert(Scope::Global, global_store.clone());
    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized roots");
    let mut context = at_migrate::apply::ApplyPlansContext {
        stores: &stores,
        backup_root: &fixture.backups,
        authorized_roots: &roots,
        database: &mut fixture.database,
        project_id: None,
    };
    let applied = at_migrate::apply::apply_plans(&[user_plan, global_plan], &mut context)
        .expect("multi-scope migration");
    (global_store, global_native, applied.operation_id)
}

/// Adds an unrelated item to a store manifest, the way a later `skills install` would.
fn add_later_manifest_entry(store: &Path) -> ItemId {
    let manifest_path = store.join("store.json");
    let mut manifest = at_store::manifest::StoreManifest::parse(
        &fs::read(&manifest_path).expect("manifest bytes"),
    )
    .expect("parse manifest");
    let later = ItemId::new();
    manifest
        .insert_item(at_store::manifest::ManifestItem::new(
            later,
            ItemKind::Skill,
            at_store::layout::Slug::parse("later-skill").expect("slug"),
            at_store::manifest::StorePath::parse("skills/later-skill").expect("store path"),
            at_store::ContentHash::parse(&at_fs::sha256_hex(b"later skill")).expect("content hash"),
        ))
        .expect("insert later entry");
    fs::write(
        &manifest_path,
        manifest.to_bytes().expect("manifest serializes"),
    )
    .expect("write manifest");
    later
}

fn manifest_has_slug(store: &Path, slug: &str) -> bool {
    let manifest = at_store::manifest::StoreManifest::parse(
        &fs::read(store.join("store.json")).expect("manifest bytes"),
    )
    .expect("parse manifest");
    manifest
        .items()
        .iter()
        .any(|item| item.slug.as_str() == slug)
}

#[test]
fn multi_scope_undo_keeps_a_later_entry_in_the_second_stores_manifest() {
    for force in [false, true] {
        let mut fixture = migrate_fixture();
        let (global_store, global_native, operation_id) = multi_scope_migration(&mut fixture);
        assert!(
            manifest_has_slug(&global_store, "other-skill"),
            "the migration wrote the Global manifest"
        );
        let later = add_later_manifest_entry(&global_store);
        let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");

        let outcome = undo_operation(
            UndoOperationRequest {
                connection: fixture.database.connection(),
                store_root: &fixture.store,
                backup_root: &fixture.backups,
                authorized_roots: &roots,
                actor: ops::OperationActor::Ui,
                force,
            },
            &operation_id,
        )
        .expect("undo the multi-scope migration");

        assert!(
            matches!(outcome, UndoOutcome::Restored { .. }),
            "force={force}: a shared document another command changed is merged, not refused: {outcome:?}"
        );
        let manifest = at_store::manifest::StoreManifest::parse(
            &fs::read(global_store.join("store.json")).expect("Global manifest bytes"),
        )
        .expect("parse Global manifest");
        assert!(
            manifest.item(later).is_some(),
            "force={force}: the later, unrelated Global entry survives the undo"
        );
        assert!(
            !manifest
                .items()
                .iter()
                .any(|item| item.slug.as_str() == "other-skill"),
            "force={force}: the migrated Global item is reverted"
        );
        assert!(
            !fs::symlink_metadata(&global_native)
                .expect("restored native tree")
                .file_type()
                .is_symlink(),
            "force={force}: the migrated Global skill is restored from its backup"
        );
    }
}
