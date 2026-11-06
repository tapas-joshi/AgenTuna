//! Read-only discovery of skills outside AgenTuna-owned stores.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use at_model::{AgentId, Scope};

use crate::adapter::{AgentEnvironment, AgentError, AgentPathCandidate};

/// Health state for a discovered native skill.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeSkillState {
    /// Skill folder and its SKILL.md are readable.
    Valid,
    /// SKILL.md exists but its frontmatter or required name is invalid.
    Invalid,
    /// Skill entry is a dangling symbolic link.
    Broken,
    /// Skill entry cannot be read due to permissions.
    PermissionDenied,
}

/// One skill found in an agent-owned discovery root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSkill {
    /// Absolute path of the skill directory (or link itself).
    pub path: PathBuf,
    /// Parsed name, falling back to the folder name when invalid.
    pub name: String,
    /// Parsed description, when available.
    pub description: Option<String>,
    /// Whether the SKILL.md frontmatter is valid.
    pub valid: bool,
    /// Parse or filesystem diagnostic.
    pub error: Option<String>,
    /// Agent that discovered the skill.
    pub agent: AgentId,
    /// Scope of the discovery root.
    pub scope: Scope,
    /// Last modified time of the skill entry, when available.
    pub mtime: Option<SystemTime>,
    /// Symlink target as written, including dangling targets.
    pub target: Option<PathBuf>,
    /// Whether this was found in a secondary, unmanaged discovery root.
    pub unmanaged_alternative: bool,
    /// Discovery state.
    pub state: NativeSkillState,
}

/// Lists skills without following untrusted links or changing any files.
///
/// Symlinks into an AgenTuna store are omitted. The output is sorted by path.
///
/// # Errors
/// Returns a typed error for a root that cannot be enumerated.
pub fn list_native_skills(
    agent: &AgentId,
    env: &AgentEnvironment,
    store_roots: &[PathBuf],
    roots: impl IntoIterator<Item = AgentPathCandidate>,
) -> Result<Vec<NativeSkill>, AgentError> {
    let mut skills = Vec::new();
    for root in roots {
        if root.kind != "skills_dir" {
            continue;
        }
        let entries = match std::fs::read_dir(&root.path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                skills.push(skill_error(
                    agent,
                    root.scope,
                    root.path,
                    NativeSkillState::PermissionDenied,
                    "permission denied listing skills root",
                ));
                continue;
            }
            Err(source) => {
                return Err(AgentError::Io {
                    path: root.path,
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| AgentError::Io {
                path: root.path.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {
                    skills.push(skill_error(
                        agent,
                        root.scope,
                        path,
                        NativeSkillState::PermissionDenied,
                        "permission denied",
                    ));
                    continue;
                }
                Err(source) => return Err(AgentError::Io { path, source }),
            };
            let target = if metadata.file_type().is_symlink() {
                std::fs::read_link(&path).ok()
            } else {
                None
            };
            let content_root = if let Some(target_path) = target.as_ref() {
                let resolved = match std::fs::canonicalize(&path) {
                    Ok(resolved) => resolved,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let mut row = skill_error(
                            agent,
                            root.scope,
                            path,
                            NativeSkillState::Broken,
                            "dangling symbolic link",
                        );
                        row.target = Some(target_path.clone());
                        skills.push(row);
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        skills.push(skill_error(
                            agent,
                            root.scope,
                            path,
                            NativeSkillState::PermissionDenied,
                            "permission denied resolving symbolic link",
                        ));
                        continue;
                    }
                    Err(source) => return Err(AgentError::Io { path, source }),
                };
                if is_store_target(&resolved, store_roots) {
                    continue;
                }
                if !is_within_injected_roots(&resolved, env) {
                    let mut row = skill_error(
                        agent,
                        root.scope,
                        path,
                        NativeSkillState::Invalid,
                        "symbolic link target is outside injected roots",
                    );
                    row.target = Some(target_path.clone());
                    skills.push(row);
                    continue;
                }
                resolved
            } else {
                if !metadata.is_dir() {
                    continue;
                }
                path.clone()
            };
            let content_path = content_root.join("SKILL.md");
            let bytes = match std::fs::read(&content_path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    skills.push(skill_error(
                        agent,
                        root.scope,
                        path,
                        NativeSkillState::PermissionDenied,
                        "permission denied reading SKILL.md",
                    ));
                    continue;
                }
                Err(source) => {
                    return Err(AgentError::Io {
                        path: content_path,
                        source,
                    });
                }
            };
            let fallback = path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let (name, description, parse_error) = parse_skill_frontmatter(&bytes, &fallback);
            let state = if parse_error.is_some() {
                NativeSkillState::Invalid
            } else {
                NativeSkillState::Valid
            };
            skills.push(NativeSkill {
                path,
                name,
                description,
                valid: parse_error.is_none(),
                error: parse_error,
                agent: agent.clone(),
                scope: root.scope,
                mtime: metadata.modified().ok(),
                target,
                unmanaged_alternative: !root.primary,
                state,
            });
        }
    }
    skills.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(skills)
}

