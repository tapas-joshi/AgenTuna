//! Tauri commands for the add-skill wizard and its reviewed file operations.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_engine::{
    skill_templates::{DescriptionIssue, SkillDraft, SkillTemplate, render_preview, slugify},
    skill_wizard::{AddSkillWizard, SourceKind, WizardEffect, WizardEvent, WizardStep},
};
use at_model::{
    AgentId, ItemKind, Scope,
    network::{NetworkAction, NetworkEventLog, NetworkTrigger},
};
use at_store::{layout::StoreLayout, skill_package::SkillPackage};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use ts_rs::TS;

use super::{
    commands::CommandError, network::NetworkState, paths::resolve_process_paths, state::AppState,
};

/// Opens the native folder picker used by Local folder skill imports.
#[tauri::command]
pub async fn skill_import_choose_folder(app: AppHandle) -> Result<Option<String>, CommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose a skill folder")
            .blocking_pick_folder()
            .map(|selected| {
                selected
                    .into_path()
                    .map(|path| path.display().to_string())
                    .map_err(|_| {
                        CommandError::unavailable("the selected folder path is unavailable")
                    })
            })
            .transpose()
    })
    .await
    .map_err(|_| CommandError::internal("folder picker task failed"))?
}

/// Opens the native ZIP picker used by ZIP skill imports.
#[tauri::command]
pub async fn skill_import_choose_zip(app: AppHandle) -> Result<Option<String>, CommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose a skill ZIP")
            .add_filter("ZIP archive", &["zip"])
            .blocking_pick_file()
            .map(|selected| {
                selected
                    .into_path()
                    .map(|path| path.display().to_string())
                    .map_err(|_| CommandError::unavailable("the selected ZIP path is unavailable"))
            })
            .transpose()
    })
    .await
    .map_err(|_| CommandError::internal("ZIP picker task failed"))?
}

/// One discovered local skill and its parsed preview, if valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillImportCandidateView {
    /// Candidate directory relative to the selected import root.
    pub id: String,
    /// Parsed skill name.
    pub name: Option<String>,
    /// Parsed skill description.
    pub description: Option<String>,
    /// Original SKILL.md text for a read-only preview.
    pub markdown: Option<String>,
    /// Parse error for candidates that cannot be selected.
    pub error: Option<String>,
}

/// Read-only candidates returned after scanning one local folder or ZIP file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillImportCandidatesView {
    /// Parsed candidates; invalid entries remain visible.
    pub candidates: Vec<SkillImportCandidateView>,
    /// Whether a ZIP validation failure prevented candidate listing.
    pub error: Option<String>,
}
/// One source store shown by the library import picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillLibrarySourceView {
    /// Opaque source identifier, stable for this local catalog snapshot.
    pub id: String,
    /// Human-readable source label.
    pub label: String,
    /// Source category (`scope` or `project`).
    pub kind: String,
    /// Number of importable skills.
    pub count: usize,
    /// Last-used project time, when known.
    pub last_used_at: Option<String>,
}

/// One skill offered by a source store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillLibrarySkillView {
    /// Opaque source-skill identifier accepted by the catalog-bound wizard.
    pub id: String,
    /// Source label.
    pub source_label: String,
    /// Source-store identifier for source filtering.
    pub source_id: String,
    /// Original skill package path.
    pub path: String,
    /// Parsed name, if valid.
    pub name: Option<String>,
    /// Parsed description, if valid.
    pub description: Option<String>,
    /// Reason an invalid skill cannot be selected.
    pub error: Option<String>,
    /// Why the destination name is unavailable, if it clashes.
    pub clash_reason: Option<String>,
}

/// Read-only source and skill inventory for the library wizard step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillLibraryCatalogView {
    /// Other non-empty scope stores and registered projects.
    pub sources: Vec<SkillLibrarySourceView>,
    /// Skills offered by those sources.
    pub skills: Vec<SkillLibrarySkillView>,
    /// Target scope summary shown beside the Copy/Link selector.
    pub target_scope: String,
}

