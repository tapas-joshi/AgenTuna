//! Native MCP-server, rule-file and skill discovery for the derived index (TC-025, TM-020,
//! TM-033).
//!
//! The MCP Servers and Skills screens read native findings from `native_items`. Those rows used
//! to be written only by a post-quarantine `at_db::rebuild`, so an index that was built while an
//! agent had no servers stayed empty, and neither Rescan nor a migration refreshed it. This
//! module is the single writer for normal operation: it scans the agent folders the adapters
//! describe through [`at_migrate::scan`], keeps only the entries AgenTuna does not own, and
//! replaces one `(agent, scope)` slice at a time with [`at_db::native::replace_scan`].
//!
//! ## What counts as managed
//!
//! A finding is dropped — or, after adoption, never written — when AgenTuna owns the entry. Three
//! signals decide that, and none of them is a name match on its own:
//!
//! 1. the entry is a link into an AgenTuna store (`ScanItem::store_link`);
//! 2. the scanner computed `Ownership::Managed`, meaning the native digest equals the canonical
//!    item's digest for the same kind and name;
//! 3. a `deployments` row for this agent and scope targets exactly this path and names a
//!    canonical item that is still stored. The planner that rendered the file wrote that row,
//!    and undo removes it. This is the signal that works for MCP servers: a canonical `mcp.json`
//!    definition has a different shape from every native document entry, so equal content never
//!    hashes equal across the two shapes.
//!
//! A same-name entry with different content stays visible as a conflict the user can resolve.
//! Because ownership is re-derived from disk, the stores and the index on every pass, adopting
//! an item removes its row and undoing the adoption brings it back without a stored flag that
//! could go stale.
//!
//! ## Safety
//!
//! Scanning is read-only: nothing here writes an agent folder, spawns a process or starts an
//! MCP server, and only the name, path and content digest of a finding are stored — never a
//! configuration body, argument value or environment variable, so a secret inside a native
//! definition cannot reach SQLite. Sizes are bounded by the scanner's per-document limit and by
//! [`MAX_NATIVE_ROWS_PER_SCOPE`].
//!
//! ## Scope coverage
//!
//! User-scope slices are always replaced. Project and Local slices are replaced only while a
//! project is active: `native_items` has no project column, so scanning one project would
//! otherwise mark another project's rows `missing`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::native::{NativeScanEntry, replace_scan};
use at_migrate::scan::{
    Ownership, ParseStatus, ScanItem, ScanRequest, ScanWarning, StoreItem, scan,
};
use at_model::{AgentId, ItemKind, Scope};
use at_store::layout::{StoreError, StoreLayout};
use at_store::manifest::{ManifestError, StoreManifest};
use at_store::paths::AppPaths;
use rusqlite::Connection;

use crate::refresh::agent_environment;

/// Upper bound on native rows one `(agent, scope)` slice keeps.
///
/// A folder holding more findings than this is a runaway directory rather than a workflow, and
/// truncating it keeps one bad root from turning every list load into a large query.
pub const MAX_NATIVE_ROWS_PER_SCOPE: usize = 4096;

/// Settings key recording that native findings have been derived at least once.
///
/// The rows are rebuildable, so the marker only answers "has a scan ever run". A read command
/// uses it to tell an agent folder that holds nothing apart from an index nobody has scanned:
/// without it the command would rescan on every invocation.
pub const NATIVE_SCAN_MARKER_KEY: &str = "native.scan_built";

/// Scopes whose rows belong to the active project.
const PROJECT_SCOPES: [Scope; 2] = [Scope::Project, Scope::Local];

