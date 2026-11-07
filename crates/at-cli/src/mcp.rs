//! MCP server and Keychain commands for `agentuna mcp`.

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;

use at_engine::mcp_plan::{
    McpCopyResolution, McpIntent, McpItemAction, McpItemPlan, McpItemPlanRequest, McpPlanRequest,
};
use at_engine::skill_cli::{ManifestItemInput, sync_manifest_item};
use at_model::{AgentId, EnvValue, ItemId, ItemKind, ItemState, McpServerDef, McpTransport, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::StoreManifest;
use at_store::mcp_store::McpStore;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::{GlobalArgs, ScopeArg};
use crate::output::{CommandOutput, Warning};
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
            .subcommand(
                Command::new("copy")
                    .about("Copy a server definition into another scope")
                    .arg(Arg::new("name").required(true).value_name("NAME"))
                    .arg(
                        Arg::new("to")
                            .long("to")
                            .required(true)
                            .value_name("SCOPE")
                            .value_parser(clap::value_parser!(ScopeArg)),
                    )
                    .arg(
                        Arg::new("on_collision")
                            .long("on-collision")
                            .value_name("ACTION")
                            .value_parser(["replace", "rename"]),
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
            "copy" => copy_server(globals, args),
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
    crate::runtime::refresh_agent_registry_if_needed(globals)?;
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
    let mut stale_rows = 0usize;
    let servers = if indexed {
        rows.iter()
            .filter_map(|row| {
                if row.native_only {
                    return Some(json!({
                        "id": row.display_name,
                        "item_id": null,
                        "scope": row.scope.wire_name(),
                        "name": row.display_name,
                        "transport": row
                            .transport_subtitle
                            .clone()
                            .unwrap_or_else(|| "unknown".to_owned()),
                        "native": true,
                        "locked": row.locked_by.is_some(),
                        "agent_state": agent_state(row),
                        "health": row.latest_health.as_deref().unwrap_or("unknown"),
                    }));
                }
                // The manifest owns the slug for an indexed row; a row written before its
                // manifest record still resolves through the canonical entry the store holds.
                let slug = row
                    .item_id
                    .and_then(|id| manifest.manifest.item(id))
                    .map(|item| item.slug.as_str().to_owned())
                    .or_else(|| {
                        loaded
                            .store
                            .servers
                            .keys()
                            .find(|slug| slug.eq_ignore_ascii_case(&row.display_name))
                            .cloned()
                    });
                let Some(definition) = slug.as_deref().and_then(|slug| loaded.store.get(slug))
                else {
                    // The canonical store is the source of truth: a row without a definition
                    // there is a stale index entry, not a server the user can act on.
                    stale_rows += 1;
                    return None;
                };
                Some(json!({
                    "id": slug,
                    "item_id": row.item_id.map(|id| id.to_string()),
                    "scope": row.scope.wire_name(),
                    "name": row.display_name,
                    "transport": definition.transport.kind(),
                    "native": row.native_only,
                    "locked": row.locked_by.is_some(),
                    "agent_state": agent_state(row),
                    "health": row.latest_health.as_deref().unwrap_or("unknown"),
                }))
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
    let mut warnings = Vec::new();
    if stale_rows > 0 {
        warnings.push(Warning::new(
            "mcp_index_stale",
            format!(
                "{stale_rows} indexed MCP row(s) have no item record in store.json; run `agentuna doctor` to repair the index"
            ),
        ));
    }
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
    )
    .with_warnings(warnings))
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
    let item = manifest
        .manifest
        .item_by_slug(ItemKind::McpServer, &slug)
        .cloned()
        .ok_or_else(|| not_found(id))?;
    let database = open_index_or_create(&paths)?;
    let connection = database.connection();
    // The manifest owns the stable id. Recording it in the index before planning means the
    // deployment row the plan writes cannot fail the `items` foreign key after the native file
    // has already changed.
    let project_id = crate::skills::project_id_for(&database, &paths, scope)?;
    let item_id = sync_manifest_item(
        connection,
        &ManifestItemInput {
            scope,
            project_id: project_id.as_deref(),
            item: &item,
        },
    )
    .map_err(|error| index_error(error.to_string()))?;
    if let Some(lock) = at_db::locks::locks_for_item(connection, &item_id)
        .map_err(|error| index_error(error.to_string()))?
        .into_iter()
        .next()
    {
        return Err(locked_error(&lock));
    }
    let (installed, deployed) = deployment_context(&database, &item_id, scope)?;
    let planned = at_engine::mcp_plan::plan_item(at_engine::mcp_plan::McpItemPlanRequest {
        paths: &paths,
        scope,
        project_id: project_id.clone(),
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
    let warnings = planned
        .plan
        .warnings
        .iter()
        .map(|warning| crate::output::Warning::new("mcp_plan", warning.clone()))
        .collect::<Vec<_>>();
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
        )
        .with_warnings(warnings));
    }
    let roots = at_fs::ApplyRoots::new(authorized_roots(&paths)).map_err(|_| {
        CliError::new(
            ErrorCode::Conflict,
            "authorized configuration roots are unavailable",
            "check that the home, store and project folders exist",
        )
    })?;
    let mut recorder =
        at_fs::DbOperationRecorder::new_with_actor(connection, 30, at_db::ops::OperationActor::Cli);
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
    let layout = StoreLayout::for_scope(&paths, scope).ok();
    let store_file = layout.as_ref().map(|l| l.mcp_file());
    let store_hash = store_file
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| at_fs::sha256_hex(&b));
    let target_hash = target_path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| at_fs::sha256_hex(&b));
    let baseline_hash = target_hash.clone();
    let hashes = at_db::deployments::DeploymentHashes::new(
        store_hash.as_deref(),
        target_hash.as_deref(),
        baseline_hash.as_deref(),
    );
    let mut spec = at_db::deployments::DeploymentSpec::applied(
        &item_id,
        &agent,
        scope,
        state,
        Some("native"),
        target_path.as_deref(),
        hashes,
    )
    .with_operation(&outcome.operation_id);
    if let Some(project) = project_id.as_deref() {
        spec = spec.with_project(project);
    }
    at_db::deployments::upsert_deployment(connection, &spec)
        .map_err(|error| index_error(error.to_string()))?;
    // The native file now holds what the plan rendered; record its hash so the next drift scan
    // compares against the file we just wrote instead of the pre-command bytes.
    sync_native_baselines(&database, &paths, scope, &planned)?;
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
    )
    .with_warnings(warnings))
}

