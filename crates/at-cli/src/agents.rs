//! TA-070 agent inventory commands.

use at_db::{ReadOnlyDatabase, agents as agent_repo};
use at_engine::health;
use clap::{Arg, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, resolve_paths};

/// `agentuna agents list|inspect`.
pub struct AgentsGroup;

impl CommandGroup for AgentsGroup {
    fn name(&self) -> &'static str {
        "agents"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("agents")
                .about("List detected coding agents or inspect one agent")
                .subcommand_required(true)
                .subcommand(Command::new("list").about("List known agents"))
                .subcommand(
                    Command::new("inspect")
                        .about("Inspect one known agent")
                        .arg(Arg::new("id").required(true).value_name("ID")),
                ),
        )
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let (subcommand, args) = matches.subcommand().ok_or_else(|| {
            CliError::new(
                ErrorCode::Internal,
                "agents subcommand was not parsed",
                "run `agentuna agents --help`",
            )
        })?;
        let id = if subcommand == "inspect" {
            Some(
                args.get_one::<String>("id")
                    .expect("required by clap")
                    .as_str(),
            )
        } else {
            None
        };
        crate::runtime::refresh_agent_registry_if_needed(globals)?;
        let paths = resolve_paths(globals)?;
        let _access = CliAccess::read(&paths)?;
        let index_path = paths.db_path();
        if !index_path.exists() {
            if id.is_some() {
                return Err(CliError::new(
                    ErrorCode::NotFound,
                    format!("agent `{}` is not known", id.unwrap_or_default()),
                    "run `agentuna agents list` to see known agents",
                ));
            }
            return Ok(CommandOutput::new(
                Value::Array(Vec::new()),
                "No agents found.\n",
            ));
        }
        let database = ReadOnlyDatabase::open(index_path).map_err(db_error)?;
        let home = paths.home_dir().unwrap_or(paths.user_store());
        let agents = agent_repo::list_agents_on_mac(database.connection()).map_err(db_error)?;
        let selected: Vec<_> = match id {
            Some(id) => agents
                .into_iter()
                .filter(|agent| agent.id.to_string() == id)
                .collect(),
            None => agents,
        };
        if let Some(id) = id
            && selected.is_empty()
        {
            return Err(CliError::new(
                ErrorCode::NotFound,
                format!("agent `{id}` is not known"),
                "run `agentuna agents list` to see known agents",
            ));
        }
        let mut rows = Vec::with_capacity(selected.len());
        for agent in selected {
            let health = health::line_with_connection(database.connection(), &agent.id, home)
                .map_err(|error| {
                    CliError::new(
                        ErrorCode::Conflict,
                        format!("cannot compute health for agent `{}`: {error}", agent.id),
                        "run `agentuna doctor` to check the index",
                    )
                })?;
            rows.push(json!({
                "id": agent.id.to_string(),
                "installed": agent.presence == at_db::agents::AgentPresence::Installed,
                "version": agent.version,
                "primary_path": agent.primary_path.as_ref().map(|path| path.display().to_string()),
                "health_line": health.text,
                "copy_fallback": health.copy_fallback,
            }));
        }
        let human = rows.iter().map(render_agent).collect::<Vec<_>>().join("\n");
        Ok(CommandOutput::new(
            Value::Array(rows),
            if human.is_empty() {
                "No agents found.\n".to_owned()
            } else {
                format!("{human}\n")
            },
        ))
    }
}

fn render_agent(row: &Value) -> String {
    format!(
        "{:<18} {:<9} {:<14} {}",
        row["id"].as_str().unwrap_or(""),
        if row["installed"] == true {
            "installed"
        } else {
            "not found"
        },
        row["version"].as_str().unwrap_or("—"),
        row["health_line"].as_str().unwrap_or("")
    )
}

fn db_error(error: impl std::fmt::Display) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("cannot read agent index: {error}"),
        "run `agentuna doctor` to check the index",
    )
}
