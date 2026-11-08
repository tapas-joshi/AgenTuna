//! Read-only discovery of native skills, MCP servers and rule files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_model::{AgentId, ItemKind, Scope};
use at_store::manifest::{hash_bytes, hash_mcp_entry, hash_skill_folder};
use serde_json::Value;

/// Ownership relationship to the AgenTuna store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Ownership {
    /// Native content hashes to the corresponding AgenTuna item.
    Managed,
    /// Native content is not present in AgenTuna.
    NotInAgenTuna,
    /// Native content differs from the matching store item.
    Conflict,
}

/// Scan processing status for one result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ParseStatus {
    /// Content was successfully read and identified.
    Parsed,
    /// Content could not be parsed or read.
    Error(String),
}

/// Secret-free summary of one MCP transport definition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct McpDefinitionSummary {
    /// Transport name such as `stdio`, `http` or `sse`.
    pub transport: String,
    /// Executable basename for stdio definitions.
    pub command: Option<String>,
    /// Number of arguments; argument values are never retained.
    pub argument_count: Option<usize>,
    /// URL host and path only; credentials, query and fragment are excluded.
    pub url: Option<String>,
    /// Environment variable names only.
    pub env_names: Vec<String>,
    /// HTTP header names only.
    pub header_names: Vec<String>,
}

/// Extracts a display-safe MCP definition summary without retaining secret values.
fn mcp_definition_summary(entry: &Value) -> McpDefinitionSummary {
    let transport = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if entry.get("command").is_some() {
                "stdio"
            } else {
                "http"
            }
        })
        .to_owned();
    let command = entry.get("command").and_then(Value::as_str).map(|value| {
        Path::new(value)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("command")
            .to_owned()
    });
    let argument_count = entry.get("args").and_then(Value::as_array).map(Vec::len);
    let url = entry.get("url").and_then(Value::as_str).and_then(safe_url);
    let env_names = object_keys(entry.get("env"));
    let header_names = object_keys(entry.get("headers"));
    McpDefinitionSummary {
        transport,
        command,
        argument_count,
        url,
        env_names,
        header_names,
    }
}

fn object_keys(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn safe_url(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host = authority.rsplit('@').next()?;
    let suffix = &rest[authority_end..];
    let path_end = suffix.find(['?', '#']).unwrap_or(suffix.len());
    Some(format!("{host}{}", &suffix[..path_end]))
}

/// A discovered native item or per-item discovery error.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanItem {
    /// Kind of discovered entry.
    pub kind: ItemKind,
    /// Name in the native configuration.
    pub name: String,
    /// Agents that can see this physical entry.
    pub agents: BTreeSet<AgentId>,
    /// Native path of the item or containing configuration file.
    pub path: PathBuf,
    /// Native scope inferred from its location.
    pub scope: Scope,
    /// SHA-256 content hash, when readable.
    pub content_hash: Option<String>,
    /// Kind-specific normalized hash for conflict detection.
    pub normalized_hash: Option<String>,
    /// Safe summary of an MCP definition; secret values are excluded.
    pub mcp_summary: Option<McpDefinitionSummary>,
    /// Parsing result.
    pub parse_status: ParseStatus,
    /// Whether this is a link into an AgenTuna store.
    pub store_link: bool,
    /// Ownership relative to the supplied store inventory.
    pub ownership: Ownership,
}

/// A discovery root or agent configuration file that could not be read.
///
/// Warnings name the exact path that failed so a partial scan can still list every item it
/// did find next to a row describing what was skipped.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanWarning {
    /// Root or configuration path that could not be read.
    pub path: PathBuf,
    /// Item kind being discovered when the failure happened.
    pub kind: ItemKind,
    /// Agent whose root or configuration failed.
    pub agent: AgentId,
    /// Scope of the failing path.
    pub scope: Scope,
    /// OS or parse error text.
    pub message: String,
}

/// Outcome of attempting to scan one adapter-provided discovery root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScanRootStatus {
    /// The root was read successfully and contained at least one item.
    Scanned,
    /// The root was read successfully and contained no items.
    Empty,
    /// The candidate root did not exist.
    NotPresent,
    /// The root could not be inspected safely.
    Unreadable,
}

/// One actual discovery candidate, shared by every agent that reads it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanRoot {
    /// Candidate folder or configuration document.
    pub path: PathBuf,
    /// Kind of native item sought at this candidate.
    pub kind: ItemKind,
    /// Agents that attempted to read this candidate.
    pub agents: BTreeSet<AgentId>,
    /// Outcome of the attempt.
    pub status: ScanRootStatus,
    /// Safe, content-free reason when the root could not be read.
    pub unreadable_reason: Option<String>,
    /// Number of distinct item rows found, including item-level failures.
    pub found_items: i64,
    /// Readable rows not already managed by AgenTuna.
    pub new_items: i64,
    /// Item rows skipped because their contents could not be read or parsed.
    pub skipped_items: i64,
    /// Readable rows already managed by AgenTuna.
    pub in_sync: i64,
}