/// Maps a planner refusal onto the published error classes.
fn plan_error(error: at_engine::mcp_plan::McpPlanError) -> CliError {
    let code = match &error {
        at_engine::mcp_plan::McpPlanError::Locked(_)
        | at_engine::mcp_plan::McpPlanError::PermissionDenied => ErrorCode::Locked,
        at_engine::mcp_plan::McpPlanError::MissingStoreEntry(_)
        | at_engine::mcp_plan::McpPlanError::NotMcpServer(_) => ErrorCode::NotFound,
        at_engine::mcp_plan::McpPlanError::AgentNotInstalled(_) => ErrorCode::NotFound,
        at_engine::mcp_plan::McpPlanError::SameScope
        | at_engine::mcp_plan::McpPlanError::MissingProject { .. }
        | at_engine::mcp_plan::McpPlanError::DestinationStoreMissing { .. }
        | at_engine::mcp_plan::McpPlanError::InvalidItem(_) => ErrorCode::Validation,
        _ => ErrorCode::Conflict,
    };
    let hint = match &error {
        at_engine::mcp_plan::McpPlanError::SameScope => {
            "pick a different destination scope with --to"
        }
        at_engine::mcp_plan::McpPlanError::MissingProject { .. }
        | at_engine::mcp_plan::McpPlanError::DestinationStoreMissing { .. } => {
            "pass --project with a registered project, or create the destination store first"
        }
        at_engine::mcp_plan::McpPlanError::PermissionDenied => {
            "run the command with the permission the Global store requires"
        }
        at_engine::mcp_plan::McpPlanError::Collision { .. } => {
            "repeat with --on-collision replace or --on-collision rename"
        }
        at_engine::mcp_plan::McpPlanError::NoTargetToReplace(_) => {
            "choose rename instead of replace"
        }
        at_engine::mcp_plan::McpPlanError::UnmanagedNative { .. } => {
            "adopt or move aside the agent's existing entry first"
        }
        at_engine::mcp_plan::McpPlanError::NoSupportedAgent { .. } => {
            "no installed agent can load this definition at the destination scope"
        }
        _ => "inspect the server definition and retry",
    };
    CliError::new(code, error.to_string(), hint)
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
    let dry_run = globals.dry_run || matches.get_flag("dry_run");
    let access = crate::runtime::CliAccess::for_command(globals, !dry_run)?;
    if !dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let (scope, layout) = scope_layout(globals)?;
    let (slug, definition) = read_definition()?;
    let loaded = McpStore::load(&layout).map_err(store_error)?;
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
    // The canonical store change is planned by the same engine path the MCP screen uses: the
    // reviewed ChangeSet carries `mcp.json` and the `store.json` record that owns its identity.
    let registry = at_adapters::adapter::AgentAdapterRegistry::register();
    let environment = at_engine::refresh::agent_environment(&paths);
    let intent = match mutation {
        Mutation::Add => McpIntent::Add {
            slug: slug.clone(),
            definition: definition.clone(),
        },
        Mutation::Edit(_) => McpIntent::Edit {
            slug: slug.clone(),
            definition: definition.clone(),
        },
    };
    let planned = at_engine::mcp_plan::plan(McpPlanRequest {
        registry: &registry,
        env: &environment,
        intent: &intent,
        targets: &[],
        definition: &definition,
        scope,
        project_id: None,
        locked_reason: None,
        store_layout: Some(&layout),
        target_store_layout: None,
    })
    .map_err(plan_error)?;
    let mut change_set = planned.store.ok_or_else(|| {
        CliError::new(
            ErrorCode::Internal,
            "the MCP plan carries no canonical store change",
            "this is a bug in agentuna; please report the command you ran",
        )
    })?;
    // The first server in a scope creates the store in the same reviewed ChangeSet. A dry run
    // lists the creation, the real run journals it, and the Global scope still refuses without
    // its explicit write consent.
    let store_init = at_store::store_init::plan_store_init(&layout, &mut change_set)
        .map_err(crate::error::store_init_error)?;
    let files = change_set
        .ops
        .iter()
        .filter_map(|op| op.destination())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let created_store = store_init
        .as_ref()
        .map(|init| init.root.display().to_string());
    if dry_run {
        return Ok(CommandOutput::new(
            json!({
                "id": slug,
                "scope": scope.wire_name(),
                "dry_run": true,
                "create_store": created_store,
                "files": files,
            }),
            match &created_store {
                Some(path) => format!(
                    "Would create the {} store at {path} and save MCP server {slug}\n",
                    scope.wire_name()
                ),
                None => format!("Would save MCP server {slug}\n"),
            },
        ));
    }
    let database = open_index_or_create(&paths)?;
    let outcome = apply_change_set(&paths, &database, &change_set)?;
    let item = index_manifest_item(&paths, &layout, scope, ItemKind::McpServer, &slug)?;
    Ok(CommandOutput::new(
        json!({
            "id": slug,
            "scope": scope.wire_name(),
            "item_id": item.to_string(),
            "operation_id": outcome.operation_id,
            "files_changed": outcome.completed,
        }),
        format!("Saved MCP server {slug}\n"),
    ))
}

