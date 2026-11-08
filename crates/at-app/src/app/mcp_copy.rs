//! Reviewed copy of one MCP server definition into another scope.
//!
//! The canonical definition is copied into the destination store as a new item and projected
//! into that scope's native documents through the shared planner. One merged at-fs ChangeSet is
//! applied, so one journaled operation — and one undo — covers every file the copy wrote.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::{LazyLock, Mutex},
};

use at_engine::mcp_plan::{
    McpAgentRefusal, McpCopyResolution, McpItemAction, McpItemPlan, McpItemPlanRequest,
    McpPlanError,
};
use at_model::{AgentId, ChangeSet, ItemId, ItemKind, ItemState, Scope};
use at_store::{
    layout::{Slug, StoreLayout},
    manifest::StoreManifest,
    mcp_store::McpStore,
    paths::AppPaths,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use ts_rs::TS;

use super::{
    AppState,
    commands::{CommandError, CommandErrorCode},
    mcp_servers::project_of,
    paths::resolve_command_paths,
};

/// Parameters used to preview copying one MCP server into another scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpCopyPreviewRequest {
    /// Canonical item id from the server row.
    pub item_id: String,
    /// Scope of the definition being copied.
    pub scope: String,
    /// Registered project identity for project and local scopes.
    pub project_id: Option<String>,
    /// Destination scope.
    pub target_scope: String,
    /// Explicit collision decision (`replace` or `rename`), absent until the user chooses.
    pub on_collision: Option<String>,
    /// Lowercase slug for a renamed copy.
    pub new_slug: Option<String>,
}

/// One destination agent whose native document the copy changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpCopyEffectView {
    /// Agent identity.
    pub agent: String,
    /// Native config path.
    pub path: String,
    /// `create` when the document does not exist yet, `edit` when it does.
    pub effect: String,
}

/// One destination agent the copy refuses, with the reason to show the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpCopyRefusalView {
    /// Agent identity.
    pub agent: String,
    /// Human-readable, secret-free reason.
    pub reason: String,
}

/// Existing destination definition that needs an explicit Replace or Rename decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpCopyCollisionView {
    /// Existing destination slug.
    pub slug: String,
    /// Existing destination item identity.
    pub item_id: String,
    /// Destination scope holding the existing definition.
    pub scope: String,
}

/// Content-bound review of a copy-to-scope operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpCopyPreviewView {
    /// Reviewed plan identity, absent while a collision waits for a decision.
    pub preview_id: Option<String>,
    /// Server display name.
    pub name: String,
    /// Source scope wire name.
    pub source_scope: String,
    /// Destination scope wire name.
    pub target_scope: String,
    /// Source store slug.
    pub source_slug: String,
    /// Destination store slug after the collision decision.
    pub target_slug: String,
    /// Destination item identity, present once a plan exists.
    pub target_item_id: Option<String>,
    /// Whether the copy replaces an existing destination definition.
    pub replaces_existing: bool,
    /// Backup root that receives the operation folder.
    pub backup_location: String,
    /// Every file the reviewed plan writes.
    pub files: Vec<String>,
    /// Per-agent effects of the reviewed plan.
    pub agents: Vec<McpCopyEffectView>,
    /// Destination agents refused, and why.
    pub refusals: Vec<McpCopyRefusalView>,
    /// Non-fatal planner warnings.
    pub warnings: Vec<String>,
    /// Existing destination definition, when the user must choose Replace or Rename.
    pub collision: Option<McpCopyCollisionView>,
    /// Store creation the same reviewed plan performs first, when the destination has no store.
    pub store_init: Option<super::store_setup::StoreSetupView>,
}

/// Parameters used to apply a reviewed copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpCopyApplyRequest {
    /// Preview identity returned by [`mcp_server_copy_preview`].
    pub preview_id: String,
}

/// Result of applying a reviewed copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpCopyApplyView {
    /// Durable operation identity for undo and history.
    pub operation_id: String,
    /// Destination item identity.
    pub target_item_id: String,
    /// Destination store slug.
    pub target_slug: String,
    /// Number of files the reviewed plan wrote.
    pub files_changed: usize,
    /// Backup root containing the operation folder.
    pub backup_location: String,
}

struct CachedCopy {
    merged: ChangeSet,
    preview_set: ChangeSet,
    preview_root: PathBuf,
    preview_id: String,
    authorized_roots: Vec<PathBuf>,
    store_root: PathBuf,
    backup_folder: PathBuf,
    target_scope: Scope,
    target_project_id: Option<String>,
    target_layout_root: PathBuf,
    target_store_file: PathBuf,
    target_slug: String,
    agents: Vec<(AgentId, PathBuf)>,
}

