//! Typed read commands for the Overview screen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use at_engine::tombstones::TombstoneSource as _;
use at_store::layout::{STORE_DIR_MODE, StoreLayout};
use at_store::paths::AppPaths;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;

/// One scope card in the Overview screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewScopeView {
    /// Scope represented by this card.
    pub scope: String,
    /// Skill count.
    pub skills: i64,
    /// MCP server count.
    pub mcp: i64,
    /// Instruction/rule count.
    pub rules: i64,
    /// Lock count, including orphaned locks.
    pub locks: i64,
    /// Timestamp of the most recent change, if any.
    pub last_edited: Option<String>,
    /// Actor that performed the most recent change, when known (`ui`, `cli`, ...).
    pub last_edited_by: Option<String>,
    /// Number of disabled items.
    pub disabled: u64,
    /// Whether the scope already has a canonical store (`store.json` present).
    pub store_ready: bool,
    /// AgenTuna store directory for this card, shown after the native roots.
    pub store_root: String,
    /// Native discovery roots this scope's agents read, shown as `~/...` or `./...`.
    pub native_roots: Vec<String>,
    /// Uncommitted changes in a Project store's git work tree, when it has one.
    pub git_uncommitted: Option<u32>,
}

/// Per-kind totals and the shadow/disabled line of the Overview summary block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewEffectiveView {
    /// Skills indexed across every scope.
    pub skills: i64,
    /// MCP servers indexed across every scope.
    pub mcp: i64,
    /// Instruction files indexed across every scope.
    pub rules: i64,
    /// Skill copies a nearer scope wins over.
    pub shadowed_skills: usize,
    /// Disabled items at the active scope.
    pub disabled: u64,
}

/// Needs-attention row and its safe route target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewAttentionView {
    /// Stable item identity when the issue concerns an item.
    pub item_id: Option<String>,
    /// One-line row title.
    pub title: String,
    /// Supporting detail.
    pub subtitle: Option<String>,
    /// Problem class in snake case.
    pub kind: String,
    /// Screen to navigate to.
    pub screen: Option<String>,
    /// Exact row filter, when available.
    pub row_id: Option<String>,
}

/// One file touched by a recent operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewChangeFileView {
    /// Path recorded in the operation journal.
    pub path: String,
    /// Scope recorded for the operation.
    pub scope: Option<String>,
}
/// One recent operation and its touched-file count and paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewChangeView {
    /// Operation identifier.
    pub id: String,
    /// Operation kind.
    pub kind: String,
    /// Summary shown in the timeline.
    pub summary: Option<String>,
    /// When the operation occurred.
    pub created_at: String,
    /// Touched-file count.
    pub file_count: i64,
    /// Scope where it occurred.
    pub scope: Option<String>,
    /// Touched files loaded with the Overview query.
    pub files: Vec<OverviewChangeFileView>,
}

/// Complete database-backed Overview query result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewView {
    /// Scope cards including empty scopes.
    pub scopes: Vec<OverviewScopeView>,
    /// Needs-attention rows.
    pub attention: Vec<OverviewAttentionView>,
    /// Total attention count before display limit.
    pub attention_total: usize,
    /// Recent operations, newest first.
    pub changes: Vec<OverviewChangeView>,
    /// Per-kind totals for the active scope's summary block.
    pub effective: OverviewEffectiveView,
    /// Resolved user store directory for copy and path display.
    pub user_path: String,
    /// Global Scope card: resolved path, provenance, availability and its locks.
    pub global_store: super::privilege::GlobalStoreView,
    /// Non-blocking problems recorded while the app started (quarantine, failed rebuild).
    pub startup_warnings: Vec<super::startup::StartupWarningView>,
}

/// Wire name for an attention problem class; the UI dispatches its badge and
/// action from these exact strings, so they must stay snake_case.
fn attention_kind_wire(kind: &at_db::overview::AttentionKind) -> &'static str {
    use at_db::overview::AttentionKind;
    match kind {
        AttentionKind::FailingHealthCheck => "failing_health_check",
        AttentionKind::DriftEvent => "drift_event",
        AttentionKind::NativeDeployment => "native_deployment",
        AttentionKind::MissingAgent => "missing_agent",
        AttentionKind::ShadowedSkill => "shadowed_skill",
        // `AttentionKind` is `#[non_exhaustive]`: a future class must still load.
        _ => "attention",
    }
}

