//! Typed commands for safely resolving refused filesystem overwrites.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;
use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::adopt::{
    AdoptError, AdoptHandling, AdoptPreviewRequest, AdoptRequest, adopt_item, preview_adopt,
};
use at_migrate::scan::{ScanItem, ScanReport, ScanRequest};
use at_model::{ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::StoreManifest;
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
    /// New path containing the preserved original bytes under `<support>/.moved-aside/`.
    pub moved_to: String,
    /// `operations` row that records the move and holds its inverse mapping.
    pub operation_id: String,
}

/// Moves a refused target aside after checking it is under a registered agent path.
///
/// The whole tree lands in `<support>/.moved-aside/<timestamp>/` and an `operations` row plus the
/// journal record that reverses it are written first, so the move is never anonymous.
///
/// # Errors
/// Returns a typed command error if the path is invalid, unregistered, or cannot be moved.
#[tauri::command]
pub fn overwrite_move_aside(
    state: State<'_, AppState>,
    path: String,
) -> Result<OverwriteMoveAsideView, CommandError> {
    overwrite_move_aside_inner(&state, &path)
}

/// Bridge-facing body of [`overwrite_move_aside`], callable without a Tauri app.
///
/// # Errors
/// See [`overwrite_move_aside`].
pub fn overwrite_move_aside_inner(
    state: &AppState,
    path: &str,
) -> Result<OverwriteMoveAsideView, CommandError> {
    if path.len() > 4096 {
        return Err(CommandError::invalid_argument(
            "path",
            "must be at most 4096 bytes",
        ));
    }
    let target = validated_absolute_path(path)?;
    let paths = super::paths::resolve_active_paths(state)?;
    ensure_registered_target(state, &paths, &target)?;
    let database = state
        .database_handle()
        .map_err(|_| CommandError::unavailable("database unavailable"))?;
    let journal =
        at_store::move_journal::MoveJournalStore::in_support(database, paths.support_dir());
    let request =
        at_fs::MoveAsideRequest::new(paths.support_dir(), &target, OffsetDateTime::now_utc());
    let outcome = at_fs::remedies::move_aside(&request, &journal).map_err(|error| {
        CommandError::unavailable(format!(
            "The refused item could not be moved aside: {error}"
        ))
    })?;
    Ok(OverwriteMoveAsideView {
        original: outcome.original.display().to_string(),
        moved_to: outcome.moved_to.display().to_string(),
        operation_id: outcome.operation_id.to_string(),
    })
}

/// Result of reversing one Move aside operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverwriteUndoMoveView {
    /// Reversed operation.
    pub operation_id: String,
    /// Original path the tree was moved back to.
    pub original: String,
    /// Moved-aside path the tree came from.
    pub moved_back_from: String,
}

