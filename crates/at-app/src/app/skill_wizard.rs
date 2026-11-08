//! Tauri commands for the add-skill wizard and its reviewed file operations.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, OnceLock},
};

use super::{
    bridge::{JobError, JobId, JobSpec, ProgressSender},
    commands::CommandError,
    network::NetworkState,
    paths::resolve_process_paths,
    state::AppState,
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
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use ts_rs::TS;

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
/// Local source selected for a background skill import job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum SkillImportKind {
    /// Scan a native folder selected by the user.
    Folder,
    /// Validate and extract a ZIP selected by the user.
    Zip,
}

/// Job handle returned when a skill import starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillImportJobStartView {
    /// Identifier used to cancel or observe progress for this job.
    pub job_id: JobId,
    /// Opaque key used to retrieve and release its candidate session.
    pub import_id: String,
}

enum SkillImportSessionSource {
    Git(at_engine::skill_import::GitSkillImport),
    Folder(PathBuf),
    Zip(at_engine::skill_import::ZipSkillImport),
}

struct SkillImportSession {
    candidates: SkillImportCandidatesView,
    source: SkillImportSessionSource,
    source_candidates: Vec<at_engine::skill_import::SkillImportCandidate>,
}

impl SkillImportSessionSource {
    fn candidate_root(&self) -> &Path {
        match self {
            Self::Git(imported) => imported.repository_root(),
            Self::Folder(root) => root,
            Self::Zip(imported) => imported.candidate_root(),
        }
    }

    fn matches_source(&self, source: &SourceKind) -> bool {
        matches!(
            (self, source),
            (Self::Git(_), SourceKind::Git)
                | (Self::Folder(_), SourceKind::Folder)
                | (Self::Zip(_), SourceKind::Zip)
        )
    }
}

static SKILL_IMPORT_SESSIONS: LazyLock<Mutex<HashMap<String, SkillImportSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn skill_import_sessions() -> &'static Mutex<HashMap<String, SkillImportSession>> {
    &SKILL_IMPORT_SESSIONS
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

/// One registered project the catalog could not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillLibrarySkipView {
    /// Project label.
    pub label: String,
    /// Project folder that could not be read.
    pub path: String,
    /// Why it was skipped and what the user can do about it.
    pub reason: String,
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
    /// Skills folder searched for the empty state, abbreviated with `~`.
    pub search_root: String,
    /// Registered projects that were skipped, with the reason.
    pub skipped: Vec<SkillLibrarySkipView>,
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
    let catalog = at_engine::skill_import::enumerate_registered_library(
        database.connection(),
        paths.global_store(),
        paths.user_store(),
        scope,
        project_id.as_deref(),
        project.as_deref(),
    )
    .map_err(|error| {
        tracing::warn!(error = %error, "skill library catalog could not be read");
        CommandError::unavailable(format!(
            "The local skill library could not be read: {error}. Check that the scope stores and project folders are readable, then retry."
        ))
    })?;
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("the target skill store is unavailable"))?;
    let search_root = display_home_path(&paths.user_store().join("skills"), paths.home_dir());
    let sources = catalog
        .sources
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
    let skills = catalog
        .skills
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
    let skipped = catalog
        .skipped
        .into_iter()
        .map(|skip| SkillLibrarySkipView {
            label: skip.label,
            path: skip.path.display().to_string(),
            reason: skip.reason,
        })
        .collect();
    Ok(SkillLibraryCatalogView {
        sources,
        skills,
        target_scope: scope.to_string(),
        search_root,
        skipped,
    })
}

