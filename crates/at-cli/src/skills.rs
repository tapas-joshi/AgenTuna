//! `agentuna skills` (TS-041): list, show, install, toggle, remove and presets.
//!
//! Every write is planned by at-engine and applied through the at-fs authorized-root
//! pipeline; this module owns only argument validation, the JSON payload shape and the
//! exit-code mapping. No command prompts, reads a TTY or opens a dialog.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::Database;
use at_engine::skill_cli::{
    ManifestItemInput, ToggleRequest, apply_toggle, plan_toggle, sync_manifest_item,
};
use at_model::{AgentId, ItemId, ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ManifestItem, StoreManifest};
use at_store::paths::AppPaths;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::{GlobalArgs, ScopeArg};
use crate::output::CommandOutput;
use crate::registry::CommandGroup;

/// The `skills` noun group.
pub struct SkillsGroup;

impl CommandGroup for SkillsGroup {
    fn name(&self) -> &'static str {
        "skills"
    }

    fn attach(&self, root: Command) -> Command {
        let preset = Command::new("preset")
            .about("List and apply skill presets")
            .subcommand_required(true)
            .subcommand(Command::new("list").about("List presets"))
            .subcommand(
                Command::new("apply")
                    .about("Apply a preset to one agent or the active project")
                    .arg(Arg::new("preset").required(true).value_name("PRESET"))
                    .arg(Arg::new("agent").long("agent").value_name("AGENT"))
                    .arg(
                        Arg::new("preset-project")
                            .long("target-project")
                            .value_name("PROJECT")
                            .help("Registered project id for a project target"),
                    ),
            );
        let skills = Command::new("skills")
            .about("Manage skills across scopes")
            .subcommand_required(true)
            .subcommand(Command::new("list").about("List skills in one scope"))
            .subcommand(
                Command::new("show")
                    .about("Show one skill")
                    .arg(Arg::new("name").required(true).value_name("NAME")),
            )
            .subcommand(
                Command::new("install")
                    .about("Install a skill from a folder, Git URL or marketplace id")
                    .arg(Arg::new("source").required(true).value_name("SOURCE"))
                    .arg(Arg::new("name").long("name").value_name("NAME"))
                    .arg(Arg::new("ref").long("ref").value_name("REF")),
            )
            .subcommand(
                Command::new("add")
                    .about("Add a skill from a folder, Git URL or marketplace id")
                    .arg(
                        Arg::new("from")
                            .long("from")
                            .required(true)
                            .value_name("SOURCE"),
                    )
                    .arg(Arg::new("name").long("name").value_name("NAME"))
                    .arg(Arg::new("ref").long("ref").value_name("REF")),
            )
            .subcommand(
                Command::new("enable")
                    .about("Enable a skill for one agent")
                    .arg(Arg::new("name").required(true).value_name("NAME"))
                    .arg(
                        Arg::new("agent")
                            .long("agent")
                            .required(true)
                            .value_name("AGENT"),
                    ),
            )
            .subcommand(
                Command::new("deploy")
                    .about("Deploy a skill to one agent (alias of enable)")
                    .arg(Arg::new("name").required(true).value_name("NAME"))
                    .arg(
                        Arg::new("agent")
                            .long("agent")
                            .required(true)
                            .value_name("AGENT"),
                    ),
            )
            .subcommand(
                Command::new("disable")
                    .about("Disable a skill for one agent")
                    .arg(Arg::new("name").required(true).value_name("NAME"))
                    .arg(
                        Arg::new("agent")
                            .long("agent")
                            .required(true)
                            .value_name("AGENT"),
                    ),
            )
            .subcommand(
                Command::new("remove")
                    .about("Remove a skill and every owned deployment")
                    .arg(Arg::new("name").required(true).value_name("NAME"))
                    .arg(
                        Arg::new("force")
                            .long("force")
                            .action(ArgAction::SetTrue)
                            .help("Remain available for compatibility; --yes also confirms"),
                    ),
            )
            .subcommand(preset);
        root.subcommand(skills)
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let (command, args) = matches.subcommand().ok_or_else(|| usage("skills"))?;
        match command {
            "list" => list(globals),
            "show" => show(globals, args),
            "install" => install(globals, args, required(args, "source")?),
            "add" => install(globals, args, required(args, "from")?),
            "enable" => set_enabled(globals, args, true),
            "deploy" => set_enabled(globals, args, true),
            "disable" => set_enabled(globals, args, false),
            "remove" => remove(globals, args),
            "preset" => dispatch_preset(globals, args),
            _ => Err(usage("skills")),
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

fn scope(globals: &GlobalArgs) -> Scope {
    globals.scope.unwrap_or(ScopeArg::User).into()
}

/// Resolves the selected scope's canonical store.
fn scope_layout(paths: &AppPaths, scope: Scope) -> Result<StoreLayout, CliError> {
    StoreLayout::for_scope(paths, scope).map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            format!("the selected scope has no canonical store: {error}"),
            "pass --project with an existing directory for project or local scope",
        )
    })
}

/// The registered project that owns `scope`, when the scope needs one.
pub(crate) fn project_id_for(
    database: &at_db::Database,
    paths: &AppPaths,
    scope: Scope,
) -> Result<Option<String>, CliError> {
    project_id_for_lookup(paths, scope, |root| {
        at_db::projects::find_project_by_path(database.connection(), root)
    })
}

/// The registered project that owns a read-only Project or Local scope.
pub(crate) fn project_id_for_read_only(
    database: &at_db::ReadOnlyDatabase,
    paths: &AppPaths,
    scope: Scope,
) -> Result<Option<String>, CliError> {
    project_id_for_lookup(paths, scope, |root| {
        at_db::projects::find_project_by_path(database.connection(), root)
    })
}

fn project_id_for_lookup(
    paths: &AppPaths,
    scope: Scope,
    find: impl FnOnce(&Path) -> Result<Option<at_db::projects::ProjectSummary>, at_db::DbError>,
) -> Result<Option<String>, CliError> {
    if !matches!(scope, Scope::Project | Scope::Local) {
        return Ok(None);
    }
    let root = paths
        .project_store()
        .and_then(Path::parent)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Validation,
                "the selected scope needs a project",
                "pass --project with an existing directory",
            )
        })?;
    let project = find(root)
        .map_err(|error| db_error(error.to_string()))?
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::NotFound,
                format!("project {} is not registered", root.display()),
                "run `agentuna projects add` first",
            )
        })?;
    Ok(Some(project.id))
}

fn db_error(message: String) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("the index could not be read or written: {message}"),
        "check the support directory and retry",
    )
}

fn store_error(error: at_store::layout::StoreError) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("the canonical store is unavailable: {error}"),
        "run `agentuna doctor` to inspect the store",
    )
}

fn manifest_error(error: at_store::manifest::ManifestError) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("store.json is unavailable: {error}"),
        "run `agentuna doctor` to inspect the store",
    )
}

/// Opens the writable index, creating only the missing derived tables.
fn open_database(paths: &AppPaths) -> Result<Database, CliError> {
    Database::open_file(paths.db_path()).map_err(|error| db_error(error.to_string()))
}

