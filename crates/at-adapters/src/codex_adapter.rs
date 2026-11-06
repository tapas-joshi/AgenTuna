//! Codex-native paths, project trust metadata and configuration discovery.

use std::path::{Path, PathBuf};

use at_model::{AgentId, Scope};

use crate::adapter::{
    AgentAdapter, AgentCapabilities, AgentDetection, AgentEnvironment, AgentPathCandidate,
    BuiltinAdapter,
};
use crate::codex_projects::{CodexProjectTrust, list_project_trust};

/// Registered Codex adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    /// Returns Codex's native user configuration root.
    #[must_use]
    pub fn config_root(&self, env: &AgentEnvironment) -> PathBuf {
        env.home.join(".codex")
    }

    /// Reads only the project path and trust level from Codex's project table.
    ///
    /// This is read-only and does not expose other project-table values or parse MCP data.
    ///
    /// # Errors
    /// Returns a typed parse error for malformed TOML or project entries.
    pub fn list_project_trust(
        &self,
        config_path: &Path,
        source: &str,
    ) -> Result<Vec<CodexProjectTrust>, crate::adapter::AgentError> {
        list_project_trust(config_path, source)
    }
}

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> AgentId {
        AgentId::Codex
    }

    fn capabilities(&self, env: &AgentEnvironment) -> AgentCapabilities {
        BuiltinAdapter { id: self.id() }.capabilities(env)
    }

    fn detect(&self, env: &AgentEnvironment) -> AgentDetection {
        BuiltinAdapter { id: self.id() }.detect(env)
    }

    fn detect_with_version(
        &self,
        env: &AgentEnvironment,
        probe_output: Option<&str>,
    ) -> crate::adapter::AgentDetectionWithVersion {
        BuiltinAdapter { id: self.id() }.detect_with_version(env, probe_output)
    }

    fn paths(
        &self,
        kind: at_model::ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<AgentPathCandidate> {
        BuiltinAdapter { id: self.id() }.paths(kind, scope, env)
    }

    fn list_project_trust(
        &self,
        config_path: &Path,
        source: &str,
    ) -> Result<Vec<CodexProjectTrust>, crate::adapter::AgentError> {
        CodexAdapter::list_project_trust(self, config_path, source)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::adapter::{AgentPathCandidate, DetectionState};
    use at_model::ItemKind;

    #[test]
    fn config_root_dual_skill_roots_and_local_paths_are_exposed() {
        let temp = tempfile::tempdir().expect("home");
        let project = temp.path().join("project");
        let env = AgentEnvironment {
            home: temp.path().to_path_buf(),
            project: Some(project.clone()),
            variables: BTreeMap::new(),
        };
        let adapter = CodexAdapter;
        assert_eq!(adapter.config_root(&env), temp.path().join(".codex"));
        let user = adapter.paths(ItemKind::Skill, Scope::User, &env);
        assert_eq!(user.len(), 2);
        assert_eq!(user[0].path, temp.path().join(".codex/skills"));
        assert_eq!(user[1].path, temp.path().join(".agents/skills"));
        assert_eq!(
            adapter.paths(ItemKind::Skill, Scope::Local, &env)[0].path,
            project.join(".agents/skills")
        );
        assert_eq!(
            adapter.paths(ItemKind::Rule, Scope::User, &env)[0].path,
            temp.path().join(".codex/AGENTS.md")
        );
    }

    #[test]
    fn codex_detection_keeps_configured_only_without_skill_root() {
        let temp = tempfile::tempdir().expect("home");
        let env = AgentEnvironment {
            home: temp.path().to_path_buf(),
            project: None,
            variables: BTreeMap::new(),
        };
        std::fs::create_dir_all(temp.path().join(".codex")).expect("config root");
        assert_eq!(
            CodexAdapter.detect(&env).state,
            DetectionState::ConfiguredOnly
        );
    }

    #[test]
    fn path_candidate_shape_remains_explicit() {
        let adapter = CodexAdapter;
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let roots: Vec<AgentPathCandidate> = adapter.paths(ItemKind::Skill, Scope::User, &env);
        assert!(roots.iter().all(|root| root.kind == "skills_dir"));
    }

    #[test]
    fn registry_routes_codex_project_trust_read() {
        use crate::adapter::AgentAdapterRegistry;

        let adapter = AgentAdapterRegistry::register()
            .get(&AgentId::Codex)
            .expect("registry")
            .expect("Codex adapter");
        let rows = adapter
            .list_project_trust(
                Path::new("config.toml"),
                "[projects.\"/fixture/project\"]\ntrust_level=\"trusted\"\nprivate=\"not returned\"\n",
            )
            .expect("trust table");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_path, PathBuf::from("/fixture/project"));
        assert_eq!(rows[0].trust_level.as_deref(), Some("trusted"));
    }
}
