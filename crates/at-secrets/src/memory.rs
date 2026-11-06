//! In-memory [`SecretStore`] for tests and dry runs.
//!
//! Values live only in this process and disappear when the store drops. Tests use it
//! instead of the real Keychain; nothing here touches the operating system.

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::error::SecretsError;
use crate::name::KeychainId;
use crate::secret::SecretString;
use crate::store::SecretStore;

/// A credential store backed by a `HashMap` behind an `RwLock`.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: RwLock<HashMap<KeychainId, SecretString>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn read_guard(&self) -> RwLockReadGuard<'_, HashMap<KeychainId, SecretString>> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_guard(&self) -> RwLockWriteGuard<'_, HashMap<KeychainId, SecretString>> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretStore for MemoryStore {
    fn create(&self, id: &KeychainId, value: SecretString) -> Result<(), SecretsError> {
        let mut map = self.write_guard();
        if map.contains_key(id) {
            return Err(id.already_exists());
        }
        map.insert(id.clone(), value);
        Ok(())
    }

    fn read(&self, id: &KeychainId) -> Result<SecretString, SecretsError> {
        let map = self.read_guard();
        map.get(id)
            .map(|value| SecretString::new(value.expose_secret()))
            .ok_or_else(|| id.not_found())
    }

    fn rotate(&self, id: &KeychainId, value: SecretString) -> Result<(), SecretsError> {
        let mut map = self.write_guard();
        let slot = map.get_mut(id).ok_or_else(|| id.not_found())?;
        *slot = value;
        Ok(())
    }

    fn delete(&self, id: &KeychainId) -> Result<(), SecretsError> {
        let mut map = self.write_guard();
        map.remove(id).map(drop).ok_or_else(|| id.not_found())
    }

    fn exists(&self, id: &KeychainId) -> Result<bool, SecretsError> {
        Ok(self.read_guard().contains_key(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> KeychainId {
        KeychainId::new("OPENAI_API_KEY").unwrap()
    }

    #[test]
    fn round_trip() {
        let store = MemoryStore::new();
        assert!(!store.exists(&id()).unwrap());
        store.create(&id(), SecretString::new("first")).unwrap();
        assert_eq!(store.read(&id()).unwrap().expose_secret(), "first");
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
    fn create_existing_returns_already_exists_and_keeps_value() {
        let store = MemoryStore::new();
        store.create(&id(), SecretString::new("original")).unwrap();
        let err = store
            .create(&id(), SecretString::new("replacement"))
            .unwrap_err();
        assert!(matches!(err, SecretsError::AlreadyExists { .. }));
        assert_eq!(store.read(&id()).unwrap().expose_secret(), "original");
        assert!(!format!("{err} {err:?}").contains("replacement"));
    }

    #[test]
    fn rotate_missing_is_not_found() {
        let store = MemoryStore::new();
        assert!(matches!(
            store.rotate(&id(), SecretString::new("value")).unwrap_err(),
            SecretsError::NotFound { .. }
        ));
    }
}
