//! Read-only previews for importing skills from folders and ZIP archives.

use std::fs;
use std::path::{Path, PathBuf};

use at_model::skill::SkillDoc;
use at_store::skill_import::{SkillImportError, StagedSkillArchive, stage_skill_archive};

/// Maximum depth searched when discovering skills from an untrusted folder.
pub const IMPORT_FOLDER_MAX_DEPTH: usize = 32;
/// Maximum candidates returned from a folder import.
pub const IMPORT_FOLDER_MAX_CANDIDATES: usize = 500;

/// One discovered skill candidate. Invalid skills remain visible with their parse error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillImportCandidate {
    /// Candidate directory relative to the selected import root.
    pub subpath: PathBuf,
    /// Parsed skill name when the document is valid.
    pub name: Option<String>,
    /// Parsed description when the document is valid.
    pub description: Option<String>,
    /// Parsed document when valid.
    pub doc: Option<SkillDoc>,
    /// Parse or filesystem failure, if invalid.
    pub error: Option<String>,
}

impl SkillImportCandidate {
    /// Whether the candidate's SKILL.md parsed successfully.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.doc.is_some()
    }
}

/// A ZIP preview keeps its extracted temporary directory alive until it is dropped.
pub struct ZipSkillImport {
    staging: StagedSkillArchive,
    /// Read-only candidates discovered in the staged archive.
    pub candidates: Vec<SkillImportCandidate>,
}

impl ZipSkillImport {
    /// Returns the root of the temporary extracted tree.
    #[must_use]
    pub fn staging_root(&self) -> &Path {
        self.staging.root()
    }
}

/// A git preview owns its temporary clone until dropped.
pub struct GitSkillImport {
    repository: at_git::import::ImportedRepository,
    url: String,
    git_ref: Option<String>,
    /// Parsed candidate skills discovered in the repository.
    pub candidates: Vec<SkillImportCandidate>,
    /// True when more than 500 candidates were present.
    pub truncated: bool,
}

impl GitSkillImport {
    /// Resolved commit SHA of the previewed source.
    #[must_use]
    pub fn commit_sha(&self) -> &str {
        &self.repository.commit_sha
    }

    /// Root of the temporary clone.
    #[must_use]
    pub fn repository_root(&self) -> &Path {
        &self.repository.repository
    }
    /// Source metadata to persist with a selected candidate in the store manifest.
    #[must_use]
    pub fn source_for(&self, subpath: &Path) -> at_store::ItemSource {
        let mut source = at_store::ItemSource::new(at_store::SourceKind::Git);
        source.url = Some(self.url.clone());
        source.git_ref.clone_from(&self.git_ref);
        source.commit_sha = Some(self.repository.commit_sha.clone());
        source.subpath = Some(subpath.to_string_lossy().replace('\\', "/"));
        source
    }
    /// Records the selected imported skill's Git provenance in the `sources` table.
    ///
    /// Call this only after the selected package has been written successfully. Calling it for
    /// each selected item leaves unselected candidates untouched.
    ///
    /// # Errors
    /// Returns the typed database error if SQLite cannot write the source row.
    pub fn record_source(
        &self,
        connection: &mut rusqlite::Connection,
        item_id: &at_model::ItemId,
        subpath: &Path,
        content_hash: &str,
    ) -> Result<String, at_db::DbError> {
        let subpath = subpath.to_string_lossy();
        let mut source = at_db::sources::SourceSpec::new(item_id, at_db::sources::SourceKind::Git);
        source.url = Some(&self.url);
        source.subpath = Some(&subpath);
        source.commit_sha = Some(&self.repository.commit_sha);
        source.git_ref = self.git_ref.as_deref();
        source.content_hash = Some(content_hash);
        at_db::sources::upsert_source(connection, &source)
    }
}

/// Clones and previews a repository, discovering skills to depth six.
///
/// The clone lifetime is owned by the returned preview. Git invocation and network consent are
/// handled by `at-git`; this function does not write to the target store.
pub fn import_git(
    url: &str,
    reference: Option<&str>,
    cache_dir: &Path,
    policy: &at_model::network::NetworkPolicy,
    log: &mut at_model::network::NetworkEventLog,
) -> Result<GitSkillImport, at_git::import::ImportCloneError> {
    let repository =
        at_git::import::clone_for_import_with_process(url, reference, cache_dir, policy, log)?;
    let mut candidates = Vec::new();
    walk_git_skills(
        &repository.repository,
        &repository.repository,
        0,
        &mut candidates,
    )?;
    candidates.sort_by(|left, right| left.subpath.cmp(&right.subpath));
    let truncated = candidates.len() > IMPORT_FOLDER_MAX_CANDIDATES;
    candidates.truncate(IMPORT_FOLDER_MAX_CANDIDATES);
    Ok(GitSkillImport {
        repository,
        url: url.to_owned(),
        git_ref: reference.map(str::to_owned),
        candidates,
        truncated,
    })
}

