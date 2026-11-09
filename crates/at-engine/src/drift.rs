//! Watcher-driven drift detection against deployment baselines.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::drift::{NewDriftEvent, record_drift};
use at_db::{Database, DbError};
use at_fs::{RecentWrites, WatchConfig, WatchEvent, WatchRoot};
use at_model::{AgentId, ExpectedState, ItemId, McpServerDef, Scope};
use at_store::mcp_store::McpStore;
use at_store::{hash_bytes, hash_rule_bytes, hash_skill_folder};
use serde::{Deserialize, Serialize};

use crate::mcp_plan::{McpIntent, McpPlanRequest, McpTarget};

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
                    // A linked deployment is also changeable through the store path it points at,
                    // so a store edit while linked must reach the same target.
                    let count = batch
                        .paths
                        .iter()
                        .filter(|path| {
                            *path == &target.path
                                || path.starts_with(&target.path)
                                || target
                                    .link_target
                                    .as_ref()
                                    .is_some_and(|link| *path == link || path.starts_with(link))
                        })
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
        // Several indexed items can share one native file (every MCP server in
        // `~/.claude.json`, every rule in one `AGENTS.md`). The first difference found for a
        // path is the one event a user can act on; a later item must not open a duplicate row
        // for the same file and the same baseline.
        let mut recorded = std::collections::HashSet::new();
        for (target, changed_file_count) in targets {
            let path = target.path.to_string_lossy().into_owned();
            if recorded.contains(&path) {
                continue;
            }
            if let Some(notice) = inspect_target(connection, target, changed_file_count)? {
                recorded.insert(path);
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
            store_side_of_link: link_target.is_some(),
            link_target: link_target.map(PathBuf::from),
        });
    }
    Ok(targets)
}

/// Creates a watcher config with the shared RecentWrites registry and drift debounce.
///
/// `allowed_link_roots` are the directories a linked skill target may resolve inside (the
/// injected home and project root, for example). An empty list follows no link target.
#[must_use]
pub fn drift_watch_config(
    roots: impl IntoIterator<Item = WatchRoot>,
    registry: std::sync::Arc<RecentWrites>,
    allowed_link_roots: Vec<std::path::PathBuf>,
) -> WatchConfig {
    WatchConfig::new(roots)
        .with_debounce(DRIFT_DEBOUNCE)
        .with_allowed_link_roots(allowed_link_roots)
        .with_registry(registry)
}

/// Drops every target whose path already has an open drift event.
///
/// Both transports reconcile the same deployment index — the app watcher on startup and after
/// overflow, the CLI on `drift list` — so a repeated pass would otherwise open a second event
/// for a drift that is still listed. The first event keeps the hashes the user reviews, which
/// is what makes a later adopt or restore detect a second external edit instead of silently
/// comparing against the newer bytes.
///
/// # Errors
/// Returns a database error when the open events cannot be read.
pub fn open_targets(
    connection: &rusqlite::Connection,
    targets: Vec<DriftTarget>,
) -> Result<Vec<DriftTarget>, DbError> {
    let open = at_db::drift::open_drift_events(connection, None)?
        .into_iter()
        .map(|event| event.path)
        .collect::<std::collections::HashSet<_>>();
    Ok(targets
        .into_iter()
        .filter(|target| !open.contains(&target.path.to_string_lossy().into_owned()))
        .collect())
}

