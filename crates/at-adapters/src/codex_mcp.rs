//! Codex MCP adapter: `[mcp_servers.<id>]` tables in `~/.codex/config.toml`.
//!
//! Codex keeps its MCP servers inside its own TOML config, next to unrelated tables
//! (`projects`, `features`, `plugins`, `marketplaces`). Every edit here is a `toml_edit`
//! edit of the parsed document, so comments, blank lines, key order and unrelated tables
//! survive byte-identically; only the keys the caller specifies are rewritten.
//!
//! The bytes then go to [`at_fs::apply`] with the hash the caller last saw, so a config
//! edited by Codex or by hand between the read and the write is refused, not overwritten.
//!
//! ```
//! use at_adapters::codex_mcp::{CodexConfigFile, CodexServerEntry};
//!
//! let source = "[mcp_servers.playwright]\ncommand = \"npx\"\n";
//! let mut config = CodexConfigFile::parse(source)?;
//! let mut entry = CodexServerEntry::default();
//! entry.command = Some("uvx".to_owned());
//! config.set_server("node_repl", &entry)?;
//! assert!(config.text().contains("[mcp_servers.node_repl]\ncommand = \"uvx\""));
//! # Ok::<(), at_adapters::codex_mcp::CodexError>(())
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use at_fs::{BackupPolicy, ExpectedState, FsError, WriteOutcome, WriteRequest, apply, sha256_hex};
use at_model::mcp::{EnvValue, McpTransport};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

/// Directory Codex owns inside a user's home directory.
pub const CODEX_HOME: &str = ".codex";

/// Config file name inside [`CODEX_HOME`].
pub const CONFIG_FILE: &str = "config.toml";

/// Root table that holds every MCP server.
const SERVERS_KEY: &str = "mcp_servers";

/// Largest config file this adapter reads or writes (8 MiB).
pub const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

/// Native config path for a user whose home directory is `home`.
///
/// This is a path only; nothing here proves Codex is installed at that home directory.
#[must_use]
pub fn config_path(home: &Path) -> PathBuf {
    home.join(CODEX_HOME).join(CONFIG_FILE)
}

/// One `[mcp_servers.<id>]` table, in the shape Codex stores it.
///
/// Every field is an optional patch: `Some` writes or replaces that key, `None` leaves an
/// existing key exactly as the file has it and omits the key in a new table. That is what
/// keeps an edit that changes only `command` from dropping `env_vars`, `enabled` or a key
/// AgenTuna does not model. To replace a whole server instead of patching it, remove it
/// with [`CodexConfigFile::remove_server`] first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexServerEntry {
    /// Executable to launch for a stdio server. Never a shell string.
    pub command: Option<String>,
    /// Arguments passed verbatim to `command`.
    pub args: Option<Vec<String>>,
    /// Environment entries. Values are stored as literal strings, never resolved values.
    pub env: Option<BTreeMap<String, EnvValue>>,
    /// Names of parent-process environment variables Codex forwards to the child.
    pub env_vars: Option<Vec<String>>,
    /// Working directory for a stdio server.
    pub cwd: Option<PathBuf>,
    /// Whether Codex should start this server.
    pub enabled: Option<bool>,
    /// Seconds Codex waits for the server to start.
    pub startup_timeout_sec: Option<u64>,
    /// Endpoint of a streamable-HTTP server.
    pub url: Option<String>,
}

impl CodexServerEntry {
    /// Projects a canonical transport onto Codex's TOML shape.
    ///
    /// Environment entries become strings: a literal is written as it is, and a secret
    /// reference is written as `${ENV_NAME}` text. The Keychain account, and therefore the
    /// value, never reach the file. Whether Codex expands `${NAME}` inside `env` is
    /// unverified, so every reference is reported as
    /// [`CodexFlag::UnverifiedInterpolation`] by [`CodexConfigFile::set_server`].
    ///
    /// HTTP headers and OAuth profiles cannot be expressed in this file; they are returned
    /// as [`CodexUnmodelled`] values instead of being written as keys Codex would ignore.
    #[must_use]
    pub fn from_transport(transport: &McpTransport) -> (Self, Vec<CodexUnmodelled>) {
        match transport {
            McpTransport::Stdio {
                command,
                args,
                cwd,
                env,
            } => (
                Self {
                    command: Some(command.clone()),
                    args: Some(args.clone()),
                    env: Some(env.clone()),
                    cwd: cwd.clone(),
                    ..Self::default()
                },
                Vec::new(),
            ),
            McpTransport::Http {
                url,
                headers,
                oauth,
            } => {
                let mut unmodelled = Vec::new();
                if !headers.is_empty() {
                    unmodelled.push(CodexUnmodelled::HttpHeaders {
                        count: headers.len(),
                    });
                }
                if oauth.is_some() {
                    unmodelled.push(CodexUnmodelled::OAuthProfile);
                }
                (
                    Self {
                        url: Some(url.as_str().to_owned()),
                        ..Self::default()
                    },
                    unmodelled,
                )
            }
        }
    }

