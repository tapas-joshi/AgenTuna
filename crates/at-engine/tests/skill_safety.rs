//! TS-044: bulk, promote, move and delete safety against a canonical temporary home.
//!
//! Every store, agent folder and backup directory lives under one canonical temporary
//! root, so no test reads or writes the real home. Filesystem trees are compared with a
//! recursive content snapshot (symlinks by target) and the database is inspected for the
//! operation, `operation_files` and `backups` rows the write pipeline must produce.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use at_db::Database;
use at_db::deployments::{DeploymentSpec, list_deployments_for_item, upsert_deployment};
use at_engine::bulk_skill::{
    BulkApplyError, BulkIntent, BulkItemPlan, BulkKind, BulkProgressStage, apply_bulk, plan_bulk,
};
use at_engine::deploy;
use at_engine::skill_delete::{
    DeleteSkill, DeleteSkillRequest, apply_delete_skill, plan_delete_skill,
};
use at_engine::skill_transfer::{
    ClashResolution, NativeTransferTarget, SkillTransfer, SkillTransferOutcome, SkillTransferPlan,
    SkillTransferRequest, apply_skill_transfer, plan_skill_transfer,
};
use at_fs::failpoint::{FailInjector, Fault, FaultPoint, with_injector};
use at_model::{AgentId, FileOp, ItemId, ItemKind, ItemState, LinkMode, Scope};
use at_store::layout::{Slug, StoreId, StoreLayout};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath, hash_skill_folder};
use rusqlite::params;
use tempfile::TempDir;

const SKILL_BODY: &[u8] = b"---\nname: review\ndescription: Review changes\n---\n\n# Review\n";

/// Recursive tree snapshot: every regular file by content digest, every symlink by target.
///
/// `.state/` is at-fs journal and lock bookkeeping, not deployed content.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    fn walk(current: &Path, root: &Path, into: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".state") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let metadata = fs::symlink_metadata(&path).expect("tree metadata");
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("link target");
                into.insert(relative, format!("link:{}", target.display()));
            } else if metadata.is_dir() {
                into.insert(relative, String::from("dir"));
                walk(&path, root, into);
            } else {
                let bytes = fs::read(&path).expect("tree file");
                into.insert(relative, at_fs::sha256_hex(&bytes));
            }
        }
    }
    let mut collected = BTreeMap::new();
    walk(root, root, &mut collected);
    collected
}

/// Creates `dir` and writes `body` as its `SKILL.md`.
fn write_skill(dir: &Path, body: &[u8]) {
    fs::create_dir_all(dir).expect("skill directory");
    fs::write(dir.join("SKILL.md"), body).expect("skill document");
}

/// Adds a managed skill package, a manifest item and an indexed item row.
fn add_skill(
    database: &Database,
    store: &StoreLayout,
    manifest: &mut StoreManifest,
    scope: Scope,
    slug_text: &str,
    body: &[u8],
) -> ItemId {
    let slug = Slug::parse(slug_text).expect("valid slug");
    let dir = store.skill_dir(&slug);
    write_skill(&dir, body);
    let hash = hash_skill_folder(&dir).expect("skill hash");
    let id = ItemId::new();
    manifest
        .insert_item(ManifestItem::new(
            id,
            ItemKind::Skill,
            slug.clone(),
            StorePath::parse(&format!("skills/{slug_text}")).expect("store path"),
            hash,
        ))
        .expect("manifest item");
    database
        .connection()
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path) \
             VALUES (?1, 'skill', ?2, ?3, ?3, ?4)",
            params![
                id.to_string(),
                scope.wire_name(),
                slug.as_str(),
                format!("skills/{slug_text}")
            ],
        )
        .expect("item row");
    id
}

/// Persists a manifest at the layout's manifest path.
fn persist(store: &StoreLayout, manifest: &StoreManifest) -> Vec<u8> {
    let bytes = manifest.to_bytes().expect("manifest bytes");
    fs::create_dir_all(store.root()).expect("store root");
    fs::write(store.manifest_path(), &bytes).expect("manifest write");
    bytes
}