const GIT_IMPORT_SKIPPED_DIRS: &[&str] =
    &[".git", "node_modules", "target", ".venv", ".next", "vendor"];

fn walk_git_skills(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<SkillImportCandidate>,
) -> Result<(), at_git::import::ImportCloneError> {
    if depth > 6 || out.len() > 500 {
        return Ok(());
    }
    let doc_path = dir.join("SKILL.md");
    match fs::symlink_metadata(&doc_path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            let subpath = dir.strip_prefix(root).unwrap_or(dir).to_path_buf();
            out.push(match at_store::read_skill_doc(&doc_path) {
                Ok(doc) => SkillImportCandidate {
                    subpath,
                    name: Some(doc.name.clone()),
                    description: Some(doc.description.clone()),
                    doc: Some(doc),
                    error: None,
                },
                Err(error) => SkillImportCandidate {
                    subpath,
                    name: None,
                    description: None,
                    doc: None,
                    error: Some(error.to_string()),
                },
            });
            return Ok(());
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Ok(()),
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        if out.len() > 500 {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        if GIT_IMPORT_SKIPPED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        walk_git_skills(root, &entry.path(), depth + 1, out)?;
    }
    Ok(())
}

/// Discovers skills in a folder without modifying the source.
///
/// A root `SKILL.md` is one candidate. Otherwise, nested directories are searched to depth 32;
/// symlinked directories are never followed. Directories without a `SKILL.md` are omitted.
///
/// # Errors
/// Returns a typed error when the selected root is not a readable directory.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportFolderError {
    /// The root could not be read.
    #[error("could not read import folder {path}: {source}")]
    ReadDir {
        /// Source directory that could not be read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The selected path is not a directory.
    #[error("import source is not a directory: {0}")]
    NotDirectory(PathBuf),
}

/// Inspects one folder and returns candidates, including documents with parse errors.
pub fn import_folder(path: &Path) -> Result<Vec<SkillImportCandidate>, ImportFolderError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ImportFolderError::ReadDir {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ImportFolderError::NotDirectory(path.to_path_buf()));
    }
    let mut found = Vec::new();
    walk_for_skills(path, path, 0, &mut found)?;
    found.sort_by(|left, right| left.subpath.cmp(&right.subpath));
    found.truncate(IMPORT_FOLDER_MAX_CANDIDATES);
    Ok(found)
}

/// Copies one selected skill into the target store through the canonical package writer.
///
/// `source_dir` is expected to be a candidate directory from this preview API. The source is
/// read-only; all persistent writes, backups and overwrite checks are delegated to
/// [`at_store::SkillPackage::write`].
///
/// # Errors
/// Returns package read, validation, guard, backup or write errors without modifying the source.
pub fn write_imported_skill(
    source_dir: &Path,
    target: &at_store::layout::StoreLayout,
    slug: &at_store::layout::Slug,
    backup_dir: Option<&Path>,
    guard: Option<&dyn at_fs::OverwriteGuard>,
) -> Result<at_store::SkillWriteOutcome, at_store::SkillPackageError> {
    let loaded = at_store::SkillPackage::read(source_dir)?;
    loaded.package.write(target, slug, backup_dir, guard)
}

/// Validates and stages a ZIP, then discovers its skills; the stage lives as long as the result.
pub fn import_zip(path: &Path, cache_dir: &Path) -> Result<ZipSkillImport, SkillImportError> {
    let staging = stage_skill_archive(path, cache_dir)?;
    let scan_root = unwrap_single_root(staging.root());
    let candidates = import_folder(&scan_root)
        .map_err(|error| SkillImportError::Io(std::io::Error::other(error.to_string())))?;
    Ok(ZipSkillImport {
        staging,
        candidates,
    })
}

fn unwrap_single_root(root: &Path) -> PathBuf {
    if root.join("SKILL.md").is_file() {
        return root.to_path_buf();
    }
    let Ok(entries) = fs::read_dir(root) else {
        return root.to_path_buf();
    };
    let mut only = None;
    for entry in entries {
        let Ok(entry) = entry else {
            return root.to_path_buf();
        };
        let Ok(kind) = entry.file_type() else {
            return root.to_path_buf();
        };
        if !kind.is_dir() || kind.is_symlink() || only.is_some() {
            return root.to_path_buf();
        }
        only = Some(entry.path());
    }
    only.unwrap_or_else(|| root.to_path_buf())
}

