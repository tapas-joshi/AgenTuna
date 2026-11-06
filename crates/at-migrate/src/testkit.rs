//! Fixtures and a round-trip harness for migration safety tests.
//!
//! Compiled only with the `testkit` cargo feature. Add it as a dev-dependency:
//!
//! ```toml
//! [dev-dependencies]
//! at-migrate = { path = ".", features = ["testkit"] }
//! ```
//!
//! # What is here
//!
//! - [`crate::testkit::MigrationFixture`] builds a throwaway home with Claude Code, Codex and omp installed, a
//!   project whose skills duplicate the home skill with different bytes, two initialised
//!   stores, and one store skill already deployed to Claude Code in link or copy mode. It sits
//!   on top of [`at_fs::testkit::TestHome`], so it never touches the real home directory.
//! - [`crate::testkit::SnapshotFilter`] and [`crate::testkit::TreeSnapshot`] capture every path below the fixture roots with
//!   its kind, permission bits, size, content hash and symlink target, and compare two captures
//!   path by path ([`crate::testkit::TreeDiff`]).
//! - [`crate::testkit::RoundTrip`] snapshots the tree around each migration step, records what the step
//!   reported, and answers the safety questions the story asks: did the dry run write anything,
//!   did it name exactly the paths apply touched, did undo restore the baseline, did a refused
//!   undo leave the tree alone, did a failed apply roll back.
//!
//! # What is not here
//!
//! No migration logic. Scan, plan, apply and undo belong to TC-006, TC-009 and TC-010; the
//! harness observes them through [`crate::testkit::MigrationUnderTest`] (or step by step through
//! [`crate::testkit::RoundTrip::record`]) and never assumes how they work. Callers translate their own report
//! types into [`crate::testkit::ScanReport`], [`crate::testkit::PlanReport`], [`crate::testkit::DryRunReport`], [`crate::testkit::ApplyReport`] and
//! [`crate::testkit::UndoReport`], whose paths are snapshot keys such as `home/.claude/skills/example/SKILL.md`.
//!
//! # Bookkeeping
//!
//! An undo restores agent and store content but keeps the audit trail: operation backups,
//! journals, the SQLite index and logs survive by design. Those prefixes are captured (a dry
//! run that writes a journal must still be caught) but excluded from **payload** comparisons:
//! the reported-path check, the undo-vs-baseline check, the refusal check and the rollback
//! check.
//!
//! ```
//! # #[cfg(feature = "testkit")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use at_migrate::testkit::{DryRunReport, MigrationFixture, MigrationStep, RoundTrip};
//! use at_model::LinkMode;
//!
//! let fixture = MigrationFixture::new(LinkMode::Link)?;
//! let mut trip = RoundTrip::begin(fixture.filter())?;
//! trip.record(MigrationStep::DryRun, || Ok::<_, at_migrate::testkit::HarnessError>(DryRunReport::new(Vec::new())))?;
//! trip.ensure_step_untouched(MigrationStep::DryRun)?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "testkit"))]
//! # fn main() {}
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_fs::deploy::DeployRequest;
use at_fs::sha256_hex;
use at_fs::testkit::{AgentFixture, ProjectFixture, TestHome};
use at_model::{AgentId, LinkMode, Scope};
use at_store::layout::{BACKUPS_DIR, STATE_DIR, StoreLayout, init_store};
use at_store::paths::{AppPaths, BACKUP_DIR_NAME, DB_FILE_NAME, LOG_DIR_NAME, PathEnvironment};

/// File name of a skill document.
pub const SKILL_FILE: &str = "SKILL.md";

/// Name of the native skill every installed agent already holds.
pub const NATIVE_SKILL: &str = at_fs::testkit::SAMPLE_SKILL;

/// Name of the store skill the fixture already applied to Claude Code.
pub const MANAGED_SKILL: &str = "managed-skill";

/// Snapshot label of the temporary home directory.
pub const HOME_LABEL: &str = "home";

/// Snapshot label of the project fixture.
pub const PROJECT_LABEL: &str = "project";

