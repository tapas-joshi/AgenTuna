//! Native omp adapter paths and detection.

use std::path::PathBuf;

use at_model::{AgentId, ItemKind, Scope};

use crate::adapter::{
    AgentAdapter, AgentCapabilities, AgentDetection, AgentDetectionWithVersion, AgentEnvironment,
    AgentPathCandidate, DetectionState, parse_probe_version,
};
use crate::mcp_capabilities::capabilities;

/// Adapter for omp's native configuration and skill locations.
#[derive(Debug, Clone, Copy, Default)]
pub struct OmpAdapter;

impl AgentAdapter for OmpAdapter {
    fn id(&self) -> AgentId {
        AgentId::Omp
    }

    fn capabilities(&self, env: &AgentEnvironment) -> AgentCapabilities {
        AgentCapabilities {
            mcp: capabilities(&AgentId::Omp, &env.home),
            supports_symlink: true,
            supports_rules: true,
        }
    }

    fn detect(&self, env: &AgentEnvironment) -> AgentDetection {
        let candidates = self.paths(ItemKind::Skill, Scope::User, env);
        let root = env.home.join(".omp/agent");
        let found = root.is_dir();
        AgentDetection {
            state: if found {
                DetectionState::Installed
            } else {
                DetectionState::NotInstalled
            },
            candidates,
        }
    }
    fn detect_with_version(
        &self,
        env: &AgentEnvironment,
        probe_output: Option<&str>,
    ) -> AgentDetectionWithVersion {
        AgentDetectionWithVersion {
            presence: self.detect(env),
            version: probe_output.and_then(parse_probe_version),
        }
    }

    fn paths(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<AgentPathCandidate> {
        let user_scope = matches!(scope, Scope::Global | Scope::User);
        let root = env.project.as_deref();
        match kind {
            ItemKind::Skill if user_scope => vec![
                candidate(
                    "skills_dir",
                    env.home.join(".omp/agent/managed-skills"),
                    scope,
                    None,
                    true,
                ),
                candidate(
                    "skills_dir",
                    env.home.join(".omp/agent/skills"),
                    scope,
                    None,
                    false,
                ),
            ],
            ItemKind::Skill => root.map_or_else(Vec::new, |project| {
                vec![
                    candidate(
                        "skills_dir",
                        project.join(".agents/skills"),
                        scope,
                        None,
                        true,
                    ),
                    candidate(
                        "skills_dir",
                        project.join(".agent/skills"),
                        scope,
                        None,
                        false,
                    ),
                ]
            }),
            ItemKind::Rule if user_scope => {
                let path = env
                    .variables
                    .get("OMP_AGENTS_MD_PATH")
                    .map_or_else(|| env.home.join(".omp/agent/AGENTS.md"), PathBuf::from);
                vec![candidate("rules_file", path, scope, Some("markdown"), true)]
            }
            ItemKind::Rule => root.map_or_else(Vec::new, |project| {
                vec![candidate(
                    "rules_file",
                    project.join("AGENTS.md"),
                    scope,
                    Some("markdown"),
                    true,
                )]
            }),
            ItemKind::McpServer if user_scope => vec![candidate(
                "mcp_file",
                env.home.join(".omp/agent/mcp.json"),
                scope,
                Some("json"),
                true,
            )],
            ItemKind::McpServer => root.map_or_else(Vec::new, |project| {
                vec![candidate(
                    "mcp_file",
                    project.join("mcp.json"),
                    scope,
                    Some("json"),
                    true,
                )]
            }),
        }
    }

    fn list_native_skills(
        &self,
        env: &AgentEnvironment,
        store_roots: &[PathBuf],
    ) -> Result<Vec<crate::native_skills::NativeSkill>, crate::adapter::AgentError> {
        let roots = [Scope::User, Scope::Project]
            .into_iter()
            .flat_map(|scope| self.paths(ItemKind::Skill, scope, env))
            .collect::<Vec<_>>();
        let mut skills =
            crate::native_skills::list_native_skills(&self.id(), env, store_roots, roots)?;
        skills.retain(|skill| {
            !skill
                .path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".bak-"))
        });
        Ok(skills)
    }

    fn list_native(
        &self,
        env: &AgentEnvironment,
    ) -> Result<Vec<crate::adapter::NativeItem>, crate::adapter::AgentError> {
        let native_skills = self.list_native_skills(env, &[])?;
        let mut items = native_skills
            .into_iter()
            .map(|skill| crate::adapter::NativeItem {
                kind: ItemKind::Skill,
                scope: skill.scope,
                name: skill.name,
                path: skill.path,
            })
            .collect::<Vec<_>>();
        for scope in [Scope::User, Scope::Project] {
            for candidate in self.paths(ItemKind::Rule, scope, env) {
                match std::fs::symlink_metadata(&candidate.path) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        items.push(crate::adapter::NativeItem {
                            kind: ItemKind::Rule,
                            scope,
                            name: String::from("AGENTS.md"),
                            path: candidate.path,
                        })
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        return Err(crate::adapter::AgentError::Permission {
                            path: candidate.path,
                        });
                    }
                    Err(source) => {
                        return Err(crate::adapter::AgentError::Io {
                            path: candidate.path,
                            source,
                        });
                    }
                }
            }
        }
        items.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(items)
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

    use super::OmpAdapter;
    use crate::adapter::{AgentAdapter, AgentEnvironment, DetectionState};

    #[test]
    fn omp_paths_and_nonexistent_skills_root_are_supported() {
        let root = tempfile::tempdir().expect("tempdir");
        let env = AgentEnvironment {
            home: root.path().join("home"),
            project: Some(root.path().join("repo")),
            variables: BTreeMap::new(),
        };
        std::fs::create_dir_all(env.home.join(".omp/agent")).expect("omp root");
        let adapter = OmpAdapter;
        assert_eq!(adapter.detect(&env).state, DetectionState::Installed);
        let probed = adapter.detect_with_version(&env, Some("omp version v1.2.3"));
        assert_eq!(probed.version.as_deref(), Some("1.2.3"));
        let paths = adapter.paths(ItemKind::Skill, Scope::User, &env);
        assert_eq!(paths[0].path, env.home.join(".omp/agent/managed-skills"));
        assert_eq!(paths[1].path, env.home.join(".omp/agent/skills"));
        assert!(!paths[1].path.exists());
        let local_paths = adapter.paths(ItemKind::Skill, Scope::Local, &env);
        assert_eq!(
            local_paths[0].path,
            env.project.as_ref().unwrap().join(".agents/skills")
        );
        assert_eq!(
            local_paths[1].path,
            env.project.as_ref().unwrap().join(".agent/skills")
        );
        let managed = env.home.join(".omp/agent/managed-skills");
        std::fs::create_dir_all(managed.join(".bak-old")).expect("backup skill");
        std::fs::create_dir_all(managed.join("sample")).expect("native skill");
        std::fs::write(managed.join(".bak-old/SKILL.md"), b"# backup").expect("backup manifest");
        std::fs::write(managed.join("sample/SKILL.md"), b"# sample").expect("skill manifest");
        let items = adapter.list_native(&env).expect("native items");
        assert!(items.iter().any(|item| item.name == "sample"));
        assert!(!items.iter().any(|item| item.path.ends_with(".bak-old")));
    }
}