/// Renders `path` with the injected home directory abbreviated to `~`.
fn display_home_path(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Host of an HTTPS Git import URL, refusing empty hosts and any embedded credentials.
///
/// The UI accepts HTTPS clone URLs only; a `user:password@host` or `token@host` form would be
/// written to the store manifest and the sources table, so it is refused before network access.
fn import_git_host(url: &str) -> Result<&str, CommandError> {
    url.strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .filter(|host| !host.is_empty() && !host.contains('@'))
        .ok_or_else(|| CommandError::invalid_argument("url", "enter a valid HTTPS Git URL"))
}

/// Starts a cancellable Git clone and skill-candidate scan.
#[tauri::command]
pub fn skill_import_git_start(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    url: String,
    reference: Option<String>,
) -> Result<SkillImportJobStartView, CommandError> {
    let url = url.trim();
    if url.is_empty() || url.len() > 2_048 {
        return Err(CommandError::invalid_argument(
            "url",
            "enter an HTTPS repository URL no longer than 2048 bytes",
        ));
    }
    let host = import_git_host(url)?;
    if reference.as_deref().is_some_and(|value| {
        value.len() > 512 || value.starts_with('-') || value.contains(['\0', '\n', '\r'])
    }) {
        return Err(CommandError::invalid_argument(
            "reference",
            "enter a valid Git ref",
        ));
    }
    network
        .authorize(NetworkAction::GitClone, host, NetworkTrigger::User)
        .map_err(|_| CommandError::unavailable("Network policy refused this Git source."))?;
    let paths = resolve_process_paths(None)
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let cache_dir = paths.cache_dir().to_path_buf();
    let policy = network.policy();
    let worker_network = network.inner().clone();
    let import_id = uuid::Uuid::now_v7().to_string();
    let job_import_id = import_id.clone();
    let url = url.to_owned();
    let reference = reference.filter(|value| !value.trim().is_empty());
    let job = JobSpec::new_cancellable(
        "Import skills from Git",
        move |progress, cancel| async move {
            progress.message("Cloning Git repository");
            let cancellation = cancel.cancellation_flag();
            let worker_progress = progress.clone();
            let worker = tokio::task::spawn_blocking(move || {
                let mut log = NetworkEventLog::default();
                let result = at_engine::skill_import::import_git_with_cancel(
                    &url,
                    reference.as_deref(),
                    &cache_dir,
                    &policy,
                    &mut log,
                    cancellation,
                    |count| worker_progress.message(format!("Found {count} skill candidates")),
                );
                (result, log.to_vec())
            })
            .await
            .map_err(|_| JobError::Failed {
                message: "Git import worker stopped unexpectedly.".to_owned(),
            })?;
            let (result, events) = worker;
            for event in events {
                worker_network.record(event);
            }
            if cancel.is_cancelled() {
                return Ok(());
            }
            let imported = result.map_err(|error| JobError::Failed {
                message: git_import_error_message(error),
            })?;
            let view = SkillImportCandidatesView {
                candidates: imported
                    .candidates
                    .iter()
                    .map(import_candidate_view)
                    .collect(),
                error: None,
            };
            let count = view.candidates.len();
            let source_candidates = imported.candidates.clone();
            store_skill_import_session(
                job_import_id,
                SkillImportSession {
                    candidates: view,
                    source: SkillImportSessionSource::Git(imported),
                    source_candidates,
                },
            )?;
            progress.fraction(1.0, format!("Found {count} skill candidates"));
            Ok(())
        },
    );
    let handle = state
        .bridge()
        .spawn_job(job)
        .map_err(|_| CommandError::unavailable("the background job service is unavailable"))?;
    Ok(SkillImportJobStartView {
        job_id: handle.id().clone(),
        import_id,
    })
}

/// Starts a cancellable folder scan or ZIP validation and extraction job.
#[tauri::command]
pub fn skill_import_local_start(
    state: State<'_, AppState>,
    path: String,
    kind: SkillImportKind,
    project_id: Option<String>,
) -> Result<SkillImportJobStartView, CommandError> {
    let path = path.trim();
    if path.is_empty() || path.len() > 4_096 || !Path::new(path).is_absolute() {
        return Err(CommandError::invalid_argument(
            "path",
            "choose an absolute folder or ZIP path no longer than 4096 bytes",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = project_root(database.connection(), project_id.as_deref(), Scope::Project)?;
    let paths = resolve_process_paths(project.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let selected = PathBuf::from(path);
    if !allowed_import_path(&selected, &paths, project.as_deref()) {
        return Err(CommandError::invalid_argument(
            "path",
            "choose a folder or ZIP under the current home or registered project",
        ));
    }
    let cache_dir = paths.cache_dir().to_path_buf();
    let import_id = uuid::Uuid::now_v7().to_string();
    let job_import_id = import_id.clone();
    let (job_name, start_label) = match kind {
        SkillImportKind::Folder => ("Scan skill folder", "Scanning selected folder"),
        SkillImportKind::Zip => ("Import skills from ZIP", "Validating ZIP archive"),
    };
    let job = JobSpec::new_cancellable(job_name, move |progress, cancel| async move {
        progress.message(start_label);
        let cancellation = cancel.cancellation_flag();
        let worker_progress = progress.clone();
        let worker_path = selected;
        let worker_cache = cache_dir;
        let worker = tokio::task::spawn_blocking(move || match kind {
            SkillImportKind::Folder => {
                let result = at_engine::skill_import::import_folder_with_cancel(
                    &worker_path,
                    cancellation.as_ref(),
                    |count| worker_progress.message(format!("Found {count} skill candidates")),
                );
                result
                    .map(|candidates| {
                        let view = SkillImportCandidatesView {
                            candidates: candidates.iter().map(import_candidate_view).collect(),
                            error: None,
                        };
                        (
                            view,
                            SkillImportSessionSource::Folder(worker_path),
                            candidates,
                        )
                    })
                    .map_err(|error| error.to_string())
            }
            SkillImportKind::Zip => {
                let extraction_progress = worker_progress.clone();
                let scan_progress = worker_progress.clone();
                let result = at_engine::skill_import::import_zip_with_cancel(
                    &worker_path,
                    &worker_cache,
                    cancellation.as_ref(),
                    move |completed, total| {
                        if total > 0 {
                            extraction_progress.fraction(
                                completed as f64 / total as f64,
                                format!("Extracting ZIP archive ({completed}/{total})"),
                            );
                        }
                    },
                    move |count| {
                        scan_progress.message(format!("Found {count} skill candidates"));
                    },
                );
                result
                    .map(|imported| {
                        let view = SkillImportCandidatesView {
                            candidates: imported
                                .candidates
                                .iter()
                                .map(import_candidate_view)
                                .collect(),
                            error: None,
                        };
                        let candidates = imported.candidates.clone();
                        (view, SkillImportSessionSource::Zip(imported), candidates)
                    })
                    .map_err(|error| error.to_string())
            }
        })
        .await
        .map_err(|_| JobError::Failed {
            message: "Local skill import worker stopped unexpectedly.".to_owned(),
        })?;
        if cancel.is_cancelled() {
            return Ok(());
        }
        let (view, source, source_candidates) =
            worker.map_err(|message| JobError::Failed { message })?;
        let count = view.candidates.len();
        store_skill_import_session(
            job_import_id,
            SkillImportSession {
                candidates: view,
                source,
                source_candidates,
            },
        )?;
        progress.fraction(1.0, format!("Found {count} skill candidates"));
        Ok(())
    });
    let handle = state
        .bridge()
        .spawn_job(job)
        .map_err(|_| CommandError::unavailable("the background job service is unavailable"))?;
    Ok(SkillImportJobStartView {
        job_id: handle.id().clone(),
        import_id,
    })
}

/// Returns candidates from a successfully completed import job.
#[tauri::command]
pub fn skill_import_job_result(
    import_id: String,
) -> Result<SkillImportCandidatesView, CommandError> {
    validate_import_id(&import_id)?;
    skill_import_sessions()
        .lock()
        .map_err(|_| CommandError::internal("skill import sessions are unavailable"))?
        .get(&import_id)
        .map(|session| session.candidates.clone())
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "import_id",
                "the import session expired; fetch or scan the source again",
            )
        })
}

/// Releases a completed import session and its temporary clone or extraction directory.
#[tauri::command]
pub fn skill_import_release(import_id: String) -> Result<bool, CommandError> {
    validate_import_id(&import_id)?;
    Ok(skill_import_sessions()
        .lock()
        .map_err(|_| CommandError::internal("skill import sessions are unavailable"))?
        .remove(&import_id)
        .is_some())
}

fn validate_import_id(import_id: &str) -> Result<(), CommandError> {
    uuid::Uuid::parse_str(import_id).map_err(|_| {
        CommandError::invalid_argument("import_id", "expected a valid import session id")
    })?;
    Ok(())
}

fn import_candidate_view(
    candidate: &at_engine::skill_import::SkillImportCandidate,
) -> SkillImportCandidateView {
    SkillImportCandidateView {
        id: candidate.subpath.to_string_lossy().replace('\\', "/"),
        name: candidate.name.clone(),
        description: candidate.description.clone(),
        markdown: candidate.doc.as_ref().map(|doc| doc.raw.clone()),
        error: candidate.error.clone(),
    }
}

fn store_skill_import_session(
    import_id: String,
    session: SkillImportSession,
) -> Result<(), JobError> {
    let mut sessions = skill_import_sessions()
        .lock()
        .map_err(|_| JobError::Failed {
            message: "Skill import sessions are unavailable.".to_owned(),
        })?;
    sessions.insert(import_id, session);
    Ok(())
}

fn git_import_error_message(error: at_git::import::ImportCloneError) -> String {
    match error {
        at_git::import::ImportCloneError::AuthRequired => String::from(
            "Authentication is required. Use a public repository URL; AgenTuna does not request credentials here.",
        ),
        at_git::import::ImportCloneError::CloneFailed { message } => {
            format!("Git clone failed: {message}")
        }
        at_git::import::ImportCloneError::Cancelled => String::from("Git import was cancelled."),
        _ => String::from("Git source could not be cloned. Check the URL and network policy."),
    }
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
    /// Import-session identity required by Git, folder and ZIP sources.
    pub import_id: Option<String>,
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
/// One selected package and its explicit source-to-destination plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddItemView {
    /// Parsed skill name shown to the user.
    pub name: String,
    /// Read-only selected source directory, when the source has one.
    pub source_path: Option<String>,
    /// Destination library entry.
    pub target_path: String,
    /// `copy` or `link`.
    pub mode: String,
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
    /// Canonical skill package directory, or the shared skills store for a batch.
    pub canonical_dir: String,
    /// Selected packages and their Copy/Link destinations.
    pub items: Vec<SkillAddItemView>,
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
    /// Store creation the same reviewed plan performs first, when the scope has no store yet.
    pub store_init: Option<super::store_setup::StoreSetupView>,
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
    /// Display names confirmed by the successful apply result.
    pub skill_names: Vec<String>,
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

/// Job and one-time result key returned when a reviewed skill plan starts applying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAddApplyJobStartView {
    /// Job identifier used to observe apply progress and completion.
    pub job_id: JobId,
    /// One-time key used to retrieve the successful apply result.
    pub result_id: String,
}
/// Request for the Scope & agents step's live targets and name-clash results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillScopeOptionsRequest {
    /// Selected destination scope, if one has been chosen.
    pub scope: Option<String>,
    /// Agent identifiers currently selected in the wizard.
    pub agents: Vec<String>,
    /// Skill names from the selected source to check for clashes.
    pub names: Vec<String>,
    /// Registered project identity for Project and Local scope.
    pub project_id: Option<String>,
}

/// One destination scope card in the add-skill wizard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillScopeCardView {
    /// Scope identifier.
    pub scope: String,
    /// Absolute AgenTuna store path, when the scope is available.
    pub store_path: Option<String>,
    /// Whether this scope can be selected in the current context.
    pub available: bool,
    /// Why this scope is unavailable, if applicable.
    pub disabled_reason: Option<String>,
    /// Context note shown on the card.
    pub note: String,
    /// Global-lock summary shown on the card, if any.
    pub lock_note: Option<String>,
}

