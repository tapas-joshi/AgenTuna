//! `agentuna sync` and `agentuna remote` (TC-031).
//!
//! `sync` reads the TA-005 label the deployment index already stores and, for `push`/`pull`,
//! plans the copy with [`at_engine::project_sync`] — the same planner the desktop app uses —
//! previews it, and applies exactly the reviewed ChangeSet. `remote` reads and writes the
//! database-backed remote-backup state through [`at_store::remote_backup`] with the git and
//! credential runners from `at-git` and `at-secrets`; it never depends on the Tauri shell.
//!
//! Read-only commands (`sync status`, `remote status` without `--refresh`) never contact the
//! network, never write a store or index file and never create a missing index: they open the
//! index read-only and resolve every other value from the store. Every destructive command
//! requires `--yes`; `--dry-run` reports the plan and touches nothing.
//!
//! Credentials come from the OS credential store. `AGENTUNA_SECRET_STORE=memory` selects an
//! empty in-memory store instead, so tests and diagnostics never open the real Keychain; it can
//! only produce a missing-credential result, never expose or persist a value.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use at_db::Database;
use at_engine::project_sync::{
    SyncAction, SyncBaselineTarget, SyncDirection, SyncPlan, SyncPlanError, SyncRecord,
    SyncTreeRequest, action_for, apply_sync_plan_and_update_baseline, package_files,
    package_hashes, plan_sync_tree, sync_record_from_rows,
};
use at_git::local::{LocalGitRunner, ProcessLocalGitRunner};
use at_git::remote::{GitAuth, ProcessRemoteGitRunner, RemoteContext, RemoteGitRunner, RemoteHead};
use at_model::network::{DenialReason, NetworkEventLog, NetworkPolicy};
use at_model::redact::Redactor;
use at_model::{ItemId, ItemKind, Scope, SyncLabel};
use at_secrets::{KeychainStore, MemoryStore, SecretStore};
use at_store::layout::{Slug, StoreError, StoreLayout};
use at_store::manifest::StoreManifest;
use at_store::remote_backup::{BackupError, BackupOutcome, BackupRequest};
use at_store::skill_package::{LoadedSkillPackage, SkillPackage, SkillPackageError};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;
use crate::registry::CommandGroup;
use crate::runtime::CliAccess;

/// Retention applied to the per-operation backups a sync apply writes.
///
/// The contract fixes the undo window at 30 days; the recorder clamps lower values up.
const BACKUP_RETENTION_DAYS: u32 = 30;

/// Environment variable that selects the credential backend for the remote commands.
///
/// Only `memory` is honoured: an empty in-memory store means every remote command reports a
/// missing credential, so the switch cannot bypass authentication or leak a value. It exists so
/// tests and diagnostics never touch the developer's Keychain.
const SECRET_STORE_ENV: &str = "AGENTUNA_SECRET_STORE";

/// `agentuna sync`: project/library synchronization status, push and pull.
pub struct SyncGroup;

impl CommandGroup for SyncGroup {
    fn name(&self) -> &'static str {
        "sync"
    }

    fn attach(&self, root: Command) -> Command {
        let item = |command: Command| {
            command.arg(
                Arg::new("item")
                    .required(true)
                    .value_name("ITEM")
                    .help("Item id (`itm_…`), slug or display name"),
            )
        };
        let sync = Command::new("sync")
            .about("Compare and copy one item between the Project store and the User library")
            .subcommand_required(true)
            .subcommand(
                Command::new("status")
                    .about("Show the stored sync label and evidence hashes for project items")
                    .arg(
                        Arg::new("item")
                            .long("item")
                            .value_name("ITEM")
                            .help("Limit the report to one item id, slug or display name"),
                    ),
            )
            .subcommand(item(Command::new("push").about(
                "Requires --yes: copy the Project copy into the User library",
            )))
            .subcommand(item(Command::new("pull").about(
                "Requires --yes: copy the User library item into the Project store",
            )));
        root.subcommand(sync)
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let (command, args) = matches.subcommand().ok_or_else(|| usage("sync"))?;
        match command {
            "status" => status(globals, args),
            "push" => transfer(globals, args, SyncDirection::Push),
            "pull" => transfer(globals, args, SyncDirection::Pull),
            _ => Err(usage("sync")),
        }
    }
}

/// `agentuna remote`: remote-backup status plus the backup and restore actions.
pub struct RemoteGroup;

impl CommandGroup for RemoteGroup {
    fn name(&self) -> &'static str {
        "remote"
    }

    fn attach(&self, root: Command) -> Command {
        let remote =
            Command::new("remote")
                .about("Inspect and update the remote backup of the User store")
                .subcommand_required(true)
                .subcommand(
                    Command::new("status")
                        .about("Show the stored remote-backup state without contacting the network")
                        .arg(
                            Arg::new("refresh")
                                .long("refresh")
                                .action(ArgAction::SetTrue)
                                .help("Contact the configured remote and report its HEAD commit"),
                        ),
                )
                .subcommand(Command::new("backup").about(
                    "Requires --yes: push the User store snapshot to the configured repository",
                ))
                .subcommand(
                    Command::new("restore")
                        .about("Requires --yes: restore one commit into an empty User store")
                        .arg(Arg::new("commit").required(true).value_name("COMMIT").help(
                            "Full 40- or 64-character commit id from `remote status --refresh`",
                        )),
                );
        root.subcommand(remote)
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let (command, args) = matches.subcommand().ok_or_else(|| usage("remote"))?;
        match command {
            "status" => remote_status(globals, args.get_flag("refresh")),
            "backup" => remote_backup(globals),
            "restore" => remote_restore(globals, args),
            _ => Err(usage("remote")),
        }
    }
}

fn usage(group: &str) -> CliError {
    CliError::new(
        ErrorCode::Usage,
        format!("missing or unsupported `{group}` command"),
        format!("run `agentuna {group} --help`"),
    )
}

fn required<'a>(args: &'a ArgMatches, name: &str) -> Result<&'a str, CliError> {
    args.get_one::<String>(name)
        .map(String::as_str)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Usage,
                format!("missing required argument `{name}`"),
                "run the command with --help to see its arguments",
            )
        })
}

