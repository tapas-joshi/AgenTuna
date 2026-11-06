//! Agent path resolution coordinated with the persistent `agent_paths` index.

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment, AgentError};
use at_adapters::paths::{NeedsPath, ResolvedAgentPath, resolve};
use at_db::agents::{
    AgentPathFormat, AgentPathKind, AgentPathProbeSpec, list_agent_paths, record_agent_path_probe,
};
use at_model::{AgentId, ItemKind, Scope};
use rusqlite::Connection;

/// Failure while resolving and persisting an agent path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentPathResolutionError {
    /// Adapter discovery or filesystem operation failed.
    #[error(transparent)]
    Adapter(#[from] AgentError),
    /// Path index operation failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// No usable path exists; the candidates and prior path are retained for UI display.
    #[error("agent path needs user selection")]
    NeedsPath(NeedsPath),
    /// Adapter emitted a path kind not represented in the database schema.
    #[error("adapter emitted unsupported path kind `{0}`")]
    UnknownPathKind(String),
}

/// Resolves an agent-native path, records all probed candidates, and keeps prior paths visible.
///
/// A primary override already present in `agent_paths` is used verbatim and suppresses probing
/// adapter alternatives. Each candidate is persisted with `verified=true` only if stat, expected
/// file/directory type and read access all succeeded.
///
/// # Errors
/// Returns [`AgentPathResolutionError::NeedsPath`] with the candidate list if no candidate is
/// usable, or a typed adapter/database failure if discovery or persistence failed.
pub fn resolve_and_record(
    connection: &Connection,
    registry: &AgentAdapterRegistry,
    agent: &AgentId,
    kind: ItemKind,
    scope: Scope,
    env: &AgentEnvironment,
) -> Result<ResolvedAgentPath, AgentPathResolutionError> {
    let adapter = registry
        .get(agent)?
        .ok_or_else(|| AgentError::NotInstalled {
            agent: agent.clone(),
            path: env.home.clone(),
        })?;
    let stored = list_agent_paths(connection, agent)?;
    let selected = stored.iter().find(|path| {
        path.scope == Some(scope)
            && path.kind == Some(db_kind(kind))
            && path.is_primary == Some(true)
    });
    let override_path = selected
        .filter(|path| path.is_override)
        .and_then(|path| path.path.as_deref());
    let last_known_path = selected.and_then(|path| path.path.as_deref());

    let result = resolve(
        adapter.as_ref(),
        kind,
        scope,
        env,
        override_path,
        last_known_path,
    );
    let candidates = match &result {
        Ok(resolved) => &resolved.candidates,
        Err(needs_path) => &needs_path.candidates,
    };
    for candidate in candidates {
        let path_kind = AgentPathKind::parse(&candidate.candidate.kind).ok_or_else(|| {
            AgentPathResolutionError::UnknownPathKind(candidate.candidate.kind.clone())
        })?;
        let format = candidate
            .candidate
            .format
            .as_deref()
            .and_then(AgentPathFormat::parse);
        let is_override = override_path.is_some_and(|path| path == candidate.candidate.path);
        record_agent_path_probe(
            connection,
            &AgentPathProbeSpec {
                agent,
                scope,
                kind: path_kind,
                path: &candidate.candidate.path,
                format,
                is_primary: candidate.candidate.primary,
                exists: candidate.exists,
                verified: candidate.verified,
                is_override,
            },
        )?;
    }
    result.map_err(AgentPathResolutionError::NeedsPath)
}

