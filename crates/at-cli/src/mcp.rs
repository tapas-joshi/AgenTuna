//! MCP server and Keychain commands for `agentuna mcp`.

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;

use at_fs::{BackupPolicy, ExpectedState};
use at_model::{AgentId, EnvValue, ItemKind, ItemState, McpServerDef, McpTransport, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::StoreManifest;
use at_store::mcp_store::{LoadedMcpStore, McpStore};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::{GlobalArgs, ScopeArg};
use crate::output::CommandOutput;
use crate::registry::CommandGroup;

/// The command group for MCP server configuration and secret references.
pub struct McpGroup;

impl CommandGroup for McpGroup {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn attach(&self, root: Command) -> Command {
        let secret = Command::new("secret")
            .about("Manage Keychain secrets without exposing their values")
            .subcommand_required(true)
            .subcommand(Command::new("list").about("List registered secret names"))
            .subcommand(
                Command::new("set")
                    .about("Read a secret value from stdin and save it to Keychain")
                    .arg(Arg::new("name").required(true).value_name("NAME")),
            )
            .subcommand(
                Command::new("rm")
                    .about("Remove a Keychain secret")
                    .arg(Arg::new("name").required(true).value_name("NAME"))
                    .arg(Arg::new("force").long("force").action(ArgAction::SetTrue)),
            );
        let mcp = Command::new("mcp")
            .about("Manage MCP server definitions")
            .subcommand_required(true)
            .subcommand(Command::new("list").about("List MCP servers"))
            .subcommand(
                Command::new("show")
                    .about("Show a redacted MCP server summary")
                    .arg(Arg::new("id").required(true).value_name("ID")),
            )
            .subcommand(
                Command::new("add").about("Read a server JSON object from stdin and add it"),
            )
            .subcommand(
                Command::new("edit")
                    .about("Read an updated server JSON object from stdin")
                    .arg(Arg::new("id").required(true).value_name("ID")),
            )
            .subcommand(
                Command::new("remove")
                    .about("Remove a server definition")
                    .arg(Arg::new("id").required(true).value_name("ID"))
                    .arg(Arg::new("force").long("force").action(ArgAction::SetTrue)),
            )
            .subcommand(
                Command::new("test")
                    .about("Test an MCP server after explicit approval")
                    .arg(Arg::new("id").required(true).value_name("ID")),
            )
            .subcommand(
                Command::new("enable")
                    .about("Enable an MCP server for one agent in a scope")
                    .arg(Arg::new("id").required(true).value_name("ID"))
                    .arg(
                        Arg::new("agent")
                            .long("agent")
                            .required(true)
                            .value_name("AGENT"),
                    ),
            )
            .subcommand(
                Command::new("disable")
                    .about("Disable an MCP server for one agent in a scope")
                    .arg(Arg::new("id").required(true).value_name("ID"))
                    .arg(
                        Arg::new("agent")
                            .long("agent")
                            .required(true)
                            .value_name("AGENT"),
                    ),
            )
            .subcommand(secret);
        root.subcommand(mcp)
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let (command, args) = matches.subcommand().ok_or_else(|| {
            CliError::new(
                ErrorCode::Usage,
                "missing MCP command",
                "run `agentuna mcp --help`",
            )
        })?;
        match command {
            "secret" => dispatch_secret(globals, args),
            "list" => list_servers(globals),
            "show" => show_server(globals, required(args, "id")?),
            "add" => mutate_server(globals, args, Mutation::Add),
            "edit" => mutate_server(globals, args, Mutation::Edit(required(args, "id")?)),
            "remove" => remove_server(globals, args, required(args, "id")?),
            "test" => test_server(globals, required(args, "id")?),
            "enable" => set_agent_enabled(globals, args, true),
            "disable" => set_agent_enabled(globals, args, false),
            _ => Err(CliError::new(
                ErrorCode::Usage,
                "unsupported MCP command",
                "run `agentuna mcp --help`",
            )),
        }
    }
}

fn dispatch_secret(globals: &GlobalArgs, matches: &ArgMatches) -> Result<CommandOutput, CliError> {
    let (command, args) = matches.subcommand().ok_or_else(|| {
        CliError::new(
            ErrorCode::Usage,
            "missing secret command",
            "run `agentuna mcp secret --help`",
        )
    })?;
    match command {
        "list" => secret_list(globals),
        "set" => secret_set(globals, args),
        "rm" => secret_remove(globals, args),
        _ => Err(CliError::new(
            ErrorCode::Usage,
            "unsupported secret command",
            "run `agentuna mcp secret --help`",
        )),
    }
}

fn list_servers(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    let _access = crate::runtime::CliAccess::for_command(globals, false)?;
    let (scope, layout) = scope_layout(globals)?;
    let loaded = McpStore::load(&layout).map_err(store_error)?;
    let paths = crate::runtime::resolve_paths(globals)?;
    let indexed = paths.db_path().exists();
    let rows = if indexed {
        at_engine::mcp_cli::list_rows(&paths, scope, None)
            .map_err(|error| mcp_index_error(error.to_string()))?
    } else {
        Vec::new()
    };
    // The index is the UI's own list query, so `mcp list` counts what the MCP screen shows.
    // Without an index (a fresh sandbox) fall back to the canonical store so a script still
    // sees the definitions it can act on; `indexed: false` says the states are unknown.
    let manifest = StoreManifest::load(&layout).map_err(store_error)?;
    let servers = if indexed {
        rows.iter()
            .map(|row| {
                let item = row.item_id.and_then(|id| manifest.manifest.item(id));
                let slug = item.map(|item| item.slug.as_str().to_owned());
                let transport = slug
                    .as_deref()
                    .and_then(|slug| loaded.store.get(slug))
                    .map_or("unknown", |definition| definition.transport.kind());
                json!({
                    "id": slug,
                    "item_id": row.item_id.map(|id| id.to_string()),
                    "scope": row.scope.wire_name(),
                    "name": row.display_name,
                    "transport": transport,
                    "native": row.native_only,
                    "locked": row.locked_by.is_some(),
                    "agent_state": agent_state(row),
                    "health": row.latest_health.as_deref().unwrap_or("unknown"),
                })
            })
            .collect::<Vec<_>>()
    } else {
        loaded
            .store
            .servers
            .iter()
            .map(|(slug, definition)| {
                json!({
                    "id": slug,
                    "item_id": null,
                    "scope": scope.wire_name(),
                    "name": definition.display_name,
                    "transport": definition.transport.kind(),
                    "native": false,
                    "locked": false,
                    "agent_state": {},
                    "health": "unknown",
                })
            })
            .collect::<Vec<_>>()
    };
    let count = servers.len();
    let human = if indexed {
        format!("{count} MCP server(s) in {} scope\n", scope.wire_name())
    } else {
        format!(
            "{count} MCP server(s) in {} scope (index missing; per-agent state unknown)\n",
            scope.wire_name()
        )
    };
    Ok(CommandOutput::new(
        json!({"servers": servers, "count": count, "indexed": indexed}),
        human,
    ))
}

/// Per-agent state map for one indexed MCP row.
fn agent_state(row: &at_db::mcp_index::McpListRow) -> serde_json::Map<String, Value> {
    row.agents
        .iter()
        .map(|cell| {
            let state = match &cell.state {
                at_db::mcp_index::McpCellState::Linked => "linked",
                at_db::mcp_index::McpCellState::Off => "off",
                at_db::mcp_index::McpCellState::Locked => "locked",
                at_db::mcp_index::McpCellState::Shadowed => "shadowed",
                at_db::mcp_index::McpCellState::Native { .. } => "native",
                at_db::mcp_index::McpCellState::None => "none",
                at_db::mcp_index::McpCellState::NotInstalled => "not_installed",
                _ => "unknown",
            };
            (
                cell.agent_id.to_string(),
                json!({
                    "state": state,
                    "config_path": cell.config_path,
                }),
            )
        })
        .collect()
}

fn show_server(globals: &GlobalArgs, id: &str) -> Result<CommandOutput, CliError> {
    let _access = crate::runtime::CliAccess::for_command(globals, false)?;
    let (scope, layout) = scope_layout(globals)?;
    let loaded = McpStore::load(&layout).map_err(store_error)?;
    let definition = loaded.store.get(id).ok_or_else(|| not_found(id))?;
    Ok(CommandOutput::new(
        json!({"id":id,"scope":scope.wire_name(),"name":definition.display_name,"transport":definition.transport.kind()}),
        format!(
            "{} ({})\n",
            definition.display_name,
            definition.transport.kind()
        ),
    ))
}
fn test_server(globals: &GlobalArgs, id: &str) -> Result<CommandOutput, CliError> {
    let _access = crate::runtime::CliAccess::for_command(globals, false)?;
    let (scope, layout) = scope_layout(globals)?;
    let loaded = McpStore::load(&layout).map_err(store_error)?;
    let definition = loaded.store.get(id).cloned().ok_or_else(|| not_found(id))?;
    let server = at_engine::mcp_cli::make_server(scope, definition);
    let result = at_engine::mcp_cli::test_server(&server).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "review the server configuration and retry",
        )
    })?;
    Ok(CommandOutput::new(
        json!({
            "id": id,
            "scope": scope.wire_name(),
            "ok": true,
            "tools_count": result.tools_count,
            "latency_ms": result.latency_ms,
            "tools": result.tools
        }),
        format!(
            "Tested {}: {} tools in {} ms\n",
            result.server_name, result.tools_count, result.latency_ms
        ),
    ))
}

