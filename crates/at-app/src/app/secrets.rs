//! Secret inventory and mutation commands. Only `at-secrets` accesses Keychain values.

use std::sync::{Arc, Mutex, PoisonError};

use at_db::secret_refs::SecretRefStore;
use at_db::{Database, secret_refs};
use at_secrets::{
    KeychainId, KeychainStore, SecretName, SecretRefs, SecretStore, SecretString, delete_with_refs,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::commands::CommandError;
use super::state::AppState;

/// A value-free row shown by the Keychain secrets inventory and picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SecretInventoryView {
    /// Stable logical name.
    pub name: String,
    /// Keychain account label.
    pub account: String,
    /// Number of skills or servers using this reference.
    #[ts(type = "number")]
    pub used_by_count: u64,
    /// Referencing skill or server labels.
    pub used_by: Vec<String>,
    /// Creation timestamp, if recorded.
    pub created_at: Option<String>,
    /// Last rotation timestamp, if recorded.
    pub rotated_at: Option<String>,
    /// Last successful read timestamp, if recorded.
    pub last_read_ok_at: Option<String>,
    /// Whether no item uses this reference.
    pub orphaned: bool,
    /// Secret availability without disclosing the value.
    pub available: bool,
}

/// Creates a Keychain item and records its value-free reference row.
///
/// The input is immediately moved into the zeroizing `SecretString`; it is never returned,
/// formatted or logged. If SQLite bookkeeping fails, the newly created Keychain entry is removed.
///
/// # Errors
/// Returns a redacted typed error when validation, Keychain, or database access fails.
pub fn create_secret(
    database: &Arc<Mutex<Database>>,
    store: &dyn SecretStore,
    name: &str,
    value: SecretString,
) -> Result<(), CommandError> {
    let name = SecretName::new(name.to_owned())
        .map_err(|_| CommandError::invalid_argument("name", "must be a valid secret name"))?;
    let id = KeychainId::new(name.as_str().to_owned())
        .map_err(|_| CommandError::invalid_argument("name", "must be a valid Keychain account"))?;
    store
        .create(&id, value)
        .map_err(|_| CommandError::unavailable("Keychain could not store the secret"))?;
    let db = database.lock().unwrap_or_else(PoisonError::into_inner);
    if secret_refs::record_reference(db.connection(), &name, &id).is_err() {
        drop(db);
        let rollback = store.delete(&id);
        return Err(CommandError::internal(if rollback.is_ok() {
            "secret reference could not be recorded; Keychain entry was rolled back"
        } else {
            "secret reference could not be recorded; Keychain rollback failed"
        }));
    }
    Ok(())
}

/// Rotates a stored Keychain value and updates the value-free rotation timestamp.
///
/// # Errors
/// Returns a redacted typed error when the reference, Keychain item or database is unavailable.
pub fn rotate_secret(
    database: &Arc<Mutex<Database>>,
    store: &dyn SecretStore,
    name: &str,
    value: SecretString,
) -> Result<(), CommandError> {
    let name = SecretName::new(name.to_owned())
        .map_err(|_| CommandError::invalid_argument("name", "must be a valid secret name"))?;
    let refs = SecretRefStore::new(Arc::clone(database));
    let id = refs
        .keychain_id(&name)
        .map_err(|_| CommandError::unavailable("secret reference is unavailable"))?;
    store
        .rotate(&id, value)
        .map_err(|_| CommandError::unavailable("Keychain could not rotate the secret"))?;
    let db = database.lock().unwrap_or_else(PoisonError::into_inner);
    secret_refs::mark_rotated(db.connection(), &name, None)
        .map_err(|_| CommandError::internal("secret rotation was not recorded"))?;
    Ok(())
}

