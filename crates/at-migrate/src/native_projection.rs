//! Read native MCP/rule sources and prepare canonical and native write payloads.
//!
//! This module is deliberately read-only. It does not create directories or write files; every
//! returned path, byte vector and expected state is owned so the apply layer can append these
//! writes to its single journalled `ChangeSet`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use at_adapters::adapter::{AgentAdapterRegistry, AgentDeployment, AgentError};
use at_adapters::mcp_projection::{
    McpProjectionError, McpProjectionOperation, McpProjectionRequest,
};
use at_model::{AgentId, ExpectedState, ItemKind, McpServerDef, McpTransport, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::mcp_store::{McpStore, McpStoreError};

use crate::plan::{MigrationAction, MigrationPlan, PlanStep};

/// One write prepared for a later `ChangeSet`.
///
/// `path` is the absolute destination observed by this helper. `expected` describes the exact
/// destination state observed before any write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWrite {
    /// Absolute destination path.
    pub path: PathBuf,
    /// Bytes to write at `path`.
    pub bytes: Vec<u8>,
    /// Destination state required before writing.
    pub expected: ExpectedState,
}

/// Metadata identifying one item represented by a prepared projection batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedProjectionItem {
    /// Item kind selected by the migration plan.
    pub kind: ItemKind,
    /// Agent format used for native parsing and projection.
    pub agent: AgentId,
    /// Name/slug selected for the migrated item.
    pub item_name: String,
    /// Original native MCP server key, or source rule name.
    pub native_item_name: String,
    /// Native source path whose bytes were read.
    pub source_path: PathBuf,
}

/// Canonical-store and native writes prepared for one or more migration items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedNativeProjectionBatch {
    /// Item identities and source paths covered by the writes.
    pub items: Vec<PreparedProjectionItem>,
    /// Canonical store writes, deduplicated by path.
    pub canonical_writes: Vec<PreparedWrite>,
    /// Native adapter-projected writes, deduplicated by path.
    pub native_writes: Vec<PreparedWrite>,
}

/// Prepared canonical-store and adapter-projected native writes for one migration item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedNativeProjection {
    /// Item kind selected by the migration plan.
    pub kind: ItemKind,
    /// Agent format used for native parsing and projection.
    pub agent: AgentId,
    /// Name/slug selected for the migrated item.
    pub item_name: String,
    /// Original native MCP server key, or source rule name.
    pub native_item_name: String,
    /// Native source path whose bytes were read.
    pub source_path: PathBuf,
    /// Canonical store file write.
    pub canonical: PreparedWrite,
    /// Native configuration write projected through the agent adapter.
    pub native: PreparedWrite,
}

/// Inputs for preparing one `RenderMcp` or `RenderRule` plan step.
pub struct NativeProjectionRequest<'a> {
    /// Reviewed plan containing `step`.
    pub plan: &'a MigrationPlan,
    /// The selected projection step.
    pub step: &'a PlanStep,
    /// Canonical store root receiving the imported item.
    pub store_root: &'a Path,
    /// Native source path; must equal `step.source`.
    pub native_source_path: &'a Path,
    /// Verified adapter that owns the native source.
    pub agent: &'a AgentId,
    /// Selected canonical item name/slug.
    pub item_name: &'a str,
    /// Original native MCP server key (unused for rule parsing).
    pub native_item_name: &'a str,
    /// Project root used by Claude's project-level MCP map, when applicable.
    pub project: Option<&'a Path>,
}

