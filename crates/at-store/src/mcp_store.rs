//! The canonical `mcp.json` document: content only, sorted servers, unknown keys kept.
//!
//! `mcp.json` lives at the root of every store scope (`<project>/.agentuna/mcp.json`,
//! `~/.agentuna/mcp.json`, `<project>/.agentuna/local/mcp.json`). It carries only the server
//! definitions: `{ schema_version: 1, servers: { <slug>: <McpServerDef> } }`. Item identity,
//! kind, scope and path live in `store.json` (contract decision 3); the key in `servers` is the
//! slug and the `ItemId` comes from the manifest of the containing store.
//!
//! The document is written with two-space indentation and one trailing newline. Keys sort
//! by slug. Unknown top-level keys are preserved verbatim across load and save. Writes go
//! through [`at_fs::apply`] with [`ExpectedState::Hash`] so an external edit between load and
//! save is refused instead of overwritten, and every save of an existing file produces a
//! backup when a [`BackupPolicy::OperationFolder`] is supplied by the caller.
//!
//! ```
//! use at_store::layout::StoreLayout;
//! use at_store::mcp_store::{McpStore, McpStoreError};
//! use at_model::Scope;
//!
//! let layout = StoreLayout::new(Scope::User, "/tmp/store");
//! let loaded = McpStore::load(&layout)?;
//! assert!(loaded.store.is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use at_fs::{
    BackupMeta, BackupPolicy, ExpectedState, WriteMode, WriteOutcome, WriteRequest, apply,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use at_model::McpServerDef;

use crate::layout::{MCP_FILE, SCHEMA_VERSION, StoreLayout};
use crate::manifest::{ContentHash, MCP_IDENTITY_KEYS};

/// Largest `mcp.json` this build reads or writes (1 MiB).
pub const MCP_STORE_MAX_BYTES: u64 = 1024 * 1024;

/// Largest number of server entries a document may hold.
pub const MCP_STORE_MAX_SERVERS: usize = 1024;

/// The canonical `mcp.json` document: schema version plus a slug-keyed server map.
///
/// Unknown top-level keys are captured in `extra` and re-emitted verbatim on save, so an
/// editor or another tool that adds a key does not lose it.
#[derive(Debug, Clone, PartialEq)]
pub struct McpStore {
    /// Document version; always [`SCHEMA_VERSION`] on a well-formed store.
    pub schema_version: u32,
    /// Server definitions keyed by slug, sorted by slug.
    pub servers: BTreeMap<String, McpServerDef>,
    /// Unknown top-level keys preserved verbatim.
    pub extra: Map<String, Value>,
}

/// A loaded `mcp.json` together with the hash of the exact bytes it was read from.
///
/// The hash is what [`McpStore::save`] presents as [`ExpectedState::Hash`] so an external
/// edit between load and save is detected instead of overwritten.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedMcpStore {
    /// Parsed and validated document.
    pub store: McpStore,
    /// SHA-256 of the bytes read from `mcp.json`.
    pub hash: ContentHash,
    /// Whether the file was absent when loaded; use [`ExpectedState::Absent`] on save.
    pub absent: bool,
}

/// What a successful `mcp.json` save did.
#[derive(Debug, Clone, PartialEq)]
pub struct McpSaveOutcome {
    /// SHA-256 of the persisted document.
    pub hash: ContentHash,
    /// The guarded write that installed it.
    pub write: WriteOutcome,
}