/// Failure while refreshing the native findings.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NativeRefreshError {
    /// The adapter registry could not be enumerated.
    #[error(transparent)]
    Scan(#[from] at_migrate::scan::ScanError),
    /// A native slice could not be written.
    #[error("native findings could not be recorded: {0}")]
    Database(#[from] at_db::DbError),
    /// A canonical store could not be read.
    #[error("a canonical store could not be read: {0}")]
    Store(#[from] StoreError),
    /// A canonical store manifest could not be read.
    #[error("a canonical store manifest could not be read: {0}")]
    Manifest(#[from] ManifestError),
}

/// What one native refresh did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeRefreshReport {
    /// Rows inserted or refreshed across every replaced slice.
    pub upserted: u64,
    /// Rows marked `missing` because the scan no longer sees them.
    pub marked_missing: u64,
    /// Rows that were `missing` and are visible again.
    pub resurrected: u64,
    /// Findings skipped because AgenTuna already owns them.
    pub managed: usize,
    /// Roots and configuration files the scan could not read.
    pub warnings: Vec<ScanWarning>,
    /// Whether a discovery step failed, so `warnings` is not the whole story.
    pub partial: bool,
}

/// One finding ready to be written, owned so the scan report can be dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedEntry {
    kind: ItemKind,
    name: String,
    path: String,
    content_hash: Option<String>,
}

/// Scans the injected machine context and replaces the derived native rows.
///
/// Detection uses the same environment the agent registry uses, so a machine with no `PATH`
/// still gets a complete scan: discovery only reads filesystem candidates.
///
/// # Errors
/// Returns a typed failure when adapters cannot be enumerated or a slice cannot be written; an
/// unreadable agent root is a warning in [`NativeRefreshReport`], not an error.
pub fn refresh_native(
    connection: &Connection,
    paths: &AppPaths,
) -> Result<NativeRefreshReport, NativeRefreshError> {
    let environment = agent_environment(paths);
    refresh_native_with_environment(connection, paths, &environment)
}

/// Scans `environment` instead of deriving it from `paths`, and replaces the native rows.
///
/// Tests inject a temporary home and project here; production callers use [`refresh_native`].
///
/// # Errors
/// Returns the same failures as [`refresh_native`].
pub fn refresh_native_with_environment(
    connection: &Connection,
    paths: &AppPaths,
    environment: &AgentEnvironment,
) -> Result<NativeRefreshReport, NativeRefreshError> {
    let adapters = AgentAdapterRegistry::register();
    let (store_roots, store_items) = store_inventory(paths)?;
    let managed = managed_deployments(connection, &store_items)?;
    let report = scan(ScanRequest {
        environment,
        adapters: &adapters,
        store_roots: &store_roots,
        store_items: &store_items,
    })?;

    let mut refresh = NativeRefreshReport {
        warnings: report.warnings,
        partial: report.partial,
        ..NativeRefreshReport::default()
    };
    let mut slices = group_findings(&report.items, &managed, &mut refresh);

    let mut agents: Vec<AgentId> = Vec::new();
    if let Ok(adapter_list) = adapters.adapters() {
        agents.extend(adapter_list.iter().map(|adapter| adapter.id()));
    }
    for agent_and_scope in slices.keys() {
        if !agents.contains(&agent_and_scope.0) {
            agents.push(agent_and_scope.0.clone());
        }
    }
    agents.sort();
    agents.dedup();

    let mut scopes = vec![Scope::User];
    if environment.project.is_some() {
        scopes.extend(PROJECT_SCOPES);
    }
    for agent in &agents {
        for scope in &scopes {
            let mut entries = slices.remove(&(agent.clone(), *scope)).unwrap_or_default();
            if entries.len() > MAX_NATIVE_ROWS_PER_SCOPE {
                let kind = entries
                    .first()
                    .map_or(ItemKind::McpServer, |entry| entry.kind);
                entries.truncate(MAX_NATIVE_ROWS_PER_SCOPE);
                refresh.warnings.push(ScanWarning {
                    path: environment.home.clone(),
                    kind,
                    agent: agent.clone(),
                    scope: *scope,
                    message: format!(
                        "more than {MAX_NATIVE_ROWS_PER_SCOPE} native findings; the rest were not \
                         recorded"
                    ),
                });
            }
            let scan_entries = entries
                .iter()
                .map(|entry| NativeScanEntry {
                    kind: entry.kind,
                    name: &entry.name,
                    path: &entry.path,
                    content_hash: entry.content_hash.as_deref(),
                })
                .collect::<Vec<_>>();
            let replaced = replace_scan(connection, agent, *scope, &scan_entries)?;
            refresh.upserted += replaced.upserted;
            refresh.marked_missing += replaced.marked_missing;
            refresh.resurrected += replaced.resurrected;
        }
    }
    mark_scanned(connection)?;
    Ok(refresh)
}

/// Records that a scan completed, so a read command can tell "nothing found" from "never
/// scanned" instead of rescanning on every invocation.
fn mark_scanned(connection: &Connection) -> Result<(), NativeRefreshError> {
    connection
        .execute(
            "INSERT INTO settings (key, value_json, updated_at) \
             VALUES (?1, 'true', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
             ON CONFLICT(key) DO UPDATE SET value_json = 'true'",
            [NATIVE_SCAN_MARKER_KEY],
        )
        .map_err(at_db::DbError::from)?;
    Ok(())
}

/// Deployments that prove AgenTuna wrote a native entry.
///
/// A deployment row is written by the planner that rendered the file and removed by an undo, so
/// the row — together with the canonical item still being present in its store — is the
/// ownership record. A matching name alone never hides a finding. The scanner's own digest
/// comparison cannot serve here: a canonical `mcp.json` definition has a different shape from
/// every native document entry, so equal content never hashes equal across the two.
struct ManagedDeployments {
    rows: BTreeSet<(String, Scope, ItemKind, String, String)>,
}

impl ManagedDeployments {
    /// Whether a deployment claims exactly this finding.
    fn claims(
        &self,
        agent: &AgentId,
        scope: Scope,
        kind: ItemKind,
        name: &str,
        path: &str,
    ) -> bool {
        self.rows.contains(&(
            agent.to_string(),
            scope,
            kind,
            normalize_name(name),
            path.to_owned(),
        ))
    }
}

/// Lowercases a name the way native and canonical identities are compared.
fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

/// Reads every deployment that targets a native path and keeps the ones whose item is stored.
///
/// A deployment whose canonical item vanished from its store would otherwise hide a finding that
/// AgenTuna no longer owns.
fn managed_deployments(
    connection: &Connection,
    store_items: &[StoreItem],
) -> Result<ManagedDeployments, NativeRefreshError> {
    let mut statement = connection
        .prepare(
            "SELECT d.agent_id, d.scope, i.kind, i.slug, i.name, d.target_path \
               FROM deployments d JOIN items i ON i.id = d.item_id \
              WHERE d.target_path IS NOT NULL",
        )
        .map_err(at_db::DbError::from)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(at_db::DbError::from)?;
    let mut managed = BTreeSet::new();
    for row in rows {
        let (agent, scope, kind, slug, name, target) = row.map_err(at_db::DbError::from)?;
        let (Some(agent), Some(scope), Some(target)) = (agent, scope, target) else {
            continue;
        };
        let (Ok(agent), Ok(scope), Ok(kind)) = (
            agent.parse::<AgentId>(),
            scope.parse::<Scope>(),
            kind.parse::<ItemKind>(),
        ) else {
            // A stored value the schema no longer understands cannot identify anything; the
            // refresh keeps running and the finding stays visible instead of being claimed.
            continue;
        };
        let stored = store_items.iter().any(|entry| {
            entry.kind == kind
                && entry.scope == scope
                && (normalize_name(&entry.name) == normalize_name(&slug)
                    || normalize_name(&entry.name) == normalize_name(&name))
        });
        if !stored {
            continue;
        }
        for identity in [normalize_name(&slug), normalize_name(&name)] {
            managed.insert((agent.to_string(), scope, kind, identity, target.clone()));
        }
    }
    Ok(ManagedDeployments { rows: managed })
}

/// Groups one scan pass into the per-agent, per-scope slices `replace_scan` consumes.
///
/// A physical entry that several agents share (the project `.agents/skills` root) becomes one
/// row for the first agent in `AgentId` order: `native_items.path` is unique, so the row names
/// one owner while the shared root still loads the skill in every agent that reads it.
fn group_findings(
    items: &[ScanItem],
    managed: &ManagedDeployments,
    refresh: &mut NativeRefreshReport,
) -> BTreeMap<(AgentId, Scope), Vec<OwnedEntry>> {
    let mut slices: BTreeMap<(AgentId, Scope), Vec<OwnedEntry>> = BTreeMap::new();
    for item in items {
        let Some(agent) = item.agents.iter().next().cloned() else {
            continue;
        };
        if item.store_link
            || item.ownership == Ownership::Managed
            || managed.claims(
                &agent,
                item.scope,
                item.kind,
                &item.name,
                &item.path.to_string_lossy(),
            )
        {
            refresh.managed += 1;
            continue;
        }
        if let ParseStatus::Error(message) = &item.parse_status {
            refresh.warnings.push(ScanWarning {
                path: item.path.clone(),
                kind: item.kind,
                agent,
                scope: item.scope,
                message: message.clone(),
            });
            continue;
        }
        let Some(path) = row_path(item) else {
            refresh.warnings.push(ScanWarning {
                path: item.path.clone(),
                kind: item.kind,
                agent,
                scope: item.scope,
                message: format!(
                    "the entry name `{}` cannot be stored as a native row path",
                    item.name
                ),
            });
            continue;
        };
        slices
            .entry((agent, item.scope))
            .or_default()
            .push(OwnedEntry {
                kind: item.kind,
                name: item.name.clone(),
                path,
                content_hash: item
                    .normalized_hash
                    .clone()
                    .or_else(|| item.content_hash.clone()),
            });
    }
    for entries in slices.values_mut() {
        entries.sort_by(|left, right| {
            (&left.path, left.kind.wire_name(), &left.name).cmp(&(
                &right.path,
                right.kind.wire_name(),
                &right.name,
            ))
        });
    }
    slices
}

/// The `native_items.path` value for one finding.
///
/// Skills and rules live in their own file or folder, so the filesystem path already identifies
/// them. MCP servers are entries inside one shared document per agent, and `native_items.path`
/// is unique: the row stores the document plus the entry key (see
/// [`at_db::native::mcp_entry_path`]) so two servers in one file stay two rows. A key that
/// cannot be encoded unambiguously yields `None` and a warning instead of a colliding row.
fn row_path(item: &ScanItem) -> Option<String> {
    let path = item.path.to_string_lossy().into_owned();
    if item.kind != ItemKind::McpServer {
        return Some(path);
    }
    if item.name.contains(at_db::native::MCP_ENTRY_SEPARATOR) {
        return None;
    }
    Some(at_db::native::mcp_entry_path(&path, &item.name))
}

/// Roots of every AgenTuna store and the canonical inventory used to classify ownership.
///
/// The Global scope is always included; Project and Local are skipped when no project is
/// active, matching the scopes the refresh itself can cover.
fn store_inventory(paths: &AppPaths) -> Result<(Vec<PathBuf>, Vec<StoreItem>), NativeRefreshError> {
    let mut roots = Vec::new();
    let mut items = Vec::new();
    for scope in [Scope::Global, Scope::User, Scope::Project, Scope::Local] {
        let layout = match StoreLayout::for_scope(paths, scope) {
            Ok(layout) => layout,
            Err(_) if matches!(scope, Scope::Project | Scope::Local) => continue,
            Err(error) => return Err(NativeRefreshError::Store(error)),
        };
        roots.push(layout.root().to_path_buf());
        match StoreManifest::load(&layout) {
            Ok(loaded) => items.extend(loaded.manifest.items().iter().map(|item| StoreItem {
                kind: item.kind,
                name: item.name.clone().unwrap_or_else(|| item.slug.to_string()),
                content_hash: item.hash.to_string(),
                scope,
                path: layout.root().join(item.path.to_string()),
            })),
            Err(error) if error.is_missing() => {}
            Err(error) => return Err(NativeRefreshError::Manifest(error)),
        }
    }
    Ok((roots, items))
}
