//! Generic, history-backed operation undo.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use at_db::retention::{RetentionError, ensure_undoable};
use at_db::{backups, ops};
use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder, OperationRecorder};
use at_model::{ChangeSet, ExpectedState, FileOp, ItemId, Scope};
use at_store::layout::StoreLayout;
use at_store::manifest::StoreManifest;
use at_store::mcp_store::McpStore;
use rusqlite::Connection;

/// Inputs for undoing one recorded operation.
pub struct UndoOperationRequest<'a> {
    /// Database containing the operation, file rows and backup catalogue.
    pub connection: &'a Connection,
    /// Store root used for journals and operation backups.
    pub store_root: &'a Path,
    /// Backup root used by the operation apply pipeline.
    pub backup_root: &'a Path,
    /// Roots authorized for absolute native paths.
    pub authorized_roots: &'a ApplyRoots,
    /// Actor recorded on the new Undo operation.
    pub actor: ops::OperationActor,
    /// Proceed despite files that changed after the operation (`false` by default).
    ///
    /// With `false`, any externally edited path refuses the undo with
    /// [`UndoOutcome::Conflict`] before anything is written. With `true`, those paths are
    /// restored from the operation backups anyway, deliberately replacing the newer edits;
    /// the inverse operations still carry the observed current state, so the at-fs pipeline
    /// refuses a path that changes again between the check and the apply.
    pub force: bool,
}

/// Outcome of undoing a recorded operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UndoOutcome {
    /// Every recorded file effect was reversed.
    Restored {
        /// New operation row id.
        undo_operation_id: String,
    },
    /// The operation is newer on disk than the recorded state.
    Conflict {
        /// Paths left unchanged because their current hash differs.
        paths: Vec<PathBuf>,
    },
    /// The operation is outside the fixed undo window.
    Expired,
    /// The operation already has a successful Undo child operation.
    AlreadyUndone,
    /// The operation did not succeed, so there are no recorded effects to reverse.
    NotUndoable,
    /// Some operation rows cannot be safely represented by the file undo pipeline.
    Partial {
        /// Paths that could not be restored.
        paths: Vec<PathBuf>,
    },
}

/// Failures querying history or applying the reviewed inverse ChangeSet.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UndoError {
    /// Database access or decoding failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// Filesystem apply rejected or rolled back the inverse ChangeSet.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// The system clock predates the Unix epoch.
    #[error("system clock is before the Unix epoch")]
    Clock,
    /// An operation row has no valid ULID.
    #[error("invalid operation id: {0}")]
    OperationId(String),
    /// A backup row needed to restore a file is missing or incomplete.
    #[error("operation backup is missing or incomplete for {0}")]
    MissingBackup(PathBuf),
    /// Retention policy refused or failed the window check.
    #[error("undo window check failed: {0}")]
    Retention(String),
    /// A recorded item id is not a valid AgenTuna item id.
    #[error("invalid item id: {0}")]
    ItemId(String),
    /// A backup could not be read.
    #[error("cannot read operation backup `{path}`: {source}")]
    BackupRead {
        /// Backup path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A shared store document could not be merged with the current content.
    #[error("cannot merge the store document: {0}")]
    Store(String),
}

struct UndoRecorder<'a> {
    connection: &'a Connection,
    actor: ops::OperationActor,
    original: ops::OperationId,
    inner: Option<DbOperationRecorder<'a>>,
}

impl OperationRecorder for UndoRecorder<'_> {
    fn begin(&mut self, set: &ChangeSet) -> Result<String, String> {
        let mut operation = ops::NewOperation::new(ops::OperationKind::Undo, self.actor)
            .with_summary(&set.summary)
            .undoing(&self.original);
        operation.scope = set.scope;
        let id = ops::begin(self.connection, &operation).map_err(|error| error.to_string())?;
        self.inner = Some(DbOperationRecorder::new_with_actor(
            self.connection,
            30,
            self.actor,
        ));
        Ok(id.to_string())
    }

    fn file_completed(
        &mut self,
        operation_id: &str,
        path: &Path,
        before_hash: Option<&str>,
        after_hash: Option<&str>,
        backup_path: Option<&Path>,
    ) -> Result<(), String> {
        let recorder = self
            .inner
            .as_mut()
            .ok_or_else(|| "undo operation was not started".to_owned())?;
        recorder
            .file_completed(operation_id, path, before_hash, after_hash, backup_path)
            .map_err(|error| format!("could not record undo file `{}`: {error}", path.display()))
    }

    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String> {
        let recorder = self
            .inner
            .as_mut()
            .ok_or_else(|| "undo operation was not started".to_owned())?;
        recorder.finish(operation_id, succeeded, manual_review)
    }
}

