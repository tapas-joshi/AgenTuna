//! omp MCP adapter: reads and writes `~/.omp/agent/mcp.json`.
//!
//! The document is the MCP surface of omp (oh-my-pi). Its shape is verified by the
//! schema omp itself publishes as `$id`:
//!
//! ```json
//! {
//!   "$schema": "https://…/mcp-schema.json",
//!   "mcpServers": {
//!     "name": {
//!       "type": "stdio",       // missing means stdio
//!       "command": "…",
//!       "args": ["…"],
//!       "env": {"NAME": "value"},
//!       "cwd": "…",
//!       "enabled": true
//!     },
//!     "remote": {"type": "http", "url": "https://…", "headers": {"A": "b"}, "enabled": true}
//!   }
//! }
//! ```
//!
//! # Mapped and opaque keys
//!
//! Modeled per server: `type`, `command`, `args`, `env`, `cwd`, `url`, `headers`, `enabled`.
//! Everything else — `timeout`, `requestIdFormat`, `instructions`, `auth`, `oauth` — is
//! carried verbatim in [`OmpServer::extra`] and is never rewritten or removed. Top-level
//! `$schema`, `disabledServers`, `enabledServers` and any unknown key are likewise preserved
//! byte for byte.
//!
//! omp's schema also lists a legacy `sse` transport. at-model models two transports, so an
//! `sse` entry is reported as [`OmpMcpError::UnsupportedTransport`] instead of being
//! converted: rewriting it as `http` would silently change how omp connects.
//!
//! # Format preservation
//!
//! Every edit is a patch of the jsonc-parser concrete syntax tree, not a re-serialisation:
//! only the value that changed is replaced, and the rest of the bytes, including indentation,
//! key order, comments, `$schema` and keys AgenTuna does not model, come back unchanged.
//! [`OmpMcpConfig::parse`] followed by [`OmpMcpConfig::render_unchanged`] is byte-identical
//! to the input.
//!
//! # Safety
//!
//! [`OmpMcpAdapter::save_edit`] writes through [`at_fs::apply`] with the hash of the bytes it
//! actually read, so a file another process changed in the meantime is a concurrency error
//! and nothing is written. New files are created exclusively, with mode `0644`.
//!
//! # Unverified
//!
//! Whether omp expands `${VAR}` in `env` values is not verified. Reads keep such values
//! verbatim as [`EnvValue::Literal`]; writes render [`EnvValue::Secret`] as `${NAME}` and
//! never a secret value.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use at_fs::{BackupMeta, BackupPolicy, ExpectedState, WriteOutcome, WriteRequest, apply};
use at_model::{EnvValue, McpTransport};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::Value;
use url::Url;

/// omp's agent directory, relative to the user's home directory.
pub const AGENT_DIR: &str = ".omp/agent";
/// File name of the MCP document inside [`AGENT_DIR`].
pub const MCP_FILE: &str = "mcp.json";
/// Skills directory that is a sibling of [`MCP_FILE`]; used only to locate the agent root.
pub const SKILLS_DIR: &str = "skills";
/// Top-level property holding the server map.
pub const SERVERS_KEY: &str = "mcpServers";
/// Top-level property holding the schema reference.
pub const SCHEMA_KEY: &str = "$schema";
/// Schema reference written into a new document when the caller has none to copy.
pub const DEFAULT_SCHEMA: &str = "https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/src/config/mcp-schema.json";
/// Permission bits of a newly created `mcp.json`.
pub const NEW_FILE_MODE: u32 = 0o644;

/// Per-server keys the adapter itself writes; never carried as opaque `extra` data.
const MANAGED_KEYS: &[&str] = &[
    "type", "command", "args", "env", "cwd", "url", "headers", "enabled",
];

/// The omp agent root under `home` (`<home>/.omp/agent`).
#[must_use]
pub fn agent_root(home: &Path) -> PathBuf {
    home.join(AGENT_DIR)
}

/// Path of the MCP document under `home`.
///
/// Read and write go through this path only; the `skills` sibling never holds MCP data.
#[must_use]
pub fn mcp_path(home: &Path) -> PathBuf {
    agent_root(home).join(MCP_FILE)
}

/// Path of the skills directory under `home` (`<home>/.omp/agent/skills`).
///
/// omp reads `managed-skills` before `skills`; resolving that order belongs to the skills
/// adapter (TA-025). Here the directory is a sibling used to locate the agent root.
#[must_use]
pub fn skills_path(home: &Path) -> PathBuf {
    agent_root(home).join(SKILLS_DIR)
}

