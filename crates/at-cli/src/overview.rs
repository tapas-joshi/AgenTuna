//! Overview command for the shared engine projection.

use at_db::ReadOnlyDatabase;
use at_engine::overview;
use at_model::Scope;
use clap::{ArgMatches, Command};
use serde_json::json;

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, resolve_paths};

/// `agentuna overview`: read the same aggregate used by the Overview screen.
pub struct OverviewGroup;

impl CommandGroup for OverviewGroup {
    fn name(&self) -> &'static str {
        "overview"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(Command::new("overview").about("Show scope counts and recent activity"))
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        _matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let scope = globals.scope().ok_or_else(|| {
            CliError::new(
                ErrorCode::Validation,
                "overview requires --scope",
                "choose global, user, project or local",
            )
        })?;
        crate::runtime::refresh_agent_registry_if_needed(globals)?;
        let paths = resolve_paths(globals)?;
        let _access = CliAccess::read(&paths)?;
        if !paths.db_path().exists() {
            return Err(CliError::new(
                ErrorCode::NotFound,
                "the AgenTuna index does not exist yet",
                "run `agentuna doctor` to initialize the index",
            ));
        }
        let database = ReadOnlyDatabase::open(paths.db_path()).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("cannot read the AgenTuna index: {error}"),
                "run `agentuna doctor` to inspect the index",
            )
        })?;
        let payload = overview::build(database.connection(), scope).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("cannot build Overview: {error}"),
                "run `agentuna doctor` to inspect the index",
            )
        })?;
        let mut attention: Vec<_> = payload
            .needs_attention
            .into_iter()
            .map(|row| {
                json!({
                    "item_id": row.item_id.map(|id| id.to_string()),
                    "title": row.title,
                    "subtitle": row.subtitle,
                    "kind": format!("{:?}", row.kind).to_lowercase(),
                    "screen": row.navigation.as_ref().map(|target| target.screen.clone()),
                    "row_id": row.navigation.and_then(|target| target.row_id),
                })
            })
            .collect();
        let locks = at_db::locks::locks_for_scope(database.connection(), Scope::Global).map_err(
            |error| {
                CliError::new(
                    ErrorCode::Conflict,
                    error.to_string(),
                    "run `agentuna doctor` to inspect the index",
                )
            },
        )?;
        let lock_ids: Vec<_> = locks.iter().map(|lock| lock.item_id).collect();
        let indexed_ids: std::collections::HashSet<_> =
            at_db::shared::items_by_ids(database.connection(), &lock_ids)
                .map_err(|error| {
                    CliError::new(
                        ErrorCode::Conflict,
                        error.to_string(),
                        "run `agentuna doctor` to inspect the index",
                    )
                })?
                .into_iter()
                .map(|item| item.id)
                .collect();
        for lock in locks {
            if !indexed_ids.contains(&lock.item_id) {
                let item_id = lock.item_id.to_string();
                attention.push(json!({
                    "item_id": item_id,
                    "title": "Locked item not found",
                    "subtitle": lock.reason,
                    "kind": "locked_item_not_found",
                    "screen": "effective",
                    "row_id": item_id,
                }));
            }
        }
        let changes = payload
            .recent_changes
            .into_iter()
            .map(|row| {
                let detail = at_db::ops::operation_detail(database.connection(), &row.operation.id)
                    .map_err(|error| CliError::new(ErrorCode::Conflict, error.to_string(), "run `agentuna doctor` to inspect the index"))?;
                let scope = row.operation.scope.map(|scope| scope.to_string());
                let files = detail
                    .into_iter()
                    .flat_map(|detail| detail.files)
                    .filter_map(|file| file.path)
                    .map(|path| json!({"path": path, "scope": scope}))
                    .collect::<Vec<_>>();
                Ok(json!({
                    "id": row.operation.id,
                    "kind": row.operation.kind.map(|kind| format!("{kind:?}").to_lowercase()).unwrap_or_else(|| "change".to_owned()),
                    "summary": row.operation.summary,
                    "created_at": row.operation.created_at,
                    "file_count": row.file_count,
                    "scope": scope,
                    "files": files,
                }))
            })
            .collect::<Result<Vec<_>, CliError>>()?;
        let settings = at_db::settings::load(database.connection()).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                error.to_string(),
                "run `agentuna doctor` to inspect the index",
            )
        })?;
        let mut paths = paths;
        paths.apply_settings_global_store(settings.global_store_path());
        let global_status = at_fs::privilege::GlobalStoreStatus::probe(paths.global_store());
        let scope_rows: Vec<_> = payload
            .scopes
            .iter()
            .map(|card| {
                json!({
                    "scope": card.scope.to_string(),
                    "skills": card.counts.skills,
                    "mcp": card.counts.mcp,
                    "rules": card.counts.rules,
                    "locks": card.counts.locks,
                    "last_edited": card.last_edited,
                    "disabled": card.disabled_count,
                })
            })
            .collect();
        let data = json!({
            "scopes": scope_rows,
            "attention": attention,
            "attention_total": payload.needs_attention_total,
            "changes": changes,
            "user_path": paths.user_store().display().to_string(),
            "global_path": paths.global_store().display().to_string(),
            "global_writable": global_status.is_available(),
            "global_reason": global_status.reason(),
        });
        let human = payload
            .scopes
            .iter()
            .map(|card| {
                format!(
                    "{}: {} skills · {} servers · {} rules · {} locks",
                    card.scope,
                    card.counts.skills,
                    card.counts.mcp,
                    card.counts.rules,
                    card.counts.locks
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(CommandOutput::new(data, format!("{human}\n")))
    }
}
