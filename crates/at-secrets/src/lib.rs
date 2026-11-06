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
pub mod refs;
pub mod secret;
pub mod store;

pub use error::SecretsError;
pub use keychain::KeychainStore;
pub use memory::MemoryStore;
pub use name::{KEYCHAIN_SERVICE, KeychainId, SecretName};
pub use refs::{SecretReference, SecretRefs, delete_with_refs};
pub use secret::SecretString;
pub use store::SecretStore;
