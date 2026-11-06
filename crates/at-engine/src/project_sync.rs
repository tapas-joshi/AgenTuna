//! Project-store consumption of TA-005 sync labels.
//!
//! This module never derives labels. It caches exactly what its provider reports and
//! maps each reported label to the action permitted by the product contract.

use std::collections::BTreeMap;
use std::sync::Mutex;

use at_db::deployments::list_deployments_for_item;
use at_db::{Database, DbError};
use at_model::{ItemId, Scope, SyncLabel};

use crate::state::{DeploymentFacts, DerivedState, classify};

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
