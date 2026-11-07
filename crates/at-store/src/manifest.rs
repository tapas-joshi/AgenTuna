//! The store manifest (`store.json`) and the content hashes it records.
//!
//! `store.json` is the only item manifest of a store (contract decision 3). It carries the
//! stable item identities, the per-scope disable tombstones (decision 16) and the content
//! hash of every item. `mcp.json` is content only and never carries an item id.
//!
//! Serialization is deterministic: items sort by kind then slug, tombstones sort by kind
//! then item id, the document is pretty-printed with two spaces and ends in one newline.
//! Two saves of an unchanged store therefore produce byte-identical files. Writes go through
//! [`at_fs::apply`], so a concurrent writer is refused instead of overwritten.
//!
//! Paths stored in the manifest are untrusted input. They are validated lexically when the
//! document is parsed ([`StorePath`]) and checked again against the filesystem before use
//! ([`resolve_item_path`]): no absolute path, no `..`, no symlinked component, no escape from
//! the store root.
//!
//! ```
//! use at_model::{ItemId, ItemKind, Scope};
//! use at_store::layout::{Slug, StoreId};
//! use at_store::manifest::{ContentHash, ManifestItem, StoreManifest, StorePath};
//!
//! let mut manifest = StoreManifest::new(Scope::User, StoreId::new());
//! manifest
//!     .insert_item(ManifestItem::new(
//!         ItemId::new(),
//!         ItemKind::Skill,
//!         Slug::parse("my-skill")?,
//!         StorePath::parse("skills/my-skill")?,
//!         ContentHash::parse(&"0".repeat(64))?,
//!     ))
//!     .expect("valid item");
//! assert_eq!(manifest.items().len(), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use at_fs::{
    BackupMeta, BackupPolicy, ExpectedState, WriteMode, WriteOutcome, WriteRequest, apply,
    sha256_hex,
};
use at_model::{AgentId, ItemId, ItemKind, Scope};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::layout::{
    MANIFEST_FILE, MCP_FILE, RULES_DIR, SCHEMA_VERSION, SKILLS_DIR, Slug, StoreHeader, StoreId,
    StoreLayout,
};

/// Largest `store.json` this build reads or writes (4 MiB).
pub const MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Largest number of items a manifest may hold.
pub const MANIFEST_MAX_ITEMS: usize = 10_000;

/// Largest number of disable tombstones a manifest may hold.
pub const MANIFEST_MAX_DISABLED: usize = 10_000;

/// Largest display name of an item, in characters.
pub const MANIFEST_MAX_NAME_CHARS: usize = 256;

/// Largest source-metadata string (URL, ref, subpath, version), in characters.
pub const MANIFEST_MAX_SOURCE_FIELD_CHARS: usize = 512;

/// Largest item path in the manifest, in characters.
pub const ITEM_PATH_MAX_CHARS: usize = 1024;

/// Largest single file inside a hashed skill folder.
pub const SKILL_HASH_MAX_FILE_BYTES: u64 = at_fs::MAX_FILE_BYTES;

/// Largest total content inside a hashed skill folder (256 MiB).
pub const SKILL_HASH_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Largest number of files inside a hashed skill folder.
pub const SKILL_HASH_MAX_FILES: usize = 4096;

/// Largest directory nesting a hashed skill folder may use, relative to its root.
pub const SKILL_HASH_MAX_DEPTH: usize = 32;

/// Top-level keys that identify an item; a server definition in `mcp.json` never carries one.
pub const MCP_IDENTITY_KEYS: &[&str] = &["id", "item_id", "hash", "path", "kind", "scope"];

/// Short, bounded rendering of untrusted text for an error message.
fn preview(text: &str) -> String {
    const LIMIT: usize = 80;
    let mut out: String = text.chars().take(LIMIT).collect();
    if text.chars().count() > LIMIT {
        out.push('…');
    }
    out
}

/// SHA-256 of an item's canonical content.
///
/// Always 64 lowercase hexadecimal characters; anything else is rejected, so a corrupt
/// manifest cannot smuggle an arbitrary string into hash comparisons.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(String);

