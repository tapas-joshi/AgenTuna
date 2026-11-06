//! Project-store consumption of TA-005 sync labels.
//!
//! This module never derives labels. It caches exactly what its provider reports and
//! maps each reported label to the action permitted by the product contract.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::state::{DeploymentFacts, DerivedState, classify};
use at_db::deployments::list_deployments_for_item;
use at_db::{Database, DbError};
use at_fs::changeset::{ApplyContext, ApplyOutcome, ChangeSetApplyError};
use at_model::{
    ChangeSet, ExpectedState as ModelExpectedState, FileOp, ItemId, OperationKind, Scope, SyncLabel,
};

/// The UI action allowed for a project/library relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncAction {
    /// No operation is needed because both copies agree.
    None,
    /// Copy the project item to the library.
    Push,
    /// Copy the library item into the project.
    Pull,
    /// Ask the user to compare the divergent copies.
    Compare,
}

/// Evidence hashes TA-005 used while deriving a sync label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEvidence {
    /// Project-store item hash.
    pub project_hash: Option<String>,
    /// User-library item hash.
    pub library_hash: Option<String>,
    /// Last common base hash.
    pub base_hash: Option<String>,
}

/// Unmodified TA-005 label with its evidence and state version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRecord {
    /// The label produced by TA-005.
    pub label: SyncLabel,
    /// Hashes recorded by TA-005.
    pub evidence: SyncEvidence,
}

impl SyncRecord {
    /// Copies a non-empty TA-005 result without deriving or changing its label.
    #[must_use]
    pub fn from_derived(derived: DerivedState<'_>) -> Option<Self> {
        Some(Self {
            label: derived.sync_label?,
            evidence: SyncEvidence {
                project_hash: derived.evidence.project_hash.map(str::to_owned),
                library_hash: derived.evidence.library_hash.map(str::to_owned),
                base_hash: derived.evidence.base_hash.map(str::to_owned),
            },
        })
    }
}

/// Source of TA-005 classifications for Project-scope items.
pub trait ProjectSyncProvider {
    /// Version that changes whenever TA-005 source state changes.
    fn state_version(&self) -> u64;

    /// Returns TA-005's existing classification for one item, without recomputing it.
    fn sync_record(&self, item_id: ItemId) -> Result<Option<SyncRecord>, DbError>;
}

/// Reads Project deployment evidence and delegates label computation to TA-005.
pub struct DatabaseProjectSyncProvider<'a> {
    /// Database containing the deployment index.
    pub database: &'a Database,
    /// Project registry id whose deployment row is consumed.
    pub project_id: &'a str,
    /// State version supplied by TA-005's owning state.
    pub version: u64,
}

impl ProjectSyncProvider for DatabaseProjectSyncProvider<'_> {
    fn state_version(&self) -> u64 {
        self.version
    }

    fn sync_record(&self, item_id: ItemId) -> Result<Option<SyncRecord>, DbError> {
        let rows = list_deployments_for_item(self.database.connection(), &item_id)?;
        Ok(rows
            .into_iter()
            .find(|row| {
                row.scope == Scope::Project && row.project_id.as_deref() == Some(self.project_id)
            })
            .and_then(|row| {
                let facts = DeploymentFacts::managed(
                    row.store_hash.as_deref(),
                    row.target_hash.as_deref(),
                    row.baseline_hash.as_deref(),
                )
                .with_state_version(self.version);
                SyncRecord::from_derived(classify(facts))
            }))
    }
}

/// Direction of a reviewed project/library synchronization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncDirection {
    /// Copy the Project item into the User library.
    Push,
    /// Copy the User library item into the Project store.
    Pull,
}

