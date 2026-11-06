//! Native Cursor adapter paths and `.mdc` rule documents.

use std::path::PathBuf;

use at_model::{AgentId, ItemKind, Scope};

use crate::adapter::{
    AgentAdapter, AgentCapabilities, AgentDetection, AgentEnvironment, AgentPathCandidate,
    DetectionState,
};
use crate::mcp_capabilities::capabilities;

/// Adapter for Cursor configuration, skills, and rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct CursorAdapter;

/// Diagnostics returned while discovering Cursor-native items.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CursorDiagnostic {
    /// A configured skills directory does not exist yet.
    FileMissing {
        /// Missing skills root.
        path: PathBuf,
    },
}

/// Cursor items, parsed rules, and non-fatal discovery diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorDiscovery {
    /// Skills and rules found in native Cursor paths.
    pub items: Vec<crate::adapter::NativeItem>,
    /// Parsed `.mdc` frontmatter indexed by source file.
    pub rules: std::collections::BTreeMap<PathBuf, CursorRule>,
    /// Missing roots that should be presented as non-fatal diagnostics.
    pub diagnostics: Vec<CursorDiagnostic>,
}

impl CursorAdapter {
    /// Discovers Cursor skills and rules, parsing every rule frontmatter.
    ///
    /// # Errors
    /// Returns a typed error for malformed rule files or unreadable paths.
    pub fn discover(
        &self,
        env: &AgentEnvironment,
    ) -> Result<CursorDiscovery, crate::adapter::AgentError> {
        let items = self.list_native(env)?;
        let mut rules = std::collections::BTreeMap::new();
        for item in items.iter().filter(|item| item.kind == ItemKind::Rule) {
            let text = std::fs::read_to_string(&item.path).map_err(|source| {
                if source.kind() == std::io::ErrorKind::PermissionDenied {
                    crate::adapter::AgentError::Permission {
                        path: item.path.clone(),
                    }
                } else {
                    crate::adapter::AgentError::Io {
                        path: item.path.clone(),
                        source,
                    }
                }
            })?;
            rules.insert(item.path.clone(), parse_mdc(&item.path, &text)?);
        }
        let mut diagnostics = Vec::new();
        for scope in [Scope::User, Scope::Project] {
            for candidate in self.paths(ItemKind::Skill, scope, env) {
                if !candidate.path.exists() {
                    diagnostics.push(CursorDiagnostic::FileMissing {
                        path: candidate.path,
                    });
                }
            }
        }
        Ok(CursorDiscovery {
            items,
            rules,
            diagnostics,
        })
    }
}
impl AgentAdapter for CursorAdapter {
    fn id(&self) -> AgentId {
        AgentId::Cursor
    }

    fn capabilities(&self, env: &AgentEnvironment) -> AgentCapabilities {
        AgentCapabilities {
            mcp: capabilities(&AgentId::Cursor, &env.home),
            supports_symlink: true,
            supports_rules: true,
        }
    }

    fn detect(&self, env: &AgentEnvironment) -> AgentDetection {
        let candidates = self.paths(ItemKind::Skill, Scope::User, env);
        let binary = env
            .variables
            .get("CURSOR_BINARY_PATH")
            .is_some_and(|path| PathBuf::from(path).exists());
        let app = env
            .variables
            .get("CURSOR_APP_PATH")
            .is_some_and(|path| PathBuf::from(path).exists());
        let configured = env.home.join(".cursor").exists();
        let has_skills = env.home.join(".cursor/skills").is_dir();
        AgentDetection {
            state: if binary || app || has_skills {
                DetectionState::Installed
            } else if configured {
                DetectionState::ConfiguredOnly
            } else {
                DetectionState::NotInstalled
            },
            candidates,
        }
    }

