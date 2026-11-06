//! Read-only extraction of Codex project trust metadata.

use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

use crate::adapter::AgentError;

/// A Codex project trust entry, exposing no other project-table values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProjectTrust {
    /// Project path used as the key under `[projects]`.
    pub project_path: PathBuf,
    /// Native trust label, when present and represented as a string.
    pub trust_level: Option<String>,
}

/// Reads `[projects."<path>"].trust_level` entries without modifying TOML bytes.
///
/// # Errors
/// Returns [`AgentError::Parse`] for invalid TOML or a malformed `[projects]` table.
pub fn list_project_trust(path: &Path, source: &str) -> Result<Vec<CodexProjectTrust>, AgentError> {
    let document = source
        .parse::<DocumentMut>()
        .map_err(|_| AgentError::Parse {
            path: path.to_path_buf(),
            line: 1,
            col: 1,
        })?;
    let Some(projects_item) = document.get("projects") else {
        return Ok(Vec::new());
    };
    let projects = projects_item.as_table().ok_or_else(|| AgentError::Parse {
        path: path.to_path_buf(),
        line: 1,
        col: 1,
    })?;
    let mut result = Vec::new();
    for (project_path, item) in projects.iter() {
        let project = item.as_table().ok_or_else(|| AgentError::Parse {
            path: path.to_path_buf(),
            line: 1,
            col: 1,
        })?;
        result.push(CodexProjectTrust {
            project_path: PathBuf::from(project_path),
            trust_level: project
                .get("trust_level")
                .and_then(toml_edit::Item::as_str)
                .map(str::to_owned),
        });
    }
    result.sort_by(|left, right| left.project_path.cmp(&right.project_path));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_reader_returns_only_project_path_and_trust_level_without_mutation() {
        let source = concat!(
            "[projects.\"/work/alpha\"]\n",
            "trust_level = \"trusted\"\n",
            "other_setting = 12\n\n",
            "[projects.\"/work/beta\"]\n",
            "trust_level = \"untrusted\"\n",
            "[mcp_servers.sample]\ncommand = \"server\"\n",
        );
        let before = source.to_owned();
        let rows = list_project_trust(Path::new("config.toml"), source).expect("trust list");
        assert_eq!(source, before);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].project_path, PathBuf::from("/work/alpha"));
        assert_eq!(rows[0].trust_level.as_deref(), Some("trusted"));
        assert_eq!(rows[1].project_path, PathBuf::from("/work/beta"));
        assert_eq!(rows[1].trust_level.as_deref(), Some("untrusted"));
    }

    #[test]
    fn missing_projects_table_is_an_empty_read() {
        assert!(
            list_project_trust(Path::new("config.toml"), "model = \"x\"\n")
                .expect("valid config")
                .is_empty()
        );
    }
}
