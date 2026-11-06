//! TU-027: `agentuna projects` registry commands and `agentuna scan` discovery.
//!
//! Both commands are thin fronts over `at-db`'s project repository and
//! `at-store`'s bounded walk: the CLI parses, resolves the roots, opens the
//! index through the shared lock and renders the TU-033 envelope. Nothing here
//! writes agent folders; `scan` never writes at all — it reports the diff a
//! registry upsert would see.
//!
//! Scan roots are `~/Projects` (the documented default) plus every distinct
//! `source_root` already recorded on registered projects — the folders the
//! user added through the picker or an earlier scan. There is no separate
//! scan-root store yet; when one lands it replaces [`scan_roots`].

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use at_db::projects::{self, ProjectSummary};
use at_db::settings;
use at_db::{Database, ReadOnlyDatabase};
use at_store::AppPaths;
use at_store::project_scan::{
    self, DiscoveredProject, ProjectDiff, ScanConfig, ScanOutcome, ScanWarning, ScanWarningKind,
};
use serde_json::{Value, json};

use crate::cli::ProjectsCommand;
use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::{CommandOutput, Warning};
use crate::runtime::CliAccess;

/// Runs `agentuna projects <list|add|remove|use>`.
///
/// # Errors
/// A validation failure for a bad path or unknown target, `Locked` under
/// `--read-only` or a busy lock, `NotFound` for an unknown project, or a
/// conflict when the index cannot be opened.
pub(crate) fn dispatch_projects(
    globals: &GlobalArgs,
    command: &ProjectsCommand,
) -> Result<CommandOutput, CliError> {
    // `--dry-run` reports intent without writing, so it takes the read lock
    // even for the write subcommands and stays allowed under `--read-only`.
    let writes = !matches!(command, ProjectsCommand::List) && !globals.dry_run;
    let access = CliAccess::for_command(globals, writes)?;
    if writes {
        // `--read-only` (and a read lock) rejects the write before any index
        // is opened for writing.
        access.ensure_writable()?;
    }

    match command {
        ProjectsCommand::List => {
            let index = ReadIndex::open(access.paths())?;
            let projects = index.list_projects()?;
            let active = index.active_project_id()?;
            Ok(list_output(&projects, active.as_deref()))
        }
        ProjectsCommand::Add { path } => add_project(globals, access.paths(), path),
        ProjectsCommand::Remove { target } => {
            remove_project(access.paths(), target, globals.dry_run)
        }
        ProjectsCommand::Use { target } => use_project(access.paths(), target, globals.dry_run),
    }
}

/// Runs `agentuna scan [--depth N]`.
///
/// The scan is read-only: it walks the configured roots, diffs the result
/// against the registered project paths and reports the four sets. Permission
/// problems become warnings; the command exits 0 unless the index itself fails.
///
/// # Errors
/// `Locked` under a busy lock, or a conflict when the index cannot be opened.
pub(crate) fn dispatch_scan(
    globals: &GlobalArgs,
    depth: Option<u32>,
) -> Result<CommandOutput, CliError> {
    let access = CliAccess::for_command(globals, false)?;
    let paths = access.paths();
    let index = ReadIndex::open(paths)?;

    let registered = index.list_projects()?;
    let roots = scan_roots(paths, &registered);
    let mut config = ScanConfig::new(roots.iter().cloned());
    if let Some(depth) = depth {
        config = config.with_max_depth(depth as usize);
    }
    let outcome = project_scan::scan(&config);
    let known: Vec<PathBuf> = registered.iter().map(|p| PathBuf::from(&p.path)).collect();
    let diff = ProjectDiff::between(&outcome.projects, &known);

    Ok(scan_output(&config, &registered, &outcome, &diff))
}

/// The index handle a read command uses: either an open read-only database or
/// the empty registry a missing index implies.
///
/// A missing index means an empty registry, not an error: read commands must
/// not create the database. [`ReadOnlyDatabase`] never migrates or writes, so
/// a read command cannot change the schema either.
enum ReadIndex {
    /// `<support>/agentuna.db` opened with `query_only`.
    Open(ReadOnlyDatabase),
    /// No index file yet; every registry read returns empty.
    Empty,
}

