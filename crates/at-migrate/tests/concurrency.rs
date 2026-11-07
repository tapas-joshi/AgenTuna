//! TC-039: concurrent external writers and lock contention.
//!
//! Every case runs a real second writer — a thread, a child process or an at-fs fault injector
//! that writes through the real filesystem — and asserts that the operation either applies a
//! fully consistent change or aborts with the external writer's bytes intact.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use at_db::Database;
use at_fs::failpoint::{FailInjector, Fault, FaultPoint, with_injector};
use at_fs::{
    ApplyContext, ApplyRoots, BackupPolicy, DbOperationRecorder, WriteRequest, apply, sha256_hex,
};
use at_migrate::apply::{ApplyPlanContext, apply_plan};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{BuildPlanRequest, MigrationStrategy, build_plan};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};
use at_model::{AgentId, ChangeSet, ExpectedState, FileOp, ItemKind, OperationKind, Scope};
use at_store::instance::LockPolicy;
use at_store::layout::{StoreLayout, init_store};
use at_store::{StoreAccess, StoreIoError};
use tempfile::TempDir;

fn change_set(ops: Vec<FileOp>) -> ChangeSet {
    ChangeSet {
        id: "concurrency-fixture".to_owned(),
        preview_id: "preview".to_owned(),
        operation_kind: OperationKind::parse("deploy").expect("operation kind"),
        scope: Some(Scope::User),
        project_id: None,
        summary: "concurrency fixture".to_owned(),
        ops,
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    }
}

fn temp_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with('.') && name.contains("agentuna"))
        })
        .count()
}

