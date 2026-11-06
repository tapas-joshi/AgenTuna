//! Format-preserving planner for Claude Code's project-local MCP settings lists.

use std::path::{Path, PathBuf};

use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::CstRootNode;

use crate::adapter::AgentError;

const ENABLED_KEY: &str = "enabledMcpjsonServers";
const DISABLED_KEY: &str = "disabledMcpjsonServers";

/// Parsed Claude Code project settings retained as their original JSONC source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeProjectSettings {
    path: PathBuf,
    source: String,
}

impl ClaudeProjectSettings {
    /// Parses one settings document without normalizing its source text.
    ///
    /// # Errors
    /// Returns [`AgentError::Parse`] when the document is not a JSONC object or either managed
    /// list is not an array of strings.
    pub fn parse(path: PathBuf, source: String) -> Result<Self, AgentError> {
        parse_jsonc(&path, &source)?;
        for key in [ENABLED_KEY, DISABLED_KEY] {
            if let Some((start, end)) =
                property_range(&source, key).map_err(|_| parse_error(&path))?
            {
                parse_string_array(&source[start..end]).map_err(|_| parse_error(&path))?;
            }
        }
        Ok(Self { path, source })
    }

    /// Original bytes, unchanged.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Builds a new settings document with `name` enabled or disabled, preserving unrelated bytes.
    /// The server name is kept in exactly one of Claude's enabled/disabled arrays.
    ///
    /// # Errors
    /// Returns [`AgentError::Parse`] for malformed settings or [`AgentError::Unsupported`] if
    /// the requested server name is empty.
    pub fn set_server_enabled(&self, name: &str, enabled: bool) -> Result<String, AgentError> {
        if name.trim().is_empty() {
            return Err(AgentError::Unsupported {
                agent: at_model::AgentId::ClaudeCode,
                kind: "empty MCP server name".to_owned(),
            });
        }
        let mut enabled_names = self.list(ENABLED_KEY)?;
        let mut disabled_names = self.list(DISABLED_KEY)?;
        enabled_names.retain(|entry| entry != name);
        disabled_names.retain(|entry| entry != name);
        if enabled {
            enabled_names.push(name.to_owned());
        } else {
            disabled_names.push(name.to_owned());
        }
        let mut rendered = replace_array(&self.source, ENABLED_KEY, &enabled_names)
            .map_err(|_| parse_error(&self.path))?;
        rendered = replace_array(&rendered, DISABLED_KEY, &disabled_names)
            .map_err(|_| parse_error(&self.path))?;
        parse_jsonc(&self.path, &rendered)?;
        Ok(rendered)
    }

    /// Plans a guarded settings write; performs no filesystem mutation.
    ///
    /// `relative_path` is rooted by the caller's reviewed ChangeSet application context.
    ///
    /// # Errors
    /// Returns a typed adapter error if rendering or ChangeSet validation fails.
    pub fn change_set(
        &self,
        relative_path: &Path,
        new_text: &str,
        created_at_unix_seconds: u64,
    ) -> Result<ChangeSet, AgentError> {
        parse_jsonc(&self.path, new_text)?;
        let source_hash = at_fs::sha256_hex(self.source.as_bytes());
        let output_hash = at_fs::sha256_hex(new_text.as_bytes());
        let expected = ExpectedState::hash(&source_hash).map_err(|error| AgentError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(error),
        })?;
        let set = ChangeSet {
            id: format!("claude-settings-{}", &output_hash[..16]),
            preview_id: source_hash,
            operation_kind: OperationKind::parse("settings").map_err(|error| AgentError::Io {
                path: self.path.clone(),
                source: std::io::Error::other(error),
            })?,
            scope: Some(at_model::Scope::Local),
            project_id: None,
            summary: "Update Claude Code local MCP settings".to_owned(),
            ops: vec![FileOp::Write {
                path: relative_path.to_path_buf(),
                bytes: new_text.as_bytes().to_vec(),
                expected,
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds,
        };
        set.validate().map_err(|error| AgentError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(error),
        })?;
        Ok(set)
    }