impl ReadIndex {
    /// Opens the index for reads; a missing file yields the empty registry.
    ///
    /// # Errors
    /// `Conflict` when the index exists but cannot be opened.
    fn open(paths: &AppPaths) -> Result<Self, CliError> {
        match std::fs::metadata(paths.db_path()) {
            Ok(_) => ReadOnlyDatabase::open(paths.db_path())
                .map(Self::Open)
                .map_err(|error| {
                    CliError::new(
                        ErrorCode::Conflict,
                        format!("cannot open the index read-only: {error}"),
                        "check the index file permissions, or run `agentuna doctor`",
                    )
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::Empty),
            Err(error) => Err(CliError::new(
                ErrorCode::Conflict,
                format!(
                    "cannot stat the index {}: {error}",
                    paths.db_path().display()
                ),
                "check the support directory permissions, or run `agentuna doctor`",
            )),
        }
    }

    /// Every registered project, or an empty list when no index exists.
    ///
    /// # Errors
    /// `Conflict` when the registry cannot be read.
    fn list_projects(&self) -> Result<Vec<ProjectSummary>, CliError> {
        match self {
            Self::Open(db) => projects::list_projects(db.connection()).map_err(db_error),
            Self::Empty => Ok(Vec::new()),
        }
    }

    /// One project by id, when an index exists.
    ///
    /// # Errors
    /// `Conflict` when the registry cannot be read.
    fn find_by_id(&self, id: &str) -> Result<Option<ProjectSummary>, CliError> {
        match self {
            Self::Open(db) => projects::find_project_by_id(db.connection(), id).map_err(db_error),
            Self::Empty => Ok(None),
        }
    }

    /// One project by path, when an index exists.
    ///
    /// # Errors
    /// `Conflict` when the registry cannot be read.
    fn find_by_path(&self, path: &Path) -> Result<Option<ProjectSummary>, CliError> {
        match self {
            Self::Open(db) => {
                projects::find_project_by_path(db.connection(), path).map_err(db_error)
            }
            Self::Empty => Ok(None),
        }
    }

    /// The `last_project_id` setting, when an index exists.
    ///
    /// # Errors
    /// `Conflict` when the settings table cannot be read.
    fn active_project_id(&self) -> Result<Option<String>, CliError> {
        match self {
            Self::Open(db) => settings::load(db.connection())
                .map(|loaded| loaded.last_project_id)
                .map_err(|error| {
                    CliError::new(
                        ErrorCode::Conflict,
                        format!("cannot read the settings table: {error}"),
                        "run `agentuna doctor` to check the index",
                    )
                }),
            Self::Empty => Ok(None),
        }
    }
}

/// `projects list` output: the golden schema is `data.projects` plus
/// `data.active_project_id`; ordering comes from the repository
/// (`last_used_at` descending, then name).
fn list_output(projects: &[ProjectSummary], active: Option<&str>) -> CommandOutput {
    let rows: Vec<Value> = projects.iter().map(project_json).collect();
    let data = json!({
        "projects": rows,
        "active_project_id": active,
    });

    let mut human = String::new();
    if projects.is_empty() {
        human.push_str("no projects registered\n");
        human.push_str("add one with `agentuna projects add <path>` or scan `~/Projects`\n");
    } else {
        let hidden = projects.iter().filter(|p| p.hidden).count();
        let _ = writeln!(
            human,
            "{} project(s){}:",
            projects.len(),
            if hidden > 0 {
                format!(", {hidden} hidden")
            } else {
                String::new()
            }
        );
        for project in projects {
            let _ = writeln!(
                human,
                "  {}{}  {}  skills={} servers={} rules={}  last_used={}",
                project.name,
                if project.hidden { " (hidden)" } else { "" },
                project.path,
                project.skill_count,
                project.server_count,
                project.rule_count,
                project.last_used_at.as_deref().unwrap_or("never"),
            );
        }
        if let Some(active) = active {
            let _ = writeln!(human, "active project: {active}");
        }
    }
    CommandOutput::new(data, human)
}

/// `projects add <path>`: registers a folder, or selects the existing row when
/// the canonical path (including through a symlink) is already registered.
fn add_project(
    globals: &GlobalArgs,
    paths: &AppPaths,
    path: &Path,
) -> Result<CommandOutput, CliError> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::new(
                ErrorCode::NotFound,
                format!("{} does not exist", path.display()),
                "check the path and try again",
            ));
        }
        Err(error) => {
            return Err(CliError::new(
                ErrorCode::Conflict,
                format!("cannot stat {}: {error}", path.display()),
                "check the path permissions and try again",
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(CliError::new(
                ErrorCode::Validation,
                format!("{} is not a directory", path.display()),
                "pass the path of an existing project folder",
            ));
        }
        Ok(_) => {}
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        CliError::new(
            ErrorCode::NotFound,
            format!("cannot resolve {}: {error}", path.display()),
            "check the path and try again",
        )
    })?;

    if globals.dry_run {
        // Report the intent without opening the index for writes: a missing
        // index means the folder is not registered yet.
        let index = ReadIndex::open(paths)?;
        let registered = index.find_by_path(&canonical)?;
        let data = json!({
            "action": "add",
            "path": canonical.display().to_string(),
            "already_registered": registered.is_some(),
            "project": registered.as_ref().map(project_json),
            "applied": false,
        });
        let mut human = format!("would register {}\n", canonical.display());
        if registered.is_some() {
            human.push_str("the path is already registered; nothing would change\n");
        }
        return Ok(CommandOutput::new(data, human));
    }

    let db = open_write_index(paths)?;
    let added = projects::add_project(db.connection(), &canonical, None, None).map_err(db_error)?;
    let summary = projects::find_project_by_id(db.connection(), &added.id)
        .map_err(db_error)?
        .ok_or_else(|| {
            CliError::internal(format!(
                "project {} vanished right after registration",
                added.id
            ))
        })?;

    let mut warnings = Vec::new();
    if !added.inserted {
        warnings.push(Warning::new(
            "already_registered",
            format!(
                "{} is already registered as {}",
                canonical.display(),
                summary.id
            ),
        ));
    }
    let data = json!({
        "project": project_json(&summary),
        "inserted": added.inserted,
    });
    let human = if added.inserted {
        format!("registered {} as {}\n", summary.path, summary.id)
    } else {
        format!("{} is already registered as {}\n", summary.path, summary.id)
    };
    Ok(CommandOutput::new(data, human).with_warnings(warnings))
}