/// Read-only results of one native scan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanReport {
    /// All discovered items and item errors, stable-sorted by path/kind/name.
    pub items: Vec<ScanItem>,
    /// True when one or more candidate paths failed to scan.
    pub partial: bool,
    /// Missing agent roots skipped by the scanner.
    pub skipped_roots: Vec<PathBuf>,
    /// Roots and configuration files that could not be read, naming each path.
    pub warnings: Vec<ScanWarning>,
    /// Outcomes for every candidate root the adapters actually attempted.
    pub roots: Vec<ScanRoot>,
}

/// One discovery step reported before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanProgress {
    /// Agent whose folders are being read.
    pub agent: AgentId,
    /// Scope being read.
    pub scope: Scope,
    /// Kind being read.
    pub kind: ItemKind,
    /// Steps finished before this one.
    pub completed: usize,
    /// Total steps this scan will attempt.
    pub total: usize,
}

/// Receives discovery progress and can stop a scan early.
pub trait ScanObserver {
    /// Called before each discovery step; return `false` to cancel.
    fn step(&mut self, progress: &ScanProgress) -> bool;
}

/// Observer that never cancels, used by [`scan`].
struct ScanEverything;

impl ScanObserver for ScanEverything {
    fn step(&mut self, _progress: &ScanProgress) -> bool {
        true
    }
}

/// Canonical store identity, location and content digest used to match native entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreItem {
    /// Managed item kind.
    pub kind: ItemKind,
    /// Canonical item name.
    pub name: String,
    /// Content digest.
    pub content_hash: String,
    /// Scope whose store holds the item.
    pub scope: Scope,
    /// Absolute path of the canonical item inside its store.
    pub path: PathBuf,
}

/// Inputs for a native scan. All paths are injected; no process-global home is consulted.
pub struct ScanRequest<'a> {
    /// Injected environment used by registered adapters.
    pub environment: &'a AgentEnvironment,
    /// Registry of adapters to scan.
    pub adapters: &'a AgentAdapterRegistry,
    /// Roots of AgenTuna stores used to identify internal links.
    pub store_roots: &'a [PathBuf],
    /// Managed item inventory across scopes.
    pub store_items: &'a [StoreItem],
}

/// Scopes every adapter is asked about, in scan order.
const SCOPES: [Scope; 3] = [Scope::User, Scope::Project, Scope::Local];
/// Item kinds read from agent configuration files.
const CONFIG_KINDS: [ItemKind; 2] = [ItemKind::McpServer, ItemKind::Rule];

/// Upper bound on bytes read from one native configuration document.
///
/// Agent configurations are hand-written documents; anything larger is not a document this scan
/// should load into memory, and skipping it with a warning keeps a pathological file from
/// dominating every scan.
const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

/// Upper bound on bytes read from one native skill tree while hashing it.
const MAX_SKILL_TREE_BYTES: u64 = 16 * 1024 * 1024;

/// Scans adapter-discovered roots and MCP configuration files without executing servers.
///
/// # Errors
/// Returns adapter registry failures; path and parse errors are represented per-item or as
/// [`ScanReport::warnings`] so scanning continues.
pub fn scan(request: ScanRequest<'_>) -> Result<ScanReport, ScanError> {
    scan_with(request, &mut ScanEverything)
}

