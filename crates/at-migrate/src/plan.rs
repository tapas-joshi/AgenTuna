//! Immutable migration plans and write-free previews.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use at_model::{AgentId, ItemKind, Scope};
use serde::{Deserialize, Serialize};

use crate::conflicts::{Conflict, ConflictSet};
use crate::scan::{ParseStatus, ScanItem, ScanReport};

/// Whether native originals stay in place or are replaced by AgenTuna deployments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MigrationStrategy {
    /// Preserve originals and record them as watched native deployments.
    LeaveAndWatch,
    /// Replace originals with links or copies after backing them up.
    ReplaceWithLinks,
}

/// User decision resolving a named conflict group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ConflictResolution {
    /// Select the first sorted copy.
    KeepA,
    /// Select the second sorted copy.
    KeepB,
    /// Keep both copies under explicit names.
    KeepBoth {
        /// Two validated item names, in A/B copy order.
        names: Vec<String>,
    },
}

/// Ordered operation in a reviewed migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MigrationAction {
    /// Save the native original to the operation backup directory.
    BackupOriginal,
    /// Copy a native item into the central store.
    CreateStoreItem,
    /// Update the canonical `store.json` item manifest.
    UpdateManifest,
    /// Install a link at the native source path.
    WriteLink,
    /// Install a manifest-bearing copy at the native source path.
    WriteCopy,
    /// Keep the native item unchanged and record its watched status.
    RecordWatched,
    /// Include the MCP entry in the store MCP document.
    RenderMcp,
    /// Include the rule content in the store rules document.
    RenderRule,
}

/// One reviewed filesystem or metadata action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    /// Action sequence number, starting at one.
    pub seq: usize,
    /// Action type.
    pub action: MigrationAction,
    /// Selected canonical item name after any conflict rename.
    pub item_name: Option<String>,
    /// Original item name discovered in the agent configuration.
    pub source_item_name: Option<String>,
    /// Existing source path, when applicable.
    pub source: Option<PathBuf>,
    /// Intended destination path, when applicable.
    pub destination: Option<PathBuf>,
    /// Expected source digest at plan time.
    pub expected_source_hash: Option<String>,
    /// Expected destination digest at plan time.
    pub expected_destination_hash: Option<String>,
    /// Operation backup path, when applicable.
    pub backup_path: Option<PathBuf>,
    /// Bytes read or written by this step, when measurable.
    pub bytes: u64,
    /// One representative agent for legacy display; use `agents` for the full shared-root set.
    pub agent: Option<AgentId>,
    /// Every agent that can read this physical native path.
    pub agents: BTreeSet<AgentId>,
    /// Scope of the source item.
    pub scope: Scope,
}

/// Counts summarizing an applicable plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanCounts {
    /// Unique item copies selected.
    pub items: usize,
    /// Number of destinations written.
    pub files_created: usize,
    /// Number of originals backed up.
    pub files_backed_up: usize,
    /// Number of links created.
    pub links_written: usize,
    /// Number of source copies left in place.
    pub files_left_in_place: usize,
}

/// Serializable, hash-bound reviewed migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// Stable ID used as the plan filename.
    pub id: String,
    /// Store scope receiving the selected items.
    pub scope: Scope,
    /// Selected strategy.
    pub strategy: MigrationStrategy,
    /// Ordered steps whose serialized bytes are covered by `plan_hash`.
    pub steps: Vec<PlanStep>,
    /// Derived summary counts.
    pub counts: PlanCounts,
    /// SHA-256 over the serialized steps and strategy.
    pub plan_hash: String,
}

/// Exact, ordered dry-run row for display.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct DryRunRow {
    /// Step sequence.
    pub seq: usize,
    /// Action name.
    pub action: MigrationAction,
    /// Source path.
    pub source: Option<PathBuf>,
    /// Destination path.
    pub destination: Option<PathBuf>,
    /// Byte count.
    pub bytes: u64,
    /// Planned backup destination.
    pub backup_path: Option<PathBuf>,
}