/// Opens the index only when it already exists; a command works without one.
fn existing_database(paths: &AppPaths) -> Result<Option<Database>, CliError> {
    if !paths.db_path().exists() {
        return Ok(None);
    }
    open_database(paths).map(Some)
}

/// Whether a package folder matches its manifest hash.
fn package_state(item: &ManifestItem, layout: &StoreLayout) -> &'static str {
    let path = layout.root().join(item.path.as_str());
    if !path.is_dir() {
        return "missing";
    }
    match at_store::manifest::hash_skill_folder(&path) {
        Ok(hash) if hash == item.hash => "present",
        Ok(_) => "drifted",
        Err(_) => "unreadable",
    }
}

/// Agent cells for one indexed skill.
fn agent_cells(
    connection: &at_db::Database,
    item_id: Option<ItemId>,
) -> Result<BTreeMap<String, Value>, CliError> {
    let mut cells = BTreeMap::new();
    let Some(item_id) = item_id else {
        return Ok(cells);
    };
    let deployments =
        at_db::deployments::list_deployments_for_item(connection.connection(), &item_id)
            .map_err(|error| db_error(error.to_string()))?;
    for cell in deployments {
        cells.insert(
            cell.agent_id.to_string(),
            json!({
                "state": cell.state.wire_name(),
                "mode": cell.mode,
                "path": cell.target_path,
            }),
        );
    }
    Ok(cells)
}

/// Reads one skill document's description, or `None` when it is unreadable.
fn description_for(item: &ManifestItem, layout: &StoreLayout) -> Option<String> {
    let path = layout.root().join(item.path.as_str()).join("SKILL.md");
    let bytes = std::fs::read(path).ok()?;
    let doc = at_store::skill::parse_skill_doc(&bytes).ok()?;
    Some(doc.description)
}

/// Finds a manifest item by slug or display name, case-insensitively.
fn find_item<'a>(manifest: &'a StoreManifest, name: &str) -> Option<&'a ManifestItem> {
    let wanted = name.to_ascii_lowercase();
    manifest.items_of_kind(ItemKind::Skill).find(|item| {
        item.slug.as_str() == wanted
            || item.slug.as_str() == name
            || item
                .name
                .as_deref()
                .is_some_and(|display| display.to_ascii_lowercase() == wanted)
    })
}

/// Display name of a manifest item, falling back to its slug.
fn display_name(item: &ManifestItem) -> String {
    item.name
        .clone()
        .unwrap_or_else(|| item.slug.as_str().to_owned())
}

/// `skills list`: canonical rows for one scope with their per-agent state.
fn list(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    crate::runtime::refresh_agent_registry_if_needed(globals)?;
    let access = crate::runtime::CliAccess::for_command(globals, false)?;
    let paths = access.paths().clone();
    let scope = scope(globals);
    let layout = scope_layout(&paths, scope)?;
    let loaded = match StoreManifest::load(&layout) {
        Ok(loaded) => Some(loaded),
        Err(error) if error.is_missing() => None,
        Err(error) => return Err(manifest_error(error)),
    };
    let database = existing_database(&paths)?;
    let mut skills = Vec::new();
    for item in loaded
        .as_ref()
        .map(|loaded| loaded.manifest.items_of_kind(ItemKind::Skill))
        .into_iter()
        .flatten()
    {
        let deployments = match &database {
            Some(database) => agent_cells(database, Some(item.id))?,
            None => BTreeMap::new(),
        };
        let state = package_state(item, &layout);
        let indexed = database
            .as_ref()
            .is_some_and(|database| item_indexed(database, item.id));
        skills.push(json!({
            "item_id": item.id.to_string(),
            "slug": item.slug.as_str(),
            "name": display_name(item),
            "scope": scope.wire_name(),
            "state": state,
            "indexed": indexed,
            "hash": item.hash.as_str(),
            "path": layout.root().join(item.path.as_str()).display().to_string(),
            "description": description_for(item, &layout),
            "agents": deployments,
        }));
    }
    skills.sort_by(|left, right| {
        left["slug"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["slug"].as_str().unwrap_or_default())
    });
    let count = skills.len();
    let human = if database.is_some() {
        format!("{count} skill(s) in {} scope\n", scope.wire_name())
    } else {
        format!(
            "{count} skill(s) in {} scope (index missing; per-agent state unknown)\n",
            scope.wire_name()
        )
    };
    Ok(CommandOutput::new(
        json!({
            "skills": skills,
            "count": count,
            "scope": scope.wire_name(),
            "indexed": database.is_some(),
        }),
        human,
    ))
}

/// Whether the derived item table contains this identity.
fn item_indexed(database: &Database, item_id: ItemId) -> bool {
    at_db::shared::items_by_ids(database.connection(), &[item_id])
        .map(|rows| !rows.is_empty())
        .unwrap_or(false)
}

/// `skills show`: one canonical package with its document and deployment cells.
fn show(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    crate::runtime::refresh_agent_registry_if_needed(globals)?;
    let access = crate::runtime::CliAccess::for_command(globals, false)?;
    let paths = access.paths().clone();
    let scope = scope(globals);
    let layout = scope_layout(&paths, scope)?;
    let name = required(args, "name")?;
    let loaded = StoreManifest::load(&layout).map_err(manifest_error)?;
    let item = find_item(&loaded.manifest, name).ok_or_else(|| {
        CliError::new(
            ErrorCode::NotFound,
            format!("no skill `{name}` in {} scope", scope.wire_name()),
            "run `agentuna skills list` to see available skills",
        )
    })?;
    let dir = layout.root().join(item.path.as_str());
    let markdown = std::fs::read(dir.join("SKILL.md"))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();
    let description = at_store::skill::parse_skill_doc(markdown.as_bytes())
        .ok()
        .map(|doc| doc.description);
    let database = existing_database(&paths)?;
    let agents = match &database {
        Some(database) => agent_cells(database, Some(item.id))?,
        None => BTreeMap::new(),
    };
    let data = json!({
        "item_id": item.id.to_string(),
        "slug": item.slug.as_str(),
        "name": display_name(item),
        "scope": scope.wire_name(),
        "path": dir.display().to_string(),
        "hash": item.hash.as_str(),
        "description": description,
        "markdown": markdown,
        "state": package_state(item, &layout),
        "agents": agents,
    });
    Ok(CommandOutput::new(
        data,
        format!("{} ({})\n", display_name(item), item.slug.as_str()),
    ))
}

/// What kind of source an `install` argument denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Folder,
    Git,
    Marketplace,
}

/// Classifies an install argument without touching the filesystem or network.
fn classify_source(value: &str) -> Option<SourceKind> {
    if value.starts_with("https://")
        || value.starts_with("ssh://")
        || value.starts_with("git@")
        || value.starts_with("file://")
        || value.ends_with(".git")
    {
        return Some(SourceKind::Git);
    }
    if Path::new(value).is_dir() {
        return Some(SourceKind::Folder);
    }
    if marketplace_id(value).is_some() {
        return Some(SourceKind::Marketplace);
    }
    None
}