/// Scans, reporting each discovery step and stopping when the observer returns `false`.
///
/// # Errors
/// [`ScanError::Cancelled`] when the observer stops the scan, plus adapter registry failures.
pub fn scan_with(
    request: ScanRequest<'_>,
    observer: &mut dyn ScanObserver,
) -> Result<ScanReport, ScanError> {
    let mut report = ScanReport {
        roots: Vec::new(),
        items: Vec::new(),
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let mut physical: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let allowed_roots = std::iter::once(&request.environment.home)
        .chain(request.environment.project.iter())
        .filter_map(|root| fs::canonicalize(root).ok())
        .collect::<Vec<_>>();
    let adapters = request.adapters.adapters().map_err(ScanError::Adapter)?;
    let total = adapters.len() * SCOPES.len() * (1 + CONFIG_KINDS.len());
    let mut completed = 0usize;
    for adapter in adapters {
        let agent = adapter.id();
        for scope in SCOPES {
            observe_step(
                observer,
                &agent,
                scope,
                ItemKind::Skill,
                &mut completed,
                total,
            )?;
            for candidate in adapter.paths(ItemKind::Skill, scope, request.environment) {
                if candidate.kind != "skills_dir" {
                    continue;
                }
                let Some(metadata) = prepare_root(
                    &mut report,
                    &candidate.path,
                    ItemKind::Skill,
                    &agent,
                    scope,
                    &allowed_roots,
                ) else {
                    continue;
                };
                if !metadata.is_dir() && !metadata.file_type().is_symlink() {
                    record_root_failure(
                        &mut report,
                        &candidate.path,
                        ItemKind::Skill,
                        &agent,
                        scope,
                        "skills root is not a directory",
                    );
                    continue;
                }
                let skills = match adapter.list_native_skills_including_managed(
                    request.environment,
                    request.store_roots,
                    candidate.clone(),
                ) {
                    Ok(items) => items,
                    Err(error) => {
                        let reason = match error {
                            at_adapters::adapter::AgentError::Permission { .. } => {
                                "permission denied"
                            }
                            at_adapters::adapter::AgentError::Io { ref source, .. }
                                if source.kind() == std::io::ErrorKind::PermissionDenied =>
                            {
                                "permission denied"
                            }
                            _ => "could not read discovery root",
                        };
                        record_root_failure(
                            &mut report,
                            &candidate.path,
                            ItemKind::Skill,
                            &agent,
                            scope,
                            reason,
                        );
                        continue;
                    }
                };
                for skill in skills {
                    if skill.path == candidate.path
                        && skill.state
                            == at_adapters::native_skills::NativeSkillState::PermissionDenied
                    {
                        record_root_failure(
                            &mut report,
                            &candidate.path,
                            ItemKind::Skill,
                            &agent,
                            scope,
                            "permission denied",
                        );
                        continue;
                    }
                    scan_skill(&mut report, &mut physical, skill, &request, &agent);
                }
            }
        }
        for scope in SCOPES {
            for kind in CONFIG_KINDS {
                observe_step(observer, &agent, scope, kind, &mut completed, total)?;
                for candidate in adapter.paths(kind, scope, request.environment) {
                    if scope == Scope::Project
                        && candidate.path.starts_with(&request.environment.home)
                        && !request
                            .environment
                            .project
                            .as_ref()
                            .is_some_and(|project| candidate.path.starts_with(project))
                    {
                        continue;
                    }
                    let Some(metadata) = prepare_root(
                        &mut report,
                        &candidate.path,
                        kind,
                        &agent,
                        scope,
                        &allowed_roots,
                    ) else {
                        continue;
                    };
                    if metadata.file_type().is_symlink() {
                        record_root_failure(
                            &mut report,
                            &candidate.path,
                            kind,
                            &agent,
                            scope,
                            "symbolic link to configuration is not followed",
                        );
                        continue;
                    }
                    if kind == ItemKind::Rule {
                        if metadata.is_dir() {
                            let entries = match fs::read_dir(&candidate.path) {
                                Ok(entries) => entries,
                                Err(error) => {
                                    record_root_failure(
                                        &mut report,
                                        &candidate.path,
                                        kind,
                                        &agent,
                                        scope,
                                        io_reason(&error),
                                    );
                                    continue;
                                }
                            };
                            for entry in entries {
                                match entry {
                                    Ok(entry)
                                        if entry
                                            .path()
                                            .extension()
                                            .is_some_and(|ext| ext == "md" || ext == "mdc") =>
                                    {
                                        scan_rule(
                                            &mut report,
                                            &mut physical,
                                            &entry.path(),
                                            agent.clone(),
                                            scope,
                                            request.store_items,
                                        )
                                    }
                                    Ok(_) => (),
                                    Err(error) => {
                                        record_root_failure(
                                            &mut report,
                                            &candidate.path,
                                            kind,
                                            &agent,
                                            scope,
                                            io_reason(&error),
                                        );
                                    }
                                }
                            }
                        } else if metadata.is_file() {
                            scan_rule(
                                &mut report,
                                &mut physical,
                                &candidate.path,
                                agent.clone(),
                                scope,
                                request.store_items,
                            );
                        }
                    } else if metadata.is_file() {
                        scan_mcp(
                            &mut report,
                            &candidate.path,
                            &agent,
                            scope,
                            candidate.format.as_deref(),
                            request.store_items,
                        );
                    }
                }
            }
        }
    }
    summarize_roots(&mut report);
    report
        .items
        .sort_by(|a, b| (&a.path, &a.kind, &a.name).cmp(&(&b.path, &b.kind, &b.name)));
    report.skipped_roots.sort();
    report.skipped_roots.dedup();
    report.warnings.sort_by(|left, right| {
        (&left.path, &left.kind, &left.agent, &left.message).cmp(&(
            &right.path,
            &right.kind,
            &right.agent,
            &right.message,
        ))
    });
    report
        .warnings
        .dedup_by(|left, right| left.path == right.path && left.message == right.message);
    Ok(report)
}

/// Reports one discovery step, returning [`ScanError::Cancelled`] when the observer stops.
fn observe_step(
    observer: &mut dyn ScanObserver,
    agent: &AgentId,
    scope: Scope,
    kind: ItemKind,
    completed: &mut usize,
    total: usize,
) -> Result<(), ScanError> {
    let progress = ScanProgress {
        agent: agent.clone(),
        scope,
        kind,
        completed: *completed,
        total,
    };
    *completed += 1;
    if observer.step(&progress) {
        Ok(())
    } else {
        Err(ScanError::Cancelled)
    }
}

/// Classifies an IO error without exposing paths or document content.
fn io_reason(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "permission denied",
        std::io::ErrorKind::NotFound => "not present",
        _ => "could not read discovery root",
    }
}