    /// Flags for the `${NAME}` references this entry writes to `env`.
    fn interpolation_flags(&self, id: &str) -> Vec<CodexFlag> {
        let Some(env) = &self.env else {
            return Vec::new();
        };
        let mut names: BTreeSet<String> = BTreeSet::new();
        for value in env.values() {
            for name in interpolations(&env_text(value)) {
                names.insert(name.to_owned());
            }
        }
        names
            .into_iter()
            .map(|env_name| CodexFlag::UnverifiedInterpolation {
                server: id.to_owned(),
                env_name,
            })
            .collect()
    }

    /// Rejects a patch that would produce a table Codex cannot read.
    fn validate(&self, id: &str, existing: Option<&Table>) -> Result<(), CodexError> {
        match (self.command.is_some(), self.url.is_some()) {
            (true, true) => return Err(CodexError::AmbiguousTransport { id: id.into() }),
            (false, false) if existing.is_none() => {
                return Err(CodexError::MissingTransport { id: id.into() });
            }
            _ => {}
        }
        if let Some(table) = existing {
            if self.command.is_some() && table.contains_key("url") {
                return Err(CodexError::AmbiguousTransport { id: id.into() });
            }
            if self.url.is_some() && table.contains_key("command") {
                return Err(CodexError::AmbiguousTransport { id: id.into() });
            }
        }
        if self.url.as_deref().is_some_and(|url| !is_http_url(url)) {
            return Err(CodexError::InvalidUrl { id: id.into() });
        }
        if self
            .startup_timeout_sec
            .is_some_and(|seconds| i64::try_from(seconds).is_err())
        {
            return Err(CodexError::TimeoutOutOfRange { id: id.into() });
        }
        if self.cwd.as_ref().is_some_and(|cwd| cwd.to_str().is_none()) {
            return Err(CodexError::NonUtf8Path { id: id.into() });
        }
        Ok(())
    }
}

/// A canonical setting Codex's `config.toml` cannot express.
///
/// The adapter reports it instead of writing a key Codex would ignore; the caller decides
/// whether to drop it with a warning or refuse the save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodexUnmodelled {
    /// HTTP request headers. `config.toml` has no header table for a server here.
    HttpHeaders {
        /// Number of headers that were not written.
        count: usize,
    },
    /// OAuth profile. Codex keeps its own credentials outside `config.toml`.
    OAuthProfile,
}

/// Something about a server that AgenTuna cannot verify from the file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CodexFlag {
    /// The value is `${NAME}` text. Codex expanding `${NAME}` inside `env` is unverified,
    /// so the caller must not promise that the child process receives the secret.
    UnverifiedInterpolation {
        /// Server the value belongs to.
        server: String,
        /// Environment variable name inside `${...}`.
        env_name: String,
    },
}

/// One server read from the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexServer {
    /// Table key under `[mcp_servers]`, with nested keys joined by `.`.
    pub id: String,
    /// Fields present in the table; `None` means the key is absent.
    pub entry: CodexServerEntry,
    /// What the file expresses but AgenTuna cannot verify.
    pub flags: Vec<CodexFlag>,
}

/// What [`CodexConfigFile::save`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexSave {
    /// The rendered document equals the bytes on disk, so nothing was written.
    Unchanged,
    /// The config file was replaced through [`at_fs::apply`].
    Written(WriteOutcome),
}

impl CodexSave {
    /// The write result, or `None` when nothing was written.
    #[must_use]
    pub fn outcome(&self) -> Option<&WriteOutcome> {
        match self {
            Self::Unchanged => None,
            Self::Written(outcome) => Some(outcome),
        }
    }
}

