//! macOS Keychain implementation over `keyring` 4.x.
//!
//! The production backend calls `keyring::Entry` for the configured service and
//! account. Operations are serialized inside one [`KeychainStore`] so the scheduler
//! and UI threads never race on the same credential. Tests replace the backend with
//! the `keyring-core` mock store; no test opens the real Keychain.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::error::SecretsError;
use crate::name::KeychainId;
use crate::secret::SecretString;
use crate::store::SecretStore;

/// Low-level operations on one credential entry.
trait KeychainBackend: Send + Sync {
    fn get(&self, id: &KeychainId) -> keyring::Result<Option<String>>;
    fn set(&self, id: &KeychainId, value: &str) -> keyring::Result<()>;
    fn delete(&self, id: &KeychainId) -> keyring::Result<()>;
}

/// The real platform credential store (macOS Keychain, or the platform equivalent).
#[derive(Debug)]
struct SystemKeychain;

impl KeychainBackend for SystemKeychain {
    fn get(&self, id: &KeychainId) -> keyring::Result<Option<String>> {
        let entry = keyring::Entry::new(id.service(), id.account())?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn set(&self, id: &KeychainId, value: &str) -> keyring::Result<()> {
        keyring::Entry::new(id.service(), id.account())?.set_password(value)
    }

    fn delete(&self, id: &KeychainId) -> keyring::Result<()> {
        keyring::Entry::new(id.service(), id.account())?.delete_credential()
    }
}

/// OS credential store: `keyring` 4.x behind [`SecretStore`].
pub struct KeychainStore {
    backend: Box<dyn KeychainBackend>,
    lock: Mutex<()>,
}

impl KeychainStore {
    /// A store over the real platform credential store (macOS Keychain).
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Box::new(SystemKeychain))
    }

    fn with_backend(backend: Box<dyn KeychainBackend>) -> Self {
        Self {
            backend,
            lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeychainStore {
    fn create(&self, id: &KeychainId, value: SecretString) -> Result<(), SecretsError> {
        let _guard = self.lock();
        if let Some(existing) = self.backend.get(id).map_err(|error| map(id, error))? {
            drop(SecretString::new(existing));
            return Err(id.already_exists());
        }
        self.backend
            .set(id, value.expose_secret())
            .map_err(|error| map(id, error))
    }

    fn read(&self, id: &KeychainId) -> Result<SecretString, SecretsError> {
        let _guard = self.lock();
        self.backend
            .get(id)
            .map_err(|error| map(id, error))?
            .map(SecretString::new)
            .ok_or_else(|| id.not_found())
    }

    fn rotate(&self, id: &KeychainId, value: SecretString) -> Result<(), SecretsError> {
        let _guard = self.lock();
        match self.backend.get(id).map_err(|error| map(id, error))? {
            Some(existing) => drop(SecretString::new(existing)),
            None => return Err(id.not_found()),
        }
        self.backend
            .set(id, value.expose_secret())
            .map_err(|error| map(id, error))
    }

    fn delete(&self, id: &KeychainId) -> Result<(), SecretsError> {
        let _guard = self.lock();
        self.backend.delete(id).map_err(|error| map(id, error))
    }

    fn exists(&self, id: &KeychainId) -> Result<bool, SecretsError> {
        let _guard = self.lock();
        match self.backend.get(id).map_err(|error| map(id, error))? {
            Some(existing) => {
                drop(SecretString::new(existing));
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// Maps a `keyring` failure without copying any stored bytes into the error.
fn map(id: &KeychainId, error: keyring::Error) -> SecretsError {
    use keyring::Error;

    match error {
        Error::NoEntry => id.not_found(),
        Error::NoStorageAccess(_) | Error::PlatformFailure(_) => SecretsError::KeychainDenied {
            service: id.service().to_owned(),
        },
        Error::BadEncoding(_) | Error::BadDataFormat(..) => SecretsError::CorruptSecret {
            service: id.service().to_owned(),
            account: id.account().to_owned(),
        },
        Error::TooLong(..) => SecretsError::Keychain {
            service: id.service().to_owned(),
            kind: "attribute too long",
        },
        Error::Invalid(..) => SecretsError::Keychain {
            service: id.service().to_owned(),
            kind: "invalid credential parameters",
        },
        Error::Ambiguous(_) => SecretsError::Keychain {
            service: id.service().to_owned(),
            kind: "ambiguous credential",
        },
        Error::NoDefaultStore => SecretsError::KeychainUnavailable {
            service: id.service().to_owned(),
        },
        Error::NotSupportedByStore(_) => SecretsError::Keychain {
            service: id.service().to_owned(),
            kind: "unsupported by this credential store",
        },
        _ => SecretsError::Keychain {
            service: id.service().to_owned(),
            kind: "unexpected credential-store failure",
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;

    use keyring_core::api::CredentialStoreApi;
    use keyring_core::mock;

    use super::*;
    use crate::name::KEYCHAIN_SERVICE;
    use crate::secret::SecretString;

    const SENTINEL: &str = "sk-live-sentinel-value";

    #[test]
    fn maps_each_platform_failure() {
        let id = KeychainId::new("OPENAI_API_KEY").unwrap();

        let missing = map(&id, keyring::Error::NoEntry);
        assert!(matches!(missing, SecretsError::NotFound { .. }));

        for error in [
            keyring::Error::NoStorageAccess(Box::new(io::Error::other("locked"))),
            keyring::Error::PlatformFailure(Box::new(io::Error::other(
                "errSecInteractionNotAllowed",
            ))),
        ] {
            let denied = map(&id, error);
            assert!(matches!(denied, SecretsError::KeychainDenied { .. }));
            let text = denied.to_string();
            assert!(text.contains("Keychain Access"), "{text}");
            assert!(text.contains(KEYCHAIN_SERVICE), "{text}");
        }

        let corrupt = map(
            &id,
            keyring::Error::BadEncoding(Vec::from(SENTINEL.as_bytes())),
        );
        assert!(matches!(corrupt, SecretsError::CorruptSecret { .. }));
        assert!(!format!("{corrupt} {corrupt:?}").contains(SENTINEL));

        let unavailable = map(&id, keyring::Error::NoDefaultStore);
        assert!(matches!(
            unavailable,
            SecretsError::KeychainUnavailable { .. }
        ));
    }

    #[test]
    fn error_payloads_never_contain_the_value() {
        let id = KeychainId::new("OPENAI_API_KEY").unwrap();
        let denied = map(
            &id,
            keyring::Error::NoStorageAccess(Box::new(io::Error::other(SENTINEL))),
        );
        assert!(!format!("{denied} {denied:?}").contains(SENTINEL));
    }

    /// A [`KeychainBackend`] backed by the `keyring-core` mock store.
    struct MockKeychain {
        store: Arc<mock::Store>,
    }

    impl MockKeychain {
        fn new() -> Self {
            Self {
                store: mock::Store::new().unwrap(),
            }
        }

        fn credential(&self, id: &KeychainId) -> keyring_core::Entry {
            self.store.build(id.service(), id.account(), None).unwrap()
        }
    }

    impl KeychainBackend for MockKeychain {
        fn get(&self, id: &KeychainId) -> keyring::Result<Option<String>> {
            match self.credential(id).get_password() {
                Ok(value) => Ok(Some(value)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(error),
            }
        }

        fn set(&self, id: &KeychainId, value: &str) -> keyring::Result<()> {
            self.credential(id).set_password(value)
        }

        fn delete(&self, id: &KeychainId) -> keyring::Result<()> {
            self.credential(id).delete_credential()
        }
    }

    fn id() -> KeychainId {
        KeychainId::new("OPENAI_API_KEY").unwrap()
    }

    #[test]
    fn keyring_backend_round_trip() {
        let store = KeychainStore::with_backend(Box::new(MockKeychain::new()));
        assert!(!store.exists(&id()).unwrap());

        store.create(&id(), SecretString::new("first")).unwrap();
        assert_eq!(store.read(&id()).unwrap().expose_secret(), "first");

        let err = store
            .create(&id(), SecretString::new("replacement"))
            .unwrap_err();
        assert!(matches!(err, SecretsError::AlreadyExists { .. }));
        assert_eq!(store.read(&id()).unwrap().expose_secret(), "first");
        assert!(!format!("{err} {err:?}").contains("replacement"));

        store.rotate(&id(), SecretString::new("second")).unwrap();
        assert_eq!(store.read(&id()).unwrap().expose_secret(), "second");

        store.delete(&id()).unwrap();
        assert!(!store.exists(&id()).unwrap());
        assert!(matches!(
            store.delete(&id()).unwrap_err(),
            SecretsError::NotFound { .. }
        ));
    }

    #[test]
    fn concurrent_reads_return_the_same_value() {
        const ITERATIONS: usize = 100;

        let store = Arc::new(KeychainStore::with_backend(Box::new(MockKeychain::new())));
        store.create(&id(), SecretString::new(SENTINEL)).unwrap();

        for iteration in 0..ITERATIONS {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let mut handles = Vec::with_capacity(2);
            for _ in 0..2 {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    store.read(&id()).unwrap().expose_secret().to_owned()
                }));
            }
            for handle in handles {
                let value = handle.join().expect("reader thread must not panic");
                assert_eq!(value, SENTINEL, "iteration {iteration}");
            }
        }
    }

    #[test]
    fn keyring_denial_maps_to_keychain_denied_without_value() {
        let backend = MockKeychain::new();
        let credential = backend.credential(&id());
        let store = KeychainStore::with_backend(Box::new(backend));
        store.create(&id(), SecretString::new(SENTINEL)).unwrap();

        let mock = credential
            .as_any()
            .downcast_ref::<mock::Cred>()
            .expect("mock credential");
        mock.set_error(keyring::Error::NoStorageAccess(Box::new(io::Error::other(
            "interaction not allowed",
        ))));

        let err = store.read(&id()).unwrap_err();
        assert!(matches!(err, SecretsError::KeychainDenied { .. }));
        let text = err.to_string();
        assert!(text.contains("Keychain Access"), "{text}");
        assert!(text.contains(KEYCHAIN_SERVICE), "{text}");
        assert!(!format!("{err} {err:?}").contains(SENTINEL));
    }
}