/// `projects remove <path|id>`: hides the project; deployments, operations and
/// backup rows are untouched.
fn remove_project(
    paths: &AppPaths,
    target: &str,
    dry_run: bool,
) -> Result<CommandOutput, CliError> {
    let summary = resolve_project(paths, target)?;
    if dry_run {
        let data = json!({
            "action": "remove",
            "project": project_json(&summary),
            "would_hide": true,
            "applied": false,
        });
        return Ok(CommandOutput::new(
            data,
            format!("would hide {} ({})\n", summary.name, summary.path),
        ));
    }
    let db = open_write_index(paths)?;
    projects::hide_project(db.connection(), &summary.id).map_err(db_error)?;
    // The same connection sees its own write, so the refreshed row already
    // carries `hidden = true`.
    let hidden = projects::find_project_by_id(db.connection(), &summary.id)
        .map_err(db_error)?
        .unwrap_or(summary);
    let data = json!({
        "project": project_json(&hidden),
        "hidden": true,
    });
    Ok(CommandOutput::new(
        data,
        format!(
            "hid {} ({}); deployments and history are kept\n",
            hidden.name, hidden.path
        ),
    ))
}

/// `projects use <path|id>`: makes the project active, updating
/// `last_used_at` and the `last_project_id` setting in one transaction.
fn use_project(paths: &AppPaths, target: &str, dry_run: bool) -> Result<CommandOutput, CliError> {
    let summary = resolve_project(paths, target)?;
    if dry_run {
        let data = json!({
            "action": "use",
            "project": project_json(&summary),
            "would_activate": true,
            "applied": false,
        });
        return Ok(CommandOutput::new(
            data,
            format!("would activate {} ({})\n", summary.name, summary.path),
        ));
    }
    let db = open_write_index(paths)?;
    let changed = projects::touch_project(db.connection(), &summary.id).map_err(db_error)?;
    let refreshed = projects::find_project_by_id(db.connection(), &summary.id)
        .map_err(db_error)?
        .unwrap_or(summary);
    let active = settings::load(db.connection())
        .map(|loaded| loaded.last_project_id)
        .map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("cannot read the settings table: {error}"),
                "run `agentuna doctor` to check the index",
            )
        })?;
    let data = json!({
        "project": project_json(&refreshed),
        "active_project_id": active,
        "changed": changed,
    });
    let human = if changed {
        format!("active project: {} ({})\n", refreshed.name, refreshed.path)
    } else {
        format!(
            "{} is already the active project ({})\n",
            refreshed.name, refreshed.path
        )
    };
    Ok(CommandOutput::new(data, human))
}