/// Seeds one linked deployment row and the native symlink it describes.
fn seed_link(
    database: &Database,
    item_id: ItemId,
    agent: &AgentId,
    scope: Scope,
    link: &Path,
    canonical: &Path,
) {
    fs::create_dir_all(link.parent().expect("link parent")).expect("agent directory");
    std::os::unix::fs::symlink(canonical, link).expect("native link");
    let target = link.to_string_lossy().into_owned();
    let spec = DeploymentSpec::applied(
        &item_id,
        agent,
        scope,
        ItemState::Linked,
        Some("symlink"),
        Some(&target),
        at_db::deployments::DeploymentHashes::none(),
    )
    .with_operation("op_seed");
    upsert_deployment(database.connection(), &spec).expect("deployment row");
}

/// Counts one operation row and its recorded files, asserting the id is unique.
fn assert_one_operation(database: &Database, operation_id: &str, expected_files: i64) {
    let operations: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE id = ?1",
            [operation_id],
            |row| row.get(0),
        )
        .expect("operation count");
    assert_eq!(
        operations, 1,
        "operation {operation_id} appears exactly once"
    );
    let files: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM operation_files WHERE operation_id = ?1",
            [operation_id],
            |row| row.get(0),
        )
        .expect("operation file count");
    assert_eq!(
        files, expected_files,
        "operation {operation_id} records {expected_files} file(s)"
    );
}

// ---------------------------------------------------------------------------
// Bulk enable
// ---------------------------------------------------------------------------

/// Fails the n-th `ChangeSetOperation` crossing with `EACCES`; every other crossing proceeds.
struct FailNthOperation {
    target: usize,
    seen: AtomicUsize,
}

impl FailInjector for FailNthOperation {
    fn fault(&self, point: FaultPoint, _path: &Path) -> Option<Fault> {
        if point != FaultPoint::ChangeSetOperation {
            return None;
        }
        let crossing = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        (crossing == self.target).then_some(Fault::Fail(13))
    }
}

struct BulkFixture {
    _temp: TempDir,
    root: PathBuf,
    database: Database,
    store: StoreLayout,
    items: Vec<ItemId>,
    plan: at_engine::bulk_skill::BulkPlan,
    agent_roots: Vec<(AgentId, PathBuf)>,
    backup_root: PathBuf,
}

/// Five managed skills, two distinct agent roots, and their reviewed bulk plan.
fn bulk_fixture() -> BulkFixture {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = temp.path().canonicalize().expect("canonical root");
    let database = Database::open_in_memory().expect("database");
    let store = StoreLayout::new(Scope::User, root.join("user-store"));
    let mut manifest = StoreManifest::new(Scope::User, StoreId::new());
    let mut items = Vec::new();
    for index in 0..5 {
        let slug = format!("skill-{index}");
        items.push(add_skill(
            &database,
            &store,
            &mut manifest,
            Scope::User,
            &slug,
            SKILL_BODY,
        ));
    }
    persist(&store, &manifest);

    let agent_roots = vec![
        (AgentId::Codex, root.join("agents/codex/skills")),
        (AgentId::ClaudeCode, root.join("agents/claude/skills")),
    ];
    for (_, skills) in &agent_roots {
        fs::create_dir_all(skills).expect("agent skills root");
    }
    let mut plans = Vec::new();
    for (index, item_id) in items.iter().enumerate() {
        let slug = Slug::parse(&format!("skill-{index}")).expect("slug");
        let source = store.skill_dir(&slug);
        let mut ops: Vec<FileOp> = Vec::new();
        for (agent, skills) in &agent_roots {
            let planned = deploy::plan(deploy::PlanRequest {
                item_id: *item_id,
                scope: Scope::User,
                agent: agent.clone(),
                store_root: store.root().to_path_buf(),
                source: source.clone(),
                destination: skills.join(slug.as_str()),
                mode: LinkMode::Link,
                link_supported: true,
                managed: true,
                destination_managed: false,
                shared_agents: Vec::new(),
                project_id: None,
                summary: format!("Enable skill-{index} for {agent}"),
            })
            .expect("deployment plan");
            if let Some(set) = planned.change_set {
                ops.extend(set.ops);
            }
        }
        plans.push(BulkItemPlan {
            item_id: *item_id,
            ops,
            refusal: None,
        });
    }
    let intent = BulkIntent {
        kind: BulkKind::Enable,
        items: items.clone(),
        targets: vec![AgentId::Codex, AgentId::ClaudeCode],
        scope: Scope::User,
        project_id: None,
    };
    let plan = plan_bulk(&intent, plans).expect("bulk plan");
    let backup_root = store.backups_dir();
    BulkFixture {
        _temp: temp,
        root,
        database,
        store,
        items,
        plan,
        agent_roots,
        backup_root,
    }
}