/// One selected skill's exact destination for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillScopeTargetPathView {
    /// Skill name.
    pub name: String,
    /// Absolute native deployment target path.
    pub path: String,
}

/// Scope-specific skill target information for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAgentScopeView {
    /// Stable agent identifier.
    pub agent: String,
    /// Display name.
    pub name: String,
    /// Whether the agent is detected as installed.
    pub installed: bool,
    /// Why this agent cannot be selected, if applicable.
    pub disabled_reason: Option<String>,
    /// Absolute skills directory for this scope.
    pub skills_dir: Option<String>,
    /// Exact absolute deployment paths for selected skill names.
    pub targets: Vec<SkillScopeTargetPathView>,
    /// Other agents coupled through the same skills directory.
    pub shared_with: Vec<String>,
}

/// Inline collision and warning results for one selected skill name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillNameClashView {
    /// Skill name checked.
    pub name: String,
    /// Collisions that prevent creating the skill at this scope.
    pub blocking: Vec<String>,
    /// Existing copies in other scopes.
    pub warnings: Vec<String>,
}

/// Live scope, agent target and clash data for the Scope & agents step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillScopeOptionsView {
    /// Global, User, Project and Local destination cards.
    pub scopes: Vec<SkillScopeCardView>,
    /// Scope-specific paths and installation state for detected agents.
    pub agents: Vec<SkillAgentScopeView>,
    /// Name-clash results for the selected destination and agents.
    pub clashes: Vec<SkillNameClashView>,
}