/// Why a fixture could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FixtureError {
    /// The shared `at-fs` fixture failed.
    #[error(transparent)]
    Testkit(#[from] at_fs::testkit::TestkitError),
    /// Store paths could not be resolved.
    #[error(transparent)]
    Paths(#[from] at_store::paths::PathError),
    /// A store could not be initialised.
    #[error(transparent)]
    Store(#[from] at_store::layout::StoreError),
    /// A deployment could not be created.
    #[error(transparent)]
    Deploy(#[from] at_fs::FsError),
    /// A fixture file or directory could not be written or read.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The resolved project store is missing although a project root was supplied.
    #[error("the fixture resolved no project store")]
    MissingProjectStore,
    /// A shared fixture path did not have the layout this fixture relies on.
    #[error("{}: unexpected fixture layout", .path.display())]
    UnexpectedLayout {
        /// Path that did not match.
        path: PathBuf,
    },
}

impl FixtureError {
    fn io(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Where every part of a [`MigrationFixture`] lives.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FixturePaths {
    /// The temporary home (`<temp>/home`), already canonical.
    pub home: PathBuf,
    /// The project fixture root, already canonical.
    pub project: PathBuf,
    /// Where the index, logs and operation backups go (`AppPaths::support_dir`).
    pub support_dir: PathBuf,
    /// `agentuna.db` inside [`Self::support_dir`].
    pub db_path: PathBuf,
    /// The user store root (`<home>/.agentuna`), initialised by the fixture.
    pub user_store: PathBuf,
    /// The project store root (`<project>/.agentuna`), initialised by the fixture.
    pub project_store: PathBuf,
    /// Snapshot key of [`Self::user_store`], e.g. `home/.agentuna`.
    pub user_store_key: String,
    /// Snapshot key of [`Self::project_store`], e.g. `project/.agentuna`.
    pub project_store_key: String,
    /// Snapshot key of [`Self::support_dir`], e.g.
    /// `home/Library/Application Support/AgenTuna`.
    pub support_key: String,
    /// The three installed agents, with the skill and MCP paths each fixture wrote.
    pub agents: Vec<AgentFixture>,
    /// Project copy of [`NATIVE_SKILL`]: same name as the home copies, different bytes.
    pub project_native_skill: PathBuf,
    /// Second project copy of [`NATIVE_SKILL`], under Claude Code's project folder.
    pub project_claude_skill: PathBuf,
    /// Project MCP file, whose `example` server conflicts with `~/.claude.json`.
    pub project_mcp: PathBuf,
    /// Canonical store skill the migration moves content into.
    pub store_skill: PathBuf,
    /// The deployment of [`MANAGED_SKILL`] the fixture already created in Claude Code's folder.
    pub managed_deployment: PathBuf,
}

/// A native agent installation set plus a conflicting project, used by migration tests.
///
/// Everything lives in one temporary directory owned by the fixture and removed on drop.
#[derive(Debug)]
pub struct MigrationFixture {
    home: TestHome,
    project: ProjectFixture,
    paths: FixturePaths,
    mode: LinkMode,
}

impl MigrationFixture {
    /// Builds the canonical migration scenario.
    ///
    /// - Claude Code, Codex and omp are installed in the home with one native skill, one MCP
    ///   server and one rules file each.
    /// - `acme` is a project with two copies of [`NATIVE_SKILL`] whose bytes differ from the
    ///   home copies and from each other, plus an `.mcp.json` whose `example` server conflicts
    ///   with `~/.claude.json`.
    /// - The user and project stores are initialised, and [`MANAGED_SKILL`] is created in the
    ///   user store and deployed to Claude Code with `mode`.
    ///
    /// # Errors
    /// [`FixtureError`] when a directory, store or deployment cannot be created.
    pub fn new(mode: LinkMode) -> Result<Self, FixtureError> {
        let home = TestHome::new()?;
        let claude = home.install_claude_code()?;
        let codex = home.install_codex()?;
        let omp = home.install_omp()?;
        let project = home.project("acme")?;

        let project_native_skill = project
            .root()
            .join(".agents/skills")
            .join(NATIVE_SKILL)
            .join(SKILL_FILE);
        write_file(&project_native_skill, project_agent_skill())?;
        let project_claude_skill = project
            .root()
            .join(".claude/skills")
            .join(NATIVE_SKILL)
            .join(SKILL_FILE);
        write_file(&project_claude_skill, project_claude_skill_doc())?;
        let project_mcp = project.root().join(".mcp.json");
        write_file(&project_mcp, project_mcp_document())?;

        let project_root = project.root().to_path_buf();
        let app_paths = home.paths_with(|home_dir| {
            AppPaths::resolve(
                &PathEnvironment::default()
                    .with_home(home_dir)
                    .with_project_root(&project_root),
            )
        })?;
        let user_store = app_paths.user_store().to_path_buf();
        let project_store = app_paths
            .project_store()
            .ok_or(FixtureError::MissingProjectStore)?
            .to_path_buf();
        init_store(Scope::User, &user_store)?;
        init_store(Scope::Project, &project_store)?;

        let store_skill_dir = StoreLayout::new(Scope::User, &user_store)
            .skills_dir()
            .join(MANAGED_SKILL);
        fs::create_dir_all(&store_skill_dir).map_err(FixtureError::io(&store_skill_dir))?;
        let store_skill = store_skill_dir.join(SKILL_FILE);
        write_file(&store_skill, managed_skill())?;
        set_mode(&store_skill, 0o600)?;

        let claude_skills = skills_dir(&claude).ok_or_else(|| FixtureError::UnexpectedLayout {
            path: claude.skill_path.clone(),
        })?;
        let managed_deployment = claude_skills.join(MANAGED_SKILL);
        at_fs::deploy(&DeployRequest::new(
            &store_skill_dir,
            &managed_deployment,
            mode,
        ))?;

        let home_root = home.home().to_path_buf();
        let user_store_key =
            snapshot_key(HOME_LABEL, &home_root, &user_store).ok_or_else(|| {
                FixtureError::UnexpectedLayout {
                    path: user_store.clone(),
                }
            })?;
        let project_store_key = snapshot_key(PROJECT_LABEL, project.root(), &project_store)
            .ok_or_else(|| FixtureError::UnexpectedLayout {
                path: project_store.clone(),
            })?;
        let support_dir = app_paths.support_dir().to_path_buf();
        let support_key = snapshot_key(HOME_LABEL, &home_root, &support_dir).ok_or_else(|| {
            FixtureError::UnexpectedLayout {
                path: support_dir.clone(),
            }
        })?;

        let paths = FixturePaths {
            home: home_root,
            project: project.root().to_path_buf(),
            support_dir,
            db_path: app_paths.db_path().to_path_buf(),
            user_store,
            project_store,
            user_store_key,
            project_store_key,
            support_key,
            agents: vec![claude, codex, omp],
            project_native_skill,
            project_claude_skill,
            project_mcp,
            store_skill,
            managed_deployment,
        };
        Ok(Self {
            home,
            project,
            paths,
            mode,
        })
    }

    /// Builds the scenario with link deployments.
    ///
    /// # Errors
    /// See [`Self::new`].
    pub fn link() -> Result<Self, FixtureError> {
        Self::new(LinkMode::Link)
    }

    /// Builds the scenario with copy deployments.
    ///
    /// # Errors
    /// See [`Self::new`].
    pub fn copy() -> Result<Self, FixtureError> {
        Self::new(LinkMode::Copy)
    }

    /// Where everything lives.
    #[must_use]
    pub fn paths(&self) -> &FixturePaths {
        &self.paths
    }

    /// Deployment mode the fixture used for [`MANAGED_SKILL`] and the mode a migration test
    /// deploys with.
    #[must_use]
    pub fn mode(&self) -> LinkMode {
        self.mode
    }

    /// The shared home fixture, for tests that need to write extra files or read them back.
    #[must_use]
    pub fn home(&self) -> &TestHome {
        &self.home
    }

    /// The project fixture.
    #[must_use]
    pub fn project(&self) -> &ProjectFixture {
        &self.project
    }

    /// Snapshot key (`home/...` or `project/...`) of an absolute path inside the fixture.
    #[must_use]
    pub fn key(&self, path: &Path) -> Option<String> {
        // The project fixture lives below `home/projects`, and the home walk skips it, so the
        // project label wins for paths inside both roots.
        snapshot_key(PROJECT_LABEL, &self.paths.project, path)
            .or_else(|| snapshot_key(HOME_LABEL, &self.paths.home, path))
    }

    /// Where a deployment of `skill` goes inside `agent`'s skills folder.
    #[must_use]
    pub fn deployment_target(&self, agent: AgentId, skill: &str) -> Option<PathBuf> {
        self.paths
            .agents
            .iter()
            .find(|fixture| fixture.agent == agent)
            .and_then(skills_dir)
            .map(|skills| skills.join(skill))
    }

    /// The snapshot filter over the home and the project, with store bookkeeping excluded from
    /// payload comparisons.
    #[must_use]
    pub fn filter(&self) -> SnapshotFilter {
        SnapshotFilter::fixture(self)
    }
}

/// Skills folder of an installed agent (`.../skills/<name>/SKILL.md` without the last two
/// components).
fn skills_dir(agent: &AgentFixture) -> Option<PathBuf> {
    agent
        .skill_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> Result<(), FixtureError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(FixtureError::io(parent))?;
    }
    fs::write(path, contents.as_ref()).map_err(FixtureError::io(path))
}

fn set_mode(path: &Path, mode: u32) -> Result<(), FixtureError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(FixtureError::io(path))
}

fn snapshot_key(label: &str, root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let key = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if key.is_empty() {
        return None;
    }
    Some(format!("{label}/{key}"))
}

fn skill_doc(name: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n",
        name = name,
        body = body
    )
}

fn project_agent_skill() -> String {
    skill_doc(
        NATIVE_SKILL,
        "Project copy installed for every agent that reads `.agents/skills`; its bytes differ \
         from the home copies.",
    )
}

fn project_claude_skill_doc() -> String {
    skill_doc(
        NATIVE_SKILL,
        "Claude Code project copy of the same skill name, with yet another body.",
    )
}

fn managed_skill() -> String {
    skill_doc(
        MANAGED_SKILL,
        "Canonical store skill the fixture already deployed to Claude Code.",
    )
}

fn project_mcp_document() -> String {
    r#"{
  "mcpServers": {
    "example": {
      "command": "uvx",
      "args": ["--from", "project-example", "serve"]
    },
    "project-only": {
      "command": "node",
      "args": ["server.js"]
    }
  }
}
"#
    .to_owned()
}

// -------------------------------------------------------------------------------------------
// Snapshots
// -------------------------------------------------------------------------------------------

/// One labelled directory a snapshot walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRoot {
    /// Key prefix for entries below the root (`home`, `project`).
    pub label: String,
    /// Absolute directory to walk.
    pub path: PathBuf,
}

