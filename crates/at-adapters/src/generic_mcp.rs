//! Parameterized JSONC/TOML MCP writer for agent formats with known key paths.

use std::path::PathBuf;

use at_fs::{BackupPolicy, ExpectedState, WriteRequest, apply, sha256_hex};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::Value as JsonValue;
use toml_edit::{DocumentMut, Item, Table, Value as TomlValue};

/// Native MCP config format used by a generic adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GenericMcpFormat {
    /// JSON with JSONC-compatible comments and span-preserving edits.
    Json,
    /// TOML edited with `toml_edit`.
    Toml,
}

/// Key-quoting policy used for TOML server table names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NameQuoting {
    /// Quote only names TOML cannot represent as bare keys.
    Bare,
    /// Always quote names.
    AlwaysQuoted,
}

/// Configurable, format-preserving writer for a native MCP server map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericMcpWriter {
    /// Document format.
    pub format: GenericMcpFormat,
    /// Native document path.
    pub file_path: PathBuf,
    /// Object/table path that contains named server entries.
    pub key_path: Vec<String>,
    /// Native-field to canonical-field mapping, retained for adapter inspection.
    pub field_map: Vec<(String, String)>,
    /// TOML key quoting policy.
    pub name_quoting: NameQuoting,
    source: String,
    expected_hash: String,
}

/// Failure parsing or updating a generic MCP document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GenericMcpError {
    /// The document could not be parsed.
    #[error("invalid MCP document: {0}")]
    Parse(String),
    /// A configured key path collides with a non-object value.
    #[error("MCP key path contains a non-object value at `{0}`")]
    InvalidKeyPath(String),
    /// Filesystem operation failed.
    #[error(transparent)]
    Fs(#[from] at_fs::FsError),
}

impl GenericMcpWriter {
    /// Parses a native document, retaining its original text for format-preserving edits.
    ///
    /// # Errors
    /// Returns [`GenericMcpError::Parse`] if the document is malformed or not an object.
    pub fn parse(
        format: GenericMcpFormat,
        file_path: impl Into<PathBuf>,
        key_path: Vec<String>,
        field_map: Vec<(String, String)>,
        name_quoting: NameQuoting,
        source: &str,
    ) -> Result<Self, GenericMcpError> {
        match format {
            GenericMcpFormat::Json => {
                let root = CstRootNode::parse(source, &ParseOptions::default())
                    .map_err(|e| GenericMcpError::Parse(e.to_string()))?;
                if root.object_value().is_none() {
                    return Err(GenericMcpError::Parse(String::from(
                        "root must be an object",
                    )));
                }
            }
            GenericMcpFormat::Toml => {
                source
                    .parse::<DocumentMut>()
                    .map_err(|e| GenericMcpError::Parse(e.to_string()))?;
            }
        }
        let expected_hash = sha256_hex(source.as_bytes());
        Ok(Self {
            format,
            file_path: file_path.into(),
            key_path,
            field_map,
            name_quoting,
            source: source.to_owned(),
            expected_hash,
        })
    }

