//! Reviewed skill deletion Tauri command backend (TS-014).
//!
//! Two-phase safe deletion: preview plans the complete deletion ChangeSet and caches
//! it; apply validates the typed confirmation, executes through the journaled safe-write
//! pipeline with full backups, and removes the item row while archiving any source row.
//! The generic `operation_undo` command can restore the deleted skill byte-identically.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use at_engine::skill_delete::{
    DeleteSkill, DeleteSkillError, DeleteSkillPlan, DeleteSkillRequest,
    apply_delete_skill_with_registry, plan_delete_skill,
};
use at_model::ItemKind;
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::StoreManifest;
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::AppState;
use super::commands::{CommandError, CommandErrorCode};
use super::paths::resolve_command_paths;

/// Request body for previewing a skill deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDeletePreviewRequest {
    /// Stored item identifier.
    pub item_id: String,
    /// Owning project id when the item is in Project or Local scope.
    pub project_id: Option<String>,
}

/// Lower-precedence copy that becomes effective after deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDeleteNextEffectiveView {
    /// Item identifier of the remaining copy.
    pub item_id: String,
    /// Slug of the remaining copy.
    pub slug: String,
    /// Scope of the remaining copy.
    pub scope: String,
}

/// Presentation review of a planned skill deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDeletePreviewView {
    /// Content-bound preview identifier for apply concurrency protection.
    pub preview_id: String,
    /// Canonical folder path being deleted.
    pub path: String,
    /// Relative files inside the skill package being deleted.
    pub files: Vec<String>,
    /// Number of files inside the package being deleted.
    pub file_count: u64,
    /// Name of the skill to type for confirmation.
    pub name: String,
    /// Scope the skill is being deleted from.
    pub scope: String,
    /// Remaining lower-precedence copy that becomes effective, if any.
    pub next_effective: Option<SkillDeleteNextEffectiveView>,
    /// Native deployment targets being removed.
    pub deployments: Vec<String>,
}

/// Request to apply a reviewed skill deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDeleteApplyRequest {
    /// Preview identifier from [`skill_delete_preview`].
    pub preview_id: String,
    /// Typed skill name or slug for confirmation.
    pub confirmed_name: String,
}

/// Outcome of applying a skill deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDeleteResultView {
    /// Operation identity for generic undo.
    pub operation_id: String,
    /// Folder where the backup was saved.
    pub backup_folder: String,
}

struct CachedDeletePlan {
    plan: DeleteSkillPlan,
    expected_name: String,
    expected_slug: String,
    authorized_roots: Vec<PathBuf>,
    backup_folder: PathBuf,
}

static DELETE_PLANS: LazyLock<Mutex<HashMap<String, CachedDeletePlan>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Previews deleting a skill from its canonical scope.
#[tauri::command]
pub fn skill_delete_preview(
    state: State<'_, AppState>,
    request: SkillDeletePreviewRequest,
) -> Result<SkillDeletePreviewView, CommandError> {
    skill_delete_preview_impl(&state, request)
}