/// Loads Overview projections through the engine and shared database.
///
/// The Project card inspects the store's git work tree (a child process with a 1.5 s bound),
/// so the body runs on the blocking pool: on the IPC thread that inspection stalled the
/// webview's asset requests and the next screen's list query (TF-035).
#[tauri::command]
pub async fn overview_load(
    app: tauri::AppHandle,
    active_scope: String,
) -> Result<OverviewView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        overview_load_inner(&state, &active_scope)
    })
    .await
}

/// Bridge-facing body of [`overview_load`], callable without a Tauri app.
///
/// # Errors
/// See [`overview_load`].
pub fn overview_load_inner(
    state: &AppState,
    active_scope: &str,
) -> Result<OverviewView, CommandError> {
    let scope = active_scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("active_scope", "unknown scope"))?;
    let project_root = active_project_root(state)?;
    let paths = super::paths::resolve_command_paths(state, project_root.as_deref())
        .map_err(|_| CommandError::internal("could not resolve application paths"))?;
    let global_store = super::privilege::overview_for_state(state)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let disabled = disabled_counts(state, project_root.as_deref());
    let payload =
        at_engine::overview::build_with_disabled_counts(database.connection(), scope, &disabled)
            .map_err(|_| CommandError::internal("could not load Overview data"))?;
    let at_engine::overview::Overview {
        scopes: scope_cards,
        agents: agent_records,
        needs_attention,
        needs_attention_total,
        recent_changes,
        shadowed_skills,
        disabled_in_active_scope,
    } = payload;
    let home = paths.home_dir().map(Path::to_path_buf);
    let environment = home
        .as_ref()
        .map(|home| at_adapters::adapter::AgentEnvironment {
            home: home.clone(),
            project: project_root.clone(),
            variables: Default::default(),
        });
    let project_git = project_git_uncommitted(&paths);
    let mut effective = OverviewEffectiveView {
        skills: 0,
        mcp: 0,
        rules: 0,
        shadowed_skills,
        disabled: disabled_in_active_scope,
    };
    let scopes = scope_cards
        .into_iter()
        .map(|card| {
            effective.skills += card.counts.skills;
            effective.mcp += card.counts.mcp;
            effective.rules += card.counts.rules;
            let native_roots = environment.as_ref().map_or_else(Vec::new, |environment| {
                native_roots(
                    state.adapters(),
                    environment,
                    &agent_records,
                    card.scope,
                    home.as_deref(),
                    project_root.as_deref(),
                )
            });
            OverviewScopeView {
                scope: card.scope.to_string(),
                skills: card.counts.skills,
                mcp: card.counts.mcp,
                rules: card.counts.rules,
                locks: card.counts.locks,
                last_edited: card.last_edited,
                last_edited_by: card.last_edited_by,
                disabled: card.disabled_count,
                store_ready: scope_store_ready(&paths, &global_store, card.scope),
                store_root: store_root_for(&paths, &global_store, card.scope),
                native_roots,
                git_uncommitted: if card.scope == at_model::Scope::Project {
                    project_git
                } else {
                    None
                },
            }
        })
        .collect();
    let mut attention = needs_attention
        .into_iter()
        .map(|row| OverviewAttentionView {
            item_id: row.item_id.map(|id| id.to_string()),
            title: row.title,
            subtitle: row.subtitle,
            kind: attention_kind_wire(&row.kind).to_owned(),
            screen: row.navigation.as_ref().map(|target| target.screen.clone()),
            row_id: row.navigation.and_then(|target| target.row_id),
        })
        .collect::<Vec<_>>();
    let locks = at_db::locks::locks_for_scope(database.connection(), at_model::Scope::Global)
        .map_err(|_| CommandError::internal("could not query Global locks"))?;
    let lock_ids = locks.iter().map(|lock| lock.item_id).collect::<Vec<_>>();
    let indexed_ids = at_db::shared::items_by_ids(database.connection(), &lock_ids)
        .map_err(|_| CommandError::internal("could not query locked items"))?
        .into_iter()
        .map(|item| item.id)
        .collect::<std::collections::HashSet<_>>();
    for lock in locks {
        if !indexed_ids.contains(&lock.item_id) {
            let item_id = lock.item_id.to_string();
            attention.push(OverviewAttentionView {
                item_id: Some(item_id.clone()),
                title: "Locked item not found".to_owned(),
                subtitle: lock.reason,
                kind: "locked_item_not_found".to_owned(),
                screen: Some("effective".to_owned()),
                row_id: Some(item_id),
            });
        }
    }
    let changes = recent_changes
        .into_iter()
        .map(|row| {
            let detail = at_db::ops::operation_detail(database.connection(), &row.operation.id)
                .map_err(|_| CommandError::internal("could not query recent change files"))?;
            let scope = row.operation.scope.map(|scope| scope.to_string());
            let files = detail
                .into_iter()
                .flat_map(|detail| detail.files)
                .filter_map(|file| file.path)
                .map(|path| OverviewChangeFileView {
                    path,
                    scope: scope.clone(),
                })
                .collect();
            Ok(OverviewChangeView {
                id: row.operation.id,
                kind: row
                    .operation
                    .kind
                    .map(|kind| kind.as_str().to_owned())
                    .unwrap_or_else(|| "change".to_owned()),
                summary: row.operation.summary,
                created_at: row.operation.created_at,
                file_count: row.file_count,
                scope,
                files,
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    Ok(OverviewView {
        scopes,
        attention,
        attention_total: needs_attention_total,
        changes,
        effective,
        user_path: paths.user_store().display().to_string(),
        global_store,
        startup_warnings: state.startup_warnings().to_vec(),
    })
}

/// One entry a reviewed scope setup plan creates or finds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ScopeSetupFileView {
    /// Path relative to the store root.
    pub path: String,
    /// Planned operation (`mkdir` or `write`).
    pub action: String,
    /// Whether the entry does not exist yet.
    pub is_new: bool,
}

/// A reviewed plan that creates a scope's canonical store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ScopeSetupPreviewView {
    /// Content-bound identity required by apply; `None` when the store already exists.
    pub preview_id: Option<String>,
    /// Scope being set up.
    pub scope: String,
    /// Store root.
    pub path: String,
    /// Files and directories the plan creates.
    pub files: Vec<ScopeSetupFileView>,
    /// Foreign entries the plan leaves byte-identical.
    pub foreign: Vec<String>,
    /// Whether the scope already has a store and nothing will be written.
    pub already_ready: bool,
}

/// Result of applying a reviewed scope setup plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ScopeSetupApplyView {
    /// Operation identity usable by `operation_undo`.
    pub operation_id: String,
    /// Scope that was set up.
    pub scope: String,
    /// Store root.
    pub path: String,
    /// Number of entries created.
    pub files_changed: usize,
}

