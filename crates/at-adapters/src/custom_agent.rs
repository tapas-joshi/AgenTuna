//! Folder-defined custom agent adapter.

use std::path::{Component, Path, PathBuf};

use at_model::{AgentId, ItemKind, Scope};

use crate::adapter::{
    AgentAdapter, AgentCapabilities, AgentDetection, AgentEnvironment, AgentError,
    AgentPathCandidate, DetectionState,
};

/// Format used for a custom agent's delegated MCP document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CustomMcpFormat {
    /// JSON/JSONC MCP document.
    Json,
    /// TOML MCP document.
    Toml,
    /// This custom agent does not expose MCP configuration.
    None,
}

/// Declarative folder layout for one custom agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAgentSpec {
    /// Stable custom-agent identifier, distinct from its display name.
    pub id: String,
    /// User-facing agent name.
    pub display_name: String,
    /// Absolute agent root folder.
    pub root: PathBuf,
    /// Relative skills root within the agent root.
    pub skills_subfolder: PathBuf,
    /// MCP document path, relative to root unless absolute.
    pub mcp_file: Option<PathBuf>,
    /// MCP document format.
    pub mcp_format: CustomMcpFormat,
    /// Rule file names relative to the root or to the project root.
    pub rule_files: Vec<PathBuf>,
    /// Whether the agent reads symlinked skill deployments.
    pub supports_symlink: bool,
}