/// Internal implementation over AppState for testing.
pub(crate) fn skill_delete_preview_impl(
    state: &AppState,
    request: SkillDeletePreviewRequest,
) -> Result<SkillDeletePreviewView, CommandError> {
    let item_id: at_model::ItemId = request
        .item_id
        .parse()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item UUID"))?;

    let database = state.database().map_err(CommandError::unavailable)?;
    let items = at_db::shared::items_by_ids(database.connection(), &[item_id])
        .map_err(|_| CommandError::internal("could not read skill index"))?;
    let item = items
        .into_iter()
        .find(|i| i.id == item_id)
        .ok_or_else(|| CommandError::new(CommandErrorCode::NotFound, "skill not found in index"))?;

    if item.kind != ItemKind::Skill {
        return Err(CommandError::invalid_argument(
            "item_id",
            "item is not a skill",
        ));
    }
    if !item.managed {
        return Err(CommandError::new(
            CommandErrorCode::RefusedOverwrite,
            "cannot delete an unmanaged or native skill",
        ));
    }

    let scope = item.scope;

    // Check project root when in Project/Local scope
    let project_root = request.project_id.as_deref().and_then(|pid| {
        at_db::projects::find_project_by_id(database.connection(), pid)
            .ok()
            .flatten()
            .map(|p| PathBuf::from(p.path))
    });

    let paths = resolve_command_paths(state, project_root.as_deref())?;
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("skill store is unavailable"))?;

    let slug = Slug::parse(&item.slug)
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid slug stored for skill"))?;

    // Read manifest
    let manifest_bytes = std::fs::read(layout.manifest_path()).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "store manifest is missing on disk",
        )
    })?;
    let manifest = StoreManifest::parse(&manifest_bytes)
        .map_err(|e| CommandError::internal(format!("failed to parse manifest: {e}")))?;

    let plan_request = DeleteSkillRequest {
        intent: DeleteSkill {
            slug: slug.clone(),
            scope,
        },
        confirmed: true,
        store: &layout,
        manifest: &manifest,
        manifest_bytes: &manifest_bytes,
        connection: database.connection(),
        project_id: request.project_id.as_deref(),
        global_permission: false,
    };

    let plan = plan_delete_skill(plan_request).map_err(|e| match e {
        DeleteSkillError::Locked { reason } => CommandError::new(
            CommandErrorCode::Locked,
            format!("skill is locked and cannot be deleted: {reason}"),
        ),
        DeleteSkillError::RefusedUnmanaged(path) => {
            CommandError::refused_overwrite(&path, at_fs::TargetKind::Dir, None)
        }
        DeleteSkillError::PermissionDenied => CommandError::new(
            CommandErrorCode::Unavailable,
            "deleting from the Global skill store requires admin permission",
        ),
        DeleteSkillError::ConfirmationRequired => CommandError::new(
            CommandErrorCode::InvalidArgument,
            "confirmation is required to delete a skill",
        ),
        other => CommandError::internal(other.to_string()),
    })?;

    let preview_id = plan.change_set.preview_id.clone();

    // Collect relative file paths in the canonical folder
    let mut files = Vec::new();
    if plan.canonical_path.is_dir()
        && let Ok(entries) = std::fs::read_dir(&plan.canonical_path)
    {
        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string() {
                files.push(name);
            }
        }
    }
    files.sort();
    let file_count = files.len() as u64;

    let deployments: Vec<String> = plan
        .deployments
        .iter()
        .filter_map(|d| d.target_path.clone())
        .collect();

    let next_effective = plan
        .next_effective
        .as_ref()
        .map(|next| SkillDeleteNextEffectiveView {
            item_id: next.item_id.to_string(),
            slug: next.slug.as_str().to_owned(),
            scope: next.scope.wire_name().to_owned(),
        });

    let mut authorized_roots = vec![
        layout.root().to_path_buf(),
        layout.backups_dir(),
        paths.backup_dir().to_path_buf(),
    ];
    if let Some(home) = paths.home_dir() {
        authorized_roots.push(home.to_path_buf());
    }
    if let Some(proj) = &project_root {
        authorized_roots.push(proj.clone());
    }
    for cell in &plan.deployments {
        if let Some(target) = cell.target_path.as_deref().map(PathBuf::from) {
            if let Some(parent) = target.parent() {
                authorized_roots.push(parent.to_path_buf());
            }
            authorized_roots.push(target);
        }
    }

    let backup_folder = paths.backup_dir().to_path_buf();
    let cached = CachedDeletePlan {
        plan,
        expected_name: item.name.clone(),
        expected_slug: item.slug.clone(),
        authorized_roots,
        backup_folder,
    };

    DELETE_PLANS
        .lock()
        .map_err(|_| CommandError::internal("preview cache unavailable"))?
        .insert(preview_id.clone(), cached);

    Ok(SkillDeletePreviewView {
        preview_id,
        path: layout.skill_dir(&slug).display().to_string(),
        files,
        file_count,
        name: item.name,
        scope: scope.wire_name().to_owned(),
        next_effective,
        deployments,
    })
}

/// Applies a reviewed skill deletion after typed confirmation.
#[tauri::command]
pub fn skill_delete_apply(
    state: State<'_, AppState>,
    request: SkillDeleteApplyRequest,
) -> Result<SkillDeleteResultView, CommandError> {
    skill_delete_apply_impl(&state, request)
}