    fn list(&self, key: &str) -> Result<Vec<String>, AgentError> {
        match property_range(&self.source, key) {
            Ok(Some((start, end))) => {
                parse_string_array(&self.source[start..end]).map_err(|_| parse_error(&self.path))
            }
            Ok(None) => Ok(Vec::new()),
            Err(()) => Err(parse_error(&self.path)),
        }
    }
}

fn parse_jsonc(path: &Path, source: &str) -> Result<(), AgentError> {
    let root = CstRootNode::parse(source, &ParseOptions::default()).map_err(|error| {
        AgentError::Parse {
            path: path.to_path_buf(),
            line: error.line_display(),
            col: error.column_display(),
        }
    })?;
    if root.object_value().is_none() {
        return Err(parse_error(path));
    }
    Ok(())
}

fn parse_error(path: &Path) -> AgentError {
    AgentError::Parse {
        path: path.to_path_buf(),
        line: 1,
        col: 1,
    }
}

fn replace_array(source: &str, key: &str, names: &[String]) -> Result<String, ()> {
    let replacement = format!(
        "[{}]",
        names
            .iter()
            .map(|name| serde_json::to_string(name).map_err(|_| ()))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ")
    );
    if let Some((start, end)) = property_range(source, key)? {
        let mut result = String::with_capacity(source.len() + replacement.len());
        result.push_str(source.get(..start).ok_or(())?);
        result.push_str(&replacement);
        result.push_str(source.get(end..).ok_or(())?);
        return Ok(result);
    }
    let open = root_open(source)?;
    let close = root_close(source)?;
    let (has_property, trailing_comma) = root_members_state(source, open, close)?;
    let insertion = format!(
        "{}\"{key}\": {replacement}",
        if has_property && !trailing_comma {
            ", "
        } else {
            ""
        }
    );
    let mut result = String::with_capacity(source.len() + insertion.len());
    result.push_str(source.get(..close).ok_or(())?);
    result.push_str(&insertion);
    result.push_str(source.get(close..).ok_or(())?);
    Ok(result)
}

fn property_range(source: &str, key: &str) -> Result<Option<(usize, usize)>, ()> {
    let open = root_open(source)?;
    let close = root_close(source)?;
    let mut cursor = open + 1;
    while cursor < close {
        cursor = skip_trivia(source, cursor)?;
        if cursor >= close {
            break;
        }
        if source.as_bytes()[cursor] == b',' {
            cursor += 1;
            continue;
        }
        let token_end = skip_string(source, cursor)?;
        let property: String =
            serde_json::from_str(source.get(cursor..token_end).ok_or(())?).map_err(|_| ())?;
        cursor = skip_trivia(source, token_end)?;
        if source.as_bytes().get(cursor) != Some(&b':') {
            return Err(());
        }
        let value_start = skip_trivia(source, cursor + 1)?;
        let value_end = skip_value(source, value_start)?;
        if property == key {
            if source.as_bytes().get(value_start) != Some(&b'[') {
                return Err(());
            }
            return Ok(Some((value_start, value_end)));
        }
        cursor = value_end;
    }
    Ok(None)
}

fn parse_string_array(text: &str) -> Result<Vec<String>, ()> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'[') || bytes.last() != Some(&b']') {
        return Err(());
    }
    let mut cursor = 1;
    let mut names = Vec::new();
    loop {
        cursor = skip_trivia(text, cursor)?;
        match bytes.get(cursor) {
            Some(b']') => return Ok(names),
            Some(b',') => cursor += 1,
            Some(b'"') => {
                let end = skip_string(text, cursor)?;
                names.push(serde_json::from_str(text.get(cursor..end).ok_or(())?).map_err(|_| ())?);
                cursor = end;
                let after = skip_trivia(text, cursor)?;
                match bytes.get(after) {
                    Some(b',') => cursor = after + 1,
                    Some(b']') => return Ok(names),
                    _ => return Err(()),
                }
            }
            _ => return Err(()),
        }
    }
}

