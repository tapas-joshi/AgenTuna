//! Shared fixtures for tests: a throwaway home directory, fake agent installations and
//! filesystem snapshots.
//!
//! The module is compiled only with the `testkit` cargo feature. Add it as a dev-dependency:
//!
//! ```toml
//! [dev-dependencies]
//! at-fs = { workspace = true, features = ["testkit"] }
//! ```
//!
//! # Isolation
//!
//! [`crate::testkit::TestHome::new`] creates a unique temporary directory per call and never touches the real
//! home directory. Tests that inject paths directly (the normal case) need nothing else and can
//! run in parallel:
//!
//! ```
//! # #[cfg(feature = "testkit")]
//! # fn main() -> Result<(), at_fs::testkit::TestkitError> {
//! use at_fs::testkit::TestHome;
//!
//! let home = TestHome::new()?;
//! let config = home.write(".claude.json", b"{}")?;
//! assert!(config.starts_with(home.home()));
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "testkit"))]
//! # fn main() {}
//! ```
//!
//! For the few APIs that still read the process environment, [`crate::testkit::TestHome::with_env`] additionally
//! installs an [`crate::testkit::EnvScope`]: it sets `HOME` and the platform config/data variables under a
//! process-wide lock and restores the previous values when it is dropped, including while a test
//! unwinds from a panic.
//!
//! # Store paths
//!
//! `at-fs` sits below `at-store`, so this module cannot name `at_store::AppPaths` without a
//! dependency cycle. [`crate::testkit::TestHome::paths_with`] takes the resolver instead:
//!
//! ```
//! # #[cfg(feature = "testkit")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # use at_fs::testkit::TestHome;
//! # use at_store::paths::{AppPaths, PathEnvironment};
//! let home = TestHome::new()?;
//! let paths = home.paths_with(|home| {
//!     AppPaths::resolve(&PathEnvironment::default().with_home(home))
//! })?;
//! assert_eq!(paths.user_store(), home.home().join(".agentuna"));
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "testkit"))]
//! # fn main() {}
//! ```
//!
//! Project fixtures carry their own root, which is what a caller adds to the same environment:
//!
//! ```
//! # #[cfg(feature = "testkit")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # use at_fs::testkit::TestHome;
//! # use at_store::paths::{AppPaths, PathEnvironment};
//! let home = TestHome::new()?;
//! let project = home.project("acme")?;
//! let paths = home.paths_with(|home| {
//!     AppPaths::resolve(
//!         &PathEnvironment::default()
//!             .with_home(home)
//!             .with_project_root(project.root()),
//!     )
//! })?;
//! assert!(paths.project_store().is_some());
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "testkit"))]
//! # fn main() {}
//! ```

pub mod generative;

/// Skips every fsync the safe-write pipeline makes for the rest of the process.
///
/// This is the only place the switch can be reached: it exists only with the `testkit`
/// cargo feature, which crates list as a dev-dependency. A release build compiles the
/// switch out, so production durability cannot be disabled.
#[cfg(feature = "testkit")]
pub fn set_fast_sync(enabled: bool) {
    crate::durability::set_fast_sync(enabled);
}

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use at_model::{AgentId, AgentIdError};
use tempfile::{Builder, TempDir};

use crate::hash::sha256_hex;

/// Name of the sample skill every installer writes.
pub const SAMPLE_SKILL: &str = "example-skill";

/// Name of the sample MCP server every installer writes.
pub const SAMPLE_SERVER: &str = "example";

/// Environment variables [`crate::testkit::EnvScope`] owns.
pub const SCOPED_ENV_VARS: [&str; 5] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "APPDATA",
    "LOCALAPPDATA",
];

/// Why a fixture could not be created.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TestkitError {
    /// A fixture file or directory could not be created, read or removed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// A custom agent name is not a valid [`at_model::AgentId`].
    #[error(transparent)]
    Agent(#[from] AgentIdError),
}

