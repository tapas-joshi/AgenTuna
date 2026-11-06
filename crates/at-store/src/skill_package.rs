//! Canonical skill folder read and write: multi-file packages under `skills/<slug>/`.
//!
//! A skill package is a directory holding `SKILL.md` plus auxiliary files (`scripts/`,
//! `references/`, `assets/`). This module reads and writes the whole tree: relative paths
//! and executable bits are preserved, `..` and absolute paths are rejected, symlinks that
//! escape the package are refused, and the write is committed atomically through a temporary
//! sibling directory plus rename.
//!
//! Limits: at most [`SKILL_PACKAGE_MAX_FILES`] files and [`SKILL_PACKAGE_MAX_BYTES`] total
//! content per skill. Overwriting an existing folder goes through the TF-005 refuse-to-
//! overwrite guard: an AgenTuna-owned package is backed up first, a foreign folder fails
//! with the absolute path and writes nothing.
//!
//! The write stages the complete tree in `skills/.<slug>.agentuna-tmp-<pid>-<n>/`, then
//! renames it into place. When the target already exists the old folder is first moved
//! aside to `skills/.<slug>.agentuna-old-<pid>/` so the install rename never exposes a
//! missing path; the aside folder is removed after the new tree is in place. A crash
//! before the final rename leaves the staging folder behind but never a partial
//! `skills/<slug>/`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use at_fs::OverwriteGuard;
use at_model::skill::SKILL_FILE_NAME;

use crate::layout::{Slug, StoreLayout};
use crate::manifest::{ContentHash, HashError, hash_skill_folder};

/// Largest number of files one skill package may hold.
pub const SKILL_PACKAGE_MAX_FILES: usize = 200;

/// Largest total content one skill package may hold (10 MiB).
pub const SKILL_PACKAGE_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Largest single file inside a skill package.
pub const SKILL_PACKAGE_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Largest relative path inside a skill package, in bytes.
pub const SKILL_PACKAGE_MAX_PATH_BYTES: usize = 1024;

/// Largest directory nesting inside a skill package.
pub const SKILL_PACKAGE_MAX_DEPTH: usize = 32;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A file inside a skill package: bytes plus the permission bits to restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFile {
    /// File content.
    pub bytes: Vec<u8>,
    /// Permission bits (e.g. `0o755` for an executable script).
    pub mode: u32,
}

impl PackageFile {
    /// A file with the given permission bits.
    #[must_use]
    pub fn new(bytes: Vec<u8>, mode: u32) -> Self {
        Self { bytes, mode }
    }
}

/// One skill package: `SKILL.md` plus auxiliary files, keyed by `/`-separated relative path.
///
/// Paths are validated on insert and again on write: no `..`, no absolute path, no empty
/// component, no backslash or NUL. The package must contain `SKILL.md` before it can be
/// written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillPackage {
    files: BTreeMap<String, PackageFile>,
}

/// What a successful package write did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SkillWriteOutcome {
    /// Directory the package was committed to.
    pub path: PathBuf,
    /// SHA-256 of the package tree after the write.
    pub hash: ContentHash,
    /// Whether an existing folder was replaced.
    pub replaced: bool,
    /// Backup directory holding the previous tree, when one was taken.
    pub backup_path: Option<PathBuf>,
}

/// A loaded skill package together with the hash of the tree it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkillPackage {
    /// Parsed package.
    pub package: SkillPackage,
    /// SHA-256 of the folder tree.
    pub hash: ContentHash,
}