fn remove_server(
    globals: &GlobalArgs,
    matches: &ArgMatches,
    id: &str,
) -> Result<CommandOutput, CliError> {
    let dry_run = globals.dry_run || matches.get_flag("dry_run");
    let access = crate::runtime::CliAccess::for_command(globals, !dry_run)?;
    if !dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let (scope, layout) = scope_layout(globals)?;
    let loaded = McpStore::load(&layout).map_err(store_error)?;
    let slug = parse_slug(id)?;
    if loaded.store.get(slug.as_str()).is_none() {
        return Err(not_found(id));
    }
    if !dry_run && !matches.get_flag("force") && !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "removing an MCP server requires --force or --yes",
            "review the server and retry with --force",
        ));
    }
    let manifest = StoreManifest::load(&layout).map_err(store_error)?;
    let item = manifest
        .manifest
        .item_by_slug(ItemKind::McpServer, &slug)
        .ok_or_else(|| not_found(id))?;
    let database = open_index_or_create(&paths)?;
    let connection = database.connection();
    // Make the index agree with the manifest before the plan reads deployments; otherwise the
    // native removal is planned against identities the store no longer owns.
    let project_id = crate::skills::project_id_for(&database, &paths, scope)?;
    let item_id = sync_manifest_item(
        connection,
        &ManifestItemInput {
            scope,
            project_id: project_id.as_deref(),
            item,
        },
    )
    .map_err(|error| index_error(error.to_string()))?;
    let (installed, deployed) = deployment_context(&database, &item_id, scope)?;
    let planned = at_engine::mcp_plan::plan_item(McpItemPlanRequest {
        paths: &paths,
        scope,
        project_id,
        item_id,
        action: McpItemAction::Delete,
        installed: &installed,
        deployed: &deployed,
        locked_reason: None,
    })
    .map_err(plan_error)?;
    let files = planned
        .plan
        .apply_order()
        .flat_map(|set| set.ops.iter())
        .filter_map(|op| op.destination())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if dry_run {
        return Ok(CommandOutput::new(
            json!({"id":id,"scope":scope.wire_name(),"dry_run":true,"files":files}),
            format!("Would remove MCP server {id}\n"),
        ));
    }
    let roots = apply_roots(&paths)?;
    let mut recorder =
        at_fs::DbOperationRecorder::new_with_actor(connection, 30, at_db::ops::OperationActor::Cli);
    let mut context = at_fs::ApplyContext {
        store_root: paths.support_dir(),
        backup_folder: paths.backup_dir(),
        recorder: &mut recorder,
    };
    let outcome = at_engine::mcp_plan::apply_item_plan(&planned, &mut context, &roots)
        .map_err(apply_error)?;
    sync_native_baselines(&database, &paths, scope, &planned)?;
    // The canonical item is gone, so its index row and its deployments go with it; deployments
    // have `ON DELETE CASCADE`, and a surviving row is what made `mcp list` show a removed
    // server as `transport: unknown`.
    at_db::items::delete_item(connection, &item_id)
        .map_err(|error| index_error(error.to_string()))?;
    Ok(CommandOutput::new(
        json!({
            "id": id,
            "scope": scope.wire_name(),
            "removed": true,
            "operation_id": outcome.operation_id,
            "files_changed": outcome.completed,
        }),
        format!("Removed MCP server {id}\n"),
    ))
}

