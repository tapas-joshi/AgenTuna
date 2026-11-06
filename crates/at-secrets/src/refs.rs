//! Reference bookkeeping for `secret_refs` and `secret_ref_uses`.
//!
//! `keyring` 4.x cannot enumerate items, so the database is the inventory. The
//! [`SecretRefs`] trait is the seam implemented by the repository story (TM-010);
//! [`delete_with_refs`] composes it with a [`SecretStore`] so deleting a secret also
//! removes its reference row and reports referencing servers before doing so.

use crate::error::SecretsError;
use crate::name::{KeychainId, SecretName};
use crate::store::SecretStore;

/// One item that references a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretReference {
    /// Item that uses the secret.
    pub item_id: String,
    /// Scope of that item (`user`, `project`, `global`).
    pub scope: String,
    /// URL-safe slug of the item, for display.
    pub slug: String,
}

/// Repository seam over `secret_refs` and `secret_ref_uses` (TM-010).
///
/// Implementations must not store values: the Keychain is the source of truth for
/// the secret itself, and these tables hold names, the Keychain identity and dates.
pub trait SecretRefs: Send + Sync {
    /// Keychain identity recorded for `name`.
    ///
    /// # Errors
    /// [`SecretsError::NotFound`] when no `secret_refs` row exists.
    fn keychain_id(&self, name: &SecretName) -> Result<KeychainId, SecretsError>;

    /// Every item that currently references `name`, for the pre-delete warning.
    ///
    /// # Errors
    /// A repository failure. An unused secret returns an empty list.
    fn referencing(&self, name: &SecretName) -> Result<Vec<SecretReference>, SecretsError>;

    /// Removes the `secret_refs` row for `name`; `secret_ref_uses` rows cascade.
    ///
    /// # Errors
    /// A repository failure, including a missing row.
    fn remove(&self, name: &SecretName) -> Result<(), SecretsError>;
}

/// Deletes a Keychain entry and its `secret_refs` row as one logical operation.
///
/// When items still reference `name`, the call fails with
/// [`SecretsError::ReferencedSecret`] and changes nothing unless `confirm` is true.
/// The value is read before deletion so a failed reference-row removal can restore
/// the Keychain entry; the error reports whether that rollback succeeded.
///
/// The Keychain and SQLite cannot share a transaction: a crash between the two steps
/// leaves the reference row behind, which the inventory reports as a missing Keychain
/// item rather than silently treating the secret as present.
///
/// # Errors
/// [`SecretsError::ReferencedSecret`] without confirmation,
/// [`SecretsError::NotFound`] when the Keychain entry is missing, or a credential-store
/// or repository failure.
pub fn delete_with_refs(
    store: &dyn SecretStore,
    refs: &dyn SecretRefs,
    name: &SecretName,
    confirm: bool,
) -> Result<(), SecretsError> {
    let references = refs.referencing(name)?;
    if !references.is_empty() && !confirm {
        return Err(SecretsError::ReferencedSecret {
            name: name.as_str().to_owned(),
            count: references.len(),
            references,
        });
    }

    let id = refs.keychain_id(name)?;
    let value = store.read(&id)?;
    store.delete(&id)?;
    match refs.remove(name) {
        Ok(()) => Ok(()),
        Err(_error) => {
            let restored = store.create(&id, value).is_ok();
            Err(SecretsError::RefsUpdateFailed {
                name: name.as_str().to_owned(),
                restored,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::memory::MemoryStore;
    use crate::secret::SecretString;

    struct FakeRefs {
        id: KeychainId,
        references: Vec<SecretReference>,
        removed: AtomicBool,
        fail_remove: bool,
    }

    impl FakeRefs {
        fn new(id: KeychainId, references: Vec<SecretReference>) -> Self {
            Self {
                id,
                references,
                removed: AtomicBool::new(false),
                fail_remove: false,
            }
        }

        fn failing(id: KeychainId, references: Vec<SecretReference>) -> Self {
            Self {
                fail_remove: true,
                ..Self::new(id, references)
            }
        }
    }

    impl SecretRefs for FakeRefs {
        fn keychain_id(&self, _name: &SecretName) -> Result<KeychainId, SecretsError> {
            Ok(self.id.clone())
        }

        fn referencing(&self, _name: &SecretName) -> Result<Vec<SecretReference>, SecretsError> {
            Ok(self.references.clone())
        }

        fn remove(&self, name: &SecretName) -> Result<(), SecretsError> {
            if self.fail_remove {
                return Err(SecretsError::RefsUpdateFailed {
                    name: name.as_str().to_owned(),
                    restored: false,
                });
            }
            self.removed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn name() -> SecretName {
        SecretName::new("OPENAI_API_KEY").unwrap()
    }

    fn reference() -> SecretReference {
        SecretReference {
            item_id: "itm_0190f5d0-0000-7000-8000-000000000001".to_owned(),
            scope: "user".to_owned(),
            slug: "openai".to_owned(),
        }
    }

    fn seeded() -> (MemoryStore, KeychainId) {
        let store = MemoryStore::new();
        let id = KeychainId::from(&name());
        store.create(&id, SecretString::new("sk-existing")).unwrap();
        (store, id)
    }

    #[test]
    fn refuses_unconfirmed_delete_while_referenced() {
        let (store, id) = seeded();
        let refs = FakeRefs::new(id.clone(), vec![reference()]);

        let err = delete_with_refs(&store, &refs, &name(), false).unwrap_err();
        match err {
            SecretsError::ReferencedSecret {
                count, references, ..
            } => {
                assert_eq!(count, 1);
                assert_eq!(references, vec![reference()]);
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(store.exists(&id).unwrap());
        assert!(!refs.removed.load(Ordering::SeqCst));
    }

    #[test]
    fn confirmed_delete_removes_entry_and_reference_row() {
        let (store, id) = seeded();
        let refs = FakeRefs::new(id.clone(), vec![reference()]);

        delete_with_refs(&store, &refs, &name(), true).unwrap();
        assert!(!store.exists(&id).unwrap());
        assert!(refs.removed.load(Ordering::SeqCst));
    }

    #[test]
    fn unreferenced_secret_deletes_without_confirmation() {
        let (store, id) = seeded();
        let refs = FakeRefs::new(id.clone(), Vec::new());

        delete_with_refs(&store, &refs, &name(), false).unwrap();
        assert!(!store.exists(&id).unwrap());
        assert!(refs.removed.load(Ordering::SeqCst));
    }

    #[test]
    fn failed_reference_removal_restores_the_keychain_entry() {
        let (store, id) = seeded();
        let refs = FakeRefs::failing(id.clone(), Vec::new());

        let err = delete_with_refs(&store, &refs, &name(), false).unwrap_err();
        assert!(matches!(
            err,
            SecretsError::RefsUpdateFailed { restored: true, .. }
        ));
        assert_eq!(
            store.read(&id).unwrap().expose_secret(),
            "sk-existing",
            "rollback must restore the original value"
        );
    }

    #[test]
    fn missing_keychain_entry_is_not_found_and_leaves_refs_alone() {
        let store = MemoryStore::new();
        let refs = FakeRefs::new(KeychainId::from(&name()), Vec::new());

        assert!(matches!(
            delete_with_refs(&store, &refs, &name(), false).unwrap_err(),
            SecretsError::NotFound { .. }
        ));
        assert!(!refs.removed.load(Ordering::SeqCst));
    }
}