impl TestkitError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn scoped_paths(home: &Path) -> [(&'static str, PathBuf); 5] {
    [
        ("HOME", home.to_path_buf()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("APPDATA", home.join("AppData/Roaming")),
        ("LOCALAPPDATA", home.join("AppData/Local")),
    ]
}

/// Process environment pointed at a temporary home.
///
/// The variables are set while the guard is alive and restored when it is dropped, including
/// during unwinding. Two scopes in one process are serialised: the second [`EnvScope::new`]
/// blocks until the first is dropped.
///
/// The guard holds a mutex guard internally, so it is not `Send`. Create it inside the test (or
/// the thread) that needs it.
pub struct EnvScope {
    guard: Option<env_lock::EnvGuard<'static>>,
    home: PathBuf,
}

impl EnvScope {
    /// Points the process environment at `home` and keeps it there until the guard is dropped.
    ///
    /// Creates `<home>/.config`, `<home>/.local/share`, `<home>/AppData/Roaming` and
    /// `<home>/AppData/Local` so the platform directory crates resolve inside the fixture.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when one of those directories cannot be created.
    pub fn new(home: impl AsRef<Path>) -> Result<Self, TestkitError> {
        let home = home.as_ref();
        let scoped = scoped_paths(home);
        for (_, dir) in &scoped {
            fs::create_dir_all(dir).map_err(|source| TestkitError::io(dir, source))?;
        }
        let values: Vec<(&'static str, Option<String>)> = scoped
            .iter()
            .map(|(name, path)| (*name, Some(path.to_string_lossy().into_owned())))
            .collect();
        let guard = env_lock::lock_env(values);
        Ok(Self {
            guard: Some(guard),
            home: home.to_path_buf(),
        })
    }

    /// The home directory the environment points at.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Whether the guard still owns the environment.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.guard.is_some()
    }

    /// Restores the previous environment now instead of at the end of the scope.
    pub fn release(mut self) {
        self.guard.take();
    }
}

impl fmt::Debug for EnvScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvScope")
            .field("home", &self.home)
            .field("active", &self.guard.is_some())
            .finish()
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        // Dropping the guard restores every variable, even while unwinding from a panic.
        self.guard.take();
    }
}

/// A temporary home directory plus the fixtures installed into it.
#[derive(Debug)]
pub struct TestHome {
    /// Owned only so the tree is removed on drop.
    #[allow(dead_code)]
    dir: TempDir,
    root: PathBuf,
    home: PathBuf,
    env: Option<EnvScope>,
    installed: Mutex<Vec<AgentFixture>>,
    projects: Mutex<Vec<PathBuf>>,
}

impl TestHome {
    /// Creates a unique temporary home. The process environment is not touched.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when the temporary directory cannot be created.
    pub fn new() -> Result<Self, TestkitError> {
        let dir = Builder::new()
            .prefix("agentuna-testhome-")
            .tempdir()
            .map_err(|source| TestkitError::io(env::temp_dir(), source))?;
        let root =
            fs::canonicalize(dir.path()).map_err(|source| TestkitError::io(dir.path(), source))?;
        let home = root.join("home");
        fs::create_dir_all(&home).map_err(|source| TestkitError::io(&home, source))?;
        Ok(Self {
            dir,
            root,
            home,
            env: None,
            installed: Mutex::new(Vec::new()),
            projects: Mutex::new(Vec::new()),
        })
    }

    /// A home with no agent configuration at all.
    ///
    /// This is the baseline for detection tests: every path in [`Self::agent_roots`] is absent
    /// and [`Self::installed_agents`] is empty, so an adapter's `detect_all()` has to find nothing
    /// and a store scan has to find no items.
    ///
    /// # Errors
    /// Same as [`Self::new`].
    pub fn empty_home() -> Result<Self, TestkitError> {
        Self::new()
    }

    /// A home that also replaces the process environment with an [`crate::testkit::EnvScope`].
    ///
    /// Blocks until no other [`crate::testkit::EnvScope`] in the process is alive.
    ///
    /// # Errors
    /// Same as [`Self::new`], plus the failures of [`EnvScope::new`].
    pub fn with_env() -> Result<Self, TestkitError> {
        let home = Self::new()?;
        let scope = EnvScope::new(&home.home)?;
        Ok(Self {
            env: Some(scope),
            ..home
        })
    }