/// Copies one canonical server definition into another scope as a new item.
///
/// The copy is planned through the same engine path the Effective screen uses, so the reviewed
/// file list, the per-agent refusals and the destination item identity are the same values the
/// GUI dialog shows.
fn copy_server(globals: &GlobalArgs, matches: &ArgMatches) -> Result<CommandOutput, CliError> {
    let dry_run = globals.dry_run;
    let access = crate::runtime::CliAccess::for_command(globals, !dry_run)?;
    if !dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let id = required(matches, "name")?;
    let target_scope: Scope = matches
        .get_one::<ScopeArg>("to")
        .copied()
        .map(Scope::from)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Usage,
                "missing --to",
                "pass --to global, user, project or local",
            )
        })?;
    if !dry_run && !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            format!(
                "copying {id} into {} scope requires --yes",
                target_scope.wire_name()
            ),
            "review the plan with --dry-run, then repeat with --yes",
        ));
    }
    let (scope, layout) = scope_layout(globals)?;
    let slug = parse_slug(id)?;
    let loaded = McpStore::load(&layout).map_err(store_error)?;
    if loaded.store.get(slug.as_str()).is_none() {
        return Err(not_found(id));
    }
    let manifest = StoreManifest::load(&layout).map_err(store_error)?;
    let item = manifest
        .manifest
        .item_by_slug(ItemKind::McpServer, &slug)
        .ok_or_else(|| not_found(id))?;
    let database = open_index_or_create(&paths)?;
    let connection = database.connection();
    // The manifest owns the stable id. Recording it before planning means the deployment rows
    // the copy writes cannot fail the `items` foreign key after the native files changed.
    let project_id = crate::skills::project_id_for(&database, &paths, scope)?;
    let item_id = sync_manifest_item(
        connection,
        &ManifestItemInput {
            scope,
            project_id: project_id.as_deref(),
            item,
        },
    )
    .map_err(|error| index_error(error.to_string()))?;
    if let Some(lock) = at_db::locks::locks_for_item(connection, &item_id)
        .map_err(|error| index_error(error.to_string()))?
        .into_iter()
        .next()
    {
        return Err(locked_error(&lock));
    }
    let (installed, _deployed) = deployment_context(&database, &item_id, scope)?;
    let target_project_id = if matches!(target_scope, Scope::Project | Scope::Local) {
        crate::skills::project_id_for(&database, &paths, target_scope)?
    } else {
        None
    };
    let on_collision = match matches
        .get_one::<String>("on_collision")
        .map(String::as_str)
    {
        None => None,
        Some("replace") => Some(McpCopyResolution::Replace),
        Some("rename") => Some(McpCopyResolution::Rename(unique_copy_slug(
            &paths,
            target_scope,
            &slug,
        )?)),
        Some(other) => {
            return Err(CliError::new(
                ErrorCode::Usage,
                format!("unsupported --on-collision value `{other}`"),
                "use `replace` or `rename`",
            ));
        }
    };
    let planned = at_engine::mcp_plan::plan_item(McpItemPlanRequest {
        paths: &paths,
        scope,
        project_id,
        item_id,
        action: McpItemAction::CopyToScope {
            target_scope,
            project_id: target_project_id.clone(),
            on_collision,
        },
        installed: &installed,
        deployed: &[],
        locked_reason: None,
    })
    .map_err(plan_error)?;
    let copy = planned.copy.clone().ok_or_else(|| {
        CliError::new(
            ErrorCode::Internal,
            "the copy plan carries no destination identity",
            "this is a bug in agentuna; please report the command you ran",
        )
    })?;
    let warnings = planned
        .plan
        .warnings
        .iter()
        .map(|warning| Warning::new("mcp_plan", warning.clone()))
        .chain(
            copy.refused_agents
                .iter()
                .map(|refusal| Warning::new("mcp_refused_agent", refusal.reason.clone())),
        )
        .collect::<Vec<_>>();
    let files = planned
        .plan
        .apply_order()
        .flat_map(|set| set.ops.iter())
        .filter_map(|op| op.destination())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if dry_run {
        return Ok(CommandOutput::new(
            json!({
                "id": id,
                "scope": scope.wire_name(),
                "target_scope": target_scope.wire_name(),
                "target_slug": copy.target_slug,
                "target_item_id": copy.target_item_id.to_string(),
                "replaces_existing": copy.replaced,
                "dry_run": true,
                "files": files,
                "agents": copy.agents.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "refused_agents": copy
                    .refused_agents
                    .iter()
                    .map(|refusal| json!({"agent": refusal.agent.to_string(), "reason": refusal.reason}))
                    .collect::<Vec<_>>(),
            }),
            format!(
                "Would copy {id} into {} scope as {}\n",
                target_scope.wire_name(),
                copy.target_slug
            ),
        )
        .with_warnings(warnings));
    }
    let merged = planned.plan.merged().map_err(|error| {
        CliError::new(
            ErrorCode::Internal,
            format!("the copy plan is invalid: {error}"),
            "this is a bug in agentuna; please report the command you ran",
        )
    })?;
    let outcome = apply_change_set(&paths, &database, &merged)?;
    let target_layout = StoreLayout::for_scope(&paths, target_scope).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "the destination scope has no store",
            "select a valid project for project or local scope",
        )
    })?;
    let target_item_id = index_manifest_item(
        &paths,
        &target_layout,
        target_scope,
        ItemKind::McpServer,
        &copy.target_slug,
    )?;
    // The native documents now hold what the plan rendered; record their hashes so the next
    // drift scan compares against the file this command wrote.
    let store_file = target_layout.mcp_file();
    let store_hash = std::fs::read(&store_file)
        .ok()
        .map(|bytes| at_fs::sha256_hex(&bytes));
    let native_paths = planned
        .plan
        .native
        .as_ref()
        .map(|set| {
            set.ops
                .iter()
                .filter_map(|op| op.destination().map(std::path::Path::to_path_buf))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (agent, path) in copy.agents.iter().zip(native_paths.iter()) {
        let target_hash = std::fs::read(path)
            .ok()
            .map(|bytes| at_fs::sha256_hex(&bytes));
        let hashes = at_db::deployments::DeploymentHashes::new(
            store_hash.as_deref(),
            target_hash.as_deref(),
            target_hash.as_deref(),
        );
        let path_text = path.to_string_lossy().into_owned();
        let mut spec = at_db::deployments::DeploymentSpec::applied(
            &target_item_id,
            agent,
            target_scope,
            ItemState::Linked,
            Some("native"),
            Some(&path_text),
            hashes,
        )
        .with_operation(&outcome.operation_id);
        if let Some(project) = target_project_id.as_deref() {
            spec = spec.with_project(project);
        }
        at_db::deployments::upsert_deployment(connection, &spec)
            .map_err(|error| index_error(error.to_string()))?;
    }
    for path in &native_paths {
        at_engine::drift::sync_file_baseline(connection, target_scope, path, &store_file)
            .map_err(|error| index_error(error.to_string()))?;
    }
    Ok(CommandOutput::new(
        json!({
            "id": id,
            "scope": scope.wire_name(),
            "target_scope": target_scope.wire_name(),
            "target_slug": copy.target_slug,
            "target_item_id": target_item_id.to_string(),
            "replaces_existing": copy.replaced,
            "agents": copy.agents.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "refused_agents": copy
                .refused_agents
                .iter()
                .map(|refusal| json!({"agent": refusal.agent.to_string(), "reason": refusal.reason}))
                .collect::<Vec<_>>(),
            "operation_id": outcome.operation_id,
            "files_changed": outcome.completed,
        }),
        format!(
            "Copied {id} into {} scope as {}: {} file(s) changed\n",
            target_scope.wire_name(),
            copy.target_slug,
            outcome.completed
        ),
    )
    .with_warnings(warnings))
}

/// A lowercase slug the destination store does not use yet.
fn unique_copy_slug(
    paths: &at_store::paths::AppPaths,
    target_scope: Scope,
    slug: &Slug,
) -> Result<String, CliError> {
    let layout = StoreLayout::for_scope(paths, target_scope).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "the destination scope has no store",
            "create the destination store first, then retry",
        )
    })?;
    let store = McpStore::load(&layout).map_err(store_error)?;
    let unique = at_store::layout::unique_slug(slug.as_str(), |candidate| {
        store.store.get(candidate).is_some()
    })
    .map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("cannot derive a free copy name: {error}"),
            "rename the server first, or pass --on-collision replace",
        )
    })?;
    Ok(unique.as_str().to_owned())
}

