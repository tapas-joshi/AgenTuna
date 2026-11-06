//! Shared MCP and secret operations used by non-GUI command transports.

use std::sync::{Arc, Mutex};

use at_db::Database;
use at_mcp::{
    HttpClient, LaunchConsent, PathResolver, StdioClient, StdioLaunch, ensure_crypto_provider,
    test_connection,
};
use at_model::{
    EnvValue, McpServer, McpServerDef, McpServerId, McpTransport, Scope, network::NetworkPolicy,
};
use at_secrets::{
    KeychainId, KeychainStore, Redactor, SecretName, SecretStore, SecretString, delete_with_refs,
};
use at_store::AppPaths;
use serde::Serialize;
use thiserror::Error;

/// Result of a successful, explicit MCP connection test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpTestReport {
    /// Server name reported by the endpoint, or the configured fallback.
    pub server_name: String,
    /// Stable transport label.
    pub transport: String,
    /// Time from connection through tool listing.
    pub latency_ms: u128,
    /// Number of tools returned.
    pub tools_count: usize,
    /// Redacted tool names.
    pub tools: Vec<String>,
}

/// Failure while operating on a Keychain secret or testing an MCP server.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpCliError {
    /// The requested secret operation failed.
    #[error("secret operation failed: {message}")]
    Secret {
        /// Safe error message; never contains the secret value.
        message: String,
    },
    /// The secret reference index could not be opened or updated.
    #[error("secret reference index failed: {message}")]
    Database {
        /// Safe database error message.
        message: String,
    },
    /// The MCP runtime could not be created.
    #[error("could not start the MCP test runtime")]
    Runtime,
    /// The connection test did not succeed.
    #[error("{message}")]
    ConnectionFailed {
        /// Redacted MCP error message.
        message: String,
    },
    /// The selected server definition is invalid for a process launch.
    #[error("invalid MCP launch: {message}")]
    InvalidLaunch {
        /// Redacted launch error message.
        message: String,
    },
}

/// Creates or rotates a Keychain secret and records only its reference metadata.
///
/// # Errors
/// Returns a typed failure for invalid names, Keychain errors or database errors.
pub fn set_secret(paths: &AppPaths, name: &str, value: String) -> Result<(), McpCliError> {
    let name = SecretName::new(name.to_owned()).map_err(secret_error)?;
    let id = KeychainId::new(name.as_str()).map_err(secret_error)?;
    let mut database = Database::open_file(paths.db_path()).map_err(database_error)?;
    let store = KeychainStore::new();
    if store.exists(&id).map_err(secret_error)? {
        store
            .rotate(&id, SecretString::new(value))
            .map_err(secret_error)?;
    } else {
        store
            .create(&id, SecretString::new(value))
            .map_err(secret_error)?;
    }
    at_db::secret_refs::record_reference(database.connection_mut(), &name, &id)
        .map_err(database_error)
}

/// Deletes a Keychain secret after checking its registered item references.
///
/// # Errors
/// Returns a typed failure for invalid names, remaining references, Keychain errors or database errors.
pub fn remove_secret(paths: &AppPaths, name: &str) -> Result<(), McpCliError> {
    let name = SecretName::new(name.to_owned()).map_err(secret_error)?;
    let database = Database::open_file(paths.db_path()).map_err(database_error)?;
    let references = at_db::secret_refs::SecretRefStore::new(Arc::new(Mutex::new(database)));
    delete_with_refs(&KeychainStore::new(), &references, &name, true).map_err(secret_error)
}

/// Tests a canonical MCP server definition after the caller explicitly requested it.
///
/// Secret references are resolved only inside the MCP client. Values are registered with the
/// redactor before the connection starts and are never returned in [`McpTestReport`].
///
/// # Errors
/// Returns a redacted launch or connection failure, or a runtime initialization error.
pub fn test_server(server: &McpServer) -> Result<McpTestReport, McpCliError> {
    ensure_crypto_provider();
    let secrets: Arc<dyn SecretStore> = Arc::new(KeychainStore::new());
    let mut redactor = Redactor::new();
    let references = match &server.transport {
        McpTransport::Stdio { env, .. } => env.values(),
        McpTransport::Http { headers, .. } => headers.values(),
    };
    for value in references {
        if let EnvValue::Secret(reference) = value
            && let Ok(keychain_id) = KeychainId::new(reference.keychain_account.clone())
            && let Ok(secret) = secrets.read(&keychain_id)
        {
            redactor.insert(secret);
        }
    }
    let stdio = StdioClient::new(Arc::new(PathResolver::default()), Arc::clone(&secrets));
    let http = HttpClient::new(Arc::new(NetworkPolicy::default()), secrets);
    let consent = match &server.transport {
        McpTransport::Stdio { .. } => {
            let launch =
                StdioLaunch::from_server(server).map_err(|error| McpCliError::InvalidLaunch {
                    message: redactor.redact(&error.to_string()),
                })?;
            Some(LaunchConsent::user_approved(&launch))
        }
        McpTransport::Http { .. } => None,
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| McpCliError::Runtime)?;
    let result = runtime.block_on(test_connection(
        server,
        &stdio,
        &http,
        consent.as_ref(),
        &cancel,
    ));
    if !result.ok {
        let message = result.error.as_ref().map_or_else(
            || "MCP connection test failed".to_owned(),
            |error| redactor.redact(error.message()),
        );
        return Err(McpCliError::ConnectionFailed { message });
    }
    Ok(McpTestReport {
        server_name: redactor.redact(&result.server_name),
        transport: redactor.redact(&result.transport),
        latency_ms: result.latency_ms,
        tools_count: result.tools_count,
        tools: result
            .tools
            .iter()
            .map(|tool| redactor.redact(&tool.name))
            .collect(),
    })
}

/// Builds an MCP server identity from its scope and stored definition.
pub fn make_server(scope: Scope, definition: McpServerDef) -> McpServer {
    McpServer::from_def(McpServerId::new(), scope, definition)
}

/// Loads the indexed MCP rows used by the CLI list command without creating or migrating a DB.
///
/// # Errors
/// Returns a typed database failure when the existing index cannot be read.
pub fn list_rows(
    paths: &AppPaths,
    scope: Scope,
    project_id: Option<&str>,
) -> Result<Vec<at_db::mcp_index::McpListRow>, McpCliError> {
    let database = at_db::ReadOnlyDatabase::open(paths.db_path()).map_err(database_error)?;
    at_db::mcp_index::mcp_list_rows(database.connection(), Some(scope), project_id)
        .map_err(database_error)
}

fn secret_error(error: impl std::fmt::Display) -> McpCliError {
    McpCliError::Secret {
        message: error.to_string(),
    }
}

fn database_error(error: impl std::fmt::Display) -> McpCliError {
    McpCliError::Database {
        message: error.to_string(),
    }
}