fn set_agent_enabled(
    globals: &GlobalArgs,
    args: &ArgMatches,
    enabled: bool,
) -> Result<CommandOutput, CliError> {
    let dry_run = globals.dry_run;
    let access = crate::runtime::CliAccess::for_command(globals, !dry_run)?;
    access.ensure_writable()?;
    let paths = access.paths().clone();
    let id = required(args, "id")?;
    let agent = AgentId::from_str(required(args, "agent")?).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "unknown agent id",
            "use an agent id from `agentuna agents list`",
        )
    })?;
    let scope: Scope = globals.scope.unwrap_or(ScopeArg::User).into();
    let layout = StoreLayout::for_scope(&paths, scope).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "the selected scope has no store",
            "select a valid project for project or local scope",
        )
    })?;
    let slug = Slug::parse(id).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            format!("invalid MCP server id `{id}`"),
            "use the id printed by `agentuna mcp list`",
        )
    })?;
    let manifest = StoreManifest::load(&layout).map_err(store_error)?;
    let item_id = manifest
        .manifest
        .item_by_slug(ItemKind::McpServer, &slug)
        .map(|item| item.id)
        .ok_or_else(|| not_found(id))?;
    let database =
        at_db::Database::open_file(paths.db_path()).map_err(|error| db_error(error.to_string()))?;
    let connection = database.connection();
    if let Some(lock) = at_db::locks::locks_for_item(connection, &item_id)
        .map_err(|error| db_error(error.to_string()))?
        .into_iter()
        .next()
    {
        return Err(locked_error(&lock));
    }
    let installed = at_db::agents::list_agents_on_mac(connection)
        .map_err(|error| db_error(error.to_string()))?
        .into_iter()
        .filter(|agent| agent.presence != at_db::agents::AgentPresence::NotInstalled)
        .map(|agent| agent.id)
        .collect::<Vec<_>>();
    let deployed = at_db::deployments::list_deployments_for_item(connection, &item_id)
        .map_err(|error| db_error(error.to_string()))?
        .into_iter()
        .filter(|cell| cell.scope == scope)
        .map(|cell| cell.agent_id)
        .collect::<Vec<_>>();
    let planned = at_engine::mcp_plan::plan_item(at_engine::mcp_plan::McpItemPlanRequest {
        paths: &paths,
        scope,
        project_id: None,
        item_id,
        action: at_engine::mcp_plan::McpItemAction::SetEnabled {
            agent: agent.clone(),
            enabled,
        },
        installed: &installed,
        deployed: &deployed,
        locked_reason: None,
    })
    .map_err(plan_error)?;
    if dry_run {
        return Ok(CommandOutput::new(
            json!({
                "id": id,
                "scope": scope.wire_name(),
                "agent": agent.to_string(),
                "enabled": enabled,
                "dry_run": true,
                "files": planned
                    .plan
                    .apply_order()
                    .flat_map(|set| set.ops.iter())
                    .filter_map(|op| op.destination())
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>(),
            }),
            format!(
                "Would {} {id} for {agent}\n",
                if enabled { "enable" } else { "disable" }
            ),
        ));
    }
    let roots = at_fs::ApplyRoots::new(authorized_roots(&paths)).map_err(|_| {
        CliError::new(
            ErrorCode::Conflict,
            "authorized configuration roots are unavailable",
            "check that the home, store and project folders exist",
        )
    })?;
    let mut recorder = at_fs::DbOperationRecorder::new(connection, 30);
    let mut context = at_fs::ApplyContext {
        store_root: paths.support_dir(),
        backup_folder: &paths.support_dir().join(at_store::layout::BACKUPS_DIR),
        recorder: &mut recorder,
    };
    let outcome = at_engine::mcp_plan::apply_item_plan(&planned, &mut context, &roots)
        .map_err(apply_error)?;
    let target_path = planned.plan.native.as_ref().and_then(|set| {
        set.ops.iter().find_map(|op| {
            let path = op.destination()?;
            (path.file_name().is_some()).then(|| path.display().to_string())
        })
    });
    let state = if enabled {
        ItemState::Linked
    } else {
        ItemState::Off
    };
    let mut spec = at_db::deployments::DeploymentSpec::new(&item_id, &agent, scope, state)
        .with_mode("native")
        .with_operation(&outcome.operation_id);
    if let Some(path) = target_path.as_deref() {
        spec = spec.with_target_path(path);
    }
    at_db::deployments::upsert_deployment(connection, &spec)
        .map_err(|error| db_error(error.to_string()))?;
    Ok(CommandOutput::new(
        json!({
            "id": id,
            "scope": scope.wire_name(),
            "agent": agent.to_string(),
            "enabled": enabled,
            "files_changed": outcome.completed,
            "operation_id": outcome.operation_id,
        }),
        format!(
            "{} {id} for {agent}: {} file(s) changed\n",
            if enabled { "Enabled" } else { "Disabled" },
            outcome.completed
        ),
    ))
}

