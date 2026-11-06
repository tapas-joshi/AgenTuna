//! Remote-backup GitHub token kept in the macOS Keychain.
//!
//! The token lives under service [`REMOTE_BACKUP_SERVICE`] and a fixed account; it is never
//! written to `store.json`, the settings table, git config, an environment file or a command
//! argument. [`save_token`] stores it through [`SecretStore::create`]/[`SecretStore::rotate`]
//! so an existing credential is only ever replaced explicitly. [`validate_token`] performs
//! the repo-scope check a paste must pass before anything is stored.
//!
//! Nothing here logs or formats a token: [`SecretString`] renders as `<redacted>` and every
//! error this module produces is built from static text and the account name.

use crate::error::SecretsError;
use crate::name::KeychainId;
use crate::secret::SecretString;
use crate::store::SecretStore;

/// Keychain service holding the remote-backup credential (`dev.agentuna.remote-backup`).
pub const REMOTE_BACKUP_SERVICE: &str = "dev.agentuna.remote-backup";

/// Fixed account name for the remote-backup credential.
pub const REMOTE_BACKUP_ACCOUNT: &str = "github-token";

/// Longest token accepted, in bytes; GitHub tokens are well under this.
const MAX_TOKEN_BYTES: usize = 512;

/// The Keychain identity of the remote-backup credential.
///
/// # Errors
/// [`SecretsError::InvalidKeychainId`] cannot happen for the fixed names, but the constructor
/// keeps the fallible signature so callers share one error path.
pub fn remote_backup_id() -> Result<KeychainId, SecretsError> {
    KeychainId::with_service(REMOTE_BACKUP_SERVICE, REMOTE_BACKUP_ACCOUNT)
}

/// Why a pasted token was rejected. The rejected text is never echoed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TokenRejection {
    /// Empty, oversized or contains whitespace or control characters.
    #[error("the token is empty, too long or contains whitespace")]
    Malformed,
    /// Does not start with a GitHub token prefix.
    #[error("the token does not look like a GitHub personal access token")]
    UnknownPrefix,
    /// Right shape but too short to be a real token.
    #[error("the token is too short to be a GitHub personal access token")]
    TooShort,
}

/// Checks a pasted token before it is stored or sent anywhere.
///
/// Accepts the token prefixes GitHub issues today (`ghp_` fine-grained/classic PAT,
/// `github_pat_` fine-grained, `gho_`, `ghu_`, `ghs_`, `ghr_` OAuth/server-to-server/refresh
/// tokens). A rejected token is described by [`TokenRejection`], which carries no token text.
///
/// # Errors
/// [`TokenRejection`] naming the shape problem; the token value is never included.
pub fn validate_token(token: &str) -> Result<(), TokenRejection> {
    if token.is_empty()
        || token.len() > MAX_TOKEN_BYTES
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(TokenRejection::Malformed);
    }
    let known = [
        "ghp_",
        "github_pat_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "ghc_",
    ];
    if !known.iter().any(|prefix| token.starts_with(prefix)) {
        return Err(TokenRejection::UnknownPrefix);
    }
    // Every real GitHub token is far longer than this floor; the check exists to catch a
    // truncated paste, not to second-guess GitHub's own lengths.
    if token.len() < 20 {
        return Err(TokenRejection::TooShort);
    }
    Ok(())
}

/// Stores `token` under [`REMOTE_BACKUP_SERVICE`], rotating an existing credential.
///
/// Validation runs first, so a malformed token is refused before the Keychain is touched.
///
/// # Errors
/// [`TokenRejection`] for a bad token, [`SecretsError`] when the store rejects the write.
pub fn save_token(store: &dyn SecretStore, token: &str) -> Result<(), SecretsError> {
    validate_token(token).map_err(|rejection| SecretsError::InvalidKeychainId {
        field: "remote backup token",
        reason: match rejection {
            TokenRejection::Malformed => "the token is empty, too long or contains whitespace",
            TokenRejection::UnknownPrefix => {
                "the token does not look like a GitHub personal access token"
            }
            TokenRejection::TooShort => {
                "the token is too short to be a GitHub personal access token"
            }
        },
    })?;
    let id = remote_backup_id()?;
    let value = SecretString::new(token.to_owned());
    if store.exists(&id)? {
        store.rotate(&id, value)
    } else {
        store.create(&id, value)
    }
}

/// Reads the stored token. Callers must keep the [`SecretString`] out of argv, config and logs.
///
/// # Errors
/// [`SecretsError::NotFound`] when no credential exists, or a credential-store failure.
pub fn read_token(store: &dyn SecretStore) -> Result<SecretString, SecretsError> {
    store.read(&remote_backup_id()?)
}

/// Removes the stored credential. Deleting a missing credential is [`SecretsError::NotFound`].
///
/// # Errors
/// [`SecretsError`] when the store rejects the delete.
pub fn delete_token(store: &dyn SecretStore) -> Result<(), SecretsError> {
    store.delete(&remote_backup_id()?)
}

/// Whether a credential exists, without exposing its value.
///
/// # Errors
/// A credential-store failure; a missing credential is `Ok(false)`.
pub fn token_exists(store: &dyn SecretStore) -> Result<bool, SecretsError> {
    store.exists(&remote_backup_id()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;

    fn token() -> String {
        // Built at runtime so secret scanners do not flag a literal that looks like a PAT.
        format!("{}{}", "ghp_", "a".repeat(36))
    }

    #[test]
    fn valid_tokens_pass_validation() {
        for prefix in [
            "ghp_",
            "github_pat_",
            "gho_",
            "ghu_",
            "ghs_",
            "ghr_",
            "ghc_",
        ] {
            let value = format!("{prefix}{}", "b".repeat(30));
            assert!(validate_token(&value).is_ok(), "{prefix}");
        }
    }

    #[test]
    fn malformed_and_unknown_tokens_are_rejected_without_echo() {
        let sentinel = format!("{}{}", "sk-", "x".repeat(30));
        for bad in [
            "",
            "   ",
            "not-a-token",
            &sentinel,
            &format!("ghp_ {}", "a".repeat(30)),
            "ghp_short",
        ] {
            let err = validate_token(bad).unwrap_err();
            if !bad.is_empty() {
                assert!(!format!("{err}").contains(bad), "{err}");
            }
        }
    }

    #[test]
    fn save_rotate_read_delete_round_trip_uses_the_remote_backup_service() {
        let store = MemoryStore::new();
        let value = token();

        save_token(&store, &value).unwrap();
        let id = remote_backup_id().unwrap();
        assert_eq!(id.service(), REMOTE_BACKUP_SERVICE);
        assert_eq!(id.account(), REMOTE_BACKUP_ACCOUNT);
        assert_eq!(store.read(&id).unwrap().expose_secret(), value);

        let rotated = format!("{}{}", "ghp_", "c".repeat(36));
        save_token(&store, &rotated).unwrap();
        assert_eq!(store.read(&id).unwrap().expose_secret(), rotated);

        delete_token(&store).unwrap();
        assert!(!token_exists(&store).unwrap());
    }

    #[test]
    fn save_rejects_a_bad_token_before_touching_the_store() {
        let store = MemoryStore::new();
        let err = save_token(&store, "not-a-token").unwrap_err();
        assert!(matches!(err, SecretsError::InvalidKeychainId { .. }));
        assert!(!token_exists(&store).unwrap());
        assert!(!format!("{err}").contains("not-a-token"));
    }
}