/// Resolves scope cards and agent targets, then checks selected names using the engine.
#[tauri::command]
pub fn skill_scope_options(
    state: State<'_, AppState>,
    request: SkillScopeOptionsRequest,
) -> Result<SkillScopeOptionsView, CommandError> {
    skill_scope_options_inner(&state, &request)
}

/// Bridge-facing body of [`skill_scope_options`], callable without a Tauri app.
///
/// # Errors
/// See [`skill_scope_options`].
pub fn skill_scope_options_inner(
    state: &AppState,
    request: &SkillScopeOptionsRequest,
) -> Result<SkillScopeOptionsView, CommandError> {
    use std::str::FromStr;

    if request.names.len() > 100
        || request.names.iter().any(|name| name.len() > 256)
        || request.agents.len() > 64
    {
        return Err(CommandError::invalid_argument(
            "request",
            "the scope request exceeds its supported limits",
        ));
    }
    let selected_scope = request
        .scope
        .as_deref()
        .map(Scope::from_str)
        .transpose()
        .map_err(|_| CommandError::invalid_argument("scope", "choose a supported scope"))?;
    // The database guard is scoped to these reads: `agents_list_impl` below re-locks the
    // same non-reentrant mutex, and holding the guard across that call self-deadlocked
    // the main thread, freezing the whole webview on the wizard's Scope & agents step.
    let (selected_project, global_store_override, global_locks) = {
        let database = state.database().map_err(CommandError::unavailable)?;
        let selected_project = project_root(
            database.connection(),
            request.project_id.as_deref(),
            Scope::User,
        )?;
        let settings = at_db::settings::load(database.connection())
            .map_err(|_| CommandError::internal("could not load store settings"))?;
        let global_locks = at_db::locks::locks_for_scope(database.connection(), Scope::Global)
            .map_err(|_| CommandError::internal("could not read Global locks"))?;
        (
            selected_project,
            settings.global_store_path().map(Path::to_path_buf),
            global_locks,
        )
    };
    let mut paths = state
        .paths()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    if let Some(project) = selected_project.as_deref() {
        paths = paths.for_project(project).map_err(|_| {
            CommandError::invalid_argument("project_id", "project path is unavailable")
        })?;
    }
    paths.apply_settings_global_store(global_store_override.as_deref());
    let global_status = at_fs::privilege::GlobalStoreStatus::probe(paths.global_store());
    let mut scopes = Vec::with_capacity(Scope::ALL.len());
    let mut stores = Vec::with_capacity(Scope::ALL.len());
    for &scope in Scope::ALL {
        let layout = StoreLayout::for_scope(&paths, scope).ok();
        let project_required = matches!(scope, Scope::Project | Scope::Local);
        let available = match scope {
            Scope::Global => global_status.is_available(),
            Scope::Project | Scope::Local => selected_project.is_some(),
            Scope::User => true,
        };
        let disabled_reason = match scope {
            Scope::Global => global_status.reason().map(str::to_owned),
            Scope::Project | Scope::Local if selected_project.is_none() => {
                Some("Choose a project before using this scope.".to_owned())
            }
            _ => None,
        };
        let note = match scope {
            Scope::Global => "Machine-wide. Requires administrator access.".to_owned(),
            Scope::User => "Your account on this Mac; not committed to a repository.".to_owned(),
            Scope::Project => "Shared with people using this repository.".to_owned(),
            Scope::Local => "Only you in this repo. Local files are not committed.".to_owned(),
        };
        let lock_note = (scope == Scope::Global && !global_locks.is_empty()).then(|| {
            format!(
                "{} Global item{} protected by locks.",
                global_locks.len(),
                if global_locks.len() == 1 {
                    " is"
                } else {
                    "s are"
                },
            )
        });
        let store_path = layout
            .as_ref()
            .map(|layout| layout.root().display().to_string());
        if let Some(layout) = layout
            && (!project_required || selected_project.is_some())
        {
            stores.push((scope, layout.root().to_path_buf()));
        }
        scopes.push(SkillScopeCardView {
            scope: scope.wire_name().to_owned(),
            store_path,
            available,
            disabled_reason,
            note,
            lock_note,
        });
    }
    let env = AgentEnvironment {
        home: paths
            .home_dir()
            .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
            .to_path_buf(),
        project: selected_project.clone(),
        variables: Default::default(),
    };
    let registry = AgentAdapterRegistry::register();
    let cards = super::workflows::agents_list_impl(state)?;
    let mut agents = Vec::with_capacity(cards.len());
    for card in cards {
        let installed = card.installed;
        let mut skills_dir = None;
        let mut targets = Vec::new();
        if let Some(scope) = selected_scope {
            let id = AgentId::from_str(&card.id)
                .map_err(|_| CommandError::internal("agent index contains an invalid id"))?;
            let adapter = registry
                .get(&id)
                .map_err(|_| CommandError::internal("agent registry is unavailable"))?
                .ok_or_else(|| CommandError::internal("agent adapter is unavailable"))?;
            let target = adapter
                .paths(ItemKind::Skill, scope, &env)
                .into_iter()
                .find(|candidate| candidate.primary)
                .map(|candidate| candidate.path);
            if let Some(target_root) = target {
                skills_dir = Some(target_root.display().to_string());
                for name in &request.names {
                    let slug = match slugify(name) {
                        Ok(slug) => slug,
                        Err(_) => continue,
                    };
                    if let Ok(slug) = at_store::layout::Slug::parse(&slug) {
                        targets.push(SkillScopeTargetPathView {
                            name: name.clone(),
                            path: target_root.join(slug.as_str()).display().to_string(),
                        });
                    }
                }
            }
        }
        let disabled_reason = if !installed {
            Some("This agent is not installed. Set a skills path to include it.".to_owned())
        } else if selected_scope.is_some() && skills_dir.is_none() {
            Some("No skills directory is configured for this scope.".to_owned())
        } else {
            None
        };
        agents.push(SkillAgentScopeView {
            agent: card.id,
            name: card.name,
            installed,
            disabled_reason,
            skills_dir,
            targets,
            shared_with: Vec::new(),
        });
    }
    let mut shared_dirs = std::collections::HashMap::<String, Vec<usize>>::new();
    for (index, agent) in agents.iter().enumerate() {
        if let Some(path) = agent.skills_dir.as_ref() {
            shared_dirs.entry(path.clone()).or_default().push(index);
        }
    }
    let agent_names = agents
        .iter()
        .map(|agent| agent.name.clone())
        .collect::<Vec<_>>();
    for indexes in shared_dirs.values().filter(|indexes| indexes.len() > 1) {
        for index in indexes {
            if let Some(agent) = agents.get_mut(*index) {
                agent.shared_with = indexes
                    .iter()
                    .filter_map(|other| {
                        (*other != *index)
                            .then(|| agent_names.get(*other).cloned())
                            .flatten()
                    })
                    .collect();
            }
        }
    }
    let mut clashes = Vec::new();
    if let Some(scope) = selected_scope {
        let selected_agents = request
            .agents
            .iter()
            .map(|agent| {
                AgentId::from_str(agent).map_err(|_| {
                    CommandError::invalid_argument("agents", "contains an unknown agent")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for name in &request.names {
            let slug = match slugify(name) {
                Ok(slug) => slug,
                Err(_) => {
                    clashes.push(SkillNameClashView {
                        name: name.clone(),
                        blocking: vec!["Use a valid skill name before continuing.".to_owned()],
                        warnings: Vec::new(),
                    });
                    continue;
                }
            };
            let database = state.database().map_err(CommandError::unavailable)?;
            let report = at_engine::skill_names::check_skill_name_for_agents(
                database.connection(),
                &slug,
                scope,
                &selected_agents,
                &stores,
            )
            .map_err(|_| CommandError::internal("could not check skill name clashes"))?;
            clashes.push(SkillNameClashView {
                name: name.clone(),
                blocking: report.blocking,
                warnings: report.warnings,
            });
        }
    }
    Ok(SkillScopeOptionsView {
        scopes,
        agents,
        clashes,
    })
}

struct CachedSkillAddPlan {
    skill_names: Vec<String>,
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

fn skill_add_results() -> &'static Mutex<HashMap<String, SkillAddApplyResultView>> {
    static RESULTS: LazyLock<Mutex<HashMap<String, SkillAddApplyResultView>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &RESULTS
}

/// Builds and caches a read-only, exact filesystem preview for Add Skill.
#[tauri::command]
pub fn skill_add_preview(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    request: SkillAddPreviewRequest,
) -> Result<SkillAddPreviewView, CommandError> {
    skill_add_preview_impl(&state, &network, request)
}

/// Bridge-facing body of [`skill_add_preview`], callable without a Tauri app.
///
/// # Errors
/// See [`skill_add_preview`].
pub fn skill_add_preview_impl(
    state: &AppState,
    network: &NetworkState,
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
    if matches!(source, SourceKind::New | SourceKind::Marketplace) && wizard.name.trim().is_empty()
    {
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
    let paths = super::paths::resolve_command_paths(state, project_root.as_deref())?;
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("canonical skill store is unavailable"))?;
    let packages = resolve_skill_packages(
        source,
        SkillPackageContext {
            wizard,
            paths: &paths,
            project_root: project_root.as_deref(),
            target_scope: scope,
            target_project_id: request.project_id.as_deref(),
            connection: database.connection(),
            network,
            import_id: request.import_id.as_deref(),
        },
    )?;
    if packages.is_empty() {
        return Err(CommandError::invalid_argument(
            "selections",
            "select at least one valid skill",
        ));
    }
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
    let mut deployment_roots = Vec::with_capacity(wizard.agents.len());
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
        let mode = if settings.link_mode(&agent) == at_model::LinkMode::Copy {
            at_engine::skill_add_plan::SkillDeploymentMode::Copy
        } else {
            at_engine::skill_add_plan::SkillDeploymentMode::Symlink
        };
        deployment_roots.push((agent.to_string(), candidate.path, mode));
    }
    let repository = if scope == Scope::Project {
        project_root.as_deref().and_then(|root| {
            at_git::detect_repository(&at_git::ProcessLocalGitRunner::git(), root).ok()
        })
    } else {
        None
    };
    let mut plans = None;
    let mut items = Vec::with_capacity(packages.len());
    let mut slugs = std::collections::HashSet::with_capacity(packages.len());
    for resolved in packages {
        let slug_text = slugify(&resolved.name).map_err(|_| {
            CommandError::invalid_argument("name", "use letters or digits in the skill name")
        })?;
        let slug = at_store::layout::Slug::parse(&slug_text).map_err(|_| {
            CommandError::invalid_argument("name", "the skill name is not a valid slug")
        })?;
        if !slugs.insert(slug.as_str().to_owned()) {
            return Err(CommandError::invalid_argument(
                "selections",
                "selected skills must have distinct destination names",
            ));
        }
        let canonical_dir = layout.skill_dir(&slug);
        let deployments = deployment_roots
            .iter()
            .map(
                |(agent, allowed_root, mode)| at_engine::skill_add_plan::SkillDeploymentTarget {
                    agent: agent.clone(),
                    target: allowed_root.join(slug.as_str()),
                    allowed_root: allowed_root.clone(),
                    mode: *mode,
                },
            )
            .collect::<Vec<_>>();
        let mut plan = at_engine::skill_add_plan::plan_skill_add(
            at_engine::skill_add_plan::SkillAddPlanRequest {
                layout: &layout,
                slug: &slug,
                package: &resolved.package,
                scope,
                deployments: &deployments,
                repository: repository.as_ref(),
                stage_requested: request.stage_requested,
            },
        )
        .map_err(|error| CommandError::invalid_argument("plan", &error.to_string()))?;
        if resolved.library_mode == Some(at_engine::skill_wizard::LibraryTransferMode::Link) {
            let source_path = resolved.source_path.as_deref().ok_or_else(|| {
                CommandError::internal("the selected library skill has no source path")
            })?;
            at_engine::skill_add_plan::convert_to_library_link(&mut plan, source_path).map_err(
                |error| match error {
                    at_engine::skill_add_plan::SkillAddPlanError::ReadExisting { .. } => {
                        CommandError::unavailable("the linked destination could not be inspected")
                    }
                    other => CommandError::invalid_argument("library_mode", &other.to_string()),
                },
            )?;
        }
        if request.stage_requested && plan.stage.is_none() {
            return Err(CommandError::invalid_argument(
                "stage_requested",
                "Git staging is unavailable for this project",
            ));
        }
        items.push(SkillAddItemView {
            name: resolved.name,
            source_path: resolved
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            target_path: canonical_dir.display().to_string(),
            mode: if resolved.library_mode
                == Some(at_engine::skill_wizard::LibraryTransferMode::Link)
            {
                "link".to_owned()
            } else {
                "copy".to_owned()
            },
        });
        merge_skill_add_plans(&mut plans, plan)?;
    }
    let mut plan = plans.ok_or_else(|| {
        CommandError::invalid_argument("selections", "select at least one valid skill")
    })?;
    plan.change_set.summary = format!("Add {} skills", items.len());
    // A scope with no store yet gets its skeleton inside this same reviewed ChangeSet, so the
    // review shows the store creation and the apply (and undo) covers both in one operation.
    let store_init = at_store::store_init::plan_store_init(&layout, &mut plan.change_set)
        .map_err(|error| super::store_setup::store_init_error(&layout, error))?;
    plan.change_set
        .validate()
        .map_err(|error| CommandError::invalid_argument("plan", &error.to_string()))?;
    let preview_id = uuid::Uuid::now_v7().to_string();
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
    let canonical_dir = if items.len() == 1 {
        items[0].target_path.clone()
    } else {
        layout.root().join("skills").display().to_string()
    };
    let skill_names = items.iter().map(|item| item.name.clone()).collect();
    let cache = CachedSkillAddPlan {
        skill_names,
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
        canonical_dir,
        items,
        files,
        deployments: deployment_views,
        stage_available,
        stage_requested: request.stage_requested,
        requires_overwrite_confirmation,
        store_init: store_init
            .as_ref()
            .map(super::store_setup::StoreSetupView::from_plan),
    })
}

fn take_skill_add_plan(request: SkillAddApplyRequest) -> Result<CachedSkillAddPlan, CommandError> {
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
    Ok(cached)
}

fn apply_cached_skill_add(
    state: &AppState,
    cached: CachedSkillAddPlan,
    progress: Option<&ProgressSender>,
) -> Result<SkillAddApplyResultView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let roots = at_fs::changeset::ApplyRoots::new(cached.apply_roots)
        .map_err(|_| CommandError::unavailable("authorized skill directories are unavailable"))?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &cached.layout_root,
        backup_folder: &cached.backup_folder,
        recorder: &mut recorder,
    };
    if let Some(progress) = progress {
        progress.fraction(
            0.15,
            format!(
                "Applying {} reviewed file operations",
                cached.plan.change_set.ops.len()
            ),
        );
    }
    let outcome = at_fs::changeset::apply_with_roots_and_registry(
        &cached.plan.change_set,
        &mut context,
        &roots,
        &state.recent_writes(),
    )
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
    if let Some(progress) = progress {
        progress.fraction(0.8, "Reviewed files applied");
    }
    let mut staged = false;
    let mut staged_paths = Vec::new();
    let mut stage_error = None;
    if let (Some(stage), Some(repository)) = (&cached.plan.stage, &cached.repository) {
        if let Some(progress) = progress {
            progress.fraction(0.9, "Staging reviewed project paths in Git");
        }
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
        skill_names: cached.skill_names,
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

/// Applies one reviewed Add Skill plan without a progress job.
///
/// The Tauri command wraps the same body in a cancellable job; tests and future automation use
/// this entry point so they exercise exactly the reviewed files and no second write path.
///
/// # Errors
/// See [`skill_add_apply_start`]; a stale preview, an unconfirmed overwrite or a refused write
/// is returned as a typed command error.
pub fn skill_add_apply_inner(
    state: &AppState,
    request: SkillAddApplyRequest,
) -> Result<SkillAddApplyResultView, CommandError> {
    let cached = take_skill_add_plan(request)?;
    apply_cached_skill_add(state, cached, None)
}

/// Starts applying the exact reviewed Add Skill plan as a progress-reporting job.
#[tauri::command]
pub fn skill_add_apply_start(
    app: AppHandle,
    state: State<'_, AppState>,
    request: SkillAddApplyRequest,
) -> Result<SkillAddApplyJobStartView, CommandError> {
    let cached = take_skill_add_plan(request)?;
    let result_id = uuid::Uuid::now_v7().to_string();
    let worker_result_id = result_id.clone();
    let job = JobSpec::new("Create skill", move |progress| {
        let worker_app = app.clone();
        async move {
            progress.message("Starting the reviewed skill apply");
            let worker_progress = progress.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                let state = worker_app.state::<AppState>();
                apply_cached_skill_add(&state, cached, Some(&worker_progress))
            })
            .await
            .map_err(|_| JobError::Failed {
                message: "The skill apply worker stopped unexpectedly.".to_owned(),
            })?
            .map_err(|error| JobError::Failed {
                message: error.message,
            })?;
            skill_add_results()
                .lock()
                .map_err(|_| JobError::Failed {
                    message: "The skill result cache is unavailable.".to_owned(),
                })?
                .insert(worker_result_id, result);
            progress.fraction(1.0, "Skill creation complete");
            Ok(())
        }
    });
    let handle = state
        .bridge()
        .spawn_job(job)
        .map_err(|_| CommandError::unavailable("the background job service is unavailable"))?;
    Ok(SkillAddApplyJobStartView {
        job_id: handle.id().clone(),
        result_id,
    })
}