/// Opens (and on first use creates) the writable index a mutating command needs.
fn open_index_or_create(paths: &at_store::paths::AppPaths) -> Result<at_db::Database, CliError> {
    at_db::Database::open_file(paths.db_path()).map_err(|error| index_error(error.to_string()))
}

/// Applies one reviewed ChangeSet under the exclusive access the caller already holds.
fn apply_change_set(
    paths: &at_store::paths::AppPaths,
    database: &at_db::Database,
    set: &at_model::ChangeSet,
) -> Result<at_fs::ApplyOutcome, CliError> {
    let roots = apply_roots(paths)?;
    let mut recorder = at_fs::DbOperationRecorder::new_with_actor(
        database.connection(),
        30,
        at_db::ops::OperationActor::Cli,
    );
    let mut context = at_fs::ApplyContext {
        store_root: paths.support_dir(),
        backup_folder: paths.backup_dir(),
        recorder: &mut recorder,
    };
    at_fs::apply_with_roots(set, &mut context, &roots).map_err(apply_error)
}

fn apply_roots(paths: &at_store::paths::AppPaths) -> Result<at_fs::ApplyRoots, CliError> {
    at_fs::ApplyRoots::new(authorized_roots(paths)).map_err(|_| {
        CliError::new(
            ErrorCode::Conflict,
            "authorized configuration roots are unavailable",
            "check that the home, store and project folders exist",
        )
    })
}

