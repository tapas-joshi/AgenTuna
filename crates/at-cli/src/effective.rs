//! TA-070 effective configuration query.

use std::str::FromStr;

use at_engine::effective::{AgentLoads, EffectiveQuery, query};
use at_engine::loader::load_scope_snapshots;
use at_model::{AgentId, ItemKind};
use clap::{Arg, ArgMatches, Command};
use serde_json::json;

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;
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
        let scope = globals.scope().ok_or_else(|| {
            CliError::new(
                ErrorCode::Validation,
                "effective requires --scope",
                "choose global, user, project or local",
            )
        })?;
        let kind = matches
            .get_one::<String>("kind")
            .map(|kind| match kind.as_str() {
                "mcp" | "mcp_server" => ItemKind::McpServer,
                "skill" => ItemKind::Skill,
                "rule" => ItemKind::Rule,
                _ => unreachable!("clap validates item kind"),
            });
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
        let paths = resolve_paths(globals)?;
        let _access = CliAccess::read(&paths)?;
        let project_root = paths.project_store().and_then(|store| store.parent());
        let loaded = load_scope_snapshots(&paths, project_root).map_err(engine_error)?;
        // Do not infer that an item is loaded from its presence in a store or deployment row.
        let agents: Vec<AgentLoads> = Vec::new();
        let result = query(EffectiveQuery {
            scope,
            layers: &loaded.layers,
            agent_filter: agent.as_ref(),
            kind_filter: kind,
            agents: &agents,
            locks: None,
            tombstones: Some(&loaded.tombstones),
        });
        let load_diagnostics: Vec<_> = loaded
            .diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "scope": diagnostic.scope.to_string(),
                    "path": diagnostic.path.display().to_string(),
                    "code": diagnostic.code,
                    "message": diagnostic.message,
                })
            })
            .collect();
        let human = format_effective(&result.items, &result.disabled_here);
        let data = json!({
            "items": result.items,
            "disabled_here": result.disabled_here,
            "diagnostics": result.diagnostics,
            "load_diagnostics": load_diagnostics,
        });
        Ok(CommandOutput::new(data, human))
    }
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