/// The agent root when omp is installed under `home`.
///
/// Only the agent root directory is required: a missing `skills` directory is not an error
/// and does not make an installed omp look absent.
#[must_use]
pub fn detect(home: &Path) -> Option<PathBuf> {
    let root = agent_root(home);
    root.is_dir().then_some(root)
}

/// Why an omp `mcp.json` could not be read, patched or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OmpMcpError {
    /// The document is not valid JSONC.
    #[error("mcp.json is not valid JSONC: {message} (line {line}, column {column})")]
    Parse {
        /// 1-indexed line of the error.
        line: usize,
        /// 1-indexed column of the error.
        column: usize,
        /// Parser message; it never contains file content.
        message: String,
    },
    /// The top-level value is not a JSON object.
    #[error("mcp.json top-level value is not a JSON object")]
    RootNotObject,
    /// `mcpServers` exists but is not a JSON object.
    #[error("`mcpServers` is not a JSON object")]
    ServersNotObject,
    /// A server entry is not a JSON object.
    #[error("server `{server}` is not a JSON object")]
    ServerNotObject {
        /// Server name.
        server: String,
    },
    /// The transport type is one at-model does not model.
    #[error(
        "server `{server}` uses transport type `{kind}`; only stdio and http are modeled, so it is left untouched"
    )]
    UnsupportedTransport {
        /// Server name.
        server: String,
        /// Value found in the file.
        kind: String,
    },
    /// A stdio entry has no usable `command`.
    #[error("server `{server}` has no `command`")]
    MissingCommand {
        /// Server name.
        server: String,
    },
    /// An http entry has no usable `url`.
    #[error("server `{server}` has no `url`")]
    MissingUrl {
        /// Server name.
        server: String,
    },
    /// An http entry's `url` is not an absolute http(s) URL.
    #[error("server `{server}` has an invalid `url` (an absolute http or https URL is required)")]
    InvalidUrl {
        /// Server name.
        server: String,
    },
    /// A field holds a value of the wrong JSON type.
    #[error("server `{server}` field `{field}` must be {expected}")]
    UnexpectedValue {
        /// Server name.
        server: String,
        /// Field name.
        field: &'static str,
        /// What the field must hold.
        expected: &'static str,
    },
    /// The same server name appears twice in `mcpServers`.
    #[error("server `{server}` is defined more than once in `mcpServers`")]
    DuplicateServer {
        /// Server name.
        server: String,
    },
    /// A remove or patch named a server that is not in the document.
    #[error("server `{server}` is not in mcp.json")]
    UnknownServer {
        /// Server name.
        server: String,
    },
    /// A name omp's schema rejects (it allows `[A-Za-z0-9_.-]{1,100}`).
    #[error(
        "`{name}` is not a valid omp server name (allowed: letters, digits, `_`, `.`, `-`, up to 100)"
    )]
    InvalidServerName {
        /// Rejected name.
        name: String,
    },
    /// A modeled field has no representation in omp's document.
    #[error("`{field}` cannot be represented in omp's mcp.json and was not written")]
    UnsupportedField {
        /// Field that cannot be rendered.
        field: &'static str,
    },
    /// `$schema` exists but is not a string.
    #[error("`$schema` must be a string")]
    SchemaNotString,
    /// The path exists but is not a regular file.
    #[error("{}: not a regular file", .path.display())]
    NotAFile {
        /// Path involved.
        path: PathBuf,
    },
    /// The document exceeds [`at_fs::MAX_FILE_BYTES`].
    #[error("{}: {size} bytes exceeds the {limit} byte limit", .path.display())]
    TooLarge {
        /// Path involved.
        path: PathBuf,
        /// Observed size.
        size: u64,
        /// Limit.
        limit: u64,
    },
    /// The document is not UTF-8.
    #[error("{}: not valid UTF-8", .path.display())]
    NotUtf8 {
        /// Path involved.
        path: PathBuf,
    },
    /// Reading or adjusting permissions failed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The guarded write pipeline refused or failed.
    #[error(transparent)]
    Fs(#[from] at_fs::FsError),
    /// A secret reference cannot be emitted because omp does not verify `${VAR}` expansion.
    #[error(
        "server `{server}`: secret variable `{variable}` cannot be rendered as ${{VAR}}; omp does not verify ${{VAR}} expansion"
    )]
    UnverifiedSecretExpansion {
        /// Server name.
        server: String,
        /// Variable name.
        variable: String,
    },
}

