//! OS credential store (Keychain) operations for AgenTuna.
//!
//! [`SecretStore`] is the seam. [`KeychainStore`] wraps `keyring` 4.x and the macOS
//! Keychain under service [`KEYCHAIN_SERVICE`]; [`MemoryStore`] is an in-process
//! implementation for tests and dry runs. Values travel as [`SecretString`], which
//! zeroes on drop and renders as `<redacted>`; errors carry names and services, never
//! values.
//!
//! This crate never passes a secret as a process argument. Callers that spawn a server
//! put the resolved value in the child environment or an HTTP header.

pub mod error;
pub mod keychain;
pub mod memory;
pub mod name;
pub mod redact;
pub mod refs;
pub mod remote_backup;
pub mod secret;
pub mod store;
pub mod var_ref;

pub use error::SecretsError;
pub use keychain::KeychainStore;
pub use memory::MemoryStore;
pub use name::{KEYCHAIN_SERVICE, KeychainId, SecretName};
pub use redact::Redactor;
pub use refs::{SecretReference, SecretRefs, delete_with_refs};
pub use remote_backup::{
    REMOTE_BACKUP_ACCOUNT, REMOTE_BACKUP_SERVICE, TokenRejection, delete_token, read_token,
    remote_backup_id, save_token, token_exists, validate_token,
};
pub use secret::SecretString;
pub use store::SecretStore;
pub use var_ref::{ImportOutcome, Imported, RefError, RefWarning, VarTemplate, import_value};
