//! Agent detection and filesystem-change loop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment, DetectionState};
use at_adapters::detect::{VersionProbe, detect_one, find_executable};
use at_db::Database;
use at_fs::{WatchConfig, WatchEvent, WatchRoot, WatchRootKind, WatchService};
use at_model::{Agent, ItemKind, Scope};
use at_store::paths::AppPaths;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// Event emitted after a complete detection pass.
pub const EVENT_AGENTS_DETECTED: &str = "agentuna:agents-detected";

#[derive(Debug, Clone, Serialize)]
struct DetectionEvent {
    agents: usize,
}

/// Starts an initial agent scan and keeps the index current as native files change.
pub fn start(
    app: AppHandle,
    paths: AppPaths,
    database: Arc<Mutex<Database>>,
    registry: Arc<at_fs::RecentWrites>,
) -> Result<(), at_fs::WatchError> {
    let adapters = Arc::new(AgentAdapterRegistry::register());
    let mut variables = BTreeMap::new();
    if let Ok(path) = std::env::var("PATH") {
        variables.insert(String::from("PATH"), path);
    }
    let environment = AgentEnvironment {
        home: paths.home_dir().unwrap_or(paths.user_store()).to_path_buf(),
        project: paths.project_store().map(ToOwned::to_owned),
        variables,
    };
    let roots = watch_roots(&adapters, &environment, &paths);
    let watcher = Arc::new(WatchService::new(
        WatchConfig::new(roots).with_registry(registry),
    ));
    watcher.start()?;

    let initial_app = app.clone();
    let initial_adapters = adapters.clone();
    let initial_environment = environment.clone();
    let initial_database = Arc::clone(&database);
    tauri::async_runtime::spawn_blocking(move || {
        detect_and_persist(
            &initial_app,
            &initial_adapters,
            &initial_environment,
            &initial_database,
        );
    });

    tauri::async_runtime::spawn_blocking(move || {
        loop {
            let Some(event) = watcher.next_event(Duration::from_secs(1)) else {
                continue;
            };
            match event {
                WatchEvent::Batch(_)
                | WatchEvent::RescanRequired { .. }
                | WatchEvent::PathGone { .. } => {
                    detect_and_persist(&app, &adapters, &environment, &database);
                }
                // WatchEvent can grow; an event we do not know needs no rescan.
                _ => {}
            }
        }
    });
    Ok(())
}

fn watch_roots(
    adapters: &AgentAdapterRegistry,
    environment: &AgentEnvironment,
    paths: &AppPaths,
) -> Vec<WatchRoot> {
    let mut roots = vec![
        WatchRoot::dir(paths.user_store(), WatchRootKind::UserStore),
        WatchRoot::dir(paths.global_store(), WatchRootKind::GlobalStore),
    ];
    if let Some(project) = paths.project_store() {
        roots.push(WatchRoot::dir(project, WatchRootKind::ProjectStore));
    }
    if let Ok(adapters) = adapters.adapters() {
        for adapter in adapters {
            for kind in [ItemKind::Skill, ItemKind::McpServer, ItemKind::Rule] {
                for scope in [Scope::User, Scope::Global, Scope::Project, Scope::Local] {
                    for candidate in adapter.paths(kind, scope, environment) {
                        let root_kind = match candidate.kind.as_str() {
                            "skills_dir" => WatchRootKind::SkillsDir,
                            "mcp_file" => WatchRootKind::McpFile,
                            "rules_file" => WatchRootKind::RulesFile,
                            "settings_file" => WatchRootKind::Other,
                            _ => continue,
                        };
                        let root = if root_kind.is_directory() {
                            WatchRoot::dir(candidate.path, root_kind)
                        } else {
                            WatchRoot::file(candidate.path, root_kind)
                        };
                        roots.push(root);
                    }
                }
            }
        }
    }
    roots.sort_by(|left, right| left.path.cmp(&right.path));
    roots.dedup_by(|left, right| left.path == right.path && left.kind == right.kind);
    roots
}

