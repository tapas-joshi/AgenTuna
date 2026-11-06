//! Change sets and file operations: the pure contracts every multi-file write shares.
//!
//! A planner (at-engine, TC-009) builds a [`ChangeSet`]: a batch of [`FileOp`]s that must be
//! applied together or not at all. The journaled apply in at-fs (TF-053) replays them with
//! backups and rollback; nothing in this module performs IO.
//!
//! Validation runs at construction time. [`ChangeSet::validate`] is pure and returns
//! [`ChangeSetError::PathCollision`] before any IO when two ops write the same destination or
//! one op targets a parent directory of another. Empty sets are rejected.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorKind, ErrorReport};
use crate::scope::Scope;

/// A SHA-256 hex digest, lowercase only.
///
/// Hash values cross the wire to journals and the `at-fs` concurrency check, which both expect
/// lowercase hex. This newtype rejects anything else at construction, so a `ChangeSet` cannot
/// carry a digest the apply layer will later refuse.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash(String);

impl ContentHash {
    /// Parses a lowercase hex SHA-256 (64 characters).
    ///
    /// # Errors
    /// [`ChangeSetError::InvalidHash`] when `text` is not 64 lowercase hex characters.
    pub fn parse(text: &str) -> Result<Self, ChangeSetError> {
        if text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(text.to_owned()))
        } else {
            Err(ChangeSetError::InvalidHash {
                value: text.to_owned(),
            })
        }
    }

    /// The hex string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ContentHash {
    type Error = ChangeSetError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ContentHash> for String {
    fn from(hash: ContentHash) -> Self {
        hash.0
    }
}

/// What the apply layer expects to find at a destination before it writes.
///
/// Mirrors `at-fs::ExpectedState`; the journal stores this domain form and the apply layer
/// translates it, so at-model stays free of filesystem types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExpectedState {
    /// Nothing exists at the destination.
    Absent,
    /// A file whose SHA-256 equals `hash`.
    Hash {
        /// Expected content digest.
        hash: ContentHash,
    },
    /// A file whose SHA-256 and modification time both match.
    HashAndMtime {
        /// Expected content digest.
        hash: ContentHash,
        /// Expected modification time, seconds since the Unix epoch.
        mtime_unix_seconds: u64,
    },
}

impl ExpectedState {
    /// Builds `Absent`.
    #[must_use]
    pub const fn absent() -> Self {
        Self::Absent
    }

    /// Builds a hash-only expectation.
    ///
    /// # Errors
    /// [`ChangeSetError::InvalidHash`] for a malformed digest.
    pub fn hash(hash: impl AsRef<str>) -> Result<Self, ChangeSetError> {
        Ok(Self::Hash {
            hash: ContentHash::parse(hash.as_ref())?,
        })
    }

    /// Builds a hash-and-mtime expectation.
    ///
    /// # Errors
    /// [`ChangeSetError::InvalidHash`] for a malformed digest.
    pub fn hash_and_mtime(
        hash: impl AsRef<str>,
        mtime: SystemTime,
    ) -> Result<Self, ChangeSetError> {
        let mtime_unix_seconds = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        Ok(Self::HashAndMtime {
            hash: ContentHash::parse(hash.as_ref())?,
            mtime_unix_seconds,
        })
    }
}

/// Deployment materialisation for [`FileOp::Symlink`].
///
/// Wire names match `at_model::LinkMode` (`link`, `copy`). The `deployments.mode` column uses
/// the at-fs stored names (`symlink`, `junction`, `copy`); the mapping between the two lives in
/// at-db / at-fs, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum DeployMode {
    /// A symbolic link to the canonical store.
    Link,
    /// A recursive copy with a manifest.
    Copy,
}

