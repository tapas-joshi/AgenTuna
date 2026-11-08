//! Refuse-to-overwrite guard: classification, refusal, remedies and race handling.

// Unix-only harness: POSIX symlinks, permission bits and raw errno constants. The Windows
// twins arrive with the slice that enables the Windows test suite.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use at_fs::atomic::{Stage, apply_with_hook};
use at_fs::{
    Classification, DeploymentLookup, FsError, OwnershipContext, OwnershipGuard, Remedy,
    TargetKind, WriteRequest, apply, classify_target, create_parents_if_target_absent, move_aside,
    sha256_hex,
};
use at_model::{AgentId, ItemId, Scope};
use tempfile::TempDir;
use time::macros::datetime;

/// Stands in for the deployments table: exact (path, item, agent, scope) rows.
#[derive(Debug, Default)]
struct Rows(Vec<(PathBuf, ItemId, AgentId, Scope)>);

impl DeploymentLookup for Rows {
    fn is_recorded(&self, path: &Path, item: &ItemId, agent: &AgentId, scope: Scope) -> bool {
        self.0
            .iter()
            .any(|(p, i, a, s)| p == path && i == item && a == agent && *s == scope)
    }
}

struct Fixture {
    _tmp: TempDir,
    agent_dir: PathBuf,
    store: PathBuf,
    ctx: OwnershipContext,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("home/.claude/skills");
    let store = tmp.path().join("home/.agentuna");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::create_dir_all(&store).unwrap();
    let ctx = OwnershipContext {
        item_id: ItemId::new(),
        agent_id: AgentId::ClaudeCode,
        scope: Scope::User,
        store_roots: vec![store.clone()],
    };
    Fixture {
        _tmp: tmp,
        agent_dir,
        store,
        ctx,
    }
}

fn guard(f: &Fixture, rows: Rows) -> OwnershipGuard {
    OwnershipGuard::new(f.ctx.clone(), Arc::new(rows))
}

fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            out.push(p.strip_prefix(base).unwrap().display().to_string());
            if p.is_dir() {
                walk(base, &p, out);
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn unmanaged_file_is_refused_with_remedies_and_left_untouched() {
    let f = fixture();
    let skill = f.agent_dir.join("my-skill/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, "hand written").unwrap();
    let before = sha256_hex(&fs::read(&skill).unwrap());

    let mut req = WriteRequest::replace(
        &skill,
        b"deployed".to_vec(),
        at_fs::ExpectedState::Hash(before.clone()),
    );
    req.guard = Some(guard(&f, Rows::default()).shared());
    let err = apply(&req).unwrap_err();

    match err {
        FsError::RefusedOverwrite {
            path,
            kind,
            existing_hash,
            remedies,
        } => {
            assert_eq!(path, skill);
            assert!(path.is_absolute());
            assert_eq!(kind, TargetKind::File);
            assert_eq!(existing_hash.as_deref(), Some(before.as_str()));
            assert_eq!(remedies, [Remedy::Adopt, Remedy::MoveAside, Remedy::Cancel]);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        sha256_hex(&fs::read(&skill).unwrap()),
        before,
        "hash before and after are equal"
    );
}

#[test]
fn recorded_deployment_for_same_item_agent_scope_may_update() {
    let f = fixture();
    let path = f.agent_dir.join("tdd/SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "v1").unwrap();
    let rows = Rows(vec![(
        path.clone(),
        f.ctx.item_id,
        f.ctx.agent_id.clone(),
        f.ctx.scope,
    )]);

    let mut req = WriteRequest::replace(
        &path,
        b"v2".to_vec(),
        at_fs::ExpectedState::Hash(sha256_hex(b"v1")),
    );
    req.guard = Some(guard(&f, rows).shared());
    apply(&req).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
}

#[test]
fn a_row_for_another_item_agent_or_scope_does_not_grant_ownership() {
    let f = fixture();
    let path = f.agent_dir.join("x/SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "theirs").unwrap();
    for (item, agent, scope) in [
        (ItemId::new(), f.ctx.agent_id.clone(), f.ctx.scope),
        (f.ctx.item_id, AgentId::Codex, f.ctx.scope),
        (f.ctx.item_id, f.ctx.agent_id.clone(), Scope::Project),
    ] {
        let rows = Rows(vec![(path.clone(), item, agent, scope)]);
        let c = classify_target(&path, &f.ctx, &rows).unwrap();
        assert!(
            matches!(
                c,
                Classification::Unmanaged {
                    kind: TargetKind::File,
                    ..
                }
            ),
            "{c:?}"
        );
    }
}

#[test]
fn store_paths_are_managed_even_through_a_non_canonical_root() {
    let f = fixture();
    let inside = f.store.join("skills/tdd/SKILL.md");
    assert_eq!(
        classify_target(&inside, &f.ctx, &Rows::default()).unwrap(),
        Classification::Absent
    );
    fs::create_dir_all(inside.parent().unwrap()).unwrap();
    fs::write(&inside, "x").unwrap();
    assert_eq!(
        classify_target(&inside, &f.ctx, &Rows::default()).unwrap(),
        Classification::Managed
    );
}

#[test]
fn a_symlinked_ancestor_cannot_smuggle_an_outside_path_into_the_store() {
    let f = fixture();
    let outside = f.agent_dir.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("file"), "x").unwrap();
    symlink(&outside, f.store.join("evil")).unwrap();

    let c = classify_target(&f.store.join("evil/file"), &f.ctx, &Rows::default()).unwrap();
    assert!(
        matches!(
            c,
            Classification::Unmanaged {
                kind: TargetKind::File,
                ..
            }
        ),
        "{c:?}"
    );
}

#[test]
fn symlinks_are_classified_without_following_them() {
    let f = fixture();
    let real = f.agent_dir.join("real");
    fs::write(&real, "x").unwrap();
    let link = f.agent_dir.join("link");
    symlink(&real, &link).unwrap();
    let c = classify_target(&link, &f.ctx, &Rows::default()).unwrap();
    assert_eq!(
        c,
        Classification::Unmanaged {
            kind: TargetKind::Symlink,
            existing_hash: None
        }
    );
}

#[test]
fn relative_and_parent_dir_paths_are_rejected() {
    let f = fixture();
    let rows = Rows::default();
    assert!(matches!(
        classify_target(Path::new("skills/x"), &f.ctx, &rows),
        Err(FsError::InvalidPath { .. })
    ));
    let sneaky = f.store.join("../.claude/skills/x");
    assert!(matches!(
        classify_target(&sneaky, &f.ctx, &rows),
        Err(FsError::InvalidPath { .. })
    ));
}

#[test]
fn non_empty_unmanaged_directory_is_refused_wholesale() {
    let f = fixture();
    let dir = f.agent_dir.join("existing");
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/keep.md"), "keep").unwrap();
    let snapshot = tree(&f.agent_dir);

    for allow in [false, true] {
        let mut req = WriteRequest::create(&dir, b"x".to_vec());
        req.allow_empty_dir = allow;
        req.guard = Some(guard(&f, Rows::default()).shared());
        let err = apply(&req).unwrap_err();
        assert!(
            matches!(
                err,
                FsError::RefusedOverwrite {
                    kind: TargetKind::Dir,
                    existing_hash: None,
                    ..
                }
            ),
            "{err:?}"
        );
    }
    assert_eq!(tree(&f.agent_dir), snapshot, "no merge, no partial copy");
}

#[test]
fn empty_directory_is_refused_unless_allowed() {
    let f = fixture();
    let dir = f.agent_dir.join("empty");
    fs::create_dir(&dir).unwrap();

    let mut req = WriteRequest::create(&dir, b"x".to_vec());
    req.guard = Some(guard(&f, Rows::default()).shared());
    assert!(matches!(
        apply(&req),
        Err(FsError::RefusedOverwrite {
            kind: TargetKind::Dir,
            ..
        })
    ));
    assert!(dir.is_dir());

    req.allow_empty_dir = true;
    apply(&req).unwrap();
    assert_eq!(fs::read_to_string(&dir).unwrap(), "x");
}

#[test]
fn file_appearing_between_preview_and_rename_is_refused_and_untouched() {
    let f = fixture();
    let target = f.agent_dir.join("racy/SKILL.md");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let mut req = WriteRequest::create(&target, b"mine".to_vec());
    req.guard = Some(guard(&f, Rows::default()).shared());

    let err = apply_with_hook(&req, &mut |stage| {
        if stage == Stage::BeforeRename {
            fs::write(&target, "appeared late").unwrap();
        }
    })
    .unwrap_err();

    assert!(
        matches!(
            err,
            FsError::RefusedOverwrite {
                kind: TargetKind::File,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "appeared late");
    assert_eq!(tree(target.parent().unwrap()), ["SKILL.md"], "temp removed");
}

#[test]
fn parents_are_created_only_when_the_target_is_absent() {
    let f = fixture();
    let absent = f.agent_dir.join("a/b/c/SKILL.md");
    assert!(create_parents_if_target_absent(&absent).unwrap());
    assert!(absent.parent().unwrap().is_dir());
    assert!(!absent.exists());

    let existing = f.agent_dir.join("e/SKILL.md");
    fs::create_dir_all(existing.parent().unwrap()).unwrap();
    fs::write(&existing, "x").unwrap();
    assert!(!create_parents_if_target_absent(&existing).unwrap());

    let blocked = f.agent_dir.join("e/SKILL.md/child");
    assert!(matches!(
        create_parents_if_target_absent(&blocked),
        Err(FsError::InvalidPath { .. })
    ));
}

#[test]
fn move_aside_renames_in_place_and_keeps_every_byte() {
    let f = fixture();
    let file = f.agent_dir.join("SKILL.md");
    fs::write(&file, "precious").unwrap();
    let dir = f.agent_dir.join("folder");
    fs::create_dir_all(dir.join("nested")).unwrap();
    fs::write(dir.join("nested/a.md"), "inside").unwrap();
    let now = datetime!(2026-11-05 17:04:09 -5);

    let one = move_aside(&file, now).unwrap();
    assert_eq!(one.original, file);
    assert_eq!(
        one.moved_to,
        f.agent_dir
            .join("SKILL.md.agentuna-backup-20261105T220409Z")
    );
    assert!(!file.exists());
    assert_eq!(fs::read_to_string(&one.moved_to).unwrap(), "precious");

    let two = move_aside(&dir, now).unwrap();
    assert_eq!(two.moved_to.parent(), dir.parent());
    assert_eq!(
        fs::read_to_string(two.moved_to.join("nested/a.md")).unwrap(),
        "inside"
    );

    // The same name again in the same second never overwrites the earlier backup.
    fs::write(&file, "second").unwrap();
    let three = move_aside(&file, now).unwrap();
    assert_eq!(
        three.moved_to,
        f.agent_dir
            .join("SKILL.md.agentuna-backup-20261105T220409Z.1")
    );
    assert_eq!(fs::read_to_string(&one.moved_to).unwrap(), "precious");
    assert_eq!(fs::read_to_string(&three.moved_to).unwrap(), "second");

    assert!(matches!(
        move_aside(&file, now),
        Err(FsError::NotFound { .. })
    ));
}

#[test]
fn after_moving_aside_the_deployment_proceeds() {
    let f = fixture();
    let skill = f.agent_dir.join("SKILL.md");
    fs::write(&skill, "hand written").unwrap();
    let g = guard(&f, Rows::default()).shared();

    let mut req = WriteRequest::create(&skill, b"deployed".to_vec());
    req.guard = Some(g);
    assert!(matches!(apply(&req), Err(FsError::RefusedOverwrite { .. })));

    let moved = move_aside(&skill, datetime!(2026-11-05 12:00:00 UTC)).unwrap();
    apply(&req).unwrap();
    assert_eq!(fs::read_to_string(&skill).unwrap(), "deployed");
    assert_eq!(fs::read_to_string(moved.moved_to).unwrap(), "hand written");
}