/// Splits the documented `owner/repository/skill` marketplace identifier.
fn marketplace_id(value: &str) -> Option<(&str, &str, &str)> {
    let mut parts = value.split('/');
    let (owner, repository, skill) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let valid = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    (valid(owner) && valid(repository) && valid(skill)).then_some((owner, repository, skill))
}

/// One resolved install source: the package bytes and the provenance to record.
struct PreparedSource {
    kind: SourceKind,
    name: String,
    package: at_store::SkillPackage,
    folder: Option<PathBuf>,
    git: Option<(String, Option<String>)>,
    marketplace: Option<String>,
}

/// Resolves the selected candidate out of a folder, Git clone or marketplace listing.
fn prepare_source(
    paths: &AppPaths,
    source: &str,
    name: Option<&str>,
    reference: Option<&str>,
) -> Result<PreparedSource, CliError> {
    let kind = classify_source(source).ok_or_else(|| {
        CliError::new(
            ErrorCode::NotFound,
            format!("no folder, Git URL or marketplace id matches `{source}`"),
            "pass an existing directory, a Git URL such as https://host/repo.git, or owner/repo/skill",
        )
    })?;
    match kind {
        SourceKind::Folder => {
            let root = Path::new(source);
            let candidates = at_engine::skill_import::import_folder(root).map_err(|error| {
                CliError::new(
                    ErrorCode::Validation,
                    format!("cannot scan the folder: {error}"),
                    "select a readable directory containing SKILL.md",
                )
            })?;
            select_and_load(kind, root, &candidates, name, None, None)
        }
        SourceKind::Git => {
            let policy = at_model::network::NetworkPolicy::default();
            let mut log = at_model::network::NetworkEventLog::default();
            let source_url = at_git::import::strip_url_credentials(source);
            let import = at_engine::skill_import::import_git(
                source,
                reference,
                paths.cache_dir(),
                &policy,
                &mut log,
            )
            .map_err(|error| {
                CliError::new(
                    ErrorCode::NetworkRequired,
                    format!("the Git import failed: {error}"),
                    "check the URL, the reference and network access",
                )
            })?;
            let mut prepared = select_and_load(
                kind,
                import.repository_root(),
                &import.candidates,
                name,
                Some(source_url.clone()),
                reference.map(ToOwned::to_owned),
            )?;
            prepared.git = Some((source_url, reference.map(ToOwned::to_owned)));
            Ok(prepared)
        }
        SourceKind::Marketplace => {
            if marketplace_id(source).is_none() {
                return Err(CliError::new(
                    ErrorCode::Validation,
                    "use owner/repository/skill for a marketplace id",
                    "pass the id shown by the marketplace listing",
                ));
            }
            let policy = at_model::network::NetworkPolicy::default();
            let mut log = at_model::network::NetworkEventLog::default();
            let import = at_engine::skill_import::import_marketplace(
                source,
                paths.cache_dir(),
                &policy,
                &mut log,
            )
            .map_err(|error| {
                CliError::new(
                    ErrorCode::NetworkRequired,
                    format!("the marketplace install failed: {error}"),
                    "check the listing id and network access",
                )
            })?;
            let mut prepared = select_and_load(
                kind,
                import.import.repository_root(),
                &import.import.candidates,
                name,
                None,
                None,
            )?;
            prepared.marketplace = Some(source.to_owned());
            Ok(prepared)
        }
    }
}

/// Picks one candidate by `--name` (or the only valid one) and loads its package.
fn select_and_load(
    kind: SourceKind,
    root: &Path,
    candidates: &[at_engine::skill_import::SkillImportCandidate],
    name: Option<&str>,
    git_url: Option<String>,
    git_ref: Option<String>,
) -> Result<PreparedSource, CliError> {
    let valid = candidates
        .iter()
        .filter(|candidate| candidate.is_valid())
        .collect::<Vec<_>>();
    if valid.is_empty() {
        return Err(CliError::new(
            ErrorCode::Validation,
            "the source contains no valid skill package",
            "each candidate needs a parseable SKILL.md",
        ));
    }
    let wanted = name.map(|value| value.to_ascii_lowercase());
    let selected = match wanted.as_deref() {
        Some(wanted) => valid
            .iter()
            .find(|candidate| {
                candidate
                    .name
                    .as_deref()
                    .map(|name| name.to_ascii_lowercase() == wanted)
                    .unwrap_or(false)
                    || candidate
                        .subpath
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
            })
            .copied()
            .ok_or_else(|| {
                CliError::new(
                    ErrorCode::NotFound,
                    format!("no imported skill matches `{wanted}`"),
                    "run the install with --dry-run to list candidate names",
                )
            })?,
        None if valid.len() == 1 => valid[0],
        None => {
            let names = valid
                .iter()
                .filter_map(|candidate| candidate.name.clone())
                .collect::<Vec<_>>();
            return Err(CliError::new(
                ErrorCode::Validation,
                "the source contains several skills; choose one with --name",
                "run the install with --dry-run to list candidate names",
            )
            .with_details(json!({ "candidates": names })));
        }
    };
    let dir = root.join(&selected.subpath);
    let loaded = at_store::SkillPackage::read(&dir).map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            format!("the selected skill package is invalid: {error}"),
            "fix the package and retry",
        )
    })?;
    let display = selected
        .name
        .clone()
        .ok_or_else(|| CliError::internal("a validated candidate lost its name"))?;
    Ok(PreparedSource {
        kind,
        name: display,
        package: loaded.package,
        folder: matches!(kind, SourceKind::Folder).then(|| dir),
        git: matches!(kind, SourceKind::Git).then_some((git_url.unwrap_or_default(), git_ref)),
        marketplace: None,
    })
}

