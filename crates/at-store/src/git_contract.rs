//! Git contract of the Project store: what is committed, what stays machine-local.
//!
//! A Project store lives in the project directory and is meant to travel with the project's
//! repository. The contract is small and explicit:
//!
//! - Committed: `store.json`, `skills/`, `rules/`, `mcp.json` and `.agentuna/.gitignore`.
//! - Never committed: `.state/` (journals, locks, staging), `backups/`, `local/` (real Local
//!   scope content) and the per-machine override file [`PER_MACHINE_OVERRIDE_FILE`].
//!
//! [`ProjectGitContract::install_gitignore`] writes those rules into the store's own
//! `.gitignore` through the `at-fs` pipeline, appending only what is missing so a hand-edited
//! file keeps its lines. [`ProjectGitContract::inspect`] reports whether the store has
//! uncommitted changes and warns when the store cannot be committed at all (a parent rule
//! ignores `.agentuna`, or the project is nested inside another repository).
//! [`ProjectGitContract::stage`] stages the committed set after checking every path with
//! `git check-ignore`; it never stages an ignored path, never runs `git add -f` and never
//! commits — the commit stays the user's action.
//!
//! A symlinked store directory is refused before any write: the bytes would otherwise land
//! outside the project the user thinks they are editing.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use at_fs::{ExpectedState, FsError, WriteRequest, sha256_hex};
use at_git::local::{self, IgnoreRule, LocalGitError, LocalGitRunner, StatusEntry};
use at_model::Scope;

use crate::layout::{
    GITIGNORE_FILE, MANIFEST_FILE, MCP_FILE, RULES_DIR, SKILLS_DIR, StoreError, StoreLayout,
};

/// File whose content is per machine and therefore never committed.
///
/// It holds overrides that only make sense on one machine (an absolute tool path, a local port).
/// It is ignored by the store's `.gitignore` even though no current story writes it: the name is
/// reserved by the contract so a later feature cannot leak one into Git by accident.
pub const PER_MACHINE_OVERRIDE_FILE: &str = "settings.local.json";

/// Every rule the store's `.gitignore` must contain, in file order.
pub const GITIGNORE_RULES: [&str; 6] = [
    ".state/",
    "local/",
    "backups/",
    "*.tmp",
    ".DS_Store",
    PER_MACHINE_OVERRIDE_FILE,
];

/// The entries of a Project store that belong in Git, in contract order.
pub const COMMITTED_ENTRIES: [&str; 5] = [
    MANIFEST_FILE,
    SKILLS_DIR,
    RULES_DIR,
    MCP_FILE,
    GITIGNORE_FILE,
];

/// Comment written above the rules the contract adds.
const GITIGNORE_HEADER: &str =
    "# AgenTuna: machine-local state, backups and the Local scope are never committed.";

/// Deepest folder the committed-file walk descends below a committed directory.
const MAX_COMMITTED_DEPTH: usize = 8;

/// Most files one staging pass will consider.
const MAX_COMMITTED_FILES: usize = 10_000;

/// The git contract of one Project store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGitContract {
    layout: StoreLayout,
}

impl ProjectGitContract {
    /// Binds the contract to a Project store.
    ///
    /// # Errors
    ///
    /// [`GitContractError::NotProjectScope`] when `layout` is not a Project store,
    /// [`GitContractError::SymlinkedStore`] when the store directory is a symlink, and
    /// [`GitContractError::StoreNotDirectory`] when a non-directory occupies the store path.
    pub fn new(layout: StoreLayout) -> Result<Self, GitContractError> {
        if layout.scope() != Scope::Project {
            return Err(GitContractError::NotProjectScope {
                scope: layout.scope(),
            });
        }
        let contract = Self { layout };
        contract.check_store_root()?;
        Ok(contract)
    }

    /// The store this contract governs.
    #[must_use]
    pub fn layout(&self) -> &StoreLayout {
        &self.layout
    }

