//! Single-item adoption of an unmanaged native entry (TC-011).
//!
//! Adoption turns one scanned native item into a store item with recorded ownership: the
//! content is copied into the canonical store, the native original is replaced by a link or a
//! manifest-bearing copy (or left watched), and the deployment and native rows are written by
//! the same reviewed `ChangeSet` that moves the bytes. Everything is refused before any write
//! when the source no longer matches the scan record.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use at_fs::ApplyRoots;
use at_model::{AgentId, ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::StoreManifest;

use crate::apply::{ApplyPlanContext, ApplyPlanError, apply_plan_with_actor};
use crate::conflicts::{Conflict, ConflictCopy, ConflictSet, CopyOrigin, ProposedResolution};
use crate::plan::{
    BuildPlanRequest, ConflictResolution, MigrationPlan, MigrationStrategy, PlanError, build_plan,
    dry_run,
};
use crate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};

/// What happens to the native original after its content is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdoptHandling {
    /// Replace the native original with a symlink into the store.
    Link,
    /// Replace the native original with a manifest-bearing copy of the store item.
    Copy,
    /// Leave the native original in place and record it as watched.
    Watch,
}

impl AdoptHandling {
    /// Wire name used by the CLI and the frontend.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Copy => "copy",
            Self::Watch => "watch",
        }
    }

    /// Parses the wire name.
    ///
    /// # Errors
    /// [`AdoptError::InvalidHandling`] for any other value.
    pub fn parse(value: &str) -> Result<Self, AdoptError> {
        match value {
            "link" => Ok(Self::Link),
            "copy" => Ok(Self::Copy),
            "watch" => Ok(Self::Watch),
            other => Err(AdoptError::InvalidHandling(other.to_owned())),
        }
    }
}

/// What the file system holds at the scanned path now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PathState {
    /// The path exists with this content digest.
    Present(String),
    /// Nothing is at the path any more.
    Missing,
}

/// Inputs for applying one adoption.
pub struct AdoptRequest<'a> {
    /// Scan record the caller reviewed.
    pub scan: &'a ScanReport,
    /// Native path selected from that record.
    pub path: &'a Path,
    /// Restricts the match to this entry name when one document holds several entries.
    ///
    /// An MCP configuration stores every server in one file, so a caller adopting one server
    /// names it here; `None` selects the first record at `path`.
    pub name: Option<&'a str>,
    /// Canonical store root receiving the item.
    pub store_root: &'a Path,
    /// Operation backup root.
    pub backup_root: &'a Path,
    /// Existing roots authorized for absolute native paths.
    pub authorized_roots: &'a ApplyRoots,
    /// Database for the operation journal and ownership rows.
    pub database: &'a mut at_db::Database,
    /// Store scope receiving the item.
    pub scope: Scope,
    /// What to do with the native original.
    pub handling: AdoptHandling,
    /// Active project id when adopting at Project or Local scope.
    pub project_id: Option<&'a str>,
}

/// Inputs for a write-free adoption preview.
pub struct AdoptPreviewRequest<'a> {
    /// Scan record the caller reviewed.
    pub scan: &'a ScanReport,
    /// Native path selected from that record.
    pub path: &'a Path,
    /// Restricts the match to this entry name when one document holds several entries.
    ///
    /// An MCP configuration stores every server in one file, so a caller previewing one server
    /// names it here; `None` selects the first record at `path`.
    pub name: Option<&'a str>,
    /// Canonical store root receiving the item.
    pub store_root: &'a Path,
    /// Operation backup root.
    pub backup_root: &'a Path,
    /// Existing roots authorized for absolute native paths.
    pub authorized_roots: &'a ApplyRoots,
    /// Store scope receiving the item.
    pub scope: Scope,
    /// What to do with the native original.
    pub handling: AdoptHandling,
}