fn db_error(message: String) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("the index could not be read or written: {message}"),
        "check the support directory and retry, or run `agentuna doctor`",
    )
}

fn store_error(error: StoreError) -> CliError {
    CliError::new(
        ErrorCode::Validation,
        format!("the store for this scope is unavailable: {error}"),
        "pass --project with an existing directory, or run `agentuna doctor`",
    )
}

fn direction_name(direction: SyncDirection) -> &'static str {
    if direction == SyncDirection::Push {
        "push"
    } else {
        "pull"
    }
}

/// Opens the writable index, creating it when it is missing.
fn open_database(paths: &at_store::paths::AppPaths) -> Result<Database, CliError> {
    Database::open_file(paths.db_path()).map_err(|error| db_error(error.to_string()))
}

/// A read-only view of the selected project and its indexed items.
///
/// Holds the CLI lock for the whole command and keeps the index read-only, so neither planning
/// nor reporting can migrate or checkpoint the database.
struct ProjectIndex {
    _access: CliAccess,
    index: at_db::ReadOnlyDatabase,
    paths: at_store::paths::AppPaths,
    project_id: String,
    project_path: PathBuf,
}

impl ProjectIndex {
    fn open(globals: &GlobalArgs, writes: bool) -> Result<Self, CliError> {
        let access = CliAccess::for_command(globals, writes)?;
        if writes {
            access.ensure_writable()?;
        }
        let paths = access.paths().clone();
        let project_store = paths.project_store().ok_or_else(|| {
            CliError::new(
                ErrorCode::Validation,
                "no project is selected",
                "pass --project <DIR> for the project whose library you want to sync",
            )
        })?;
        let project_path = project_store
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| CliError::internal("the project store has no parent directory"))?;
        if !paths.db_path().exists() {
            return Err(CliError::new(
                ErrorCode::NotFound,
                "the AgenTuna index does not exist yet",
                "register the project with `agentuna projects add <DIR>` first",
            ));
        }
        let index = crate::runtime::open_index_read_only(&paths)?;
        let project = at_db::projects::find_project_by_path(index.connection(), &project_path)
            .map_err(|error| db_error(error.to_string()))?
            .ok_or_else(|| {
                CliError::new(
                    ErrorCode::NotFound,
                    format!("project {} is not registered", project_path.display()),
                    "register it with `agentuna projects add <DIR>`",
                )
            })?;
        Ok(Self {
            _access: access,
            index,
            paths,
            project_id: project.id,
            project_path,
        })
    }

    fn paths(&self) -> &at_store::paths::AppPaths {
        &self.paths
    }

    fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Canonical skill rows indexed for any scope.
    fn items(&self) -> Result<Vec<at_db::shared::ItemRow>, CliError> {
        let skills = at_db::skills_index::list_skills(
            self.index.connection(),
            &at_db::skills_index::SkillFilter::default(),
        )
        .map_err(|error| db_error(error.to_string()))?;
        let ids = skills
            .iter()
            .filter_map(|row| row.item_id)
            .collect::<Vec<_>>();
        at_db::shared::items_by_ids(self.index.connection(), &ids)
            .map_err(|error| db_error(error.to_string()))
    }

    /// TA-005's stored classification for one item, without recomputing it.
    fn record(&self, item_id: ItemId) -> Result<Option<SyncRecord>, CliError> {
        let rows = at_db::deployments::list_deployments_for_item(self.index.connection(), &item_id)
            .map_err(|error| db_error(error.to_string()))?;
        Ok(sync_record_from_rows(&rows, &self.project_id))
    }

    /// The item named by id, slug or display name, plus its stored sync classification.
    ///
    /// A Project-scope row owns the deployment row the baseline is updated on, so it is tried
    /// first; a library-only item still resolves through its own row.
    fn item_and_record(
        &self,
        text: &str,
    ) -> Result<(at_db::shared::ItemRow, SyncRecord), CliError> {
        let wanted_id = ItemId::from_str(text).ok();
        let wanted = text.to_ascii_lowercase();
        let mut matches = self
            .items()?
            .into_iter()
            .filter(|item| {
                item.kind == ItemKind::Skill
                    && (Some(item.id) == wanted_id
                        || item.slug.to_ascii_lowercase() == wanted
                        || item.name.to_ascii_lowercase() == wanted)
                    && (item.scope == Scope::User
                        || (item.scope == Scope::Project
                            && item.project_id.as_deref() == Some(self.project_id.as_str())))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|item| item.scope != Scope::Project);
        if matches.is_empty() {
            return Err(CliError::new(
                ErrorCode::NotFound,
                format!("no item `{text}` belongs to this project"),
                "run `agentuna sync status --json` to list items",
            ));
        }
        for item in matches {
            if let Some(record) = self.record(item.id)? {
                return Ok((item, record));
            }
        }
        Err(CliError::new(
            ErrorCode::Conflict,
            "this item has no project sync baseline",
            "deploy or adopt the item into the project first",
        ))
    }
}