/// Records the manifest identity of `slug` in the index and returns it.
fn index_manifest_item(
    paths: &at_store::paths::AppPaths,
    layout: &StoreLayout,
    scope: Scope,
    kind: ItemKind,
    slug: &str,
) -> Result<ItemId, CliError> {
    let slug = parse_slug(slug)?;
    let manifest = StoreManifest::load(layout).map_err(store_error)?;
    let item = manifest
        .manifest
        .item_by_slug(kind, &slug)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Conflict,
                "the saved server has no record in store.json",
                "run `agentuna doctor` to rebuild the store manifest",
            )
        })?
        .clone();
    let database = open_index_or_create(paths)?;
    let project_id = crate::skills::project_id_for(&database, paths, scope)?;
    sync_manifest_item(
        database.connection(),
        &ManifestItemInput {
            scope,
            project_id: project_id.as_deref(),
            item: &item,
        },
    )
    .map_err(|error| index_error(error.to_string()))
}

/// Records the observed hashes for every native file one MCP plan wrote.
fn sync_native_baselines(
    database: &at_db::Database,
    paths: &at_store::paths::AppPaths,
    scope: Scope,
    planned: &McpItemPlan,
) -> Result<(), CliError> {
    let layout = StoreLayout::for_scope(paths, scope).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "the selected scope has no store",
            "select a valid project for project or local scope",
        )
    })?;
    let store_file = layout.mcp_file();
    let native_paths = planned
        .plan
        .native
        .iter()
        .flat_map(|set| set.ops.iter())
        .filter_map(|op| op.destination())
        .collect::<Vec<_>>();
    for path in native_paths {
        at_engine::drift::sync_file_baseline(database.connection(), scope, path, &store_file)
            .map_err(|error| index_error(error.to_string()))?;
    }
    Ok(())
}

