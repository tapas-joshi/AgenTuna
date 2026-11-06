//! Typed commands for safely resolving refused filesystem overwrites.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;
use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_fs::MoveAsideOutcome;
use at_migrate::adopt::{AdoptError, AdoptHandling, AdoptRequest, adopt_item, preview_adopt};
use at_migrate::scan::{ScanReport, ScanRequest};
use at_model::Scope;
use at_store::layout::StoreLayout;
use at_store::paths::AppPaths;
use serde::{Deserialize, Serialize};
use tauri::State;
use time::OffsetDateTime;
use ts_rs::TS;

/// Result of moving a refused path aside without copying or overwriting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverwriteMoveAsideView {
    /// Path that was moved.
    pub original: String,
    /// New path containing the preserved original bytes.
    pub moved_to: String,
}

impl From<MoveAsideOutcome> for OverwriteMoveAsideView {
    fn from(outcome: MoveAsideOutcome) -> Self {
        Self {
            original: outcome.original.display().to_string(),
            moved_to: outcome.moved_to.display().to_string(),
        }
    }
}

/// Moves a refused target aside after checking it is under a registered agent path.
///
/// # Errors
/// Returns a typed command error if the path is invalid, unregistered, or cannot be moved.
#[tauri::command]
pub fn overwrite_move_aside(
    state: State<'_, AppState>,
    path: String,
) -> Result<OverwriteMoveAsideView, CommandError> {
    if path.len() > 4096 {
        return Err(CommandError::invalid_argument(
            "path",
            "must be at most 4096 bytes",
        ));
    }
    let target = validated_absolute_path(&path)?;
    ensure_registered_target(&state, &target)?;
    let outcome = at_fs::move_aside(&target, OffsetDateTime::now_utc())
        .map_err(|_| CommandError::unavailable("The refused item could not be moved aside"))?;
    Ok(outcome.into())
}

/// Validates a caller-supplied absolute path: absolute, normalized and bounded.
fn validated_absolute_path(path: &str) -> Result<PathBuf, CommandError> {
    if path.len() > 4096 {
        return Err(CommandError::invalid_argument(
            "path",
            "must be at most 4096 bytes",
        ));
    }
    let target = PathBuf::from(path);
    if !target.is_absolute() || target.components().any(|part| part == Component::ParentDir) {
        return Err(CommandError::invalid_argument(
            "path",
            "must be an absolute normalized path",
        ));
    }
    Ok(target)
}

/// Refuses a path that is not below a registered agent root.
fn ensure_registered_target(state: &AppState, target: &Path) -> Result<(), CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not verify the registered agent paths"))?;
    let mut registered_paths = Vec::new();
    for agent in agents {
        let paths = at_db::agents::list_agent_paths(database.connection(), &agent.id)
            .map_err(|_| CommandError::internal("could not verify the registered agent paths"))?;
        registered_paths.extend(paths.into_iter().filter_map(|entry| entry.path));
    }
    drop(database);
    if !is_registered_target(target, &registered_paths) {
        return Err(CommandError::invalid_argument(
            "path",
            "is outside registered agent paths",
        ));
    }
    Ok(())
}

fn is_registered_target(target: &Path, registered_paths: &[PathBuf]) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    let Ok(canonical_parent) = parent.canonicalize() else {
        return false;
    };
    registered_paths.iter().any(|registered| {
        if registered == target {
            return true;
        }
        let Ok(canonical_root) = registered.canonicalize() else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(&canonical_root) else {
            return false;
        };
        metadata.is_dir() && canonical_parent.starts_with(canonical_root)
    })
}

#[cfg(test)]
mod tests {
    use super::is_registered_target;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn accepts_only_existing_targets_under_registered_roots() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("agent-skills");
        std::fs::create_dir(&root).expect("create root");
        let target = root.join("native-skill");
        std::fs::create_dir(&target).expect("create target");
        let sibling = temp.path().join("unregistered");
        std::fs::create_dir(&sibling).expect("create sibling");
        assert!(is_registered_target(&target, std::slice::from_ref(&root)));
        assert!(!is_registered_target(&sibling, &[root]));
        assert!(!is_registered_target(&PathBuf::from("relative"), &[]));
    }
}

/// Review of adopting one unmanaged native item into the canonical store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AdoptPreviewView {
    /// Native path being adopted.
    pub source: String,
    /// Canonical store root that will receive the item.
    pub store_root: String,
    /// Where the store item will live.
    pub store_destination: String,
    /// Chosen store slug.
    pub slug: String,
    /// Original native item name.
    pub native_name: String,
    /// Item kind (`skill`, `mcp_server`, `rule`).
    pub kind: String,
    /// Agents that read the native path.
    pub agents: Vec<String>,
    /// Handling that will be applied (`link`, `copy`, `watch`).
    pub handling: String,
    /// Whether a name collision forced a suffixed slug.
    pub renamed: bool,
    /// Whether the path exists now (`present` or `missing`).
    pub current: String,
}

