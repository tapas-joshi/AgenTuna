//! Read-only, content-bound previews for filesystem ChangeSets.

use std::fs;
use std::path::{Component, Path, PathBuf};

use at_model::{ChangeSet, ExpectedState, FileOp};
use serde::{Deserialize, Serialize};

const INLINE_DIFF_LIMIT: usize = 1024 * 1024;

/// File-level diff produced for review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DiffKind {
    /// Small UTF-8 text files use a unified diff.
    Unified {
        /// Unified diff text for review.
        text: String,
        /// Number of changed hunks.
        hunks: usize,
    },
    /// Binary content is summarized without returning its bytes.
    Binary {
        /// Size of the proposed content in bytes.
        size: u64,
    },
    /// Text over the inline limit is summarized without returning its bytes.
    TooLarge {
        /// Size of the proposed content in bytes.
        size: u64,
    },
    /// The proposed content equals the current content.
    Unchanged,
}

/// One path's proposed effect and its observed state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePreview {
    /// Path relative to the reviewed store root.
    pub path: PathBuf,
    /// Planned file operation name.
    pub action: String,
    /// Current file hash, absent when the path is absent or not a regular file.
    pub before_hash: Option<String>,
    /// Proposed file hash when the operation contains byte content.
    pub after_hash: Option<String>,
    /// Reviewable content diff.
    pub diff: DiffKind,
    /// Whether the destination is not currently present.
    pub is_new: bool,
    /// Whether an existing target would be overwritten without an expected hash.
    pub is_unmanaged: bool,
    /// Whether apply will create a pre-write backup.
    pub backup_required: bool,
    /// What the change set expects at this path.
    pub expected: Option<ExpectedState>,
}

/// Aggregate preview counts and byte sizes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewTotals {
    /// Number of file operations represented.
    pub files: usize,
    /// Sum of proposed and observed file bytes.
    pub bytes: u64,
}

/// A warning that must be visible before a preview is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewWarning {
    /// Stable machine-readable warning code.
    pub code: String,
    /// Affected relative path.
    pub path: PathBuf,
}

/// A complete review snapshot bound to a ChangeSet and the observed filesystem state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    /// Identity that must be copied into `ChangeSet::preview_id` before apply.
    pub id: String,
    /// ChangeSet being previewed.
    pub change_set_id: String,
    /// Each operation's observed and proposed effect.
    pub files: Vec<FilePreview>,
    /// Counts and byte totals.
    pub totals: PreviewTotals,
    /// Safety and stale-state warnings.
    pub warnings: Vec<PreviewWarning>,
    /// Estimated bytes written by the proposed file content.
    pub estimated_bytes: u64,
}

/// Errors while constructing or checking a read-only preview.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PreviewError {
    /// The ChangeSet does not pass pure model validation.
    #[error("invalid change set: {0}")]
    Invalid(#[from] at_model::ChangeSetError),
    /// A path is invalid or escapes the supplied root.
    #[error("unsafe preview path: {0}")]
    UnsafePath(PathBuf),
    /// A filesystem path could not be safely read.
    #[error("cannot inspect preview path {path}: {reason}")]
    Io {
        /// Path that could not be inspected.
        path: PathBuf,
        /// Filesystem error description.
        reason: String,
    },
    /// The reviewed preview identity no longer matches this ChangeSet and disk state.
    #[error("preview is stale; review the current files again")]
    ConcurrentModification,
    /// Could not serialize the preview identity input.
    #[error("cannot create preview identity")]
    Identity,
    /// Filesystem execution failed after the reviewed state matched.
    #[error("could not apply reviewed ChangeSet: {0}")]
    Apply(String),
    /// This preview implementation does not yet understand a future file operation.
    #[error("this file operation cannot be previewed")]
    UnsupportedOperation,
}