/// Loads a read-only library catalog for a destination scope.
#[tauri::command]
pub fn skill_library_catalog(
    state: State<'_, AppState>,
    scope: String,
    project_id: Option<String>,
) -> Result<SkillLibraryCatalogView, CommandError> {
    use std::str::FromStr;

    let scope = Scope::from_str(&scope)
        .map_err(|_| CommandError::invalid_argument("scope", "choose a supported scope"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = project_root(database.connection(), project_id.as_deref(), scope)?;
    let paths = resolve_process_paths(project.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let (sources, skills) = at_engine::skill_import::enumerate_registered_library(
        database.connection(),
        paths.global_store(),
        paths.user_store(),
        scope,
        project_id.as_deref(),
        project.as_deref(),
    )
    .map_err(|_| CommandError::unavailable("the local skill library could not be read"))?;
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("the target skill store is unavailable"))?;
    let sources = sources
        .into_iter()
        .map(|source| SkillLibrarySourceView {
            id: source.path.to_string_lossy().into_owned(),
            label: source.label,
            kind: match source.kind {
                at_engine::skill_import::LibrarySourceKind::Scope => "scope".to_owned(),
                at_engine::skill_import::LibrarySourceKind::Project => "project".to_owned(),
                _ => "unknown".to_owned(),
            },
            count: source.count,
            last_used_at: source.last_used_at,
        })
        .collect();
    let skills = skills
        .into_iter()
        .map(|skill| {
            let clash_reason = skill.name.as_deref().and_then(|name| {
                at_engine::skill_templates::slugify(name)
                    .ok()
                    .and_then(|slug| at_store::layout::Slug::parse(&slug).ok())
                    .filter(|slug| layout.skill_dir(slug).exists())
                    .map(|_| {
                        "A skill with this name already exists in the target scope.".to_owned()
                    })
            });
            let id = skill.path.to_string_lossy().into_owned();
            SkillLibrarySkillView {
                id,
                source_label: skill.source_label,
                source_id: skill.source_root.to_string_lossy().into_owned(),
                path: skill.path.to_string_lossy().into_owned(),
                name: skill.name,
                description: skill.description,
                error: skill.error,
                clash_reason,
            }
        })
        .collect();
    Ok(SkillLibraryCatalogView {
        sources,
        skills,
        target_scope: scope.to_string(),
    })
}

/// Scans a user-selected folder or ZIP without modifying its contents.
#[tauri::command]
pub fn skill_import_candidates(
    path: String,
    zip: bool,
    project_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<SkillImportCandidatesView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = project_root(database.connection(), project_id.as_deref(), Scope::Project)?;
    let paths = resolve_process_paths(project.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let selected = Path::new(path.trim());
    if selected.as_os_str().is_empty() || !allowed_import_path(selected, &paths, project.as_deref())
    {
        return Err(CommandError::invalid_argument(
            "path",
            "choose a folder or ZIP under the current home or registered project",
        ));
    }
    let candidates = if zip {
        let imported = at_engine::skill_import::import_zip(selected, paths.cache_dir())
            .map_err(|error| CommandError::invalid_argument("path", &error.to_string()))?;
        imported.candidates
    } else {
        at_engine::skill_import::import_folder(selected)
            .map_err(|error| CommandError::invalid_argument("path", &error.to_string()))?
    };
    Ok(SkillImportCandidatesView {
        candidates: candidates
            .into_iter()
            .map(|candidate| SkillImportCandidateView {
                id: candidate.subpath.to_string_lossy().replace('\\', "/"),
                name: candidate.name,
                description: candidate.description,
                markdown: candidate.doc.map(|doc| doc.raw),
                error: candidate.error,
            })
            .collect(),
        error: None,
    })
}

/// Fetches a Git repository after the user explicitly requests candidate discovery.
#[tauri::command]
pub fn skill_import_git_candidates(
    url: String,
    reference: Option<String>,
    network: State<'_, NetworkState>,
) -> Result<SkillImportCandidatesView, CommandError> {
    let host = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .filter(|host| !host.is_empty() && !host.contains('@'))
        .ok_or_else(|| CommandError::invalid_argument("url", "enter a valid HTTPS Git URL"))?;
    network
        .authorize(NetworkAction::GitClone, host, NetworkTrigger::User)
        .map_err(|_| CommandError::unavailable("Network policy refused this Git source."))?;
    let paths = resolve_process_paths(None)
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let policy = network.policy();
    let mut log = NetworkEventLog::default();
    let imported = at_engine::skill_import::import_git(
        &url,
        reference
            .as_deref()
            .filter(|reference| !reference.trim().is_empty()),
        paths.cache_dir(),
        &policy,
        &mut log,
    )
    .map_err(|_| {
        CommandError::unavailable(
            "Git source could not be cloned. Check the URL and network policy.",
        )
    })?;
    Ok(SkillImportCandidatesView {
        candidates: imported
            .candidates
            .into_iter()
            .map(|candidate| SkillImportCandidateView {
                id: candidate.subpath.to_string_lossy().replace('\\', "/"),
                name: candidate.name,
                description: candidate.description,
                markdown: candidate.doc.map(|doc| doc.raw),
                error: candidate.error,
            })
            .collect(),
        error: None,
    })
}

/// One skills.sh marketplace search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MarketplaceSkillView {
    /// Stable marketplace item identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// GitHub author.
    pub author: String,
    /// Reported install count.
    pub installs: u64,
    /// Git repository URL used to import the skill.
    pub source: String,
    /// Short listing description.
    pub description: String,
}

/// Search results and their cache/offline provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MarketplaceSearchView {
    /// Marketplace rows.
    pub results: Vec<MarketplaceSkillView>,
    /// Results came from cache because network access was unavailable.
    pub offline: bool,
    /// The cached result is older than the marketplace freshness period.
    pub stale: bool,
    /// Data provider.
    pub provenance: String,
}

/// Searches or refreshes the skills.sh marketplace after an explicit user action.
#[tauri::command]
pub async fn skill_marketplace_search(
    query: String,
    refresh: bool,
    network: State<'_, NetworkState>,
) -> Result<MarketplaceSearchView, CommandError> {
    let policy = network.policy();
    if !policy.marketplace_enabled() && !policy.offline() {
        return Err(CommandError::unavailable(
            "Marketplace access is disabled by network policy.",
        ));
    }
    let paths = resolve_process_paths(None)
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    query.trim().to_ascii_lowercase().hash(&mut hasher);
    let cache_file = paths
        .cache_dir()
        .join(format!("skills-sh-{:016x}.json", hasher.finish()));
    at_mcp::ensure_crypto_provider();
    let client = at_git::marketplace::MarketplaceClient::new()
        .map_err(|_| CommandError::unavailable("Marketplace search is unavailable"))?;
    let mut log = NetworkEventLog::default();
    let page = if refresh {
        client.refresh(&query, &cache_file, &policy, &mut log).await
    } else {
        client.search(&query, &cache_file, &policy, &mut log).await
    }
    .map_err(|_| {
        CommandError::unavailable("Marketplace search failed or no cached results are available")
    })?;
    Ok(MarketplaceSearchView {
        results: page
            .skills
            .into_iter()
            .map(|skill| MarketplaceSkillView {
                id: skill.id,
                name: skill.name,
                author: skill.author,
                installs: skill.installs,
                source: skill.source_url,
                description: skill.description,
            })
            .collect(),
        offline: page.offline,
        stale: page.stale,
        provenance: "skills.sh".to_owned(),
    })
}

/// Applies one typed wizard event without performing filesystem or network work.
#[tauri::command]
pub fn skill_wizard_transition(
    wizard: Option<AddSkillWizard>,
    event: WizardEvent,
) -> (AddSkillWizard, Vec<WizardEffect>) {
    let mut wizard = wizard.unwrap_or_default();
    let effects = wizard.dispatch(event);
    (wizard, effects)
}

/// Read-only preview generated by the canonical skill template renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTemplatePreviewView {
    /// Canonical filesystem slug when the name is valid.
    pub slug: Option<String>,
    /// Complete generated SKILL.md document.
    pub markdown: String,
    /// Name parser issue, if any.
    pub name_error: Option<String>,
    /// Description parser issue, if any.
    pub description_error: Option<String>,
}

