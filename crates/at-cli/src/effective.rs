//! TA-070 effective configuration query.

use std::str::FromStr;

use at_adapters::adapter::AgentAdapterRegistry;
use at_db::ReadOnlyDatabase;
use at_engine::effective::{AgentLoads, EffectiveQuery, EffectiveQueryView, query};
use at_engine::loader::{AgentObservation, agent_loads, load_scope_snapshots, observe_agents};
use at_engine::locks::{LockStore, MemoryLockStore};
use at_model::{AgentId, ItemKind, Scope, ScopeSnapshot};
use clap::{Arg, ArgMatches, Command};
use serde_json::json;

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::{CommandOutput, Warning};
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, resolve_paths};

/// `agentuna effective`: inspect the resolved configuration without changing it.
pub struct EffectiveGroup;

impl CommandGroup for EffectiveGroup {
    fn name(&self) -> &'static str {
        "effective"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("effective")
                .about("Show effective configuration at a scope")
                .long_about(
                    "Show the items that resolve at a scope, joined with per-agent load \
                     observations. Without --scope the resolved project's scope answers; on a \
                     machine with no project it falls back to user. The chosen scope is \
                     reported in `data.scope` and, in human mode, as a warning line.",
                )
                .arg(
                    Arg::new("agent")
                        .long("agent")
                        .value_name("ID")
                        .help("Limit output to one agent"),
                )
                .arg(
                    Arg::new("kind")
                        .long("kind")
                        .value_name("KIND")
                        .value_parser(["skill", "mcp", "mcp_server", "rule"])
                        .help("Limit output to one item kind"),
                )
                .subcommand(
                    Command::new("lock")
                        .about("Lock an item at Global scope")
                        .arg(Arg::new("item").required(true).value_name("ITEM"))
                        .arg(
                            Arg::new("reason")
                                .long("reason")
                                .required(true)
                                .value_name("TEXT"),
                        ),
                )
                .subcommand(
                    Command::new("unlock")
                        .about("Remove a Global lock")
                        .arg(Arg::new("item").required(true).value_name("ITEM")),
                ),
        )
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        if globals.read_only && matches!(matches.subcommand(), Some(("lock" | "unlock", _))) {
            let access = CliAccess::for_command(globals, true)?;
            access.ensure_writable()?;
        }
        if let Some((command, args)) = matches.subcommand() {
            return match command {
                "lock" => mutate_lock(globals, args, true),
                "unlock" => mutate_lock(globals, args, false),
                _ => Err(CliError::new(
                    ErrorCode::Usage,
                    "unsupported effective subcommand",
                    "run `agentuna effective --help`",
                )),
            };
        }
        let kind = matches
            .get_one::<String>("kind")
            .map(|kind| match kind.as_str() {
                "mcp" | "mcp_server" => Ok(ItemKind::McpServer),
                "skill" => Ok(ItemKind::Skill),
                "rule" => Ok(ItemKind::Rule),
                other => Err(CliError::new(
                    ErrorCode::Usage,
                    format!("unknown item kind `{other}`"),
                    "use --kind skill, mcp or rule",
                )),
            })
            .transpose()?;
        let agent = matches
            .get_one::<String>("agent")
            .map(|value| {
                AgentId::from_str(value).map_err(|_| {
                    CliError::new(
                        ErrorCode::Validation,
                        format!("invalid agent id `{value}`"),
                        "use an agent id from `agentuna agents list`",
                    )
                })
            })
            .transpose()?;
        crate::runtime::refresh_agent_registry_if_needed(globals)?;
        let paths = resolve_paths(globals)?;
        let _access = CliAccess::read(&paths)?;
        // `--scope` stays the explicit answer. Without it the resolved project decides,
        // because that is the context the command was run in, and User is the fallback
        // for a machine with no project above the working directory. The chosen scope is
        // reported in `data.scope` and as a warning line; it is never silent.
        let (scope, scope_warning) = resolve_scope(globals, &paths);
        let project_root = paths.project_store().and_then(|store| store.parent());
        let loaded = load_scope_snapshots(&paths, project_root).map_err(engine_error)?;
        // Agent load state comes from adapter discovery plus the indexed deployments and native
        // items; store presence alone is never treated as loaded.
        let environment = at_engine::refresh::agent_environment(&paths);
        let observations = observe_agents(&AgentAdapterRegistry::register(), &environment)
            .map_err(|error| {
                CliError::new(
                    ErrorCode::Conflict,
                    format!("agent adapters unavailable: {error}"),
                    "run `agentuna doctor` to check the agent adapters",
                )
            })?;
        let agents = load_agent_loads(&paths, &observations, &loaded.layers, scope)?;
        let locks = load_locks(&paths)?;
        let result = query(EffectiveQuery {
            scope,
            layers: &loaded.layers,
            agent_filter: agent.as_ref(),
            kind_filter: kind,
            agents: &agents,
            locks: locks.as_ref().map(|store| store as &dyn LockStore),
            tombstones: Some(&loaded.tombstones),
        });
        // One struct, one serializer: this is the same `EffectiveQueryView` the Tauri command
        // returns, so `agentuna effective --json` and the Effective screen render identical JSON.
        // `with_scope` only adds which scope answered, so a defaulted request stays legible.
        let view = EffectiveQueryView::new(result, loaded.diagnostics).with_scope(scope);
        let human = format_effective(&view.items, &view.disabled_here);
        let data = serde_json::to_value(&view).map_err(|error| {
            CliError::new(
                ErrorCode::Internal,
                format!("cannot serialize effective configuration: {error}"),
                "this is a bug in agentuna; please report the command you ran",
            )
        })?;
        let warnings = scope_warning.into_iter().collect::<Vec<_>>();
        Ok(CommandOutput::new(data, human).with_warnings(warnings))
    }
}