impl SnapshotRoot {
    /// A root labelled `label` that walks `path`.
    #[must_use]
    pub fn new(label: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
        }
    }
}

/// What one filesystem entry is.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A real directory. A symlink to a directory is [`Self::Symlink`] and is never followed.
    Dir,
    /// A symbolic link, recorded by target and never followed.
    Symlink,
    /// A device, socket or FIFO.
    Other,
}

impl EntryKind {
    /// Short name used in diff summaries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

/// Everything a snapshot records about one path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EntrySnapshot {
    /// What is at the path, without following symlinks.
    pub kind: EntryKind,
    /// Permission bits (`mode & 0o7777`).
    pub mode: u32,
    /// Size in bytes; `0` for directories and symlinks.
    pub size: u64,
    /// SHA-256 of the file contents, or of the symlink target bytes.
    pub hash: Option<String>,
    /// Target of a symlink exactly as stored (relative or absolute).
    pub link_target: Option<PathBuf>,
}

/// A path-by-path capture of every entry below a set of roots.
///
/// Keys are `"<label>/<relative path>"`, so a diff names a path the same way a test would:
/// `home/.claude/skills/example-skill/SKILL.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeSnapshot {
    entries: BTreeMap<String, EntrySnapshot>,
}

impl TreeSnapshot {
    /// Number of paths captured.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One entry by key.
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<&EntrySnapshot> {
        self.entries.get(key)
    }

    /// Every key, in sorted order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Every entry, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &EntrySnapshot)> {
        self.entries
            .iter()
            .map(|(key, entry)| (key.as_str(), entry))
    }

    /// Compares this capture (the earlier one) with `after`.
    #[must_use]
    pub fn diff(&self, after: &Self) -> TreeDiff {
        let mut entries = Vec::new();
        for (path, before) in &self.entries {
            match after.entries.get(path) {
                None => entries.push(DiffEntry::Removed {
                    path: path.clone(),
                    before: before.clone(),
                }),
                Some(entry) if entry != before => entries.push(DiffEntry::Changed {
                    path: path.clone(),
                    before: before.clone(),
                    after: entry.clone(),
                    fields: changed_fields(before, entry),
                }),
                Some(_) => {}
            }
        }
        for (path, entry) in &after.entries {
            if !self.entries.contains_key(path) {
                entries.push(DiffEntry::Added {
                    path: path.clone(),
                    after: entry.clone(),
                });
            }
        }
        entries.sort_by(|left, right| left.path().cmp(right.path()));
        TreeDiff { entries }
    }
}

/// One path that differs between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiffEntry {
    /// The path exists only in the later snapshot.
    Added {
        /// Snapshot key.
        path: String,
        /// What is there now.
        after: EntrySnapshot,
    },
    /// The path exists only in the earlier snapshot.
    Removed {
        /// Snapshot key.
        path: String,
        /// What was there.
        before: EntrySnapshot,
    },
    /// The path exists in both snapshots but something about it differs.
    Changed {
        /// Snapshot key.
        path: String,
        /// Earlier entry.
        before: EntrySnapshot,
        /// Later entry.
        after: EntrySnapshot,
        /// Which fields differ.
        fields: Vec<ChangedField>,
    },
}

impl DiffEntry {
    /// Snapshot key of the path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Added { path, .. } | Self::Removed { path, .. } | Self::Changed { path, .. } => {
                path
            }
        }
    }
}

/// A field that differs between two [`EntrySnapshot`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangedField {
    /// File, directory, symlink or other.
    Kind,
    /// Permission bits.
    Mode,
    /// Size in bytes.
    Size,
    /// Content hash, or the hash of a symlink target.
    Hash,
    /// Target path of a symlink.
    LinkTarget,
}

