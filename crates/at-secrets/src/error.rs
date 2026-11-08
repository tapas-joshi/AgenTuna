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

    /// The platform credential store is locked or denied access to the item.
    ///
    /// On Linux this typically means that the Secret Service provider is locked or denied
    /// access; on macOS it includes a locked login keychain or suppressed Keychain prompt.
    #[cfg_attr(
        target_os = "macos",
        error(
            "Keychain access for service `{service}` was denied or the keychain is locked; open Keychain Access, unlock the login keychain and allow AgenTuna to access the item, then retry"
        )
    )]
    #[cfg_attr(
        target_os = "linux",
        error(
            "access to credential service `{service}` was denied or its store is locked; unlock and authorize the Secret Service provider, then retry"
        )
    )]
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux")),
        error("access to credential service `{service}` was denied or its store is locked")
    )]
    KeychainDenied {
        /// Keychain service name.
        service: String,
    },

    /// No platform credential store is available in this process. Linux users need
    /// a running Secret Service provider; no secret is written outside the OS store.
    #[cfg_attr(
        target_os = "linux",
        error(
            "the credential store for Keychain service `{service}` is unavailable; install and start a Secret Service provider such as gnome-keyring or KWallet, then retry. AgenTuna never writes secrets to disk"
        )
    )]
    #[cfg_attr(
        not(target_os = "linux"),
        error(
            "the credential store for Keychain service `{service}` is unavailable; configure the platform credential store and retry. AgenTuna never writes secrets to disk"
        )
    )]
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
