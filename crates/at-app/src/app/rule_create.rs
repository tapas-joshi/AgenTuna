//! New rule file dialog backend (TA-057).
//!
//! The dialog needs three typed commands: scope availability options, a read-only
//! target path preview resolved from the adapter path table, and the create action
//! over [`at_engine::rule_editor::new_rule`]. Preview never writes; create applies the
//! reviewed create-only ChangeSet through at-fs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::AppState;
use super::commands::CommandError;

/// Maps a native rule filename onto its engine file type.
fn rule_file_type(file_type: &str) -> Option<at_engine::rule_editor::RuleFileType> {
    match file_type {
        "CLAUDE.md" => Some(at_engine::rule_editor::RuleFileType::Claude),
        "AGENTS.md" => Some(at_engine::rule_editor::RuleFileType::Agents),
        "CLAUDE.local.md" => Some(at_engine::rule_editor::RuleFileType::ClaudeLocal),
        _ => None,
    }
}

fn rule_file_slug(kind: at_engine::rule_editor::RuleFileType) -> &'static str {
    match kind {
        at_engine::rule_editor::RuleFileType::Claude => "claude",
        at_engine::rule_editor::RuleFileType::Agents => "agents",
        at_engine::rule_editor::RuleFileType::ClaudeLocal => "claude-local",
    }
}
/// Scope availability for the new rule file dialog.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct InstructionCreateOptions {
    /// Whether the Global scope accepts writes right now.
    pub global_writable: bool,
    /// Why Global is read-only, shown as the disabled reason.
    pub global_reason: Option<String>,
    /// Whether the Local scope is available (project inside a Git work tree).
    pub local_available: bool,
    /// Why Local is unavailable, shown as the disabled reason.
    pub local_reason: Option<String>,
    /// Whether a project is active at all.
    pub has_project: bool,
}

/// Read-only preview of the resolved target path for one file type and scope.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct InstructionCreatePreview {
    /// Resolved absolute target path, when the adapter table has one.
    pub path: Option<String>,
    /// Whether a file already exists at the target.
    pub exists: bool,
    /// Why the target cannot be created right now, when it cannot.
    pub reason: Option<String>,
}

/// Request body for creating one native rule file.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct InstructionCreateRequest {
    /// Native rule filename: `CLAUDE.md`, `AGENTS.md` or `CLAUDE.local.md`.
    pub file_type: String,
    /// Target scope: `global`, `user`, `project` or `local`.
    pub scope: String,
}

fn active_project_path(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<Option<PathBuf>, CommandError> {
    let Some(id) = project_id else {
        return Ok(None);
    };
    let database = state.database().map_err(CommandError::unavailable)?;
    Ok(Some(PathBuf::from(
        at_db::projects::find_project_by_id(database.connection(), id)
            .map_err(|_| CommandError::internal("could not query project"))?
            .ok_or_else(|| {
                CommandError::invalid_argument("project_id", "project is not registered")
            })?
            .path,
    )))
}

fn home_dir(state: &AppState) -> Result<PathBuf, CommandError> {
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))
        .map(Path::to_path_buf)
}

fn global_writability(state: &AppState) -> (bool, Option<String>) {
    let Some(paths) = state.paths() else {
        return (
            false,
            Some(String::from("application paths are unavailable")),
        );
    };
    let status = at_fs::privilege::GlobalStoreStatus::probe(paths.global_store());
    (status.is_available(), status.reason().map(str::to_owned))
}

fn local_availability(
    state: &AppState,
    project: Option<&Path>,
) -> Result<(bool, Option<String>), CommandError> {
    let Some(project) = project else {
        return Ok((
            false,
            Some(String::from("select a project to use the Local scope")),
        ));
    };
    let paths = state
        .paths()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let store = at_store::local_store::LocalStorePaths::for_paths(&paths)
        .map_err(|_| CommandError::unavailable("Local store paths are unavailable"))?;
    let scope = at_engine::local_scope::LocalScope::detect(project.to_path_buf(), store);
    Ok(match scope.status().is_available() {
        true => (true, None),
        false => (
            false,
            Some(String::from("Local scope needs a Git work tree")),
        ),
    })
}

