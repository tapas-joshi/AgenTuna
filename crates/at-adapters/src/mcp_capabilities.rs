//! Verified MCP capability declarations and explicit unsupported-field acknowledgement.

use std::path::{Path, PathBuf};

use at_model::AgentId;

/// Confidence level for an agent's environment-variable expansion behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapabilityVerification {
    /// Behavior was verified against the agent.
    Verified,
    /// Behavior has not been measured; do not promise it to users.
    Unverified,
    /// The agent does not support the behavior.
    Unsupported,
}

/// Constraints on MCP server names for an agent's native config format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpNameRules {
    /// Whether an empty name is rejected.
    pub non_empty: bool,
    /// Whether names are stored as quoted keys in TOML documents.
    pub toml_always_quoted: bool,
}

/// Agent-specific MCP format capabilities used by forms and planners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMcpCapabilities {
    /// Adapter identity.
    pub agent: AgentId,
    /// Whether the agent is installed in the detected environment.
    pub installed: bool,
    /// Confidence that streamable HTTP transport is supported.
    pub supports_http: CapabilityVerification,
    /// Confidence that HTTP headers are supported and modeled.
    pub supports_headers: CapabilityVerification,
    /// Confidence that environment values are supported.
    pub supports_env: CapabilityVerification,
    /// Confidence that names listed in `env_vars` are passed through from the parent process.
    pub supports_env_vars: CapabilityVerification,
    /// Whether `${NAME}` references in environment values are expanded.
    pub env_interpolation: CapabilityVerification,
    /// Environment interpolation confidence specific to each native config surface.
    pub interpolation_by_path: Vec<(PathBuf, CapabilityVerification)>,
    /// Confidence that the agent honors an enabled/disabled flag per server.
    pub supports_enabled_flag: CapabilityVerification,
    /// Confidence that native MCP config can be scoped to a project.
    pub supports_project_scope: CapabilityVerification,
    /// Name constraints for native server keys.
    pub name_rules: McpNameRules,
    /// Candidate native MCP config locations under the injected home directory.
    pub config_paths: Vec<PathBuf>,
}

/// An MCP field omitted from the rendered agent configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnsupportedField {
    /// HTTP headers cannot be represented by the target agent format.
    Headers,
    /// OAuth profile cannot be represented by the target agent format.
    OAuth,
}

impl UnsupportedField {
    /// Stable lower-case name shown in acknowledgement warnings and previews.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Headers => "headers",
            Self::OAuth => "oauth",
        }
    }
}

/// MCP data retained for deployment after unsupported fields were explicitly acknowledged.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedEntry {
    /// The transport fields representable by the target adapter.
    pub transport: at_model::McpTransport,
    /// Fields deliberately dropped from this rendering.
    pub unsupported: Vec<UnsupportedField>,
}

/// A deployment cannot discard unsupported fields until the user acknowledges the warning.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unsupported MCP fields require explicit acknowledgement")]
pub struct UnsupportedFieldsUnacknowledged;

/// Returns the capability declaration for a built-in agent and injected home directory.
#[must_use]
pub fn capabilities(agent: &AgentId, home: &Path) -> Option<AgentMcpCapabilities> {
    let name_rules = McpNameRules {
        non_empty: true,
        toml_always_quoted: false,
    };
    let (
        installed,
        supports_http,
        supports_headers,
        supports_env,
        supports_env_vars,
        env_interpolation,
        supports_enabled_flag,
        supports_project_scope,
        config_paths,
    ) = match agent {
        AgentId::ClaudeCode => (
            home.join(".claude").exists(),
            CapabilityVerification::Verified,
            CapabilityVerification::Verified,
            CapabilityVerification::Verified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unsupported,
            CapabilityVerification::Verified,
            vec![home.join(".claude.json"), home.join(".mcp.json")],
        ),
        AgentId::Codex => (
            home.join(".codex").exists(),
            CapabilityVerification::Verified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Verified,
            CapabilityVerification::Verified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Verified,
            CapabilityVerification::Unsupported,
            vec![home.join(".codex/config.toml")],
        ),
        AgentId::Omp => (
            home.join(".omp").exists(),
            CapabilityVerification::Verified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Verified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Verified,
            CapabilityVerification::Verified,
            vec![home.join(".omp/agent/mcp.json")],
        ),
        AgentId::Cursor => (
            home.join(".cursor").exists(),
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            vec![home.join(".cursor/mcp.json")],
        ),
        AgentId::Custom(_) => (
            false,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            CapabilityVerification::Unverified,
            Vec::new(),
        ),
    };
    let interpolation_by_path = config_paths
        .iter()
        .map(|path| {
            let confidence = if matches!(agent, AgentId::ClaudeCode) && path.ends_with(".mcp.json")
            {
                CapabilityVerification::Verified
            } else {
                env_interpolation
            };
            (path.clone(), confidence)
        })
        .collect();
    Some(AgentMcpCapabilities {
        agent: agent.clone(),
        installed,
        supports_http,
        supports_headers,
        supports_env,
        supports_env_vars,
        env_interpolation,
        interpolation_by_path,
        supports_enabled_flag,
        supports_project_scope,
        name_rules,
        config_paths,
    })
}

