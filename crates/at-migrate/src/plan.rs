//! Immutable migration plans and write-free previews.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

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
    /// The native link this step replaces or records, captured at plan time.
    ///
    /// `None` for an item that is not a symbolic link and for steps that do not touch the
    /// native entry. A plan persisted before link snapshots existed deserializes without it
    /// and is then refused as stale, because its hash no longer matches.
    #[serde(default)]
    pub planned_link: Option<PlannedLink>,
}

/// The native symbolic link a step replaces or records, as it was when the plan was built.
///
/// A manager-installed skill is a link into a library, and the plan replaces that link. Between
/// the preview the user reviewed and the apply, the same path can be retargeted — a link whose
/// content hash stays equal is still a different entry, and replacing it would silently discard
/// the user's change. The snapshot travels inside the plan (and therefore inside its hash), and
/// [`validate_plan`] re-reads the link before anything is backed up or written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedLink {
    /// The link entry itself.
    pub path: PathBuf,
    /// Target text the link held, exactly as `read_link` reported it.
    pub text: PathBuf,
    /// Canonical folder the link resolved to.
    pub target: PathBuf,
    /// Size of the link entry from `lstat`.
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch, when the platform reported one.
    pub modified_unix_nanos: Option<u64>,
    /// Device and inode on Unix; `None` where the platform exposes no stable entry id.
    pub device_inode: Option<(u64, u64)>,
}

impl PlannedLink {
    /// Reads the native link a scan record described.
    ///
    /// Returns `Ok(None)` for a real folder and [`PlanError::StalePlan`] when the entry is no
    /// longer the link the scan recorded (retargeted, replaced by a real folder, or gone).
    fn capture(item: &ScanItem) -> Result<Option<Self>, PlanError> {
        let Some(text) = item.link_target.as_deref() else {
            let meta = fs::symlink_metadata(&item.path).map_err(|_| PlanError::StalePlan)?;
            if meta.file_type().is_symlink() {
                return Err(PlanError::StalePlan);
            }
            return Ok(None);
        };
        let resolved = item.resolved.as_deref().ok_or(PlanError::StalePlan)?;
        let snapshot = Self::read(&item.path, text, resolved)?;
        Ok(Some(snapshot))
    }

    /// Records `path` only when it is still the link the scan described.
    fn read(path: &Path, text: &Path, resolved: &Path) -> Result<Self, PlanError> {
        let meta = fs::symlink_metadata(path).map_err(|_| PlanError::StalePlan)?;
        if !meta.file_type().is_symlink() {
            return Err(PlanError::StalePlan);
        }
        let actual = fs::read_link(path).map_err(|_| PlanError::StalePlan)?;
        if actual != text {
            return Err(PlanError::StalePlan);
        }
        let target = fs::canonicalize(path).map_err(|_| PlanError::StalePlan)?;
        if target != resolved {
            return Err(PlanError::StalePlan);
        }
        Ok(Self {
            path: path.to_path_buf(),
            text: actual,
            target,
            size: meta.len(),
            modified_unix_nanos: modified_unix_nanos(&meta),
            device_inode: device_inode(&meta),
        })
    }

    /// Re-reads the link and refuses when it is no longer the recorded entry.
    ///
    /// # Errors
    /// [`PlanError::StalePlan`] when the path is gone, is no longer a symbolic link, carries
    /// different target text, resolves to a different folder, or is a different entry on disk
    /// (lstat identity or modification time changed).
    pub fn verify(&self) -> Result<(), PlanError> {
        let meta = fs::symlink_metadata(&self.path).map_err(|_| PlanError::StalePlan)?;
        if !meta.file_type().is_symlink() {
            return Err(PlanError::StalePlan);
        }
        if fs::read_link(&self.path).map_err(|_| PlanError::StalePlan)? != self.text {
            return Err(PlanError::StalePlan);
        }
        if fs::canonicalize(&self.path).map_err(|_| PlanError::StalePlan)? != self.target {
            return Err(PlanError::StalePlan);
        }
        if meta.len() != self.size
            || modified_unix_nanos(&meta) != self.modified_unix_nanos
            || device_inode(&meta) != self.device_inode
        {
            return Err(PlanError::StalePlan);
        }
        Ok(())
    }
}

/// Modification time of one entry, in nanoseconds since the Unix epoch.
fn modified_unix_nanos(meta: &fs::Metadata) -> Option<u64> {
    let modified = meta.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_nanos()).ok()
}