fn root_open(source: &str) -> Result<usize, ()> {
    let start = skip_trivia(source, 0)?;
    (source.as_bytes().get(start) == Some(&b'{'))
        .then_some(start)
        .ok_or(())
}

fn root_members_state(source: &str, open: usize, close: usize) -> Result<(bool, bool), ()> {
    let bytes = source.as_bytes();
    let mut cursor = open + 1;
    let mut has_property = false;
    let mut trailing_comma = false;
    loop {
        cursor = skip_trivia(source, cursor)?;
        if cursor >= close {
            return Ok((has_property, trailing_comma));
        }
        if bytes[cursor] == b',' {
            trailing_comma = true;
            cursor += 1;
            continue;
        }
        let key_end = skip_string(source, cursor)?;
        cursor = skip_trivia(source, key_end)?;
        if bytes.get(cursor) != Some(&b':') {
            return Err(());
        }
        cursor = skip_trivia(source, cursor + 1)?;
        cursor = skip_value(source, cursor)?;
        has_property = true;
        trailing_comma = false;
    }
}

fn root_close(source: &str) -> Result<usize, ()> {
    let open = root_open(source)?;
    let mut depth = 0_usize;
    let mut cursor = open;
    while cursor < source.len() {
        match source.as_bytes()[cursor] {
            b'"' => cursor = skip_string(source, cursor)?,
            b'/' if source.as_bytes().get(cursor + 1) == Some(&b'/') => {
                cursor = source[cursor..]
                    .find('\n')
                    .map_or(source.len(), |offset| cursor + offset + 1);
            }
            b'/' if source.as_bytes().get(cursor + 1) == Some(&b'*') => {
                cursor = source[cursor + 2..]
                    .find("*/")
                    .map_or(source.len(), |offset| cursor + offset + 4);
            }
            b'{' => {
                depth = depth.checked_add(1).ok_or(())?;
                cursor += 1;
            }
            b'}' => {
                depth = depth.checked_sub(1).ok_or(())?;
                if depth == 0 {
                    return Ok(cursor);
                }
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    Err(())
}

fn skip_value(source: &str, mut cursor: usize) -> Result<usize, ()> {
    let bytes = source.as_bytes();
    match bytes.get(cursor) {
        Some(b'[') | Some(b'{') => {
            let open = bytes[cursor];
            let close = if open == b'[' { b']' } else { b'}' };
            let mut depth = 0_usize;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'"' => cursor = skip_string(source, cursor)?,
                    b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                        cursor = source[cursor..]
                            .find('\n')
                            .map_or(source.len(), |offset| cursor + offset + 1);
                    }
                    b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                        cursor = source[cursor + 2..]
                            .find("*/")
                            .map_or(source.len(), |offset| cursor + offset + 4);
                    }
                    byte if byte == open => {
                        depth = depth.checked_add(1).ok_or(())?;
                        cursor += 1;
                    }
                    byte if byte == close => {
                        depth = depth.checked_sub(1).ok_or(())?;
                        cursor += 1;
                        if depth == 0 {
                            return Ok(cursor);
                        }
                    }
                    _ => cursor += 1,
                }
            }
            Err(())
        }
        Some(b'"') => skip_string(source, cursor),
        Some(_) => {
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'\"' => cursor = skip_string(source, cursor)?,
                    b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                        cursor = source[cursor..]
                            .find('\n')
                            .map_or(source.len(), |offset| cursor + offset + 1);
                    }
                    b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                        cursor = source[cursor + 2..]
                            .find("*/")
                            .map_or(source.len(), |offset| cursor + offset + 4);
                    }
                    b',' | b'}' | b']' => break,
                    _ => cursor += 1,
                }
            }
            Ok(cursor)
        }
        None => Err(()),
    }
}