/// Records a candidate before reading it, refusing symlinked roots outside the injected roots.
fn prepare_root(
    report: &mut ScanReport,
    path: &Path,
    kind: ItemKind,
    agent: &AgentId,
    scope: Scope,
    allowed_roots: &[PathBuf],
) -> Option<fs::Metadata> {
    if let Some(root) = report
        .roots
        .iter_mut()
        .find(|root| root.path == path && root.kind == kind)
    {
        root.agents.insert(agent.clone());
    } else {
        report.roots.push(ScanRoot {
            path: path.to_path_buf(),
            kind,
            agents: BTreeSet::from([agent.clone()]),
            status: ScanRootStatus::Scanned,
            unreadable_reason: None,
            found_items: 0,
            new_items: 0,
            skipped_items: 0,
            in_sync: 0,
        });
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            report.skipped_roots.push(path.to_path_buf());
            if let Some(root) = report
                .roots
                .iter_mut()
                .find(|root| root.path == path && root.kind == kind)
                && root.status != ScanRootStatus::Unreadable
            {
                root.status = ScanRootStatus::NotPresent;
            }
            return None;
        }
        Err(error) => {
            record_root_failure(report, path, kind, agent, scope, io_reason(&error));
            return None;
        }
    };
    let resolved = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => {
            let reason = if metadata.file_type().is_symlink()
                && error.kind() == std::io::ErrorKind::NotFound
            {
                "symbolic link target is not present"
            } else {
                io_reason(&error)
            };
            record_root_failure(report, path, kind, agent, scope, reason);
            return None;
        }
    };
    // A configured custom-agent root may live outside the injected sandbox; only a link that
    // resolves outside it can escape, so the containment check applies to symlinked candidates.
    if metadata.file_type().is_symlink()
        && !allowed_roots.iter().any(|root| resolved.starts_with(root))
    {
        record_root_failure(
            report,
            path,
            kind,
            agent,
            scope,
            "symbolic link target is outside injected roots",
        );
        return None;
    }
    if let Some(root) = report
        .roots
        .iter_mut()
        .find(|root| root.path == path && root.kind == kind)
        && root.status == ScanRootStatus::NotPresent
    {
        root.status = ScanRootStatus::Scanned;
    }
    Some(metadata)
}

/// Preserves a partial result and a content-free reason for a failed root.
fn record_root_failure(
    report: &mut ScanReport,
    path: &Path,
    kind: ItemKind,
    agent: &AgentId,
    scope: Scope,
    reason: &str,
) {
    report.partial = true;
    if let Some(root) = report
        .roots
        .iter_mut()
        .find(|root| root.path == path && root.kind == kind)
    {
        root.status = ScanRootStatus::Unreadable;
        root.unreadable_reason = Some(reason.to_owned());
    }
    report.warnings.push(ScanWarning {
        path: path.to_path_buf(),
        kind,
        agent: agent.clone(),
        scope,
        message: reason.to_owned(),
    });
}

/// Adds one native skill, preserving physical deduplication across shared roots.
fn scan_skill(
    report: &mut ScanReport,
    physical: &mut BTreeMap<PathBuf, usize>,
    skill: at_adapters::native_skills::NativeSkill,
    request: &ScanRequest<'_>,
    agent: &AgentId,
) {
    let canonical = fs::canonicalize(&skill.path).unwrap_or_else(|_| skill.path.clone());
    if let Some(index) = physical.get(&canonical).copied() {
        report.items[index].agents.insert(agent.clone());
        return;
    }
    // Invalid links may point outside the sandbox; never hash their targets.
    let digest = if skill.valid {
        hash_skill_folder(&skill.path)
            .ok()
            .map(|hash| hash.to_string())
    } else {
        None
    };
    let raw_digest = if skill.valid {
        raw_skill_hash(&skill.path).ok()
    } else {
        None
    };
    let is_store_link = skill.target.as_ref().is_some_and(|target| {
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            skill
                .path
                .parent()
                .map_or_else(|| target.clone(), |parent| parent.join(target))
        };
        let resolved = fs::canonicalize(&resolved).unwrap_or(resolved);
        request.store_roots.iter().any(|root| {
            let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            resolved.starts_with(root)
        })
    });
    let status = if skill.valid && digest.is_some() && raw_digest.is_some() {
        ParseStatus::Parsed
    } else {
        report.partial = true;
        let reason = skill
            .error
            .unwrap_or_else(|| "unreadable skill content".to_owned());
        ParseStatus::Error(if reason.starts_with("invalid YAML frontmatter:") {
            "invalid YAML frontmatter".to_owned()
        } else {
            reason
        })
    };
    let ownership = if is_store_link {
        Ownership::Managed
    } else {
        ownership(
            ItemKind::Skill,
            &skill.name,
            digest.as_deref(),
            request.store_items,
        )
    };
    physical.insert(canonical, report.items.len());
    report.items.push(ScanItem {
        kind: ItemKind::Skill,
        name: skill.name,
        agents: BTreeSet::from([agent.clone()]),
        path: skill.path,
        scope: skill.scope,
        content_hash: raw_digest,
        normalized_hash: digest,
        mcp_summary: None,
        parse_status: status,
        store_link: is_store_link,
        ownership,
    });
}