impl SkillPackage {
    /// An empty package; `SKILL.md` must be added before [`Self::write`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the package holds no files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Number of files in the package.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Every relative path in the package, sorted.
    pub fn paths(&self) -> impl Iterator<Item = &str> + '_ {
        self.files.keys().map(String::as_str)
    }

    /// The file at `path`, if present.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&PackageFile> {
        self.files.get(path)
    }

    /// Inserts or replaces a file. `path` is `/`-separated and relative to the package root.
    ///
    /// # Errors
    /// [`SkillPackageError::InvalidPath`] for `..`, absolute paths, empty components,
    /// backslashes, NULs or over-long paths.
    pub fn insert(
        &mut self,
        path: impl Into<String>,
        bytes: Vec<u8>,
        mode: u32,
    ) -> Result<(), SkillPackageError> {
        let path = path.into();
        validate_relative_path(&path)?;
        self.files
            .insert(path, PackageFile::new(bytes, mode & 0o7777));
        Ok(())
    }

    /// Removes a file by relative path.
    pub fn remove(&mut self, path: &str) -> Option<PackageFile> {
        self.files.remove(path)
    }

    /// Reads a skill package from `dir` on disk.
    ///
    /// The walk never follows a symlink; a symlink inside the tree is an error. Files are
    /// bounded by [`SKILL_PACKAGE_MAX_FILES`], [`SKILL_PACKAGE_MAX_BYTES`] and
    /// [`SKILL_PACKAGE_MAX_FILE_BYTES`].
    ///
    /// # Errors
    /// [`SkillPackageError::Read`], [`SkillPackageError::NotADirectory`],
    /// [`SkillPackageError::Symlink`], [`SkillPackageError::TooManyFiles`],
    /// [`SkillPackageError::TooLarge`], [`SkillPackageError::FileTooLarge`],
    /// [`SkillPackageError::TooDeep`], [`SkillPackageError::InvalidPath`],
    /// [`SkillPackageError::NonUtf8Name`], [`SkillPackageError::Unsupported`],
    /// [`SkillPackageError::Changed`], [`SkillPackageError::MissingSkillDoc`] or
    /// [`SkillPackageError::Hash`].
    pub fn read(dir: &Path) -> Result<LoadedSkillPackage, SkillPackageError> {
        let meta = fs::symlink_metadata(dir).map_err(|source| SkillPackageError::Read {
            path: dir.to_path_buf(),
            source,
        })?;
        if meta.file_type().is_symlink() {
            return Err(SkillPackageError::Symlink {
                path: dir.to_path_buf(),
            });
        }
        if !meta.is_dir() {
            return Err(SkillPackageError::NotADirectory {
                path: dir.to_path_buf(),
            });
        }
        let mut package = SkillPackage::new();
        let mut total: u64 = 0;
        read_tree(dir, dir, &mut package, &mut total, 0)?;
        if !package.files.contains_key(SKILL_FILE_NAME) {
            return Err(SkillPackageError::MissingSkillDoc {
                path: dir.join(SKILL_FILE_NAME),
            });
        }
        let hash = hash_skill_folder(dir).map_err(|source| SkillPackageError::Hash {
            path: dir.to_path_buf(),
            source,
        })?;
        Ok(LoadedSkillPackage { package, hash })
    }

    /// Validates the package and writes it to `<store>/skills/<slug>/` atomically.
    ///
    /// The write stages the whole tree in a temporary sibling directory, then renames it
    /// into place. An existing folder is moved aside first so the install rename never
    /// exposes a missing path; the aside folder is removed after the new tree is in place.
    /// `guard` is the TF-005 refuse-to-overwrite guard: when supplied it is checked before
    /// staging and again before the install rename, and a foreign folder fails with
    /// [`SkillPackageError::RefusedOverwrite`] naming the absolute path.
    ///
    /// `backup_dir` is the operation folder supplied by the caller; when the target already
    /// exists its tree is copied there before the swap so a backup entry exists for every
    /// overwrite.
    ///
    /// # Errors
    /// [`SkillPackageError::MissingSkillDoc`], [`SkillPackageError::TooManyFiles`],
    /// [`SkillPackageError::TooLarge`], [`SkillPackageError::FileTooLarge`],
    /// [`SkillPackageError::InvalidPath`], [`SkillPackageError::RefusedOverwrite`],
    /// [`SkillPackageError::Write`], [`SkillPackageError::Symlink`], or an I/O error while
    /// staging.
    pub fn write(
        &self,
        layout: &StoreLayout,
        slug: &Slug,
        backup_dir: Option<&Path>,
        guard: Option<&dyn OverwriteGuard>,
    ) -> Result<SkillWriteOutcome, SkillPackageError> {
        self.validate()?;
        let target = layout.skill_dir(slug);
        let skills_dir = layout.skills_dir();
        fs::create_dir_all(&skills_dir).map_err(|source| SkillPackageError::Write {
            path: skills_dir.clone(),
            source: at_fs::FsError::Io {
                path: skills_dir.to_path_buf(),
                source,
            },
        })?;

        if let Some(guard) = guard {
            guard
                .check(&target, true)
                .map_err(|source| SkillPackageError::RefusedOverwrite {
                    path: target.clone(),
                    source,
                })?;
        }

        let staging = staging_path(&skills_dir, slug.as_str());
        let _staging_guard = StagingGuard::new(staging.clone());
        fs::create_dir_all(&staging).map_err(|source| SkillPackageError::Write {
            path: staging.clone(),
            source: at_fs::FsError::Io {
                path: staging.to_path_buf(),
                source,
            },
        })?;
        write_tree(&staging, self)?;

        if let Some(guard) = guard {
            guard
                .check(&target, true)
                .map_err(|source| SkillPackageError::RefusedOverwrite {
                    path: target.clone(),
                    source,
                })?;
        }

        let target_exists = fs::symlink_metadata(&target).is_ok();
        let mut backup_path = None;
        if let (true, Some(dir)) = (target_exists, backup_dir) {
            fs::create_dir_all(dir).map_err(|source| SkillPackageError::Write {
                path: dir.to_path_buf(),
                source: at_fs::FsError::Io {
                    path: dir.to_path_buf(),
                    source,
                },
            })?;
            let blob = dir.join(format!("{}.skill-package", slug.as_str()));
            copy_tree_for_backup(&target, &blob).map_err(|source| SkillPackageError::Write {
                path: target.clone(),
                source,
            })?;
            backup_path = Some(blob);
        }

        at_fs::install_dir_tree(&staging, &target).map_err(|source| SkillPackageError::Write {
            path: target.clone(),
            source,
        })?;
        let _ = _staging_guard.disarm();

        let hash = hash_skill_folder(&target).map_err(|source| SkillPackageError::Hash {
            path: target.clone(),
            source,
        })?;
        Ok(SkillWriteOutcome {
            path: target,
            hash,
            replaced: target_exists,
            backup_path,
        })
    }

    /// Validates the package: `SKILL.md` present, file count and total size within limits.
    fn validate(&self) -> Result<(), SkillPackageError> {
        if !self.files.contains_key(SKILL_FILE_NAME) {
            return Err(SkillPackageError::MissingSkillDoc {
                path: PathBuf::from(SKILL_FILE_NAME),
            });
        }
        if self.files.len() > SKILL_PACKAGE_MAX_FILES {
            return Err(SkillPackageError::TooManyFiles {
                count: self.files.len(),
                limit: SKILL_PACKAGE_MAX_FILES,
            });
        }
        let mut total: u64 = 0;
        for (path, file) in &self.files {
            if file.bytes.len() as u64 > SKILL_PACKAGE_MAX_FILE_BYTES {
                return Err(SkillPackageError::FileTooLarge {
                    path: PathBuf::from(path),
                    size: file.bytes.len() as u64,
                    limit: SKILL_PACKAGE_MAX_FILE_BYTES,
                });
            }
            total =
                total
                    .checked_add(file.bytes.len() as u64)
                    .ok_or(SkillPackageError::TooLarge {
                        total: u64::MAX,
                        limit: SKILL_PACKAGE_MAX_BYTES,
                    })?;
            if total > SKILL_PACKAGE_MAX_BYTES {
                return Err(SkillPackageError::TooLarge {
                    total,
                    limit: SKILL_PACKAGE_MAX_BYTES,
                });
            }
        }
        Ok(())
    }
}

