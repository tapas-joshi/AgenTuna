//! Pure MCP entry projection through the format-preserving native adapters.

use at_model::{AgentId, McpTransport, Scope};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

/// One native MCP projection operation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum McpProjectionOperation {
    /// Insert or update one managed entry.
    Upsert {
        /// Native server key.
        name: String,
        /// Canonical transport after capability projection.
        transport: Box<McpTransport>,
        /// Whether the agent should start the server when it supports a native flag.
        enabled: bool,
        /// Unknown native entry fields to preserve or add.
        extra: BTreeMap<String, Value>,
        /// Verified environment-variable pass-through names (Codex only).
        env_vars: Vec<String>,
    },
    /// Remove one entry from the native configuration.
    Remove {
        /// Native server key.
        name: String,
    },
}

/// Input to a pure native-format adapter projection.
#[derive(Debug, Clone, PartialEq)]
pub struct McpProjectionRequest {
    /// Original native document bytes as UTF-8 text, or empty for a missing document.
    pub source: String,
    /// Native configuration path (used for scope routing and the returned document).
    pub path: PathBuf,
    /// Scope selected by the engine.
    pub scope: Scope,
    /// Project path, required for Claude project maps.
    pub project: Option<PathBuf>,
    /// Entry-level edit.
    pub operation: McpProjectionOperation,
}

/// Format-specific projection failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpProjectionError {
    /// The chosen agent has no registered native MCP format.
    #[error("MCP projection is unsupported for agent {0}")]
    Unsupported(AgentId),
    /// A native format parser or renderer refused the operation.
    #[error("MCP projection failed for {agent}: {message}")]
    Native {
        /// Agent adapter that failed.
        agent: AgentId,
        /// Safe parser/validation error text.
        message: String,
    },
}

/// Projects one MCP operation while retaining the native document's unrelated formatting.
pub fn project(
    agent: &AgentId,
    request: &McpProjectionRequest,
) -> Result<Vec<u8>, McpProjectionError> {
    let rendered = match agent {
        AgentId::ClaudeCode => project_claude(request).map_err(|e| native_error(agent, e))?,
        AgentId::Codex => project_codex(request).map_err(|e| native_error(agent, e))?,
        AgentId::Omp => project_omp(request).map_err(|e| native_error(agent, e))?,
        other => return Err(McpProjectionError::Unsupported(other.clone())),
    };
    Ok(rendered.into_bytes())
}

fn project_claude(request: &McpProjectionRequest) -> Result<String, String> {
    use crate::claude_mcp::{ClaudeConfigScope, ClaudeMcpConfig, ClaudeServer};
    let scope = if request.scope == Scope::User {
        ClaudeConfigScope::User
    } else if request
        .path
        .file_name()
        .is_some_and(|name| name == ".mcp.json")
    {
        ClaudeConfigScope::ProjectFile
    } else {
        let project_path = request
            .project
            .as_deref()
            .ok_or_else(|| "project path required".to_owned())?;
        ClaudeConfigScope::Project {
            project_path: project_path.to_string_lossy().into_owned(),
        }
    };
    let config = ClaudeMcpConfig::parse(&request.source, scope).map_err(|e| e.to_string())?;
    match &request.operation {
        McpProjectionOperation::Upsert {
            name,
            transport,
            extra,
            enabled,
            ..
        } => {
            if !enabled {
                return Err(
                    "Claude Code has no per-entry enabled flag; remove the entry instead"
                        .to_owned(),
                );
            }
            let server = ClaudeServer {
                name: name.clone(),
                transport: (**transport).clone(),
                extra: extra.clone(),
            };
            config.upsert(&server).map_err(|e| e.to_string())
        }
        McpProjectionOperation::Remove { name } => config.remove(name).map_err(|e| e.to_string()),
    }
}

fn project_codex(request: &McpProjectionRequest) -> Result<String, String> {
    use crate::codex_mcp::{CodexConfigFile, CodexServerEntry};
    let mut config = CodexConfigFile::parse(&request.source).map_err(|e| e.to_string())?;
    match &request.operation {
        McpProjectionOperation::Upsert {
            name,
            transport,
            enabled,
            env_vars,
            ..
        } => {
            let (mut entry, _unsupported) = CodexServerEntry::from_transport(transport);
            entry.enabled = Some(*enabled);
            if !env_vars.is_empty() {
                entry.env_vars = Some(env_vars.clone());
            }
            config.set_server(name, &entry).map_err(|e| e.to_string())?;
            Ok(config.text())
        }
        McpProjectionOperation::Remove { name } => {
            config.remove_server(name).map_err(|e| e.to_string())?;
            Ok(config.text())
        }
    }
}

fn project_omp(request: &McpProjectionRequest) -> Result<String, String> {
    use crate::omp_mcp::{OmpMcpConfig, OmpServer};
    let config = OmpMcpConfig::parse(&request.source).map_err(|e| e.to_string())?;
    match &request.operation {
        McpProjectionOperation::Upsert {
            name,
            transport,
            extra,
            enabled,
            ..
        } => {
            let mut server = OmpServer::new(name, (**transport).clone(), *enabled);
            server.extra = extra.clone();
            config.upsert_server(&server).map_err(|e| e.to_string())
        }
        McpProjectionOperation::Remove { name } => {
            config.remove_server(name).map_err(|e| e.to_string())
        }
    }
}

fn native_error(agent: &AgentId, message: String) -> McpProjectionError {
    McpProjectionError::Native {
        agent: agent.clone(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_model::AgentId;

    fn request(source: &str, operation: McpProjectionOperation) -> McpProjectionRequest {
        McpProjectionRequest {
            source: source.to_owned(),
            path: PathBuf::from("/fixture/native-mcp"),
            scope: Scope::User,
            project: None,
            operation,
        }
    }

    fn upsert(enabled: bool) -> McpProjectionOperation {
        McpProjectionOperation::Upsert {
            name: "fixture".to_owned(),
            transport: Box::new(McpTransport::Stdio {
                command: "fixture-server".to_owned(),
                args: vec!["--safe".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
            }),
            enabled,
            env_vars: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn claude_projection_upserts_and_removes_one_jsonc_entry() {
        let result = project(
            &AgentId::ClaudeCode,
            &request("{\"mcpServers\":{}}", upsert(true)),
        )
        .expect("project");
        let text = String::from_utf8(result).expect("UTF-8");
        assert!(text.contains("\"fixture\""));
        assert!(text.contains("fixture-server"));
        let result = project(
            &AgentId::ClaudeCode,
            &request(
                &text,
                McpProjectionOperation::Remove {
                    name: "fixture".to_owned(),
                },
            ),
        )
        .expect("remove");
        assert!(
            !String::from_utf8(result)
                .expect("UTF-8")
                .contains("fixture-server")
        );
    }

    #[test]
    fn codex_and_omp_projection_honor_native_enabled_flags() {
        let codex = project(&AgentId::Codex, &request("", upsert(false))).expect("Codex");
        let codex = String::from_utf8(codex).expect("UTF-8");
        assert!(codex.contains("enabled = false"));
        let omp = project(&AgentId::Omp, &request("{}", upsert(false))).expect("omp");
        let omp = String::from_utf8(omp).expect("UTF-8");
        assert!(omp.contains("\"enabled\": false"));
    }
}