fn walk_for_skills(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<SkillImportCandidate>,
) -> Result<(), ImportFolderError> {
    if depth > IMPORT_FOLDER_MAX_DEPTH || out.len() >= IMPORT_FOLDER_MAX_CANDIDATES {
        return Ok(());
    }
    let doc_path = dir.join("SKILL.md");
    match fs::symlink_metadata(&doc_path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            let subpath = dir.strip_prefix(root).unwrap_or(dir).to_path_buf();
            let result = at_store::read_skill_doc(&doc_path);
            out.push(match result {
                Ok(doc) => SkillImportCandidate {
                    subpath,
                    name: Some(doc.name.clone()),
                    description: Some(doc.description.clone()),
                    doc: Some(doc),
                    error: None,
                },
                Err(error) => SkillImportCandidate {
                    subpath,
                    name: None,
                    description: None,
                    doc: None,
                    error: Some(error.to_string()),
                },
            });
            return Ok(());
        }
        Ok(meta) if meta.file_type().is_symlink() => {
            let subpath = dir.strip_prefix(root).unwrap_or(dir).to_path_buf();
            out.push(SkillImportCandidate {
                subpath,
                name: None,
                description: None,
                doc: None,
                error: Some(
                    "SKILL.md is a symlink; imported skill files must be regular files".to_owned(),
                ),
            });
            return Ok(());
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ImportFolderError::ReadDir {
                path: doc_path,
                source,
            });
        }
    }
    let entries = fs::read_dir(dir).map_err(|source| ImportFolderError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        if out.len() >= IMPORT_FOLDER_MAX_CANDIDATES {
            break;
        }
        let entry = entry.map_err(|source| ImportFolderError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let kind = entry
            .file_type()
            .map_err(|source| ImportFolderError::ReadDir {
                path: entry.path(),
                source,
            })?;
        if kind.is_dir() && !kind.is_symlink() {
            walk_for_skills(root, &entry.path(), depth + 1, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn skill(name: &str) -> String {
        format!("---\nname: {name}\ndescription: Does {name}\n---\n")
    }

    #[test]
    fn folder_import_lists_valid_and_invalid_nested_skills_without_writing() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("source");
        fs::create_dir_all(root.join("good")).expect("dir");
        fs::create_dir_all(root.join("bad")).expect("dir");
        fs::write(root.join("good/SKILL.md"), skill("good")).expect("good");
        fs::write(root.join("bad/SKILL.md"), "not a document").expect("bad");
        let candidates = import_folder(&root).expect("preview");
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.is_valid())
                .count(),
            1
        );
        assert!(candidates.iter().any(|candidate| candidate.error.is_some()));
        assert_eq!(fs::read_dir(&root).expect("source").count(), 2);
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("archive");
        let mut writer = zip::ZipWriter::new(file);
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("entry");
            writer.write_all(content.as_bytes()).expect("content");
        }
        writer.finish().expect("finish");
    }

    #[test]
    fn zip_import_unwraps_single_root_and_keeps_multiple_root_skills() {
        let temp = tempfile::tempdir().expect("temp");
        let cache = temp.path().join("cache");
        let wrapped = temp.path().join("wrapped.zip");
        write_zip(
            &wrapped,
            &[
                ("repo/alpha/SKILL.md", &skill("alpha")),
                ("repo/beta/SKILL.md", &skill("beta")),
            ],
        );
        let imported = import_zip(&wrapped, &cache).expect("wrapped import");
        assert_eq!(imported.candidates.len(), 2);
        assert_eq!(imported.candidates[0].subpath, PathBuf::from("alpha"));
        drop(imported);
        assert!(fs::read_dir(&cache).expect("cache").next().is_none());

        let roots = temp.path().join("roots.zip");
        write_zip(
            &roots,
            &[
                ("alpha/SKILL.md", &skill("alpha")),
                ("beta/SKILL.md", &skill("beta")),
            ],
        );
        let imported = import_zip(&roots, &cache).expect("roots import");
        assert_eq!(imported.candidates.len(), 2);
    }

    #[test]
    fn git_import_discovers_skills_and_keeps_resolved_commit() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("temp");
        let bare = temp.path().join("source.git");
        let work = temp.path().join("work");
        fs::create_dir_all(&work).expect("work");
        assert!(
            Command::new("git")
                .args(["init", "--bare"])
                .arg(&bare)
                .status()
                .expect("bare init")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&work)
                .status()
                .expect("work init")
                .success()
        );
        for name in ["one", "two", "three"] {
            fs::create_dir_all(work.join(name)).expect("skill dir");
            fs::write(work.join(name).join("SKILL.md"), skill(name)).expect("skill");
        }
        for (key, value) in [
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&work)
                    .args(["config", key, value])
                    .status()
                    .expect("config")
                    .success()
            );
        }
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(["add", "."])
                .status()
                .expect("add")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(["commit", "-m", "fixture"])
                .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
                .status()
                .expect("commit")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&work)
                .arg("push")
                .arg(&bare)
                .arg("HEAD:main")
                .status()
                .expect("push")
                .success()
        );

        let url = format!("file://{}", bare.display());
        let policy = at_model::network::NetworkPolicy::default().with_updates_opt_in(true);
        let mut log = at_model::network::NetworkEventLog::default();
        let import = import_git(
            &url,
            Some("main"),
            &temp.path().join("cache"),
            &policy,
            &mut log,
        )
        .expect("preview");
        assert_eq!(import.candidates.len(), 3);
        let paths = import
            .candidates
            .iter()
            .map(|candidate| candidate.subpath.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec!["one".to_owned(), "three".to_owned(), "two".to_owned()]
        );
        let target_layout = at_store::layout::StoreLayout::new(
            at_model::Scope::User,
            temp.path().join("target-store"),
        );
        target_layout.init().expect("target store");
        let mut database = at_db::Database::open_in_memory().expect("database");
        let mut imported_ids = Vec::new();
        for candidate in import.candidates.iter().take(2) {
            let name = candidate.name.as_deref().expect("valid candidate");
            let slug = at_store::layout::Slug::parse(name).expect("slug");
            let source_dir = import.repository_root().join(&candidate.subpath);
            let outcome = write_imported_skill(&source_dir, &target_layout, &slug, None, None)
                .expect("package write");
            let item_id = at_model::ItemId::new();
            database.connection().execute(
                "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, 'skill', 'user', ?2, ?3)",
                rusqlite::params![item_id.to_string(), name, name],
            ).expect("item row");
            import
                .record_source(
                    database.connection_mut(),
                    &item_id,
                    &candidate.subpath,
                    &outcome.hash.to_string(),
                )
                .expect("source row");
            imported_ids.push(item_id);
        }
        assert_eq!(
            fs::read_dir(target_layout.skills_dir())
                .expect("skills")
                .count(),
            2
        );
        let unselected = at_store::layout::Slug::parse("two").expect("slug");
        assert!(!target_layout.skill_dir(&unselected).exists());
        for item_id in imported_ids {
            let source = at_db::sources::source_for(database.connection(), &item_id)
                .expect("source query")
                .expect("source recorded");
            assert_eq!(source.url.as_deref(), Some(url.as_str()));
            assert_eq!(source.commit_sha.as_deref(), Some(import.commit_sha()));
            assert!(source.subpath.is_some());
        }
    }

    #[test]
    fn git_import_caps_candidate_count_at_500_and_marks_truncation() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("temp");
        let bare = temp.path().join("source.git");
        let work = temp.path().join("work");
        fs::create_dir_all(&work).expect("work");
        assert!(
            Command::new("git")
                .args(["init", "--bare"])
                .arg(&bare)
                .status()
                .expect("bare init")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&work)
                .status()
                .expect("work init")
                .success()
        );
        for index in 0..501 {
            let name = format!("skill-{index:04}");
            let directory = work.join(&name);
            fs::create_dir_all(&directory).expect("skill dir");
            fs::write(directory.join("SKILL.md"), skill(&name)).expect("skill");
        }
        for (key, value) in [
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&work)
                    .args(["config", key, value])
                    .status()
                    .expect("config")
                    .success()
            );
        }
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(["add", "."])
                .status()
                .expect("add")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(["commit", "-m", "fixture"])
                .status()
                .expect("commit")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&work)
                .arg("push")
                .arg(&bare)
                .arg("HEAD:main")
                .status()
                .expect("push")
                .success()
        );

        let url = format!("file://{}", bare.display());
        let policy = at_model::network::NetworkPolicy::default().with_updates_opt_in(true);
        let mut log = at_model::network::NetworkEventLog::default();
        let import = import_git(
            &url,
            Some("main"),
            &temp.path().join("cache"),
            &policy,
            &mut log,
        )
        .expect("preview");
        assert_eq!(import.candidates.len(), 500);
        assert!(import.truncated);
    }
}
