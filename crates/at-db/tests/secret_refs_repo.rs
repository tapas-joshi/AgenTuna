//! TM-010: secret_refs / secret_ref_uses inventory and the SecretRefs seam.

use std::sync::{Arc, Mutex};

use at_db::Database;
use at_db::secret_refs::{
    SecretRefStore, delete_reference, forget_use, inventory, keychain_id, mark_read_ok,
    mark_rotated, missing_keychain, orphaned, record_reference, record_use, referenced_by,
};
use at_model::Scope;
use at_secrets::{
    KeychainId, MemoryStore, SecretName, SecretStore, SecretString, SecretsError, delete_with_refs,
};

fn database() -> Database {
    Database::open_in_memory().expect("in-memory database opens")
}

fn name(value: &str) -> SecretName {
    SecretName::new(value).expect("valid secret name")
}

fn insert_item(connection: &rusqlite::Connection, id: &str, scope: Scope, slug: &str) {
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, 'mcp_server', ?2, ?3, ?3)",
            rusqlite::params![id, scope.wire_name(), slug],
        )
        .expect("item inserted");
}

#[test]
fn inventory_counts_uses_and_passes_timestamps_through() {
    let database = database();
    let connection = database.connection();
    let api_key = name("OPENAI_API_KEY");
    let other = name("GITHUB_TOKEN");
    let keychain = KeychainId::from(&api_key);

    record_reference(connection, &api_key, &keychain).expect("reference");
    record_reference(connection, &other, &KeychainId::from(&other)).expect("reference");
    record_use(connection, &api_key, "itm_one", Scope::User).expect("use");
    record_use(connection, &api_key, "itm_two", Scope::Project).expect("use");
    // Repeating a use row is a no-op.
    record_use(connection, &api_key, "itm_one", Scope::User).expect("repeat");

    let created_at: String = connection
        .query_row(
            "SELECT created_at FROM secret_refs WHERE name = 'OPENAI_API_KEY'",
            [],
            |row| row.get(0),
        )
        .expect("created_at");
    assert!(created_at.ends_with('Z'));

    assert!(mark_read_ok(connection, &api_key, Some("2026-02-03T04:05:06.000Z")).expect("read"));
    assert!(mark_rotated(connection, &api_key, Some("2026-02-04T00:00:00.000Z")).expect("rotate"));

    let entries = inventory(connection).expect("inventory");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "GITHUB_TOKEN");
    assert_eq!(entries[0].use_count, 0);
    assert_eq!(entries[1].name, "OPENAI_API_KEY");
    assert_eq!(entries[1].use_count, 2);
    assert_eq!(
        entries[1].last_read_ok_at.as_deref(),
        Some("2026-02-03T04:05:06.000Z"),
        "last_read_ok_at is passed through unchanged"
    );
    assert_eq!(
        entries[1].rotated_at.as_deref(),
        Some("2026-02-04T00:00:00.000Z")
    );
    assert_eq!(
        entries[1].keychain_service.as_deref(),
        Some("dev.agentuna.mcp")
    );
    assert!(!mark_read_ok(connection, &name("ABSENT"), None).expect("missing ref"));
}

#[test]
fn referenced_by_reports_every_item_and_scope_with_its_slug() {
    let database = database();
    let connection = database.connection();
    let api_key = name("OPENAI_API_KEY");
    record_reference(connection, &api_key, &KeychainId::from(&api_key)).expect("reference");
    insert_item(connection, "itm_user", Scope::User, "openai");
    insert_item(connection, "itm_project", Scope::Project, "openai");

    record_use(connection, &api_key, "itm_user", Scope::User).expect("use");
    record_use(connection, &api_key, "itm_project", Scope::Project).expect("use");

    let references = referenced_by(connection, &api_key).expect("references");
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].item_id, "itm_project");
    assert_eq!(references[0].scope, "project");
    assert_eq!(references[0].slug, "openai");
    assert_eq!(references[1].item_id, "itm_user");
    assert_eq!(references[1].scope, "user");

    // A use row whose item is gone still blocks deletion, with the id as slug.
    record_use(connection, &api_key, "itm_gone", Scope::Global).expect("orphan use");
    let references = referenced_by(connection, &api_key).expect("references");
    assert_eq!(references.len(), 3);
    assert!(
        references
            .iter()
            .any(|reference| reference.item_id == "itm_gone" && reference.slug == "itm_gone")
    );

    assert!(
        referenced_by(connection, &name("UNUSED"))
            .expect("unused")
            .is_empty()
    );
    assert!(forget_use(connection, &api_key, "itm_gone", Scope::Global).expect("remove"));
    assert!(!forget_use(connection, &api_key, "itm_gone", Scope::Global).expect("already gone"));
}