/// One primitive file operation inside a [`ChangeSet`].
///
/// Serde uses an internally tagged enum (`{"op": "write", ...}`) so journals stay readable and
/// the tag names are stable. Unknown tags fail with the offending field path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FileOp {
    /// Write `bytes` to `path`, checking `expected` first.
    Write {
        /// Destination path, relative to the store or agent root the change set addresses.
        path: PathBuf,
        /// Content to write. Journalled as base64 so arbitrary bytes survive JSON.
        #[serde(with = "serde_bytes_base64")]
        bytes: Vec<u8>,
        /// What must be at `path` for the write to proceed.
        expected: ExpectedState,
        /// Whether to follow a symlink at `path`. Defaults to false; only agent-config writers
        /// may set it true.
        follow_symlink: bool,
        /// Whether to back up the replaced file. Defaults to true.
        backup: bool,
    },
    /// Delete `path`, checking `expected` first.
    Delete {
        /// Path to remove.
        path: PathBuf,
        /// What must be at `path` for the delete to proceed.
        expected: ExpectedState,
    },
    /// Create a link at `link_path` pointing to `target`.
    Symlink {
        /// Link location.
        link_path: PathBuf,
        /// Where the link points.
        target: PathBuf,
        /// Link materialisation.
        mode: DeployMode,
    },
    /// Copy `source` to `dest`, checking `expected` at `dest`.
    Copy {
        /// Source path.
        source: PathBuf,
        /// Destination path.
        dest: PathBuf,
        /// What must be at `dest`.
        expected: ExpectedState,
    },
    /// Rename `from` to `to`, checking `expected` at both ends.
    Move {
        /// Current path.
        from: PathBuf,
        /// New path.
        to: PathBuf,
        /// What must be at `from` and `to`.
        expected: ExpectedState,
    },
    /// Create a directory.
    Mkdir {
        /// Directory to create.
        path: PathBuf,
        /// Permission bits, when the platform needs them set explicitly.
        #[serde(default)]
        mode: Option<u32>,
        /// Create missing parents first.
        parents: bool,
    },
}

impl FileOp {
    /// The destination path this op writes, when it has one.
    ///
    /// `Symlink` reports `link_path`; `Copy` and `Move` report their destination; `Mkdir`
    /// reports its directory. Used by collision validation.
    #[must_use]
    pub fn destination(&self) -> Option<&Path> {
        match self {
            Self::Write { path, .. } | Self::Delete { path, .. } | Self::Mkdir { path, .. } => {
                Some(path)
            }
            Self::Symlink { link_path, .. } => Some(link_path),
            Self::Copy { dest, .. } => Some(dest),
            Self::Move { to, .. } => Some(to),
        }
    }

    /// Every path this op reads or writes, for parent-directory collision checks.
    fn touched_paths(&self) -> Vec<&Path> {
        match self {
            Self::Write { path, .. } | Self::Delete { path, .. } | Self::Mkdir { path, .. } => {
                vec![path]
            }
            Self::Symlink { link_path, .. } => vec![link_path],
            Self::Copy { source, dest, .. } => vec![source, dest],
            Self::Move { from, to, .. } => vec![from, to],
        }
    }
}

/// Why a [`ChangeSet`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ChangeSetError {
    /// The set has no operations.
    #[error("change set has no operations")]
    Empty,
    /// Two operations write the same destination, or one targets a parent directory of another.
    #[error("change set path collision at `{path}`: {reason}")]
    PathCollision {
        /// The colliding path, display form.
        path: String,
        /// Why it collides.
        reason: String,
    },
    /// A hash string is not 64 lowercase hex characters.
    #[error("invalid content hash `{value}`; expected 64 lowercase hex characters")]
    InvalidHash {
        /// The rejected value.
        value: String,
    },
    /// A path is empty or otherwise unusable as a change-set target.
    #[error("invalid change-set path `{path}`: {reason}")]
    InvalidPath {
        /// The rejected path, display form.
        path: String,
        /// What is wrong with it.
        reason: String,
    },
    /// An operation kind string is not one of the 26 known names.
    #[error("unknown operation kind `{value}`; expected one of the 26 journal kinds")]
    InvalidOperationKind {
        /// The rejected value.
        value: String,
    },
}

impl ChangeSetError {
    /// Builds the engine-facing report for this failure.
    #[must_use]
    pub fn report(&self) -> ErrorReport {
        let kind = ErrorKind::InvalidFormat;
        let mut report = ErrorReport::new(kind, self.to_string());
        if let Self::PathCollision { path, .. } | Self::InvalidPath { path, .. } = self {
            report.path = Some(PathBuf::from(path));
        }
        report
    }
}

/// What kind of user action produced a change set.
///
/// Kept as a plain string at the model layer so journals written before this type existed stay
/// readable; known names come from the operations log (`at_db::ops::OperationKind`, 26
/// variants). [`OperationKind::parse`] validates against that set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OperationKind(String);

/// The 26 operation kinds the operations log accepts, in declaration order.
///
/// Mirrors `at_db::ops::OperationKind::WIRE_NAMES`; kept here as data so at-model does not
/// depend on at-db. A drift test in at-db asserts the two lists stay identical.
pub const OPERATION_KINDS: [&str; 26] = [
    "deploy",
    "undeploy",
    "edit_skill",
    "edit_mcp",
    "edit_rule",
    "migrate",
    "adopt",
    "restore",
    "backup",
    "scan",
    "settings",
    "update",
    "drift_adopt",
    "drift_restore",
    "sync_push",
    "sync_pull",
    "remote_backup",
    "remote_restore",
    "undo",
    "delete",
    "promote",
    "move",
    "preset_apply",
    "preset_unapply",
    "lock",
    "unlock",
];

