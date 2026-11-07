//! First-run store initialisation folded into an already reviewed write plan.
//!
//! Every intent that writes into a scope store (an MCP server, a skill, an adopted item, a
//! migration) needs the store layout to exist first. This module is the single seam that turns
//! that requirement into part of the *same* reviewed [`ChangeSet`]: [`plan_store_init`] reads a
//! caller's planned operations, plans the missing layout entries through
//! [`StoreLayout::plan_init`], and prepends the layout operations whose destinations the caller
//! does not already write.
//!
//! Policy that belongs to more than one caller lives here too:
//!
//! * a store that is already ready is left completely alone (`Ok(None)`);
//! * the Global store is never created implicitly — an unavailable Global root is refused with
//!   [`StoreInitError::GlobalConsentRequired`], and the caller points at "Enable global writes";
//! * a layout `Mkdir` that is an ancestor of a planned file is dropped, because
//!   [`ChangeSet::validate`] refuses a parent-directory operation and `at-fs` creates the
//!   missing parents of the file operation itself.
//!
//! The merged set stays one operation, so one journal, one operation row and one Undo cover the
//! store creation and the item that needed it.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use at_fs::privilege::GlobalStoreStatus;
use at_model::{ChangeSet, FileOp, Scope};

use crate::layout::{StoreError, StoreLayout};

/// One store initialisation merged into a caller's reviewed plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStoreInit {
    /// Absolute store root the plan creates.
    pub root: PathBuf,
    /// Entries the merged plan creates, relative to the store root.
    pub created: Vec<PathBuf>,
    /// Entries that were already present, relative to the store root.
    pub existing: Vec<PathBuf>,
    /// Foreign entries left byte-identical, relative to the store root.
    pub foreign: Vec<PathBuf>,
    /// Whether applying the merged plan creates the store root itself.
    pub creates_root: bool,
    /// Whether applying the merged plan creates `.state/` as a parent of planned state entries.
    pub creates_state_dir: bool,
    /// Initialisation operations added to the reviewed set.
    pub operations: usize,
}

/// Why the store layout could not be folded into the caller's plan.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreInitError {
    /// Creating the Global store needs the explicit "Enable global writes" consent first.
    #[error(
        "{}: creating the Global store requires enabling Global writes first ({reason})",
        .path.display()
    )]
    GlobalConsentRequired {
        /// Global store root the plan would create.
        path: PathBuf,
        /// Why the root is not writable.
        reason: String,
    },
    /// The layout itself could not be planned.
    #[error("{}: the store layout could not be planned: {source}", .path.display())]
    Plan {
        /// Store root that could not be planned.
        path: PathBuf,
        /// Underlying planner failure.
        #[source]
        source: Box<StoreError>,
    },
    /// A planned initialisation entry is not beneath the store root.
    #[error("{}: store initialisation escapes the store root", .path.display())]
    OutsideRoot {
        /// Offending entry.
        path: PathBuf,
    },
    /// The caller mixed absolute and store-relative destinations in one set.
    #[error("the reviewed plan mixes absolute and store-relative destinations")]
    MixedPathSpace,
}

impl StoreInitError {
    /// Whether the failure means the Global scope needs its explicit write consent first.
    #[must_use]
    pub const fn needs_global_consent(&self) -> bool {
        matches!(self, Self::GlobalConsentRequired { .. })
    }
}

/// Folds the missing entries of `layout` into `change_set` when the store is not initialised.
///
/// Returns `Ok(None)` when the store already has `store.json`: nothing is planned, nothing is
/// written and the caller's plan is byte-identical. Otherwise the layout operations come first,
/// so a preview lists the store creation before the item files.
///
/// # Errors
/// [`StoreInitError::GlobalConsentRequired`] for an unwritable Global root,
/// [`StoreInitError::Plan`] when the layout cannot be planned, `OutsideRoot` or
/// `MixedPathSpace` when the caller's set cannot be merged safely.
pub fn plan_store_init(
    layout: &StoreLayout,
    change_set: &mut ChangeSet,
) -> Result<Option<PlannedStoreInit>, StoreInitError> {
    let absolute = path_space(change_set)?;
    plan_store_init_in(layout, change_set, absolute)
}

