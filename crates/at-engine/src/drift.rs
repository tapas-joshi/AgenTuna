//! Watcher-driven drift detection against deployment baselines.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use at_db::drift::{NewDriftEvent, record_drift};
use at_db::{Database, DbError};
use at_fs::{RecentWrites, WatchConfig, WatchEvent, WatchRoot};
use at_model::{AgentId, ItemId, Scope};
use at_store::{hash_bytes, hash_rule_bytes, hash_skill_folder};
use serde::{Deserialize, Serialize};

/// Required debounce delay for drift batches.
pub const DRIFT_DEBOUNCE: Duration = Duration::from_millis(500);

/// Meaning assigned to a changed deployment path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DriftKind {
    /// Deployed content differs from its baseline.
    Drifted,
    /// A managed link was replaced by an ordinary directory or file.
    LinkReplacedByFile,
    /// The managed link target no longer exists.
    LinkRemoved,
    /// A deployed file was deleted.
    FileDeleted,
    /// The store-side content changed while a link points at it.
    StoreFileEditedWhileLinked,
    /// The target exists but cannot be read.
    Unreadable,
}

/// Information about one path whose state differs from the recorded deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftNotice {
    /// Persisted drift-event row id.
    pub event_id: i64,
    /// Classification of the detected change.
    pub kind: DriftKind,
    /// Affected canonical item, if known.
    pub item_id: Option<ItemId>,
    /// Affected agent.
    pub agent_id: AgentId,
    /// Scope of the deployment.
    pub scope: Scope,
    /// Path that changed.
    pub path: PathBuf,
    /// Hash expected by the deployment baseline.
    pub expected_hash: Option<String>,
    /// Hash currently observed, absent for unreadable or deleted content.
    pub actual_hash: Option<String>,
    /// Link target recorded for a replaced link, when available.
    pub link_target: Option<PathBuf>,
    /// Number of changed files represented by the coalesced notice.
    pub changed_file_count: usize,
}

/// One path tracked by the engine as part of a deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftTarget {
    /// Canonical item id, or `None` for a file not mapped to one item.
    pub item_id: Option<ItemId>,
    /// Agent whose deployment is tracked.
    pub agent_id: AgentId,
    /// Scope of the deployment.
    pub scope: Scope,
    /// Deployed or store path watched for changes.
    pub path: PathBuf,
    /// Hash expected by item_versions or deployments.
    pub expected_hash: Option<String>,
    /// Hashing method for this path.
    pub content: DriftContent,
    /// Whether the target is expected to be a symbolic link.
    pub link_target: Option<PathBuf>,
    /// Whether this path is the canonical store content behind a link.
    pub store_side_of_link: bool,
}

/// Content representation used for hashing one tracked path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DriftContent {
    /// Hash a file's bytes.
    File,
    /// Hash a skill folder using the store's canonical tree hash.
    SkillFolder,
    /// Hash rule-file bytes using the store's canonical rule hash.
    RuleFile,
}

/// Engine-side consumer for the shared debounced watcher and drift repository.
pub struct DriftService<'a> {
    /// Database where unresolved events are recorded.
    pub database: &'a Database,
    /// Targets associated with watched filesystem paths.
    pub targets: &'a [DriftTarget],
}

impl DriftService<'_> {
    /// Processes a watcher event and returns the persisted drift notices.
    ///
    /// `RescanRequired` triggers a full target reconciliation, making startup and overflow
    /// recovery use the same baseline comparison as ordinary batches.
    ///
    /// # Errors
    /// Returns a database error when an event cannot be recorded.
    pub fn process(&self, event: &WatchEvent) -> Result<Vec<DriftNotice>, DbError> {
        let selected: Vec<(&DriftTarget, usize)> = match event {
            WatchEvent::Batch(batch) => self
                .targets
                .iter()
                .filter_map(|target| {
                    let count = batch
                        .paths
                        .iter()
                        .filter(|path| *path == &target.path || path.starts_with(&target.path))
                        .count();
                    (count > 0).then_some((target, count))
                })
                .collect(),
            WatchEvent::RescanRequired { .. } => {
                self.targets.iter().map(|target| (target, 1)).collect()
            }
            WatchEvent::PathGone { .. } => Vec::new(),
            _ => Vec::new(),
        };
        self.process_targets(selected)
    }

    /// Reconciles all known targets after startup, watcher overflow, or restart.
    ///
    /// # Errors
    /// Returns a database error when an event cannot be recorded.
    pub fn reconcile(&self) -> Result<Vec<DriftNotice>, DbError> {
        self.process_targets(self.targets.iter().map(|target| (target, 1)).collect())
    }

    fn process_targets(
        &self,
        targets: Vec<(&DriftTarget, usize)>,
    ) -> Result<Vec<DriftNotice>, DbError> {
        let connection = self.database.connection();
        let mut notices = Vec::new();
        for (target, changed_file_count) in targets {
            if let Some(notice) = inspect_target(connection, target, changed_file_count)? {
                notices.push(notice);
            }
        }
        Ok(notices)
    }
}