/// `skills install` / `skills add`: import one package and index it.
fn install(
    globals: &GlobalArgs,
    args: &ArgMatches,
    source: &str,
) -> Result<CommandOutput, CliError> {
    let access = crate::runtime::CliAccess::for_command(globals, !globals.dry_run)?;
    if !globals.dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let scope = scope(globals);
    let layout = scope_layout(&paths, scope)?;
    let name_override = args.get_one::<String>("name").map(String::as_str);
    let reference = args.get_one::<String>("ref").map(String::as_str);
    let kind = classify_source(source).ok_or_else(|| {
        CliError::new(
            ErrorCode::NotFound,
            format!("no folder, Git URL or marketplace id matches `{source}`"),
            "pass an existing directory, a Git URL such as https://host/repo.git, or owner/repo/skill",
        )
    })?;
    if globals.dry_run && kind == SourceKind::Marketplace {
        // The listing id already names the repository, so a preview needs no request.
        let (owner, repository, skill) = marketplace_id(source)
            .ok_or_else(|| CliError::internal("a classified marketplace id lost its parts"))?;
        return Ok(CommandOutput::new(
            json!({
                "source": "marketplace",
                "mode": "dry_run",
                "id": source,
                "repository": format!("https://github.com/{owner}/{repository}"),
                "skill": skill,
                "files": [],
            }),
            format!("Would install marketplace listing {source}\n"),
        ));
    }
    let prepared = prepare_source(&paths, source, name_override, reference)?;
    let slug_text =
        at_engine::skill_templates::slugify(name_override.unwrap_or(prepared.name.as_str()))
            .map_err(|error| {
                CliError::new(
                    ErrorCode::Validation,
                    format!("the skill name is not usable as a slug: {error}"),
                    "pass --name with letters, digits or spaces",
                )
            })?;
    let slug = Slug::parse(&slug_text).map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            error.to_string(),
            "choose another --name",
        )
    })?;
    let mut planned =
        at_engine::skill_add_plan::plan_skill_add(at_engine::skill_add_plan::SkillAddPlanRequest {
            layout: &layout,
            slug: &slug,
            package: &prepared.package,
            scope,
            deployments: &[],
            repository: None,
            stage_requested: false,
        })
        .map_err(|error| {
            CliError::new(
                ErrorCode::Validation,
                format!("the skill cannot be added: {error}"),
                "inspect the package and any existing files it would replace",
            )
        })?;
    // A scope with no store yet gets its skeleton in the same apply. The dry run lists it, so a
    // script previews exactly what the confirmed run writes.
    let store_init = at_store::store_init::plan_store_init(&layout, &mut planned.change_set)
        .map_err(crate::error::store_init_error)?;
    let mut files = planned
        .files
        .iter()
        .map(|file| file.path.display().to_string())
        .collect::<Vec<_>>();
    if let Some(init) = &store_init {
        let mut created = init
            .created
            .iter()
            .map(|entry| layout.root().join(entry).display().to_string())
            .collect::<Vec<_>>();
        created.append(&mut files);
        files = created;
    }
    let created_store = store_init
        .as_ref()
        .map(|init| init.root.display().to_string());
    if globals.dry_run {
        return Ok(CommandOutput::new(
            json!({
                "source": prepared.kind.as_str(),
                "mode": "dry_run",
                "name": prepared.name,
                "slug": slug.as_str(),
                "scope": scope.wire_name(),
                "create_store": created_store,
                "files": files,
                "overwrites": planned
                    .overwrites
                    .iter()
                    .map(|entry| entry.path.display().to_string())
                    .collect::<Vec<_>>(),
            }),
            match &created_store {
                Some(path) => format!(
                    "Would create the {} store at {path} and add {} skill(s)\n",
                    scope.wire_name(),
                    planned.files.len()
                ),
                None => format!(
                    "Would add {} skill(s) to {}\n",
                    planned.files.len(),
                    scope.wire_name()
                ),
            },
        ));
    }
    let roots = at_fs::ApplyRoots::new(authorized_roots(&paths, layout.root())).map_err(|_| {
        CliError::new(
            ErrorCode::Conflict,
            "authorized skill directories are unavailable",
            "check that the home, store and project folders exist",
        )
    })?;
    let mut database = open_database(&paths)?;
    let outcome = {
        let mut recorder = at_fs::DbOperationRecorder::new_with_actor(
            database.connection(),
            30,
            at_db::ops::OperationActor::Cli,
        );
        let mut context = at_fs::ApplyContext {
            store_root: layout.root(),
            backup_folder: paths.backup_dir(),
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots(&planned.change_set, &mut context, &roots).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("the reviewed import could not be applied: {error}"),
                "review the current store state and retry",
            )
        })?
    };
    // The canonical manifest is reconstructed from the package that now exists, which
    // allocates or preserves the stable identity, then the derived row is made to agree.
    let rebuild = at_store::manifest_rebuild::plan_manifest_rebuild(&layout).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("store.json could not be rebuilt: {error}"),
            "run `agentuna doctor` to inspect the store",
        )
    })?;
    let token = rebuild.token().clone();
    at_store::manifest_rebuild::apply_manifest_rebuild(&layout, &rebuild, &token).map_err(
        |error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("store.json could not be written: {error}"),
                "review the current store state and retry",
            )
        },
    )?;
    let reloaded = StoreManifest::load(&layout).map_err(manifest_error)?;
    let item = reloaded
        .manifest
        .item_by_slug(ItemKind::Skill, &slug)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::Conflict,
                "the imported package is missing from the rebuilt manifest",
                "run `agentuna doctor` to inspect the store",
            )
        })?
        .clone();
    let project_id = project_id_for(&database, &paths, scope)?;
    let item_id = sync_manifest_item(
        database.connection(),
        &ManifestItemInput {
            scope,
            project_id: project_id.as_deref(),
            item: &item,
        },
    )
    .map_err(|error| db_error(error.to_string()))?;
    record_source(
        &mut database,
        &paths,
        &prepared,
        &item,
        item_id,
        scope,
        &slug,
    )?;
    Ok(CommandOutput::new(
        json!({
            "source": prepared.kind.as_str(),
            "name": prepared.name,
            "slug": slug.as_str(),
            "scope": scope.wire_name(),
            "item_id": item_id.to_string(),
            "path": layout.root().join(item.path.as_str()).display().to_string(),
            "hash": item.hash.as_str(),
            "operation_id": outcome.operation_id,
            "files_written": outcome.completed,
        }),
        format!(
            "Added {} ({}) to {} scope\n",
            prepared.name,
            slug.as_str(),
            scope.wire_name()
        ),
    ))
}

impl SourceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::Git => "git",
            Self::Marketplace => "marketplace",
        }
    }
}

/// Records folder, Git or marketplace provenance for one imported package.
fn record_source(
    database: &mut Database,
    paths: &AppPaths,
    prepared: &PreparedSource,
    item: &ManifestItem,
    item_id: ItemId,
    scope: Scope,
    slug: &Slug,
) -> Result<(), CliError> {
    let _ = scope;
    if let Some(folder) = &prepared.folder {
        let mut spec =
            at_db::sources::SourceSpec::new(&item_id, at_db::sources::SourceKind::Folder);
        let url = folder.display().to_string();
        spec.url = Some(&url);
        spec.content_hash = Some(item.hash.as_str());
        at_db::sources::upsert_source(database.connection_mut(), &spec)
            .map_err(|error| db_error(error.to_string()))?;
    } else if let Some((url, reference)) = &prepared.git {
        let mut spec = at_db::sources::SourceSpec::new(&item_id, at_db::sources::SourceKind::Git);
        spec.url = Some(url);
        spec.git_ref = reference.as_deref();
        spec.content_hash = Some(item.hash.as_str());
        let subpath = format!("skills/{}", slug.as_str());
        spec.subpath = Some(&subpath);
        at_db::sources::upsert_source(database.connection_mut(), &spec)
            .map_err(|error| db_error(error.to_string()))?;
    } else if let Some(id) = &prepared.marketplace {
        let subpath = format!("skills/{}", slug.as_str());
        at_engine::skill_import::record_marketplace_source(
            database.connection_mut(),
            &item_id,
            id,
            "",
            &subpath,
            item.hash.as_str(),
        )
        .map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("the marketplace source could not be recorded: {error}"),
                "the package was installed; run `agentuna doctor` to inspect the index",
            )
        })?;
    }
    let _ = paths;
    Ok(())
}

