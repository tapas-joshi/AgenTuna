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

/// A store that can supply skills to the library picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryStore {
    /// Display label in the source picker.
    pub label: String,
    /// Store root containing `skills/`.
    pub path: PathBuf,
    /// Source scope. `None` identifies a registered project store.
    pub scope: Option<at_model::Scope>,
    /// Last-used timestamp for a registered project, if known.
    pub last_used_at: Option<String>,
}

/// Source category for a library picker row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LibrarySourceKind {
    /// A different scope store.
    Scope,
    /// A registered project store.
    Project,
}

/// One non-empty source shown by the library picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySource {
    /// Human-readable source label.
    pub label: String,
    /// Root of the source store.
    pub path: PathBuf,
    /// Source category.
    pub kind: LibrarySourceKind,
    /// Number of discoverable skills in this source.
    pub count: usize,
    /// Last-used timestamp for a registered project, if known.
    pub last_used_at: Option<String>,
}

/// A skill offered by a library source, including parse failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySkill {
    /// Source label.
    pub source_label: String,
    /// Root store that owns this candidate.
    pub source_root: PathBuf,
    /// Skill folder.
    pub path: PathBuf,
    /// Parsed skill name, when valid.
    pub name: Option<String>,
    /// Parsed description, when valid.
    pub description: Option<String>,
    /// Parse or access error, when invalid.
    pub error: Option<String>,
}

/// The Copy or Link behavior requested for a library import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LibraryImportMode {
    /// Materialize a separate canonical package.
    Copy,
    /// Create a symlink to the source package.
    Link,
}

/// Typed failure while importing a skill from another store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LibraryImportError {
    /// Source is not a readable skill package.
    #[error(transparent)]
    Package(#[from] at_store::SkillPackageError),
    /// Link creation failed.
    #[error(transparent)]
    Filesystem(#[from] at_fs::FsError),
    /// The requested name already exists in the destination store.
    #[error("target skill already exists: {0}")]
    NameClash(PathBuf),
    /// A read-only source cannot be linked because the platform cannot create the link.
    #[error("cannot create library link for read-only source {0}")]
    PermissionDenied(PathBuf),
}

/// One library import result, or a name-clash result for that candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryImportResult {
    /// Destination folder.
    pub target: PathBuf,
    /// Actual import mode.
    pub mode: LibraryImportMode,
    /// Canonical AgenTuna library provenance.
    pub source: at_store::ItemSource,
}
/// Observable status of an imported library link.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LibraryLinkState {
    /// The symlink target exists and can be read.
    Linked {
        /// Resolved target path.
        target: PathBuf,
    },
    /// The symlink remains but its target has disappeared.
    BrokenLink {
        /// Missing target path.
        target: PathBuf,
    },
    /// The library entry is not a symlink.
    NotLink,
}