/// Undo an operation after checking the 30-day window and every recorded after-hash.
///
/// The inverse plan is applied through at-fs so the Undo operation has its own journal,
/// backups and file rows. Conflicts are returned before any write is submitted.
///
/// # Errors
/// Returns [`UndoError`] when history is invalid, a backup cannot be read, or the at-fs
/// ChangeSet pipeline refuses the inverse.
pub fn undo_operation(
    request: UndoOperationRequest<'_>,
    operation_id: &str,
) -> Result<UndoOutcome, UndoError> {
    let operation = ops::OperationId::parse(operation_id)
        .map_err(|_| UndoError::OperationId(operation_id.to_owned()))?;
    let detail = match ops::operation_detail(request.connection, operation_id)? {
        Some(detail) => detail,
        None => return Err(UndoError::OperationId(operation_id.to_owned())),
    };
    let already_undone: bool = request
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE undo_of = ?1 AND status = 'succeeded')",
            [operation_id],
            |row| row.get(0),
        )
        .map_err(|error| UndoError::Database(error.into()))?;
    if already_undone {
        return Ok(UndoOutcome::AlreadyUndone);
    }
    // A failed or interrupted operation left no landed effects to reverse; offering Undo would
    // rewrite files the rollback already restored.
    if detail.operation.status != Some(ops::OperationStatus::Succeeded) {
        return Ok(UndoOutcome::NotUndoable);
    }
    let now = SystemTime::now();
    match ensure_undoable(request.connection, &operation, now) {
        Ok(()) => {}
        Err(RetentionError::UndoExpired { .. }) => return Ok(UndoOutcome::Expired),
        Err(RetentionError::UnknownOperation { .. }) => {
            return Err(UndoError::OperationId(operation_id.to_owned()));
        }
        Err(RetentionError::Db(error)) => return Err(UndoError::Database(error)),
        Err(error) => return Err(UndoError::Retention(error.to_string())),
    }
    let list = ops::undo_file_list_with(request.connection, operation_id, &TreeFileProbe)?
        .ok_or_else(|| UndoError::OperationId(operation_id.to_owned()))?;
    let effective_files = collapse_file_rows(&list.files);
    let satisfied = satisfied_paths(&effective_files);
    // A shared store document (`store.json`, `mcp.json`) is merged entry by entry, so a later
    // unrelated `skills install` or `mcp add` cannot block reverting this operation.
    let conflicts = effective_files
        .iter()
        .filter_map(|file| refusing_path(file, &satisfied))
        .filter(|path| container_kind(request.store_root, path).is_none())
        .collect::<Vec<_>>();
    if !conflicts.is_empty() && !request.force {
        return Ok(UndoOutcome::Conflict { paths: conflicts });
    }
    let operation_items = at_db::deployments::operation_item_ids(request.connection, operation_id)?;

    let mut inverse = Vec::new();
    let mut links = Vec::new();
    let mut incomplete = Vec::new();
    for file in effective_files.iter().rev() {
        let Some(path_text) = file.path.as_deref() else {
            incomplete.push(PathBuf::new());
            continue;
        };
        let path = PathBuf::from(path_text);
        if satisfied.contains(&path) {
            continue;
        }
        let Some(action) = file.inverse else {
            incomplete.push(path);
            continue;
        };
        match action {
            ops::UndoAction::Delete => {
                let Some(hash) = file.recorded_hash.as_deref() else {
                    incomplete.push(path);
                    continue;
                };
                let expected = ExpectedState::hash(hash)
                    .map_err(|_| UndoError::MissingBackup(path.clone()))?;
                inverse.push(FileOp::Delete { path, expected });
            }
            ops::UndoAction::Restore => {
                if let Some((container, container_root)) = container_kind(request.store_root, &path)
                {
                    let current = match fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(source) => {
                            return Err(UndoError::BackupRead { path, source });
                        }
                    };
                    let before = match file.backup_id.as_deref() {
                        Some(backup_id) => {
                            let backup = backups::backup_by_id(request.connection, backup_id)?
                                .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                            let blob = backup
                                .backup_path
                                .as_deref()
                                .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                            Some(fs::read(blob).map_err(|source| UndoError::BackupRead {
                                path: PathBuf::from(blob),
                                source,
                            })?)
                        }
                        None => None,
                    };
                    // Untouched since the operation: the exact inverse is the backed-up
                    // bytes, so apply-then-undo is byte-identical. Only a shared document
                    // changed by another command is merged entry by entry.
                    let merged = if !file.conflicted {
                        before.unwrap_or(current.clone())
                    } else {
                        match container {
                            StoreContainer::Manifest => {
                                merge_manifest(&current, before.as_deref(), &operation_items)?
                            }
                            StoreContainer::Mcp => {
                                let manifest_bytes =
                                    fs::read(container_root.join("store.json")).unwrap_or_default();
                                let slugs = operation_slugs(
                                    request.connection,
                                    &operation_items,
                                    &manifest_bytes,
                                )?;
                                merge_mcp(&current, before.as_deref(), &slugs)?
                            }
                        }
                    };
                    if merged != current {
                        inverse.push(FileOp::Write {
                            path: path.clone(),
                            bytes: merged,
                            expected: ExpectedState::hash(at_fs::sha256_hex(&current))
                                .map_err(|_| UndoError::MissingBackup(path.clone()))?,
                            follow_symlink: false,
                            backup: true,
                        });
                    }
                    continue;
                }
                let Some(backup_id) = file.backup_id.as_deref() else {
                    incomplete.push(path);
                    continue;
                };
                let backup = backups::backup_by_id(request.connection, backup_id)?
                    .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                let blob = backup
                    .backup_path
                    .as_deref()
                    .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                if !Path::new(blob).starts_with(request.backup_root) {
                    incomplete.push(path);
                    continue;
                }
                let metadata =
                    fs::symlink_metadata(blob).map_err(|source| UndoError::BackupRead {
                        path: PathBuf::from(blob),
                        source,
                    })?;
                if metadata.is_dir() || metadata.file_type().is_symlink() {
                    // Removing what the operation left at this path. A ChangeSet accepts a
                    // repeated destination only when a Delete immediately precedes the
                    // operation replacing it, so this removal must travel with its
                    // replacement instead of being emitted on its own.
                    let removal = match &file.current {
                        ops::CurrentState::Hash(hash) => Some(FileOp::Delete {
                            path: path.clone(),
                            expected: ExpectedState::hash(hash)
                                .map_err(|_| UndoError::MissingBackup(path.clone()))?,
                        }),
                        ops::CurrentState::Missing => None,
                        ops::CurrentState::Unverifiable => {
                            incomplete.push(path);
                            continue;
                        }
                    };
                    // The backup is re-created rather than moved out of the operation folder:
                    // a copy keeps the backup intact and leaves only invertible file rows, so
                    // this Undo operation is itself undoable. A symlink backup is re-created as
                    // the link it was; a tree is copied.
                    if metadata.file_type().is_symlink() {
                        let recorded =
                            fs::read_link(blob).map_err(|source| UndoError::BackupRead {
                                path: PathBuf::from(blob),
                                source,
                            })?;
                        // A link's recorded target is relative to where the link lived, not to
                        // the operation backup folder it was moved into.
                        let target = if recorded.is_absolute() {
                            recorded
                        } else {
                            let parent = path
                                .parent()
                                .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                            normalize_lexically(&parent.join(recorded))
                        };
                        links.extend(removal);
                        links.push(FileOp::Symlink {
                            link_path: path,
                            target,
                            mode: at_model::DeployMode::Link,
                        });
                    } else {
                        // Plain tree copy: no copy manifest, so the restored tree hashes
                        // exactly like the backed-up original.
                        inverse.extend(removal);
                        inverse.push(FileOp::CopyTree {
                            source: PathBuf::from(blob),
                            dest: path,
                            expected: ExpectedState::Absent,
                        });
                    }
                    continue;
                }
                let bytes = fs::read(blob).map_err(|source| UndoError::BackupRead {
                    path: PathBuf::from(blob),
                    source,
                })?;
                let expected = match &file.current {
                    ops::CurrentState::Hash(hash) => ExpectedState::hash(hash)
                        .map_err(|_| UndoError::MissingBackup(path.clone()))?,
                    ops::CurrentState::Missing => ExpectedState::Absent,
                    ops::CurrentState::Unverifiable => {
                        incomplete.push(path);
                        continue;
                    }
                };
                inverse.push(FileOp::Write {
                    path,
                    bytes,
                    expected,
                    follow_symlink: false,
                    backup: true,
                });
            }
            // `operation_files` records a move's destination, never its source, so a move
            // cannot be reversed from history alone. Undo never plans one itself: it copies
            // backups and re-creates links, so its own rows stay invertible.
            ops::UndoAction::Move => incomplete.push(path),
            _ => incomplete.push(path),
        }
    }
    // A restored link must follow the content it points at. The reverse walk restores the
    // package after its deployment link, so defer every link to the end of the inverse set,
    // together with the removal of the link the operation had created there.
    inverse.extend(links);
    if !incomplete.is_empty() {
        return Ok(UndoOutcome::Partial { paths: incomplete });
    }
    if inverse.is_empty() {
        // Kinds without file effects (scan, settings, an empty restore) still get an audit
        // row, so the operation is marked undone and a second undo is refused.
        let recorded = ops::record(
            request.connection,
            &ops::NewOperation::new(ops::OperationKind::Undo, request.actor)
                .with_scope(detail.operation.scope.unwrap_or(at_model::Scope::User))
                .with_summary(&format!("Undo operation {operation_id} (no file effects)"))
                .undoing(&operation),
            &[],
            ops::OperationStatus::Succeeded,
            None,
        )?;
        return Ok(UndoOutcome::Restored {
            undo_operation_id: recorded.to_string(),
        });
    }
    let created_at_unix_seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UndoError::Clock)?
        .as_secs();
    let set = ChangeSet {
        id: at_model::ItemId::new().to_string(),
        preview_id: format!("undo:{operation_id}"),
        operation_kind: at_model::OperationKind::parse("undo")
            .map_err(|error| UndoError::OperationId(error.to_string()))?,
        scope: detail.operation.scope,
        project_id: None,
        summary: format!("Undo operation {operation_id}"),
        ops: inverse,
        created_at_unix_seconds,
    };
    let mut recorder = UndoRecorder {
        connection: request.connection,
        actor: request.actor,
        original: operation,
        inner: None,
    };
    let context = ApplyContext {
        store_root: request.store_root,
        backup_folder: request.backup_root,
        recorder: &mut recorder,
    };
    // A selection that spans scopes writes and deletes in every store it touches, so the prune
    // roots are the stores its shared documents name, not just the request's first store.
    let mut store_roots = std::collections::BTreeSet::new();
    store_roots.insert(request.store_root.to_path_buf());
    for file in &effective_files {
        if let Some(path) = file.path.as_deref()
            && let Some((_, root)) = container_kind(request.store_root, Path::new(path))
        {
            store_roots.insert(root);
        }
    }
    let removed_store_paths = set
        .ops
        .iter()
        .filter_map(|op| match op {
            FileOp::Delete { path, .. }
                if store_roots.iter().any(|root| path.starts_with(root)) =>
            {
                Some(path.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let outcome = at_fs::apply_with_roots(&set, &mut { context }, request.authorized_roots)?;
    for root in &store_roots {
        let protected = layout_directories(root);
        // The manifest is created by a first-run operation; when this undo deleted that file
        // the whole skeleton belongs to the operation, so empty layout directories may go too.
        let created_store = removed_store_paths
            .iter()
            .any(|path| path == &root.join(at_store::layout::MANIFEST_FILE));
        prune_empty_store_dirs(root, &protected, &removed_store_paths);
        if created_store {
            prune_created_store_dirs(root, &protected);
        }
    }
    if let Some(kind) = detail.operation.kind {
        revert_migration_metadata(request.connection, operation_id, kind)?;
    }
    // A delete removes rows that no restored file can recreate (which agents held a
    // deployment, its mode and hashes). The command captured them next to the operation
    // backups; put them back so the index describes the restored tree again.
    if let Some(metadata) = crate::undo_meta::read(request.backup_root, operation_id) {
        crate::undo_meta::restore(request.connection, &metadata)?;
    }
    Ok(UndoOutcome::Restored {
        undo_operation_id: outcome.operation_id,
    })
}

/// A shared store document that holds several items and must be merged, not restored.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StoreContainer {
    /// `store.json`, the item manifest.
    Manifest,
    /// `mcp.json`, the canonical server definitions.
    Mcp,
}

/// Recognizes a shared store document and returns the store root that owns it.
///
/// One operation can span scopes, so its rows name `store.json` or `mcp.json` in more than the
/// request's first store. The owning root is the file's own directory, accepted only when it is
/// the request's store or provably a store — every store holds both documents — so a document is
/// never recognized from its file name alone.
fn container_kind(store_root: &Path, path: &Path) -> Option<(StoreContainer, PathBuf)> {
    let parent = path.parent()?;
    let container = match path.file_name().and_then(|name| name.to_str())? {
        "store.json" => StoreContainer::Manifest,
        "mcp.json" => StoreContainer::Mcp,
        _ => return None,
    };
    let is_store = parent == store_root
        || (parent.join("store.json").is_file() && parent.join("mcp.json").is_file());
    is_store.then(|| (container, parent.to_path_buf()))
}

/// Whether the undo would find the path already in its target state.
///
/// A created link the user has since removed, or a file already holding its previous bytes,
/// needs no write; failing the whole operation because the recorded after-state is gone would
/// refuse an undo that has nothing left to do for that path.
fn desired_state_holds(file: &ops::UndoFile, path: &Path) -> bool {
    match file.inverse {
        Some(ops::UndoAction::Delete) => matches!(
            fs::symlink_metadata(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ),
        Some(ops::UndoAction::Restore) => file.restore_hash.as_deref().is_some_and(|expected| {
            at_fs::changeset::hash_node(path)
                .ok()
                .flatten()
                .is_some_and(|current| current == expected)
        }),
        _ => false,
    }
}

/// Recorded paths whose desired end state already holds and need no write at all.
fn satisfied_paths(files: &[ops::UndoFile]) -> std::collections::BTreeSet<PathBuf> {
    files
        .iter()
        .filter(|file| {
            file.path
                .as_deref()
                .is_some_and(|path| desired_state_holds(file, Path::new(path)))
        })
        .filter_map(|file| file.path.as_ref().map(PathBuf::from))
        .collect()
}

/// The path one recorded row makes the undo refuse, if any.
///
/// [`undo_operation`] and [`undo_readiness`] share this predicate, so the check never offers an
/// undo the undo then refuses on the same evidence. Two rows do not refuse:
///
///  * a path whose desired end state already holds — nothing would be written for it;
///  * a path the operation replaced that a later command then removed, which is re-created from
///    the operation backup with [`ExpectedState::Absent`]; the removal did not edit the bytes
///    the backup holds.
///
/// A store container (`store.json`, `mcp.json`) is reported here: readiness holds no store root
/// and cannot tell a shared document from an unrelated file with that name, so it stays
/// conservative, while [`undo_operation`] merges the document instead of refusing it.
fn refusing_path(
    file: &ops::UndoFile,
    satisfied: &std::collections::BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    if !file.conflicted {
        return None;
    }
    if matches!(file.current, ops::CurrentState::Missing) && file.backup_id.is_some() {
        return None;
    }
    let path = PathBuf::from(file.path.as_ref()?);
    (!satisfied.contains(&path)).then_some(path)
}

/// Reverts this operation's items inside the manifest the user has now, keeping every other.
fn merge_manifest(
    current: &[u8],
    before: Option<&[u8]>,
    items: &[String],
) -> Result<Vec<u8>, UndoError> {
    let mut manifest =
        StoreManifest::parse(current).map_err(|error| UndoError::Store(error.to_string()))?;
    let previous = before
        .map(StoreManifest::parse)
        .transpose()
        .map_err(|error| UndoError::Store(error.to_string()))?;
    for text in items {
        let Ok(id) = text.parse::<ItemId>() else {
            continue;
        };
        match previous.as_ref().and_then(|previous| previous.item(id)) {
            Some(original) => {
                if manifest.item(id).is_some() {
                    manifest
                        .update_item(original.clone())
                        .map_err(|error| UndoError::Store(error.to_string()))?;
                }
            }
            None => {
                manifest.remove_item(id);
            }
        }
    }
    manifest
        .to_bytes()
        .map_err(|error| UndoError::Store(error.to_string()))
}

/// Reverts this operation's server entries inside the canonical `mcp.json`, keeping every other.
fn merge_mcp(
    current: &[u8],
    before: Option<&[u8]>,
    slugs: &[String],
) -> Result<Vec<u8>, UndoError> {
    let mut store =
        McpStore::parse(current).map_err(|error| UndoError::Store(error.to_string()))?;
    let previous = before
        .map(McpStore::parse)
        .transpose()
        .map_err(|error| UndoError::Store(error.to_string()))?;
    for slug in slugs {
        match previous.as_ref().and_then(|previous| previous.get(slug)) {
            Some(definition) => store.set(slug.clone(), definition.clone()),
            None => {
                store.remove(slug);
            }
        }
    }
    store
        .to_bytes()
        .map_err(|error| UndoError::Store(error.to_string()))
}

/// Slugs of the operation's MCP items, read from the manifest and then the index.
fn operation_slugs(
    connection: &Connection,
    items: &[String],
    manifest_bytes: &[u8],
) -> Result<Vec<String>, UndoError> {
    let manifest = StoreManifest::parse(manifest_bytes).ok();
    let mut slugs = Vec::new();
    for text in items {
        let Ok(id) = text.parse::<ItemId>() else {
            continue;
        };
        if let Some(item) = manifest.as_ref().and_then(|manifest| manifest.item(id)) {
            slugs.push(item.slug.as_str().to_owned());
            continue;
        }
        if let Some(record) = at_db::shared::items_by_ids(connection, &[id])?
            .into_iter()
            .next()
        {
            slugs.push(record.slug);
        }
    }
    Ok(slugs)
}

/// Resolves `.` and `..` components without touching the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// What a caller needs to decide whether undo is offered and how it will behave.
///
/// Produced by [`undo_readiness`] without writing anything; [`undo_operation`] re-checks
/// everything at undo time, so this is advisory, not an authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UndoReadiness {
    /// The operation row exists.
    pub known: bool,
    /// The operation already has an Undo child operation.
    pub already_undone: bool,
    /// The operation started before the fixed undo window.
    pub expired: bool,
    /// The fixed undo window in days.
    pub window_days: u32,
    /// Unix seconds when the window closes, when the operation exists.
    pub expires_at_unix_seconds: Option<i64>,
    /// Paths whose current state refuses the undo unless `force` is set.
    ///
    /// Computed with the same rules as [`undo_operation`], with one deliberate exception: a
    /// shared store document (`store.json`, `mcp.json`) is listed here even though the undo
    /// merges it, because readiness holds no store root and cannot tell the document from an
    /// unrelated file of that name.
    pub conflicted_paths: Vec<PathBuf>,
}

/// Reads undo readiness for one operation without writing anything.
///
/// # Errors
/// [`UndoError`] when history cannot be queried.
pub fn undo_readiness(
    connection: &Connection,
    operation_id: &str,
) -> Result<UndoReadiness, UndoError> {
    let known = ops::operation_detail(connection, operation_id)?.is_some();
    if !known {
        return Ok(UndoReadiness {
            known: false,
            already_undone: false,
            expired: false,
            window_days: at_db::retention::UNDO_WINDOW_DAYS,
            expires_at_unix_seconds: None,
            conflicted_paths: Vec::new(),
        });
    }
    let already_undone: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE undo_of = ?1 AND status = 'succeeded')",
            [operation_id],
            |row| row.get(0),
        )
        .map_err(|error| UndoError::Database(error.into()))?;
    let expires_at: Option<String> = connection
        .query_row(
            "SELECT strftime('%s', started_at, ?1) FROM operations WHERE id = ?2",
            rusqlite::params![
                format!("+{} days", at_db::retention::UNDO_WINDOW_DAYS),
                operation_id
            ],
            |row| row.get(0),
        )
        .map_err(|error| UndoError::Database(error.into()))?;
    let expires_at_unix_seconds = expires_at.and_then(|text| text.parse::<i64>().ok());
    let now_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UndoError::Clock)?
        .as_secs();
    let expired = expires_at_unix_seconds
        .is_some_and(|expires| expires <= i64::try_from(now_unix_seconds).unwrap_or(i64::MAX));
    let conflicted_paths = match ops::undo_file_list_with(connection, operation_id, &TreeFileProbe)?
    {
        Some(list) => {
            let effective = collapse_file_rows(&list.files);
            let satisfied = satisfied_paths(&effective);
            effective
                .iter()
                .filter_map(|file| refusing_path(file, &satisfied))
                .collect()
        }
        None => Vec::new(),
    };
    Ok(UndoReadiness {
        known: true,
        already_undone,
        expired,
        window_days: at_db::retention::UNDO_WINDOW_DAYS,
        expires_at_unix_seconds,
        conflicted_paths,
    })
}

