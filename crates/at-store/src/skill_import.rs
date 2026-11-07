//! Safe staging and discovery helpers for untrusted skill imports.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::TempDir;
use zip::ZipArchive;

/// Maximum compressed archive entries accepted for skill import.
pub const SKILL_IMPORT_MAX_ENTRIES: usize = 2_000;
/// Maximum uncompressed bytes accepted across an archive.
pub const SKILL_IMPORT_MAX_UNCOMPRESSED_BYTES: u64 = 25 * 1024 * 1024;
/// Maximum permitted per-entry decompression ratio.
pub const SKILL_IMPORT_MAX_COMPRESSION_RATIO: u64 = 100;
/// Maximum path depth in an imported archive.
pub const SKILL_IMPORT_MAX_DEPTH: usize = 32;

/// Why an archive could not be safely staged.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SkillImportError {
    /// The archive or one of its entries could not be read.
    #[error("could not read skill archive: {0}")]
    Archive(#[from] zip::result::ZipError),
    /// An archive entry uses an unsafe path or file type.
    #[error("unsafe archive entry `{entry}`: {reason}")]
    UnsafeEntry {
        /// Entry name supplied by the archive.
        entry: String,
        /// Why the entry is forbidden.
        reason: &'static str,
    },
    /// The archive exceeded an import safety bound.
    #[error("archive import limit exceeded: {limit}")]
    Limit {
        /// Human-readable bound that was exceeded.
        limit: &'static str,
    },
    /// Staging I/O failed.
    #[error("could not stage skill archive: {0}")]
    Io(#[from] io::Error),
    /// The caller cancelled the archive import.
    #[error("skill archive import was cancelled")]
    Cancelled,
}

/// An extracted archive whose temporary directory is removed on drop.
pub struct StagedSkillArchive {
    root: TempDir,
}

impl StagedSkillArchive {
    /// Root directory containing the safely extracted archive.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }
}

/// Validates and extracts an archive beneath the caller's AgenTuna cache directory.
///
/// All names and declared sizes are validated before the first entry is extracted. The staging
/// directory is automatically removed on success, error and cancellation (drop).
///
/// # Errors
/// Returns [`SkillImportError`] when an archive is malformed, unsafe, over limit or unreadable.
pub fn stage_skill_archive(
    archive_path: &Path,
    cache_dir: &Path,
) -> Result<StagedSkillArchive, SkillImportError> {
    stage_skill_archive_with_cancel(archive_path, cache_dir, &AtomicBool::new(false), |_, _| {})
}

/// Validates and extracts an archive while reporting extraction progress and observing cancel.
///
/// All names and declared sizes are validated before the first entry is extracted. The staging
/// directory is removed on success, error and cancellation when the returned value is dropped.
///
/// # Errors
/// Returns [`SkillImportError`] when the archive is malformed, unsafe, over limit, unreadable or
/// cancelled.
pub fn stage_skill_archive_with_cancel(
    archive_path: &Path,
    cache_dir: &Path,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<StagedSkillArchive, SkillImportError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(SkillImportError::Cancelled);
    }
    fs::create_dir_all(cache_dir)?;
    let archive_file = File::open(archive_path)?;
    let mut archive = ZipArchive::new(archive_file)?;
    if archive.len() > SKILL_IMPORT_MAX_ENTRIES {
        return Err(SkillImportError::Limit {
            limit: "2000 entries",
        });
    }
    let mut total = 0_u64;
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        if cancelled.load(Ordering::Acquire) {
            return Err(SkillImportError::Cancelled);
        }
        let entry = archive.by_index(index)?;
        let name = entry.name()?.into_owned();
        validate_entry_name(&name)?;
        if entry.encrypted() {
            return Err(SkillImportError::UnsafeEntry {
                entry: name,
                reason: "encrypted entries are not supported",
            });
        }
        if entry.is_symlink() {
            return Err(SkillImportError::UnsafeEntry {
                entry: name,
                reason: "symlink entries are not allowed",
            });
        }
        let mode = entry.unix_mode().unwrap_or(0);
        let file_type = mode & 0o170000;
        if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
            return Err(SkillImportError::UnsafeEntry {
                entry: name,
                reason: "links and special files are not allowed",
            });
        }
        let depth = Path::new(&name).components().count();
        if depth > SKILL_IMPORT_MAX_DEPTH {
            return Err(SkillImportError::Limit {
                limit: "32 path components",
            });
        }
        total = total
            .checked_add(entry.size())
            .ok_or(SkillImportError::Limit {
                limit: "25 MiB uncompressed",
            })?;
        if total > SKILL_IMPORT_MAX_UNCOMPRESSED_BYTES {
            return Err(SkillImportError::Limit {
                limit: "25 MiB uncompressed",
            });
        }
        let compressed = entry.compressed_size();
        if entry.size() > 0
            && (compressed == 0
                || entry.size() > compressed.saturating_mul(SKILL_IMPORT_MAX_COMPRESSION_RATIO))
        {
            return Err(SkillImportError::Limit {
                limit: "100:1 decompression ratio",
            });
        }
        entries.push((index, name, entry.size(), entry.is_dir()));
    }

    let total_entries = entries.len();
    let staging = tempfile::Builder::new()
        .prefix("skill-import-")
        .tempdir_in(cache_dir)?;
    let root = staging.path().to_path_buf();
    for (completed, (index, name, declared_size, is_dir)) in entries.into_iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Err(SkillImportError::Cancelled);
        }
        let target = safe_target(&root, &name)?;
        if is_dir {
            fs::create_dir_all(&target)?;
            progress(completed + 1, total_entries);
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut source = archive.by_index(index)?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(SkillImportError::Cancelled);
            }
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or(SkillImportError::Limit {
                    limit: "25 MiB uncompressed",
                })?;
            if copied > declared_size || copied > SKILL_IMPORT_MAX_UNCOMPRESSED_BYTES {
                return Err(SkillImportError::Limit {
                    limit: "declared entry size",
                });
            }
            destination.write_all(&buffer[..count])?;
        }
        if copied != declared_size {
            return Err(SkillImportError::UnsafeEntry {
                entry: name,
                reason: "entry size did not match its declaration",
            });
        }
        destination.flush()?;
        progress(completed + 1, total_entries);
    }
    Ok(StagedSkillArchive { root: staging })
}