/// Counts distinct rows against the adapter candidates, not against inferred item parents.
fn summarize_roots(report: &mut ScanReport) {
    let items = report
        .items
        .iter()
        .map(|item| {
            let parent = item
                .path
                .parent()
                .and_then(|path| fs::canonicalize(path).ok());
            let identity = fs::canonicalize(&item.path).unwrap_or_else(|_| item.path.clone());
            (item, parent, identity)
        })
        .collect::<Vec<_>>();
    for root in &mut report.roots {
        if let Some(warning) = report.warnings.iter().find(|warning| {
            warning.path == root.path
                && warning.kind == root.kind
                && root.agents.contains(&warning.agent)
        }) {
            root.status = ScanRootStatus::Unreadable;
            root.unreadable_reason = Some(warning.message.clone());
        }
        let canonical_root = fs::canonicalize(&root.path).ok();
        let mut seen = BTreeSet::new();
        for (item, canonical_parent, identity) in &items {
            if item.kind != root.kind || item.agents.is_disjoint(&root.agents) {
                continue;
            }
            let parent_matches = item.path.parent().is_some_and(|parent| parent == root.path)
                || canonical_root
                    .as_ref()
                    .is_some_and(|resolved| canonical_parent.as_ref() == Some(resolved));
            if item.path != root.path && !parent_matches {
                continue;
            }
            if !seen.insert((identity, &item.name)) {
                continue;
            }
            root.found_items += 1;
            if !matches!(item.parse_status, ParseStatus::Parsed) {
                root.skipped_items += 1;
                if item.path == root.path && root.kind == ItemKind::Rule {
                    root.status = ScanRootStatus::Unreadable;
                    root.unreadable_reason = Some("could not read rule document".to_owned());
                }
            } else if item.ownership == Ownership::Managed {
                root.in_sync += 1;
            } else {
                root.new_items += 1;
            }
        }
        if root.status == ScanRootStatus::Scanned && root.found_items == 0 {
            root.status = ScanRootStatus::Empty;
        }
    }
    report
        .roots
        .sort_by(|left, right| (&left.path, &left.kind).cmp(&(&right.path, &right.kind)));
}

/// Scan failure outside item-level filesystem or syntax errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScanError {
    /// Adapter enumeration failed.
    #[error(transparent)]
    Adapter(#[from] at_adapters::adapter::AgentError),
    /// The observer stopped the scan before all roots were read.
    #[error("native scan was cancelled")]
    Cancelled,
}

fn ownership(kind: ItemKind, name: &str, digest: Option<&str>, stores: &[StoreItem]) -> Ownership {
    match stores
        .iter()
        .find(|entry| entry.kind == kind && normalize(&entry.name) == normalize(name))
    {
        None => Ownership::NotInAgenTuna,
        Some(entry) if digest == Some(entry.content_hash.as_str()) => Ownership::Managed,
        Some(_) => Ownership::Conflict,
    }
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

fn raw_skill_hash(root: &Path) -> Result<String, std::io::Error> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "skill tree contains a symbolic link",
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
                if total > MAX_SKILL_TREE_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "skill tree is larger than the scan size limit",
                    ));
                }
                let relative = path
                    .strip_prefix(root)
                    .map_err(std::io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push((relative, fs::read(path)?));
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let size = files
        .iter()
        .map(|(path, bytes)| path.len() + bytes.len() + 2)
        .sum();
    let mut contents = Vec::with_capacity(size);
    for (path, bytes) in files {
        contents.extend_from_slice(path.as_bytes());
        contents.push(0);
        contents.extend_from_slice(&bytes);
        contents.push(b'\n');
    }
    Ok(hash_bytes(&contents).to_string())
}

fn normalize_rule(bytes: &[u8]) -> String {
    crate::normalize::rule_bytes(bytes).to_string()
}

/// Reads the MCP server map from a native configuration file, inferring the format.
///
/// `.toml` files (Codex) are read with `mcp_servers`; every other file is JSON with
/// `mcpServers`/`mcp_servers`. Only the server map is returned: comments and unrelated keys are
/// left alone because this function never writes.
///
/// # Errors
/// A human-readable parse error when the document is not valid JSON/TOML of that shape.
pub fn read_mcp_entries(path: &Path, bytes: &[u8]) -> Result<BTreeMap<String, Value>, String> {
    let format = path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| *extension == "toml");
    read_mcp_entries_with(bytes, format)
}