/// Removes the staging directory on drop unless disarmed.
struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(mut self) -> Self {
        self.armed = false;
        self
    }
}

// Staging cleanup removes a directory tree; at-fs has no directory-removal primitive.
#[allow(clippy::disallowed_methods)]
impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn staging_path(dir: &Path, slug: &str) -> PathBuf {
    let pid = std::process::id();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{slug}.agentuna-tmp-{pid}-{counter}"))
}

fn validate_relative_path(path: &str) -> Result<(), SkillPackageError> {
    if path.is_empty() {
        return Err(SkillPackageError::InvalidPath {
            path: path.to_owned(),
            reason: "path is empty".to_owned(),
        });
    }
    if path.len() > SKILL_PACKAGE_MAX_PATH_BYTES {
        return Err(SkillPackageError::InvalidPath {
            path: path.to_owned(),
            reason: format!("path exceeds {SKILL_PACKAGE_MAX_PATH_BYTES} bytes"),
        });
    }
    if path.starts_with('/') {
        return Err(SkillPackageError::InvalidPackage {
            path: PathBuf::from(path),
            reason: "absolute path".to_owned(),
        });
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(SkillPackageError::InvalidPath {
                path: path.to_owned(),
                reason: "empty component".to_owned(),
            });
        }
        if component == "." {
            return Err(SkillPackageError::InvalidPath {
                path: path.to_owned(),
                reason: "`.` component".to_owned(),
            });
        }
        if component == ".." {
            return Err(SkillPackageError::InvalidPackage {
                path: PathBuf::from(path),
                reason: "`..` component".to_owned(),
            });
        }
        if component.contains('\\') || component.contains('\0') {
            return Err(SkillPackageError::InvalidPath {
                path: path.to_owned(),
                reason: "backslash or NUL in component".to_owned(),
            });
        }
    }
    Ok(())
}