/// `sync status`: the stored label and evidence hashes per project item.
fn status(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    let project = ProjectIndex::open(globals, false)?;
    let filter = args
        .get_one::<String>("item")
        .map(|value| value.to_ascii_lowercase());
    let items = project.items()?;
    let mut rows: BTreeMap<ItemId, Value> = BTreeMap::new();
    for item in items {
        let project_copy = item.scope == Scope::Project
            && item.project_id.as_deref() == Some(project.project_id());
        if item.scope != Scope::User && !project_copy {
            continue;
        }
        let record = project.record(item.id)?;
        if !project_copy && record.is_none() {
            continue;
        }
        if let Some(wanted) = &filter {
            let id = item.id.to_string().to_ascii_lowercase();
            if id != *wanted
                && !item.slug.to_ascii_lowercase().contains(wanted)
                && !item.name.to_ascii_lowercase().contains(wanted)
            {
                continue;
            }
        }
        let (label, action) = match &record {
            Some(record) => (
                Some(record.label.wire_name()),
                action_name(action_for(record.label)),
            ),
            None => (None, "untracked"),
        };
        let evidence = record.as_ref().map_or_else(
            || {
                json!({
                    "project_hash": Value::Null,
                    "library_hash": Value::Null,
                    "base_hash": Value::Null,
                })
            },
            evidence_json,
        );
        let row = json!({
            "item_id": item.id.to_string(),
            "slug": item.slug,
            "name": item.name,
            "scope": item.scope.wire_name(),
            "label": label,
            "action": action,
            "evidence": evidence,
        });
        match rows.get_mut(&item.id) {
            // A project-scope row names the copy that participates in the sync; it wins.
            Some(existing) if project_copy => *existing = row,
            Some(_) => {}
            None => {
                rows.insert(item.id, row);
            }
        }
    }
    let mut rows = rows.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left["slug"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["slug"].as_str().unwrap_or_default())
    });
    let count = rows.len();
    let mut human = format!(
        "{count} item(s) for project {}\n",
        project.project_path.display()
    );
    for row in &rows {
        human.push_str(&format!(
            "  {} [{}] {}\n",
            row["slug"].as_str().unwrap_or_default(),
            row["label"].as_str().unwrap_or("untracked"),
            row["action"].as_str().unwrap_or_default()
        ));
    }
    Ok(CommandOutput::new(
        json!({
            "project_id": project.project_id(),
            "project_path": project.project_path.display().to_string(),
            "items": rows,
            "count": count,
        }),
        human,
    ))
}

fn action_name(action: SyncAction) -> &'static str {
    match action {
        SyncAction::None => "none",
        SyncAction::Push => "push",
        SyncAction::Pull => "pull",
        SyncAction::Compare => "compare",
        _ => "compare",
    }
}

fn evidence_json(record: &SyncRecord) -> Value {
    json!({
        "project_hash": record.evidence.project_hash,
        "library_hash": record.evidence.library_hash,
        "base_hash": record.evidence.base_hash,
    })
}

/// What a planned sync needs to preview and apply.
struct PlannedSync {
    plan: SyncPlan,
    label: SyncLabel,
    target_root: PathBuf,
    backup_folder: PathBuf,
    source_hash: String,
}

/// `sync push <item>` / `sync pull <item>`.
fn transfer(
    globals: &GlobalArgs,
    args: &ArgMatches,
    direction: SyncDirection,
) -> Result<CommandOutput, CliError> {
    let direction = direction_name(direction);
    let item_text = required(args, "item")?;
    if !globals.dry_run && !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            format!("sync {direction} changes files and needs --yes"),
            format!(
                "review `agentuna sync {direction} {item_text} --dry-run`, then rerun with --yes"
            ),
        )
        .with_details(json!({ "requires": "yes" })));
    }
    let project = ProjectIndex::open(globals, !globals.dry_run)?;
    let (item, record) = project.item_and_record(item_text)?;
    let planned = build_plan(
        project.paths(),
        project.project_id(),
        &item,
        direction,
        &record,
    )?;
    let facts = json!({
        "direction": direction,
        "item_id": item.id.to_string(),
        "slug": item.slug,
        "project_id": project.project_id(),
        "label": planned.label.wire_name(),
        "evidence": evidence_json(&record),
    });
    let (change_set, missing) = match planned.plan {
        SyncPlan::AlreadyInSync => {
            return Ok(CommandOutput::new(
                merge(
                    facts,
                    json!({ "changed": false, "status": "already_in_sync" }),
                ),
                format!("Nothing to do for {}: the copies match\n", item.slug),
            ));
        }
        SyncPlan::MissingSide(paths) => {
            let missing = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            return Ok(CommandOutput::new(
                merge(
                    facts,
                    json!({
                        "changed": false,
                        "status": "missing_source_paths",
                        "missing_source_paths": missing,
                    }),
                ),
                format!(
                    "Nothing to copy for {}: the destination has {} extra file(s); sync never deletes them\n",
                    item.slug,
                    missing.len()
                ),
            ));
        }
        SyncPlan::Change(change_set) => (change_set, Vec::new()),
        SyncPlan::ChangeWithMissing {
            change_set,
            missing_source_paths,
        } => (
            change_set,
            missing_source_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        ),
        _ => {
            return Err(CliError::new(
                ErrorCode::Conflict,
                "this sync plan is not supported",
                "compare the copies in the desktop app",
            ));
        }
    };
    let preview =
        at_engine::preview::preview(&change_set, &planned.target_root).map_err(preview_error)?;
    let files = preview_rows(&preview);
    if globals.dry_run {
        return Ok(CommandOutput::new(
            merge(
                facts,
                json!({
                    "dry_run": true,
                    "changed": false,
                    "files": files,
                    "files_changed": 0,
                    "missing_source_paths": missing,
                    "target_root": planned.target_root.display().to_string(),
                }),
            ),
            format!(
                "Dry run: {} file(s) would change for {}; nothing was written\n",
                files.len(),
                item.slug
            ),
        ));
    }
    let database = open_database(project.paths())?;
    let mut change_set = change_set;
    change_set.preview_id = preview.id;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new_with_actor(
        database.connection(),
        BACKUP_RETENTION_DAYS,
        at_db::ops::OperationActor::Cli,
    );
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &planned.target_root,
        backup_folder: &planned.backup_folder,
        recorder: &mut recorder,
    };
    let target = SyncBaselineTarget {
        database: &database,
        item_id: &item.id,
        project_id: project.project_id(),
        hash: &planned.source_hash,
    };
    let outcome = apply_sync_plan_and_update_baseline(&change_set, &mut context, target)
        .map_err(sync_plan_error)?;
    Ok(CommandOutput::new(
        merge(
            facts,
            json!({
                "dry_run": false,
                "changed": true,
                "status": "applied",
                "operation_id": outcome.operation_id,
                "files_changed": outcome.completed,
                "files": files,
                "missing_source_paths": missing,
                "target_root": planned.target_root.display().to_string(),
            }),
        ),
        format!(
            "{direction} {}: {} file(s) changed (operation {})\n",
            item.slug, outcome.completed, outcome.operation_id
        ),
    ))
}

/// Merges two JSON objects; the later object wins on a shared key.
fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