/// Builds the watcher targets from the deployment index.
///
/// Every deployment row with a target path becomes one target: its kind selects the hashing
/// method, and its recorded baseline (falling back to the observed target hash) is the value the
/// current file must still match. Rows without a target path, and rows whose state is `off`, are
/// not deployed and therefore not watched.
///
/// # Errors
/// Returns a database error when the deployment matrix cannot be read.
pub fn targets_from_index(connection: &rusqlite::Connection) -> Result<Vec<DriftTarget>, DbError> {
    let mut statement = connection.prepare(
        "SELECT d.item_id, d.agent_id, d.scope, d.target_path, d.link_target, \
                COALESCE(d.baseline_hash, d.target_hash, \
                  (SELECT v.content_hash FROM item_versions v \
                    WHERE v.item_id = d.item_id AND v.scope = d.scope \
                    ORDER BY v.captured_at DESC, v.id DESC LIMIT 1)) AS expected, \
                i.kind \
           FROM deployments d JOIN items i ON i.id = d.item_id \
          WHERE d.target_path IS NOT NULL AND d.state != 'off' \
          ORDER BY d.target_path, d.agent_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut targets = Vec::new();
    for row in rows {
        let (item_id, agent_id, scope, target_path, link_target, expected_hash, kind) = row?;
        let item_id = item_id
            .parse::<ItemId>()
            .map_err(|error| DbError::InvalidValue {
                field: "deployments.item_id",
                reason: error.to_string(),
            })?;
        let agent_id = agent_id
            .parse::<AgentId>()
            .map_err(|error| DbError::InvalidValue {
                field: "deployments.agent_id",
                reason: error.to_string(),
            })?;
        let scope = scope
            .parse::<Scope>()
            .map_err(|error| DbError::InvalidValue {
                field: "deployments.scope",
                reason: error.to_string(),
            })?;
        let content = match kind.as_str() {
            "skill" => DriftContent::SkillFolder,
            "rule" => DriftContent::RuleFile,
            _ => DriftContent::File,
        };
        targets.push(DriftTarget {
            item_id: Some(item_id),
            agent_id,
            scope,
            path: PathBuf::from(target_path),
            expected_hash,
            content,
            link_target: link_target.map(PathBuf::from),
            store_side_of_link: false,
        });
    }
    Ok(targets)
}

/// Creates a watcher config with the shared RecentWrites registry and drift debounce.
#[must_use]
pub fn drift_watch_config(
    roots: impl IntoIterator<Item = WatchRoot>,
    registry: std::sync::Arc<RecentWrites>,
) -> WatchConfig {
    WatchConfig::new(roots)
        .with_debounce(DRIFT_DEBOUNCE)
        .with_registry(registry)
}

fn inspect_target(
    connection: &rusqlite::Connection,
    target: &DriftTarget,
    changed_file_count: usize,
) -> Result<Option<DriftNotice>, DbError> {
    let metadata = match fs::symlink_metadata(&target.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let kind = if target.link_target.is_some() {
                DriftKind::LinkRemoved
            } else {
                DriftKind::FileDeleted
            };
            return record_notice(
                connection,
                target,
                kind,
                None,
                target.link_target.clone(),
                changed_file_count,
            )
            .map(Some);
        }
        Err(_) => {
            return record_notice(
                connection,
                target,
                DriftKind::Unreadable,
                None,
                target.link_target.clone(),
                changed_file_count,
            )
            .map(Some);
        }
    };
    if target.link_target.is_some() && !metadata.file_type().is_symlink() {
        return record_notice(
            connection,
            target,
            DriftKind::LinkReplacedByFile,
            hash_path(&target.path, target.content).ok(),
            target.link_target.clone(),
            changed_file_count,
        )
        .map(Some);
    }
    let content_path = if metadata.file_type().is_symlink() {
        match fs::read_link(&target.path) {
            Ok(path) => {
                if path.is_absolute() {
                    path
                } else {
                    target.path.parent().unwrap_or(Path::new(".")).join(path)
                }
            }
            Err(_) => {
                return record_notice(
                    connection,
                    target,
                    DriftKind::Unreadable,
                    None,
                    target.link_target.clone(),
                    changed_file_count,
                )
                .map(Some);
            }
        }
    } else {
        target.path.clone()
    };
    let actual_hash = match hash_path(&content_path, target.content) {
        Ok(hash) => hash,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let kind = if target.link_target.is_some() {
                DriftKind::LinkRemoved
            } else {
                DriftKind::FileDeleted
            };
            return record_notice(
                connection,
                target,
                kind,
                None,
                target.link_target.clone(),
                changed_file_count,
            )
            .map(Some);
        }
        Err(_) => {
            return record_notice(
                connection,
                target,
                DriftKind::Unreadable,
                None,
                target.link_target.clone(),
                changed_file_count,
            )
            .map(Some);
        }
    };
    if target.expected_hash.as_deref() == Some(actual_hash.as_str()) {
        return Ok(None);
    }
    let kind = if target.store_side_of_link {
        DriftKind::StoreFileEditedWhileLinked
    } else {
        DriftKind::Drifted
    };
    record_notice(
        connection,
        target,
        kind,
        Some(actual_hash),
        target.link_target.clone(),
        changed_file_count,
    )
    .map(Some)
}