    /// `<project>/.agentuna`, the directory that becomes visible to git.
    #[must_use]
    pub fn store_root(&self) -> &Path {
        self.layout.root()
    }

    /// `<project>/.agentuna/.gitignore`.
    #[must_use]
    pub fn gitignore_path(&self) -> PathBuf {
        self.layout.root().join(GITIGNORE_FILE)
    }

    /// The project directory that holds the store.
    ///
    /// # Errors
    ///
    /// [`GitContractError::NoProjectDirectory`] when the store root has no parent.
    pub fn project_root(&self) -> Result<&Path, GitContractError> {
        self.store_root()
            .parent()
            .ok_or_else(|| GitContractError::NoProjectDirectory {
                path: self.store_root().to_path_buf(),
            })
    }

    /// Writes the contract rules into the store's `.gitignore`, appending only what is missing.
    ///
    /// The write goes through `at-fs`, replacing the file atomically only when its bytes still
    /// match what was read. An existing rule is never duplicated and an existing line is never
    /// rewritten, so a hand-edited `.gitignore` keeps its comments and extra rules.
    ///
    /// # Errors
    ///
    /// [`GitContractError::SymlinkedStore`], [`GitContractError::SymlinkedGitignore`],
    /// [`GitContractError::StoreMissing`], [`GitContractError::GitignoreUnreadable`] and
    /// [`GitContractError::Write`] when the guarded write fails.
    pub fn install_gitignore(&self) -> Result<GitignoreInstall, GitContractError> {
        self.check_store_root()?;
        let path = self.gitignore_path();
        let existing = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(GitContractError::SymlinkedGitignore { path });
            }
            Ok(meta) if meta.is_file() => {
                Some(fs::read(&path).map_err(|source| GitContractError::Read {
                    path: path.clone(),
                    source,
                })?)
            }
            Ok(_) => return Err(GitContractError::GitignoreNotAFile { path }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(GitContractError::Read { path, source }),
        };