impl ContentHash {
    /// Parses a hexadecimal SHA-256 digest, as written by [`Self::from_bytes`].
    ///
    /// # Errors
    /// [`ContentHashError`] when the text is not 64 lowercase hexadecimal characters.
    pub fn parse(text: &str) -> Result<Self, ContentHashError> {
        if text.len() != 64 {
            return Err(ContentHashError::NotSha256 {
                value: preview(text),
            });
        }
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ContentHashError::NotLowercaseHex {
                value: preview(text),
            });
        }
        Ok(Self(text.to_owned()))
    }

    /// Hashes `bytes` with SHA-256.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(sha256_hex(bytes))
    }

    /// The digest as 64 lowercase hexadecimal characters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ContentHash {
    type Err = ContentHashError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Why a string is not a [`ContentHash`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ContentHashError {
    /// The text is not 64 characters long.
    #[error("content hash `{value}` is not a SHA-256 digest (64 characters)")]
    NotSha256 {
        /// Rejected text, truncated for display.
        value: String,
    },
    /// The text is 64 characters but not lowercase hexadecimal.
    #[error("content hash `{value}` must be lowercase hexadecimal")]
    NotLowercaseHex {
        /// Rejected text, truncated for display.
        value: String,
    },
}

/// A path stored in the manifest, relative to the store root.
///
/// Validation is lexical and deliberately strict: no absolute path, no `.` or `..`
/// component, no empty component, no backslash, colon or control character, and a bounded
/// length. The path is still untrusted when the filesystem is involved; use
/// [`resolve_item_path`] before reading it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StorePath(String);

impl StorePath {
    /// Validates `text` as a store-relative path.
    ///
    /// # Errors
    /// [`StorePathError`] describing the first rule the text breaks.
    pub fn parse(text: &str) -> Result<Self, StorePathError> {
        if text.is_empty() {
            return Err(StorePathError::Empty);
        }
        let chars = text.chars().count();
        if chars > ITEM_PATH_MAX_CHARS {
            return Err(StorePathError::TooLong {
                path: preview(text),
                chars,
                limit: ITEM_PATH_MAX_CHARS,
            });
        }
        if text.starts_with('/') {
            return Err(StorePathError::Absolute {
                path: preview(text),
            });
        }
        if let Some(character) = text
            .chars()
            .find(|c| matches!(c, '\\' | ':' | '\0') || c.is_control())
        {
            return Err(StorePathError::InvalidCharacter {
                path: preview(text),
                character,
            });
        }
        for part in text.split('/') {
            if part.is_empty() {
                return Err(StorePathError::EmptyComponent {
                    path: preview(text),
                });
            }
            if part == "." {
                return Err(StorePathError::CurrentComponent {
                    path: preview(text),
                });
            }
            if part == ".." {
                return Err(StorePathError::ParentComponent {
                    path: preview(text),
                });
            }
            if part.len() > 255 {
                return Err(StorePathError::ComponentTooLong {
                    path: preview(text),
                    limit: 255,
                });
            }
        }
        Ok(Self(text.to_owned()))
    }

    /// The path text, always with `/` separators.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The validated path components, outermost first.
    pub fn components(&self) -> impl Iterator<Item = &str> + '_ {
        self.0.split('/')
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for StorePath {
    type Err = StorePathError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl Serialize for StorePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for StorePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Why a string is not a [`StorePath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StorePathError {
    /// The path is empty.
    #[error("item path must not be empty")]
    Empty,
    /// The path is longer than [`ITEM_PATH_MAX_CHARS`].
    #[error("item path `{path}` is {chars} characters; the limit is {limit}")]
    TooLong {
        /// Rejected path, truncated for display.
        path: String,
        /// Observed length in characters.
        chars: usize,
        /// Limit.
        limit: usize,
    },
    /// The path starts with `/`.
    #[error("item path `{path}` must be relative to the store root")]
    Absolute {
        /// Rejected path, truncated for display.
        path: String,
    },
    /// The path contains a backslash, colon, NUL or control character.
    #[error("item path `{path}` contains the unsupported character `{character:?}`")]
    InvalidCharacter {
        /// Rejected path, truncated for display.
        path: String,
        /// Offending character.
        character: char,
    },
    /// The path contains two adjacent separators, or starts or ends with one.
    #[error("item path `{path}` contains an empty component")]
    EmptyComponent {
        /// Rejected path, truncated for display.
        path: String,
    },
    /// The path contains a `.` component.
    #[error("item path `{path}` contains a `.` component")]
    CurrentComponent {
        /// Rejected path, truncated for display.
        path: String,
    },
    /// The path contains a `..` component.
    #[error("item path `{path}` contains a `..` component")]
    ParentComponent {
        /// Rejected path, truncated for display.
        path: String,
    },
    /// One component is longer than the portable 255-byte limit.
    #[error("item path `{path}` has a component longer than {limit} bytes")]
    ComponentTooLong {
        /// Rejected path, truncated for display.
        path: String,
        /// Limit in bytes.
        limit: usize,
    },
}

/// Why an item path could not be trusted against the filesystem.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PathTrustError {
    /// A path component could not be inspected.
    #[error("{}: could not inspect item path: {source}", .path.display())]
    Inspect {
        /// Path that could not be inspected.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A component that has to be a directory is not one.
    #[error("{}: item path component is not a directory", .path.display())]
    NotADirectory {
        /// Offending component.
        path: PathBuf,
    },
    /// A component is a symlink; following it could escape the store root.
    #[error("{}: symlink in item path; refusing to follow it", .path.display())]
    Symlink {
        /// Offending component.
        path: PathBuf,
    },
}

/// Resolves a validated manifest path inside `root`, refusing symlink components.
///
/// The store root itself may be a symlink (its canonical target becomes the root); no
/// component below it may be. A missing trailing component is not an error: the returned
/// path simply does not exist, and the caller reports the missing item. The returned path
/// starts with the canonical root, which can differ from `root` when an ancestor is a
/// symlink.
///
/// # Errors
/// [`PathTrustError`] when the root is unreadable or a component is a symlink or a
/// non-directory in the middle of the path.
pub fn resolve_item_path(root: &Path, path: &StorePath) -> Result<PathBuf, PathTrustError> {
    let root = fs::canonicalize(root).map_err(|source| PathTrustError::Inspect {
        path: root.to_path_buf(),
        source,
    })?;
    if !root.is_dir() {
        return Err(PathTrustError::NotADirectory { path: root });
    }
    let parts: Vec<&str> = path.components().collect();
    let mut target = root;
    for (index, part) in parts.iter().enumerate() {
        target.push(part);
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PathTrustError::Symlink { path: target });
            }
            Ok(meta) => {
                let last = index + 1 == parts.len();
                if !last && !meta.is_dir() {
                    return Err(PathTrustError::NotADirectory { path: target });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(target),
            Err(source) => {
                return Err(PathTrustError::Inspect {
                    path: target,
                    source,
                });
            }
        }
    }
    Ok(target)
}

/// Where an item came from, when it was not authored in this store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSource {
    /// Kind of origin.
    pub kind: SourceKind,
    /// Repository URL or local folder the item was imported from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Git branch or tag the item was installed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// Git commit the item was installed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    /// Folder inside the source holding the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    /// Version reported by the source, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// How this library package is materialized in the target store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_mode: Option<at_model::LinkMode>,
}

impl ItemSource {
    /// A source of `kind` with no further metadata.
    #[must_use]
    pub fn new(kind: SourceKind) -> Self {
        Self {
            kind,
            url: None,
            git_ref: None,
            commit_sha: None,
            subpath: None,
            version: None,
            link_mode: None,
        }
    }
}

/// Kind of an item's origin, matching the `sources.kind` database column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceKind {
    /// A Git repository.
    Git,
    /// Another AgenTuna store or project library.
    Library,
    /// A local folder or file.
    Local,
    /// A folder the user picked in the import dialog.
    Folder,
    /// A marketplace listing.
    Marketplace,
    /// A zip archive.
    Zip,
}

/// The agents a tombstone disables.
///
/// In `store.json` this is `null` (every agent) or a non-empty, duplicate-free array of
/// agent ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledAgents {
    /// Disable the item for every agent, serialized as `null`.
    All,
    /// Disable the item only for the listed agents.
    Only(Vec<AgentId>),
}

impl DisabledAgents {
    /// Whether the tombstone covers every agent.
    #[must_use]
    pub const fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// Whether `agent` is disabled, when the tombstone is per-agent.
    #[must_use]
    pub fn contains(&self, agent: &AgentId) -> bool {
        match self {
            Self::All => true,
            Self::Only(agents) => agents.contains(agent),
        }
    }
}

impl Serialize for DisabledAgents {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => serializer.serialize_none(),
            Self::Only(agents) => agents.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for DisabledAgents {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let agents = Option::<Vec<AgentId>>::deserialize(deserializer)?;
        Ok(match agents {
            None => Self::All,
            Some(list) => Self::Only(list),
        })
    }
}

/// One disable tombstone: an item removed from the effective set of this scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisabledEntry {
    /// Item being disabled; must exist in `items` with the same kind.
    pub item_id: ItemId,
    /// Kind of the disabled item; must match the item record.
    pub kind: ItemKind,
    /// Agents the tombstone applies to; `All` is serialized as `null`.
    pub agents: DisabledAgents,
}

/// One item record in `store.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestItem {
    /// Stable item identity from TF-002; never reused.
    pub id: ItemId,
    /// What the item is.
    pub kind: ItemKind,
    /// Folder name or document key of the item.
    pub slug: Slug,
    /// Location relative to the store root: `skills/<slug>`, `rules/<file>` or `mcp.json`.
    pub path: StorePath,
    /// Canonical content hash; see [`hash_skill_folder`], [`hash_rule_bytes`] and
    /// [`hash_mcp_entry`].
    pub hash: ContentHash,
    /// Display name; `slug` is shown when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Origin metadata for imported items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ItemSource>,
}

impl ManifestItem {
    /// A minimal item record without a display name or source.
    #[must_use]
    pub fn new(id: ItemId, kind: ItemKind, slug: Slug, path: StorePath, hash: ContentHash) -> Self {
        Self {
            id,
            kind,
            slug,
            path,
            hash,
            name: None,
            source: None,
        }
    }

    /// Sets the display name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the origin metadata.
    #[must_use]
    pub fn with_source(mut self, source: ItemSource) -> Self {
        self.source = Some(source);
        self
    }
}

/// A field-level manifest validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldProblem {
    /// The manifest declares a schema version this build does not write.
    #[error("schema version {found} is not supported (expected {supported})")]
    UnsupportedSchema {
        /// Version found in the document.
        found: u32,
        /// Version this build writes.
        supported: u32,
    },
    /// A tombstone references an item id that is not in `items`.
    #[error("disabled item `{item_id}` does not exist in items")]
    MissingItem {
        /// Missing item id.
        item_id: String,
    },
    /// An item id was not found when updating it.
    #[error("item `{item_id}` does not exist in the manifest")]
    UnknownItem {
        /// Missing item id.
        item_id: String,
    },
    /// The tombstone kind differs from the item kind.
    #[error("disabled kind `{found}` does not match item kind `{expected}`")]
    KindMismatch {
        /// Kind recorded on the item.
        expected: ItemKind,
        /// Kind recorded on the tombstone.
        found: ItemKind,
    },
    /// `agents` is an empty array.
    #[error("agents must be null (every agent) or a non-empty array")]
    EmptyAgents,
    /// `agents` lists the same agent twice.
    #[error("agent `{agent}` appears more than once")]
    DuplicateAgent {
        /// Repeated agent.
        agent: String,
    },
    /// The same item has more than one tombstone.
    #[error("item `{item_id}` has more than one disable tombstone")]
    DuplicateTombstone {
        /// Repeated item id.
        item_id: String,
    },
    /// Two items share an id.
    #[error("item id `{item_id}` appears more than once")]
    DuplicateItemId {
        /// Repeated item id.
        item_id: String,
    },
    /// Two items of one kind share a slug.
    #[error("{kind} slug `{slug}` appears more than once")]
    DuplicateSlug {
        /// Item kind.
        kind: ItemKind,
        /// Repeated slug.
        slug: String,
    },
    /// Two items of one kind point at the same path.
    #[error("{kind} path `{path}` is claimed by more than one item")]
    DuplicatePath {
        /// Item kind.
        kind: ItemKind,
        /// Shared path.
        path: String,
    },
    /// The manifest holds more items than [`MANIFEST_MAX_ITEMS`].
    #[error("manifest holds more than {limit} items")]
    TooManyItems {
        /// Limit.
        limit: usize,
    },
    /// The manifest holds more tombstones than [`MANIFEST_MAX_DISABLED`].
    #[error("manifest holds more than {limit} disable tombstones")]
    TooManyTombstones {
        /// Limit.
        limit: usize,
    },
    /// A display name is empty.
    #[error("display name must not be empty")]
    EmptyName,
    /// A display name is longer than [`MANIFEST_MAX_NAME_CHARS`].
    #[error("display name is {chars} characters; the limit is {limit}")]
    NameTooLong {
        /// Observed length.
        chars: usize,
        /// Limit.
        limit: usize,
    },
    /// A source-metadata string is too long.
    #[error("source `{field}` is {chars} characters; the limit is {limit}")]
    SourceFieldTooLong {
        /// Which source field.
        field: String,
        /// Observed length.
        chars: usize,
        /// Limit.
        limit: usize,
    },
    /// A name or source field contains a control character.
    #[error("contains the control character `{character:?}`")]
    ControlCharacter {
        /// Offending character.
        character: char,
    },
    /// The item path does not belong to the layout of its kind.
    #[error("{kind} path `{path}` is not a location this kind may use")]
    PathKindMismatch {
        /// Item kind.
        kind: ItemKind,
        /// Offending path.
        path: String,
    },
}

/// One validation failure, naming the JSON field it belongs to.
///
/// `field` uses the document shape, e.g. `disabled[2].agents` or `items[0].hash`. Indices
/// count positions in the canonical document written by [`StoreManifest::to_bytes`], so a
/// UI or CLI can point at the offending entry without re-deriving its position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("`{field}`: {problem}")]
pub struct FieldError {
    /// JSON field that failed validation.
    pub field: String,
    /// What is wrong with it.
    pub problem: FieldProblem,
}

fn field(field: impl Into<String>, problem: FieldProblem) -> FieldError {
    FieldError {
        field: field.into(),
        problem,
    }
}

/// A manifest and the hash of the exact bytes it was read from.
///
/// The hash is what [`StoreManifest::save`] expects on the next save, so an external edit
/// between load and save is detected instead of overwritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedManifest {
    /// Parsed and validated manifest.
    pub manifest: StoreManifest,
    /// SHA-256 of the bytes read from `store.json`.
    pub hash: ContentHash,
}

/// A `store.json` read for planning: the parsed document and its hash, or an empty manifest
/// when the file does not exist yet.
///
/// First-run intents plan the item and the store skeleton in one reviewed `ChangeSet`. They need
/// a manifest to extend before the store exists, and they must present [`at_model::ExpectedState::Absent`]
/// instead of a hash for the file that apply creates. [`StoreManifest::load_for_plan`] is the one
/// place that decides between those two states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedManifest {
    /// Parsed and validated manifest, or an empty one for an absent file.
    pub manifest: StoreManifest,
    /// SHA-256 of the bytes read; `None` when the file is absent.
    pub hash: Option<ContentHash>,
}

/// What a successful manifest save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    /// SHA-256 of the persisted document.
    pub hash: ContentHash,
    /// The guarded write that installed it.
    pub write: WriteOutcome,
}