/// Renders a live preview with the same engine used by skill creation.
#[tauri::command]
pub fn skill_template_preview(
    name: String,
    description: String,
    template: String,
) -> Result<SkillTemplatePreviewView, CommandError> {
    let template = SkillTemplate::ALL
        .into_iter()
        .find(|candidate| candidate.as_str() == template)
        .ok_or_else(|| CommandError::invalid_argument("template", "choose a supported template"))?;
    let preview = render_preview(&SkillDraft::new(&name, &description, template));
    Ok(SkillTemplatePreviewView {
        slug: preview.slug,
        markdown: preview.markdown,
        name_error: preview.name_error.map(|error| error.to_string()),
        description_error: preview.description_error.map(|issue| match issue {
            DescriptionIssue::Empty => "Description is required".to_owned(),
            DescriptionIssue::TooLong { len, limit } => {
                format!("Description is {len} characters; the limit is {limit}")
            }
            _ => "Description is invalid".to_owned(),
        }),
    })
}

/// Request to build a read-only Add Skill review plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddPreviewRequest {
    /// The completed wizard draft at the Review step.
    pub wizard: AddSkillWizard,
    /// Registered project identity required for Project and Local scope.
    pub project_id: Option<String>,
    /// Whether the user explicitly requested Git staging.
    pub stage_requested: bool,
}

/// One exact filesystem path proposed by the skill planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddFileView {
    /// The target path the plan will write or link.
    pub path: String,
    /// Relative file name within the skill package.
    pub package_path: String,
    /// Unified text diff for an existing file, when present.
    pub diff: Option<String>,
}

/// One agent deployment from the reviewed plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddDeploymentView {
    /// Stable agent identifier.
    pub agent: String,
    /// Exact destination path.
    pub path: String,
    /// `symlink` or `copy`.
    pub mode: String,
}