impl BulkFixture {
    fn context<'a>(
        &'a self,
        recorder: &'a mut at_fs::DbOperationRecorder<'a>,
    ) -> (at_fs::ApplyContext<'a>, at_fs::ApplyRoots) {
        let context = at_fs::ApplyContext {
            store_root: self.store.root(),
            backup_folder: &self.backup_root,
            recorder,
        };
        let roots = at_fs::ApplyRoots::new([self.root.clone()]).expect("authorized root");
        (context, roots)
    }

    fn links(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .enumerate()
            .flat_map(|(index, _)| {
                self.agent_roots
                    .iter()
                    .map(move |(_, skills)| skills.join(format!("skill-{index}")))
            })
            .collect()
    }
}

#[test]
fn second_write_failure_rolls_back_the_whole_bulk_batch() {
    let fixture = bulk_fixture();
    fs::create_dir_all(fixture.store.backups_dir()).expect("backup directory");
    let before = snapshot(&fixture.root);
    let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
    let (mut context, roots) = fixture.context(&mut recorder);
    let injector = Arc::new(FailNthOperation {
        target: 2,
        seen: AtomicUsize::new(0),
    });
    let mut events = Vec::new();
    let outcome = with_injector(injector, || {
        apply_bulk(&fixture.plan, &mut context, &roots, |event| {
            events.push(event);
        })
    });
    assert!(
        matches!(outcome, Err(BulkApplyError::Apply(_))),
        "the injected permission failure surfaces as an apply error"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.stage == BulkProgressStage::ItemStarted)
            .count(),
        fixture.items.len(),
        "every selected skill reports started"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.stage == BulkProgressStage::ItemFailed)
            .count(),
        fixture.items.len(),
        "every selected skill reports the rollback"
    );
    assert_eq!(
        snapshot(&fixture.root),
        before,
        "the tree hash equals the pre-state, so the first write was rolled back"
    );
    for link in fixture.links() {
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "no link survives the rollback: {}",
            link.display()
        );
    }
}

#[test]
fn clean_bulk_enable_records_one_operation_with_one_file_row_per_write() {
    let fixture = bulk_fixture();
    fs::create_dir_all(fixture.store.backups_dir()).expect("backup directory");
    let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
    let (mut context, roots) = fixture.context(&mut recorder);
    let mut events = Vec::new();
    let outcome = apply_bulk(&fixture.plan, &mut context, &roots, |event| {
        events.push(event);
    })
    .expect("clean batch applies");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.stage == BulkProgressStage::ItemDone)
            .count(),
        fixture.items.len(),
        "every selected skill reports done"
    );
    let links = fixture.links();
    assert_eq!(links.len(), 10, "five skills for two agents");
    for link in &links {
        assert!(
            fs::symlink_metadata(link).is_ok_and(|meta| meta.file_type().is_symlink()),
            "the reviewed link exists: {}",
            link.display()
        );
    }
    assert_one_operation(
        &fixture.database,
        &outcome.operation_id,
        i64::try_from(fixture.plan.change_set.ops.len()).expect("op count"),
    );
}

// ---------------------------------------------------------------------------
// Promote, move and duplicate
// ---------------------------------------------------------------------------

struct TransferFixture {
    _temp: TempDir,
    root: PathBuf,
    database: Database,
    source_store: StoreLayout,
    target_store: StoreLayout,
    source_manifest: StoreManifest,
    target_manifest: StoreManifest,
    source_bytes: Vec<u8>,
    target_bytes: Vec<u8>,
    source_id: ItemId,
    target_id: Option<ItemId>,
    slug: Slug,
    link: PathBuf,
    native_targets: Vec<NativeTransferTarget>,
}