fn read_tree(
    root: &Path,
    dir: &Path,
    package: &mut SkillPackage,
    total: &mut u64,
    depth: usize,
) -> Result<(), SkillPackageError> {
    if depth > SKILL_PACKAGE_MAX_DEPTH {
        return Err(SkillPackageError::TooDeep {
            path: dir.to_path_buf(),
            limit: SKILL_PACKAGE_MAX_DEPTH,
        });
    }
    let entries = fs::read_dir(dir).map_err(|source| SkillPackageError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SkillPackageError::Read {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|source| SkillPackageError::Read {
            path: path.clone(),
            source,
        })?;
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            // A symlink that resolves outside the package root is an InvalidPackage;
            // any other symlink is still refused (we never follow them).
            if let Ok(target) = fs::read_link(&path) {
                let resolved = if target.is_absolute() {
                    target
                } else {
                    path.parent().map(|p| p.join(&target)).unwrap_or(target)
                };
                let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
                let canonical_target = fs::canonicalize(&resolved).unwrap_or(resolved);
                if !canonical_target.starts_with(&canonical_root) {
                    return Err(SkillPackageError::InvalidPackage {
                        path: path.clone(),
                        reason: "symlink escapes the skill directory".to_owned(),
                    });
                }
            }
            return Err(SkillPackageError::Symlink { path });
        }
        if file_type.is_dir() {
            read_tree(root, &path, package, total, depth + 1)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(SkillPackageError::Unsupported { path });
        }
        if package.files.len() >= SKILL_PACKAGE_MAX_FILES {
            return Err(SkillPackageError::TooManyFiles {
                count: package.files.len() + 1,
                limit: SKILL_PACKAGE_MAX_FILES,
            });
        }
        if meta.len() > SKILL_PACKAGE_MAX_FILE_BYTES {
            return Err(SkillPackageError::FileTooLarge {
                path: path.clone(),
                size: meta.len(),
                limit: SKILL_PACKAGE_MAX_FILE_BYTES,
            });
        }
        *total = total
            .checked_add(meta.len())
            .ok_or(SkillPackageError::TooLarge {
                total: u64::MAX,
                limit: SKILL_PACKAGE_MAX_BYTES,
            })?;
        if *total > SKILL_PACKAGE_MAX_BYTES {
            return Err(SkillPackageError::TooLarge {
                total: *total,
                limit: SKILL_PACKAGE_MAX_BYTES,
            });
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| SkillPackageError::InvalidPath {
                path: path.display().to_string(),
                reason: "path escapes package root".to_owned(),
            })?
            .to_str()
            .ok_or_else(|| SkillPackageError::NonUtf8Name { path: path.clone() })?
            .replace('\\', "/");
        validate_relative_path(&relative)?;
        let bytes = read_file(&path, meta.len())?;
        let mode = meta.permissions().mode() & 0o7777;
        package
            .files
            .insert(relative, PackageFile::new(bytes, mode));
    }
    Ok(())
}