    fn paths(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<AgentPathCandidate> {
        let user_scope = matches!(scope, Scope::Global | Scope::User);
        let Some(project) = env.project.as_deref() else {
            if !user_scope {
                return Vec::new();
            }
            return cursor_user_paths(kind, scope, env);
        };
        if user_scope {
            return cursor_user_paths(kind, scope, env);
        }
        match kind {
            ItemKind::Skill => vec![candidate(
                "skills_dir",
                project.join(".cursor/skills"),
                scope,
                None,
                true,
            )],
            ItemKind::Rule => vec![candidate(
                "rules_dir",
                project.join(".cursor/rules"),
                scope,
                Some("mdc"),
                true,
            )],
            ItemKind::McpServer => vec![candidate(
                "mcp_file",
                project.join(".cursor/mcp.json"),
                scope,
                Some("json"),
                true,
            )],
        }
    }

    fn list_native_skills(
        &self,
        env: &AgentEnvironment,
        store_roots: &[PathBuf],
    ) -> Result<Vec<crate::native_skills::NativeSkill>, crate::adapter::AgentError> {
        let skill_roots = [Scope::User, Scope::Project]
            .into_iter()
            .flat_map(|scope| self.paths(ItemKind::Skill, scope, env))
            .collect::<Vec<_>>();
        crate::native_skills::list_native_skills(&self.id(), env, store_roots, skill_roots)
    }

    fn list_native(
        &self,
        env: &AgentEnvironment,
    ) -> Result<Vec<crate::adapter::NativeItem>, crate::adapter::AgentError> {
        let skills = self.list_native_skills(env, &[])?;
        let mut items = skills
            .into_iter()
            .map(|skill| crate::adapter::NativeItem {
                kind: ItemKind::Skill,
                scope: skill.scope,
                name: skill.name,
                path: skill.path,
            })
            .collect::<Vec<_>>();
        for scope in [Scope::User, Scope::Project] {
            for root in self.paths(ItemKind::Rule, scope, env) {
                let entries = match std::fs::read_dir(&root.path) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        return Err(crate::adapter::AgentError::Permission { path: root.path });
                    }
                    Err(source) => {
                        return Err(crate::adapter::AgentError::Io {
                            path: root.path,
                            source,
                        });
                    }
                };
                for entry in entries {
                    let entry = entry.map_err(|source| crate::adapter::AgentError::Io {
                        path: root.path.clone(),
                        source,
                    })?;
                    let path = entry.path();
                    if path.extension().is_none_or(|extension| extension != "mdc")
                        || !entry
                            .file_type()
                            .map_err(|source| crate::adapter::AgentError::Io {
                                path: path.clone(),
                                source,
                            })?
                            .is_file()
                    {
                        continue;
                    }
                    let content = std::fs::read_to_string(&path).map_err(|source| {
                        if source.kind() == std::io::ErrorKind::PermissionDenied {
                            crate::adapter::AgentError::Permission { path: path.clone() }
                        } else {
                            crate::adapter::AgentError::Io {
                                path: path.clone(),
                                source,
                            }
                        }
                    })?;
                    parse_mdc(&path, &content)?;
                    let name = path
                        .file_stem()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                    items.push(crate::adapter::NativeItem {
                        kind: ItemKind::Rule,
                        scope,
                        name,
                        path,
                    });
                }
            }
        }
        items.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(items)
    }
}

fn cursor_user_paths(
    kind: ItemKind,
    scope: Scope,
    env: &AgentEnvironment,
) -> Vec<AgentPathCandidate> {
    let path = |default: PathBuf| {
        env.variables
            .get("CURSOR_MCP_PATH")
            .map_or(default, PathBuf::from)
    };
    match kind {
        ItemKind::Skill => vec![candidate(
            "skills_dir",
            env.home.join(".cursor/skills"),
            scope,
            None,
            true,
        )],
        ItemKind::Rule => vec![candidate(
            "rules_dir",
            env.home.join(".cursor/rules"),
            scope,
            Some("mdc"),
            true,
        )],
        ItemKind::McpServer => vec![candidate(
            "mcp_file",
            path(env.home.join(".cursor/mcp.json")),
            scope,
            Some("json"),
            true,
        )],
    }
}

/// Cursor rule fields stored in the `.mdc` YAML frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorRule {
    /// Human-readable rule description.
    pub description: String,
    /// File glob patterns that activate the rule.
    pub globs: Vec<String>,
    /// Whether Cursor always applies the rule.
    pub always_apply: bool,
    /// Markdown body after the frontmatter.
    pub body: String,
}

/// Parses one Cursor `.mdc` rule, preserving the body as text.
///
/// # Errors
/// Returns [`crate::adapter::AgentError::Parse`] with the malformed line and column.
pub fn parse_mdc(
    path: &std::path::Path,
    source: &str,
) -> Result<CursorRule, crate::adapter::AgentError> {
    let fail = |line, col| crate::adapter::AgentError::Parse {
        path: path.to_path_buf(),
        line,
        col,
    };
    let lines: Vec<&str> = source.lines().collect();
    if lines.first().copied() != Some("---") {
        return Err(fail(1, 1));
    }
    let close = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| **line == "---")
        .map(|(index, _)| index)
        .ok_or_else(|| fail(lines.len().max(1), 1))?;
    if !lines[1].starts_with("description:") {
        return Err(fail(2, 1));
    }
    if !lines[2].starts_with("globs:") {
        return Err(fail(3, 1));
    }
    if close < 4 {
        return Err(fail(close + 1, 1));
    }
    let description = parse_yaml_scalar(lines[1].strip_prefix("description:").unwrap_or_default())
        .ok_or_else(|| fail(2, "description:".len() + 1))?;
    let globs_line = lines[2].strip_prefix("globs:").unwrap_or_default();
    if !globs_line.trim().is_empty() {
        return Err(fail(3, "globs:".len() + 1));
    }
    let mut globs = Vec::new();
    for (index, line) in lines[3..close - 1].iter().enumerate() {
        let Some(value) = line.strip_prefix("  - ") else {
            return Err(fail(index + 4, 1));
        };
        globs.push(parse_yaml_scalar(value).ok_or_else(|| fail(index + 4, 5))?);
    }
    let always = lines[close - 1]
        .strip_prefix("alwaysApply:")
        .ok_or_else(|| fail(close, 1))?
        .trim();
    let always_apply = match always {
        "true" => true,
        "false" => false,
        _ => return Err(fail(close, "alwaysApply:".len() + 1)),
    };
    let body_start = if lines.get(close + 1).is_some_and(|line| line.is_empty()) {
        close + 2
    } else {
        return Err(fail(close + 2, 1));
    };
    Ok(CursorRule {
        description,
        globs,
        always_apply,
        body: lines[body_start..].join("\n"),
    })
}