/// Builds a read-only preview for `set` under `root`.
///
/// Existing content is opened with `O_NOFOLLOW`; no file, directory, operation row or journal is
/// created. The returned identity includes the ChangeSet and observed destination hashes.
///
/// # Errors
/// Returns `Invalid`, `UnsafePath`, or `Io` for invalid plans and unreadable targets.
/// Rewrites a ChangeSet whose destinations are absolute so preview can run from one root.
///
/// The common ancestor of every destination becomes the root, and each operation path becomes
/// relative to it. Callers use this to review a plan that touches the canonical store and several
/// agent documents in one preview.
///
/// # Errors
/// Returns [`PreviewError::UnsafePath`] for a relative or non-normal destination, and
/// [`PreviewError::UnsafePath`] when the destinations share no usable ancestor.
pub fn relative_to_common_root(set: &ChangeSet) -> Result<(ChangeSet, PathBuf), PreviewError> {
    set.validate()?;
    let mut destinations = Vec::with_capacity(set.ops.len());
    for op in &set.ops {
        let path = op
            .destination()
            .ok_or_else(|| PreviewError::UnsafePath(PathBuf::new()))?;
        if !path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::CurDir | Component::ParentDir | Component::Prefix(_)
                )
            })
        {
            return Err(PreviewError::UnsafePath(path.to_path_buf()));
        }
        destinations.push(path.to_path_buf());
    }
    let mut root = destinations
        .first()
        .and_then(|path| path.parent())
        .ok_or_else(|| PreviewError::UnsafePath(PathBuf::new()))?
        .to_path_buf();
    for path in &destinations {
        while !path.starts_with(&root) {
            if !root.pop() {
                return Err(PreviewError::UnsafePath(path.clone()));
            }
        }
    }
    let mut relative = set.clone();
    for op in &mut relative.ops {
        let destination = op
            .destination()
            .ok_or_else(|| PreviewError::UnsafePath(PathBuf::new()))?
            .to_path_buf();
        let stripped = destination
            .strip_prefix(&root)
            .map_err(|_| PreviewError::UnsafePath(destination.clone()))?;
        if stripped
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(PreviewError::UnsafePath(destination.clone()));
        }
        let stripped = stripped.to_path_buf();
        match op {
            at_model::FileOp::Write { path, .. }
            | at_model::FileOp::Delete { path, .. }
            | at_model::FileOp::Mkdir { path, .. } => *path = stripped,
            at_model::FileOp::Symlink { link_path, .. } => *link_path = stripped,
            at_model::FileOp::Copy { dest, .. } => *dest = stripped,
            at_model::FileOp::Move { to, .. } => *to = stripped,
            // FileOp is non-exhaustive; a future operation without a destination cannot be
            // previewed from a common root.
            _ => return Err(PreviewError::UnsafePath(destination)),
        }
    }
    Ok((relative, root))
}