impl OperationKind {
    /// Parses a wire name.
    ///
    /// # Errors
    /// [`ChangeSetError::InvalidOperationKind`] when `text` is not one of the 26 known names.
    pub fn parse(text: &str) -> Result<Self, ChangeSetError> {
        if OPERATION_KINDS.contains(&text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(ChangeSetError::InvalidOperationKind {
                value: text.to_owned(),
            })
        }
    }

    /// The wire name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for OperationKind {
    type Error = ChangeSetError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<OperationKind> for String {
    fn from(kind: OperationKind) -> Self {
        kind.0
    }
}

/// A batch of file operations applied together, with the identity a journal needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    /// Stable id, unique per planned change set.
    pub id: String,
    /// Identity of the preview the user reviewed; apply refuses a stale preview.
    pub preview_id: String,
    /// The user action this set belongs to.
    pub operation_kind: OperationKind,
    /// Scope the change belongs to, when scoped.
    pub scope: Option<Scope>,
    /// Project the change belongs to, when project-scoped.
    pub project_id: Option<String>,
    /// One-line summary shown in history.
    pub summary: String,
    /// The operations, in apply order.
    pub ops: Vec<FileOp>,
    /// When the set was planned, seconds since the Unix epoch.
    pub created_at_unix_seconds: u64,
}