// ---------------------------------------------------------------------------
// 1. External writer changes a migration source between plan and apply.
// ---------------------------------------------------------------------------
#[test]
fn a_source_changed_between_plan_and_apply_aborts_before_any_backup() -> Result<(), Box<dyn Error>>
{
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let store = root.join("store");
    init_store(Scope::User, &store)?;
    let backups = root.join("backups");
    fs::create_dir_all(&backups)?;
    let native = root.join("native/skills/sample-skill");
    fs::create_dir_all(&native)?;
    fs::write(native.join("SKILL.md"), b"# Sample\nreviewed\n")?;
    let normalized = at_store::manifest::hash_skill_folder(&native)?.to_string();
    let item = ScanItem {
        kind: ItemKind::Skill,
        name: "sample-skill".to_owned(),
        agents: BTreeSet::from([AgentId::ClaudeCode]),
        path: native.clone(),
        scope: Scope::User,
        content_hash: Some(sha256_hex(b"# Sample\nreviewed\n")),
        normalized_hash: Some(normalized),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    };
    let scan = ScanReport {
        roots: Vec::new(),
        items: vec![item],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts(&scan);
    let selected = BTreeSet::from([native.clone()]);
    let plan = build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected,
        store_root: store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: backups.clone(),
    })?;

    // A real second writer edits the reviewed source before apply.
    let external = native.join("SKILL.md");
    let writer =
        thread::spawn(move || fs::write(external, b"# Sample\nchanged by another process\n"));
    writer.join().expect("writer thread")?;

    let mut database = Database::open_in_memory()?;
    let roots = ApplyRoots::new([root.clone()])?;
    let mut context = ApplyPlanContext {
        store_root: &store,
        backup_root: &backups,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    let error = apply_plan(&plan, &mut context).expect_err("stale source refuses");
    assert!(
        error.to_string().contains("stale"),
        "expected a stale-plan refusal, got {error}"
    );
    assert_eq!(
        fs::read(native.join("SKILL.md"))?,
        b"# Sample\nchanged by another process\n",
        "the external writer's bytes stay"
    );
    assert!(!store.join("skills/sample-skill").exists(), "no store item");
    assert_eq!(
        fs::read_dir(&backups)?.count(),
        0,
        "no backup folder was created"
    );
    assert!(
        at_db::ops::recent_changes(
            database.connection(),
            &at_db::ops::RecentChangesQuery::new(5)
        )?
        .is_empty(),
        "no operation was recorded"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. External writer changes the store manifest between preflight and write.
// ---------------------------------------------------------------------------
#[test]
fn a_store_manifest_edited_during_apply_rolls_back_the_earlier_step() -> Result<(), Box<dyn Error>>
{
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let store = root.join("store");
    init_store(Scope::User, &store)?;
    let backups = root.join("backups");
    fs::create_dir_all(&backups)?;
    let manifest = store.join("store.json");
    let manifest_bytes = fs::read(&manifest)?;
    let first = store.join("rules/one.md");
    let second = store.join("rules/two.md");
    fs::create_dir_all(store.join("rules"))?;
    fs::write(&first, b"one before\n")?;
    fs::write(&second, b"two before\n")?;

    let database = Database::open_in_memory()?;
    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: &store,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let set = change_set(vec![
        FileOp::Write {
            path: first.clone(),
            bytes: b"one after\n".to_vec(),
            expected: ExpectedState::hash(sha256_hex(b"one before\n"))?,
            follow_symlink: false,
            backup: true,
        },
        FileOp::Write {
            path: second.clone(),
            bytes: b"two after\n".to_vec(),
            expected: ExpectedState::hash(sha256_hex(b"two before\n"))?,
            follow_symlink: false,
            backup: true,
        },
    ]);
    // The external writer lands on the second destination exactly before its rename.
    struct SecondWriter {
        target: PathBuf,
    }
    impl FailInjector for SecondWriter {
        fn fault(&self, point: FaultPoint, path: &Path) -> Option<Fault> {
            (point == FaultPoint::BeforeRename && path == self.target)
                .then(|| Fault::ExternalWrite(b"written by another process\n".to_vec()))
        }
    }
    let injector = Arc::new(SecondWriter {
        target: second.clone(),
    });
    let error = with_injector(injector, || {
        at_fs::apply_with_roots(&set, &mut context, &ApplyRoots::new([root.clone()])?)
    })
    .expect_err("concurrent modification refuses");
    let text = error.to_string();
    assert!(
        text.contains("concurrent modification") && text.contains("rollback: complete"),
        "expected a rolled-back concurrency refusal, got {text}"
    );
    assert_eq!(
        fs::read(&first)?,
        b"one before\n",
        "the completed first step was rolled back"
    );
    assert_eq!(
        fs::read(&second)?,
        b"written by another process\n",
        "the external writer's bytes stay"
    );
    assert_eq!(temp_files(&store.join("rules")), 0, "no temp debris");
    assert_eq!(
        fs::read(&manifest)?,
        manifest_bytes,
        "the manifest was never touched"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. An external edit to store.json between load and save.
// ---------------------------------------------------------------------------
#[test]
fn an_external_store_manifest_edit_between_load_and_save_is_a_concurrent_modification()
-> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let store = fs::canonicalize(temp.path())?.join("store");
    init_store(Scope::User, &store)?;
    let layout = StoreLayout::new(Scope::User, &store);
    let snapshot = at_store::load_store(&layout, StoreAccess::ReadWrite)?;
    let manifest = snapshot.manifest.clone();
    let path = layout.manifest_path();
    let external = fs::read(&path)?;
    let writer = thread::spawn(move || {
        let mut bytes = external;
        bytes.extend_from_slice(b"\n");
        fs::write(&path, bytes)
    });
    writer.join().expect("writer thread")?;

    let error = at_store::save_store(
        &layout,
        &manifest,
        &snapshot.manifest_hash,
        StoreAccess::ReadWrite,
        BackupPolicy::None,
    )
    .expect_err("external edit refuses the save");
    assert!(
        matches!(error, StoreIoError::ConcurrentModification { .. }),
        "expected ConcurrentModification, got {error}"
    );
    let on_disk = fs::read(layout.manifest_path())?;
    assert!(on_disk.ends_with(b"\n"), "the external bytes stay");
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Two processes contend on the fs4 store lock.
// ---------------------------------------------------------------------------
#[test]
fn helper_child_process_holds_the_store_lock() {
    let Some(handshake) = std::env::var_os("AGENTUNA_TEST_LOCK_HANDSHAKE") else {
        return;
    };
    let Ok(root) = std::env::var("AGENTUNA_TEST_LOCK_ROOT") else {
        return;
    };
    let layout = StoreLayout::new(Scope::User, PathBuf::from(root));
    let _guard =
        at_store::lock_exclusive(&layout, StoreAccess::ReadWrite, LockPolicy::CLI_TRANSACTION)
            .expect("child takes the store lock");
    fs::write(&handshake, b"locked").expect("handshake");
    thread::sleep(Duration::from_secs(20));
}

#[test]
fn a_second_process_times_out_on_the_store_lock_without_writing() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let store = fs::canonicalize(temp.path())?.join("store");
    init_store(Scope::User, &store)?;
    let layout = StoreLayout::new(Scope::User, &store);
    let manifest = layout.manifest_path();
    let before = fs::read(&manifest)?;
    let handshake = store.join("lock-handshake");

    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "helper_child_process_holds_the_store_lock",
            "--nocapture",
        ])
        .env("AGENTUNA_TEST_LOCK_HANDSHAKE", &handshake)
        .env("AGENTUNA_TEST_LOCK_ROOT", &store)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !handshake.exists() {
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let child_locked = handshake.exists();

    let error = at_store::lock_exclusive(
        &layout,
        StoreAccess::ReadWrite,
        LockPolicy::waiting(Duration::from_millis(300)),
    )
    .expect_err("the second process must time out");
    let _ = child.kill();
    let _ = child.wait();
    assert!(child_locked, "the child process never took the lock");
    assert!(
        matches!(error, StoreIoError::LockTimeout { .. }),
        "expected LockTimeout, got {error}"
    );
    assert_eq!(
        fs::read(&manifest)?,
        before,
        "the loser wrote nothing while it waited"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. A same-content touch is not a concurrency failure.
// ---------------------------------------------------------------------------
#[test]
fn a_same_content_touch_with_a_new_mtime_is_allowed() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let dest = temp.path().join("config.json");
    let original = b"{\"key\":\"value\"}\n";
    fs::write(&dest, original)?;
    let hash = sha256_hex(original);
    let touched = fs::metadata(&dest)?.modified()?;
    thread::sleep(Duration::from_millis(30));
    // Same bytes, new mtime: an editor saved without changing anything.
    fs::write(&dest, original)?;
    assert_ne!(
        fs::metadata(&dest)?.modified()?,
        touched,
        "the fixture really changed the mtime"
    );
    apply(&WriteRequest::replace(
        &dest,
        b"{\"key\":\"new\"}\n".to_vec(),
        at_fs::ExpectedState::Hash(hash),
    ))?;
    assert_eq!(fs::read(&dest)?, b"{\"key\":\"new\"}\n");
    Ok(())
}

// ---------------------------------------------------------------------------
// 6. An agent config edited during apply keeps the external writer's bytes.
// ---------------------------------------------------------------------------
#[test]
fn an_agent_config_edited_during_apply_is_never_overwritten_by_rollback()
-> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let store = root.join("store");
    fs::create_dir_all(&store)?;
    let backups = root.join("backups");
    fs::create_dir_all(&backups)?;
    let first = root.join("agents/claude/settings.json");
    let second = root.join("agents/codex/config.toml");
    fs::create_dir_all(first.parent().expect("parent"))?;
    fs::create_dir_all(second.parent().expect("parent"))?;
    fs::write(&first, b"{\"theme\":\"dark\"}\n")?;
    fs::write(&second, b"model = \"one\"\n")?;
    let database = Database::open_in_memory()?;
    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: &store,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let set = change_set(vec![
        FileOp::Write {
            path: first.clone(),
            bytes: b"{\"theme\":\"light\"}\n".to_vec(),
            expected: ExpectedState::hash(sha256_hex(b"{\"theme\":\"dark\"}\n"))?,
            follow_symlink: false,
            backup: true,
        },
        FileOp::Write {
            path: second.clone(),
            bytes: b"model = \"two\"\n".to_vec(),
            expected: ExpectedState::hash(sha256_hex(b"model = \"one\"\n"))?,
            follow_symlink: false,
            backup: true,
        },
    ]);
    struct AgentWriter {
        target: PathBuf,
    }
    impl FailInjector for AgentWriter {
        fn fault(&self, point: FaultPoint, path: &Path) -> Option<Fault> {
            (point == FaultPoint::BeforeRename && path == self.target)
                .then(|| Fault::ExternalWrite(b"model = \"external\"\n".to_vec()))
        }
    }
    let injector = Arc::new(AgentWriter {
        target: second.clone(),
    });
    let roots = ApplyRoots::new([root.clone()])?;
    let error = with_injector(injector, || {
        at_fs::apply_with_roots(&set, &mut context, &roots)
    })
    .expect_err("agent edit refuses");
    let text = error.to_string();
    assert!(
        text.contains("concurrent modification") && text.contains("rollback: complete"),
        "expected a rolled-back concurrency refusal, got {text}"
    );
    assert_eq!(
        fs::read(&first)?,
        b"{\"theme\":\"dark\"}\n",
        "the rolled-back agent config is byte-identical to its original"
    );
    assert_eq!(
        fs::read(&second)?,
        b"model = \"external\"\n",
        "the external edit stays exactly as written"
    );
    assert_eq!(temp_files(&root.join("agents/claude")), 0);
    assert_eq!(temp_files(&root.join("agents/codex")), 0);
    Ok(())
}
