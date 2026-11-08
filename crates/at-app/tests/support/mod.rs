//! Shared sandbox and assertions for the at-app command-layer flow tests.
//!
//! Every test builds one [`Sandbox`]: a throwaway home, support directory, user store, Global
//! store and project root with an injected path set. No real agent folder, Keychain item or
//! network endpoint is touched.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use at_app::app::flow_harness;
use at_app::app::state::AppState;
use at_db::Database;
use at_model::{Agent, AgentId, ItemId, ItemKind, Scope};
use at_store::layout::{Slug, StoreId, StoreLayout};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath, hash_skill_folder};
use at_store::paths::{AppPaths, HomeSource, PathEnvironment, PathOverrides};
use tempfile::TempDir;

/// Boxed error alias so test helpers compose with `?`.
pub type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// One injected application environment on disk.
pub struct Sandbox {
    dir: TempDir,
    /// Fake `$HOME`; agent-native folders live under it.
    pub home: PathBuf,
    /// Support directory holding the SQLite index.
    pub support: PathBuf,
    /// `~/.agentuna`, the User store root.
    pub user_store: PathBuf,
    /// Global store root.
    pub global: PathBuf,
    /// Registered project root.
    pub project: PathBuf,
}

/// One skill package written into a store with its index row and manifest entry.
pub struct InstalledSkill {
    /// Stable identity allocated by the sandbox.
    pub item_id: ItemId,
    /// Parsed slug.
    pub slug: String,
    /// Skill package directory.
    pub dir: PathBuf,
    /// Package hash recorded in the manifest.
    pub hash: String,
    /// Scope holding the package.
    pub scope: Scope,
}

impl Sandbox {
    /// Creates the empty sandbox directory tree.
    ///
    /// # Errors
    /// Propagates the temporary-directory and directory-creation failures.
    pub fn new() -> TestResult<Self> {
        let dir = TempDir::new()?;
        // Resolve `/var` -> `/private/var` once so every derived path and every path the engine
        // reports back compare as strings.
        let root = fs::canonicalize(dir.path())?;
        let home = root.join("home");
        let support = root.join("support");
        let user_store = home.join(".agentuna");
        let global = root.join("global");
        let project = root.join("project");
        for directory in [&home, &support, &global, &project] {
            fs::create_dir_all(directory)?;
        }
        Ok(Self {
            dir,
            home,
            support,
            user_store,
            global,
            project,
        })
    }

    /// Resolved application paths over the sandbox.
    ///
    /// # Errors
    /// Propagates path-resolution failures.
    pub fn paths(&self) -> TestResult<AppPaths> {
        Ok(AppPaths::resolve(&PathEnvironment {
            home: HomeSource::Injected(self.home.clone()),
            overrides: PathOverrides {
                home: Some(self.support.clone()),
                user_store: Some(self.user_store.clone()),
                project_store: None,
                global_store: Some(self.global.clone()),
            },
            project_root: Some(self.project.clone()),
        })?)
    }

    /// Application paths scoped to the sandbox project.
    ///
    /// # Errors
    /// Propagates path-resolution failures.
    pub fn project_paths(&self) -> TestResult<AppPaths> {
        Ok(self.paths()?.for_project(&self.project)?)
    }

    /// File-backed SQLite index inside the support directory.
    ///
    /// # Errors
    /// Propagates the open and migration failure.
    pub fn database(&self) -> TestResult<Database> {
        Ok(Database::open_file(self.support.join("agentuna.db"))?)
    }

    /// A fresh app state over the same index file.
    ///
    /// # Errors
    /// Propagates path resolution and database-open failures.
    pub fn state(&self) -> TestResult<AppState> {
        Ok(flow_harness::test_state(self.database()?, self.paths()?))
    }

    /// A fresh app state over the same index file with project-scoped paths.
    ///
    /// # Errors
    /// Propagates path resolution and database-open failures.
    pub fn project_state(&self) -> TestResult<AppState> {
        Ok(flow_harness::test_state(
            self.database()?,
            self.project_paths()?,
        ))
    }

    /// Store layout for one scope.
    ///
    /// # Errors
    /// Propagates layout resolution failures.
    pub fn layout(&self, scope: Scope) -> TestResult<StoreLayout> {
        let paths = match scope {
            Scope::Project | Scope::Local => self.project_paths()?,
            _ => self.paths()?,
        };
        Ok(StoreLayout::for_scope(&paths, scope)?)
    }

