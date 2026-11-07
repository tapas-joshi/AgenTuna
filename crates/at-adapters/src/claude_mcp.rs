//! Claude Code MCP adapter for `~/.claude.json` and `.mcp.json`.
//!
//! Existing documents are edited through the JSONC CST so unrelated bytes are retained.
//! `${NAME}` interpolation is deliberately unverified and remains a literal on import.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use at_fs::{BackupPolicy, ExpectedState as FsExpectedState, WriteOutcome, WriteRequest, apply};
use at_model::{
    ChangeSet, EnvValue, ExpectedState, FileOp, McpTransport, OperationKind, SecretRef,
};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::Value;
use url::Url;

/// A Claude Code MCP server entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeServer {
    /// Name as stored in the server map.
    pub name: String,
    /// Transport and settings.
    pub transport: McpTransport,
    /// Unknown entry keys retained when read and patched.
    pub extra: BTreeMap<String, Value>,
}

/// Scope location of an MCP map within a Claude JSON document.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClaudeConfigScope {
    /// User-level root `mcpServers` map.
    User,
    /// Project map at `.projects[project_path].mcpServers`.
    Project {
        /// Absolute project directory used as the `.projects` key.
        project_path: String,
    },
    /// Repository-level `.mcp.json` root `mcpServers` map.
    ProjectFile,
}

/// Failures parsing or patching Claude's MCP configuration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClaudeMcpError {
    /// JSONC syntax error.
    #[error("invalid Claude MCP JSONC (line {line}, column {column}): {message}")]
    Parse {
        /// 1-based source line of the parse failure.
        line: usize,
        /// 1-based source column of the parse failure.
        column: usize,
        /// Parser diagnostic.
        message: String,
    },
    /// The selected server map is not an object.
    #[error("Claude MCP server map is not an object")]
    ServersNotObject,
    /// The same server name appears more than once, so neither entry is authoritative.
    #[error("Claude MCP server `{name}` is defined more than once")]
    DuplicateServer {
        /// Repeated server-map key.
        name: String,
    },
    /// A key the adapter edits appears more than once, so a patch could change the wrong value.
    #[error("Claude config repeats the key `{key}`")]
    DuplicateKey {
        /// Repeated object key.
        key: String,
    },
    /// A server entry is not an object.
    #[error("Claude MCP entry `{name}` is not an object")]
    EntryNotObject {
        /// The server-map key.
        name: String,
    },
    /// The name already exists, but it was not parsed as a managed Claude entry.
    #[error("refusing to overwrite unmanaged Claude MCP entry `{name}`")]
    UnmanagedEntry {
        /// Existing unmanaged server key.
        name: String,
    },
    /// The requested server does not exist in this document.
    #[error("Claude MCP server `{name}` does not exist")]
    UnknownServer {
        /// Missing server key.
        name: String,
    },
    /// Entry has a transport that this adapter does not model.
    #[error("Claude MCP entry `{name}` has unsupported transport `{kind}`")]
    UnsupportedTransport {
        /// The server-map key.
        name: String,
        /// Unsupported transport tag.
        kind: String,
    },
    /// A mapped field has the wrong type or is missing.
    #[error("Claude MCP entry `{name}` has invalid field `{field}`")]
    InvalidField {
        /// The server-map key.
        name: String,
        /// Name of the malformed field.
        field: &'static str,
    },
    /// Change-set construction failed.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// Filesystem operation failed.
    #[error(transparent)]
    Fs(#[from] at_fs::FsError),
    /// Input/output failed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying IO failure.
        #[source]
        source: io::Error,
    },
}

/// Parsed Claude MCP document retaining original JSONC source bytes.
#[derive(Debug, Clone)]
pub struct ClaudeMcpConfig {
    source: String,
    source_mtime: Option<SystemTime>,
    scope: ClaudeConfigScope,
    servers: BTreeMap<String, ClaudeServer>,
}

impl ClaudeMcpConfig {
    /// Parses a document in the given scope without assuming `${VAR}` interpolation support.
    ///
    /// Unmatched `${VAR}` strings remain literals; whether Claude expands these values is
    /// unverified and is not inferred from their syntax.
    pub fn parse(source: &str, scope: ClaudeConfigScope) -> Result<Self, ClaudeMcpError> {
        Self::parse_with_secret_refs(source, scope, &BTreeMap::new())
    }

