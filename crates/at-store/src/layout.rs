//! Canonical store layout and idempotent initialization.
//!
//! A store is one directory with a fixed set of entries: `skills/`, `rules/`, `mcp.json`,
//! `store.json`, `backups/`, `.state/` and, for the Project store only, `local/`.
//! [`init_store`] creates the entries that are missing and nothing else. An existing directory
//! is never emptied, rewritten or reordered; content AgenTuna does not own is reported in
//! [`InitOutcome::foreign`] and left byte-identical.
//!
//! The four scopes map to roots as follows, and only [`StoreLayout::for_scope`] may decide:
//! Global is [`crate::paths::AppPaths::global_store`], User is
//! [`crate::paths::AppPaths::user_store`], Project is
//! [`crate::paths::AppPaths::project_store`] and Local is `<project store>/local`.
//!
//! File writes go through the `at-fs` pipeline with [`at_fs::WriteMode::Create`] and
//! [`at_fs::ExpectedState::Absent`], so a file that appeared between the existence check and the
//! write is an error, never an overwrite.
//!
//! Unix only, like `at-fs`: store directories are created with mode 0700.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use at_fs::{WriteRequest, apply};
use at_model::skill::SKILL_FILE_NAME;
use at_model::{ChangeSet, ExpectedState, FileOp, ItemId, OperationKind, Scope};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::paths::AppPaths;

/// Version of every store document AgenTuna writes (`schema_version`).
pub const SCHEMA_VERSION: u32 = 1;

/// Directory holding one folder per skill.
pub const SKILLS_DIR: &str = "skills";

/// Directory holding rule and instruction files.
pub const RULES_DIR: &str = "rules";

/// Standard rule file inside [`RULES_DIR`].
pub const RULES_FILE: &str = "AGENTS.md";

/// Canonical MCP server document, content only (owned by the MCP stories).
pub const MCP_FILE: &str = "mcp.json";

/// The only item manifest of a store.
pub const MANIFEST_FILE: &str = "store.json";

/// Directory of operation backups.
pub const BACKUPS_DIR: &str = "backups";

/// Machine-local state: journals, locks, staging and temp files.
pub const STATE_DIR: &str = ".state";

/// Journals of in-flight multi-file operations.
///
/// `at-fs` writes one `<operation id>.journal` file here (`<store>/.state/journal`); startup
/// recovery scans this directory, so the name must match the writer exactly.
pub const JOURNALS_DIR: &str = "journal";

/// Advisory locks on AgenTuna's own files.
pub const LOCKS_DIR: &str = "locks";

/// Files staged before a journaled apply.
pub const STAGING_DIR: &str = "staging";

/// Scratch space that is safe to delete at any time.
pub const TMP_DIR: &str = "tmp";

/// Local scope content of a project store.
pub const LOCAL_DIR: &str = "local";

/// Git ignore rules of a project store.
///
/// Never written here: the project-store git contract owns its content. It is recognized so
/// that a project store carrying it is not reported as holding foreign content.
pub const GITIGNORE_FILE: &str = ".gitignore";

/// Mode of every directory [`init_store`] creates.
pub const STORE_DIR_MODE: u32 = 0o700;

/// Maximum number of characters in a [`Slug`].
pub const SLUG_MAX_CHARS: usize = 64;

/// Highest numeric suffix [`unique_slug`] tries before giving up.
pub const SLUG_MAX_SUFFIX: u32 = 9999;

/// Stable identity of one store directory: `store.json`'s `store_id`.
///
/// Generated once when the store is created and never regenerated; a fresh id means a fresh
/// store. Remote backups are filed under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoreId(Uuid);

impl StoreId {
    /// Allocates a fresh id. Ids from one process sort in creation order.
    // A `Default` impl would hide that every call allocates a new identity.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.hyphenated())
    }
}

/// Why a string is not a [`StoreId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreIdError {
    /// The text is not a UUID.
    #[error("store id `{0}` is not a UUID")]
    Invalid(String),
}

impl FromStr for StoreId {
    type Err = StoreIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text)
            .map(Self)
            .map_err(|_| StoreIdError::Invalid(text.to_owned()))
    }
}

impl serde::Serialize for StoreId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StoreId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The header of `store.json`: the fields the layout bootstrap reads and writes.
///
/// The document also carries `items` and `disabled`; those keys belong to the manifest stories
/// and are ignored here, so reading a header never rejects a manifest it does not understand
/// beyond [`SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreHeader {
    /// Document version. Only [`SCHEMA_VERSION`] is accepted.
    pub schema_version: u32,
    /// Identity of this store.
    pub store_id: StoreId,
    /// Scope whose content this store holds.
    pub scope: Scope,
    /// When the manifest last changed (RFC 3339, UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// The fixed layout of one store root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoreLayout {
    scope: Scope,
    root: PathBuf,
}