fn hash_path(path: &Path, content: DriftContent) -> std::io::Result<String> {
    match content {
        DriftContent::File => fs::read(path).map(|bytes| hash_bytes(&bytes).to_string()),
        DriftContent::SkillFolder => hash_skill_folder(path)
            .map(|hash| hash.to_string())
            .map_err(|error| std::io::Error::other(error.to_string())),
        DriftContent::RuleFile => fs::read(path).map(|bytes| hash_rule_bytes(&bytes).to_string()),
    }
}

fn node_hash(path: &Path) -> std::io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        return fs::read(path).map(|bytes| at_fs::sha256_hex(&bytes));
    }
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend_from_slice(entry.file_name().as_encoded_bytes());
            bytes.push(0);
            bytes.extend_from_slice(node_hash(&entry.path())?.as_bytes());
            bytes.push(0xff);
        }
        return Ok(at_fs::sha256_hex(&bytes));
    }
    if metadata.file_type().is_symlink() {
        return fs::read_link(path)
            .map(|target| at_fs::sha256_hex(target.as_os_str().as_encoded_bytes()));
    }
    Ok(String::new())
}

fn record_notice(
    connection: &rusqlite::Connection,
    target: &DriftTarget,
    kind: DriftKind,
    actual_hash: Option<String>,
    link_target: Option<PathBuf>,
    changed_file_count: usize,
) -> Result<DriftNotice, DbError> {
    let item_id = target.item_id.as_ref().map(ToString::to_string);
    let path = target.path.to_string_lossy().into_owned();
    let expected_hash = target.expected_hash.as_deref();
    let actual_hash_ref = actual_hash.as_deref();
    let row_id = record_drift(
        connection,
        &NewDriftEvent {
            item_id: item_id.as_deref(),
            agent_id: &target.agent_id,
            scope: target.scope,
            path: &path,
            expected_hash,
            actual_hash: actual_hash_ref,
            detected_at: None,
        },
    )?;
    Ok(DriftNotice {
        event_id: row_id,
        kind,
        item_id: target.item_id,
        agent_id: target.agent_id.clone(),
        scope: target.scope,
        path: target.path.clone(),
        expected_hash: target.expected_hash.clone(),
        actual_hash,
        link_target,
        changed_file_count,
    })
}

/// Error arising from drift resolution (TC-013).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DriftResolutionError {
    /// Event ID not found in database.
    #[error("drift event {0} was not found")]
    EventNotFound(i64),
    /// Event has already been resolved.
    #[error("drift event {0} is already resolved")]
    AlreadyResolved(i64),
    /// The item was deleted from the store.
    #[error("item was deleted from the store; cannot adopt or restore")]
    OrphanedDrift,
    /// The on-disk file changed again during resolution.
    #[error("file changed again on disk during resolution: expected {expected}, found {actual}")]
    DriftChangedAgain {
        /// Expected hash.
        expected: String,
        /// Observed actual hash.
        actual: String,
    },
    /// IO error during resolution.
    #[error("failed to inspect path {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    /// Database error.
    #[error(transparent)]
    Database(#[from] DbError),
    /// ChangeSet error.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    #[error(transparent)]
    /// ChangeSet apply error.
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// Store error.
    #[error("store error: {0}")]
    Store(String),
}

/// Parameters for adopting drifted content into the store (TC-013).
pub struct DriftAdoptRequest<'a> {
    /// Drift event row id to resolve.
    pub event_id: i64,
    /// Canonical store root.
    pub store_root: &'a Path,
    /// Directory for operation backups.
    pub backup_root: &'a Path,
    /// Authorized roots for filesystem operations.
    pub authorized_roots: &'a at_fs::ApplyRoots,
    /// Database connection.
    pub connection: &'a rusqlite::Connection,
}

