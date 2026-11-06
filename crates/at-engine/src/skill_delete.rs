//! Reviewed skill deletion from one canonical scope, with backup and generic undo.
//!
//! Planning returns a content-bound ChangeSet that moves the canonical folder and every
//! AgenTuna-owned deployment into the operation backup folder; application journals both
//! phases through the at-fs pipeline and then removes the item row, archives the source
//! row and lets the deployment rows cascade. Unmanaged or native folders are refused, and
//! an unconfirmed request performs no writes at all.

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use at_db::shared::DeploymentCell;
use at_model::{ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, OperationKind, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::StoreManifest;
use thiserror::Error;

/// Identifies the skill copy to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteSkill {
    /// Slug of the canonical skill folder in the addressed scope.
    pub slug: Slug,
    /// Canonical scope whose store holds the folder.
    pub scope: Scope,
}

/// Inputs to the read-only skill deletion planner.
pub struct DeleteSkillRequest<'a> {
    /// Skill and scope to delete.
    pub intent: DeleteSkill,
    /// Explicit user confirmation; `false` refuses before any planning or write.
    pub confirmed: bool,
    /// Canonical store layout for `intent.scope`.
    pub store: &'a StoreLayout,
    /// Parsed manifest snapshot of that store.
    pub manifest: &'a StoreManifest,
    /// Exact original manifest bytes, used for concurrency protection.
    pub manifest_bytes: &'a [u8],
    /// Database connection used to snapshot deployment rows and remaining copies.
    pub connection: &'a rusqlite::Connection,
    /// Owning project id when the scope is Project or Local.
    pub project_id: Option<&'a str>,
    /// Whether the caller holds the explicit permission to modify the Global store.
    pub global_permission: bool,
}

/// The copy that becomes the resolution winner after this delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextEffectiveCopy {
    /// Remaining item identity.
    pub item_id: ItemId,
    /// Remaining copy slug.
    pub slug: Slug,
    /// Scope of the remaining copy.
    pub scope: Scope,
    /// Canonical store path of the remaining copy.
    pub path: PathBuf,
}

/// Failure while preparing or applying a skill deletion.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DeleteSkillError {
    /// The request was not confirmed; no planning read and no write happened.
    #[error("confirmation is required to delete a skill")]
    ConfirmationRequired,
    /// The addressed folder is not a managed canonical skill and is never deleted.
    #[error("folder is native or unmanaged and cannot be deleted: {0}")]
    RefusedUnmanaged(PathBuf),
    /// The Global store requires explicit admin permission.
    #[error("permission denied: deleting from the Global skill store requires admin permission")]
    PermissionDenied,
    /// The skill is locked and cannot be deleted.
    #[error("skill is locked and cannot be deleted: {reason}")]
    Locked {
        /// Reason for the lock.
        reason: String,
    },
    /// The manifest has no such skill and no unmanaged folder exists at its path.
    #[error("skill {slug} is missing from the {scope} store manifest")]
    MissingSkill {
        /// Requested slug.
        slug: String,
        /// Addressed scope.
        scope: Scope,
    },
    /// The manifest item is not a skill.
    #[error("manifest item {0} is not a skill")]
    NotASkill(ItemId),
    /// The manifest does not belong to the supplied store scope.
    #[error("manifest scope does not match the addressed store scope")]
    ScopeMismatch,
    /// An invalid manifest path was requested.
    #[error(transparent)]
    StorePath(#[from] at_store::manifest::StorePathError),
    /// Manifest validation or serialization failed.
    #[error(transparent)]
    Manifest(#[from] at_store::manifest::ManifestError),
    /// Invalid expected state or ChangeSet.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// A native deployment cannot be safely planned from its stored row.
    #[error("deployment cannot be planned safely for agent {agent}")]
    DeploymentUnavailable {
        /// Agent with a deployment that cannot be reconstructed.
        agent: at_model::AgentId,
    },
    /// The canonical or native ChangeSet failed to apply.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// A canonical folder could not be inspected safely.
    #[error("could not inspect skill folder {path}: {source}")]
    Filesystem {
        /// Folder being inspected.
        path: PathBuf,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// Database deployment query or identity update failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// Database updates failed after the filesystem phases; files were rolled back.
    #[error("database update failed: {cause}; canonical rollback: {rollback}")]
    Rehome {
        /// Database failure.
        cause: String,
        /// `complete` or the rollback error.
        rollback: String,
    },
}

/// Reviewed deletion ChangeSet with the post-delete resolution winner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteSkillPlan {
    /// Canonical folder, deployment removals and manifest phase.
    pub change_set: ChangeSet,
    /// Deleted item identity.
    pub item_id: ItemId,
    /// Deleted slug.
    pub slug: Slug,
    /// Canonical folder removed by the plan.
    pub canonical_path: PathBuf,
    /// Native deployment rows observed during planning.
    pub deployments: Vec<DeploymentCell>,
    /// Highest-precedence remaining copy that becomes effective, when one exists.
    pub next_effective: Option<NextEffectiveCopy>,
    /// Store identity for apply-time context validation.
    pub store_root: PathBuf,
    /// Scope the deletion addresses.
    pub scope: Scope,
}

/// Durable id of the applied deletion operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteSkillOutcome {
    /// Canonical operation id recorded in the operations log.
    pub operation_id: String,
}

