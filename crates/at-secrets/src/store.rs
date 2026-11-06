//! The [`SecretStore`] trait shared by the Keychain and in-memory implementations.

use crate::error::SecretsError;
use crate::name::KeychainId;
use crate::secret::SecretString;

/// Credential-store operations used by the rest of AgenTuna.
///
/// Implementations are `Send + Sync`: the scheduler and UI threads read concurrently.
/// Values are passed into the store by value and returned in [`SecretString`], which
/// zeroes them on drop; they are never accepted or returned as plain `String`.
pub trait SecretStore: Send + Sync {
    /// Stores `value`, failing with [`SecretsError::AlreadyExists`] when the account is taken.
    ///
    /// An existing value is never overwritten; callers that mean to replace it must call
    /// [`SecretStore::rotate`].
    ///
    /// # Errors
    /// [`SecretsError::AlreadyExists`], an invalid identity, or a credential-store failure.
    fn create(&self, id: &KeychainId, value: SecretString) -> Result<(), SecretsError>;

    /// Reads the secret for `id`.
    ///
    /// # Errors
    /// [`SecretsError::NotFound`] when no credential exists, or a credential-store failure.
    fn read(&self, id: &KeychainId) -> Result<SecretString, SecretsError>;

    /// Replaces the value for an existing account.
    ///
    /// # Errors
    /// [`SecretsError::NotFound`] when no credential exists, or a credential-store failure.
    fn rotate(&self, id: &KeychainId, value: SecretString) -> Result<(), SecretsError>;

    /// Removes the credential.
    ///
    /// # Errors
    /// [`SecretsError::NotFound`] when no credential exists. Deleting a missing secret
    /// never prompts and never panics.
    fn delete(&self, id: &KeychainId) -> Result<(), SecretsError>;

    /// Whether a credential exists for `id`, without exposing its value.
    ///
    /// # Errors
    /// A credential-store failure. A missing credential is `Ok(false)`.
    fn exists(&self, id: &KeychainId) -> Result<bool, SecretsError>;
}
