//! Locked store load/save and drift reconciliation (TC-003).
//!
//! [`StoreManifest::load`] and [`StoreManifest::save`] already provide optimistic
//! concurrency on `store.json`; this module adds the pieces the story requires around
//! them:
//!
//! - an advisory `flock` on `.state/store.lock` — shared for [`load_store`], exclusive
//!   for [`save_store`] — so two AgenTuna processes serialise store transactions and a
//!   contended writer fails with [`StoreIoError::LockTimeout`] instead of interleaving;
//! - [`StoreSnapshot`], the classified view of one store: every manifest item is
//!   `Present`, `Modified` (content hash differs) or `Missing`, and content on disk that
//!   the manifest does not know is an `Orphan`. `mcp.json` is reconciled the same way:
//!   a server entry without a manifest item is an orphan, a manifest server missing from
//!   `mcp.json` is `Missing`, and a changed definition is `Modified`;
//! - [`reconcile_with`], which either reports the drift (the default) or — only with the
//!   confirmation [`ReconcileToken`] a report produced — imports orphan entries into the
//!   manifest and drops `Missing` entries. It never deletes an orphan file.
//!
//! Read-only access ([`StoreAccess::ReadOnly`]) is for CLI status and for a Global store
//! the process may not write: it never creates `.state/store.lock`, takes at most a
//! shared lock when the file already exists, and refuses every mutation with
//! [`StoreIoError::ReadOnly`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use at_fs::{BackupPolicy, ExpectedState, FsError};
use at_model::{ItemId, ItemKind};
use fs4::{FileExt, TryLockError};
use serde_json::Value;

use crate::instance::LockPolicy;
use crate::layout::{Slug, StoreError, StoreLayout};
use crate::manifest::{
    CanonicalJsonError, ContentHash, HashError, LoadedManifest, ManifestError, ManifestItem,
    SaveOutcome, StoreManifest, StorePath, canonical_json, hash_bytes, hash_mcp_entry,
    hash_rule_bytes, hash_skill_folder, resolve_item_path,
};

/// File name of the store lock inside `.state/`.
pub const STORE_LOCK_FILE: &str = "store.lock";

/// How long a store transaction waits for `.state/store.lock` before failing.
///
/// Five seconds matches the SQLite busy timeout used by `at-db`; a contended writer
/// reports [`StoreIoError::LockTimeout`] instead of writing half a change.
pub const STORE_LOCK_WAIT: Duration = Duration::from_secs(5);

/// Poll interval while waiting for the store lock.
const LOCK_POLL: Duration = Duration::from_millis(50);

/// The lock policy [`load_store`] and [`save_store`] use unless the caller overrides it.
#[must_use]
pub fn default_lock_policy() -> LockPolicy {
    LockPolicy {
        wait: STORE_LOCK_WAIT,
        poll: LOCK_POLL,
    }
}

/// Whether the caller may write the store.
///
/// [`StoreAccess::ReadOnly`] never creates `.state/` or `store.lock`, never writes
/// `store.json` or `mcp.json`, and is what CLI status and an unwritable Global store use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoreAccess {
    /// Reads and writes are allowed; `.state/store.lock` is created when missing.
    ReadWrite,
    /// Reads only; mutations return [`StoreIoError::ReadOnly`].
    ReadOnly,
}