/// Same as [`plan_store_init`], but the caller states whether its operations for this store use
/// absolute destinations (`absolute == true`) or paths relative to the store root.
///
/// A plan that spans stores addresses the store owning the operation relatively and every other
/// store absolutely, so one set can mix both spaces. Each store's initialisation is planned in
/// the space its own operations use.
///
/// # Errors
/// See [`plan_store_init`].
pub fn plan_store_init_in(
    layout: &StoreLayout,
    change_set: &mut ChangeSet,
    absolute: bool,
) -> Result<Option<PlannedStoreInit>, StoreInitError> {
    if layout.is_ready() {
        return Ok(None);
    }
    if layout.scope() == Scope::Global {
        let status = GlobalStoreStatus::probe(layout.root());
        if !status.is_available() {
            return Err(StoreInitError::GlobalConsentRequired {
                path: layout.root().to_path_buf(),
                reason: status.reason().map_or_else(
                    || String::from("the Global store is not writable"),
                    str::to_owned,
                ),
            });
        }
    }
    let plan = layout.plan_init().map_err(|source| StoreInitError::Plan {
        path: layout.root().to_path_buf(),
        source: Box::new(source),
    })?;
    let root = layout.root();
    let mut destinations = change_set
        .ops
        .iter()
        .filter_map(FileOp::destination)
        .filter(|path| path.is_absolute() == absolute)
        .filter(|path| !path.is_absolute() || path.starts_with(root))
        .map(|path| normalize(root, path))
        .collect::<BTreeSet<_>>();
    let mut init_ops = Vec::new();
    for op in plan.change_set.ops {
        let Some(destination) = op.destination() else {
            continue;
        };
        let absolute_destination = normalize(root, destination);
        if destinations.contains(&absolute_destination) {
            // The caller already writes this file (for example `store.json` with the item in
            // it); its operation stays and the empty bootstrap is skipped.
            continue;
        }
        if matches!(op, FileOp::Mkdir { .. })
            && destinations
                .iter()
                .any(|planned| is_strict_ancestor(&absolute_destination, planned))
        {
            // A directory the caller's file operations create as a parent. `ChangeSet` refuses
            // a parent-directory operation, and at-fs creates the missing parents itself.
            destinations.insert(absolute_destination);
            continue;
        }
        let op = rebase(op, root, absolute)?;
        destinations.insert(absolute_destination);
        init_ops.push(op);
    }
    let operations = init_ops.len();
    let mut merged = init_ops;
    merged.append(&mut change_set.ops);
    change_set.ops = merged;
    Ok(Some(PlannedStoreInit {
        root: root.to_path_buf(),
        created: plan.created,
        existing: plan.existing,
        foreign: plan.foreign,
        creates_root: plan.creates_root,
        creates_state_dir: plan.creates_state_dir,
        operations,
    }))
}

/// Whether the reviewed set addresses absolute destinations (`true`) or store-relative ones.
fn path_space(change_set: &ChangeSet) -> Result<bool, StoreInitError> {
    let mut absolute = None;
    for destination in change_set.ops.iter().filter_map(FileOp::destination) {
        match absolute {
            None => absolute = Some(destination.is_absolute()),
            Some(expected) if expected != destination.is_absolute() => {
                return Err(StoreInitError::MixedPathSpace);
            }
            Some(_) => {}
        }
    }
    Ok(absolute.unwrap_or(true))
}

/// Absolute form of `path` for destination comparisons; `root` is used for relative paths.
fn normalize(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Converts one layout operation, planned with absolute paths, into the caller's path space.
fn rebase(op: FileOp, root: &Path, absolute: bool) -> Result<FileOp, StoreInitError> {
    Ok(match op {
        FileOp::Write {
            path,
            bytes,
            expected,
            follow_symlink,
            backup,
        } => FileOp::Write {
            path: rebase_path(&path, root, absolute)?,
            bytes,
            expected,
            follow_symlink,
            backup,
        },
        FileOp::Delete { path, expected } => FileOp::Delete {
            path: rebase_path(&path, root, absolute)?,
            expected,
        },
        FileOp::Mkdir {
            path,
            mode,
            parents,
        } => FileOp::Mkdir {
            path: rebase_path(&path, root, absolute)?,
            mode,
            parents,
        },
        FileOp::Symlink {
            link_path,
            target,
            mode,
        } => FileOp::Symlink {
            link_path: rebase_path(&link_path, root, absolute)?,
            target: rebase_path(&target, root, absolute)?,
            mode,
        },
        FileOp::Copy {
            source,
            dest,
            expected,
        } => FileOp::Copy {
            source: rebase_path(&source, root, absolute)?,
            dest: rebase_path(&dest, root, absolute)?,
            expected,
        },
        FileOp::CopyTree {
            source,
            dest,
            expected,
        } => FileOp::CopyTree {
            source: rebase_path(&source, root, absolute)?,
            dest: rebase_path(&dest, root, absolute)?,
            expected,
        },
        FileOp::Move { from, to, expected } => FileOp::Move {
            from: rebase_path(&from, root, absolute)?,
            to: rebase_path(&to, root, absolute)?,
            expected,
        },
        other => other,
    })
}

/// One path in the requested space.
fn rebase_path(path: &Path, root: &Path, absolute: bool) -> Result<PathBuf, StoreInitError> {
    if absolute {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            Ok(root.join(path))
        }
    } else if let Ok(relative) = path.strip_prefix(root) {
        Ok(relative.to_path_buf())
    } else {
        Err(StoreInitError::OutsideRoot {
            path: path.to_path_buf(),
        })
    }
}