struct ToggleRequestInputs<'a> {
    paths: &'a AppPaths,
    store: &'a StoreLayout,
    scope: Scope,
    project_id: Option<String>,
    item: &'a ManifestItem,
    item_name: &'a str,
    agent: &'a AgentId,
    enabled: bool,
}

fn toggle_request(inputs: ToggleRequestInputs<'_>) -> ToggleRequest<'_> {
    ToggleRequest {
        paths: inputs.paths,
        store: inputs.store,
        scope: inputs.scope,
        project_id: inputs.project_id,
        project_root: inputs
            .paths
            .project_store()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        item_id: inputs.item.id,
        item_name: inputs.item_name.to_owned(),
        slug: inputs.item.slug.clone(),
        agent: inputs.agent.clone(),
        enabled: inputs.enabled,
    }
}

/// `skills enable|disable|deploy`: one reviewed per-agent deployment change.
fn set_enabled(
    globals: &GlobalArgs,
    args: &ArgMatches,
    enabled: bool,
) -> Result<CommandOutput, CliError> {
    let access = crate::runtime::CliAccess::for_command(globals, !globals.dry_run)?;
    if !globals.dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let scope = scope(globals);
    let layout = scope_layout(&paths, scope)?;
    let name = required(args, "name")?;
    let agent = AgentId::from_str(required(args, "agent")?).map_err(|_| {
        CliError::new(
            ErrorCode::Validation,
            "unknown agent id",
            "use an agent id from `agentuna agents list`",
        )
    })?;
    let loaded = StoreManifest::load(&layout).map_err(manifest_error)?;
    let item = find_item(&loaded.manifest, name)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::NotFound,
                format!("no skill `{name}` in {} scope", scope.wire_name()),
                "run `agentuna skills list` to see available skills",
            )
        })?
        .clone();
    let slug = item.slug.clone();
    let item_name = display_name(&item);
    if globals.dry_run {
        let database = crate::runtime::open_index_read_only(&paths)?;
        let project_id = project_id_for_read_only(&database, &paths, scope)?;
        let request = toggle_request(ToggleRequestInputs {
            paths: &paths,
            store: &layout,
            scope,
            project_id,
            item: &item,
            item_name: &item_name,
            agent: &agent,
            enabled,
        });
        let plan = plan_toggle(database.connection(), &request).map_err(toggle_error)?;
        return Ok(CommandOutput::new(
            json!({
                "name": item_name,
                "slug": slug.as_str(),
                "scope": scope.wire_name(),
                "agent": agent.to_string(),
                "enabled": enabled,
                "dry_run": true,
                "mode": plan.mode.map(|mode| mode.to_string()),
                "paths": plan.paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                "warnings": plan.warnings,
                "no_op": plan.no_op,
                "change_set": plan.change_set,
            }),
            format!(
                "Would {} {} for {}\n",
                if enabled { "enable" } else { "disable" },
                item_name,
                agent
            ),
        ));
    }

    let database = open_database(&paths)?;
    let project_id = project_id_for(&database, &paths, scope)?;
    sync_manifest_item(
        database.connection(),
        &ManifestItemInput {
            scope,
            project_id: project_id.as_deref(),
            item: &item,
        },
    )
    .map_err(|error| db_error(error.to_string()))?;
    let request = toggle_request(ToggleRequestInputs {
        paths: &paths,
        store: &layout,
        scope,
        project_id,
        item: &item,
        item_name: &item_name,
        agent: &agent,
        enabled,
    });
    let plan = plan_toggle(database.connection(), &request).map_err(toggle_error)?;
    let outcome = apply_toggle(database.connection(), &plan).map_err(toggle_error)?;
    Ok(CommandOutput::new(
        json!({
            "name": item_name,
            "slug": slug.as_str(),
            "scope": scope.wire_name(),
            "agent": agent.to_string(),
            "enabled": enabled,
            "changed": outcome.changed,
            "no_op": plan.no_op,
            "operation_id": outcome.operation_id,
            "mode": plan.mode.map(|mode| mode.to_string()),
            "paths": plan.paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "warnings": plan.warnings,
        }),
        format!(
            "{} {} for {}: {} file(s) changed\n",
            if enabled { "Enabled" } else { "Disabled" },
            item_name,
            agent,
            plan.paths.len()
        ),
    ))
}

/// Maps engine toggle failures onto the published error classes.
fn toggle_error(error: at_engine::skill_cli::SkillCliError) -> CliError {
    use at_engine::skill_cli::SkillCliError;
    let (code, hint) = match &error {
        SkillCliError::AgentNotInstalled { .. }
        | SkillCliError::AgentUnmanaged { .. }
        | SkillCliError::Package { .. }
        | SkillCliError::MissingDeployment { .. } => (
            ErrorCode::NotFound,
            "inspect the agent and skill state, then retry",
        ),
        SkillCliError::Locked { .. } => (
            ErrorCode::Locked,
            "remove the lock in Effective config, or ask an administrator",
        ),
        SkillCliError::NativeConflict { .. } => (
            ErrorCode::Conflict,
            "adopt the unmanaged folder or move it aside first",
        ),
        _ => (ErrorCode::Conflict, "review the current state and retry"),
    };
    CliError::new(code, error.to_string(), hint)
}