/// Inputs needed to preview one file-level sync replacement.
///
/// Paths are relative to the shared root supplied to [`apply_sync_plan`]. The caller provides
/// hashes classified by TA-005 and exact file bytes from the selected source and destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCopyRequest {
    /// Stable identity of the item being synchronized.
    pub item_id: ItemId,
    /// Registry id of the project whose copy participates in this sync.
    pub project_id: String,
    /// Direction requested by the caller.
    pub direction: SyncDirection,
    /// Unmodified classification from TA-005.
    pub label: SyncLabel,
    /// Last common base hash used by TA-005.
    pub base_hash: Option<String>,
    /// Current destination hash, or `None` when the destination is absent.
    pub destination_hash: Option<String>,
    /// Source content hash.
    pub source_hash: String,
    /// Hash of the destination file at `destination_path`, or `None` when absent.
    pub destination_file_hash: Option<String>,
    /// Caller explicitly reviewed the destination diff for a target-changed case.
    pub force_reviewed: bool,
    /// Hash of source file bytes (distinct from the item's aggregate hash for skill trees).
    pub source_file_hash: String,
    /// Destination path relative to the root passed to apply.
    pub destination_path: PathBuf,
    /// Whether the destination store root exists; pull must never recreate a missing project.
    pub destination_root_exists: bool,
    /// Exact source bytes shown in the reviewed preview.
    pub source_bytes: Vec<u8>,
}

/// Result of planning a sync action.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncPlan {
    /// The exact source and destination contents already agree.
    AlreadyInSync,
    /// Reviewed file replacement, to apply without rebuilding.
    Change(ChangeSet),
}

/// Errors encountered while planning a project/library synchronization.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncPlanError {
    /// The TA-005 label does not permit the requested direction.
    #[error("sync label {label:?} does not permit {direction:?}")]
    DirectionNotAllowed {
        /// Supplied TA-005 label.
        label: SyncLabel,
        /// Requested direction.
        direction: SyncDirection,
    },
    /// Both copies changed from the recorded base; compare them instead of overwriting.
    #[error("project and library copies conflict; compare before synchronizing")]
    Conflict,
    /// The destination changed after the common base was recorded.
    #[error("sync destination changed from the recorded base")]
    TargetChanged,
    /// Pull cannot create a project store whose root is missing.
    #[error("project store is missing; sync does not recreate it")]
    MissingProjectStore,
    /// Source content does not match its claimed hash.
    #[error("sync source bytes do not match the supplied content hash")]
    SourceHashMismatch,
    /// Invalid content hash or change-set structure.
    #[error("invalid sync change set: {0}")]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// The filesystem apply pipeline rejected the reviewed plan.
    #[error(transparent)]
    Apply(#[from] ChangeSetApplyError),
}

/// Builds a reviewed ChangeSet for a single-file project/library copy.
///
/// The plan contains a hash precondition for the current destination, so changes made after
/// preview are rejected by at-fs. Conflicts and drift are always refused. The `base_hash`
/// comparison prevents replacing an externally edited destination.
///
/// # Errors
/// Returns a typed refusal when the label/direction is incompatible, the copies conflict, or
/// the destination no longer matches the recorded base; invalid paths/hashes are also refused.
pub fn plan_sync_copy(request: SyncCopyRequest) -> Result<SyncPlan, SyncPlanError> {
    if matches!(request.label, SyncLabel::Conflict | SyncLabel::Drifted) {
        return Err(SyncPlanError::Conflict);
    }
    let permitted = matches!(
        (request.direction, request.label),
        (
            SyncDirection::Push,
            SyncLabel::ProjectOnly | SyncLabel::ProjectNewer | SyncLabel::InSync,
        ) | (
            SyncDirection::Pull,
            SyncLabel::LibraryOnly | SyncLabel::LibraryNewer | SyncLabel::InSync,
        )
    );
    if !permitted {
        return Err(SyncPlanError::DirectionNotAllowed {
            label: request.label,
            direction: request.direction,
        });
    }
    if request.direction == SyncDirection::Pull && !request.destination_root_exists {
        return Err(SyncPlanError::MissingProjectStore);
    }
    if at_model::ContentHash::parse(&request.source_hash).is_err()
        || at_fs::sha256_hex(&request.source_bytes) != request.source_file_hash
    {
        return Err(SyncPlanError::SourceHashMismatch);
    }
    if let Some(hash) = &request.destination_hash {
        at_model::ContentHash::parse(hash)?;
    }
    if let Some(hash) = &request.destination_file_hash {
        at_model::ContentHash::parse(hash)?;
    }
    if request.source_hash == request.destination_hash.as_deref().unwrap_or_default() {
        return Ok(SyncPlan::AlreadyInSync);
    }
    if request.destination_hash != request.base_hash && !request.force_reviewed {
        return Err(SyncPlanError::TargetChanged);
    }
    let expected = match request.destination_file_hash {
        Some(hash) => ModelExpectedState::hash(hash)?,
        None => ModelExpectedState::absent(),
    };
    let action = match request.direction {
        SyncDirection::Push => "sync_push",
        SyncDirection::Pull => "sync_pull",
    };
    let now = time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64;
    let change_set = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: uuid::Uuid::now_v7().to_string(),
        operation_kind: OperationKind::parse(action)?,
        scope: Some(match request.direction {
            SyncDirection::Push => Scope::User,
            SyncDirection::Pull => Scope::Project,
        }),
        project_id: Some(request.project_id),
        summary: format!("{action} item {}", request.item_id),
        ops: vec![FileOp::Write {
            path: request.destination_path,
            bytes: request.source_bytes,
            expected,
            follow_symlink: false,
            backup: true,
        }],
        created_at_unix_seconds: now,
    };
    change_set.validate()?;
    Ok(SyncPlan::Change(change_set))
}

