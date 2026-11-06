//! Rule-file listing, inspection and safe editing commands.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use at_engine::rules;
use at_model::{ItemId, Scope};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::json;

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, resolve_paths};

/// `agentuna rules list|show` for native rule files.
pub struct RulesGroup;

impl CommandGroup for RulesGroup {
    fn name(&self) -> &'static str {
        "rules"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("rules")
                .about("List and inspect instruction files")
                .subcommand_required(true)
                .subcommand(Command::new("list").about("List discovered rule files"))
                .subcommand(
                    Command::new("show")
                        .about("Print one rule file")
                        .arg(Arg::new("path").required(true).value_name("PATH")),
                )
                .subcommand(
                    Command::new("save")
                        .about("Save a reviewed rule-file buffer")
                        .arg(Arg::new("path").required(true).value_name("PATH"))
                        .arg(
                            Arg::new("stdin")
                                .long("stdin")
                                .action(ArgAction::SetTrue)
                                .required(true),
                        )
                        .arg(Arg::new("force").long("force").action(ArgAction::SetTrue)),
                )
                .subcommand(
                    Command::new("new")
                        .about("Create a native rule file without overwriting")
                        .arg(
                            Arg::new("type")
                                .long("type")
                                .required(true)
                                .value_name("TYPE"),
                        ),
                ),
        )
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        match matches.subcommand() {
            Some(("save", args)) => save_rule(globals, args),
            Some(("new", args)) => new_rule(globals, args),
            Some(("list", _)) => list_rules(globals),
            Some(("show", args)) => show_rule(required_path(args)?),
            _ => Err(CliError::new(
                ErrorCode::Usage,
                "missing rules subcommand",
                "run `agentuna rules --help`",
            )),
        }
    }
}

fn list_rules(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    let scope = globals.scope().ok_or_else(|| {
        CliError::new(
            ErrorCode::Validation,
            "rules list requires --scope",
            "choose global, user, project or local",
        )
    })?;
    let paths = resolve_paths(globals)?;
    let _access = CliAccess::read(&paths)?;
    let scan = scan_rules(&paths, scope);
    let rows: Vec<_> = scan
        .sources
        .iter()
        .map(|source| {
            json!({
                "scope": source.scope.to_string(),
                "path": source.path.display().to_string(),
                "managed": source.managed,
                "size": source.bytes.len(),
                "hash": source.sha256,
            })
        })
        .collect();
    let diagnostics: Vec<_> = scan
        .diagnostics
        .iter()
        .map(|diagnostic| match diagnostic {
            rules::RuleDiagnostic::BrokenSymlink { path } => {
                json!({ "path": path.display().to_string(), "message": "broken symlink" })
            }
            rules::RuleDiagnostic::PermissionDenied { path } => {
                json!({ "path": path.display().to_string(), "message": "permission denied" })
            }
            rules::RuleDiagnostic::Io { path, message } => {
                json!({ "path": path.display().to_string(), "message": message })
            }
            rules::RuleDiagnostic::Unsupported { agent } => {
                json!({ "agent": agent.to_string(), "message": "rule discovery is unsupported" })
            }
            _ => json!({ "message": "unknown rule discovery diagnostic" }),
        })
        .collect();
    let human = scan
        .sources
        .iter()
        .map(|source| {
            format!(
                "{}  {}  {} bytes  {}",
                source.scope,
                if source.managed { "managed" } else { "native" },
                source.bytes.len(),
                source.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(CommandOutput::new(
        json!({ "rules": rows, "count": rows.len(), "diagnostics": diagnostics }),
        if human.is_empty() {
            "No rule files found.\n".to_owned()
        } else {
            format!("{human}\n")
        },
    ))
}

fn show_rule(path: &str) -> Result<CommandOutput, CliError> {
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path).map_err(|error| {
        CliError::new(
            ErrorCode::NotFound,
            format!("cannot read rule file {}: {error}", path.display()),
            "run `agentuna rules list --scope <scope>` to list available rules",
        )
    })?;
    let content = String::from_utf8(bytes).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "rule file is not valid UTF-8",
            "inspect the file with a tool that supports its encoding",
        )
    })?;
    let data = json!({ "path": path.display().to_string(), "content": content.clone() });
    Ok(CommandOutput::new(data, content))
}

fn save_rule(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    let scope = globals.scope().ok_or_else(|| {
        CliError::new(
            ErrorCode::Validation,
            "rules save requires --scope",
            "choose global, user, project or local",
        )
    })?;
    let path = PathBuf::from(required_path(args)?);
    let paths = resolve_paths(globals)?;
    let _access = CliAccess::write(&paths)?;
    let source = scan_rules(&paths, scope)
        .sources
        .into_iter()
        .find(|source| source.path == path)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::NotFound,
                "rule file was not found in the selected scope",
                "run `agentuna rules list --scope <scope>`",
            )
        })?;
    let mut buffer = Vec::new();
    std::io::stdin().read_to_end(&mut buffer).map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            format!("cannot read rule buffer from stdin: {error}"),
            "pipe the complete file to `agentuna rules save --stdin`",
        )
    })?;
    let source = if args.get_flag("force") {
        let current = std::fs::read(&source.path).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                error.to_string(),
                "check the rule file and retry",
            )
        })?;
        let mut refreshed = source;
        refreshed.sha256 = at_fs::sha256_hex(&current);
        refreshed.bytes = current;
        refreshed.mtime = std::fs::metadata(&refreshed.path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        refreshed
    } else {
        source
    };
    apply_rule_buffer(&paths, &source, &buffer)
}

