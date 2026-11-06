//! Behaviour tests against the in-memory keychain stub. No test touches the real
//! Keychain, the developer's home directory or the network.

use std::sync::{Arc, Barrier};
use std::thread;

use at_secrets::{
    KeychainId, MemoryStore, SecretName, SecretReference, SecretRefs, SecretStore, SecretString,
    SecretsError, delete_with_refs,
};

fn id() -> KeychainId {
    KeychainId::new("OPENAI_API_KEY").unwrap()
}

#[test]
fn create_read_rotate_delete_round_trip() {
    let store = MemoryStore::new();
    assert!(!store.exists(&id()).unwrap());

    store.create(&id(), SecretString::new("first")).unwrap();
    assert_eq!(store.read(&id()).unwrap().expose_secret(), "first");

    store.rotate(&id(), SecretString::new("second")).unwrap();
    assert_eq!(store.read(&id()).unwrap().expose_secret(), "second");

    store.delete(&id()).unwrap();
    assert!(!store.exists(&id()).unwrap());
}

#[test]
fn create_on_existing_name_returns_already_exists_and_keeps_value() {
    let store = MemoryStore::new();
    store.create(&id(), SecretString::new("original")).unwrap();

    let err = store
        .create(&id(), SecretString::new("sk-sentinel-replacement"))
        .unwrap_err();
    assert!(matches!(err, SecretsError::AlreadyExists { .. }));
    assert!(!format!("{err} {err:?}").contains("sk-sentinel-replacement"));
    assert_eq!(store.read(&id()).unwrap().expose_secret(), "original");
}

#[test]
fn delete_of_missing_name_returns_not_found() {
    let store = MemoryStore::new();
    let err = store.delete(&id()).unwrap_err();
    assert!(matches!(err, SecretsError::NotFound { .. }));
    assert!(!format!("{err} {err:?}").contains("sk-"));
}

#[test]
fn named_secret_maps_to_the_default_keychain_service() {
    let name = SecretName::new("OPENAI_API_KEY").unwrap();
    let id = KeychainId::from(&name);
    assert_eq!(id.service(), at_secrets::KEYCHAIN_SERVICE);
    assert_eq!(id.account(), "OPENAI_API_KEY");

    let store = MemoryStore::new();
    store.create(&id, SecretString::new("value")).unwrap();
    assert_eq!(store.read(&id).unwrap().expose_secret(), "value");
}

#[test]
fn two_threads_reading_the_same_secret_see_the_correct_value() {
    const ITERATIONS: usize = 100;
    const SENTINEL: &str = "sk-concurrent-sentinel";

    let store = Arc::new(MemoryStore::new());
    let id = id();
    store.create(&id, SecretString::new(SENTINEL)).unwrap();

    for iteration in 0..ITERATIONS {
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::with_capacity(2);
        for _ in 0..2 {
            let store = Arc::clone(&store);
            let id = id.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                store.read(&id).unwrap().expose_secret().to_owned()
            }));
        }
        for handle in handles {
            let value = handle.join().expect("reader thread must not panic");
            assert_eq!(value, SENTINEL, "iteration {iteration}");
        }
    }
}

#[test]
fn concurrent_create_of_the_same_name_has_exactly_one_winner() {
    const ITERATIONS: usize = 20;

    for _ in 0..ITERATIONS {
        let store = Arc::new(MemoryStore::new());
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::with_capacity(2);
        for value in ["first", "second"] {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                store.create(&id(), SecretString::new(value))
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("writer thread must not panic"))
            .collect();
        let winners = results.iter().filter(|result| result.is_ok()).count();
        let losers = results
            .iter()
            .filter(|result| matches!(result, Err(SecretsError::AlreadyExists { .. })))
            .count();
        assert_eq!(winners, 1, "exactly one create must succeed");
        assert_eq!(losers, 1, "the other create must see AlreadyExists");
    }
}

/// Minimal `SecretRefs` for the guarded-delete test.
struct Refs {
    id: KeychainId,
    references: Vec<SecretReference>,
}

impl SecretRefs for Refs {
    fn keychain_id(&self, _name: &SecretName) -> Result<KeychainId, SecretsError> {
        Ok(self.id.clone())
    }

    fn referencing(&self, _name: &SecretName) -> Result<Vec<SecretReference>, SecretsError> {
        Ok(self.references.clone())
    }

    fn remove(&self, _name: &SecretName) -> Result<(), SecretsError> {
        Ok(())
    }
}

#[test]
fn guarded_delete_reports_references_before_removing_anything() {
    let store = MemoryStore::new();
    let id = id();
    store.create(&id, SecretString::new("value")).unwrap();
    let refs = Refs {
        id: id.clone(),
        references: vec![SecretReference {
            item_id: "itm_0190f5d0-0000-7000-8000-000000000001".to_owned(),
            scope: "project".to_owned(),
            slug: "openai".to_owned(),
        }],
    };

    let err = delete_with_refs(
        &store,
        &refs,
        &SecretName::new("OPENAI_API_KEY").unwrap(),
        false,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SecretsError::ReferencedSecret { count: 1, .. }
    ));
    assert!(store.exists(&id).unwrap(), "nothing may be deleted");
}