/// Whether `ancestor` is a strict component-wise prefix of `descendant`.
fn is_strict_ancestor(ancestor: &Path, descendant: &Path) -> bool {
    let mut left = ancestor.components();
    let mut right = descendant.components();
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b)) if a == b => {}
            (None, Some(_)) => return true,
            (Some(Component::Normal(_)), None) | (Some(_), None) | (None, None) => return false,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Slug;
    use at_model::{ExpectedState, OperationKind};

    fn set(ops: Vec<FileOp>) -> ChangeSet {
        ChangeSet {
            id: String::from("cs_first_run"),
            preview_id: String::from("pv_first_run"),
            operation_kind: OperationKind::parse("edit_mcp").expect("known kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: String::from("test"),
            ops,
            created_at_unix_seconds: 0,
        }
    }

    fn write(path: PathBuf) -> FileOp {
        FileOp::Write {
            path,
            bytes: b"{}".to_vec(),
            expected: ExpectedState::Absent,
            follow_symlink: false,
            backup: true,
        }
    }

    #[test]
    fn merges_the_layout_before_the_item_and_skips_written_files() {
        let temp = tempfile::TempDir::new().expect("temp");
        let root = temp.path().join("home");
        std::fs::create_dir_all(&root).expect("home");
        let layout = StoreLayout::new(Scope::User, root.join(".agentuna"));
        let mut change_set = set(vec![
            write(layout.mcp_file()),
            write(layout.manifest_path()),
        ]);
        let init = plan_store_init(&layout, &mut change_set)
            .expect("plan")
            .expect("a missing store is planned");
        assert!(init.creates_root);
        assert!(init.operations > 0);
        assert!(change_set.ops.len() > 2);
        let first = change_set.ops[0]
            .destination()
            .expect("init destination")
            .to_path_buf();
        assert!(first.starts_with(layout.root()));
        for file in [layout.mcp_file(), layout.manifest_path()] {
            assert_eq!(
                change_set
                    .ops
                    .iter()
                    .filter_map(FileOp::destination)
                    .filter(|path| **path == file)
                    .count(),
                1,
                "{} is written by the caller only",
                file.display()
            );
        }
        change_set.validate().expect("merged set validates");
    }

    #[test]
    fn ready_store_is_left_alone() {
        let temp = tempfile::TempDir::new().expect("temp");
        let layout = StoreLayout::new(Scope::User, temp.path().join("store"));
        layout.init().expect("init");
        let mut change_set = set(vec![write(layout.mcp_file())]);
        let before = change_set.clone();
        assert!(
            plan_store_init(&layout, &mut change_set)
                .expect("plan")
                .is_none()
        );
        assert_eq!(change_set, before);
    }

    #[test]
    fn relative_plans_get_relative_initialisation() {
        let temp = tempfile::TempDir::new().expect("temp");
        let layout = StoreLayout::new(Scope::User, temp.path().join("store"));
        let mut change_set = set(vec![FileOp::CopyTree {
            source: PathBuf::from("source"),
            dest: PathBuf::from("skills/demo"),
            expected: ExpectedState::Absent,
        }]);
        let init = plan_store_init(&layout, &mut change_set)
            .expect("plan")
            .expect("planned");
        assert!(init.operations > 0);
        assert!(
            change_set
                .ops
                .iter()
                .filter_map(FileOp::destination)
                .all(|path| !path.is_absolute())
        );
        change_set.validate().expect("merged set validates");
    }

    #[test]
    fn parent_mkdir_for_a_planned_file_is_dropped() {
        let temp = tempfile::TempDir::new().expect("temp");
        let layout = StoreLayout::new(Scope::User, temp.path().join("store"));
        let slug = Slug::parse("demo").expect("slug");
        let mut change_set = set(vec![write(layout.skill_dir(&slug).join("SKILL.md"))]);
        plan_store_init(&layout, &mut change_set).expect("plan");
        assert!(
            !change_set.ops.iter().any(|op| {
                matches!(op, FileOp::Mkdir { path, .. } if path == &layout.skills_dir())
            }),
            "skills/ is created as the planned file's parent"
        );
        change_set.validate().expect("merged set validates");
    }

    #[test]
    fn an_absent_global_store_is_refused_without_consent() {
        let temp = tempfile::TempDir::new().expect("temp");
        // The Global root does not exist, which is exactly the state before "Enable global
        // writes"; the plan must refuse instead of creating it.
        let layout = StoreLayout::new(Scope::Global, temp.path().join("global"));
        let mut change_set = set(vec![write(layout.mcp_file())]);
        let before = change_set.clone();
        let error = plan_store_init(&layout, &mut change_set).expect_err("refused");
        assert!(error.needs_global_consent(), "{error}");
        assert_eq!(change_set, before, "a refusal changes nothing");
        assert!(!layout.root().exists(), "a refusal writes nothing");
    }
}