/// `skills remove`: deletion needs `--yes`; without it the command is a
/// `confirmation_required` refusal (exit 9), like every other destructive command.
fn remove(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    let access = crate::runtime::CliAccess::for_command(globals, !globals.dry_run)?;
    if !globals.dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let scope = scope(globals);
    let layout = scope_layout(&paths, scope)?;
    let name = required(args, "name")?;
    if !globals.yes && !globals.dry_run {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "removing a skill requires --yes",
            "inspect the skill, then retry with --yes to confirm the backup-backed delete",
        )
        .with_details(json!({ "requires": "yes" })));
    }
    let loaded = StoreManifest::load(&layout).map_err(manifest_error)?;
    let item = find_item(&loaded.manifest, name)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::NotFound,
                format!("no skill `{name}` in {} scope", scope.wire_name()),
                "run `agentuna skills list` to see available skills",
            )
        })?
        .clone();
    let item_name = display_name(&item);
    let manifest_bytes = std::fs::read(layout.manifest_path()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("store.json could not be read: {error}"),
            "run `agentuna doctor` to inspect the store",
        )
    })?;
    let database = open_database(&paths)?;
    let project_id = project_id_for(&database, &paths, scope)?;
    sync_manifest_item(
        database.connection(),
        &ManifestItemInput {
            scope,
            project_id: project_id.as_deref(),
            item: &item,
        },
    )
    .map_err(|error| db_error(error.to_string()))?;
    let plan =
        at_engine::skill_delete::plan_delete_skill(at_engine::skill_delete::DeleteSkillRequest {
            intent: at_engine::skill_delete::DeleteSkill {
                slug: item.slug.clone(),
                scope,
            },
            confirmed: true,
            store: &layout,
            manifest: &loaded.manifest,
            manifest_bytes: &manifest_bytes,
            connection: database.connection(),
            project_id: project_id.as_deref(),
            global_permission: false,
        })
        .map_err(delete_error)?;
    let paths_changed = plan
        .change_set
        .ops
        .iter()
        .filter_map(|op| op.destination().map(|path| path.display().to_string()))
        .collect::<Vec<_>>();
    if globals.dry_run {
        return Ok(CommandOutput::new(
            json!({
                "name": item_name,
                "slug": item.slug.as_str(),
                "scope": scope.wire_name(),
                "dry_run": true,
                "paths": paths_changed,
                "next_effective": plan.next_effective.as_ref().map(|next| json!({
                    "item_id": next.item_id.to_string(),
                    "scope": next.scope.wire_name(),
                })),
            }),
            format!(
                "Would remove {} from {} scope\n",
                item_name,
                scope.wire_name()
            ),
        ));
    }
    let roots = at_fs::ApplyRoots::new(authorized_roots(&paths, layout.root())).map_err(|_| {
        CliError::new(
            ErrorCode::Conflict,
            "authorized skill directories are unavailable",
            "check that the home, store and project folders exist",
        )
    })?;
    let outcome = {
        let mut recorder = at_fs::DbOperationRecorder::new_with_actor(
            database.connection(),
            30,
            at_db::ops::OperationActor::Cli,
        );
        let mut context = at_fs::ApplyContext {
            store_root: layout.root(),
            backup_folder: paths.backup_dir(),
            recorder: &mut recorder,
        };
        at_engine::skill_delete::apply_delete_skill(
            database.connection(),
            &plan,
            &mut context,
            &roots,
        )
        .map_err(delete_error)?
    };
    let backups =
        at_db::backups::backups_for_operation(database.connection(), &outcome.operation_id)
            .map_err(|error| db_error(error.to_string()))?;
    let backup_path = std::fs::read_dir(paths.backup_dir())
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .ends_with(&format!("-{}", outcome.operation_id))
            })
        })
        .unwrap_or_else(|| paths.backup_dir().to_path_buf());
    Ok(CommandOutput::new(
        json!({
            "name": item_name,
            "slug": item.slug.as_str(),
            "scope": scope.wire_name(),
            "removed": true,
            "operation_id": outcome.operation_id,
            "backup_path": backup_path.display().to_string(),
            "backups": backups.len(),
            "next_effective": plan.next_effective.as_ref().map(|next| json!({
                "item_id": next.item_id.to_string(),
                "scope": next.scope.wire_name(),
            })),
        }),
        format!(
            "Removed {} from {} scope; backup at {}\n",
            item_name,
            scope.wire_name(),
            backup_path.display()
        ),
    ))
}

/// Maps deletion failures onto the published error classes.
fn delete_error(error: at_engine::skill_delete::DeleteSkillError) -> CliError {
    use at_engine::skill_delete::DeleteSkillError;
    let (code, hint) = match &error {
        DeleteSkillError::MissingSkill { .. } => (
            ErrorCode::NotFound,
            "run `agentuna skills list` to see available skills",
        ),
        DeleteSkillError::Locked { .. } => (
            ErrorCode::Locked,
            "remove the lock in Effective config, or ask an administrator",
        ),
        DeleteSkillError::RefusedUnmanaged(_) => (
            ErrorCode::Conflict,
            "the folder is not managed by AgenTuna; adopt it or move it aside",
        ),
        DeleteSkillError::PermissionDenied => (
            ErrorCode::Locked,
            "writing the Global store requires the admin permission flag",
        ),
        DeleteSkillError::ConfirmationRequired => {
            (ErrorCode::ConfirmationRequired, "retry with --yes")
        }
        _ => (ErrorCode::Conflict, "review the current state and retry"),
    };
    CliError::new(code, error.to_string(), hint)
}

/// `skills preset ...`.
fn dispatch_preset(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    let (command, rest) = args.subcommand().ok_or_else(|| usage("skills preset"))?;
    match command {
        "list" => preset_list(globals),
        "apply" => preset_apply(globals, rest),
        _ => Err(usage("skills preset")),
    }
}

/// `skills preset list`.
fn preset_list(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    let access = crate::runtime::CliAccess::for_command(globals, false)?;
    let paths = access.paths().clone();
    let mut database = open_database(&paths)?;
    let service = at_engine::presets::PresetService::new(&mut database);
    let presets = service.list().map_err(preset_error)?;
    let rows = presets
        .iter()
        .map(|preset| {
            json!({
                "id": preset.id,
                "name": preset.name,
                "description": preset.description,
                "scope": preset.scope.wire_name(),
                "builtin": preset.is_builtin,
                "member_count": preset.member_count,
                "enabled_count": preset.enabled_count,
            })
        })
        .collect::<Vec<_>>();
    let count = rows.len();
    Ok(CommandOutput::new(
        json!({ "presets": rows, "count": count }),
        format!("{count} preset(s)\n"),
    ))
}

/// Facts needed to plan each preset member without holding a database borrow.
struct MemberFacts {
    item_id: ItemId,
    kind: ItemKind,
    scope: Scope,
    slug: String,
    name: String,
    layout: StoreLayout,
    source: PathBuf,
    skill_targets: Vec<SkillTarget>,
    definition: Option<at_model::McpServerDef>,
    mcp_targets: Vec<(AgentId, PathBuf)>,
}

struct SkillTarget {
    agent: AgentId,
    destination: PathBuf,
    destination_managed: bool,
    already_linked: bool,
    mode: at_model::LinkMode,
    link_supported: bool,
}