    /// Parses a document and maps exact `${NAME}` values to supplied secret references.
    ///
    /// The caller provides known variable names and their Keychain accounts. No value is
    /// resolved or read here.
    pub fn parse_with_secret_refs(
        source: &str,
        scope: ClaudeConfigScope,
        refs: &BTreeMap<String, String>,
    ) -> Result<Self, ClaudeMcpError> {
        let root = parse_root(source)?;
        let map = server_object(&root, &scope)?;
        let mut servers = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for prop in map.properties() {
            let Some(name) = prop.decoded_name() else {
                continue;
            };
            if !seen.insert(name.clone()) {
                return Err(ClaudeMcpError::DuplicateServer { name });
            }
            let value = prop
                .to_serde_value()
                .ok_or_else(|| ClaudeMcpError::EntryNotObject { name: name.clone() })?;
            let Some(obj) = value.as_object() else {
                return Err(ClaudeMcpError::EntryNotObject { name });
            };
            let kind = obj.get("type").and_then(Value::as_str).unwrap_or("stdio");
            let transport = match kind {
                "stdio" => {
                    let command = obj.get("command").and_then(Value::as_str).ok_or_else(|| {
                        ClaudeMcpError::InvalidField {
                            name: name.clone(),
                            field: "command",
                        }
                    })?;
                    let args = strings(obj.get("args"), &name, "args")?;
                    let env = env_map(obj.get("env"), &name, "env")?;
                    let cwd = obj.get("cwd").and_then(Value::as_str).map(PathBuf::from);
                    McpTransport::Stdio {
                        command: command.to_owned(),
                        args,
                        cwd,
                        env,
                    }
                }
                "http" | "sse" => {
                    let raw = obj.get("url").and_then(Value::as_str).ok_or_else(|| {
                        ClaudeMcpError::InvalidField {
                            name: name.clone(),
                            field: "url",
                        }
                    })?;
                    let url = Url::parse(raw).map_err(|_| ClaudeMcpError::InvalidField {
                        name: name.clone(),
                        field: "url",
                    })?;
                    let headers = env_map(obj.get("headers"), &name, "headers")?;
                    McpTransport::Http {
                        url,
                        headers,
                        oauth: None,
                    }
                }
                other => {
                    return Err(ClaudeMcpError::UnsupportedTransport {
                        name,
                        kind: other.to_owned(),
                    });
                }
            };
            let extra = obj
                .iter()
                .filter(|(key, _)| {
                    !["type", "command", "args", "env", "cwd", "url", "headers"]
                        .contains(&key.as_str())
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            servers.insert(
                name.clone(),
                ClaudeServer {
                    name,
                    transport,
                    extra,
                },
            );
        }
        for server in servers.values_mut() {
            match &mut server.transport {
                McpTransport::Stdio { env, .. } => apply_secret_refs(env, refs),
                McpTransport::Http { headers, .. } => apply_secret_refs(headers, refs),
            }
        }
        Ok(Self {
            source: source.to_owned(),
            source_mtime: None,
            scope,
            servers,
        })
    }

    /// Original source, unchanged.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Parsed MCP servers in this document.
    #[must_use]
    pub fn servers(&self) -> &BTreeMap<String, ClaudeServer> {
        &self.servers
    }

    /// Returns original source unchanged for a no-op save.
    #[must_use]
    pub fn render_unchanged(&self) -> &str {
        &self.source
    }

    /// Adds a server, refusing a same-name entry that cannot be modeled.
    pub fn upsert(&self, server: &ClaudeServer) -> Result<String, ClaudeMcpError> {
        let root = parse_root(&self.source)?;
        let servers = server_object(&root, &self.scope)?;
        if let Some(existing) = self.servers.get(&server.name) {
            let prop = servers
                .get(&server.name)
                .ok_or_else(|| ClaudeMcpError::UnmanagedEntry {
                    name: server.name.clone(),
                })?;
            let entry = prop
                .object_value()
                .ok_or_else(|| ClaudeMcpError::EntryNotObject {
                    name: server.name.clone(),
                })?;
            patch_entry(&entry, existing, server);
        } else {
            if servers.get(&server.name).is_some() {
                return Err(ClaudeMcpError::UnmanagedEntry {
                    name: server.name.clone(),
                });
            }
            servers.append(&server.name, entry_value(server));
        }
        Ok(root.to_string())
    }

    /// Removes a modeled server while preserving all other JSONC bytes.
    ///
    /// # Errors
    /// Returns [`ClaudeMcpError::UnknownServer`] when the server is absent, or
    /// [`ClaudeMcpError::UnmanagedEntry`] when a same-name entry is not modeled.
    pub fn remove(&self, name: &str) -> Result<String, ClaudeMcpError> {
        let root = parse_root(&self.source)?;
        let servers = server_object(&root, &self.scope)?;
        let Some(entry) = servers.get(name) else {
            return Err(ClaudeMcpError::UnknownServer {
                name: name.to_owned(),
            });
        };
        if !self.servers.contains_key(name) {
            return Err(ClaudeMcpError::UnmanagedEntry {
                name: name.to_owned(),
            });
        }
        entry.remove();
        Ok(root.to_string())
    }

    /// Plans a server upsert as a validated `ChangeSet`; performs no filesystem writes.
    pub fn plan_upsert(
        &self,
        relative_path: &Path,
        server: &ClaudeServer,
    ) -> Result<ChangeSet, ClaudeMcpError> {
        let rendered = self.upsert(server)?;
        self.change_set(relative_path, &rendered)
    }

    /// Produces a reviewed single-file `ChangeSet` without performing IO.
    pub fn change_set(
        &self,
        relative_path: &Path,
        new_text: &str,
    ) -> Result<ChangeSet, ClaudeMcpError> {
        parse_root(new_text)?;
        let source_hash = at_fs::sha256_hex(self.source.as_bytes());
        let output_hash = at_fs::sha256_hex(new_text.as_bytes());
        let created_at_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let expected = match self.source_mtime {
            Some(mtime) => ExpectedState::hash_and_mtime(&source_hash, mtime)?,
            None => ExpectedState::hash(&source_hash)?,
        };
        let set = ChangeSet {
            id: format!("claude-{}", &output_hash[..16]),
            preview_id: source_hash.clone(),
            operation_kind: OperationKind::parse("edit_mcp")?,
            scope: None,
            project_id: None,
            summary: "Update Claude Code MCP configuration".to_owned(),
            ops: vec![FileOp::Write {
                path: relative_path.to_path_buf(),
                bytes: new_text.as_bytes().to_vec(),
                expected,
                follow_symlink: true,
                backup: true,
            }],
            created_at_unix_seconds,
        };
        set.validate()?;
        Ok(set)
    }

    /// Builds a guarded replacement request. Agent config paths follow symlinks by policy.
    #[must_use]
    pub fn replacement_request(
        &self,
        path: &Path,
        new_text: &str,
        backup: BackupPolicy,
    ) -> WriteRequest {
        let hash = at_fs::sha256_hex(self.source.as_bytes());
        let expected = match self.source_mtime {
            Some(mtime) => FsExpectedState::HashAndMtime { hash, mtime },
            None => FsExpectedState::Hash(hash),
        };
        let mut request = WriteRequest::replace(path, new_text.as_bytes().to_vec(), expected);
        request.follow_symlink = true;
        request.backup = backup;
        request
    }

    /// Applies an edit, rebasing once when a concurrent writer changed only unrelated bytes.
    pub fn save_edit(
        &self,
        path: &Path,
        new_text: &str,
        backup: BackupPolicy,
    ) -> Result<WriteOutcome, ClaudeMcpError> {
        parse_root(new_text)?;
        match apply(&self.replacement_request(path, new_text, backup.clone())) {
            Ok(outcome) => Ok(outcome),
            Err(conflict @ at_fs::FsError::ConcurrentModification { .. }) => {
                let latest_text =
                    fs::read_to_string(path).map_err(|source| ClaudeMcpError::Io {
                        path: path.to_path_buf(),
                        source,
                    })?;
                let mut latest = Self::parse(&latest_text, self.scope.clone())?;
                latest.source_mtime = fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .map_err(|source| ClaudeMcpError::Io {
                        path: path.to_path_buf(),
                        source,
                    })?
                    .into();
                if latest.servers != self.servers {
                    return Err(conflict.into());
                }
                let intended = Self::parse(new_text, self.scope.clone())?;
                let mut rebased_text = latest_text;
                for (name, server) in &intended.servers {
                    if self.servers.get(name) != Some(server) {
                        let rebased = Self::parse(&rebased_text, self.scope.clone())?;
                        rebased_text = rebased.upsert(server)?;
                    }
                }
                apply(&latest.replacement_request(path, &rebased_text, backup)).map_err(Into::into)
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Reads and writes Claude MCP documents through the guarded filesystem layer.
pub struct ClaudeMcpAdapter;

impl ClaudeMcpAdapter {
    /// Loads an existing JSONC config; missing files return `None`.
    pub fn read(
        path: &Path,
        scope: ClaudeConfigScope,
    ) -> Result<Option<ClaudeMcpConfig>, ClaudeMcpError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ClaudeMcpError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let mut config = ClaudeMcpConfig::parse(&text, scope)?;
        config.source_mtime = Some(
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .map_err(|source| ClaudeMcpError::Io {
                    path: path.to_path_buf(),
                    source,
                })?,
        );
        Ok(Some(config))
    }

    /// Upserts an entry in an existing config with an optimistic hash check.
    pub fn upsert(
        path: &Path,
        scope: ClaudeConfigScope,
        server: &ClaudeServer,
        backup: BackupPolicy,
    ) -> Result<WriteOutcome, ClaudeMcpError> {
        let config = Self::read(path, scope)?.ok_or_else(|| ClaudeMcpError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::NotFound, "Claude config is missing"),
        })?;
        let edited = config.upsert(server)?;
        config.save_edit(path, &edited, backup)
    }
}

fn parse_root(source: &str) -> Result<CstRootNode, ClaudeMcpError> {
    CstRootNode::parse(source, &ParseOptions::default()).map_err(|error| ClaudeMcpError::Parse {
        line: error.line_display(),
        column: error.column_display(),
        message: error.to_string(),
    })
}

/// Rejects a key that occurs more than once, which would make a lookup or patch ambiguous.
fn ensure_unique_key(object: &CstObject, key: &str) -> Result<(), ClaudeMcpError> {
    let occurrences = object
        .properties()
        .iter()
        .filter(|property| property.decoded_name().as_deref() == Some(key))
        .count();
    if occurrences > 1 {
        return Err(ClaudeMcpError::DuplicateKey {
            key: key.to_owned(),
        });
    }
    Ok(())
}

fn server_object(
    root: &CstRootNode,
    scope: &ClaudeConfigScope,
) -> Result<CstObject, ClaudeMcpError> {
    // A Claude config that does not exist yet (or is an empty file) is an empty object. A
    // non-blank document whose root is not an object is user content AgenTuna must not replace.
    let obj = match root.object_value() {
        Some(obj) => obj,
        None if root.to_string().trim().is_empty() => root.object_value_or_set(),
        None => return Err(ClaudeMcpError::ServersNotObject),
    };
    ensure_unique_key(&obj, "mcpServers")?;
    ensure_unique_key(&obj, "projects")?;
    match scope {
        ClaudeConfigScope::User | ClaudeConfigScope::ProjectFile => match obj.get("mcpServers") {
            Some(prop) => prop.object_value().ok_or(ClaudeMcpError::ServersNotObject),
            None => {
                obj.append("mcpServers", CstInputValue::Object(Vec::new()));
                obj.get("mcpServers")
                    .and_then(|prop| prop.object_value())
                    .ok_or(ClaudeMcpError::ServersNotObject)
            }
        },
        ClaudeConfigScope::Project { project_path } => {
            let projects = match obj.get("projects") {
                Some(prop) => prop
                    .object_value()
                    .ok_or(ClaudeMcpError::ServersNotObject)?,
                None => {
                    obj.append("projects", CstInputValue::Object(Vec::new()));
                    obj.get("projects")
                        .and_then(|prop| prop.object_value())
                        .ok_or(ClaudeMcpError::ServersNotObject)?
                }
            };
            ensure_unique_key(&projects, project_path)?;
            let project = match projects.get(project_path) {
                Some(prop) => prop
                    .object_value()
                    .ok_or(ClaudeMcpError::ServersNotObject)?,
                None => {
                    projects.append(project_path, CstInputValue::Object(Vec::new()));
                    projects
                        .get(project_path)
                        .and_then(|prop| prop.object_value())
                        .ok_or(ClaudeMcpError::ServersNotObject)?
                }
            };
            ensure_unique_key(&project, "mcpServers")?;
            match project.get("mcpServers") {
                Some(prop) => prop.object_value().ok_or(ClaudeMcpError::ServersNotObject),
                None => {
                    project.append("mcpServers", CstInputValue::Object(Vec::new()));
                    project
                        .get("mcpServers")
                        .and_then(|prop| prop.object_value())
                        .ok_or(ClaudeMcpError::ServersNotObject)
                }
            }
        }
    }
}

fn apply_secret_refs(values: &mut BTreeMap<String, EnvValue>, refs: &BTreeMap<String, String>) {
    for value in values.values_mut() {
        let EnvValue::Literal(text) = value else {
            continue;
        };
        let Some(name) = text
            .strip_prefix("${")
            .and_then(|rest| rest.strip_suffix('}'))
        else {
            continue;
        };
        if !valid_env_name(name) {
            continue;
        }
        if let Some(account) = refs.get(name) {
            *value = EnvValue::Secret(SecretRef {
                env_name: name.to_owned(),
                keychain_account: account.clone(),
            });
        }
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn strings(
    value: Option<&Value>,
    name: &str,
    field: &'static str,
) -> Result<Vec<String>, ClaudeMcpError> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ClaudeMcpError::InvalidField {
                        name: name.to_owned(),
                        field,
                    })
            })
            .collect(),
        _ => Err(ClaudeMcpError::InvalidField {
            name: name.to_owned(),
            field,
        }),
    }
}