/// Why a Codex config could not be read, edited or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodexError {
    /// The file exists but could not be read.
    #[error("{}: cannot read Codex config: {source}", .path.display())]
    Read {
        /// Path that was read.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid UTF-8.
    #[error("{}: Codex config is not valid UTF-8", .path.display())]
    NotUtf8 {
        /// Path that was read.
        path: PathBuf,
    },
    /// The file is above [`MAX_CONFIG_BYTES`].
    #[error("{}: {size} bytes exceeds the {limit} byte limit", .path.display())]
    TooLarge {
        /// Path that was read.
        path: PathBuf,
        /// Observed size.
        size: u64,
        /// Limit.
        limit: u64,
    },
    /// The document is not valid TOML.
    #[error("invalid TOML: {0}")]
    InvalidToml(#[from] toml_edit::TomlError),
    /// `mcp_servers` exists but is not a table.
    #[error("`mcp_servers` must be a table, found {found}")]
    RootNotTable {
        /// What the key holds.
        found: String,
    },
    /// A server id resolves to something that is not a table.
    #[error("mcp server `{server}` must be a table, found {found}")]
    EntryNotTable {
        /// Server id.
        server: String,
        /// What the key holds.
        found: String,
    },
    /// A key has the wrong type for the field it must hold.
    #[error("mcp server `{server}`: `{key}` must be {expected}")]
    WrongType {
        /// Server id.
        server: String,
        /// Key inside the server table.
        key: &'static str,
        /// What the key must hold.
        expected: &'static str,
    },
    /// The server id is empty.
    #[error("mcp server id is empty")]
    EmptyServerId,
    /// The server id contains characters a TOML table key cannot hold.
    #[error("mcp server id `{0}` contains control characters")]
    ControlCharacterInId(String),
    /// The patch sets both `command` and `url`, or would add one next to the other.
    #[error("mcp server `{id}` sets both `command` and `url`, which Codex cannot start")]
    AmbiguousTransport {
        /// Server id.
        id: String,
    },
    /// The patch would create a server with neither `command` nor `url`.
    #[error("mcp server `{id}` needs a `command` or a `url`")]
    MissingTransport {
        /// Server id.
        id: String,
    },
    /// `url` is not an absolute http(s) URL. The value is not echoed: a URL can carry a token.
    #[error("mcp server `{id}`: `url` is not an absolute http(s) URL")]
    InvalidUrl {
        /// Server id.
        id: String,
    },
    /// `startup_timeout_sec` does not fit the integer TOML can store.
    #[error("mcp server `{id}`: `startup_timeout_sec` is out of range")]
    TimeoutOutOfRange {
        /// Server id.
        id: String,
    },
    /// `cwd` is not valid UTF-8 and cannot be stored in a TOML string.
    #[error("mcp server `{id}`: `cwd` is not valid UTF-8")]
    NonUtf8Path {
        /// Server id.
        id: String,
    },
    /// The guarded write was refused.
    #[error(transparent)]
    Write(#[from] FsError),
}

/// A parsed Codex `config.toml` plus the state a write must still find on disk.
///
/// Parsing keeps every byte the parser can attribute; edits only touch the keys named in
/// [`CodexServerEntry`]. A document that was loaded and not edited renders back to exactly
/// the bytes it was read from.
#[derive(Debug, Clone)]
pub struct CodexConfigFile {
    doc: DocumentMut,
    expected: ExpectedState,
    dirty: bool,
    newline: Newline,
}

/// Line ending of a config document.
///
/// `toml_edit` renders newly inserted tables with LF. A file whose every newline is CRLF is
/// parsed with LF and rendered back with CRLF, so an edit never mixes line endings and a
/// load-without-edit still round-trips byte-identically. A file with mixed endings is left as
/// the parser sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Newline {
    Lf,
    Crlf,
}

impl Newline {
    /// CRLF only when the document has at least one CRLF and no bare LF anywhere.
    fn of(text: &str) -> Self {
        let mut saw_crlf = false;
        for (index, _) in text.match_indices('\n') {
            if index > 0 && text.as_bytes()[index - 1] == b'\r' {
                saw_crlf = true;
            } else {
                return Self::Lf;
            }
        }
        if saw_crlf { Self::Crlf } else { Self::Lf }
    }

    /// Parses `text`, normalising CRLF first when the document uses it.
    fn parse(self, text: &str) -> Result<DocumentMut, toml_edit::TomlError> {
        match self {
            Self::Lf => text.parse::<DocumentMut>(),
            Self::Crlf => text.replace("\r\n", "\n").parse::<DocumentMut>(),
        }
    }

    /// Rendered text in the document's own line endings.
    fn rendered(self, text: String) -> String {
        match self {
            Self::Lf => text,
            Self::Crlf => text.replace('\n', "\r\n"),
        }
    }
}

impl Default for CodexConfigFile {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexConfigFile {
    /// An empty document. Saving it expects the file to be absent.
    #[must_use]
    pub fn new() -> Self {
        Self {
            doc: DocumentMut::new(),
            expected: ExpectedState::Absent,
            dirty: false,
            newline: Newline::Lf,
        }
    }

    /// Parses config text from memory.
    ///
    /// The recorded expectation is "absent", so [`Self::save`] refuses to overwrite an
    /// existing file; use [`Self::load`] for a file that already exists, or
    /// [`Self::save_expecting`] with the hash the caller knows.
    ///
    /// # Errors
    /// [`CodexError::InvalidToml`] when the text is not valid TOML.
    pub fn parse(text: &str) -> Result<Self, CodexError> {
        let newline = Newline::of(text);
        Ok(Self {
            doc: newline.parse(text)?,
            expected: ExpectedState::Absent,
            dirty: false,
            newline,
        })
    }

    /// Reads a config file, recording the hash the write must still find.
    ///
    /// A missing file is not an error: it yields an empty document that expects the file to
    /// be absent, so the first save creates it.
    ///
    /// # Errors
    /// [`CodexError::Read`], [`CodexError::TooLarge`], [`CodexError::NotUtf8`] or
    /// [`CodexError::InvalidToml`].
    pub fn load(path: &Path) -> Result<Self, CodexError> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(source) => {
                return Err(CodexError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(CodexError::TooLarge {
                path: path.to_path_buf(),
                size: metadata.len(),
                limit: MAX_CONFIG_BYTES,
            });
        }
        let bytes = fs::read(path).map_err(|source| CodexError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let text = core::str::from_utf8(&bytes).map_err(|_| CodexError::NotUtf8 {
            path: path.to_path_buf(),
        })?;
        let newline = Newline::of(text);
        Ok(Self {
            doc: newline.parse(text)?,
            expected: ExpectedState::Hash(sha256_hex(&bytes)),
            dirty: false,
            newline,
        })
    }

    /// The current document text. Equal to the source bytes while no edit was applied.
    #[must_use]
    pub fn text(&self) -> String {
        self.newline.rendered(self.doc.to_string())
    }

    /// What the next [`Self::save`] requires the file to look like.
    #[must_use]
    pub fn expected(&self) -> &ExpectedState {
        &self.expected
    }

    /// Whether an edit changed something since load or parse.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Every server table, in file order.
    ///
    /// A nested header such as `[mcp_servers.org.team]` is one server with id `org.team`,
    /// because that is the key path TOML gives it. Keys that are not tables are skipped:
    /// they are not server definitions and [`Self::set_server`] refuses to overwrite them.
    /// `env` is read both as an inline table and as a `[mcp_servers.<id>.env]` sub-table;
    /// new values are always written inline.
    ///
    /// # Errors
    /// [`CodexError::RootNotTable`] or [`CodexError::WrongType`] for a field the adapter
    /// cannot interpret.
    pub fn servers(&self) -> Result<Vec<CodexServer>, CodexError> {
        let mut servers = Vec::new();
        if let Some(root) = self.root()? {
            collect_servers(root, "", &mut servers)?;
        }
        Ok(servers)
    }

    /// The ids of every server table, in file order.
    ///
    /// # Errors
    /// The same errors as [`Self::servers`].
    pub fn server_ids(&self) -> Result<Vec<String>, CodexError> {
        Ok(self
            .servers()?
            .into_iter()
            .map(|server| server.id)
            .collect())
    }

    /// One server by id, or `None` when the table is absent.
    ///
    /// # Errors
    /// [`CodexError::RootNotTable`], [`CodexError::EntryNotTable`] or
    /// [`CodexError::WrongType`].
    pub fn server(&self, id: &str) -> Result<Option<CodexServer>, CodexError> {
        let Some(root) = self.root()? else {
            return Ok(None);
        };
        let Some(table) = find_table(root, id)? else {
            return Ok(None);
        };
        read_entry(id, table).map(Some)
    }

    /// Writes the fields of `entry` into the `[mcp_servers.<id>]` table, creating the table
    /// when it is absent.
    ///
    /// An existing table keeps its position, its comments and every key the patch does not
    /// name; a new table is appended after the last existing `mcp_servers` table, comment
    /// free, with keys in the order command, args, env, env_vars, cwd, enabled,
    /// startup_timeout_sec, url. A server id that needs quoting (it contains a dot, a quote
    /// or anything else outside a bare key) is stored as `[mcp_servers."id"]`, so a dot
    /// never produces a nested table.
    ///
    /// Returns the flags the caller must surface, e.g. an unverified `${NAME}` reference.
    ///
    /// # Errors
    /// [`CodexError::EmptyServerId`], [`CodexError::ControlCharacterInId`],
    /// [`CodexError::AmbiguousTransport`], [`CodexError::MissingTransport`],
    /// [`CodexError::InvalidUrl`], [`CodexError::TimeoutOutOfRange`],
    /// [`CodexError::NonUtf8Path`], [`CodexError::RootNotTable`] or
    /// [`CodexError::EntryNotTable`].
    pub fn set_server(
        &mut self,
        id: &str,
        entry: &CodexServerEntry,
    ) -> Result<Vec<CodexFlag>, CodexError> {
        validate_id(id)?;
        let flags = entry.interpolation_flags(id);
        let doc_empty = self.doc.as_table().is_empty();
        let root = self.created_root()?;
        match find_table_mut(root, id)? {
            Some(table) => {
                entry.validate(id, Some(table))?;
                fill_table(table, entry, id)?;
            }
            None => {
                entry.validate(id, None)?;
                let mut table = Table::new();
                fill_table(&mut table, entry, id)?;
                if !doc_empty {
                    table.decor_mut().set_prefix("\n");
                }
                root.insert(id, Item::Table(table));
            }
        }
        self.dirty = true;
        Ok(flags)
    }

    /// Removes the whole `[mcp_servers.<id>]` table.
    ///
    /// The table goes with the blank line that separates it from the block above it; the
    /// next header keeps its own line and its own leading comment. Returns `false` when no
    /// table with that id exists; an id that names a non-table value is refused with
    /// [`CodexError::EntryNotTable`] instead of being deleted.
    ///
    /// # Errors
    /// [`CodexError::RootNotTable`], [`CodexError::EntryNotTable`] or
    /// [`CodexError::EmptyServerId`].
    pub fn remove_server(&mut self, id: &str) -> Result<bool, CodexError> {
        validate_id(id)?;
        let Some(root) = self.existing_root_mut()? else {
            return Ok(false);
        };
        if find_table(root, id)?.is_none() {
            return Ok(false);
        }
        let removed = remove_key(root, id);
        if removed {
            self.dirty = true;
        }
        Ok(removed)
    }

    /// Writes the document through [`at_fs::apply`], expecting the state recorded by
    /// [`Self::load`].
    ///
    /// Nothing is written when the document still renders to the loaded bytes.
    ///
    /// # Errors
    /// [`CodexError::Write`] when the guarded write refuses: concurrent modification, a
    /// symlink that cannot be followed, a missing parent directory, or a refused overwrite.
    pub fn save(&mut self, path: &Path, backup: BackupPolicy) -> Result<CodexSave, CodexError> {
        let expected = self.expected.clone();
        self.save_expecting(path, expected, backup)
    }

    /// Like [`Self::save`], with an expectation the caller supplies.
    ///
    /// Use this when the caller read the file itself and holds the hash, e.g. a plan built
    /// from a preview rather than from this document.
    ///
    /// # Errors
    /// The same errors as [`Self::save`].
    pub fn save_expecting(
        &mut self,
        path: &Path,
        expected: ExpectedState,
        backup: BackupPolicy,
    ) -> Result<CodexSave, CodexError> {
        if !self.dirty {
            return Ok(CodexSave::Unchanged);
        }
        let new_bytes = self.text().into_bytes();
        let new_hash = sha256_hex(&new_bytes);
        if let ExpectedState::Hash(hash) = &expected
            && *hash == new_hash
        {
            // The file already holds these bytes: the document is clean from now on.
            self.expected = ExpectedState::Hash(new_hash);
            self.dirty = false;
            return Ok(CodexSave::Unchanged);
        }
        let new_len = new_bytes.len() as u64;
        if new_len > MAX_CONFIG_BYTES {
            return Err(CodexError::TooLarge {
                path: path.to_path_buf(),
                size: new_len,
                limit: MAX_CONFIG_BYTES,
            });
        }
        let mut request = match &expected {
            ExpectedState::Absent => WriteRequest::create(path, new_bytes),
            _ => WriteRequest::replace(path, new_bytes, expected),
        };
        request.backup = backup;
        // Adapter decision: `~/.codex/config.toml` is often symlinked into a dotfiles repo.
        // `at-fs` resolves the link once, validates that the target exists, and compares the
        // hash of the target, so a changed target is still refused.
        request.follow_symlink = true;
        let outcome = apply(&request)?;
        // The file now holds exactly these bytes; a second save is a no-op until the next edit.
        self.expected = ExpectedState::Hash(outcome.after_hash.clone());
        self.dirty = false;
        Ok(CodexSave::Written(outcome))
    }

    /// The `mcp_servers` table, when it exists and is a table.
    fn root(&self) -> Result<Option<&Table>, CodexError> {
        match self.doc.as_table().get(SERVERS_KEY) {
            None => Ok(None),
            Some(Item::Table(table)) => Ok(Some(table)),
            Some(item) => Err(CodexError::RootNotTable {
                found: item_kind(item).to_owned(),
            }),
        }
    }

    /// The `mcp_servers` table, or `None` when the document has none.
    fn existing_root_mut(&mut self) -> Result<Option<&mut Table>, CodexError> {
        if !self.doc.as_table().contains_key(SERVERS_KEY) {
            return Ok(None);
        }
        match self.doc.as_table_mut().get_mut(SERVERS_KEY) {
            Some(Item::Table(table)) => Ok(Some(table)),
            Some(item) => Err(CodexError::RootNotTable {
                found: item_kind(item).to_owned(),
            }),
            None => Ok(None),
        }
    }

    /// The `mcp_servers` table, created as an implicit table when absent.
    fn created_root(&mut self) -> Result<&mut Table, CodexError> {
        if !self.doc.as_table().contains_key(SERVERS_KEY) {
            let mut table = Table::new();
            table.set_implicit(true);
            self.doc
                .as_table_mut()
                .insert(SERVERS_KEY, Item::Table(table));
        }
        match self.doc.as_table_mut().get_mut(SERVERS_KEY) {
            Some(Item::Table(table)) => Ok(table),
            Some(item) => Err(CodexError::RootNotTable {
                found: item_kind(item).to_owned(),
            }),
            None => Err(CodexError::RootNotTable {
                found: "nothing".to_owned(),
            }),
        }
    }
}

/// Rejects ids that TOML or Codex cannot carry.
fn validate_id(id: &str) -> Result<(), CodexError> {
    if id.is_empty() {
        return Err(CodexError::EmptyServerId);
    }
    if id.chars().any(char::is_control) {
        return Err(CodexError::ControlCharacterInId(id.to_owned()));
    }
    Ok(())
}

/// `true` for an absolute `http`/`https` URL.
fn is_http_url(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
        Err(_) => false,
    }
}

/// The text stored in the file for one environment value.
fn env_text(value: &EnvValue) -> String {
    match value {
        EnvValue::Literal(text) => text.clone(),
        EnvValue::Secret(secret) => format!("${{{}}}", secret.env_name),
    }
}

/// Names referenced as `${NAME}` in `text`.
fn interpolations(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        let name = &after[..end];
        let mut chars = name.chars();
        if chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            names.push(name);
        }
        rest = &after[end + 1..];
    }
    names
}