/// One skill promoted or moved between two stores, optionally onto an existing copy.
fn transfer_fixture(
    source_scope: Scope,
    target_scope: Scope,
    target_exists: bool,
) -> TransferFixture {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = temp.path().canonicalize().expect("canonical root");
    let database = Database::open_in_memory().expect("database");
    let source_store = StoreLayout::new(source_scope, root.join("project-store"));
    let target_store = StoreLayout::new(target_scope, root.join("user-store"));
    let slug = Slug::parse("review").expect("slug");
    let mut source_manifest = StoreManifest::new(source_scope, StoreId::new());
    let source_id = add_skill(
        &database,
        &source_store,
        &mut source_manifest,
        source_scope,
        "review",
        SKILL_BODY,
    );
    let source_bytes = persist(&source_store, &source_manifest);

    let mut target_manifest = StoreManifest::new(target_scope, StoreId::new());
    let mut target_id = None;
    if target_exists {
        target_id = Some(add_skill(
            &database,
            &target_store,
            &mut target_manifest,
            target_scope,
            "review",
            b"---\nname: review\ndescription: Existing target\n---\n",
        ));
    }
    let target_bytes = persist(&target_store, &target_manifest);

    let link = root.join("agents/codex/review");
    seed_link(
        &database,
        source_id,
        &AgentId::Codex,
        source_scope,
        &link,
        &source_store.skill_dir(&slug),
    );
    // The agent folder is the same path in both scopes here, so the native phase detaches the
    // source link and re-creates it against the target package.
    let native_targets = vec![NativeTransferTarget {
        agent: AgentId::Codex,
        source_path: link.clone(),
        target_root: link.parent().expect("link parent").to_path_buf(),
        mode: LinkMode::Link,
        link_supported: true,
    }];
    fs::create_dir_all(target_store.backups_dir()).expect("backup directory");
    TransferFixture {
        _temp: temp,
        root,
        database,
        source_store,
        target_store,
        source_manifest,
        target_manifest,
        source_bytes,
        target_bytes,
        source_id,
        target_id,
        slug,
        link,
        native_targets,
    }
}

impl TransferFixture {
    fn request<'a>(
        &'a self,
        transfer: SkillTransfer,
        resolution: Option<ClashResolution>,
    ) -> SkillTransferRequest<'a> {
        SkillTransferRequest {
            transfer,
            source_store: &self.source_store,
            target_store: &self.target_store,
            item_id: self.source_id,
            source_manifest: &self.source_manifest,
            target_manifest: &self.target_manifest,
            source_manifest_bytes: &self.source_bytes,
            target_manifest_bytes: &self.target_bytes,
            global_permission: false,
            resolution,
            connection: self.database.connection(),
            target_project_id: None,
            native_targets: &self.native_targets,
        }
    }

    fn apply(&self, plan: &SkillTransferPlan) -> SkillTransferOutcome {
        let mut recorder = at_fs::DbOperationRecorder::new(self.database.connection(), 30);
        let mut context = at_fs::ApplyContext {
            store_root: self.target_store.root(),
            backup_folder: &self.target_store.backups_dir(),
            recorder: &mut recorder,
        };
        let roots = at_fs::ApplyRoots::new([self.root.clone()]).expect("authorized root");
        apply_skill_transfer(self.database.connection(), plan, &mut context, &roots)
            .expect("apply the reviewed transfer")
    }

    /// Assert the deployment table and the native links agree in both directions.
    fn assert_no_orphans(&self, item_id: ItemId, expected_links: usize) {
        let cells =
            list_deployments_for_item(self.database.connection(), &item_id).expect("deployments");
        let linked: Vec<_> = cells
            .iter()
            .filter(|cell| cell.state == ItemState::Linked)
            .collect();
        assert_eq!(
            linked.len(),
            expected_links,
            "one linked row per existing native link"
        );
        for cell in &linked {
            let target = cell.target_path.as_deref().expect("linked target path");
            let metadata = fs::symlink_metadata(target)
                .unwrap_or_else(|_| panic!("linked target exists: {target}"));
            assert!(metadata.file_type().is_symlink());
        }
        let item_slug = self.slug.as_str();
        let canonical = self
            .target_store
            .skill_dir(&Slug::parse(item_slug).expect("slug"));
        if expected_links > 0 {
            let resolved = fs::canonicalize(&self.link).expect("link resolves");
            assert_eq!(
                resolved,
                fs::canonicalize(&canonical).expect("canonical resolves"),
                "the agent link resolves to the promoted copy"
            );
        }
    }
}