/// One server entry as it appears in omp's `mcpServers` object.
#[derive(Debug, Clone, PartialEq)]
pub struct OmpServer {
    /// Key in `mcpServers`.
    pub name: String,
    /// Transport and its settings.
    pub transport: McpTransport,
    /// `enabled` flag; absent in the file means `true`.
    pub enabled: bool,
    /// Per-server keys AgenTuna does not model, carried verbatim.
    pub extra: BTreeMap<String, Value>,
}

impl OmpServer {
    /// A server with the given name, transport and `enabled` flag, no extra keys.
    #[must_use]
    pub fn new(name: impl Into<String>, transport: McpTransport, enabled: bool) -> Self {
        Self {
            name: name.into(),
            transport,
            enabled,
            extra: BTreeMap::new(),
        }
    }
}

/// A parsed, format-preserving view of one omp `mcp.json`.
///
/// Parsing keeps the original text, so a subsequent patch and the original hash always refer
/// to the same bytes. After a successful write, re-read the file instead of reusing this value.
#[derive(Debug, Clone, PartialEq)]
pub struct OmpMcpConfig {
    source: String,
    schema: Option<String>,
    servers: Vec<OmpServer>,
    extra: BTreeMap<String, Value>,
}

impl OmpMcpConfig {
    /// Parses `source` without touching the filesystem.
    ///
    /// A whitespace-only document is accepted as empty and patched into `{}` on the first
    /// write; omp itself would reject such a file, and refusing to read it would strand the
    /// user with no way to repair it.
    ///
    /// # Errors
    /// [`OmpMcpError`] when the document is not JSONC, is not an object, or holds a server
    /// entry the adapter cannot represent faithfully.
    pub fn parse(source: &str) -> Result<Self, OmpMcpError> {
        if source.trim().is_empty() {
            return Ok(Self {
                source: source.to_owned(),
                schema: None,
                servers: Vec::new(),
                extra: BTreeMap::new(),
            });
        }
        let root = parse_root(source)?;
        let object = root.object_value().ok_or(OmpMcpError::RootNotObject)?;

        let schema = match member(&object, SCHEMA_KEY) {
            None => None,
            Some(Value::String(schema)) => Some(schema),
            Some(_) => return Err(OmpMcpError::SchemaNotString),
        };

        let mut servers = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(prop) = object.get(SERVERS_KEY) {
            let map = prop.object_value().ok_or(OmpMcpError::ServersNotObject)?;
            for entry in map.properties() {
                let name = entry.decoded_name().ok_or(OmpMcpError::InvalidServerName {
                    name: String::from("<undecodable key>"),
                })?;
                // A set keeps duplicate detection linear: a large document can hold thousands
                // of servers, and the scan below used to be quadratic.
                if !seen.insert(name.clone()) {
                    return Err(OmpMcpError::DuplicateServer { server: name });
                }
                let object = entry
                    .object_value()
                    .ok_or_else(|| OmpMcpError::ServerNotObject {
                        server: name.clone(),
                    })?;
                servers.push(parse_server(&name, &object)?);
            }
        }

        let mut extra = BTreeMap::new();
        for prop in object.properties() {
            let Some(key) = prop.decoded_name() else {
                continue;
            };
            if key == SCHEMA_KEY || key == SERVERS_KEY {
                continue;
            }
            if let Some(value) = prop.to_serde_value() {
                extra.insert(key, value);
            }
        }

        Ok(Self {
            source: source.to_owned(),
            schema,
            servers,
            extra,
        })
    }

    /// The document text exactly as it was read.
    ///
    /// This is the state a write must expect to find; see [`OmpMcpAdapter::save_edit`].
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The `$schema` value, when the document declares one.
    #[must_use]
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// Server entries in file order.
    #[must_use]
    pub fn servers(&self) -> &[OmpServer] {
        &self.servers
    }

    /// One server by name.
    #[must_use]
    pub fn server(&self, name: &str) -> Option<&OmpServer> {
        self.servers.iter().find(|server| server.name == name)
    }

    /// Top-level keys other than `$schema` and `mcpServers`, carried verbatim.
    #[must_use]
    pub fn extra(&self) -> &BTreeMap<String, Value> {
        &self.extra
    }

    /// Renders the document with no edits.
    ///
    /// The result is byte-identical to [`OmpMcpConfig::source`]; the method exists to prove
    /// the parse-and-render round trip and to feed an unchanged save.
    ///
    /// # Errors
    /// [`OmpMcpError`] when the document does not parse.
    pub fn render_unchanged(&self) -> Result<String, OmpMcpError> {
        if self.source.trim().is_empty() {
            return Ok(self.source.clone());
        }
        Ok(parse_root(&self.source)?.to_string())
    }

