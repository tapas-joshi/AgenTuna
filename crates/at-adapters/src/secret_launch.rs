//! Safe launch-time handling for MCP secret references.
//!
//! Capability status as of 2026-11-06 (probe/documentation evidence recorded in the technical
//! story; this module does not claim to have run a new live-agent probe):
//!
//! | Agent | Verified launch behavior | Date |
//! |---|---|---|
//! | `claude_code` | `${VAR}` expansion is documented for project `.mcp.json`; other surfaces remain `UNVERIFIED`. | 2026-11-06 |
//! | `codex` | `env_vars = ["NAME"]` forwards an inherited environment variable; `${VAR}` interpolation remains `UNVERIFIED`. | 2026-11-06 |
//! | `omp` | `${VAR}` expansion is `UNVERIFIED`; never emit the reference. | 2026-11-06 |
//! | `cursor` | Expansion is `UNVERIFIED`; never emit the reference. | 2026-11-06 |
//! | `custom:*` | Expansion is `UNVERIFIED`; never emit the reference. | 2026-11-06 |
//!
//! The table is also available as [`SECRET_STRATEGY_TABLE`] for UI and planner use.

use std::path::Path;

use crate::mcp_capabilities::{AgentMcpCapabilities, CapabilityVerification};
use at_model::AgentId;

/// One dated row in the per-agent launch-time secret capability table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretStrategyStatus {
    /// Stable agent key; `custom:*` covers dynamically registered custom agents.
    pub agent: &'static str,
    /// Verified behavior or explicit `UNVERIFIED` status.
    pub verified_expansion: &'static str,
    /// Date the status was recorded.
    pub date: &'static str,
}

/// Dated launch-time secret behavior for every built-in and custom MCP agent family.
pub const SECRET_STRATEGY_TABLE: &[SecretStrategyStatus] = &[
    SecretStrategyStatus {
        agent: "claude_code",
        verified_expansion: "Documented ${VAR} expansion in project .mcp.json; other surfaces UNVERIFIED.",
        date: "2026-11-06",
    },
    SecretStrategyStatus {
        agent: "codex",
        verified_expansion: "Verified env_vars pass-through from inherited environment; ${VAR} expansion UNVERIFIED.",
        date: "2026-11-06",
    },
    SecretStrategyStatus {
        agent: "omp",
        verified_expansion: "UNVERIFIED; do not emit ${VAR}.",
        date: "2026-11-06",
    },
    SecretStrategyStatus {
        agent: "cursor",
        verified_expansion: "UNVERIFIED; do not emit ${VAR}.",
        date: "2026-11-06",
    },
    SecretStrategyStatus {
        agent: "custom:*",
        verified_expansion: "UNVERIFIED; do not emit ${VAR}.",
        date: "2026-11-06",
    },
];

/// Returns the dated secret capability row for a built-in or custom agent.
#[must_use]
pub fn strategy_status(agent: &AgentId) -> Option<&'static SecretStrategyStatus> {
    let key = match agent {
        AgentId::ClaudeCode => "claude_code",
        AgentId::Codex => "codex",
        AgentId::Omp => "omp",
        AgentId::Cursor => "cursor",
        AgentId::Custom(_) => "custom:*",
    };
    SECRET_STRATEGY_TABLE.iter().find(|row| row.agent == key)
}

/// Agent-specific safe action for a secret environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretInjection {
    /// The selected native config path has verified `${NAME}` expansion.
    ConfigReference {
        /// Variable name referenced by the agent config.
        variable: String,
    },
    /// The agent has verified pass-through support and should receive the variable name only.
    EnvironmentPassThrough {
        /// Variable names to place in Codex `env_vars`.
        variable: String,
    },
    /// No verified config expansion is available; refuse to write the reference.
    Refused {
        /// Variable name that needs to be supplied at launch.
        variable: String,
        /// Safe shell snippet, containing no secret value.
        shell_snippet: String,
    },
}