/// Every directory the store layout owns.
///
/// An undo must not remove one of these, even when it is empty: a `skills/` folder without items
/// is still a valid store, and a migration that emptied the last item never asked for the folder
/// itself to disappear. The scope only decides whether `local/` exists, so the union protects it
/// in every store.
fn layout_directories(store_root: &Path) -> Vec<PathBuf> {
    let layout = StoreLayout::new(Scope::Project, store_root);
    let mut directories = vec![
        layout.skills_dir(),
        layout.rules_dir(),
        layout.backups_dir(),
        layout.state_dir(),
        layout.journals_dir(),
        layout.locks_dir(),
        layout.staging_dir(),
        layout.tmp_dir(),
    ];
    directories.extend(layout.local_dir());
    directories
}

/// Removes directories the undo emptied inside the store.
///
/// Deleting the files of a migrated item leaves its folder behind; an empty folder is debris
/// the user did not ask for. Only empty directories strictly below `store_root` are removed, and
/// never one the store layout owns ([`layout_directories`]): those belong to the store, not to
/// the operation being undone.
fn prune_empty_store_dirs(store_root: &Path, protected: &[PathBuf], removed: &[PathBuf]) {
    let mut candidates = Vec::new();
    for path in removed {
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory == store_root || !directory.starts_with(store_root) {
                break;
            }
            candidates.push(directory.to_path_buf());
            parent = directory.parent();
        }
    }
    candidates.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    candidates.dedup();
    for directory in candidates {
        if protected.contains(&directory) {
            continue;
        }
        let _ = fs::remove_dir(&directory);
    }
}