/// Maps a planner refusal onto the published error classes.
fn plan_error(error: at_engine::mcp_plan::McpPlanError) -> CliError {
    let code = match &error {
        at_engine::mcp_plan::McpPlanError::Locked(_) => ErrorCode::Locked,
        at_engine::mcp_plan::McpPlanError::MissingStoreEntry(_)
        | at_engine::mcp_plan::McpPlanError::NotMcpServer(_) => ErrorCode::NotFound,
        at_engine::mcp_plan::McpPlanError::AgentNotInstalled(_) => ErrorCode::NotFound,
        at_engine::mcp_plan::McpPlanError::CapabilityRefusal { .. }
        | at_engine::mcp_plan::McpPlanError::UnreadableTarget(_) => ErrorCode::Conflict,
        _ => ErrorCode::Conflict,
    };
    CliError::new(
        code,
        error.to_string(),
        "inspect the server definition and retry",
    )
}

/// Maps a ChangeSet application failure onto the published error classes.
fn apply_error(error: at_fs::ChangeSetApplyError) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("the MCP change could not be applied: {error}"),
        "review the current file state and retry",
    )
}

/// The roots a reviewed MCP write may touch.
fn authorized_roots(paths: &at_store::paths::AppPaths) -> Vec<PathBuf> {
    let mut roots = vec![paths.support_dir().to_path_buf()];
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    roots.push(paths.global_store().to_path_buf());
    roots.push(paths.user_store().to_path_buf());
    if let Some(project) = paths.project_store().and_then(std::path::Path::parent) {
        roots.push(project.to_path_buf());
    }
    roots.retain(|path| path.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

/// The error returned when a lock blocks a write.
fn locked_error(lock: &at_db::shared::LockRow) -> CliError {
    CliError::new(
        ErrorCode::Locked,
        format!(
            "the MCP server is locked at {} scope{}",
            lock.scope.wire_name(),
            lock.reason
                .as_deref()
                .map_or_else(String::new, |reason| format!(": {reason}"))
        ),
        "remove the lock in Effective config, or ask an administrator",
    )
    .with_details(json!({
        "locked": true,
        "scope": lock.scope.wire_name(),
        "reason": lock.reason,
        "item_id": lock.item_id.to_string(),
    }))
}

enum Mutation<'a> {
    Add,
    Edit(&'a str),
}

fn mutate_server(
    globals: &GlobalArgs,
    matches: &ArgMatches,
    mutation: Mutation<'_>,
) -> Result<CommandOutput, CliError> {
    let _access = crate::runtime::CliAccess::for_command(
        globals,
        !(globals.dry_run || matches.get_flag("dry_run")),
    )?;
    let (scope, layout) = scope_layout(globals)?;
    let (slug, definition) = read_definition()?;
    let mut loaded = McpStore::load(&layout).map_err(store_error)?;
    match mutation {
        Mutation::Add => {
            if loaded.store.servers.contains_key(&slug) {
                return Err(CliError::new(
                    ErrorCode::Conflict,
                    "an MCP server with that id already exists",
                    "choose a unique id",
                ));
            }
        }
        Mutation::Edit(id) => {
            if id != slug {
                return Err(CliError::new(
                    ErrorCode::Validation,
                    "the JSON id must match the requested server id",
                    "set the input id to the requested ID",
                ));
            }
            if !loaded.store.servers.contains_key(id) {
                return Err(not_found(id));
            }
        }
    }
    let dry_run = globals.dry_run || matches.get_flag("dry_run");
    if dry_run {
        return Ok(CommandOutput::new(
            json!({"id":slug,"scope":scope.wire_name(),"dry_run":true}),
            format!("Would save MCP server {slug}\n"),
        ));
    }
    loaded.store.set(slug.clone(), definition);
    save_store(&layout, &loaded)?;
    Ok(CommandOutput::new(
        json!({"id":slug,"scope":scope.wire_name()}),
        format!("Saved MCP server {slug}\n"),
    ))
}

fn remove_server(
    globals: &GlobalArgs,
    matches: &ArgMatches,
    id: &str,
) -> Result<CommandOutput, CliError> {
    let _access = crate::runtime::CliAccess::for_command(
        globals,
        !(globals.dry_run || matches.get_flag("dry_run")),
    )?;
    let (scope, layout) = scope_layout(globals)?;
    let mut loaded = McpStore::load(&layout).map_err(store_error)?;
    if !loaded.store.servers.contains_key(id) {
        return Err(not_found(id));
    }
    if globals.dry_run || matches.get_flag("dry_run") {
        return Ok(CommandOutput::new(
            json!({"id":id,"scope":scope.wire_name(),"dry_run":true}),
            format!("Would remove MCP server {id}\n"),
        ));
    }
    if !matches.get_flag("force") && !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "removing an MCP server requires --force or --yes",
            "review the server and retry with --force",
        ));
    }
    loaded.store.remove(id);
    save_store(&layout, &loaded)?;
    Ok(CommandOutput::new(
        json!({"id":id,"scope":scope.wire_name(),"removed":true}),
        format!("Removed MCP server {id}\n"),
    ))
}