fn detect_and_persist(
    app: &AppHandle,
    adapters: &Arc<AgentAdapterRegistry>,
    environment: &AgentEnvironment,
    database: &Arc<Mutex<Database>>,
) {
    let Ok(adapter_list) = adapters.adapters() else {
        tracing::warn!("agent adapter registry unavailable during detection");
        return;
    };
    let results = tauri::async_runtime::block_on(async {
        let mut tasks = tokio::task::JoinSet::new();
        for adapter in adapter_list {
            let id = adapter.id();
            let registry = Arc::clone(adapters);
            let environment = environment.clone();
            tasks.spawn(async move {
                let probe = version_probe_for(&id, &environment);
                let result = detect_one(&registry, &id, &environment, probe.as_ref()).await;
                (id, result)
            });
        }
        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(result) => results.push(result),
                Err(error) => tracing::warn!(%error, "agent detection task failed"),
            }
        }
        results
    });
    let Ok(database) = database.lock() else {
        tracing::warn!("database lock poisoned during agent detection");
        return;
    };
    let connection = database.connection();
    let mut detected_count = 0;
    for (id, result) in results {
        match result {
            Ok(result) => {
                let presence = result.detection.presence.state;
                let agent = Agent {
                    id: id.clone(),
                    display_name: result.agent.to_string(),
                    detected: matches!(presence, DetectionState::Installed),
                    version: result.detection.version,
                };
                match at_db::agents::upsert_agent(connection, &agent) {
                    Ok(()) => {
                        persist_candidates(connection, &id, &result.detection.presence.candidates);
                        detected_count += 1;
                    }
                    Err(error) => {
                        tracing::warn!(%error, agent = %agent.id, "agent detection persistence failed")
                    }
                }
            }
            Err(error) => tracing::warn!(%error, agent = %id, "agent detection failed"),
        }
    }
    if let Some(state) = app.try_state::<super::state::AppState>() {
        state.invalidate_scope_cache();
    }
    if let Err(error) = app.emit(
        EVENT_AGENTS_DETECTED,
        DetectionEvent {
            agents: detected_count,
        },
    ) {
        tracing::warn!(%error, "agent detection event emission failed");
    }
}

fn version_probe_for(
    agent: &at_model::AgentId,
    environment: &AgentEnvironment,
) -> Option<VersionProbe> {
    let executable = match agent {
        at_model::AgentId::ClaudeCode => "claude",
        at_model::AgentId::Codex => "codex",
        at_model::AgentId::Omp => "omp",
        at_model::AgentId::Cursor => "cursor",
        at_model::AgentId::Custom(_) => return None,
    };
    let executable = find_executable(
        environment.variables.get("PATH").map(String::as_str),
        executable,
    )?;
    Some(VersionProbe {
        executable,
        args: vec![String::from("--version")],
    })
}

fn persist_candidates(
    connection: &rusqlite::Connection,
    agent: &at_model::AgentId,
    candidates: &[at_adapters::adapter::AgentPathCandidate],
) {
    for candidate in candidates {
        let Some(kind) = at_db::agents::AgentPathKind::parse(&candidate.kind) else {
            continue;
        };
        let metadata = std::fs::metadata(&candidate.path);
        let exists = metadata.is_ok();
        let verified = metadata.is_ok_and(|metadata| {
            if kind == at_db::agents::AgentPathKind::SkillsDir
                || candidate.format.as_deref() == Some("dir")
            {
                metadata.is_dir() && std::fs::read_dir(&candidate.path).is_ok()
            } else {
                metadata.is_file() && std::fs::File::open(&candidate.path).is_ok()
            }
        });
        let format = candidate
            .format
            .as_deref()
            .and_then(at_db::agents::AgentPathFormat::parse);
        if let Err(error) = at_db::agents::record_agent_path_probe(
            connection,
            &at_db::agents::AgentPathProbeSpec {
                agent,
                scope: candidate.scope,
                kind,
                path: &candidate.path,
                format,
                is_primary: candidate.primary,
                exists,
                verified,
                is_override: false,
            },
        ) {
            tracing::warn!(%error, agent = %agent, path = %candidate.path.display(), "agent path detection persistence failed");
        }
    }
}
