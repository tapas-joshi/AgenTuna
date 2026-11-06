//! Validated Keychain identity and logical secret names.

use core::fmt;
use core::str::FromStr;

use crate::error::SecretsError;

/// Keychain service holding MCP server secrets (`dev.agentuna.mcp`).
pub const KEYCHAIN_SERVICE: &str = "dev.agentuna.mcp";

const MAX_SERVICE_BYTES: usize = 255;
const MAX_ACCOUNT_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 255;

fn validate(field: &'static str, value: &str, max: usize) -> Result<(), SecretsError> {
    if value.is_empty() {
        return Err(SecretsError::InvalidKeychainId {
            field,
            reason: "must not be empty",
        });
    }
    if value.len() > max {
        return Err(SecretsError::InvalidKeychainId {
            field,
            reason: "is too long",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(SecretsError::InvalidKeychainId {
            field,
            reason: "must not contain control characters",
        });
    }
    Ok(())
}

/// Logical name of a secret, matching `secret_refs.name`.
///
/// The name is a label, not a secret value: it can appear in errors, logs and the UI.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretName(String);

impl SecretName {
    /// Validates `name`.
    ///
    /// # Errors
    /// [`SecretsError::InvalidKeychainId`] when the name is empty, oversized or
    /// contains control characters.
    pub fn new(name: impl Into<String>) -> Result<Self, SecretsError> {
        let name = name.into();
        validate("secret name", &name, MAX_NAME_BYTES)?;
        Ok(Self(name))
    }

    /// The name as stored in `secret_refs.name`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SecretName {
    type Err = SecretsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Which Keychain entry holds a secret: a service plus an account.
///
/// [`KeychainId::new`] uses [`KEYCHAIN_SERVICE`]; [`KeychainId::with_service`] is for
/// the other services AgenTuna owns (for example `dev.agentuna.remote-backup`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeychainId {
    service: String,
    account: String,
}

impl KeychainId {
    /// A credential in [`KEYCHAIN_SERVICE`] with the given account.
    ///
    /// # Errors
    /// [`SecretsError::InvalidKeychainId`] when the account is invalid.
    pub fn new(account: impl Into<String>) -> Result<Self, SecretsError> {
        Self::with_service(KEYCHAIN_SERVICE, account)
    }

    /// A credential in a custom service.
    ///
    /// # Errors
    /// [`SecretsError::InvalidKeychainId`] when the service or account is invalid.
    pub fn with_service(
        service: impl Into<String>,
        account: impl Into<String>,
    ) -> Result<Self, SecretsError> {
        let service = service.into();
        let account = account.into();
        validate("Keychain service", &service, MAX_SERVICE_BYTES)?;
        validate("Keychain account", &account, MAX_ACCOUNT_BYTES)?;
        Ok(Self { service, account })
    }

    /// Keychain service name.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Keychain account name.
    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    pub(crate) fn already_exists(&self) -> SecretsError {
        SecretsError::AlreadyExists {
            service: self.service.clone(),
            account: self.account.clone(),
        }
    }

    pub(crate) fn not_found(&self) -> SecretsError {
        SecretsError::NotFound {
            service: self.service.clone(),
            account: self.account.clone(),
        }
    }
}

impl From<&SecretName> for KeychainId {
    /// The default identity: [`KEYCHAIN_SERVICE`] with the name as the account.
    fn from(name: &SecretName) -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_owned(),
            account: name.as_str().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_oversized_and_control_names() {
        assert!(SecretName::new("").is_err());
        assert!(SecretName::new("a".repeat(MAX_NAME_BYTES + 1)).is_err());
        assert!(SecretName::new("bad\nname").is_err());
        assert!(SecretName::new("OPENAI_API_KEY").is_ok());
    }

    #[test]
    fn default_id_is_service_and_account() {
        let name = SecretName::new("OPENAI_API_KEY").unwrap();
        let id = KeychainId::from(&name);
        assert_eq!(id.service(), KEYCHAIN_SERVICE);
        assert_eq!(id.account(), "OPENAI_API_KEY");
    }

    #[test]
    fn custom_service_is_supported() {
        let id = KeychainId::with_service("dev.agentuna.remote-backup", "github-token").unwrap();
        assert_eq!(id.service(), "dev.agentuna.remote-backup");
        assert_eq!(id.account(), "github-token");
        assert!(KeychainId::with_service("", "x").is_err());
    }
}