impl McpStore {
    /// An empty store: no servers, no extra keys.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            servers: BTreeMap::new(),
            extra: Map::new(),
        }
    }

    /// Whether the store holds no server definitions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// The definition of `slug`, if present.
    #[must_use]
    pub fn get(&self, slug: &str) -> Option<&McpServerDef> {
        self.servers.get(slug)
    }

    /// Inserts or replaces the definition for `slug`.
    pub fn set(&mut self, slug: impl Into<String>, def: McpServerDef) {
        self.servers.insert(slug.into(), def);
    }

    /// Removes the definition for `slug`, returning it when present.
    pub fn remove(&mut self, slug: &str) -> Option<McpServerDef> {
        self.servers.remove(slug)
    }

    /// Serializes the document to its canonical bytes.
    ///
    /// Keys sort by slug; `schema_version` and `servers` come first, then any unknown
    /// top-level keys in their original order. The text is pretty-printed with two spaces
    /// and ends in one newline.
    ///
    /// # Errors
    /// [`McpStoreError::DocumentTooLarge`] when the result exceeds [`MCP_STORE_MAX_BYTES`],
    /// or [`McpStoreError::Json`] when a value cannot be serialized.
    pub fn to_bytes(&self) -> Result<Vec<u8>, McpStoreError> {
        let mut object = Map::new();
        object.insert(
            "schema_version".to_owned(),
            Value::from(self.schema_version),
        );
        let mut servers = Map::new();
        for (slug, def) in &self.servers {
            servers.insert(slug.clone(), serde_json::to_value(def)?);
        }
        object.insert("servers".to_owned(), Value::Object(servers));
        for (key, value) in &self.extra {
            object.insert(key.clone(), value.clone());
        }
        let mut text = serde_json::to_string_pretty(&Value::Object(object))?;
        text.push('\n');
        let bytes = text.into_bytes();
        if bytes.len() as u64 > MCP_STORE_MAX_BYTES {
            return Err(McpStoreError::DocumentTooLarge {
                bytes: bytes.len() as u64,
                limit: MCP_STORE_MAX_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Parses and validates `mcp.json` bytes without touching the filesystem.
    ///
    /// A missing `servers` key is treated as empty; a `servers` value that is not an object
    /// is an error. Unknown top-level keys are kept. Server entries that fail to parse as
    /// [`McpServerDef`] are an error.
    ///
    /// # Errors
    /// [`McpStoreError::Json`], [`McpStoreError::UnsupportedStoreVersion`],
    /// [`McpStoreError::InvalidDocument`], [`McpStoreError::TooManyServers`], or
    /// [`McpStoreError::IdentityKey`] when a server entry carries an identity key.
    pub fn parse(bytes: &[u8]) -> Result<Self, McpStoreError> {
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(Self::empty());
        }
        let value: Value = serde_json::from_slice(bytes)?;
        let object = value.as_object().ok_or(McpStoreError::InvalidDocument {
            reason: "top level is not an object".to_owned(),
        })?;

        let schema_version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(McpStoreError::InvalidDocument {
                reason: "missing or invalid `schema_version`".to_owned(),
            })?;
        if schema_version > SCHEMA_VERSION {
            return Err(McpStoreError::UnsupportedStoreVersion {
                found: schema_version,
                supported: SCHEMA_VERSION,
            });
        }

        let mut servers = BTreeMap::new();
        match object.get("servers") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                if map.len() > MCP_STORE_MAX_SERVERS {
                    return Err(McpStoreError::TooManyServers {
                        count: map.len(),
                        limit: MCP_STORE_MAX_SERVERS,
                    });
                }
                for (slug, entry) in map {
                    if let Some(key) = mcp_identity_key_in(entry) {
                        return Err(McpStoreError::IdentityKey {
                            slug: slug.clone(),
                            key: key.to_owned(),
                        });
                    }
                    let def: McpServerDef =
                        serde_json::from_value(entry.clone()).map_err(|error| {
                            McpStoreError::InvalidDocument {
                                reason: format!("server `{slug}`: {error}"),
                            }
                        })?;
                    servers.insert(slug.clone(), def);
                }
            }
            Some(_) => {
                return Err(McpStoreError::InvalidDocument {
                    reason: "`servers` is not an object".to_owned(),
                });
            }
        }

        let mut extra = Map::new();
        for (key, value) in object {
            if key != "schema_version" && key != "servers" {
                extra.insert(key.clone(), value.clone());
            }
        }

        Ok(Self {
            schema_version,
            servers,
            extra,
        })
    }

    /// Loads `mcp.json` of `layout`.
    ///
    /// A missing file returns an empty store with a hash of the empty bytes; no file is
    /// created. The returned hash is the observation a later [`Self::save`] must present.
    ///
    /// # Errors
    /// [`McpStoreError::Read`], [`McpStoreError::NotRegularFile`],
    /// [`McpStoreError::TooLarge`], [`McpStoreError::Changed`], [`McpStoreError::Json`],
    /// [`McpStoreError::UnsupportedStoreVersion`], [`McpStoreError::InvalidDocument`],
    /// [`McpStoreError::TooManyServers`] or [`McpStoreError::IdentityKey`].
    pub fn load(layout: &StoreLayout) -> Result<LoadedMcpStore, McpStoreError> {
        let path = layout.mcp_file();
        let (bytes, absent) = match read_mcp_file(&path) {
            Ok(bytes) => (bytes, false),
            Err(error) if error.is_missing() => (Vec::new(), true),
            Err(error) => return Err(error),
        };
        let hash = ContentHash::from_bytes(&bytes);
        let store = Self::parse(&bytes)?;
        Ok(LoadedMcpStore {
            store,
            hash,
            absent,
        })
    }

    /// Writes the document through the `at-fs` guarded write pipeline.
    ///
    /// `expected` must be the observation from [`Self::load`] (or [`ExpectedState::Absent`]
    /// for a new file); a concurrent writer makes the save fail with
    /// [`McpStoreError::Write`] and leaves the external bytes in place. `backup` selects
    /// where the replaced bytes are copied; pass [`BackupPolicy::OperationFolder`] with the
    /// operation folder supplied by the caller so every save of an existing file produces a
    /// backup entry.
    ///
    /// # Errors
    /// [`McpStoreError::DocumentTooLarge`], [`McpStoreError::Json`], or
    /// [`McpStoreError::Write`] from the pipeline.
    pub fn save(
        &self,
        layout: &StoreLayout,
        expected: ExpectedState,
        backup: BackupPolicy,
    ) -> Result<McpSaveOutcome, McpStoreError> {
        let bytes = self.to_bytes()?;
        let hash = ContentHash::from_bytes(&bytes);
        let path = layout.mcp_file();
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
                relpath: Some(MCP_FILE.to_owned()),
                operation_id: None,
                mode: None,
            },
            guard: None,
        };
        let write = apply(&request).map_err(|source| McpStoreError::Write { path, source })?;
        Ok(McpSaveOutcome { hash, write })
    }
}