fn read_mcp_entries_with(
    bytes: &[u8],
    format: Option<&str>,
) -> Result<BTreeMap<String, Value>, String> {
    if format == Some("toml") {
        std::str::from_utf8(bytes)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                toml_edit::de::from_str::<BTreeMap<String, Value>>(text)
                    .map_err(|error| error.to_string())
            })
            .map(|mut document| {
                document
                    .remove("mcp_servers")
                    .and_then(|servers| match servers {
                        Value::Object(map) => Some(map.into_iter().collect()),
                        _ => None,
                    })
                    .unwrap_or_default()
            })
    } else {
        let source = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        let root =
            jsonc_parser::cst::CstRootNode::parse(source, &jsonc_parser::ParseOptions::default())
                .map_err(|error| error.to_string())?;
        let servers = root
            .object_value()
            .and_then(|object| {
                object
                    .get("mcpServers")
                    .or_else(|| object.get("mcp_servers"))
            })
            .and_then(|property| property.to_serde_value());
        Ok(match servers {
            Some(Value::Object(map)) => map.into_iter().collect(),
            _ => BTreeMap::new(),
        })
    }
}

/// Reads one agent configuration document, refusing one larger than [`MAX_CONFIG_BYTES`].
///
/// Returns the failure text so a caller can record a warning or item error and keep scanning;
/// an oversized or unreadable document never aborts the rest of the pass.
fn read_document(path: &Path) -> Result<Vec<u8>, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_CONFIG_BYTES => {
            return Err(format!(
                "the document is larger than the {} MiB scan limit",
                MAX_CONFIG_BYTES / (1024 * 1024)
            ));
        }
        Ok(_) => {}
        Err(error) => return Err(io_reason(&error).to_owned()),
    }
    fs::read(path).map_err(|error| io_reason(&error).to_owned())
}

fn item_error(
    kind: ItemKind,
    name: String,
    agent: AgentId,
    path: PathBuf,
    scope: Scope,
    error: String,
) -> ScanItem {
    ScanItem {
        kind,
        name,
        agents: BTreeSet::from([agent]),
        path,
        scope,
        content_hash: None,
        normalized_hash: None,
        mcp_summary: None,
        parse_status: ParseStatus::Error(error),
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    }
}

fn scan_rule(
    report: &mut ScanReport,
    physical: &mut BTreeMap<PathBuf, usize>,
    path: &Path,
    agent: AgentId,
    scope: Scope,
    stores: &[StoreItem],
) {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(index) = physical.get(&canonical).copied() {
        report.items[index].agents.insert(agent);
        return;
    }
    let bytes = match read_document(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.partial = true;
            report.items.push(item_error(
                ItemKind::Rule,
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                agent,
                path.to_path_buf(),
                scope,
                error,
            ));
            return;
        }
    };
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();
    let raw_hash = at_fs::sha256_hex(&bytes);
    let normalized_hash = normalize_rule(&bytes);
    physical.insert(canonical, report.items.len());
    report.items.push(ScanItem {
        kind: ItemKind::Rule,
        name: name.clone(),
        agents: BTreeSet::from([agent]),
        path: path.to_path_buf(),
        scope,
        content_hash: Some(raw_hash),
        normalized_hash: Some(normalized_hash.clone()),
        mcp_summary: None,
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: ownership(ItemKind::Rule, &name, Some(&normalized_hash), stores),
    });
}