        let missing: Vec<&str> = match &existing {
            Some(bytes) => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| GitContractError::GitignoreUnreadable { path: path.clone() })?;
                missing_rules(text)
            }
            None => GITIGNORE_RULES.to_vec(),
        };
        if existing.is_some() && missing.is_empty() {
            return Ok(GitignoreInstall {
                path,
                created: false,
                appended: Vec::new(),
            });
        }

        let mut text = match &existing {
            Some(bytes) => {
                let mut text = String::from_utf8(bytes.clone())
                    .map_err(|_| GitContractError::GitignoreUnreadable { path: path.clone() })?;
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text
            }
            None => String::new(),
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(GITIGNORE_HEADER);
        text.push('\n');
        for rule in &missing {
            text.push_str(rule);
            text.push('\n');
        }

        let request = match &existing {
            Some(bytes) => WriteRequest::replace(
                &path,
                text.into_bytes(),
                ExpectedState::Hash(sha256_hex(bytes)),
            ),
            None => WriteRequest::create(&path, text.into_bytes()),
        };
        at_fs::apply(&request)?;
        Ok(GitignoreInstall {
            path,
            created: existing.is_none(),
            appended: missing.into_iter().map(str::to_owned).collect(),
        })
    }

    /// Rules the store's `.gitignore` file is missing; empty means the file is complete, and
    /// every rule is missing when the file does not exist.
    ///
    /// # Errors
    ///
    /// [`GitContractError::Read`] when the file cannot be read and
    /// [`GitContractError::GitignoreUnreadable`] when it is not UTF-8.
    pub fn missing_gitignore_rules(&self) -> Result<Vec<String>, GitContractError> {
        let path = self.gitignore_path();
        match fs::read(&path) {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| GitContractError::GitignoreUnreadable { path })?;
                Ok(missing_rules(text).into_iter().map(str::to_owned).collect())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(GITIGNORE_RULES
                .iter()
                .map(|rule| (*rule).to_owned())
                .collect()),
            Err(source) => Err(GitContractError::Read { path, source }),
        }
    }

    /// Files of the committed set that exist right now, store-relative, sorted.
    ///
    /// Directories are walked (without following symlinks) so staging can name every file and
    /// report exactly what it staged. `store.json`, `mcp.json` and `.gitignore` are included only
    /// when present.
    ///
    /// # Errors
    ///
    /// [`GitContractError::Read`] when an entry cannot be listed, and
    /// [`GitContractError::TooManyFiles`] when the store holds more than
    /// `MAX_COMMITTED_FILES` committed files.
    pub fn committed_files(&self) -> Result<Vec<String>, GitContractError> {
        let mut files = Vec::new();
        for entry in COMMITTED_ENTRIES {
            let path = self.store_root().join(entry);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() => files.push(entry.to_owned()),
                Ok(meta) if meta.is_dir() => {
                    walk_files(&path, entry, 0, &mut files)?;
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(GitContractError::Read { path, source });
                }
            }
        }
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Reads the store's Git state: repository, uncommitted changes and blockers.
    ///
    /// A project outside a repository is not an error here; the returned state simply has no
    /// repository, so the UI can offer "stage in git" only where it would work.
    ///
    /// # Errors
    ///
    /// [`GitContractError::Git`] for a git failure other than "not a repository",
    /// [`GitContractError::StoreOutsideRepository`] when git found a repository that does not
    /// contain the store, and [`GitContractError::Read`] when `.gitignore` cannot be read.
    pub fn inspect(&self, runner: &dyn LocalGitRunner) -> Result<StoreGitState, GitContractError> {
        self.check_store_root()?;
        let project = self.project_root()?;
        let mut state = StoreGitState {
            repository_root: None,
            store_path: None,
            nested_in: None,
            ignored: None,
            dirty: false,
            status: Vec::new(),
            gitignore_missing: self.missing_gitignore_rules()?,
            warnings: Vec::new(),
        };
        let repository = match local::detect_repository(runner, project) {
            Ok(repository) => repository,
            Err(error) if error.is_not_a_repository() => return Ok(state),
            Err(error) => return Err(error.into()),
        };
        let store_path = self.store_path_in(&repository)?;
        if !same_directory(project, repository.root()) {
            state.nested_in = Some(repository.root().to_path_buf());
            state.warnings.push(GitWarning::NestedRepository {
                root: repository.root().to_path_buf(),
            });
        }
        if let Some(rule) = local::explain_ignore(runner, &repository, &store_path)? {
            state
                .warnings
                .push(GitWarning::StoreIgnored { rule: rule.clone() });
            state.ignored = Some(rule);
        }
        state.status = local::status_entries(runner, &repository, Some(&store_path))?;
        state.dirty = !state.status.is_empty();
        state.repository_root = Some(repository.root().to_path_buf());
        state.store_path = Some(store_path);
        Ok(state)
    }

    /// Stages the committed set and reports exactly which paths were staged.
    ///
    /// Nothing is written when the project is not a git repository. Before staging, the store
    /// must not itself be ignored and its `.gitignore` must carry every contract rule; then each
    /// candidate file is checked with `git check-ignore` and only the paths no rule covers are
    /// passed to `git add`. No commit happens.
    ///
    /// # Errors
    ///
    /// [`GitContractError::NotAGitRepository`], [`GitContractError::StoreIgnored`],
    /// [`GitContractError::GitignoreMissing`], [`GitContractError::GitignoreIncomplete`],
    /// [`GitContractError::StoreOutsideRepository`] and [`GitContractError::Git`] for a refused
    /// `git add`.
    pub fn stage(&self, runner: &dyn LocalGitRunner) -> Result<StageOutcome, GitContractError> {
        self.check_store_root()?;
        let project = self.project_root()?;
        let repository = match local::detect_repository(runner, project) {
            Ok(repository) => repository,
            Err(error) if error.is_not_a_repository() => {
                return Err(GitContractError::NotAGitRepository {
                    path: project.to_path_buf(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let store_path = self.store_path_in(&repository)?;
        if let Some(rule) = local::explain_ignore(runner, &repository, &store_path)? {
            return Err(GitContractError::StoreIgnored {
                detail: describe_rule(&rule),
            });
        }
        if !self.gitignore_path().is_file() {
            return Err(GitContractError::GitignoreMissing {
                path: self.gitignore_path(),
            });
        }
        let missing = self.missing_gitignore_rules()?;
        if !missing.is_empty() {
            return Err(GitContractError::GitignoreIncomplete {
                path: self.gitignore_path(),
                missing,
            });
        }

        let candidates: Vec<String> = self
            .committed_files()?
            .into_iter()
            .map(|file| format!("{store_path}/{file}"))
            .collect();
        let references: Vec<&str> = candidates.iter().map(String::as_str).collect();
        let ignored = local::ignored_paths(runner, &repository, &references)?;
        let staged: Vec<String> = candidates
            .iter()
            .filter(|candidate| !ignored.contains(candidate))
            .cloned()
            .collect();
        let staged_references: Vec<&str> = staged.iter().map(String::as_str).collect();
        local::add_paths(runner, &repository, &staged_references)?;
        Ok(StageOutcome {
            repository_root: repository.root().to_path_buf(),
            store_path,
            staged,
            skipped_ignored: ignored,
        })
    }

    /// The store directory relative to the repository root, for git pathspecs and display.
    fn store_path_in(&self, repository: &local::Repository) -> Result<String, GitContractError> {
        let root = canonical_or(repository.root());
        let store = canonical_or(self.store_root());
        let relative =
            store
                .strip_prefix(&root)
                .map_err(|_| GitContractError::StoreOutsideRepository {
                    path: self.store_root().to_path_buf(),
                    repository: repository.root().to_path_buf(),
                })?;
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(part) => match part.to_str() {
                    Some(text) => parts.push(text.to_owned()),
                    None => {
                        return Err(GitContractError::NotUtf8 {
                            path: relative.to_path_buf(),
                        });
                    }
                },
                _ => {
                    return Err(GitContractError::NotUtf8 {
                        path: relative.to_path_buf(),
                    });
                }
            }
        }
        Ok(parts.join("/"))
    }

    /// Refuses a symlinked or non-directory store root before anything is read or written.
    fn check_store_root(&self) -> Result<(), GitContractError> {
        let root = self.store_root();
        match fs::symlink_metadata(root) {
            Ok(meta) if meta.file_type().is_symlink() => Err(GitContractError::SymlinkedStore {
                path: root.to_path_buf(),
            }),
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(_) => Err(GitContractError::StoreNotDirectory {
                path: root.to_path_buf(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(GitContractError::StoreMissing {
                    path: root.to_path_buf(),
                })
            }
            Err(source) => Err(GitContractError::Read {
                path: root.to_path_buf(),
                source,
            }),
        }
    }
}

/// What [`ProjectGitContract::install_gitignore`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitignoreInstall {
    /// The `.gitignore` the contract owns.
    pub path: PathBuf,
    /// Whether the file was created by this call.
    pub created: bool,
    /// Rules appended by this call, in contract order.
    pub appended: Vec<String>,
}

impl GitignoreInstall {
    /// Whether this call changed the file.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.created || !self.appended.is_empty()
    }
}

/// What [`ProjectGitContract::inspect`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreGitState {
    /// Work-tree root that contains the project, when there is one.
    pub repository_root: Option<PathBuf>,
    /// Store directory relative to [`Self::repository_root`], for example `.agentuna`.
    pub store_path: Option<String>,
    /// Repository the project is nested inside when the project is not the repository root.
    pub nested_in: Option<PathBuf>,
    /// Rule that ignores the store itself, when one does.
    pub ignored: Option<IgnoreRule>,
    /// Whether the store has uncommitted changes.
    pub dirty: bool,
    /// Every change git reports for the store directory.
    pub status: Vec<StatusEntry>,
    /// Contract rules the store's `.gitignore` lacks; empty when it is complete.
    pub gitignore_missing: Vec<String>,
    /// Problems the caller must surface as warnings.
    pub warnings: Vec<GitWarning>,
}

impl StoreGitState {
    /// Whether there is a repository to stage into.
    #[must_use]
    pub fn has_repository(&self) -> bool {
        self.repository_root.is_some()
    }

    /// Whether staging is possible: a repository exists, the store is not ignored and
    /// `.gitignore` is complete.
    #[must_use]
    pub fn can_stage(&self) -> bool {
        self.has_repository() && self.ignored.is_none() && self.gitignore_missing.is_empty()
    }
}

/// A warning [`ProjectGitContract::inspect`] reports instead of failing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitWarning {
    /// A rule above the store ignores `.agentuna`, so nothing inside it can be committed.
    StoreIgnored {
        /// The ignore rule, including where it is written.
        rule: IgnoreRule,
    },
    /// The project sits inside a repository rooted somewhere above it.
    NestedRepository {
        /// Root of the repository that contains the project.
        root: PathBuf,
    },
}