impl StoreAccess {
    /// Whether this access may write.
    #[must_use]
    pub const fn is_writable(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// Why a store I/O operation did not happen.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreIoError {
    /// `.state/store.lock` stayed contended for the whole policy window. Nothing was
    /// read or written past the lock attempt.
    #[error("{}: store lock is held by another process (waited {waited:?})", .path.display())]
    LockTimeout {
        /// Lock file path.
        path: PathBuf,
        /// How long the caller waited.
        waited: Duration,
    },
    /// The lock file could not be opened or locked.
    #[error("{}: could not take the store lock: {source}", .path.display())]
    Lock {
        /// Lock file path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A mutation was requested through [`StoreAccess::ReadOnly`].
    #[error("{}: store is read-only; the change was not written", .path.display())]
    ReadOnly {
        /// Store root or file the mutation targeted.
        path: PathBuf,
    },
    /// `store.json` changed between the observed load and this save. Nothing was
    /// written; the external bytes are intact.
    #[error("{}: store manifest changed since it was loaded (expected {expected}, found {actual})", .path.display())]
    ConcurrentModification {
        /// Manifest path.
        path: PathBuf,
        /// Hash the caller observed at load time.
        expected: String,
        /// Hash found at save time, or `absent`/`directory`.
        actual: String,
    },
    /// The manifest could not be loaded, parsed, validated or written.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A store directory could not be listed while scanning for orphans.
    #[error(transparent)]
    Layout(#[from] StoreError),
    /// An orphan entry could not be hashed for import.
    #[error("{}: could not hash orphan content for import: {source}", .path.display())]
    OrphanHash {
        /// Offending path.
        path: PathBuf,
        /// Hashing failure.
        source: HashError,
    },
    /// The confirmation token does not match the plan the snapshot produces.
    #[error("reconcile confirmation token does not match the pending plan")]
    StaleToken,
    /// Canonical JSON rendering failed while hashing the reconcile plan.
    #[error(transparent)]
    CanonicalJson(#[from] CanonicalJsonError),
    /// JSON serialization failed while producing the reconcile token.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Classification of one manifest item against the bytes on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoreItemState {
    /// The content hash matches the manifest.
    Present,
    /// The path exists but its hash differs from the manifest, or the content could
    /// not be verified (unreadable file, symlink inside a skill folder).
    Modified,
    /// The path the manifest lists does not exist.
    Missing,
}

/// One manifest item plus what was found at its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreItemEntry {
    /// The manifest record.
    pub item: ManifestItem,
    /// Observed state.
    pub state: StoreItemState,
    /// Resolved absolute path the item was checked at.
    pub path: PathBuf,
}

/// Content on disk that no manifest item claims.
///
/// Orphans are reported, never deleted. [`reconcile_with`] can import them into the
/// manifest; the files themselves stay exactly where they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanEntry {
    /// Kind the orphan would import as.
    pub kind: ItemKind,
    /// Folder name (skill), file stem (rule) or `mcp.json` server slug.
    pub slug: String,
    /// Path on disk: the skill folder, the rule file, or `mcp.json` for a server.
    pub path: PathBuf,
}

/// State of `mcp.json` itself while reconciling server entries.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpFileState {
    /// `mcp.json` parsed and its `servers` object was read.
    Ok,
    /// `mcp.json` does not exist; every manifest MCP item is `Missing`.
    Missing,
    /// `mcp.json` exists but could not be read or parsed; MCP items are `Modified`
    /// because their definitions cannot be verified.
    Unreadable {
        /// Why the document could not be used.
        reason: String,
    },
}

/// Everything [`load_store`] observed in one store.
#[derive(Debug, Clone)]
pub struct StoreSnapshot {
    /// The store this snapshot describes.
    pub layout: StoreLayout,
    /// The parsed manifest.
    pub manifest: StoreManifest,
    /// Hash of the exact `store.json` bytes the manifest was read from. Present this
    /// to [`save_store`] or [`reconcile_with`] so a concurrent edit is refused.
    pub manifest_hash: ContentHash,
    /// Every manifest item with its observed state, in manifest order.
    pub items: Vec<StoreItemEntry>,
    /// On-disk content absent from the manifest.
    pub orphans: Vec<OrphanEntry>,
    /// `mcp.json` parse state; `Ok` or `Missing` for well-formed stores.
    pub mcp: McpFileState,
    /// Access the snapshot was taken with.
    pub access: StoreAccess,
}

impl StoreSnapshot {
    /// Items whose state is `Missing`.
    pub fn missing(&self) -> impl Iterator<Item = &StoreItemEntry> {
        self.items
            .iter()
            .filter(|entry| entry.state == StoreItemState::Missing)
    }

    /// Items whose state is `Modified`.
    pub fn modified(&self) -> impl Iterator<Item = &StoreItemEntry> {
        self.items
            .iter()
            .filter(|entry| entry.state == StoreItemState::Modified)
    }