/// Plans one copy, reading the exact source and destination bytes.
fn build_plan(
    paths: &at_store::paths::AppPaths,
    project_id: &str,
    item: &at_db::shared::ItemRow,
    direction: &str,
    record: &SyncRecord,
) -> Result<PlannedSync, CliError> {
    let direction = if direction == "push" {
        SyncDirection::Push
    } else {
        SyncDirection::Pull
    };
    let project_store = StoreLayout::for_scope(paths, Scope::Project).map_err(store_error)?;
    let user_store = StoreLayout::for_scope(paths, Scope::User).map_err(store_error)?;
    let (source_store, target_store) = if direction == SyncDirection::Push {
        (&project_store, &user_store)
    } else {
        (&user_store, &project_store)
    };
    let slug = Slug::parse(&item.slug)
        .map_err(|_| CliError::internal("the indexed skill slug is not a valid slug"))?;
    let source =
        load_package(&source_store.skill_dir(&slug), &item.slug, true)?.ok_or_else(|| {
            CliError::new(
                ErrorCode::NotFound,
                format!("`{}` has no source copy to {direction:?}", item.slug),
                "run `agentuna sync status --json` to see which side exists",
            )
        })?;
    let destination = load_package(&target_store.skill_dir(&slug), &item.slug, false)?;
    let source_hash = source.hash.to_string();
    // Package paths are relative to `<store>/skills/<slug>`; the reviewed plan, the preview
    // and the apply all address the target store root, so every path is prefixed once here.
    let prefix = PathBuf::from("skills").join(slug.as_str());
    let plan = plan_sync_tree(SyncTreeRequest {
        item_id: item.id,
        project_id: project_id.to_owned(),
        direction,
        label: record.label,
        base_hash: record.evidence.base_hash.clone(),
        destination_hash: destination.as_ref().map(|loaded| loaded.hash.to_string()),
        source_hash: source_hash.clone(),
        // The reviewed preview (`preview_id` below) is the review gate: `--dry-run` prints it and
        // `--yes` accepts it. Labels that may not be overwritten are refused before this point.
        force_reviewed: true,
        destination_root_exists: target_store.root().is_dir(),
        source_files: prefixed(package_files(&source.package), &prefix),
        source_file_hashes: prefixed(package_hashes(&source.package), &prefix),
        destination_files: destination.as_ref().map_or_else(BTreeMap::new, |loaded| {
            prefixed(package_hashes(&loaded.package), &prefix)
        }),
    })
    .map_err(sync_plan_error)?;
    Ok(PlannedSync {
        plan,
        label: record.label,
        target_root: target_store.root().to_path_buf(),
        backup_folder: target_store.backups_dir(),
        source_hash,
    })
}

/// Rebases package-relative keys onto the item's folder inside the store.
fn prefixed<T>(entries: BTreeMap<PathBuf, T>, prefix: &Path) -> BTreeMap<PathBuf, T> {
    entries
        .into_iter()
        .map(|(path, value)| (prefix.join(path), value))
        .collect()
}

/// Reads a skill package, treating only a missing directory as absent.
fn load_package(
    path: &Path,
    slug: &str,
    required: bool,
) -> Result<Option<LoadedSkillPackage>, CliError> {
    match SkillPackage::read(path) {
        Ok(loaded) => Ok(Some(loaded)),
        Err(SkillPackageError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            if required {
                Err(CliError::new(
                    ErrorCode::NotFound,
                    format!("the `{slug}` package is missing from its store"),
                    "run `agentuna doctor` to inspect the store",
                ))
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(CliError::new(
            ErrorCode::Conflict,
            format!("the `{slug}` package cannot be safely read: {error}"),
            "inspect the package; symlinked or unreadable files are refused",
        )),
    }
}

fn sync_plan_error(error: SyncPlanError) -> CliError {
    match &error {
        SyncPlanError::Conflict => CliError::new(
            ErrorCode::Conflict,
            "the project and library copies conflict; sync will not overwrite either",
            "compare both copies and resolve the difference first",
        ),
        SyncPlanError::TargetChanged => CliError::new(
            ErrorCode::TargetChanged,
            "the sync destination changed since the recorded baseline",
            "review the destination with --dry-run, then retry",
        ),
        SyncPlanError::DirectionNotAllowed { .. } => CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "run `agentuna sync status --json` and use the direction it reports",
        ),
        SyncPlanError::MissingProjectStore => CliError::new(
            ErrorCode::NotFound,
            "the project store is missing; sync never recreates it",
            "run `agentuna doctor` in the project first",
        ),
        SyncPlanError::SourceHashMismatch => CliError::new(
            ErrorCode::Conflict,
            "the source changed while the plan was built",
            "retry the command",
        ),
        SyncPlanError::InvalidTreePath(path) => CliError::new(
            ErrorCode::Validation,
            format!("invalid sync path `{path}`"),
            "rename the file to a safe store-relative path",
        ),
        SyncPlanError::Preview(_) => CliError::new(
            ErrorCode::TargetChanged,
            "files changed since the reviewed preview",
            "review the sync again with --dry-run",
        ),
        SyncPlanError::BaselineNotFound => CliError::new(
            ErrorCode::Conflict,
            "the project deployment row for this item no longer exists",
            "deploy or adopt the item into the project again",
        ),
        _ => CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "inspect both copies with `agentuna sync status`, then retry",
        ),
    }
}

fn preview_error(error: at_engine::preview::PreviewError) -> CliError {
    match error {
        at_engine::preview::PreviewError::ConcurrentModification => CliError::new(
            ErrorCode::TargetChanged,
            "the sync destination changed since the plan was built",
            "review the sync again with --dry-run",
        ),
        other => CliError::new(
            ErrorCode::Conflict,
            format!("the sync preview could not be read: {other}"),
            "inspect both copies and retry",
        ),
    }
}

fn preview_rows(preview: &at_engine::preview::Preview) -> Vec<Value> {
    preview
        .files
        .iter()
        .map(|file| {
            json!({
                "path": file.path.display().to_string(),
                "action": file.action,
                "before_hash": file.before_hash,
                "after_hash": file.after_hash,
                "is_new": file.is_new,
                "is_unmanaged": file.is_unmanaged,
                "backup_required": file.backup_required,
                "diff": diff_text(&file.diff),
            })
        })
        .collect()
}