    /// Adds `server`, or patches the existing entry of the same name.
    ///
    /// Only the properties that differ are touched: an `enabled: false` entry whose `args`
    /// change keeps `enabled: false` and every unknown key. Keys of the other transport are
    /// removed when the transport changes, and every other byte of the document is preserved.
    ///
    /// # Errors
    /// [`OmpMcpError`] when the server cannot be represented, its name is not valid for omp,
    /// or the document cannot be patched.
    pub fn upsert_server(&self, server: &OmpServer) -> Result<String, OmpMcpError> {
        validate_server(server)?;
        let root = parse_root(patch_base(&self.source))?;
        match self.server(&server.name) {
            Some(current) => {
                let servers = existing_servers(&root)?;
                let entry = servers
                    .get(&server.name)
                    .ok_or_else(|| OmpMcpError::UnknownServer {
                        server: server.name.clone(),
                    })?
                    .object_value()
                    .ok_or_else(|| OmpMcpError::ServerNotObject {
                        server: server.name.clone(),
                    })?;
                patch_entry(&entry, current, server)?;
            }
            None => {
                let servers = servers_for_insert(&root)?;
                servers.append(&server.name, entry_value(server)?);
            }
        }
        Ok(preserve_trailing_newline(&self.source, root.to_string()))
    }

    /// Removes the entry named `name`.
    ///
    /// # Errors
    /// [`OmpMcpError::UnknownServer`] when no such entry exists; the document is unchanged.
    pub fn remove_server(&self, name: &str) -> Result<String, OmpMcpError> {
        if self.server(name).is_none() {
            return Err(OmpMcpError::UnknownServer {
                server: name.to_owned(),
            });
        }
        let root = parse_root(patch_base(&self.source))?;
        let servers = existing_servers(&root)?;
        servers
            .get(name)
            .ok_or_else(|| OmpMcpError::UnknownServer {
                server: name.to_owned(),
            })?
            .remove();
        Ok(preserve_trailing_newline(&self.source, root.to_string()))
    }
}

/// The `$schema` declared by the first document in `paths` that has one.
///
/// omp reads a user-level `mcp.json` plus project `.omp/mcp.json`/`.mcp.json` documents.
/// When one of them is being created, a schema another already declares is the right header
/// to copy instead of inventing one. A file that cannot be read, parsed, or has no string
/// `$schema` is skipped, so a broken project document cannot block creation.
#[must_use]
pub fn schema_from(paths: &[PathBuf]) -> Option<String> {
    paths.iter().find_map(|path| {
        let source = std::fs::read_to_string(path).ok()?;
        schema_of(&source)
    })
}

fn schema_of(source: &str) -> Option<String> {
    let root = parse_root(source).ok()?;
    let object = root.object_value()?;
    match member(&object, SCHEMA_KEY)? {
        Value::String(schema) => Some(schema),
        _ => None,
    }
}

/// Reads and writes omp's MCP document.
///
/// The adapter is stateless: every call takes the path to act on, so tests and callers point
/// it at a sandbox path instead of the developer's home directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct OmpMcpAdapter;