    /// Whether the snapshot observed any drift at all.
    #[must_use]
    pub fn has_drift(&self) -> bool {
        self.items
            .iter()
            .any(|entry| entry.state != StoreItemState::Present)
            || !self.orphans.is_empty()
    }
}

/// A held `.state/store.lock` flock. Dropping it releases the lock.
#[derive(Debug)]
pub struct StoreLockGuard {
    file: File,
    path: PathBuf,
    shared: bool,
}

impl StoreLockGuard {
    /// The lock file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this guard holds a shared (read) lock.
    #[must_use]
    pub const fn is_shared(&self) -> bool {
        self.shared
    }
}

impl Drop for StoreLockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// `<store>/.state/store.lock`.
#[must_use]
pub fn store_lock_path(layout: &StoreLayout) -> PathBuf {
    layout.state_dir().join(STORE_LOCK_FILE)
}

fn lock_error(path: &Path, source: io::Error) -> StoreIoError {
    StoreIoError::Lock {
        path: path.to_path_buf(),
        source,
    }
}

/// Opens the lock file. Read-write access creates `.state/` and the file when
/// missing; read-only access opens an existing file read-only and reports `None`
/// when it does not exist (nothing to exclude, and creating it would write).
fn open_lock_file(layout: &StoreLayout, access: StoreAccess) -> Result<Option<File>, StoreIoError> {
    let path = store_lock_path(layout);
    if access.is_writable() {
        let state_dir = layout.state_dir();
        fs::create_dir_all(&state_dir).map_err(|source| lock_error(&state_dir, source))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| lock_error(&path, source))?;
        return Ok(Some(file));
    }
    match OpenOptions::new().read(true).open(&path) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(lock_error(&path, source)),
    }
}

/// Takes the store lock, retrying until `policy.wait` expires.
fn acquire(
    layout: &StoreLayout,
    shared: bool,
    access: StoreAccess,
    policy: LockPolicy,
) -> Result<Option<StoreLockGuard>, StoreIoError> {
    let Some(file) = open_lock_file(layout, access)? else {
        return Ok(None);
    };
    let path = store_lock_path(layout);
    let deadline = Instant::now() + policy.wait;
    loop {
        let attempt = if shared {
            FileExt::try_lock_shared(&file)
        } else {
            FileExt::try_lock(&file)
        };
        match attempt {
            Ok(()) => {
                return Ok(Some(StoreLockGuard { file, path, shared }));
            }
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(StoreIoError::LockTimeout {
                        path,
                        waited: policy.wait,
                    });
                }
                sleep(policy.poll);
            }
            Err(TryLockError::Error(source)) => return Err(lock_error(&path, source)),
        }
    }
}

/// Shared store lock for reads. `policy` bounds the wait; [`default_lock_policy`]
/// waits five seconds.
///
/// # Errors
/// [`StoreIoError::LockTimeout`] or [`StoreIoError::Lock`].
pub fn lock_shared(
    layout: &StoreLayout,
    access: StoreAccess,
    policy: LockPolicy,
) -> Result<Option<StoreLockGuard>, StoreIoError> {
    acquire(layout, true, access, policy)
}

/// Exclusive store lock for writes. `policy` bounds the wait; [`default_lock_policy`]
/// waits five seconds.
///
/// # Errors
/// [`StoreIoError::LockTimeout`] or [`StoreIoError::Lock`].
pub fn lock_exclusive(
    layout: &StoreLayout,
    access: StoreAccess,
    policy: LockPolicy,
) -> Result<Option<StoreLockGuard>, StoreIoError> {
    acquire(layout, false, access, policy)
}

/// Loads `store.json` under a shared store lock and classifies every item and every
/// orphan on disk.
///
/// The returned [`StoreSnapshot::manifest_hash`] is the observation [`save_store`]
/// requires; presenting anything else fails with [`StoreIoError::ConcurrentModification`].
///
/// # Errors
/// [`StoreIoError::LockTimeout`], [`StoreIoError::Lock`], [`StoreIoError::Manifest`]
/// (missing, unreadable, invalid or foreign-scope manifest) and [`StoreIoError::Layout`].
/// An unreadable `mcp.json` is reported on the snapshot, never a load failure.
pub fn load_store(
    layout: &StoreLayout,
    access: StoreAccess,
) -> Result<StoreSnapshot, StoreIoError> {
    load_store_with(layout, access, default_lock_policy())
}