/// Read-only review result; `preview_id` is required to apply it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddPreviewView {
    /// Content-bound identity of the reviewed plan.
    pub preview_id: String,
    /// Canonical skill package directory.
    pub canonical_dir: String,
    /// Every actual target path from the planner.
    pub files: Vec<SkillAddFileView>,
    /// Native agent deployments in this plan.
    pub deployments: Vec<SkillAddDeploymentView>,
    /// Whether optional Git staging is valid for this plan.
    pub stage_available: bool,
    /// Whether Git staging was requested.
    pub stage_requested: bool,
    /// Whether existing content requires explicit user confirmation.
    pub requires_overwrite_confirmation: bool,
}

/// Request to apply exactly one reviewed Add Skill plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddApplyRequest {
    /// Identity returned by `skill_add_preview`.
    pub preview_id: String,
    /// Required when the review plan contains existing targets.
    pub confirm_overwrites: bool,
}

/// Actual outcome of applying a reviewed skill add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddApplyResultView {
    /// Display name confirmed by the successful apply result.
    pub skill_name: String,
    /// Operation journal identity.
    pub operation_id: String,
    /// Paths reported by the applied plan.
    pub paths_written: Vec<String>,
    /// Actual agent deployments.
    pub deployments: Vec<SkillAddDeploymentView>,
    /// Whether the reviewed paths were staged.
    pub staged: bool,
    /// Exact paths passed to Git staging.
    pub staged_paths: Vec<String>,
    /// Typed, actionable staging failure after filesystem apply succeeded.
    pub stage_error: Option<String>,
    /// Recovery hint tied to the operation and real backup catalog.
    pub backup_hint: String,
}

struct CachedSkillAddPlan {
    skill_name: String,
    plan: at_engine::skill_add_plan::SkillAddFilePlan,
    layout_root: PathBuf,
    backup_folder: PathBuf,
    apply_roots: Vec<PathBuf>,
    repository: Option<at_git::Repository>,
}