#[test]
fn promote_onto_an_existing_copy_needs_replace_and_backs_up_the_target() {
    let fixture = transfer_fixture(Scope::Project, Scope::User, true);
    let before = snapshot(&fixture.root);
    let error = plan_skill_transfer(fixture.request(SkillTransfer::Promote, None))
        .expect_err("an unconfirmed collision must be refused");
    assert!(
        matches!(
            error,
            at_engine::skill_transfer::SkillTransferError::Clash(_)
        ),
        "the refusal is the explicit clash report"
    );
    assert_eq!(
        snapshot(&fixture.root),
        before,
        "a refused promote writes nothing"
    );

    let plan = plan_skill_transfer(
        fixture.request(SkillTransfer::Promote, Some(ClashResolution::Replace)),
    )
    .expect("Replace resolves the collision");
    let outcome = fixture.apply(&plan);

    let target_dir = fixture.target_store.skill_dir(&fixture.slug);
    assert_eq!(
        fs::read(target_dir.join("SKILL.md")).expect("promoted package"),
        SKILL_BODY,
        "the promoted copy carries the source bytes"
    );
    assert_eq!(
        fs::read(
            fixture
                .source_store
                .skill_dir(&fixture.slug)
                .join("SKILL.md")
        )
        .expect("source stays"),
        SKILL_BODY,
        "promote keeps the source copy"
    );

    let backups = at_db::backups::backups_for_operation(
        fixture.database.connection(),
        &outcome.canonical_operation_id,
    )
    .expect("backup rows");
    let overwritten = backups
        .iter()
        .find(|backup| {
            backup
                .original_path
                .as_deref()
                .is_some_and(|path| path.ends_with("user-store/skills/review/SKILL.md"))
        })
        .expect("the overwritten target file has a backups row");
    let blob = overwritten.backup_path.as_deref().expect("backup blob");
    assert_eq!(
        fs::read(Path::new(blob)).expect("backup bytes"),
        b"---\nname: review\ndescription: Existing target\n---\n",
        "the backup restores the pre-state byte-identically"
    );
    assert_one_operation(
        &fixture.database,
        &outcome.canonical_operation_id,
        i64::try_from(plan.change_set.ops.len()).expect("op count"),
    );
    fixture.assert_no_orphans(fixture.source_id, 1);
    if let Some(replaced) = fixture.target_id {
        let remaining: i64 = fixture
            .database
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM items WHERE id = ?1",
                [replaced.to_string()],
                |row| row.get(0),
            )
            .expect("replaced item count");
        assert_eq!(remaining, 0, "the replaced target identity is removed");
    }
}

