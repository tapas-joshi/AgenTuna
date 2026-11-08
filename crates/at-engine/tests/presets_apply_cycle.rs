//! TS-046: preset apply/unapply round-trips and applied ratios over the database.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::path::Path;

use at_db::Database;
use at_engine::presets::{MemberTransition, PresetService, Target};
use at_model::{AgentId, ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, OperationKind, Scope};
use rusqlite::params;

/// Journal seam used only to satisfy the apply pipeline; the plan's application
/// history is recorded by `PresetService::apply_plan` itself.
#[derive(Default)]
struct NullRecorder;

impl at_fs::changeset::OperationRecorder for NullRecorder {
    fn begin(&mut self, set: &ChangeSet) -> Result<String, String> {
        Ok(format!("op-{}", set.id))
    }

    fn file_completed(
        &mut self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&Path>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
        Ok(())
    }
}

fn change_set(kind: &str, ops: Vec<FileOp>) -> ChangeSet {
    ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: String::new(),
        operation_kind: OperationKind::parse(kind).expect("known operation kind"),
        scope: Some(Scope::User),
        project_id: None,
        summary: String::from("preset fixture"),
        ops,
        created_at_unix_seconds: 1,
    }
}

fn write_member(path: &Path, bytes: &[u8]) -> ChangeSet {
    change_set(
        "deploy",
        vec![FileOp::Write {
            path: path.to_path_buf(),
            bytes: bytes.to_vec(),
            expected: ExpectedState::absent(),
            follow_symlink: false,
            backup: false,
        }],
    )
}

fn delete_member(path: &Path, current: &[u8]) -> ChangeSet {
    change_set(
        "undeploy",
        vec![FileOp::Delete {
            path: path.to_path_buf(),
            expected: ExpectedState::hash(at_fs::sha256_hex(current)).expect("hash"),
        }],
    )
}

fn insert_skill(database: &Database, slug: &str) -> ItemId {
    let id = ItemId::new();
    database
        .connection()
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, 'skill', 'user', ?2, ?2)",
            params![id.to_string(), slug],
        )
        .expect("insert skill item");
    id
}

