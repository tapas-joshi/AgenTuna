//! Skill slug validation and read-only cross-scope clash detection.

use std::fs;
use std::path::{Path, PathBuf};

use at_db::agents::{AgentPathKind, list_agent_paths};
use at_model::AgentId;
use at_model::{Scope, skill::validate_skill_name};

/// Checks a name using selected agent IDs and their indexed skills directories.
///
/// # Errors
/// Returns a path inspection error or the database query error text as a typed error.
pub fn check_skill_name_for_agents(
    connection: &rusqlite::Connection,
    name: &str,
    target_scope: Scope,
    selected_agents: &[AgentId],
    stores: &[(Scope, PathBuf)],
) -> Result<ClashReport, SkillNameCheckError> {
    let mut dirs = Vec::new();
    for agent in selected_agents {
        for path in list_agent_paths(connection, agent)
            .map_err(|error| SkillNameCheckError::Database(error.to_string()))?
        {
            if path.kind == Some(AgentPathKind::SkillsDir)
                && let Some(path) = path.path
            {
                dirs.push(AgentSkillsDir {
                    agent: agent.to_string(),
                    path,
                });
            }
        }
    }
    check_skill_name(name, target_scope, &dirs, stores)
}

/// A suggested remedy for a detected skill-name collision.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClashAction {
    /// Inspect and import the unmanaged skill into AgenTuna.
    Adopt,
    /// Move the unmanaged folder aside before creating a skill.
    MoveAside,
    /// Choose another skill name.
    ChooseAnotherName,
}

/// One matching skill folder or reserved-name collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClashHit {
    /// Scope containing the matching skill, when known.
    pub scope: Option<Scope>,
    /// Absolute path observed without following a final symlink.
    pub path: PathBuf,
    /// Suggested remediation actions.
    pub actions: Vec<ClashAction>,
    /// Whether this collision prevents creation at the requested scope.
    pub blocking: bool,
}

/// Validity and collision results for a requested skill name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClashReport {
    /// Rules preventing this name from being used.
    pub blocking: Vec<String>,
    /// Non-blocking messages about copies in other scopes.
    pub warnings: Vec<String>,
    /// Filesystem and reserved-name collisions.
    pub hits: Vec<ClashHit>,
}

/// A skills directory belonging to an agent selected for the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSkillsDir {
    /// Agent identifier used for reporting.
    pub agent: String,
    /// Absolute skills directory.
    pub path: PathBuf,
}

/// Inputs required to check a candidate skill name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillNameCheck<'a> {
    /// Candidate slug.
    pub name: &'a str,
    /// Destination scope.
    pub target_scope: Scope,
    /// Store roots indexed by scope.
    pub stores: &'a [(Scope, PathBuf)],
    /// Skills directories of selected agents.
    pub agent_dirs: &'a [AgentSkillsDir],
}

/// Checks slug rules, canonical scope copies and selected agents' native folders.
///
/// This function is read-only. Every candidate is inspected with `symlink_metadata`,
/// so a symlink at the candidate path is reported but never followed.
///
/// # Errors
/// Returns [`SkillNameCheckError`] when a filesystem inspection fails unexpectedly.
pub fn check_skill_name(
    name: &str,
    target_scope: Scope,
    selected_agents: &[AgentSkillsDir],
    stores: &[(Scope, PathBuf)],
) -> Result<ClashReport, SkillNameCheckError> {
    check_skill_name_with_input(&SkillNameCheck {
        name,
        target_scope,
        stores,
        agent_dirs: selected_agents,
    })
}

/// Checks a candidate using explicit roots; retained as the implementation seam.
pub fn check_skill_name_with_input(
    input: &SkillNameCheck<'_>,
) -> Result<ClashReport, SkillNameCheckError> {
    let mut report = ClashReport {
        blocking: Vec::new(),
        warnings: Vec::new(),
        hits: Vec::new(),
    };
    if matches!(input.name, "agentuna" | "." | "..") {
        report
            .blocking
            .push(format!("`{}` is a reserved name", input.name));
        report.hits.push(ClashHit {
            scope: None,
            path: PathBuf::from(input.name),
            actions: vec![ClashAction::ChooseAnotherName],
            blocking: true,
        });
        return Ok(report);
    }
    if let Err(error) = validate_skill_name(input.name) {
        report.blocking.push(error.to_string());
        return Ok(report);
    }
    for (scope, root) in input.stores {
        let path = root.join("skills").join(input.name);
        if *scope == input.target_scope
            && let Some(existing) = case_insensitive_match(&root.join("skills"), input.name)?
            && existing.file_name() != Some(std::ffi::OsStr::new(input.name))
        {
            report.hits.push(ClashHit {
                scope: Some(*scope),
                path: existing.clone(),
                actions: vec![ClashAction::ChooseAnotherName],
                blocking: true,
            });
            report.blocking.push(format!(
                "Case-insensitive collision with {}",
                existing.display()
            ));
            continue;
        }
        if !exists_lstat(&path)? {
            continue;
        }
        let same_scope = *scope == input.target_scope;
        report.hits.push(ClashHit {
            scope: Some(*scope),
            path: path.clone(),
            actions: vec![ClashAction::ChooseAnotherName],
            blocking: same_scope,
        });
        if same_scope {
            report
                .blocking
                .push(format!("A skill already exists at {}", path.display()));
        } else {
            report
                .warnings
                .push(format!("A skill exists at {} ({scope})", path.display()));
        }
    }
    for dir in input.agent_dirs {
        let target = dir.path.join(input.name);
        let mut collision = None;
        if exists_lstat(&target)? {
            collision = Some(target.clone());
        } else if let Some(existing) = case_insensitive_match(&dir.path, input.name)? {
            collision = Some(existing);
        }
        if let Some(path) = collision {
            report.hits.push(ClashHit {
                scope: None,
                path: path.clone(),
                actions: vec![ClashAction::Adopt, ClashAction::MoveAside],
                blocking: true,
            });
            report.blocking.push(format!(
                "Unmanaged skill folder for agent {} at {}",
                dir.agent,
                path.display()
            ));
        }
    }
    // Every selected target path was checked in the per-scope pass above.
    Ok(report)
}
/// Adds resolver-derived managed-copy clashes to the filesystem check.
///
/// The resolver input is authoritative for logical skill identity and avoids treating
/// blocked locked copies as ordinary precedence clashes.
#[must_use]
pub fn include_resolved_clashes(
    mut report: ClashReport,
    name: &str,
    target_scope: Scope,
    resolved: &crate::resolve::ResolvedSet,
) -> ClashReport {
    let normalized = at_model::normalized_name(name);
    for item in resolved.items.iter().filter(|item| {
        item.logical_id.kind == at_model::ItemKind::Skill
            && item.definition.normalized_name() == normalized
    }) {
        let blocking = item.scope == target_scope;
        let hit = ClashHit {
            scope: Some(item.scope),
            path: item.definition.path.clone(),
            actions: vec![ClashAction::ChooseAnotherName],
            blocking,
        };
        if blocking {
            report.blocking.push(format!(
                "A skill with this name resolves from {} at {}",
                item.scope,
                item.definition.path.display()
            ));
        } else {
            report.warnings.push(format!(
                "A skill resolves from {} at {}",
                item.scope,
                item.definition.path.display()
            ));
        }
        report.hits.push(hit);
    }
    for shadow in resolved.shadowed.iter().filter(|entry| {
        entry.logical_id.kind == at_model::ItemKind::Skill
            && entry.definition.normalized_name() == normalized
            && entry.blocked.is_none()
    }) {
        if !report
            .hits
            .iter()
            .any(|hit| hit.path == shadow.definition.path)
        {
            report.hits.push(ClashHit {
                scope: Some(shadow.scope),
                path: shadow.definition.path.clone(),
                actions: vec![ClashAction::ChooseAnotherName],
                blocking: shadow.scope == target_scope,
            });
        }
    }
    report
}