/// Like [`load_store`] with an explicit lock policy (tests and short CLI waits).
///
/// # Errors
/// Same as [`load_store`].
pub fn load_store_with(
    layout: &StoreLayout,
    access: StoreAccess,
    policy: LockPolicy,
) -> Result<StoreSnapshot, StoreIoError> {
    let _guard = lock_shared(layout, access, policy)?;
    let loaded = StoreManifest::load(layout)?;
    classify(layout, access, loaded)
}

/// Writes `manifest` to `store.json` under an exclusive store lock.
///
/// `observed` must be the [`StoreSnapshot::manifest_hash`] the caller loaded. A
/// `store.json` changed by another process or editor fails with
/// [`StoreIoError::ConcurrentModification`] and the external bytes stay in place.
///
/// # Errors
/// [`StoreIoError::ReadOnly`], [`StoreIoError::LockTimeout`], [`StoreIoError::Lock`],
/// [`StoreIoError::ConcurrentModification`], [`StoreIoError::Manifest`] (validation or
/// pipeline failure).
pub fn save_store(
    layout: &StoreLayout,
    manifest: &StoreManifest,
    observed: &ContentHash,
    access: StoreAccess,
    backup: BackupPolicy,
) -> Result<SaveOutcome, StoreIoError> {
    save_store_with(
        layout,
        manifest,
        observed,
        access,
        backup,
        default_lock_policy(),
    )
}

/// Like [`save_store`] with an explicit lock policy.
///
/// # Errors
/// Same as [`save_store`].
pub fn save_store_with(
    layout: &StoreLayout,
    manifest: &StoreManifest,
    observed: &ContentHash,
    access: StoreAccess,
    backup: BackupPolicy,
    policy: LockPolicy,
) -> Result<SaveOutcome, StoreIoError> {
    if !access.is_writable() {
        return Err(StoreIoError::ReadOnly {
            path: layout.manifest_path(),
        });
    }
    let _guard = lock_exclusive(layout, access, policy)?;
    manifest
        .save(layout, ExpectedState::Hash(observed.to_string()), backup)
        .map_err(|error| match error {
            ManifestError::Write { path, source } => match source {
                FsError::ConcurrentModification {
                    expected, actual, ..
                } => StoreIoError::ConcurrentModification {
                    path,
                    expected,
                    actual,
                },
                source => StoreIoError::Manifest(ManifestError::Write { path, source }),
            },
            other => StoreIoError::Manifest(other),
        })
}

// ---------------------------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------------------------

/// The `servers` object of `mcp.json`, or the reason it could not be read.
enum McpServers {
    /// Parsed server map (empty when the file or `servers` key is absent).
    Map(BTreeMap<String, Value>),
    /// `mcp.json` does not exist.
    Missing,
    /// Present but unreadable or not the expected document shape.
    Unreadable { reason: String },
}

fn read_mcp_servers(path: &Path) -> McpServers {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return McpServers::Missing,
        Err(source) => {
            return McpServers::Unreadable {
                reason: source.to_string(),
            };
        }
    };
    let document: Value = match serde_json::from_slice(&bytes) {
        Ok(document) => document,
        Err(source) => {
            return McpServers::Unreadable {
                reason: source.to_string(),
            };
        }
    };
    match document.get("servers") {
        None | Some(Value::Null) => McpServers::Map(BTreeMap::new()),
        Some(Value::Object(map)) => McpServers::Map(
            map.iter()
                .map(|(slug, entry)| (slug.clone(), entry.clone()))
                .collect(),
        ),
        Some(_) => McpServers::Unreadable {
            reason: "`servers` is not an object".to_owned(),
        },
    }
}