fn validate_entry_name(name: &str) -> Result<(), SkillImportError> {
    let unsafe_path = || SkillImportError::UnsafeEntry {
        entry: name.to_owned(),
        reason: "absolute paths and traversal are not allowed",
    };
    if name.len() > 1_024 {
        return Err(SkillImportError::Limit {
            limit: "1024-byte entry path",
        });
    }
    if name.is_empty()
        || name.starts_with(['/', '\\'])
        || name.contains('\\')
        || name.as_bytes().contains(&0)
    {
        return Err(unsafe_path());
    }
    if name.as_bytes().get(1) == Some(&b':') {
        return Err(unsafe_path());
    }
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(unsafe_path());
    }
    Ok(())
}

fn safe_target(root: &Path, name: &str) -> Result<PathBuf, SkillImportError> {
    validate_entry_name(name)?;
    Ok(root.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn archive(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("entry");
            writer.write_all(bytes).expect("content");
        }
        writer.finish().expect("finish");
    }

    #[test]
    fn rejects_traversal_before_writing_entries() {
        let temp = tempfile::tempdir().expect("temp");
        let archive_path = temp.path().join("unsafe.zip");
        archive(
            &archive_path,
            &[("safe/SKILL.md", b"ok"), ("../evil/SKILL.md", b"bad")],
        );
        let cache = temp.path().join("cache");
        let result = stage_skill_archive(&archive_path, &cache);
        assert!(
            matches!(result, Err(SkillImportError::UnsafeEntry { ref entry, .. }) if entry == "../evil/SKILL.md")
        );
        assert!(!temp.path().join("evil").exists());
        assert!(fs::read_dir(cache).expect("cache").next().is_none());
    }

    #[test]
    fn cancellation_during_extraction_removes_the_staging_directory() {
        let temp = tempfile::tempdir().expect("temp");
        let archive_path = temp.path().join("cancelled.zip");
        archive(
            &archive_path,
            &[("one/SKILL.md", b"first"), ("two/SKILL.md", b"second")],
        );
        let cache = temp.path().join("cache");
        let cancelled = AtomicBool::new(false);
        let mut reports = Vec::new();

        let result = stage_skill_archive_with_cancel(
            &archive_path,
            &cache,
            &cancelled,
            |completed, total| {
                reports.push((completed, total));
                cancelled.store(true, Ordering::Release);
            },
        );

        assert!(matches!(result, Err(SkillImportError::Cancelled)));
        assert_eq!(reports, [(1, 2)]);
        assert!(fs::read_dir(cache).expect("cache").next().is_none());
    }

    #[test]
    fn stages_safe_archive_and_removes_it_on_drop() {
        let temp = tempfile::tempdir().expect("temp");
        let archive_path = temp.path().join("safe.zip");
        archive(&archive_path, &[("repo/alpha/SKILL.md", b"skill")]);
        let cache = temp.path().join("cache");
        let staged = stage_skill_archive(&archive_path, &cache).expect("staged");
        assert_eq!(
            fs::read(staged.root().join("repo/alpha/SKILL.md")).expect("read"),
            b"skill"
        );
        drop(staged);
        assert!(fs::read_dir(cache).expect("cache").next().is_none());
    }

    #[test]
    fn rejects_archive_entry_count_limit_before_extraction() {
        let temp = tempfile::tempdir().expect("temp");
        let archive_path = temp.path().join("many.zip");
        let file = File::create(&archive_path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        for index in 0..=SKILL_IMPORT_MAX_ENTRIES {
            writer
                .start_file(
                    format!("skill-{index}/SKILL.md"),
                    SimpleFileOptions::default(),
                )
                .expect("entry");
        }
        writer.finish().expect("finish");
        let cache = temp.path().join("cache");
        assert!(matches!(
            stage_skill_archive(&archive_path, &cache),
            Err(SkillImportError::Limit {
                limit: "2000 entries"
            })
        ));
        assert!(fs::read_dir(cache).expect("cache").next().is_none());
    }

    #[test]
    fn rejects_uncompressed_size_limit_before_extraction() {
        let temp = tempfile::tempdir().expect("temp");
        let archive_path = temp.path().join("large.zip");
        archive(
            &archive_path,
            &[(
                "skill/SKILL.md",
                &vec![b'x'; (SKILL_IMPORT_MAX_UNCOMPRESSED_BYTES + 1) as usize],
            )],
        );
        let cache = temp.path().join("cache");
        assert!(matches!(
            stage_skill_archive(&archive_path, &cache),
            Err(SkillImportError::Limit {
                limit: "25 MiB uncompressed"
            })
        ));
        assert!(fs::read_dir(cache).expect("cache").next().is_none());
    }

    #[test]
    fn rejects_symlink_archive_entries_before_extraction() {
        let temp = tempfile::tempdir().expect("temp");
        let archive_path = temp.path().join("symlink.zip");
        let file = File::create(&archive_path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("skill/SKILL.md", SimpleFileOptions::default())
            .expect("entry");
        writer.write_all(b"../../outside").expect("target");
        writer.finish().expect("finish");
        let mut bytes = fs::read(&archive_path).expect("read archive");
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory");
        bytes[central + 5] = 3;
        bytes[central + 38..central + 42].copy_from_slice(&(0o120777_u32 << 16).to_le_bytes());
        fs::write(&archive_path, bytes).expect("write archive metadata");
        let cache = temp.path().join("cache");
        assert!(matches!(
            stage_skill_archive(&archive_path, &cache),
            Err(SkillImportError::UnsafeEntry { ref entry, .. }) if entry == "skill/SKILL.md"
        ));
        assert!(fs::read_dir(cache).expect("cache").next().is_none());
    }
}