    /// Returns the original document bytes unchanged when no mutation was requested.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.source
    }

    /// Inserts or replaces a server object while preserving unrelated document content.
    ///
    /// # Errors
    /// Returns [`GenericMcpError`] if parsing or traversing the configured key path fails.
    pub fn set_server(&mut self, name: &str, entry: &JsonValue) -> Result<(), GenericMcpError> {
        match self.format {
            GenericMcpFormat::Json => {
                let root = CstRootNode::parse(&self.source, &ParseOptions::default())
                    .map_err(|e| GenericMcpError::Parse(e.to_string()))?;
                let map = json_map_for_insert(&root, &self.key_path)?;
                map.get(name).map_or_else(
                    || {
                        map.append(name, json_to_cst(entry));
                    },
                    |p| p.set_value(json_to_cst(entry)),
                );
                self.source = root.to_string();
            }
            GenericMcpFormat::Toml => {
                let mut doc = self
                    .source
                    .parse::<DocumentMut>()
                    .map_err(|e| GenericMcpError::Parse(e.to_string()))?;
                let table = toml_map_for_insert(&mut doc, &self.key_path)?;
                let mut server = Table::new();
                let object = entry.as_object().ok_or_else(|| {
                    GenericMcpError::Parse(String::from("server entry must be an object"))
                })?;
                for (key, value) in object {
                    server.insert(key, json_to_toml(value)?);
                }
                table.insert(name, Item::Table(server));
                let rendered = doc.to_string();
                if self.name_quoting == NameQuoting::AlwaysQuoted {
                    let parent = self
                        .key_path
                        .iter()
                        .map(|key| toml_edit::Key::new(key).to_string())
                        .collect::<Vec<_>>()
                        .join(".");
                    let bare = toml_edit::Key::new(name).to_string();
                    let quoted = serde_json::to_string(name)
                        .map_err(|error| GenericMcpError::Parse(error.to_string()))?;
                    let old_header = format!("[{parent}.{bare}]");
                    let new_header = format!("[{parent}.{quoted}]");
                    self.source = rendered.replace(&old_header, &new_header);
                } else {
                    self.source = rendered;
                }
            }
        }
        Ok(())
    }

    /// Removes a named server, leaving all other document entries intact.
    ///
    /// # Errors
    /// Returns [`GenericMcpError`] if the document is malformed or the key path is invalid.
    pub fn remove_server(&mut self, name: &str) -> Result<(), GenericMcpError> {
        match self.format {
            GenericMcpFormat::Json => {
                let root = CstRootNode::parse(&self.source, &ParseOptions::default())
                    .map_err(|e| GenericMcpError::Parse(e.to_string()))?;
                if let Some(map) = json_map(&root, &self.key_path)?
                    && let Some(prop) = map.get(name)
                {
                    prop.remove();
                }
                self.source = root.to_string();
            }
            GenericMcpFormat::Toml => {
                let mut doc = self
                    .source
                    .parse::<DocumentMut>()
                    .map_err(|e| GenericMcpError::Parse(e.to_string()))?;
                if let Some(table) = toml_map(&mut doc, &self.key_path)? {
                    table.remove(name);
                }
                self.source = doc.to_string();
            }
        }
        Ok(())
    }

    /// Writes the edited document through at-fs and follows an existing config symlink.
    ///
    /// # Errors
    /// Returns the typed filesystem error when the optimistic write fails.
    pub fn save(&mut self, backup: BackupPolicy) -> Result<(), GenericMcpError> {
        let mut request = WriteRequest::replace(
            &self.file_path,
            self.source.as_bytes().to_vec(),
            ExpectedState::Hash(self.expected_hash.clone()),
        );
        request.follow_symlink = true;
        request.backup = backup;
        apply(&request)?;
        self.expected_hash = sha256_hex(self.source.as_bytes());
        Ok(())
    }
}

fn json_map(root: &CstRootNode, path: &[String]) -> Result<Option<CstObject>, GenericMcpError> {
    let mut object = root
        .object_value()
        .ok_or_else(|| GenericMcpError::Parse(String::from("root must be an object")))?;
    for key in path {
        let Some(property) = object.get(key) else {
            return Ok(None);
        };
        object = property
            .object_value()
            .ok_or_else(|| GenericMcpError::InvalidKeyPath(key.clone()))?;
    }
    Ok(Some(object))
}

fn json_map_for_insert(root: &CstRootNode, path: &[String]) -> Result<CstObject, GenericMcpError> {
    let mut object = root
        .object_value()
        .ok_or_else(|| GenericMcpError::Parse(String::from("root must be an object")))?;
    if !object.children().iter().any(|child| child.is_newline()) {
        object.ensure_multiline();
    }
    for key in path {
        object = match object.get(key) {
            Some(property) => property
                .object_value()
                .ok_or_else(|| GenericMcpError::InvalidKeyPath(key.clone()))?,
            None => object.object_value_or_set(key),
        };
        object.ensure_multiline();
    }
    Ok(object)
}

fn toml_map<'a>(
    doc: &'a mut DocumentMut,
    path: &[String],
) -> Result<Option<&'a mut Table>, GenericMcpError> {
    let mut table = doc.as_table_mut();
    for key in path {
        let Some(item) = table.get_mut(key) else {
            return Ok(None);
        };
        table = item
            .as_table_mut()
            .ok_or_else(|| GenericMcpError::InvalidKeyPath(key.clone()))?;
    }
    Ok(Some(table))
}

fn toml_map_for_insert<'a>(
    doc: &'a mut DocumentMut,
    path: &[String],
) -> Result<&'a mut Table, GenericMcpError> {
    let mut table = doc.as_table_mut();
    for key in path {
        if !table.contains_key(key) {
            table.insert(key, Item::Table(Table::new()));
        }
        table = table
            .get_mut(key)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| GenericMcpError::InvalidKeyPath(key.clone()))?;
    }
    Ok(table)
}

fn json_to_cst(value: &JsonValue) -> CstInputValue {
    match value {
        JsonValue::Null => CstInputValue::Null,
        JsonValue::Bool(v) => CstInputValue::Bool(*v),
        JsonValue::Number(v) => CstInputValue::Number(v.to_string()),
        JsonValue::String(v) => CstInputValue::String(v.clone()),
        JsonValue::Array(v) => CstInputValue::Array(v.iter().map(json_to_cst).collect()),
        JsonValue::Object(v) => {
            CstInputValue::Object(v.iter().map(|(k, v)| (k.clone(), json_to_cst(v))).collect())
        }
    }
}