/// Consumes the successful result for a completed Add Skill apply job.
#[tauri::command]
pub fn skill_add_apply_result(result_id: String) -> Result<SkillAddApplyResultView, CommandError> {
    skill_add_results()
        .lock()
        .map_err(|_| CommandError::internal("skill result cache is unavailable"))?
        .remove(&result_id)
        .ok_or_else(|| {
            CommandError::new(
                super::commands::CommandErrorCode::NotFound,
                "the completed skill result is no longer available",
            )
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
    import_id: Option<&'a str>,
}

struct ResolvedSkillPackage {
    name: String,
    package: SkillPackage,
    source_path: Option<PathBuf>,
    library_mode: Option<at_engine::skill_wizard::LibraryTransferMode>,
}

fn resolve_skill_packages(
    source: &SourceKind,
    context: SkillPackageContext<'_>,
) -> Result<Vec<ResolvedSkillPackage>, CommandError> {
    match source {
        SourceKind::New => {
            let template = context
                .wizard
                .details
                .template
                .as_deref()
                .unwrap_or("blank");
            let template = SkillTemplate::ALL
                .into_iter()
                .find(|candidate| candidate.as_str() == template)
                .ok_or_else(|| {
                    CommandError::invalid_argument("template", "choose a supported template")
                })?;
            let rendered = render_preview(&SkillDraft::new(
                context.wizard.name.trim(),
                context.wizard.details.description.trim(),
                template,
            ));
            let mut package = SkillPackage::new();
            package
                .insert("SKILL.md", rendered.markdown.into_bytes(), 0o644)
                .map_err(|_| {
                    CommandError::invalid_argument("template", "could not build the skill document")
                })?;
            Ok(vec![ResolvedSkillPackage {
                name: context.wizard.name.trim().to_owned(),
                package,
                source_path: None,
                library_mode: None,
            }])
        }
        SourceKind::Library => resolve_library_packages(context),
        SourceKind::Git | SourceKind::Folder | SourceKind::Zip => {
            resolve_import_session_packages(source, context)
        }
        SourceKind::Marketplace => resolve_marketplace_package(context),
        _ => Err(CommandError::invalid_argument(
            "source",
            "choose a supported skill source",
        )),
    }
}

fn resolve_library_packages(
    context: SkillPackageContext<'_>,
) -> Result<Vec<ResolvedSkillPackage>, CommandError> {
    let SkillPackageContext {
        wizard,
        paths,
        project_root,
        target_scope,
        target_project_id,
        connection,
        ..
    } = context;
    if wizard.details.library_selections.is_empty() {
        return Err(CommandError::invalid_argument(
            "library_selections",
            "select at least one valid library skill",
        ));
    }
    let catalog = at_engine::skill_import::enumerate_registered_library(
        connection,
        paths.global_store(),
        paths.user_store(),
        target_scope,
        target_project_id,
        project_root,
    )
    .map_err(|error| {
        tracing::warn!(error = %error, "skill library selections could not be resolved");
        CommandError::unavailable(format!(
            "The local skill library could not be read: {error}. Check that the scope stores and project folders are readable, then retry."
        ))
    })?;
    let candidates = catalog.skills;
    let mode = wizard.details.library_mode;
    let mut selections =
        std::collections::HashSet::with_capacity(wizard.details.library_selections.len());
    let mut packages = Vec::with_capacity(wizard.details.library_selections.len());
    for id in &wizard.details.library_selections {
        if !selections.insert(id) {
            return Err(CommandError::invalid_argument(
                "library_selections",
                "a library skill may only be selected once",
            ));
        }
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.path.to_string_lossy() == id.as_str())
            .filter(|candidate| candidate.error.is_none())
            .ok_or_else(|| {
                CommandError::invalid_argument(
                    "library_selections",
                    "a selected library skill is no longer available",
                )
            })?;
        let name = candidate.name.as_deref().ok_or_else(|| {
            CommandError::invalid_argument(
                "library_selections",
                "a selected library skill is invalid",
            )
        })?;
        let source_root = candidate
            .source_root
            .canonicalize()
            .map_err(|_| CommandError::unavailable("the library source is no longer readable"))?;
        let source_path = candidate.path.canonicalize().map_err(|_| {
            CommandError::unavailable("the library skill package is no longer readable")
        })?;
        if !source_path.starts_with(&source_root) {
            return Err(CommandError::invalid_argument(
                "library_selections",
                "a library skill resolves outside its registered source",
            ));
        }
        let package = SkillPackage::read(&source_path)
            .map(|loaded| loaded.package)
            .map_err(|_| {
                CommandError::unavailable("the library skill package changed or is unreadable")
            })?;
        packages.push(ResolvedSkillPackage {
            name: name.to_owned(),
            package,
            source_path: Some(source_path),
            library_mode: Some(mode),
        });
    }
    Ok(packages)
}

fn resolve_import_session_packages(
    source: &SourceKind,
    context: SkillPackageContext<'_>,
) -> Result<Vec<ResolvedSkillPackage>, CommandError> {
    let wizard = context.wizard;
    let selections = match source {
        SourceKind::Git => &wizard.details.fetched_repo_selections,
        SourceKind::Folder => &wizard.details.folder_selections,
        SourceKind::Zip => &wizard.details.zip_selections,
        _ => {
            return Err(CommandError::invalid_argument(
                "source",
                "choose a Git, folder or ZIP import",
            ));
        }
    };
    if selections.is_empty() {
        return Err(CommandError::invalid_argument(
            "selections",
            "select at least one valid imported skill",
        ));
    }
    let import_id = context.import_id.ok_or_else(|| {
        CommandError::invalid_argument("import_id", "fetch or scan the source before reviewing")
    })?;
    validate_import_id(import_id)?;
    let sessions = skill_import_sessions()
        .lock()
        .map_err(|_| CommandError::internal("skill import sessions are unavailable"))?;
    let session = sessions.get(import_id).ok_or_else(|| {
        CommandError::invalid_argument(
            "import_id",
            "the import session expired; fetch or scan the source again",
        )
    })?;
    if !session.source.matches_source(source) {
        return Err(CommandError::invalid_argument(
            "import_id",
            "the current import session belongs to another source",
        ));
    }
    let root = session.source.candidate_root();
    let canonical_root = root
        .canonicalize()
        .map_err(|_| CommandError::unavailable("the imported source is no longer available"))?;
    let mut unique_selections = std::collections::HashSet::with_capacity(selections.len());
    let mut packages = Vec::with_capacity(selections.len());
    for selection in selections {
        if !unique_selections.insert(selection) {
            return Err(CommandError::invalid_argument(
                "selections",
                "an imported skill may only be selected once",
            ));
        }
        let candidate = session
            .source_candidates
            .iter()
            .find(|candidate| candidate.subpath.to_string_lossy().replace('\\', "/") == *selection)
            .filter(|candidate| candidate.is_valid())
            .ok_or_else(|| {
                CommandError::invalid_argument(
                    "selections",
                    "a selected imported skill is no longer valid",
                )
            })?;
        let candidate_dir = root.join(&candidate.subpath);
        let metadata = std::fs::symlink_metadata(&candidate_dir).map_err(|_| {
            CommandError::unavailable("the selected imported skill is no longer readable")
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CommandError::invalid_argument(
                "selections",
                "the selected imported skill is no longer a regular folder",
            ));
        }
        let source_path = candidate_dir.canonicalize().map_err(|_| {
            CommandError::unavailable("the selected imported skill is no longer readable")
        })?;
        if !source_path.starts_with(&canonical_root) {
            return Err(CommandError::invalid_argument(
                "selections",
                "the selected imported skill resolves outside its source",
            ));
        }
        let name = candidate.name.as_deref().ok_or_else(|| {
            CommandError::invalid_argument("selections", "a selected imported skill is invalid")
        })?;
        let package = SkillPackage::read(&source_path)
            .map(|loaded| loaded.package)
            .map_err(|_| {
                CommandError::unavailable("the selected imported skill package is unreadable")
            })?;
        packages.push(ResolvedSkillPackage {
            name: name.to_owned(),
            package,
            source_path: Some(source_path),
            library_mode: None,
        });
    }
    Ok(packages)
}

fn resolve_marketplace_package(
    context: SkillPackageContext<'_>,
) -> Result<Vec<ResolvedSkillPackage>, CommandError> {
    let wizard = context.wizard;
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
        &context.network.policy(),
    )
    .map_err(|_| CommandError::unavailable("Network policy refused this marketplace install."))?;
    context
        .network
        .authorize(NetworkAction::GitClone, "github.com", NetworkTrigger::User)
        .map_err(|_| {
            CommandError::unavailable("Git clone access is disabled by network policy.")
        })?;
    let policy = context.network.policy();
    let mut log = NetworkEventLog::default();
    let imported = at_engine::skill_import::import_git(
        url,
        None,
        context.paths.cache_dir(),
        &policy,
        &mut log,
    )
    .map_err(|_| CommandError::unavailable("Marketplace skill source could not be fetched."))?;
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
    let package = SkillPackage::read(&imported.repository_root().join(&candidate.subpath))
        .map(|loaded| loaded.package)
        .map_err(|_| CommandError::unavailable("the marketplace skill package is unreadable"))?;
    Ok(vec![ResolvedSkillPackage {
        name: wizard.name.trim().to_owned(),
        package,
        source_path: None,
        library_mode: None,
    }])
}