/// Reconciles every indexed deployment and records the differences as open drift events.
///
/// This is the shared detection entry for transports that have no filesystem watcher: it reads
/// the recorded deployment baselines, hashes the current agent files, and inserts a
/// `drift_events` row per changed path. It writes no agent file and resolves nothing.
///
/// # Errors
/// Returns a database error when targets or events cannot be read or written.
pub fn reconcile_indexed(database: &Database) -> Result<Vec<DriftNotice>, DbError> {
    let targets = targets_from_index(database.connection())?;
    let targets = open_targets(database.connection(), targets)?;
    DriftService {
        database,
        targets: &targets,
    }
    .reconcile()
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
        // A resolved drift or a copy-mode fallback leaves real bytes where the deployment once
        // had a link. Reporting that as drift unconditionally would re-open the event the user
        // just resolved, with nothing left to act on; only a content difference is drift.
        let actual = hash_path(&target.path, target.content).ok();
        if actual.as_deref() != target.expected_hash.as_deref() {
            return record_notice(
                connection,
                target,
                DriftKind::LinkReplacedByFile,
                actual,
                target.link_target.clone(),
                changed_file_count,
            )
            .map(Some);
        }
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

/// Whether `path` is a managed link whose target is `store_dest`.
///
/// Drift hashes a link through its target, so an edit made "in the agent folder" of a linked
/// skill lands in the canonical store. Adopting or restoring that path must not copy the link
/// over its own target, and must not replace the link with a directory.
fn is_link_to(path: &Path, store_dest: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(target) = fs::read_link(path) else {
        return false;
    };
    let target = if target.is_absolute() {
        target
    } else {
        path.parent().unwrap_or(Path::new(".")).join(target)
    };
    match (fs::canonicalize(&target), fs::canonicalize(store_dest)) {
        (Ok(target), Ok(store)) => target == store,
        _ => false,
    }
}

/// Records the observed file hashes for every deployment that shares one native file.
///
/// MCP servers live in one document (`~/.claude.json`, `config.toml`, omp's `mcp.json`), so a
/// resolution updates the file, not just the resolved item's row. Leaving the other rows on
/// their migration baseline made the next scan report the same file as drifted again. This is
/// [`sync_written_target`] with a known canonical store file.
///
/// # Errors
/// Returns [`DriftResolutionError::Io`] when the written path cannot be hashed and
/// [`DriftResolutionError::Database`] when the deployment rows cannot be updated.
pub fn sync_file_baseline(
    connection: &rusqlite::Connection,
    scope: Scope,
    native_path: &Path,
    store_file: &Path,
) -> Result<(), DriftResolutionError> {
    sync_written_target(connection, Some(scope), native_path, Some(store_file)).map(|_| ())
}

/// Records fresh target and baseline hashes for every deployment row that targets
/// `native_path`.
///
/// Call this after a command applies a ChangeSet that wrote `native_path`. Drift compares each
/// deployment baseline with the bytes on disk, so a row left on the pre-write baseline reports
/// AgenTuna's own write as an external edit forever. Every row sharing the path is refreshed:
/// MCP servers share one native document (`~/.claude.json`, `config.toml`) and a shared skills
/// root is one directory for several agents.
///
/// `scope` restricts the rows to one scope; `None` refreshes every scope. `store_path` is the
/// canonical content behind the deployment, hashed with the same method drift uses for the
/// item kind. When it is absent the recorded store hash is left unchanged, and a missing store
/// path falls back to the native hash so an in-place rule file still gets a baseline.
///
/// Returns the number of refreshed rows. A target path with no deployment rows, or one that no
/// longer exists, refreshes nothing and is not an error.
///
/// # Errors
/// Returns [`DriftResolutionError::Io`] when the written path cannot be hashed and
/// [`DriftResolutionError::Database`] when the deployment rows cannot be read or updated.
pub fn sync_written_target(
    connection: &rusqlite::Connection,
    scope: Option<Scope>,
    native_path: &Path,
    store_path: Option<&Path>,
) -> Result<usize, DriftResolutionError> {
    let path_text = native_path.to_string_lossy().into_owned();
    let scope_text = scope.map(Scope::wire_name);
    let kinds = {
        let mut statement = connection
            .prepare(
                "SELECT DISTINCT i.kind FROM deployments d JOIN items i ON i.id = d.item_id \
                 WHERE d.target_path = ?1 AND (?2 IS NULL OR d.scope = ?2)",
            )
            .map_err(at_db::DbError::Sqlite)?;
        let rows = statement
            .query_map(rusqlite::params![path_text, scope_text], |row| {
                row.get::<_, String>(0)
            })
            .map_err(at_db::DbError::Sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(at_db::DbError::Sqlite)?
    };
    if kinds.is_empty() {
        return Ok(0);
    }
    match fs::symlink_metadata(native_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(drift_io(native_path)(error)),
    }
    let mut updated = 0;
    for kind in kinds {
        let content = content_for_kind(&kind);
        let native_hash = hash_written_path(native_path, content).map_err(drift_io(native_path))?;
        let store_hash = match store_path {
            Some(path) => match hash_written_path(path, content) {
                Ok(hash) => Some(hash),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Some(native_hash.clone())
                }
                Err(error) => return Err(drift_io(path)(error)),
            },
            None => None,
        };
        updated += at_db::deployments::update_hashes_for_target_path(
            connection,
            native_path,
            scope,
            &kind,
            store_hash.as_deref(),
            &native_hash,
        )?;
    }
    Ok(updated)
}

/// Records the current content of every deployment target of one item.
///
/// A store-side edit reaches a linked deployment through the link, so the target's bytes change
/// without any command writing the native path entry. Refresh the item's rows after that edit;
/// otherwise the next scan reports the edit as external drift. Copy deployments keep their
/// recorded target hash, which is what the sync labels compare with the new store hash.
///
/// # Errors
/// Returns [`DriftResolutionError::Io`] when a target cannot be hashed and
/// [`DriftResolutionError::Database`] when the deployment rows cannot be read or updated.
pub fn sync_item_baselines(
    connection: &rusqlite::Connection,
    item_id: &ItemId,
    scope: Scope,
    store_path: Option<&Path>,
) -> Result<usize, DriftResolutionError> {
    let kind: String = {
        use rusqlite::OptionalExtension;
        connection
            .query_row(
                "SELECT kind FROM items WHERE id = ?1",
                [item_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(at_db::DbError::Sqlite)?
            .unwrap_or_default()
    };
    if kind.is_empty() {
        return Ok(0);
    }
    let content = content_for_kind(&kind);
    let rows = {
        let mut statement = connection
            .prepare(
                "SELECT agent_id, COALESCE(project_id, ''), target_path FROM deployments \
                 WHERE item_id = ?1 AND scope = ?2 AND target_path IS NOT NULL",
            )
            .map_err(at_db::DbError::Sqlite)?;
        let rows = statement
            .query_map(
                rusqlite::params![item_id.to_string(), scope.wire_name()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(at_db::DbError::Sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(at_db::DbError::Sqlite)?
    };
    let store_hash = store_path.and_then(|path| hash_written_path(path, content).ok());
    let mut updated = 0;
    for (agent_id, project_id, target_path) in rows {
        let target = Path::new(&target_path);
        let target_hash = match hash_written_path(target, content) {
            Ok(hash) => hash,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(drift_io(target)(error)),
        };
        updated += at_db::deployments::update_item_baseline_hashes(
            connection,
            item_id,
            &agent_id,
            scope,
            &project_id,
            store_hash.as_deref(),
            &target_hash,
        )?;
    }
    Ok(updated)
}

/// Drift hashing method for one stored `items.kind` value.
fn content_for_kind(kind: &str) -> DriftContent {
    match kind {
        "skill" => DriftContent::SkillFolder,
        "rule" => DriftContent::RuleFile,
        _ => DriftContent::File,
    }
}

/// Hashes a written target the way [`inspect_target`] will hash it, following a managed link.
fn hash_written_path(path: &Path, content: DriftContent) -> std::io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    let content_path = if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or(Path::new(".")).join(target)
        }
    } else {
        path.to_path_buf()
    };
    hash_path(&content_path, content)
}

/// Maps an IO failure on a resolution path into the typed drift error.
fn drift_io(path: &Path) -> impl Fn(std::io::Error) -> DriftResolutionError + '_ {
    move |error| DriftResolutionError::Io(path.to_path_buf(), error)
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
    /// Transport that resolved the drift; recorded on the operation row.
    pub actor: at_db::ops::OperationActor,
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
    /// Injected home directory, used to read the agent's MCP capability declaration.
    pub home: &'a Path,
    /// Transport that resolved the drift; recorded on the operation row.
    pub actor: at_db::ops::OperationActor,
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
    adopt_drift_inner(request, None)
}

/// Adopts drift while registering expected file writes before their atomic replacements.
pub fn adopt_drift_with_registry(
    request: DriftAdoptRequest<'_>,
    registry: &at_fs::RecentWrites,
) -> Result<DriftResolutionOutcome, DriftResolutionError> {
    adopt_drift_inner(request, Some(registry))
}

fn adopt_drift_inner(
    request: DriftAdoptRequest<'_>,
    registry: Option<&at_fs::RecentWrites>,
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

    // A linked skill is edited through the link, so the store already holds the change.
    let linked = item.kind == at_model::ItemKind::Skill && is_link_to(&path, &store_dest);
    let hash_source = if linked { &store_dest } else { &path };
    let current_disk_hash = hash_path(hash_source, content_kind)
        .map_err(|e| DriftResolutionError::Io(hash_source.clone(), e))?;
    if let Some(expected_actual) = &event.actual_hash
        && &current_disk_hash != expected_actual
    {
        return Err(DriftResolutionError::DriftChangedAgain {
            expected: expected_actual.clone(),
            actual: current_disk_hash,
        });
    }

    let operation_id = at_db::ops::OperationId::new().to_string();
    let preview_id = format!("pv_{}", uuid::Uuid::now_v7());
    let mut ops = Vec::new();
    let mut touched_paths = vec![store_dest.clone()];

    if linked {
        touched_paths.clear();
    } else if item.kind == at_model::ItemKind::Skill {
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
        let disk_bytes = fs::read(&path).map_err(|e| DriftResolutionError::Io(path.clone(), e))?;
        let store_bytes = if store_dest.exists() {
            fs::read(&store_dest).map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?
        } else {
            Vec::new()
        };
        let expected = if store_dest.exists() {
            let prev_hash = hash_path(&store_dest, DriftContent::File)
                .map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
            at_model::ExpectedState::hash(prev_hash)?
        } else {
            at_model::ExpectedState::absent()
        };
        // A canonical MCP store holds every server; adopting writes back the one entry the
        // drifted native file holds, never the native document itself.
        let (bytes, manifest_hash) = if item.kind == at_model::ItemKind::McpServer {
            let definition =
                adopted_mcp_definition(&event, item.slug.as_str(), &path, &disk_bytes)?;
            let mut store = McpStore::parse(&store_bytes)
                .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
            if store.get(item.slug.as_str()).is_none() {
                return Err(DriftResolutionError::OrphanedDrift);
            }
            let mut definition_record = store
                .get(item.slug.as_str())
                .cloned()
                .ok_or(DriftResolutionError::OrphanedDrift)?;
            definition_record.transport = definition.0;
            definition_record.extra = definition.1;
            store.set(item.slug.as_str().to_owned(), definition_record);
            let hash = at_store::hash_mcp_entry(
                &serde_json::to_value(
                    store
                        .get(item.slug.as_str())
                        .ok_or(DriftResolutionError::OrphanedDrift)?,
                )
                .map_err(|error| DriftResolutionError::Store(error.to_string()))?,
            )
            .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
            (
                store
                    .to_bytes()
                    .map_err(|error| DriftResolutionError::Store(error.to_string()))?,
                Some(hash),
            )
        } else {
            (disk_bytes, None)
        };
        ops.push(at_model::FileOp::Write {
            path: store_dest.clone(),
            bytes,
            expected,
            follow_symlink: false,
            backup: true,
        });
        if let Some(hash) = manifest_hash
            && let Some(op) = manifest_hash_op(event.scope, request.store_root, item_id, &hash)?
        {
            ops.push(op);
        }
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

    let operation_id = if linked {
        // Nothing to copy: the edit already reached the canonical tree through the managed
        // link. The operation row still records that the drift was adopted.
        at_db::ops::record(
            request.connection,
            &at_db::ops::NewOperation::new(at_db::ops::OperationKind::Adopt, request.actor)
                .with_scope(event.scope)
                .with_summary(&format!(
                    "Adopt drifted {} for {} (already canonical through the managed link)",
                    item.name, event.agent_id
                )),
            &[],
            at_db::ops::OperationStatus::Succeeded,
            None,
        )?
        .to_string()
    } else {
        let mut recorder =
            at_fs::DbOperationRecorder::new_with_actor(request.connection, 30, request.actor);
        let mut context = at_fs::ApplyContext {
            store_root: request.store_root,
            backup_folder: request.backup_root,
            recorder: &mut recorder,
        };
        let outcome = match registry {
            Some(registry) => at_fs::apply_with_roots_and_registry(
                &change_set,
                &mut context,
                request.authorized_roots,
                registry,
            )?,
            None => at_fs::apply_with_roots(&change_set, &mut context, request.authorized_roots)?,
        };
        outcome.operation_id
    };

    let new_store_hash = hash_path(&store_dest, content_kind)
        .map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
    at_db::versions::record_skill_version(
        request.connection,
        &at_db::versions::NewVersion::new(&item_id, event.scope, &new_store_hash),
    )?;
    at_db::deployments::update_drift_hashes(
        request.connection,
        &item_id,
        &event.agent_id,
        event.scope,
        &new_store_hash,
        &new_store_hash,
    )?;
    if item.kind == at_model::ItemKind::McpServer {
        sync_file_baseline(request.connection, event.scope, &path, &store_dest)?;
    }
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

/// Reads the canonical definition one native MCP file holds for `slug`.
fn adopted_mcp_definition(
    event: &at_db::drift::DriftEvent,
    slug: &str,
    native_path: &Path,
    disk_bytes: &[u8],
) -> Result<(at_model::McpTransport, BTreeMap<String, serde_json::Value>), DriftResolutionError> {
    let text = std::str::from_utf8(disk_bytes).map_err(|error| {
        DriftResolutionError::Store(format!("native MCP file is not UTF-8: {error}"))
    })?;
    match &event.agent_id {
        AgentId::ClaudeCode => {
            let scope = claude_config_scope(event.scope, native_path)?;
            let config = at_adapters::claude_mcp::ClaudeMcpConfig::parse(text, scope)
                .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
            let server = config.servers().get(slug).ok_or_else(|| {
                DriftResolutionError::Store(format!(
                    "server `{slug}` is not in {}; nothing to adopt",
                    native_path.display()
                ))
            })?;
            Ok((server.transport.clone(), server.extra.clone()))
        }
        AgentId::Omp => {
            let config = at_adapters::omp_mcp::OmpMcpConfig::parse(text)
                .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
            let server = config.server(slug).ok_or_else(|| {
                DriftResolutionError::Store(format!(
                    "server `{slug}` is not in {}; nothing to adopt",
                    native_path.display()
                ))
            })?;
            Ok((server.transport.clone(), server.extra.clone()))
        }
        AgentId::Codex => {
            let config = at_adapters::codex_mcp::CodexConfigFile::parse(text)
                .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
            let server = config
                .server(slug)
                .map_err(|error| DriftResolutionError::Store(error.to_string()))?
                .ok_or_else(|| {
                    DriftResolutionError::Store(format!(
                        "server `{slug}` is not in {}; nothing to adopt",
                        native_path.display()
                    ))
                })?;
            let transport = codex_entry_transport(&server.entry)?;
            Ok((transport, BTreeMap::new()))
        }
        other => Err(DriftResolutionError::Store(format!(
            "adopting MCP servers from {other} is not supported; the store was left unchanged"
        ))),
    }
}

/// Converts one Codex table back into a canonical transport.
fn codex_entry_transport(
    entry: &at_adapters::codex_mcp::CodexServerEntry,
) -> Result<at_model::McpTransport, DriftResolutionError> {
    if let Some(url) = entry.url.as_deref() {
        let url = url.parse().map_err(|error| {
            DriftResolutionError::Store(format!("invalid Codex server URL: {error}"))
        })?;
        return Ok(at_model::McpTransport::Http {
            url,
            headers: BTreeMap::new(),
            oauth: None,
        });
    }
    let command = entry.command.clone().ok_or_else(|| {
        DriftResolutionError::Store("the Codex entry has no command to adopt".to_owned())
    })?;
    Ok(at_model::McpTransport::Stdio {
        command,
        args: entry.args.clone().unwrap_or_default(),
        cwd: entry.cwd.clone(),
        env: entry.env.clone().unwrap_or_default(),
    })
}

/// Picks the Claude Code root a native MCP file holds: `.mcp.json` is the project file, and
/// the user file is the user scope. Other project files need a project path drift lacks.
fn claude_config_scope(
    scope: Scope,
    native_path: &Path,
) -> Result<at_adapters::claude_mcp::ClaudeConfigScope, DriftResolutionError> {
    if native_path
        .file_name()
        .is_some_and(|name| name == ".mcp.json")
    {
        Ok(at_adapters::claude_mcp::ClaudeConfigScope::ProjectFile)
    } else if scope == Scope::User {
        Ok(at_adapters::claude_mcp::ClaudeConfigScope::User)
    } else if let Some(project) = native_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::to_str)
    {
        Ok(at_adapters::claude_mcp::ClaudeConfigScope::Project {
            project_path: project.to_owned(),
        })
    } else {
        Err(DriftResolutionError::Store(
            "the Claude Code project path for this MCP file is unavailable".to_owned(),
        ))
    }
}

/// Builds the `store.json` write that re-records one item's canonical hash.
///
/// A store without a manifest record for the item (an imported, pre-manifest store) keeps
/// adopting without a manifest write; there is no identity record to refresh.
fn manifest_hash_op(
    scope: Scope,
    store_root: &Path,
    item_id: ItemId,
    hash: &at_store::manifest::ContentHash,
) -> Result<Option<at_model::FileOp>, DriftResolutionError> {
    let layout = at_store::layout::StoreLayout::new(scope, store_root);
    if !layout.manifest_path().exists() {
        return Ok(None);
    }
    let mut manifest = at_store::manifest::StoreManifest::load(&layout)
        .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
    let Some(mut item) = manifest.manifest.item(item_id).cloned() else {
        return Ok(None);
    };
    item.hash = hash.clone();
    manifest
        .manifest
        .update_item(item)
        .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
    Ok(Some(at_model::FileOp::Write {
        path: layout.manifest_path(),
        bytes: manifest
            .manifest
            .to_bytes()
            .map_err(|error| DriftResolutionError::Store(error.to_string()))?,
        expected: ExpectedState::hash(manifest.hash.as_str())?,
        follow_symlink: false,
        backup: true,
    }))
}

/// Restores the store's canonical version over a drifted on-disk path (TC-013).
pub fn restore_drift(
    request: DriftRestoreRequest<'_>,
) -> Result<DriftResolutionOutcome, DriftResolutionError> {
    restore_drift_inner(request, None)
}

/// Restores drift while registering expected file writes before their atomic replacements.
pub fn restore_drift_with_registry(
    request: DriftRestoreRequest<'_>,
    registry: &at_fs::RecentWrites,
) -> Result<DriftResolutionOutcome, DriftResolutionError> {
    restore_drift_inner(request, Some(registry))
}

fn restore_drift_inner(
    request: DriftRestoreRequest<'_>,
    registry: Option<&at_fs::RecentWrites>,
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

    // A linked skill already reads the store through the link; there is nothing to write.
    let linked = item.kind == at_model::ItemKind::Skill && is_link_to(&path, &store_dest);
    let hash_source = if linked { &store_dest } else { &path };
    let current_disk_hash = hash_path(hash_source, content_kind)
        .map_err(|e| DriftResolutionError::Io(hash_source.clone(), e))?;
    if let Some(expected_actual) = &event.actual_hash
        && &current_disk_hash != expected_actual
    {
        return Err(DriftResolutionError::DriftChangedAgain {
            expected: expected_actual.clone(),
            actual: current_disk_hash,
        });
    }

    let operation_id = at_db::ops::OperationId::new().to_string();
    let preview_id = format!("pv_{}", uuid::Uuid::now_v7());
    let mut ops = Vec::new();
    let touched_paths = vec![path.clone()];
    let mut expected_written: Option<Vec<u8>> = None;

    if linked {
        // No file operation: the link already exposes the canonical content.
    } else if item.kind == at_model::ItemKind::Skill {
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
        let (expected, disk_bytes) = if path.exists() {
            let bytes = fs::read(&path).map_err(|e| DriftResolutionError::Io(path.clone(), e))?;
            (at_model::ExpectedState::hash(&current_disk_hash)?, bytes)
        } else {
            (at_model::ExpectedState::absent(), Vec::new())
        };
        let bytes = if item.kind == at_model::ItemKind::McpServer {
            // The canonical entry is projected into the current native document through the
            // same adapter path `mcp enable` uses, so only this server changes: other servers,
            // comments and unrelated keys stay byte-identical.
            let rendered = projected_mcp_bytes(
                &request,
                &event,
                item_id,
                item.slug.as_str(),
                item.project_id.as_deref(),
                &path,
                &store_layout,
                &disk_bytes,
            )?;
            expected_written = Some(rendered.clone());
            rendered
        } else {
            fs::read(&store_dest).map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?
        };
        ops.push(at_model::FileOp::Write {
            path: path.clone(),
            bytes,
            expected,
            follow_symlink: at_adapters::adapter::follows_native_config_symlink(&event.agent_id),
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

    let operation_id = if linked {
        at_db::ops::record(
            request.connection,
            &at_db::ops::NewOperation::new(at_db::ops::OperationKind::DriftRestore, request.actor)
                .with_scope(event.scope)
                .with_summary(&format!(
                    "Restore {} for {} (already canonical through the managed link)",
                    item.name, event.agent_id
                )),
            &[],
            at_db::ops::OperationStatus::Succeeded,
            None,
        )?
        .to_string()
    } else {
        let mut recorder =
            at_fs::DbOperationRecorder::new_with_actor(request.connection, 30, request.actor);
        let mut context = at_fs::ApplyContext {
            store_root: request.store_root,
            backup_folder: request.backup_root,
            recorder: &mut recorder,
        };
        let outcome = match registry {
            Some(registry) => at_fs::apply_with_roots_and_registry(
                &change_set,
                &mut context,
                request.authorized_roots,
                registry,
            )?,
            None => at_fs::apply_with_roots(&change_set, &mut context, request.authorized_roots)?,
        };
        outcome.operation_id
    };

    // An MCP restore reports success only when the bytes the adapter rendered reached the file.
    if let Some(rendered) = expected_written {
        let written = fs::read(&path).map_err(|e| DriftResolutionError::Io(path.clone(), e))?;
        if written != rendered {
            return Err(DriftResolutionError::Store(format!(
                "the restored MCP entry did not reach {}; the drift event stays open",
                path.display()
            )));
        }
    }

    let store_hash = hash_path(&store_dest, content_kind)
        .map_err(|e| DriftResolutionError::Io(store_dest.clone(), e))?;
    // The projected MCP file keeps unrelated servers, so its hash can differ from the store's.
    let restored_hash = if linked {
        store_hash.clone()
    } else {
        hash_path(&path, content_kind).map_err(|e| DriftResolutionError::Io(path.clone(), e))?
    };
    at_db::deployments::update_drift_hashes(
        request.connection,
        &item_id,
        &event.agent_id,
        event.scope,
        &store_hash,
        &restored_hash,
    )?;
    if item.kind == at_model::ItemKind::McpServer {
        sync_file_baseline(request.connection, event.scope, &path, &store_dest)?;
    }
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

/// Builds the bytes an MCP restore writes for one canonical definition (TC-040).
///
/// The canonical `mcp.json` is content, not a native document, so it cannot be parsed by an
/// agent's own config parser. The entry is instead projected into the current native file
/// through [`crate::mcp_plan::plan`], the same path `mcp enable` and the MCP screen use: the
/// adapter patches only that server's key and keeps every other server, comment and key. The
/// agent's capability policy is applied too, so a refused secret is dropped with the same rule
/// as a normal deployment instead of being written as an unverified reference.
// The projection needs the drift request, event, item identity and native target together;
// splitting them into a struct would only move the same context one line away.
#[allow(clippy::too_many_arguments)]
fn projected_mcp_bytes(
    request: &DriftRestoreRequest<'_>,
    event: &at_db::drift::DriftEvent,
    item_id: ItemId,
    slug: &str,
    project_id: Option<&str>,
    native_path: &Path,
    store_layout: &at_store::layout::StoreLayout,
    disk_bytes: &[u8],
) -> Result<Vec<u8>, DriftResolutionError> {
    let loaded = McpStore::load(store_layout)
        .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
    let definition: McpServerDef = loaded
        .store
        .get(slug)
        .cloned()
        .ok_or(DriftResolutionError::OrphanedDrift)?;
    let source = std::str::from_utf8(disk_bytes).map_err(|error| {
        DriftResolutionError::Store(format!("native MCP file is not UTF-8: {error}"))
    })?;
    let registry = AgentAdapterRegistry::register();
    let env = AgentEnvironment {
        home: request.home.to_path_buf(),
        project: None,
        variables: std::env::var("PATH")
            .map(|path| BTreeMap::from([(String::from("PATH"), path)]))
            .unwrap_or_default(),
    };
    let target = McpTarget {
        agent: event.agent_id.clone(),
        scope: event.scope,
        item_id,
        slug: slug.to_owned(),
        path: native_path.to_path_buf(),
        source: source.to_owned(),
        expected: ExpectedState::hash(at_fs::sha256_hex(disk_bytes))?,
        unsupported_acknowledged: false,
        installed: true,
    };
    let intent = McpIntent::SetEnabledForAgent {
        agent: event.agent_id.clone(),
        enabled: true,
    };
    let plan = crate::mcp_plan::plan(McpPlanRequest {
        registry: &registry,
        env: &env,
        intent: &intent,
        targets: std::slice::from_ref(&target),
        definition: &definition,
        scope: event.scope,
        project_id: project_id.map(str::to_owned),
        locked_reason: None,
        store_layout: None,
        target_store_layout: None,
    })
    .map_err(|error| DriftResolutionError::Store(error.to_string()))?;
    let native = plan.native.ok_or_else(|| {
        DriftResolutionError::Store(
            "the MCP restore plan produced no native change; the file was left unchanged"
                .to_owned(),
        )
    })?;
    let mut rendered = None;
    for op in native.ops {
        if let at_model::FileOp::Write {
            path,
            bytes: written,
            ..
        } = op
            && path == native_path
        {
            rendered = Some(written);
        }
    }
    rendered.ok_or_else(|| {
        DriftResolutionError::Store(
            "the MCP restore plan did not target the drifted file; it was left unchanged"
                .to_owned(),
        )
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
            actor: at_db::ops::OperationActor::Ui,
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
            home: &root,
            actor: at_db::ops::OperationActor::Ui,
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
            home: &root,
            actor: at_db::ops::OperationActor::Ui,
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
