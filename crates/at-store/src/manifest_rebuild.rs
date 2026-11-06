//! Reviewed reconstruction of `store.json` from a store's existing item files.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use at_fs::{BackupPolicy, ExpectedState};
use at_model::{ItemId, ItemKind};

use crate::layout::{SCHEMA_VERSION, Slug, StoreHeader, StoreId, StoreLayout};
use crate::manifest::{
    CanonicalJsonError, ContentHash, ManifestError, ManifestItem, StoreManifest, StorePath,
    hash_mcp_entry, hash_rule_bytes, hash_skill_folder,
};
use crate::mcp_store::{McpStore, McpStoreError};
use crate::skill::parse_skill_doc;

/// A reconstructed item and the decision made for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuiltItem {
    /// Rebuilt manifest record.
    pub item: ManifestItem,
    /// Existing item identity was matched by kind, name and content hash.
    pub matched_existing_id: bool,
    /// Existing identity was retained while its path or slug changed.
    pub renamed: bool,
}

/// A disk entry intentionally left outside the rebuilt manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRebuildEntry {
    /// Store-relative path that was not included.
    pub path: String,
    /// Why it was not included.
    pub reason: String,
}

/// Counts and details from a completed manifest rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestRebuildReport {
    /// Number of newly allocated item identities.
    pub added: usize,
    /// Number of old item identities retained.
    pub matched: usize,
    /// Number of retained identities whose slug/path changed.
    pub renamed: usize,
    /// Number of files or entries excluded from the new manifest.
    pub skipped: usize,
    /// Per-item identity and path decisions.
    pub items: Vec<RebuiltItem>,
    /// Per-entry exclusions; each remains on disk as an orphan.
    pub skipped_entries: Vec<SkippedRebuildEntry>,
}

/// A manifest candidate and the exact old-file state it was planned against.
#[derive(Debug, Clone)]
pub struct ManifestRebuildPlan {
    /// Candidate manifest reconstructed from disk.
    pub manifest: StoreManifest,
    /// Optimistic-concurrency state of the old manifest.
    expected: ExpectedState,
    /// Digest binding this confirmation to the candidate and old manifest.
    token: ContentHash,
    /// Item decisions captured during planning.
    report: ManifestRebuildReport,
}

impl ManifestRebuildPlan {
    /// Confirmation token for this exact plan.
    #[must_use]
    pub fn token(&self) -> &ContentHash {
        &self.token
    }

    /// Counts and decisions that a caller should present for review.
    #[must_use]
    pub fn report(&self) -> &ManifestRebuildReport {
        &self.report
    }
}