impl CustomAgentSpec {
    /// Validates the custom-agent folder declaration against its injected filesystem root.
    ///
    /// # Errors
    /// Returns a specific typed validation error for an unsafe/missing root or inconsistent MCP
    /// declaration.
    pub fn validate(&self) -> Result<(), AgentError> {
        if !self.root.is_absolute() || self.root == Path::new("/") {
            return Err(AgentError::Unsupported {
                agent: AgentId::Custom(self.id.clone()),
                kind: "custom root must be an absolute, non-root directory".to_owned(),
            });
        }
        if !self.skills_subfolder.is_relative()
            || self.skills_subfolder.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(AgentError::Unsupported {
                agent: AgentId::Custom(self.id.clone()),
                kind: "custom skills subfolder must be a safe relative path".to_owned(),
            });
        }
        if !self.root.is_dir() {
            return Err(AgentError::NotInstalled {
                agent: AgentId::Custom(self.id.clone()),
                path: self.root.clone(),
            });
        }
        if matches!(self.mcp_format, CustomMcpFormat::None) != self.mcp_file.is_none() {
            return Err(AgentError::Unsupported {
                agent: AgentId::Custom(self.id.clone()),
                kind: "custom MCP path and format must both be configured or both be none"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// Adapter instance bound to one validated custom-agent specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAgentAdapter {
    spec: CustomAgentSpec,
}

impl CustomAgentAdapter {
    /// Validates and constructs an adapter for one registry row.
    ///
    /// # Errors
    /// Returns the validation failure without creating a partially configured adapter.
    pub fn new(spec: CustomAgentSpec) -> Result<Self, AgentError> {
        spec.validate()?;
        Ok(Self { spec })
    }

    /// Returns the validated specification bound to this adapter.
    #[must_use]
    pub fn spec(&self) -> &CustomAgentSpec {
        &self.spec
    }
}

impl AgentAdapter for CustomAgentAdapter {
    fn id(&self) -> AgentId {
        AgentId::Custom(self.spec.id.clone())
    }
    fn supports_mcp(&self) -> bool {
        !matches!(self.spec.mcp_format, CustomMcpFormat::None)
    }

    fn capabilities(&self, _env: &AgentEnvironment) -> AgentCapabilities {
        AgentCapabilities {
            mcp: None,
            supports_symlink: self.spec.supports_symlink,
            supports_rules: !self.spec.rule_files.is_empty(),
        }
    }

    fn detect(&self, env: &AgentEnvironment) -> AgentDetection {
        let candidates = self.paths(ItemKind::Skill, Scope::User, env);
        AgentDetection {
            state: if self.spec.root.is_dir() {
                DetectionState::Installed
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
        let base = if user_scope {
            self.spec.root.clone()
        } else {
            let Some(project) = env.project.as_ref() else {
                return Vec::new();
            };
            project.clone()
        };
        match kind {
            ItemKind::Skill => vec![candidate(
                "skills_dir",
                if user_scope {
                    self.spec.root.join(&self.spec.skills_subfolder)
                } else {
                    base.join(&self.spec.skills_subfolder)
                },
                scope,
                None,
                true,
            )],
            ItemKind::McpServer => {
                if matches!(self.spec.mcp_format, CustomMcpFormat::None) {
                    return Vec::new();
                }
                let Some(path) = self.spec.mcp_file.as_ref() else {
                    return Vec::new();
                };
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    base.join(path)
                };
                vec![candidate(
                    "mcp_file",
                    path,
                    scope,
                    Some(match self.spec.mcp_format {
                        CustomMcpFormat::Json => "json",
                        CustomMcpFormat::Toml => "toml",
                        CustomMcpFormat::None => "",
                    }),
                    true,
                )]
            }
            ItemKind::Rule if scope == Scope::Global => Vec::new(),
            ItemKind::Rule => self
                .spec
                .rule_files
                .iter()
                .map(|path| {
                    let path = if path.is_absolute() {
                        path.clone()
                    } else {
                        resolve_on_disk(&base, path)
                    };
                    candidate("rules_file", path, scope, Some("markdown"), true)
                })
                .collect(),
        }
    }
    fn read_io(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Result<crate::adapter::AgentIoDocument, AgentError> {
        if kind == ItemKind::McpServer && matches!(self.spec.mcp_format, CustomMcpFormat::None) {
            return Err(AgentError::Unsupported {
                agent: self.id(),
                kind: "MCP format none".to_owned(),
            });
        }
        if kind == ItemKind::McpServer {
            return Err(AgentError::Delegate {
                source: crate::adapter::AgentDelegateFailure::new(
                    "custom MCP read is delegated to the registered TM adapter",
                ),
            });
        }
        if kind == ItemKind::Skill {
            // The declared skills folder is scanned into the shared native listing; reading
            // it as one file would fail with EISDIR.
            return crate::adapter::read_io_default(self, kind, scope, env);
        }
        let candidates = self.paths(kind, scope, env);
        let candidate = candidates
            .into_iter()
            .find(|candidate| candidate.primary)
            .ok_or_else(|| AgentError::Unsupported {
                agent: self.id(),
                kind: kind.to_string(),
            })?;
        let content = std::fs::read(&candidate.path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                AgentError::NotInstalled {
                    agent: self.id(),
                    path: candidate.path.clone(),
                }
            } else if source.kind() == std::io::ErrorKind::PermissionDenied {
                AgentError::Permission {
                    path: candidate.path.clone(),
                }
            } else {
                AgentError::Io {
                    path: candidate.path.clone(),
                    source,
                }
            }
        })?;
        Ok(crate::adapter::AgentIoDocument {
            path: candidate.path,
            content,
        })
    }

    fn write_io(
        &self,
        deployment: &crate::adapter::AgentDeployment,
    ) -> Result<crate::adapter::AgentIoDocument, AgentError> {
        if deployment.kind == ItemKind::McpServer {
            return Err(AgentError::Unsupported {
                agent: self.id(),
                kind: "custom MCP write must use the registered TM adapter".to_owned(),
            });
        }
        Ok(crate::adapter::AgentIoDocument {
            path: deployment.path.clone(),
            content: deployment.content.clone(),
        })
    }
}

/// Resolves a declared rule path under `base`, preferring the spelling found on disk.
///
/// Rule file names are matched case-insensitively (macOS volumes usually are, so a spec may
/// say `AGENTS.md` while the file is `agents.md`). Returning the entry's real name keeps a
/// later write on that file instead of creating a second, differently-cased one.
fn resolve_on_disk(base: &Path, declared: &Path) -> PathBuf {
    // Never shortcut on `base.join(declared).exists()`: on a case-insensitive volume that is
    // true for `AGENTS.md`/`agents.md` alike and would report the declared spelling.
    let mut current = base.to_path_buf();
    for component in declared.components() {
        let Component::Normal(part) = component else {
            return base.join(declared);
        };
        let wanted = part.to_string_lossy().into_owned();
        let mut exact = None;
        let mut folded = None;
        if let Ok(entries) = std::fs::read_dir(&current) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy() == wanted {
                    exact = Some(name);
                    break;
                }
                if folded.is_none() && name.to_string_lossy().eq_ignore_ascii_case(&wanted) {
                    folded = Some(name);
                }
            }
        }
        match exact.or(folded) {
            Some(name) => current.push(name),
            None => return base.join(declared),
        }
    }
    current
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
    use std::path::PathBuf;
    use std::sync::Arc;

    use at_model::{ItemKind, Scope};

    use super::{CustomAgentAdapter, CustomAgentSpec, CustomMcpFormat};
    use crate::adapter::{AgentAdapter, AgentEnvironment, AgentError};

    fn spec(root: PathBuf) -> CustomAgentSpec {
        CustomAgentSpec {
            id: "mytool".to_owned(),
            display_name: "My Tool".to_owned(),
            root,
            skills_subfolder: PathBuf::from("skills"),
            mcp_file: Some(PathBuf::from("mcp.json")),
            mcp_format: CustomMcpFormat::Json,
            rule_files: vec![PathBuf::from("RULES.md")],
            supports_symlink: false,
        }
    }

    #[test]
    fn custom_adapter_uses_declared_roots_and_capabilities() {
        let temp = tempfile::tempdir().expect("root");
        let root = temp.path().join("tool");
        std::fs::create_dir_all(&root).expect("agent root");
        let adapter = CustomAgentAdapter::new(spec(root.clone())).expect("valid spec");
        let env = AgentEnvironment {
            home: temp.path().join("home"),
            project: Some(temp.path().join("project")),
            variables: BTreeMap::new(),
        };
        assert_eq!(
            adapter.paths(ItemKind::Skill, Scope::User, &env)[0].path,
            root.join("skills")
        );
        assert_eq!(
            adapter.paths(ItemKind::Skill, Scope::Local, &env)[0].path,
            temp.path().join("project/skills")
        );
        assert_eq!(
            adapter.paths(ItemKind::McpServer, Scope::User, &env)[0]
                .format
                .as_deref(),
            Some("json")
        );
        assert!(!adapter.capabilities(&env).supports_symlink);
        assert!(adapter.capabilities(&env).supports_rules);

        let mut no_mcp = spec(root);
        no_mcp.mcp_file = None;
        no_mcp.mcp_format = CustomMcpFormat::None;
        let no_mcp = CustomAgentAdapter::new(no_mcp).expect("valid no-MCP spec");
        assert!(
            no_mcp
                .paths(ItemKind::McpServer, Scope::User, &env)
                .is_empty()
        );
        assert!(matches!(
            no_mcp.read_io(ItemKind::McpServer, Scope::User, &env),
            Err(AgentError::Unsupported { .. })
        ));
        let registry = crate::adapter::AgentAdapterRegistry::register();
        registry.insert(Arc::new(no_mcp)).expect("register");
        assert!(matches!(
            registry.read_io(
                &at_model::AgentId::Custom("mytool".to_owned()),
                ItemKind::McpServer,
                Scope::User,
                &env
            ),
            Err(AgentError::Unsupported { kind, .. }) if kind == "MCP configuration"
        ));
    }

    #[test]
    fn custom_adapter_rejects_invalid_roots_without_registration() {
        let relative = spec(PathBuf::from("relative"));
        assert!(matches!(
            relative.validate(),
            Err(AgentError::Unsupported { kind, .. }) if kind.contains("absolute")
        ));
        let root = spec(PathBuf::from("/"));
        assert!(matches!(
            root.validate(),
            Err(AgentError::Unsupported { kind, .. }) if kind.contains("non-root")
        ));
        let temp = tempfile::tempdir().expect("root");
        let missing = spec(temp.path().join("missing"));
        assert!(matches!(
            missing.validate(),
            Err(AgentError::NotInstalled { .. })
        ));
    }
}