#[test]
fn move_leaves_no_gap_and_keeps_the_old_folder_only_in_backups() {
    let fixture = transfer_fixture(Scope::Project, Scope::User, false);
    let plan = plan_skill_transfer(fixture.request(SkillTransfer::Move, None)).expect("move plan");
    let outcome = fixture.apply(&plan);

    let old_dir = fixture.source_store.skill_dir(&fixture.slug);
    assert!(!old_dir.exists(), "the old canonical folder is removed");
    let new_dir = fixture.target_store.skill_dir(&fixture.slug);
    assert_eq!(
        fs::read(new_dir.join("SKILL.md")).expect("moved package"),
        SKILL_BODY
    );
    assert_eq!(
        fs::read(fixture.link.join("SKILL.md")).expect("agent sees the moved package"),
        SKILL_BODY,
        "every enabled agent resolves to the new location"
    );
    let backups = at_db::backups::backups_for_operation(
        fixture.database.connection(),
        &outcome.canonical_operation_id,
    )
    .expect("backup rows");
    let old_copy = backups
        .iter()
        .find(|backup| {
            backup
                .original_path
                .as_deref()
                .is_some_and(|path| path.ends_with("project-store/skills/review"))
        })
        .expect("the removed source folder has a backups row");
    let blob = old_copy.backup_path.as_deref().expect("backup blob");
    assert_eq!(
        fs::read(Path::new(blob).join("SKILL.md")).expect("backup bytes"),
        SKILL_BODY,
        "the old folder exists only in backups and restores byte-identically"
    );
    assert_one_operation(
        &fixture.database,
        &outcome.canonical_operation_id,
        i64::try_from(plan.change_set.ops.len()).expect("op count"),
    );
    fixture.assert_no_orphans(fixture.source_id, 1);
}