/// The kind of a TOML item, for error messages.
fn item_kind(item: &Item) -> &'static str {
    match item {
        Item::None => "nothing",
        Item::Value(value) => value.type_name(),
        Item::Table(_) => "a table",
        Item::ArrayOfTables(_) => "an array of tables",
    }
}

/// Whether `table` is only a namespace for deeper tables, e.g. `[mcp_servers.org.team]`
/// parses as an implicit `org` table holding the real `team` server.
fn is_namespace(table: &Table) -> bool {
    table.is_implicit()
        && !table.is_empty()
        && table.iter().all(|(_, item)| matches!(item, Item::Table(_)))
}

/// Appends every server under `table` to `out`.
fn collect_servers(
    table: &Table,
    prefix: &str,
    out: &mut Vec<CodexServer>,
) -> Result<(), CodexError> {
    for (key, item) in table.iter() {
        let id = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        if let Item::Table(child) = item {
            if is_namespace(child) {
                collect_servers(child, &id, out)?;
            } else {
                out.push(read_entry(&id, child)?);
            }
        }
    }
    Ok(())
}

/// Finds a server table by id: an exact key first (so a quoted `a.b` wins), then the dotted
/// path TOML produces for a nested header.
fn find_table<'a>(root: &'a Table, id: &str) -> Result<Option<&'a Table>, CodexError> {
    if let Some(item) = root.get(id) {
        return match item {
            Item::Table(table) => Ok(Some(table)),
            other => Err(CodexError::EntryNotTable {
                server: id.to_owned(),
                found: item_kind(other).to_owned(),
            }),
        };
    }
    let mut current = root;
    for segment in id.split('.') {
        current = match current.get(segment) {
            None => return Ok(None),
            Some(Item::Table(next)) => next,
            Some(other) => {
                return Err(CodexError::EntryNotTable {
                    server: id.to_owned(),
                    found: item_kind(other).to_owned(),
                });
            }
        };
    }
    Ok(Some(current))
}