fn diff_text(diff: &at_engine::preview::DiffKind) -> String {
    match diff {
        at_engine::preview::DiffKind::Unified { text, .. } => text.clone(),
        at_engine::preview::DiffKind::Binary { size } => format!("Binary file ({size} bytes)"),
        at_engine::preview::DiffKind::TooLarge { size } => {
            format!("File too large to display ({size} bytes)")
        }
        at_engine::preview::DiffKind::Unchanged => String::new(),
        _ => "This file type cannot be previewed.".to_owned(),
    }
}

/// Remote-backup state when no index exists yet.
fn unconfigured_state() -> at_db::remote_backup::RemoteBackupState {
    at_db::remote_backup::RemoteBackupState {
        repo_url: None,
        branch: None,
        last_push_at: None,
        last_pull_at: None,
        last_commit: None,
        status: at_db::remote_backup::RemoteBackupStatus::Unconfigured,
        error_kind: None,
    }
}

/// The stored device name, resolved through settings exactly like the app does.
fn stored_device_name(index: Option<&at_db::ReadOnlyDatabase>) -> Result<Option<String>, CliError> {
    let Some(index) = index else {
        return Ok(None);
    };
    let settings = at_db::settings::load(index.connection()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("remote-backup settings are unavailable: {error}"),
            "run `agentuna doctor` to inspect the index",
        )
    })?;
    Ok(Some(settings.remote_backup_device().map_or_else(
        at_store::remote_backup::default_device_name,
        ToOwned::to_owned,
    )))
}

/// The credential backend selected for this invocation.
fn secret_store() -> Box<dyn SecretStore> {
    match std::env::var(SECRET_STORE_ENV).ok().as_deref() {
        Some("memory") => Box::new(MemoryStore::new()),
        _ => Box::new(KeychainStore::new()),
    }
}

/// Injected IO for one remote operation, so tests never spawn git or read the Keychain.
struct RemoteIo<'a> {
    secrets: &'a dyn SecretStore,
    remote: &'a dyn RemoteGitRunner,
    local: &'a dyn LocalGitRunner,
    policy: NetworkPolicy,
    now: OffsetDateTime,
}

/// One remote-backup action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteOperation {
    Backup,
    Restore,
}

/// `remote status [--refresh]`.
fn remote_status(globals: &GlobalArgs, refresh: bool) -> Result<CommandOutput, CliError> {
    let access = CliAccess::for_command(globals, false)?;
    let paths = access.paths().clone();
    let index = if paths.db_path().exists() {
        Some(crate::runtime::open_index_read_only(&paths)?)
    } else {
        None
    };
    let store = StoreLayout::for_scope(&paths, Scope::User).map_err(store_error)?;
    let state = match &index {
        Some(index) => at_db::remote_backup::remote_backup_state(index.connection())
            .map_err(|error| db_error(error.to_string()))?,
        None => unconfigured_state(),
    };
    let policy = NetworkPolicy::default();
    let conflict_count = at_store::remote_backup::remote_conflict_count(&store)
        .map_err(|error| remote_error(error, &Redactor::new()))?;
    let token_saved = if state.repo_url.is_some() {
        let secrets = secret_store();
        at_secrets::token_exists(secrets.as_ref()).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("the credential store could not be read: {error}"),
                "unlock the login keychain and retry",
            )
        })?
    } else {
        false
    };
    let device = stored_device_name(index.as_ref())?;
    let mut network_contacted = false;
    let mut head: Option<RemoteHead> = None;
    if refresh {
        let repo_url = state.repo_url.clone().ok_or_else(unconfigured)?;
        let secrets = secret_store();
        let remote = ProcessRemoteGitRunner::default();
        let local = ProcessLocalGitRunner::default();
        let io = RemoteIo {
            secrets: secrets.as_ref(),
            remote: &remote,
            local: &local,
            policy: policy.clone(),
            now: OffsetDateTime::now_utc(),
        };
        head = Some(refresh_head(&repo_url, &io)?);
        network_contacted = true;
    }
    let branch = head
        .as_ref()
        .and_then(|head| head.branch.clone())
        .or_else(|| state.branch.clone());
    let remote_commit = head.as_ref().map(|head| head.commit.clone());
    let data = json!({
        "configured": state.repo_url.is_some(),
        "repo_url": state.repo_url,
        "branch": branch,
        "device_name": device,
        "status": state.status.as_str(),
        "error_kind": state.error_kind,
        "last_push_at": state.last_push_at,
        "last_sync_at": state.last_pull_at,
        "last_commit": state.last_commit,
        "remote_commit": remote_commit,
        "conflict_count": conflict_count,
        "offline": policy.offline(),
        "token_saved": token_saved,
        "network_contacted": network_contacted,
    });
    let repo = data["repo_url"].as_str().unwrap_or("no repository");
    let human = format!(
        "remote backup: {} ({repo})\n  configured: {}\n  last commit: {}\n  conflicts: {conflict_count}\n",
        state.status.as_str(),
        if data["configured"].as_bool() == Some(true) {
            "yes"
        } else {
            "no"
        },
        data["last_commit"].as_str().unwrap_or("-"),
    );
    Ok(CommandOutput::new(data, human))
}

/// Probes the configured remote's HEAD; the only network call `remote status` can make.
fn refresh_head(repo_url: &str, io: &RemoteIo<'_>) -> Result<RemoteHead, CliError> {
    let token = at_secrets::read_token(io.secrets).map_err(secrets_error)?;
    let auth = GitAuth::token(token.expose_secret()).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "the stored credential cannot be used for git authentication",
            "replace the GitHub token in the desktop app",
        )
    })?;
    let mut redactor = Redactor::new();
    redactor.insert_value(token.expose_secret());
    let mut log = NetworkEventLog::new();
    let head = {
        let mut context = RemoteContext {
            policy: &io.policy,
            log: &mut log,
            runner: io.remote,
            local: io.local,
            redact: &|text| redactor.redact_text(text),
            now: io.now,
        };
        context.remote_head(repo_url, &auth)
    };
    head.map_err(|error| remote_error(BackupError::from(error), &redactor))
}

