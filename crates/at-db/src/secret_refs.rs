//! Secret reference inventory (TM-010) over `secret_refs` and `secret_ref_uses`
//! (migration v9, owned by TF-018).
//!
//! These tables hold references only: a logical name, the Keychain service and
//! account, and dates. No column can hold a secret value, and no query here
//! returns one — the Keychain stays the source of truth for existence and for
//! the value itself.
//!
//! The free functions are the repository: [`inventory`] lists every reference
//! with its use count, [`referenced_by`] resolves the items behind one name,
//! [`orphaned`] finds references nothing uses, and [`missing_keychain`] lists
//! references whose Keychain item is gone. [`SecretRefStore`] wraps the same
//! queries behind the [`at_secrets::SecretRefs`] seam, so
//! [`at_secrets::delete_with_refs`] can refuse to delete a referenced secret
//! without confirmation and can remove the reference row afterwards.
//!
//! This module owns no schema: every statement below is a query or a row
//! change against the tables TF-018 created.

use std::sync::{Arc, Mutex, PoisonError};

use at_model::Scope;
use at_secrets::{
    KEYCHAIN_SERVICE, KeychainId, SecretName, SecretReference, SecretRefs, SecretStore,
    SecretString, SecretsError,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Database;
use crate::DbError;

/// One reference with how many items use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretInventoryEntry {
    /// Logical name, matching `secret_refs.name`.
    pub name: String,
    /// Keychain service recorded for the reference, when it differs from the default.
    pub keychain_service: Option<String>,
    /// Keychain account recorded for the reference, when it differs from the name.
    pub account: Option<String>,
    /// When the reference was first created, as stored.
    pub created_at: Option<String>,
    /// When the value was last rotated, as stored.
    pub rotated_at: Option<String>,
    /// When the Keychain last returned the value successfully.
    pub last_read_ok_at: Option<String>,
    /// Number of `secret_ref_uses` rows for the reference.
    pub use_count: i64,
}

/// Why a secret-reference query did not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretRefError {
    /// The reference tables could not be read or written.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The credential store or a stored Keychain identity was rejected.
    #[error(transparent)]
    Secrets(#[from] SecretsError),
}

const INVENTORY_SQL: &str = "\
SELECT r.name, r.keychain_service, r.account, r.created_at, r.rotated_at, \
       r.last_read_ok_at, COUNT(u.id) AS use_count \
  FROM secret_refs r \
  LEFT JOIN secret_ref_uses u ON u.secret_name = r.name \
 GROUP BY r.name \
 ORDER BY r.name ASC";

const REFERENCE_SQL: &str = "\
SELECT r.name, r.keychain_service, r.account, r.created_at, r.rotated_at, \
       r.last_read_ok_at, \
       (SELECT COUNT(*) FROM secret_ref_uses u WHERE u.secret_name = r.name) AS use_count \
  FROM secret_refs r \
 WHERE r.name = ?1";

const ORPHANED_SQL: &str = "\
SELECT r.name, r.keychain_service, r.account, r.created_at, r.rotated_at, \
       r.last_read_ok_at, 0 AS use_count \
  FROM secret_refs r \
 WHERE NOT EXISTS (SELECT 1 FROM secret_ref_uses u WHERE u.secret_name = r.name) \
 ORDER BY r.name ASC";

const REFERENCED_BY_SQL: &str = "\
SELECT COALESCE(u.item_id, ''), COALESCE(u.scope, ''), COALESCE(i.slug, u.item_id, '') \
  FROM secret_ref_uses u \
  LEFT JOIN items i ON i.id = u.item_id \
 WHERE u.secret_name = ?1 \
 ORDER BY u.item_id ASC, u.scope ASC";

const RECORD_REFERENCE_SQL: &str = "\
INSERT INTO secret_refs (name, keychain_service, account, created_at) \
VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
ON CONFLICT(name) DO UPDATE SET \
    keychain_service = excluded.keychain_service, \
    account = excluded.account";

const RECORD_USE_SQL: &str = "\
INSERT INTO secret_ref_uses (secret_name, item_id, scope) VALUES (?1, ?2, ?3) \
ON CONFLICT(secret_name, item_id, scope) DO NOTHING";

const FORGET_USE_SQL: &str =
    "DELETE FROM secret_ref_uses WHERE secret_name = ?1 AND item_id = ?2 AND scope = ?3";