/// Failures while preparing canonical and native migration writes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NativeProjectionError {
    /// The request batch is empty.
    #[error("native projection batch is empty")]
    EmptyBatch,
    /// A step is not one of its plan's `RenderMcp` or `RenderRule` operations.
    #[error("migration step is not a member RenderMcp or RenderRule operation")]
    InvalidStep,
    /// Steps in one batch do not share the same plan and store root.
    #[error("native projection batch mixes plans or store roots")]
    BatchMismatch,
    /// The step has no source or destination path, or its path does not match this request.
    #[error("migration step paths do not match the requested source and store destination")]
    PathMismatch,
    /// More than one selected item would write the same canonical name.
    #[error("native projection batch selects the same item name more than once: {0}")]
    DuplicateSelection(String),
    /// A native path was selected for incompatible projections or multiple rule contents.
    #[error("native projection batch has incompatible writes for `{0}`")]
    NativePathConflict(PathBuf),
    /// The selected item name is not a valid canonical store slug.
    #[error(transparent)]
    Slug(#[from] at_store::layout::SlugError),
    /// A source or destination could not be inspected or read.
    #[error("cannot read migration file `{path}`: {source}")]
    FileRead {
        /// File path involved.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// A source or destination is not a regular non-symlink file.
    #[error("migration file is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    /// Native source is not UTF-8 text required by its adapter format.
    #[error("native source is not valid UTF-8: {0}")]
    NativeNotUtf8(PathBuf),
    /// Source bytes no longer match the reviewed plan hash.
    #[error("native source changed since the migration plan was reviewed: {0}")]
    StaleSource(PathBuf),
    /// Selected server key does not exist in the native source.
    #[error("native MCP server `{name}` was not found in `{path}`")]
    MissingServer {
        /// Native server key requested.
        name: String,
        /// Native configuration path.
        path: PathBuf,
    },
    /// A native MCP document could not be parsed into the canonical transport model.
    #[error("cannot parse {agent} MCP source `{path}`: {message}")]
    NativeMcp {
        /// Adapter that owns the source.
        agent: AgentId,
        /// Native configuration path.
        path: PathBuf,
        /// Safe parser or validation message.
        message: String,
    },
    /// Native MCP server has conflicting or missing transport fields.
    #[error("native MCP server `{name}` has no unambiguous supported transport")]
    InvalidCodexTransport {
        /// Native server key requested.
        name: String,
    },
    /// The selected agent has no verified native projection for this item kind.
    #[error("native {kind} projection is unsupported for agent {agent}")]
    UnsupportedAgent {
        /// Adapter identifier.
        agent: AgentId,
        /// Item kind requested.
        kind: ItemKind,
    },
    /// Canonical MCP store parsing or serialization failed.
    #[error(transparent)]
    McpStore(#[from] McpStoreError),
    /// The adapter refused MCP projection or managed-rule rendering.
    #[error(transparent)]
    Adapter(#[from] AgentError),
    /// The MCP projection API refused this agent format or document.
    #[error(transparent)]
    McpProjection(#[from] McpProjectionError),
    /// Expected-state construction failed for an observed file hash.
    #[error(transparent)]
    ExpectedState(#[from] at_model::ChangeSetError),
}

/// One source file and its expected state, read once per batch.
struct SourceState {
    bytes: Vec<u8>,
    hash: String,
    expected: ExpectedState,
}

/// A validated item request with its paths and projection context owned.
struct WorkItem {
    kind: ItemKind,
    agent: AgentId,
    item_name: String,
    native_item_name: String,
    source_path: PathBuf,
    destination: PathBuf,
    scope: Scope,
    project: Option<PathBuf>,
}

/// A native source group; multiple MCP items can share one config document.
struct NativeGroup {
    kind: ItemKind,
    agent: AgentId,
    scope: Scope,
    project: Option<PathBuf>,
    items: Vec<usize>,
}

/// Prepares the canonical and native writes for one projection step.
///
/// This convenience wrapper has the same behavior as [`prepare_native_projections`] with one
/// request. The batch function should be used when multiple MCP servers are selected from the
/// same native config or store.
///
/// # Errors
/// Returns [`NativeProjectionError`] for invalid, stale or unsupported input and adapter errors.
pub fn prepare_native_projection(
    request: NativeProjectionRequest<'_>,
) -> Result<PreparedNativeProjection, NativeProjectionError> {
    let batch = prepare_native_projections(std::slice::from_ref(&request))?;
    let item = batch
        .items
        .into_iter()
        .next()
        .ok_or(NativeProjectionError::InvalidStep)?;
    let mut canonical_writes = batch.canonical_writes.into_iter();
    let canonical = canonical_writes
        .next()
        .ok_or(NativeProjectionError::InvalidStep)?;
    let mut native_writes = batch.native_writes.into_iter();
    let native = native_writes
        .next()
        .ok_or(NativeProjectionError::InvalidStep)?;
    if canonical_writes.next().is_some() || native_writes.next().is_some() {
        return Err(NativeProjectionError::NativePathConflict(item.source_path));
    }
    Ok(PreparedNativeProjection {
        kind: item.kind,
        agent: item.agent,
        item_name: item.item_name,
        native_item_name: item.native_item_name,
        source_path: item.source_path,
        canonical,
        native,
    })
}

/// Prepares one batch of canonical store changes and native adapter projections without writes.
///
/// Each MCP store document is loaded once and receives every selected server before a single
/// canonical `mcp.json` write is returned. Selected entries sharing a native MCP config are
/// projected sequentially into one final native write, so earlier entries are not overwritten.
/// Rule content is stored under its selected `rules/<slug>.md` destination and rendered through
/// its adapter's managed-rule API.
///
/// MCP projection supports Claude Code, Codex and omp. Rule projection supports the registered
/// Markdown rule renderers for Claude Code, Codex, omp and Cursor. Other agents are rejected.
///
/// # Errors
/// Returns [`NativeProjectionError`] for invalid or mixed batches, stale or unsafe files, invalid
/// slugs, unsupported adapters, invalid native formats, or failed canonical/adaptor projection.
pub fn prepare_native_projections(
    requests: &[NativeProjectionRequest<'_>],
) -> Result<PreparedNativeProjectionBatch, NativeProjectionError> {
    let first = requests.first().ok_or(NativeProjectionError::EmptyBatch)?;
    let mut sources = BTreeMap::<PathBuf, SourceState>::new();
    let mut work = Vec::with_capacity(requests.len());
    let mut selected = BTreeSet::<(ItemKind, String)>::new();

    for request in requests {
        if request.plan != first.plan || request.store_root != first.store_root {
            return Err(NativeProjectionError::BatchMismatch);
        }
        let step = request.step;
        if !request.plan.steps.contains(step)
            || !matches!(
                step.action,
                MigrationAction::RenderMcp | MigrationAction::RenderRule
            )
        {
            return Err(NativeProjectionError::InvalidStep);
        }
        if step.source.as_deref() != Some(request.native_source_path) {
            return Err(NativeProjectionError::PathMismatch);
        }
        let destination = step
            .destination
            .as_deref()
            .ok_or(NativeProjectionError::PathMismatch)?;
        let slug = Slug::parse(&request.item_name.to_ascii_lowercase())?;
        let kind = match step.action {
            MigrationAction::RenderMcp => ItemKind::McpServer,
            MigrationAction::RenderRule => ItemKind::Rule,
            _ => return Err(NativeProjectionError::InvalidStep),
        };
        let expected_relative = match kind {
            ItemKind::McpServer => Path::new("mcp.json").to_path_buf(),
            ItemKind::Rule => Path::new("rules").join(format!("{}.md", slug.as_str())),
            ItemKind::Skill => return Err(NativeProjectionError::InvalidStep),
        };
        let relative_destination = destination
            .strip_prefix(request.store_root)
            .map_err(|_| NativeProjectionError::PathMismatch)?;
        if relative_destination != expected_relative {
            return Err(NativeProjectionError::PathMismatch);
        }
        if !selected.insert((kind, slug.as_str().to_owned())) {
            return Err(NativeProjectionError::DuplicateSelection(
                slug.as_str().to_owned(),
            ));
        }

        let source_path = request.native_source_path.to_path_buf();
        if !sources.contains_key(&source_path) {
            let (bytes, expected) = read_regular_file(&source_path)?;
            let hash = at_fs::sha256_hex(&bytes);
            sources.insert(
                source_path.clone(),
                SourceState {
                    bytes,
                    hash,
                    expected,
                },
            );
        }
        let source = sources
            .get(&source_path)
            .ok_or(NativeProjectionError::PathMismatch)?;
        if step.expected_source_hash.as_deref() != Some(source.hash.as_str()) {
            return Err(NativeProjectionError::StaleSource(source_path));
        }
        work.push(WorkItem {
            kind,
            agent: request.agent.clone(),
            item_name: slug.as_str().to_owned(),
            native_item_name: request.native_item_name.to_owned(),
            source_path,
            destination: destination.to_path_buf(),
            scope: step.scope,
            project: request.project.map(Path::to_path_buf),
        });
    }

    let mut groups = BTreeMap::<PathBuf, NativeGroup>::new();
    for (index, item) in work.iter().enumerate() {
        match groups.get_mut(&item.source_path) {
            Some(group)
                if group.kind == item.kind
                    && group.agent == item.agent
                    && group.scope == item.scope
                    && group.project == item.project =>
            {
                group.items.push(index);
            }
            Some(_) => {
                return Err(NativeProjectionError::NativePathConflict(
                    item.source_path.clone(),
                ));
            }
            None => {
                groups.insert(
                    item.source_path.clone(),
                    NativeGroup {
                        kind: item.kind,
                        agent: item.agent.clone(),
                        scope: item.scope,
                        project: item.project.clone(),
                        items: vec![index],
                    },
                );
            }
        }
    }

    let has_mcp = work.iter().any(|item| item.kind == ItemKind::McpServer);
    let mut mcp_store = if has_mcp {
        let layout = StoreLayout::new(first.plan.scope, first.store_root);
        let loaded = McpStore::load(&layout)?;
        let expected = if loaded.absent {
            ExpectedState::Absent
        } else {
            ExpectedState::hash(loaded.hash.to_string())?
        };
        Some((loaded.store, expected, layout.mcp_file()))
    } else {
        None
    };
    let mut canonical_writes = BTreeMap::<PathBuf, PreparedWrite>::new();
    let mut native_writes = BTreeMap::<PathBuf, PreparedWrite>::new();
    let registry = AgentAdapterRegistry::register();

    for (source_path, mut group) in groups {
        group
            .items
            .sort_by(|left, right| work[*left].item_name.cmp(&work[*right].item_name));
        let source = sources
            .get(&source_path)
            .ok_or(NativeProjectionError::PathMismatch)?;
        match group.kind {
            ItemKind::McpServer => {
                if !matches!(
                    &group.agent,
                    AgentId::ClaudeCode | AgentId::Codex | AgentId::Omp
                ) {
                    return Err(NativeProjectionError::UnsupportedAgent {
                        agent: group.agent,
                        kind: ItemKind::McpServer,
                    });
                }
                let text = std::str::from_utf8(&source.bytes)
                    .map_err(|_| NativeProjectionError::NativeNotUtf8(source_path.clone()))?;
                let names = group
                    .items
                    .iter()
                    .map(|index| work[*index].native_item_name.clone())
                    .collect::<Vec<_>>();
                let mut parsed = parse_native_mcp_entries(
                    &group.agent,
                    group.scope,
                    &source_path,
                    group.project.as_deref(),
                    text,
                    &names,
                )?;
                let mut current = source.bytes.clone();
                for index in &group.items {
                    let item = &work[*index];
                    let entry = parsed
                        .remove(&item.native_item_name)
                        .ok_or_else(|| missing_server(&source_path, &item.native_item_name))?;
                    let store = mcp_store
                        .as_mut()
                        .ok_or(NativeProjectionError::InvalidStep)?;
                    store.0.set(
                        item.item_name.clone(),
                        McpServerDef {
                            display_name: item.item_name.clone(),
                            description: None,
                            transport: entry.transport.clone(),
                            extra: entry.extra.clone(),
                        },
                    );
                    let current_text = String::from_utf8(current)
                        .map_err(|_| NativeProjectionError::NativeNotUtf8(source_path.clone()))?;
                    current = at_adapters::mcp_projection::project(
                        &group.agent,
                        &McpProjectionRequest {
                            source: current_text,
                            path: source_path.clone(),
                            scope: group.scope,
                            project: group.project.clone(),
                            operation: McpProjectionOperation::Upsert {
                                name: item.item_name.clone(),
                                transport: Box::new(entry.transport),
                                enabled: entry.enabled,
                                extra: entry.extra,
                                env_vars: entry.env_vars,
                            },
                        },
                    )?;
                }
                verify_source_unchanged(&source_path, &source.bytes)?;
                insert_write(
                    &mut native_writes,
                    PreparedWrite {
                        path: source_path.clone(),
                        bytes: current,
                        expected: source.expected.clone(),
                    },
                )?;
            }
            ItemKind::Rule => {
                if group.items.len() != 1 {
                    return Err(NativeProjectionError::NativePathConflict(source_path));
                }
                if !matches!(
                    &group.agent,
                    AgentId::ClaudeCode | AgentId::Codex | AgentId::Omp | AgentId::Cursor
                ) {
                    return Err(NativeProjectionError::UnsupportedAgent {
                        agent: group.agent,
                        kind: ItemKind::Rule,
                    });
                }
                let index = group
                    .items
                    .first()
                    .copied()
                    .ok_or(NativeProjectionError::InvalidStep)?;
                let item = &work[index];
                let expected_store = expected_for_destination(&item.destination)?;
                let native_bytes = render_rule_or_adopt(
                    &registry,
                    &item.agent,
                    item.scope,
                    &source_path,
                    &source.bytes,
                )?;
                verify_source_unchanged(&source_path, &source.bytes)?;
                insert_write(
                    &mut canonical_writes,
                    PreparedWrite {
                        path: item.destination.clone(),
                        bytes: source.bytes.clone(),
                        expected: expected_store,
                    },
                )?;
                insert_write(
                    &mut native_writes,
                    PreparedWrite {
                        path: source_path.clone(),
                        bytes: native_bytes,
                        expected: source.expected.clone(),
                    },
                )?;
            }
            ItemKind::Skill => return Err(NativeProjectionError::InvalidStep),
        }
    }

    if let Some((store, expected, path)) = mcp_store {
        insert_write(
            &mut canonical_writes,
            PreparedWrite {
                path,
                bytes: store.to_bytes()?,
                expected,
            },
        )?;
    }
    let items = work
        .into_iter()
        .map(|item| PreparedProjectionItem {
            kind: item.kind,
            agent: item.agent,
            item_name: item.item_name,
            native_item_name: item.native_item_name,
            source_path: item.source_path,
        })
        .collect();
    Ok(PreparedNativeProjectionBatch {
        items,
        canonical_writes: canonical_writes.into_values().collect(),
        native_writes: native_writes.into_values().collect(),
    })
}

struct ParsedMcp {
    transport: McpTransport,
    extra: BTreeMap<String, serde_json::Value>,
    enabled: bool,
    env_vars: Vec<String>,
}

fn parse_native_mcp_entries(
    agent: &AgentId,
    scope: Scope,
    path: &Path,
    project: Option<&Path>,
    source: &str,
    names: &[String],
) -> Result<BTreeMap<String, ParsedMcp>, NativeProjectionError> {
    match agent {
        AgentId::ClaudeCode => {
            use at_adapters::claude_mcp::{ClaudeConfigScope, ClaudeMcpConfig};
            let location = if scope == Scope::User {
                ClaudeConfigScope::User
            } else if path.file_name().is_some_and(|name| name == ".mcp.json") {
                ClaudeConfigScope::ProjectFile
            } else {
                let project_path = project.ok_or_else(|| NativeProjectionError::NativeMcp {
                    agent: agent.clone(),
                    path: path.to_path_buf(),
                    message: String::from("project path is required for this Claude MCP source"),
                })?;
                ClaudeConfigScope::Project {
                    project_path: project_path.to_string_lossy().into_owned(),
                }
            };
            let config = ClaudeMcpConfig::parse(source, location)
                .map_err(|error| native_mcp_error(agent, path, error.to_string()))?;
            let mut parsed = BTreeMap::new();
            for name in names {
                let server = config
                    .servers()
                    .get(name)
                    .ok_or_else(|| missing_server(path, name))?;
                parsed.insert(
                    name.clone(),
                    ParsedMcp {
                        transport: server.transport.clone(),
                        extra: server.extra.clone(),
                        enabled: true,
                        env_vars: Vec::new(),
                    },
                );
            }
            Ok(parsed)
        }
        AgentId::Codex => {
            use at_adapters::codex_mcp::CodexConfigFile;
            let config = CodexConfigFile::parse(source)
                .map_err(|error| native_mcp_error(agent, path, error.to_string()))?;
            let mut parsed = BTreeMap::new();
            for name in names {
                let server = config
                    .server(name)
                    .map_err(|error| native_mcp_error(agent, path, error.to_string()))?
                    .ok_or_else(|| missing_server(path, name))?;
                let entry = server.entry;
                let transport = match (entry.command, entry.url) {
                    (Some(command), None) => McpTransport::Stdio {
                        command,
                        args: entry.args.unwrap_or_default(),
                        cwd: entry.cwd,
                        env: entry.env.unwrap_or_default(),
                    },
                    (None, Some(url)) => serde_json::from_value(serde_json::json!({
                        "type": "http",
                        "url": url,
                        "headers": {},
                        "oauth": null
                    }))
                    .map_err(|_| {
                        NativeProjectionError::InvalidCodexTransport { name: name.clone() }
                    })?,
                    _ => {
                        return Err(NativeProjectionError::InvalidCodexTransport {
                            name: name.clone(),
                        });
                    }
                };
                parsed.insert(
                    name.clone(),
                    ParsedMcp {
                        transport,
                        extra: BTreeMap::new(),
                        enabled: entry.enabled.unwrap_or(true),
                        env_vars: entry.env_vars.unwrap_or_default(),
                    },
                );
            }
            Ok(parsed)
        }
        AgentId::Omp => {
            let config = at_adapters::omp_mcp::OmpMcpConfig::parse(source)
                .map_err(|error| native_mcp_error(agent, path, error.to_string()))?;
            let mut parsed = BTreeMap::new();
            for name in names {
                let server = config
                    .server(name)
                    .ok_or_else(|| missing_server(path, name))?;
                parsed.insert(
                    name.clone(),
                    ParsedMcp {
                        transport: server.transport.clone(),
                        extra: server.extra.clone(),
                        enabled: server.enabled,
                        env_vars: Vec::new(),
                    },
                );
            }
            Ok(parsed)
        }
        _ => Err(NativeProjectionError::UnsupportedAgent {
            agent: agent.clone(),
            kind: ItemKind::McpServer,
        }),
    }
}

/// Renders one rule through its adapter, adopting an unmarked native file first.
///
/// A rule file without an AgenTuna managed block is unmanaged content: the adapter refuses to
/// replace it. Adoption wraps the existing bytes in a managed block instead of discarding
/// them, which is what makes the file AgenTuna-owned from then on; the same bytes are stored
/// canonically by the caller.
fn render_rule_or_adopt(
    registry: &AgentAdapterRegistry,
    agent: &AgentId,
    scope: Scope,
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<u8>, NativeProjectionError> {
    let deployment = AgentDeployment {
        kind: ItemKind::Rule,
        scope,
        path: path.to_path_buf(),
        content: bytes.to_vec(),
    };
    match registry.render_io(agent, &deployment) {
        Ok(rendered) => Ok(rendered),
        Err(AgentError::RuleRender(at_fs::managed_block::ManagedBlockError::AdoptRequired {
            ..
        })) => at_fs::managed_block::render_managed_block(path, None, bytes)
            .map_err(|error| NativeProjectionError::Adapter(AgentError::RuleRender(error))),
        Err(error) => Err(NativeProjectionError::Adapter(error)),
    }
}

fn read_regular_file(path: &Path) -> Result<(Vec<u8>, ExpectedState), NativeProjectionError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| NativeProjectionError::FileRead {
            path: path.to_path_buf(),
            source,
        })?;
    if !metadata.file_type().is_file() {
        return Err(NativeProjectionError::NotRegularFile(path.to_path_buf()));
    }
    let bytes = fs::read(path).map_err(|source| NativeProjectionError::FileRead {
        path: path.to_path_buf(),
        source,
    })?;
    let expected = ExpectedState::hash(at_fs::sha256_hex(&bytes))?;
    Ok((bytes, expected))
}

fn expected_for_destination(path: &Path) -> Result<ExpectedState, NativeProjectionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let bytes = fs::read(path).map_err(|source| NativeProjectionError::FileRead {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(ExpectedState::hash(at_fs::sha256_hex(&bytes))?)
        }
        Ok(_) => Err(NativeProjectionError::NotRegularFile(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ExpectedState::Absent),
        Err(source) => Err(NativeProjectionError::FileRead {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn verify_source_unchanged(path: &Path, original: &[u8]) -> Result<(), NativeProjectionError> {
    let (current, _) = read_regular_file(path)?;
    if current == original {
        Ok(())
    } else {
        Err(NativeProjectionError::StaleSource(path.to_path_buf()))
    }
}

fn insert_write(
    writes: &mut BTreeMap<PathBuf, PreparedWrite>,
    write: PreparedWrite,
) -> Result<(), NativeProjectionError> {
    let path = write.path.clone();
    if writes.insert(path.clone(), write).is_some() {
        return Err(NativeProjectionError::NativePathConflict(path));
    }
    Ok(())
}

fn native_mcp_error(agent: &AgentId, path: &Path, message: String) -> NativeProjectionError {
    NativeProjectionError::NativeMcp {
        agent: agent.clone(),
        path: path.to_path_buf(),
        message,
    }
}

fn missing_server(path: &Path, name: &str) -> NativeProjectionError {
    NativeProjectionError::MissingServer {
        name: name.to_owned(),
        path: path.to_path_buf(),
    }
}