/// Projects canonical MCP transport fields to those representable by an agent.
#[must_use]
pub fn project_transport(agent: &AgentId, transport: &at_model::McpTransport) -> RenderedEntry {
    let supports_headers = !matches!(agent, AgentId::Codex | AgentId::Cursor | AgentId::Custom(_));
    let mut unsupported = Vec::new();
    let projected = match transport {
        at_model::McpTransport::Stdio {
            command,
            args,
            cwd,
            env,
        } => at_model::McpTransport::Stdio {
            command: command.clone(),
            args: args.clone(),
            cwd: cwd.clone(),
            env: env.clone(),
        },
        at_model::McpTransport::Http {
            url,
            headers,
            oauth,
        } => {
            let projected_headers = if supports_headers {
                headers.clone()
            } else {
                if !headers.is_empty() {
                    unsupported.push(UnsupportedField::Headers);
                }
                Default::default()
            };
            if oauth.is_some() {
                unsupported.push(UnsupportedField::OAuth);
            }
            at_model::McpTransport::Http {
                url: url.clone(),
                headers: projected_headers,
                oauth: None,
            }
        }
    };
    RenderedEntry {
        transport: projected,
        unsupported,
    }
}

/// Enforces the explicit drop-with-warning policy before deployment planning.
///
/// # Errors
/// Returns [`UnsupportedFieldsUnacknowledged`] if unsupported fields exist and the user has
/// not acknowledged their omission.
pub fn require_unsupported_acknowledgement(
    entry: &RenderedEntry,
    acknowledged: bool,
) -> Result<(), UnsupportedFieldsUnacknowledged> {
    if entry.unsupported.is_empty() || acknowledged {
        Ok(())
    } else {
        Err(UnsupportedFieldsUnacknowledged)
    }
}

#[cfg(test)]
mod tests {
    use at_model::{AgentId, EnvValue, McpTransport};
    use std::fs;
    use tempfile::tempdir;

    use super::{
        CapabilityVerification, UnsupportedField, capabilities, project_transport,
        require_unsupported_acknowledgement,
    };

    #[test]
    fn capability_matrix_is_explicit_and_uninstalled_agents_are_marked() {
        let home = tempdir().expect("home");
        let cases = [
            (
                AgentId::ClaudeCode,
                CapabilityVerification::Verified,
                CapabilityVerification::Verified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unsupported,
                CapabilityVerification::Verified,
            ),
            (
                AgentId::Codex,
                CapabilityVerification::Verified,
                CapabilityVerification::Verified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Verified,
                CapabilityVerification::Unsupported,
            ),
            (
                AgentId::Omp,
                CapabilityVerification::Verified,
                CapabilityVerification::Verified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Verified,
                CapabilityVerification::Verified,
            ),
            (
                AgentId::Cursor,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
            ),
            (
                AgentId::custom("custom_probe").expect("custom"),
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
                CapabilityVerification::Unverified,
            ),
        ];
        for (agent, http, env, interpolation, enabled, project) in cases {
            let cap = capabilities(&agent, home.path()).expect("capability");
            assert!(!cap.installed);
            assert_eq!(cap.supports_http, http);
            assert_eq!(
                cap.supports_headers,
                if matches!(agent, AgentId::ClaudeCode) {
                    CapabilityVerification::Verified
                } else {
                    CapabilityVerification::Unverified
                }
            );
            assert_eq!(cap.supports_env, env);
            assert_eq!(
                cap.supports_env_vars,
                if matches!(agent, AgentId::Codex) {
                    CapabilityVerification::Verified
                } else {
                    CapabilityVerification::Unverified
                }
            );
            assert_eq!(cap.env_interpolation, interpolation);
            if matches!(agent, AgentId::ClaudeCode) {
                assert_eq!(
                    cap.interpolation_by_path[0].1,
                    CapabilityVerification::Unverified
                );
                assert_eq!(
                    cap.interpolation_by_path[1].1,
                    CapabilityVerification::Verified
                );
            }
            assert_eq!(cap.supports_enabled_flag, enabled);
            assert_eq!(cap.supports_project_scope, project);
        }
        fs::create_dir_all(home.path().join(".cursor")).expect("cursor directory");
        assert!(
            capabilities(&AgentId::Cursor, home.path())
                .expect("cursor")
                .installed
        );
    }

    #[test]
    fn codex_http_projection_drops_headers_and_requires_acknowledgement() {
        let transport = McpTransport::Http {
            url: "https://example.invalid/mcp".parse().expect("url"),
            headers: [(
                String::from("Authorization"),
                EnvValue::Literal(String::from("test")),
            )]
            .into(),
            oauth: None,
        };
        let projected = project_transport(&AgentId::Codex, &transport);
        assert_eq!(projected.unsupported, vec![UnsupportedField::Headers]);
        assert!(require_unsupported_acknowledgement(&projected, false).is_err());
        assert!(require_unsupported_acknowledgement(&projected, true).is_ok());
        assert!(
            matches!(projected.transport, McpTransport::Http { ref headers, .. } if headers.is_empty())
        );
        let (codex_entry, omitted) = crate::codex_mcp::CodexServerEntry::from_transport(&transport);
        assert_eq!(omitted.len(), 1);
        let mut config = crate::codex_mcp::CodexConfigFile::new();
        config
            .set_server("remote", &codex_entry)
            .expect("Codex projection");
        assert!(!config.text().contains("headers"));
    }
}