impl ChangedField {
    /// Short name used in diff summaries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Mode => "mode",
            Self::Size => "size",
            Self::Hash => "hash",
            Self::LinkTarget => "link-target",
        }
    }
}

fn changed_fields(before: &EntrySnapshot, after: &EntrySnapshot) -> Vec<ChangedField> {
    let mut fields = Vec::new();
    if before.kind != after.kind {
        fields.push(ChangedField::Kind);
    }
    if before.mode != after.mode {
        fields.push(ChangedField::Mode);
    }
    if before.size != after.size {
        fields.push(ChangedField::Size);
    }
    if before.hash != after.hash {
        fields.push(ChangedField::Hash);
    }
    if before.link_target != after.link_target {
        fields.push(ChangedField::LinkTarget);
    }
    fields
}

/// Everything that differs between two snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeDiff {
    entries: Vec<DiffEntry>,
}

impl TreeDiff {
    /// The individual differences, sorted by path.
    #[must_use]
    pub fn entries(&self) -> &[DiffEntry] {
        &self.entries
    }

    /// Number of differing paths.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the snapshots are identical.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every differing path.
    #[must_use]
    pub fn paths(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .map(|entry| entry.path().to_owned())
            .collect()
    }

    /// Differing paths that are not below any ignored prefix.
    #[must_use]
    pub fn paths_excluding(&self, ignored: &[String]) -> BTreeSet<String> {
        self.entries
            .iter()
            .map(DiffEntry::path)
            .filter(|path| !is_ignored(path, ignored))
            .map(str::to_owned)
            .collect()
    }

