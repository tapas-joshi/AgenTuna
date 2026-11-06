//! Verification of adapter path candidates and explicit user overrides.

use std::fs;
use std::path::{Path, PathBuf};

use at_model::{ItemKind, Scope};

use crate::adapter::{AgentAdapter, AgentEnvironment, AgentPathCandidate};

/// Candidate path and whether it currently exists and can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAgentPath {
    /// Original adapter candidate, including priority and format.
    pub candidate: AgentPathCandidate,
    /// Whether metadata lookup found the target.
    pub exists: bool,
    /// Whether the target is the expected file/directory kind.
    pub expected_kind: bool,
    /// True only when stat and read access both succeeded.
    pub verified: bool,
}

/// Successful path resolution plus alternative candidates for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentPath {
    /// Preferred usable path.
    pub primary: PathBuf,
    /// All verified/unverified alternatives in adapter order.
    pub candidates: Vec<VerifiedAgentPath>,
    /// Whether the chosen primary is a persisted user override.
    pub is_override: bool,
}

/// No usable candidate exists; contains every candidate and any prior path for UI display.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NeedsPath {
    /// Paths with live exists/verification state.
    pub candidates: Vec<VerifiedAgentPath>,
    /// Last path known to have been selected, retained when it disappears.
    pub last_known_path: Option<PathBuf>,
}

/// Resolves and verifies an agent path, preferring a user override.
///
/// An override is checked directly and adapter alternatives are not probed. Without an
/// override, candidates are checked in order; the first usable one wins.
///
/// # Errors
/// Returns [`NeedsPath`] when no candidate has the expected type and can be read.
pub fn resolve(
    adapter: &dyn AgentAdapter,
    kind: ItemKind,
    scope: Scope,
    env: &AgentEnvironment,
    override_path: Option<&Path>,
    last_known_path: Option<&Path>,
) -> Result<ResolvedAgentPath, NeedsPath> {
    let candidates = if let Some(path) = override_path {
        vec![AgentPathCandidate {
            kind: kind_name(kind).to_owned(),
            path: path.to_path_buf(),
            scope,
            format: None,
            primary: true,
        }]
    } else {
        adapter.paths(kind, scope, env)
    };
    let verified: Vec<_> = candidates.into_iter().map(verify_candidate).collect();
    let selected = verified
        .iter()
        .find(|candidate| candidate.candidate.primary && candidate.verified)
        .or_else(|| verified.iter().find(|candidate| candidate.verified));
    match selected {
        Some(path) => Ok(ResolvedAgentPath {
            primary: path.candidate.path.clone(),
            candidates: verified,
            is_override: override_path.is_some(),
        }),
        None => Err(NeedsPath {
            candidates: verified,
            last_known_path: last_known_path.map(Path::to_path_buf),
        }),
    }
}

fn verify_candidate(candidate: AgentPathCandidate) -> VerifiedAgentPath {
    let metadata = fs::metadata(&candidate.path);
    let exists = metadata.is_ok();
    let expected_kind = metadata.as_ref().is_ok_and(|metadata| {
        if candidate.kind.ends_with("_dir") {
            metadata.is_dir()
        } else {
            metadata.is_file()
        }
    });
    let readable = expected_kind
        && if candidate.kind.ends_with("_dir") {
            fs::read_dir(&candidate.path).is_ok()
        } else {
            fs::File::open(&candidate.path).is_ok()
        };
    VerifiedAgentPath {
        candidate,
        exists,
        expected_kind,
        verified: exists && expected_kind && readable,
    }
}

fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Skill => "skills_dir",
        ItemKind::McpServer => "mcp_file",
        ItemKind::Rule => "rules_file",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use at_model::{AgentId, ItemKind, Scope};
    use tempfile::tempdir;

    use super::resolve;
    use crate::adapter::{AgentAdapterRegistry, AgentEnvironment};

    fn env(home: PathBuf) -> AgentEnvironment {
        AgentEnvironment {
            home,
            project: None,
            variables: BTreeMap::new(),
        }
    }

    #[test]
    fn candidate_resolution_preserves_order_and_missing_alternatives() {
        let home = tempdir().expect("home");
        let first = home.path().join(".codex/skills");
        let second = home.path().join(".agents/skills");
        fs::create_dir_all(&second).expect("second root");
        let registry = AgentAdapterRegistry::register();
        let adapter = registry
            .get(&AgentId::Codex)
            .expect("registry")
            .expect("Codex adapter");
        let result = resolve(
            adapter.as_ref(),
            ItemKind::Skill,
            Scope::User,
            &env(home.path().to_path_buf()),
            None,
            None,
        )
        .expect("verified second candidate");
        assert_eq!(result.primary, second);
        assert!(!result.candidates[0].exists);
        assert!(result.candidates[1].verified);
        assert_eq!(result.candidates[0].candidate.path, first);
    }

    #[test]
    fn override_is_checked_verbatim_and_missing_path_retains_previous_value() {
        let home = tempdir().expect("home");
        let override_path = home.path().join("my-skills");
        fs::create_dir_all(&override_path).expect("override root");
        let registry = AgentAdapterRegistry::register();
        let adapter = registry
            .get(&AgentId::ClaudeCode)
            .expect("registry")
            .expect("Claude adapter");
        let result = resolve(
            adapter.as_ref(),
            ItemKind::Skill,
            Scope::User,
            &env(home.path().to_path_buf()),
            Some(&override_path),
            None,
        )
        .expect("override");
        assert_eq!(result.primary, override_path);
        assert!(result.is_override);
        assert_eq!(result.candidates.len(), 1);
        let gone = home.path().join("gone");
        let error = resolve(
            adapter.as_ref(),
            ItemKind::Skill,
            Scope::User,
            &env(home.path().to_path_buf()),
            Some(&gone),
            Some(&gone),
        )
        .expect_err("missing override");
        assert_eq!(error.last_known_path, Some(gone));
        assert!(!error.candidates[0].exists);
    }
}
