//! Per-agent health-line computation for the Agents screen.
//!
//! Counts and path verification come from the `at-db` repositories. The fallback marker is
//! supplied by the deployment caller because the shipped deployments schema does not persist
//! whether a `copy` row followed a failed symlink attempt.

use std::path::Path;

use at_db::{Database, DbError};
use at_model::AgentId;

/// Health-line diagnostic requiring a user action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HealthDiagnostic {
    /// The last known primary path has not been verified and needs probing.
    NeedsPath,
}

/// Computed one-line health summary and optional diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthLine {
    /// Exact text displayed on the Agents card.
    pub text: String,
    /// Diagnostic associated with an unverified primary path.
    pub diagnostic: Option<HealthDiagnostic>,
}

/// Why the health line could not be computed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HealthError {
    /// A database repository query failed.
    #[error(transparent)]
    Database(#[from] DbError),
}

/// Computes the summary for `agent_id` using the current database contents.
///
/// `home` is injected so tests and callers can shorten paths without consulting the process
/// environment. `copy_fallback` must be true only when deployment recorded that a symlink
/// attempt failed and the deployment fell back to copy mode; a healthy copy-only agent passes
/// false.
///
/// # Errors
/// Returns [`HealthError::Database`] if an agent, path or deployment query fails.
pub fn line(
    database: &Database,
    agent_id: &AgentId,
    home: &Path,
    copy_fallback: bool,
) -> Result<HealthLine, HealthError> {
    let connection = database.connection();
    let agents = at_db::agents::list_agents_on_mac(connection)?;
    let agent = agents.iter().find(|agent| &agent.id == agent_id);
    let paths = at_db::agents::list_agent_paths(connection, agent_id)?;
    let primary_path = agent.and_then(|agent| agent.primary_path.as_deref());
    let verified = primary_path.is_some_and(|primary| {
        paths.iter().any(|entry| {
            entry.is_primary == Some(true)
                && entry.path.as_deref() == Some(primary)
                && entry.verified
                && entry.exists == Some(true)
        })
    });
    let path = primary_path.map_or_else(|| "<path>".to_owned(), |path| display_path(path, home));
    let path_text = if verified { path } else { format!("!{path}") };
    let counts = if agent.is_some_and(|agent| agent.managed) {
        at_db::deployments::health_counts(connection, agent_id)?
    } else {
        at_db::deployments::HealthCounts::default()
    };
    let mut text = format!(
        "{path_text} · {} / {} managed · {} in sync",
        counts.managed, counts.total, counts.in_sync
    );
    if copy_fallback {
        text.push_str(" · copy fallback");
    }
    Ok(HealthLine {
        text,
        diagnostic: (!verified).then_some(HealthDiagnostic::NeedsPath),
    })
}

fn display_path(path: &Path, home: &Path) -> String {
    path.strip_prefix(home).map_or_else(
        |_| path.to_string_lossy().into_owned(),
        |relative| {
            if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.to_string_lossy())
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::display_path;
    use std::path::Path;

    #[test]
    fn paths_under_home_use_tilde_and_external_paths_remain_absolute() {
        assert_eq!(
            display_path(
                Path::new("/home/example/.tool/skills"),
                Path::new("/home/example")
            ),
            "~/.tool/skills"
        );
        assert_eq!(
            display_path(Path::new("/opt/tool/skills"), Path::new("/home/example")),
            "/opt/tool/skills"
        );
    }
}