/// Why a plan could not be built or validated.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    /// Nothing was selected for migration.
    #[error("an empty selection cannot form a migration plan")]
    EmptySelection,
    /// A conflict has no explicit resolution.
    #[error("conflict `{name}` requires an explicit resolution")]
    UnresolvedConflict {
        /// Normalized name of the conflict group.
        name: String,
    },
    /// The caller supplied invalid names for KeepBoth.
    #[error("KeepBoth requires exactly two distinct valid names for `{name}`")]
    InvalidKeepBoth {
        /// Name of the item that could not be resolved.
        name: String,
    },
    /// The plan hash or one of its source files changed after review.
    #[error("migration plan is stale")]
    StalePlan,
    /// Plan persistence failed through the safe filesystem pipeline.
    #[error("could not persist migration plan: {0}")]
    Persistence(String),
    /// A selected source could not be read or hashed.
    #[error("cannot read migration source `{path}`: {source}")]
    SourceRead {
        /// Source path.
        path: PathBuf,
        /// Underlying IO failure.
        #[source]
        source: std::io::Error,
    },
    /// Plan serialization or hash computation failed.
    #[error("could not serialize migration plan: {0}")]
    Serialize(#[from] serde_json::Error),
    /// Two selected items would land on one store name on a case-insensitive filesystem.
    #[error(
        "items `{first}` and `{second}` differ only by case; \
         a case-insensitive filesystem would store them as one item"
    )]
    CaseInsensitiveCollision {
        /// First selected name, as reviewed.
        first: String,
        /// Second selected name, as reviewed.
        second: String,
    },
    /// A planned path exceeds the platform's maximum path length.
    #[error("path `{path}` is {length} bytes, over the {limit}-byte platform limit")]
    PathTooLong {
        /// The path that cannot be written.
        path: PathBuf,
        /// Its length in bytes.
        length: usize,
        /// The platform limit in bytes.
        limit: usize,
    },
}

/// Longest path the platform accepts, in bytes.
///
/// `PATH_MAX` on macOS is 1024 and 4096 on Linux; Windows uses the extended-length limit.
/// Planning rejects longer paths before any backup or write exists.
#[must_use]
pub const fn platform_path_limit() -> usize {
    if cfg!(windows) {
        32_767
    } else if cfg!(target_os = "macos") {
        1024
    } else {
        4096
    }
}

/// Parameters used by [`build_plan`].
#[derive(Debug, Clone)]
pub struct BuildPlanRequest<'a> {
    /// Scan result.
    pub scan: &'a ScanReport,
    /// Detected content conflicts.
    pub conflicts: &'a ConflictSet,
    /// Explicit decisions keyed by normalized item name.
    pub resolutions: &'a BTreeMap<String, ConflictResolution>,
    /// Selected native paths. Every path must occur in the scan result.
    pub selected_paths: &'a BTreeSet<PathBuf>,
    /// Store root where items will be created.
    pub store_root: PathBuf,
    /// Deployment mode per agent.
    pub copy_agents: &'a BTreeSet<AgentId>,
    /// Original-preservation strategy.
    pub strategy: MigrationStrategy,
    /// Store scope.
    pub scope: Scope,
    /// Operation backup root.
    pub backup_root: PathBuf,
}