/// `remote backup`.
fn remote_backup(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    destructive_gate(
        globals,
        "remote backup",
        "push the User store to the configured remote",
    )?;
    let access = CliAccess::for_command(globals, !globals.dry_run)?;
    if !globals.dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let store = StoreLayout::for_scope(&paths, Scope::User).map_err(store_error)?;
    if globals.dry_run {
        let index = crate::runtime::open_index_read_only(&paths).map_err(|error| {
            if error.code() == ErrorCode::NotFound {
                unconfigured()
            } else {
                error
            }
        })?;
        let state = at_db::remote_backup::remote_backup_state(index.connection())
            .map_err(|error| db_error(error.to_string()))?;
        let repo_url = state.repo_url.clone().ok_or_else(unconfigured)?;
        let device = stored_device_name(Some(&index))?
            .unwrap_or_else(at_store::remote_backup::default_device_name);
        let items = store_item_count(&store)?;
        return Ok(CommandOutput::new(
            json!({
                "dry_run": true,
                "repo_url": repo_url,
                "device_name": device,
                "items": items,
                "changed": false,
            }),
            format!(
                "Dry run: {items} item(s) from the User store would be committed and pushed; nothing was written\n"
            ),
        ));
    }
    let database = configured_database(&paths)?;
    let state = at_db::remote_backup::remote_backup_state(database.connection())
        .map_err(|error| db_error(error.to_string()))?;
    let repo_url = state.repo_url.clone().ok_or_else(unconfigured)?;
    let device = at_store::remote_backup::device_name(&database)
        .map_err(|error| remote_error(error, &Redactor::new()))?;
    let secrets = secret_store();
    let remote = ProcessRemoteGitRunner::default();
    let local = ProcessLocalGitRunner::default();
    let io = RemoteIo {
        secrets: secrets.as_ref(),
        remote: &remote,
        local: &local,
        policy: NetworkPolicy::default(),
        now: OffsetDateTime::now_utc(),
    };
    let outcome = perform_remote(&database, &store, &io, RemoteOperation::Backup, None)?;
    Ok(CommandOutput::new(
        backup_json(&outcome, &repo_url, &device),
        render_outcome("backup", &outcome),
    ))
}

/// `remote restore <commit>`.
fn remote_restore(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    let commit = required(args, "commit")?;
    validate_commit(commit)?;
    destructive_gate(
        globals,
        "remote restore",
        "replace the empty User store with the selected snapshot",
    )?;
    let access = CliAccess::for_command(globals, !globals.dry_run)?;
    if !globals.dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let store = StoreLayout::for_scope(&paths, Scope::User).map_err(store_error)?;
    if globals.dry_run {
        let index = crate::runtime::open_index_read_only(&paths).map_err(|error| {
            if error.code() == ErrorCode::NotFound {
                unconfigured()
            } else {
                error
            }
        })?;
        let state = at_db::remote_backup::remote_backup_state(index.connection())
            .map_err(|error| db_error(error.to_string()))?;
        let repo_url = state.repo_url.clone().ok_or_else(unconfigured)?;
        let empty = store_item_count(&store)? == 0;
        return Ok(CommandOutput::new(
            json!({
                "dry_run": true,
                "commit": commit,
                "repo_url": repo_url,
                "target_empty": empty,
                "changed": false,
            }),
            format!(
                "Dry run: commit {commit} would be restored into the User store (target empty: {empty}); nothing was written\n"
            ),
        ));
    }
    let database = configured_database(&paths)?;
    let state = at_db::remote_backup::remote_backup_state(database.connection())
        .map_err(|error| db_error(error.to_string()))?;
    let repo_url = state.repo_url.clone().ok_or_else(unconfigured)?;
    let device = at_store::remote_backup::device_name(&database)
        .map_err(|error| remote_error(error, &Redactor::new()))?;
    let secrets = secret_store();
    let remote = ProcessRemoteGitRunner::default();
    let local = ProcessLocalGitRunner::default();
    let io = RemoteIo {
        secrets: secrets.as_ref(),
        remote: &remote,
        local: &local,
        policy: NetworkPolicy::default(),
        now: OffsetDateTime::now_utc(),
    };
    let outcome = perform_remote(
        &database,
        &store,
        &io,
        RemoteOperation::Restore,
        Some(commit),
    )?;
    Ok(CommandOutput::new(
        backup_json(&outcome, &repo_url, &device),
        render_outcome("restore", &outcome),
    ))
}

/// Applies the `--yes` rule every destructive sync/remote command follows: without it the
/// command refuses with `confirmation_required` (exit 9) and writes nothing.
fn destructive_gate(globals: &GlobalArgs, command: &str, action: &str) -> Result<(), CliError> {
    if globals.dry_run || globals.yes {
        return Ok(());
    }
    Err(CliError::new(
        ErrorCode::ConfirmationRequired,
        format!("{command} would {action} and needs --yes"),
        format!("inspect it with `agentuna {command} --dry-run`, then rerun with --yes"),
    )
    .with_details(json!({ "requires": "yes" })))
}

/// Opens the index a remote write needs, refusing when remote backup was never configured.
fn configured_database(paths: &at_store::paths::AppPaths) -> Result<Database, CliError> {
    if !paths.db_path().exists() {
        return Err(unconfigured());
    }
    let database = open_database(paths)?;
    let state = at_db::remote_backup::remote_backup_state(database.connection())
        .map_err(|error| db_error(error.to_string()))?;
    if state.repo_url.is_none() {
        return Err(unconfigured());
    }
    Ok(database)
}

fn unconfigured() -> CliError {
    CliError::new(
        ErrorCode::Validation,
        "no remote repository is configured",
        "configure the repository in the desktop app, then retry",
    )
}

/// Number of items in a store manifest; a missing manifest is an empty store.
fn store_item_count(store: &StoreLayout) -> Result<usize, CliError> {
    match StoreManifest::load(store) {
        Ok(loaded) => Ok(loaded.manifest.items().len()),
        Err(error) if error.is_missing() => Ok(0),
        Err(error) => Err(CliError::new(
            ErrorCode::Conflict,
            format!("store.json is unavailable: {error}"),
            "run `agentuna doctor` to inspect the store",
        )),
    }
}