/// Returns the first top-level identity key in a `mcp.json` server definition, if any.
///
/// `mcp.json` is content only; item identity lives in `store.json` alone. This mirrors
/// [`crate::manifest::mcp_identity_key`] but is applied to a nested value so the caller can
/// report which slug is at fault.
fn mcp_identity_key_in(value: &Value) -> Option<&'static str> {
    let object = value.as_object()?;
    MCP_IDENTITY_KEYS
        .iter()
        .copied()
        .find(|key| object.contains_key(*key))
}

fn read_mcp_file(path: &Path) -> Result<Vec<u8>, McpStoreError> {
    let meta = fs::symlink_metadata(path).map_err(|source| McpStoreError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(McpStoreError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    if meta.len() > MCP_STORE_MAX_BYTES {
        return Err(McpStoreError::TooLarge {
            path: path.to_path_buf(),
            bytes: meta.len(),
            limit: MCP_STORE_MAX_BYTES,
        });
    }
    let mut file = fs::File::open(path).map_err(|source| McpStoreError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let opened = file.metadata().map_err(|source| McpStoreError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    use std::os::unix::fs::MetadataExt;
    if opened.dev() != meta.dev() || opened.ino() != meta.ino() {
        return Err(McpStoreError::Changed {
            path: path.to_path_buf(),
        });
    }
    let capacity = usize::try_from(meta.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(MCP_STORE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| McpStoreError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MCP_STORE_MAX_BYTES {
        return Err(McpStoreError::TooLarge {
            path: path.to_path_buf(),
            bytes: bytes.len() as u64,
            limit: MCP_STORE_MAX_BYTES,
        });
    }
    Ok(bytes)
}

/// Why an `mcp.json` document could not be read, parsed or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpStoreError {
    /// The file could not be read.
    #[error("{}: could not read mcp.json: {source}", .path.display())]
    Read {
        /// Document path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// `mcp.json` is not a regular file (a directory or a symlink).
    #[error("{}: mcp.json is not a regular file", .path.display())]
    NotRegularFile {
        /// Document path.
        path: PathBuf,
    },
    /// The file is larger than [`MCP_STORE_MAX_BYTES`].
    #[error("{}: mcp.json is {bytes} bytes; the limit is {limit}", .path.display())]
    TooLarge {
        /// Document path.
        path: PathBuf,
        /// Observed size.
        bytes: u64,
        /// Limit.
        limit: u64,
    },
    /// The document grew past the limit while it was read.
    #[error("{}: mcp.json changed while it was read", .path.display())]
    Changed {
        /// Document path.
        path: PathBuf,
    },
    /// The canonical document would exceed [`MCP_STORE_MAX_BYTES`].
    #[error("mcp.json would be {bytes} bytes; the limit is {limit}")]
    DocumentTooLarge {
        /// Serialized size.
        bytes: u64,
        /// Limit.
        limit: u64,
    },
    /// The bytes are not valid JSON.
    #[error("mcp.json is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The document declares a schema version this build does not write.
    #[error(
        "mcp.json schema version {found} is newer than this AgenTuna supports (version {supported}); update AgenTuna to open this store"
    )]
    UnsupportedStoreVersion {
        /// Version found in the document.
        found: u32,
        /// Highest version this build supports.
        supported: u32,
    },
    /// The document is valid JSON but not a usable `mcp.json`.
    #[error("mcp.json is invalid: {reason}")]
    InvalidDocument {
        /// What is wrong with it.
        reason: String,
    },
    /// The document holds more server entries than [`MCP_STORE_MAX_SERVERS`].
    #[error("mcp.json holds {count} servers; the limit is {limit}")]
    TooManyServers {
        /// Observed count.
        count: usize,
        /// Limit.
        limit: usize,
    },
    /// A server entry carries an identity key that belongs to `store.json`.
    #[error("mcp.json server `{slug}` carries identity key `{key}`; identity lives in store.json")]
    IdentityKey {
        /// Slug of the offending server.
        slug: String,
        /// The identity key found.
        key: String,
    },
    /// The guarded write failed; nothing was overwritten.
    #[error("{}: could not write mcp.json: {source}", .path.display())]
    Write {
        /// Document path.
        path: PathBuf,
        /// Pipeline error.
        source: at_fs::FsError,
    },
}

impl McpStoreError {
    /// Whether the document is simply absent.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Read { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}

/// The [`ExpectedState`] a save must present for `loaded`.
///
/// Returns [`ExpectedState::Absent`] when the file was missing at load time, or
/// [`ExpectedState::Hash`] of the exact bytes otherwise.
#[must_use]
pub fn expected_state(loaded: &LoadedMcpStore) -> ExpectedState {
    if loaded.absent {
        ExpectedState::Absent
    } else {
        ExpectedState::Hash(loaded.hash.as_str().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_serializes_to_canonical_shape() {
        let store = McpStore::empty();
        let bytes = store.to_bytes().expect("serialize");
        assert_eq!(
            String::from_utf8(bytes).expect("utf8"),
            "{\n  \"schema_version\": 1,\n  \"servers\": {}\n}\n"
        );
    }

    #[test]
    fn parse_rejects_identity_key() {
        let bytes = br#"{"schema_version":1,"servers":{"demo":{"id":"itm_x","display_name":"Demo","transport":{"type":"stdio","command":"true","args":[],"cwd":null,"env":{}}}}}"#;
        let error = McpStore::parse(bytes).expect_err("identity key rejected");
        assert!(matches!(error, McpStoreError::IdentityKey { .. }));
    }

    #[test]
    fn parse_rejects_newer_schema() {
        let bytes = br#"{"schema_version":2,"servers":{}}"#;
        let error = McpStore::parse(bytes).expect_err("schema 2 rejected");
        assert!(matches!(
            error,
            McpStoreError::UnsupportedStoreVersion { .. }
        ));
    }

    #[test]
    fn parse_keeps_unknown_top_level_keys() {
        let bytes = br#"{"schema_version":1,"servers":{},"$schema":"https://example.com/schema.json","custom":{"a":1}}"#;
        let store = McpStore::parse(bytes).expect("parse");
        assert!(store.extra.contains_key("$schema"));
        assert!(store.extra.contains_key("custom"));
        let out = store.to_bytes().expect("serialize");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("\"$schema\""));
        assert!(text.contains("\"custom\""));
    }
}