impl OmpMcpAdapter {
    /// A new adapter handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Reads and parses `path`.
    ///
    /// Returns `Ok(None)` when nothing exists there, which is the signal to create the file.
    ///
    /// # Errors
    /// [`OmpMcpError`] when the path is not a regular file, too large, not UTF-8, or holds a
    /// document that does not parse.
    pub fn read(&self, path: &Path) -> Result<Option<OmpMcpConfig>, OmpMcpError> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(OmpMcpError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if !metadata.is_file() {
            return Err(OmpMcpError::NotAFile {
                path: path.to_path_buf(),
            });
        }
        if metadata.len() > at_fs::MAX_FILE_BYTES {
            return Err(OmpMcpError::TooLarge {
                path: path.to_path_buf(),
                size: metadata.len(),
                limit: at_fs::MAX_FILE_BYTES,
            });
        }
        let bytes = std::fs::read(path).map_err(|source| OmpMcpError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let source = String::from_utf8(bytes).map_err(|_| OmpMcpError::NotUtf8 {
            path: path.to_path_buf(),
        })?;
        OmpMcpConfig::parse(&source).map(Some)
    }

    /// The text of a new document: `$schema` plus an empty `mcpServers` object.
    ///
    /// `schema` is copied verbatim when the caller already knows one from another omp file;
    /// otherwise [`DEFAULT_SCHEMA`] is used.
    #[must_use]
    pub fn create_text(schema: Option<&str>) -> String {
        let schema = Value::String(schema.unwrap_or(DEFAULT_SCHEMA).to_owned()).to_string();
        format!("{{\n  \"$schema\": {schema},\n  \"mcpServers\": {{}}\n}}\n")
    }

    /// A parsed empty document, ready for [`OmpMcpConfig::upsert_server`].
    ///
    /// # Errors
    /// [`OmpMcpError`] when the generated text does not parse; it always does, so this is a
    /// defensive path only.
    pub fn create_document(schema: Option<&str>) -> Result<OmpMcpConfig, OmpMcpError> {
        OmpMcpConfig::parse(&Self::create_text(schema))
    }

    /// The guarded replacement for `config`'s bytes with `new_text`.
    ///
    /// The expected state is the hash of the text [`OmpMcpAdapter::read`] returned, so a file
    /// that changed since then produces [`at_fs::FsError::ConcurrentModification`] and is left
    /// at the other writer's version. Symlinks are followed: this is an agent config writer
    /// (integration decision 5).
    #[must_use]
    pub fn replacement_request(
        path: &Path,
        config: &OmpMcpConfig,
        new_text: &str,
        backup: BackupPolicy,
    ) -> WriteRequest {
        let mut request = WriteRequest::replace(
            path,
            new_text.as_bytes().to_vec(),
            ExpectedState::Hash(at_fs::sha256_hex(config.source().as_bytes())),
        );
        request.follow_symlink = true;
        request.backup = backup;
        request.backup_meta = BackupMeta::default();
        request
    }

    /// Applies an edited document with a stale-value check and an optional backup.
    ///
    /// # Errors
    /// [`OmpMcpError::Parse`] when `new_text` is not valid JSONC (nothing is written), or
    /// [`at_fs::FsError`] wrapped in [`OmpMcpError`].
    pub fn save_edit(
        &self,
        path: &Path,
        config: &OmpMcpConfig,
        new_text: &str,
        backup: BackupPolicy,
    ) -> Result<WriteOutcome, OmpMcpError> {
        ensure_syntax(new_text)?;
        let request = Self::replacement_request(path, config, new_text, backup);
        apply(&request).map_err(OmpMcpError::Fs)
    }

    /// Creates `path` exclusively with `text` and mode [`NEW_FILE_MODE`].
    ///
    /// # Errors
    /// [`OmpMcpError::Parse`] when `text` is not valid JSONC, [`at_fs::FsError::AlreadyExists`]
    /// when the file appeared since the caller looked, or [`OmpMcpError::Io`] when the
    /// permissions cannot be set.
    pub fn create(
        &self,
        path: &Path,
        text: &str,
        backup: BackupPolicy,
    ) -> Result<WriteOutcome, OmpMcpError> {
        ensure_syntax(text)?;
        let mut request = WriteRequest::create(path, text.as_bytes().to_vec());
        request.backup = backup;
        let outcome = apply(&request)?;
        set_mode(path, NEW_FILE_MODE)?;
        Ok(outcome)
    }

    /// Reads `path`, applies `server`, and writes the result in one call.
    ///
    /// A missing file is created with [`DEFAULT_SCHEMA`] and one write; an existing file keeps
    /// its `$schema` and every unrelated byte.
    ///
    /// # Errors
    /// [`OmpMcpError`] from reading, patching or writing.
    pub fn upsert_server(
        &self,
        path: &Path,
        server: &OmpServer,
        backup: BackupPolicy,
    ) -> Result<WriteOutcome, OmpMcpError> {
        match self.read(path)? {
            Some(config) => {
                let text = config.upsert_server(server)?;
                self.save_edit(path, &config, &text, backup)
            }
            None => {
                let config = Self::create_document(None)?;
                let text = config.upsert_server(server)?;
                self.create(path, &text, backup)
            }
        }
    }

    /// Reads `path`, removes `name`, and writes the result.
    ///
    /// # Errors
    /// [`OmpMcpError`] when the file is absent, unreadable or does not hold `name`.
    pub fn remove_server(
        &self,
        path: &Path,
        name: &str,
        backup: BackupPolicy,
    ) -> Result<WriteOutcome, OmpMcpError> {
        let config = self.read(path)?.ok_or_else(|| OmpMcpError::UnknownServer {
            server: name.to_owned(),
        })?;
        let text = config.remove_server(name)?;
        self.save_edit(path, &config, &text, backup)
    }
}

fn parse_root(source: &str) -> Result<CstRootNode, OmpMcpError> {
    CstRootNode::parse(source, &ParseOptions::default()).map_err(|error| OmpMcpError::Parse {
        line: error.line_display(),
        column: error.column_display(),
        message: error.to_string(),
    })
}

/// Refuses to write text that is not valid JSONC; a broken config is worse than no write.
fn ensure_syntax(text: &str) -> Result<(), OmpMcpError> {
    parse_root(text).map(|_| ())
}

/// The text a patch starts from: a blank document becomes an empty object.
fn patch_base(source: &str) -> &str {
    if source.trim().is_empty() {
        "{}"
    } else {
        source
    }
}

/// A patch base of `{}` has no trivia to carry, so the file's own line ending is put back.
fn preserve_trailing_newline(source: &str, mut text: String) -> String {
    if text.ends_with('\n') || !source.ends_with('\n') {
        return text;
    }
    if source.ends_with("\r\n") {
        text.push_str("\r\n");
    } else {
        text.push('\n');
    }
    text
}

fn member(object: &CstObject, name: &str) -> Option<Value> {
    object.get(name).and_then(|prop| prop.to_serde_value())
}

fn is_managed(key: &str) -> bool {
    MANAGED_KEYS.contains(&key)
}

fn parse_server(name: &str, entry: &CstObject) -> Result<OmpServer, OmpMcpError> {
    let kind = match member(entry, "type") {
        None => String::from("stdio"),
        Some(Value::String(kind)) => kind,
        Some(_) => return Err(unexpected(name, "type", "a string")),
    };
    let enabled = match member(entry, "enabled") {
        None => true,
        Some(Value::Bool(enabled)) => enabled,
        Some(_) => return Err(unexpected(name, "enabled", "a boolean")),
    };
    let transport = match kind.as_str() {
        "stdio" => {
            let command = match member(entry, "command") {
                Some(Value::String(command)) if !command.is_empty() => command,
                Some(Value::String(_)) | None => {
                    return Err(OmpMcpError::MissingCommand {
                        server: name.to_owned(),
                    });
                }
                Some(_) => return Err(unexpected(name, "command", "a string")),
            };
            let args = match member(entry, "args") {
                None => Vec::new(),
                Some(Value::Array(values)) => values
                    .into_iter()
                    .map(|value| match value {
                        Value::String(arg) => Ok(arg),
                        _ => Err(unexpected(name, "args", "an array of strings")),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err(unexpected(name, "args", "an array of strings")),
            };
            let env = string_map(name, "env", member(entry, "env"))?;
            let cwd = match member(entry, "cwd") {
                None => None,
                Some(Value::String(dir)) => Some(PathBuf::from(dir)),
                Some(_) => return Err(unexpected(name, "cwd", "a string")),
            };
            McpTransport::Stdio {
                command,
                args,
                cwd,
                env,
            }
        }
        "http" => {
            let raw = match member(entry, "url") {
                Some(Value::String(url)) if !url.is_empty() => url,
                Some(Value::String(_)) | None => {
                    return Err(OmpMcpError::MissingUrl {
                        server: name.to_owned(),
                    });
                }
                Some(_) => return Err(unexpected(name, "url", "a string")),
            };
            let url = Url::parse(&raw).map_err(|_| OmpMcpError::InvalidUrl {
                server: name.to_owned(),
            })?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(OmpMcpError::InvalidUrl {
                    server: name.to_owned(),
                });
            }
            let headers = string_map(name, "headers", member(entry, "headers"))?;
            McpTransport::Http {
                url,
                headers,
                oauth: None,
            }
        }
        other => {
            return Err(OmpMcpError::UnsupportedTransport {
                server: name.to_owned(),
                kind: other.to_owned(),
            });
        }
    };

    let mut extra = BTreeMap::new();
    for prop in entry.properties() {
        let Some(key) = prop.decoded_name() else {
            continue;
        };
        if is_managed(&key) {
            continue;
        }
        if let Some(value) = prop.to_serde_value() {
            extra.insert(key, value);
        }
    }

    Ok(OmpServer {
        name: name.to_owned(),
        transport,
        enabled,
        extra,
    })
}

fn string_map(
    server: &str,
    field: &'static str,
    value: Option<Value>,
) -> Result<BTreeMap<String, EnvValue>, OmpMcpError> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Value::Object(map)) => map
            .into_iter()
            .map(|(key, value)| match value {
                Value::String(text) => Ok((key, EnvValue::Literal(text))),
                _ => Err(unexpected(server, field, "an object of string values")),
            })
            .collect(),
        Some(_) => Err(unexpected(server, field, "an object of string values")),
    }
}