/// Moves a previously moved-aside tree back to its original path.
///
/// The journal is the authority: a missing operation, an already-reversed operation or a new
/// file at the original path is refused instead of clobbering it.
///
/// # Errors
/// Returns a typed conflict when the original path is occupied again, and a typed unavailable
/// error when the journal or the moved tree cannot be read.
#[tauri::command]
pub fn overwrite_undo_move(
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<OverwriteUndoMoveView, CommandError> {
    overwrite_undo_move_inner(&state, &operation_id)
}

/// Bridge-facing body of [`overwrite_undo_move`], callable without a Tauri app.
///
/// # Errors
/// See [`overwrite_undo_move`].
pub fn overwrite_undo_move_inner(
    state: &AppState,
    operation_id: &str,
) -> Result<OverwriteUndoMoveView, CommandError> {
    if operation_id.len() > 128 {
        return Err(CommandError::invalid_argument(
            "operationId",
            "must be at most 128 bytes",
        ));
    }
    let database = state
        .database_handle()
        .map_err(|_| CommandError::unavailable("database unavailable"))?;
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let journal =
        at_store::move_journal::MoveJournalStore::in_support(database, paths.support_dir());
    let id = at_fs::MoveOperationId::new(operation_id);
    let outcome = at_fs::undo_move(&id, &journal, &at_fs::remedies::SystemMoveFs).map_err(
        |error| match error {
            at_fs::FsError::AlreadyExists { path } => CommandError::new(
                CommandErrorCode::Conflict,
                format!(
                    "The original path is occupied again ({}); remove or adopt that item first.",
                    path.display()
                ),
            ),
            other => CommandError::unavailable(format!(
                "The moved-aside item could not be restored: {other}"
            )),
        },
    )?;
    Ok(OverwriteUndoMoveView {
        operation_id: outcome.operation_id.to_string(),
        original: outcome.original.display().to_string(),
        moved_back_from: outcome.moved_back_from.display().to_string(),
    })
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

/// Validates the optional entry name that identifies one item inside a shared document.
///
/// The name reaches a scan lookup, so it is bounded and may not contain the separator
/// `native_items` uses between a document and its entry key.
fn validated_entry_name(name: Option<&str>) -> Result<Option<&str>, CommandError> {
    match name {
        None => Ok(None),
        Some(value)
            if !value.is_empty()
                && value.len() <= 256
                && !value.contains(at_db::native::MCP_ENTRY_SEPARATOR) =>
        {
            Ok(Some(value))
        }
        Some(_) => Err(CommandError::invalid_argument(
            "name",
            "must be 1-256 bytes without `#`",
        )),
    }
}

/// The scanned record a caller selected.
///
/// `name` narrows the match to one entry inside a shared native document, because every MCP
/// server in one configuration file scans to the same path. Paths compare spelling-tolerantly:
/// the registered project is stored canonicalised (`/private/tmp` for `/tmp`), while native
/// rows keep the raw `$HOME` spelling, so the same folder arrives under two prefixes.
fn selected_scan_item<'r>(
    scan: &'r ScanReport,
    target: &Path,
    name: Option<&str>,
) -> Option<&'r ScanItem> {
    scan.items
        .iter()
        .find(|item| same_path(&item.path, target) && name.is_none_or(|wanted| item.name == wanted))
}

/// Compares two spellings of one path, resolving symlinks and prefix symlinks such as
/// `/tmp` → `/private/tmp`; falls back to exact equality when a side cannot be resolved.
fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Refuses a path that is not below a registered agent discovery root.
///
/// `paths` must be the command's resolved paths, i.e. re-resolved for the database-selected
/// project: every adapter's native skill root is derived from the project store, so base shell
/// paths without a project would leave project items with no matching root at all. The adapter
/// roots mirror exactly what the native scan enumerates, so adoption accepts the same set the
/// Skills screen offers.
fn ensure_registered_target(
    state: &AppState,
    paths: &AppPaths,
    target: &Path,
) -> Result<(), CommandError> {
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
    if let Some(home) = paths.home_dir() {
        let environment = AgentEnvironment {
            home: home.to_path_buf(),
            project: paths
                .project_store()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            variables: BTreeMap::new(),
        };
        let adapters = AgentAdapterRegistry::register();
        if let Ok(all_adapters) = adapters.adapters() {
            for adapter in all_adapters {
                for scope in [Scope::User, Scope::Project, Scope::Local] {
                    registered_paths.extend(
                        adapter
                            .paths(ItemKind::Skill, scope, &environment)
                            .into_iter()
                            .map(|candidate| candidate.path),
                    );
                }
            }
        }
    }
    if !is_registered_target(target, &registered_paths) {
        return Err(CommandError::invalid_argument(
            "path",
            &outside_registered_roots_message(&registered_paths, paths),
        ));
    }
    Ok(())
}

/// Explains a registered-target refusal: which roots the check consulted and where project
/// roots come from, so the caller knows which list to extend. Only paths are named.
fn outside_registered_roots_message(registered_paths: &[PathBuf], paths: &AppPaths) -> String {
    const MAX_LISTED_ROOTS: usize = 8;
    let mut message = String::from("is outside registered agent paths");
    if registered_paths.is_empty() {
        message.push_str("; no agent discovery roots are registered yet");
    } else {
        message.push_str(&format!("; checked {} roots: ", registered_paths.len()));
        let shown = registered_paths.len().min(MAX_LISTED_ROOTS);
        for (index, root) in registered_paths.iter().take(shown).enumerate() {
            if index > 0 {
                message.push_str(", ");
            }
            message.push_str(&root.display().to_string());
        }
        let hidden = registered_paths.len() - shown;
        if hidden > 0 {
            message.push_str(&format!(" and {hidden} more"));
        }
    }
    match paths.project_store().and_then(Path::parent) {
        Some(project) => message.push_str(&format!(
            "; project roots follow the selected project {}",
            project.display()
        )),
        None => message.push_str(
            "; project roots need a selected project, so project items stay refused until one is selected",
        ),
    }
    message
}