static COPY_PLANS: LazyLock<Mutex<std::collections::HashMap<String, CachedCopy>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Plans a copy of one MCP server into another scope and caches its exact operations.
#[tauri::command]
pub async fn mcp_server_copy_preview(
    app: AppHandle,
    request: McpCopyPreviewRequest,
) -> Result<McpCopyPreviewView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        mcp_server_copy_preview_impl(&state, request)
    })
    .await
}

/// Internal implementation over injected state for tests.
pub(crate) fn mcp_server_copy_preview_impl(
    state: &AppState,
    request: McpCopyPreviewRequest,
) -> Result<McpCopyPreviewView, CommandError> {
    let scope = parse_scope(&request.scope)?;
    let target_scope = parse_scope(&request.target_scope)?;
    let item_id = ItemId::from_str(&request.item_id)
        .map_err(|_| CommandError::invalid_argument("item_id", "expected a canonical item id"))?;
    let on_collision =
        parse_collision(request.on_collision.as_deref(), request.new_slug.as_deref())?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = project_of(&database, request.project_id.as_deref())?;
    let project_id = project.as_ref().map(|row| row.id.clone());
    // The source project and the destination project are independent: copying out of Project
    // into User must not stamp the source project id on the User item.
    let target_project_id = matches!(target_scope, Scope::Project | Scope::Local)
        .then(|| project_id.clone())
        .flatten();
    if (matches!(scope, Scope::Project | Scope::Local)
        || matches!(target_scope, Scope::Project | Scope::Local))
        && project_id.is_none()
    {
        return Err(CommandError::invalid_argument(
            "project_id",
            "select a project before copying into or out of a project scope",
        ));
    }
    let paths = resolve_command_paths(state, project.as_ref().map(|row| Path::new(&row.path)))?;
    let source_layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::invalid_argument("scope", "canonical store is unavailable"))?;
    let manifest = StoreManifest::load(&source_layout).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "server is missing or was deleted externally",
        )
    })?;
    let item = manifest.manifest.item(item_id).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "server is missing or was deleted externally",
        )
    })?;
    let loaded = McpStore::load(&source_layout).map_err(|_| {
        CommandError::new(CommandErrorCode::NotFound, "server definition is missing")
    })?;
    let definition = loaded.store.get(item.slug.as_str()).ok_or_else(|| {
        CommandError::new(CommandErrorCode::NotFound, "server definition is missing")
    })?;
    let name = definition.display_name.clone();
    let source_slug = item.slug.as_str().to_owned();
    let locked_reason = at_db::locks::locks_for_item(database.connection(), &item_id)
        .map_err(|error| CommandError::internal(format!("cannot read locks: {error}")))?
        .into_iter()
        .next()
        .map(|lock| {
            lock.reason.unwrap_or_else(|| {
                format!("this server is locked at {} scope", lock.scope.wire_name())
            })
        });
    let installed = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|error| CommandError::internal(format!("cannot read agents: {error}")))?
        .into_iter()
        .filter(|agent| agent.presence != at_db::agents::AgentPresence::NotInstalled)
        .map(|agent| agent.id)
        .collect::<Vec<_>>();
    let planned = at_engine::mcp_plan::plan_item(McpItemPlanRequest {
        paths: &paths,
        scope,
        project_id: project_id.clone(),
        item_id,
        action: McpItemAction::CopyToScope {
            target_scope,
            project_id: target_project_id.clone(),
            on_collision,
        },
        installed: &installed,
        deployed: &[],
        locked_reason: locked_reason.as_deref(),
    });
    let planned = match planned {
        Ok(planned) => planned,
        Err(McpPlanError::Collision {
            slug,
            item_id,
            scope: collision_scope,
        }) => {
            return Ok(McpCopyPreviewView {
                preview_id: None,
                name,
                source_scope: scope.wire_name().to_owned(),
                target_scope: target_scope.wire_name().to_owned(),
                source_slug: source_slug.clone(),
                target_slug: slug.clone(),
                target_item_id: None,
                replaces_existing: false,
                backup_location: paths.backup_dir().display().to_string(),
                files: Vec::new(),
                agents: Vec::new(),
                refusals: Vec::new(),
                warnings: Vec::new(),
                collision: Some(McpCopyCollisionView {
                    slug,
                    item_id: item_id.to_string(),
                    scope: collision_scope.wire_name().to_owned(),
                }),
                store_init: None,
            });
        }
        Err(error) => return Err(copy_error(error)),
    };
    let copy = planned
        .copy
        .clone()
        .ok_or_else(|| CommandError::internal("the copy plan lost its destination identity"))?;
    let target_layout = StoreLayout::for_scope(&paths, target_scope).map_err(|_| {
        CommandError::invalid_argument("target_scope", "destination store is unavailable")
    })?;
    let mut merged = planned.plan.merged().map_err(|error| {
        CommandError::invalid_argument("server", &format!("the copy cannot be previewed: {error}"))
    })?;
    // A destination scope that has no store yet gets its skeleton in this same reviewed set:
    // the copy and its store land as one operation, and the preview lists the creation first.
    let store_init = at_store::store_init::plan_store_init(&target_layout, &mut merged)
        .map_err(|error| super::store_setup::store_init_error(&target_layout, error))?;
    let (preview_set, preview_root) = at_engine::preview::relative_to_common_root(&merged)
        .map_err(|error| {
            CommandError::invalid_argument(
                "server",
                &format!("the copy cannot be previewed: {error}"),
            )
        })?;
    let preview = at_engine::preview::preview(&preview_set, &preview_root).map_err(|error| {
        CommandError::invalid_argument("server", &format!("the copy cannot be previewed: {error}"))
    })?;
    let preview_id = preview.id.clone();
    let files = merged
        .ops
        .iter()
        .filter_map(|operation| operation.destination())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let native_paths = planned
        .plan
        .native
        .as_ref()
        .map(|set| {
            set.ops
                .iter()
                .filter_map(|operation| operation.destination().map(|path| path.to_path_buf()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let agents = planned
        .agents
        .iter()
        .cloned()
        .zip(native_paths.iter().cloned())
        .map(|(agent, path)| McpCopyEffectView {
            agent: agent.to_string(),
            effect: if path.exists() { "edit" } else { "create" }.to_owned(),
            path: path.display().to_string(),
        })
        .collect::<Vec<_>>();
    let refusals = copy
        .refused_agents
        .iter()
        .map(refusal_view)
        .collect::<Vec<_>>();
    let warnings = planned.plan.warnings.clone();
    let target_store_file = target_layout.mcp_file();
    let target_layout_root = target_layout.root().to_path_buf();
    let backup_folder = paths.backup_dir().to_path_buf();
    let cache = CachedCopy {
        merged,
        preview_set,
        preview_root,
        preview_id: preview_id.clone(),
        authorized_roots: authorized_roots(&paths, &source_layout, &target_store_file),
        store_root: paths.support_dir().to_path_buf(),
        backup_folder: backup_folder.clone(),
        target_scope,
        target_project_id,
        target_layout_root,
        target_store_file,
        target_slug: copy.target_slug.clone(),
        agents: agents_targets(&planned),
    };
    COPY_PLANS
        .lock()
        .map_err(|_| CommandError::internal("the MCP copy cache is unavailable"))?
        .insert(preview_id.clone(), cache);
    Ok(McpCopyPreviewView {
        preview_id: Some(preview_id),
        name,
        source_scope: copy.source_scope.wire_name().to_owned(),
        target_scope: copy.target_scope.wire_name().to_owned(),
        source_slug,
        target_slug: copy.target_slug,
        target_item_id: Some(copy.target_item_id.to_string()),
        replaces_existing: copy.replaced,
        backup_location: backup_folder.display().to_string(),
        files,
        agents,
        refusals,
        warnings,
        collision: None,
        store_init: store_init
            .as_ref()
            .map(super::store_setup::StoreSetupView::from_plan),
    })
}

/// Applies the exact cached copy plan returned by [`mcp_server_copy_preview`].
#[tauri::command]
pub async fn mcp_server_copy_apply(
    app: AppHandle,
    request: McpCopyApplyRequest,
) -> Result<McpCopyApplyView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        mcp_server_copy_apply_impl(&state, request)
    })
    .await
}

