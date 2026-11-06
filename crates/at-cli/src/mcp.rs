//! MCP server and Keychain commands for `agentuna mcp`.

use std::io::{self, IsTerminal, Read};

use at_fs::{BackupPolicy, ExpectedState};
use at_model::{EnvValue, McpServerDef, McpTransport, Scope};
use at_store::layout::StoreLayout;
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
    let indexed = if paths.db_path().exists() {
        at_engine::mcp_cli::list_rows(&paths, scope, None)
            .map_err(|error| mcp_index_error(error.to_string()))?
    } else {
        Vec::new()
    };
    let servers = loaded
        .store
        .servers
        .iter()
        .map(|(slug, definition)| {
            let row = indexed.iter().find(|row| {
                !row.native_only
                    && row.scope == scope
                    && row.display_name == definition.display_name
            });
            let agent_state = row
                .map(|row| {
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
                        .collect::<serde_json::Map<_, _>>()
                })
                .unwrap_or_default();
            json!({
                "id": slug,
                "scope": scope.wire_name(),
                "name": definition.display_name,
                "transport": definition.transport.kind(),
                "agent_state": agent_state,
                "health": row.and_then(|row| row.latest_health.as_deref()).unwrap_or("unknown")
            })
        })
        .collect::<Vec<_>>();
    Ok(CommandOutput::new(
        json!({"servers":servers,"count":servers.len()}),
        format!(
            "{} MCP server(s) in {} scope\n",
            servers.len(),
            scope.wire_name()
        ),
    ))
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