#[test]
fn orphaned_lists_references_without_a_use() {
    let database = database();
    let connection = database.connection();
    let used = name("USED_KEY");
    let unused = name("UNUSED_KEY");
    record_reference(connection, &used, &KeychainId::from(&used)).expect("reference");
    record_reference(connection, &unused, &KeychainId::from(&unused)).expect("reference");
    record_use(connection, &used, "itm_one", Scope::User).expect("use");

    let orphans = orphaned(connection).expect("orphans");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].name, "UNUSED_KEY");
    assert_eq!(orphans[0].use_count, 0);
}

#[test]
fn deleting_a_referenced_secret_needs_confirmation_and_changes_nothing() {
    let database = database();
    record_reference(
        database.connection(),
        &name("OPENAI_API_KEY"),
        &KeychainId::from(&name("OPENAI_API_KEY")),
    )
    .expect("reference");
    record_use(
        database.connection(),
        &name("OPENAI_API_KEY"),
        "itm_one",
        Scope::User,
    )
    .expect("use");
    record_use(
        database.connection(),
        &name("OPENAI_API_KEY"),
        "itm_two",
        Scope::Project,
    )
    .expect("use");

    let memory = MemoryStore::new();
    let api_key = name("OPENAI_API_KEY");
    let keychain = KeychainId::from(&api_key);
    memory
        .create(&keychain, SecretString::new("sk-test-value"))
        .expect("keychain seeded");
    let shared = Arc::new(Mutex::new(database));
    let store = SecretRefStore::new(Arc::clone(&shared));

    let error = delete_with_refs(&memory, &store, &api_key, false).expect_err("no confirmation");
    match error {
        SecretsError::ReferencedSecret {
            count, references, ..
        } => {
            assert_eq!(count, 2);
            assert_eq!(references.len(), 2);
        }
        other => panic!("expected ReferencedSecret, got {other}"),
    }
    assert!(
        memory.exists(&keychain).expect("exists"),
        "the value is untouched"
    );
    assert_eq!(
        store.inventory().expect("inventory").len(),
        1,
        "the reference row is untouched"
    );

    delete_with_refs(&memory, &store, &api_key, true).expect("confirmed delete");
    assert!(!memory.exists(&keychain).expect("exists"));
    assert!(store.inventory().expect("inventory").is_empty());
    assert!(
        !delete_reference(shared.lock().expect("lock").connection(), &api_key)
            .expect("row already gone")
    );
}

#[test]
fn missing_keychain_is_reported_and_projection_never_substitutes_an_empty_value() {
    let database = database();
    let present = name("PRESENT_KEY");
    let missing = name("MISSING_KEY");
    for key in [&present, &missing] {
        record_reference(database.connection(), key, &KeychainId::from(key)).expect("reference");
    }

    let memory = MemoryStore::new();
    memory
        .create(
            &KeychainId::from(&present),
            SecretString::new("secret-value"),
        )
        .expect("keychain seeded");

    let lacking = missing_keychain(database.connection(), &memory).expect("missing");
    assert_eq!(lacking.len(), 1);
    assert_eq!(lacking[0].name, "MISSING_KEY");

    let shared = Arc::new(Mutex::new(database));
    let store = SecretRefStore::new(shared);
    let value = store.resolve(&present, &memory).expect("present value");
    assert_eq!(value.expose_secret(), "secret-value");

    match store.resolve(&missing, &memory) {
        Err(SecretsError::MissingSecret { name }) => assert_eq!(name, "MISSING_KEY"),
        Err(other) => panic!("expected MissingSecret, got {other}"),
        Ok(_) => panic!("a missing Keychain item must not resolve"),
    }
}

#[test]
fn keychain_identity_comes_from_the_stored_row() {
    let database = database();
    let connection = database.connection();
    let token = name("REMOTE_TOKEN");
    let custom =
        KeychainId::with_service("dev.agentuna.remote-backup", "github-token").expect("identity");
    record_reference(connection, &token, &custom).expect("reference");

    let stored = keychain_id(connection, &token).expect("identity");
    assert_eq!(stored.service(), "dev.agentuna.remote-backup");
    assert_eq!(stored.account(), "github-token");

    assert!(matches!(
        keychain_id(connection, &name("ABSENT")),
        Err(at_db::secret_refs::SecretRefError::Secrets(
            SecretsError::NotFound { .. }
        ))
    ));
}