fn db_kind(kind: ItemKind) -> AgentPathKind {
    match kind {
        ItemKind::Skill => AgentPathKind::SkillsDir,
        ItemKind::McpServer => AgentPathKind::McpFile,
        ItemKind::Rule => AgentPathKind::RulesFile,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
    use at_db::Database;
    use at_db::agents::{AgentPathKind, list_agent_paths, set_path_override, upsert_agent};
    use at_model::{Agent, AgentId, ItemKind, Scope};
    use tempfile::tempdir;

    use super::{AgentPathResolutionError, resolve_and_record};

    #[test]
    fn missing_candidates_are_persisted_unverified_then_promoted_when_usable() {
        let home = tempdir().expect("home");
        let db = Database::open_in_memory().expect("database");
        let connection = db.connection();
        let agent = Agent {
            id: AgentId::Codex,
            display_name: String::from("Codex"),
            detected: false,
            version: None,
        };
        upsert_agent(connection, &agent).expect("agent row");
        let env = AgentEnvironment {
            home: home.path().to_path_buf(),
            project: None,
            variables: BTreeMap::new(),
        };
        let registry = AgentAdapterRegistry::register();

        let error = resolve_and_record(
            connection,
            &registry,
            &AgentId::Codex,
            ItemKind::Skill,
            Scope::User,
            &env,
        )
        .expect_err("no candidate exists");
        assert!(matches!(error, AgentPathResolutionError::NeedsPath(_)));
        let paths = list_agent_paths(connection, &AgentId::Codex).expect("recorded candidates");
        assert_eq!(paths.len(), 2);
        assert!(
            paths
                .iter()
                .all(|path| !path.verified && path.exists == Some(false))
        );

        let usable = home.path().join(".agents/skills");
        fs::create_dir_all(&usable).expect("skills root");
        let resolved = resolve_and_record(
            connection,
            &registry,
            &AgentId::Codex,
            ItemKind::Skill,
            Scope::User,
            &env,
        )
        .expect("usable fallback");
        assert_eq!(resolved.primary, usable);
        let paths = list_agent_paths(connection, &AgentId::Codex).expect("updated candidates");
        assert!(
            paths
                .iter()
                .any(|path| path.verified && path.exists == Some(true))
        );
    }

    #[test]
    fn saved_override_is_used_verbatim_and_retained_when_unavailable() {
        let home = tempdir().expect("home");
        let mut db = Database::open_in_memory().expect("database");
        let connection = db.connection_mut();
        let agent = Agent {
            id: AgentId::ClaudeCode,
            display_name: String::from("Claude Code"),
            detected: false,
            version: None,
        };
        upsert_agent(connection, &agent).expect("agent row");
        let override_path = home.path().join("custom/skills");
        set_path_override(
            connection,
            &AgentId::ClaudeCode,
            Scope::User,
            AgentPathKind::SkillsDir,
            &override_path,
        )
        .expect("override");
        let env = AgentEnvironment {
            home: home.path().to_path_buf(),
            project: None,
            variables: BTreeMap::new(),
        };
        let registry = AgentAdapterRegistry::register();
        let error = resolve_and_record(
            connection,
            &registry,
            &AgentId::ClaudeCode,
            ItemKind::Skill,
            Scope::User,
            &env,
        )
        .expect_err("missing override");
        let AgentPathResolutionError::NeedsPath(needs_path) = error else {
            panic!("NeedsPath expected")
        };
        assert_eq!(needs_path.candidates.len(), 1);
        assert_eq!(needs_path.candidates[0].candidate.path, override_path);
        assert_eq!(needs_path.last_known_path, Some(override_path.clone()));
        assert!(!needs_path.candidates[0].verified);

        fs::create_dir_all(&override_path).expect("override directory");
        let resolved = resolve_and_record(
            connection,
            &registry,
            &AgentId::ClaudeCode,
            ItemKind::Skill,
            Scope::User,
            &env,
        )
        .expect("verified override");
        assert_eq!(resolved.primary, override_path);
        assert!(resolved.is_override);
        fs::remove_dir(&override_path).expect("remove vanished override");
        let error = resolve_and_record(
            connection,
            &registry,
            &AgentId::ClaudeCode,
            ItemKind::Skill,
            Scope::User,
            &env,
        )
        .expect_err("path vanished after a successful scan");
        let AgentPathResolutionError::NeedsPath(needs_path) = error else {
            panic!("NeedsPath expected")
        };
        assert_eq!(needs_path.last_known_path, Some(override_path.clone()));
        assert!(!needs_path.candidates[0].verified);
        let paths = list_agent_paths(connection, &AgentId::ClaudeCode).expect("refreshed override");
        assert!(paths.iter().any(|path| {
            path.path.as_ref() == Some(&override_path)
                && !path.verified
                && path.exists == Some(false)
        }));
    }
}