/// The scope a request without `--scope` answers for, plus the warning that says so.
///
/// A resolved project is the working context behind the invocation, so Project answers
/// there; a machine with no project above the working directory falls back to User.
fn resolve_scope(globals: &GlobalArgs, paths: &at_store::AppPaths) -> (Scope, Option<Warning>) {
    if let Some(scope) = globals.scope() {
        return (scope, None);
    }
    match paths.project_store().and_then(|store| store.parent()) {
        Some(project) => (
            Scope::Project,
            Some(Warning::new(
                "scope_defaulted",
                format!(
                    "no --scope given; answering for the project scope of {}",
                    project.display()
                ),
            )),
        ),
        None => (
            Scope::User,
            Some(Warning::new(
                "scope_defaulted",
                "no --scope given and no project resolved; answering for the user scope",
            )),
        ),
    }
}
/// Reads Global locks from the `locks` table so locked rows carry their id and reason.
///
/// A missing index has no locks; the command still answers, and every row is reported unlocked
/// rather than locked by absence of data.
fn load_locks(paths: &at_store::paths::AppPaths) -> Result<Option<MemoryLockStore>, CliError> {
    if !paths.db_path().exists() {
        return Ok(None);
    }
    let database = ReadOnlyDatabase::open(paths.db_path()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("cannot read the AgenTuna index: {error}"),
            "run `agentuna doctor` to check the index",
        )
    })?;
    at_engine::locks::load_database_locks(database.connection())
        .map(Some)
        .map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("cannot read item locks: {error}"),
                "run `agentuna doctor` to check the index",
            )
        })
}

/// Joins deployments and native discovery for every detected agent with the loaded scopes.
///
/// A missing index cannot describe deployments; an in-memory database keeps the same typed
/// query path and reports every item as not loaded instead of guessing from store presence.
fn load_agent_loads(
    paths: &at_store::paths::AppPaths,
    observations: &[AgentObservation],
    layers: &[ScopeSnapshot],
    scope: Scope,
) -> Result<Vec<AgentLoads>, CliError> {
    let indexed = if paths.db_path().exists() {
        Some(ReadOnlyDatabase::open(paths.db_path()).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("cannot read the AgenTuna index: {error}"),
                "run `agentuna doctor` to check the index",
            )
        })?)
    } else {
        None
    };
    let project_id = indexed.as_ref().and_then(|database| {
        let store = paths.project_store()?;
        let project_root = store.parent().unwrap_or(store);
        at_db::projects::find_project_by_path(database.connection(), project_root)
            .ok()
            .flatten()
            .map(|project| project.id)
    });
    let memory;
    let connection = if let Some(database) = &indexed {
        database.connection()
    } else {
        memory = at_db::Database::open_in_memory().map_err(|error| {
            CliError::new(
                ErrorCode::Internal,
                format!("cannot open an empty agent index: {error}"),
                "this is a bug in agentuna; please report the command you ran",
            )
        })?;
        memory.connection()
    };
    agent_loads(
        connection,
        observations,
        layers,
        scope,
        project_id.as_deref(),
    )
    .map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("cannot read agent deployments: {error}"),
            "run `agentuna doctor` to check the index",
        )
    })
}