/// Reports the scope availability the dialog radio cards render.
#[tauri::command]
pub fn instruction_create_options(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<InstructionCreateOptions, CommandError> {
    instruction_create_options_impl(&state, project_id.as_deref())
}

pub(crate) fn instruction_create_options_impl(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<InstructionCreateOptions, CommandError> {
    let project = active_project_path(state, project_id)?;
    let (global_writable, global_reason) = global_writability(state);
    let (local_available, local_reason) = local_availability(state, project.as_deref())?;
    Ok(InstructionCreateOptions {
        global_writable,
        global_reason,
        local_available,
        local_reason,
        has_project: project.is_some(),
    })
}

/// Resolves the target path for one file type and scope without writing anything.
#[tauri::command]
pub fn instruction_create_preview(
    state: State<'_, AppState>,
    file_type: String,
    scope: String,
    project_id: Option<String>,
) -> Result<InstructionCreatePreview, CommandError> {
    instruction_create_preview_impl(&state, &file_type, &scope, project_id.as_deref())
}

pub(crate) fn instruction_create_preview_impl(
    state: &AppState,
    file_type: &str,
    scope: &str,
    project_id: Option<&str>,
) -> Result<InstructionCreatePreview, CommandError> {
    let kind = rule_file_type(file_type)
        .ok_or_else(|| CommandError::invalid_argument("file_type", "unknown rule file name"))?;
    let scope = scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("scope", "unknown scope"))?;
    let project = active_project_path(state, project_id)?;
    let home = home_dir(state)?;
    match at_engine::rule_editor::new_registered_rule(kind, scope, &home, project.as_deref(), true)
    {
        Ok(intent) => {
            let exists = std::fs::symlink_metadata(&intent.path).is_ok();
            Ok(InstructionCreatePreview {
                path: Some(intent.path.display().to_string()),
                exists,
                reason: exists.then(|| String::from("File already exists")),
            })
        }
        Err(error) => Ok(InstructionCreatePreview {
            path: error_path(&error).map(|path| path.display().to_string()),
            exists: false,
            reason: Some(preview_reason(&error)),
        }),
    }
}

fn error_path(error: &at_engine::rule_editor::RuleEditorError) -> Option<PathBuf> {
    match error {
        at_engine::rule_editor::RuleEditorError::AlreadyExists { path }
        | at_engine::rule_editor::RuleEditorError::ReadOnly { path } => Some(path.clone()),
        _ => None,
    }
}

fn preview_reason(error: &at_engine::rule_editor::RuleEditorError) -> String {
    match error {
        at_engine::rule_editor::RuleEditorError::AlreadyExists { path } => {
            format!("File already exists: {}", path.display())
        }
        at_engine::rule_editor::RuleEditorError::ReadOnly { .. } => {
            String::from("Global is read-only; grant write access to create files here")
        }
        at_engine::rule_editor::RuleEditorError::MissingProject { .. } => {
            String::from("Select a project to create files in this scope")
        }
        at_engine::rule_editor::RuleEditorError::NoPath { file_type, .. } => {
            format!("No {file_type} path is known for this scope")
        }
        other => other.to_string(),
    }
}

/// Creates the reviewed rule file and returns its absolute path.
#[tauri::command]
pub fn instruction_create(
    state: State<'_, AppState>,
    request: InstructionCreateRequest,
    project_id: Option<String>,
) -> Result<String, CommandError> {
    instruction_create_impl(&state, request, project_id.as_deref())
}

pub(crate) fn instruction_create_impl(
    state: &AppState,
    request: InstructionCreateRequest,
    project_id: Option<&str>,
) -> Result<String, CommandError> {
    let kind = rule_file_type(&request.file_type)
        .ok_or_else(|| CommandError::invalid_argument("file_type", "unknown rule file name"))?;
    let scope = request
        .scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("scope", "unknown scope"))?;
    let project = active_project_path(state, project_id)?;
    if matches!(scope, at_model::Scope::Global) {
        let (writable, reason) = global_writability(state);
        if !writable {
            return Err(CommandError::new(
                super::commands::CommandErrorCode::Locked,
                reason.unwrap_or_else(|| String::from("Global is read-only")),
            ));
        }
    }
    if matches!(scope, at_model::Scope::Project | at_model::Scope::Local) && project.is_none() {
        return Err(CommandError::invalid_argument(
            "scope",
            "select a project to create files in this scope",
        ));
    }
    let home = home_dir(state)?;
    let intent =
        at_engine::rule_editor::new_registered_rule(kind, scope, &home, project.as_deref(), true)
            .map_err(|error| {
            CommandError::new(
                super::commands::CommandErrorCode::Conflict,
                preview_reason(&error),
            )
        })?;
    let paths = state
        .paths()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let item_id = at_model::ItemId::new();
    let slug = rule_file_slug(kind);
    database
        .connection()
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path, managed) \
             VALUES (?1, 'rule', ?2, ?3, ?4, ?5, 1)",
            rusqlite::params![
                item_id.to_string(),
                scope.wire_name(),
                slug,
                request.file_type,
                intent.path.to_string_lossy(),
            ],
        )
        .map_err(|error| CommandError::internal(format!("could not record rule item: {error}")))?;
    let target_parent = intent.path.parent().unwrap_or_else(|| Path::new("/"));
    let mut native_root = target_parent.to_path_buf();
    while !native_root.is_dir() {
        if let Some(parent) = native_root.parent() {
            native_root = parent.to_path_buf();
        } else {
            break;
        }
    }
    if !paths.support_dir().exists() {
        let _ = std::fs::create_dir_all(paths.support_dir());
    }
    if !paths.backup_dir().exists() {
        let _ = std::fs::create_dir_all(paths.backup_dir());
    }
    let roots =
        at_fs::changeset::ApplyRoots::new(vec![native_root, paths.support_dir().to_path_buf()])
            .map_err(|_| {
                CommandError::unavailable("authorized instruction paths are unavailable")
            })?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: paths.support_dir(),
        backup_folder: paths.backup_dir(),
        recorder: &mut recorder,
    };
    let (source, _) = at_engine::rule_editor::apply_new_rule(
        &intent,
        &item_id,
        database.connection(),
        &mut context,
        &roots,
        &state.recent_writes(),
    )
    .map_err(|error| {
        CommandError::new(
            super::commands::CommandErrorCode::Conflict,
            preview_reason(&error),
        )
    })?;
    Ok(source.path.display().to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
    use at_model::{Agent, AgentId};
    use at_store::paths::{AppPaths, PathEnvironment};
    use std::sync::Arc;

    struct NullSink;
    impl EventSink for NullSink {
        fn emit(&self, _event: JobEvent) {}
    }

    struct TestHarness {
        _home: at_fs::testkit::TestHome,
        state: AppState,
    }

    impl TestHarness {
        fn new() -> Self {
            let home = at_fs::testkit::TestHome::new().expect("test home");
            let paths =
                AppPaths::resolve(&PathEnvironment::default().with_home(home.home().to_path_buf()))
                    .expect("app paths");
            let db_dir = home.home().join("agentuna");
            std::fs::create_dir_all(&db_dir).expect("db dir");
            let database = at_db::Database::open_file(db_dir.join("index.db")).expect("database");
            let connection = database.connection();
            at_db::agents::upsert_agent(
                connection,
                &Agent {
                    id: AgentId::ClaudeCode,
                    display_name: "Claude Code".to_owned(),
                    detected: true,
                    version: Some("1.0".to_owned()),
                },
            )
            .expect("agent");

            let state = AppState::new(
                Arc::new(JobBridge::new(
                    Arc::new(NullSink),
                    Arc::new(TracingOperationsLog),
                )),
                None,
            )
            .with_database(database)
            .with_paths(paths);

            Self { _home: home, state }
        }
    }

    #[test]
    fn options_reflect_project_absence() {
        let harness = TestHarness::new();
        let options = instruction_create_options_impl(&harness.state, None).expect("options");
        assert!(!options.has_project);
    }

    #[test]
    fn preview_and_create_user_rule_round_trip() {
        let harness = TestHarness::new();
        let preview = instruction_create_preview_impl(&harness.state, "CLAUDE.md", "user", None)
            .expect("preview");

        assert!(!preview.exists);
        let target_path = preview.path.expect("target path");
        assert!(target_path.ends_with(".claude/CLAUDE.md"));

        let created = instruction_create_impl(
            &harness.state,
            InstructionCreateRequest {
                file_type: "CLAUDE.md".to_owned(),
                scope: "user".to_owned(),
            },
            None,
        )
        .expect("create");

        assert_eq!(created, target_path);
        assert!(std::path::Path::new(&created).exists());

        // Second create should conflict because file exists
        let repeat = instruction_create_impl(
            &harness.state,
            InstructionCreateRequest {
                file_type: "CLAUDE.md".to_owned(),
                scope: "user".to_owned(),
            },
            None,
        );
        assert!(repeat.is_err());
    }
}