/// `skills preset apply` against one agent or one registered project.
fn preset_apply(globals: &GlobalArgs, args: &ArgMatches) -> Result<CommandOutput, CliError> {
    let access = crate::runtime::CliAccess::for_command(globals, !globals.dry_run)?;
    if !globals.dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();
    let preset_id = required(args, "preset")?.to_owned();
    let agent = args.get_one::<String>("agent").map(String::as_str);
    let project = args.get_one::<String>("preset-project").map(String::as_str);
    let mut database = open_database(&paths)?;
    // A preset id that does not exist must not report success: the apply would otherwise
    // touch nothing and exit 0, which an agent reads as "applied".
    let known = at_db::presets::list_presets(database.connection())
        .map_err(|error| db_error(error.to_string()))?
        .iter()
        .any(|preset| preset.id == preset_id);
    if !known {
        return Err(CliError::new(
            ErrorCode::NotFound,
            format!("preset `{preset_id}` does not exist"),
            "run `agentuna skills preset list` to see the available presets",
        ));
    }
    let (target, native_scope, project_root) = match (agent, project) {
        (Some(agent), None) => {
            let agent = AgentId::from_str(agent).map_err(|_| {
                CliError::new(
                    ErrorCode::Validation,
                    "unknown agent id",
                    "run `agentuna agents list` for known agents",
                )
            })?;
            (at_engine::presets::Target::Agent(agent), Scope::User, None)
        }
        (None, Some(project)) => {
            let row = at_db::projects::find_project_by_id(database.connection(), project)
                .map_err(|error| db_error(error.to_string()))?
                .ok_or_else(|| {
                    CliError::new(
                        ErrorCode::NotFound,
                        format!("project `{project}` is not registered"),
                        "run `agentuna projects list`",
                    )
                })?;
            (
                at_engine::presets::Target::Repo(project.to_owned()),
                Scope::Project,
                Some(PathBuf::from(row.path)),
            )
        }
        _ => {
            return Err(CliError::new(
                ErrorCode::Usage,
                "choose one target: --agent or --project",
                "run `agentuna skills preset apply --help`",
            ));
        }
    };
    let members =
        at_db::presets::preset_members(database.connection(), &preset_id).map_err(|_| {
            CliError::new(
                ErrorCode::NotFound,
                format!("preset `{preset_id}` is unavailable"),
                "run `agentuna skills preset list`",
            )
        })?;
    let env = AgentEnvironment {
        home: paths
            .home_dir()
            .ok_or_else(|| {
                CliError::new(
                    ErrorCode::Validation,
                    "the home directory is unavailable",
                    "set AGENTUNA_HOME",
                )
            })?
            .to_path_buf(),
        project: project_root.clone(),
        variables: Default::default(),
    };
    let registry = AgentAdapterRegistry::register();
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|error| db_error(error.to_string()))?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| db_error("deployment settings are unavailable".to_owned()))?;
    let managed = agents
        .iter()
        .filter(|row| row.managed && row.presence != at_db::agents::AgentPresence::NotInstalled)
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let selected_agents: Vec<AgentId> = match &target {
        at_engine::presets::Target::Agent(agent) => vec![agent.clone()],
        at_engine::presets::Target::Repo(_) => managed.clone(),
        _ => Vec::new(),
    };
    let project_id = project_root
        .as_deref()
        .and_then(|root| {
            at_db::projects::find_project_by_path(database.connection(), root)
                .ok()
                .flatten()
        })
        .map(|project| project.id);
    let mut facts = BTreeMap::new();
    for member in &members {
        if !member.enabled || !member.exists {
            continue;
        }
        let Ok(item_id) = member.item_id.parse::<ItemId>() else {
            continue;
        };
        let rows = at_db::shared::items_by_ids(database.connection(), &[item_id])
            .map_err(|error| db_error(error.to_string()))?;
        let Some(row) = rows.into_iter().next() else {
            continue;
        };
        let layout = StoreLayout::for_scope(&paths, row.scope).map_err(store_error)?;
        let Ok(slug) = Slug::parse(&row.slug) else {
            continue;
        };
        let source = layout.skill_dir(&slug);
        let mut skill_targets = Vec::new();
        let mut definition = None;
        let mut mcp_targets = Vec::new();
        match row.kind {
            ItemKind::Skill => {
                for agent in &selected_agents {
                    let Some(adapter) = registry.get(agent).ok().flatten() else {
                        continue;
                    };
                    let Some(candidate) = adapter
                        .paths(ItemKind::Skill, native_scope, &env)
                        .into_iter()
                        .find(|candidate| candidate.primary)
                    else {
                        continue;
                    };
                    let destination = candidate.path.join(slug.as_str());
                    let deployment = at_db::deployments::find_deployment(
                        database.connection(),
                        &item_id,
                        agent,
                        row.scope,
                        project_id.as_deref(),
                    )
                    .map_err(|error| db_error(error.to_string()))?;
                    let already_linked = deployment
                        .as_ref()
                        .is_some_and(|cell| cell.state == at_model::ItemState::Linked);
                    let destination_managed = deployment.as_ref().is_some_and(|cell| {
                        cell.state == at_model::ItemState::Linked
                            && cell.target_path.as_deref()
                                == Some(destination.to_string_lossy().as_ref())
                    });
                    skill_targets.push(SkillTarget {
                        agent: agent.clone(),
                        destination,
                        destination_managed,
                        already_linked,
                        mode: settings.link_mode(agent),
                        link_supported: adapter.capabilities(&env).supports_symlink,
                    });
                }
            }
            ItemKind::McpServer => {
                definition = at_store::mcp_store::McpStore::load(&layout)
                    .ok()
                    .and_then(|store| store.store.get(&row.slug).cloned());
                for agent in &selected_agents {
                    let Some(adapter) = registry.get(agent).ok().flatten() else {
                        continue;
                    };
                    let Some(candidate) = adapter
                        .paths(ItemKind::McpServer, native_scope, &env)
                        .into_iter()
                        .find(|candidate| candidate.primary)
                    else {
                        continue;
                    };
                    mcp_targets.push((agent.clone(), candidate.path));
                }
            }
            _ => continue,
        }
        facts.insert(
            item_id.to_string(),
            MemberFacts {
                item_id,
                kind: row.kind,
                scope: row.scope,
                slug: row.slug.clone(),
                name: row.name.clone(),
                layout,
                source,
                skill_targets,
                definition,
                mcp_targets,
            },
        );
    }
    let prior: BTreeMap<String, bool> = facts
        .iter()
        .map(|(item_id, member)| {
            let enabled = member.kind == ItemKind::Skill
                && !member.skill_targets.is_empty()
                && member
                    .skill_targets
                    .iter()
                    .all(|target| target.already_linked);
            (item_id.clone(), enabled)
        })
        .collect();
    let overlay = at_engine::presets::NativeFileOverlay::default();
    let plan = {
        let service = at_engine::presets::PresetService::new(&mut database);
        service
            .plan_apply_with(&preset_id, target.clone(), overlay, |member, overlay| {
                let prior_enabled = prior.get(&member.item_id).copied().unwrap_or(false);
                if prior_enabled {
                    return Ok(at_engine::presets::MemberTransition {
                        item_id: member.item_id.clone(),
                        prior_enabled: Some(true),
                        change_set: None,
                    });
                }
                let Some(member_facts) = facts.get(&member.item_id) else {
                    return Err(String::from("preset member is no longer indexed"));
                };
                let change_set = match member_facts.kind {
                    ItemKind::Skill => plan_preset_skill(project_id.as_deref(), member_facts)?,
                    ItemKind::McpServer => plan_preset_mcp(
                        &registry,
                        &env,
                        native_scope,
                        project_id.as_deref(),
                        member_facts,
                        overlay,
                    )?,
                    _ => return Err(String::from("presets accept skills and MCP servers only")),
                };
                Ok(at_engine::presets::MemberTransition {
                    item_id: member.item_id.clone(),
                    prior_enabled: Some(false),
                    change_set: Some(change_set),
                })
            })
            .map_err(preset_error)?
    };
    let ratio_before = ratio_json(&plan.ratio_before);
    let ratio_after = ratio_json(&plan.ratio_after);
    if globals.dry_run {
        return Ok(CommandOutput::new(
            json!({
                "preset_id": preset_id,
                "target": target_json(&plan.target),
                "dry_run": true,
                "ratio_before": ratio_before,
                "ratio_after": ratio_after,
                "changed_members": plan.transitions.iter().filter(|transition| transition.change_set.is_some()).count(),
            }),
            format!(
                "Would apply preset: {}/{} -> {}/{}\n",
                ratio_before["enabled"],
                ratio_before["total"],
                ratio_after["enabled"],
                ratio_after["total"]
            ),
        ));
    }
    let Some(set) = plan.change_set.as_ref() else {
        return Ok(CommandOutput::new(
            json!({
                "preset_id": preset_id,
                "target": target_json(&plan.target),
                "changed": false,
                "ratio_before": ratio_before,
                "ratio_after": ratio_after,
            }),
            "Preset already applied; nothing changed\n",
        ));
    };
    let roots =
        at_fs::ApplyRoots::new(authorized_roots(&paths, paths.support_dir())).map_err(|_| {
            CliError::new(
                ErrorCode::Conflict,
                "authorized configuration roots are unavailable",
                "check that the home, store and project folders exist",
            )
        })?;
    let outcome = {
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
        at_fs::apply_with_roots(set, &mut context, &roots).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("the preset could not be applied: {error}"),
                "review the current file state and retry",
            )
        })?
    };
    let mut service = at_engine::presets::PresetService::new(&mut database);
    service
        .record_apply(&plan.preset_id, &plan.target, &outcome, &plan.transitions)
        .map_err(preset_error)?;
    Ok(CommandOutput::new(
        json!({
            "preset_id": plan.preset_id,
            "target": target_json(&plan.target),
            "changed": true,
            "operation_id": outcome.operation_id,
            "changed_members": plan.transitions.iter().filter(|transition| transition.change_set.is_some()).count(),
            "ratio_before": ratio_before,
            "ratio_after": ratio_after,
        }),
        format!(
            "Applied preset: {}/{} -> {}/{}\n",
            ratio_before["enabled"],
            ratio_before["total"],
            ratio_after["enabled"],
            ratio_after["total"]
        ),
    ))
}