fn skill_add_cache() -> &'static Mutex<HashMap<String, CachedSkillAddPlan>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedSkillAddPlan>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Builds and caches a read-only, exact filesystem preview for Add Skill.
#[tauri::command]
pub fn skill_add_preview(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    request: SkillAddPreviewRequest,
) -> Result<SkillAddPreviewView, CommandError> {
    use std::str::FromStr;

    let wizard = &request.wizard;
    if wizard.step != WizardStep::Review {
        return Err(CommandError::invalid_argument(
            "wizard.step",
            "finish the source and destination steps before reviewing",
        ));
    }
    let source = wizard
        .source
        .as_ref()
        .ok_or_else(|| CommandError::invalid_argument("source", "choose an import source"))?;
    let name = wizard.name.trim();
    if name.is_empty() {
        return Err(CommandError::invalid_argument("name", "enter a skill name"));
    }
    if wizard.agents.is_empty() {
        return Err(CommandError::invalid_argument(
            "agents",
            "select at least one agent",
        ));
    }
    let scope = Scope::from_str(
        wizard
            .scope
            .as_deref()
            .ok_or_else(|| CommandError::invalid_argument("scope", "choose a scope"))?,
    )
    .map_err(|_| CommandError::invalid_argument("scope", "choose a supported scope"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let project_root = project_root(database.connection(), request.project_id.as_deref(), scope)?;
    let paths = resolve_process_paths(project_root.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("canonical skill store is unavailable"))?;
    let slug_text = slugify(name)
        .map_err(|_| CommandError::invalid_argument("name", "use letters or digits in the name"))?;
    let slug = at_store::layout::Slug::parse(&slug_text).map_err(|_| {
        CommandError::invalid_argument("name", "the skill name is not a valid slug")
    })?;
    let package = resolve_skill_package(
        source,
        SkillPackageContext {
            wizard: &request.wizard,
            paths: &paths,
            project_root: project_root.as_deref(),
            target_scope: scope,
            target_project_id: request.project_id.as_deref(),
            connection: database.connection(),
            network: &network,
        },
    )?;
    let env = AgentEnvironment {
        home: paths
            .home_dir()
            .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
            .to_path_buf(),
        project: project_root.clone(),
        variables: Default::default(),
    };
    let registry = AgentAdapterRegistry::register();
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not read deployment settings"))?;
    let mut deployments = Vec::with_capacity(wizard.agents.len());
    let mut roots = vec![env.home.clone()];
    if let Some(root) = project_root
        .as_ref()
        .filter(|root| !root.starts_with(&env.home))
    {
        roots.push(root.clone());
    }
    if !layout.root().starts_with(&env.home)
        && project_root
            .as_ref()
            .is_none_or(|root| !layout.root().starts_with(root))
    {
        roots.push(existing_authorized_ancestor(layout.root())?);
    }
    for raw_agent in &wizard.agents {
        let agent = AgentId::from_str(raw_agent)
            .map_err(|_| CommandError::invalid_argument("agents", "contains an unknown agent"))?;
        let adapter = registry
            .get(&agent)
            .map_err(|_| CommandError::internal("agent registry is unavailable"))?
            .ok_or_else(|| {
                CommandError::invalid_argument("agents", "agent adapter is unavailable")
            })?;
        let candidate = adapter
            .paths(ItemKind::Skill, scope, &env)
            .into_iter()
            .find(|candidate| candidate.primary)
            .ok_or_else(|| {
                CommandError::invalid_argument("scope", "agent has no skill path for this scope")
            })?;
        if !candidate.path.starts_with(&env.home)
            && project_root
                .as_ref()
                .is_none_or(|root| !candidate.path.starts_with(root))
        {
            roots.push(existing_authorized_ancestor(&candidate.path)?);
        }
        let mode = settings.link_mode(&agent);
        let mode = if mode == at_model::LinkMode::Copy {
            at_engine::skill_add_plan::SkillDeploymentMode::Copy
        } else {
            at_engine::skill_add_plan::SkillDeploymentMode::Symlink
        };
        deployments.push(at_engine::skill_add_plan::SkillDeploymentTarget {
            agent: agent.to_string(),
            target: candidate.path.join(slug.as_str()),
            allowed_root: candidate.path,
            mode,
        });
    }
    let repository = if scope == Scope::Project {
        project_root.as_deref().and_then(|root| {
            at_git::detect_repository(&at_git::ProcessLocalGitRunner::git(), root).ok()
        })
    } else {
        None
    };
    let plan =
        at_engine::skill_add_plan::plan_skill_add(at_engine::skill_add_plan::SkillAddPlanRequest {
            layout: &layout,
            slug: &slug,
            package: &package,
            scope,
            deployments: &deployments,
            repository: repository.as_ref(),
            stage_requested: request.stage_requested,
        })
        .map_err(|error| CommandError::invalid_argument("plan", &error.to_string()))?;
    if request.stage_requested && plan.stage.is_none() {
        return Err(CommandError::invalid_argument(
            "stage_requested",
            "Git staging is unavailable for this project",
        ));
    }
    let preview_id = uuid::Uuid::now_v7().to_string();
    let mut plan = plan;
    plan.change_set.preview_id.clone_from(&preview_id);
    let files = plan
        .files
        .iter()
        .map(|file| SkillAddFileView {
            path: file.path.display().to_string(),
            package_path: file.package_path.clone(),
            diff: plan
                .overwrites
                .iter()
                .find(|overwrite| overwrite.path == file.path)
                .map(|overwrite| overwrite.diff.clone()),
        })
        .collect();
    let deployment_views = deployment_views(&plan);
    let stage_available = plan.stage_available;
    let requires_overwrite_confirmation = !plan.overwrites.is_empty();
    let cache = CachedSkillAddPlan {
        skill_name: wizard.name.trim().to_owned(),
        plan,
        layout_root: layout.root().to_path_buf(),
        backup_folder: paths.support_dir().join(at_store::layout::BACKUPS_DIR),
        apply_roots: roots,
        repository,
    };
    skill_add_cache()
        .lock()
        .map_err(|_| CommandError::internal("skill plan cache is unavailable"))?
        .insert(preview_id.clone(), cache);
    Ok(SkillAddPreviewView {
        preview_id,
        canonical_dir: layout.skill_dir(&slug).display().to_string(),
        files,
        deployments: deployment_views,
        stage_available,
        stage_requested: request.stage_requested,
        requires_overwrite_confirmation,
    })
}

/// Applies a reviewed Add Skill plan and optionally stages its exact Git path list.
#[tauri::command]
pub fn skill_add_apply(
    state: State<'_, AppState>,
    request: SkillAddApplyRequest,
) -> Result<SkillAddApplyResultView, CommandError> {
    let cached = skill_add_cache()
        .lock()
        .map_err(|_| CommandError::internal("skill plan cache is unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "this review expired; return to Review and preview the current files again",
            )
        })?;
    if !cached.plan.overwrites.is_empty() && !request.confirm_overwrites {
        return Err(CommandError::invalid_argument(
            "confirm_overwrites",
            "review and confirm every existing-file change before importing",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let roots = at_fs::changeset::ApplyRoots::new(cached.apply_roots)
        .map_err(|_| CommandError::unavailable("authorized skill directories are unavailable"))?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &cached.layout_root,
        backup_folder: &cached.backup_folder,
        recorder: &mut recorder,
    };
    let outcome = at_fs::changeset::apply_with_roots(&cached.plan.change_set, &mut context, &roots)
        .map_err(|error| match error {
            at_fs::changeset::ChangeSetApplyError::Stale(path) => CommandError::new(
                super::commands::CommandErrorCode::Conflict,
                format!(
                    "The reviewed target changed at {}. Remedy: return to Review and preview the current files.",
                    path.display()
                ),
            ),
            other => CommandError::from(other),
        })?;
    state.record_writes(&cached.plan.change_set, &cached.layout_root);
    let mut staged = false;
    let mut staged_paths = Vec::new();
    let mut stage_error = None;
    if let (Some(stage), Some(repository)) = (&cached.plan.stage, &cached.repository) {
        let runner = at_git::ProcessLocalGitRunner::git();
        let refs = stage.paths.iter().map(String::as_str).collect::<Vec<_>>();
        match at_git::add_paths(&runner, repository, &refs) {
            Ok(()) => {
                staged = true;
                staged_paths.clone_from(&stage.paths);
            }
            Err(error) => {
                stage_error = Some(format!(
                    "The skill was added but could not be staged ({error}). Review Git status and stage the listed files manually."
                ));
            }
        }
    }
    Ok(SkillAddApplyResultView {
        skill_name: cached.skill_name,
        operation_id: outcome.operation_id.clone(),
        paths_written: cached
            .plan
            .files
            .iter()
            .map(|file| file.path.display().to_string())
            .collect(),
        deployments: deployment_views(&cached.plan),
        staged,
        staged_paths,
        stage_error,
        backup_hint: format!(
            "Operation {} is recorded. Backups for replaced files, if any, are stored in {} and can be restored through operation history and recovery.",
            outcome.operation_id,
            cached.backup_folder.display()
        ),
    })
}

fn project_root(
    connection: &rusqlite::Connection,
    project_id: Option<&str>,
    scope: Scope,
) -> Result<Option<PathBuf>, CommandError> {
    match project_id {
        Some(id) => at_db::projects::find_project_by_id(connection, id)
            .map_err(|_| CommandError::internal("could not read the selected project"))?
            .map(|project| Some(PathBuf::from(project.path)))
            .ok_or_else(|| {
                CommandError::invalid_argument("project_id", "project is not registered")
            }),
        None if matches!(scope, Scope::Project | Scope::Local) => {
            Err(CommandError::invalid_argument(
                "project_id",
                "this scope requires a registered project",
            ))
        }
        None => Ok(None),
    }
}

struct SkillPackageContext<'a> {
    wizard: &'a AddSkillWizard,
    paths: &'a at_store::paths::AppPaths,
    project_root: Option<&'a Path>,
    target_scope: Scope,
    target_project_id: Option<&'a str>,
    connection: &'a rusqlite::Connection,
    network: &'a NetworkState,
}

fn resolve_skill_package(
    source: &SourceKind,
    context: SkillPackageContext<'_>,
) -> Result<SkillPackage, CommandError> {
    let SkillPackageContext {
        wizard,
        paths,
        project_root,
        target_scope,
        target_project_id,
        connection,
        network,
    } = context;
    match source {
        SourceKind::New => {
            let template = wizard.details.template.as_deref().unwrap_or("blank");
            let template = SkillTemplate::ALL
                .into_iter()
                .find(|candidate| candidate.as_str() == template)
                .ok_or_else(|| {
                    CommandError::invalid_argument("template", "choose a supported template")
                })?;
            let rendered = render_preview(&SkillDraft::new(
                wizard.name.trim(),
                wizard.details.description.trim(),
                template,
            ));
            let mut package = SkillPackage::new();
            package
                .insert("SKILL.md", rendered.markdown.into_bytes(), 0o644)
                .map_err(|_| {
                    CommandError::invalid_argument("template", "could not build the skill document")
                })?;
            Ok(package)
        }
        SourceKind::Folder => {
            let path = Path::new(wizard.details.folder_path.trim());
            if path.as_os_str().is_empty() || !allowed_import_path(path, paths, project_root) {
                return Err(CommandError::invalid_argument(
                    "folder_path",
                    "choose a folder under the current home or registered project",
                ));
            }
            let candidates = at_engine::skill_import::import_folder(path).map_err(|_| {
                CommandError::invalid_argument(
                    "folder_path",
                    "the selected folder could not be scanned",
                )
            })?;
            let candidate = select_import_candidate(
                &candidates,
                wizard.details.folder_selection.as_deref(),
                "folder_selection",
            )?;
            SkillPackage::read(&path.join(&candidate.subpath))
                .map(|loaded| loaded.package)
                .map_err(|_| {
                    CommandError::invalid_argument(
                        "folder_path",
                        "the selected folder is not a safe skill package",
                    )
                })
        }
        SourceKind::Library => {
            let id = wizard.details.library_selection.as_deref().ok_or_else(|| {
                CommandError::invalid_argument("library_selection", "choose a library skill")
            })?;
            let (_, candidates) = at_engine::skill_import::enumerate_registered_library(
                connection,
                paths.global_store(),
                paths.user_store(),
                target_scope,
                target_project_id,
                project_root,
            )
            .map_err(|_| CommandError::unavailable("the local skill library could not be read"))?;
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.path.to_string_lossy() == id)
            {
                return SkillPackage::read(&candidate.path)
                    .map(|loaded| loaded.package)
                    .map_err(|_| {
                        CommandError::unavailable(
                            "the library skill package changed or is unreadable",
                        )
                    });
            }
            let parsed = id.parse::<at_model::ItemId>().map_err(|_| {
                CommandError::invalid_argument(
                    "library_selection",
                    "the selected skill is no longer available",
                )
            })?;
            let item = at_db::shared::items_by_ids(connection, std::slice::from_ref(&parsed))
                .map_err(|_| CommandError::internal("could not read the skill library"))?
                .into_iter()
                .find(|item| item.kind == ItemKind::Skill)
                .ok_or_else(|| {
                    CommandError::invalid_argument(
                        "library_selection",
                        "the selected skill is no longer available",
                    )
                })?;
            let path = item.source_path.as_deref().map(Path::new).ok_or_else(|| {
                CommandError::unavailable("the library skill has no readable package path")
            })?;
            SkillPackage::read(path)
                .map(|loaded| loaded.package)
                .map_err(|_| {
                    CommandError::unavailable("the library skill package changed or is unreadable")
                })
        }
        SourceKind::Git => {
            let url = wizard.details.repo_url.trim();
            let Some(host) = url
                .strip_prefix("https://")
                .and_then(|rest| rest.split('/').next())
                .filter(|host| !host.is_empty() && !host.contains('@'))
            else {
                return Err(CommandError::invalid_argument(
                    "repo_url",
                    "enter a valid HTTPS Git URL",
                ));
            };
            network
                .authorize(NetworkAction::GitClone, host, NetworkTrigger::User)
                .map_err(|_| {
                    CommandError::unavailable(
                        "Network policy refused this Git source. Enable Git access and retry.",
                    )
                })?;
            let policy = network.policy();
            let mut log = NetworkEventLog::default();
            let imported = at_engine::skill_import::import_git(
                url,
                nonempty(&wizard.details.repo_reference),
                paths.cache_dir(),
                &policy,
                &mut log,
            )
            .map_err(|_| {
                CommandError::unavailable(
                    "Git source could not be cloned. Check the URL, access and network policy.",
                )
            })?;
            let selection = wizard
                .details
                .fetched_repo_selection
                .as_deref()
                .ok_or_else(|| {
                    CommandError::invalid_argument(
                        "fetched_repo_selection",
                        "select a skill from the repository",
                    )
                })?;
            let candidate = imported
                .candidates
                .iter()
                .find(|candidate| candidate.subpath == Path::new(selection) && candidate.is_valid())
                .ok_or_else(|| {
                    CommandError::invalid_argument(
                        "fetched_repo_selection",
                        "the selected repository skill is no longer valid",
                    )
                })?;
            SkillPackage::read(&imported.repository_root().join(&candidate.subpath))
                .map(|loaded| loaded.package)
                .map_err(|_| {
                    CommandError::unavailable(
                        "the selected Git skill package changed during preview",
                    )
                })
        }
        SourceKind::Zip => {
            let path = Path::new(wizard.details.zip_path.trim());
            if path.as_os_str().is_empty() || !allowed_import_path(path, paths, project_root) {
                return Err(CommandError::invalid_argument(
                    "zip_path",
                    "choose an archive under the current home or registered project",
                ));
            }
            let imported =
                at_engine::skill_import::import_zip(path, paths.cache_dir()).map_err(|_| {
                    CommandError::invalid_argument(
                        "zip_path",
                        "the ZIP archive is invalid or unsafe",
                    )
                })?;
            let candidate = select_import_candidate(
                &imported.candidates,
                wizard.details.zip_selection.as_deref(),
                "zip_selection",
            )?;
            SkillPackage::read(&imported.staging_root().join(&candidate.subpath))
                .map(|loaded| loaded.package)
                .map_err(|_| CommandError::unavailable("the extracted skill package is unreadable"))
        }
        SourceKind::Marketplace => {
            let id = wizard.details.marketplace_id.trim();
            let mut id_parts = id.split('/');
            let (Some(author), Some(repository), Some(skill_name)) =
                (id_parts.next(), id_parts.next(), id_parts.next())
            else {
                return Err(CommandError::invalid_argument(
                    "marketplace_id",
                    "select a valid marketplace listing",
                ));
            };
            if id_parts.next().is_some()
                || [author, repository, skill_name]
                    .iter()
                    .any(|part| part.is_empty())
            {
                return Err(CommandError::invalid_argument(
                    "marketplace_id",
                    "select a valid marketplace listing",
                ));
            }
            let listing = at_git::marketplace::MarketplaceSkill {
                id: id.to_owned(),
                name: wizard.name.clone(),
                description: String::new(),
                author: author.to_owned(),
                installs: 0,
                source_url: format!("https://github.com/{author}/{repository}"),
                updated_at: None,
            };
            let url = at_git::marketplace::MarketplaceClient::authorize_install(
                &listing,
                &network.policy(),
            )
            .map_err(|_| {
                CommandError::unavailable("Network policy refused this marketplace install.")
            })?;
            let host = "github.com";
            network
                .authorize(NetworkAction::GitClone, host, NetworkTrigger::User)
                .map_err(|_| {
                    CommandError::unavailable("Git clone access is disabled by network policy.")
                })?;
            let policy = network.policy();
            let mut log = NetworkEventLog::default();
            let imported = at_engine::skill_import::import_git(
                url,
                None,
                paths.cache_dir(),
                &policy,
                &mut log,
            )
            .map_err(|_| {
                CommandError::unavailable("Marketplace skill source could not be fetched.")
            })?;
            let candidate = imported
                .candidates
                .iter()
                .find(|candidate| {
                    candidate.is_valid()
                        && (candidate
                            .name
                            .as_deref()
                            .is_some_and(|name| name.eq_ignore_ascii_case(skill_name))
                            || candidate.subpath.file_name().is_some_and(|name| {
                                name.to_string_lossy().eq_ignore_ascii_case(skill_name)
                            }))
                })
                .ok_or_else(|| {
                    CommandError::invalid_argument(
                        "marketplace_id",
                        "the selected marketplace skill was not found in its repository",
                    )
                })?;
            SkillPackage::read(&imported.repository_root().join(&candidate.subpath))
                .map(|loaded| loaded.package)
                .map_err(|_| {
                    CommandError::unavailable("the marketplace skill package is unreadable")
                })
        }
        _ => Err(CommandError::invalid_argument(
            "source",
            "choose a supported skill source",
        )),
    }
}

fn select_import_candidate<'a>(
    candidates: &'a [at_engine::skill_import::SkillImportCandidate],
    selection: Option<&str>,
    argument: &str,
) -> Result<&'a at_engine::skill_import::SkillImportCandidate, CommandError> {
    let valid = candidates
        .iter()
        .filter(|candidate| candidate.is_valid())
        .collect::<Vec<_>>();
    let selected = match selection {
        Some(selection) => valid
            .into_iter()
            .find(|candidate| candidate.subpath == Path::new(selection)),
        None if valid.len() == 1 => valid.first().copied(),
        None => None,
    };
    selected
        .ok_or_else(|| CommandError::invalid_argument(argument, "choose one valid skill candidate"))
}