/// Finds a project by id first, then by path; `NotFound` when neither matches.
fn resolve_project(paths: &AppPaths, target: &str) -> Result<ProjectSummary, CliError> {
    let index = ReadIndex::open(paths)?;
    if let Some(found) = index.find_by_id(target)? {
        return Ok(found);
    }
    if let Some(found) = index.find_by_path(Path::new(target))? {
        return Ok(found);
    }
    Err(CliError::new(
        ErrorCode::NotFound,
        format!("no registered project matches `{target}`"),
        "list projects with `agentuna projects list`, or register the folder with `agentuna projects add <path>`",
    ))
}

/// Opens the index for a command that writes, creating it when needed.
fn open_write_index(paths: &AppPaths) -> Result<Database, CliError> {
    Database::open_file(paths.db_path()).map_err(|error| match error {
        at_db::DbError::PathUnresolved { .. } => CliError::new(
            ErrorCode::Validation,
            error.to_string(),
            "pass an existing directory to `projects add`",
        ),
        other => CliError::new(
            ErrorCode::Conflict,
            format!("cannot open the index for writing: {other}"),
            "check the support directory permissions, or run `agentuna doctor`",
        ),
    })
}

/// Maps a repository failure to the CLI error table.
fn db_error(error: at_db::DbError) -> CliError {
    match error {
        at_db::DbError::PathUnresolved { path, .. } => CliError::new(
            ErrorCode::NotFound,
            format!("{} does not exist or cannot be resolved", path.display()),
            "check the path and try again",
        ),
        other => CliError::new(
            ErrorCode::Conflict,
            format!("the project registry could not be read: {other}"),
            "run `agentuna doctor` to check the index",
        ),
    }
}

/// The roots `scan` walks: `~/Projects` plus every distinct `source_root`
/// recorded on registered projects (the user-added folders).
fn scan_roots(paths: &AppPaths, registered: &[ProjectSummary]) -> Vec<PathBuf> {
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
    if let Some(home) = paths.home_dir() {
        roots.insert(home.join(project_scan::DEFAULT_ROOT_DIR));
    }
    for project in registered {
        if let Some(root) = &project.source_root
            && !root.is_empty()
        {
            roots.insert(PathBuf::from(root));
        }
    }
    roots.into_iter().collect()
}

/// One registered project as the JSON contract spells it.
fn project_json(project: &ProjectSummary) -> Value {
    json!({
        "id": project.id,
        "name": project.name,
        "path": project.path,
        "source_root": project.source_root,
        "last_used_at": project.last_used_at,
        "hidden": project.hidden,
        "skill_count": project.skill_count,
        "server_count": project.server_count,
        "rule_count": project.rule_count,
    })
}