/// Plans one skill member for every selected agent, preserving engine operation order.
fn plan_preset_skill(
    project_id: Option<&str>,
    facts: &MemberFacts,
) -> Result<at_model::ChangeSet, String> {
    if !facts.source.is_dir() {
        return Err(format!(
            "the canonical skill package is missing at {}",
            facts.source.display()
        ));
    }
    let mut combined: Option<at_model::ChangeSet> = None;
    for target in &facts.skill_targets {
        if target.already_linked {
            continue;
        }
        let planned = at_engine::deploy::plan(at_engine::deploy::PlanRequest {
            item_id: facts.item_id,
            scope: facts.scope,
            agent: target.agent.clone(),
            store_root: facts.layout.root().to_path_buf(),
            source: facts.source.clone(),
            destination: target.destination.clone(),
            mode: target.mode,
            link_supported: target.link_supported,
            managed: true,
            destination_managed: target.destination_managed,
            shared_agents: Vec::new(),
            project_id: project_id.map(ToOwned::to_owned),
            summary: format!("Enable {} for {}", facts.name, target.agent),
        })
        .map_err(|error| error.to_string())?;
        if let Some(mut set) = planned.change_set {
            for op in &mut set.ops {
                if let at_model::FileOp::Symlink { target, .. } = op
                    && target.is_relative()
                {
                    *target = facts.layout.root().join(&*target);
                }
            }
            combined = Some(match combined {
                Some(mut existing) => {
                    existing.ops.extend(set.ops);
                    existing
                }
                None => set,
            });
        }
    }
    combined.ok_or_else(|| String::from("no selected agent has a skill root for this target"))
}

/// Plans one MCP member for every selected agent against the shared native overlay.
fn plan_preset_mcp(
    registry: &AgentAdapterRegistry,
    env: &AgentEnvironment,
    native_scope: Scope,
    project_id: Option<&str>,
    facts: &MemberFacts,
    overlay: &mut at_engine::presets::NativeFileOverlay,
) -> Result<at_model::ChangeSet, String> {
    let definition = facts
        .definition
        .as_ref()
        .ok_or_else(|| String::from("the canonical MCP definition is missing"))?;
    let mut combined: Option<at_model::ChangeSet> = None;
    for (agent, path) in &facts.mcp_targets {
        let bytes = overlay
            .get(path)
            .map(<[u8]>::to_vec)
            .or_else(|| std::fs::read(path).ok());
        let source = bytes
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        let expected = match &bytes {
            Some(bytes) => at_model::ExpectedState::hash(at_fs::sha256_hex(bytes))
                .map_err(|_| String::from("the native MCP document hash is invalid"))?,
            None => at_model::ExpectedState::absent(),
        };
        let planned = at_engine::mcp_plan::plan(at_engine::mcp_plan::McpPlanRequest {
            registry,
            env,
            intent: &at_engine::mcp_plan::McpIntent::SetEnabledForAgent {
                agent: agent.clone(),
                enabled: true,
            },
            targets: &[at_engine::mcp_plan::McpTarget {
                agent: agent.clone(),
                scope: native_scope,
                item_id: facts.item_id,
                slug: facts.slug.clone(),
                path: path.clone(),
                source,
                expected,
                unsupported_acknowledged: false,
                installed: true,
            }],
            definition,
            scope: facts.scope,
            project_id: project_id.map(ToOwned::to_owned),
            locked_reason: None,
            store_layout: Some(&facts.layout),
            target_store_layout: None,
        })
        .map_err(|error| error.to_string())?;
        for set in planned.apply_order() {
            combined = Some(match combined {
                Some(mut existing) => {
                    existing.ops.extend(set.ops.clone());
                    existing
                }
                None => set.clone(),
            });
        }
    }
    combined.ok_or_else(|| String::from("no selected agent has a native MCP document"))
}

fn ratio_json(ratio: &at_db::presets::AppliedRatio) -> Value {
    json!({
        "enabled": ratio.enabled,
        "total": ratio.total,
        "complete": ratio.complete,
    })
}

fn target_json(target: &at_engine::presets::Target) -> Value {
    match target {
        at_engine::presets::Target::Agent(agent) => {
            json!({ "kind": "agent", "id": agent.to_string() })
        }
        at_engine::presets::Target::Repo(id) => json!({ "kind": "project", "id": id }),
        _ => json!({ "kind": "unknown", "id": "" }),
    }
}

/// Maps preset planner failures onto the published error classes.
fn preset_error(error: at_engine::presets::PresetServiceError) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        error.to_string(),
        "inspect the preset members, their locks and the native files, then retry",
    )
}

/// Roots a reviewed skill or preset write may touch.
fn authorized_roots(paths: &AppPaths, extra: &Path) -> Vec<PathBuf> {
    let mut roots = vec![paths.support_dir().to_path_buf()];
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    roots.push(paths.global_store().to_path_buf());
    roots.push(paths.user_store().to_path_buf());
    if let Some(store) = paths.project_store()
        && let Some(project) = store.parent()
    {
        roots.push(project.to_path_buf());
    }
    roots.push(extra.to_path_buf());
    roots.retain(|path| path.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}
