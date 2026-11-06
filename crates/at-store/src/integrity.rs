//! Read-only validation of a canonical store and its files.

use std::collections::BTreeMap;
use std::fs;

use at_model::ItemKind;

use crate::layout::StoreLayout;
use crate::skill::parse_skill_doc;
use crate::store_io::{StoreAccess, StoreIoError, StoreSnapshot, load_store};
/// Two manifest items with the same non-empty display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateItemName {
    /// Shared display name.
    pub name: String,
    /// Store-relative locations for the colliding items.
    pub paths: Vec<String>,
}

/// Integrity findings from one locked store snapshot.
#[derive(Debug, Clone)]
pub struct StoreIntegrityReport {
    /// Full reconciliation snapshot, including modified, missing and orphan files.
    pub snapshot: StoreSnapshot,
    /// Duplicate names within this store scope.
    pub duplicate_names: Vec<DuplicateItemName>,
}

impl StoreIntegrityReport {
    /// Whether any hash drift, missing item, orphan or duplicate name was observed.
    #[must_use]
    pub fn has_issues(&self) -> bool {
        self.snapshot.has_drift() || !self.duplicate_names.is_empty()
    }
}

/// Recomputes item hashes and reports missing items, unclaimed store files and duplicate names.
///
/// Hash computation and orphan discovery are supplied by [`load_store`]; this is a read-only
/// operation and never repairs or deletes files.
///
/// # Errors
/// Returns [`StoreIoError`] when the manifest or store layout cannot be read safely.
pub fn verify_store(
    layout: &StoreLayout,
    access: StoreAccess,
) -> Result<StoreIntegrityReport, StoreIoError> {
    let snapshot = load_store(layout, access)?;
    let mut names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in snapshot.manifest.items() {
        let skill_name = if item.kind == ItemKind::Skill {
            let skill_file = snapshot.layout.skill_file(&item.slug);
            fs::symlink_metadata(&skill_file)
                .ok()
                .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
                .and_then(|_| fs::read(skill_file).ok())
                .and_then(|bytes| parse_skill_doc(&bytes).ok())
                .map(|skill| skill.name)
        } else {
            None
        };
        let name = skill_name
            .as_deref()
            .or(item.name.as_deref())
            .unwrap_or(item.slug.as_str())
            .trim();
        if !name.is_empty() {
            names
                .entry(name.to_lowercase())
                .or_default()
                .push(item.path.as_str().to_owned());
        }
    }
    let duplicate_names = names
        .into_iter()
        .filter_map(|(name, paths)| (paths.len() > 1).then_some(DuplicateItemName { name, paths }))
        .collect();
    Ok(StoreIntegrityReport {
        snapshot,
        duplicate_names,
    })
}