#[test]
fn duplicate_allocates_a_new_identity_and_keeps_the_source() {
    let fixture = transfer_fixture(Scope::User, Scope::User, false);
    let duplicate = Slug::parse("review-2").expect("duplicate slug");
    let plan = plan_skill_transfer(fixture.request(
        SkillTransfer::Duplicate {
            slug: duplicate.clone(),
        },
        None,
    ))
    .expect("duplicate plan");
    let outcome = fixture.apply(&plan);

    assert!(fixture.source_store.skill_dir(&fixture.slug).exists());
    let copy = fixture.target_store.skill_dir(&duplicate);
    assert_eq!(
        fs::read(copy.join("SKILL.md")).expect("duplicate package"),
        SKILL_BODY
    );
    let duplicated: String = fixture
        .database
        .connection()
        .query_row(
            "SELECT id FROM items WHERE slug = 'review-2' AND scope = 'user'",
            [],
            |row| row.get(0),
        )
        .expect("duplicated item row");
    assert_eq!(duplicated, plan.target_item_id.to_string());
    assert_ne!(plan.target_item_id, fixture.source_id);
    let deployments =
        list_deployments_for_item(fixture.database.connection(), &plan.target_item_id)
            .expect("duplicate deployments");
    assert!(deployments.is_empty(), "a duplicate starts undeployed");
    assert_one_operation(
        &fixture.database,
        &outcome.canonical_operation_id,
        i64::try_from(plan.change_set.ops.len()).expect("op count"),
    );

    let reloaded = StoreManifest::load(&fixture.target_store).expect("reload target manifest");
    let reloaded_bytes =
        fs::read(fixture.target_store.manifest_path()).expect("reloaded manifest bytes");
    let clash = plan_skill_transfer(SkillTransferRequest {
        transfer: SkillTransfer::Duplicate {
            slug: duplicate.clone(),
        },
        source_store: &fixture.source_store,
        target_store: &fixture.target_store,
        item_id: fixture.source_id,
        source_manifest: &fixture.source_manifest,
        target_manifest: &reloaded.manifest,
        source_manifest_bytes: &fixture.source_bytes,
        target_manifest_bytes: &reloaded_bytes,
        global_permission: false,
        resolution: None,
        connection: fixture.database.connection(),
        target_project_id: None,
        native_targets: &fixture.native_targets,
    })
    .expect_err("an existing slug clashes");
    assert!(matches!(
        clash,
        at_engine::skill_transfer::SkillTransferError::Clash(_)
    ));
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

struct DeleteFixture {
    _temp: TempDir,
    root: PathBuf,
    database: Database,
    store: StoreLayout,
    manifest: StoreManifest,
    manifest_bytes: Vec<u8>,
    item_id: ItemId,
    slug: Slug,
    link: PathBuf,
    link_target: PathBuf,
    lower_copy: PathBuf,
}

/// A User skill with a Codex link and a lower-precedence Project copy of the same slug.
fn delete_fixture() -> DeleteFixture {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = temp.path().canonicalize().expect("canonical root");
    let database = Database::open_in_memory().expect("database");
    let store = StoreLayout::new(Scope::User, root.join("user-store"));
    let mut manifest = StoreManifest::new(Scope::User, StoreId::new());
    let item_id = add_skill(
        &database,
        &store,
        &mut manifest,
        Scope::User,
        "review",
        SKILL_BODY,
    );
    let manifest_bytes = persist(&store, &manifest);
    let slug = Slug::parse("review").expect("slug");
    let link_target = root.join("link-target");
    fs::create_dir_all(&link_target).expect("external link target");
    let link = root.join("agents/codex/review");
    fs::create_dir_all(link.parent().expect("link parent")).expect("agent directory");
    std::os::unix::fs::symlink(&link_target, &link).expect("external link");
    let target = link.to_string_lossy().into_owned();
    let spec = DeploymentSpec::applied(
        &item_id,
        &AgentId::Codex,
        Scope::User,
        ItemState::Linked,
        Some("symlink"),
        Some(&target),
        at_db::deployments::DeploymentHashes::none(),
    )
    .with_operation("op_seed");
    upsert_deployment(database.connection(), &spec).expect("deployment row");

    let lower = StoreLayout::new(Scope::Project, root.join("project-store"));
    let mut lower_manifest = StoreManifest::new(Scope::Project, StoreId::new());
    add_skill(
        &database,
        &lower,
        &mut lower_manifest,
        Scope::Project,
        "review",
        b"---\nname: review\ndescription: Project copy\n---\n",
    );
    persist(&lower, &lower_manifest);
    let lower_copy = lower.skill_dir(&slug);
    fs::create_dir_all(store.backups_dir()).expect("backup directory");
    DeleteFixture {
        _temp: temp,
        root,
        database,
        store,
        manifest,
        manifest_bytes,
        item_id,
        slug,
        link,
        link_target,
        lower_copy,
    }
}

#[test]
fn delete_removes_links_and_rows_and_reports_the_lower_precedence_copy() {
    let fixture = delete_fixture();
    let plan = plan_delete_skill(DeleteSkillRequest {
        intent: DeleteSkill {
            slug: fixture.slug.clone(),
            scope: Scope::User,
        },
        confirmed: true,
        store: &fixture.store,
        manifest: &fixture.manifest,
        manifest_bytes: &fixture.manifest_bytes,
        connection: fixture.database.connection(),
        project_id: None,
        global_permission: false,
    })
    .expect("delete plan");
    let next = plan.next_effective.as_ref().expect("a remaining copy");
    assert_eq!(next.scope, Scope::Project);
    assert_eq!(next.path, PathBuf::from("skills/review"));

    let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: fixture.store.root(),
        backup_folder: &fixture.store.backups_dir(),
        recorder: &mut recorder,
    };
    let roots = at_fs::ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = apply_delete_skill(fixture.database.connection(), &plan, &mut context, &roots)
        .expect("apply delete");

    assert!(!fixture.store.skill_dir(&fixture.slug).exists());
    assert!(fixture.link_target.exists(), "the link target is untouched");
    assert!(
        fs::symlink_metadata(&fixture.link).is_err(),
        "the owned link is removed"
    );
    let rows: i64 = fixture
        .database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM deployments WHERE item_id = ?1",
            [fixture.item_id.to_string()],
            |row| row.get(0),
        )
        .expect("deployment rows");
    assert_eq!(rows, 0, "deployment rows cascade with the deleted item");
    assert_eq!(
        fs::read(fixture.lower_copy.join("SKILL.md")).expect("lower copy"),
        b"---\nname: review\ndescription: Project copy\n---\n",
        "the lower-precedence copy is untouched"
    );
    assert_one_operation(
        &fixture.database,
        &outcome.operation_id,
        i64::try_from(plan.change_set.ops.len()).expect("op count"),
    );

    // Generic undo restores the package and the link byte-for-byte.
    let undo = at_migrate::undo::undo_operation(
        at_migrate::undo::UndoOperationRequest {
            connection: fixture.database.connection(),
            store_root: fixture.store.root(),
            backup_root: &fixture.store.backups_dir(),
            authorized_roots: &roots,
            actor: at_db::ops::OperationActor::Ui,
            force: false,
        },
        &outcome.operation_id,
    )
    .expect("undo the delete");
    assert!(matches!(
        undo,
        at_migrate::undo::UndoOutcome::Restored { .. }
    ));
    assert_eq!(
        fs::read(fixture.store.skill_dir(&fixture.slug).join("SKILL.md"))
            .expect("restored package"),
        SKILL_BODY,
        "the canonical package restores byte-identically"
    );
    assert_eq!(
        fs::canonicalize(&fixture.link).expect("restored link resolves"),
        fs::canonicalize(&fixture.link_target).expect("link target resolves"),
        "the removed link restores byte-identically"
    );
}