#[test]
fn preset_apply_then_unapply_restores_the_tree_hash_and_keeps_enabled_members() {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = std::fs::canonicalize(temp.path()).expect("canonical root");
    let tree = root.join("agent");
    std::fs::create_dir_all(&tree).expect("agent tree");
    let file_a = tree.join("a.txt");
    let file_b = tree.join("b.txt");
    std::fs::write(&file_a, b"member-a").expect("previously enabled member file");

    let mut database = Database::open_in_memory().expect("database");
    let first = insert_skill(&database, "alpha");
    let second = insert_skill(&database, "beta");
    let preset_id = {
        let mut service = PresetService::new(&mut database);
        service
            .create(
                "Daily setup",
                None,
                Scope::User,
                &[(first, ItemKind::Skill), (second, ItemKind::Skill)],
            )
            .expect("create preset")
    };
    let target = Target::Agent(AgentId::Codex);
    let hash_before = at_fs::hash_node(&tree)
        .expect("tree hash before")
        .expect("tree exists");

    let plan = {
        let service = PresetService::new(&mut database);
        service
            .plan_apply(
                &preset_id,
                target.clone(),
                vec![
                    MemberTransition {
                        item_id: first.to_string(),
                        prior_enabled: Some(true),
                        change_set: None,
                    },
                    MemberTransition {
                        item_id: second.to_string(),
                        prior_enabled: Some(false),
                        change_set: Some(write_member(&file_b, b"member-b")),
                    },
                ],
            )
            .expect("apply plan")
    };
    assert_eq!((plan.ratio_before.enabled, plan.ratio_before.total), (1, 2));
    assert_eq!((plan.ratio_after.enabled, plan.ratio_after.total), (2, 2));
    assert!(plan.deleted_members.is_empty());

    let backups = root.join("backups");
    std::fs::create_dir_all(&backups).expect("backup folder");
    let roots = at_fs::changeset::ApplyRoots::new(vec![root.clone()]).expect("roots");
    {
        let mut recorder = NullRecorder;
        let mut context = at_fs::changeset::ApplyContext {
            store_root: &root,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let mut service = PresetService::new(&mut database);
        service
            .apply_plan(&plan, &mut context, &roots)
            .expect("apply preset");
    }
    assert_eq!(
        std::fs::read(&file_b).expect("member B was written"),
        b"member-b"
    );
    assert_eq!(
        std::fs::read(&file_a).expect("member A untouched"),
        b"member-a"
    );
    let hash_applied = at_fs::hash_node(&tree)
        .expect("tree hash applied")
        .expect("tree exists");
    assert_ne!(hash_applied, hash_before);

    let application_id: i64 = database
        .connection()
        .query_row(
            "SELECT id FROM preset_applications ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("recorded application");

    let current_b = std::fs::read(&file_b).expect("member B content");
    let unapply = {
        let service = PresetService::new(&mut database);
        service
            .plan_unapply(
                application_id,
                target.clone(),
                vec![
                    // Already-enabled members keep their prior value and need no changes.
                    MemberTransition {
                        item_id: first.to_string(),
                        prior_enabled: None,
                        change_set: None,
                    },
                    MemberTransition {
                        item_id: second.to_string(),
                        prior_enabled: None,
                        change_set: Some(delete_member(&file_b, &current_b)),
                    },
                ],
            )
            .expect("unapply plan")
    };
    assert_eq!(
        (unapply.ratio_before.enabled, unapply.ratio_before.total),
        (2, 2)
    );
    assert_eq!(
        (unapply.ratio_after.enabled, unapply.ratio_after.total),
        (1, 2),
        "the previously enabled member stays enabled"
    );
    assert_eq!(
        unapply.transitions.len(),
        1,
        "only the member this application changed is reverted"
    );
    assert_eq!(
        unapply
            .transitions
            .iter()
            .find(|transition| transition.item_id == second.to_string())
            .and_then(|transition| transition.prior_enabled),
        Some(false)
    );
    assert_eq!(
        unapply
            .ratio_after
            .members
            .iter()
            .find(|member| member.item_id == first.to_string())
            .map(|member| member.enabled),
        Some(true),
        "the previously enabled member stays enabled"
    );

    {
        let mut recorder = NullRecorder;
        let mut context = at_fs::changeset::ApplyContext {
            store_root: &root,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let mut service = PresetService::new(&mut database);
        service
            .apply_plan(&unapply, &mut context, &roots)
            .expect("apply unapply plan");
    }
    assert!(!file_b.exists(), "the unapply removes the added member");
    assert_eq!(
        std::fs::read(&file_a).expect("previously enabled member kept"),
        b"member-a"
    );
    let hash_restored = at_fs::hash_node(&tree)
        .expect("tree hash restored")
        .expect("tree exists");
    assert_eq!(
        hash_restored, hash_before,
        "the exact tree hash round-trips"
    );
}

#[test]
fn applied_ratios_match_the_database_after_member_deletion_and_re_addition() {
    let mut database = Database::open_in_memory().expect("database");
    let present = insert_skill(&database, "alpha");
    let missing = ItemId::new();
    let preset_id = {
        let mut service = PresetService::new(&mut database);
        service
            .create(
                "Ratios",
                None,
                Scope::User,
                &[(present, ItemKind::Skill), (missing, ItemKind::Skill)],
            )
            .expect("create preset")
    };
    let target = Target::Agent(AgentId::Codex);

    let (ratio, warnings) = {
        let service = PresetService::new(&mut database);
        service
            .applied_ratio(&preset_id, &target)
            .expect("ratio with a deleted member")
    };
    assert_eq!((ratio.enabled, ratio.total), (1, 1));
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].item_id, missing.to_string());

    database
        .connection()
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, 'skill', 'user', 'beta', 'beta')",
            [missing.to_string()],
        )
        .expect("re-add the deleted member item");
    let (ratio, warnings) = {
        let service = PresetService::new(&mut database);
        service
            .applied_ratio(&preset_id, &target)
            .expect("ratio after re-add")
    };
    assert_eq!((ratio.enabled, ratio.total), (2, 2));
    assert!(warnings.is_empty());
    assert!(ratio.complete);

    {
        let mut service = PresetService::new(&mut database);
        service
            .remove_member(&preset_id, &present)
            .expect("remove member");
    }
    let (ratio, _) = {
        let service = PresetService::new(&mut database);
        service
            .applied_ratio(&preset_id, &target)
            .expect("ratio after membership removal")
    };
    assert_eq!((ratio.enabled, ratio.total), (1, 1));

    {
        let mut service = PresetService::new(&mut database);
        service
            .set_member(&preset_id, &present, ItemKind::Skill, true)
            .expect("re-add membership");
    }
    let (ratio, _) = {
        let service = PresetService::new(&mut database);
        service
            .applied_ratio(&preset_id, &target)
            .expect("ratio after membership re-add")
    };
    assert_eq!((ratio.enabled, ratio.total), (2, 2));
}