impl ChangeSet {
    /// Validates the set: at least one op, unique destinations, no parent/child collisions.
    ///
    /// Pure; performs no IO. Returns [`ChangeSetError::PathCollision`] naming the first
    /// collision found.
    ///
    /// # Errors
    /// [`ChangeSetError::Empty`] when `ops` is empty; [`ChangeSetError::PathCollision`]
    /// when two destinations collide or one op targets a parent directory of another.
    pub fn validate(&self) -> Result<(), ChangeSetError> {
        if self.ops.is_empty() {
            return Err(ChangeSetError::Empty);
        }
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        for op in &self.ops {
            let Some(destination) = op.destination() else {
                continue;
            };
            if destination.as_os_str().is_empty() {
                return Err(ChangeSetError::InvalidPath {
                    path: destination.display().to_string(),
                    reason: "path is empty".to_owned(),
                });
            }
            if !seen.insert(destination.to_path_buf()) {
                return Err(ChangeSetError::PathCollision {
                    path: destination.display().to_string(),
                    reason: "two operations write the same destination".to_owned(),
                });
            }
        }
        // Parent-directory rule: no op may target a directory that is an ancestor of another
        // op's touched path. We compare every touched path of every op against every other
        // op's destination.
        let destinations: Vec<&Path> = self.ops.iter().filter_map(FileOp::destination).collect();
        for (index, op) in self.ops.iter().enumerate() {
            for touched in op.touched_paths() {
                for (other_index, other_destination) in destinations.iter().enumerate() {
                    if index == other_index {
                        continue;
                    }
                    if is_strict_ancestor(touched, other_destination) {
                        return Err(ChangeSetError::PathCollision {
                            path: touched.display().to_string(),
                            reason: format!(
                                "op {} targets a parent directory of op {}'s destination `{}`",
                                index + 1,
                                other_index + 1,
                                other_destination.display()
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Whether `ancestor` is a strict path-prefix of `descendant` (component-wise, never equal).
fn is_strict_ancestor(ancestor: &Path, descendant: &Path) -> bool {
    let mut ancestor_components = ancestor.components().peekable();
    let mut descendant_components = descendant.components().peekable();
    loop {
        match (ancestor_components.peek(), descendant_components.peek()) {
            (Some(a), Some(d)) if a == d => {
                ancestor_components.next();
                descendant_components.next();
            }
            (None, Some(_)) => return true,
            _ => return false,
        }
    }
}

/// Base64 codec for `FileOp::Write` bytes, kept private so the JSON shape stays stable.
mod serde_bytes_base64 {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialises bytes as a base64 string.
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64_encode(bytes))
    }

    /// Deserialises a base64 string into bytes.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64_decode(&text).map_err(serde::de::Error::custom)
    }

    fn base64_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
            let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[((triple >> 18) & 63) as usize] as char);
            out.push(ALPHABET[((triple >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[((triple >> 6) & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[(triple & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
        let mut table = [255_u8; 256];
        for (index, byte) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
            .iter()
            .enumerate()
        {
            table[usize::from(*byte)] = index as u8;
        }
        let bytes = text.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return Err("base64 length must be a multiple of 4".to_owned());
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks(4) {
            let mut triple = 0_u32;
            let mut padding = 0_u32;
            for (position, byte) in chunk.iter().enumerate() {
                if *byte == b'=' {
                    if position < 2 {
                        return Err("misplaced base64 padding".to_owned());
                    }
                    padding += 1;
                    continue;
                }
                if padding > 0 {
                    return Err("data after base64 padding".to_owned());
                }
                let value = table[usize::from(*byte)];
                if value == 255 {
                    return Err("invalid base64 character".to_owned());
                }
                triple |= u32::from(value) << (18 - 6 * position as u32);
            }
            out.push((triple >> 16) as u8);
            if chunk[2] != b'=' {
                out.push((triple >> 8) as u8);
            }
            if chunk[3] != b'=' {
                out.push(triple as u8);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash() -> ContentHash {
        ContentHash::parse(&"a".repeat(64)).expect("valid hash")
    }

    fn write_op(path: &str) -> FileOp {
        FileOp::Write {
            path: PathBuf::from(path),
            bytes: b"hello".to_vec(),
            expected: ExpectedState::Absent,
            follow_symlink: false,
            backup: true,
        }
    }

    fn set(ops: Vec<FileOp>) -> ChangeSet {
        ChangeSet {
            id: "cs_test".to_owned(),
            preview_id: "pv_test".to_owned(),
            operation_kind: OperationKind::parse("deploy").expect("known kind"),
            scope: None,
            project_id: None,
            summary: "test".to_owned(),
            ops,
            created_at_unix_seconds: 0,
        }
    }

    #[test]
    fn rejects_empty_set() {
        let error = set(Vec::new()).validate().expect_err("empty must fail");
        assert!(matches!(error, ChangeSetError::Empty));
    }

    #[test]
    fn rejects_duplicate_destinations() {
        let error = set(vec![write_op("a.txt"), write_op("a.txt")])
            .validate()
            .expect_err("duplicate must fail");
        assert!(matches!(error, ChangeSetError::PathCollision { .. }));
    }

    #[test]
    fn rejects_parent_directory_collision() {
        let ops = vec![
            FileOp::Mkdir {
                path: PathBuf::from("skills"),
                mode: None,
                parents: true,
            },
            write_op("skills/foo/SKILL.md"),
        ];
        let error = set(ops).validate().expect_err("parent must fail");
        assert!(matches!(error, ChangeSetError::PathCollision { .. }));
    }

    #[test]
    fn accepts_disjoint_paths() {
        let ops = vec![
            write_op("a.txt"),
            FileOp::Delete {
                path: PathBuf::from("b.txt"),
                expected: ExpectedState::Absent,
            },
        ];
        set(ops).validate().expect("disjoint paths pass");
    }

    #[test]
    fn file_op_round_trips_stable_tags() {
        let op = write_op("config.toml");
        let json = serde_json::to_string(&op).expect("serialize");
        assert!(json.contains(r#""op":"write""#), "got {json}");
        let back: FileOp = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(op, back);
    }

    #[test]
    fn unknown_variant_fails_with_field_path() {
        let json = r#"{"op":"bogus","path":"x"}"#;
        let error = serde_json::from_str::<FileOp>(json).expect_err("unknown op");
        let text = error.to_string();
        assert!(text.contains("op"), "error should name the field: {text}");
    }

    #[test]
    fn expected_state_round_trips() {
        for state in [
            ExpectedState::Absent,
            ExpectedState::hash("b".repeat(64)).expect("hash"),
            ExpectedState::hash_and_mtime(
                "c".repeat(64),
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(42),
            )
            .expect("hash_and_mtime"),
        ] {
            let json = serde_json::to_string(&state).expect("serialize");
            let back: ExpectedState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(state, back);
        }
    }

    #[test]
    fn hash_rejects_bad_values() {
        assert!(ContentHash::parse("xyz").is_err());
        assert!(
            ContentHash::parse(&"A".repeat(64)).is_err(),
            "uppercase rejected"
        );
        assert!(
            ContentHash::parse(&"a".repeat(63)).is_err(),
            "short rejected"
        );
        assert!(
            ContentHash::parse(&"a".repeat(65)).is_err(),
            "long rejected"
        );
        assert!(ContentHash::parse(&"a".repeat(64)).is_ok());
        let _ = hash();
    }

    #[test]
    fn operation_kind_accepts_all_26_and_rejects_unknown() {
        assert_eq!(OPERATION_KINDS.len(), 26);
        for name in OPERATION_KINDS {
            assert!(OperationKind::parse(name).is_ok(), "{name} must parse");
        }
        assert!(OperationKind::parse("not_a_kind").is_err());
    }

    #[test]
    fn changeset_serializes_stably() {
        let set = set(vec![write_op("a.txt")]);
        let json = serde_json::to_string(&set).expect("serialize");
        let back: ChangeSet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(set, back);
    }
}