impl fmt::Display for GitWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreIgnored { rule } => write!(
                formatter,
                "the project store is ignored by {}; remove that rule before committing it",
                describe_rule(rule)
            ),
            Self::NestedRepository { root } => write!(
                formatter,
                "the project is nested inside the repository at {}; the store would be committed there",
                root.display()
            ),
        }
    }
}

/// What [`ProjectGitContract::stage`] staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageOutcome {
    /// Work-tree root the paths are relative to.
    pub repository_root: PathBuf,
    /// Store directory relative to [`Self::repository_root`].
    pub store_path: String,
    /// Paths passed to `git add`, repository-relative and sorted.
    pub staged: Vec<String>,
    /// Committed paths an ignore rule covers, left unstaged.
    pub skipped_ignored: Vec<String>,
}

/// Why the git contract could not be applied.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitContractError {
    /// Only a Project store is committed.
    #[error("scope `{scope}` has no git contract; only the Project store is committed")]
    NotProjectScope {
        /// Scope of the store the caller used.
        scope: Scope,
    },
    /// The store directory is a symlink; following it would write outside the project.
    #[error("{}: project store is a symlink; refusing before any write", .path.display())]
    SymlinkedStore {
        /// The store root.
        path: PathBuf,
    },
    /// Something that is not a directory occupies the store path.
    #[error("{}: project store is not a directory", .path.display())]
    StoreNotDirectory {
        /// The store root.
        path: PathBuf,
    },
    /// The store has not been initialized yet.
    #[error("{}: project store does not exist; initialize the store first", .path.display())]
    StoreMissing {
        /// The store root.
        path: PathBuf,
    },
    /// The store root has no project directory above it.
    #[error("{}: project store has no project directory", .path.display())]
    NoProjectDirectory {
        /// The store root.
        path: PathBuf,
    },
    /// The store's `.gitignore` is a symlink.
    #[error("{}: store .gitignore is a symlink; refusing to replace it", .path.display())]
    SymlinkedGitignore {
        /// The `.gitignore` path.
        path: PathBuf,
    },
    /// A non-file occupies the `.gitignore` path.
    #[error("{}: store .gitignore is not a regular file", .path.display())]
    GitignoreNotAFile {
        /// The `.gitignore` path.
        path: PathBuf,
    },
    /// The store's `.gitignore` is not valid UTF-8 and cannot be extended safely.
    #[error("{}: store .gitignore is not UTF-8", .path.display())]
    GitignoreUnreadable {
        /// The `.gitignore` path.
        path: PathBuf,
    },
    /// Staging needs a `.gitignore` so backups and Local content stay out of the commit.
    #[error("{}: store .gitignore is missing; install the git contract first", .path.display())]
    GitignoreMissing {
        /// The `.gitignore` path.
        path: PathBuf,
    },
    /// Staging needs every contract rule in place.
    #[error(
        "{}: store .gitignore is incomplete (missing {missing:?}); install the git contract first",
        .path.display()
    )]
    GitignoreIncomplete {
        /// The `.gitignore` path.
        path: PathBuf,
        /// Rules that are missing.
        missing: Vec<String>,
    },
    /// The project is not inside a git work tree.
    #[error("{}: not a git repository; nothing was written", .path.display())]
    NotAGitRepository {
        /// The project directory git was asked about.
        path: PathBuf,
    },
    /// Git found a repository, but it does not contain the store.
    #[error(
        "{}: the project store is not inside the repository at {}",
        .path.display(),
        .repository.display()
    )]
    StoreOutsideRepository {
        /// The store root.
        path: PathBuf,
        /// Repository root git reported.
        repository: PathBuf,
    },
    /// A rule above the store ignores it, so `git add` would refuse the whole store.
    #[error("the project store cannot be staged: {detail}")]
    StoreIgnored {
        /// Rule, source and pattern, as reported by `git check-ignore`.
        detail: String,
    },
    /// Paths are UTF-8 on every platform AgenTuna documents; a store reached through another
    /// encoding is reported instead of being guessed at.
    #[error("{}: path is not UTF-8", .path.display())]
    NotUtf8 {
        /// Offending path.
        path: PathBuf,
    },
    /// A committed directory holds more entries than staging will consider.
    #[error("{}: more than {limit} files in the committed set", .directory.display())]
    TooManyFiles {
        /// Directory that was being walked.
        directory: PathBuf,
        /// The cap that was hit.
        limit: usize,
    },
    /// A store entry could not be read.
    #[error("{}: could not read store entry: {source}", .path.display())]
    Read {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The guarded write failed.
    #[error(transparent)]
    Write(#[from] FsError),
    /// Store layout resolution failed.
    #[error(transparent)]
    Layout(#[from] StoreError),
    /// A local git command failed.
    #[error(transparent)]
    Git(#[from] LocalGitError),
}

/// Rules of [`GITIGNORE_RULES`] that `text` does not already contain as a line.
fn missing_rules(text: &str) -> Vec<&'static str> {
    GITIGNORE_RULES
        .iter()
        .copied()
        .filter(|rule| {
            !text
                .lines()
                .any(|line| line.trim_end_matches('\r').trim() == *rule)
        })
        .collect()
}

/// Walks `directory` for regular files, skipping symlinks and bounding depth and count.
fn walk_files(
    directory: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), GitContractError> {
    if depth >= MAX_COMMITTED_DEPTH {
        return Ok(());
    }
    let entries = fs::read_dir(directory).map_err(|source| GitContractError::Read {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| GitContractError::Read {
            path: directory.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| GitContractError::Read {
            path: entry.path(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = format!("{prefix}/{name}");
        if file_type.is_dir() {
            walk_files(&entry.path(), &relative, depth + 1, out)?;
        } else if file_type.is_file() {
            if out.len() >= MAX_COMMITTED_FILES {
                return Err(GitContractError::TooManyFiles {
                    directory: directory.to_path_buf(),
                    limit: MAX_COMMITTED_FILES,
                });
            }
            out.push(relative);
        }
    }
    Ok(())
}

/// Renders an ignore rule as `source:line:pattern`, the form git itself prints.
fn describe_rule(rule: &IgnoreRule) -> String {
    let source = if rule.source.is_empty() {
        "command line"
    } else {
        rule.source.as_str()
    };
    match rule.line {
        Some(line) => format!("{source}:{line}:{}", rule.pattern),
        None => format!("{source}:{}", rule.pattern),
    }
}

/// Canonicalises when possible so two spellings of one directory compare equal.
fn canonical_or(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_directory(left: &Path, right: &Path) -> bool {
    canonical_or(left) == canonical_or(right)
}