impl StoreLayout {
    /// Builds the layout of `root` for `scope`. Nothing is read or created.
    #[must_use]
    pub fn new(scope: Scope, root: impl Into<PathBuf>) -> Self {
        Self {
            scope,
            root: root.into(),
        }
    }

    /// Resolves the root of `scope` from [`AppPaths`]; the only place a store root is chosen.
    ///
    /// Global comes from [`AppPaths::global_store`], so `AGENTUNA_GLOBAL_STORE` and the
    /// `global_store_path` setting are honoured without a second rule here.
    ///
    /// # Errors
    /// [`StoreError::NoActiveProject`] for Project and Local when no project is selected.
    pub fn for_scope(paths: &AppPaths, scope: Scope) -> Result<Self, StoreError> {
        let root = match scope {
            Scope::Global => paths.global_store().to_path_buf(),
            Scope::User => paths.user_store().to_path_buf(),
            Scope::Project => paths
                .project_store()
                .ok_or(StoreError::NoActiveProject { scope })?
                .to_path_buf(),
            Scope::Local => paths
                .project_store()
                .ok_or(StoreError::NoActiveProject { scope })?
                .join(LOCAL_DIR),
        };
        Ok(Self::new(scope, root))
    }

    /// Store root: the directory holding every entry below.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Scope whose content this store holds.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// `skills/`.
    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join(SKILLS_DIR)
    }

    /// `skills/<slug>/`.
    #[must_use]
    pub fn skill_dir(&self, slug: &Slug) -> PathBuf {
        self.skills_dir().join(slug.as_str())
    }

    /// `skills/<slug>/SKILL.md`.
    #[must_use]
    pub fn skill_file(&self, slug: &Slug) -> PathBuf {
        self.skill_dir(slug).join(SKILL_FILE_NAME)
    }

    /// `rules/`.
    #[must_use]
    pub fn rules_dir(&self) -> PathBuf {
        self.root.join(RULES_DIR)
    }

    /// `rules/AGENTS.md`.
    #[must_use]
    pub fn rules_file(&self) -> PathBuf {
        self.rules_dir().join(RULES_FILE)
    }

    /// `mcp.json`.
    #[must_use]
    pub fn mcp_file(&self) -> PathBuf {
        self.root.join(MCP_FILE)
    }

    /// `store.json`.
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    /// `backups/`.
    #[must_use]
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join(BACKUPS_DIR)
    }

    /// `.state/`.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    /// `.state/journal/`.
    #[must_use]
    pub fn journals_dir(&self) -> PathBuf {
        self.state_dir().join(JOURNALS_DIR)
    }

    /// `.state/locks/`.
    #[must_use]
    pub fn locks_dir(&self) -> PathBuf {
        self.state_dir().join(LOCKS_DIR)
    }

    /// `.state/staging/`.
    #[must_use]
    pub fn staging_dir(&self) -> PathBuf {
        self.state_dir().join(STAGING_DIR)
    }

    /// `.state/tmp/`.
    #[must_use]
    pub fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join(TMP_DIR)
    }

    /// `local/`, the Local scope content of a Project store; `None` for every other scope.
    #[must_use]
    pub fn local_dir(&self) -> Option<PathBuf> {
        (self.scope == Scope::Project).then(|| self.root.join(LOCAL_DIR))
    }

    /// The Local scope store nested in this project store; `None` when this is not a project
    /// store. Its root is [`Self::local_dir`] and it has no `local/` of its own.
    #[must_use]
    pub fn local_scope(&self) -> Option<Self> {
        self.local_dir().map(|root| Self::new(Scope::Local, root))
    }

    /// Creates every missing entry of this store. Idempotent.
    ///
    /// # Errors
    /// [`StoreError::RootNotDirectory`] when the root exists as a file; a
    /// [`StoreError::PermissionDenied`] when the process may not write the path;
    /// [`StoreError::UnknownSchema`] or [`StoreError::ManifestInvalid`] when an existing
    /// `store.json` cannot be used.
    pub fn init(&self) -> Result<InitOutcome, StoreError> {
        init_at(self)
    }

    /// Whether `store.json` exists as a regular file, i.e. the scope has a store.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        fs::symlink_metadata(self.manifest_path()).is_ok_and(|metadata| metadata.is_file())
    }

    /// Plans the entries [`Self::init`] would create as one reviewed change set.
    ///
    /// Nothing is written. The plan follows the same rules as [`Self::init`]: only missing
    /// entries are created, existing directories and files are reported, and foreign entries
    /// are left byte-identical. `store.json` is validated before anything is planned.
    ///
    /// `.state/` itself is never a planned destination because the journaled apply creates it
    /// before the first operation; its child directories are planned with `parents: true`.
    ///
    /// # Errors
    /// See [`Self::init`], plus [`StoreError::Plan`] when the change set cannot be built.
    pub fn plan_init(&self) -> Result<InitPlan, StoreError> {
        plan_init_at(self)
    }

    /// Reads and validates `store.json` without modifying anything.
    ///
    /// # Errors
    /// [`StoreError::ManifestMissing`], [`StoreError::ManifestUnreadable`],
    /// [`StoreError::ManifestInvalid`], [`StoreError::UnknownSchema`] and
    /// [`StoreError::ScopeMismatch`].
    pub fn read_header(&self) -> Result<StoreHeader, StoreError> {
        let path = self.manifest_path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                return Err(StoreError::ManifestInvalid {
                    path,
                    reason: "not a regular file".to_owned(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::ManifestMissing { path });
            }
            Err(source) => return Err(StoreError::ManifestUnreadable { path, source }),
        }
        let bytes = fs::read(&path).map_err(|source| StoreError::ManifestUnreadable {
            path: path.clone(),
            source,
        })?;
        let header: StoreHeader =
            serde_json::from_slice(&bytes).map_err(|error| StoreError::ManifestInvalid {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnknownSchema {
                path,
                found: header.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        if header.scope != self.scope {
            return Err(StoreError::ScopeMismatch {
                path,
                expected: self.scope,
                found: header.scope,
            });
        }
        Ok(header)
    }

    /// Slugs that already occupy a folder under `skills/`, sorted.
    ///
    /// A missing `skills/` is an empty set.
    ///
    /// # Errors
    /// [`StoreError::ReadDir`] or [`StoreError::PermissionDenied`].
    pub fn skill_slugs(&self) -> Result<BTreeSet<String>, StoreError> {
        let dir = self.skills_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(error) => return Err(map_read_io(&dir, error)),
        };
        let mut slugs = BTreeSet::new();
        for entry in entries {
            let entry = entry.map_err(|error| map_read_io(&dir, error))?;
            let is_dir = entry
                .file_type()
                .map_err(|error| map_read_io(&dir, error))?
                .is_dir();
            if is_dir && let Some(name) = entry.file_name().to_str() {
                slugs.insert(name.to_owned());
            }
        }
        Ok(slugs)
    }

    /// Picks the folder name for a new skill: `requested`, or `requested-2`, `requested-3`, …
    /// until the name is free under `skills/`.
    ///
    /// Identity is not involved: two items may share a requested name because they never share
    /// a slug, and each keeps its own [`at_model::ItemId`].
    ///
    /// # Errors
    /// Any [`SlugError`] from validating `requested`, or [`SlugError::Exhausted`] when every
    /// suffix up to [`SLUG_MAX_SUFFIX`] is taken.
    pub fn allocate_skill_slug(&self, requested: &str) -> Result<Slug, SlugError> {
        let skills = self.skills_dir();
        unique_slug(requested, |candidate| {
            skills.join(candidate).symlink_metadata().is_ok()
        })
    }

    /// Every directory of the layout, parents before children.
    fn directories(&self) -> Vec<PathBuf> {
        let mut dirs = vec![
            self.skills_dir(),
            self.rules_dir(),
            self.backups_dir(),
            self.state_dir(),
            self.journals_dir(),
            self.locks_dir(),
            self.staging_dir(),
            self.tmp_dir(),
        ];
        dirs.extend(self.local_dir());
        dirs
    }

    /// Every entry the layout owns, files included.
    fn known_entries(&self) -> BTreeSet<PathBuf> {
        let mut entries: BTreeSet<PathBuf> = self.directories().into_iter().collect();
        entries.insert(self.mcp_file());
        entries.insert(self.manifest_path());
        entries.insert(self.root.join(GITIGNORE_FILE));
        entries
    }
}

/// What [`init_store`] created, found and refused to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// The layout that was initialized.
    pub layout: StoreLayout,
    /// Identity read from an existing `store.json`, or the id allocated by this call.
    pub store_id: StoreId,
    /// Document version of the manifest.
    pub schema_version: u32,
    /// Entries this call created, in layout order.
    pub created: Vec<PathBuf>,
    /// Entries that were already present, in layout order.
    pub existing: Vec<PathBuf>,
    /// Entries present in the root that AgenTuna does not own, sorted.
    ///
    /// They were not deleted, moved or rewritten. An entry the layout needs but could not
    /// create because a foreign file occupies its path is listed here too, so the store is
    /// usable except for that entry until the caller resolves it.
    pub foreign: Vec<PathBuf>,
}

