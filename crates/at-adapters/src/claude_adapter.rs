//! Claude Code native paths and reviewed local-skill deployment planning.

use std::path::{Path, PathBuf};

use at_model::{AgentId, ChangeSet, DeployMode, ExpectedState, FileOp, OperationKind, Scope};

use crate::adapter::{
    AgentAdapter, AgentCapabilities, AgentDetection, AgentEnvironment, AgentError,
    AgentPathCandidate, BuiltinAdapter,
};
use crate::claude_settings::{ClaudeProjectSettings, settings_local_path};

/// Registered Claude Code adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeAdapter;

impl ClaudeAdapter {
    /// Returns Claude's project-local settings document path.
    ///
    /// # Errors
    /// Returns [`AgentError::Unsupported`] when no project root is available.
    pub fn settings_path(&self, env: &AgentEnvironment) -> Result<PathBuf, AgentError> {
        let project = env
            .project
            .as_deref()
            .ok_or_else(|| AgentError::Unsupported {
                agent: AgentId::ClaudeCode,
                kind: "project settings path requires a project root".to_owned(),
            })?;
        Ok(settings_local_path(project))
    }

    /// Parses the project-local Claude settings file without changing its bytes.
    pub fn parse_settings(
        &self,
        env: &AgentEnvironment,
        source: String,
    ) -> Result<ClaudeProjectSettings, AgentError> {
        ClaudeProjectSettings::parse(self.settings_path(env)?, source)
    }