/// Hashes the content an item kind stores at `resolved`, returning `None` when the
/// path is absent. `Err(())` means present but unverifiable.
fn observed_hash(item: &ManifestItem, resolved: &Path) -> Result<Option<ContentHash>, ()> {
    match item.kind {
        ItemKind::Skill => {
            if !resolved.exists() {
                return Ok(None);
            }
            let folder = if resolved.is_dir() {
                resolved.to_path_buf()
            } else {
                resolved.parent().unwrap_or(resolved).to_path_buf()
            };
            match hash_skill_folder(&folder) {
                Ok(hash) => Ok(Some(hash)),
                Err(HashError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                    Ok(None)
                }
                Err(_) => Err(()),
            }
        }
        ItemKind::Rule => match fs::read(resolved) {
            Ok(bytes) => Ok(Some(hash_rule_bytes(&bytes))),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        },
        ItemKind::McpServer => Ok(None),
    }
}

fn classify(
    layout: &StoreLayout,
    access: StoreAccess,
    loaded: LoadedManifest,
) -> Result<StoreSnapshot, StoreIoError> {
    let manifest = loaded.manifest;
    let mcp = read_mcp_servers(&layout.mcp_file());
    let mcp_state = match &mcp {
        McpServers::Map(_) => McpFileState::Ok,
        McpServers::Missing => McpFileState::Missing,
        McpServers::Unreadable { reason } => McpFileState::Unreadable {
            reason: reason.clone(),
        },
    };

    let mut items = Vec::with_capacity(manifest.items().len());
    let mut managed_skill_slugs = BTreeSet::new();
    let mut managed_rule_paths = BTreeSet::new();
    let mut managed_mcp_slugs = BTreeSet::new();

    for item in manifest.items() {
        // `resolve_item_path` only fails on an unreadable root or a symlinked
        // component; a failed resolution means the item cannot be verified, which is
        // `Modified` rather than `Missing` — the manifest path itself may be fine.
        let resolved = match resolve_item_path(layout.root(), &item.path) {
            Ok(path) => path,
            Err(_) => layout.root().join(item.path.as_str()),
        };
        let state = match item.kind {
            ItemKind::McpServer => {
                managed_mcp_slugs.insert(item.slug.as_str().to_owned());
                match &mcp {
                    McpServers::Map(servers) => match servers.get(item.slug.as_str()) {
                        None => StoreItemState::Missing,
                        Some(entry) => match hash_mcp_entry(entry) {
                            Ok(hash) if hash == item.hash => StoreItemState::Present,
                            _ => StoreItemState::Modified,
                        },
                    },
                    McpServers::Missing => StoreItemState::Missing,
                    McpServers::Unreadable { .. } => StoreItemState::Modified,
                }
            }
            ItemKind::Skill => {
                managed_skill_slugs.insert(item.slug.as_str().to_owned());
                match observed_hash(item, &resolved) {
                    Ok(Some(hash)) if hash == item.hash => StoreItemState::Present,
                    Ok(Some(_)) | Err(()) => StoreItemState::Modified,
                    Ok(None) => StoreItemState::Missing,
                }
            }
            ItemKind::Rule => {
                managed_rule_paths.insert(item.path.as_str().to_owned());
                match observed_hash(item, &resolved) {
                    Ok(Some(hash)) if hash == item.hash => StoreItemState::Present,
                    Ok(Some(_)) | Err(()) => StoreItemState::Modified,
                    Ok(None) => StoreItemState::Missing,
                }
            }
        };
        items.push(StoreItemEntry {
            item: item.clone(),
            state,
            path: resolved,
        });
    }

    let orphans = find_orphans(
        layout,
        &managed_skill_slugs,
        &managed_rule_paths,
        &managed_mcp_slugs,
        &mcp,
    );

    Ok(StoreSnapshot {
        layout: layout.clone(),
        manifest,
        manifest_hash: loaded.hash,
        items,
        orphans,
        mcp: mcp_state,
        access,
    })
}

