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
}