fn json_to_toml(value: &JsonValue) -> Result<Item, GenericMcpError> {
    match value {
        JsonValue::Null => Err(GenericMcpError::Parse(String::from(
            "TOML has no null value",
        ))),
        JsonValue::Bool(v) => Ok(Item::Value(TomlValue::from(*v))),
        JsonValue::Number(v) => v
            .as_i64()
            .map(|v| Item::Value(TomlValue::from(v)))
            .or_else(|| {
                v.as_u64()
                    .and_then(|v| i64::try_from(v).ok())
                    .map(|v| Item::Value(TomlValue::from(v)))
            })
            .or_else(|| v.as_f64().map(|v| Item::Value(TomlValue::from(v))))
            .ok_or_else(|| GenericMcpError::Parse(String::from("unsupported TOML number"))),
        JsonValue::String(v) => Ok(Item::Value(TomlValue::from(v.as_str()))),
        JsonValue::Array(v) => {
            let mut array = toml_edit::Array::new();
            for value in v {
                match json_to_toml(value)? {
                    Item::Value(value) => array.push(value),
                    _ => {
                        return Err(GenericMcpError::Parse(String::from(
                            "nested object in TOML array is unsupported",
                        )));
                    }
                }
            }
            Ok(Item::Value(TomlValue::Array(array)))
        }
        JsonValue::Object(v) => {
            let mut table = toml_edit::InlineTable::new();
            for (key, value) in v {
                table.insert(
                    key,
                    match json_to_toml(value)? {
                        Item::Value(value) => value,
                        _ => {
                            return Err(GenericMcpError::Parse(String::from(
                                "nested table is unsupported",
                            )));
                        }
                    },
                );
            }
            Ok(Item::Value(TomlValue::InlineTable(table)))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{GenericMcpFormat, GenericMcpWriter, NameQuoting};

    #[test]
    fn cursor_json_path_keeps_unknown_siblings_and_adds_servers() {
        let source = "{\n  \"other\": {\"keep\": true},\n  \"mcpServers\": {\n    \"old\": {\"command\": \"old\"}\n  }\n}\n";
        let mut writer = GenericMcpWriter::parse(
            GenericMcpFormat::Json,
            "settings.json",
            vec![String::from("mcpServers")],
            Vec::new(),
            NameQuoting::Bare,
            source,
        )
        .expect("parse");
        assert_eq!(writer.text(), source);
        writer
            .set_server(
                "remote",
                &json!({"url":"https://example.invalid","type":"http"}),
            )
            .expect("set");
        assert!(writer.text().contains("\"other\": {\"keep\": true}"));
        assert!(writer.text().contains("\"remote\""));
        assert!(writer.text().contains("\"old\""));
    }

    #[test]
    fn custom_toml_writer_creates_key_path_and_preserves_siblings() {
        let source = "[unrelated]\nkeep = true\n";
        let mut writer = GenericMcpWriter::parse(
            GenericMcpFormat::Toml,
            "agent.toml",
            vec![String::from("mcp_servers")],
            Vec::new(),
            NameQuoting::AlwaysQuoted,
            source,
        )
        .expect("parse");
        writer
            .set_server(
                "custom-server",
                &json!({"command":"run","args":["--stdio"]}),
            )
            .expect("set");
        assert!(writer.text().contains("[unrelated]\nkeep = true"));
        assert!(writer.text().contains("[mcp_servers.\"custom-server\"]"));
        assert!(writer.text().contains("command = \"run\""));
    }

    #[cfg(unix)]
    #[test]
    fn saving_a_symlinked_config_preserves_the_link() {
        use std::os::unix::fs::symlink;

        use at_fs::BackupPolicy;
        use tempfile::tempdir;

        let dir = tempdir().expect("sandbox");
        let target = dir.path().join("dotfiles/mcp.json");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("dotfiles");
        let source = "{\"mcpServers\":{}}\n";
        std::fs::write(&target, source).expect("config");
        let link = dir.path().join("mcp.json");
        symlink(&target, &link).expect("symlink");
        let mut writer = GenericMcpWriter::parse(
            GenericMcpFormat::Json,
            &link,
            vec![String::from("mcpServers")],
            Vec::new(),
            NameQuoting::Bare,
            source,
        )
        .expect("parse");
        writer
            .set_server("server", &json!({"command":"run"}))
            .expect("set");
        writer.save(BackupPolicy::None).expect("save");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&target)
                .expect("target")
                .contains("\"server\"")
        );
    }
}