/// Builds an ordered plan from a scan and explicit conflict resolutions.
///
/// # Errors
/// Returns [`PlanError::EmptySelection`], an unresolved conflict or an IO/serialization error.
pub fn build_plan(request: BuildPlanRequest<'_>) -> Result<MigrationPlan, PlanError> {
    if request.selected_paths.is_empty() {
        return Err(PlanError::EmptySelection);
    }
    let selected = request
        .scan
        .items
        .iter()
        .filter(|item| {
            request.selected_paths.contains(&item.path)
                && matches!(item.parse_status, ParseStatus::Parsed)
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(PlanError::EmptySelection);
    }
    let mut steps = Vec::new();
    let mut items = 0usize;
    // Selected store names by `(kind, lowercased name)`: the value keeps the reviewed spelling
    // so a collision can name both names the user saw.
    let mut selected_names: BTreeMap<(ItemKind, String), String> = BTreeMap::new();
    for item in selected {
        let normalized_name = normalize(&item.name);
        let conflict = request
            .conflicts
            .conflicts
            .iter()
            .find(|conflict| conflict.name == normalized_name && conflict.kind == item.kind);
        let resolution = if conflict.is_some() {
            Some(request.resolutions.get(&normalized_name).ok_or_else(|| {
                PlanError::UnresolvedConflict {
                    name: normalized_name.clone(),
                }
            })?)
        } else {
            None
        };
        let names = choose_names(item, conflict, resolution)?;
        for selected_name in names {
            check_selected_name(&mut selected_names, item.kind, &selected_name)?;
            let (digest, bytes) = hash_source(item)?;
            let store_path = item_destination(&request.store_root, item, &selected_name);
            ensure_path_length(&store_path)?;
            let backup_path = request.backup_root.join(&selected_name);
            ensure_path_length(&backup_path)?;
            if request.strategy == MigrationStrategy::ReplaceWithLinks
                && item.kind == ItemKind::Skill
            {
                steps.push(PlanStep {
                    seq: 0,
                    action: MigrationAction::BackupOriginal,
                    item_name: Some(selected_name.clone()),
                    source_item_name: Some(item.name.clone()),
                    source: Some(item.path.clone()),
                    destination: None,
                    expected_source_hash: Some(digest.clone()),
                    expected_destination_hash: None,
                    backup_path: Some(backup_path),
                    bytes,
                    agent: None,
                    agents: BTreeSet::new(),
                    scope: item.scope,
                });
            }
            let action = match item.kind {
                ItemKind::McpServer => MigrationAction::RenderMcp,
                ItemKind::Rule => MigrationAction::RenderRule,
                _ => MigrationAction::CreateStoreItem,
            };
            steps.push(PlanStep {
                seq: 0,
                action,
                item_name: Some(selected_name.clone()),
                source_item_name: Some(item.name.clone()),
                source: Some(item.path.clone()),
                destination: Some(store_path.clone()),
                expected_source_hash: Some(digest.clone()),
                expected_destination_hash: None,
                backup_path: None,
                bytes,
                agent: item.agents.iter().next().cloned(),
                agents: item.agents.clone(),
                scope: item.scope,
            });
            if item.kind == ItemKind::Skill {
                let action = if request.strategy == MigrationStrategy::LeaveAndWatch {
                    MigrationAction::RecordWatched
                } else if !item.agents.is_empty()
                    && item
                        .agents
                        .iter()
                        .all(|agent| request.copy_agents.contains(agent))
                {
                    MigrationAction::WriteCopy
                } else {
                    MigrationAction::WriteLink
                };
                let watched = action == MigrationAction::RecordWatched;
                steps.push(PlanStep {
                    seq: 0,
                    action,
                    item_name: Some(selected_name.clone()),
                    source_item_name: Some(item.name.clone()),
                    source: Some(if watched {
                        item.path.clone()
                    } else {
                        store_path.clone()
                    }),
                    destination: (!watched).then(|| item.path.clone()),
                    expected_source_hash: Some(digest.clone()),
                    expected_destination_hash: None,
                    backup_path: None,
                    bytes,
                    agent: item.agents.iter().next().cloned(),
                    agents: item.agents.clone(),
                    scope: item.scope,
                });
            }
            items += 1;
        }
    }
    if steps.iter().any(|step| {
        matches!(
            step.action,
            MigrationAction::CreateStoreItem
                | MigrationAction::RenderMcp
                | MigrationAction::RenderRule
        )
    }) {
        let manifest_path = request.store_root.join(at_store::layout::MANIFEST_FILE);
        let manifest_hash = hash_path(&manifest_path)?;
        steps.push(PlanStep {
            seq: 0,
            action: MigrationAction::UpdateManifest,
            item_name: None,
            source_item_name: None,
            source: None,
            destination: Some(manifest_path),
            expected_source_hash: None,
            expected_destination_hash: Some(manifest_hash),
            backup_path: None,
            bytes: 0,
            agent: None,
            agents: BTreeSet::new(),
            scope: request.scope,
        });
    }
    steps.sort_by_key(|step| match step.action {
        MigrationAction::BackupOriginal => 0,
        MigrationAction::CreateStoreItem
        | MigrationAction::RenderMcp
        | MigrationAction::RenderRule => 1,
        MigrationAction::UpdateManifest => 2,
        MigrationAction::WriteLink
        | MigrationAction::WriteCopy
        | MigrationAction::RecordWatched => 3,
    });
    for (index, step) in steps.iter_mut().enumerate() {
        step.seq = index + 1;
    }
    if items == 0 {
        return Err(PlanError::EmptySelection);
    }
    let files_left_in_place = steps
        .iter()
        .filter(|step| step.action == MigrationAction::RecordWatched)
        .filter_map(|step| step.source.as_ref())
        .collect::<BTreeSet<_>>()
        .len();
    let counts = PlanCounts {
        items,
        files_created: steps
            .iter()
            .filter(|step| {
                matches!(
                    step.action,
                    MigrationAction::CreateStoreItem
                        | MigrationAction::UpdateManifest
                        | MigrationAction::RenderMcp
                        | MigrationAction::RenderRule
                        | MigrationAction::WriteLink
                        | MigrationAction::WriteCopy
                )
            })
            .count(),
        files_backed_up: steps
            .iter()
            .filter(|step| step.action == MigrationAction::BackupOriginal)
            .count(),
        links_written: steps
            .iter()
            .filter(|step| step.action == MigrationAction::WriteLink)
            .count(),
        files_left_in_place,
    };
    let id = at_model::ItemId::new().to_string();
    let hash_payload = serde_json::to_vec(&(request.strategy, &steps))?;
    let plan_hash = at_fs::sha256_hex(&hash_payload);
    Ok(MigrationPlan {
        id,
        scope: request.scope,
        strategy: request.strategy,
        steps,
        counts,
        plan_hash,
    })
}

/// Projects a plan into ordered display rows without reading or writing any files.
#[must_use]
pub fn dry_run(plan: &MigrationPlan) -> Vec<DryRunRow> {
    plan.steps
        .iter()
        .map(|step| DryRunRow {
            seq: step.seq,
            action: step.action.clone(),
            source: step.source.clone(),
            destination: step.destination.clone(),
            bytes: step.bytes,
            backup_path: step.backup_path.clone(),
        })
        .collect()
}

fn choose_names(
    item: &ScanItem,
    conflict: Option<&Conflict>,
    resolution: Option<&ConflictResolution>,
) -> Result<Vec<String>, PlanError> {
    let Some(conflict) = conflict else {
        return Ok(vec![item.name.clone()]);
    };
    let index = conflict
        .copies
        .iter()
        .position(|copy| copy.path == item.path)
        .ok_or_else(|| PlanError::UnresolvedConflict {
            name: conflict.name.clone(),
        })?;
    match resolution {
        Some(ConflictResolution::KeepA) if index == 0 => Ok(vec![item.name.clone()]),
        Some(ConflictResolution::KeepA) => Ok(Vec::new()),
        Some(ConflictResolution::KeepB) if index == 1 => Ok(vec![item.name.clone()]),
        Some(ConflictResolution::KeepB) => Ok(Vec::new()),
        Some(ConflictResolution::KeepBoth { names })
            if names.len() == 2
                && names[0] != names[1]
                && names.iter().all(|name| valid_name(name))
                && index < names.len() =>
        {
            Ok(vec![names[index].clone()])
        }
        Some(ConflictResolution::KeepBoth { .. }) => Err(PlanError::InvalidKeepBoth {
            name: item.name.clone(),
        }),
        None => Err(PlanError::UnresolvedConflict {
            name: conflict.name.clone(),
        }),
    }
}

/// Rechecks the plan hash and every source hash immediately before application.
///
/// # Errors
/// Returns [`PlanError::StalePlan`] if the reviewed data or any source changed.
pub fn validate_plan(plan: &MigrationPlan) -> Result<(), PlanError> {
    let payload = serde_json::to_vec(&(plan.strategy, &plan.steps))?;
    if at_fs::sha256_hex(&payload) != plan.plan_hash {
        return Err(PlanError::StalePlan);
    }
    let mut selected_names: BTreeMap<(ItemKind, String), String> = BTreeMap::new();
    for step in &plan.steps {
        if step.action == MigrationAction::UpdateManifest
            && let (Some(destination), Some(expected)) =
                (&step.destination, &step.expected_destination_hash)
            && &hash_path(destination)? != expected
        {
            return Err(PlanError::StalePlan);
        }
        if let Some(destination) = &step.destination {
            ensure_path_length(destination)?;
        }
        if let Some(backup_path) = &step.backup_path {
            ensure_path_length(backup_path)?;
        }
        if let (Some(kind), Some(name)) = (store_kind(&step.action), step.item_name.as_deref()) {
            check_selected_name(&mut selected_names, kind, name)?;
        }
        if !matches!(
            step.action,
            MigrationAction::BackupOriginal
                | MigrationAction::CreateStoreItem
                | MigrationAction::RenderMcp
                | MigrationAction::RenderRule
                | MigrationAction::RecordWatched
        ) {
            continue;
        }
        let (Some(source), Some(expected)) = (&step.source, &step.expected_source_hash) else {
            return Err(PlanError::StalePlan);
        };
        let actual = hash_path(source)?;
        if &actual != expected {
            return Err(PlanError::StalePlan);
        }
    }
    Ok(())
}

/// Persists a reviewed plan beneath `<store>/.state/plans/` through at-fs.
///
/// # Errors
/// Returns an IO or safe-write error without replacing an existing plan.
pub fn persist_plan(
    store_root: &std::path::Path,
    plan: &MigrationPlan,
) -> Result<PathBuf, PlanError> {
    let path = store_root
        .join(".state")
        .join("plans")
        .join(format!("{}.json", plan.id));
    at_fs::guard::create_parents_if_target_absent(&path)
        .map_err(|error| PlanError::Persistence(error.to_string()))?;
    let bytes = serde_json::to_vec_pretty(plan)?;
    at_fs::apply(&at_fs::WriteRequest::create(&path, bytes))
        .map_err(|error| PlanError::Persistence(error.to_string()))?;
    Ok(path)
}

fn hash_path(path: &std::path::Path) -> Result<String, PlanError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PlanError::SourceRead {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_dir() {
        at_store::manifest::hash_skill_folder(path)
            .map(|hash| hash.to_string())
            .map_err(|error| PlanError::SourceRead {
                path: path.to_path_buf(),
                source: std::io::Error::other(error),
            })
    } else {
        let bytes = fs::read(path).map_err(|source| PlanError::SourceRead {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(at_fs::sha256_hex(&bytes))
    }
}

fn item_destination(root: &std::path::Path, item: &ScanItem, name: &str) -> PathBuf {
    match item.kind {
        // Store skill folders are slugs: `skills/<slug>/`, so the reviewed name is lowercased.
        ItemKind::Skill => root.join("skills").join(name.to_ascii_lowercase()),
        ItemKind::Rule => root
            .join("rules")
            .join(format!("{}.md", name.to_ascii_lowercase())),
        ItemKind::McpServer => root.join("mcp.json"),
    }
}

fn hash_source(item: &ScanItem) -> Result<(String, u64), PlanError> {
    let metadata = fs::symlink_metadata(&item.path).map_err(|source| PlanError::SourceRead {
        path: item.path.clone(),
        source,
    })?;
    if metadata.is_dir() {
        let hash = at_store::manifest::hash_skill_folder(&item.path).map_err(|error| {
            PlanError::SourceRead {
                path: item.path.clone(),
                source: std::io::Error::other(error),
            }
        })?;
        Ok((hash.to_string(), metadata.len()))
    } else {
        let bytes = fs::read(&item.path).map_err(|source| PlanError::SourceRead {
            path: item.path.clone(),
            source,
        })?;
        Ok((at_fs::sha256_hex(&bytes), bytes.len() as u64))
    }
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

/// Rejects two selected names that differ only by case.
///
/// Store skill folders and rule files are lowercased, so `Foo` and `foo` would land on one
/// path on a case-insensitive filesystem (the macOS default) and silently overwrite each other.
/// The check runs while planning, before any backup or write exists.
fn check_selected_name(
    names: &mut BTreeMap<(ItemKind, String), String>,
    kind: ItemKind,
    name: &str,
) -> Result<(), PlanError> {
    let key = (kind, name.to_ascii_lowercase());
    match names.get(&key) {
        Some(existing) if existing != name => Err(PlanError::CaseInsensitiveCollision {
            first: existing.clone(),
            second: name.to_owned(),
        }),
        Some(_) => Ok(()),
        None => {
            names.insert(key, name.to_owned());
            Ok(())
        }
    }
}

/// The canonical kind a store-writing step represents.
fn store_kind(action: &MigrationAction) -> Option<ItemKind> {
    match action {
        MigrationAction::CreateStoreItem => Some(ItemKind::Skill),
        MigrationAction::RenderMcp => Some(ItemKind::McpServer),
        MigrationAction::RenderRule => Some(ItemKind::Rule),
        _ => None,
    }
}

/// Rejects a path longer than [`platform_path_limit`].
///
/// The limit is counted in bytes, as the operating system counts it; a rejected plan never
/// reaches the backup or write stage.
fn ensure_path_length(path: &std::path::Path) -> Result<(), PlanError> {
    let length = path.as_os_str().as_encoded_bytes().len();
    let limit = platform_path_limit();
    if length > limit {
        return Err(PlanError::PathTooLong {
            path: path.to_path_buf(),
            length,
            limit,
        });
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}