/// Returns the inventory without reading or returning secret values.
///
/// # Errors
/// Returns a typed error when the database or Keychain availability check fails.
pub fn list_secrets(
    database: &Arc<Mutex<Database>>,
    store: &dyn SecretStore,
) -> Result<Vec<SecretInventoryView>, CommandError> {
    let refs = SecretRefStore::new(Arc::clone(database));
    let rows = refs
        .inventory()
        .map_err(|_| CommandError::internal("secret inventory is unavailable"))?;
    let mut output = Vec::with_capacity(rows.len());
    for row in rows {
        let name = SecretName::new(row.name.clone())
            .map_err(|_| CommandError::internal("secret inventory contains an invalid name"))?;
        let id = refs
            .keychain_id(&name)
            .map_err(|_| CommandError::internal("secret inventory identity is unavailable"))?;
        let available = store
            .exists(&id)
            .map_err(|_| CommandError::unavailable("Keychain availability could not be checked"))?;
        let used_by_count = u64::try_from(row.use_count).map_err(|_| {
            CommandError::internal("secret inventory has an invalid reference count")
        })?;
        let used_by = refs
            .referenced_by(&name)
            .map_err(|_| CommandError::internal("secret reference list is unavailable"))?
            .into_iter()
            .map(|reference| reference.slug)
            .collect();
        output.push(SecretInventoryView {
            name: row.name,
            account: id.account().to_owned(),
            used_by_count,
            used_by,
            created_at: row.created_at,
            rotated_at: row.rotated_at,
            last_read_ok_at: row.last_read_ok_at,
            orphaned: used_by_count == 0,
            available,
        });
    }
    Ok(output)
}

/// Lists known references with value-free metadata for the picker and inventory screen.
#[tauri::command]
pub async fn secrets_list(
    state: State<'_, AppState>,
) -> Result<Vec<SecretInventoryView>, CommandError> {
    let database = state.database_handle().map_err(CommandError::unavailable)?;
    tokio::task::spawn_blocking(move || list_secrets(&database, &KeychainStore::new()))
        .await
        .map_err(|_| CommandError::internal("secret inventory task failed"))?
}

/// Creates a Keychain secret and its reference record without returning the value.
#[tauri::command]
pub async fn secrets_create(
    state: State<'_, AppState>,
    name: String,
    value: String,
) -> Result<(), CommandError> {
    let database = state.database_handle().map_err(CommandError::unavailable)?;
    tokio::task::spawn_blocking(move || {
        create_secret(
            &database,
            &KeychainStore::new(),
            &name,
            SecretString::new(value),
        )
    })
    .await
    .map_err(|_| CommandError::internal("secret creation task failed"))?
}

/// Rotates a Keychain value and records the rotation timestamp.
#[tauri::command]
pub async fn secrets_rotate(
    state: State<'_, AppState>,
    name: String,
    value: String,
) -> Result<(), CommandError> {
    let database = state.database_handle().map_err(CommandError::unavailable)?;
    tokio::task::spawn_blocking(move || {
        rotate_secret(
            &database,
            &KeychainStore::new(),
            &name,
            SecretString::new(value),
        )
    })
    .await
    .map_err(|_| CommandError::internal("secret rotation task failed"))?
}

/// Deletes a Keychain secret only after the caller confirms the named reference.
#[tauri::command]
pub async fn secrets_delete(
    state: State<'_, AppState>,
    name: String,
    confirm: bool,
) -> Result<(), CommandError> {
    let database = state.database_handle().map_err(CommandError::unavailable)?;
    tokio::task::spawn_blocking(move || {
        let name = SecretName::new(name)
            .map_err(|_| CommandError::invalid_argument("name", "must be a valid secret name"))?;
        let refs = SecretRefStore::new(database);
        delete_with_refs(&KeychainStore::new(), &refs, &name, confirm).map_err(|_| {
            CommandError::unavailable("secret cannot be deleted; inspect references and confirm")
        })?;
        Ok(())
    })
    .await
    .map_err(|_| CommandError::internal("secret deletion task failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_and_picker_views_never_contain_secret_values() {
        let database = at_db::Database::open_in_memory().expect("database");
        let database = Arc::new(Mutex::new(database));
        let store = at_secrets::MemoryStore::new();
        let sentinel = concat!("token-", "private-value");
        create_secret(
            &database,
            &store,
            "github-token",
            SecretString::new(sentinel),
        )
        .expect("create secret");

        let inventory = list_secrets(&database, &store).expect("inventory");
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].name, "github-token");
        assert!(inventory[0].available);
        let encoded = serde_json::to_string(&inventory).expect("serialize value-free inventory");
        assert!(!encoded.contains(sentinel));

        rotate_secret(
            &database,
            &store,
            "github-token",
            SecretString::new("rotated"),
        )
        .expect("rotate secret");
        assert!(
            list_secrets(&database, &store).expect("reloaded inventory")[0]
                .rotated_at
                .is_some()
        );
    }
}
