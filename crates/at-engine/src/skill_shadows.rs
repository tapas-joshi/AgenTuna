//! Skill shadow ordering and side-by-side payloads for scope copies.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::resolve::{ResolvedSet, Shadowed};
use at_model::Scope;
use at_model::skill::SKILL_FILE_NAME;

/// Metadata and complete skill document for one scope copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCopy {
    /// Scope containing this copy.
    pub scope: Scope,
    /// Absolute skill folder.
    pub path: PathBuf,
    /// Complete `SKILL.md` text.
    pub text: String,
    /// SHA-256 digest of all files in the folder.
    pub folder_hash: String,
    /// Modification time of `SKILL.md`.
    pub mtime: SystemTime,
    /// Deployment records associated with this copy.
    pub deployments: Vec<String>,
}

/// Comparison payload for two skill copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareCopies {
    /// First copy, nearer to effective winner.
    pub a: SkillCopy,
    /// Second copy, farther from effective winner.
    pub b: SkillCopy,
    /// Whether the complete folder hashes are equal.
    pub identical: bool,
}

/// One effective winner and its shadowed copies from nearest to farthest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillShadowChain {
    /// Winning scope.
    pub winner: Scope,
    /// Ordered copies that lost to the winner.
    pub shadowed: Vec<Scope>,
}

/// Builds ordered skill shadow groups from a resolver result.
///
/// Every winner is followed by its losing copies in nearest-to-farthest scope order.
#[must_use]
pub fn chains_from_resolved(resolved: &ResolvedSet) -> Vec<(Scope, Vec<Shadowed>)> {
    let mut groups = std::collections::BTreeMap::<(Scope, String), Vec<Shadowed>>::new();
    for shadow in &resolved.shadowed {
        if shadow.blocked.is_some() || shadow.logical_id.kind != at_model::ItemKind::Skill {
            continue;
        }
        groups
            .entry((
                shadow.shadowed_by.scope,
                shadow.definition.normalized_name(),
            ))
            .or_default()
            .push(shadow.clone());
    }
    groups
        .into_iter()
        .map(|((winner, _), mut shadows)| {
            shadows.sort_by_key(|entry| std::cmp::Reverse(entry.scope.precedence()));
            (winner, shadows)
        })
        .collect()
}

/// Builds pairwise comparison payloads for a resolved skill's winner and shadows.
///
/// # Errors
/// Returns the first unreadable copy with its path.
pub fn compare_resolved_skill(
    resolved: &ResolvedSet,
    name: &str,
    connection: &rusqlite::Connection,
) -> Result<Vec<CompareCopies>, CopyUnreadable> {
    let normalized = at_model::normalized_name(name);
    let Some(winner) = resolved.items.iter().find(|item| {
        item.logical_id.kind == at_model::ItemKind::Skill
            && item.definition.normalized_name() == normalized
    }) else {
        return Ok(Vec::new());
    };
    let mut copies = vec![SkillCopyPath {
        scope: winner.scope,
        path: winner.definition.path.clone(),
        deployments: deployments_for(connection, winner.scope, winner.definition.item_id)?,
    }];
    let mut shadows = resolved
        .shadowed
        .iter()
        .filter(|entry| entry.logical_id == winner.logical_id && entry.blocked.is_none())
        .map(|entry| {
            (
                entry.scope,
                entry.definition.path.clone(),
                entry.definition.item_id,
            )
        })
        .collect::<Vec<_>>();
    shadows.extend(
        resolved
            .disabled
            .iter()
            .filter(|entry| entry.logical_id == winner.logical_id)
            .map(|entry| {
                (
                    entry.scope,
                    entry.definition.path.clone(),
                    Some(entry.item_id),
                )
            }),
    );
    shadows.sort_by_key(|entry| std::cmp::Reverse(entry.0.precedence()));
    shadows.dedup_by(|left, right| left.0 == right.0);
    for (scope, path, item_id) in shadows {
        copies.push(SkillCopyPath {
            scope,
            path,
            deployments: deployments_for(connection, scope, item_id)?,
        });
    }
    copies
        .windows(2)
        .map(|pair| compare_copies(&pair[0], &pair[1]))
        .collect()
}

