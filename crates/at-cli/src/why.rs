//! TA-070 scope explanation command.

use std::str::FromStr;

use at_db::{ReadOnlyDatabase, agents as agent_repo};
use at_engine::loader::load_scope_snapshots;
use at_engine::resolve::{ResolveInput, resolve};
use at_engine::why::{StepStatus, TraceEvidence, store_location, trace};
use at_model::{AgentId, ItemId, Scope};
use clap::{Arg, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, resolve_paths};

/// `agentuna why <item>`: explain the item decision across all four scopes.
pub struct WhyGroup;

impl CommandGroup for WhyGroup {
    fn name(&self) -> &'static str {
        "why"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("why")
                .about("Explain why an item is effective or disabled")
                .arg(
                    Arg::new("item")
                        .required(true)
                        .value_name("ITEM")
                        .help("Item id, or a skill, server or rule name that matches one item"),
                )
                .arg(
                    Arg::new("agent")
                        .long("agent")
                        .value_name("ID")
                        .help("Agent whose effective state to explain"),
                ),
        )
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let item_text = matches
            .get_one::<String>("item")
            .ok_or_else(|| CliError::internal("why dispatch without an item"))?;
        let scope = globals.scope().ok_or_else(|| {
            CliError::new(
                ErrorCode::Validation,
                "why requires --scope",
                "choose global, user, project or local",
            )
        })?;
        crate::runtime::refresh_agent_registry_if_needed(globals)?;
        let paths = resolve_paths(globals)?;
        let _access = CliAccess::read(&paths)?;
        let agent = match matches.get_one::<String>("agent") {
            Some(value) => AgentId::from_str(value).map_err(|error| {
                CliError::new(
                    ErrorCode::Validation,
                    error.to_string(),
                    "use an agent id from `agentuna agents list`",
                )
            })?,
            None => default_agent(paths.db_path())?,
        };
        let project_root = paths.project_store().and_then(|store| store.parent());
        let loaded = load_scope_snapshots(&paths, project_root).map_err(engine_error)?;
        let item_id = resolve_item(item_text, &loaded.layers)?;
        let resolution = resolve(
            ResolveInput::new(&loaded.layers)
                .with_tombstones(&loaded.tombstones)
                .for_agent(&agent),
        );
        let definitions: Vec<_> = loaded
            .layers
            .iter()
            .flat_map(|layer| {
                layer
                    .definitions
                    .iter()
                    .filter(move |definition| definition.item_id == Some(item_id))
                    .map(move |definition| (layer.scope, definition))
            })
            .collect();
        if definitions.is_empty() {
            return Err(CliError::new(
                ErrorCode::NotFound,
                format!("item `{item_id}` is not present in the selected stores"),
                "check the scope and project path",
            ));
        }
        let winning = resolution
            .items
            .iter()
            .find(|item| item.definition.item_id == Some(item_id));
        let disabled = resolution
            .disabled
            .iter()
            .find(|item| item.item_id == item_id);
        let shadowed: Vec<Scope> = resolution
            .shadowed
            .iter()
            .filter(|item| item.definition.item_id == Some(item_id))
            .map(|item| item.scope)
            .collect();
        let evidence = [Scope::Global, Scope::User, Scope::Project, Scope::Local].map(|scope_at| {
            let definition = definitions
                .iter()
                .find(|(scope, _)| *scope == scope_at)
                .map(|(_, definition)| *definition);
            let is_disabled = disabled.is_some_and(|item| item.scope == scope_at);
            let is_winner = winning.is_some_and(|item| item.scope == scope_at);
            let lock_reason = winning
                .filter(|item| item.scope == scope_at)
                .and_then(|item| item.lock_reason())
                .map(str::to_owned);
            let location = definition.map(|definition| store_location(definition, None));
            (
                scope_at,
                TraceEvidence {
                    defined: definition.is_some(),
                    disabled: is_disabled,
                    winning: is_winner,
                    shadowed: shadowed.contains(&scope_at),
                    lock_reason,
                    unreadable: None,
                    location,
                },
            )
        });
        let result = trace(item_id, scope, agent, evidence);
        let data = trace_json(&result);
        let human = render_trace(&result);
        Ok(CommandOutput::new(data, human))
    }
}