fn validate_commit(commit: &str) -> Result<(), CliError> {
    let valid = (commit.len() == 40 || commit.len() == 64)
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        return Ok(());
    }
    Err(CliError::new(
        ErrorCode::Validation,
        "the commit must be the full 40- or 64-character hexadecimal id",
        "copy the commit id from `agentuna remote status --refresh` or the desktop app",
    ))
}

/// Runs one engine operation with the supplied IO.
fn perform_remote(
    database: &Database,
    store: &StoreLayout,
    io: &RemoteIo<'_>,
    operation: RemoteOperation,
    commit: Option<&str>,
) -> Result<BackupOutcome, CliError> {
    let mut redactor = Redactor::new();
    match at_secrets::read_token(io.secrets) {
        Ok(token) => {
            redactor.insert_value(token.expose_secret());
        }
        Err(error) if is_missing_secret(&error) => {}
        Err(error) => return Err(secrets_error(error)),
    }
    let mut log = NetworkEventLog::new();
    let mut request = BackupRequest {
        store,
        db: database,
        secrets: io.secrets,
        policy: &io.policy,
        log: &mut log,
        remote_runner: io.remote,
        local_runner: io.local,
        redactor: &redactor,
        now: io.now,
    };
    let outcome = match operation {
        RemoteOperation::Backup => at_store::remote_backup::push_backup(&mut request),
        RemoteOperation::Restore => {
            let commit = commit.unwrap_or_default();
            at_store::remote_backup::restore_from_remote(&mut request, commit, false)
        }
    };
    outcome.map_err(|error| remote_error(error, &redactor))
}

fn is_missing_secret(error: &at_secrets::SecretsError) -> bool {
    matches!(error, at_secrets::SecretsError::NotFound { .. })
}

fn secrets_error(error: at_secrets::SecretsError) -> CliError {
    if is_missing_secret(&error) {
        return CliError::new(
            ErrorCode::AuthFailed,
            "no GitHub token is stored for remote backup",
            "save a repository-scoped token with the desktop app, then retry",
        );
    }
    CliError::new(
        ErrorCode::Conflict,
        format!("the credential store could not be read: {error}"),
        "unlock the login keychain and retry",
    )
}

/// Maps a remote failure onto the published CLI codes; the message is always redacted.
fn remote_error(error: BackupError, redactor: &Redactor) -> CliError {
    match &error {
        BackupError::TokenMissing | BackupError::AuthRejected => CliError::new(
            ErrorCode::AuthFailed,
            redactor.redact_text(&error.to_string()),
            "save a repository-scoped GitHub token with the desktop app, then retry",
        ),
        BackupError::Unconfigured => unconfigured(),
        BackupError::Offline => CliError::new(
            ErrorCode::Offline,
            redactor.redact_text(&error.to_string()),
            "check the network connection, then retry",
        ),
        BackupError::Denied(denied) => match denied.reason {
            DenialReason::Offline => CliError::new(
                ErrorCode::Offline,
                "offline mode refused the remote operation",
                "turn offline mode off in the desktop app, then retry",
            ),
            _ => CliError::new(
                ErrorCode::NetworkRequired,
                "the network policy refused the remote operation",
                "review the network policy in the desktop app",
            ),
        },
        BackupError::Conflict => CliError::new(
            ErrorCode::Conflict,
            redactor.redact_text(&error.to_string()),
            "sync the remote first with the desktop app, then retry",
        ),
        BackupError::DeletionConfirmationRequired => CliError::new(
            ErrorCode::ConfirmationRequired,
            redactor.redact_text(&error.to_string()),
            "confirm remote deletions in the desktop app, then retry",
        ),
        BackupError::NonEmptyRestore => CliError::new(
            ErrorCode::Conflict,
            redactor.redact_text(&error.to_string()),
            "restore only into an empty User store",
        ),
        BackupError::InvalidSnapshotPath(path) => CliError::new(
            ErrorCode::Validation,
            format!("invalid remote snapshot path `{}`", path.display()),
            "choose another commit",
        ),
        BackupError::CorruptSnapshot { item } => CliError::new(
            ErrorCode::Conflict,
            format!("the remote snapshot failed integrity verification at `{item}`"),
            "choose another commit",
        ),
        BackupError::Git(git) if git.is_auth() => CliError::new(
            ErrorCode::AuthFailed,
            redactor.redact_text(&error.to_string()),
            "replace the GitHub token in the desktop app, then retry",
        ),
        BackupError::Git(git) if git.is_offline() => CliError::new(
            ErrorCode::Offline,
            redactor.redact_text(&error.to_string()),
            "check the network connection, then retry",
        ),
        BackupError::InvalidDeviceName(_) => CliError::new(
            ErrorCode::Validation,
            redactor.redact_text(&error.to_string()),
            "set a valid device name in the desktop app",
        ),
        _ => CliError::new(
            ErrorCode::Conflict,
            redactor.redact_text(&error.to_string()),
            "run `agentuna doctor`, then retry",
        ),
    }
}

fn backup_json(outcome: &BackupOutcome, repo_url: &str, device: &str) -> Value {
    json!({
        "repo_url": repo_url,
        "device_name": device,
        "commit": outcome.commit,
        "committed": outcome.committed,
        "pushed": outcome.pushed,
        "status": outcome.status.as_str(),
        "error_kind": outcome.error_kind,
        "conflicts": outcome
            .conflicts
            .iter()
            .map(|conflict| json!({
                "local_path": conflict.local_path.display().to_string(),
                "remote_path": conflict.remote_path.display().to_string(),
            }))
            .collect::<Vec<_>>(),
        "changed": outcome.committed,
    })
}