    /// Marks one agent installed and managed in the index.
    ///
    /// # Errors
    /// Propagates the index write failure.
    pub fn register_agent(&self, database: &Database, agent: AgentId) -> TestResult<()> {
        at_db::agents::upsert_agent(
            database.connection(),
            &Agent {
                id: agent.clone(),
                display_name: agent.to_string(),
                detected: true,
                version: Some("1.0".to_owned()),
            },
        )?;
        at_db::agents::set_agent_managed(database.connection(), &agent, true)?;
        // The app records each agent's native paths during detection; several commands derive
        // their authorized filesystem roots from those rows, so the sandbox records them too.
        for (item_kind, path_kind, format) in [
            (
                ItemKind::Skill,
                at_db::agents::AgentPathKind::SkillsDir,
                at_db::agents::AgentPathFormat::Dir,
            ),
            (
                ItemKind::McpServer,
                at_db::agents::AgentPathKind::McpFile,
                at_db::agents::AgentPathFormat::Json,
            ),
            (
                ItemKind::Rule,
                at_db::agents::AgentPathKind::RulesFile,
                at_db::agents::AgentPathFormat::Markdown,
            ),
        ] {
            let Ok(path) = self.agent_path(&agent, item_kind, Scope::User) else {
                continue;
            };
            at_db::agents::upsert_agent_path(
                database.connection(),
                &at_db::agents::AgentPathSpec {
                    agent: &agent,
                    scope: Scope::User,
                    kind: path_kind,
                    path: &path,
                    format: Some(format),
                    is_primary: true,
                    exists: true,
                },
            )?;
        }
        Ok(())
    }

    /// Creates one scope store through the reviewed Overview setup action.
    ///
    /// # Errors
    /// Propagates preview, confirmation and apply failures.
    pub fn setup_scope(
        &self,
        state: &AppState,
        scope: &str,
        project_id: Option<&str>,
    ) -> TestResult<()> {
        let preview =
            at_app::app::overview::overview_setup_preview_inner(state, scope, project_id)?;
        if preview.already_ready {
            return Ok(());
        }
        let preview_id = preview
            .preview_id
            .ok_or("a new store must come with a reviewed plan")?;
        at_app::app::overview::overview_setup_apply_inner(state, &preview_id, true)?;
        Ok(())
    }

    /// Records a secret *name* in the index without touching the OS Keychain.
    ///
    /// # Errors
    /// Propagates invalid-name and index-write failures.
    pub fn register_secret(&self, database: &Database, name: &str) -> TestResult<()> {
        let name = at_secrets::SecretName::new(name.to_owned())?;
        let keychain = at_secrets::KeychainId::new(name.as_str())?;
        at_db::secret_refs::record_reference(database.connection(), &name, &keychain)?;
        Ok(())
    }

    /// Registers the sandbox project and selects it as the active project.
    ///
    /// # Errors
    /// Propagates the index write failures.
    pub fn register_project(&self, database: &mut Database) -> TestResult<String> {
        let project = at_db::projects::add_project(
            database.connection(),
            &self.project,
            Some("Sandbox"),
            None,
        )?;
        at_db::settings::set_last_project_id(
            database.connection_mut(),
            Some(&project.id),
            at_db::ops::OperationActor::Ui,
        )?;
        Ok(project.id)
    }

    /// Writes one skill package and records it in the manifest and `items`.
    ///
    /// # Errors
    /// Propagates filesystem, manifest and index failures.
    pub fn install_skill(
        &self,
        database: &Database,
        scope: Scope,
        slug: &str,
        name: &str,
        markdown: &[u8],
        project_id: Option<&str>,
    ) -> TestResult<InstalledSkill> {
        install_skill(
            database,
            &self.layout(scope)?,
            scope,
            slug,
            name,
            markdown,
            project_id,
        )
    }

    /// Absolute skills directory an adapter deploys to, for one agent and scope.
    ///
    /// # Errors
    /// Propagates registry and adapter lookup failures.
    pub fn agent_skills_dir(&self, agent: &AgentId, scope: Scope) -> TestResult<PathBuf> {
        self.agent_path(agent, ItemKind::Skill, scope)
    }