// ---------------------------------------------------------------------------
// Unmanaged folders
// ---------------------------------------------------------------------------

/// Creates an unmanaged folder with a distinctive mode and mtime.
fn unmanaged_folder(root: &Path) -> (PathBuf, u32, std::time::SystemTime) {
    use std::os::unix::fs::PermissionsExt as _;

    let folder = root.join("unmanaged/keep");
    fs::create_dir_all(&folder).expect("unmanaged folder");
    fs::write(folder.join("SKILL.md"), b"unmanaged bytes").expect("unmanaged file");
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o750)).expect("folder permissions");
    fs::set_permissions(folder.join("SKILL.md"), fs::Permissions::from_mode(0o640))
        .expect("file permissions");
    let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    for path in [&folder, &folder.join("SKILL.md")] {
        let file = fs::File::open(path).expect("open for times");
        file.set_times(fs::FileTimes::new().set_accessed(stamp).set_modified(stamp))
            .expect("set times");
    }
    let mode = fs::metadata(&folder)
        .expect("folder metadata")
        .permissions()
        .mode()
        & 0o7777;
    (folder, mode, stamp)
}

/// Asserts the folder's mode, mtime and bytes are exactly what they were.
fn assert_untouched(folder: &Path, mode: u32, stamp: std::time::SystemTime) {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = fs::metadata(folder).expect("unmanaged folder survives");
    assert_eq!(metadata.permissions().mode() & 0o7777, mode);
    assert_eq!(metadata.modified().expect("mtime"), stamp);
    assert_eq!(
        fs::read(folder.join("SKILL.md")).expect("unmanaged bytes"),
        b"unmanaged bytes"
    );
}

#[test]
fn promote_move_and_delete_never_touch_an_unmanaged_folder() {
    // Promote.
    let fixture = transfer_fixture(Scope::Project, Scope::User, false);
    let (unmanaged, mode, stamp) = unmanaged_folder(fixture.target_store.root());
    let plan =
        plan_skill_transfer(fixture.request(SkillTransfer::Promote, None)).expect("promote plan");
    fixture.apply(&plan);
    assert_untouched(&unmanaged, mode, stamp);

    // Move.
    let fixture = transfer_fixture(Scope::Project, Scope::User, false);
    let (unmanaged, mode, stamp) = unmanaged_folder(fixture.target_store.root());
    let plan = plan_skill_transfer(fixture.request(SkillTransfer::Move, None)).expect("move plan");
    fixture.apply(&plan);
    assert_untouched(&unmanaged, mode, stamp);

    // Delete.
    let fixture = delete_fixture();
    let (unmanaged, mode, stamp) = unmanaged_folder(fixture.store.root());
    let plan = plan_delete_skill(DeleteSkillRequest {
        intent: DeleteSkill {
            slug: fixture.slug.clone(),
            scope: Scope::User,
        },
        confirmed: true,
        store: &fixture.store,
        manifest: &fixture.manifest,
        manifest_bytes: &fixture.manifest_bytes,
        connection: fixture.database.connection(),
        project_id: None,
        global_permission: false,
    })
    .expect("delete plan");
    let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: fixture.store.root(),
        backup_folder: &fixture.store.backups_dir(),
        recorder: &mut recorder,
    };
    let roots = at_fs::ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    apply_delete_skill(fixture.database.connection(), &plan, &mut context, &roots)
        .expect("apply delete");
    assert_untouched(&unmanaged, mode, stamp);
}