/// Mutable [`find_table`].
fn find_table_mut<'a>(root: &'a mut Table, id: &str) -> Result<Option<&'a mut Table>, CodexError> {
    if root.contains_key(id) {
        return direct_table_mut(root, id);
    }
    let mut current = root;
    for segment in id.split('.') {
        current = match current.get_mut(segment) {
            None => return Ok(None),
            Some(Item::Table(next)) => next,
            Some(other) => {
                return Err(CodexError::EntryNotTable {
                    server: id.to_owned(),
                    found: item_kind(other).to_owned(),
                });
            }
        };
    }
    Ok(Some(current))
}

/// The table stored under the exact key `id`, which may contain dots.
fn direct_table_mut<'a>(
    root: &'a mut Table,
    id: &str,
) -> Result<Option<&'a mut Table>, CodexError> {
    match root.get_mut(id) {
        Some(Item::Table(table)) => Ok(Some(table)),
        Some(other) => Err(CodexError::EntryNotTable {
            server: id.to_owned(),
            found: item_kind(other).to_owned(),
        }),
        None => Ok(None),
    }
}

/// Removes the table at `id`, including the nested form TOML gives a dotted header.
fn remove_key(root: &mut Table, id: &str) -> bool {
    if root.remove(id).is_some() {
        return true;
    }
    let segments: Vec<&str> = id.split('.').collect();
    let Some((last, parents)) = segments.split_last() else {
        return false;
    };
    let mut current = root;
    for segment in parents {
        current = match current.get_mut(segment) {
            Some(Item::Table(next)) => next,
            _ => return false,
        };
    }
    current.remove(last).is_some()
}