/// Renders Cursor frontmatter in canonical key order followed by the Markdown body.
#[must_use]
pub fn render_mdc(rule: &CursorRule) -> String {
    let description =
        serde_json::to_string(&rule.description).unwrap_or_else(|_| "\"\"".to_owned());
    let mut output = format!("---\ndescription: {description}\nglobs:\n");
    for glob in &rule.globs {
        let value = serde_json::to_string(glob).unwrap_or_else(|_| "\"\"".to_owned());
        output.push_str("  - ");
        output.push_str(&value);
        output.push('\n');
    }
    output.push_str(if rule.always_apply {
        "alwaysApply: true\n---\n\n"
    } else {
        "alwaysApply: false\n---\n\n"
    });
    output.push_str(&rule.body);
    if !rule.body.is_empty() && !rule.body.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn parse_yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('"') {
        serde_json::from_str(value).ok()
    } else if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        Some(value[1..value.len() - 1].replace("''", "'"))
    } else if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn candidate(
    kind: &str,
    path: PathBuf,
    scope: Scope,
    format: Option<&str>,
    primary: bool,
) -> AgentPathCandidate {
    AgentPathCandidate {
        kind: kind.to_owned(),
        path,
        scope,
        format: format.map(str::to_owned),
        primary,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use at_model::{ItemKind, Scope};

    use super::CursorAdapter;
    use crate::adapter::{AgentAdapter, AgentEnvironment, DetectionState};

    #[test]
    fn configured_cursor_without_skills_is_not_reported_as_installed() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        std::fs::create_dir_all(home.join(".cursor/unrelated")).expect("cursor config");
        let env = AgentEnvironment {
            home: home.clone(),
            project: Some(root.path().join("repo")),
            variables: BTreeMap::new(),
        };
        let adapter = CursorAdapter;
        let detection = adapter.detect(&env);
        assert_eq!(detection.state, DetectionState::ConfiguredOnly);
        assert!(
            !adapter.paths(ItemKind::Skill, Scope::User, &env)[0]
                .path
                .exists()
        );
        assert_eq!(
            adapter.paths(ItemKind::Skill, Scope::Project, &env)[0].path,
            root.path().join("repo/.cursor/skills")
        );
    }

    #[test]
    fn mdc_render_parse_preserves_fields_and_rejects_malformed_frontmatter() {
        let rule = super::CursorRule {
            description: "Only on source files".to_owned(),
            globs: vec!["src/**/*.rs".to_owned(), "*.tsx".to_owned()],
            always_apply: false,
            body: "# Rust rules".to_owned(),
        };
        let output = super::render_mdc(&rule);
        assert!(output.starts_with(
            "---\ndescription: \"Only on source files\"\nglobs:\n  - \"src/**/*.rs\"\n  - \"*.tsx\"\nalwaysApply: false\n---\n\n"
        ));
        assert_eq!(
            super::parse_mdc(std::path::Path::new("rule.mdc"), &output).expect("parse"),
            rule
        );
        let error = super::parse_mdc(std::path::Path::new("bad.mdc"), "---\nglobs:\n---\n")
            .expect_err("bad field order");
        assert!(matches!(
            error,
            crate::adapter::AgentError::Parse {
                path,
                line: 2,
                col: 1
            } if path == std::path::Path::new("bad.mdc")
        ));
    }

    #[test]
    fn native_discovery_parses_mdc_and_ignores_missing_skills_roots() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let rules = home.join(".cursor/rules");
        std::fs::create_dir_all(&rules).expect("rules dir");
        std::fs::write(
            rules.join("safe.mdc"),
            "---\ndescription: \"Safe rule\"\nglobs:\n  - \"src/**\"\nalwaysApply: true\n---\n\n# Body\n",
        )
        .expect("mdc file");
        let env = AgentEnvironment {
            home,
            project: None,
            variables: BTreeMap::new(),
        };
        let discovery = CursorAdapter.discover(&env).expect("discovery");
        assert_eq!(discovery.items.len(), 1);
        assert_eq!(discovery.items[0].name, "safe");
        assert_eq!(discovery.items[0].kind, ItemKind::Rule);
        assert!(discovery.items[0].path.ends_with("safe.mdc"));
        assert_eq!(discovery.rules[&discovery.items[0].path].globs, ["src/**"]);
        assert!(
            discovery
                .diagnostics
                .iter()
                .any(|diagnostic| matches!(diagnostic, super::CursorDiagnostic::FileMissing { path } if path.ends_with(".cursor/skills")))
        );
    }
}