fn skip_string(source: &str, start: usize) -> Result<usize, ()> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return Err(());
    }
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = cursor.checked_add(2).ok_or(())?,
            b'"' => return Ok(cursor + 1),
            _ => cursor += 1,
        }
    }
    Err(())
}

fn skip_trivia(source: &str, mut cursor: usize) -> Result<usize, ()> {
    let bytes = source.as_bytes();
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"//") {
            cursor = source[cursor..]
                .find('\n')
                .map_or(source.len(), |offset| cursor + offset + 1);
        } else if bytes.get(cursor..cursor + 2) == Some(b"/*") {
            cursor = source[cursor + 2..]
                .find("*/")
                .map_or(source.len(), |offset| cursor + offset + 4);
        } else {
            return Ok(cursor);
        }
    }
}
/// Returns the project-local Claude settings file path.
#[must_use]
pub fn settings_local_path(project: &Path) -> PathBuf {
    project.join(".claude/settings.local.json")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_edit_changes_only_managed_arrays() {
        let source = concat!(
            "{\n",
            "  \"permissions\": { \"allow\": [\"Read\"] },\n",
            "  \"hooks\": { \"PreToolUse\": [] },\n",
            "  \"enabledMcpjsonServers\": [\"keep\", \"server\", \"server\"],\n",
            "  \"disabledMcpjsonServers\": [\"server\"]\n",
            "}\n"
        );
        let settings =
            ClaudeProjectSettings::parse(PathBuf::from("settings.local.json"), source.to_owned())
                .expect("valid settings");
        let updated = settings
            .set_server_enabled("server", true)
            .expect("enable server");
        assert!(updated.contains("\"permissions\": { \"allow\": [\"Read\"] }"));
        assert!(updated.contains("\"hooks\": { \"PreToolUse\": [] }"));
        assert!(updated.contains("\"enabledMcpjsonServers\": [\"keep\", \"server\"]"));
        assert!(updated.contains("\"disabledMcpjsonServers\": []"));
        assert_eq!(settings.source(), source);
    }

    #[test]
    fn settings_change_set_is_a_guarded_file_operation() {
        let source = "{\"enabledMcpjsonServers\":[],\"disabledMcpjsonServers\":[]}";
        let settings =
            ClaudeProjectSettings::parse(PathBuf::from("settings.local.json"), source.to_owned())
                .expect("valid settings");
        let updated = settings
            .set_server_enabled("server", false)
            .expect("disable server");
        let change = settings
            .change_set(Path::new(".claude/settings.local.json"), &updated, 7)
            .expect("change set");
        assert_eq!(change.ops.len(), 1);
        assert!(matches!(
            &change.ops[0],
            FileOp::Write {
                path,
                follow_symlink: false,
                backup: true,
                ..
            } if path == Path::new(".claude/settings.local.json")
        ));
    }

    #[test]
    fn settings_parser_rejects_non_string_entries() {
        let error = ClaudeProjectSettings::parse(
            PathBuf::from("settings.local.json"),
            "{\"enabledMcpjsonServers\":[7]}".to_owned(),
        )
        .expect_err("non-string server key");
        assert!(matches!(error, AgentError::Parse { .. }));
    }

    #[test]
    fn missing_settings_arrays_are_inserted_around_jsonc_comments_and_trailing_commas() {
        let source = "// header\n{\n  \"permissions\": {}, // preserve\n}\n";
        let settings =
            ClaudeProjectSettings::parse(PathBuf::from("settings.local.json"), source.to_owned())
                .expect("valid JSONC");
        let updated = settings
            .set_server_enabled("server", true)
            .expect("insert settings");
        assert!(updated.contains("// header"));
        assert!(updated.contains("\"permissions\": {}, // preserve"));
        assert!(updated.contains("\"enabledMcpjsonServers\": [\"server\"]"));
        assert!(updated.contains("\"disabledMcpjsonServers\": []"));
        parse_jsonc(Path::new("settings.local.json"), &updated).expect("valid updated JSONC");
    }
}