fn secret_list(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    let _access = crate::runtime::CliAccess::for_command(globals, false)?;
    let paths = crate::runtime::resolve_paths(globals)?;
    let entries = if globals.read_only {
        let database = crate::runtime::open_index_read_only(&paths)?;
        at_db::secret_refs::inventory(database.connection())
            .map_err(|error| db_error(error.to_string()))?
    } else {
        let database = at_db::Database::open_file(paths.db_path())
            .map_err(|error| db_error(error.to_string()))?;
        at_db::secret_refs::inventory(database.connection())
            .map_err(|error| db_error(error.to_string()))?
    };
    let data = entries
        .into_iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "use_count": entry.use_count,
                "last_read_ok_at": entry.last_read_ok_at
            })
        })
        .collect::<Vec<_>>();
    let count = data.len();
    Ok(CommandOutput::new(
        json!({"secrets": data, "count": count}),
        format!("{count} registered secret(s)\n"),
    ))
}

fn secret_set(globals: &GlobalArgs, matches: &ArgMatches) -> Result<CommandOutput, CliError> {
    if globals.read_only {
        return Err(CliError::new(
            ErrorCode::Locked,
            "secret writes are unavailable in read-only mode",
            "drop --read-only and retry",
        ));
    }
    if io::stdin().is_terminal() {
        return Err(CliError::new(
            ErrorCode::Validation,
            "secret input must be piped through stdin",
            "pipe the secret value to `agentuna mcp secret set <NAME>`",
        ));
    }
    let name = required(matches, "name")?.to_owned();
    if !valid_secret_name(&name) {
        return Err(CliError::new(
            ErrorCode::Validation,
            "invalid secret name",
            "use a non-empty name of at most 255 bytes without control characters",
        ));
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            CliError::new(
                ErrorCode::Validation,
                "could not read secret from stdin",
                "pipe the secret value to stdin",
            )
        })?;
    if bytes.len() > 64 * 1024 {
        return Err(CliError::new(
            ErrorCode::Validation,
            "secret input exceeds 64 KiB",
            "provide a smaller value on stdin",
        ));
    }
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes.pop();
    }
    let value = String::from_utf8(bytes).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "secret input is not valid UTF-8",
            "provide UTF-8 bytes on stdin",
        )
    })?;
    if value.is_empty() {
        return Err(CliError::new(
            ErrorCode::Validation,
            "secret input is empty",
            "pipe a non-empty secret value to stdin",
        ));
    }
    if globals.dry_run {
        return Ok(CommandOutput::new(
            json!({"name":name,"dry_run":true}),
            format!("Would set secret {name}\n"),
        ));
    }
    let _access = crate::runtime::CliAccess::for_command(globals, true)?;
    let paths = crate::runtime::resolve_paths(globals)?;
    at_engine::mcp_cli::set_secret(&paths, &name, value).map_err(engine_secret_error)?;
    Ok(CommandOutput::new(
        json!({"name":name,"saved":true}),
        format!("Saved secret {name}\n"),
    ))
}