    /// The home directory (`<temp>/home`), already canonicalised.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The canonical root temporary directory that owns the home and every project fixture.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The environment scope, when [`Self::with_env`] was used.
    #[must_use]
    pub fn env_scope(&self) -> Option<&EnvScope> {
        self.env.as_ref()
    }

    /// Resolves a path relative to the home; absolute paths are returned unchanged.
    #[must_use]
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        let relative = relative.as_ref();
        if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            self.home.join(relative)
        }
    }

    /// Writes a file below the home, creating parent directories.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when the parents or the file cannot be written.
    pub fn write(
        &self,
        relative: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<PathBuf, TestkitError> {
        let path = self.path(relative);
        self.write_at(&path, contents)?;
        Ok(path)
    }

    /// Reads a file below the home as UTF-8.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when the file cannot be read.
    pub fn read(&self, relative: impl AsRef<Path>) -> Result<String, TestkitError> {
        let path = self.path(relative);
        fs::read_to_string(&path).map_err(|source| TestkitError::io(&path, source))
    }

    /// Creates a directory below the home.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when the directory cannot be created.
    pub fn mkdir(&self, relative: impl AsRef<Path>) -> Result<PathBuf, TestkitError> {
        let path = self.path(relative);
        fs::create_dir_all(&path).map_err(|source| TestkitError::io(&path, source))?;
        Ok(path)
    }

    /// Builds the caller's path set rooted at this home.
    ///
    /// See the [module documentation](self) for the reason this is a closure rather than a
    /// `TestHome::paths()` accessor: `at-store` depends on `at-fs`, so the fixture cannot name
    /// `at_store::AppPaths` without a cycle.
    pub fn paths_with<T>(&self, resolve: impl FnOnce(&Path) -> T) -> T {
        resolve(&self.home)
    }

    /// Every home-level path the installers below may create.
    #[must_use]
    pub fn agent_roots(&self) -> Vec<PathBuf> {
        [
            ".claude",
            ".claude.json",
            ".codex",
            ".agents",
            ".omp",
            ".cursor",
        ]
        .iter()
        .map(|name| self.home.join(name))
        .collect()
    }

    /// Identities of the fixtures installed so far.
    #[must_use]
    pub fn installed_agents(&self) -> Vec<AgentId> {
        lock(&self.installed)
            .iter()
            .map(|fixture| fixture.agent.clone())
            .collect()
    }

    /// The fixtures installed so far.
    #[must_use]
    pub fn installed(&self) -> Vec<AgentFixture> {
        lock(&self.installed).clone()
    }

    /// Roots of the project fixtures created so far.
    #[must_use]
    pub fn registered_projects(&self) -> Vec<PathBuf> {
        lock(&self.projects).clone()
    }

    /// Installs a Claude Code home: `~/.claude/skills/<skill>/SKILL.md`, `~/.claude.json` with
    /// one `mcpServers` entry and `~/.claude/CLAUDE.md`.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when a file cannot be written.
    pub fn install_claude_code(&self) -> Result<AgentFixture, TestkitError> {
        let skill_path = self
            .home
            .join(".claude/skills")
            .join(SAMPLE_SKILL)
            .join("SKILL.md");
        self.write_at(&skill_path, skill_doc(SAMPLE_SKILL))?;
        let mcp_path = self.home.join(".claude.json");
        self.write_at(&mcp_path, mcp_json(SAMPLE_SERVER))?;
        let rules_path = self.home.join(".claude/CLAUDE.md");
        self.write_at(&rules_path, rules_doc("Claude Code"))?;
        Ok(self.record(
            AgentId::ClaudeCode,
            &self.home,
            skill_path,
            mcp_path,
            Some(rules_path),
        ))
    }

    /// Installs a Codex home: `~/.codex/config.toml` with one `[mcp_servers.<name>]` table,
    /// `~/.agents/skills/<skill>/SKILL.md` (the root shared with other tools) and
    /// `~/.codex/AGENTS.md`.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when a file cannot be written.
    pub fn install_codex(&self) -> Result<AgentFixture, TestkitError> {
        let skill_path = self
            .home
            .join(".agents/skills")
            .join(SAMPLE_SKILL)
            .join("SKILL.md");
        self.write_at(&skill_path, skill_doc(SAMPLE_SKILL))?;
        let mcp_path = self.home.join(".codex/config.toml");
        self.write_at(&mcp_path, codex_config(SAMPLE_SERVER))?;
        let rules_path = self.home.join(".codex/AGENTS.md");
        self.write_at(&rules_path, rules_doc("Codex"))?;
        Ok(self.record(
            AgentId::Codex,
            &self.home.join(".codex"),
            skill_path,
            mcp_path,
            Some(rules_path),
        ))
    }

    /// Installs an omp home: `~/.omp/agent/mcp.json`, `~/.omp/agent/skills/<skill>/SKILL.md`
    /// and `~/.omp/agent/AGENTS.md`.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when a file cannot be written.
    pub fn install_omp(&self) -> Result<AgentFixture, TestkitError> {
        let root = self.home.join(".omp/agent");
        let skill_path = root.join("skills").join(SAMPLE_SKILL).join("SKILL.md");
        self.write_at(&skill_path, skill_doc(SAMPLE_SKILL))?;
        let mcp_path = root.join("mcp.json");
        self.write_at(&mcp_path, omp_mcp_json(SAMPLE_SERVER))?;
        let rules_path = root.join("AGENTS.md");
        self.write_at(&rules_path, rules_doc("Oh My Pi"))?;
        Ok(self.record(AgentId::Omp, &root, skill_path, mcp_path, Some(rules_path)))
    }

    /// Installs a Cursor home: `~/.cursor/mcp.json`, `~/.cursor/skills/<skill>/SKILL.md` and
    /// `~/.cursor/rules/<skill>.mdc`.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when a file cannot be written.
    pub fn install_cursor(&self) -> Result<AgentFixture, TestkitError> {
        let root = self.home.join(".cursor");
        let skill_path = root.join("skills").join(SAMPLE_SKILL).join("SKILL.md");
        self.write_at(&skill_path, skill_doc(SAMPLE_SKILL))?;
        let mcp_path = root.join("mcp.json");
        self.write_at(&mcp_path, mcp_json(SAMPLE_SERVER))?;
        let rules_path = root.join("rules").join(format!("{SAMPLE_SKILL}.mdc"));
        self.write_at(&rules_path, cursor_rule(SAMPLE_SKILL))?;
        Ok(self.record(
            AgentId::Cursor,
            &root,
            skill_path,
            mcp_path,
            Some(rules_path),
        ))
    }

    /// Installs a user-defined agent under `root` (relative roots live below the home) with the
    /// same layout the built-in fixtures use.
    ///
    /// # Errors
    /// [`TestkitError::Agent`] when `name` is not a valid custom agent id, and
    /// [`crate::testkit::TestkitError::Io`] when a file cannot be written.
    pub fn install_custom_agent(
        &self,
        name: &str,
        root: impl AsRef<Path>,
    ) -> Result<AgentFixture, TestkitError> {
        let agent = AgentId::custom(name)?;
        let root = self.path(root);
        let skill_path = root.join("skills").join(SAMPLE_SKILL).join("SKILL.md");
        self.write_at(&skill_path, skill_doc(SAMPLE_SKILL))?;
        let mcp_path = root.join("mcp.json");
        self.write_at(&mcp_path, mcp_json(SAMPLE_SERVER))?;
        let rules_path = root.join("AGENTS.md");
        self.write_at(&rules_path, rules_doc(name))?;
        Ok(self.record(agent, &root, skill_path, mcp_path, Some(rules_path)))
    }

    /// Creates a project fixture and registers its root.
    ///
    /// The fixture holds `.mcp.json`, `.claude/settings.local.json` (JSONC), `CLAUDE.md`,
    /// `AGENTS.md` and one skill under `.agents/skills/`.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when the fixture cannot be written.
    pub fn project(&self, name: &str) -> Result<ProjectFixture, TestkitError> {
        let projects = self.path("projects");
        fs::create_dir_all(&projects).map_err(|source| TestkitError::io(&projects, source))?;
        let prefix = format!("{}-", sanitise(name));
        let dir = Builder::new()
            .prefix(&prefix)
            .tempdir_in(&projects)
            .map_err(|source| TestkitError::io(&projects, source))?;
        let root =
            fs::canonicalize(dir.path()).map_err(|source| TestkitError::io(dir.path(), source))?;
        let fixture = ProjectFixture {
            dir,
            root: root.clone(),
            name: name.to_owned(),
            skill_name: SAMPLE_SKILL.to_owned(),
        };
        let mut files = vec![
            (root.join(".mcp.json"), mcp_json(SAMPLE_SERVER)),
            (root.join(".claude/settings.local.json"), project_settings()),
            (root.join("CLAUDE.md"), rules_doc("Claude Code")),
            (root.join("AGENTS.md"), rules_doc("Agents")),
            (
                root.join(".agents/skills")
                    .join(SAMPLE_SKILL)
                    .join("SKILL.md"),
                skill_doc(SAMPLE_SKILL),
            ),
        ];
        for (path, contents) in files.drain(..) {
            self.write_at(&path, contents)?;
        }
        lock(&self.projects).push(root);
        Ok(fixture)
    }

    /// Sorted relative paths below `dir` mapped to their SHA-256.
    ///
    /// # Errors
    /// [`crate::testkit::TestkitError::Io`] when the tree cannot be read.
    pub fn snapshot(
        &self,
        dir: impl AsRef<Path>,
    ) -> Result<BTreeMap<String, String>, TestkitError> {
        snapshot(dir)
    }

    fn write_at(&self, path: &Path, contents: impl AsRef<[u8]>) -> Result<(), TestkitError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| TestkitError::io(parent, source))?;
        }
        fs::write(path, contents.as_ref()).map_err(|source| TestkitError::io(path, source))
    }

    fn record(
        &self,
        agent: AgentId,
        root: &Path,
        skill_path: PathBuf,
        mcp_path: PathBuf,
        rules_path: Option<PathBuf>,
    ) -> AgentFixture {
        let fixture = AgentFixture {
            agent,
            root: root.to_path_buf(),
            skill_name: SAMPLE_SKILL.to_owned(),
            skill_path,
            mcp_name: SAMPLE_SERVER.to_owned(),
            mcp_path,
            rules_path,
        };
        lock(&self.installed).push(fixture.clone());
        fixture
    }
}