/// Reads one server table.
fn read_entry(id: &str, table: &Table) -> Result<CodexServer, CodexError> {
    let mut flags = Vec::new();
    let env = read_env(table, id, &mut flags)?;
    let entry = CodexServerEntry {
        command: read_string(table, id, "command")?,
        args: read_string_array(table, id, "args")?,
        env,
        env_vars: read_string_array(table, id, "env_vars")?,
        cwd: read_string(table, id, "cwd")?.map(PathBuf::from),
        enabled: read_bool(table, id, "enabled")?,
        startup_timeout_sec: read_timeout(table, id)?,
        url: read_string(table, id, "url")?,
    };
    Ok(CodexServer {
        id: id.to_owned(),
        entry,
        flags,
    })
}

fn read_string(
    table: &Table,
    server: &str,
    key: &'static str,
) -> Result<Option<String>, CodexError> {
    match table.get(key) {
        None => Ok(None),
        Some(Item::Value(Value::String(value))) => Ok(Some(value.value().clone())),
        Some(_) => Err(CodexError::WrongType {
            server: server.to_owned(),
            key,
            expected: "a string",
        }),
    }
}

fn read_string_array(
    table: &Table,
    server: &str,
    key: &'static str,
) -> Result<Option<Vec<String>>, CodexError> {
    let array = match table.get(key) {
        None => return Ok(None),
        Some(Item::Value(Value::Array(array))) => array,
        Some(_) => {
            return Err(CodexError::WrongType {
                server: server.to_owned(),
                key,
                expected: "an array of strings",
            });
        }
    };
    let mut items = Vec::with_capacity(array.len());
    for value in array.iter() {
        let Value::String(text) = value else {
            return Err(CodexError::WrongType {
                server: server.to_owned(),
                key,
                expected: "an array of strings",
            });
        };
        items.push(text.value().clone());
    }
    Ok(Some(items))
}