fn parse_skill_frontmatter(
    bytes: &[u8],
    fallback: &str,
) -> (String, Option<String>, Option<String>) {
    use saphyr::{LoadableYamlNode, MarkedYaml, YamlData};

    let Ok(text) = std::str::from_utf8(bytes) else {
        return (
            fallback.to_owned(),
            None,
            Some("SKILL.md is not valid UTF-8".to_owned()),
        );
    };
    let normalized = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = normalized.strip_prefix("---") else {
        return (
            fallback.to_owned(),
            None,
            Some("missing YAML frontmatter".to_owned()),
        );
    };
    let Some(end) = rest.find("\n---") else {
        return (
            fallback.to_owned(),
            None,
            Some("unterminated YAML frontmatter".to_owned()),
        );
    };
    let documents = match MarkedYaml::load_from_str(&rest[..end]) {
        Ok(documents) if documents.len() == 1 => documents,
        Ok(_) => {
            return (
                fallback.to_owned(),
                None,
                Some("frontmatter must contain exactly one YAML document".to_owned()),
            );
        }
        Err(error) => {
            return (
                fallback.to_owned(),
                None,
                Some(format!("invalid YAML frontmatter: {error}")),
            );
        }
    };
    let YamlData::Mapping(mapping) = &documents[0].data else {
        return (
            fallback.to_owned(),
            None,
            Some("frontmatter must be a YAML mapping".to_owned()),
        );
    };
    let get = |field: &str| {
        mapping
            .iter()
            .find(|(key, _)| key.data.as_str() == Some(field))
            .and_then(|(_, value)| value.data.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    match get("name") {
        Some(name) => (name.to_owned(), get("description").map(str::to_owned), None),
        None => (
            fallback.to_owned(),
            get("description").map(str::to_owned),
            Some("frontmatter is missing name".to_owned()),
        ),
    }
}

fn is_store_target(resolved: &Path, store_roots: &[PathBuf]) -> bool {
    store_roots
        .iter()
        .filter_map(|store| std::fs::canonicalize(store).ok())
        .any(|store| resolved.starts_with(store))
}

fn is_within_injected_roots(path: &Path, env: &AgentEnvironment) -> bool {
    let roots = [Some(env.home.clone()), env.project.clone()];
    roots
        .into_iter()
        .flatten()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| path.starts_with(root))
}

fn skill_error(
    agent: &AgentId,
    scope: Scope,
    path: PathBuf,
    state: NativeSkillState,
    message: &str,
) -> NativeSkill {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let target = (state == NativeSkillState::Broken)
        .then(|| std::fs::read_link(&path).ok())
        .flatten();
    NativeSkill {
        path,
        name,
        description: None,
        valid: false,
        error: Some(message.to_owned()),
        agent: agent.clone(),
        scope,
        mtime: None,
        target,
        unmanaged_alternative: false,
        state,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use at_model::ItemKind;
    use std::collections::BTreeMap;

    fn environment(home: PathBuf, project: PathBuf) -> AgentEnvironment {
        AgentEnvironment {
            home,
            project: Some(project),
            variables: BTreeMap::new(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn lists_valid_invalid_broken_and_excludes_store_link_in_stable_order() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let root = home.join(".claude/skills");
        std::fs::create_dir_all(root.join("good")).expect("skill root");
        std::fs::create_dir_all(root.join("invalid")).expect("invalid skill");
        let outside = temp.path().join("outside/skill");
        std::fs::create_dir_all(&outside).expect("outside skill");
        std::fs::write(
            outside.join("SKILL.md"),
            "---\nname: outside\ndescription: hidden\n---\n",
        )
        .expect("outside content");
        std::os::unix::fs::symlink(&outside, root.join("outside")).expect("external link");
        let store_root = temp.path().join("canonical-store");
        std::fs::create_dir_all(store_root.join("skills/managed")).expect("store skill");
        std::fs::write(
            root.join("good/SKILL.md"),
            "---\nname: good\ndescription: useful\n---\nbody\n",
        )
        .expect("valid skill");
        std::fs::write(root.join("invalid/SKILL.md"), "---\nno-name: value\n---\n")
            .expect("invalid skill");
        std::os::unix::fs::symlink(store_root.join("skills/managed"), root.join("managed"))
            .expect("managed link");
        std::os::unix::fs::symlink(root.join("absent"), root.join("broken")).expect("broken link");
        let env = environment(home, project);
        let rows = list_native_skills(
            &AgentId::ClaudeCode,
            &env,
            std::slice::from_ref(&store_root),
            [AgentPathCandidate {
                kind: "skills_dir".to_owned(),
                path: root,
                scope: Scope::User,
                format: None,
                primary: true,
            }],
        )
        .expect("scan");
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
            ["broken", "good", "invalid", "outside"]
        );
        assert_eq!(rows[0].state, NativeSkillState::Broken);
        assert_eq!(
            rows[0].target.as_ref().map(|target| target.is_absolute()),
            Some(true)
        );
        assert!(rows[1].valid);
        assert_eq!(rows[1].description.as_deref(), Some("useful"));
        assert_eq!(rows[2].state, NativeSkillState::Invalid);
        assert!(rows[2].error.is_some());
        assert_eq!(rows[3].state, NativeSkillState::Invalid);
        assert_eq!(
            rows[3].error.as_deref(),
            Some("symbolic link target is outside injected roots")
        );
        let second = list_native_skills(
            &AgentId::ClaudeCode,
            &env,
            std::slice::from_ref(&store_root),
            [AgentPathCandidate {
                kind: "skills_dir".to_owned(),
                path: env.home.join(".claude/skills"),
                scope: Scope::User,
                format: None,
                primary: true,
            }],
        )
        .expect("second scan");
        assert_eq!(rows, second);
    }

    #[test]
    fn malformed_utf8_is_reported_instead_of_dropped() {
        let (name, _, error) = parse_skill_frontmatter(&[0xff], "folder");
        assert_eq!(name, "folder");
        assert_eq!(error.as_deref(), Some("SKILL.md is not valid UTF-8"));
    }

    #[test]
    fn malformed_yaml_frontmatter_is_reported() {
        let (_, _, error) = parse_skill_frontmatter(b"---\nname: [broken\n---\n", "folder");
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("invalid YAML frontmatter"))
        );
    }

    #[test]
    fn agent_adapter_exposes_native_skill_listing() {
        use crate::adapter::AgentAdapterRegistry;

        let temp = tempfile::tempdir().expect("temporary directory");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let skill = home.join(".claude/skills/native");
        std::fs::create_dir_all(&skill).expect("skill directory");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: native\ndescription: test\n---\n",
        )
        .expect("skill document");
        let env = environment(home, project);
        let registry = AgentAdapterRegistry::register();
        let adapter = registry
            .get(&AgentId::ClaudeCode)
            .expect("registry")
            .expect("Claude adapter");
        let rows = adapter
            .list_native_skills(&env, &[])
            .expect("native listing");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "native");
        assert_eq!(rows[0].scope, Scope::User);
    }

    #[test]
    fn claude_and_codex_paths_match_their_native_roots() {
        use crate::adapter::AgentAdapterRegistry;

        let temp = tempfile::tempdir().expect("temporary directory");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let env = environment(home.clone(), project.clone());
        let registry = AgentAdapterRegistry::register();
        let codex = registry
            .get(&AgentId::Codex)
            .expect("registry")
            .expect("Codex adapter");
        let codex_user_skills = codex.paths(ItemKind::Skill, Scope::User, &env);
        assert_eq!(codex_user_skills.len(), 2);
        assert_eq!(codex_user_skills[0].path, home.join(".codex/skills"));
        assert_eq!(codex_user_skills[1].path, home.join(".agents/skills"));
        assert_eq!(
            codex.paths(ItemKind::Skill, Scope::Local, &env)[0].path,
            project.join(".agents/skills")
        );
        assert_eq!(
            codex.paths(ItemKind::Rule, Scope::User, &env)[0].path,
            home.join(".codex/AGENTS.md")
        );
        assert_eq!(
            codex.paths(ItemKind::Rule, Scope::Project, &env)[0].path,
            project.join("AGENTS.md")
        );

        let claude = registry
            .get(&AgentId::ClaudeCode)
            .expect("registry")
            .expect("Claude adapter");
        assert_eq!(
            claude.paths(ItemKind::Rule, Scope::User, &env)[0].path,
            home.join(".claude/CLAUDE.md")
        );
        assert_eq!(
            claude.paths(ItemKind::Rule, Scope::Project, &env)[1].path,
            project.join("CLAUDE.local.md")
        );
        assert_eq!(
            claude.paths(ItemKind::Rule, Scope::Local, &env)[0].path,
            project.join("CLAUDE.local.md")
        );
    }
}