/// One installed agent, with the paths a test needs to assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFixture {
    /// Agent identity the fixture imitates.
    pub agent: AgentId,
    /// Configuration root the fixture wrote into.
    pub root: PathBuf,
    /// Name of the skill the fixture installed.
    pub skill_name: String,
    /// Path of the installed `SKILL.md`.
    pub skill_path: PathBuf,
    /// Name of the MCP server the fixture configured.
    pub mcp_name: String,
    /// Path of the file holding that MCP server.
    pub mcp_path: PathBuf,
    /// Path of the rules file the fixture wrote, when the agent has one.
    pub rules_path: Option<PathBuf>,
}

/// A project directory fixture owned by the [`TestHome`] that created it.
#[derive(Debug)]
pub struct ProjectFixture {
    dir: TempDir,
    root: PathBuf,
    name: String,
    skill_name: String,
}

impl ProjectFixture {
    /// The name passed to [`crate::testkit::TestHome::project`].
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Canonical project root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The temporary directory that owns the project tree.
    #[must_use]
    pub fn temp_dir(&self) -> &Path {
        self.dir.path()
    }

    /// Name of the skill installed under `.agents/skills`.
    #[must_use]
    pub fn skill_name(&self) -> &str {
        &self.skill_name
    }

    /// Path of the project's `SKILL.md`.
    #[must_use]
    pub fn skill_path(&self) -> PathBuf {
        self.root
            .join(".agents/skills")
            .join(&self.skill_name)
            .join("SKILL.md")
    }

