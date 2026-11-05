//! Optimistic concurrency: what the caller expects to find at the destination.
//!
//! A hash comparison followed by a rename is not an atomic compare-and-swap against an
//! external writer. The pipeline narrows the window by re-checking immediately before the
//! rename; a write landing inside that window can still be lost, but the replaced bytes
//! are preserved in the backup.

use std::time::SystemTime;

/// What the destination must look like for a write to proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedState {
    /// Nothing exists at the destination.
    Absent,
    /// A file exists whose SHA-256 (lowercase hex) equals this value.
    Hash(String),
    /// Same as `Hash`, and the modification time must also match.
    HashAndMtime {
        /// Expected SHA-256, lowercase hex.
        hash: String,
        /// Expected modification time.
        mtime: SystemTime,
    },
}

impl ExpectedState {
    /// Human-readable form used in error messages.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Absent => "absent".to_owned(),
            Self::Hash(hash) | Self::HashAndMtime { hash, .. } => hash.clone(),
        }
    }

    /// Whether the observed state satisfies this expectation.
    pub(crate) fn matches(&self, observed: &Observed) -> bool {
        match (self, observed) {
            (Self::Absent, Observed::Absent) => true,
            (Self::Hash(want), Observed::File { hash, .. }) => want == hash,
            (
                Self::HashAndMtime {
                    hash: want,
                    mtime: want_mtime,
                },
                Observed::File { hash, mtime, .. },
            ) => want == hash && want_mtime == mtime,
            _ => false,
        }
    }
}

/// What was actually found at the destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observed {
    /// Nothing there (or an empty directory the caller allowed replacing).
    Absent,
    /// A regular file.
    File {
        /// SHA-256, lowercase hex.
        hash: String,
        /// Modification time.
        mtime: SystemTime,
    },
    /// A directory that may not be replaced.
    Directory,
}

impl Observed {
    /// Human-readable form used in error messages.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Absent => "absent".to_owned(),
            Self::File { hash, .. } => hash.clone(),
            Self::Directory => "directory".to_owned(),
        }
    }
}