impl InitOutcome {
    /// Whether the root holds content AgenTuna does not own (the "accepted with a warning"
    /// case). The warning text is the UI's to compose from [`Self::foreign`].
    #[must_use]
    pub fn has_foreign_entries(&self) -> bool {
        !self.foreign.is_empty()
    }

    /// Whether this call created anything.
    #[must_use]
    pub fn created_anything(&self) -> bool {
        !self.created.is_empty()
    }
}

/// A reviewed plan that creates the missing entries of one store through `at-fs`.
///
/// Built by [`StoreLayout::plan_init`] from the same rules as [`StoreLayout::init`]. The
/// caller previews [`Self::change_set`] and applies that reviewed plan, so a file or
/// directory that appeared in the meantime is refused instead of overwritten.
#[derive(Debug, Clone)]
pub struct InitPlan {
    /// Reviewed multi-file change set with absolute destinations, ready for
    /// `at_fs::apply_with_roots`.
    pub change_set: ChangeSet,
    /// Identity read from an existing `store.json`, or the id this plan writes.
    pub store_id: StoreId,
    /// Document version of the manifest.
    pub schema_version: u32,
    /// Entries the plan creates, relative to the store root.
    pub created: Vec<PathBuf>,
    /// Entries that were already present, relative to the store root.
    pub existing: Vec<PathBuf>,
    /// Foreign entries left byte-identical, relative to the store root.
    pub foreign: Vec<PathBuf>,
    /// Whether applying the plan creates the store root itself.
    pub creates_root: bool,
    /// Whether applying the plan creates `.state/` as a parent of planned state directories.
    pub creates_state_dir: bool,
}