/// Reads the current filesystem state into a content-bound preview.
///
/// # Errors
/// Returns [`PreviewError`] when the set is invalid, a path escapes `root`, or a file cannot be
/// read.
pub fn preview(set: &ChangeSet, root: &Path) -> Result<Preview, PreviewError> {
    set.validate()?;
    let mut files = Vec::with_capacity(set.ops.len());
    let mut warnings = Vec::new();
    let mut totals = PreviewTotals::default();
    let mut estimated_bytes = 0u64;
    let mut observed = Vec::with_capacity(set.ops.len());
    for op in &set.ops {
        let relative = op
            .destination()
            .ok_or_else(|| PreviewError::UnsafePath(PathBuf::new()))?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(PreviewError::UnsafePath(relative.to_path_buf()));
        }
        let path = root.join(relative);
        let (before, before_hash) = read_optional(&path)?;
        let (action, proposed, expected, backup_required) = proposed(op, root)?;
        let after_hash = if action == "delete" {
            None
        } else {
            proposed.as_deref().map(at_fs::sha256_hex)
        };
        let existed = fs::symlink_metadata(&path).is_ok();
        let is_unmanaged = existed && matches!(expected, Some(ExpectedState::Absent));
        if is_unmanaged {
            warnings.push(PreviewWarning {
                code: "overwrite_refused".to_owned(),
                path: relative.to_path_buf(),
            });
        }
        let diff = match (before.as_deref(), proposed.as_deref()) {
            (Some(old), Some(new)) if old == new => DiffKind::Unchanged,
            (old, Some(new))
                if old.is_some_and(|bytes| bytes.len() > INLINE_DIFF_LIMIT)
                    || new.len() > INLINE_DIFF_LIMIT =>
            {
                DiffKind::TooLarge {
                    size: u64::try_from(new.len()).unwrap_or(u64::MAX),
                }
            }
            (old, Some(new))
                if std::str::from_utf8(new).is_err()
                    || old.is_some_and(|bytes| std::str::from_utf8(bytes).is_err()) =>
            {
                DiffKind::Binary {
                    size: u64::try_from(new.len()).unwrap_or(u64::MAX),
                }
            }
            (old, Some(new)) => {
                let text = at_fs::diff::unified(&relative.display().to_string(), old, new);
                let hunks = text.matches("@@").count() / 2;
                DiffKind::Unified { text, hunks }
            }
            (_, None) => DiffKind::Unchanged,
        };
        let before_size = before.as_ref().map_or(0, |bytes| bytes.len() as u64);
        let after_size = proposed.as_ref().map_or(0, |bytes| bytes.len() as u64);
        totals.files += 1;
        totals.bytes = totals
            .bytes
            .saturating_add(before_size)
            .saturating_add(after_size);
        estimated_bytes = estimated_bytes.saturating_add(after_size);
        observed.push((relative.to_path_buf(), before_hash.clone()));
        files.push(FilePreview {
            path: relative.to_path_buf(),
            action: action.to_owned(),
            before_hash,
            after_hash,
            diff,
            is_new: !existed,
            is_unmanaged,
            backup_required,
            expected,
        });
    }
    let mut identity_set = set.clone();
    identity_set.preview_id.clear();
    let serialized = serde_json::to_vec(&identity_set).map_err(|_| PreviewError::Identity)?;
    let mut identity_bytes = serialized;
    for (path, hash) in &observed {
        identity_bytes.extend_from_slice(path.as_os_str().to_string_lossy().as_bytes());
        if let Some(hash) = hash {
            identity_bytes.extend_from_slice(hash.as_bytes());
        }
    }
    let id = at_fs::sha256_hex(&identity_bytes);
    for (op, file) in set.ops.iter().zip(&files) {
        if !expected_matches(&file.expected, file.before_hash.as_deref()) {
            warnings.push(PreviewWarning {
                code: "changed_since_plan".to_owned(),
                path: file.path.clone(),
            });
        }
        if file.backup_required && !root.join("backups").is_dir() {
            warnings.push(PreviewWarning {
                code: "backup_will_fail".to_owned(),
                path: op.destination().unwrap_or(Path::new(".")).to_path_buf(),
            });
        }
    }
    Ok(Preview {
        id,
        change_set_id: set.id.clone(),
        files,
        totals,
        warnings,
        estimated_bytes,
    })
}

/// Applies exactly the ChangeSet whose preview identity still matches disk.
///
/// # Errors
/// Returns `ConcurrentModification` when the reviewed identity changed, otherwise propagates
/// the filesystem ChangeSet apply errors.
pub fn apply_reviewed(
    set: &ChangeSet,
    context: &mut at_fs::changeset::ApplyContext<'_>,
) -> Result<at_fs::changeset::ApplyOutcome, PreviewError> {
    let current = preview(set, context.store_root)?;
    if current.id != set.preview_id {
        return Err(PreviewError::ConcurrentModification);
    }
    at_fs::changeset::apply(set, context).map_err(|error| match error {
        at_fs::changeset::ChangeSetApplyError::Stale(_) => PreviewError::ConcurrentModification,
        other => PreviewError::Apply(other.to_string()),
    })
}