fn read_file(path: &Path, expected_len: u64) -> Result<Vec<u8>, SkillPackageError> {
    let before = fs::symlink_metadata(path).map_err(|source| SkillPackageError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut file = fs::File::open(path).map_err(|source| SkillPackageError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let opened = file.metadata().map_err(|source| SkillPackageError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if opened.dev() != before.dev() || opened.ino() != before.ino() {
        return Err(SkillPackageError::Changed {
            path: path.to_path_buf(),
        });
    }
    let capacity = usize::try_from(expected_len).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    std::io::Read::by_ref(&mut file)
        .take(expected_len + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| SkillPackageError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 != expected_len {
        return Err(SkillPackageError::Changed {
            path: path.to_path_buf(),
        });
    }
    Ok(bytes)
}

// at-fs owns the atomic install/swap for staged skill package directory trees.
// Staging and backup construction remain package-specific operations.
fn write_tree(dir: &Path, package: &SkillPackage) -> Result<(), SkillPackageError> {
    for (relative, file) in &package.files {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| SkillPackageError::Write {
                path: parent.to_path_buf(),
                source: at_fs::FsError::Io {
                    path: parent.to_path_buf(),
                    source,
                },
            })?;
        }
        let mut out = fs::File::create(&path).map_err(|source| SkillPackageError::Write {
            path: path.clone(),
            source: at_fs::FsError::Io {
                path: path.to_path_buf(),
                source,
            },
        })?;
        out.write_all(&file.bytes)
            .map_err(|source| SkillPackageError::Write {
                path: path.clone(),
                source: at_fs::FsError::Io {
                    path: path.to_path_buf(),
                    source,
                },
            })?;
        out.sync_all().map_err(|source| SkillPackageError::Write {
            path: path.clone(),
            source: at_fs::FsError::Io {
                path: path.to_path_buf(),
                source,
            },
        })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(file.mode)).map_err(|source| {
            SkillPackageError::Write {
                path: path.clone(),
                source: at_fs::FsError::Io {
                    path: path.to_path_buf(),
                    source,
                },
            }
        })?;
    }
    Ok(())
}

// The package staging writer creates files inside a private temporary tree; the at-fs
// single-file pipeline cannot express this directory-tree staging operation.
#[allow(clippy::disallowed_methods)]
fn copy_tree_for_backup(from: &Path, to: &Path) -> Result<(), at_fs::FsError> {
    fn copy(src: &Path, dst: &Path) -> io::Result<()> {
        let meta = fs::symlink_metadata(src)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "symlink in skill package",
            ));
        }
        if meta.is_dir() {
            fs::create_dir_all(dst)?;
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                copy(&entry.path(), &dst.join(entry.file_name()))?;
            }
        } else {
            fs::copy(src, dst)?;
            fs::set_permissions(dst, meta.permissions())?;
        }
        Ok(())
    }
    copy(from, to).map_err(|source| at_fs::FsError::Io {
        path: to.to_path_buf(),
        source,
    })
}

