//! Typed errors for credential-store operations.
//!
//! No error variant, `Display` text or `Debug` output ever contains a secret value.
//! Names, services and static reason codes are safe to show and log.

use crate::refs::SecretReference;

/// Why a credential-store operation did not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretsError {
    /// The account already holds a secret. `create` never overwrites; use `rotate`.
    #[error(
        "a secret already exists for account `{account}` in Keychain service `{service}`; rotate it instead"
    )]
    AlreadyExists {
        /// Keychain service name.
        service: String,
        /// Keychain account name.
        account: String,
    },

    /// No credential exists for this account.
    #[error("no secret exists for account `{account}` in Keychain service `{service}`")]
    NotFound {
        /// Keychain service name.
        service: String,
        /// Keychain account name.
        account: String,
    },

    /// macOS locked the keychain or the user denied AgenTuna access to the item.
    ///
    /// `errSecInteractionNotAllowed` (a background process with the GUI prompt
    /// suppressed) also lands here: the remedy is the same.
    #[error(
        "Keychain access for service `{service}` was denied or the keychain is locked; open Keychain Access, unlock the login keychain and allow AgenTuna to access the item, then retry"
    )]
    KeychainDenied {
        /// Keychain service name.
        service: String,
    },

    /// No platform credential store is available in this process.
    #[error("the credential store for Keychain service `{service}` is unavailable")]
    KeychainUnavailable {
        /// Keychain service name.
        service: String,
    },

    /// The stored blob is not valid UTF-8, so it cannot be returned as a string.
    #[error(
        "the stored secret for account `{account}` in Keychain service `{service}` is not valid UTF-8; delete and recreate it"
    )]
    CorruptSecret {
        /// Keychain service name.
        service: String,
        /// Keychain account name.
        account: String,
    },

    /// A service or account name was empty, oversized or contained control characters.
    #[error("{field} is not a valid Keychain name: {reason}")]
    InvalidKeychainId {
        /// Which input was rejected.
        field: &'static str,
        /// Why it was rejected; a static description that never echoes the value.
        reason: &'static str,
    },

    /// The credential store rejected the operation for a reason other than access denial.
    #[error("Keychain operation failed for service `{service}` ({kind})")]
    Keychain {
        /// Keychain service name.
        service: String,
        /// Static category of the failure.
        kind: &'static str,
    },

    /// Deleting without confirmation while items still reference the secret.
    #[error(
        "secret `{name}` is referenced by {count} item(s); remove those references or pass confirm"
    )]
    ReferencedSecret {
        /// Logical secret name.
        name: String,
        /// How many items reference it.
        count: usize,
        /// The referencing items, for the UI to list.
        references: Vec<SecretReference>,
    },

    /// The Keychain entry was deleted but the `secret_refs` bookkeeping failed.
    #[error(
        "secret `{name}` was deleted from Keychain but its secret_refs row could not be removed (rollback restored={restored})"
    )]
    RefsUpdateFailed {
        /// Logical secret name.
        name: String,
        /// Whether the Keychain entry was restored by rollback.
        restored: bool,
    },

    /// A reference points at a Keychain item that no longer exists.
    ///
    /// Callers must fail the operation instead of substituting an empty value:
    /// a server that receives an empty credential looks configured but cannot
    /// authenticate.
    #[error(
        "secret `{name}` is referenced but no Keychain item exists for it; re-add the value or remove the reference"
    )]
    MissingSecret {
        /// Logical secret name.
        name: String,
    },

    /// The `secret_refs` / `secret_ref_uses` repository failed.
    #[error("secret reference store failed: {reason}")]
    ReferenceStore {
        /// Inventory failure description; never contains a secret value.
        reason: String,
    },
}