/// Installed agents and the agents with a deployment for `item_id` in `scope`.
fn deployment_context(
    database: &at_db::Database,
    item_id: &ItemId,
    scope: Scope,
) -> Result<(Vec<AgentId>, Vec<AgentId>), CliError> {
    let connection = database.connection();
    let installed = at_db::agents::list_agents_on_mac(connection)
        .map_err(|error| index_error(error.to_string()))?
        .into_iter()
        .filter(|agent| agent.presence != at_db::agents::AgentPresence::NotInstalled)
        .map(|agent| agent.id)
        .collect::<Vec<_>>();
    let deployed = at_db::deployments::list_deployments_for_item(connection, item_id)
        .map_err(|error| index_error(error.to_string()))?
        .into_iter()
        .filter(|cell| cell.scope == scope)
        .map(|cell| cell.agent_id)
        .collect::<Vec<_>>();
    Ok((installed, deployed))
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
    parse_slug(&slug)?;
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
/// Validates an MCP id with the store's own slug rules.
///
/// The store owns the canonical id form (`at-store`'s `Slug`: lowercase, digits, `-`, `_` and
/// `.`), and a migration imports native names such as `claude_alpha` unchanged. A stricter
/// CLI-only rule made exactly those imported servers impossible to edit.
fn parse_slug(id: &str) -> Result<Slug, CliError> {
    Slug::parse(id).map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            format!("invalid MCP server id `{id}`: {error}"),
            "use lowercase letters, numbers, `-`, `_` or `.`, not starting with `-` or `.`",
        )
    })
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

/// Maps an index read or write failure for MCP state (never a secret inventory).
fn index_error(message: String) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("the MCP index is unavailable: {message}"),
        "run `agentuna doctor` to initialize or repair the index",
    )
}