fn mutate_lock(
    globals: &GlobalArgs,
    args: &ArgMatches,
    lock: bool,
) -> Result<CommandOutput, CliError> {
    let scope = globals.scope().ok_or_else(|| {
        CliError::new(
            ErrorCode::Validation,
            "lock commands require --scope global",
            "select Global scope",
        )
    })?;
    if scope != at_model::Scope::Global {
        return Err(CliError::new(
            ErrorCode::Locked,
            "locks can only be changed at Global scope",
            "select `--scope global`",
        ));
    }
    let raw_id = args
        .get_one::<String>("item")
        .map(String::as_str)
        .unwrap_or_default();
    let item_id = raw_id.parse::<at_model::ItemId>().map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "invalid item id",
            "use an item id from `agentuna effective --scope global`",
        )
    })?;
    let reason = if lock {
        let reason = args
            .get_one::<String>("reason")
            .map(String::as_str)
            .unwrap_or_default()
            .trim();
        if reason.is_empty() || reason.chars().count() > 200 {
            return Err(CliError::new(
                ErrorCode::Validation,
                "lock reason must contain 1–200 characters",
                "provide a concise reason with `--reason`",
            ));
        }
        Some(reason)
    } else {
        None
    };
    let paths = resolve_paths(globals)?;
    let _access = CliAccess::write(&paths)?;
    if let Some(reason) = reason {
        let record =
            at_engine::locks::lock_in_database(&paths, scope, item_id, reason, "agentuna CLI")
                .map_err(|error| lock_cli_error(error, item_id))?;
        Ok(CommandOutput::new(
            json!({"locked": true, "reason": record.lock.reason, "item_id": item_id}),
            format!("Locked {item_id}: {reason}\n"),
        ))
    } else {
        let record = at_engine::locks::unlock_in_database(&paths, scope, item_id)
            .map_err(|error| lock_cli_error(error, item_id))?;
        Ok(CommandOutput::new(
            json!({"locked": false, "reason": record.lock.reason, "item_id": item_id}),
            format!("Unlocked {item_id}\n"),
        ))
    }
}

fn lock_cli_error(error: at_engine::EngineError, item_id: at_model::ItemId) -> CliError {
    let message = error.to_string();
    let code = if message.contains("does not exist") || message.contains("not locked") {
        ErrorCode::NotFound
    } else if message.contains("Global-only") {
        ErrorCode::Locked
    } else {
        ErrorCode::Conflict
    };
    CliError::new(
        code,
        format!("{item_id}: {message}"),
        "check the selected scope and index, then retry",
    )
}

fn format_effective(
    items: &[at_engine::effective::EffectiveItem],
    disabled: &[at_engine::effective::EffectiveDisabled],
) -> String {
    let mut output = String::from("ITEM  KIND  SCOPE  NAME\n");
    for item in items {
        output.push_str(&format!(
            "{}  {}  {}  {}\n",
            item.item_id
                .map_or_else(|| "-".to_owned(), |id| id.to_string()),
            item.kind,
            item.winner_scope,
            item.name
        ));
    }
    if !disabled.is_empty() {
        output.push_str("\nDISABLED HERE\n");
        for item in disabled {
            output.push_str(&format!("{}  {}  {}\n", item.item_id, item.kind, item.name));
        }
    }
    output
}

fn engine_error(error: impl std::fmt::Display) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("cannot load effective configuration: {error}"),
        "check the selected store paths and run `agentuna doctor`",
    )
}