/// Checks filesystem clashes and managed copies known to the resolver.
///
/// # Errors
/// Returns a filesystem inspection error from the base name check.
pub fn check_skill_name_resolved(
    input: &SkillNameCheck<'_>,
    resolved: &crate::resolve::ResolvedSet,
) -> Result<ClashReport, SkillNameCheckError> {
    let report = check_skill_name_with_input(input)?;
    Ok(include_resolved_clashes(
        report,
        input.name,
        input.target_scope,
        resolved,
    ))
}

/// Why a skill name collision check could not inspect its filesystem inputs.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SkillNameCheckError {
    /// Reading a parent skills directory failed.
    #[error("could not inspect {path}: {source}")]
    ReadDir {
        /// Directory whose entries could not be read.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Reading a candidate path failed.
    #[error("could not inspect {path}: {source}")]
    Inspect {
        /// Candidate path that could not be inspected.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// An indexed agent path query failed.
    #[error("could not load selected agent skills paths: {0}")]
    Database(String),
}

fn exists_lstat(path: &Path) -> Result<bool, SkillNameCheckError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(SkillNameCheckError::Inspect {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn case_insensitive_match(dir: &Path, name: &str) -> Result<Option<PathBuf>, SkillNameCheckError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SkillNameCheckError::ReadDir {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| SkillNameCheckError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_name = entry.file_name();
        if file_name.to_string_lossy().eq_ignore_ascii_case(name) {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn validates_rules_and_reports_scope_and_native_clashes_without_writing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let user = temp.path().join("user");
        let project = temp.path().join("project");
        let native = temp.path().join("agent/skills");
        fs::create_dir_all(user.join("skills/example")).expect("user folder");
        fs::create_dir_all(project.join("skills/example")).expect("target folder");
        fs::create_dir_all(project.join("skills/Foo")).expect("case folder");
        fs::create_dir_all(native.join("example")).expect("native folder");
        let stores = [
            (Scope::User, user.clone()),
            (Scope::Project, project.clone()),
        ];
        let agents = [AgentSkillsDir {
            agent: "codex".into(),
            path: native.clone(),
        }];
        let report = check_skill_name("example", Scope::Project, &agents, &stores).expect("check");
        assert!(
            report
                .warnings
                .iter()
                .any(|message| message.contains("user/skills/example"))
        );
        assert!(
            report
                .blocking
                .iter()
                .any(|message| message.contains("agent/skills/example"))
        );
        assert!(
            report
                .hits
                .iter()
                .any(|hit| hit.actions.contains(&ClashAction::Adopt)
                    && hit.actions.contains(&ClashAction::MoveAside))
        );
        for name in ["Foo", "foo bar", "../foo", &"a".repeat(65)] {
            let report = check_skill_name(name, Scope::Project, &[], &stores).expect("check");
            assert!(!report.blocking.is_empty());
        }
        let case = check_skill_name("foo", Scope::Project, &[], &stores).expect("case check");
        assert!(
            case.blocking
                .iter()
                .any(|message| message.contains("Case-insensitive")),
            "case report: {case:?}"
        );
        assert!(case.hits.iter().any(|hit| hit.path.ends_with("Foo")));
        let reserved =
            check_skill_name("agentuna", Scope::Project, &[], &stores).expect("reserved check");
        assert!(
            reserved
                .blocking
                .iter()
                .any(|message| message.contains("reserved"))
        );
        let entries = fs::read_dir(project.join("skills"))
            .expect("skills folder")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert!(!entries.iter().any(|entry| entry == "foo"));
    }
}