    /// Absolute native path an adapter uses for one item kind, agent and scope.
    ///
    /// # Errors
    /// Propagates registry and adapter lookup failures.
    pub fn agent_path(&self, agent: &AgentId, kind: ItemKind, scope: Scope) -> TestResult<PathBuf> {
        use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};

        let registry = AgentAdapterRegistry::register();
        let adapter = registry
            .get(agent)?
            .ok_or_else(|| format!("no adapter registered for {agent}"))?;
        let env = AgentEnvironment {
            home: self.home.clone(),
            project: matches!(scope, Scope::Project | Scope::Local).then(|| self.project.clone()),
            variables: Default::default(),
        };
        adapter
            .paths(kind, scope, &env)
            .into_iter()
            .find(|candidate| candidate.primary)
            .map(|candidate| candidate.path)
            .ok_or_else(|| format!("{agent} has no primary {kind:?} path for {scope}").into())
    }

    /// Snapshots the user-visible tree: fake home, project and Global store.
    ///
    /// # Errors
    /// Propagates filesystem walk failures.
    pub fn snapshot(&self) -> TestResult<TreeSnapshot> {
        TreeSnapshot::capture(&[
            ("home", self.home.clone()),
            ("project", self.project.clone()),
            ("global", self.global.clone()),
        ])
    }

    /// Number of operation rows recorded so far.
    ///
    /// # Errors
    /// Propagates the query failure.
    pub fn operation_count(&self, database: &Database) -> TestResult<i64> {
        Ok(database
            .connection()
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))?)
    }

    /// Temporary root, so callers can build extra paths inside the sandbox.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The owned temporary directory, so a caller can keep a fully built sandbox alive as a
    /// template instead of letting [`TempDir`] delete it when the [`Sandbox`] drops.
    #[must_use]
    pub fn into_dir(self) -> TempDir {
        self.dir
    }

    /// Rebuilds the sandbox layout in a fresh temporary directory from `source`.
    ///
    /// Copies the four sandbox roots (`home`, `support`, `global`, `project`) byte for byte
    /// and preserves symlinks. A symlink whose target is absolute and inside `source` is
    /// re-pointed into the new root, so the copy is self-contained. The SQLite index under
    /// `support` is copied as a plain file; callers re-open it and rewrite the absolute paths
    /// the template recorded.
    ///
    /// # Errors
    /// Propagates directory, read, write and symlink failures.
    pub fn clone_tree(source: &Path) -> TestResult<Self> {
        let dir = TempDir::new()?;
        let root = fs::canonicalize(dir.path())?;
        let source_root = fs::canonicalize(source)?;
        for entry in fs::read_dir(&source_root)? {
            let entry = entry?;
            copy_node(
                &entry.path(),
                &root.join(entry.file_name()),
                &source_root,
                &root,
            )?;
        }
        Ok(Self {
            dir,
            home: root.join("home"),
            support: root.join("support"),
            user_store: root.join("home/.agentuna"),
            global: root.join("global"),
            project: root.join("project"),
        })
    }
}

/// Recursively copies one file, directory or symlink, rewriting absolute symlink targets that
/// point inside `source_root` to the matching path under `dest_root`.
fn copy_node(source: &Path, dest: &Path, source_root: &Path, dest_root: &Path) -> TestResult<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        let rewritten = match target.strip_prefix(source_root) {
            Ok(relative) => dest_root.join(relative),
            Err(_) => target,
        };
        // A copied fixture tree keeps its links on Unix; Windows fixtures are built without
        // links until the test-suite slice adds junction twins.
        #[cfg(unix)]
        std::os::unix::fs::symlink(rewritten, dest)?;
        #[cfg(not(unix))]
        {
            let _ = rewritten;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlinked fixture trees are unsupported on this platform",
            )
            .into());
        }
    } else if metadata.is_dir() {
        fs::create_dir(dest)?;
        fs::set_permissions(dest, metadata.permissions())?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_node(
                &entry.path(),
                &dest.join(entry.file_name()),
                source_root,
                dest_root,
            )?;
        }
    } else {
        fs::copy(source, dest)?;
        fs::set_permissions(dest, metadata.permissions())?;
    }
    Ok(())
}

