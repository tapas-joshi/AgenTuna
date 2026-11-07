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
    /// Create the canonical store skeleton before the item steps.
    CreateStore,
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
    ///
    /// One plan never spans scopes: a selection that mixes native scopes becomes one plan per
    /// receiving scope, so this is the scope of every item in `steps`.
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

/// Store roots a migration writes into, keyed by receiving scope.
///
/// One reviewed selection can span native scopes: a user-level skill and a project-level rule
/// belong in two different stores. This type carries the root the caller resolved for each
/// scope, so planning never guesses a directory from a scope name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeStores {
    roots: BTreeMap<Scope, PathBuf>,
}

impl ScopeStores {
    /// An empty set; every lookup misses.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A set holding one scope's store root.
    #[must_use]
    pub fn single(scope: Scope, root: impl Into<PathBuf>) -> Self {
        let mut stores = Self::new();
        stores.insert(scope, root);
        stores
    }

    /// Registers the store root of `scope`, replacing any earlier root.
    pub fn insert(&mut self, scope: Scope, root: impl Into<PathBuf>) {
        self.roots.insert(scope, root.into());
    }

    /// Store root of `scope`, when this set has one.
    #[must_use]
    pub fn root(&self, scope: Scope) -> Option<&std::path::Path> {
        self.roots.get(&scope).map(PathBuf::as_path)
    }