fn allowed_import_path(
    path: &Path,
    paths: &at_store::paths::AppPaths,
    project_root: Option<&Path>,
) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    paths.home_dir().is_some_and(|home| path.starts_with(home))
        || project_root.is_some_and(|project| path.starts_with(project))
}

fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub(super) fn existing_authorized_ancestor(path: &Path) -> Result<PathBuf, CommandError> {
    if !path.is_absolute() {
        return Err(CommandError::unavailable(
            "an agent skill path is not absolute and cannot be authorized",
        ));
    }
    let mut ancestor = path;
    while !ancestor.is_dir() {
        ancestor = ancestor.parent().ok_or_else(|| {
            CommandError::unavailable("no existing directory can authorize the skill path")
        })?;
    }
    let canonical = ancestor
        .canonicalize()
        .map_err(|_| CommandError::unavailable("the skill path root cannot be verified"))?;
    if canonical == Path::new("/") {
        return Err(CommandError::unavailable(
            "the skill path is too broad to authorize safely",
        ));
    }
    Ok(canonical)
}
fn deployment_views(
    plan: &at_engine::skill_add_plan::SkillAddFilePlan,
) -> Vec<SkillAddDeploymentView> {
    plan.deployments
        .iter()
        .map(|deployment| SkillAddDeploymentView {
            agent: deployment.agent.clone(),
            path: deployment.target.display().to_string(),
            mode: if deployment.mode == at_engine::skill_add_plan::SkillDeploymentMode::Copy {
                String::from("copy")
            } else {
                String::from("symlink")
            },
        })
        .collect()
}