/// `store.json`: the only item manifest of a store.
///
/// Values are always checked by [`Self::parse`], [`Self::validate`] and [`Self::save`]; the
/// mutation helpers validate eagerly and leave the manifest unchanged on failure. A value
/// built by hand with `serde` and never saved is the caller's responsibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreManifest {
    schema_version: u32,
    store_id: StoreId,
    scope: Scope,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    items: Vec<ManifestItem>,
    disabled: Vec<DisabledEntry>,
}

impl StoreManifest {
    /// An empty manifest for a new store, stamped with the current time.
    #[must_use]
    pub fn new(scope: Scope, store_id: StoreId) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            store_id,
            scope,
            updated_at: OffsetDateTime::now_utc(),
            items: Vec::new(),
            disabled: Vec::new(),
        }
    }

    /// An empty manifest carrying an existing header (used by store bootstrap and rebuild).
    #[must_use]
    pub fn from_header(header: &StoreHeader) -> Self {
        Self {
            schema_version: header.schema_version,
            store_id: header.store_id,
            scope: header.scope,
            updated_at: header.updated_at,
            items: Vec::new(),
            disabled: Vec::new(),
        }
    }

    /// Replaces the timestamp; normal mutations stamp it themselves.
    #[must_use]
    pub fn with_updated_at(mut self, updated_at: OffsetDateTime) -> Self {
        self.updated_at = updated_at;
        self
    }

    /// Document version of the manifest.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Identity of the store this manifest belongs to.
    #[must_use]
    pub const fn store_id(&self) -> StoreId {
        self.store_id
    }

    /// Scope whose content this manifest describes.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// When the manifest last changed.
    #[must_use]
    pub const fn updated_at(&self) -> OffsetDateTime {
        self.updated_at
    }

    /// The header view of this manifest, for code that only needs identity.
    #[must_use]
    pub const fn header(&self) -> StoreHeader {
        StoreHeader {
            schema_version: self.schema_version,
            store_id: self.store_id,
            scope: self.scope,
            updated_at: self.updated_at,
        }
    }

    /// Every item, in canonical order (kind, then slug).
    #[must_use]
    pub fn items(&self) -> &[ManifestItem] {
        &self.items
    }

    /// Every disable tombstone, in canonical order (kind, then item id).
    #[must_use]
    pub fn disabled(&self) -> &[DisabledEntry] {
        &self.disabled
    }

    /// The item with `id`, if it exists.
    #[must_use]
    pub fn item(&self, id: ItemId) -> Option<&ManifestItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// The item of `kind` with `slug`, if it exists.
    #[must_use]
    pub fn item_by_slug(&self, kind: ItemKind, slug: &Slug) -> Option<&ManifestItem> {
        self.items
            .iter()
            .find(|item| item.kind == kind && item.slug == *slug)
    }

    /// Every item of `kind`, in canonical order.
    pub fn items_of_kind(&self, kind: ItemKind) -> impl Iterator<Item = &ManifestItem> + '_ {
        self.items.iter().filter(move |item| item.kind == kind)
    }

    /// The tombstone for `id`, if the item is disabled here.
    #[must_use]
    pub fn disabled_for(&self, id: ItemId) -> Option<&DisabledEntry> {
        self.disabled.iter().find(|entry| entry.item_id == id)
    }

    /// Replaces the timestamp with the current time. Called by every real mutation.
    fn touch(&mut self) {
        self.updated_at = OffsetDateTime::now_utc();
    }

    fn sort_items(&mut self) {
        self.items
            .sort_by(|left, right| (left.kind, &left.slug).cmp(&(right.kind, &right.slug)));
    }

    fn sort_disabled(&mut self) {
        self.disabled
            .sort_by_key(|entry| (entry.kind, entry.item_id));
    }

    /// Loads and sorts the manifest and collapses byte-identical duplicate tombstones
    /// produced by older writers.
    ///
    /// Agent lists are sorted but never deduplicated: a duplicate agent and an empty list
    /// stay validation errors. Duplicate tombstones that differ (for example in their agent
    /// list) are also left for validation to reject.
    fn normalized_for_load(mut self) -> Self {
        self.sort_items();
        for entry in &mut self.disabled {
            if let DisabledAgents::Only(agents) = &mut entry.agents {
                agents.sort();
            }
        }
        self.disabled
            .sort_by_key(|entry| (entry.kind, entry.item_id));
        self.disabled.dedup();
        self
    }

    /// Adds an item.
    ///
    /// # Errors
    /// A [`FieldError`] for an invalid item, a duplicate id, slug or path, or a manifest
    /// limit. The manifest is unchanged on failure.
    pub fn insert_item(&mut self, item: ManifestItem) -> Result<(), FieldError> {
        let mut next = self.clone();
        next.items.push(item);
        next.sort_items();
        next.validate()?;
        next.touch();
        *self = next;
        Ok(())
    }

    /// Replaces the item with the same id, returning the previous record.
    ///
    /// The id is preserved by definition: a rename changes `slug` and `path`, never `id`.
    ///
    /// # Errors
    /// [`FieldProblem::UnknownItem`] when the id is not present, or a [`FieldError`] for an
    /// invalid or colliding replacement. The manifest is unchanged on failure.
    pub fn update_item(&mut self, item: ManifestItem) -> Result<ManifestItem, FieldError> {
        let mut next = self.clone();
        let position = next
            .items
            .iter()
            .position(|existing| existing.id == item.id)
            .ok_or_else(|| {
                field(
                    "item.id",
                    FieldProblem::UnknownItem {
                        item_id: item.id.to_string(),
                    },
                )
            })?;
        let previous = std::mem::replace(&mut next.items[position], item.clone());
        next.sort_items();
        next.validate()?;
        if previous != item {
            next.touch();
        }
        *self = next;
        Ok(previous)
    }

    /// Removes an item and every tombstone that referenced it.
    ///
    /// The id is retired with the item: this crate never allocates ids, and callers must
    /// not reuse the removed id for a new item.
    pub fn remove_item(&mut self, id: ItemId) -> Option<ManifestItem> {
        let position = self.items.iter().position(|item| item.id == id)?;
        let mut next = self.clone();
        let removed = next.items.remove(position);
        next.disabled.retain(|entry| entry.item_id != id);
        next.touch();
        *self = next;
        Some(removed)
    }

    /// Writes one tombstone for `item_id`, replacing an existing tombstone for the same item.
    ///
    /// # Errors
    /// [`FieldProblem::MissingItem`] when the item does not exist, [`FieldProblem::EmptyAgents`]
    /// or [`FieldProblem::DuplicateAgent`] for an unusable agents list. The manifest is
    /// unchanged on failure.
    pub fn disable(&mut self, item_id: ItemId, agents: DisabledAgents) -> Result<(), FieldError> {
        let mut next = self.clone();
        let kind = next
            .items
            .iter()
            .find(|item| item.id == item_id)
            .map(|item| item.kind)
            .ok_or_else(|| {
                field(
                    "item_id",
                    FieldProblem::MissingItem {
                        item_id: item_id.to_string(),
                    },
                )
            })?;
        let entry = DisabledEntry {
            item_id,
            kind,
            agents: match agents {
                DisabledAgents::All => DisabledAgents::All,
                DisabledAgents::Only(mut list) => {
                    list.sort();
                    DisabledAgents::Only(list)
                }
            },
        };
        if let Some(existing) = next
            .disabled
            .iter_mut()
            .find(|existing| existing.item_id == item_id)
        {
            if *existing == entry {
                return Ok(());
            }
            *existing = entry;
        } else {
            next.disabled.push(entry);
        }
        next.sort_disabled();
        next.validate()?;
        next.touch();
        *self = next;
        Ok(())
    }
    /// Disables an inherited item that was defined in an ancestor scope.
    ///
    /// If the item exists in this scope's `items`, its kind must match.
    ///
    /// # Errors
    /// [`FieldProblem::KindMismatch`] when `item_id` exists in `items` with a different
    /// kind, [`FieldProblem::MissingItem`] on Global scope when `item_id` is absent from
    /// `items`, or [`FieldProblem::EmptyAgents`] / [`FieldProblem::DuplicateAgent`].
    pub fn disable_inherited(
        &mut self,
        item_id: ItemId,
        kind: ItemKind,
        agents: DisabledAgents,
    ) -> Result<(), FieldError> {
        let mut next = self.clone();
        if let Some(item) = next.items.iter().find(|item| item.id == item_id)
            && item.kind != kind
        {
            return Err(field(
                "kind",
                FieldProblem::KindMismatch {
                    expected: item.kind,
                    found: kind,
                },
            ));
        }
        let entry = DisabledEntry {
            item_id,
            kind,
            agents: match agents {
                DisabledAgents::All => DisabledAgents::All,
                DisabledAgents::Only(mut list) => {
                    list.sort();
                    DisabledAgents::Only(list)
                }
            },
        };
        if let Some(existing) = next
            .disabled
            .iter_mut()
            .find(|existing| existing.item_id == item_id)
        {
            if *existing == entry {
                return Ok(());
            }
            *existing = entry;
        } else {
            next.disabled.push(entry);
        }
        next.sort_disabled();
        next.validate()?;
        next.touch();
        *self = next;
        Ok(())
    }

    /// Removes the tombstone for `item_id`. Returns whether one was present.
    pub fn enable(&mut self, item_id: ItemId) -> bool {
        let before = self.disabled.len();
        let mut next = self.clone();
        next.disabled.retain(|entry| entry.item_id != item_id);
        if next.disabled.len() == before {
            return false;
        }
        next.touch();
        *self = next;
        true
    }

    /// Checks every schema invariant.
    ///
    /// # Errors
    /// The first [`FieldError`] in document order.
    pub fn validate(&self) -> Result<(), FieldError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(field(
                "schema_version",
                FieldProblem::UnsupportedSchema {
                    found: self.schema_version,
                    supported: SCHEMA_VERSION,
                },
            ));
        }
        if self.items.len() > MANIFEST_MAX_ITEMS {
            return Err(field(
                "items",
                FieldProblem::TooManyItems {
                    limit: MANIFEST_MAX_ITEMS,
                },
            ));
        }
        let mut by_id: BTreeMap<ItemId, ItemKind> = BTreeMap::new();
        let mut slugs: BTreeSet<(ItemKind, &str)> = BTreeSet::new();
        let mut paths: BTreeSet<(ItemKind, &str)> = BTreeSet::new();
        for (index, item) in self.items.iter().enumerate() {
            validate_item(item, &format!("items[{index}]"))?;
            if by_id.insert(item.id, item.kind).is_some() {
                return Err(field(
                    format!("items[{index}].id"),
                    FieldProblem::DuplicateItemId {
                        item_id: item.id.to_string(),
                    },
                ));
            }
            if !slugs.insert((item.kind, item.slug.as_str())) {
                return Err(field(
                    format!("items[{index}].slug"),
                    FieldProblem::DuplicateSlug {
                        kind: item.kind,
                        slug: item.slug.to_string(),
                    },
                ));
            }
            if item.kind != ItemKind::McpServer && !paths.insert((item.kind, item.path.as_str())) {
                return Err(field(
                    format!("items[{index}].path"),
                    FieldProblem::DuplicatePath {
                        kind: item.kind,
                        path: item.path.to_string(),
                    },
                ));
            }
        }
        if self.disabled.len() > MANIFEST_MAX_DISABLED {
            return Err(field(
                "disabled",
                FieldProblem::TooManyTombstones {
                    limit: MANIFEST_MAX_DISABLED,
                },
            ));
        }
        let mut seen: BTreeSet<ItemId> = BTreeSet::new();
        for (index, entry) in self.disabled.iter().enumerate() {
            let prefix = format!("disabled[{index}]");
            if let Some(kind) = by_id.get(&entry.item_id) {
                if *kind != entry.kind {
                    return Err(field(
                        format!("{prefix}.kind"),
                        FieldProblem::KindMismatch {
                            expected: *kind,
                            found: entry.kind,
                        },
                    ));
                }
            } else if self.scope == Scope::Global {
                return Err(field(
                    format!("{prefix}.item_id"),
                    FieldProblem::MissingItem {
                        item_id: entry.item_id.to_string(),
                    },
                ));
            }
            if let DisabledAgents::Only(agents) = &entry.agents {
                if agents.is_empty() {
                    return Err(field(format!("{prefix}.agents"), FieldProblem::EmptyAgents));
                }
                let mut unique = BTreeSet::new();
                for agent in agents {
                    if !unique.insert(agent) {
                        return Err(field(
                            format!("{prefix}.agents"),
                            FieldProblem::DuplicateAgent {
                                agent: agent.to_string(),
                            },
                        ));
                    }
                }
            }
            if !seen.insert(entry.item_id) {
                return Err(field(
                    format!("{prefix}.item_id"),
                    FieldProblem::DuplicateTombstone {
                        item_id: entry.item_id.to_string(),
                    },
                ));
            }
        }
        Ok(())
    }

    /// Serializes the manifest to its canonical bytes.
    ///
    /// Items sort by kind then slug, tombstones by kind then item id, the text is
    /// pretty-printed with two spaces and ends in one newline.
    ///
    /// # Errors
    /// A [`FieldError`] from [`Self::validate`], or [`ManifestError::DocumentTooLarge`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        let mut canonical = self.clone();
        canonical.sort_items();
        for entry in &mut canonical.disabled {
            if let DisabledAgents::Only(agents) = &mut entry.agents {
                agents.sort();
            }
        }
        canonical.sort_disabled();
        canonical.validate()?;
        let mut text = serde_json::to_string_pretty(&canonical)?;
        text.push('\n');
        let bytes = text.into_bytes();
        if bytes.len() as u64 > MANIFEST_MAX_BYTES {
            return Err(ManifestError::DocumentTooLarge {
                bytes: bytes.len() as u64,
                limit: MANIFEST_MAX_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Reads and validates `store.json` of `layout`.
    ///
    /// The returned hash is the observation a later [`Self::save`] must present.
    ///
    /// # Errors
    /// [`ManifestError`] for a missing or unreadable file, a symlink, an oversized file,
    /// invalid JSON, an unsupported schema, a scope mismatch or an invalid manifest.
    pub fn load(layout: &StoreLayout) -> Result<LoadedManifest, ManifestError> {
        let path = layout.manifest_path();
        let bytes = read_manifest_file(&path)?;
        let hash = ContentHash::from_bytes(&bytes);
        let manifest = Self::parse(&bytes)?;
        if manifest.scope != layout.scope() {
            return Err(ManifestError::ScopeMismatch {
                expected: layout.scope(),
                found: manifest.scope,
            });
        }
        Ok(LoadedManifest { manifest, hash })
    }

    /// Reads `store.json` for a plan, treating an absent file as an empty manifest.
    ///
    /// A first-run intent plans the item and the store skeleton together. This is the only seam
    /// that decides between extending an existing manifest and starting from an empty one; a
    /// caller never guesses whether the file exists. Every other read failure is returned
    /// unchanged, so a corrupt or foreign-scope store is refused instead of overwritten.
    ///
    /// # Errors
    /// Every [`ManifestError`] except a missing file; those become a [`PlannedManifest`] with
    /// [`PlannedManifest::hash`] `None`.
    pub fn load_for_plan(layout: &StoreLayout) -> Result<PlannedManifest, ManifestError> {
        match Self::load(layout) {
            Ok(loaded) => Ok(PlannedManifest {
                manifest: loaded.manifest,
                hash: Some(loaded.hash),
            }),
            Err(error) if error.is_missing() => Ok(PlannedManifest {
                manifest: Self::new(layout.scope(), StoreId::new()),
                hash: None,
            }),
            Err(error) => Err(error),
        }
    }

    /// Parses and validates manifest bytes without touching the filesystem.
    ///
    /// Duplicate tombstones collapse to one entry before validation; every other invariant
    /// is enforced.
    ///
    /// # Errors
    /// [`ManifestError::Json`], [`ManifestError::UnknownSchema`] or
    /// [`ManifestError::Validation`].
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_slice(bytes)?;
        if manifest.schema_version != SCHEMA_VERSION {
            return Err(ManifestError::UnknownSchema {
                found: manifest.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        let manifest = manifest.normalized_for_load();
        manifest.validate()?;
        Ok(manifest)
    }

    /// Writes the manifest through the `at-fs` guarded write pipeline.
    ///
    /// `expected` must be the observation from [`Self::load`] (or
    /// [`ExpectedState::Absent`] for a new store); a concurrent writer makes the save fail
    /// with [`ManifestError::Write`] and leaves the external bytes in place.
    ///
    /// # Errors
    /// A [`FieldError`] from validation, or [`ManifestError::Write`] from the pipeline.
    pub fn save(
        &self,
        layout: &StoreLayout,
        expected: ExpectedState,
        backup: BackupPolicy,
    ) -> Result<SaveOutcome, ManifestError> {
        if self.scope != layout.scope() {
            return Err(ManifestError::ScopeMismatch {
                expected: layout.scope(),
                found: self.scope,
            });
        }
        let bytes = self.to_bytes()?;
        let hash = ContentHash::from_bytes(&bytes);
        let path = layout.manifest_path();
        let mode = if matches!(expected, ExpectedState::Absent) {
            WriteMode::Create
        } else {
            WriteMode::Replace
        };
        let request = WriteRequest {
            path: path.clone(),
            new_bytes: bytes,
            expected,
            backup,
            follow_symlink: false,
            mode,
            allow_empty_dir: false,
            backup_meta: BackupMeta {
                scope: Some(layout.scope()),
                relpath: Some(MANIFEST_FILE.to_owned()),
                operation_id: None,
                mode: None,
            },
            guard: None,
        };
        let write = apply(&request).map_err(|source| ManifestError::Write { path, source })?;
        Ok(SaveOutcome { hash, write })
    }
}

fn validate_item(item: &ManifestItem, prefix: &str) -> Result<(), FieldError> {
    if let Some(name) = &item.name {
        let chars = name.chars().count();
        if name.trim().is_empty() {
            return Err(field(format!("{prefix}.name"), FieldProblem::EmptyName));
        }
        if chars > MANIFEST_MAX_NAME_CHARS {
            return Err(field(
                format!("{prefix}.name"),
                FieldProblem::NameTooLong {
                    chars,
                    limit: MANIFEST_MAX_NAME_CHARS,
                },
            ));
        }
        validate_text(&format!("{prefix}.name"), name)?;
    }
    if let Some(source) = &item.source {
        for (key, value) in [
            ("url", &source.url),
            ("git_ref", &source.git_ref),
            ("commit_sha", &source.commit_sha),
            ("subpath", &source.subpath),
            ("version", &source.version),
        ] {
            if let Some(text) = value {
                let chars = text.chars().count();
                if chars > MANIFEST_MAX_SOURCE_FIELD_CHARS {
                    return Err(field(
                        format!("{prefix}.source.{key}"),
                        FieldProblem::SourceFieldTooLong {
                            field: key.to_owned(),
                            chars,
                            limit: MANIFEST_MAX_SOURCE_FIELD_CHARS,
                        },
                    ));
                }
                validate_text(&format!("{prefix}.source.{key}"), text)?;
            }
        }
    }
    let components: Vec<&str> = item.path.components().collect();
    let layout_ok = match item.kind {
        ItemKind::Skill => components.len() == 2 && components[0] == SKILLS_DIR,
        ItemKind::McpServer => item.path.as_str() == MCP_FILE,
        ItemKind::Rule => components.len() >= 2 && components[0] == RULES_DIR,
    };
    if !layout_ok {
        return Err(field(
            format!("{prefix}.path"),
            FieldProblem::PathKindMismatch {
                kind: item.kind,
                path: item.path.to_string(),
            },
        ));
    }
    Ok(())
}

fn validate_text(field_name: &str, text: &str) -> Result<(), FieldError> {
    if let Some(character) = text.chars().find(|character| character.is_control()) {
        return Err(field(
            field_name.to_owned(),
            FieldProblem::ControlCharacter { character },
        ));
    }
    Ok(())
}

fn read_manifest_file(path: &Path) -> Result<Vec<u8>, ManifestError> {
    let meta = fs::symlink_metadata(path).map_err(|source| ManifestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(ManifestError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    if meta.len() > MANIFEST_MAX_BYTES {
        return Err(ManifestError::TooLarge {
            path: path.to_path_buf(),
            bytes: meta.len(),
            limit: MANIFEST_MAX_BYTES,
        });
    }
    let mut file = fs::File::open(path).map_err(|source| ManifestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let opened = file.metadata().map_err(|source| ManifestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    use std::os::unix::fs::MetadataExt;
    if opened.dev() != meta.dev() || opened.ino() != meta.ino() {
        return Err(ManifestError::Changed {
            path: path.to_path_buf(),
        });
    }
    let capacity = usize::try_from(meta.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(MANIFEST_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MANIFEST_MAX_BYTES {
        return Err(ManifestError::TooLarge {
            path: path.to_path_buf(),
            bytes: bytes.len() as u64,
            limit: MANIFEST_MAX_BYTES,
        });
    }
    Ok(bytes)
}

/// Why a manifest could not be read, validated or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// The file could not be read.
    #[error("{}: could not read store manifest: {source}", .path.display())]
    Read {
        /// Manifest path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// `store.json` is not a regular file (a directory or a symlink).
    #[error("{}: store manifest is not a regular file", .path.display())]
    NotRegularFile {
        /// Manifest path.
        path: PathBuf,
    },
    /// The file is larger than [`MANIFEST_MAX_BYTES`].
    #[error("{}: store manifest is {bytes} bytes; the limit is {limit}", .path.display())]
    TooLarge {
        /// Manifest path.
        path: PathBuf,
        /// Observed size.
        bytes: u64,
        /// Limit.
        limit: u64,
    },
    /// The document grew past the limit while it was read.
    #[error("{}: store manifest changed while it was read", .path.display())]
    Changed {
        /// Manifest path.
        path: PathBuf,
    },
    /// The canonical document would exceed [`MANIFEST_MAX_BYTES`].
    #[error("store manifest would be {bytes} bytes; the limit is {limit}")]
    DocumentTooLarge {
        /// Serialized size.
        bytes: u64,
        /// Limit.
        limit: u64,
    },
    /// The bytes are not valid JSON for the manifest schema.
    #[error("store manifest is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The manifest declares a schema version this build does not write.
    #[error(
        "store manifest schema version {found} is newer than this AgenTuna supports (version {supported}); update AgenTuna to open this store"
    )]
    UnknownSchema {
        /// Version found in the document.
        found: u32,
        /// Version this build supports.
        supported: u32,
    },
    /// The manifest belongs to a different scope than the layout being used.
    #[error("store manifest holds scope `{found}` but scope `{expected}` was requested")]
    ScopeMismatch {
        /// Scope of the store root.
        expected: Scope,
        /// Scope recorded in the manifest.
        found: Scope,
    },
    /// A schema invariant failed.
    #[error(transparent)]
    Validation(#[from] FieldError),
    /// The guarded write failed; nothing was overwritten.
    #[error("{}: could not write store manifest: {source}", .path.display())]
    Write {
        /// Manifest path.
        path: PathBuf,
        /// Pipeline error.
        source: at_fs::FsError,
    },
}

impl ManifestError {
    /// Whether the manifest file is simply absent.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Read { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}

/// Hashes arbitrary bytes with SHA-256.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(bytes)
}

/// Hashes a rule file after normalizing CRLF line endings to LF.
///
/// Only `\r\n` sequences change; a lone `\r` and the trailing newline are content and do
/// change the hash. Copying a file through a tool that rewrites line endings therefore does
/// not look like an edit.
#[must_use]
pub fn hash_rule_bytes(bytes: &[u8]) -> ContentHash {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                normalized.push(b'\n');
                index += 2;
            }
            byte => {
                normalized.push(byte);
                index += 1;
            }
        }
    }
    ContentHash::from_bytes(&normalized)
}

/// Renders a JSON value as canonical JSON: sorted object keys, no insignificant whitespace.
///
/// Strings are escaped by `serde_json`; numbers use their shortest exact form. `${VAR}`
/// references are kept literal: no environment lookup happens here.
///
/// # Errors
/// [`CanonicalJsonError`] when a string cannot be rendered (never in practice).
pub fn canonical_json(value: &serde_json::Value) -> Result<String, CanonicalJsonError> {
    let mut out = String::new();
    write_canonical(value, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &serde_json::Value, out: &mut String) -> Result<(), CanonicalJsonError> {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(number) => out.push_str(&number.to_string()),
        serde_json::Value::String(text) => out.push_str(&serde_json::to_string(text)?),
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(object) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = object.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                write_canonical(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Why canonical JSON could not be rendered.
#[derive(Debug, thiserror::Error)]
#[error("could not render canonical JSON: {0}")]
pub struct CanonicalJsonError(#[from] serde_json::Error);

/// Hashes one MCP server entry from `mcp.json`.
///
/// The hash covers canonical JSON only, so reordering keys or changing whitespace keeps it
/// stable while changing a value (including an env value) changes it. `${VAR}` references
/// stay literal.
///
/// # Errors
/// [`CanonicalJsonError`] when the value cannot be rendered.
pub fn hash_mcp_entry(value: &serde_json::Value) -> Result<ContentHash, CanonicalJsonError> {
    let text = canonical_json(value)?;
    Ok(ContentHash::from_bytes(text.as_bytes()))
}

/// Returns the first top-level identity key in a `mcp.json` server definition, if any.
///
/// `mcp.json` is content only; item identity lives in `store.json` alone.
#[must_use]
pub fn mcp_identity_key(value: &serde_json::Value) -> Option<&'static str> {
    let object = value.as_object()?;
    MCP_IDENTITY_KEYS
        .iter()
        .copied()
        .find(|key| object.contains_key(*key))
}

/// Hashes a skill folder as a tree.
///
/// The digest is SHA-256 over one line per file, sorted by relative path:
/// `<rel_path>\0<size>\0<sha256(bytes)>\n`. Names, sizes and bytes are content; mtimes,
/// inode numbers and the folder name are not. `SKILL.md` hashes normalize CRLF and sort
/// straightforward valid frontmatter fields; other files retain byte-exact content.
///
/// The walk never follows a symlink and refuses one inside the tree. It bounds the number
/// of files, each file's size, the total content and the nesting depth.
///
/// # Errors
/// [`HashError`] for a missing or non-directory root, a symlink, an unsupported file type,
/// an unreadable file, a file that changed mid-read, or an exceeded bound.
pub fn hash_skill_folder(folder: &Path) -> Result<ContentHash, HashError> {
    let meta = fs::symlink_metadata(folder).map_err(|source| HashError::Read {
        path: folder.to_path_buf(),
        source,
    })?;
    if meta.file_type().is_symlink() {
        return Err(HashError::Symlink {
            path: folder.to_path_buf(),
        });
    }
    if !meta.is_dir() {
        return Err(HashError::NotADirectory {
            path: folder.to_path_buf(),
        });
    }
    let mut files: Vec<(String, u64, String)> = Vec::new();
    let mut total: u64 = 0;
    walk_skill(folder, folder, 1, &mut files, &mut total)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut lines = String::new();
    for (relative, size, digest) in files {
        lines.push_str(&relative);
        lines.push('\0');
        lines.push_str(&size.to_string());
        lines.push('\0');
        lines.push_str(&digest);
        lines.push('\n');
    }
    Ok(ContentHash::from_bytes(lines.as_bytes()))
}

fn walk_skill(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut Vec<(String, u64, String)>,
    total: &mut u64,
) -> Result<(), HashError> {
    if depth > SKILL_HASH_MAX_DEPTH {
        return Err(HashError::TooDeep {
            path: directory.to_path_buf(),
            limit: SKILL_HASH_MAX_DEPTH,
        });
    }
    let entries = fs::read_dir(directory).map_err(|source| HashError::Read {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| HashError::Read {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|source| HashError::Read {
            path: path.clone(),
            source,
        })?;
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            return Err(HashError::Symlink { path });
        }
        if file_type.is_dir() {
            walk_skill(root, &path, depth + 1, files, total)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(HashError::Unsupported { path });
        }
        if files.len() >= SKILL_HASH_MAX_FILES {
            return Err(HashError::TooManyFiles {
                path: root.to_path_buf(),
                limit: SKILL_HASH_MAX_FILES,
            });
        }
        if meta.len() > SKILL_HASH_MAX_FILE_BYTES {
            return Err(HashError::FileTooLarge {
                path,
                size: meta.len(),
                limit: SKILL_HASH_MAX_FILE_BYTES,
            });
        }
        *total = total
            .checked_add(meta.len())
            .ok_or(HashError::TotalTooLarge {
                path: root.to_path_buf(),
                limit: SKILL_HASH_MAX_TOTAL_BYTES,
            })?;
        if *total > SKILL_HASH_MAX_TOTAL_BYTES {
            return Err(HashError::TotalTooLarge {
                path: root.to_path_buf(),
                limit: SKILL_HASH_MAX_TOTAL_BYTES,
            });
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| HashError::Unsupported { path: path.clone() })?;
        let Some(relative) = relative.to_str() else {
            return Err(HashError::NonUtf8Name { path });
        };
        let relative = relative.replace('\\', "/");
        let raw_bytes = read_skill_file(&path, &meta)?;
        let bytes = if relative == "SKILL.md" {
            crate::skill::canonical_hash_bytes(&raw_bytes)
        } else {
            raw_bytes
        };
        files.push((relative, bytes.len() as u64, sha256_hex(&bytes)));
    }
    Ok(())
}

fn read_skill_file(path: &Path, expected: &fs::Metadata) -> Result<Vec<u8>, HashError> {
    use std::os::unix::fs::MetadataExt;
    let mut file = fs::File::open(path).map_err(|source| HashError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let opened = file.metadata().map_err(|source| HashError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if opened.dev() != expected.dev() || opened.ino() != expected.ino() {
        return Err(HashError::Changed {
            path: path.to_path_buf(),
        });
    }
    let capacity = usize::try_from(expected.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(expected.len() + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| HashError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 != expected.len() {
        return Err(HashError::Changed {
            path: path.to_path_buf(),
        });
    }
    Ok(bytes)
}

/// Why a skill folder could not be hashed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HashError {
    /// A file or directory could not be read.
    #[error("{}: could not read skill content: {source}", .path.display())]
    Read {
        /// Offending path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A symlink was found inside the tree.
    #[error("{}: symlink inside a skill folder; refusing to follow it", .path.display())]
    Symlink {
        /// Symlink path.
        path: PathBuf,
    },
    /// The root is not a directory.
    #[error("{}: skill folder is not a directory", .path.display())]
    NotADirectory {
        /// Offending path.
        path: PathBuf,
    },
    /// A file name is not valid UTF-8, so it cannot enter a text hash.
    #[error("{}: file name is not valid UTF-8", .path.display())]
    NonUtf8Name {
        /// Offending path.
        path: PathBuf,
    },
    /// A directory entry is not a regular file or directory.
    #[error("{}: unsupported file type inside a skill folder", .path.display())]
    Unsupported {
        /// Offending path.
        path: PathBuf,
    },
    /// One file exceeds [`SKILL_HASH_MAX_FILE_BYTES`].
    #[error("{}: skill file is {size} bytes; the limit is {limit}", .path.display())]
    FileTooLarge {
        /// Offending file.
        path: PathBuf,
        /// Observed size.
        size: u64,
        /// Limit.
        limit: u64,
    },
    /// The tree holds more files than [`SKILL_HASH_MAX_FILES`].
    #[error("{}: skill folder holds more than {limit} files", .path.display())]
    TooManyFiles {
        /// Skill folder.
        path: PathBuf,
        /// Limit.
        limit: usize,
    },
    /// The tree content exceeds [`SKILL_HASH_MAX_TOTAL_BYTES`].
    #[error("{}: skill folder content exceeds {limit} bytes", .path.display())]
    TotalTooLarge {
        /// Skill folder.
        path: PathBuf,
        /// Limit.
        limit: u64,
    },
    /// The tree nests deeper than [`SKILL_HASH_MAX_DEPTH`].
    #[error("{}: skill folder nests deeper than {limit} levels", .path.display())]
    TooDeep {
        /// Offending directory.
        path: PathBuf,
        /// Limit.
        limit: usize,
    },
    /// A file changed between the directory scan and the read.
    #[error("{}: skill folder changed while hashing it", .path.display())]
    Changed {
        /// Offending file.
        path: PathBuf,
    },
}