fn is_registered_target(target: &Path, registered_paths: &[PathBuf]) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    let Ok(canonical_target) = target.canonicalize() else {
        return false;
    };
    registered_paths.iter().any(|registered| {
        let Ok(canonical_root) = registered.canonicalize() else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(&canonical_root) else {
            return false;
        };
        if !metadata.is_dir() {
            return canonical_target == canonical_root;
        }
        let Ok(canonical_parent) = parent.canonicalize() else {
            return false;
        };
        canonical_target.starts_with(&canonical_root)
            && canonical_parent.starts_with(canonical_root)
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
    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_target_that_escapes_a_registered_root() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("agent-skills");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).expect("registered root");
        std::fs::create_dir_all(&outside).expect("outside root");
        let target = root.join("escaped");
        std::os::unix::fs::symlink(&outside, &target).expect("symlink target");
        assert!(!is_registered_target(&target, std::slice::from_ref(&root)));
    }

    #[test]
    fn export_bindings() {
        use ts_rs::TS;
        super::AdoptPreviewView::export(&ts_rs::Config::default())
            .expect("export AdoptPreviewView");
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
    /// Backup directory where originals are stored before writes.
    pub backup_dir: String,
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
    /// Content digest the preview verified.
    ///
    /// Pass it back to `adopt_apply` as `expected_hash`: the apply refuses with a conflict
    /// when the source no longer holds these bytes, so the reviewed item is the item adopted.
    pub hash: String,
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
                    scope,
                    path: layout.root().join(item.path.to_string()),
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

/// Registered project that owns the selected project store, when adopting at Project scope.
///
/// A Project-scope adoption without a registered owning project would record an item row
/// with no project and make the owner filter disagree with the store path, so it is refused.
fn adopt_project_id(
    paths: &AppPaths,
    scope: Scope,
    database: &at_db::Database,
) -> Result<Option<String>, CommandError> {
    let Scope::Project = scope else {
        return Ok(None);
    };
    let root = paths
        .project_store()
        .and_then(Path::parent)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "scope",
                "a project-scope adoption needs a registered project",
            )
        })?;
    let project = at_db::projects::find_project_by_path(database.connection(), root)
        .map_err(|_| CommandError::internal("could not query registered projects"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                format!("project {} is not registered", root.display()),
            )
        })?;
    Ok(Some(project.id))
}