/// Result of adopting one unmanaged native item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AdoptApplyView {
    /// Durable operation id accepted by the undo command.
    pub operation_id: String,
    /// Where the store item was created.
    pub store_destination: String,
    /// Chosen store slug.
    pub slug: String,
    /// Handling that was applied.
    pub handling: String,
    /// One-line summary for the dialog.
    pub summary: String,
}

/// Scans agent folders into a reviewable record for adoption.
fn adopt_scan(paths: &AppPaths) -> Result<ScanReport, CommandError> {
    let home = paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
        .to_path_buf();
    let project = paths
        .project_store()
        .and_then(|store| store.parent())
        .map(Path::to_path_buf);
    let (roots, store_items) = adopt_store_inventory(paths)?;
    let environment = AgentEnvironment {
        home,
        project,
        variables: BTreeMap::new(),
    };
    let adapters = AgentAdapterRegistry::register();
    at_migrate::scan::scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &roots,
        store_items: &store_items,
    })
    .map_err(|_| CommandError::internal("native scan failed"))
}

/// Store roots and canonical inventory used to classify native ownership.
fn adopt_store_inventory(
    paths: &AppPaths,
) -> Result<(Vec<PathBuf>, Vec<at_migrate::scan::StoreItem>), CommandError> {
    let mut roots = Vec::new();
    let mut items = Vec::new();
    for scope in [Scope::Global, Scope::User, Scope::Project, Scope::Local] {
        let layout = match StoreLayout::for_scope(paths, scope) {
            Ok(layout) => layout,
            Err(_) if matches!(scope, Scope::Project | Scope::Local) => continue,
            Err(_) => return Err(CommandError::unavailable("store paths are unavailable")),
        };
        roots.push(layout.root().to_path_buf());
        match at_store::manifest::StoreManifest::load(&layout) {
            Ok(loaded) => items.extend(loaded.manifest.items().iter().map(|item| {
                at_migrate::scan::StoreItem {
                    kind: item.kind,
                    name: item.name.clone().unwrap_or_else(|| item.slug.to_string()),
                    content_hash: item.hash.to_string(),
                }
            })),
            Err(error) if error.is_missing() => {}
            Err(_) => {
                return Err(CommandError::internal(
                    "could not read a canonical store manifest",
                ));
            }
        }
    }
    Ok((roots, items))
}

/// Store scope and root receiving a scanned native item.
fn adopt_store_target(
    paths: &AppPaths,
    item_scope: Scope,
) -> Result<(Scope, PathBuf), CommandError> {
    let scope = match item_scope {
        Scope::Project | Scope::Local if paths.project_store().is_some() => Scope::Project,
        _ => Scope::User,
    };
    let layout = StoreLayout::for_scope(paths, scope)
        .map_err(|_| CommandError::unavailable("store paths are unavailable"))?;
    Ok((scope, layout.root().to_path_buf()))
}

/// Authorized roots for the native paths an adoption may replace.
pub(crate) fn adopt_authorized_roots(paths: &AppPaths) -> Result<at_fs::ApplyRoots, CommandError> {
    let mut roots = Vec::new();
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    if let Some(project) = paths.project_store().and_then(|store| store.parent()) {
        roots.push(project.to_path_buf());
    }
    at_fs::ApplyRoots::new(roots)
        .map_err(|_| CommandError::unavailable("native agent roots are unavailable"))
}

/// Maps an adopt failure onto the typed command error contract.
fn adopt_error(error: AdoptError) -> CommandError {
    match error {
        AdoptError::ItemNotFound(path) => CommandError::new(
            CommandErrorCode::NotFound,
            format!("No native item was found at {}", path.display()),
        ),
        AdoptError::StaleScanRecord { path, current } => CommandError::new(
            CommandErrorCode::Conflict,
            format!(
                "{} changed since the scan ({current:?}); rescan before adopting",
                path.display()
            ),
        ),
        AdoptError::AlreadyManaged(path) => CommandError::new(
            CommandErrorCode::Conflict,
            format!("{} is already managed by AgenTuna", path.display()),
        ),
        AdoptError::Unparsed(path) => CommandError::new(
            CommandErrorCode::Conflict,
            format!("{} could not be read as a native item", path.display()),
        ),
        AdoptError::InvalidName(name) => {
            CommandError::invalid_argument("path", &format!("`{name}` cannot become a store slug"))
        }
        AdoptError::InvalidHandling(handling) => {
            CommandError::invalid_argument("handling", &format!("unknown handling `{handling}`"))
        }
        AdoptError::Store(message) => CommandError::unavailable(message),
        AdoptError::Plan(error) => CommandError::new(
            CommandErrorCode::Conflict,
            format!("the adopt plan was refused: {error}"),
        ),
        AdoptError::Apply(error) => CommandError::new(
            CommandErrorCode::Conflict,
            format!("the adopt could not be applied: {error}"),
        ),
        _ => CommandError::internal("the adopt could not be completed"),
    }
}