fn deployments_for(
    connection: &rusqlite::Connection,
    scope: Scope,
    item_id: Option<at_model::ItemId>,
) -> Result<Vec<String>, CopyUnreadable> {
    let Some(item_id) = item_id else {
        return Ok(Vec::new());
    };
    let mut statement = connection
        .prepare("SELECT agent_id FROM deployments WHERE item_id=?1 AND scope=?2 ORDER BY agent_id")
        .map_err(|source| CopyUnreadable {
            path: PathBuf::from("deployments"),
            source: CopyUnreadableSource::Database(source.to_string()),
        })?;
    let rows = statement
        .query_map([item_id.to_string(), scope.wire_name().to_owned()], |row| {
            row.get(0)
        })
        .map_err(|source| CopyUnreadable {
            path: PathBuf::from("deployments"),
            source: CopyUnreadableSource::Database(source.to_string()),
        })?;
    rows.collect::<Result<Vec<String>, _>>()
        .map_err(|source| CopyUnreadable {
            path: PathBuf::from("deployments"),
            source: CopyUnreadableSource::Database(source.to_string()),
        })
}

/// Builds a shadow chain for every skill row returned by the skills index.
///
/// # Errors
/// Returns the SQLite query error.
pub fn indexed_skill_shadows(
    connection: &rusqlite::Connection,
) -> Result<Vec<(String, SkillShadowChain)>, at_db::DbError> {
    let rows = at_db::skills_index::list_skills(
        connection,
        &at_db::skills_index::SkillFilter {
            include_native: false,
            ..Default::default()
        },
    )?;
    let mut groups = std::collections::BTreeMap::<(String, Scope), Vec<Scope>>::new();
    for row in rows {
        if let Some(winner) = row.shadowed_by {
            groups
                .entry((row.name, winner))
                .or_default()
                .push(row.scope);
        }
    }
    Ok(groups
        .into_iter()
        .map(|((name, winner), scopes)| (name, shadow_chain(winner, &scopes)))
        .collect())
}

/// Reads and compares two copies by their absolute directories.
pub fn compare_copies_from_paths(
    a: &SkillCopyPath,
    b: &SkillCopyPath,
) -> Result<CompareCopies, CopyUnreadable> {
    compare_copies(a, b)
}
/// Read and compare two skill folders.
///
/// # Errors
/// Returns [`CopyUnreadable`] with the offending path if any required input is unreadable.
pub fn compare_copies(
    a: &SkillCopyPath,
    b: &SkillCopyPath,
) -> Result<CompareCopies, CopyUnreadable> {
    let a = read_copy(a)?;
    let b = read_copy(b)?;
    let identical = a.folder_hash == b.folder_hash;
    Ok(CompareCopies { a, b, identical })
}

/// Filesystem location and associated indexed deployments of a skill copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCopyPath {
    /// Scope of the copy.
    pub scope: Scope,
    /// Absolute path to its folder.
    pub path: PathBuf,
    /// Deployment records to expose in the comparison.
    pub deployments: Vec<String>,
}

/// An unreadable or invalid skill copy, retaining its filesystem path.
#[derive(Debug, thiserror::Error)]
#[error("cannot read skill copy at {path}: {source}")]
pub struct CopyUnreadable {
    /// Path that could not be read.
    pub path: PathBuf,
    /// Underlying filesystem or hashing error.
    #[source]
    pub source: CopyUnreadableSource,
}