/// Every entry under `skills/` and `rules/` plus `mcp.json` servers that no manifest
/// item claims. Missing directories simply contribute nothing.
fn find_orphans(
    layout: &StoreLayout,
    managed_skill_slugs: &BTreeSet<String>,
    managed_rule_paths: &BTreeSet<String>,
    managed_mcp_slugs: &BTreeSet<String>,
    mcp: &McpServers,
) -> Vec<OrphanEntry> {
    let mut orphans = Vec::new();

    // skills/: one orphan per top-level entry whose name is not a managed slug.
    if let Ok(entries) = fs::read_dir(layout.skills_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if managed_skill_slugs.contains(name) {
                continue;
            }
            orphans.push(OrphanEntry {
                kind: ItemKind::Skill,
                slug: name.to_owned(),
                path: entry.path(),
            });
        }
    }

    // rules/: recursive walk; every file whose `rules/<rel>` is not a managed path.
    collect_rule_orphans(
        &layout.rules_dir(),
        &layout.rules_dir(),
        managed_rule_paths,
        &mut orphans,
    );

    // mcp.json: server slugs without a manifest item.
    if let McpServers::Map(servers) = mcp {
        for slug in servers.keys() {
            if !managed_mcp_slugs.contains(slug) {
                orphans.push(OrphanEntry {
                    kind: ItemKind::McpServer,
                    slug: slug.clone(),
                    path: layout.mcp_file(),
                });
            }
        }
    }

    orphans.sort_by(|a, b| {
        (kind_order(a.kind), &a.slug, &a.path).cmp(&(kind_order(b.kind), &b.slug, &b.path))
    });
    orphans
}

fn kind_order(kind: ItemKind) -> u8 {
    match kind {
        ItemKind::Skill => 0,
        ItemKind::McpServer => 1,
        ItemKind::Rule => 2,
    }
}

fn collect_rule_orphans(
    root: &Path,
    directory: &Path,
    managed: &BTreeSet<String>,
    orphans: &mut Vec<OrphanEntry>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_rule_orphans(root, &path, managed, orphans);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let rel_unix = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if managed.contains(&format!("rules/{rel_unix}")) {
            continue;
        }
        let slug = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned();
        orphans.push(OrphanEntry {
            kind: ItemKind::Rule,
            slug,
            path,
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Reconcile
// ---------------------------------------------------------------------------------------------

/// One orphan [`reconcile_with`] would import into the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedImport {
    /// Item kind.
    pub kind: ItemKind,
    /// Validated slug the item will occupy.
    pub slug: Slug,
    /// Manifest-relative path (`skills/<slug>`, `rules/<file>` or `mcp.json`).
    pub path: StorePath,
    /// Content hash computed from the orphan bytes.
    pub hash: ContentHash,
    /// Path on disk the content was hashed from.
    pub source: PathBuf,
}

/// One `Missing` manifest item [`reconcile_with`] would drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDrop {
    /// The item record that would be removed.
    pub item: ManifestItem,
}

/// An orphan that cannot be imported; it stays on disk and is only reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedOrphan {
    /// The orphan that was skipped.
    pub orphan: OrphanEntry,
    /// Why it cannot become a manifest item.
    pub reason: String,
}

/// The confirmation a [`ReconcileMode::Apply`] must present.
///
/// Opaque on purpose: it is the hash of the pending plan plus the observed manifest
/// hash, so a plan computed against different store bytes cannot be confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileToken(pub ContentHash);

impl ReconcileToken {
    /// Constructs a token from a raw content hash.
    #[must_use]
    pub const fn from_hash(hash: ContentHash) -> Self {
        Self(hash)
    }
    /// The token text (SHA-256 hex).
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for ReconcileToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

/// What [`reconcile_with`] reports or applies.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReconcilePlan {
    /// Hash of the `store.json` bytes the plan was computed against.
    pub manifest_hash: ContentHash,
    /// Orphans that would be imported as new manifest items.
    pub imports: Vec<PlannedImport>,
    /// `Missing` items that would be dropped from the manifest.
    pub drops: Vec<PlannedDrop>,
    /// Orphans that cannot be imported; always left on disk.
    pub skipped: Vec<SkippedOrphan>,
    /// Confirmation required by [`ReconcileMode::Apply`].
    pub token: ReconcileToken,
}