#[derive(Clone)]
struct CachedSetup {
    change_set: at_model::ChangeSet,
    preview_set: at_model::ChangeSet,
    preview_root: PathBuf,
    preview_id: String,
    store_root: PathBuf,
    backup_folder: PathBuf,
    authorized_roots: Vec<PathBuf>,
    creates_root: bool,
    creates_state_dir: bool,
    scope: at_model::Scope,
}

static SETUP_CACHE: LazyLock<Mutex<HashMap<String, CachedSetup>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Builds the reviewed change set that creates the selected scope's store.
///
/// Global requires the existing "Enable global writes" consent; Project and Local require a
/// registered project whose folder still exists. Nothing is written until
/// [`overview_setup_apply_inner`] applies the exact reviewed plan.
///
/// # Errors
/// Returns a typed conflict when Global writes are not authorized, an invalid-argument error for
/// an unknown scope or project, and a conflict when the store cannot be planned safely.
pub fn overview_setup_preview_inner(
    state: &AppState,
    scope_text: &str,
    project_id: Option<&str>,
) -> Result<ScopeSetupPreviewView, CommandError> {
    let scope = scope_text
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("scope", "unknown scope"))?;
    let (paths, layout) = setup_paths(state, scope, project_id)?;
    if scope == at_model::Scope::Global {
        let service = super::privilege::service(state)?;
        if !service.status().is_available() {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                "Enable Global writes before setting up the Global store.",
            ));
        }
    }
    let plan = layout
        .plan_init()
        .map_err(|error| setup_error(layout.root(), error))?;
    let path = layout.root().display().to_string();
    let foreign = plan
        .foreign
        .iter()
        .map(|entry| entry.display().to_string())
        .collect::<Vec<_>>();
    if plan.change_set.ops.is_empty() {
        return Ok(ScopeSetupPreviewView {
            preview_id: None,
            scope: scope.to_string(),
            path,
            files: Vec::new(),
            foreign,
            already_ready: true,
        });
    }
    let (preview_set, preview_root) = at_engine::preview::relative_to_common_root(&plan.change_set)
        .map_err(|_| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "The store setup plan cannot be reviewed safely.",
            )
        })?;
    let preview = at_engine::preview::preview(&preview_set, &preview_root).map_err(|_| {
        CommandError::new(
            CommandErrorCode::Conflict,
            "The store setup preview could not be read; reload and try again.",
        )
    })?;
    let preview_id = preview.id.clone();
    let files = plan
        .change_set
        .ops
        .iter()
        .zip(preview.files.iter())
        .map(|(op, file)| ScopeSetupFileView {
            path: op
                .destination()
                .and_then(|path| path.strip_prefix(layout.root()).ok())
                .map_or_else(
                    || file.path.display().to_string(),
                    |path| path.display().to_string(),
                ),
            action: file.action.clone(),
            is_new: file.is_new,
        })
        .collect();
    let authorized_root = nearest_existing_directory(layout.root())
        .filter(|root| root != Path::new("/"))
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::Unavailable,
                "No writable parent directory was found for this store.",
            )
        })?;
    let mut change_set = plan.change_set;
    change_set.preview_id = preview_id.clone();
    if matches!(scope, at_model::Scope::Project | at_model::Scope::Local) {
        change_set.project_id = project_id.map(str::to_owned);
    }
    SETUP_CACHE
        .lock()
        .map_err(|_| CommandError::internal("the store setup plan cache is unavailable"))?
        .insert(
            preview_id.clone(),
            CachedSetup {
                change_set,
                preview_set,
                preview_root,
                preview_id: preview_id.clone(),
                store_root: layout.root().to_path_buf(),
                backup_folder: paths.backup_dir().to_path_buf(),
                authorized_roots: vec![authorized_root],
                creates_root: plan.creates_root,
                creates_state_dir: plan.creates_state_dir,
                scope,
            },
        );
    Ok(ScopeSetupPreviewView {
        preview_id: Some(preview_id),
        scope: scope.to_string(),
        path,
        files,
        foreign,
        already_ready: false,
    })
}