const MARK_READ_OK_SQL: &str = "\
UPDATE secret_refs \
   SET last_read_ok_at = COALESCE(?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
 WHERE name = ?1";

const MARK_ROTATED_SQL: &str = "\
UPDATE secret_refs \
   SET rotated_at = COALESCE(?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
 WHERE name = ?1";

const DELETE_REFERENCE_SQL: &str = "DELETE FROM secret_refs WHERE name = ?1";

/// Every statement this module issues, for the schema-guard test.
#[cfg(test)]
const STATEMENTS: &[&str] = &[
    INVENTORY_SQL,
    REFERENCE_SQL,
    ORPHANED_SQL,
    REFERENCED_BY_SQL,
    RECORD_REFERENCE_SQL,
    RECORD_USE_SQL,
    FORGET_USE_SQL,
    MARK_READ_OK_SQL,
    MARK_ROTATED_SQL,
    DELETE_REFERENCE_SQL,
];

/// Every stored reference with its use count, ordered by name.
///
/// `last_read_ok_at` is passed through unchanged; it is written by whoever read
/// the value, not by this query.
///
/// # Errors
/// A SQLite failure.
pub fn inventory(connection: &Connection) -> Result<Vec<SecretInventoryEntry>, DbError> {
    let mut statement = connection.prepare(INVENTORY_SQL)?;
    let rows = statement.query_map([], inventory_from_row)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// The items that use `name`, with their scope and slug.
///
/// A use row whose item is no longer in the `items` table still appears: the
/// secret is still referenced somewhere, so deleting it must keep warning.
/// Such a row reports the stored item id as its slug.
///
/// # Errors
/// A SQLite failure.
pub fn referenced_by(
    connection: &Connection,
    name: &SecretName,
) -> Result<Vec<SecretReference>, DbError> {
    let mut statement = connection.prepare(REFERENCED_BY_SQL)?;
    let rows = statement.query_map([name.as_str()], |row| {
        Ok(SecretReference {
            item_id: row.get(0)?,
            scope: row.get(1)?,
            slug: row.get(2)?,
        })
    })?;
    let mut references = Vec::new();
    for row in rows {
        references.push(row?);
    }
    Ok(references)
}

/// References that no `secret_ref_uses` row points at, ordered by name.
///
/// An orphan is reported, not deleted: the Keychain item is removed by
/// [`at_secrets::delete_with_refs`], which also needs the confirmation rule.
///
/// # Errors
/// A SQLite failure.
pub fn orphaned(connection: &Connection) -> Result<Vec<SecretInventoryEntry>, DbError> {
    let mut statement = connection.prepare(ORPHANED_SQL)?;
    let rows = statement.query_map([], inventory_from_row)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// References whose Keychain item no longer exists.
///
/// Existence is checked per reference through the injected store, so tests never
/// touch the real Keychain. The rows are returned with their use counts; a
/// reference with uses is the dangerous case, because the servers that read it
/// will fail.
///
/// # Errors
/// [`SecretRefError::Secrets`] when a stored Keychain identity is invalid or the
/// credential store fails, or [`SecretRefError::Db`] on a SQLite failure.
pub fn missing_keychain(
    connection: &Connection,
    store: &dyn SecretStore,
) -> Result<Vec<SecretInventoryEntry>, SecretRefError> {
    let mut missing = Vec::new();
    for entry in inventory(connection)? {
        let id = identity(&entry)?;
        if !store.exists(&id)? {
            missing.push(entry);
        }
    }
    Ok(missing)
}

/// Keychain identity recorded for `name`.
///
/// A row that records only one of service and account fills the other from the
/// defaults (service [`KEYCHAIN_SERVICE`], account the secret name).
///
/// # Errors
/// [`SecretsError::NotFound`] when no reference row exists, or
/// [`SecretsError::InvalidKeychainId`] when the stored identity is unusable.
pub fn keychain_id(
    connection: &Connection,
    name: &SecretName,
) -> Result<KeychainId, SecretRefError> {
    match find_reference(connection, name)? {
        Some(entry) => identity(&entry).map_err(SecretRefError::from),
        None => Err(SecretsError::NotFound {
            service: KEYCHAIN_SERVICE.to_owned(),
            account: name.as_str().to_owned(),
        }
        .into()),
    }
}

/// Records the Keychain identity for `name`, creating the reference row.
///
/// `created_at` is set on the first call and kept afterwards; the identity is
/// refreshed so a renamed service or account is reflected.
///
/// # Errors
/// A SQLite failure.
pub fn record_reference(
    connection: &Connection,
    name: &SecretName,
    keychain: &KeychainId,
) -> Result<(), DbError> {
    connection.execute(
        RECORD_REFERENCE_SQL,
        params![name.as_str(), keychain.service(), keychain.account()],
    )?;
    Ok(())
}

/// Records that `item_id` in `scope` uses `name`. Repeating it is a no-op.
///
/// # Errors
/// A SQLite failure, including a missing reference row.
pub fn record_use(
    connection: &Connection,
    name: &SecretName,
    item_id: &str,
    scope: Scope,
) -> Result<(), DbError> {
    connection.execute(
        RECORD_USE_SQL,
        params![name.as_str(), item_id, scope.wire_name()],
    )?;
    Ok(())
}

/// Removes one use row. Returns whether a row was removed; the reference row
/// stays, even when that leaves it orphaned.
///
/// # Errors
/// A SQLite failure.
pub fn forget_use(
    connection: &Connection,
    name: &SecretName,
    item_id: &str,
    scope: Scope,
) -> Result<bool, DbError> {
    let removed = connection.execute(
        FORGET_USE_SQL,
        params![name.as_str(), item_id, scope.wire_name()],
    )?;
    Ok(removed == 1)
}

/// Records that the value for `name` was read successfully.
///
/// `at` is an RFC 3339 timestamp for deterministic tests; `None` uses now.
///
/// # Errors
/// A SQLite failure. Returns `false` when no reference row exists.
pub fn mark_read_ok(
    connection: &Connection,
    name: &SecretName,
    at: Option<&str>,
) -> Result<bool, DbError> {
    let updated = connection.execute(MARK_READ_OK_SQL, params![name.as_str(), at])?;
    Ok(updated == 1)
}

/// Records that the value for `name` was rotated.
///
/// `at` is an RFC 3339 timestamp for deterministic tests; `None` uses now.
///
/// # Errors
/// A SQLite failure. Returns `false` when no reference row exists.
pub fn mark_rotated(
    connection: &Connection,
    name: &SecretName,
    at: Option<&str>,
) -> Result<bool, DbError> {
    let updated = connection.execute(MARK_ROTATED_SQL, params![name.as_str(), at])?;
    Ok(updated == 1)
}

/// Removes the reference row for `name`; its use rows cascade.
///
/// This is the repository half of a secret deletion. Callers should go through
/// [`at_secrets::delete_with_refs`], which deletes the Keychain item, refuses to
/// delete a referenced secret without confirmation and restores the Keychain
/// entry when this row removal fails.
///
/// # Errors
/// A SQLite failure. Returns `false` when no row was removed.
pub fn delete_reference(connection: &Connection, name: &SecretName) -> Result<bool, DbError> {
    let removed = connection.execute(DELETE_REFERENCE_SQL, [name.as_str()])?;
    Ok(removed == 1)
}

/// The one reference row for `name`, when it exists.
fn find_reference(
    connection: &Connection,
    name: &SecretName,
) -> Result<Option<SecretInventoryEntry>, DbError> {
    connection
        .query_row(REFERENCE_SQL, [name.as_str()], inventory_from_row)
        .optional()
        .map_err(DbError::from)
}

/// Keychain identity for a stored reference.
fn identity(entry: &SecretInventoryEntry) -> Result<KeychainId, SecretsError> {
    let service = entry
        .keychain_service
        .clone()
        .unwrap_or_else(|| KEYCHAIN_SERVICE.to_owned());
    let account = entry.account.clone().unwrap_or_else(|| entry.name.clone());
    KeychainId::with_service(service, account)
}

fn inventory_from_row(row: &Row<'_>) -> rusqlite::Result<SecretInventoryEntry> {
    Ok(SecretInventoryEntry {
        name: row.get(0)?,
        keychain_service: row.get(1)?,
        account: row.get(2)?,
        created_at: row.get(3)?,
        rotated_at: row.get(4)?,
        last_read_ok_at: row.get(5)?,
        use_count: row.get(6)?,
    })
}

/// SQLite-backed [`SecretRefs`] and secret inventory, sharing the app database.
///
/// The database handle is shared (`Arc<Mutex<Database>>`) because the same
/// connection serves the rest of the app and SQLite serialises access through
/// the mutex; a poisoned lock is recovered, never unwrapped.
pub struct SecretRefStore {
    database: Arc<Mutex<Database>>,
}

impl SecretRefStore {
    /// Wraps the process database handle.
    #[must_use]
    pub fn new(database: Arc<Mutex<Database>>) -> Self {
        Self { database }
    }

    fn with_connection<T>(
        &self,
        query: impl FnOnce(&Connection) -> Result<T, SecretRefError>,
    ) -> Result<T, SecretRefError> {
        let guard = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        query(guard.connection())
    }

    /// See [`inventory`].
    ///
    /// # Errors
    /// A SQLite failure.
    pub fn inventory(&self) -> Result<Vec<SecretInventoryEntry>, SecretRefError> {
        self.with_connection(|connection| inventory(connection).map_err(SecretRefError::from))
    }

    /// See [`referenced_by`].
    ///
    /// # Errors
    /// A SQLite failure.
    pub fn referenced_by(&self, name: &SecretName) -> Result<Vec<SecretReference>, SecretRefError> {
        self.with_connection(|connection| {
            referenced_by(connection, name).map_err(SecretRefError::from)
        })
    }

    /// See [`orphaned`].
    ///
    /// # Errors
    /// A SQLite failure.
    pub fn orphaned(&self) -> Result<Vec<SecretInventoryEntry>, SecretRefError> {
        self.with_connection(|connection| orphaned(connection).map_err(SecretRefError::from))
    }

    /// See [`missing_keychain`].
    ///
    /// # Errors
    /// A SQLite failure, an invalid stored Keychain identity or a credential
    /// store failure.
    pub fn missing_keychain(
        &self,
        store: &dyn SecretStore,
    ) -> Result<Vec<SecretInventoryEntry>, SecretRefError> {
        self.with_connection(|connection| missing_keychain(connection, store))
    }

    /// Reads the Keychain value behind a stored reference.
    ///
    /// A reference whose Keychain item is gone fails with
    /// [`SecretsError::MissingSecret`]; an empty environment value is never
    /// substituted for it.
    ///
    /// # Errors
    /// [`SecretsError::MissingSecret`] when there is no Keychain item,
    /// [`SecretsError::NotFound`] when there is no reference row, or a
    /// credential-store failure.
    pub fn resolve(
        &self,
        name: &SecretName,
        store: &dyn SecretStore,
    ) -> Result<SecretString, SecretsError> {
        let id = self.keychain_id(name)?;
        store.read(&id).map_err(|error| match error {
            SecretsError::NotFound { .. } => SecretsError::MissingSecret {
                name: name.as_str().to_owned(),
            },
            other => other,
        })
    }
}

impl SecretRefs for SecretRefStore {
    fn keychain_id(&self, name: &SecretName) -> Result<KeychainId, SecretsError> {
        self.with_connection(|connection| crate::secret_refs::keychain_id(connection, name))
            .map_err(reference_failure)
    }

    fn referencing(&self, name: &SecretName) -> Result<Vec<SecretReference>, SecretsError> {
        self.with_connection(|connection| {
            referenced_by(connection, name).map_err(SecretRefError::from)
        })
        .map_err(reference_failure)
    }

    fn remove(&self, name: &SecretName) -> Result<(), SecretsError> {
        let removed = self
            .with_connection(|connection| {
                delete_reference(connection, name).map_err(SecretRefError::from)
            })
            .map_err(reference_failure)?;
        if removed {
            Ok(())
        } else {
            Err(SecretsError::NotFound {
                service: KEYCHAIN_SERVICE.to_owned(),
                account: name.as_str().to_owned(),
            })
        }
    }
}

/// Collapses a repository failure into the error the [`SecretRefs`] seam allows.
fn reference_failure(error: SecretRefError) -> SecretsError {
    match error {
        SecretRefError::Secrets(error) => error,
        SecretRefError::Db(error) => SecretsError::ReferenceStore {
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        for statement in STATEMENTS {
            connection
                .prepare(statement)
                .unwrap_or_else(|error| panic!("statement does not compile: {error}\n{statement}"));
        }
    }

    #[test]
    fn module_changes_no_schema() {
        for statement in STATEMENTS {
            let lowered = statement.to_ascii_lowercase();
            for keyword in ["create ", "alter ", "drop "] {
                assert!(
                    !lowered.contains(keyword),
                    "statement must not change the schema: {statement}"
                );
            }
        }
    }
}