    /// Plans Local skill link deployment and the matching Git exclude line.
    ///
    /// The Git exclude only keeps the deployed path out of Git; it does not hide it from Claude.
    /// An existing deployment path is refused rather than overwritten. Applying the returned
    /// operations remains the caller's reviewed at-fs responsibility.
    ///
    /// # Errors
    /// Returns a typed error for an invalid slug, missing project, existing destination, invalid
    /// exclude bytes, or invalid ChangeSet.
    pub fn plan_local_skill(
        &self,
        project: &Path,
        slug: &str,
        store_skill: &Path,
        exclude_contents: Option<&[u8]>,
        created_at_unix_seconds: u64,
    ) -> Result<ChangeSet, AgentError> {
        validate_slug(slug)?;
        let skills_root = project.join(".claude/skills");
        let destination = skills_root.join(slug);
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err(AgentError::Unsupported {
                    agent: AgentId::ClaudeCode,
                    kind: format!(
                        "refusing to overwrite existing skill path {}",
                        destination.display()
                    ),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(AgentError::Permission { path: destination });
            }
            Err(source) => {
                return Err(AgentError::Io {
                    path: destination,
                    source,
                });
            }
        }
        let exclude_path = PathBuf::from(".git/info/exclude");
        let old = exclude_contents.unwrap_or_default();
        let old_text = std::str::from_utf8(old).map_err(|_| AgentError::Parse {
            path: project.join(&exclude_path),
            line: 1,
            col: 1,
        })?;
        let entry = format!("/.claude/skills/{slug}");
        let new_text = append_exclude_entry(old_text, &entry);
        let old_hash = at_fs::sha256_hex(old);
        let new_hash = at_fs::sha256_hex(new_text.as_bytes());
        let expected = if exclude_contents.is_some() {
            ExpectedState::hash(&old_hash).map_err(|error| AgentError::Io {
                path: project.join(&exclude_path),
                source: std::io::Error::other(error),
            })?
        } else {
            ExpectedState::absent()
        };
        let operation_kind = OperationKind::parse("deploy").map_err(|error| AgentError::Io {
            path: project.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        let change = ChangeSet {
            id: format!("claude-local-{}", &new_hash[..16]),
            preview_id: old_hash,
            operation_kind,
            scope: Some(Scope::Local),
            project_id: None,
            summary: format!("Deploy Claude skill `{slug}` locally"),
            ops: vec![
                FileOp::Symlink {
                    link_path: destination
                        .strip_prefix(project)
                        .map_err(|error| AgentError::Io {
                            path: destination.clone(),
                            source: std::io::Error::other(error),
                        })?
                        .to_path_buf(),
                    target: store_skill.to_path_buf(),
                    mode: DeployMode::Link,
                },
                FileOp::Write {
                    path: exclude_path,
                    bytes: new_text.into_bytes(),
                    expected,
                    follow_symlink: false,
                    backup: true,
                },
            ],
            created_at_unix_seconds,
        };
        change.validate().map_err(|error| AgentError::Io {
            path: project.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        Ok(change)
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> AgentId {
        AgentId::ClaudeCode
    }

    fn capabilities(&self, env: &AgentEnvironment) -> AgentCapabilities {
        BuiltinAdapter { id: self.id() }.capabilities(env)
    }

    fn detect(&self, env: &AgentEnvironment) -> AgentDetection {
        BuiltinAdapter { id: self.id() }.detect(env)
    }

    fn detect_with_version(
        &self,
        env: &AgentEnvironment,
        probe_output: Option<&str>,
    ) -> crate::adapter::AgentDetectionWithVersion {
        BuiltinAdapter { id: self.id() }.detect_with_version(env, probe_output)
    }

    fn paths(
        &self,
        kind: at_model::ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<AgentPathCandidate> {
        BuiltinAdapter { id: self.id() }.paths(kind, scope, env)
    }

    fn plan_settings_server_enabled(
        &self,
        env: &AgentEnvironment,
        source: &str,
        server_name: &str,
        enabled: bool,
        created_at_unix_seconds: u64,
    ) -> Result<ChangeSet, AgentError> {
        let settings = self.parse_settings(env, source.to_owned())?;
        let rendered = settings.set_server_enabled(server_name, enabled)?;
        settings.change_set(
            Path::new(".claude/settings.local.json"),
            &rendered,
            created_at_unix_seconds,
        )
    }

    fn plan_local_skill(
        &self,
        project: &Path,
        slug: &str,
        store_skill: &Path,
        exclude_contents: Option<&[u8]>,
        created_at_unix_seconds: u64,
    ) -> Result<ChangeSet, AgentError> {
        ClaudeAdapter::plan_local_skill(
            self,
            project,
            slug,
            store_skill,
            exclude_contents,
            created_at_unix_seconds,
        )
    }
}

fn validate_slug(slug: &str) -> Result<(), AgentError> {
    let mut components = Path::new(slug).components();
    if slug.is_empty()
        || components
            .next()
            .is_none_or(|component| !matches!(component, std::path::Component::Normal(_)))
        || components.next().is_some()
        || slug.contains(['/', '\\'])
    {
        return Err(AgentError::Unsupported {
            agent: AgentId::ClaudeCode,
            kind: "skill slug must be one path component".to_owned(),
        });
    }
    Ok(())
}

fn append_exclude_entry(contents: &str, entry: &str) -> String {
    let has_entry = contents
        .lines()
        .any(|line| line.trim_end_matches('\r') == entry);
    if has_entry {
        return contents.to_owned();
    }
    let newline = if contents.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut result = String::with_capacity(contents.len() + entry.len() + newline.len());
    result.push_str(contents);
    if !contents.is_empty() && !contents.ends_with('\n') {
        result.push_str(newline);
    }
    result.push_str(entry);
    result.push_str(newline);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_plan_adds_git_exclude_once_and_refuses_existing_destination() {
        let temp = tempfile::tempdir().expect("project");
        let project = temp.path();
        let store = temp.path().join("store/skill");
        std::fs::create_dir_all(&store).expect("store skill");
        let adapter = ClaudeAdapter;
        let existing = b"# user rule\n/.claude/skills/demo\n";
        let change = adapter
            .plan_local_skill(project, "demo", &store, Some(existing), 1)
            .expect("plan");
        assert_eq!(change.ops.len(), 2);
        assert!(matches!(&change.ops[1], FileOp::Write { bytes, .. } if bytes == existing));
        std::fs::create_dir_all(project.join(".claude/skills/demo")).expect("unmanaged path");
        assert!(
            adapter
                .plan_local_skill(project, "demo", &store, Some(existing), 2)
                .is_err()
        );
    }

    #[test]
    fn local_exclude_appends_once_and_preserves_crlf() {
        assert_eq!(
            append_exclude_entry("# Git\r\n", "/.claude/skills/a"),
            "# Git\r\n/.claude/skills/a\r\n"
        );
        assert_eq!(
            append_exclude_entry("/.claude/skills/a\n", "/.claude/skills/a"),
            "/.claude/skills/a\n"
        );
    }

    #[test]
    fn registry_routes_claude_settings_and_local_skill_plans() {
        use crate::adapter::{AgentAdapterRegistry, AgentEnvironment};
        use std::collections::BTreeMap;

        let temp = tempfile::tempdir().expect("sandbox");
        let project = temp.path().join("project");
        let store_skill = temp.path().join("store/demo");
        std::fs::create_dir_all(&store_skill).expect("store skill");
        let env = AgentEnvironment {
            home: temp.path().join("home"),
            project: Some(project.clone()),
            variables: BTreeMap::new(),
        };
        let registry = AgentAdapterRegistry::register();
        let adapter = registry
            .get(&AgentId::ClaudeCode)
            .expect("registry")
            .expect("Claude adapter");
        let settings = adapter
            .plan_settings_server_enabled(
                &env,
                "{\"permissions\":{},\"enabledMcpjsonServers\":[],\"disabledMcpjsonServers\":[]}",
                "server",
                true,
                1,
            )
            .expect("settings plan");
        assert!(
            matches!(settings.ops.as_slice(), [FileOp::Write { path, .. }] if path == Path::new(".claude/settings.local.json"))
        );
        let local = adapter
            .plan_local_skill(&project, "demo", &store_skill, None, 2)
            .expect("local skill plan");
        assert!(
            matches!(local.ops.last(), Some(FileOp::Write { path, .. }) if path == Path::new(".git/info/exclude"))
        );
    }
}