/// Applies the exact reviewed plan; refuses when the store changed after review.
///
/// # Errors
/// Returns an invalid-argument error without explicit confirmation, a conflict for an expired or
/// stale preview, and a typed apply error when `at-fs` refuses the change set.
pub fn overview_setup_apply_inner(
    state: &AppState,
    preview_id: &str,
    confirmed: bool,
) -> Result<ScopeSetupApplyView, CommandError> {
    if !confirmed {
        return Err(CommandError::invalid_argument(
            "confirmed",
            "explicit confirmation is required",
        ));
    }
    let cached = SETUP_CACHE
        .lock()
        .map_err(|_| CommandError::internal("the store setup plan cache is unavailable"))?
        .remove(preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "the reviewed setup expired; preview again",
            )
        })?;
    if cached.preview_id != preview_id {
        return Err(CommandError::invalid_argument(
            "preview_id",
            "the reviewed setup identity does not match",
        ));
    }
    let current = at_engine::preview::preview(&cached.preview_set, &cached.preview_root)
        .map_err(|_| setup_stale())?;
    if current.id != cached.preview_id {
        return Err(setup_stale());
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let roots = at_fs::changeset::ApplyRoots::new(cached.authorized_roots.clone())
        .map_err(|_| CommandError::unavailable("the store root is not writable"))?;
    let outcome = {
        let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::changeset::ApplyContext {
            store_root: &cached.store_root,
            backup_folder: &cached.backup_folder,
            recorder: &mut recorder,
        };
        at_fs::changeset::apply_with_roots_and_registry(
            &cached.change_set,
            &mut context,
            &roots,
            &state.recent_writes(),
        )
        .map_err(|error| match error {
            at_fs::changeset::ChangeSetApplyError::Stale(_) => setup_stale(),
            other => CommandError::new(
                CommandErrorCode::Internal,
                format!("the reviewed store setup could not be applied: {other}"),
            ),
        })?
    };
    drop(database);
    if cached.creates_root {
        tighten_directory_mode(&cached.store_root);
    }
    if cached.creates_state_dir {
        tighten_directory_mode(&cached.store_root.join(at_store::layout::STATE_DIR));
    }
    Ok(ScopeSetupApplyView {
        operation_id: outcome.operation_id,
        scope: cached.scope.to_string(),
        path: cached.store_root.display().to_string(),
        files_changed: cached.change_set.ops.len(),
    })
}

/// Tauri wrapper for [`overview_setup_preview_inner`].
#[tauri::command]
pub fn overview_setup_preview(
    state: State<'_, AppState>,
    scope: String,
    project_id: Option<String>,
) -> Result<ScopeSetupPreviewView, CommandError> {
    overview_setup_preview_inner(&state, &scope, project_id.as_deref())
}