    /// The project store directory the application will create (`<root>/.agentuna`), which does
    /// not exist until something writes to it.
    #[must_use]
    pub fn store(&self) -> PathBuf {
        self.root.join(".agentuna")
    }
}

/// Sorted relative paths below `dir` mapped to their SHA-256.
///
/// Directories are skipped. Regular files are hashed by content. Symbolic links are hashed by
/// their target path and are never followed, so a link pointing outside `dir` cannot pull foreign
/// content into the snapshot.
///
/// # Errors
/// [`crate::testkit::TestkitError::Io`] when the tree cannot be read.
pub fn snapshot(dir: impl AsRef<Path>) -> Result<BTreeMap<String, String>, TestkitError> {
    let dir = dir.as_ref();
    let mut entries = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let children =
            fs::read_dir(&current).map_err(|source| TestkitError::io(&current, source))?;
        for child in children {
            let child = child.map_err(|source| TestkitError::io(&current, source))?;
            let path = child.path();
            let meta =
                fs::symlink_metadata(&path).map_err(|source| TestkitError::io(&path, source))?;
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                let target =
                    fs::read_link(&path).map_err(|source| TestkitError::io(&path, source))?;
                entries.insert(
                    relative_key(dir, &path),
                    sha256_hex(target.as_os_str().as_encoded_bytes()),
                );
            } else if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let bytes = fs::read(&path).map_err(|source| TestkitError::io(&path, source))?;
                entries.insert(relative_key(dir, &path), sha256_hex(&bytes));
            }
        }
    }
    Ok(entries)
}