/// Why a store could not be laid out or read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The chosen root exists but is not a directory.
    #[error("{}: store root is not a directory", .path.display())]
    RootNotDirectory {
        /// The offending root.
        path: PathBuf,
    },
    /// A store directory could not be created.
    #[error("{}: could not create store directory: {source}", .path.display())]
    CreateDir {
        /// Directory being created.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A store directory could not be listed.
    #[error("{}: could not read store directory: {source}", .path.display())]
    ReadDir {
        /// Directory being listed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The operating system denied access. AgenTuna reports this instead of silently using
    /// another scope; a Global store only exists when the process may write its admin path.
    #[error("{}: permission denied; AgenTuna does not fall back to another store", .path.display())]
    PermissionDenied {
        /// Path that could not be written.
        path: PathBuf,
    },
    /// `store.json` is expected by the caller but absent.
    #[error("{}: store manifest is missing", .path.display())]
    ManifestMissing {
        /// Expected manifest path.
        path: PathBuf,
    },
    /// `store.json` exists but could not be read.
    #[error("{}: could not read store manifest: {source}", .path.display())]
    ManifestUnreadable {
        /// Manifest path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A store document could not be serialized.
    #[error("{}: could not serialize store document: {source}", .path.display())]
    Serialize {
        /// Document path the bytes were meant for.
        path: PathBuf,
        /// Serializer error.
        source: serde_json::Error,
    },
    /// `store.json` is not the document AgenTuna understands.
    #[error("{}: store manifest is invalid ({reason})", .path.display())]
    ManifestInvalid {
        /// Manifest path.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// `store.json` declares a document version newer than this build supports.
    #[error(
        "{}: store schema version {found} is newer than this AgenTuna supports (version {supported}); update AgenTuna to open this store",
        .path.display()
    )]
    UnknownSchema {
        /// Manifest path.
        path: PathBuf,
        /// Version found in the file.
        found: u32,
        /// Highest version this build understands.
        supported: u32,
    },
    /// `store.json` belongs to a different scope than the root being used.
    #[error(
        "{}: store holds scope `{found}` but scope `{expected}` was requested",
        .path.display()
    )]
    ScopeMismatch {
        /// Manifest path.
        path: PathBuf,
        /// Scope the caller asked for.
        expected: Scope,
        /// Scope recorded in the manifest.
        found: Scope,
    },
    /// A project-scoped store was requested without a selected project.
    #[error("scope `{scope}` needs an active project; no project store was resolved")]
    NoActiveProject {
        /// Requested scope (Project or Local).
        scope: Scope,
    },
    /// The store setup change set could not be built.
    #[error("{}: could not plan store setup: {reason}", .path.display())]
    Plan {
        /// Store root being planned.
        path: PathBuf,
        /// Why the plan is invalid.
        reason: String,
    },
    /// A guarded file write failed.
    #[error("{}: could not write store file: {source}", .path.display())]
    Write {
        /// File being written.
        path: PathBuf,
        /// Underlying pipeline error.
        source: at_fs::FsError,
    },
}