fn scan_mcp(
    report: &mut ScanReport,
    path: &Path,
    agent: &AgentId,
    scope: Scope,
    format: Option<&str>,
    stores: &[StoreItem],
) {
    let bytes = match read_document(path) {
        Ok(bytes) => bytes,
        Err(message) => {
            report.partial = true;
            report.warnings.push(ScanWarning {
                path: path.to_path_buf(),
                kind: ItemKind::McpServer,
                agent: agent.clone(),
                scope,
                message,
            });
            return;
        }
    };
    let parsed = read_mcp_entries_with(&bytes, format);
    match parsed {
        Ok(entries) => {
            for (name, entry) in entries {
                match hash_mcp_entry(&entry) {
                    Ok(hash) => report.items.push(ScanItem {
                        kind: ItemKind::McpServer,
                        name: name.clone(),
                        agents: BTreeSet::from([agent.clone()]),
                        path: path.to_path_buf(),
                        scope,
                        content_hash: Some(hash.to_string()),
                        normalized_hash: Some(hash.to_string()),
                        mcp_summary: Some(mcp_definition_summary(&entry)),
                        parse_status: ParseStatus::Parsed,
                        store_link: false,
                        ownership: ownership(
                            ItemKind::McpServer,
                            &name,
                            Some(&hash.to_string()),
                            stores,
                        ),
                    }),
                    Err(error) => {
                        report.partial = true;
                        report.items.push(item_error(
                            ItemKind::McpServer,
                            name,
                            agent.clone(),
                            path.to_path_buf(),
                            scope,
                            error.to_string(),
                        ));
                    }
                }
            }
        }
        Err(_) => {
            report.partial = true;
            report.warnings.push(ScanWarning {
                path: path.to_path_buf(),
                kind: ItemKind::McpServer,
                agent: agent.clone(),
                scope,
                message: "invalid MCP configuration".to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod root_tests {
    use super::*;
    use std::sync::Arc;

    fn root<'a>(report: &'a ScanReport, path: &Path, kind: ItemKind) -> &'a ScanRoot {
        report
            .roots
            .iter()
            .find(|root| root.path == path && root.kind == kind)
            .expect("candidate root is recorded")
    }

    #[test]
    fn configured_custom_root_outside_the_injected_roots_is_still_scanned() {
        use at_adapters::custom_agent::{CustomAgentAdapter, CustomAgentSpec, CustomMcpFormat};

        let temp = tempfile::tempdir().expect("temporary sandbox");
        let home = temp.path().join("home");
        let custom_root = temp.path().join("outside-agent");
        let skills = custom_root.join("skills");
        let found = skills.join("ported");
        fs::create_dir_all(&found).expect("custom skill directory");
        fs::create_dir_all(&home).expect("home");
        fs::write(
            found.join("SKILL.md"),
            "---\nname: ported\ndescription: example\n---\n",
        )
        .expect("skill file");
        let adapters = AgentAdapterRegistry::register();
        adapters
            .insert(Arc::new(
                CustomAgentAdapter::new(CustomAgentSpec {
                    id: "outside".to_owned(),
                    display_name: "Outside".to_owned(),
                    root: custom_root,
                    skills_subfolder: PathBuf::from("skills"),
                    mcp_file: None,
                    mcp_format: CustomMcpFormat::None,
                    rule_files: Vec::new(),
                    supports_symlink: true,
                })
                .expect("valid custom spec"),
            ))
            .expect("register custom adapter");
        let environment = AgentEnvironment {
            home,
            project: None,
            variables: BTreeMap::new(),
        };
        let report = scan(ScanRequest {
            environment: &environment,
            adapters: &adapters,
            store_roots: &[],
            store_items: &[],
        })
        .expect("scan");
        let custom = root(&report, &skills, ItemKind::Skill);
        assert_eq!(custom.status, ScanRootStatus::Scanned);
        assert_eq!((custom.found_items, custom.new_items), (1, 1));
        assert!(report.items.iter().any(|item| item.name == "ported"));
    }

    #[test]
    fn root_outcomes_include_found_empty_missing_and_redacted_parse_failures() {
        let temp = tempfile::tempdir().expect("temporary sandbox");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let populated = home.join(".claude/skills");
        for name in ["fresh", "managed", "broken"] {
            fs::create_dir_all(populated.join(name)).expect("skill directory");
        }
        for name in ["fresh", "managed"] {
            fs::write(
                populated.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: example\n---\n\n# Skill\n"),
            )
            .expect("skill file");
        }
        fs::write(populated.join("broken/SKILL.md"), "invalid").expect("broken skill");
        let empty = home.join(".codex/skills");
        fs::create_dir_all(&empty).expect("empty skills root");
        let omp_backup = home.join(".omp/agent/managed-skills/.bak-fixture");
        fs::create_dir_all(&omp_backup).expect("omp backup directory");
        fs::write(
            omp_backup.join("SKILL.md"),
            "---\nname: should-not-appear\ndescription: backup\n---\n",
        )
        .expect("omp backup skill");
        fs::create_dir_all(&project).expect("project root");
        let mcp = home.join(".codex/config.toml");
        fs::write(&mcp, "token = \"fixture-secret-value\"\ninvalid = [").expect("malformed config");
        let environment = AgentEnvironment {
            home: home.clone(),
            project: Some(project),
            variables: BTreeMap::new(),
        };
        let adapters = AgentAdapterRegistry::register();
        let managed_path = populated.join("managed");
        let store_items = [StoreItem {
            kind: ItemKind::Skill,
            name: "managed".to_owned(),
            content_hash: hash_skill_folder(&managed_path)
                .expect("hash managed skill")
                .to_string(),
            scope: Scope::User,
            path: managed_path,
        }];
        let report = scan(ScanRequest {
            environment: &environment,
            adapters: &adapters,
            store_roots: &[],
            store_items: &store_items,
        })
        .expect("scan");
        assert!(
            !report
                .items
                .iter()
                .any(|item| item.name == "should-not-appear")
        );
        let omp_backup_root = root(
            &report,
            &home.join(".omp/agent/managed-skills"),
            ItemKind::Skill,
        );
        assert_eq!(omp_backup_root.found_items, 0);
        assert_eq!(omp_backup_root.status, ScanRootStatus::Empty);
        let populated_root = root(&report, &populated, ItemKind::Skill);
        assert_eq!(populated_root.status, ScanRootStatus::Scanned);
        assert_eq!(
            (
                populated_root.found_items,
                populated_root.new_items,
                populated_root.skipped_items,
                populated_root.in_sync
            ),
            (3, 1, 1, 1)
        );
        let empty_root = root(&report, &empty, ItemKind::Skill);
        assert_eq!(empty_root.status, ScanRootStatus::Empty);
        assert_eq!(empty_root.found_items, 0);
        let missing_root = root(&report, &home.join(".agents/skills"), ItemKind::Skill);
        assert_eq!(missing_root.status, ScanRootStatus::NotPresent);
        assert_eq!(missing_root.found_items, 0);
        let bad_mcp = root(&report, &mcp, ItemKind::McpServer);
        assert_eq!(bad_mcp.status, ScanRootStatus::Unreadable);
        assert_eq!(
            bad_mcp.unreadable_reason.as_deref(),
            Some("invalid MCP configuration")
        );
        assert!(
            !serde_json::to_string(&report)
                .expect("serialize scan")
                .contains("fixture-secret-value")
        );
    }

    #[cfg(unix)]
    #[test]
    fn permission_denial_and_escaping_root_are_reported_without_losing_other_results() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        struct RestorePermissions(PathBuf, fs::Permissions);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, self.1.clone());
            }
        }

        let temp = tempfile::tempdir().expect("temporary sandbox");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        let found = home.join(".claude/skills/found");
        let unreadable = home.join(".cursor/skills");
        let shared = project.join(".agents/skills");
        for path in [&found, &unreadable, &outside, &project.join(".agents")] {
            fs::create_dir_all(path).expect("fixture directory");
        }
        fs::write(
            found.join("SKILL.md"),
            "---\nname: found\ndescription: example\n---\n",
        )
        .expect("skill");
        fs::create_dir_all(outside.join("secret")).expect("outside skill");
        fs::write(
            outside.join("secret/SKILL.md"),
            "---\nname: secret\ndescription: example\n---\n",
        )
        .expect("outside file");
        symlink(&outside, &shared).expect("escaping shared root");
        let original = fs::metadata(&unreadable)
            .expect("permissions")
            .permissions();
        let _restore = RestorePermissions(unreadable.clone(), original);
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).expect("chmod");
        let permission_enforced = fs::read_dir(&unreadable).is_err();
        let environment = AgentEnvironment {
            home,
            project: Some(project),
            variables: BTreeMap::new(),
        };
        let adapters = AgentAdapterRegistry::register();
        let report = scan(ScanRequest {
            environment: &environment,
            adapters: &adapters,
            store_roots: &[],
            store_items: &[],
        })
        .expect("scan");
        assert!(report.partial);
        assert!(report.items.iter().any(|item| item.name == "found"));
        assert!(!report.items.iter().any(|item| item.name == "secret"));
        let escaped = root(&report, &shared, ItemKind::Skill);
        assert_eq!(escaped.status, ScanRootStatus::Unreadable);
        assert_eq!(
            escaped.unreadable_reason.as_deref(),
            Some("symbolic link target is outside injected roots")
        );
        assert_eq!(
            escaped.agents,
            BTreeSet::from([AgentId::Codex, AgentId::Omp])
        );
        if permission_enforced {
            let denied = root(&report, &unreadable, ItemKind::Skill);
            assert_eq!(denied.status, ScanRootStatus::Unreadable);
            assert_eq!(
                denied.unreadable_reason.as_deref(),
                Some("permission denied")
            );
            assert!(
                report
                    .warnings
                    .iter()
                    .any(|warning| warning.path == unreadable)
            );
        }
    }

    #[test]
    fn mcp_summary_keeps_transport_shape_and_excludes_secret_values() {
        let secret = concat!("sk-", "live-", "fixture-secret");
        let entry = serde_json::json!({
            "command": "/usr/local/bin/node",
            "args": ["server.js", secret],
            "env": { "API_TOKEN": secret },
            "url": format!("https://user:{secret}@example.test/mcp?token={secret}"),
            "headers": { "Authorization": secret }
        });
        let summary = mcp_definition_summary(&entry);
        assert_eq!(summary.transport, "stdio");
        assert_eq!(summary.command.as_deref(), Some("node"));
        assert_eq!(summary.argument_count, Some(2));
        assert_eq!(summary.url.as_deref(), Some("example.test/mcp"));
        assert_eq!(summary.env_names, ["API_TOKEN"]);
        assert_eq!(summary.header_names, ["Authorization"]);
        let serialized = serde_json::to_string(&summary).expect("serialize summary");
        assert!(!serialized.contains(secret));
    }
}