/// Removes the layout directories a first-run operation created, only when they are empty.
///
/// Called only when the undone operation created `store.json`, so every directory the layout
/// owns belonged to that operation. `fs::remove_dir` never removes a non-empty directory, so
/// user content written later survives and the store root stays. The operation's own
/// `.state/locks` lock file keeps that directory in place; the rest (skills, rules, backups,
/// plan journals) can disappear with the operation that created them.
fn prune_created_store_dirs(store_root: &Path, directories: &[PathBuf]) {
    let mut candidates = directories.to_vec();
    candidates.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    candidates.dedup();
    for directory in candidates {
        if directory.starts_with(store_root) {
            let _ = fs::remove_dir(&directory);
        }
    }
    let _ = fs::remove_dir(store_root);
}

/// Removes the deployment and native adoption rows a Migrate or Adopt operation wrote.
///
/// The inverse ChangeSet has already deleted the store files and restored the native paths, so
/// the metadata rows it wrote must disappear with them: an item that no longer exists on disk
/// must not keep a deployment, and the native rows it adopted become unmanaged findings again.
fn revert_migration_metadata(
    connection: &Connection,
    operation_id: &str,
    kind: ops::OperationKind,
) -> Result<(), UndoError> {
    if !matches!(
        kind,
        ops::OperationKind::Migrate | ops::OperationKind::Adopt
    ) {
        return Ok(());
    }
    let item_ids = at_db::deployments::operation_item_ids(connection, operation_id)?;
    at_db::deployments::delete_for_operation(connection, operation_id)?;
    for text in item_ids {
        let item_id: ItemId = text.parse().map_err(|_| UndoError::ItemId(text.clone()))?;
        if at_db::deployments::count_for_item(connection, &item_id)? == 0 {
            at_db::native::release_adoptions(connection, &item_id)?;
            at_db::items::delete_item(connection, &item_id)?;
        }
    }
    Ok(())
}

