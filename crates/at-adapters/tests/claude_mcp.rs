//! Regression tests for the Claude Code MCP JSONC editor.

use at_adapters::claude_mcp::{ClaudeConfigScope, ClaudeMcpConfig, ClaudeMcpError};

#[test]
fn duplicate_server_keys_are_refused_instead_of_collapsed() {
    // Minimised case from the generative property test: the parsed map kept the last entry
    // while `upsert` patched the first one, so the model and the file disagreed.
    let source = "{\n  \"mcpServers\": {\n    \"a\": {\"type\": \"stdio\", \"command\": \"first\"},\n    \"a\": {\"type\": \"stdio\", \"command\": \"second\"}\n  }\n}\n";
    let error = ClaudeMcpConfig::parse(source, ClaudeConfigScope::User).expect_err("duplicate");
    assert!(matches!(error, ClaudeMcpError::DuplicateServer { name } if name == "a"));
}

#[test]
fn a_duplicate_mcp_servers_key_is_refused() {
    let source = "{\n  \"mcpServers\": {},\n  \"mcpServers\": {}\n}\n";
    let error = ClaudeMcpConfig::parse(source, ClaudeConfigScope::User).expect_err("duplicate key");
    assert!(matches!(error, ClaudeMcpError::DuplicateKey { key } if key == "mcpServers"));
}