fn read_bool(table: &Table, server: &str, key: &'static str) -> Result<Option<bool>, CodexError> {
    match table.get(key) {
        None => Ok(None),
        Some(Item::Value(Value::Boolean(value))) => Ok(Some(*value.value())),
        Some(_) => Err(CodexError::WrongType {
            server: server.to_owned(),
            key,
            expected: "a boolean",
        }),
    }
}

fn read_timeout(table: &Table, server: &str) -> Result<Option<u64>, CodexError> {
    match table.get("startup_timeout_sec") {
        None => Ok(None),
        Some(Item::Value(Value::Integer(value))) => u64::try_from(*value.value())
            .map(Some)
            .map_err(|_| CodexError::WrongType {
                server: server.to_owned(),
                key: "startup_timeout_sec",
                expected: "a non-negative integer",
            }),
        Some(_) => Err(CodexError::WrongType {
            server: server.to_owned(),
            key: "startup_timeout_sec",
            expected: "a non-negative integer",
        }),
    }
}

/// Reads `env`, accepting both `env = { ... }` and a `[mcp_servers.<id>.env]` sub-table.
fn read_env(
    table: &Table,
    server: &str,
    flags: &mut Vec<CodexFlag>,
) -> Result<Option<BTreeMap<String, EnvValue>>, CodexError> {
    let wrong_type = || CodexError::WrongType {
        server: server.to_owned(),
        key: "env",
        expected: "a table of strings",
    };
    let mut env = BTreeMap::new();
    let mut push = |name: &str, text: &Value| -> Result<(), CodexError> {
        let Value::String(value) = text else {
            return Err(wrong_type());
        };
        push_interpolation_flags(server, value.value(), flags);
        env.insert(name.to_owned(), EnvValue::Literal(value.value().clone()));
        Ok(())
    };
    match table.get("env") {
        None => return Ok(None),
        Some(Item::Value(Value::InlineTable(inline))) => {
            for (name, value) in inline.iter() {
                push(name, value)?;
            }
        }
        Some(Item::Table(sub)) => {
            for (name, item) in sub.iter() {
                let Item::Value(value) = item else {
                    return Err(wrong_type());
                };
                push(name, value)?;
            }
        }
        Some(_) => return Err(wrong_type()),
    }
    Ok(Some(env))
}