fn secret_remove(globals: &GlobalArgs, matches: &ArgMatches) -> Result<CommandOutput, CliError> {
    if globals.read_only {
        return Err(CliError::new(
            ErrorCode::Locked,
            "secret writes are unavailable in read-only mode",
            "drop --read-only and retry",
        ));
    }
    if !matches.get_flag("force") && !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "removing a secret requires --force or --yes",
            "review the secret name and retry with --force",
        ));
    }
    let name = required(matches, "name")?.to_owned();
    if !valid_secret_name(&name) {
        return Err(CliError::new(
            ErrorCode::Validation,
            "invalid secret name",
            "use a non-empty name of at most 255 bytes without control characters",
        ));
    }
    if globals.dry_run {
        return Ok(CommandOutput::new(
            json!({"name":name,"dry_run":true}),
            format!("Would remove secret {name}\n"),
        ));
    }
    let _access = crate::runtime::CliAccess::for_command(globals, true)?;
    let paths = crate::runtime::resolve_paths(globals)?;
    at_engine::mcp_cli::remove_secret(&paths, &name).map_err(engine_secret_error)?;
    Ok(CommandOutput::new(
        json!({"name":name,"removed":true}),
        format!("Removed secret {name}\n"),
    ))
}

fn read_definition() -> Result<(String, McpServerDef), CliError> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            CliError::new(
                ErrorCode::Validation,
                "could not read server JSON from stdin",
                "pipe one server JSON object to stdin",
            )
        })?;
    if bytes.len() > 1024 * 1024 {
        return Err(CliError::new(
            ErrorCode::Validation,
            "server JSON exceeds 1 MiB",
            "provide a smaller definition",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "server input is not valid JSON",
            "pipe a JSON object with id and definition",
        )
    })?;
    let slug = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Validation,
                "server JSON requires string field `id`",
                "provide an id and definition object",
            )
        })?
        .to_owned();
    if !valid_slug(&slug) {
        return Err(CliError::new(
            ErrorCode::Validation,
            "server id must be a lowercase slug",
            "use lowercase letters, numbers, and single hyphens",
        ));
    }
    let definition: McpServerDef =
        serde_json::from_value(value.get("definition").cloned().ok_or_else(|| {
            CliError::new(
                ErrorCode::Validation,
                "server JSON requires `definition`",
                "provide a valid MCP server definition",
            )
        })?)
        .map_err(|_| {
            CliError::new(
                ErrorCode::Validation,
                "server definition is invalid",
                "check the MCP transport and fields",
            )
        })?;
    if has_plaintext_credential(&definition.transport) {
        return Err(CliError::new(
            ErrorCode::Validation,
            "credential-like fields must use a Keychain secret reference",
            "store the value with `agentuna mcp secret set` and use its secret reference",
        ));
    }
    Ok((slug, definition))
}

