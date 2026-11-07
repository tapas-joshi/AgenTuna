//! Read-only loading of canonical scope stores into engine query snapshots.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::local_scope::{LocalScope, LocalScopeStatus};
use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment, DetectionState, NativeItem};
use at_model::{Definition, ItemKind, ItemState, Scope, ScopeSnapshot};
use at_store::layout::StoreLayout;
use at_store::local_store::LocalStorePaths;
use at_store::manifest::{ManifestError, StoreManifest, resolve_item_path};
use at_store::mcp_store::McpStore;
use at_store::paths::AppPaths;
use at_store::skill::read_skill_doc;
use rusqlite::Connection;

use crate::tombstones::ManifestTombstones;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Diagnostic from a store entry that could not be represented as a definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct LoadDiagnostic {
    /// Scope whose input was skipped.
    #[ts(type = "string")]
    pub scope: Scope,
    /// File or directory associated with the problem.
    #[ts(type = "string")]
    pub path: PathBuf,
    /// Stable machine-readable problem label.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

/// Loaded scope definitions and metadata needed by effective/why queries.
#[derive(Debug, Clone)]
pub struct LoadedScopes {
    /// Global, User, Project, and Local snapshots, including empty layers.
    pub layers: Vec<ScopeSnapshot>,
    /// Manifest-backed disabled-item tombstones.
    pub tombstones: ManifestTombstones,
    /// Recoverable file-level issues. Store-wide failures are returned as errors.
    pub diagnostics: Vec<LoadDiagnostic>,
}

/// Failure to prepare or read a scope store as a whole.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The selected project is missing from the resolved application paths.
    #[error("no active project store is available")]
    NoActiveProject,
    /// Store manifest could not be read or parsed.
    #[error("{path}: cannot load store manifest: {source}")]
    Manifest {
        /// Manifest path.
        path: PathBuf,
        /// Underlying store error.
        #[source]
        source: Box<ManifestError>,
    },
    /// An unexpected filesystem error prevented listing a scope directory.
    #[error("{path}: cannot list scope entries: {source}")]
    ReadDirectory {
        /// Directory path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Loads the four available scopes from explicit resolved application paths.
///
/// The caller owns path resolution and supplies an `AppPaths` built from injected inputs;
/// this function never consults process `HOME` or environment variables. A missing store
/// directory or manifest produces an empty snapshot and a diagnostic. Per-item parse and
/// content errors are also diagnostics and do not fail the complete query.
///
/// # Errors
/// Returns [`LoadError::NoActiveProject`] when `active_project` is supplied without a
/// matching project store in `paths`, or a typed error for an unreadable manifest or
/// directory listing.
pub fn load_scope_snapshots(
    paths: &AppPaths,
    active_project: Option<&Path>,
) -> Result<LoadedScopes, LoadError> {
    if active_project.is_some() && paths.project_store().is_none() {
        return Err(LoadError::NoActiveProject);
    }
    let mut loaded = LoadedScopes {
        layers: Vec::with_capacity(4),
        tombstones: ManifestTombstones::new(),
        diagnostics: Vec::new(),
    };
    for scope in [Scope::Global, Scope::User, Scope::Project, Scope::Local] {
        if matches!(scope, Scope::Project | Scope::Local) && active_project.is_none() {
            continue;
        }
        if scope == Scope::Local {
            let Some(project_root) = active_project else {
                continue;
            };
            let local_store =
                LocalStorePaths::for_paths(paths).map_err(|_| LoadError::NoActiveProject)?;
            let local_scope = LocalScope::detect(project_root.to_path_buf(), local_store);
            if !matches!(local_scope.status(), LocalScopeStatus::Available) {
                loaded.diagnostics.push(LoadDiagnostic {
                    scope,
                    path: project_root.to_path_buf(),
                    code: "local_scope_unavailable".to_owned(),
                    message: "Local scope is available only inside a Git work tree".to_owned(),
                });
                continue;
            }
        }
        let layout =
            StoreLayout::for_scope(paths, scope).map_err(|_| LoadError::NoActiveProject)?;
        let (definitions, manifest) = load_store(&layout, &mut loaded.diagnostics)?;
        if let Some(manifest) = manifest {
            loaded.tombstones.insert_manifest(scope, &manifest);
            loaded
                .tombstones
                .insert_manifest_metadata(scope, None, Some(layout.manifest_path()));
        }
        loaded.layers.push(ScopeSnapshot { scope, definitions });
    }
    Ok(loaded)
}

fn load_store(
    layout: &StoreLayout,
    diagnostics: &mut Vec<LoadDiagnostic>,
) -> Result<(Vec<Definition>, Option<StoreManifest>), LoadError> {
    let manifest_path = layout.manifest_path();
    let manifest = match StoreManifest::load(layout) {
        Ok(loaded) => Some(loaded.manifest),
        Err(source) if manifest_is_missing(&source) => {
            diagnostics.push(LoadDiagnostic {
                scope: layout.scope(),
                path: manifest_path.clone(),
                code: "store_manifest_missing".to_owned(),
                message: source.to_string(),
            });
            None
        }
        Err(source) => {
            return Err(LoadError::Manifest {
                path: manifest_path,
                source: Box::new(source),
            });
        }
    };
    let mut definitions = Vec::new();
    let Some(manifest) = manifest.as_ref() else {
        return Ok((definitions, manifest));
    };
    let mcp = load_mcp(layout, diagnostics);
    for item in manifest.items() {
        match item.kind {
            ItemKind::Skill => {
                let path = match resolve_item_path(layout.root(), &item.path) {
                    Ok(path) => path.join("SKILL.md"),
                    Err(error) => {
                        report(
                            diagnostics,
                            layout.scope(),
                            manifest_path.clone(),
                            "skill_path_invalid",
                            error.to_string(),
                        );
                        continue;
                    }
                };
                match read_skill_doc(&path) {
                    Ok(doc) => definitions.push(Definition {
                        kind: item.kind,
                        name: doc.name,
                        declared_id: None,
                        item_id: Some(item.id),
                        path,
                    }),
                    Err(error) => report(
                        diagnostics,
                        layout.scope(),
                        path,
                        "skill_invalid",
                        error.to_string(),
                    ),
                }
            }
            ItemKind::McpServer => {
                let Some(entry) = mcp.as_ref().and_then(|store| store.get(item.slug.as_str()))
                else {
                    report(
                        diagnostics,
                        layout.scope(),
                        layout.mcp_file(),
                        "mcp_definition_missing",
                        format!("manifest item {} has no MCP entry", item.id),
                    );
                    continue;
                };
                definitions.push(Definition {
                    kind: item.kind,
                    name: item
                        .name
                        .clone()
                        .unwrap_or_else(|| entry.display_name.clone()),
                    declared_id: None,
                    item_id: Some(item.id),
                    path: layout.mcp_file(),
                });
            }

            ItemKind::Rule => {
                let path = match resolve_item_path(layout.root(), &item.path) {
                    Ok(path) => path,
                    Err(error) => {
                        report(
                            diagnostics,
                            layout.scope(),
                            manifest_path.clone(),
                            "rule_path_invalid",
                            error.to_string(),
                        );
                        continue;
                    }
                };
                if !path.is_file() {
                    report(
                        diagnostics,
                        layout.scope(),
                        path,
                        "rule_missing",
                        format!("rule item {} is not a file", item.id),
                    );
                    continue;
                }
                definitions.push(Definition {
                    kind: item.kind,
                    name: item.name.clone().unwrap_or_else(|| item.slug.to_string()),
                    declared_id: None,
                    item_id: Some(item.id),
                    path,
                });
            }
        }
    }
    Ok((definitions, Some(manifest.clone())))
}

fn load_mcp(layout: &StoreLayout, diagnostics: &mut Vec<LoadDiagnostic>) -> Option<McpStore> {
    match McpStore::load(layout) {
        Ok(loaded) => Some(loaded.store),
        Err(error) if error.is_missing() => Some(McpStore::empty()),
        Err(error) => {
            report(
                diagnostics,
                layout.scope(),
                layout.mcp_file(),
                "mcp_store_invalid",
                error.to_string(),
            );
            None
        }
    }
}

fn manifest_is_missing(error: &ManifestError) -> bool {
    error.is_missing()
}

fn report(
    diagnostics: &mut Vec<LoadDiagnostic>,
    scope: Scope,
    path: PathBuf,
    code: &'static str,
    message: String,
) {
    diagnostics.push(LoadDiagnostic {
        scope,
        path,
        code: code.to_owned(),
        message,
    });
}

/// Adapter discovery output kept separate from canonical scope loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentObservation {
    /// Stable adapter identity.
    pub agent: at_model::AgentId,
    /// Whether adapter detection found this agent installed.
    pub installed: bool,
    /// Native item definitions reported by the adapter.
    pub native: Vec<NativeItem>,
    /// Recoverable adapter diagnostics.
    pub diagnostics: Vec<String>,
}

/// Detects agents and lists their native items through the registered adapters.
///
/// # Errors
/// Returns a typed adapter registry error when adapter enumeration fails.
pub fn observe_agents(
    registry: &AgentAdapterRegistry,
    environment: &AgentEnvironment,
) -> Result<Vec<AgentObservation>, at_adapters::adapter::AgentError> {
    let adapters = registry.adapters()?;
    let mut observations = Vec::with_capacity(adapters.len());
    for adapter in adapters {
        let presence = adapter.detect(environment);
        let installed = matches!(
            presence.state,
            DetectionState::Installed | DetectionState::ConfiguredOnly
        );
        let (native, diagnostics) = if installed {
            match adapter.list_native(environment) {
                Ok(native) => (native, Vec::new()),
                Err(error) => (Vec::new(), vec![error.to_string()]),
            }
        } else {
            (Vec::new(), Vec::new())
        };
        observations.push(AgentObservation {
            agent: adapter.id(),
            installed,
            native,
            diagnostics,
        });
    }
    Ok(observations)
}

/// Joins adapter observations, indexed deployments and native discovery into per-agent loads.
///
/// This is the one composition shared by the Tauri effective command and `agentuna effective`:
/// an agent loads an item when a deployment for the winning definition exists at or below the
/// queried scope, or when the adapter reports a native item with the same kind and normalized
/// name. Store presence alone never counts as loaded.
///
/// # Errors
/// Returns a typed database error when the deployment index cannot be read.
pub fn agent_loads(
    connection: &Connection,
    observations: &[AgentObservation],
    layers: &[ScopeSnapshot],
    scope: Scope,
    project_id: Option<&str>,
) -> Result<Vec<crate::effective::AgentLoads>, at_db::DbError> {
    let mut agents = Vec::with_capacity(observations.len());
    for observation in observations {
        let deployments =
            at_db::deployments::list_deployments_for_agent(connection, &observation.agent)?;
        let mut loaded = BTreeMap::new();
        for definition in layers.iter().flat_map(|layer| &layer.definitions) {
            let deployment = deployments
                .iter()
                .filter(|row| Some(row.item_id) == definition.item_id)
                .filter(|row| row.scope.precedence() <= scope.precedence())
                .filter(|row| match row.scope {
                    Scope::Global | Scope::User => row.project_id.is_none(),
                    Scope::Project | Scope::Local => row.project_id.as_deref() == project_id,
                })
                .max_by_key(|row| row.scope.precedence());
            if let Some(deployment) = deployment {
                loaded.insert(definition.logical_id(), deployment.state);
            } else if observation.native.iter().any(|native| {
                native.kind == definition.kind
                    && at_model::normalized_name(&native.name) == definition.normalized_name()
            }) {
                loaded.insert(definition.logical_id(), ItemState::Native);
            }
        }
        agents.push(crate::effective::AgentLoads {
            agent: observation.agent.clone(),
            installed: observation.installed,
            loaded,
        });
    }
    Ok(agents)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::locks::LockStore;
    use at_model::{ItemId, ItemKind, Lock, Scope};
    use at_store::layout::{Slug, StoreId, StoreLayout};
    use at_store::manifest::{DisabledAgents, ManifestItem, StoreManifest, StorePath};
    use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};

    use super::*;

    fn paths(root: &Path, project: &Path) -> AppPaths {
        AppPaths::resolve(
            &PathEnvironment::default()
                .with_home(root)
                .with_project_root(project)
                .with_overrides(PathOverrides {
                    global_store: Some(root.join("global-store")),
                    ..PathOverrides::default()
                }),
        )
        .expect("sandbox paths")
    }

    fn add_skill(layout: &StoreLayout, name: &str, text: &[u8]) -> ItemId {
        let slug = Slug::parse(name).expect("valid test slug");
        let folder = layout.skill_dir(&slug);
        fs::create_dir_all(&folder).expect("skill folder");
        fs::write(folder.join("SKILL.md"), text).expect("skill source");
        let id = ItemId::new();
        let manifest_path = layout.manifest_path();
        let mut manifest = if manifest_path.exists() {
            StoreManifest::load(layout)
                .expect("existing store manifest")
                .manifest
        } else {
            StoreManifest::new(layout.scope(), StoreId::new())
        };
        manifest
            .insert_item(ManifestItem::new(
                id,
                ItemKind::Skill,
                slug,
                StorePath::parse(&format!("skills/{name}")).expect("relative path"),
                at_store::manifest::hash_skill_folder(&folder).expect("skill hash"),
            ))
            .expect("add item");
        fs::create_dir_all(layout.root()).expect("store root");
        fs::write(
            layout.manifest_path(),
            manifest.to_bytes().expect("manifest bytes"),
        )
        .expect("store manifest");
        id
    }

    fn add_inherited_tombstone(layout: &StoreLayout, item_id: ItemId) {
        let mut manifest = StoreManifest::load(layout)
            .expect("existing manifest")
            .manifest;
        manifest
            .disable_inherited(item_id, ItemKind::Skill, DisabledAgents::All)
            .expect("tombstone");
        fs::create_dir_all(layout.root()).expect("store root");
        fs::write(
            layout.manifest_path(),
            manifest.to_bytes().expect("manifest bytes"),
        )
        .expect("store manifest");
    }

    #[test]
    fn loads_all_four_scopes_and_reports_malformed_skill_files() {
        let temp = tempfile::tempdir().expect("temporary root");
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".git")).expect("git work tree");
        let paths = paths(temp.path(), &project);
        let scopes = [Scope::Global, Scope::User, Scope::Project, Scope::Local];
        for scope in scopes {
            let layout = StoreLayout::for_scope(&paths, scope).expect("layout");
            add_skill(
                &layout,
                "review",
                b"---\nname: review\ndescription: Review\n---\n",
            );
        }
        let local = StoreLayout::for_scope(&paths, Scope::Local).expect("local layout");
        add_skill(&local, "broken", b"not a valid skill document");

        let loaded = load_scope_snapshots(&paths, Some(&project)).expect("load scopes");
        assert_eq!(
            loaded
                .layers
                .iter()
                .map(|layer| layer.scope)
                .collect::<Vec<_>>(),
            scopes
        );
        assert!(loaded.layers.iter().all(|layer| {
            layer
                .definitions
                .iter()
                .any(|definition| definition.name == "review")
        }));
        assert!(loaded.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "skill_invalid" && diagnostic.path.ends_with("broken/SKILL.md")
        }));
    }

    #[test]
    fn resolution_uses_loaded_tombstones_and_global_locks() {
        let temp = tempfile::tempdir().expect("temporary root");
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".git")).expect("git work tree");
        let paths = paths(temp.path(), &project);
        let global = StoreLayout::for_scope(&paths, Scope::Global).expect("global layout");
        let user = StoreLayout::for_scope(&paths, Scope::User).expect("user layout");
        let project_layout =
            StoreLayout::for_scope(&paths, Scope::Project).expect("project layout");
        let local = StoreLayout::for_scope(&paths, Scope::Local).expect("local layout");
        let global_id = add_skill(
            &global,
            "review",
            b"---\nname: review\ndescription: Global\n---\n",
        );
        add_skill(
            &user,
            "review",
            b"---\nname: review\ndescription: User\n---\n",
        );
        add_skill(
            &project_layout,
            "review",
            b"---\nname: review\ndescription: Project\n---\n",
        );
        add_skill(
            &local,
            "review",
            b"---\nname: review\ndescription: Local\n---\n",
        );
        let inherited_id = add_skill(
            &user,
            "inherited",
            b"---\nname: inherited\ndescription: Inherited\n---\n",
        );
        add_inherited_tombstone(&project_layout, inherited_id);

        let loaded = load_scope_snapshots(&paths, Some(&project)).expect("load scopes");
        let locks = crate::locks::MemoryLockStore::new();
        locks
            .put(crate::locks::LockRecord {
                id: crate::locks::LockId::new(),
                lock: Lock {
                    scope: Scope::Global,
                    item_id: global_id,
                    reason: "pinned".to_owned(),
                    locked_by: "test".to_owned(),
                    locked_at: time::OffsetDateTime::now_utc(),
                },
            })
            .expect("lock record");
        let result = crate::resolve::resolve(
            crate::resolve::ResolveInput::new(&loaded.layers)
                .with_locks(&locks)
                .with_tombstones(&loaded.tombstones),
        );
        let review = result
            .items
            .iter()
            .find(|item| item.definition.name == "review")
            .expect("review winner");
        assert_eq!(review.scope, Scope::Global);
        assert!(review.lock.is_some());
        assert!(
            result
                .disabled
                .iter()
                .any(|item| item.item_id == inherited_id)
        );
    }

    #[test]
    fn missing_store_directories_are_empty_diagnostic_layers() {
        let temp = tempfile::tempdir().expect("temporary root");
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".git")).expect("git work tree");
        let paths = paths(temp.path(), &project);
        let loaded = load_scope_snapshots(&paths, Some(&project)).expect("load empty stores");
        assert_eq!(loaded.layers.len(), 4);
        assert!(
            loaded
                .layers
                .iter()
                .all(|layer| layer.definitions.is_empty())
        );
        assert!(
            loaded
                .diagnostics
                .iter()
                .filter(|entry| entry.code == "store_manifest_missing")
                .count()
                >= 4
        );
    }
}