/// Preflights the deletion and returns a content-bound ChangeSet without writing files.
///
/// An unconfirmed request returns [`DeleteSkillError::ConfirmationRequired`] before any
/// filesystem or manifest read. Native and unmanaged folders are refused; deployments are
/// removed into the operation backup without following symlinks; copies are backed up
/// before removal. Version history is retained through the operation rows, file rows and
/// backups row recorded by the apply pipeline, because `item_versions` cascades with the
/// deleted item row.
///
/// # Errors
/// Returns a typed confirmation, permission, unmanaged, manifest or ChangeSet error.
pub fn plan_delete_skill(
    request: DeleteSkillRequest<'_>,
) -> Result<DeleteSkillPlan, DeleteSkillError> {
    if !request.confirmed {
        return Err(DeleteSkillError::ConfirmationRequired);
    }
    if request.store.scope() == Scope::Global && !request.global_permission {
        return Err(DeleteSkillError::PermissionDenied);
    }
    if request.manifest.scope() != request.store.scope() {
        return Err(DeleteSkillError::ScopeMismatch);
    }
    let slug = request.intent.slug.clone();
    let canonical_path = request.store.skill_dir(&slug);
    let Some(item) = request
        .manifest
        .item_by_slug(ItemKind::Skill, &slug)
        .cloned()
    else {
        // No manifest entry: the folder, when it exists, is native or unmanaged.
        if fs::symlink_metadata(&canonical_path).is_ok() {
            return Err(DeleteSkillError::RefusedUnmanaged(canonical_path));
        }
        return Err(DeleteSkillError::MissingSkill {
            slug: slug.as_str().to_owned(),
            scope: request.store.scope(),
        });
    };
    let locks = at_db::locks::locks_for_item(request.connection, &item.id)?;
    if let Some(lock) = locks
        .iter()
        .find(|l| l.scope == request.intent.scope || l.scope == Scope::Global)
    {
        return Err(DeleteSkillError::Locked {
            reason: lock
                .reason
                .clone()
                .unwrap_or_else(|| "locked by policy".to_owned()),
        });
    }
    let deployments = at_db::deployments::list_deployments_for_item(request.connection, &item.id)?;
    let next_effective = remaining_effective_copy(
        request.connection,
        &slug,
        &item.id,
        request.intent.scope,
        request.project_id,
    )?;

    let manifest_path = request.store.manifest_path();
    let mut next_manifest = request.manifest.clone();
    next_manifest.remove_item(item.id);
    let mut ops = vec![FileOp::Delete {
        path: canonical_path.clone(),
        expected: ExpectedState::hash(node_hash(&canonical_path).map_err(|source| {
            DeleteSkillError::Filesystem {
                path: canonical_path.clone(),
                source,
            }
        })?)?,
    }];
    for cell in deployments
        .iter()
        .filter(|cell| cell.state == at_model::ItemState::Linked)
    {
        let Some(target) = cell
            .target_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        else {
            return Err(DeleteSkillError::DeploymentUnavailable {
                agent: cell.agent_id.clone(),
            });
        };
        ops.push(FileOp::Delete {
            expected: ExpectedState::hash(node_hash(&target).map_err(|source| {
                DeleteSkillError::Filesystem {
                    path: target.clone(),
                    source,
                }
            })?)?,
            path: target,
        });
    }
    ops.push(FileOp::Write {
        path: manifest_path,
        bytes: next_manifest.to_bytes()?,
        expected: ExpectedState::hash(at_fs::sha256_hex(request.manifest_bytes))?,
        follow_symlink: false,
        backup: true,
    });

    let operation_id = format!("op_{}", uuid::Uuid::now_v7());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let change_set = ChangeSet {
        id: operation_id,
        preview_id: format!("preview_{}", uuid::Uuid::now_v7()),
        operation_kind: OperationKind::parse("delete")?,
        scope: Some(request.intent.scope),
        project_id: request.project_id.map(str::to_owned),
        summary: format!(
            "delete skill {} from {}",
            slug.as_str(),
            request.intent.scope.wire_name()
        ),
        ops,
        created_at_unix_seconds: now,
    };
    change_set.validate()?;
    Ok(DeleteSkillPlan {
        change_set,
        item_id: item.id,
        slug,
        canonical_path,
        deployments,
        next_effective,
        store_root: request.store.root().to_path_buf(),
        scope: request.intent.scope,
    })
}