/// Parameters for restoring store content over drifted disk files (TC-013).
pub struct DriftRestoreRequest<'a> {
    /// Drift event row id to resolve.
    pub event_id: i64,
    /// Canonical store root.
    pub store_root: &'a Path,
    /// Directory for operation backups.
    pub backup_root: &'a Path,
    /// Authorized roots for filesystem operations.
    pub authorized_roots: &'a at_fs::ApplyRoots,
    /// Database connection.
    pub connection: &'a rusqlite::Connection,
}

/// Result of adopting or restoring a drift event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftResolutionOutcome {
    /// Resolved drift event id.
    pub event_id: i64,
    /// Operation id recorded in the journal.
    pub operation_id: String,
    /// Paths touched during the operation.
    pub touched_paths: Vec<PathBuf>,
}

/// Adopts an on-disk external edit into the canonical store (TC-013).
pub fn adopt_drift(
    request: DriftAdoptRequest<'_>,
) -> Result<DriftResolutionOutcome, DriftResolutionError> {
    let event = at_db::drift::drift_event(request.connection, request.event_id)?
        .ok_or(DriftResolutionError::EventNotFound(request.event_id))?;
    if !event.is_open() {
        return Err(DriftResolutionError::AlreadyResolved(request.event_id));
    }
    let item_id_str = event
        .item_id
        .as_deref()
        .ok_or(DriftResolutionError::OrphanedDrift)?;
    let item_id: ItemId = item_id_str
        .parse()
        .map_err(|_| DriftResolutionError::OrphanedDrift)?;

    let item = at_db::shared::items_by_ids(request.connection, &[item_id])?
        .into_iter()
        .next()
        .ok_or(DriftResolutionError::OrphanedDrift)?;

    let path = PathBuf::from(&event.path);
    let content_kind = match item.kind {
        at_model::ItemKind::Skill => DriftContent::SkillFolder,
        at_model::ItemKind::Rule => DriftContent::RuleFile,
        _ => DriftContent::File,
    };
    let current_disk_hash =
        hash_path(&path, content_kind).map_err(|e| DriftResolutionError::Io(path.clone(), e))?;
    if let Some(expected_actual) = &event.actual_hash
        && &current_disk_hash != expected_actual
    {
        return Err(DriftResolutionError::DriftChangedAgain {
            expected: expected_actual.clone(),
            actual: current_disk_hash,
        });
    }

    let store_layout = at_store::layout::StoreLayout::new(event.scope, request.store_root);
    let store_dest = match item.kind {
        at_model::ItemKind::Skill => {
            let slug = at_store::layout::Slug::parse(&item.slug)
                .map_err(|e| DriftResolutionError::Store(e.to_string()))?;
            store_layout.skill_dir(&slug)
        }
        at_model::ItemKind::Rule => {
            if let Some(ref src) = item.source_path {
                request.store_root.join(src)
            } else {
                request.store_root.join("rules").join(&item.slug)
            }
        }
        at_model::ItemKind::McpServer => store_layout.mcp_file(),
    };
    if !store_dest.exists() && item.kind != at_model::ItemKind::Skill {
        return Err(DriftResolutionError::OrphanedDrift);
    }

    let operation_id = at_db::ops::OperationId::new().to_string();
    let preview_id = format!("pv_{}", uuid::Uuid::now_v7());
    let mut ops = Vec::new();
    let touched_paths = vec![store_dest.clone()];

    if item.kind == at_model::ItemKind::Skill {
        if store_dest.exists() {
            let prev_hash = node_hash(&store_dest)
                .map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
            ops.push(at_model::FileOp::Delete {
                path: store_dest.clone(),
                expected: at_model::ExpectedState::hash(prev_hash)?,
            });
        }
        ops.push(at_model::FileOp::CopyTree {
            source: path.clone(),
            dest: store_dest.clone(),
            expected: at_model::ExpectedState::absent(),
        });
    } else {
        let bytes = fs::read(&path).map_err(|e| DriftResolutionError::Io(path.clone(), e))?;
        let expected = if store_dest.exists() {
            let prev_hash = hash_path(&store_dest, DriftContent::File)
                .map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
            at_model::ExpectedState::hash(prev_hash)?
        } else {
            at_model::ExpectedState::absent()
        };
        ops.push(at_model::FileOp::Write {
            path: store_dest.clone(),
            bytes,
            expected,
            follow_symlink: false,
            backup: true,
        });
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let change_set = at_model::ChangeSet {
        id: operation_id.clone(),
        preview_id,
        operation_kind: at_model::OperationKind::parse("drift_adopt")?,
        scope: Some(event.scope),
        project_id: item.project_id.clone(),
        summary: format!("Adopt drifted {} for {}", item.name, event.agent_id),
        ops,
        created_at_unix_seconds: now_secs,
    };

    let mut recorder = at_fs::DbOperationRecorder::new(request.connection, 30);
    let mut context = at_fs::ApplyContext {
        store_root: request.store_root,
        backup_folder: request.backup_root,
        recorder: &mut recorder,
    };
    let applied = at_fs::apply_with_roots(&change_set, &mut context, request.authorized_roots)?;
    let operation_id = applied.operation_id;

    let new_store_hash = hash_path(&store_dest, content_kind)
        .map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
    at_db::versions::record_skill_version(
        request.connection,
        &at_db::versions::NewVersion::new(&item_id, event.scope, &new_store_hash),
    )?;
    request
        .connection
        .execute(
            "UPDATE deployments SET target_hash = ?1, baseline_hash = ?1 \
         WHERE item_id = ?2 AND agent_id = ?3 AND scope = ?4",
            rusqlite::params![
                new_store_hash,
                item_id.to_string(),
                event.agent_id.to_string(),
                event.scope.wire_name(),
            ],
        )
        .map_err(at_db::DbError::Sqlite)?;
    at_db::drift::resolve_drift(
        request.connection,
        event.id,
        at_db::drift::DriftResolution::Adopt,
        Some(&operation_id),
    )?;

    Ok(DriftResolutionOutcome {
        event_id: event.id,
        operation_id,
        touched_paths,
    })
}

/// Restores the store's canonical version over a drifted on-disk path (TC-013).
pub fn restore_drift(
    request: DriftRestoreRequest<'_>,
) -> Result<DriftResolutionOutcome, DriftResolutionError> {
    let event = at_db::drift::drift_event(request.connection, request.event_id)?
        .ok_or(DriftResolutionError::EventNotFound(request.event_id))?;
    if !event.is_open() {
        return Err(DriftResolutionError::AlreadyResolved(request.event_id));
    }
    let item_id_str = event
        .item_id
        .as_deref()
        .ok_or(DriftResolutionError::OrphanedDrift)?;
    let item_id: ItemId = item_id_str
        .parse()
        .map_err(|_| DriftResolutionError::OrphanedDrift)?;

    let item = at_db::shared::items_by_ids(request.connection, &[item_id])?
        .into_iter()
        .next()
        .ok_or(DriftResolutionError::OrphanedDrift)?;

    let path = PathBuf::from(&event.path);
    let content_kind = match item.kind {
        at_model::ItemKind::Skill => DriftContent::SkillFolder,
        at_model::ItemKind::Rule => DriftContent::RuleFile,
        _ => DriftContent::File,
    };
    let current_disk_hash =
        hash_path(&path, content_kind).map_err(|e| DriftResolutionError::Io(path.clone(), e))?;
    if let Some(expected_actual) = &event.actual_hash
        && &current_disk_hash != expected_actual
    {
        return Err(DriftResolutionError::DriftChangedAgain {
            expected: expected_actual.clone(),
            actual: current_disk_hash,
        });
    }

    let store_layout = at_store::layout::StoreLayout::new(event.scope, request.store_root);
    let store_dest = match item.kind {
        at_model::ItemKind::Skill => {
            let slug = at_store::layout::Slug::parse(&item.slug)
                .map_err(|e| DriftResolutionError::Store(e.to_string()))?;
            store_layout.skill_dir(&slug)
        }
        at_model::ItemKind::Rule => {
            if let Some(ref src) = item.source_path {
                request.store_root.join(src)
            } else {
                request.store_root.join("rules").join(&item.slug)
            }
        }
        at_model::ItemKind::McpServer => store_layout.mcp_file(),
    };
    if !store_dest.exists() {
        return Err(DriftResolutionError::OrphanedDrift);
    }

    let operation_id = at_db::ops::OperationId::new().to_string();
    let preview_id = format!("pv_{}", uuid::Uuid::now_v7());
    let mut ops = Vec::new();
    let touched_paths = vec![path.clone()];

    if item.kind == at_model::ItemKind::Skill {
        if path.exists() {
            let prev_hash =
                node_hash(&path).map_err(|e| DriftResolutionError::Io(path.clone(), e))?;
            ops.push(at_model::FileOp::Delete {
                path: path.clone(),
                expected: at_model::ExpectedState::hash(prev_hash)?,
            });
        }
        ops.push(at_model::FileOp::CopyTree {
            source: store_dest.clone(),
            dest: path.clone(),
            expected: at_model::ExpectedState::absent(),
        });
    } else {
        let store_bytes =
            fs::read(&store_dest).map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
        let expected = if path.exists() {
            at_model::ExpectedState::hash(&current_disk_hash)?
        } else {
            at_model::ExpectedState::absent()
        };
        ops.push(at_model::FileOp::Write {
            path: path.clone(),
            bytes: store_bytes,
            expected,
            follow_symlink: false,
            backup: true,
        });
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let change_set = at_model::ChangeSet {
        id: operation_id.clone(),
        preview_id,
        operation_kind: at_model::OperationKind::parse("drift_restore")?,
        scope: Some(event.scope),
        project_id: item.project_id.clone(),
        summary: format!("Restore {} for {} from store", item.name, event.agent_id),
        ops,
        created_at_unix_seconds: now_secs,
    };

    let mut recorder = at_fs::DbOperationRecorder::new(request.connection, 30);
    let mut context = at_fs::ApplyContext {
        store_root: request.store_root,
        backup_folder: request.backup_root,
        recorder: &mut recorder,
    };
    let applied = at_fs::apply_with_roots(&change_set, &mut context, request.authorized_roots)?;
    let operation_id = applied.operation_id;

    let store_hash = hash_path(&store_dest, content_kind)
        .map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
    request
        .connection
        .execute(
            "UPDATE deployments SET target_hash = ?1, baseline_hash = ?1 \
         WHERE item_id = ?2 AND agent_id = ?3 AND scope = ?4",
            rusqlite::params![
                store_hash,
                item_id.to_string(),
                event.agent_id.to_string(),
                event.scope.wire_name(),
            ],
        )
        .map_err(at_db::DbError::Sqlite)?;
    at_db::drift::resolve_drift(
        request.connection,
        event.id,
        at_db::drift::DriftResolution::Restore,
        Some(&operation_id),
    )?;

    Ok(DriftResolutionOutcome {
        event_id: event.id,
        operation_id,
        touched_paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_db::drift::open_drift_events;
    use at_fs::{WatchBatch, WatchChange};
    use time::OffsetDateTime;

    #[test]
    fn watcher_edit_records_open_drift_with_expected_and_actual_hashes() {
        let temp = tempfile::tempdir().expect("sandbox");
        let path = temp.path().join("settings.json");
        fs::write(&path, b"{\"mcpServers\":{}}").expect("write fixture");
        let database = Database::open_in_memory().expect("database");
        let target = DriftTarget {
            item_id: None,
            agent_id: AgentId::ClaudeCode,
            scope: Scope::User,
            path: path.clone(),
            expected_hash: Some(hash_bytes(b"{\"mcpServers\":{}}").to_string()),
            content: DriftContent::File,
            link_target: None,
            store_side_of_link: false,
        };
        fs::write(&path, b"{\"mcpServers\":{\"server\":{}}}").expect("external edit");
        let service = DriftService {
            database: &database,
            targets: &[target],
        };
        let event = WatchEvent::Batch(WatchBatch {
            paths: vec![path.clone()],
            kinds: vec![WatchChange::Modified(path.clone())],
            observed_at: OffsetDateTime::now_utc(),
        });

        let notices = service.process(&event).expect("process watcher event");
        let open = open_drift_events(database.connection(), Some(Scope::User)).expect("open drift");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, DriftKind::Drifted);
        let expected_actual = hash_bytes(b"{\"mcpServers\":{\"server\":{}}}").to_string();
        assert_eq!(
            notices[0].actual_hash.as_deref(),
            Some(expected_actual.as_str())
        );
        assert_eq!(open.len(), 1);
        assert!(open[0].resolved_at.is_none());
    }

    #[test]
    fn reconciliation_skips_matching_hashes_and_detects_deleted_files() {
        let temp = tempfile::tempdir().expect("sandbox");
        let unchanged = temp.path().join("unchanged.json");
        let deleted = temp.path().join("deleted.json");
        fs::write(&unchanged, b"same").expect("write unchanged");
        fs::write(&deleted, b"gone").expect("write deleted");
        let database = Database::open_in_memory().expect("database");
        let targets = [
            DriftTarget {
                item_id: None,
                agent_id: AgentId::Codex,
                scope: Scope::User,
                path: unchanged.clone(),
                expected_hash: Some(hash_bytes(b"same").to_string()),
                content: DriftContent::File,
                link_target: None,
                store_side_of_link: false,
            },
            DriftTarget {
                item_id: None,
                agent_id: AgentId::Codex,
                scope: Scope::User,
                path: deleted.clone(),
                expected_hash: Some(hash_bytes(b"gone").to_string()),
                content: DriftContent::File,
                link_target: None,
                store_side_of_link: false,
            },
        ];
        fs::remove_file(deleted).expect("delete target");
        let service = DriftService {
            database: &database,
            targets: &targets,
        };

        let notices = service.reconcile().expect("reconcile");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, DriftKind::FileDeleted);
        assert_eq!(
            open_drift_events(database.connection(), Some(Scope::User))
                .expect("open drift")
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacing_managed_skill_link_with_regular_folder_is_classified() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("sandbox");
        let destination = temp.path().join("deployed-skill");
        let store = temp.path().join("store-skill");
        fs::create_dir_all(&store).expect("store skill");
        fs::write(store.join("SKILL.md"), "canonical").expect("store content");
        symlink(&store, &destination).expect("managed link");
        fs::remove_file(&destination).expect("remove managed link");
        fs::create_dir(&destination).expect("replacement directory");
        fs::write(destination.join("SKILL.md"), "edited").expect("replacement content");
        let database = Database::open_in_memory().expect("database");
        let target = DriftTarget {
            item_id: None,
            agent_id: AgentId::ClaudeCode,
            scope: Scope::User,
            path: destination.clone(),
            expected_hash: Some(
                hash_skill_folder(&store)
                    .expect("baseline hash")
                    .to_string(),
            ),
            content: DriftContent::SkillFolder,
            link_target: Some(store.clone()),
            store_side_of_link: false,
        };
        let service = DriftService {
            database: &database,
            targets: &[target],
        };

        let notices = service.reconcile().expect("reconcile");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, DriftKind::LinkReplacedByFile);
        assert_eq!(notices[0].link_target.as_deref(), Some(store.as_path()));
        assert!(notices[0].actual_hash.is_some());
    }

    #[test]
    fn adopt_drift_updates_store_and_closes_event() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let store_dir = root.join("store");
        let backup_dir = root.join("backups");
        let disk_skill = root.join("agent_skills").join("my-skill");
        fs::create_dir_all(&backup_dir).expect("backup dir");
        fs::create_dir_all(&disk_skill).expect("disk skill");
        fs::write(disk_skill.join("SKILL.md"), b"edited on disk").expect("write disk skill");

        let store_layout = at_store::layout::StoreLayout::new(Scope::User, &store_dir);
        let slug = at_store::layout::Slug::parse("my-skill").expect("slug");
        let store_skill = store_layout.skill_dir(&slug);
        fs::create_dir_all(&store_skill).expect("store skill");
        fs::write(store_skill.join("SKILL.md"), b"original in store").expect("write store skill");

        let item_id = ItemId::new();
        let database = Database::open_in_memory().expect("database");
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name, source_path) \
                 VALUES (?1, 'skill', 'user', 'my-skill', 'my-skill', ?2)",
                rusqlite::params![item_id.to_string(), store_skill.to_string_lossy().as_ref()],
            )
            .expect("insert item");
        database
            .connection()
            .execute(
                "INSERT INTO deployments (item_id, agent_id, scope, target_path, state, baseline_hash, target_hash) \
                 VALUES (?1, 'claude_code', 'user', ?2, 'drifted', 'prev_hash', 'prev_hash')",
                rusqlite::params![item_id.to_string(), disk_skill.to_string_lossy().as_ref()],
            )
            .expect("insert deployment");

        let disk_hash = hash_skill_folder(&disk_skill)
            .expect("hash disk")
            .to_string();
        let event_id = at_db::drift::record_drift(
            database.connection(),
            &NewDriftEvent {
                item_id: Some(&item_id.to_string()),
                agent_id: &AgentId::ClaudeCode,
                scope: Scope::User,
                path: &disk_skill.to_string_lossy(),
                expected_hash: Some("prev_hash"),
                actual_hash: Some(&disk_hash),
                detected_at: None,
            },
        )
        .expect("record drift");

        let roots = at_fs::ApplyRoots::new([root.clone()]).expect("authorized roots");
        let outcome = adopt_drift(DriftAdoptRequest {
            event_id,
            store_root: &store_dir,
            backup_root: &backup_dir,
            authorized_roots: &roots,
            connection: database.connection(),
        })
        .expect("adopt drift succeeds");

        assert_eq!(outcome.event_id, event_id);
        // Store now equals disk content
        assert_eq!(
            fs::read(store_skill.join("SKILL.md")).expect("read store"),
            b"edited on disk"
        );
        // Event is closed
        let event = at_db::drift::drift_event(database.connection(), event_id)
            .expect("read event")
            .expect("event exists");
        assert!(!event.is_open());
        assert_eq!(event.resolution, Some(at_db::drift::DriftResolution::Adopt));

        // TC-010 undo works on the operation
        let undo = at_migrate::undo::undo_operation(
            at_migrate::undo::UndoOperationRequest {
                connection: database.connection(),
                store_root: &store_dir,
                backup_root: &backup_dir,
                authorized_roots: &roots,
                actor: at_db::ops::OperationActor::Cli,
                force: false,
            },
            &outcome.operation_id,
        )
        .expect("undo succeeds");
        assert!(matches!(
            undo,
            at_migrate::undo::UndoOutcome::Restored { .. }
        ));
        assert_eq!(
            fs::read(store_skill.join("SKILL.md")).expect("read store after undo"),
            b"original in store"
        );
    }

    #[test]
    fn restore_drift_restores_store_content_and_refuses_when_changed_again() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let store_dir = root.join("store");
        let backup_dir = root.join("backups");
        let disk_skill = root.join("agent_skills").join("my-skill");
        fs::create_dir_all(&backup_dir).expect("backup dir");
        fs::create_dir_all(&disk_skill).expect("disk skill");
        fs::write(disk_skill.join("SKILL.md"), b"drifted on disk").expect("write disk skill");

        let store_layout = at_store::layout::StoreLayout::new(Scope::User, &store_dir);
        let slug = at_store::layout::Slug::parse("my-skill").expect("slug");
        let store_skill = store_layout.skill_dir(&slug);
        fs::create_dir_all(&store_skill).expect("store skill");
        fs::write(store_skill.join("SKILL.md"), b"golden in store").expect("write store skill");

        let item_id = ItemId::new();
        let database = Database::open_in_memory().expect("database");
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name, source_path) \
                 VALUES (?1, 'skill', 'user', 'my-skill', 'my-skill', ?2)",
                rusqlite::params![item_id.to_string(), store_skill.to_string_lossy().as_ref()],
            )
            .expect("insert item");
        database
            .connection()
            .execute(
                "INSERT INTO deployments (item_id, agent_id, scope, target_path, state, baseline_hash, target_hash) \
                 VALUES (?1, 'claude_code', 'user', ?2, 'drifted', 'golden_hash', 'golden_hash')",
                rusqlite::params![item_id.to_string(), disk_skill.to_string_lossy().as_ref()],
            )
            .expect("insert deployment");

        let disk_hash = hash_skill_folder(&disk_skill)
            .expect("hash disk")
            .to_string();
        let event_id = at_db::drift::record_drift(
            database.connection(),
            &NewDriftEvent {
                item_id: Some(&item_id.to_string()),
                agent_id: &AgentId::ClaudeCode,
                scope: Scope::User,
                path: &disk_skill.to_string_lossy(),
                expected_hash: Some("golden_hash"),
                actual_hash: Some(&disk_hash),
                detected_at: None,
            },
        )
        .expect("record drift");

        let roots = at_fs::ApplyRoots::new([root.clone()]).expect("authorized roots");

        // External edit changed again before restore
        fs::write(disk_skill.join("SKILL.md"), b"changed again").expect("second edit");
        let err = restore_drift(DriftRestoreRequest {
            event_id,
            store_root: &store_dir,
            backup_root: &backup_dir,
            authorized_roots: &roots,
            connection: database.connection(),
        })
        .expect_err("must abort with DriftChangedAgain");
        assert!(matches!(
            err,
            DriftResolutionError::DriftChangedAgain { .. }
        ));

        // Event is still open
        let event = at_db::drift::drift_event(database.connection(), event_id)
            .expect("read event")
            .expect("event exists");
        assert!(event.is_open());

        // Update actual_hash to current and restore succeeds
        let fresh_hash = hash_skill_folder(&disk_skill)
            .expect("hash disk")
            .to_string();
        database
            .connection()
            .execute(
                "UPDATE drift_events SET actual_hash = ?1 WHERE id = ?2",
                rusqlite::params![fresh_hash, event_id],
            )
            .expect("update drift event");

        let outcome = restore_drift(DriftRestoreRequest {
            event_id,
            store_root: &store_dir,
            backup_root: &backup_dir,
            authorized_roots: &roots,
            connection: database.connection(),
        })
        .expect("restore succeeds");

        // Disk now has store golden content
        assert_eq!(
            fs::read(disk_skill.join("SKILL.md")).expect("read disk"),
            b"golden in store"
        );

        // Undo restores drifted content
        let undo = at_migrate::undo::undo_operation(
            at_migrate::undo::UndoOperationRequest {
                connection: database.connection(),
                store_root: &store_dir,
                backup_root: &backup_dir,
                authorized_roots: &roots,
                actor: at_db::ops::OperationActor::Cli,
                force: false,
            },
            &outcome.operation_id,
        )
        .expect("undo succeeds");
        assert!(matches!(
            undo,
            at_migrate::undo::UndoOutcome::Restored { .. }
        ));
        assert_eq!(
            fs::read(disk_skill.join("SKILL.md")).expect("read disk after undo"),
            b"changed again"
        );
    }
}