fn relative_key(dir: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(dir).unwrap_or(path);
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn sanitise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            out.push(character);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "project".to_owned()
    } else {
        out
    }
}

fn skill_doc(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: Fixture skill written by at_fs::testkit\n---\n\n# {name}\n\nSteps the agent follows when this skill is active.\n"
    )
}

fn rules_doc(title: &str) -> String {
    format!(
        "# {title}\n\n- Prefer small, reviewed changes.\n- Never write secrets to a configuration file.\n"
    )
}

fn mcp_json(name: &str) -> String {
    format!(
        "{{\n  \"mcpServers\": {{\n    \"{name}\": {{\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"@example/mcp-server\"],\n      \"env\": {{}}\n    }}\n  }}\n}}\n"
    )
}

fn omp_mcp_json(name: &str) -> String {
    format!(
        "{{\n  \"$schema\": \"https://example.invalid/omp-mcp.schema.json\",\n  \"mcpServers\": {{\n    \"{name}\": {{\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"@example/mcp-server\"]\n    }}\n  }}\n}}\n"
    )
}

fn codex_config(name: &str) -> String {
    format!(
        "model = \"gpt-5\"\napproval_policy = \"on-request\"\n\n[mcp_servers.{name}]\ncommand = \"npx\"\nargs = [\"-y\", \"@example/mcp-server\"]\nstartup_timeout_sec = 20\n"
    )
}

fn cursor_rule(name: &str) -> String {
    format!(
        "---\ndescription: {name} fixture rule\nglobs: [\"**/*.rs\"]\n---\n\nPrefer typed errors over strings.\n"
    )
}

fn project_settings() -> String {
    "{\n  // Local overrides; this file is not committed.\n  \"permissions\": { \"allow\": [\"Bash(cargo test:*)\"] },\n  \"env\": { \"EXAMPLE_TOKEN\": \"${EXAMPLE_TOKEN}\" }\n}\n"
        .to_owned()
}

impl fmt::Display for TestHome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestHome")
            .field("home", &self.home)
            .field("installed", &self.installed_agents())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_keeps_simple_names() {
        assert_eq!(sanitise("acme"), "acme");
        assert_eq!(sanitise("acme corp/eu"), "acme-corp-eu");
        assert_eq!(sanitise(""), "project");
    }

    #[test]
    fn snapshot_keys_use_forward_slashes() {
        let home = TestHome::new().expect("home");
        home.write("a/b/c.txt", b"x").expect("write");
        let map = snapshot(home.home()).expect("snapshot");
        assert!(map.contains_key("a/b/c.txt"), "{map:?}");
    }
}