/// Resolves the `why` argument: a canonical item id, or a name that matches exactly one item.
///
/// A skill's folder slug and its frontmatter name are both accepted, because either is what
/// a reader sees. A name that matches several items is a typed refusal that lists every
/// candidate with its scope, so the caller can retry with the id instead of guessing.
fn resolve_item(text: &str, layers: &[at_model::ScopeSnapshot]) -> Result<ItemId, CliError> {
    if let Ok(item_id) = ItemId::from_str(text) {
        return Ok(item_id);
    }
    let needle = text.trim().to_lowercase();
    if needle.is_empty() {
        return Err(CliError::new(
            ErrorCode::Validation,
            "the item to explain is empty",
            "pass an item id or a name from `agentuna effective --scope <scope> --json`",
        ));
    }
    let mut candidates: Vec<(ItemId, Scope, at_model::ItemKind, String, String)> = Vec::new();
    for layer in layers {
        for definition in &layer.definitions {
            let Some(item_id) = definition.item_id else {
                continue;
            };
            if !matches_item_name(definition, &needle)
                || candidates.iter().any(|(id, ..)| *id == item_id)
            {
                continue;
            }
            candidates.push((
                item_id,
                layer.scope,
                definition.kind,
                definition.name.clone(),
                definition.path.display().to_string(),
            ));
        }
    }
    match candidates.as_slice() {
        [] => Err(CliError::new(
            ErrorCode::NotFound,
            format!("no item named `{text}` is present in the selected stores"),
            "list the items with `agentuna effective --scope <scope> --json`, or pass an item id",
        )),
        [(item_id, ..)] => Ok(*item_id),
        many => Err(CliError::new(
            ErrorCode::Validation,
            format!(
                "`{text}` matches {} items; pass the item id of the one to explain",
                many.len()
            ),
            "rerun with one of the item ids in `error.details.candidates`",
        )
        .with_details(json!({
            "candidates": many
                .iter()
                .map(|(item_id, scope, kind, name, path)| json!({
                    "item_id": item_id.to_string(),
                    "scope": scope.to_string(),
                    "kind": kind.to_string(),
                    "name": name,
                    "path": path,
                }))
                .collect::<Vec<_>>(),
        }))),
    }
}

/// Whether a definition answers to `needle`: its display name, or a skill's folder slug.
fn matches_item_name(definition: &at_model::Definition, needle: &str) -> bool {
    if definition.normalized_name() == needle {
        return true;
    }
    if definition.kind != at_model::ItemKind::Skill {
        return false;
    }
    // A skill lives in `<store>/skills/<slug>/SKILL.md`; the folder is its slug.
    definition
        .path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_lowercase() == needle)
}

fn default_agent(path: &std::path::Path) -> Result<AgentId, CliError> {
    if !path.exists() {
        return Ok(AgentId::ClaudeCode);
    }
    let database = ReadOnlyDatabase::open(path).map_err(db_error)?;
    let agents = agent_repo::list_agents_on_mac(database.connection()).map_err(db_error)?;
    Ok(agents
        .into_iter()
        .find(|agent| agent.presence == at_db::agents::AgentPresence::Installed)
        .map_or(AgentId::ClaudeCode, |agent| agent.id))
}

fn trace_json(trace: &at_engine::why::WhyTrace) -> Value {
    let steps: Vec<_> = trace.steps.iter().map(|step| {
        let (status, detail) = match &step.status {
            StepStatus::Absent => ("absent", Value::Null),
            StepStatus::Inherited => ("inherited", Value::Null),
            StepStatus::Defined => ("defined", Value::Null),
            StepStatus::Disabled => ("disabled", Value::Null),
            StepStatus::Locked { reason } => ("locked", json!({"reason": reason})),
            StepStatus::Shadowed => ("shadowed", Value::Null),
            StepStatus::Winning => ("winning", Value::Null),
            StepStatus::Unreadable { os_error } => ("unreadable", json!({"error": os_error})),
            _ => ("unknown", Value::Null),
        };
        json!({
            "scope": step.scope.to_string(), "status": status, "detail": detail,
            "reason": step.reason,
            "location": step.location.as_ref().map(|location| json!({
                "path": location.path.display().to_string(), "line": location.line,
                "byte_span": location.byte_span.as_ref().map(|span| [span.start, span.end]),
                "rendering_path": location.rendering_path.as_ref().map(|path| path.display().to_string()),
            })),
        })
    }).collect();
    json!({
        "item_id": trace.item_id.to_string(), "active_scope": trace.active_scope.to_string(),
        "agent": trace.agent.to_string(), "steps": steps,
        "verdict": {"on": trace.verdict.on, "decisive_step": trace.verdict.decisive_step},
    })
}

fn render_trace(trace: &at_engine::why::WhyTrace) -> String {
    let mut output = format!(
        "{} for {} at {}\n",
        trace.item_id, trace.agent, trace.active_scope
    );
    for step in &trace.steps {
        output.push_str(&format!(
            "{}  {:?}  {}",
            step.scope, step.status, step.reason
        ));
        if let Some(location) = &step.location {
            output.push_str(&format!("  {}", location.path.display()));
            if let Some(line) = location.line {
                output.push_str(&format!(":{line}"));
            }
        }
        output.push('\n');
    }
    output.push_str(&format!(
        "Effective: {}\n",
        if trace.verdict.on { "on" } else { "off" }
    ));
    output
}

fn engine_error(error: impl std::fmt::Display) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("cannot load why trace: {error}"),
        "check the selected store paths and run `agentuna doctor`",
    )
}

fn db_error(error: impl std::fmt::Display) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("cannot read agent index: {error}"),
        "run `agentuna doctor` to check the index",
    )
}