fn env_map(
    value: Option<&Value>,
    name: &str,
    field: &'static str,
) -> Result<BTreeMap<String, EnvValue>, ClaudeMcpError> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Value::Object(values)) => values
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), EnvValue::Literal(value.to_owned())))
                    .ok_or_else(|| ClaudeMcpError::InvalidField {
                        name: name.to_owned(),
                        field,
                    })
            })
            .collect(),
        _ => Err(ClaudeMcpError::InvalidField {
            name: name.to_owned(),
            field,
        }),
    }
}

fn env_value(value: &EnvValue) -> CstInputValue {
    match value {
        EnvValue::Literal(value) => CstInputValue::String(value.clone()),
        EnvValue::Secret(secret) => CstInputValue::String(format!("${{{}}}", secret.env_name)),
    }
}

fn entry_value(server: &ClaudeServer) -> CstInputValue {
    let mut fields = vec![(
        "type".to_owned(),
        CstInputValue::String(server.transport.kind().to_owned()),
    )];
    match &server.transport {
        McpTransport::Stdio {
            command,
            args,
            cwd,
            env,
        } => {
            fields.push(("command".to_owned(), CstInputValue::String(command.clone())));
            if !args.is_empty() {
                fields.push((
                    "args".to_owned(),
                    CstInputValue::Array(args.iter().cloned().map(CstInputValue::String).collect()),
                ));
            }
            if !env.is_empty() {
                fields.push((
                    "env".to_owned(),
                    CstInputValue::Object(
                        env.iter()
                            .map(|(key, value)| (key.clone(), env_value(value)))
                            .collect(),
                    ),
                ));
            }
            if let Some(cwd) = cwd {
                fields.push((
                    "cwd".to_owned(),
                    CstInputValue::String(cwd.to_string_lossy().into_owned()),
                ));
            }
        }
        McpTransport::Http { url, headers, .. } => {
            fields.push((
                "url".to_owned(),
                CstInputValue::String(url.as_str().to_owned()),
            ));
            if !headers.is_empty() {
                fields.push((
                    "headers".to_owned(),
                    CstInputValue::Object(
                        headers
                            .iter()
                            .map(|(key, value)| (key.clone(), env_value(value)))
                            .collect(),
                    ),
                ));
            }
        }
    }
    fields.extend(
        server
            .extra
            .iter()
            .map(|(key, value)| (key.clone(), json_to_cst(value))),
    );
    CstInputValue::Object(fields)
}