fn read_optional(path: &Path) -> Result<(Option<Vec<u8>>, Option<String>), PreviewError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((None, None)),
        Err(error) => Err(PreviewError::Io {
            path: path.to_path_buf(),
            reason: error.to_string(),
        }),
        Ok(metadata) if metadata.is_file() => {
            let bytes = at_fs::symlink::read_nofollow(path).map_err(|error| PreviewError::Io {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
            let hash = at_fs::sha256_hex(&bytes);
            Ok((Some(bytes), Some(hash)))
        }
        Ok(_) => Ok((None, None)),
    }
}

type ProposedData = (&'static str, Option<Vec<u8>>, Option<ExpectedState>, bool);

fn proposed(op: &FileOp, root: &Path) -> Result<ProposedData, PreviewError> {
    let source_bytes = |source: &Path| -> Result<Option<Vec<u8>>, PreviewError> {
        if source.is_absolute()
            || source
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(PreviewError::UnsafePath(source.to_path_buf()));
        }
        read_optional(&root.join(source)).map(|(bytes, _)| bytes)
    };
    Ok(match op {
        FileOp::Write {
            bytes,
            expected,
            backup,
            ..
        } => (
            "write",
            Some(bytes.clone()),
            Some(expected.clone()),
            *backup,
        ),
        FileOp::Delete { expected, .. } => {
            ("delete", Some(Vec::new()), Some(expected.clone()), true)
        }
        FileOp::Copy {
            source, expected, ..
        } => ("copy", source_bytes(source)?, Some(expected.clone()), false),
        FileOp::Move { from, expected, .. } => {
            ("move", source_bytes(from)?, Some(expected.clone()), false)
        }
        FileOp::Symlink { target, .. } => (
            "symlink",
            Some(target.to_string_lossy().as_bytes().to_vec()),
            Some(ExpectedState::Absent),
            false,
        ),
        FileOp::Mkdir { .. } => ("mkdir", None, Some(ExpectedState::Absent), false),
        // Replace this fallback when preview support is added for newly introduced FileOps.
        _ => return Err(PreviewError::UnsupportedOperation),
    })
}