fn merge_skill_add_plans(
    current: &mut Option<at_engine::skill_add_plan::SkillAddFilePlan>,
    mut next: at_engine::skill_add_plan::SkillAddFilePlan,
) -> Result<(), CommandError> {
    let Some(current) = current else {
        *current = Some(next);
        return Ok(());
    };
    if current.scope != next.scope
        || current.stage_available != next.stage_available
        || current.stage.is_some() != next.stage.is_some()
    {
        return Err(CommandError::internal(
            "selected skill plans cannot be combined safely",
        ));
    }
    current.files.append(&mut next.files);
    current.deployments.append(&mut next.deployments);
    current.overwrites.append(&mut next.overwrites);
    current.change_set.ops.append(&mut next.change_set.ops);
    if let (Some(current_stage), Some(next_stage)) = (&mut current.stage, next.stage) {
        current_stage.paths.extend(next_stage.paths);
        current_stage.paths.sort();
        current_stage.paths.dedup();
    }
    Ok(())
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

#[cfg(test)]
mod import_git_host_tests {
    use super::import_git_host;

    #[test]
    fn embedded_credentials_and_empty_hosts_are_refused() {
        for url in [
            "https://user:secret@example.invalid/repo.git",
            "https://token@example.invalid/repo.git",
            "https:///repo.git",
            "ssh://example.invalid/repo.git",
        ] {
            assert!(import_git_host(url).is_err(), "{url} must be refused");
        }
        assert_eq!(
            import_git_host("https://example.invalid/repo.git").unwrap_or_default(),
            "example.invalid"
        );
    }
}