/// Tauri wrapper for [`overview_setup_apply_inner`].
#[tauri::command]
pub fn overview_setup_apply(
    state: State<'_, AppState>,
    preview_id: String,
    confirmed: bool,
) -> Result<ScopeSetupApplyView, CommandError> {
    overview_setup_apply_inner(&state, &preview_id, confirmed)
}

fn setup_stale() -> CommandError {
    CommandError::new(
        CommandErrorCode::Conflict,
        "The store changed since review; preview the setup again.",
    )
}

fn setup_error(path: &Path, error: at_store::layout::StoreError) -> CommandError {
    match error {
        at_store::layout::StoreError::RootNotDirectory { .. } => CommandError::new(
            CommandErrorCode::Conflict,
            format!("{} is not a directory.", path.display()),
        ),
        at_store::layout::StoreError::PermissionDenied { .. } => CommandError::new(
            CommandErrorCode::Unavailable,
            format!("{} is not writable by this user.", path.display()),
        ),
        other => CommandError::new(
            CommandErrorCode::Conflict,
            format!("The store could not be planned: {other}"),
        ),
    }
}

fn active_project_root(state: &AppState) -> Result<Option<PathBuf>, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not read application settings"))?;
    let Some(id) = settings.last_project_id else {
        return Ok(None);
    };
    let project = at_db::projects::find_project_by_id(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not read the selected project"))?;
    Ok(project
        .map(|project| PathBuf::from(project.path))
        .filter(|path| path.is_dir()))
}

fn scope_store_ready(
    paths: &AppPaths,
    global: &super::privilege::GlobalStoreView,
    scope: at_model::Scope,
) -> bool {
    let root = match scope {
        at_model::Scope::Global => PathBuf::from(&global.path),
        at_model::Scope::User => paths.user_store().to_path_buf(),
        at_model::Scope::Project => match paths.project_store() {
            Some(store) => store.to_path_buf(),
            None => return false,
        },
        at_model::Scope::Local => match paths.project_store() {
            Some(store) => store.join(at_store::layout::LOCAL_DIR),
            None => return false,
        },
    };
    StoreLayout::new(scope, root).is_ready()
}

/// Resolves the store directory a scope card shows under its native roots.
fn store_root_for(
    paths: &AppPaths,
    global: &super::privilege::GlobalStoreView,
    scope: at_model::Scope,
) -> String {
    let root = match scope {
        at_model::Scope::Global => PathBuf::from(&global.path),
        at_model::Scope::User => paths.user_store().to_path_buf(),
        at_model::Scope::Project | at_model::Scope::Local => match paths.project_store() {
            Some(store) => store.to_path_buf(),
            None => return String::new(),
        },
    };
    root.display().to_string()
}

/// Counts disabled items per scope from each store's `disabled[]` tombstones.
///
/// A store that cannot be read is left out: the Overview must still render when one
/// scope is broken, and the screen already reports store diagnostics separately.
fn disabled_counts(
    state: &AppState,
    project: Option<&Path>,
) -> std::collections::BTreeMap<at_model::Scope, u64> {
    let mut counts = std::collections::BTreeMap::new();
    let Ok(loaded) = state.load_scope_snapshots(project) else {
        return counts;
    };
    for scope in [
        at_model::Scope::Global,
        at_model::Scope::User,
        at_model::Scope::Project,
    ] {
        if let Ok(entries) = loaded.tombstones.disabled(scope) {
            counts.insert(scope, u64::try_from(entries.len()).unwrap_or(u64::MAX));
        }
    }
    counts
}

/// Counts uncommitted changes inside the Project store's git work tree.
///
/// The Overview is a query the screen waits on, so the probe gets a short deadline of its
/// own: a large or slow work tree reports nothing instead of stalling the card.
fn project_git_uncommitted(paths: &AppPaths) -> Option<u32> {
    let layout = StoreLayout::for_scope(paths, at_model::Scope::Project).ok()?;
    if !layout.root().is_dir() {
        return None;
    }
    let contract = at_store::git_contract::ProjectGitContract::new(layout).ok()?;
    let runner = at_git::ProcessLocalGitRunner::new("git", OVERVIEW_GIT_TIMEOUT);
    let state = contract.inspect(&runner).ok()?;
    u32::try_from(state.status.len()).ok()
}