/// Selects a secret deployment strategy only from verified native capabilities.
#[must_use]
pub fn plan_secret_injection(
    capabilities: &AgentMcpCapabilities,
    path: &Path,
    variable: &str,
    keychain_account: &str,
) -> SecretInjection {
    if !valid_environment_name(variable) {
        return SecretInjection::Refused {
            variable: variable.to_owned(),
            shell_snippet: String::new(),
        };
    }
    if capabilities.agent == AgentId::Codex
        && capabilities.supports_env_vars == CapabilityVerification::Verified
    {
        return SecretInjection::EnvironmentPassThrough {
            variable: variable.to_owned(),
        };
    }
    let path_support = capabilities
        .interpolation_by_path
        .iter()
        .find(|(candidate, _)| candidate == path)
        .map(|(_, level)| *level)
        .unwrap_or(CapabilityVerification::Unverified);
    if path_support == CapabilityVerification::Verified {
        return SecretInjection::ConfigReference {
            variable: variable.to_owned(),
        };
    }
    SecretInjection::Refused {
        variable: variable.to_owned(),
        shell_snippet: shell_export_snippet(variable, keychain_account),
    }
}

fn shell_export_snippet(variable: &str, account: &str) -> String {
    format!(
        "export {variable}=$(security find-generic-password -s {} -a {} -w)",
        shell_quote(at_secrets::KEYCHAIN_SERVICE),
        shell_quote(account),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn valid_environment_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use at_model::AgentId;

    use super::{SECRET_STRATEGY_TABLE, SecretInjection, plan_secret_injection, strategy_status};
    use crate::adapter::{AgentAdapterRegistry, AgentEnvironment};
    use crate::mcp_capabilities::{CapabilityVerification, capabilities};

    #[test]
    fn secret_strategy_uses_only_verified_path_capabilities() {
        let home = Path::new("/tmp/agentuna-secret-test");
        let claude = capabilities(&AgentId::ClaudeCode, home).expect("Claude capabilities");
        assert_eq!(
            plan_secret_injection(&claude, &home.join(".mcp.json"), "TOKEN", "account",),
            SecretInjection::ConfigReference {
                variable: "TOKEN".to_owned()
            }
        );
        let codex = capabilities(&AgentId::Codex, home).expect("Codex capabilities");
        assert_eq!(
            plan_secret_injection(&codex, Path::new("/tmp/config.toml"), "TOKEN", "account"),
            SecretInjection::EnvironmentPassThrough {
                variable: "TOKEN".to_owned()
            }
        );
        let omp = capabilities(&AgentId::Omp, home).expect("omp capabilities");
        let refused = plan_secret_injection(&omp, Path::new("/tmp/mcp.json"), "TOKEN", "account");
        let SecretInjection::Refused {
            shell_snippet,
            variable,
        } = refused
        else {
            panic!("unverified omp expansion must be refused")
        };
        assert_eq!(variable, "TOKEN");
        assert!(shell_snippet.contains("dev.agentuna.mcp"));
        assert!(shell_snippet.contains("account"));
        assert!(!shell_snippet.contains("SENTINEL_SECRET_VALUE"));
        assert_eq!(CapabilityVerification::Unverified, omp.env_interpolation);
        let invalid =
            plan_secret_injection(&omp, Path::new("/tmp/mcp.json"), "TOKEN;false", "acct");
        assert_eq!(
            invalid,
            SecretInjection::Refused {
                variable: "TOKEN;false".to_owned(),
                shell_snippet: String::new()
            }
        );
    }

    #[test]
    fn every_registered_builtin_and_custom_agent_has_a_dated_strategy_row() {
        let root = tempfile::tempdir().expect("temp");
        let env = AgentEnvironment {
            home: root.path().join("home"),
            project: None,
            variables: Default::default(),
        };
        let registry = AgentAdapterRegistry::register();
        for id in [
            AgentId::ClaudeCode,
            AgentId::Codex,
            AgentId::Omp,
            AgentId::Cursor,
            AgentId::Custom("generic".to_owned()),
        ] {
            let adapter = registry.get(&id).expect("registry").expect("adapter");
            assert!(
                adapter.capabilities(&env).mcp.is_some(),
                "{id} should expose MCP capabilities"
            );
            let row = strategy_status(&id).expect("strategy row");
            assert!(!row.date.is_empty());
            assert!(SECRET_STRATEGY_TABLE.contains(row));
        }
        assert!(
            SECRET_STRATEGY_TABLE
                .iter()
                .any(|row| row.agent == "custom:*")
        );
    }
}