fn has_plaintext_credential(transport: &McpTransport) -> bool {
    let entries = match transport {
        McpTransport::Stdio { env, .. } => env,
        McpTransport::Http { headers, .. } => headers,
    };
    entries.iter().any(|(name, value)| {
        let sensitive = name.to_ascii_lowercase();
        let sensitive_name = [
            "token",
            "secret",
            "password",
            "credential",
            "authorization",
            "api_key",
            "apikey",
        ]
        .iter()
        .any(|marker| sensitive.contains(marker));
        match value {
            EnvValue::Secret(_) => false,
            EnvValue::Literal(value) => sensitive_name || looks_like_credential(value),
        }
    })
}

fn looks_like_credential(value: &str) -> bool {
    ["sk-", "ghp_", "github_pat_", "xoxb-", "xoxp-", "glpat-"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn save_store(layout: &StoreLayout, loaded: &LoadedMcpStore) -> Result<(), CliError> {
    let expected = if loaded.absent {
        ExpectedState::Absent
    } else {
        ExpectedState::Hash(loaded.hash.to_string())
    };
    loaded
        .store
        .save(layout, expected, BackupPolicy::None)
        .map_err(store_error)?;
    Ok(())
}

fn scope_layout(globals: &GlobalArgs) -> Result<(Scope, StoreLayout), CliError> {
    let scope: Scope = globals.scope.unwrap_or(ScopeArg::User).into();
    let paths = crate::runtime::resolve_paths(globals)?;
    let layout = StoreLayout::for_scope(&paths, scope).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "the selected scope has no store",
            "select a valid project for project or local scope",
        )
    })?;
    Ok((scope, layout))
}