struct TreeFileProbe;

impl ops::FileProbe for TreeFileProbe {
    fn current_state(&self, path: &Path) -> ops::CurrentState {
        match at_fs::changeset::hash_node(path) {
            Ok(Some(hash)) => ops::CurrentState::Hash(hash),
            Ok(None) => ops::CurrentState::Missing,
            Err(_) => ops::CurrentState::Unverifiable,
        }
    }
}

fn collapse_file_rows(files: &[ops::UndoFile]) -> Vec<ops::UndoFile> {
    let mut indexes: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut effective: Vec<ops::UndoFile> = Vec::new();
    for file in files {
        let Some(path) = file.path.as_deref() else {
            effective.push(file.clone());
            continue;
        };
        if let Some(index) = indexes.get(path).copied() {
            if let Some(existing) = effective.get_mut(index) {
                // Two rows can share one path when an operation replaces it (a migrate
                // deletes the native item and creates a link there; a delete can journal a
                // transient re-creation). The row that carries the backup is the only one
                // that can restore the pre-operation bytes, so it wins over a hash-only row.
                if existing.backup_id.is_some() && file.backup_id.is_none() {
                    // If a later row's recorded state is what is on disk now, the current
                    // bytes are the operation's own result, not a newer external edit.
                    if let ops::CurrentState::Hash(current) = &file.current
                        && file.recorded_hash.as_deref() == Some(current)
                    {
                        existing.conflicted = false;
                    }
                    continue;
                }
                existing.recorded_hash.clone_from(&file.recorded_hash);
                existing.restore_hash.clone_from(&file.restore_hash);
                existing.current.clone_from(&file.current);
                existing.conflicted = file.conflicted;
                existing.backup_id.clone_from(&file.backup_id);
                existing.inverse = file.inverse;
                existing.action = file.action;
            }
        } else {
            indexes.insert(path.to_owned(), effective.len());
            effective.push(file.clone());
        }
    }
    effective
}

#[cfg(test)]
mod tests {
    use super::{Scope, layout_directories, prune_empty_store_dirs};

    #[test]
    fn pruning_emptied_directories_keeps_the_store_layout() {
        let temp = tempfile::TempDir::new().expect("temp");
        let root = temp.path();
        let layout = at_store::layout::StoreLayout::new(Scope::Project, root);
        std::fs::create_dir_all(layout.skills_dir()).expect("skills");
        std::fs::create_dir_all(layout.rules_dir()).expect("rules");
        let item = layout.skills_dir().join("demo");
        std::fs::create_dir_all(&item).expect("item folder");

        // The undo deleted the item's files and the rule, so every directory above them is
        // empty; only the item folder the migration created is debris.
        let removed = vec![item.join("SKILL.md"), layout.rules_dir().join("claude.md")];
        prune_empty_store_dirs(root, &layout_directories(root), &removed);

        assert!(!item.exists(), "an emptied item folder is removed");
        assert!(layout.skills_dir().is_dir(), "the store owns skills/");
        assert!(layout.rules_dir().is_dir(), "the store owns rules/");
    }
}