/// Refuses a path whose bytes no longer match the hash the refusal dialog reported.
fn ensure_unchanged_since_refusal(
    target: &Path,
    expected_hash: Option<&str>,
) -> Result<(), CommandError> {
    let Some(expected) = expected_hash else {
        return Ok(());
    };
    let current = at_fs::changeset::hash_node(target)
        .map_err(|_| CommandError::internal("the refused path could not be read"))?;
    if current.as_deref() == Some(expected) {
        return Ok(());
    }
    Err(CommandError::new(
        CommandErrorCode::Conflict,
        "The item changed since the scan; rescan before adopting.",
    ))
}

/// Reviews adopting one refused native item without writing item content.
///
/// # Errors
/// Returns a typed command error for an invalid or unregistered path, a stale scan, or an
/// unavailable store.
#[tauri::command]
pub fn adopt_preview(
    state: State<'_, AppState>,
    path: String,
    expected_hash: Option<String>,
    handling: Option<String>,
) -> Result<AdoptPreviewView, CommandError> {
    adopt_preview_inner(&state, &path, expected_hash, handling)
}

/// Bridge-facing body of [`adopt_preview`], callable without a Tauri app.
///
/// # Errors
/// See [`adopt_preview`].
pub fn adopt_preview_inner(
    state: &AppState,
    path: &str,
    expected_hash: Option<String>,
    handling: Option<String>,
) -> Result<AdoptPreviewView, CommandError> {
    let target = validated_absolute_path(path)?;
    ensure_registered_target(state, &target)?;
    ensure_unchanged_since_refusal(&target, expected_hash.as_deref())?;
    let handling = match handling.as_deref() {
        None => AdoptHandling::Link,
        Some(value) => AdoptHandling::parse(value).map_err(adopt_error)?,
    };
    let paths = super::paths::resolve_command_paths(state, None)
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let scan = adopt_scan(&paths)?;
    let item = scan
        .items
        .iter()
        .find(|item| item.path == target)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                format!("No native item was found at {}", target.display()),
            )
        })?
        .clone();
    let (scope, store_root) = adopt_store_target(&paths, item.scope)?;
    let authorized = adopt_authorized_roots(&paths)?;
    let mut database = state.database().map_err(CommandError::unavailable)?;
    let preview = preview_adopt(&AdoptRequest {
        scan: &scan,
        path: &item.path,
        store_root: &store_root,
        backup_root: paths.backup_dir(),
        authorized_roots: &authorized,
        database: &mut database,
        scope,
        handling,
    })
    .map_err(adopt_error)?;
    Ok(AdoptPreviewView {
        source: preview.source.display().to_string(),
        store_root: preview.store_root.display().to_string(),
        store_destination: preview.store_destination.display().to_string(),
        slug: preview.slug,
        native_name: preview.native_name,
        kind: preview.kind.to_string(),
        agents: preview.agents.iter().map(ToString::to_string).collect(),
        handling: preview.handling.as_str().to_owned(),
        renamed: preview.renamed,
        current: match preview.current {
            at_migrate::adopt::PathState::Present(_) => "present".to_owned(),
            at_migrate::adopt::PathState::Missing => "missing".to_owned(),
            _ => "unknown".to_owned(),
        },
    })
}

/// Adopts one refused native item into the store as a reviewed, undoable operation.
///
/// # Errors
/// Returns a typed command error for an invalid or unregistered path, a stale scan, or any
/// plan/apply failure.
#[tauri::command]
pub fn adopt_apply(
    state: State<'_, AppState>,
    path: String,
    expected_hash: Option<String>,
    handling: Option<String>,
) -> Result<AdoptApplyView, CommandError> {
    adopt_apply_inner(&state, &path, expected_hash, handling)
}