fn patch_entry(entry: &CstObject, current: &ClaudeServer, desired: &ClaudeServer) {
    let new_value = entry_value(desired);
    if let CstInputValue::Object(fields) = new_value {
        for (key, value) in fields {
            if key == "type" && current.transport.kind() == desired.transport.kind() {
                continue;
            }
            match entry.get(&key) {
                Some(prop) => prop.set_value(value),
                None => {
                    entry.append(&key, value);
                }
            }
        }
        let keep: Vec<&str> = fields_for(&desired.transport);
        for key in ["command", "args", "env", "cwd", "url", "headers"] {
            if !keep.contains(&key)
                && current.transport.kind() != desired.transport.kind()
                && let Some(prop) = entry.get(key)
            {
                prop.remove();
            }
        }
    }
}

fn fields_for(transport: &McpTransport) -> Vec<&'static str> {
    match transport {
        McpTransport::Stdio { .. } => vec!["command", "args", "env", "cwd"],
        McpTransport::Http { .. } => vec!["url", "headers"],
    }
}

fn json_to_cst(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(*value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value.clone()),
        Value::Array(values) => CstInputValue::Array(values.iter().map(json_to_cst).collect()),
        Value::Object(values) => CstInputValue::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), json_to_cst(value)))
                .collect(),
        ),
    }
}