fn required<'a>(matches: &'a ArgMatches, key: &str) -> Result<&'a str, CliError> {
    matches
        .get_one::<String>(key)
        .map(String::as_str)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Usage,
                format!("missing {key}"),
                "run `agentuna mcp --help`",
            )
        })
}
fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}
fn valid_secret_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control)
}

fn not_found(id: &str) -> CliError {
    CliError::new(
        ErrorCode::NotFound,
        format!("MCP server `{id}` was not found"),
        "run `agentuna mcp list` to see available servers",
    )
}
fn store_error(error: impl std::fmt::Display) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("MCP store operation failed: {error}"),
        "check the store and retry",
    )
}

fn mcp_index_error(message: String) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("MCP state index is unavailable: {message}"),
        "run `agentuna doctor` to inspect the index",
    )
}
fn engine_secret_error(error: at_engine::mcp_cli::McpCliError) -> CliError {
    let code = match &error {
        at_engine::mcp_cli::McpCliError::Runtime => ErrorCode::Internal,
        at_engine::mcp_cli::McpCliError::InvalidLaunch { .. } => ErrorCode::Validation,
        at_engine::mcp_cli::McpCliError::ConnectionFailed { .. } => ErrorCode::Conflict,
        at_engine::mcp_cli::McpCliError::Secret { .. }
        | at_engine::mcp_cli::McpCliError::Database { .. } => ErrorCode::Conflict,
        _ => ErrorCode::Conflict,
    };
    CliError::new(code, error.to_string(), "check the configuration and retry")
}
fn db_error(message: String) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("secret inventory is unavailable: {message}"),
        "initialize or repair the AgenTuna database",
    )
}