    /// The same diff with ignored paths removed.
    #[must_use]
    pub fn excluding(&self, ignored: &[String]) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .filter(|entry| !is_ignored(entry.path(), ignored))
                .cloned()
                .collect(),
        }
    }

    /// One line per differing path, for assertion messages.
    #[must_use]
    pub fn summary(&self) -> String {
        self.entries
            .iter()
            .map(|entry| match entry {
                DiffEntry::Added { path, after } => {
                    format!("  + {path} ({} {:04o})", after.kind.as_str(), after.mode)
                }
                DiffEntry::Removed { path, before } => {
                    format!("  - {path} ({} {:04o})", before.kind.as_str(), before.mode)
                }
                DiffEntry::Changed {
                    path,
                    before,
                    after,
                    fields,
                } => format!(
                    "  ~ {path} ({} -> {}; {})",
                    before.kind.as_str(),
                    after.kind.as_str(),
                    fields
                        .iter()
                        .map(|field| field.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Succeeds when the trees are identical, otherwise fails with the path list.
    ///
    /// # Errors
    /// [`HarnessError::TreeChanged`] naming every differing path.
    pub fn ensure_empty(&self, context: &str) -> Result<(), HarnessError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(HarnessError::TreeChanged {
                context: context.to_owned(),
                diff: self.summary(),
            })
        }
    }
}

fn is_ignored(key: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        key.strip_prefix(prefix.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// What to capture, and which paths a payload comparison treats as bookkeeping.
///
/// Capture walks every root and records every entry, so a dry run that writes a journal file
/// is still visible in the step diff. [`Self::fixture`] additionally lists the store backups,
/// journals, index and logs as bookkeeping: they legitimately survive an undo.
#[derive(Debug, Clone)]
pub struct SnapshotFilter {
    roots: Vec<SnapshotRoot>,
    bookkeeping: Vec<String>,
}

impl SnapshotFilter {
    /// Roots with no bookkeeping: every difference counts.
    #[must_use]
    pub fn new(roots: impl IntoIterator<Item = SnapshotRoot>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
            bookkeeping: Vec::new(),
        }
    }

    /// The home and the project of a migration fixture, plus the fixture's bookkeeping
    /// prefixes.
    ///
    /// A root nested inside another root (the project fixture lives below `home/projects`) is
    /// captured once, under its own label.
    #[must_use]
    pub fn fixture(fixture: &MigrationFixture) -> Self {
        let paths = fixture.paths();
        let mut bookkeeping = Vec::new();
        for store_key in [&paths.user_store_key, &paths.project_store_key] {
            bookkeeping.push(format!("{store_key}/{BACKUPS_DIR}"));
            bookkeeping.push(format!("{store_key}/{STATE_DIR}"));
        }
        bookkeeping.push(format!("{}/{BACKUP_DIR_NAME}", paths.support_key));
        bookkeeping.push(format!("{}/{LOG_DIR_NAME}", paths.support_key));
        for suffix in ["", "-wal", "-shm", "-journal"] {
            bookkeeping.push(format!("{}/{DB_FILE_NAME}{suffix}", paths.support_key));
        }
        Self {
            roots: vec![
                SnapshotRoot::new(HOME_LABEL, &paths.home),
                SnapshotRoot::new(PROJECT_LABEL, &paths.project),
            ],
            bookkeeping,
        }
    }

    /// Replaces the bookkeeping prefixes.
    #[must_use]
    pub fn with_bookkeeping(mut self, prefixes: impl IntoIterator<Item = String>) -> Self {
        self.bookkeeping = prefixes.into_iter().collect();
        self
    }

    /// The roots walked by [`Self::capture`].
    #[must_use]
    pub fn roots(&self) -> &[SnapshotRoot] {
        &self.roots
    }

    /// Prefixes a payload comparison ignores.
    #[must_use]
    pub fn bookkeeping(&self) -> &[String] {
        &self.bookkeeping
    }

    /// Captures every entry below every root.
    ///
    /// # Errors
    /// [`HarnessError::MissingRoot`] when a root is absent or not a directory, and
    /// [`HarnessError::Io`] when a directory or file cannot be read.
    pub fn capture(&self) -> Result<TreeSnapshot, HarnessError> {
        for root in &self.roots {
            let meta = fs::symlink_metadata(&root.path).map_err(|source| {
                if source.kind() == io::ErrorKind::NotFound {
                    HarnessError::MissingRoot {
                        path: root.path.clone(),
                    }
                } else {
                    HarnessError::Io {
                        path: root.path.clone(),
                        source,
                    }
                }
            })?;
            if !meta.is_dir() {
                return Err(HarnessError::MissingRoot {
                    path: root.path.clone(),
                });
            }
        }
        let mut entries = BTreeMap::new();
        for root in &self.roots {
            capture_root(root, &self.roots, &mut entries)?;
        }
        Ok(TreeSnapshot { entries })
    }
}

fn capture_root(
    root: &SnapshotRoot,
    roots: &[SnapshotRoot],
    entries: &mut BTreeMap<String, EntrySnapshot>,
) -> Result<(), HarnessError> {
    let mut stack = vec![root.path.clone()];
    while let Some(dir) = stack.pop() {
        let children = fs::read_dir(&dir).map_err(|source| HarnessError::Io {
            path: dir.clone(),
            source,
        })?;
        for child in children {
            let child = child.map_err(|source| HarnessError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = child.path();
            let Some(key) = snapshot_key(&root.label, &root.path, &path) else {
                continue;
            };
            let meta = fs::symlink_metadata(&path).map_err(|source| HarnessError::Io {
                path: path.clone(),
                source,
            })?;
            let file_type = meta.file_type();
            let mode = mode_bits(&meta);
            if file_type.is_symlink() {
                let target = fs::read_link(&path).map_err(|source| HarnessError::Io {
                    path: path.clone(),
                    source,
                })?;
                entries.insert(
                    key,
                    EntrySnapshot {
                        kind: EntryKind::Symlink,
                        mode,
                        size: 0,
                        hash: Some(sha256_hex(target.as_os_str().as_encoded_bytes())),
                        link_target: Some(target),
                    },
                );
            } else if file_type.is_dir() {
                if roots.iter().any(|other| other.path == path) {
                    // Captured under its own label; walking it here would duplicate every key.
                    continue;
                }
                entries.insert(
                    key,
                    EntrySnapshot {
                        kind: EntryKind::Dir,
                        mode,
                        size: 0,
                        hash: None,
                        link_target: None,
                    },
                );
                stack.push(path);
            } else if file_type.is_file() {
                let bytes = fs::read(&path).map_err(|source| HarnessError::Io {
                    path: path.clone(),
                    source,
                })?;
                entries.insert(
                    key,
                    EntrySnapshot {
                        kind: EntryKind::File,
                        mode,
                        size: meta.len(),
                        hash: Some(sha256_hex(&bytes)),
                        link_target: None,
                    },
                );
            } else {
                entries.insert(
                    key,
                    EntrySnapshot {
                        kind: EntryKind::Other,
                        mode,
                        size: 0,
                        hash: None,
                        link_target: None,
                    },
                );
            }
        }
    }
    Ok(())
}

fn mode_bits(meta: &fs::Metadata) -> u32 {
    meta.permissions().mode() & 0o7777
}

// -------------------------------------------------------------------------------------------
// Step reports
// -------------------------------------------------------------------------------------------

/// The lifecycle steps a migration round trip drives, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MigrationStep {
    /// Discovery of native items.
    Scan,
    /// The plan the engine would execute.
    Plan,
    /// A rehearsal that must write nothing.
    DryRun,
    /// The real mutation.
    Apply,
    /// Restoring the pre-apply tree.
    Undo,
}

impl MigrationStep {
    /// Every step in the order a round trip drives them.
    pub const ORDER: [Self; 5] = [
        Self::Scan,
        Self::Plan,
        Self::DryRun,
        Self::Apply,
        Self::Undo,
    ];

    /// Lowercase name used in messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Plan => "plan",
            Self::DryRun => "dry run",
            Self::Apply => "apply",
            Self::Undo => "undo",
        }
    }
}

impl fmt::Display for MigrationStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a step refused to change anything.
///
/// These are the harness's projections of the engine's own errors: [`Self::Conflict`] stands
/// for the generic undo conflict (a current hash differs from the recorded one) and
/// [`Self::Expired`] for `MigrationExpired` (the operation is older than
/// `UNDO_WINDOW_DAYS`, fixed at 30 days by TC-010). Until those errors exist, the test's driver
/// maps its own error names onto these variants.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The file changed on disk after the migration recorded it.
    Conflict,
    /// The operation is older than the undo window (`MigrationExpired`).
    Expired,
    /// Any other refusal, named by the code under test.
    Other(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => formatter.write_str("a conflict"),
            Self::Expired => formatter.write_str("expiry"),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

/// How a step ended.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepResult {
    /// The step did what it was asked.
    Completed,
    /// The step declined, stating why.
    Refused(Refusal),
    /// The step errored.
    Failed(String),
}

impl fmt::Display for StepResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed => formatter.write_str("success"),
            Self::Refused(refusal) => write!(formatter, "a refusal ({refusal})"),
            Self::Failed(message) => write!(formatter, "a failure: {message}"),
        }
    }
}

/// What a step reported, in the harness's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    /// Item labels (scan) or snapshot keys (every other step) the step reported.
    pub reported: Vec<String>,
    /// How it ended.
    pub result: StepResult,
    /// Operation id returned by apply, when the engine issues one.
    pub operation_id: Option<String>,
}

impl StepOutcome {
    fn completed(reported: Vec<String>) -> Self {
        Self {
            reported,
            result: StepResult::Completed,
            operation_id: None,
        }
    }
}

/// What scan found. Item labels are opaque strings the caller chooses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Discovered items, e.g. `skill:example-skill@claude_code`.
    pub items: Vec<String>,
}

impl ScanReport {
    /// A report with these items.
    #[must_use]
    pub fn new(items: impl IntoIterator<Item = String>) -> Self {
        Self {
            items: items.into_iter().collect(),
        }
    }
}

impl From<ScanReport> for StepOutcome {
    fn from(report: ScanReport) -> Self {
        Self::completed(report.items)
    }
}

/// The paths a plan would change, as snapshot keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanReport {
    /// Paths the plan touches.
    pub planned_touches: Vec<String>,
}

impl PlanReport {
    /// A report touching these paths.
    #[must_use]
    pub fn new(planned_touches: impl IntoIterator<Item = String>) -> Self {
        Self {
            planned_touches: planned_touches.into_iter().collect(),
        }
    }
}