fn render_outcome(action: &str, outcome: &BackupOutcome) -> String {
    let commit = outcome.commit.as_deref().unwrap_or("-");
    if outcome.committed {
        format!("{action}: commit {commit} pushed\n")
    } else {
        format!("{action}: nothing new to push\n")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Deterministic token-shaped fixture; never a real credential.
    const TOKEN: &str = concat!("ghp_", "sentinel", "0123456789abcdefghij");

    /// A remote runner that fails every call with fixed stderr.
    struct FixtureRemote {
        stderr: &'static [u8],
        calls: AtomicUsize,
    }

    impl at_git::RemoteGitRunner for FixtureRemote {
        fn run(
            &self,
            _call: &at_git::RemoteGitCall<'_>,
        ) -> Result<at_git::GitOutput, at_git::GitError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(at_git::GitOutput {
                success: false,
                code: Some(128),
                stdout: Vec::new(),
                stderr: self.stderr.to_vec(),
            })
        }
    }

    /// A local runner that succeeds without touching git.
    struct FixtureLocal;

    impl at_git::LocalGitRunner for FixtureLocal {
        fn run(
            &self,
            _call: &at_git::LocalGitCall<'_>,
        ) -> Result<at_git::GitOutput, at_git::GitError> {
            Ok(at_git::GitOutput {
                success: true,
                code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    fn sandbox() -> (tempfile::TempDir, Database, StoreLayout) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = temp.path().join("user-store");
        at_store::layout::init_store(Scope::User, &root).expect("init store");
        let database = Database::open_file(temp.path().join("agentuna.db")).expect("index");
        let store = StoreLayout::new(Scope::User, root);
        (temp, database, store)
    }

    fn configure(database: &Database, repo: &str) {
        let mut state =
            at_db::remote_backup::remote_backup_state(database.connection()).expect("remote state");
        state.repo_url = Some(repo.to_owned());
        at_db::remote_backup::save_remote_backup_state(database.connection(), &state)
            .expect("save state");
    }

    fn io<'a>(
        secrets: &'a MemoryStore,
        remote: &'a FixtureRemote,
        local: &'a FixtureLocal,
    ) -> RemoteIo<'a> {
        RemoteIo {
            secrets,
            remote,
            local,
            policy: NetworkPolicy::default(),
            now: OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("time"),
        }
    }

    #[test]
    fn remote_backup_auth_failure_is_typed_and_never_echoes_the_token() {
        let (temp, database, store) = sandbox();
        let repo = temp.path().join("remote-repo");
        configure(&database, repo.to_str().expect("utf8 repo path"));
        let secrets = MemoryStore::new();
        at_secrets::save_token(&secrets, TOKEN).expect("store token");
        let remote = FixtureRemote {
            stderr: b"remote: invalid username or token\nfatal: Authentication failed for '<repo>'",
            calls: AtomicUsize::new(0),
        };
        let local = FixtureLocal;
        let io = io(&secrets, &remote, &local);
        let error = perform_remote(&database, &store, &io, RemoteOperation::Backup, None)
            .expect_err("auth failure");
        assert_eq!(error.code(), ErrorCode::AuthFailed);
        let rendered = format!(
            "{}|{}|{:?}|{:?}",
            error.message(),
            error.hint(),
            error.details(),
            error.data()
        );
        assert!(
            !rendered.contains("sentinel"),
            "the token leaked: {rendered}"
        );
        assert!(
            !repo.exists(),
            "a failed backup must not create the remote repository"
        );
        assert!(
            remote.calls.load(Ordering::SeqCst) >= 1,
            "the remote was contacted"
        );
    }

    #[test]
    fn refresh_head_classifies_a_dns_failure_as_offline() {
        let secrets = MemoryStore::new();
        at_secrets::save_token(&secrets, TOKEN).expect("store token");
        let remote = FixtureRemote {
            stderr: b"fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com",
            calls: AtomicUsize::new(0),
        };
        let local = FixtureLocal;
        let io = io(&secrets, &remote, &local);
        let error = refresh_head("https://github.com/x/y.git", &io).expect_err("offline");
        assert_eq!(error.code(), ErrorCode::Offline);
        assert_eq!(error.exit_code(), 10);
        assert_eq!(remote.calls.load(Ordering::SeqCst), 1);
        assert!(!format!("{error}").contains("sentinel"));
    }

    #[test]
    fn missing_or_rejected_credentials_are_auth_failed() {
        assert_eq!(
            remote_error(BackupError::TokenMissing, &Redactor::new()).code(),
            ErrorCode::AuthFailed
        );
        assert_eq!(
            remote_error(BackupError::AuthRejected, &Redactor::new()).code(),
            ErrorCode::AuthFailed
        );
        assert_eq!(
            remote_error(BackupError::Offline, &Redactor::new()).code(),
            ErrorCode::Offline
        );
    }

    #[test]
    fn destructive_commands_without_yes_exit_with_the_confirmation_code() {
        let globals = GlobalArgs {
            json: true,
            project: None,
            scope: None,
            yes: false,
            dry_run: false,
            read_only: false,
            verbose: 0,
            no_color: true,
        };
        let error = destructive_gate(&globals, "remote backup", "push").expect_err("needs --yes");
        assert_eq!(error.code(), ErrorCode::ConfirmationRequired);
        assert_eq!(error.exit_code(), crate::exit_code::CONFIRMATION_REQUIRED);
        let confirmed = GlobalArgs {
            yes: true,
            ..globals
        };
        assert!(destructive_gate(&confirmed, "remote backup", "push").is_ok());
    }

    #[test]
    fn sync_plan_failures_use_the_published_typed_codes() {
        assert_eq!(
            sync_plan_error(SyncPlanError::TargetChanged).code(),
            ErrorCode::TargetChanged
        );
        assert_eq!(
            sync_plan_error(SyncPlanError::Conflict).code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            sync_plan_error(SyncPlanError::MissingProjectStore).code(),
            ErrorCode::NotFound
        );
        assert_eq!(
            preview_error(at_engine::preview::PreviewError::ConcurrentModification).code(),
            ErrorCode::TargetChanged
        );
    }

    #[test]
    fn commit_ids_must_be_full_hexadecimal() {
        assert!(validate_commit(&"a".repeat(40)).is_ok());
        assert!(validate_commit(&"A".repeat(64)).is_ok());
        assert_eq!(
            validate_commit("abc").expect_err("short").code(),
            ErrorCode::Validation
        );
    }
}