/// Applies the reviewed deletion and removes the database identity after filesystem success.
///
/// If the database updates fail, the completed filesystem phases are rolled back before
/// returning. On success the generic TC-010 `undo_operation` can restore every recorded
/// file from the operation backup within the retention window.
///
/// # Errors
/// Returns typed ChangeSet or database errors; [`DeleteSkillError::Rehome`] reports a
/// rollback attempt when database updates fail.
pub fn apply_delete_skill(
    connection: &rusqlite::Connection,
    plan: &DeleteSkillPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
) -> Result<DeleteSkillOutcome, DeleteSkillError> {
    if context.store_root != plan.store_root {
        return Err(at_model::ChangeSetError::InvalidPath {
            path: context.store_root.display().to_string(),
            reason: "apply context must use the addressed store root".to_owned(),
        }
        .into());
    }
    let applied = at_fs::apply_with_roots_reversible(&plan.change_set, context, roots)?;
    let Some(handle) = applied.rollback_handle else {
        return Err(DeleteSkillError::Rehome {
            cause: "apply did not return a rollback token".to_owned(),
            rollback: "not attempted".to_owned(),
        });
    };
    connection
        .execute_batch("SAVEPOINT skill_delete_db")
        .map_err(at_db::DbError::Sqlite)?;
    let database_update = (|| -> Result<(), at_db::DbError> {
        let source_id: Option<String> = connection
            .query_row(
                "SELECT source_id FROM items WHERE id = ?1",
                [plan.item_id.to_string()],
                |row| row.get(0),
            )
            .map_err(at_db::DbError::Sqlite)?;
        at_db::items::delete_item(connection, &plan.item_id)?;
        if let Some(source_id) = source_id.as_deref() {
            at_db::sources::archive_source(connection, source_id, None)?;
        }
        Ok(())
    })();
    if let Err(error) = database_update {
        let rollback_error = connection
            .execute_batch("ROLLBACK TO skill_delete_db; RELEASE skill_delete_db")
            .err();
        let files = match at_fs::rollback_committed(handle) {
            Ok(()) => "complete".to_owned(),
            Err(rollback_failure) => format!("failed: {rollback_failure}"),
        };
        return Err(DeleteSkillError::Rehome {
            cause: match rollback_error {
                Some(rollback_error) => {
                    format!("{error}; database rollback failed: {rollback_error}")
                }
                None => error.to_string(),
            },
            rollback: files,
        });
    }
    connection
        .execute_batch("RELEASE skill_delete_db")
        .map_err(at_db::DbError::Sqlite)?;
    at_fs::commit_reversible(handle)?;
    Ok(DeleteSkillOutcome {
        operation_id: applied.operation_id,
    })
}