impl From<PlanReport> for StepOutcome {
    fn from(report: PlanReport) -> Self {
        Self::completed(report.planned_touches)
    }
}

/// The paths a dry run would change, as snapshot keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunReport {
    /// Paths apply would touch, including directories it would create.
    pub would_touch: Vec<String>,
}

impl DryRunReport {
    /// A report touching these paths.
    #[must_use]
    pub fn new(would_touch: impl IntoIterator<Item = String>) -> Self {
        Self {
            would_touch: would_touch.into_iter().collect(),
        }
    }
}

impl From<DryRunReport> for StepOutcome {
    fn from(report: DryRunReport) -> Self {
        Self::completed(report.would_touch)
    }
}

/// What apply changed, as snapshot keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Operation id recorded for the mutation, when the engine issues one.
    pub operation_id: Option<String>,
    /// Every path the apply touched, including created directories.
    pub touched: Vec<String>,
}

impl ApplyReport {
    /// A report touching these paths.
    #[must_use]
    pub fn new(touched: impl IntoIterator<Item = String>) -> Self {
        Self {
            operation_id: None,
            touched: touched.into_iter().collect(),
        }
    }

    /// Records the operation id.
    #[must_use]
    pub fn with_operation(mut self, operation_id: impl Into<String>) -> Self {
        self.operation_id = Some(operation_id.into());
        self
    }
}

impl From<ApplyReport> for StepOutcome {
    fn from(report: ApplyReport) -> Self {
        Self {
            reported: report.touched,
            result: StepResult::Completed,
            operation_id: report.operation_id,
        }
    }
}

/// What undo did, as snapshot keys.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UndoReport {
    /// The files were restored.
    Restored {
        /// Paths the undo restored or removed.
        paths: Vec<String>,
    },
    /// The undo declined.
    Refused {
        /// Why it declined.
        refusal: Refusal,
        /// Paths that blocked or would have been restored.
        paths: Vec<String>,
    },
}

impl UndoReport {
    /// A successful undo of these paths.
    #[must_use]
    pub fn restored(paths: impl IntoIterator<Item = String>) -> Self {
        Self::Restored {
            paths: paths.into_iter().collect(),
        }
    }

    /// A refused undo naming these paths.
    #[must_use]
    pub fn refused(refusal: Refusal, paths: impl IntoIterator<Item = String>) -> Self {
        Self::Refused {
            refusal,
            paths: paths.into_iter().collect(),
        }
    }
}

impl From<UndoReport> for StepOutcome {
    fn from(report: UndoReport) -> Self {
        match report {
            UndoReport::Restored { paths } => Self::completed(paths),
            UndoReport::Refused { refusal, paths } => Self {
                reported: paths,
                result: StepResult::Refused(refusal),
                operation_id: None,
            },
        }
    }
}

// -------------------------------------------------------------------------------------------
// The round-trip harness
// -------------------------------------------------------------------------------------------

/// One recorded lifecycle step with the tree before and after it.
#[derive(Debug, Clone)]
pub struct RecordedStep {
    /// Which step ran.
    pub step: MigrationStep,
    /// Capture taken immediately before the step ran.
    pub before: TreeSnapshot,
    /// Capture taken immediately after the step ran.
    pub after: TreeSnapshot,
    /// Everything the step changed on disk.
    pub diff: TreeDiff,
    /// What the step reported.
    pub outcome: StepOutcome,
}

impl RecordedStep {
    /// Whether the step reported success.
    #[must_use]
    pub fn completed(&self) -> bool {
        self.outcome.result == StepResult::Completed
    }

    /// The refusal, when the step refused.
    #[must_use]
    pub fn refusal(&self) -> Option<&Refusal> {
        match &self.outcome.result {
            StepResult::Refused(refusal) => Some(refusal),
            _ => None,
        }
    }

    /// The failure message, when the step failed.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match &self.outcome.result {
            StepResult::Failed(message) => Some(message),
            _ => None,
        }
    }
}

/// Records migration steps and answers the safety questions about them.
///
/// [`Self::begin`] captures the baseline; [`Self::record`] captures the tree before and after
/// each step, so a step's diff never includes changes made between steps (an external edit, for
/// example, belongs to neither step).
///
/// ```
/// # #[cfg(feature = "testkit")]
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use at_migrate::testkit::{
///     DryRunReport, MigrationFixture, MigrationStep, RoundTrip,
/// };
/// use at_model::LinkMode;
///
/// let fixture = MigrationFixture::new(LinkMode::Copy)?;
/// let mut trip = RoundTrip::begin(fixture.filter())?;
/// trip.record(MigrationStep::DryRun, || Ok::<_, at_migrate::testkit::HarnessError>(DryRunReport::new(Vec::new())))?;
/// trip.ensure_step_untouched(MigrationStep::DryRun)?;
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "testkit"))]
/// # fn main() {}
/// ```
#[derive(Debug)]
pub struct RoundTrip {
    filter: SnapshotFilter,
    baseline: TreeSnapshot,
    steps: Vec<RecordedStep>,
}

impl RoundTrip {
    /// Captures the baseline and starts recording.
    ///
    /// # Errors
    /// See [`SnapshotFilter::capture`].
    pub fn begin(filter: SnapshotFilter) -> Result<Self, HarnessError> {
        let baseline = filter.capture()?;
        Ok(Self {
            filter,
            baseline,
            steps: Vec::new(),
        })
    }

    /// The filter this trip captures with.
    #[must_use]
    pub fn filter(&self) -> &SnapshotFilter {
        &self.filter
    }

    /// The tree as it was before the first step.
    #[must_use]
    pub fn baseline(&self) -> &TreeSnapshot {
        &self.baseline
    }

    /// The tree as of the last recorded step, or the baseline when nothing ran.
    #[must_use]
    pub fn current(&self) -> &TreeSnapshot {
        self.steps.last().map_or(&self.baseline, |step| &step.after)
    }

    /// Every recorded step, in order.
    #[must_use]
    pub fn steps(&self) -> &[RecordedStep] {
        &self.steps
    }