/// Applies the exact ChangeSet returned from [`plan_sync_copy`].
///
/// # Errors
/// Returns an at-fs error if the reviewed destination changed or the journaled write fails.
pub fn apply_sync_plan(
    change_set: &ChangeSet,
    context: &mut ApplyContext<'_>,
) -> Result<ApplyOutcome, SyncPlanError> {
    Ok(at_fs::changeset::apply(change_set, context)?)
}

/// Failure reading a project sync record or using its cache.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectSyncError {
    /// The deployment query failed.
    #[error(transparent)]
    Database(#[from] DbError),
    /// Another thread poisoned the in-memory cache lock.
    #[error("project sync cache lock is poisoned")]
    CachePoisoned,
}

/// Maps a TA-005 sync label to the allowed user action.
#[must_use]
pub const fn action_for(label: SyncLabel) -> SyncAction {
    match label {
        SyncLabel::InSync => SyncAction::None,
        SyncLabel::ProjectOnly | SyncLabel::ProjectNewer => SyncAction::Push,
        SyncLabel::LibraryOnly | SyncLabel::LibraryNewer => SyncAction::Pull,
        SyncLabel::Conflict | SyncLabel::Drifted => SyncAction::Compare,
        _ => SyncAction::Compare,
    }
}

/// Cache of project sync records, invalidated by provider version or explicit item change.
#[derive(Debug, Default)]
pub struct ProjectSyncCache {
    state: Mutex<CacheState>,
}

#[derive(Debug, Default)]
struct CacheState {
    version: Option<u64>,
    records: BTreeMap<ItemId, Option<SyncRecord>>,
}

impl ProjectSyncCache {
    /// Gets the provider's unchanged label and evidence, refreshing on state-version changes.
    ///
    /// # Errors
    /// Returns a database error or a poisoned-cache error.
    pub fn get(
        &self,
        provider: &impl ProjectSyncProvider,
        item_id: ItemId,
    ) -> Result<Option<SyncRecord>, ProjectSyncError> {
        let version = provider.state_version();
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProjectSyncError::CachePoisoned)?;
        if state.version != Some(version) {
            state.records.clear();
            state.version = Some(version);
        }
        if let Some(record) = state.records.get(&item_id) {
            return Ok(record.clone());
        }
        let record = provider.sync_record(item_id)?;
        state.records.insert(item_id, record.clone());
        Ok(record)
    }

    /// Invalidates one item after a successful push, pull, store change or scope switch.
    pub fn invalidate(&self, item_id: ItemId) {
        if let Ok(mut state) = self.state.lock() {
            state.records.remove(&item_id);
        }
    }

    /// Invalidates every cached item after loading another project or changing stores.
    pub fn invalidate_all(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.records.clear();
            state.version = None;
        }
    }
}