/// Finds the highest-precedence remaining copy of the slug that becomes effective.
///
/// Remaining Project and Local rows only qualify when they belong to the same project.
/// A tombstoned (disabled) scope is not considered because the tombstone suppresses the
/// copy at resolution time.
fn remaining_effective_copy(
    connection: &rusqlite::Connection,
    slug: &Slug,
    deleted_id: &ItemId,
    deleted_scope: Scope,
    project_id: Option<&str>,
) -> Result<Option<NextEffectiveCopy>, at_db::DbError> {
    let mut statement = connection.prepare(
        "SELECT id, scope, source_path FROM items \
         WHERE kind = 'skill' AND slug = ?1 AND id != ?2",
    )?;
    let rows = statement.query_map(
        rusqlite::params![slug.as_str(), deleted_id.to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )?;
    let mut best: Option<(u8, ItemId, Scope, Option<String>)> = None;
    for row in rows {
        let (id_text, scope_text, path_text) = row?;
        let Ok(id) = ItemId::from_str(&id_text) else {
            continue;
        };
        let Ok(scope) = Scope::from_str(&scope_text) else {
            continue;
        };
        // Copies belonging to a different project never shadow this project's resolution.
        if matches!(scope, Scope::Project | Scope::Local) {
            let stored_project: Option<String> = connection
                .query_row(
                    "SELECT project_id FROM items WHERE id = ?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .map_err(at_db::DbError::Sqlite)?;
            let same_project = match (stored_project.as_deref(), project_id) {
                (Some(stored), Some(requested)) => stored == requested,
                (None, _) | (_, None) => true,
            };
            if !same_project {
                continue;
            }
        }
        let candidate = (scope.precedence(), id, scope, path_text);
        if best
            .as_ref()
            .is_none_or(|(best_precedence, _, _, _)| candidate.0 > *best_precedence)
        {
            best = Some(candidate);
        }
    }
    let _ = deleted_scope;
    Ok(
        best.map(|(_, item_id, scope, path_text)| NextEffectiveCopy {
            item_id,
            slug: slug.clone(),
            scope,
            path: PathBuf::from(path_text.unwrap_or_default()),
        }),
    )
}

/// Computes the filesystem ChangeSet node digest used for directory preconditions.
///
/// This mirrors at-fs's node digest: sorted child names, NUL separators, child digests and
/// `0xff` separators. Symlinks hash their recorded target.
fn node_hash(path: &Path) -> std::io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        return fs::read(path).map(|bytes| at_fs::sha256_hex(&bytes));
    }
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend_from_slice(entry.file_name().as_encoded_bytes());
            bytes.push(0);
            bytes.extend_from_slice(node_hash(&entry.path())?.as_bytes());
            bytes.push(0xff);
        }
        return Ok(at_fs::sha256_hex(&bytes));
    }
    if metadata.file_type().is_symlink() {
        return fs::read_link(path)
            .map(|target| at_fs::sha256_hex(target.as_os_str().as_encoded_bytes()));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "unsupported skill filesystem entry",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_db::deployments::{DeploymentSpec, upsert_deployment};
    use at_model::{AgentId, ItemState};
    use at_store::{
        layout::StoreId,
        manifest::{StorePath, hash_skill_folder},
    };

    const SKILL_MD: &[u8] = b"---\nname: review\ndescription: Review changes\n---\n";

    struct Fixture {
        _temp: tempfile::TempDir,
        database: at_db::Database,
        store: StoreLayout,
        manifest: StoreManifest,
        manifest_bytes: Vec<u8>,
        id: ItemId,
        slug: Slug,
        backup_dir: PathBuf,
    }

    /// Creates one store with a managed `review` skill and its item row.
    fn fixture(scope: Scope) -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let database = at_db::Database::open_in_memory().expect("database");
        let store = StoreLayout::new(scope, temp.path().join("store"));
        std::fs::create_dir_all(store.skill_dir(&Slug::parse("review").expect("slug")))
            .expect("skill dirs");
        std::fs::write(
            store
                .skill_dir(&Slug::parse("review").expect("slug"))
                .join("SKILL.md"),
            SKILL_MD,
        )
        .expect("skill file");
        let id = ItemId::new();
        let slug = Slug::parse("review").expect("slug");
        let hash = hash_skill_folder(&store.skill_dir(&slug)).expect("hash");
        let mut manifest = StoreManifest::new(scope, StoreId::new());
        manifest
            .insert_item(at_store::manifest::ManifestItem::new(
                id,
                ItemKind::Skill,
                slug.clone(),
                StorePath::parse("skills/review").expect("path"),
                hash,
            ))
            .expect("item");
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name, source_path) \
                 VALUES (?1, 'skill', ?2, 'review', 'review', 'skills/review')",
                rusqlite::params![id.to_string(), scope.wire_name()],
            )
            .expect("item row");
        let manifest_bytes = manifest.to_bytes().expect("manifest bytes");
        std::fs::write(store.manifest_path(), &manifest_bytes).expect("manifest write");
        let backup_dir = store.backups_dir();
        std::fs::create_dir_all(&backup_dir).expect("backup directory");
        Fixture {
            _temp: temp,
            database,
            store,
            manifest,
            manifest_bytes,
            id,
            slug,
            backup_dir,
        }
    }

    fn request<'a>(
        fixture: &'a Fixture,
        confirmed: bool,
        project_id: Option<&'a str>,
    ) -> DeleteSkillRequest<'a> {
        DeleteSkillRequest {
            intent: DeleteSkill {
                slug: fixture.slug.clone(),
                scope: fixture.store.scope(),
            },
            confirmed,
            store: &fixture.store,
            manifest: &fixture.manifest,
            manifest_bytes: &fixture.manifest_bytes,
            connection: fixture.database.connection(),
            project_id,
            global_permission: false,
        }
    }

    fn apply_context<'a>(
        fixture: &'a Fixture,
        recorder: &'a mut at_fs::DbOperationRecorder<'a>,
    ) -> (at_fs::ApplyContext<'a>, at_fs::ApplyRoots) {
        let context = at_fs::ApplyContext {
            store_root: fixture.store.root(),
            backup_folder: &fixture.backup_dir,
            recorder,
        };
        let root = fixture._temp.path().canonicalize().expect("canonical root");
        let roots = at_fs::ApplyRoots::new([root]).expect("authorized root");
        (context, roots)
    }

    fn deployment_row(fixture: &Fixture, agent: &AgentId, mode: &str, target: &Path) {
        let target_text = target.to_string_lossy().into_owned();
        let spec =
            DeploymentSpec::new(&fixture.id, agent, fixture.store.scope(), ItemState::Linked)
                .with_mode(mode)
                .with_target_path(&target_text)
                .with_operation("op_seed");
        upsert_deployment(fixture.database.connection(), &spec).expect("deployment row");
    }

    #[test]
    fn unconfirmed_request_refuses_before_any_read_or_write() {
        let fixture = fixture(Scope::User);
        let error = plan_delete_skill(request(&fixture, false, None))
            .expect_err("unconfirmed delete must refuse");
        assert!(matches!(error, DeleteSkillError::ConfirmationRequired));
        assert!(
            fixture
                .store
                .skill_dir(&fixture.slug)
                .join("SKILL.md")
                .exists()
        );
    }

    #[test]
    fn unmanaged_folder_is_refused_with_its_path() {
        let fixture = fixture(Scope::User);
        let unmanaged = fixture.store.skill_dir(&fixture.slug);
        // Remove the manifest entry so the on-disk folder is unmanaged.
        let bare = StoreManifest::new(Scope::User, StoreId::new());
        let bytes = bare.to_bytes().expect("manifest bytes");
        let error = plan_delete_skill(DeleteSkillRequest {
            manifest: &bare,
            manifest_bytes: &bytes,
            ..request(&fixture, true, None)
        })
        .expect_err("unmanaged folder");
        assert!(
            matches!(error, DeleteSkillError::RefusedUnmanaged(ref path) if path == &unmanaged)
        );
    }

    #[test]
    fn global_store_requires_permission_before_planning() {
        let fixture = fixture(Scope::Global);
        let request = DeleteSkillRequest {
            global_permission: false,
            ..request(&fixture, true, None)
        };
        assert!(matches!(
            plan_delete_skill(request),
            Err(DeleteSkillError::PermissionDenied)
        ));
    }
    #[test]
    fn locked_skill_refuses_delete() {
        let fixture = fixture(Scope::User);
        at_db::locks::upsert_lock(
            fixture.database.connection(),
            &at_db::locks::LockSpec {
                item_id: &fixture.id,
                scope: Scope::User,
                reason: Some("locked by team"),
                locked_by: Some("admin"),
            },
        )
        .expect("upsert lock");
        let error = plan_delete_skill(request(&fixture, true, None)).expect_err("locked skill");
        assert!(
            matches!(error, DeleteSkillError::Locked { ref reason } if reason == "locked by team")
        );
    }

    #[test]
    fn delete_backs_up_package_and_undo_restores_it_byte_identically() {
        let fixture = fixture(Scope::User);
        let plan = plan_delete_skill(request(&fixture, true, None)).expect("delete plan");
        let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
        let (mut context, roots) = apply_context(&fixture, &mut recorder);
        let outcome =
            apply_delete_skill(fixture.database.connection(), &plan, &mut context, &roots)
                .expect("apply delete");

        assert!(!fixture.store.skill_dir(&fixture.slug).exists());
        let backup_root = fixture.store.backups_dir();
        let backups = at_db::backups::backups_for_operation(
            fixture.database.connection(),
            &outcome.operation_id,
        )
        .expect("backup rows");
        let canonical_backup = backups
            .iter()
            .find(|backup| backup.original_path.as_deref() == Some("skills/review"))
            .or_else(|| {
                backups.iter().find(|backup| {
                    backup
                        .original_path
                        .as_deref()
                        .is_some_and(|path| path.ends_with("skills/review"))
                })
            })
            .expect("a backups row for the canonical folder");
        let backup_blob = canonical_backup
            .backup_path
            .as_deref()
            .expect("backup blob");
        let restored_bytes =
            std::fs::read(Path::new(backup_blob).join("SKILL.md")).expect("backup content");
        assert_eq!(restored_bytes, SKILL_MD);

        // TC-010 generic undo restores the folder byte-identically.
        let undo = at_migrate::undo::undo_operation(
            at_migrate::undo::UndoOperationRequest {
                connection: fixture.database.connection(),
                store_root: fixture.store.root(),
                backup_root: &backup_root,
                authorized_roots: &roots,
                actor: at_db::ops::OperationActor::Ui,
                force: false,
            },
            &outcome.operation_id,
        )
        .expect("undo");
        assert!(matches!(
            undo,
            at_migrate::undo::UndoOutcome::Restored { .. }
        ));
        let restored = std::fs::read(fixture.store.skill_dir(&fixture.slug).join("SKILL.md"))
            .expect("restored skill file");
        assert_eq!(restored, SKILL_MD);
    }

    #[test]
    fn deployments_are_removed_with_symlink_targets_intact_and_rows_cascaded() {
        let fixture = fixture(Scope::User);
        let agent_root = fixture._temp.path().join("agents");
        let link_target_dir = agent_root.join("codex-skills-store");
        std::fs::create_dir_all(&link_target_dir).expect("link target");
        let deployed_link = agent_root.join("codex/review");
        std::fs::create_dir_all(deployed_link.parent().expect("parent")).expect("agent dir");
        std::os::unix::fs::symlink(&link_target_dir, &deployed_link).expect("symlink");
        let deployed_copy = agent_root.join("claude/review");
        std::fs::create_dir_all(&deployed_copy).expect("copy dir");
        std::fs::write(deployed_copy.join("SKILL.md"), SKILL_MD).expect("copy content");
        deployment_row(&fixture, &AgentId::Codex, "symlink", &deployed_link);
        deployment_row(&fixture, &AgentId::ClaudeCode, "copy", &deployed_copy);

        let plan = plan_delete_skill(request(&fixture, true, None)).expect("delete plan");
        assert_eq!(plan.deployments.len(), 2);
        let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
        let (mut context, roots) = apply_context(&fixture, &mut recorder);
        apply_delete_skill(fixture.database.connection(), &plan, &mut context, &roots)
            .expect("apply delete");

        // The symlink itself is gone (renamed into the backup), its target untouched.
        assert!(!deployed_link.exists());
        assert!(link_target_dir.exists());
        // The copy is removed from the agent folder but preserved in the operation backup.
        assert!(!deployed_copy.exists());
        let linked_rows: i64 = fixture
            .database
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM deployments WHERE item_id = ?1",
                [fixture.id.to_string()],
                |row| row.get(0),
            )
            .expect("deployment count");
        assert_eq!(linked_rows, 0, "deployment rows cascade with the item row");
        let items: i64 = fixture
            .database
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM items WHERE id = ?1",
                [fixture.id.to_string()],
                |row| row.get(0),
            )
            .expect("item count");
        assert_eq!(items, 0);
    }

    #[test]
    fn deleting_user_copy_reports_project_copy_as_next_effective() {
        let temp = tempfile::tempdir().expect("tempdir");
        let database = at_db::Database::open_in_memory().expect("database");
        let user_store = StoreLayout::new(Scope::User, temp.path().join("user"));
        let project_store = StoreLayout::new(Scope::Project, temp.path().join("project"));
        let slug = Slug::parse("review").expect("slug");
        let user_id = ItemId::new();
        let project_id = ItemId::new();
        let mut user_manifest = StoreManifest::new(Scope::User, StoreId::new());
        let mut project_manifest = StoreManifest::new(Scope::Project, StoreId::new());
        for (store, id, manifest) in [
            (&user_store, &user_id, &mut user_manifest),
            (&project_store, &project_id, &mut project_manifest),
        ] {
            std::fs::create_dir_all(store.skill_dir(&slug)).expect("skill dirs");
            std::fs::write(store.skill_dir(&slug).join("SKILL.md"), SKILL_MD).expect("skill file");
            let hash = hash_skill_folder(&store.skill_dir(&slug)).expect("hash");
            manifest
                .insert_item(at_store::manifest::ManifestItem::new(
                    *id,
                    ItemKind::Skill,
                    slug.clone(),
                    StorePath::parse("skills/review").expect("path"),
                    hash,
                ))
                .expect("item");
        }
        let connection = database.connection();
        for (id, scope) in [(&user_id, Scope::User), (&project_id, Scope::Project)] {
            connection
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name, source_path) \
                     VALUES (?1, 'skill', ?2, 'review', 'review', 'skills/review')",
                    rusqlite::params![id.to_string(), scope.wire_name()],
                )
                .expect("item row");
        }
        let user_bytes = user_manifest.to_bytes().expect("manifest bytes");
        std::fs::write(user_store.manifest_path(), &user_bytes).expect("manifest write");
        let project_bytes = project_manifest.to_bytes().expect("manifest bytes");
        std::fs::write(project_store.manifest_path(), &project_bytes).expect("manifest write");
        let plan = plan_delete_skill(DeleteSkillRequest {
            intent: DeleteSkill {
                slug: slug.clone(),
                scope: Scope::User,
            },
            confirmed: true,
            store: &user_store,
            manifest: &user_manifest,
            manifest_bytes: &user_bytes,
            connection,
            project_id: None,
            global_permission: false,
        })
        .expect("delete plan");
        let next = plan.next_effective.expect("a remaining copy exists");
        assert_eq!(next.item_id, project_id);
        assert_eq!(next.scope, Scope::Project);
        assert_eq!(next.path, PathBuf::from("skills/review"));
        assert_eq!(next.slug, slug);
    }

    #[test]
    fn source_row_is_archived_when_present() {
        let fixture = fixture(Scope::User);
        fixture
            .database
            .connection()
            .execute(
                "INSERT INTO sources (id, item_id, kind) VALUES ('src_test', ?1, 'folder')",
                rusqlite::params![fixture.id.to_string()],
            )
            .expect("source row");
        fixture
            .database
            .connection()
            .execute(
                "UPDATE items SET source_id = 'src_test' WHERE id = ?1",
                rusqlite::params![fixture.id.to_string()],
            )
            .expect("link source");
        let plan = plan_delete_skill(request(&fixture, true, None)).expect("delete plan");
        let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
        let (mut context, roots) = apply_context(&fixture, &mut recorder);
        apply_delete_skill(fixture.database.connection(), &plan, &mut context, &roots)
            .expect("apply delete");
        let deleted_at: Option<String> = fixture
            .database
            .connection()
            .query_row(
                "SELECT deleted_at FROM sources WHERE id = 'src_test'",
                [],
                |row| row.get(0),
            )
            .expect("source row after delete");
        assert!(deleted_at.is_some(), "source row must be archived");
    }
}