/// A reviewed, write-free adopt projection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdoptPreview {
    /// Native path being adopted.
    pub source: PathBuf,
    /// Canonical store root.
    pub store_root: PathBuf,
    /// Where the store item will live.
    pub store_destination: PathBuf,
    /// Chosen store slug.
    pub slug: String,
    /// Original native item name (the MCP server key for MCP items).
    pub native_name: String,
    /// Item kind.
    pub kind: ItemKind,
    /// Agents that read the native path.
    pub agents: Vec<AgentId>,
    /// What the file system holds at the source now.
    pub current: PathState,
    /// Handling that will be applied.
    pub handling: AdoptHandling,
    /// Whether a collision forced a suffixed slug.
    pub renamed: bool,
    /// Ordered dry-run rows of the one-item plan.
    pub rows: Vec<crate::plan::DryRunRow>,
}

/// Result of one completed adoption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptOutcome {
    /// Reviewed plan that was applied.
    pub plan: MigrationPlan,
    /// Durable operation id, accepted by [`crate::undo::undo_operation`].
    pub operation_id: String,
    /// Where the store item was created.
    pub store_destination: PathBuf,
    /// Chosen store slug.
    pub slug: String,
    /// Original native item name.
    pub native_name: String,
    /// Handling that was applied.
    pub handling: AdoptHandling,
    /// Paths the operation reported as touched.
    pub touched_paths: Vec<PathBuf>,
}