fn expected_matches(expected: &Option<ExpectedState>, observed: Option<&str>) -> bool {
    match expected {
        Some(ExpectedState::Absent) => observed.is_none(),
        Some(ExpectedState::Hash { hash }) | Some(ExpectedState::HashAndMtime { hash, .. }) => {
            observed == Some(hash.as_str())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_model::{OperationKind, Scope};

    use tempfile::TempDir;

    fn set(path: &str, before: &[u8], after: &[u8]) -> ChangeSet {
        ChangeSet {
            id: "cs_preview_test".to_owned(),
            preview_id: String::new(),
            operation_kind: OperationKind::parse("edit_mcp").expect("kind"),
            scope: Some(Scope::Project),
            project_id: None,
            summary: "preview test".to_owned(),
            ops: vec![FileOp::Write {
                path: PathBuf::from(path),
                bytes: after.to_vec(),
                expected: if before.is_empty() {
                    ExpectedState::Absent
                } else {
                    ExpectedState::hash(at_fs::sha256_hex(before)).expect("hash")
                },
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: 0,
        }
    }

    #[test]
    fn jsonc_edit_returns_single_reviewable_hunk_without_writing() {
        let sandbox = TempDir::new().expect("tempdir");
        let path = sandbox.path().join("mcp.jsonc");
        let before = b"{\n  // keep comment\n  \"servers\": { \"alpha\": { \"enabled\": true } },\n  \"tail\": 1\n}\n";
        let after = b"{\n  // keep comment\n  \"servers\": { \"alpha\": { \"enabled\": false } },\n  \"tail\": 1\n}\n";
        fs::write(&path, before).expect("seed");
        let modified = fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        let review = preview(&set("mcp.jsonc", before, after), sandbox.path()).expect("preview");
        let DiffKind::Unified { text, hunks } = &review.files[0].diff else {
            panic!("text diff expected");
        };
        assert_eq!(*hunks, 1);
        assert!(text.contains("\"enabled\": true"));
        assert!(text.contains("\"enabled\": false"));
        assert_eq!(fs::read(&path).expect("content"), before);
        assert_eq!(
            fs::metadata(&path)
                .expect("metadata")
                .modified()
                .expect("mtime"),
            modified
        );
    }

    #[test]
    fn binary_and_large_text_previews_never_include_content() {
        let sandbox = TempDir::new().expect("tempdir");
        let binary = set("binary", b"", b"\0\xffbinary");
        let binary_preview = preview(&binary, sandbox.path()).expect("binary preview");
        assert_eq!(binary_preview.files[0].diff, DiffKind::Binary { size: 8 });
        let large = vec![b'x'; 2 * 1024 * 1024];
        let large_set = set("large.txt", b"", &large);
        let large_preview = preview(&large_set, sandbox.path()).expect("large preview");
        assert_eq!(
            large_preview.files[0].diff,
            DiffKind::TooLarge {
                size: 2 * 1024 * 1024
            }
        );
    }

    #[test]
    fn unmanaged_target_is_warned_and_proposal_remains_visible() {
        let sandbox = TempDir::new().expect("tempdir");
        fs::write(sandbox.path().join("owned-by-other"), b"external").expect("seed");
        let review = preview(
            &set("owned-by-other", b"", b"proposed content"),
            sandbox.path(),
        )
        .expect("preview");
        assert!(
            review
                .warnings
                .iter()
                .any(|warning| warning.code == "overwrite_refused")
        );
        assert!(review.files[0].diff != DiffKind::Unchanged);
    }

    #[test]
    fn changed_preview_identity_is_rejected_before_apply() {
        let sandbox = TempDir::new().expect("tempdir");
        let initial = set("file.txt", b"", b"planned");
        let reviewed = preview(&initial, sandbox.path()).expect("preview");
        fs::write(sandbox.path().join("file.txt"), b"external").expect("external edit");
        let mut stale = initial;
        stale.preview_id = reviewed.id;
        let current = preview(&stale, sandbox.path()).expect("recomputed preview");
        assert_ne!(current.id, stale.preview_id);
    }

    #[test]
    fn preview_counts_files_and_proposed_bytes() {
        let sandbox = TempDir::new().expect("tempdir");
        let mut change_set = set("first", b"", b"one");
        change_set.ops.push(FileOp::Write {
            path: PathBuf::from("second"),
            bytes: b"two".to_vec(),
            expected: ExpectedState::Absent,
            follow_symlink: false,
            backup: true,
        });
        let review = preview(&change_set, sandbox.path()).expect("preview");
        assert_eq!(review.totals.files, 2);
        assert_eq!(review.estimated_bytes, 6);
    }

    #[derive(Default)]
    struct Recorder {
        begin_count: usize,
    }

    impl at_fs::changeset::OperationRecorder for Recorder {
        fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
            self.begin_count += 1;
            Ok(format!("restore-test-{}", self.begin_count))
        }

        fn file_completed(
            &mut self,
            _: &str,
            _: &Path,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&Path>,
        ) -> Result<(), String> {
            Ok(())
        }

        fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn applying_stale_review_returns_concurrent_modification_without_history() {
        let sandbox = TempDir::new().expect("tempdir");
        let mut change_set = set("file.txt", b"", b"planned");
        change_set.preview_id = preview(&change_set, sandbox.path()).expect("preview").id;
        fs::write(sandbox.path().join("file.txt"), b"external").expect("external edit");
        let backup_root = sandbox.path().join("backups");
        let mut recorder = Recorder::default();
        let mut context = at_fs::changeset::ApplyContext {
            store_root: sandbox.path(),
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };
        assert!(matches!(
            apply_reviewed(&change_set, &mut context),
            Err(PreviewError::ConcurrentModification)
        ));
        assert_eq!(recorder.begin_count, 0);
        assert_eq!(
            fs::read(sandbox.path().join("file.txt")).expect("content"),
            b"external"
        );
    }
}