/// Failure while inspecting a library link.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LibraryLinkError {
    /// The entry could not be inspected.
    #[error("could not inspect library link {path}: {source}")]
    Io {
        /// Link path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Reports whether a library link target is present or broken.
///
/// # Errors
/// Returns an IO error when the link itself cannot be inspected.
pub fn library_link_state(path: &Path) -> Result<LibraryLinkState, LibraryLinkError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| LibraryLinkError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_symlink() {
        return Ok(LibraryLinkState::NotLink);
    }
    let raw_target = std::fs::read_link(path).map_err(|source| LibraryLinkError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let target = if raw_target.is_absolute() {
        raw_target
    } else {
        path.parent().unwrap_or(Path::new(".")).join(raw_target)
    };
    if target.exists() {
        Ok(LibraryLinkState::Linked { target })
    } else {
        Ok(LibraryLinkState::BrokenLink { target })
    }
}

/// Lists importable skills across supplied scope and project stores.
///
/// `current_store` is excluded; empty source stores are omitted. Scope stores are supplied
/// explicitly by the caller, while registered project stores should come from `at_db::projects`.
///
/// # Errors
/// Returns a typed error if a source store cannot be enumerated.
pub fn enumerate_library(
    stores: &[LibraryStore],
    current_store: &Path,
) -> Result<(Vec<LibrarySource>, Vec<LibrarySkill>), ImportFolderError> {
    let mut sources = Vec::new();
    let mut skills = Vec::new();
    for store in stores {
        if same_path(&store.path, current_store) {
            continue;
        }
        let candidates = match import_folder(&store.path.join("skills")) {
            Ok(candidates) => candidates,
            Err(ImportFolderError::ReadDir { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if candidates.is_empty() {
            continue;
        }
        let kind = if store.scope.is_some() {
            LibrarySourceKind::Scope
        } else {
            LibrarySourceKind::Project
        };
        sources.push(LibrarySource {
            label: store.label.clone(),
            path: store.path.clone(),
            kind,
            count: candidates.len(),
            last_used_at: store.last_used_at.clone(),
        });
        skills.extend(candidates.into_iter().map(|candidate| LibrarySkill {
            source_label: store.label.clone(),
            source_root: store.path.clone(),
            path: store.path.join("skills").join(candidate.subpath),
            name: candidate.name,
            description: candidate.description,
            error: candidate.error,
        }));
    }
    Ok((sources, skills))
}
/// One selected source skill and its destination options.
pub struct LibraryImportRequest<'a> {
    /// Source skill folder.
    pub source_dir: &'a Path,
    /// Destination AgenTuna store.
    pub target: &'a at_store::layout::StoreLayout,
    /// Destination slug.
    pub slug: &'a at_store::layout::Slug,
    /// Copy or Link.
    pub mode: LibraryImportMode,
    /// Optional backup folder for an owned destination replacement.
    pub backup_dir: Option<&'a Path>,
    /// Optional overwrite guard.
    pub guard: Option<&'a dyn at_fs::OverwriteGuard>,
}

/// Imports selected library skills independently, retaining errors per candidate.
///
/// A clash or invalid package blocks only its own result; later candidates are still processed.
#[must_use]
pub fn import_library_selection(
    requests: &[LibraryImportRequest<'_>],
) -> Vec<Result<LibraryImportResult, LibraryImportError>> {
    requests
        .iter()
        .map(|request| {
            import_library_skill(
                request.source_dir,
                request.target,
                request.slug,
                request.mode,
                request.backup_dir,
                request.guard,
            )
        })
        .collect()
}

/// Imports a library package without mutating its source.
///
/// # Errors
/// Returns a package, destination clash, permission or filesystem error.
pub fn import_library_skill(
    source_dir: &Path,
    target: &at_store::layout::StoreLayout,
    slug: &at_store::layout::Slug,
    mode: LibraryImportMode,
    backup_dir: Option<&Path>,
    guard: Option<&dyn at_fs::OverwriteGuard>,
) -> Result<LibraryImportResult, LibraryImportError> {
    let destination = target.skill_dir(slug);
    if destination.exists() {
        return Err(LibraryImportError::NameClash(destination));
    }
    match mode {
        LibraryImportMode::Copy => {
            let package = at_store::SkillPackage::read(source_dir)?;
            package.package.write(target, slug, backup_dir, guard)?;
        }
        LibraryImportMode::Link => {
            let metadata = std::fs::metadata(source_dir).map_err(|source| {
                LibraryImportError::Package(at_store::SkillPackageError::Read {
                    path: source_dir.to_path_buf(),
                    source,
                })
            })?;
            #[cfg(unix)]
            if metadata.permissions().readonly() {
                return Err(LibraryImportError::PermissionDenied(
                    source_dir.to_path_buf(),
                ));
            }
            at_fs::deploy::link_dir(source_dir, &destination, at_model::LinkMode::Link).map_err(
                |error| match error {
                    at_fs::FsError::PermissionDenied { .. } => {
                        LibraryImportError::PermissionDenied(source_dir.to_path_buf())
                    }
                    other => LibraryImportError::Filesystem(other),
                },
            )?;
        }
    }
    let mut source = at_store::ItemSource::new(at_store::SourceKind::Library);
    source.url = Some(source_dir.to_string_lossy().into_owned());
    source.link_mode = Some(match mode {
        LibraryImportMode::Copy => at_model::LinkMode::Copy,
        LibraryImportMode::Link => at_model::LinkMode::Link,
    });
    Ok(LibraryImportResult {
        target: destination,
        mode,
        source,
    })
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}
/// Errors while enumerating scope and project library stores.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LibraryCatalogError {
    /// Registered project query failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// Registered project roots could not be resolved.
    #[error(transparent)]
    Paths(#[from] at_store::paths::PathError),
    /// A project scope store could not be constructed.
    #[error(transparent)]
    Store(#[from] at_store::layout::StoreError),
    /// A source skills directory could not be read.
    #[error(transparent)]
    Folder(ImportFolderError),
}

fn registered_project_store(
    project_root: &Path,
    user_store: &Path,
) -> Result<at_store::layout::StoreLayout, LibraryCatalogError> {
    let paths = at_store::paths::AppPaths::resolve(
        &at_store::paths::PathEnvironment::default()
            .with_home(user_store)
            .with_project_root(project_root),
    )?;
    Ok(at_store::layout::StoreLayout::for_scope(
        &paths,
        at_model::Scope::Project,
    )?)
}

/// Enumerates global, user and registered project stores other than the current target.
///
/// Project paths come from the registry; global and user roots are injected by the caller.
///
/// # Errors
/// Returns a typed database, path, store-layout or source-folder error.
pub fn enumerate_registered_library(
    connection: &rusqlite::Connection,
    global_store: &Path,
    user_store: &Path,
    target_scope: at_model::Scope,
    current_project_id: Option<&str>,
    current_project_path: Option<&Path>,
) -> Result<(Vec<LibrarySource>, Vec<LibrarySkill>), LibraryCatalogError> {
    let mut stores = Vec::new();
    for (scope, label, path) in [
        (at_model::Scope::Global, "Global", global_store),
        (at_model::Scope::User, "User", user_store),
    ] {
        if scope != target_scope {
            stores.push(LibraryStore {
                label: label.to_owned(),
                path: path.to_path_buf(),
                scope: Some(scope),
                last_used_at: None,
            });
        }
    }
    if target_scope != at_model::Scope::Local
        && let Some(project_path) = current_project_path
    {
        let layout = registered_project_store(project_path, user_store)?;
        if let Some(local_layout) = layout.local_scope() {
            stores.push(LibraryStore {
                label: "Local".to_owned(),
                path: local_layout.root().to_path_buf(),
                scope: Some(at_model::Scope::Local),
                last_used_at: None,
            });
        }
    }
    for project in at_db::projects::list_projects(connection)? {
        if target_scope == at_model::Scope::Project
            && current_project_id == Some(project.id.as_str())
        {
            continue;
        }
        let layout = registered_project_store(Path::new(&project.path), user_store)?;
        stores.push(LibraryStore {
            label: project.name,
            path: layout.root().to_path_buf(),
            scope: None,
            last_used_at: project.last_used_at,
        });
    }
    enumerate_library(&stores, Path::new("")).map_err(LibraryCatalogError::Folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn library_catalog_excludes_current_store_and_copy_is_independent() {
        let temp = tempfile::tempdir().expect("temp");
        let current =
            at_store::layout::StoreLayout::new(at_model::Scope::User, temp.path().join("current"));
        current.init().expect("current store");
        let source =
            at_store::layout::StoreLayout::new(at_model::Scope::Global, temp.path().join("source"));
        source.init().expect("source store");
        let slug = at_store::layout::Slug::parse("alpha").expect("slug");
        fs::create_dir_all(source.skill_dir(&slug)).expect("skill directory");
        fs::write(
            source.skill_file(&slug),
            b"---\nname: alpha\ndescription: source package\n---\n",
        )
        .expect("skill document");

        let (sources, skills) = enumerate_library(
            &[
                LibraryStore {
                    label: "Global".into(),
                    path: source.root().to_path_buf(),
                    scope: Some(at_model::Scope::Global),
                    last_used_at: None,
                },
                LibraryStore {
                    label: "Current".into(),
                    path: current.root().to_path_buf(),
                    scope: Some(at_model::Scope::User),
                    last_used_at: None,
                },
            ],
            current.root(),
        )
        .expect("catalog");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].kind, LibrarySourceKind::Scope);
        assert_eq!(sources[0].count, 1);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name.as_deref(), Some("alpha"));

        let copied = import_library_skill(
            &skills[0].path,
            &current,
            &at_store::layout::Slug::parse("alpha-copy").expect("target slug"),
            LibraryImportMode::Copy,
            None,
            None,
        )
        .expect("copy");
        let copied_doc = copied.target.join("SKILL.md");
        fs::write(&copied_doc, "independent edit").expect("edit target");
        assert_ne!(
            fs::read_to_string(&copied_doc).expect("target"),
            fs::read_to_string(source.skill_file(&slug)).expect("source")
        );
        assert_eq!(copied.source.kind, at_store::SourceKind::Library);
        let second_slug = at_store::layout::Slug::parse("alpha-second").expect("second slug");
        let results = import_library_selection(&[
            LibraryImportRequest {
                source_dir: &skills[0].path,
                target: &current,
                slug: &at_store::layout::Slug::parse("alpha-copy").expect("clashing slug"),
                mode: LibraryImportMode::Copy,
                backup_dir: None,
                guard: None,
            },
            LibraryImportRequest {
                source_dir: &skills[0].path,
                target: &current,
                slug: &second_slug,
                mode: LibraryImportMode::Copy,
                backup_dir: None,
                guard: None,
            },
        ]);
        assert!(matches!(results[0], Err(LibraryImportError::NameClash(_))));
        assert!(results[1].is_ok());
    }
    #[test]
    fn registered_library_enumerates_other_projects_and_skips_current_and_empty_sources() {
        let temp = tempfile::tempdir().expect("temp");
        let current = temp.path().join("current-project");
        let other = temp.path().join("other-project");
        fs::create_dir_all(&current).expect("current project");
        fs::create_dir_all(&other).expect("other project");
        let current_item = current.join(".agentuna/skills/current");
        let other_item = other.join(".agentuna/skills/other");
        fs::create_dir_all(&current_item).expect("current skill");
        fs::create_dir_all(&other_item).expect("other skill");
        fs::write(
            current_item.join("SKILL.md"),
            "---\nname: current\ndescription: current\n---\n",
        )
        .expect("current document");
        fs::write(
            other_item.join("SKILL.md"),
            "---\nname: other\ndescription: other\n---\n",
        )
        .expect("other document");
        let global = temp.path().join("global");
        let global_item = global.join("skills/global-skill");
        fs::create_dir_all(&global_item).expect("global skill");
        fs::write(
            global_item.join("SKILL.md"),
            "---\nname: global\ndescription: global\n---\n",
        )
        .expect("global document");
        let database = at_db::Database::open_in_memory().expect("database");
        let current_id =
            at_db::projects::add_project(database.connection(), &current, Some("Current"), None)
                .expect("register current")
                .id;
        at_db::projects::add_project(database.connection(), &other, Some("Other"), None)
            .expect("register other");
        let (sources, skills) = enumerate_registered_library(
            database.connection(),
            &global,
            &temp.path().join("empty-user"),
            at_model::Scope::Project,
            Some(&current_id),
            Some(&current),
        )
        .expect("registered library");
        assert_eq!(sources.len(), 2);
        assert_eq!(skills.len(), 2);
        assert!(sources.iter().any(|source| source.label == "Global"));
        assert!(sources.iter().any(|source| source.label == "Other"));
        assert!(
            skills
                .iter()
                .all(|skill| skill.name.as_deref() != Some("current"))
        );
    }
    #[test]
    fn read_only_library_source_can_copy_but_cannot_link() {
        let temp = tempfile::tempdir().expect("temp");
        let source =
            at_store::layout::StoreLayout::new(at_model::Scope::Global, temp.path().join("source"));
        let target =
            at_store::layout::StoreLayout::new(at_model::Scope::User, temp.path().join("target"));
        source.init().expect("source store");
        target.init().expect("target store");
        let slug = at_store::layout::Slug::parse("readonly").expect("slug");
        fs::create_dir_all(source.skill_dir(&slug)).expect("skill directory");
        fs::write(source.skill_file(&slug), b"source bytes").expect("skill file");
        let mut permissions = fs::metadata(source.skill_dir(&slug))
            .expect("source metadata")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(source.skill_dir(&slug), permissions).expect("read-only mode");

        let copied_slug = at_store::layout::Slug::parse("readonly-copy").expect("copy slug");
        let copied = import_library_skill(
            &source.skill_dir(&slug),
            &target,
            &copied_slug,
            LibraryImportMode::Copy,
            None,
            None,
        )
        .expect("copy from read-only source");
        assert_eq!(copied.source.link_mode, Some(at_model::LinkMode::Copy));

        let link_slug = at_store::layout::Slug::parse("readonly-link").expect("link slug");
        assert!(matches!(
            import_library_skill(
                &source.skill_dir(&slug),
                &target,
                &link_slug,
                LibraryImportMode::Link,
                None,
                None
            ),
            Err(LibraryImportError::PermissionDenied(_))
        ));
    }

    #[test]
    fn library_link_reads_source_and_becomes_broken_after_source_removal() {
        let temp = tempfile::tempdir().expect("temp");
        let source =
            at_store::layout::StoreLayout::new(at_model::Scope::Global, temp.path().join("source"));
        let target =
            at_store::layout::StoreLayout::new(at_model::Scope::User, temp.path().join("target"));
        source.init().expect("source");
        target.init().expect("target");
        let slug = at_store::layout::Slug::parse("linked").expect("slug");
        fs::create_dir_all(source.skill_dir(&slug)).expect("skill directory");
        fs::write(source.skill_file(&slug), b"linked content").expect("skill document");
        let imported = import_library_skill(
            &source.skill_dir(&slug),
            &target,
            &slug,
            LibraryImportMode::Link,
            None,
            None,
        )
        .expect("link");
        assert_eq!(imported.mode, LibraryImportMode::Link);
        assert_eq!(imported.source.link_mode, Some(at_model::LinkMode::Link));
        assert_eq!(
            fs::read(imported.target.join("SKILL.md")).expect("linked read"),
            b"linked content"
        );
        assert!(matches!(
            library_link_state(&imported.target),
            Ok(LibraryLinkState::Linked { .. })
        ));
        fs::remove_dir_all(source.skill_dir(&slug)).expect("remove source");
        assert!(matches!(
            library_link_state(&imported.target),
            Ok(LibraryLinkState::BrokenLink { .. })
        ));
    }
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