/// Internal implementation over injected state for tests.
pub(crate) fn mcp_server_copy_apply_impl(
    state: &AppState,
    request: McpCopyApplyRequest,
) -> Result<McpCopyApplyView, CommandError> {
    let cached = COPY_PLANS
        .lock()
        .map_err(|_| CommandError::internal("the MCP copy cache is unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument("preview_id", "copy preview expired; preview again")
        })?;
    let current =
        at_engine::preview::preview(&cached.preview_set, &cached.preview_root).map_err(|_| {
            CommandError::external_write_conflict("reviewed files could not be re-read")
        })?;
    if current.id != cached.preview_id {
        return Err(CommandError::external_write_conflict(
            "a reviewed file changed outside AgenTuna",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let connection = database.connection();
    let roots = at_fs::ApplyRoots::new(cached.authorized_roots.clone())
        .map_err(|_| CommandError::unavailable("authorized configuration roots are unavailable"))?;
    let outcome = {
        let mut recorder = at_fs::DbOperationRecorder::new(connection, 30);
        let mut context = at_fs::ApplyContext {
            store_root: &cached.store_root,
            backup_folder: &cached.backup_folder,
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots_and_registry(
            &cached.merged,
            &mut context,
            &roots,
            &state.recent_writes(),
        )
        .map_err(CommandError::from)?
    };
    // The copied item is only usable once the derived index knows it: the MCP list, the row
    // actions and the drift baselines all read those rows.
    let target_layout = StoreLayout::new(cached.target_scope, cached.target_layout_root.clone());
    let manifest = StoreManifest::load(&target_layout).map_err(|error| {
        CommandError::internal(format!("the copied item could not be read back: {error}"))
    })?;
    let slug = Slug::parse(&cached.target_slug)
        .map_err(|_| CommandError::internal("the copied store slug is invalid"))?;
    let item = manifest
        .manifest
        .item_by_slug(ItemKind::McpServer, &slug)
        .cloned()
        .ok_or_else(|| CommandError::internal("the copied server has no store.json record"))?;
    let target_item_id = at_engine::skill_cli::sync_manifest_item(
        connection,
        &at_engine::skill_cli::ManifestItemInput {
            scope: cached.target_scope,
            project_id: cached.target_project_id.as_deref(),
            item: &item,
        },
    )
    .map_err(|error| {
        CommandError::internal(format!("the copied server could not be indexed: {error}"))
    })?;
    let store_hash = std::fs::read(&cached.target_store_file)
        .ok()
        .map(|bytes| at_fs::sha256_hex(&bytes));
    for (agent, path) in &cached.agents {
        let target_hash = std::fs::read(path)
            .ok()
            .map(|bytes| at_fs::sha256_hex(&bytes));
        let hashes = at_db::deployments::DeploymentHashes::new(
            store_hash.as_deref(),
            target_hash.as_deref(),
            target_hash.as_deref(),
        );
        let path_text = path.to_string_lossy().into_owned();
        let mut spec = at_db::deployments::DeploymentSpec::applied(
            &target_item_id,
            agent,
            cached.target_scope,
            ItemState::Linked,
            Some("native"),
            Some(&path_text),
            hashes,
        )
        .with_operation(&outcome.operation_id);
        if let Some(project) = cached.target_project_id.as_deref() {
            spec = spec.with_project(project);
        }
        at_db::deployments::upsert_deployment(connection, &spec).map_err(|error| {
            CommandError::internal(format!(
                "the copy deployment could not be recorded: {error}"
            ))
        })?;
    }
    // The destination document now holds one more server: refresh the canonical hash on every
    // row of the destination scope, not only on the copied one.
    if let Ok(bytes) = std::fs::read(&cached.target_store_file) {
        at_db::deployments::update_store_hash_for_scope(
            connection,
            at_model::ItemKind::McpServer,
            cached.target_scope,
            cached.target_project_id.as_deref(),
            &at_fs::sha256_hex(&bytes),
        )
        .map_err(|error| {
            CommandError::internal(format!(
                "the destination store hashes could not be refreshed: {error}"
            ))
        })?;
    }
    // The native documents now hold what the plan rendered; refresh their baselines so the next
    // drift scan does not report AgenTuna's own copy as an external edit.
    for (_, path) in &cached.agents {
        at_engine::drift::sync_written_target(
            connection,
            // A shared native document (Claude's project MCP file is the user file) can carry
            // rows of another scope at the same path; refresh every row that targets it.
            None,
            path,
            Some(&cached.target_store_file),
        )
        .map_err(|error| {
            CommandError::internal(format!(
                "copy deployment baselines could not be refreshed: {error}"
            ))
        })?;
    }
    Ok(McpCopyApplyView {
        operation_id: outcome.operation_id,
        target_item_id: target_item_id.to_string(),
        target_slug: cached.target_slug,
        files_changed: outcome.completed,
        backup_location: cached.backup_folder.display().to_string(),
    })
}

fn agents_targets(plan: &McpItemPlan) -> Vec<(AgentId, PathBuf)> {
    let paths = plan
        .plan
        .native
        .as_ref()
        .map(|set| {
            set.ops
                .iter()
                .filter_map(|operation| operation.destination().map(Path::to_path_buf))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    plan.agents.iter().cloned().zip(paths).collect::<Vec<_>>()
}

fn refusal_view(refusal: &McpAgentRefusal) -> McpCopyRefusalView {
    McpCopyRefusalView {
        agent: refusal.agent.to_string(),
        reason: refusal.reason.clone(),
    }
}

fn parse_scope(value: &str) -> Result<Scope, CommandError> {
    Scope::from_str(value)
        .map_err(|_| CommandError::invalid_argument("scope", "expected a supported scope"))
}

fn parse_collision(
    on_collision: Option<&str>,
    new_slug: Option<&str>,
) -> Result<Option<McpCopyResolution>, CommandError> {
    match on_collision {
        None => Ok(None),
        Some("replace") => {
            if new_slug.is_some() {
                return Err(CommandError::invalid_argument(
                    "new_slug",
                    "Replace keeps the destination name and takes no new slug",
                ));
            }
            Ok(Some(McpCopyResolution::Replace))
        }
        Some("rename") => {
            let slug = new_slug.ok_or_else(|| {
                CommandError::invalid_argument(
                    "new_slug",
                    "enter a lowercase slug for the renamed copy",
                )
            })?;
            let parsed = Slug::parse(slug).map_err(|_| {
                CommandError::invalid_argument("new_slug", "enter a valid lowercase slug")
            })?;
            Ok(Some(McpCopyResolution::Rename(parsed.as_str().to_owned())))
        }
        Some(_) => Err(CommandError::invalid_argument(
            "on_collision",
            "expected `replace` or `rename`",
        )),
    }
}

/// Roots a reviewed copy may write to.
///
/// Authorization comes from the support, backup and store roots plus the agent-native home and
/// project directories; a planned destination never grants itself authorization.
fn authorized_roots(
    paths: &AppPaths,
    source_layout: &StoreLayout,
    target_store_file: &Path,
) -> Vec<PathBuf> {
    let mut roots = vec![
        paths.support_dir().to_path_buf(),
        paths.backup_dir().to_path_buf(),
        source_layout.root().to_path_buf(),
        paths.global_store().to_path_buf(),
        paths.user_store().to_path_buf(),
    ];
    if let Some(target_root) = target_store_file.parent() {
        roots.push(target_root.to_path_buf());
    }
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    if let Some(project) = paths.project_store().and_then(Path::parent) {
        roots.push(project.to_path_buf());
    }
    roots.retain(|path| path.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

fn copy_error(error: McpPlanError) -> CommandError {
    match &error {
        McpPlanError::Locked(reason) => CommandError::new(CommandErrorCode::Locked, reason.clone()),
        McpPlanError::PermissionDenied => {
            CommandError::new(CommandErrorCode::Locked, error.to_string())
        }
        McpPlanError::SameScope
        | McpPlanError::MissingProject { .. }
        | McpPlanError::NoTargetToReplace(_)
        | McpPlanError::InvalidItem(_)
        | McpPlanError::NoNativeTarget { .. } => {
            CommandError::invalid_argument("target_scope", &error.to_string())
        }
        McpPlanError::UnmanagedNative { .. } => {
            CommandError::new(CommandErrorCode::RefusedOverwrite, error.to_string())
        }
        _ => CommandError::new(CommandErrorCode::Conflict, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{McpCopyResolution, parse_collision};

    #[test]
    fn collision_parsing_rejects_a_slug_replace_and_invalid_names() {
        assert_eq!(parse_collision(None, None).expect("no decision"), None);
        assert_eq!(
            parse_collision(Some("replace"), None).expect("replace"),
            Some(McpCopyResolution::Replace)
        );
        assert!(
            parse_collision(Some("replace"), Some("other")).is_err(),
            "Replace keeps the destination name"
        );
        assert!(
            parse_collision(Some("rename"), None).is_err(),
            "Rename needs a slug"
        );
        assert!(parse_collision(Some("rename"), Some("Bad Slug")).is_err());
        assert!(parse_collision(Some("overwrite"), None).is_err());
    }
}