/// Why a skill package could not be read, written or validated.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SkillPackageError {
    /// A file or directory could not be read.
    #[error("{}: could not read skill package: {source}", .path.display())]
    Read {
        /// Offending path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The package root is not a directory.
    #[error("{}: skill package is not a directory", .path.display())]
    NotADirectory {
        /// Offending path.
        path: PathBuf,
    },
    /// A symlink was found inside the package.
    #[error("{}: symlink inside a skill package; refusing to follow it", .path.display())]
    Symlink {
        /// Symlink path.
        path: PathBuf,
    },
    /// A directory entry is not a regular file or directory.
    #[error("{}: unsupported file type inside a skill package", .path.display())]
    Unsupported {
        /// Offending path.
        path: PathBuf,
    },
    /// A file name is not valid UTF-8.
    #[error("{}: file name is not valid UTF-8", .path.display())]
    NonUtf8Name {
        /// Offending path.
        path: PathBuf,
    },
    /// A file changed between the directory scan and the read.
    #[error("{}: skill package changed while reading it", .path.display())]
    Changed {
        /// Offending path.
        path: PathBuf,
    },
    /// The package holds more files than [`SKILL_PACKAGE_MAX_FILES`].
    #[error("skill package holds {count} files; the limit is {limit}")]
    TooManyFiles {
        /// Observed count.
        count: usize,
        /// Limit.
        limit: usize,
    },
    /// The package content exceeds [`SKILL_PACKAGE_MAX_BYTES`].
    #[error("skill package content is {total} bytes; the limit is {limit}")]
    TooLarge {
        /// Observed total.
        total: u64,
        /// Limit.
        limit: u64,
    },
    /// One file exceeds [`SKILL_PACKAGE_MAX_FILE_BYTES`].
    #[error("{}: skill file is {size} bytes; the limit is {limit}", .path.display())]
    FileTooLarge {
        /// Offending file.
        path: PathBuf,
        /// Observed size.
        size: u64,
        /// Limit.
        limit: u64,
    },
    /// The package nests deeper than [`SKILL_PACKAGE_MAX_DEPTH`] levels.
    #[error("{}: skill package nests deeper than {limit} levels", .path.display())]
    TooDeep {
        /// Offending directory.
        path: PathBuf,
        /// Limit.
        limit: usize,
    },
    /// `SKILL.md` is missing.
    #[error("{}: skill package is missing SKILL.md", .path.display())]
    MissingSkillDoc {
        /// Expected path.
        path: PathBuf,
    },
    /// A relative path is invalid (`..`, absolute, empty component, backslash, NUL).
    #[error("invalid skill package path `{path}`: {reason}")]
    InvalidPath {
        /// Rejected path.
        path: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The package contains a path or symlink that escapes the skill directory.
    #[error("{}: skill package path escapes the skill directory", .path.display())]
    InvalidPackage {
        /// Offending path.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The target folder is foreign; nothing was written.
    #[error("{}: refusing to overwrite a foreign skill folder", .path.display())]
    RefusedOverwrite {
        /// Absolute path of the foreign folder.
        path: PathBuf,
        /// Pipeline error.
        source: at_fs::FsError,
    },
    /// A write or staging step failed.
    #[error("{}: could not write skill package: {source}", .path.display())]
    Write {
        /// Offending path.
        path: PathBuf,
        /// Pipeline error.
        source: at_fs::FsError,
    },
    /// The package hash could not be computed.
    #[error("{}: could not hash skill package: {source}", .path.display())]
    Hash {
        /// Offending path.
        path: PathBuf,
        /// Underlying error.
        source: HashError,
    },
}

#[cfg(test)]
// Tests write fixture files directly; production code routes through at-fs or the
// package staging mechanism.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::layout::init_store;
    use at_fs::{Remedy, TargetKind};
    use at_model::Scope;

    fn layout() -> (tempfile::TempDir, StoreLayout) {
        let dir = tempfile::tempdir().expect("temp dir");
        let layout = StoreLayout::new(Scope::User, dir.path().join("store"));
        init_store(Scope::User, layout.root()).expect("init");
        (dir, layout)
    }

    fn demo_package() -> SkillPackage {
        let mut package = SkillPackage::new();
        package
            .insert(
                "SKILL.md",
                b"---\nname: demo\ndescription: test\n---\nbody\n".to_vec(),
                0o644,
            )
            .expect("SKILL.md");
        package
            .insert("scripts/x.sh", b"#!/bin/sh\necho hi\n".to_vec(), 0o755)
            .expect("script");
        package
            .insert("references/a.md", b"# Ref\n".to_vec(), 0o644)
            .expect("ref");
        package
    }

    #[test]
    fn round_trip_preserves_bytes_and_modes() {
        let (_dir, layout) = layout();
        let slug = Slug::parse("demo").expect("slug");
        let package = demo_package();

        let outcome = package.write(&layout, &slug, None, None).expect("write");
        assert!(!outcome.replaced);

        let loaded = SkillPackage::read(&outcome.path).expect("read");
        assert_eq!(
            loaded.package.get("SKILL.md").expect("SKILL.md").bytes,
            b"---\nname: demo\ndescription: test\n---\nbody\n"
        );
        assert_eq!(
            loaded.package.get("scripts/x.sh").expect("script").bytes,
            b"#!/bin/sh\necho hi\n"
        );
        assert_eq!(
            loaded.package.get("references/a.md").expect("ref").bytes,
            b"# Ref\n"
        );
        assert_eq!(
            loaded.package.get("scripts/x.sh").expect("script").mode & 0o777,
            0o755
        );
        assert_eq!(
            loaded.package.get("SKILL.md").expect("SKILL.md").mode & 0o777,
            0o644
        );
    }

    #[test]
    fn rejects_dotdot_and_absolute_paths() {
        let mut package = SkillPackage::new();
        let err = package.insert("../escape", vec![], 0o644).expect_err("..");
        assert!(matches!(err, SkillPackageError::InvalidPackage { .. }));
        let err = package.insert("/abs/path", vec![], 0o644).expect_err("abs");
        assert!(matches!(err, SkillPackageError::InvalidPackage { .. }));
        let err = package
            .insert("a/../b", vec![], 0o644)
            .expect_err("nested ..");
        assert!(matches!(err, SkillPackageError::InvalidPackage { .. }));
        assert!(package.insert("a//b", vec![], 0o644).is_err());
        assert!(package.insert("a\\b", vec![], 0o644).is_err());
    }

    #[test]
    fn rejects_too_many_files() {
        let mut package = SkillPackage::new();
        package
            .insert("SKILL.md", b"x".to_vec(), 0o644)
            .expect("SKILL.md");
        for i in 0..SKILL_PACKAGE_MAX_FILES {
            package
                .insert(format!("f{i}"), vec![], 0o644)
                .expect("file");
        }
        let error = package
            .write(
                &StoreLayout::new(Scope::User, "/tmp/nonexistent"),
                &Slug::parse("x").expect("slug"),
                None,
                None,
            )
            .expect_err("too many files");
        assert!(matches!(error, SkillPackageError::TooManyFiles { .. }));
    }

    #[test]
    fn rejects_too_large() {
        let mut package = SkillPackage::new();
        package
            .insert(
                "SKILL.md",
                vec![0u8; SKILL_PACKAGE_MAX_BYTES as usize + 1],
                0o644,
            )
            .expect("SKILL.md");
        let error = package
            .write(
                &StoreLayout::new(Scope::User, "/tmp/nonexistent"),
                &Slug::parse("x").expect("slug"),
                None,
                None,
            )
            .expect_err("too large");
        assert!(matches!(
            error,
            SkillPackageError::FileTooLarge { .. } | SkillPackageError::TooLarge { .. }
        ));
    }

    #[test]
    fn missing_skill_md_rejected() {
        let package = SkillPackage::new();
        let error = package
            .write(
                &StoreLayout::new(Scope::User, "/tmp/nonexistent"),
                &Slug::parse("x").expect("slug"),
                None,
                None,
            )
            .expect_err("missing SKILL.md");
        assert!(matches!(error, SkillPackageError::MissingSkillDoc { .. }));
    }

    #[test]
    fn symlink_inside_package_rejected_on_read() {
        let dir = tempfile::tempdir().expect("temp dir");
        let skill = dir.path().join("skill");
        fs::create_dir_all(&skill).expect("skill dir");
        fs::write(skill.join("SKILL.md"), b"---\nname: x\n---\n").expect("SKILL.md");
        std::os::unix::fs::symlink("/etc/passwd", skill.join("link")).expect("symlink");
        let error = SkillPackage::read(&skill).expect_err("symlink rejected");
        assert!(matches!(error, SkillPackageError::InvalidPackage { .. }));
    }

    #[test]
    fn overwrite_foreign_folder_refused() {
        let (_dir, layout) = layout();
        let slug = Slug::parse("demo").expect("slug");
        let target = layout.skill_dir(&slug);
        fs::create_dir_all(&target).expect("target dir");
        fs::write(target.join("SKILL.md"), b"foreign\n").expect("foreign file");

        let package = demo_package();

        #[derive(Debug)]
        struct Refuse;
        impl OverwriteGuard for Refuse {
            fn check(&self, path: &Path, _allow_empty_dir: bool) -> Result<(), at_fs::FsError> {
                Err(at_fs::FsError::RefusedOverwrite {
                    path: path.to_path_buf(),
                    kind: TargetKind::Dir,
                    existing_hash: None,
                    remedies: vec![Remedy::Cancel],
                })
            }
        }
        let error = package
            .write(&layout, &slug, None, Some(&Refuse))
            .expect_err("foreign folder refused");
        assert!(matches!(error, SkillPackageError::RefusedOverwrite { .. }));
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).expect("read"),
            "foreign\n"
        );
    }

    #[test]
    fn overwrite_owned_folder_backs_up() {
        let (_dir, layout) = layout();
        let slug = Slug::parse("demo").expect("slug");
        let package = demo_package();
        let first = package
            .write(&layout, &slug, None, None)
            .expect("first write");
        assert!(!first.replaced);

        let backup_dir = _dir.path().join("backups/op1");
        fs::create_dir_all(&backup_dir).expect("backup dir");
        let second = package
            .write(&layout, &slug, Some(&backup_dir), None)
            .expect("second write");
        assert!(second.replaced);
        assert!(second.backup_path.is_some());
        // The backup holds the previous tree.
        let backup_skill = second.backup_path.expect("backup").join("SKILL.md");
        assert!(backup_skill.is_file());
    }

    #[test]
    fn crash_between_stage_and_rename_leaves_no_partial() {
        let (_dir, layout) = layout();
        let slug = Slug::parse("demo").expect("slug");
        let skills = layout.skills_dir();
        fs::create_dir_all(&skills).expect("skills dir");

        // Simulate a crash: a staging dir exists, the target does not.
        // Use a name the real writer would never pick (different pid).
        let staging = skills.join(".demo.agentuna-tmp-99999-0");
        fs::create_dir_all(staging.join("scripts")).expect("staging");
        fs::write(staging.join("SKILL.md"), b"partial").expect("partial");
        assert!(staging.exists());
        assert!(!layout.skill_dir(&slug).exists());

        // A real write after the simulated crash succeeds; the target is complete
        // and the writer's own staging dir is gone. The simulated crash residue
        // remains (that is what a real crash leaves behind).
        let package = demo_package();
        let outcome = package.write(&layout, &slug, None, None).expect("write");
        assert!(outcome.path.is_dir());
        assert!(outcome.path.join("SKILL.md").is_file());
        assert!(outcome.path.join("scripts/x.sh").is_file());
        // The simulated crash residue is still there (expected: crash leaves it).
        assert!(staging.exists());
        // But the writer's own staging dir (different name) is gone.
        for entry in fs::read_dir(&skills).expect("skills") {
            let name = entry.expect("entry").file_name();
            let name = name.to_string_lossy();
            if name == staging.file_name().expect("name").to_string_lossy() {
                continue;
            }
            assert!(!name.contains(".agentuna-tmp-"), "staging left: {name}");
            assert!(!name.contains(".agentuna-old-"), "aside left: {name}");
        }
    }
}