/// Device and inode of one entry on Unix; `None` where `std` exposes no stable entry id.
fn device_inode(meta: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
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
    // Physical content already written to the store by this plan. Two agent links that resolve
    // to one library folder are two deployments of one skill, not two store items: the store
    // copy is written once and every link is replaced.
    let mut planned_content: BTreeMap<(ItemKind, PathBuf), String> = BTreeMap::new();
    let mut planned_backups: BTreeSet<PathBuf> = BTreeSet::new();
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
            // A skill entry that is a link is snapshotted as it is now: the plan binds the
            // reviewed target and entry identity, so a retarget between preview and apply is
            // refused instead of replacing a link the user changed.
            let planned_link = if item.kind == ItemKind::Skill {
                PlannedLink::capture(item)?
            } else {
                None
            };
            let (digest, bytes) = hash_source(item)?;
            let store_path = item_destination(&request.store_root, item, &selected_name);
            ensure_path_length(&store_path)?;
            let mut backup_path = request.backup_root.join(&selected_name);
            let mut suffix = 2usize;
            while planned_backups.contains(&backup_path) {
                backup_path = request
                    .backup_root
                    .join(format!("{selected_name}-{suffix}"));
                suffix += 1;
            }
            ensure_path_length(&backup_path)?;
            if request.strategy == MigrationStrategy::ReplaceWithLinks
                && item.kind == ItemKind::Skill
            {
                planned_backups.insert(backup_path.clone());
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
                    planned_link: planned_link.clone(),
                });
            }
            let action = match item.kind {
                ItemKind::McpServer => MigrationAction::RenderMcp,
                ItemKind::Rule => MigrationAction::RenderRule,
                _ => MigrationAction::CreateStoreItem,
            };
            // The store item is copied from the folder that holds the content. For a
            // manager-installed link that is the validated target, not the link entry:
            // copying the entry would put a link inside the store.
            let content_source = if item.kind == ItemKind::Skill {
                item.resolved.clone().unwrap_or_else(|| item.path.clone())
            } else {
                item.path.clone()
            };
            // A second agent's link to the same library folder reuses the one store item the
            // first link planned: one write, one manifest entry, two deployments.
            let shares_content = planned_content
                .get(&(item.kind, content_source.clone()))
                .is_some_and(|existing| *existing == selected_name);
            if !shares_content {
                if item.kind == ItemKind::Skill {
                    planned_content
                        .insert((item.kind, content_source.clone()), selected_name.clone());
                }
                steps.push(PlanStep {
                    seq: 0,
                    action,
                    item_name: Some(selected_name.clone()),
                    source_item_name: Some(item.name.clone()),
                    source: Some(content_source),
                    destination: Some(store_path.clone()),
                    expected_source_hash: Some(digest.clone()),
                    expected_destination_hash: None,
                    backup_path: None,
                    bytes,
                    agent: item.agents.iter().next().cloned(),
                    agents: item.agents.clone(),
                    scope: item.scope,
                    planned_link: None,
                });
            }
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
                    planned_link: planned_link.clone(),
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
            planned_link: None,
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
            planned_link: None,
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
        // The native entry the plan replaces must still be the link the user reviewed: a
        // retargeted link keeps an unchanged content hash when both targets hold the same
        // bytes, so the entry itself is compared.
        if let Some(link) = &step.planned_link {
            link.verify()?;
        }
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
        return at_store::manifest::hash_skill_folder(path)
            .map(|hash| hash.to_string())
            .map_err(|error| PlanError::SourceRead {
                path: path.to_path_buf(),
                source: std::io::Error::other(error),
            });
    }
    if metadata.file_type().is_symlink() {
        // A manager-installed skill is a link to a folder, and the plan's source hash is that
        // folder's content. Validation must read the same content the plan did; the apply never
        // writes through the link, so this follows it only to hash.
        if let Ok(target) = fs::canonicalize(path)
            && target.is_dir()
        {
            // The target is canonical, but it may have been swapped for a symlink after the
            // scan validated it; re-check it at use before reading any content.
            let validated = at_store::manifest::ValidatedSkillRoot::from_trusted_scan(&target)
                .map_err(|error| PlanError::SourceRead {
                    path: path.to_path_buf(),
                    source: std::io::Error::other(error),
                })?;
            return at_store::manifest::hash_skill_folder_resolved(&validated)
                .map(|hash| hash.to_string())
                .map_err(|error| PlanError::SourceRead {
                    path: path.to_path_buf(),
                    source: std::io::Error::other(error),
                });
        }
        // A link to a file hashes by its target text, the way the write pipeline records links.
        let target = fs::read_link(path).map_err(|source| PlanError::SourceRead {
            path: path.to_path_buf(),
            source,
        })?;
        return Ok(at_fs::sha256_hex(target.as_os_str().as_encoded_bytes()));
    }
    let bytes = fs::read(path).map_err(|source| PlanError::SourceRead {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(at_fs::sha256_hex(&bytes))
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
    // A manager-installed skill is a link; the content lives at the target the adapter already
    // resolved and validated. Hashing the entry would either follow a link (copying is still
    // refused) or, for a link to a folder, read it as a file.
    let source = item.resolved.as_deref().unwrap_or(&item.path);
    let metadata = fs::metadata(source).map_err(|source_error| PlanError::SourceRead {
        path: item.path.clone(),
        source: source_error,
    })?;
    if metadata.is_dir() {
        let hash = hash_folder(source, item.resolved.is_some()).map_err(|error| {
            PlanError::SourceRead {
                path: item.path.clone(),
                source: std::io::Error::other(error),
            }
        })?;
        Ok((hash.to_string(), metadata.len()))
    } else {
        let bytes = fs::read(source).map_err(|source_error| PlanError::SourceRead {
            path: item.path.clone(),
            source: source_error,
        })?;
        Ok((at_fs::sha256_hex(&bytes), bytes.len() as u64))
    }
}

/// Hashes a skill folder, allowing a resolved root only when the adapter produced one.
fn hash_folder(
    root: &std::path::Path,
    resolved: bool,
) -> Result<at_store::manifest::ContentHash, at_store::manifest::HashError> {
    if resolved {
        let validated = at_store::manifest::ValidatedSkillRoot::from_trusted_scan(root)?;
        at_store::hash_skill_folder_resolved(&validated)
    } else {
        at_store::manifest::hash_skill_folder(root)
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