/// Why an adoption was refused or failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdoptError {
    /// The scan record has no item at the requested path.
    #[error("scan record has no native item at {0}")]
    ItemNotFound(PathBuf),
    /// The scan row could not be read, so its content cannot be verified.
    #[error("scan record for {0} is not a parsed item")]
    Unparsed(PathBuf),
    /// The item is already managed by AgenTuna (a store link or matching store item).
    #[error("{0} is already managed by AgenTuna")]
    AlreadyManaged(PathBuf),
    /// The scanned content changed (or vanished) after the scan.
    #[error("scan record for {path} is stale; the path is now {current:?}")]
    StaleScanRecord {
        /// Scanned path.
        path: PathBuf,
        /// What the path holds now.
        current: PathState,
    },
    /// The item name cannot become a store slug.
    #[error("`{0}` cannot be used as a store slug")]
    InvalidName(String),
    /// The handling name is unknown.
    #[error("unknown adopt handling `{0}`")]
    InvalidHandling(String),
    /// The store could not be initialised or read.
    #[error("canonical store is unavailable: {0}")]
    Store(String),
    /// Plan construction failed.
    #[error(transparent)]
    Plan(#[from] PlanError),
    /// Plan application failed.
    #[error(transparent)]
    Apply(#[from] ApplyPlanError),
}

/// Projects one adoption without writing.
///
/// Verifies the source still matches the scan record, picks a collision-free slug and returns
/// the exact plan rows the adoption would apply. A missing store is treated as empty; preview
/// never creates its directory or manifest.
///
/// # Errors
/// [`AdoptError::StaleScanRecord`] when the source changed or vanished, [`AdoptError::Plan`] for
/// an unusable name, or [`AdoptError::Store`] when the store cannot be read.
pub fn preview_adopt(request: &AdoptPreviewRequest<'_>) -> Result<AdoptPreview, AdoptError> {
    let item = selected_item(request.scan, request.path, request.name)
        .ok_or_else(|| AdoptError::ItemNotFound(request.path.to_path_buf()))?;
    if !matches!(item.parse_status, ParseStatus::Parsed) {
        return Err(AdoptError::Unparsed(item.path.clone()));
    }
    if item.store_link || item.ownership == Ownership::Managed {
        return Err(AdoptError::AlreadyManaged(item.path.clone()));
    }
    let current = current_state(item)?;
    let scanned = scanned_digest(item);
    if scanned.as_deref() != Some(current_digest(&current).as_str()) {
        return Err(AdoptError::StaleScanRecord {
            path: item.path.clone(),
            current,
        });
    }

    let layout = StoreLayout::new(request.scope, request.store_root);
    let taken = taken_slugs(request.scan, item, &layout)?;
    let base = slug_for(&item.name).ok_or_else(|| AdoptError::InvalidName(item.name.clone()))?;
    let slug =
        unique_slug(&base, &taken).ok_or_else(|| AdoptError::InvalidName(item.name.clone()))?;
    let plan = build_adopt_plan(request, item, &slug)?;
    Ok(AdoptPreview {
        source: item.path.clone(),
        store_root: request.store_root.to_path_buf(),
        store_destination: plan
            .steps
            .iter()
            .find(|step| step.item_name.as_deref() == Some(slug.as_str()))
            .and_then(|step| step.destination.clone())
            .unwrap_or_else(|| request.store_root.to_path_buf()),
        slug: slug.clone(),
        native_name: item.name.clone(),
        kind: item.kind,
        agents: item.agents.iter().cloned().collect(),
        current,
        handling: request.handling,
        renamed: slug != base,
        rows: dry_run(&plan),
    })
}

/// Adopts one scanned native item as a reviewed, undoable UI operation.
///
/// A scope with no store yet gets its skeleton inside the same reviewed `ChangeSet` the adoption
/// applies, so the store creation and the item are one operation and one undo. The plan is built
/// from the scan record after re-verifying the source digest, so an item that changed (or
/// vanished) since the scan is refused with [`AdoptError::StaleScanRecord`] and nothing is
/// written.
///
/// # Errors
/// Returns [`AdoptError`] for a stale record, an unusable name, or any plan/apply failure.
pub fn adopt_item(request: AdoptRequest<'_>) -> Result<AdoptOutcome, AdoptError> {
    adopt_item_with_actor(request, at_db::ops::OperationActor::Ui)
}

/// Adopts one scanned item and records the supplied operation actor.
///
/// # Errors
/// Returns the same failures as [`adopt_item`].
pub fn adopt_item_with_actor(
    request: AdoptRequest<'_>,
    actor: at_db::ops::OperationActor,
) -> Result<AdoptOutcome, AdoptError> {
    let preview_request = AdoptPreviewRequest {
        scan: request.scan,
        path: request.path,
        name: request.name,
        store_root: request.store_root,
        backup_root: request.backup_root,
        authorized_roots: request.authorized_roots,
        scope: request.scope,
        handling: request.handling,
    };
    let preview = preview_adopt(&preview_request)?;
    let item = selected_item(request.scan, request.path, request.name)
        .ok_or_else(|| AdoptError::ItemNotFound(request.path.to_path_buf()))?;
    let plan = build_adopt_plan(&preview_request, item, &preview.slug)?;
    let mut context = ApplyPlanContext {
        store_root: request.store_root,
        backup_root: request.backup_root,
        authorized_roots: request.authorized_roots,
        database: request.database,
        project_id: request.project_id,
    };
    let applied = apply_plan_with_actor(&plan, &mut context, actor)?;
    Ok(AdoptOutcome {
        plan,
        operation_id: applied.operation_id,
        store_destination: preview.store_destination,
        slug: preview.slug,
        native_name: preview.native_name,
        handling: preview.handling,
        touched_paths: applied.touched_paths,
    })
}

/// Builds the one-item plan, feeding the chosen slug through the KeepBoth naming path.
///
/// `build_plan` derives store names from `ScanItem::name`, and that name is also the native
/// identity the MCP/rule projections look up. A suffixed slug therefore travels as an explicit
/// conflict resolution instead of a renamed scan row: the plan keeps the original native name
/// and writes the chosen store name.
fn build_adopt_plan(
    request: &AdoptPreviewRequest<'_>,
    item: &ScanItem,
    slug: &str,
) -> Result<MigrationPlan, AdoptError> {
    let scan = ScanReport {
        roots: Vec::new(),
        items: vec![item.clone()],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let selected_paths = BTreeSet::from([item.path.clone()]);
    let mut resolutions = BTreeMap::new();
    let conflicts = if slug == item.name {
        ConflictSet::default()
    } else {
        let name = normalize(&item.name);
        let placeholder = ConflictCopy {
            agent: Some(other_agent(item)),
            origin: CopyOrigin::Native,
            path: request.store_root.join(".adopt-name-placeholder"),
            scope: item.scope,
            raw_hash: String::new(),
            normalized_hash: String::new(),
        };
        resolutions.insert(
            name.clone(),
            ConflictResolution::KeepBoth {
                names: vec![slug.to_owned(), format!("{slug}-placeholder")],
            },
        );
        ConflictSet {
            conflicts: vec![Conflict {
                kind: item.kind,
                name,
                copies: vec![
                    ConflictCopy {
                        agent: Some(
                            item.agents
                                .iter()
                                .next()
                                .cloned()
                                .unwrap_or(AgentId::ClaudeCode),
                        ),
                        origin: CopyOrigin::Native,
                        path: item.path.clone(),
                        scope: item.scope,
                        raw_hash: item.content_hash.clone().unwrap_or_default(),
                        normalized_hash: item.normalized_hash.clone().unwrap_or_default(),
                    },
                    placeholder,
                ],
                formatting_only: false,
                unified_diff: String::new(),
                proposed_resolution: ProposedResolution::NeedsChoice,
            }],
            ..ConflictSet::default()
        }
    };
    let copy_agents = if request.handling == AdoptHandling::Copy {
        item.agents.clone()
    } else {
        BTreeSet::new()
    };
    let strategy = if request.handling == AdoptHandling::Watch {
        MigrationStrategy::LeaveAndWatch
    } else {
        MigrationStrategy::ReplaceWithLinks
    };
    build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &resolutions,
        selected_paths: &selected_paths,
        store_root: request.store_root.to_path_buf(),
        copy_agents: &copy_agents,
        strategy,
        scope: request.scope,
        backup_root: request.backup_root.to_path_buf(),
    })
    .map_err(AdoptError::Plan)
}