/// Writes one skill package into `layout`, updates the manifest and the `items` row.
///
/// # Errors
/// Propagates filesystem, manifest and index failures.
pub fn install_skill(
    database: &Database,
    layout: &StoreLayout,
    scope: Scope,
    slug: &str,
    name: &str,
    markdown: &[u8],
    project_id: Option<&str>,
) -> TestResult<InstalledSkill> {
    let slug = Slug::parse(slug)?;
    let dir = layout.skill_dir(&slug);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("SKILL.md"), markdown)?;
    let hash = hash_skill_folder(&dir)?;

    let mut manifest = if layout.manifest_path().exists() {
        StoreManifest::load(layout)?.manifest
    } else {
        StoreManifest::new(scope, StoreId::new())
    };
    let item_id = ItemId::new();
    manifest.insert_item(
        ManifestItem::new(
            item_id,
            ItemKind::Skill,
            slug.clone(),
            StorePath::parse(&format!("skills/{slug}"))?,
            hash.clone(),
        )
        .with_name(name.to_owned()),
    )?;
    fs::create_dir_all(layout.root())?;
    fs::write(layout.manifest_path(), manifest.to_bytes()?)?;

    database.connection().execute(
        "INSERT INTO items (id, kind, scope, project_id, slug, name, source_path, content_hash, \
         state, managed) VALUES (?1, 'skill', ?2, ?3, ?4, ?5, ?6, ?7, 'present', 1)",
        rusqlite::params![
            item_id.to_string(),
            scope.wire_name(),
            project_id,
            slug.as_str(),
            name,
            dir.to_string_lossy(),
            hash.to_string(),
        ],
    )?;

    Ok(InstalledSkill {
        item_id,
        slug: slug.as_str().to_owned(),
        dir,
        hash: hash.to_string(),
        scope,
    })
}

/// Hashes every entry under the given roots, ignoring AgenTuna bookkeeping files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TreeSnapshot {
    entries: BTreeMap<String, String>,
}

impl TreeSnapshot {
    /// Captures every entry under the labelled roots.
    ///
    /// # Errors
    /// Propagates filesystem errors other than a missing root.
    pub fn capture(roots: &[(&str, PathBuf)]) -> TestResult<Self> {
        let mut snapshot = Self::default();
        for (label, root) in roots {
            let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            if !root.is_dir() {
                continue;
            }
            snapshot.walk(label, &root, &root)?;
        }
        Ok(snapshot)
    }

    fn walk(&mut self, label: &str, root: &Path, current: &Path) -> TestResult<()> {
        let mut children = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            let key = format!("{label}/{relative}");
            if is_bookkeeping(&key) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path)?;
                self.entries
                    .insert(key, format!("link:{}", target.to_string_lossy()));
            } else if metadata.is_dir() {
                self.walk(label, root, &path)?;
            } else {
                let bytes = fs::read(&path)?;
                self.entries
                    .insert(key, format!("file:{}", at_fs::sha256_hex(&bytes)));
            }
        }
        Ok(())
    }

    /// Every captured path, sorted.
    #[must_use]
    pub fn keys(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// One captured entry.
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// Fails when the two snapshots differ, printing the symmetric difference.
    #[must_use]
    pub fn difference(&self, other: &Self) -> String {
        let mut lines = Vec::new();
        for (key, value) in &self.entries {
            match other.entries.get(key) {
                None => lines.push(format!("removed {key}")),
                Some(other_value) if other_value != value => {
                    lines.push(format!("changed {key}: {value} -> {other_value}"));
                }
                Some(_) => {}
            }
        }
        for key in other.entries.keys() {
            if !self.entries.contains_key(key) {
                lines.push(format!("added {key}"));
            }
        }
        lines.join("\n")
    }

    /// Asserts two snapshots are byte-identical.
    ///
    /// # Panics
    /// When any entry differs.
    pub fn assert_unchanged(&self, after: &Self, context: &str) {
        let difference = self.difference(after);
        assert!(difference.is_empty(), "{context}:\n{difference}");
    }
}

/// Paths that are pure AgenTuna bookkeeping rather than user-visible content.
fn is_bookkeeping(key: &str) -> bool {
    key.ends_with("-shm")
        || key.ends_with("-wal")
        || key.ends_with("-journal")
        || key.ends_with(".db")
        || key.contains("/.state/")
        || key.ends_with("/.state")
        || key.ends_with(".DS_Store")
}