/// Internal implementation over AppState for testing.
pub(crate) fn skill_delete_apply_impl(
    state: &AppState,
    request: SkillDeleteApplyRequest,
) -> Result<SkillDeleteResultView, CommandError> {
    let cached = DELETE_PLANS
        .lock()
        .map_err(|_| CommandError::internal("preview cache unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "skill deletion preview expired or not found; preview again",
            )
        })?;

    let typed = request.confirmed_name.trim();
    if typed != cached.expected_name && typed != cached.expected_slug {
        return Err(CommandError::invalid_argument(
            "confirmed_name",
            "confirmation name does not match skill name or slug",
        ));
    }

    at_fs::ensure_dir(&cached.backup_folder)
        .map_err(|e| CommandError::internal(format!("failed to create backup folder: {e}")))?;

    let mut existing_roots = Vec::new();
    for root in cached.authorized_roots {
        if root.is_dir() {
            existing_roots.push(root);
        } else if let Some(parent) = root.parent()
            && parent.is_dir()
        {
            existing_roots.push(parent.to_path_buf());
        }
    }
    let roots = at_fs::ApplyRoots::new(existing_roots).map_err(|e| {
        CommandError::internal(format!("failed to construct authorized roots: {e}"))
    })?;

    let database = state.database().map_err(CommandError::unavailable)?;
    // Capture the index rows the delete removes; undo restores them exactly.
    let undo_metadata = at_migrate::undo_meta::capture(database.connection(), &cached.plan.item_id)
        .map_err(|error| {
            CommandError::internal(format!("could not snapshot the skill index rows: {error}"))
        })?;
    let operations_before = at_migrate::undo_meta::operation_rowid_high_water(
        database.connection(),
    )
    .map_err(|error| {
        CommandError::internal(format!("could not read the operation history: {error}"))
    })?;
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: &cached.plan.store_root,
        backup_folder: &cached.backup_folder,
        recorder: &mut recorder,
    };

    let outcome = apply_delete_skill_with_registry(
        database.connection(),
        &cached.plan,
        &mut context,
        &roots,
        &state.recent_writes(),
    )
    .map_err(|e| match e {
        DeleteSkillError::Apply(at_fs::ChangeSetApplyError::Stale(path)) => CommandError::new(
            CommandErrorCode::Conflict,
            format!(
                "skill files changed at {} since review; preview again",
                path.display()
            ),
        ),
        other => CommandError::internal(other.to_string()),
    })?;

    if let Err(error) = at_migrate::undo_meta::write_for_new_operations(
        database.connection(),
        &cached.backup_folder,
        operations_before,
        &undo_metadata,
    ) {
        tracing::warn!(%error, "skill delete undo metadata could not be written");
    }

    Ok(SkillDeleteResultView {
        operation_id: outcome.operation_id,
        backup_folder: cached.backup_folder.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_db::Database;
    use at_model::Scope;
    use at_store::layout::StoreId;
    use at_store::manifest::{ManifestItem, StorePath, hash_skill_folder};
    use at_store::paths::{AppPaths, PathEnvironment};

    struct TestSandbox {
        _home: at_fs::testkit::TestHome,
        state: AppState,
        item_id: at_model::ItemId,
        name: String,
        skill_dir: PathBuf,
    }

    impl TestSandbox {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let home = at_fs::testkit::TestHome::new()?;
            let paths = AppPaths::resolve(
                &PathEnvironment::default().with_home(home.home().to_path_buf()),
            )?;
            let database_path = home.home().join("agentuna").join("index.db");
            if let Some(parent) = database_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let database = Database::open_file(&database_path)?;
            let user_store = StoreLayout::for_scope(&paths, Scope::User)?;
            std::fs::create_dir_all(user_store.skills_dir())?;

            let slug = Slug::parse("test-delete-skill")?;
            let skill_dir = user_store.skill_dir(&slug);
            std::fs::create_dir_all(&skill_dir)?;
            std::fs::write(
                skill_dir.join("SKILL.md"),
                b"---\nname: Test Delete Skill\ndescription: delete me\n---\n",
            )?;

            let item_id = at_model::ItemId::new();
            let hash = hash_skill_folder(&skill_dir)?;

            let mut manifest = StoreManifest::new(Scope::User, StoreId::new());
            manifest.insert_item(ManifestItem::new(
                item_id,
                ItemKind::Skill,
                slug,
                StorePath::parse("skills/test-delete-skill")?,
                hash,
            ))?;
            let manifest_bytes = manifest.to_bytes()?;
            std::fs::write(user_store.manifest_path(), &manifest_bytes)?;

            database.connection().execute(
                "INSERT INTO items (id, kind, scope, slug, name, source_path, managed) \
                 VALUES (?1, 'skill', 'user', 'test-delete-skill', 'Test Delete Skill', ?2, 1)",
                rusqlite::params![item_id.to_string(), skill_dir.to_string_lossy().as_ref()],
            )?;

            let state = AppState::new(
                std::sync::Arc::new(super::super::bridge::JobBridge::new(
                    std::sync::Arc::new(super::super::bridge::NullSink),
                    std::sync::Arc::new(super::super::bridge::TracingOperationsLog),
                )),
                None,
            )
            .with_database(database)
            .with_paths(paths);

            Ok(Self {
                _home: home,
                state,
                item_id,
                name: "Test Delete Skill".to_string(),
                skill_dir,
            })
        }
    }

    #[test]
    fn skill_delete_preview_and_apply_flow() {
        let sandbox = TestSandbox::new().expect("sandbox");

        let preview = skill_delete_preview_impl(
            &sandbox.state,
            SkillDeletePreviewRequest {
                item_id: sandbox.item_id.to_string(),
                project_id: None,
            },
        )
        .expect("preview succeeds");

        assert_eq!(preview.name, sandbox.name);
        assert_eq!(preview.scope, "user");
        assert_eq!(preview.files, vec!["SKILL.md"]);
        assert_eq!(preview.file_count, 1);
        assert!(sandbox.skill_dir.exists());

        // Apply with mismatched name fails
        let err = skill_delete_apply_impl(
            &sandbox.state,
            SkillDeleteApplyRequest {
                preview_id: preview.preview_id.clone(),
                confirmed_name: "wrong name".to_string(),
            },
        )
        .expect_err("mismatched confirmation must fail");
        assert_eq!(err.code, CommandErrorCode::InvalidArgument);

        // Preview again since previous apply entry was removed or failed
        let preview2 = skill_delete_preview_impl(
            &sandbox.state,
            SkillDeletePreviewRequest {
                item_id: sandbox.item_id.to_string(),
                project_id: None,
            },
        )
        .expect("preview2 succeeds");

        // Apply with matching name succeeds
        let result = skill_delete_apply_impl(
            &sandbox.state,
            SkillDeleteApplyRequest {
                preview_id: preview2.preview_id,
                confirmed_name: sandbox.name.clone(),
            },
        )
        .expect("apply succeeds");

        assert!(!sandbox.skill_dir.exists());

        // Verify item is deleted from DB
        let count: i64 = {
            let db = sandbox.state.database().expect("db");
            db.connection()
                .query_row(
                    "SELECT COUNT(*) FROM items WHERE id = ?1",
                    [sandbox.item_id.to_string()],
                    |row| row.get(0),
                )
                .expect("query")
        };
        assert_eq!(count, 0);

        // Verify generic undo restores it
        let undo = super::super::overwrite::operation_undo_inner(
            &sandbox.state,
            &result.operation_id,
            false,
        )
        .expect("undo succeeds");
        assert!(!undo.undo_operation_id.is_empty());
        assert!(sandbox.skill_dir.exists());
    }

    #[test]
    fn skill_delete_refuses_locked_skill() {
        let sandbox = TestSandbox::new().expect("sandbox");
        {
            let db = sandbox.state.database().expect("db");
            at_db::locks::upsert_lock(
                db.connection(),
                &at_db::locks::LockSpec {
                    item_id: &sandbox.item_id,
                    scope: Scope::User,
                    reason: Some("locked by policy"),
                    locked_by: Some("admin"),
                },
            )
            .expect("insert lock");
        }

        let err = skill_delete_preview_impl(
            &sandbox.state,
            SkillDeletePreviewRequest {
                item_id: sandbox.item_id.to_string(),
                project_id: None,
            },
        )
        .expect_err("locked skill must fail");

        assert_eq!(err.code, CommandErrorCode::Locked);
        assert!(err.message.contains("locked"));
    }
}