/// Failure while inspecting or applying a manifest rebuild.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestRebuildError {
    /// Filesystem read failed.
    #[error("cannot read `{path}`: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying filesystem error.
        source: io::Error,
    },
    /// Candidate or old manifest could not be serialized or saved.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// MCP store could not be parsed.
    #[error(transparent)]
    Mcp(#[from] McpStoreError),
    /// A server definition could not be serialized.
    #[error("cannot serialize MCP definition: {0}")]
    McpSerialize(#[from] serde_json::Error),
    /// A server definition could not be canonically hashed.
    #[error(transparent)]
    CanonicalMcp(#[from] CanonicalJsonError),
    /// Store item could not be hashed safely.
    #[error("cannot verify item at `{path}`")]
    UnsafeItem {
        /// Affected path.
        path: PathBuf,
    },
    /// The user confirmation does not match the current plan.
    #[error("manifest rebuild confirmation does not match the reviewed plan")]
    StaleConfirmation,
    /// The file changed after planning.
    #[error("manifest changed since the rebuild was planned")]
    ConcurrentModification,
}

/// Scans a store and constructs a manifest candidate without writing any files.
///
/// Skills come from `skills/*/SKILL.md`, rules from `rules/*.md`, and server records from
/// `mcp.json`. Duplicate skill display names are kept only for the lexicographically first
/// path; later paths remain on disk and are reported as skipped orphans.
///
/// # Errors
/// Returns a typed error when store content cannot be read, parsed or hashed safely.
pub fn plan_manifest_rebuild(
    layout: &StoreLayout,
) -> Result<ManifestRebuildPlan, ManifestRebuildError> {
    let manifest_path = layout.manifest_path();
    let old_bytes = match fs::read(&manifest_path) {
        Ok(bytes) => Some(bytes),
        Err(source) if source.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(ManifestRebuildError::Io {
                path: manifest_path,
                source,
            });
        }
    };
    let expected = old_bytes.as_ref().map_or(ExpectedState::Absent, |bytes| {
        ExpectedState::Hash(ContentHash::from_bytes(bytes).as_str().to_owned())
    });
    let old_manifest = old_bytes
        .as_deref()
        .and_then(|bytes| StoreManifest::parse(bytes).ok());
    let header = old_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<StoreHeader>(bytes).ok())
        .filter(|header| header.schema_version == SCHEMA_VERSION && header.scope == layout.scope());
    let mut manifest = header.as_ref().map_or_else(
        || StoreManifest::new(layout.scope(), StoreId::new()),
        StoreManifest::from_header,
    );
    let mut old_ids: BTreeMap<(ItemKind, String, ContentHash), (ItemId, String)> = BTreeMap::new();
    if let Some(old) = &old_manifest {
        for item in old.items() {
            let name = item
                .name
                .as_deref()
                .unwrap_or(item.slug.as_str())
                .to_lowercase();
            old_ids.insert(
                (item.kind, name, item.hash.clone()),
                (item.id, item.path.as_str().to_owned()),
            );
        }
    }
    if old_manifest.is_none()
        && let Some(items) = old_bytes
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
            .and_then(|value| value.get("items").cloned())
            .and_then(|value| value.as_array().cloned())
    {
        for entry in items {
            let Some(id) = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<ItemId>().ok())
            else {
                continue;
            };
            let Some(kind) = entry
                .get("kind")
                .cloned()
                .and_then(|value| serde_json::from_value::<ItemKind>(value).ok())
            else {
                continue;
            };
            let Some(hash) = entry
                .get("hash")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| ContentHash::parse(value).ok())
            else {
                continue;
            };
            let slug = entry
                .get("slug")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let name = entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(slug)
                .to_lowercase();
            let path = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            old_ids.insert((kind, name, hash), (id, path));
        }
    }

    let mut candidates = Vec::<(ItemKind, String, String, ContentHash)>::new();
    let mut skipped_entries = Vec::new();
    let mut seen_skill_names = BTreeMap::new();
    let mut skill_dirs = read_sorted(layout.skills_dir())?;
    for entry in skill_dirs.drain(..) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let folder = entry.path();
        let skill_file = folder.join("SKILL.md");
        let metadata = match fs::symlink_metadata(&folder) {
            Ok(value) => value,
            Err(source) => {
                return Err(ManifestRebuildError::Io {
                    path: folder,
                    source,
                });
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            skipped_entries.push(skipped(
                format!("skills/{name}"),
                "not a regular skill directory",
            ));
            continue;
        }
        let bytes = match fs::read(&skill_file) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                skipped_entries.push(skipped(format!("skills/{name}"), "missing SKILL.md"));
                continue;
            }
            Err(source) => {
                return Err(ManifestRebuildError::Io {
                    path: skill_file,
                    source,
                });
            }
        };
        let display_name = parse_skill_doc(&bytes)
            .ok()
            .map(|doc| doc.name)
            .unwrap_or_else(|| name.clone());
        let name_key = display_name.to_lowercase();
        if let Some(winner) = seen_skill_names.get(&name_key) {
            skipped_entries.push(skipped(
                format!("skills/{name}"),
                &format!("duplicate skill name; `skills/{winner}` wins over `skills/{name}`"),
            ));
            continue;
        }
        seen_skill_names.insert(name_key, name.clone());
        let hash = hash_skill_folder(&folder)
            .map_err(|_| ManifestRebuildError::UnsafeItem { path: folder })?;
        candidates.push((ItemKind::Skill, name, display_name, hash));
    }

    for entry in read_sorted(layout.rules_dir())? {
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| ManifestRebuildError::Io {
                path: path.clone(),
                source,
            })?
            .is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("md")
        {
            continue;
        }
        let filename = entry.file_name().to_string_lossy().into_owned();
        let slug = filename.trim_end_matches(".md").to_owned();
        let bytes = fs::read(&path).map_err(|source| ManifestRebuildError::Io {
            path: path.clone(),
            source,
        })?;
        candidates.push((ItemKind::Rule, slug.clone(), slug, hash_rule_bytes(&bytes)));
    }

    let mcp_path = layout.mcp_file();
    match fs::read(&mcp_path) {
        Ok(bytes) => {
            let mcp = McpStore::parse(&bytes)?;
            for (slug, definition) in mcp.servers {
                let value = serde_json::to_value(definition)?;
                candidates.push((
                    ItemKind::McpServer,
                    slug.clone(),
                    slug,
                    hash_mcp_entry(&value)?,
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ManifestRebuildError::Io {
                path: mcp_path,
                source,
            });
        }
    }

    let mut report = ManifestRebuildReport {
        added: 0,
        matched: 0,
        renamed: 0,
        skipped: skipped_entries.len(),
        items: Vec::new(),
        skipped_entries,
    };
    for (kind, slug_text, name, hash) in candidates {
        let slug = Slug::parse(&slug_text).map_err(|_| ManifestRebuildError::UnsafeItem {
            path: layout.root().join(&slug_text),
        })?;
        let path_text = match kind {
            ItemKind::Skill => format!("skills/{slug}"),
            ItemKind::Rule => format!("rules/{slug}.md"),
            ItemKind::McpServer => "mcp.json".to_owned(),
        };
        let item_path =
            StorePath::parse(&path_text).map_err(|_| ManifestRebuildError::UnsafeItem {
                path: layout.root().join(&path_text),
            })?;
        let key = (kind, name.to_lowercase(), hash.clone());
        let prior = old_ids.remove(&key);
        let (id, matched, renamed) = match prior {
            Some((id, old_path)) => {
                let renamed = old_path != path_text;
                report.matched += 1;
                report.renamed += usize::from(renamed);
                (id, true, renamed)
            }
            None => {
                report.added += 1;
                (ItemId::new(), false, false)
            }
        };
        let item = ManifestItem::new(id, kind, slug, item_path, hash).with_name(name);
        manifest
            .insert_item(item.clone())
            .map_err(|_| ManifestRebuildError::UnsafeItem {
                path: layout.root().join(path_text),
            })?;
        report.items.push(RebuiltItem {
            item,
            matched_existing_id: matched,
            renamed,
        });
    }
    report.skipped = report.skipped_entries.len();
    let _ = manifest.to_bytes()?;
    let mut digest = Vec::new();
    for decision in &report.items {
        digest.extend_from_slice(decision.item.kind.wire_name().as_bytes());
        digest.push(0);
        digest.extend_from_slice(decision.item.name.as_deref().unwrap_or("").as_bytes());
        digest.push(0);
        digest.extend_from_slice(decision.item.slug.as_str().as_bytes());
        digest.push(0);
        digest.extend_from_slice(decision.item.path.as_str().as_bytes());
        digest.push(0);
        digest.extend_from_slice(decision.item.hash.as_str().as_bytes());
        digest.push(0xff);
    }
    if let Some(bytes) = &old_bytes {
        digest.extend_from_slice(bytes);
    }
    let token = ContentHash::from_bytes(&digest);
    Ok(ManifestRebuildPlan {
        manifest,
        expected,
        token,
        report,
    })
}

/// Applies a reviewed manifest-rebuild plan through the guarded `at-fs` pipeline.
///
/// Any readable old manifest, including corrupt bytes, is backed up under `backups/`.
///
/// # Errors
/// Returns [`ManifestRebuildError::StaleConfirmation`] for a mismatched token and typed
/// manifest errors for stale files or failed guarded writes.
pub fn apply_manifest_rebuild(
    layout: &StoreLayout,
    plan: &ManifestRebuildPlan,
    confirmation: &ContentHash,
) -> Result<ManifestRebuildReport, ManifestRebuildError> {
    if confirmation != &plan.token {
        return Err(ManifestRebuildError::StaleConfirmation);
    }
    if plan_manifest_rebuild(layout)?.token != plan.token {
        return Err(ManifestRebuildError::ConcurrentModification);
    }
    let current = fs::read(layout.manifest_path());
    match (&plan.expected, current) {
        (ExpectedState::Absent, Err(error)) if error.kind() == io::ErrorKind::NotFound => {}
        (ExpectedState::Hash(expected), Ok(bytes))
            if ContentHash::from_bytes(&bytes).as_str() == expected => {}
        _ => return Err(ManifestRebuildError::ConcurrentModification),
    }
    plan.manifest.save(
        layout,
        plan.expected.clone(),
        BackupPolicy::OperationFolder(layout.backups_dir()),
    )?;
    Ok(plan.report.clone())
}
fn read_sorted(path: PathBuf) -> Result<Vec<fs::DirEntry>, ManifestRebuildError> {
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ManifestRebuildError::Io { path, source }),
    };
    let mut entries = entries
        .map(|entry| {
            entry.map_err(|source| ManifestRebuildError::Io {
                path: path.clone(),
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn skipped(path: String, reason: &str) -> SkippedRebuildEntry {
    SkippedRebuildEntry {
        path,
        reason: reason.to_owned(),
    }
}