/// Slugs already occupied by the store or by another native copy of the same name.
fn taken_slugs(
    scan: &ScanReport,
    item: &ScanItem,
    layout: &StoreLayout,
) -> Result<BTreeSet<String>, AdoptError> {
    let mut taken = BTreeSet::new();
    match StoreManifest::load(layout) {
        Ok(loaded) => {
            for existing in loaded.manifest.items_of_kind(item.kind) {
                taken.insert(existing.slug.to_string());
            }
        }
        Err(error) if error.is_missing() => {}
        Err(error) => return Err(AdoptError::Store(error.to_string())),
    }
    let normalized = normalize(&item.name);
    for other in &scan.items {
        // Same-named copies compete for one slug. The copy with the smallest path keeps the
        // base name; later copies take a suffix, so the choice is deterministic whichever one
        // the user adopts first.
        if other.path != item.path
            && other.kind == item.kind
            && normalize(&other.name) == normalized
            && other.path < item.path
        {
            taken.insert(normalize(&other.name));
        }
    }
    Ok(taken)
}

/// Finds the scan record a caller selected.
///
/// `name` narrows the match to one entry inside a shared native document — an MCP server key
/// — because every server in one configuration file scans to the same `path`. Without it the
/// first record at `path` is chosen, which is what a skill or rule adoption needs.
fn selected_item<'r>(
    scan: &'r ScanReport,
    path: &Path,
    name: Option<&str>,
) -> Option<&'r ScanItem> {
    scan.items
        .iter()
        .find(|item| item.path == path && name.is_none_or(|wanted| item.name == wanted))
}

/// The digest the scanner recorded for this row.
fn scanned_digest(item: &ScanItem) -> Option<String> {
    match item.kind {
        // Skills and MCP entries are compared by their normalized digest; rules by file bytes.
        ItemKind::Skill | ItemKind::McpServer => item.normalized_hash.clone(),
        _ => item.content_hash.clone(),
    }
}