/// Bridge-facing body of [`adopt_apply`], callable without a Tauri app.
///
/// # Errors
/// See [`adopt_apply`].
pub fn adopt_apply_inner(
    state: &AppState,
    path: &str,
    expected_hash: Option<String>,
    handling: Option<String>,
) -> Result<AdoptApplyView, CommandError> {
    let target = validated_absolute_path(path)?;
    ensure_registered_target(state, &target)?;
    ensure_unchanged_since_refusal(&target, expected_hash.as_deref())?;
    let handling = match handling.as_deref() {
        None => AdoptHandling::Link,
        Some(value) => AdoptHandling::parse(value).map_err(adopt_error)?,
    };
    let paths = super::paths::resolve_command_paths(state, None)
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let scan = adopt_scan(&paths)?;
    let item = scan
        .items
        .iter()
        .find(|item| item.path == target)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                format!("No native item was found at {}", target.display()),
            )
        })?
        .clone();
    let (scope, store_root) = adopt_store_target(&paths, item.scope)?;
    let authorized = adopt_authorized_roots(&paths)?;
    let mut database = state.database().map_err(CommandError::unavailable)?;
    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        store_root: &store_root,
        backup_root: paths.backup_dir(),
        authorized_roots: &authorized,
        database: &mut database,
        scope,
        handling,
    })
    .map_err(adopt_error)?;
    Ok(AdoptApplyView {
        summary: format!(
            "Adopted `{}` as `{}` ({}).",
            outcome.native_name,
            outcome.slug,
            outcome.handling.as_str()
        ),
        operation_id: outcome.operation_id,
        store_destination: outcome.store_destination.display().to_string(),
        slug: outcome.slug,
        handling: outcome.handling.as_str().to_owned(),
    })
}

/// Result of undoing one recorded operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OperationUndoView {
    /// New Undo operation id.
    pub undo_operation_id: String,
    /// One-line summary for the dialog.
    pub summary: String,
}

/// Undoes one recorded operation through the shared at-migrate undo pipeline.
///
/// With `force` false (the default), an operation whose files changed afterwards is refused
/// with a typed conflict naming the changed paths; `operation_undo_check` returns the same
/// paths as data. With `force` true, the newer edits are deliberately replaced by the
/// recorded backups.
///
/// # Errors
/// Returns a typed conflict for an expired, already-undone or externally edited operation.
#[tauri::command]
pub fn operation_undo(
    state: State<'_, AppState>,
    operation_id: String,
    force: Option<bool>,
) -> Result<OperationUndoView, CommandError> {
    operation_undo_inner(&state, &operation_id, force.unwrap_or(false))
}

/// Bridge-facing body of [`operation_undo`], callable without a Tauri app.
///
/// # Errors
/// See [`operation_undo`].
pub fn operation_undo_inner(
    state: &AppState,
    operation_id: &str,
    force: bool,
) -> Result<OperationUndoView, CommandError> {
    let paths = super::paths::resolve_command_paths(state, None)
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let authorized = adopt_authorized_roots(&paths)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let detail = at_db::ops::operation_detail(database.connection(), operation_id)
        .map_err(|_| CommandError::internal("operation history is unavailable"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                format!("operation {operation_id} was not found"),
            )
        })?;
    let scope = detail.operation.scope.unwrap_or(Scope::User);
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("store paths are unavailable"))?;
    let store_root = layout.root().to_path_buf();
    drop(database);
    let database = state.database().map_err(CommandError::unavailable)?;
    let outcome = at_migrate::undo::undo_operation(
        at_migrate::undo::UndoOperationRequest {
            connection: database.connection(),
            store_root: &store_root,
            backup_root: paths.backup_dir(),
            authorized_roots: &authorized,
            actor: at_db::ops::OperationActor::Ui,
            force,
        },
        operation_id,
    )
    .map_err(|error| {
        CommandError::new(
            CommandErrorCode::Conflict,
            format!("the operation could not be undone: {error}"),
        )
    })?;
    let changed_paths = |paths: &[std::path::PathBuf]| {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    match outcome {
        at_migrate::undo::UndoOutcome::Restored { undo_operation_id } => Ok(OperationUndoView {
            summary: format!("Operation {operation_id} was undone."),
            undo_operation_id,
        }),
        at_migrate::undo::UndoOutcome::AlreadyUndone => Err(CommandError::new(
            CommandErrorCode::Conflict,
            "this operation was already undone",
        )),
        at_migrate::undo::UndoOutcome::Expired => Err(CommandError::new(
            CommandErrorCode::Conflict,
            "this operation is outside the 30-day undo window",
        )),
        at_migrate::undo::UndoOutcome::Conflict { paths } => Err(CommandError::new(
            CommandErrorCode::Conflict,
            format!(
                "The undo was refused; these paths changed after the operation:\n{}",
                changed_paths(&paths)
            ),
        )),
        at_migrate::undo::UndoOutcome::Partial { paths } => Err(CommandError::new(
            CommandErrorCode::Conflict,
            format!(
                "These paths cannot be restored safely:\n{}",
                changed_paths(&paths)
            ),
        )),
        _ => Err(CommandError::internal("the undo could not be completed")),
    }
}