/// One discovered project as the JSON contract spells it.
fn discovered_json(project: &DiscoveredProject) -> Value {
    json!({
        "path": project.path.display().to_string(),
        "name": project.name,
        "source_root": project.source_root.display().to_string(),
        "depth": project.depth,
        "markers": project.markers.iter().map(|m| m.entry_name()).collect::<Vec<_>>(),
    })
}

/// A scan warning as it appears inside `data.warnings`.
fn warning_json(warning: &ScanWarning) -> Value {
    json!({
        "path": warning.path.display().to_string(),
        "kind": warning_kind_str(warning.kind),
        "message": warning.message,
    })
}

/// The stable wire name of a [`ScanWarningKind`].
fn warning_kind_str(kind: ScanWarningKind) -> &'static str {
    match kind {
        ScanWarningKind::MissingRoot => "missing_root",
        ScanWarningKind::NotADirectory => "not_a_directory",
        ScanWarningKind::PermissionDenied => "permission_denied",
        ScanWarningKind::Unreadable => "unreadable",
        _ => "other",
    }
}

/// `scan` output: the four sets plus per-root warnings and walk statistics.
fn scan_output(
    config: &ScanConfig,
    registered: &[ProjectSummary],
    outcome: &ScanOutcome,
    diff: &ProjectDiff,
) -> CommandOutput {
    let discovered: Vec<Value> = outcome.projects.iter().map(discovered_json).collect();
    let new: Vec<Value> = diff.new.iter().map(discovered_json).collect();
    let unchanged: Vec<Value> = diff.unchanged.iter().map(discovered_json).collect();
    let missing: Vec<Value> = diff
        .missing
        .iter()
        .map(|path| json!(path.display().to_string()))
        .collect();
    let warnings: Vec<Value> = outcome.warnings.iter().map(warning_json).collect();

    let data = json!({
        "roots": config.roots().iter().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        "depth": config.max_depth(),
        "registered_count": registered.len(),
        "discovered": discovered,
        "new": new,
        "missing": missing,
        "unchanged": unchanged,
        "warnings": warnings,
        "stats": {
            "visited": outcome.visited,
            "skipped_directories": outcome.skipped_directories,
            "skipped_symlinks": outcome.skipped_symlinks,
            "depth_limited": outcome.depth_limited,
            "cancelled": outcome.cancelled,
        },
    });

    let envelope_warnings: Vec<Warning> = outcome
        .warnings
        .iter()
        .map(|warning| {
            Warning::new(
                format!("scan_{}", warning_kind_str(warning.kind)),
                format!("{}: {}", warning.path.display(), warning.message),
            )
        })
        .collect();

    CommandOutput::new(data, render_scan_human(config, outcome, diff))
        .with_warnings(envelope_warnings)
}

/// The human rendering of a scan report.
fn render_scan_human(config: &ScanConfig, outcome: &ScanOutcome, diff: &ProjectDiff) -> String {
    let mut text = format!(
        "scan: {} root(s), depth {} — {} discovered, {} new, {} missing, {} unchanged\n",
        config.roots().len(),
        config.max_depth(),
        outcome.projects.len(),
        diff.new.len(),
        diff.missing.len(),
        diff.unchanged.len(),
    );
    if !outcome.warnings.is_empty() {
        text.push_str("warnings:\n");
        for warning in &outcome.warnings {
            let _ = writeln!(
                text,
                "  {}: {}",
                warning_kind_str(warning.kind),
                warning.path.display()
            );
        }
    }
    if !diff.new.is_empty() {
        text.push_str("new:\n");
        for project in &diff.new {
            let _ = writeln!(text, "  {} ({})", project.path.display(), project.name);
        }
    }
    if !diff.missing.is_empty() {
        text.push_str("missing:\n");
        for path in &diff.missing {
            let _ = writeln!(text, "  {}", path.display());
        }
    }
    text
}