    /// Recorded steps of one kind, in order.
    pub fn records(&self, step: MigrationStep) -> impl DoubleEndedIterator<Item = &RecordedStep> {
        self.steps.iter().filter(move |record| record.step == step)
    }

    /// The last recorded step of one kind.
    #[must_use]
    pub fn last_record(&self, step: MigrationStep) -> Option<&RecordedStep> {
        self.records(step).next_back()
    }

    /// The last recorded step of any kind.
    #[must_use]
    pub fn last(&self) -> Option<&RecordedStep> {
        self.steps.last()
    }

    /// Runs `run` under a snapshot window and records what it reported and changed.
    ///
    /// A driver error is recorded as [`StepResult::Failed`] rather than returned: the sad paths
    /// (an injected failure, a refused undo) are what these tests observe. Snapshot failures are
    /// returned.
    ///
    /// # Errors
    /// See [`SnapshotFilter::capture`].
    pub fn record<T, E>(
        &mut self,
        step: MigrationStep,
        run: impl FnOnce() -> Result<T, E>,
    ) -> Result<(), HarnessError>
    where
        T: Into<StepOutcome>,
        E: fmt::Display,
    {
        let before = self.filter.capture()?;
        let outcome = match run() {
            Ok(report) => report.into(),
            Err(error) => StepOutcome {
                reported: Vec::new(),
                result: StepResult::Failed(error.to_string()),
                operation_id: None,
            },
        };
        let after = self.filter.capture()?;
        let diff = before.diff(&after);
        self.steps.push(RecordedStep {
            step,
            before,
            after,
            diff,
            outcome,
        });
        Ok(())
    }

    /// Fails unless every recorded occurrence of `step` left the tree byte-identical.
    ///
    /// # Errors
    /// [`HarnessError::MissingStep`] when the step never ran, and
    /// [`HarnessError::TreeChanged`] naming every path it changed.
    pub fn ensure_step_untouched(&self, step: MigrationStep) -> Result<(), HarnessError> {
        let mut seen = false;
        for record in self.records(step) {
            seen = true;
            record
                .diff
                .ensure_empty(&format!("the {step} step must not touch the tree"))?;
        }
        if seen {
            Ok(())
        } else {
            Err(HarnessError::MissingStep { step })
        }
    }

    /// Fails unless the dry run wrote nothing and reported exactly the paths apply changed.
    ///
    /// Bookkeeping paths (backups, journals, index, logs) are excluded from the comparison;
    /// everything else must match exactly, including created directories.
    ///
    /// # Errors
    /// [`HarnessError::MissingStep`], [`HarnessError::StepFailed`],
    /// [`HarnessError::StepNotCompleted`], [`HarnessError::TreeChanged`] when the dry run wrote
    /// something, and [`HarnessError::DryRunMismatch`] when the path sets differ.
    pub fn ensure_dry_run_matched_apply(&self) -> Result<(), HarnessError> {
        let dry = self.require_completed(MigrationStep::DryRun)?;
        dry.diff
            .ensure_empty("a dry run must leave the tree byte-identical")?;
        let apply = self.require_completed(MigrationStep::Apply)?;
        let observed = apply.diff.paths_excluding(self.filter.bookkeeping());
        let reported: BTreeSet<String> = dry.outcome.reported.iter().cloned().collect();
        let missing: Vec<String> = observed.difference(&reported).cloned().collect();
        let unexpected: Vec<String> = reported.difference(&observed).cloned().collect();
        if missing.is_empty() && unexpected.is_empty() {
            Ok(())
        } else {
            Err(HarnessError::DryRunMismatch {
                missing,
                unexpected,
            })
        }
    }

    /// Fails unless the tree after undo equals the baseline, bookkeeping aside.
    ///
    /// # Errors
    /// [`HarnessError::MissingStep`], [`HarnessError::StepNotCompleted`] and
    /// [`HarnessError::TreeChanged`] listing what the undo did not restore.
    pub fn ensure_undo_restored(&self) -> Result<(), HarnessError> {
        self.require_completed(MigrationStep::Undo)?;
        self.baseline
            .diff(self.current())
            .excluding(self.filter.bookkeeping())
            .ensure_empty("undo must restore the baseline")
    }

    /// Fails unless the apply step failed and left the opening tree as it found it.
    ///
    /// # Errors
    /// [`HarnessError::MissingStep`], [`HarnessError::StepNotFailed`] when apply succeeded, and
    /// [`HarnessError::TreeChanged`] naming the paths that were not rolled back.
    pub fn ensure_failed_apply_rolled_back(&self) -> Result<(), HarnessError> {
        let apply = self
            .last_record(MigrationStep::Apply)
            .ok_or(HarnessError::MissingStep {
                step: MigrationStep::Apply,
            })?;
        if !matches!(apply.outcome.result, StepResult::Failed(_)) {
            return Err(HarnessError::StepNotFailed {
                step: MigrationStep::Apply,
            });
        }
        apply
            .diff
            .excluding(self.filter.bookkeeping())
            .ensure_empty("a failed apply must roll back every touched path")
    }

    /// Fails unless `step` last refused with `refusal` and changed no payload path.
    ///
    /// # Errors
    /// [`HarnessError::MissingStep`], [`HarnessError::WrongRefusal`], and
    /// [`HarnessError::TreeChanged`] when the refusal still wrote something.
    pub fn ensure_refused(
        &self,
        step: MigrationStep,
        refusal: &Refusal,
    ) -> Result<(), HarnessError> {
        let record = self.require_record(step)?;
        record
            .diff
            .excluding(self.filter.bookkeeping())
            .ensure_empty("a refusal must not change the tree")?;
        match &record.outcome.result {
            StepResult::Refused(found) if found == refusal => Ok(()),
            other => Err(HarnessError::WrongRefusal {
                step,
                expected: refusal.to_string(),
                found: other.to_string(),
            }),
        }
    }