/// The digest the same content has now, computed the way the scanner computes it.
fn current_state(item: &ScanItem) -> Result<PathState, AdoptError> {
    match item.kind {
        ItemKind::Skill => {
            let hash = at_store::manifest::hash_skill_folder(&item.path)
                .map(|hash| hash.to_string())
                .map_err(|error| match error {
                    at_store::manifest::HashError::Read { source, .. }
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        AdoptError::StaleScanRecord {
                            path: item.path.clone(),
                            current: PathState::Missing,
                        }
                    }
                    other => AdoptError::Store(other.to_string()),
                })?;
            Ok(PathState::Present(hash))
        }
        ItemKind::McpServer => {
            let bytes = match fs::read(&item.path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(PathState::Missing);
                }
                Err(error) => return Err(AdoptError::Store(error.to_string())),
            };
            let entries =
                crate::scan::read_mcp_entries(&item.path, &bytes).map_err(AdoptError::Store)?;
            match entries.get(&item.name) {
                Some(entry) => {
                    let hash = crate::normalize::mcp_entry(entry)
                        .map(|hash| hash.to_string())
                        .map_err(|error| AdoptError::Store(error.to_string()))?;
                    Ok(PathState::Present(hash))
                }
                None => Ok(PathState::Missing),
            }
        }
        _ => match fs::read(&item.path) {
            Ok(bytes) => Ok(PathState::Present(at_fs::sha256_hex(&bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(PathState::Missing),
            Err(error) => Err(AdoptError::Store(error.to_string())),
        },
    }
}

/// Digest of a [`PathState`]; a missing path has none.
fn current_digest(state: &PathState) -> String {
    match state {
        PathState::Present(hash) => hash.clone(),
        PathState::Missing => String::new(),
    }
}

/// Lowercase slug for a native item name, or `None` when nothing usable remains.
fn slug_for(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    for character in name.trim().to_lowercase().chars() {
        if character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '-' | '_' | '.')
        {
            slug.push(character);
        } else if character.is_whitespace() || character == '/' || character == '\\' {
            slug.push('-');
        }
    }
    let slug = slug
        .trim_matches(|character| character == '-' || character == '.')
        .to_owned();
    if slug.is_empty() || Slug::parse(&slug).is_err() {
        None
    } else {
        Some(slug)
    }
}

/// First free slug: `base`, then `base-2`, `base-3`, …
fn unique_slug(base: &str, taken: &BTreeSet<String>) -> Option<String> {
    if !taken.contains(base) {
        return Some(base.to_owned());
    }
    for suffix in 2..=9999 {
        let candidate = format!("{base}-{suffix}");
        if Slug::parse(&candidate).is_ok() && !taken.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

fn other_agent(item: &ScanItem) -> AgentId {
    item.agents
        .iter()
        .find(|agent| **agent != AgentId::ClaudeCode)
        .cloned()
        .unwrap_or(AgentId::Codex)
}

#[cfg(test)]
mod tests {
    use super::{slug_for, unique_slug};
    use std::collections::BTreeSet;

    #[test]
    fn slugs_lowercase_and_replace_separators() {
        assert_eq!(slug_for("Team Rules"), Some("team-rules".to_owned()));
        assert_eq!(slug_for("AGENTS"), Some("agents".to_owned()));
        assert_eq!(slug_for("my_skill"), Some("my_skill".to_owned()));
        assert_eq!(slug_for("  "), None);
        assert_eq!(slug_for("!!!"), None);
    }

    #[test]
    fn collisions_take_the_first_free_suffix() {
        let taken = BTreeSet::from(["rule".to_owned(), "rule-2".to_owned()]);
        assert_eq!(unique_slug("rule", &taken), Some("rule-3".to_owned()));
        assert_eq!(unique_slug("other", &taken), Some("other".to_owned()));
    }
}