#[test]
fn update_checks_cover_unchanged_advanced_force_pushed_and_deleted_heads() {
    use at_engine::skill_source::{SkillSourceCheck, check_skill_source};
    use at_model::network::{NetworkEventLog, NetworkPolicy};

    let temp = tempfile::tempdir().expect("temporary root");
    let root = std::fs::canonicalize(temp.path()).expect("canonical root");
    let bare = root.join("remote.git");
    let work = root.join("work");
    let run = |directory: &Path, args: &[&str]| {
        let status = std::process::Command::new("git")
            .current_dir(directory)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .status()
            .expect("run fixture git");
        assert!(status.success(), "git {args:?} failed");
    };
    let head = |directory: &Path| {
        let output = std::process::Command::new("git")
            .current_dir(directory)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("read fixture head");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("head utf8")
            .trim()
            .to_owned()
    };
    std::fs::create_dir(&work).expect("work tree");
    run(&root, &["init", "--bare", bare.to_str().expect("bare")]);
    run(&work, &["init", "-b", "main"]);
    std::fs::write(work.join("SKILL.md"), "one").expect("first revision");
    run(&work, &["add", "SKILL.md"]);
    run(&work, &["commit", "-m", "first"]);
    run(
        &work,
        &["remote", "add", "origin", bare.to_str().expect("bare")],
    );
    run(&work, &["push", "origin", "main"]);
    let first = head(&work);
    let installed = root.join("installed");
    std::fs::create_dir(&installed).expect("installed package");
    std::fs::write(installed.join("SKILL.md"), "one").expect("installed document");

    let mut database = Database::open_in_memory().expect("database");
    let item = insert_skill(&database, "tracked");
    let content_hash = at_store::hash_skill_folder(&installed)
        .expect("content hash")
        .to_string();
    let url = format!("file://{}", bare.display());
    let mut spec = at_db::sources::SourceSpec::new(&item, at_db::sources::SourceKind::Git);
    spec.url = Some(&url);
    spec.git_ref = Some("main");
    spec.commit_sha = Some(&first);
    spec.content_hash = Some(&content_hash);
    at_db::sources::upsert_source(database.connection_mut(), &spec).expect("source row");

    let policy = NetworkPolicy::default();
    let mut log = NetworkEventLog::new();
    let check = |database: &Database, log: &mut NetworkEventLog| {
        check_skill_source(
            database.connection(),
            &item.to_string(),
            &installed,
            &policy,
            log,
        )
        .expect("source check")
    };

    // Unchanged commit: the installed sha is still the remote head.
    assert_eq!(
        check(&database, &mut log),
        SkillSourceCheck::Current {
            remote_sha: first.clone(),
            local_modified: false,
        }
    );

    // Advanced commit: a new head advertises an update.
    std::fs::write(work.join("SKILL.md"), "two").expect("advanced revision");
    run(&work, &["add", "SKILL.md"]);
    run(&work, &["commit", "-m", "second"]);
    run(&work, &["push", "origin", "main"]);
    let second = head(&work);
    assert_eq!(
        check(&database, &mut log),
        SkillSourceCheck::UpdateAvailable {
            remote_sha: second.clone(),
            local_modified: false,
        }
    );
    assert!(
        at_db::sources::source_for(database.connection(), &item)
            .expect("source")
            .expect("live source")
            .update_available
    );

    // Force push: the head moves to a rewritten commit, which is a new update.
    run(&work, &["reset", "--hard", &first]);
    std::fs::write(work.join("SKILL.md"), "rewritten").expect("rewritten revision");
    run(&work, &["add", "SKILL.md"]);
    run(&work, &["commit", "-m", "rewritten"]);
    run(&work, &["push", "--force", "origin", "main"]);
    let rewritten = head(&work);
    assert_ne!(rewritten, second);
    assert_eq!(
        check(&database, &mut log),
        SkillSourceCheck::UpdateAvailable {
            remote_sha: rewritten,
            local_modified: false,
        }
    );

    // Deleted branch: reported as gone, and no update badge remains.
    run(
        &root,
        &[
            "--git-dir",
            bare.to_str().expect("bare"),
            "update-ref",
            "-d",
            "refs/heads/main",
        ],
    );
    assert_eq!(check(&database, &mut log), SkillSourceCheck::SourceGone);
    let gone = at_db::sources::source_for(database.connection(), &item)
        .expect("source")
        .expect("live source");
    assert!(!gone.update_available);
    assert!(gone.remote_sha.is_none());

    // Offline: unknown result, prior badge state untouched.
    {
        let source = at_db::sources::source_for(database.connection(), &item)
            .expect("source")
            .expect("live source");
        at_db::sources::record_update_check(
            database.connection(),
            &source.id,
            &at_db::sources::CheckOutcome {
                remote_sha: Some("pending-head"),
                checked_at: Some("2026-11-06T00:00:00Z"),
                update_available: true,
                ..Default::default()
            },
        )
        .expect("record pending state");
    }
    let offline = check_skill_source(
        database.connection(),
        &item.to_string(),
        &installed,
        &NetworkPolicy::default().with_offline(true),
        &mut log,
    )
    .expect("offline check");
    assert_eq!(offline, SkillSourceCheck::Unknown);
    let after = at_db::sources::source_for(database.connection(), &item)
        .expect("source")
        .expect("live source");
    assert_eq!(after.remote_sha.as_deref(), Some("pending-head"));
    assert!(after.update_available);
}