impl ReconcilePlan {
    /// Whether the plan changes anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.imports.is_empty() && self.drops.is_empty()
    }
}

/// How [`reconcile_with`] behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReconcileMode {
    /// Report only (the default): compute the plan, write nothing.
    Report,
    /// Import orphans and drop missing entries, but only when `token` is the
    /// [`ReconcileToken`] the report for this snapshot produced.
    Apply {
        /// Confirmation token from a [`ReconcileMode::Report`] plan.
        token: ReconcileToken,
    },
}

fn plan_token(
    manifest_hash: &ContentHash,
    plan: &ReconcilePlan,
) -> Result<ReconcileToken, StoreIoError> {
    #[derive(serde::Serialize)]
    struct TokenDoc<'a> {
        manifest_hash: &'a str,
        imports: Vec<(&'a str, &'a str, &'a str, &'a str)>,
        drops: Vec<String>,
    }
    let doc = TokenDoc {
        manifest_hash: manifest_hash.as_str(),
        imports: plan
            .imports
            .iter()
            .map(|entry| {
                (
                    kind_str(entry.kind),
                    entry.slug.as_str(),
                    entry.path.as_str(),
                    entry.hash.as_str(),
                )
            })
            .collect(),
        drops: plan
            .drops
            .iter()
            .map(|drop| drop.item.id.to_string())
            .collect(),
    };
    let value = serde_json::to_value(doc)?;
    let text = canonical_json(&value)?;
    Ok(ReconcileToken(hash_bytes(text.as_bytes())))
}

fn kind_str(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Skill => "skill",
        ItemKind::McpServer => "mcp_server",
        ItemKind::Rule => "rule",
    }
}

/// Computes the reconcile plan for `snapshot` without writing anything.
///
/// Orphans are imported with a fresh [`ItemId`], a slug derived from their name and a
/// hash of their current bytes. `Missing` items are listed for removal. Orphans that
/// cannot become manifest items (unhashable content) land in
/// [`ReconcilePlan::skipped`] and stay on disk.
///
/// # Errors
/// [`StoreIoError::CanonicalJson`] only; per-orphan failures are skipped, not fatal.
pub fn reconcile(snapshot: &StoreSnapshot) -> Result<ReconcilePlan, StoreIoError> {
    reconcile_with(snapshot, ReconcileMode::Report).and_then(|outcome| match outcome {
        ReconcileOutcome::Plan(plan) => Ok(*plan),
        ReconcileOutcome::Applied { .. } => Err(StoreIoError::StaleToken),
    })
}

/// What [`reconcile_with`] returns.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReconcileOutcome {
    /// Report mode: the plan only, nothing was written.
    Plan(Box<ReconcilePlan>),
    /// Apply mode: the plan was confirmed and written.
    Applied {
        /// The confirmed plan.
        plan: Box<ReconcilePlan>,
        /// Result of the manifest save.
        outcome: SaveOutcome,
    },
}

/// Reconciles `snapshot` under `mode`.
///
/// [`ReconcileMode::Report`] (the default behaviour callers should start with)
/// returns the plan and writes nothing — `store.json` and `mcp.json` stay
/// byte-identical. [`ReconcileMode::Apply`] recomputes the plan, requires `token` to
/// match it, then writes the mutated manifest through [`save_store`]: orphan files are
/// never deleted, `Missing` entries stay in the manifest when the token is absent or
/// wrong, and a `store.json` changed since the snapshot fails with
/// [`StoreIoError::ConcurrentModification`].
///
/// # Errors
/// [`StoreIoError::StaleToken`], [`StoreIoError::ReadOnly`], lock and manifest errors
/// from [`save_store`], and [`StoreIoError::CanonicalJson`].
pub fn reconcile_with(
    snapshot: &StoreSnapshot,
    mode: ReconcileMode,
) -> Result<ReconcileOutcome, StoreIoError> {
    let plan = build_plan(snapshot)?;
    match mode {
        ReconcileMode::Report => Ok(ReconcileOutcome::Plan(Box::new(plan))),
        ReconcileMode::Apply { token } => {
            if token != plan.token {
                return Err(StoreIoError::StaleToken);
            }
            if !snapshot.access.is_writable() {
                return Err(StoreIoError::ReadOnly {
                    path: snapshot.layout.manifest_path(),
                });
            }
            let mut manifest = snapshot.manifest.clone();
            for drop in &plan.drops {
                manifest.remove_item(drop.item.id);
            }
            for import in &plan.imports {
                let item = ManifestItem::new(
                    ItemId::new(),
                    import.kind,
                    import.slug.clone(),
                    import.path.clone(),
                    import.hash.clone(),
                );
                manifest.insert_item(item).map_err(ManifestError::from)?;
            }
            let outcome = save_store(
                &snapshot.layout,
                &manifest,
                &snapshot.manifest_hash,
                snapshot.access,
                BackupPolicy::None,
            )?;
            Ok(ReconcileOutcome::Applied {
                plan: Box::new(plan),
                outcome,
            })
        }
    }
}