/// Authorized roots for the native paths an adoption may replace.
pub fn adopt_authorized_roots(paths: &AppPaths) -> Result<at_fs::ApplyRoots, CommandError> {
    let mut roots = Vec::new();
    if let Some(home) = paths.home_dir()
        && home.is_dir()
    {
        roots.push(home.to_path_buf());
    }
    if let Some(project) = paths.project_store().and_then(|store| store.parent())
        && project.is_dir()
    {
        roots.push(project.to_path_buf());
    }
    let _ = at_fs::ensure_dir(paths.backup_dir());
    if paths.backup_dir().is_dir() {
        roots.push(paths.backup_dir().to_path_buf());
    }
    let _ = at_fs::ensure_dir(paths.support_dir());
    if paths.support_dir().is_dir() {
        roots.push(paths.support_dir().to_path_buf());
    }
    if paths.user_store().is_dir() {
        roots.push(paths.user_store().to_path_buf());
    }
    if paths.global_store().is_dir() {
        roots.push(paths.global_store().to_path_buf());
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
    name: Option<String>,
) -> Result<AdoptPreviewView, CommandError> {
    adopt_preview_inner(&state, &path, expected_hash, handling, name.as_deref())
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
    name: Option<&str>,
) -> Result<AdoptPreviewView, CommandError> {
    let name = validated_entry_name(name)?;
    let target = validated_absolute_path(path)?;
    let paths = super::paths::resolve_active_paths(state)?;
    ensure_registered_target(state, &paths, &target)?;
    ensure_unchanged_since_refusal(&target, expected_hash.as_deref())?;
    let handling = match handling.as_deref() {
        None => AdoptHandling::Link,
        Some(value) => AdoptHandling::parse(value).map_err(adopt_error)?,
    };
    let scan = adopt_scan(&paths)?;
    let item = selected_scan_item(&scan, &target, name)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                format!("No native item was found at {}", target.display()),
            )
        })?
        .clone();
    let (scope, store_root) = adopt_store_target(&paths, item.scope)?;
    let authorized = adopt_authorized_roots(&paths)?;
    // The digest travels back to `adopt_apply` and is compared with `hash_node` there, the
    // same representation `ensure_unchanged_since_refusal` uses. Read it before the preview
    // verifies the source: a write that lands in between then makes the returned digest older
    // than the verified bytes, and the apply refuses it, instead of handing the apply a digest
    // of bytes the reviewer never saw.
    let hash = at_fs::changeset::hash_node(&target)
        .map_err(|_| CommandError::internal("the refused path could not be read"))?
        .unwrap_or_default();
    let preview = preview_adopt(&AdoptPreviewRequest {
        scan: &scan,
        path: &item.path,
        name,
        store_root: &store_root,
        backup_root: paths.backup_dir(),
        authorized_roots: &authorized,
        scope,
        handling,
    })
    .map_err(adopt_error)?;
    let current = match preview.current {
        at_migrate::adopt::PathState::Present(_) => "present".to_owned(),
        at_migrate::adopt::PathState::Missing => "missing".to_owned(),
        _ => "unknown".to_owned(),
    };
    Ok(AdoptPreviewView {
        source: preview.source.display().to_string(),
        store_root: preview.store_root.display().to_string(),
        store_destination: preview.store_destination.display().to_string(),
        backup_dir: paths.backup_dir().display().to_string(),
        slug: preview.slug,
        native_name: preview.native_name,
        kind: preview.kind.to_string(),
        agents: preview.agents.iter().map(ToString::to_string).collect(),
        handling: preview.handling.as_str().to_owned(),
        renamed: preview.renamed,
        current,
        hash,
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
    name: Option<String>,
) -> Result<AdoptApplyView, CommandError> {
    adopt_apply_inner(&state, &path, expected_hash, handling, name.as_deref())
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
    name: Option<&str>,
) -> Result<AdoptApplyView, CommandError> {
    let name = validated_entry_name(name)?;
    let target = validated_absolute_path(path)?;
    let paths = super::paths::resolve_active_paths(state)?;
    ensure_registered_target(state, &paths, &target)?;
    ensure_unchanged_since_refusal(&target, expected_hash.as_deref())?;
    let handling = match handling.as_deref() {
        None => AdoptHandling::Link,
        Some(value) => AdoptHandling::parse(value).map_err(adopt_error)?,
    };
    let scan = adopt_scan(&paths)?;
    let item = selected_scan_item(&scan, &target, name)
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
    let project_id = adopt_project_id(&paths, scope, &database)?;
    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name,
        store_root: &store_root,
        backup_root: paths.backup_dir(),
        authorized_roots: &authorized,
        database: &mut database,
        scope,
        handling,
        project_id: project_id.as_deref(),
    })
    .map_err(adopt_error)?;
    // The adopted item is AgenTuna's now, so its native finding must leave the "not in
    // AgenTuna" set before the next list load reads the rows.
    super::native_scan::refresh_native_findings(&paths, database.connection());
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

/// Authorized roots for [`operation_undo_inner`].
///
/// Undo restores files the app itself wrote, so the roots are the native roots an adoption may
/// replace plus the Global store: a Global-scope write was authorized when the operation was
/// applied, and refusing its undo would strand the user with no way back. Paths outside these
/// roots stay refused.
fn undo_authorized_roots(paths: &AppPaths) -> Result<at_fs::ApplyRoots, CommandError> {
    let mut roots = Vec::new();
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    if let Some(project) = paths.project_store().and_then(|store| store.parent()) {
        roots.push(project.to_path_buf());
    }
    if paths.global_store().is_dir() {
        roots.push(paths.global_store().to_path_buf());
    }
    at_fs::ApplyRoots::new(roots)
        .map_err(|_| CommandError::unavailable("native agent roots are unavailable"))
}