    /// Registered scopes, in precedence order.
    pub fn scopes(&self) -> impl Iterator<Item = Scope> + '_ {
        self.roots.keys().copied()
    }

    /// Whether no scope has a root.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
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
    /// No store root was supplied for a scope that receives items.
    #[error("no {scope} store is available for this migration")]
    MissingStore {
        /// Receiving scope whose store root was not supplied.
        scope: Scope,
    },
    /// A Project or Local item needs a selected project.
    #[error("{scope}-scope items need an active project; select one and rebuild the plan")]
    MissingProject {
        /// Scope that needs the project.
        scope: Scope,
    },
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
    /// A reviewed name cannot be a valid store name.
    #[error("`{name}` cannot be stored as a {kind} name: {reason}")]
    InvalidName {
        /// Name shown in the review.
        name: String,
        /// Item kind whose store name is invalid.
        kind: &'static str,
        /// The slug parser's reason.
        reason: String,
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
            let stored_name = normalize(&selected_name);
            if item.kind == ItemKind::Skill
                && let Err(error) = at_store::layout::Slug::parse(&stored_name)
            {
                return Err(PlanError::InvalidName {
                    name: selected_name,
                    kind: "skill",
                    reason: error.to_string(),
                });
            }
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
            // Skills are deployed at their own path; MCP and rule items live inside a shared
            // agent config, so they are only recorded as watched and never linked.
            let replaced = item.kind == ItemKind::Skill
                && request.strategy == MigrationStrategy::ReplaceWithLinks;
            let watched = item.kind == ItemKind::Skill
                || request.strategy == MigrationStrategy::LeaveAndWatch;
            if replaced || watched {
                let action = if !replaced {
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
        let manifest_hash = match fs::symlink_metadata(&manifest_path) {
            Ok(_) => Some(hash_path(&manifest_path)?),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(PlanError::SourceRead {
                    path: manifest_path.clone(),
                    source,
                });
            }
        };
        steps.push(PlanStep {
            seq: 0,
            action: MigrationAction::UpdateManifest,
            item_name: None,
            source_item_name: None,
            source: None,
            destination: Some(manifest_path),
            expected_source_hash: None,
            expected_destination_hash: manifest_hash,
            backup_path: None,
            bytes: 0,
            agent: None,
            agents: BTreeSet::new(),
            scope: request.scope,
        });
    }
    // A first migration into a scope with no store yet starts with its skeleton, so the
    // reviewed plan and its dry run show the store creation before any item step. Apply merges
    // the same layout operations into the ChangeSet that carries the items.
    if !at_store::layout::StoreLayout::new(request.scope, &request.store_root).is_ready() {
        steps.push(PlanStep {
            seq: 0,
            action: MigrationAction::CreateStore,
            item_name: None,
            source_item_name: None,
            source: None,
            destination: Some(request.store_root.clone()),
            expected_source_hash: None,
            expected_destination_hash: None,
            backup_path: None,
            bytes: 0,
            agent: None,
            agents: BTreeSet::new(),
            scope: request.scope,
        });
    }
    steps.sort_by_key(|step| match step.action {
        MigrationAction::CreateStore => 0,
        MigrationAction::BackupOriginal => 1,
        MigrationAction::CreateStoreItem
        | MigrationAction::RenderMcp
        | MigrationAction::RenderRule => 2,
        MigrationAction::UpdateManifest => 3,
        MigrationAction::WriteLink
        | MigrationAction::WriteCopy
        | MigrationAction::RecordWatched => 4,
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

/// Parameters for [`build_scope_plans`].
#[derive(Debug, Clone)]
pub struct ScopePlansRequest<'a> {
    /// Scan result covering the whole selection.
    pub scan: &'a ScanReport,
    /// Explicit decisions keyed by normalized item name, applied in every scope they occur.
    pub resolutions: &'a BTreeMap<String, ConflictResolution>,
    /// Selected native paths. Every path must occur in the scan result.
    pub selected_paths: &'a BTreeSet<PathBuf>,
    /// Store root per receiving scope.
    pub stores: &'a ScopeStores,
    /// Explicit target scope for every item; `None` keeps each item's discovered scope.
    pub target_scope: Option<Scope>,
    /// Registered project id; every Project or Local plan requires one.
    pub project_id: Option<&'a str>,
    /// Deployment mode per agent.
    pub copy_agents: &'a BTreeSet<AgentId>,
    /// Original-preservation strategy.
    pub strategy: MigrationStrategy,
    /// Operation backup root.
    pub backup_root: PathBuf,
}

/// Builds one reviewed plan per receiving scope from one selection.
///
/// Each item is migrated into the store of the scope it was discovered in — a user-level
/// native skill into the User store, a project-level rule into the Project store — unless
/// `target_scope` overrides every item explicitly. Conflicts are detected inside each scope,
/// because two same-named copies in different scopes land in different stores and never
/// contend for one name.
///
/// The plans come back in precedence order (Global, User, Project, Local); the first one is the
/// operation root the apply uses for its journal and lock.
///
/// # Errors
/// Returns [`PlanError::EmptySelection`], [`PlanError::MissingProject`] when a Project or Local
/// item is planned without a selected project, [`PlanError::MissingStore`] for a receiving scope
/// the caller supplied no root for, an unresolved conflict, or an IO/serialization error.
pub fn build_scope_plans(request: ScopePlansRequest<'_>) -> Result<Vec<MigrationPlan>, PlanError> {
    let mut selected_by_scope: BTreeMap<Scope, BTreeSet<PathBuf>> = BTreeMap::new();
    for item in &request.scan.items {
        if !request.selected_paths.contains(&item.path) {
            continue;
        }
        let target = request.target_scope.unwrap_or(item.scope);
        selected_by_scope
            .entry(target)
            .or_default()
            .insert(item.path.clone());
    }
    if selected_by_scope.is_empty() {
        return Err(PlanError::EmptySelection);
    }
    let mut plans = Vec::with_capacity(selected_by_scope.len());
    for (target, selected_paths) in selected_by_scope {
        if matches!(target, Scope::Project | Scope::Local) && request.project_id.is_none() {
            return Err(PlanError::MissingProject { scope: target });
        }
        let Some(store_root) = request.stores.root(target) else {
            return Err(PlanError::MissingStore { scope: target });
        };
        let subset = ScanReport {
            roots: Vec::new(),
            items: request
                .scan
                .items
                .iter()
                .filter(|item| selected_paths.contains(&item.path))
                .cloned()
                .collect(),
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let conflicts = crate::conflicts::detect_conflicts(&subset);
        let plan = build_plan(BuildPlanRequest {
            scan: &subset,
            conflicts: &conflicts,
            resolutions: request.resolutions,
            selected_paths: &selected_paths,
            store_root: store_root.to_path_buf(),
            copy_agents: request.copy_agents,
            strategy: request.strategy,
            scope: target,
            backup_root: request.backup_root.clone(),
        })?;
        validate_plan(&plan)?;
        plans.push(plan);
    }
    Ok(plans)
}

/// Stable hash of one reviewed selection across every receiving scope.
///
/// Apply rebuilds the plans from disk and compares this hash against the reviewed one, so it
/// binds every scope, step and the strategy without depending on the random plan ids.
///
/// # Errors
/// Returns [`PlanError::Serialize`] when the plans cannot be serialized.
pub fn plans_hash(plans: &[MigrationPlan]) -> Result<String, PlanError> {
    let payload = plans
        .iter()
        .map(|plan| (plan.scope, plan.strategy, &plan.steps))
        .collect::<Vec<_>>();
    Ok(at_fs::sha256_hex(&serde_json::to_vec(&payload)?))
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

/// One persisted reviewed selection: every plan and the hash binding them.
///
/// A selection that spans scopes is persisted as one document keyed by [`Self::id`], so the
/// reviewed apply can be loaded and confirmed as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedPlans {
    /// Stable id used as the plan file name.
    pub id: String,
    /// SHA-256 over every plan, as reviewed.
    pub plan_hash: String,
    /// One plan per receiving scope, in precedence order.
    pub plans: Vec<MigrationPlan>,
}

impl PersistedPlans {
    /// File that holds this selection inside `store_root`.
    #[must_use]
    pub fn path_in(&self, store_root: &std::path::Path) -> PathBuf {
        store_root
            .join(".state")
            .join("plans")
            .join(format!("{}.json", self.id))
    }
}

/// Builds the persisted document for one reviewed selection without writing it.
///
/// # Errors
/// Returns [`PlanError::EmptySelection`] for an empty selection or [`PlanError::Serialize`] when
/// the plans cannot be hashed.
pub fn plans_document(plans: &[MigrationPlan]) -> Result<PersistedPlans, PlanError> {
    if plans.is_empty() {
        return Err(PlanError::EmptySelection);
    }
    Ok(PersistedPlans {
        id: at_model::ItemId::new().to_string(),
        plan_hash: plans_hash(plans)?,
        plans: plans.to_vec(),
    })
}

/// Persists one reviewed selection beneath `<store_root>/.state/plans/` through at-fs.
///
/// # Errors
/// Returns an IO or safe-write error without replacing an existing plan file.
pub fn persist_plans(
    store_root: &std::path::Path,
    document: &PersistedPlans,
) -> Result<PathBuf, PlanError> {
    let path = document.path_in(store_root);
    at_fs::guard::create_parents_if_target_absent(&path)
        .map_err(|error| PlanError::Persistence(error.to_string()))?;
    let bytes = serde_json::to_vec_pretty(document)?;
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
        ItemKind::Skill => root.join("skills").join(normalize(name)),
        ItemKind::Rule => root.join("rules").join(format!("{}.md", normalize(name))),
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
    // Whitespace and underscores are separators in a skill name but not in a store slug; a
    // native skill called `Native Migrate` is stored as `native-migrate`.
    name.trim()
        .to_lowercase()
        .replace('_', "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
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
    let key = (kind, normalize(name));
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