/// How long the Overview card waits for one `git status` of the Project store.
const OVERVIEW_GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Native discovery roots of managed agents at one scope, collapsed to `~/x` or `./x`.
fn native_roots(
    registry: &at_adapters::adapter::AgentAdapterRegistry,
    environment: &at_adapters::adapter::AgentEnvironment,
    agents: &[at_db::agents::AgentSummary],
    scope: at_model::Scope,
    home: Option<&Path>,
    project: Option<&Path>,
) -> Vec<String> {
    if !matches!(scope, at_model::Scope::User | at_model::Scope::Project) {
        return Vec::new();
    }
    let mut roots: Vec<String> = Vec::new();
    for agent in agents.iter().filter(|agent| agent.managed) {
        let Ok(Some(adapter)) = registry.get(&agent.id) else {
            continue;
        };
        for kind in [
            at_model::ItemKind::Skill,
            at_model::ItemKind::McpServer,
            at_model::ItemKind::Rule,
        ] {
            for candidate in adapter.paths(kind, scope, environment) {
                let Some(root) = display_native_root(&candidate.path, home, project) else {
                    continue;
                };
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
    }
    roots
}

/// The top-level entry of `path` under `base`, e.g. `.claude` for `~/.claude/skills`.
fn top_level_entry(path: &Path, base: &Path) -> Option<String> {
    let rest = path.strip_prefix(base).ok()?;
    let mut components = rest.components();
    let first = components
        .next()?
        .as_os_str()
        .to_string_lossy()
        .into_owned();
    if first.is_empty() || first == "~" {
        return None;
    }
    Some(first)
}

/// Renders one native path as the root an agent reads: `~/<top>` or `./<top>`.
fn display_native_root(path: &Path, home: Option<&Path>, project: Option<&Path>) -> Option<String> {
    // The project is often inside the home directory, so test it first.
    if let Some(project) = project
        && let Some(entry) = top_level_entry(path, project)
    {
        return Some(if entry.starts_with('.') {
            format!("./{entry}")
        } else {
            entry
        });
    }
    if let Some(home) = home
        && let Some(entry) = top_level_entry(path, home)
    {
        return Some(format!("~/{entry}"));
    }
    None
}

fn setup_paths(
    state: &AppState,
    scope: at_model::Scope,
    project_id: Option<&str>,
) -> Result<(AppPaths, StoreLayout), CommandError> {
    let project_root = match scope {
        at_model::Scope::Project | at_model::Scope::Local => {
            let id = project_id.ok_or_else(|| {
                CommandError::invalid_argument(
                    "project_id",
                    "select a project before setting up this scope",
                )
            })?;
            let database = state.database().map_err(CommandError::unavailable)?;
            let project = at_db::projects::find_project_by_id(database.connection(), id)
                .map_err(|_| CommandError::internal("could not query the project registry"))?
                .ok_or_else(|| {
                    CommandError::invalid_argument("project_id", "the project is not registered")
                })?;
            let path = PathBuf::from(project.path);
            if !path.is_dir() {
                return Err(CommandError::invalid_argument(
                    "project_id",
                    "the project folder does not exist",
                ));
            }
            Some(path)
        }
        _ => None,
    };
    let paths = if scope == at_model::Scope::Global {
        super::privilege::service(state)?.paths().clone()
    } else {
        super::paths::resolve_command_paths(state, project_root.as_deref())?
    };
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|error| CommandError::invalid_argument("scope", &error.to_string()))?;
    Ok((paths, layout))
}

fn nearest_existing_directory(path: &Path) -> Option<PathBuf> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        if current.is_dir() {
            return Some(current.to_path_buf());
        }
        candidate = current.parent();
    }
    None
}

fn tighten_directory_mode(path: &Path) {
    if let Err(error) = at_fs::set_directory_mode(path, STORE_DIR_MODE) {
        tracing::warn!(path = %path.display(), %error, "could not tighten store directory mode");
    }
}

/// Explicitly prepares the Global store after the user requests write access.
///
/// Runs the shell's one Global service, so the action and any later Global write are recorded in
/// the `operations` history through the same seam.
///
/// # Errors
/// Returns a typed unavailable error if authorization is denied or the store remains read-only.
#[tauri::command]
pub fn overview_enable_global_writes(state: State<'_, AppState>) -> Result<(), CommandError> {
    let service = super::privilege::service(&state)?;
    service
        .enable_global_writes()
        .map_err(|_| CommandError::unavailable("Global store authorization failed"))?;
    if !service.status().is_available() {
        return Err(CommandError::unavailable(
            "Global store is still not writable by this user",
        ));
    }
    Ok(())
}