/// Registered project that owns a Project- or Local-scope operation's store.
///
/// The operation's deployment rows name the project; a row that predates that link falls back
/// to the deepest registered project containing a path the operation touched, mirroring the
/// CLI's resolution so the same operation undoes the same way from both front ends.
fn undo_project_root(
    connection: &rusqlite::Connection,
    detail: &at_db::ops::OperationDetail,
) -> Result<Option<PathBuf>, CommandError> {
    for project_id in at_db::deployments::operation_project_ids(connection, &detail.operation.id)
        .map_err(|_| CommandError::internal("operation history is unavailable"))?
    {
        if let Some(project) = at_db::projects::find_project_by_id(connection, &project_id)
            .map_err(|_| CommandError::internal("could not query registered projects"))?
        {
            return Ok(Some(PathBuf::from(project.path)));
        }
    }
    let mut best: Option<(usize, PathBuf)> = None;
    for project in at_db::projects::list_projects(connection)
        .map_err(|_| CommandError::internal("could not query registered projects"))?
    {
        let root = PathBuf::from(&project.path);
        let depth = root.components().count();
        if best.as_ref().is_some_and(|(found, _)| *found >= depth) {
            continue;
        }
        let touched = detail
            .files
            .iter()
            .filter_map(|file| file.path.as_deref())
            .any(|path| Path::new(path).starts_with(&root));
        if touched {
            best = Some((depth, root));
        }
    }
    Ok(best.map(|(_, root)| root))
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
    // A Project-scope operation lives in that project's own store. A selection that spans scopes
    // is filed under its first scope (User before Project) yet still writes the project store, so
    // the owning project is resolved from the operation's own rows whenever one names it.
    let project_root = undo_project_root(database.connection(), &detail)?;
    let paths = super::paths::resolve_command_paths(state, project_root.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let authorized = undo_authorized_roots(&paths)?;
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
        at_migrate::undo::UndoOutcome::Restored { undo_operation_id } => {
            refresh_undo_targets(
                database.connection(),
                &paths,
                &undo_operation_id,
                operation_id,
            )?;
            // Undo may return an adopted item to its agent folder; the scan re-derives whether
            // it is AgenTuna-owned again rather than trusting a stored flag.
            super::native_scan::refresh_native_findings(&paths, database.connection());
            Ok(OperationUndoView {
                summary: format!("Operation {operation_id} was undone."),
                undo_operation_id,
            })
        }
        at_migrate::undo::UndoOutcome::AlreadyUndone => Err(CommandError::new(
            CommandErrorCode::Conflict,
            "this operation was already undone",
        )),
        at_migrate::undo::UndoOutcome::NotUndoable => Err(CommandError::new(
            CommandErrorCode::Conflict,
            "this operation did not succeed, so it has nothing to undo",
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

/// Re-aligns the derived index with the tree an undo operation restored.
///
/// Undo writes the pre-operation bytes back and deletes the paths the operation created, but
/// `items` and `deployments` are a projection: an item the operation deleted has lost its row,
/// and an item it created still has one. This pass reconciles the affected store scopes from
/// their restored manifests, repairs rule rows from the restored files, re-aligns deployment
/// cells, and refreshes the drift baselines. It never touches rows in scopes the operation did
/// not write.
fn refresh_undo_targets(
    connection: &rusqlite::Connection,
    paths: &AppPaths,
    undo_operation_id: &str,
    undone_operation_id: &str,
) -> Result<(), CommandError> {
    let Some(detail) = at_db::ops::operation_detail(connection, undo_operation_id)
        .map_err(|_| CommandError::internal("the undo history is unavailable"))?
    else {
        return Ok(());
    };
    let touched = detail
        .files
        .iter()
        .filter_map(|file| file.path.clone())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    reconcile_undo_store_items(connection, paths, detail.operation.scope, &touched)?;
    refresh_undo_store_hashes(connection, paths, &touched)?;
    reconcile_undo_rule_items(connection, &touched)?;
    for path in &touched {
        let deployed = std::fs::symlink_metadata(path).is_ok();
        at_db::deployments::align_undone_target(connection, undone_operation_id, path, deployed)
            .map_err(|error| {
                CommandError::internal(format!(
                    "deployment rows could not be re-aligned after undo: {error}"
                ))
            })?;
        if deployed {
            // Rules have no bespoke hash formula: syncing their written target refreshes the
            // baseline for every kind. The exact pass below then corrects skill/MCP cells,
            // whose recorded mode and store hash are more than the current file bytes.
            at_engine::drift::sync_written_target(connection, None, path, None).map_err(
                |error| {
                    CommandError::internal(format!(
                        "deployment baselines could not be refreshed after undo: {error}"
                    ))
                },
            )?;
            refresh_undo_deployment_hashes(connection, paths, path)?;
        }
    }
    // A delete phase that removes the native file marks every row at that path off. Its second
    // phase restores the file and the captured rows; put the captured rows back last so the
    // intermediate off state cannot win.
    if let Some(metadata) = at_migrate::undo_meta::read(paths.backup_dir(), undone_operation_id) {
        at_migrate::undo_meta::restore(connection, &metadata).map_err(|error| {
            CommandError::internal(format!(
                "captured index rows could not be restored: {error}"
            ))
        })?;
        for path in &touched {
            if std::fs::symlink_metadata(path).is_ok() {
                refresh_undo_deployment_hashes(connection, paths, path)?;
            }
        }
    }
    Ok(())
}

/// Recomputes the hashes of every deployment cell whose native path the undo restored.
///
/// A shared store document is one hash for all servers it holds: a later `mcp add` changes it,
/// and undoing that add restores the bytes but not the hash a pre-existing row recorded. Reading
/// the restored files is the only exact answer.
fn refresh_undo_deployment_hashes(
    connection: &rusqlite::Connection,
    paths: &AppPaths,
    native_path: &Path,
) -> Result<(), CommandError> {
    let rows =
        at_db::deployments::cells_for_target(connection, native_path.to_string_lossy().as_ref())
            .map_err(|_| CommandError::internal("deployment rows are unavailable"))?;
    for cell in rows {
        let at_db::deployments::DeploymentCellSnapshot {
            item_id,
            agent_id,
            scope: scope_text,
            project_id,
            kind,
            slug,
            mode: current_mode,
            link_target: current_link,
        } = cell;
        let Ok(scope) = scope_text.parse::<Scope>() else {
            continue;
        };
        let Ok(item_id) = item_id.parse::<at_model::ItemId>() else {
            continue;
        };
        let Ok(agent_id) = agent_id.parse::<at_model::AgentId>() else {
            continue;
        };
        let Ok(layout) = StoreLayout::for_scope(paths, scope) else {
            continue;
        };
        let (store_hash, target_hash, mode, link_target) = if kind == "skill" {
            let Ok(slug) = Slug::parse(&slug) else {
                continue;
            };
            let package = layout.skill_dir(&slug);
            let store_hash = at_store::manifest::hash_skill_folder(&package)
                .ok()
                .map(|hash| hash.to_string());
            let linked = std::fs::symlink_metadata(native_path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink());
            // A link carries the canonical package content; hashing the link entry itself would
            // record the target string, not what the agent reads.
            let target_hash = if linked {
                store_hash.clone()
            } else {
                at_fs::hash_node(native_path)
                    .map_err(|_| CommandError::internal("the restored skill target is unreadable"))?
                    .map(|hash| hash.to_string())
            };
            // A row that still describes a materialised deployment keeps its recorded mode (a
            // drift undo restores bytes without rewriting what AgenTuna last did). A row the
            // undo re-materialised after an `off` state has `none` and needs the mode back.
            if current_mode.as_deref().is_some_and(|mode| mode != "none") {
                (
                    store_hash,
                    target_hash,
                    current_mode.clone().unwrap_or_default(),
                    current_link.clone(),
                )
            } else {
                let mode = if linked { "symlink" } else { "copy" };
                let link_target = linked.then(|| package.to_string_lossy().into_owned());
                (store_hash, target_hash, mode.to_owned(), link_target)
            }
        } else {
            let store_hash = std::fs::read(layout.mcp_file())
                .ok()
                .map(|bytes| at_fs::sha256_hex(&bytes));
            let target_hash = std::fs::read(native_path)
                .ok()
                .map(|bytes| at_fs::sha256_hex(&bytes));
            (store_hash, target_hash, String::from("native"), None)
        };
        let Some(target_hash) = target_hash else {
            continue;
        };
        let baseline_hash = if kind == "skill" {
            store_hash.clone()
        } else {
            Some(target_hash.clone())
        };
        at_db::deployments::update_hashes_and_mode(
            connection,
            &item_id.to_string(),
            &agent_id.to_string(),
            scope,
            project_id.as_deref(),
            store_hash.as_deref(),
            &target_hash,
            baseline_hash.as_deref(),
            &mode,
            link_target.as_deref(),
        )
        .map_err(|_| CommandError::internal("deployment hashes could not be refreshed"))?;
    }
    Ok(())
}

/// Recomputes the canonical hash every MCP row in an affected scope should carry.
///
/// Deleting or adding one server rewrites `mcp.json` for the whole scope. The undo restores the
/// bytes; this refreshes rows whose own native document the undo never touched.
fn refresh_undo_store_hashes(
    connection: &rusqlite::Connection,
    paths: &AppPaths,
    touched: &[PathBuf],
) -> Result<(), CommandError> {
    let project_root = paths
        .project_store()
        .and_then(|store| store.parent())
        .map(Path::to_path_buf);
    let project_id = match project_root.as_deref() {
        Some(root) => at_db::projects::find_project_by_path(connection, root)
            .map_err(|_| CommandError::internal("the project registry is unavailable"))?
            .map(|project| project.id),
        None => None,
    };
    for scope in [Scope::Global, Scope::User, Scope::Project, Scope::Local] {
        if matches!(scope, Scope::Project | Scope::Local) && project_root.is_none() {
            continue;
        }
        let Ok(layout) = StoreLayout::for_scope(paths, scope) else {
            continue;
        };
        if !touched.iter().any(|path| path.starts_with(layout.root())) {
            continue;
        }
        let Ok(bytes) = std::fs::read(layout.mcp_file()) else {
            continue;
        };
        let scope_project = if matches!(scope, Scope::Project | Scope::Local) {
            project_id.as_deref()
        } else {
            None
        };
        at_db::deployments::update_store_hash_for_scope(
            connection,
            at_model::ItemKind::McpServer,
            scope,
            scope_project,
            &at_fs::sha256_hex(&bytes),
        )
        .map_err(|_| CommandError::internal("the restored store hash could not be recorded"))?;
    }
    Ok(())
}

/// Makes the `items` projection of every affected store scope match its restored manifest.
///
/// A manifest entry is the canonical identity of a store item; the row is derived from it. The
/// reverse is not true, so a row absent from the restored manifest is debris from an undone
/// create and is removed here (its deployments cascade). A promote keeps the source manifest
/// entry while moving the row to the target scope, so a row stale in one scope is re-homed to
/// the scope whose restored manifest still lists it rather than deleted.
fn reconcile_undo_store_items(
    connection: &rusqlite::Connection,
    paths: &AppPaths,
    operation_scope: Option<Scope>,
    touched: &[PathBuf],
) -> Result<(), CommandError> {
    let project_root = paths
        .project_store()
        .and_then(|store| store.parent())
        .map(Path::to_path_buf);
    let project_id = match project_root.as_deref() {
        Some(root) => at_db::projects::find_project_by_path(connection, root)
            .map_err(|_| CommandError::internal("the project registry is unavailable"))?
            .map(|project| project.id),
        None => None,
    };
    // Load every scope manifest once: the stale-row decision needs to know whether another
    // restored scope still owns the identity before removing the row.
    let mut manifests = Vec::new();
    for scope in [Scope::Global, Scope::User, Scope::Project, Scope::Local] {
        if matches!(scope, Scope::Project | Scope::Local) && project_root.is_none() {
            continue;
        }
        let Ok(layout) = StoreLayout::for_scope(paths, scope) else {
            continue;
        };
        if !layout.root().is_dir() {
            continue;
        }
        let Ok(manifest) = StoreManifest::load(&layout) else {
            continue;
        };
        manifests.push((scope, layout, manifest.manifest));
    }
    for (scope, layout, manifest) in &manifests {
        let scope = *scope;
        let affected = operation_scope == Some(scope)
            || touched.iter().any(|path| path.starts_with(layout.root()));
        if !affected {
            continue;
        }
        let scope_project = if matches!(scope, Scope::Project | Scope::Local) {
            project_id.as_deref()
        } else {
            None
        };
        let mut manifest_ids = std::collections::BTreeSet::new();
        for item in manifest.items() {
            let spec = at_db::items::ManifestItemSpec {
                id: item.id,
                kind: item.kind,
                scope,
                project_id: scope_project,
                slug: item.slug.as_str(),
                name: item.name.as_deref().unwrap_or(item.slug.as_str()),
                source_path: &if item.kind == ItemKind::Skill {
                    layout.skill_dir(&item.slug).to_string_lossy().into_owned()
                } else {
                    item.path.as_str().to_owned()
                },
                content_hash: item.hash.as_str(),
            };
            at_db::items::sync_manifest_item(connection, &spec).map_err(|error| {
                CommandError::internal(format!(
                    "the restored store item could not be indexed: {error}"
                ))
            })?;
            manifest_ids.insert(item.id.to_string());
        }
        let stale = at_db::items::managed_ids_for_scope(connection, scope, scope_project)
            .map_err(|_| CommandError::internal("the item index is unavailable"))?;
        for text in stale {
            if manifest_ids.contains(&text) {
                continue;
            }
            if rehome_stale_item(connection, &manifests, &text, project_id.as_deref())? {
                continue;
            }
            let item_id: at_model::ItemId = text.parse().map_err(|_| {
                CommandError::internal("the item index holds an invalid identifier")
            })?;
            at_db::items::delete_item(connection, &item_id).map_err(|error| {
                CommandError::internal(format!(
                    "a stale item row could not be removed after undo: {error}"
                ))
            })?;
        }
    }
    Ok(())
}

/// Re-indexes a row another restored manifest still lists under that manifest's scope.
///
/// Returns `false` when no restored manifest owns the identity, so the caller removes the row.
fn rehome_stale_item(
    connection: &rusqlite::Connection,
    manifests: &[(Scope, StoreLayout, at_store::manifest::StoreManifest)],
    item_text: &str,
    project_id: Option<&str>,
) -> Result<bool, CommandError> {
    for (scope, layout, manifest) in manifests {
        let Some(item) = manifest
            .items()
            .iter()
            .find(|item| item.id.to_string() == item_text)
        else {
            continue;
        };
        let scope_project = if matches!(scope, Scope::Project | Scope::Local) {
            project_id
        } else {
            None
        };
        let source_path = if item.kind == ItemKind::Skill {
            layout.skill_dir(&item.slug).to_string_lossy().into_owned()
        } else {
            item.path.as_str().to_owned()
        };
        let spec = at_db::items::ManifestItemSpec {
            id: item.id,
            kind: item.kind,
            scope: *scope,
            project_id: scope_project,
            slug: item.slug.as_str(),
            name: item.name.as_deref().unwrap_or(item.slug.as_str()),
            source_path: &source_path,
            content_hash: item.hash.as_str(),
        };
        at_db::items::sync_manifest_item(connection, &spec).map_err(|error| {
            CommandError::internal(format!(
                "the re-homed store item could not be indexed: {error}"
            ))
        })?;
        return Ok(true);
    }
    Ok(false)
}

/// Repairs rule rows from the restored rule files.
///
/// Rule files live outside the stores and have no manifest: a restored file keeps its row and
/// needs its content hash refreshed, while a removed file's row is debris from an undone create.
fn reconcile_undo_rule_items(
    connection: &rusqlite::Connection,
    touched: &[PathBuf],
) -> Result<(), CommandError> {
    for path in touched {
        let text = path.to_string_lossy();
        if std::fs::symlink_metadata(path).is_ok() {
            if let Ok(bytes) = std::fs::read(path) {
                at_db::items::update_content_hash_for_source(
                    connection,
                    text.as_ref(),
                    &at_fs::sha256_hex(&bytes),
                )
                .map_err(|_| CommandError::internal("a rule row could not be repaired"))?;
            }
        } else {
            at_db::items::delete_rules_for_source(connection, text.as_ref())
                .map_err(|_| CommandError::internal("a rule row could not be removed"))?;
        }
    }
    Ok(())
}