fn unexpected(server: &str, field: &'static str, expected: &'static str) -> OmpMcpError {
    OmpMcpError::UnexpectedValue {
        server: server.to_owned(),
        field,
        expected,
    }
}

/// Validates everything the write path would otherwise turn into a document omp rejects.
fn validate_server(server: &OmpServer) -> Result<(), OmpMcpError> {
    let valid_name = !server.name.is_empty()
        && server.name.len() <= 100
        && server
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid_name {
        return Err(OmpMcpError::InvalidServerName {
            name: server.name.clone(),
        });
    }
    match &server.transport {
        McpTransport::Stdio { command, env, .. } => {
            if command.is_empty() {
                return Err(OmpMcpError::MissingCommand {
                    server: server.name.clone(),
                });
            }
            for value in env.values() {
                if let EnvValue::Secret(secret) = value
                    && !crate::secret_launch::verifies_var_expansion(&at_model::AgentId::Omp)
                {
                    return Err(OmpMcpError::UnverifiedSecretExpansion {
                        server: server.name.clone(),
                        variable: secret.env_name.clone(),
                    });
                }
            }
        }
        McpTransport::Http {
            url,
            oauth,
            headers,
            ..
        } => {
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(OmpMcpError::InvalidUrl {
                    server: server.name.clone(),
                });
            }
            if oauth.is_some() {
                return Err(OmpMcpError::UnsupportedField { field: "oauth" });
            }
            for value in headers.values() {
                if let EnvValue::Secret(secret) = value
                    && !crate::secret_launch::verifies_var_expansion(&at_model::AgentId::Omp)
                {
                    return Err(OmpMcpError::UnverifiedSecretExpansion {
                        server: server.name.clone(),
                        variable: secret.env_name.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Returns the `mcpServers` object, which must already exist.
fn existing_servers(root: &CstRootNode) -> Result<CstObject, OmpMcpError> {
    root.object_value()
        .ok_or(OmpMcpError::RootNotObject)?
        .get(SERVERS_KEY)
        .ok_or(OmpMcpError::ServersNotObject)?
        .object_value()
        .ok_or(OmpMcpError::ServersNotObject)
}

/// Returns the `mcpServers` object, creating it (and a multiline root) when needed.
///
/// A single-line root is expanded before a nested object goes in; otherwise the insertion
/// would leave `{"$schema": …, "mcpServers": {` on one line with the rest indented under it.
fn servers_for_insert(root: &CstRootNode) -> Result<CstObject, OmpMcpError> {
    let root_object = root.object_value().ok_or(OmpMcpError::RootNotObject)?;
    if !root_object
        .children()
        .iter()
        .any(|child| child.is_newline())
    {
        root_object.ensure_multiline();
    }
    let servers = match root_object.get(SERVERS_KEY) {
        Some(prop) => prop.object_value().ok_or(OmpMcpError::ServersNotObject)?,
        None => root_object.object_value_or_set(SERVERS_KEY),
    };
    servers.ensure_multiline();
    Ok(servers)
}

fn set_member(entry: &CstObject, key: &str, value: impl Into<CstInputValue>) {
    match entry.get(key) {
        Some(prop) => prop.set_value(value.into()),
        None => {
            entry.append(key, value.into());
        }
    }
}

fn remove_member(entry: &CstObject, key: &str) {
    if let Some(prop) = entry.get(key) {
        prop.remove();
    }
}

fn patch_entry(
    entry: &CstObject,
    current: &OmpServer,
    desired: &OmpServer,
) -> Result<(), OmpMcpError> {
    let kind_changed = current.transport.kind() != desired.transport.kind();
    if kind_changed {
        set_member(entry, "type", desired.transport.kind());
    }
    match &desired.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            if kind_changed {
                remove_member(entry, "url");
                remove_member(entry, "headers");
            }
            let (current_command, current_args, current_env, current_cwd) = match &current.transport
            {
                McpTransport::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                } => (Some(command), Some(args), Some(env), Some(cwd)),
                McpTransport::Http { .. } => (None, None, None, None),
            };
            if current_command.is_none_or(|value| value != command) {
                set_member(entry, "command", command.clone());
            }
            if current_args.is_none_or(|value| value != args) {
                if args.is_empty() {
                    remove_member(entry, "args");
                } else {
                    set_member(entry, "args", strings(args));
                }
            }
            if current_env.is_none_or(|value| value != env) {
                if env.is_empty() {
                    remove_member(entry, "env");
                } else {
                    set_member(entry, "env", string_object(env));
                }
            }
            if current_cwd.is_none_or(|value| value != cwd) {
                match cwd {
                    Some(dir) => set_member(entry, "cwd", dir.to_string_lossy().into_owned()),
                    None => remove_member(entry, "cwd"),
                }
            }
        }
        McpTransport::Http {
            url,
            headers,
            oauth,
        } => {
            if oauth.is_some() {
                return Err(OmpMcpError::UnsupportedField { field: "oauth" });
            }
            if kind_changed {
                remove_member(entry, "command");
                remove_member(entry, "args");
                remove_member(entry, "env");
                remove_member(entry, "cwd");
            }
            let (current_url, current_headers) = match &current.transport {
                McpTransport::Http { url, headers, .. } => (Some(url), Some(headers)),
                McpTransport::Stdio { .. } => (None, None),
            };
            if current_url.is_none_or(|value| value != url) {
                set_member(entry, "url", url.as_str());
            }
            if current_headers.is_none_or(|value| value != headers) {
                if headers.is_empty() {
                    remove_member(entry, "headers");
                } else {
                    set_member(entry, "headers", string_object(headers));
                }
            }
        }
    }

    if current.enabled != desired.enabled {
        set_member(entry, "enabled", desired.enabled);
    }

    for (key, value) in &desired.extra {
        if is_managed(key) {
            continue;
        }
        match current.extra.get(key) {
            Some(existing) if existing == value => {}
            Some(_) => set_member(entry, key, json_to_cst(value)),
            None => {
                entry.append(key, json_to_cst(value));
            }
        }
    }
    Ok(())
}

fn entry_value(server: &OmpServer) -> Result<CstInputValue, OmpMcpError> {
    let mut fields: Vec<(String, CstInputValue)> = Vec::new();
    fields.push((
        String::from("type"),
        CstInputValue::from(server.transport.kind()),
    ));
    match &server.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            fields.push((
                String::from("command"),
                CstInputValue::from(command.clone()),
            ));
            if !args.is_empty() {
                fields.push((String::from("args"), strings(args)));
            }
            if !env.is_empty() {
                fields.push((String::from("env"), string_object(env)));
            }
            if let Some(dir) = cwd {
                fields.push((
                    String::from("cwd"),
                    CstInputValue::from(dir.to_string_lossy().into_owned()),
                ));
            }
        }
        McpTransport::Http {
            url,
            headers,
            oauth,
        } => {
            if oauth.is_some() {
                return Err(OmpMcpError::UnsupportedField { field: "oauth" });
            }
            fields.push((String::from("url"), CstInputValue::from(url.as_str())));
            if !headers.is_empty() {
                fields.push((String::from("headers"), string_object(headers)));
            }
        }
    }
    fields.push((String::from("enabled"), CstInputValue::Bool(server.enabled)));
    for (key, value) in &server.extra {
        if !is_managed(key) {
            fields.push((key.clone(), json_to_cst(value)));
        }
    }
    Ok(CstInputValue::Object(fields))
}

fn strings(values: &[String]) -> CstInputValue {
    CstInputValue::Array(
        values
            .iter()
            .map(|value| CstInputValue::String(value.clone()))
            .collect(),
    )
}

/// Renders an environment/header map. Secret values become `${NAME}` references, never values.
fn string_object(map: &BTreeMap<String, EnvValue>) -> CstInputValue {
    CstInputValue::Object(
        map.iter()
            .map(|(key, value)| {
                let text = match value {
                    EnvValue::Literal(text) => text.clone(),
                    EnvValue::Secret(secret) => format!("${{{}}}", secret.env_name),
                };
                (key.clone(), CstInputValue::String(text))
            })
            .collect(),
    )
}

fn json_to_cst(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(flag) => CstInputValue::Bool(*flag),
        Value::Number(number) => CstInputValue::Number(number.to_string()),
        Value::String(text) => CstInputValue::String(text.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(json_to_cst).collect()),
        Value::Object(map) => CstInputValue::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_cst(value)))
                .collect(),
        ),
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), OmpMcpError> {
    at_fs::set_permissions(path, mode).map_err(|source| match source {
        at_fs::FsError::Io { path, source } => OmpMcpError::Io { path, source },
        other => OmpMcpError::Fs(other),
    })
}