fn apply_rule_buffer(
    paths: &at_store::paths::AppPaths,
    source: &rules::RuleSource,
    buffer: &[u8],
) -> Result<CommandOutput, CliError> {
    let database = at_db::Database::open_file(paths.db_path()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "run `agentuna doctor` to inspect the index",
        )
    })?;
    let support = paths.support_dir();
    let backup = paths.backup_dir();
    let roots = at_fs::ApplyRoots::new(vec![
        source.path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        support.to_path_buf(),
    ])
    .map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            error.to_string(),
            "check rule and support directories",
        )
    })?;
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: support,
        backup_folder: backup,
        recorder: &mut recorder,
    };
    let outcome = at_engine::rule_editor::save_rule(
        source,
        buffer,
        &ItemId::new(),
        database.connection(),
        &mut context,
        &roots,
        &at_fs::RecentWrites::new(),
    )
    .map_err(|error| {
        let code = if matches!(
            error,
            at_engine::rule_editor::RuleEditorError::Conflict { .. }
        ) {
            ErrorCode::Conflict
        } else {
            ErrorCode::Validation
        };
        CliError::new(
            code,
            error.to_string(),
            "review the rule-file diff and retry",
        )
    })?;
    let data = json!({ "path": source.path.display().to_string(), "changed": outcome.changed, "scope": source.scope.to_string() });
    let human = if outcome.changed {
        format!(
            "Saved {}. Backup: {}\n",
            source.path.display(),
            backup.display()
        )
    } else {
        format!("{} is unchanged.\n", source.path.display())
    };
    Ok(CommandOutput::new(data, human))
}

fn new_rule(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    let scope = globals.scope().ok_or_else(|| {
        CliError::new(
            ErrorCode::Validation,
            "rules new requires --scope",
            "choose global, user, project or local",
        )
    })?;
    let file_type = match args
        .get_one::<String>("type")
        .map(String::as_str)
        .unwrap_or_default()
    {
        "AGENTS.md" => at_engine::rule_editor::RuleFileType::Agents,
        "CLAUDE.md" => at_engine::rule_editor::RuleFileType::Claude,
        "CLAUDE.local.md" => at_engine::rule_editor::RuleFileType::ClaudeLocal,
        _ => {
            return Err(CliError::new(
                ErrorCode::Validation,
                "unsupported rule type",
                "choose AGENTS.md, CLAUDE.md or CLAUDE.local.md",
            ));
        }
    };
    let paths = resolve_paths(globals)?;
    let _access = CliAccess::write(&paths)?;
    let intent = at_engine::rule_editor::new_registered_rule(
        file_type,
        scope,
        paths.home_dir().unwrap_or(paths.user_store()),
        paths.project_store().and_then(Path::parent),
        true,
    )
    .map_err(|error| {
        let code = if matches!(
            error,
            at_engine::rule_editor::RuleEditorError::AlreadyExists { .. }
        ) {
            ErrorCode::Conflict
        } else {
            ErrorCode::Validation
        };
        CliError::new(
            code,
            error.to_string(),
            "check the selected scope and rule path",
        )
    })?;
    let database = at_db::Database::open_file(paths.db_path()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "run `agentuna doctor` to inspect the index",
        )
    })?;
    let backup = paths.backup_dir();
    let support = paths.support_dir();
    let roots = at_fs::ApplyRoots::new(vec![
        intent.path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        support.to_path_buf(),
    ])
    .map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            error.to_string(),
            "check rule and support directories",
        )
    })?;
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: support,
        backup_folder: backup,
        recorder: &mut recorder,
    };
    let (source, _) = at_engine::rule_editor::apply_new_rule(
        &intent,
        &ItemId::new(),
        database.connection(),
        &mut context,
        &roots,
        &at_fs::RecentWrites::new(),
    )
    .map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "the rule path changed; retry",
        )
    })?;
    Ok(CommandOutput::new(
        json!({ "path": source.path.display().to_string(), "scope": source.scope.to_string() }),
        format!("Created {}\n", source.path.display()),
    ))
}

fn scan_rules(paths: &at_store::paths::AppPaths, scope: Scope) -> rules::RuleScan {
    rules::scan_registered(
        paths.home_dir().unwrap_or(paths.user_store()),
        paths.project_store().and_then(Path::parent),
        scope,
    )
}

fn required_path(args: &ArgMatches) -> Result<&str, CliError> {
    args.get_one::<String>("path")
        .map(String::as_str)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Usage,
                "missing rule path",
                "run `agentuna rules show --help`",
            )
        })
}