fn push_interpolation_flags(server: &str, text: &str, flags: &mut Vec<CodexFlag>) {
    for reference in interpolations(text) {
        flags.push(CodexFlag::UnverifiedInterpolation {
            server: server.to_owned(),
            env_name: reference.to_owned(),
        });
    }
}

/// Writes the fields of `entry` into a table, keeping every other key where it is.
fn fill_table(table: &mut Table, entry: &CodexServerEntry, id: &str) -> Result<(), CodexError> {
    if let Some(command) = &entry.command {
        table.insert("command", Item::Value(Value::from(command.as_str())));
    }
    if let Some(args) = &entry.args {
        table.insert("args", Item::Value(string_array(args)));
    }
    if let Some(env) = &entry.env {
        let mut inline = InlineTable::new();
        for (name, value) in env {
            inline.insert(name.as_str(), Value::from(env_text(value)));
        }
        table.insert("env", Item::Value(Value::InlineTable(inline)));
    }
    if let Some(env_vars) = &entry.env_vars {
        table.insert("env_vars", Item::Value(string_array(env_vars)));
    }
    if let Some(cwd) = &entry.cwd {
        let cwd = cwd
            .as_path()
            .to_str()
            .ok_or_else(|| CodexError::NonUtf8Path { id: id.to_owned() })?;
        table.insert("cwd", Item::Value(Value::from(cwd)));
    }
    if let Some(enabled) = entry.enabled {
        table.insert("enabled", Item::Value(Value::from(enabled)));
    }
    if let Some(timeout) = entry.startup_timeout_sec {
        let value = i64::try_from(timeout)
            .map_err(|_| CodexError::TimeoutOutOfRange { id: id.to_owned() })?;
        table.insert("startup_timeout_sec", Item::Value(Value::from(value)));
    }
    if let Some(url) = &entry.url {
        table.insert("url", Item::Value(Value::from(url.as_str())));
    }
    Ok(())
}

fn string_array(items: &[String]) -> Value {
    let mut array = Array::new();
    for item in items {
        array.push(item.as_str());
    }
    Value::Array(array)
}