#[test]
fn preset_apply_writes_through_a_symlinked_member_document() {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = std::fs::canonicalize(temp.path()).expect("canonical root");
    let dotfiles = root.join("dotfiles");
    std::fs::create_dir_all(&dotfiles).expect("dotfiles dir");
    let target = dotfiles.join("member.json");
    std::fs::write(&target, b"[]").expect("seed dotfile");
    let link = root.join("member.json");
    std::os::unix::fs::symlink(&target, &link).expect("member symlink");
    let mut database = Database::open_in_memory().expect("database");
    let member = insert_skill(&database, "alpha");
    let preset_id = {
        let mut service = PresetService::new(&mut database);
        service
            .create("Symlinked", None, Scope::User, &[(member, ItemKind::Skill)])
            .expect("create preset")
    };
    let plan = {
        let service = PresetService::new(&mut database);
        service
            .plan_apply(
                &preset_id,
                Target::Agent(AgentId::ClaudeCode),
                vec![MemberTransition {
                    item_id: member.to_string(),
                    prior_enabled: Some(false),
                    change_set: Some(change_set(
                        "deploy",
                        vec![FileOp::Write {
                            path: link.clone(),
                            bytes: b"[\"alpha\"]".to_vec(),
                            expected: ExpectedState::hash(at_fs::sha256_hex(b"[]")).expect("hash"),
                            follow_symlink: true,
                            backup: true,
                        }],
                    )),
                }],
            )
            .expect("apply plan")
    };
    let backups = root.join("backups");
    std::fs::create_dir_all(&backups).expect("backup folder");
    let roots = at_fs::changeset::ApplyRoots::new(vec![root.clone()]).expect("roots");
    let mut recorder = NullRecorder;
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &root,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    PresetService::new(&mut database)
        .apply_plan(&plan, &mut context, &roots)
        .expect("apply preset");

    assert!(
        std::fs::symlink_metadata(&link)
            .expect("member link")
            .file_type()
            .is_symlink(),
        "the member link survives the preset apply"
    );
    assert_eq!(
        std::fs::read(&target).expect("dotfile bytes"),
        b"[\"alpha\"]"
    );
}