/// Lays out the store of `scope` at `root`, creating only what is missing.
///
/// A second call creates nothing, changes no byte and returns the same `store_id`.
///
/// # Errors
/// See [`StoreLayout::init`].
pub fn init_store(scope: Scope, root: impl Into<PathBuf>) -> Result<InitOutcome, StoreError> {
    StoreLayout::new(scope, root).init()
}

/// Builds the reviewed change set for [`StoreLayout::plan_init`].
fn plan_init_at(layout: &StoreLayout) -> Result<InitPlan, StoreError> {
    let root = layout.root();
    let root_exists = match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => true,
        Ok(_) => {
            return Err(StoreError::RootNotDirectory {
                path: root.to_path_buf(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling symlink reports `NotFound` but is still something at the path.
            if fs::symlink_metadata(root).is_ok() {
                return Err(StoreError::RootNotDirectory {
                    path: root.to_path_buf(),
                });
            }
            false
        }
        Err(error) => return Err(map_read_io(root, error)),
    };
    let manifest_path = layout.manifest_path();
    // Validate a store written by another AgenTuna before planning to extend it.
    let existing_header = match entry_state(&manifest_path)? {
        EntryState::File => Some(layout.read_header()?),
        EntryState::Absent => None,
        EntryState::Dir | EntryState::Other => {
            return Err(StoreError::ManifestInvalid {
                path: manifest_path,
                reason: "not a regular file".to_owned(),
            });
        }
    };
    let state_dir = layout.state_dir();
    let locks_dir = layout.locks_dir();
    let journals_dir = layout.journals_dir();
    let mut created = Vec::new();
    let mut existing = Vec::new();
    let mut foreign = BTreeSet::new();
    let mut ops = Vec::new();
    let mut creates_state_dir = false;
    for dir in layout.directories() {
        if dir == state_dir || dir == locks_dir || dir == journals_dir {
            // The journaled apply writes its journal into `.state/journal` and takes its lock in
            // `.state/locks` before the first operation, so it creates those directories (and
            // `.state/` above them) itself: planning them again would fail with `File exists`.
            // Other `.state` children are planned with `parents: true`.
            match entry_state(&dir)? {
                EntryState::Dir => existing.push(relative_path(root, &dir)),
                EntryState::Absent => {}
                EntryState::File | EntryState::Other => {
                    foreign.insert(relative_path(root, &dir));
                }
            }
            continue;
        }
        match entry_state(&dir)? {
            EntryState::Absent => {
                if dir.starts_with(&state_dir) {
                    creates_state_dir = true;
                }
                ops.push(FileOp::Mkdir {
                    path: dir.clone(),
                    mode: Some(STORE_DIR_MODE),
                    parents: true,
                });
                created.push(relative_path(root, &dir));
            }
            EntryState::Dir => existing.push(relative_path(root, &dir)),
            EntryState::File | EntryState::Other => {
                foreign.insert(relative_path(root, &dir));
            }
        }
    }
    let mcp_file = layout.mcp_file();
    match entry_state(&mcp_file)? {
        EntryState::Absent => {
            let bytes = empty_mcp_document(&mcp_file)?;
            ops.push(FileOp::Write {
                path: mcp_file.clone(),
                bytes,
                expected: ExpectedState::Absent,
                follow_symlink: false,
                backup: false,
            });
            created.push(relative_path(root, &mcp_file));
        }
        EntryState::File => existing.push(relative_path(root, &mcp_file)),
        EntryState::Dir | EntryState::Other => {
            foreign.insert(relative_path(root, &mcp_file));
        }
    }
    let (store_id, schema_version) = match existing_header {
        Some(header) => {
            existing.push(relative_path(root, &manifest_path));
            (header.store_id, header.schema_version)
        }
        None => {
            let header = StoreHeader {
                schema_version: SCHEMA_VERSION,
                store_id: StoreId::new(),
                scope: layout.scope(),
                updated_at: OffsetDateTime::now_utc(),
            };
            let document = manifest_document(&header, &manifest_path)?;
            ops.push(FileOp::Write {
                path: manifest_path.clone(),
                bytes: document,
                expected: ExpectedState::Absent,
                follow_symlink: false,
                backup: false,
            });
            created.push(relative_path(root, &manifest_path));
            (header.store_id, header.schema_version)
        }
    };
    if root_exists {
        let known = layout.known_entries();
        let entries = fs::read_dir(root).map_err(|error| map_read_io(root, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| map_read_io(root, error))?;
            let path = entry.path();
            if !known.contains(&path) {
                foreign.insert(relative_path(root, &path));
            }
        }
    }
    // The operations log has no store-setup kind; a Global store write is exactly that kind
    // after the authorization prompt, and another scope's setup is recorded as a settings
    // change so the operation stays undoable through `operation_undo`.
    let operation_kind = if layout.scope() == Scope::Global {
        "global_write"
    } else {
        "settings"
    };
    let operation_kind =
        OperationKind::parse(operation_kind).map_err(|error| StoreError::Plan {
            path: root.to_path_buf(),
            reason: error.to_string(),
        })?;
    let change_set = ChangeSet {
        id: ItemId::new().to_string(),
        preview_id: String::new(),
        operation_kind,
        scope: Some(layout.scope()),
        project_id: None,
        summary: format!("Set up the {} store", layout.scope()),
        ops,
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
    };
    Ok(InitPlan {
        change_set,
        store_id,
        schema_version,
        created,
        existing,
        foreign: foreign.into_iter().collect(),
        creates_root: !root_exists,
        creates_state_dir,
    })
}

fn relative_path(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

/// What is at a path, without following symlinks.
enum EntryState {
    /// Nothing.
    Absent,
    /// A regular file.
    File,
    /// A real directory (never a symlink to one).
    Dir,
    /// A symlink, device, socket or fifo.
    Other,
}

fn entry_state(path: &Path) -> Result<EntryState, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(EntryState::File),
        Ok(meta) if meta.is_dir() => Ok(EntryState::Dir),
        Ok(_) => Ok(EntryState::Other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(EntryState::Absent),
        Err(error) => Err(map_read_io(path, error)),
    }
}

fn init_at(layout: &StoreLayout) -> Result<InitOutcome, StoreError> {
    let root = layout.root().to_path_buf();
    let mut created = Vec::new();
    let mut existing = Vec::new();
    let mut foreign = BTreeSet::new();

    ensure_root(&root, &mut created, &mut existing)?;

    // The manifest is validated before anything is written: a store written by a newer
    // AgenTuna is never extended with files that version may not use.
    let manifest_path = layout.manifest_path();
    let existing_header = match entry_state(&manifest_path)? {
        EntryState::File => Some(layout.read_header()?),
        EntryState::Absent => None,
        EntryState::Dir | EntryState::Other => {
            return Err(StoreError::ManifestInvalid {
                path: manifest_path,
                reason: "not a regular file".to_owned(),
            });
        }
    };

    for dir in layout.directories() {
        match entry_state(&dir)? {
            EntryState::Absent => {
                create_dir(&dir)?;
                created.push(dir);
            }
            EntryState::Dir => existing.push(dir),
            // A file or symlink where the layout needs a directory: report, never replace.
            EntryState::File | EntryState::Other => {
                foreign.insert(dir);
            }
        }
    }

    let mcp_file = layout.mcp_file();
    match entry_state(&mcp_file)? {
        EntryState::Absent => {
            let document = empty_mcp_document(&mcp_file)?;
            write_new_file(&mcp_file, &document)?;
            created.push(mcp_file);
        }
        EntryState::File => existing.push(mcp_file),
        EntryState::Dir | EntryState::Other => {
            foreign.insert(mcp_file);
        }
    }

    let (store_id, schema_version) = match existing_header {
        Some(header) => {
            existing.push(manifest_path);
            (header.store_id, header.schema_version)
        }
        None => {
            let header = StoreHeader {
                schema_version: SCHEMA_VERSION,
                store_id: StoreId::new(),
                scope: layout.scope(),
                updated_at: OffsetDateTime::now_utc(),
            };
            let document = manifest_document(&header, &manifest_path)?;
            write_new_file(&manifest_path, &document)?;
            created.push(manifest_path);
            (header.store_id, header.schema_version)
        }
    };

    let known = layout.known_entries();
    let entries = fs::read_dir(&root).map_err(|error| map_read_io(&root, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| map_read_io(&root, error))?;
        let path = entry.path();
        if !known.contains(&path) {
            foreign.insert(path);
        }
    }

    Ok(InitOutcome {
        layout: layout.clone(),
        store_id,
        schema_version,
        created,
        existing,
        foreign: foreign.into_iter().collect(),
    })
}

fn ensure_root(
    root: &Path,
    created: &mut Vec<PathBuf>,
    existing: &mut Vec<PathBuf>,
) -> Result<(), StoreError> {
    match fs::metadata(root) {
        Ok(meta) if meta.is_dir() => {
            existing.push(root.to_path_buf());
            Ok(())
        }
        Ok(_) => Err(StoreError::RootNotDirectory {
            path: root.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling symlink reports `NotFound` but is still something at the path.
            if fs::symlink_metadata(root).is_ok() {
                return Err(StoreError::RootNotDirectory {
                    path: root.to_path_buf(),
                });
            }
            if let Some(parent) = root.parent().filter(|path| !path.as_os_str().is_empty()) {
                // Ancestors outside the store are not AgenTuna's entries: they keep the mode
                // the platform gives them, while the store root itself is 0700.
                at_fs::ensure_dir(parent).map_err(|source| match source {
                    at_fs::FsError::Io { source, .. }
                        if source.kind() == io::ErrorKind::PermissionDenied =>
                    {
                        StoreError::PermissionDenied {
                            path: parent.to_path_buf(),
                        }
                    }
                    at_fs::FsError::Io { source, .. } => StoreError::CreateDir {
                        path: parent.to_path_buf(),
                        source,
                    },
                    other => StoreError::CreateDir {
                        path: parent.to_path_buf(),
                        source: io::Error::other(other.to_string()),
                    },
                })?;
            }
            create_dir(root)?;
            created.push(root.to_path_buf());
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::PermissionDenied => {
            Err(StoreError::PermissionDenied {
                path: root.to_path_buf(),
            })
        }
        Err(source) => Err(StoreError::CreateDir {
            path: root.to_path_buf(),
            source,
        }),
    }
}

fn create_dir(path: &Path) -> Result<(), StoreError> {
    fs::DirBuilder::new()
        .mode(STORE_DIR_MODE)
        .create(path)
        .map_err(|source| {
            if source.kind() == io::ErrorKind::PermissionDenied {
                StoreError::PermissionDenied {
                    path: path.to_path_buf(),
                }
            } else {
                StoreError::CreateDir {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })
}

fn map_read_io(path: &Path, source: io::Error) -> StoreError {
    if source.kind() == io::ErrorKind::PermissionDenied {
        StoreError::PermissionDenied {
            path: path.to_path_buf(),
        }
    } else {
        StoreError::ReadDir {
            path: path.to_path_buf(),
            source,
        }
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    apply(&WriteRequest::create(path, bytes.to_vec())).map_err(|source| match source {
        at_fs::FsError::PermissionDenied { .. } => StoreError::PermissionDenied {
            path: path.to_path_buf(),
        },
        source => StoreError::Write {
            path: path.to_path_buf(),
            source,
        },
    })?;
    Ok(())
}

/// The canonical empty `mcp.json`: content only, no item identity (see the MCP stories).
fn empty_mcp_document(path: &Path) -> Result<Vec<u8>, StoreError> {
    #[derive(Serialize)]
    struct Seed<'a> {
        schema_version: u32,
        servers: &'a BTreeMap<String, serde_json::Value>,
    }
    let servers = BTreeMap::new();
    let seed = Seed {
        schema_version: SCHEMA_VERSION,
        servers: &servers,
    };
    canonical_json(&seed, path)
}

/// A new `store.json`: header plus the empty manifest keys, in one fixed order.
fn manifest_document(header: &StoreHeader, path: &Path) -> Result<Vec<u8>, StoreError> {
    #[derive(Serialize)]
    struct Seed<'a> {
        schema_version: u32,
        store_id: &'a StoreId,
        scope: Scope,
        #[serde(with = "time::serde::rfc3339")]
        updated_at: OffsetDateTime,
        items: &'a [serde_json::Value],
        disabled: &'a [serde_json::Value],
    }
    let seed = Seed {
        schema_version: header.schema_version,
        store_id: &header.store_id,
        scope: header.scope,
        updated_at: header.updated_at,
        items: &[],
        disabled: &[],
    };
    canonical_json(&seed, path)
}

fn canonical_json<T: Serialize>(value: &T, path: &Path) -> Result<Vec<u8>, StoreError> {
    let mut text = serde_json::to_string_pretty(value).map_err(|source| StoreError::Serialize {
        path: path.to_path_buf(),
        source,
    })?;
    text.push('\n');
    Ok(text.into_bytes())
}

/// A validated folder name for a skill or MCP server.
///
/// Lowercase, at most [`SLUG_MAX_CHARS`] characters, drawn from `a-z`, `0-9`, `-`, `_` and `.`;
/// no path separator, no leading `.` or `-`, no trailing `.`. A slug is a name, never identity:
/// the [`at_model::ItemId`] is what survives a rename.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slug(String);

impl Slug {
    /// Validates `value`.
    ///
    /// # Errors
    /// The first rule the value breaks, in this order: empty, length, leading `.`/`-`,
    /// uppercase, path separator, trailing `.`, any other character.
    pub fn parse(value: &str) -> Result<Self, SlugError> {
        if value.is_empty() {
            return Err(SlugError::Empty);
        }
        let len = value.chars().count();
        if len > SLUG_MAX_CHARS {
            return Err(SlugError::TooLong {
                slug: value.to_owned(),
                len,
                limit: SLUG_MAX_CHARS,
            });
        }
        if let Some(character) = value.chars().next().filter(|c| matches!(c, '.' | '-')) {
            return Err(SlugError::LeadingInvalid {
                slug: value.to_owned(),
                character,
            });
        }
        if let Some(character) = value.chars().find(|c| c.is_uppercase()) {
            return Err(SlugError::Uppercase {
                slug: value.to_owned(),
                character,
            });
        }
        if let Some(character) = value.chars().find(|c| matches!(c, '/' | '\\' | ':' | '\0')) {
            return Err(SlugError::Separator {
                slug: value.to_owned(),
                character,
            });
        }
        if value.ends_with('.') {
            return Err(SlugError::TrailingDot {
                slug: value.to_owned(),
            });
        }
        if let Some(character) = value.chars().find(|c| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        }) {
            return Err(SlugError::InvalidCharacter {
                slug: value.to_owned(),
                character,
            });
        }
        Ok(Self(value.to_owned()))
    }

    /// The slug text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Slug {
    type Err = SlugError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl Serialize for Slug {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Slug {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Why a string is not a [`Slug`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SlugError {
    /// The slug is empty.
    #[error("slug must not be empty")]
    Empty,
    /// The slug is longer than [`SLUG_MAX_CHARS`].
    #[error("slug `{slug}` is {len} characters; the limit is {limit}")]
    TooLong {
        /// Rejected slug.
        slug: String,
        /// Observed length in characters.
        len: usize,
        /// Limit.
        limit: usize,
    },
    /// The slug starts with a dot (`hidden folder`) or a hyphen (`looks like a flag`).
    #[error("slug `{slug}` must not start with `{character}`")]
    LeadingInvalid {
        /// Rejected slug.
        slug: String,
        /// The offending first character.
        character: char,
    },
    /// The slug contains an uppercase character.
    #[error("slug `{slug}` contains uppercase `{character}`")]
    Uppercase {
        /// Rejected slug.
        slug: String,
        /// The first offending character.
        character: char,
    },
    /// The slug contains a path separator or a NUL.
    #[error("slug `{slug}` contains the separator `{character:?}`")]
    Separator {
        /// Rejected slug.
        slug: String,
        /// The offending character.
        character: char,
    },
    /// The slug ends with a dot, which Windows silently strips.
    #[error("slug `{slug}` must not end with a dot")]
    TrailingDot {
        /// Rejected slug.
        slug: String,
    },
    /// The slug contains a character outside `a-z`, `0-9`, `-`, `_` and `.`.
    #[error("slug `{slug}` contains the unsupported character `{character:?}`")]
    InvalidCharacter {
        /// Rejected slug.
        slug: String,
        /// The first offending character.
        character: char,
    },
    /// Every numeric suffix up to [`SLUG_MAX_SUFFIX`] is taken.
    #[error("slug `{base}` has no free numeric suffix up to {SLUG_MAX_SUFFIX}")]
    Exhausted {
        /// The requested base slug.
        base: String,
    },
}

/// Picks `base`, or the first free `base-<n>` with `n` from 2 to [`SLUG_MAX_SUFFIX`].
///
/// The suffixed name is truncated to [`SLUG_MAX_CHARS`] so it stays a valid [`Slug`].
///
/// # Errors
/// Any [`SlugError`] from validating `base`, or [`SlugError::Exhausted`].
pub fn unique_slug(base: &str, taken: impl Fn(&str) -> bool) -> Result<Slug, SlugError> {
    let slug = Slug::parse(base)?;
    if !taken(slug.as_str()) {
        return Ok(slug);
    }
    for suffix_number in 2..=SLUG_MAX_SUFFIX {
        let suffix = format!("-{suffix_number}");
        let keep = SLUG_MAX_CHARS.saturating_sub(suffix.chars().count());
        let mut stem = String::with_capacity(base.len());
        stem.extend(base.chars().take(keep));
        let trimmed = stem.trim_end_matches(['.', '-']);
        let stem = &stem[..trimmed.len()];
        let candidate = format!("{stem}{suffix}");
        if !taken(&candidate) {
            return Ok(Slug(candidate));
        }
    }
    Err(SlugError::Exhausted {
        base: base.to_owned(),
    })
}