/// Underlying error encountered while reading a skill copy.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CopyUnreadableSource {
    /// Filesystem read or metadata lookup failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Folder tree could not be safely hashed.
    #[error(transparent)]
    Hash(#[from] at_store::manifest::HashError),
    /// SKILL.md was not valid UTF-8.
    #[error("SKILL.md is not valid UTF-8")]
    Utf8(#[from] std::string::FromUtf8Error),
    /// A deployment index query failed.
    #[error("deployment index query failed: {0}")]
    Database(String),
}

/// Derives a nearest-to-farthest shadow chain from resolver output.
///
/// Locked definitions are excluded because a lock blocks an override rather than creating
/// an ordinary shadow relationship. Disabled copies remain eligible for comparison.
#[must_use]
pub fn shadow_chain(winner: Scope, candidates: &[Scope]) -> SkillShadowChain {
    let mut shadowed = candidates
        .iter()
        .copied()
        .filter(|scope| *scope != winner)
        .collect::<Vec<_>>();
    shadowed.sort_by_key(|scope| std::cmp::Reverse(scope.precedence()));
    SkillShadowChain { winner, shadowed }
}

fn read_copy(copy: &SkillCopyPath) -> Result<SkillCopy, CopyUnreadable> {
    let doc_path = copy.path.join(SKILL_FILE_NAME);
    let result = (|| -> Result<SkillCopy, CopyUnreadableSource> {
        let metadata = fs::symlink_metadata(&doc_path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "SKILL.md is not a regular file",
            )
            .into());
        }
        let bytes = fs::read(&doc_path)?;
        let text = String::from_utf8(bytes)?;
        let folder_hash = at_store::manifest::hash_skill_folder(&copy.path)?.to_string();
        let mtime = metadata.modified()?;
        Ok(SkillCopy {
            scope: copy.scope,
            path: copy.path.clone(),
            text,
            folder_hash,
            mtime,
            deployments: copy.deployments.clone(),
        })
    })();
    result.map_err(|source: CopyUnreadableSource| CopyUnreadable {
        path: doc_path,
        source,
    })
}

/// Checks a folder path using lstat semantics without following its final component.
#[must_use]
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn compare_reads_documents_hashes_and_keeps_disabled_copy_comparable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        let user = temp.path().join("user");
        for path in [&project, &user] {
            fs::create_dir_all(path).expect("folder");
            fs::write(path.join("SKILL.md"), "---\nname: example\n---\n").expect("doc");
        }
        let compared = compare_copies(
            &SkillCopyPath {
                scope: Scope::Project,
                path: project,
                deployments: vec!["codex".into()],
            },
            &SkillCopyPath {
                scope: Scope::User,
                path: user,
                deployments: vec![],
            },
        )
        .expect("comparison");
        assert!(compared.identical);
        assert_eq!(compared.a.text, compared.b.text);
        assert_eq!(
            shadow_chain(Scope::Project, &[Scope::User, Scope::Global, Scope::Local]).shadowed,
            vec![Scope::Local, Scope::User, Scope::Global]
        );
    }

    #[test]
    fn missing_document_returns_pathful_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("missing");
        let error = compare_copies(
            &SkillCopyPath {
                scope: Scope::User,
                path: path.clone(),
                deployments: vec![],
            },
            &SkillCopyPath {
                scope: Scope::Project,
                path,
                deployments: vec![],
            },
        )
        .expect_err("missing");
        assert!(error.path.ends_with("SKILL.md"));
    }

    #[test]
    fn resolver_chains_order_all_shadows_nearest_to_farthest() {
        let layers = [Scope::Global, Scope::User, Scope::Project]
            .into_iter()
            .map(|scope| at_model::ScopeSnapshot {
                scope,
                definitions: vec![at_model::Definition {
                    kind: at_model::ItemKind::Skill,
                    name: "example".into(),
                    declared_id: None,
                    item_id: None,
                    path: std::path::PathBuf::from(format!("/{scope}/skills/example")),
                }],
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(crate::resolve::ResolveInput::new(&layers));
        let chains = chains_from_resolved(&resolved);
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].0, Scope::Project);
        assert_eq!(
            chains[0]
                .1
                .iter()
                .map(|shadow| shadow.scope)
                .collect::<Vec<_>>(),
            vec![Scope::User, Scope::Global]
        );
    }
}