fn build_plan(snapshot: &StoreSnapshot) -> Result<ReconcilePlan, StoreIoError> {
    let mut imports = Vec::new();
    let mut skipped = Vec::new();
    for orphan in &snapshot.orphans {
        match plan_import(snapshot, orphan) {
            Ok(import) => imports.push(import),
            Err(reason) => skipped.push(SkippedOrphan {
                orphan: orphan.clone(),
                reason,
            }),
        }
    }
    imports.sort_by(|a, b| {
        (kind_order(a.kind), a.slug.as_str()).cmp(&(kind_order(b.kind), b.slug.as_str()))
    });
    skipped.sort_by(|a, b| a.orphan.path.cmp(&b.orphan.path));

    let drops: Vec<PlannedDrop> = snapshot
        .missing()
        .map(|entry| PlannedDrop {
            item: entry.item.clone(),
        })
        .collect();

    let mut plan = ReconcilePlan {
        manifest_hash: snapshot.manifest_hash.clone(),
        imports,
        drops,
        skipped,
        token: ReconcileToken(ContentHash::from_bytes(b"")),
    };
    plan.token = plan_token(&snapshot.manifest_hash, &plan)?;
    Ok(plan)
}

/// Hashes one orphan into a [`PlannedImport`]. Failures are reported as a reason so
/// the caller can list the orphan under `skipped` instead of aborting the plan.
fn plan_import(snapshot: &StoreSnapshot, orphan: &OrphanEntry) -> Result<PlannedImport, String> {
    let (slug_text, path_text, hash) = match orphan.kind {
        ItemKind::Skill => {
            if !orphan.path.is_dir() {
                return Err("orphan is not a directory".to_owned());
            }
            let hash = hash_skill_folder(&orphan.path).map_err(|source| source.to_string())?;
            (orphan.slug.clone(), format!("skills/{}", orphan.slug), hash)
        }
        ItemKind::Rule => {
            if !orphan.path.is_file() {
                return Err("orphan is not a file".to_owned());
            }
            let bytes = fs::read(&orphan.path).map_err(|source| source.to_string())?;
            let hash = hash_rule_bytes(&bytes);
            let rel = orphan
                .path
                .strip_prefix(snapshot.layout.rules_dir())
                .map_err(|_| "orphan is outside rules/".to_owned())?;
            let rel_unix = rel
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            (orphan.slug.clone(), format!("rules/{rel_unix}"), hash)
        }
        ItemKind::McpServer => {
            let McpServers::Map(servers) = read_mcp_servers(&orphan.path) else {
                return Err("mcp.json could not be re-read".to_owned());
            };
            let Some(entry) = servers.get(&orphan.slug) else {
                return Err("server entry vanished".to_owned());
            };
            let hash = hash_mcp_entry(entry).map_err(|source| source.to_string())?;
            (orphan.slug.clone(), "mcp.json".to_owned(), hash)
        }
    };
    let slug = Slug::parse(&slug_text).map_err(|source| source.to_string())?;
    let path = StorePath::parse(&path_text).map_err(|source| source.to_string())?;
    Ok(PlannedImport {
        kind: orphan.kind,
        slug,
        path,
        hash,
        source: orphan.path.clone(),
    })
}
