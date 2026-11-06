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
    /// Parsing result.
    pub parse_status: ParseStatus,
    /// Whether this is a link into an AgenTuna store.
    pub store_link: bool,
    /// Ownership relative to the supplied store inventory.
    pub ownership: Ownership,
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
}

/// Canonical store identity and content digest used to match native entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreItem {
    /// Managed item kind.
    pub kind: ItemKind,
    /// Canonical item name.
    pub name: String,
    /// Content digest.
    pub content_hash: String,
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

/// Scans adapter-discovered roots and MCP configuration files without executing servers.
///
/// # Errors
/// Returns adapter registry failures; path and parse errors are represented per-item so
/// scanning continues.
pub fn scan(request: ScanRequest<'_>) -> Result<ScanReport, ScanError> {
    let mut report = ScanReport {
        items: Vec::new(),
        partial: false,
        skipped_roots: Vec::new(),
    };
    let mut physical: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let adapters = request.adapters.adapters().map_err(ScanError::Adapter)?;
    for adapter in adapters {
        let agent = adapter.id();
        for scope in [Scope::User, Scope::Project, Scope::Local] {
            for candidate in adapter.paths(ItemKind::Skill, scope, request.environment) {
                if candidate.kind != "skills_dir" {
                    continue;
                }
                if !candidate.path.exists() {
                    report.skipped_roots.push(candidate.path);
                    continue;
                }
            }
            let skills = match adapter.list_native_skills(request.environment, request.store_roots)
            {
                Ok(items) => items,
                Err(error) => {
                    report.partial = true;
                    report.items.push(item_error(
                        ItemKind::Skill,
                        String::new(),
                        agent.clone(),
                        PathBuf::new(),
                        scope,
                        error.to_string(),
                    ));
                    continue;
                }
            };
            for skill in skills.into_iter().filter(|item| item.scope == scope) {
                let canonical =
                    fs::canonicalize(&skill.path).unwrap_or_else(|_| skill.path.clone());
                if let Some(index) = physical.get(&canonical).copied() {
                    report.items[index].agents.insert(agent.clone());
                    continue;
                }
                let digest = hash_skill_folder(&skill.path)
                    .ok()
                    .map(|hash| hash.to_string());
                let raw_digest = raw_skill_hash(&skill.path).ok();
                let is_store_link = skill.target.as_ref().is_some_and(|target| {
                    request
                        .store_roots
                        .iter()
                        .any(|root| target.starts_with(root))
                });
                let status = if skill.valid && digest.is_some() && raw_digest.is_some() {
                    ParseStatus::Parsed
                } else {
                    report.partial = true;
                    ParseStatus::Error(
                        skill
                            .error
                            .unwrap_or_else(|| "unreadable skill content".into()),
                    )
                };
                let ownership = ownership(
                    ItemKind::Skill,
                    &skill.name,
                    digest.as_deref(),
                    request.store_items,
                );
                if is_store_link {
                    continue;
                }
                let mut agents = BTreeSet::new();
                agents.insert(agent.clone());
                physical.insert(canonical, report.items.len());
                report.items.push(ScanItem {
                    kind: ItemKind::Skill,
                    name: skill.name,
                    agents,
                    path: skill.path,
                    scope: skill.scope,
                    content_hash: raw_digest,
                    normalized_hash: digest.clone(),
                    parse_status: status,
                    store_link: is_store_link,
                    ownership,
                });
            }
        }
        for scope in [Scope::User, Scope::Project, Scope::Local] {
            for kind in [ItemKind::McpServer, ItemKind::Rule] {
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
                    let metadata = match fs::symlink_metadata(&candidate.path) {
                        Ok(metadata) => metadata,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            report.skipped_roots.push(candidate.path);
                            continue;
                        }
                        Err(error) => {
                            report.partial = true;
                            report.items.push(item_error(
                                kind,
                                String::new(),
                                agent.clone(),
                                candidate.path,
                                scope,
                                error.to_string(),
                            ));
                            continue;
                        }
                    };
                    if metadata.file_type().is_symlink() {
                        continue;
                    }
                    if kind == ItemKind::Rule {
                        if metadata.is_dir() {
                            let entries = match fs::read_dir(&candidate.path) {
                                Ok(entries) => entries,
                                Err(error) => {
                                    report.partial = true;
                                    report.items.push(item_error(
                                        kind,
                                        String::new(),
                                        agent.clone(),
                                        candidate.path,
                                        scope,
                                        error.to_string(),
                                    ));
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
                                        report.partial = true;
                                        report.items.push(item_error(
                                            kind,
                                            String::new(),
                                            agent.clone(),
                                            candidate.path.clone(),
                                            scope,
                                            error.to_string(),
                                        ));
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
    report
        .items
        .sort_by(|a, b| (&a.path, &a.kind, &a.name).cmp(&(&b.path, &b.kind, &b.name)));
    report.skipped_roots.sort();
    report.skipped_roots.dedup();
    Ok(report)
}

/// Scan failure outside item-level filesystem or syntax errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScanError {
    /// Adapter enumeration failed.
    #[error(transparent)]
    Adapter(#[from] at_adapters::adapter::AgentError),
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
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let normalized = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    at_store::manifest::hash_rule_bytes(normalized.trim_end().as_bytes()).to_string()
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
    let bytes = match fs::read(path) {
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
                error.to_string(),
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
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.partial = true;
            report.items.push(item_error(
                ItemKind::McpServer,
                String::new(),
                agent.clone(),
                path.to_path_buf(),
                scope,
                error.to_string(),
            ));
            return;
        }
    };
    let parsed: Result<BTreeMap<String, Value>, String> = if format == Some("toml") {
        std::str::from_utf8(&bytes)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                toml_edit::de::from_str::<BTreeMap<String, BTreeMap<String, Value>>>(text)
                    .map_err(|error| error.to_string())
            })
            .map(|mut doc| doc.remove("mcp_servers").unwrap_or_default())
    } else {
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| error.to_string())
            .map(|doc| {
                doc.get("mcpServers")
                    .or_else(|| doc.get("mcp_servers"))
                    .and_then(Value::as_object)
                    .map(|map| {
                        map.iter()
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect()
                    })
                    .unwrap_or_default()
            })
    };
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
        Err(error) => {
            report.partial = true;
            report.items.push(item_error(
                ItemKind::McpServer,
                String::new(),
                agent.clone(),
                path.to_path_buf(),
                scope,
                error,
            ));
        }
    }
}