    /// Fails unless the last `step` refusal named `path`.
    ///
    /// # Errors
    /// [`HarnessError::MissingStep`], [`HarnessError::WrongRefusal`] when the step did not
    /// refuse, and [`HarnessError::RefusalMissingPath`].
    pub fn ensure_refusal_named(
        &self,
        step: MigrationStep,
        path: &str,
    ) -> Result<(), HarnessError> {
        let record = self.require_record(step)?;
        if record.refusal().is_none() {
            return Err(HarnessError::WrongRefusal {
                step,
                expected: "a refusal".to_owned(),
                found: record.outcome.result.to_string(),
            });
        }
        if record
            .outcome
            .reported
            .iter()
            .any(|reported| reported == path)
        {
            Ok(())
        } else {
            Err(HarnessError::RefusalMissingPath {
                step,
                path: path.to_owned(),
            })
        }
    }

    /// Fails unless a second undo changed nothing.
    ///
    /// Idempotency is about the tree, not the outcome: a second undo may refuse ("already
    /// undone") or restore nothing, but it must never corrupt a payload path.
    ///
    /// # Errors
    /// [`HarnessError::MissingStep`] when undo ran fewer than twice, and
    /// [`HarnessError::TreeChanged`] naming what the second undo changed.
    pub fn ensure_undo_idempotent(&self) -> Result<(), HarnessError> {
        let undos: Vec<&RecordedStep> = self.records(MigrationStep::Undo).collect();
        let second = undos.get(1).ok_or(HarnessError::MissingStep {
            step: MigrationStep::Undo,
        })?;
        second
            .diff
            .excluding(self.filter.bookkeeping())
            .ensure_empty("a second undo must change nothing")
    }

    fn require_record(&self, step: MigrationStep) -> Result<&RecordedStep, HarnessError> {
        self.last_record(step)
            .ok_or(HarnessError::MissingStep { step })
    }

    fn require_completed(&self, step: MigrationStep) -> Result<&RecordedStep, HarnessError> {
        let record = self.require_record(step)?;
        match &record.outcome.result {
            StepResult::Completed => Ok(record),
            StepResult::Failed(message) => Err(HarnessError::StepFailed {
                step,
                message: message.clone(),
            }),
            other => Err(HarnessError::StepNotCompleted {
                step,
                found: other.to_string(),
            }),
        }
    }
}

/// Why the harness could not observe what a test asked for.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HarnessError {
    /// A file or directory could not be read.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// A snapshot root is absent or is not a directory.
    #[error("snapshot root {} is not an existing directory", .path.display())]
    MissingRoot {
        /// Root that could not be walked.
        path: PathBuf,
    },
    /// A comparison found differences.
    #[error("{context}: the tree changed:\n{diff}")]
    TreeChanged {
        /// What was being checked.
        context: String,
        /// One line per differing path.
        diff: String,
    },
    /// A step the check needs was never recorded.
    #[error("no {step} step was recorded")]
    MissingStep {
        /// Step that is missing.
        step: MigrationStep,
    },
    /// A step that had to succeed failed.
    #[error("the {step} step failed: {message}")]
    StepFailed {
        /// Step that failed.
        step: MigrationStep,
        /// Driver error text.
        message: String,
    },
    /// A step that had to succeed did not complete.
    #[error("the {step} step did not complete; it reported {found}")]
    StepNotCompleted {
        /// Step in question.
        step: MigrationStep,
        /// What the step reported instead.
        found: String,
    },
    /// A step that had to fail succeeded.
    #[error("the {step} step succeeded although the test expected it to fail")]
    StepNotFailed {
        /// Step in question.
        step: MigrationStep,
    },
    /// The dry run's path list and the paths apply changed disagree.
    #[error(
        "apply changed {missing:?} that the dry run did not report; the dry run reported \
         {unexpected:?} that apply did not change"
    )]
    DryRunMismatch {
        /// Paths apply changed but the dry run did not report.
        missing: Vec<String>,
        /// Paths the dry run reported but apply did not change.
        unexpected: Vec<String>,
    },
    /// A step refused for a different reason than the test expected.
    #[error("the {step} step reported {found}, expected {expected}")]
    WrongRefusal {
        /// Step in question.
        step: MigrationStep,
        /// Refusal the test expected.
        expected: String,
        /// What the step reported.
        found: String,
    },
    /// A refusal did not name a path the test expected it to name.
    #[error("the {step} refusal did not name {path}")]
    RefusalMissingPath {
        /// Step in question.
        step: MigrationStep,
        /// Path the test expected in the refusal.
        path: String,
    },
}

/// The migration lifecycle under test, projected into the harness's report types.
///
/// TC-006 and TC-009 will implement this for the real engine (or a test adapter around it);
/// the harness only needs the path lists.
pub trait MigrationUnderTest {
    /// Error type of the code under test.
    type Error: fmt::Display;

    /// Discovers native items.
    ///
    /// # Errors
    /// Whatever the code under test returns.
    fn scan(&mut self) -> Result<ScanReport, Self::Error>;

    /// Computes the plan without touching anything.
    ///
    /// # Errors
    /// Whatever the code under test returns.
    fn plan(&mut self) -> Result<PlanReport, Self::Error>;

    /// Rehearses the plan without writing.
    ///
    /// # Errors
    /// Whatever the code under test returns.
    fn dry_run(&mut self) -> Result<DryRunReport, Self::Error>;

    /// Applies the plan.
    ///
    /// # Errors
    /// Whatever the code under test returns.
    fn apply(&mut self) -> Result<ApplyReport, Self::Error>;

    /// Undoes the applied operation.
    ///
    /// # Errors
    /// Whatever the code under test returns.
    fn undo(&mut self) -> Result<UndoReport, Self::Error>;
}

/// Drives scan, plan, dry run, apply and undo in that order, snapshotting each.
///
/// Every step is recorded even if an earlier one failed: undo after a failed apply is one of
/// the cases a migration test has to cover.
///
/// # Errors
/// See [`SnapshotFilter::capture`].
pub fn run_round_trip<M: MigrationUnderTest>(
    filter: SnapshotFilter,
    engine: &mut M,
) -> Result<RoundTrip, HarnessError> {
    let mut trip = RoundTrip::begin(filter)?;
    trip.record(MigrationStep::Scan, || engine.scan())?;
    trip.record(MigrationStep::Plan, || engine.plan())?;
    trip.record(MigrationStep::DryRun, || engine.dry_run())?;
    trip.record(MigrationStep::Apply, || engine.apply())?;
    trip.record(MigrationStep::Undo, || engine.undo())?;
    Ok(trip)
}
