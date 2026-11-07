//! Per-agent health-line computation for the Agents screen.
//!
//! Counts and path verification come from the `at-db` repositories. The copy-fallback marker is
//! derived from the recorded deployments: a `copy` row for an agent whose configured link mode is
//! `link` is exactly a state that began as a requested symlink and fell back. An agent configured
//! for copies from the start (`link_mode.<agent> = copy`, e.g. a custom agent without symlink
//! support) records `copy` rows too, so it never reports a fallback.

use std::path::Path;

use at_db::{Database, DbError};
use at_model::{AgentId, LinkMode};

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
    /// Whether a recorded `copy` deployment stands in for a requested link.
    pub copy_fallback: bool,
}

/// Why the health line could not be computed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HealthError {
    /// A database repository query failed.
    #[error(transparent)]
    Database(#[from] DbError),
    /// The settings row could not be read.
    #[error(transparent)]
    Settings(#[from] at_db::settings::SettingsError),
}

/// Computes the summary for `agent_id` using the current database contents.
///
/// `home` is injected so tests and callers can shorten paths without consulting the process
/// environment.
///
/// # Errors
/// Returns [`HealthError::Database`] if an agent, path, deployment or settings query fails.
pub fn line(
    database: &Database,
    agent_id: &AgentId,
    home: &Path,
) -> Result<HealthLine, HealthError> {
    line_with_connection(database.connection(), agent_id, home)
}

/// Whether a recorded `copy` deployment stands in for a requested link.
///
/// True when the agent is configured to link and at least one deployment of it is stored with
/// mode `copy`. A copy-only agent (`link_mode.<agent> = copy`) is never a fallback.
///
/// # Errors
/// Returns [`HealthError::Database`] when the settings or deployment read fails.
pub fn copy_fallback(
    connection: &rusqlite::Connection,
    agent_id: &AgentId,
) -> Result<bool, HealthError> {
    let settings = at_db::settings::load(connection)?;
    if settings.link_mode(agent_id) != LinkMode::Link {
        return Ok(false);
    }
    Ok(
        at_db::deployments::list_deployments_for_agent(connection, agent_id)?
            .iter()
            .any(|cell| cell.mode.as_deref() == Some("copy")),
    )
}

/// Computes an agent health summary over a caller-owned read connection.
///
/// This is useful for commands that must inspect the index without opening a
/// migration-capable [`Database`] handle.
///
/// # Errors
/// Returns [`HealthError::Database`] if an agent, path, deployment or settings query fails.
pub fn line_with_connection(
    connection: &rusqlite::Connection,
    agent_id: &AgentId,
    home: &Path,
) -> Result<HealthLine, HealthError> {
    let copy_fallback = copy_fallback(connection, agent_id)?;
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
        copy_fallback,
    })
}

/// UI state for one MCP server's latest health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LatestHealthDot {
    /// The newest completed check succeeded.
    Healthy,
    /// The newest completed check failed.
    Failing,
    /// The server is disabled in the active project.
    Disabled,
    /// No completed check exists.
    NotTested,
    /// A check is currently running.
    RunningCheck,
}

/// Health data consumed by a server row or detail panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestHealth {
    /// State indicator.
    pub dot: LatestHealthDot,
    /// Round-trip time for successful checks.
    pub latency_ms: Option<i64>,
    /// Short redacted reason from the failed check.
    pub short_reason: Option<String>,
    /// Suggested user action, when a failure provides one.
    pub suggested_action: Option<String>,
}

/// One failing server for the Overview attention list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingServerSummary {
    /// Stable managed item identity.
    pub item_id: String,
    /// Display name.
    pub server_name: String,
    /// Failing scope.
    pub scope: at_model::Scope,
    /// Redacted short reason.
    pub short_reason: Option<String>,
    /// Detail-screen route for the server.
    pub route: String,
}

/// Tracks one server's failure transition and emits only once until recovery.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FailureTransition {
    failing: bool,
}

impl FailureTransition {
    /// Observes a check; returns true only for the first failing check after success.
    pub fn observe(&mut self, status: at_db::health::HealthStatus) -> bool {
        let was_failing = self.failing;
        self.failing = status == at_db::health::HealthStatus::Failed;
        self.failing && !was_failing
    }
}

/// Computes the latest health state for a server.
///
/// # Errors
/// Returns a database error if the latest check cannot be read.
pub fn latest_health(
    connection: &rusqlite::Connection,
    item_id: &str,
    disabled_in_repo: bool,
    running_check: bool,
) -> Result<LatestHealth, at_db::DbError> {
    let dot = if disabled_in_repo {
        LatestHealthDot::Disabled
    } else if running_check {
        LatestHealthDot::RunningCheck
    } else {
        match at_db::health::latest_check(connection, item_id)? {
            None => LatestHealthDot::NotTested,
            Some(check) if check.status == at_db::health::HealthStatus::Ok => {
                return Ok(LatestHealth {
                    dot: LatestHealthDot::Healthy,
                    latency_ms: check.latency_ms,
                    short_reason: None,
                    suggested_action: None,
                });
            }
            Some(check) if check.status == at_db::health::HealthStatus::Failed => {
                let action = match check.error_code.as_deref() {
                    Some("Unauthorized401" | "SignInRequired") => Some("Sign in".to_owned()),
                    Some("UnresolvedCommand" | "NotExecutable") => Some("Fix command".to_owned()),
                    Some("TlsError") => Some("Review server configuration".to_owned()),
                    Some(_) => Some("Review server configuration".to_owned()),
                    None => None,
                };
                return Ok(LatestHealth {
                    dot: LatestHealthDot::Failing,
                    latency_ms: None,
                    short_reason: check.error_text_redacted,
                    suggested_action: action,
                });
            }
            Some(_) => LatestHealthDot::NotTested,
        }
    };
    Ok(LatestHealth {
        dot,
        latency_ms: None,
        short_reason: None,
        suggested_action: None,
    })
}

/// Queries failing MCP servers for one scope and builds detail routes.
///
/// Disabled rows are excluded when their item ids are supplied in `disabled_item_ids`.
///
/// # Errors
/// Returns a database error if the scope health query fails.
pub fn failing_summary(
    connection: &rusqlite::Connection,
    scope: at_model::Scope,
    disabled_item_ids: &std::collections::HashSet<String>,
) -> Result<Vec<FailingServerSummary>, at_db::DbError> {
    Ok(at_db::health::failing_servers(connection, scope)?
        .into_iter()
        .filter_map(|check| {
            let item_id = check.item_id?;
            if disabled_item_ids.contains(&item_id) {
                return None;
            }
            Some(FailingServerSummary {
                route: format!("/mcp/{item_id}"),
                item_id,
                server_name: check.server_name,
                scope: check.scope,
                short_reason: check.error_text_redacted,
            })
        })
        .collect())
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
    use super::*;
    use at_model::{ItemState, Scope, SyncLabel};
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
    const CLAUDE_SKILLS: &str = "/Users/example/.claude/skills";

    fn home() -> &'static Path {
        Path::new("/Users/example")
    }

    fn seed_agent(connection: &rusqlite::Connection, agent: AgentId, path: &str, verified: bool) {
        connection
            .execute(
                "INSERT INTO agents (id, display_name, installed, managed) VALUES (?1, ?1, 1, 1)",
                [agent.to_string()],
            )
            .expect("seed agent");
        connection
            .execute(
                "INSERT INTO agent_paths (agent_id, scope, kind, path, is_primary, \"exists\", verified) \
                 VALUES (?1, 'user', 'skills_dir', ?2, 1, 1, ?3)",
                rusqlite::params![agent.to_string(), path, verified],
            )
            .expect("seed agent path");
    }

    fn deploy(
        connection: &rusqlite::Connection,
        agent: AgentId,
        slug: &str,
        mode: &str,
        state: ItemState,
        sync_label: Option<SyncLabel>,
    ) -> at_model::ItemId {
        let item_id = at_model::ItemId::new();
        connection
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name, managed) \
                 VALUES (?1, 'skill', 'user', ?2, ?2, 1)",
                rusqlite::params![item_id.to_string(), slug],
            )
            .expect("seed item");
        let target = format!("{CLAUDE_SKILLS}/{slug}");
        let mut spec = at_db::deployments::DeploymentSpec::applied(
            &item_id,
            &agent,
            Scope::User,
            state,
            Some(mode),
            Some(&target),
            at_db::deployments::DeploymentHashes::none(),
        );
        if let Some(label) = sync_label {
            spec = spec.with_sync_label(label);
        }
        at_db::deployments::upsert_deployment(connection, &spec).expect("deployment");
        item_id
    }

    fn undeploy(connection: &rusqlite::Connection, item: &at_model::ItemId, agent: AgentId) {
        connection
            .execute(
                "DELETE FROM deployments WHERE item_id = ?1 AND agent_id = ?2",
                rusqlite::params![item.to_string(), agent.to_string()],
            )
            .expect("undeploy");
    }

    #[test]
    fn healthy_agent_without_deployments_shows_zero_counts_and_no_diagnostics() {
        let db = Database::open_in_memory().expect("database");
        seed_agent(db.connection(), AgentId::ClaudeCode, CLAUDE_SKILLS, true);
        let result = line(&db, &AgentId::ClaudeCode, home()).expect("health line");
        assert_eq!(result.text, "~/.claude/skills · 0 / 0 managed · 0 in sync");
        assert_eq!(result.diagnostic, None);
        assert!(!result.copy_fallback);
    }

    #[test]
    fn unverified_primary_path_keeps_the_last_known_path_and_needs_path() {
        let db = Database::open_in_memory().expect("database");
        seed_agent(db.connection(), AgentId::Codex, CLAUDE_SKILLS, false);
        let result = line(&db, &AgentId::Codex, home()).expect("health line");
        assert_eq!(result.text, "!~/.claude/skills · 0 / 0 managed · 0 in sync");
        assert_eq!(result.diagnostic, Some(HealthDiagnostic::NeedsPath));
        assert!(!result.copy_fallback);
    }

    #[test]
    fn missing_agent_reports_the_placeholder_path() {
        let db = Database::open_in_memory().expect("database");
        let result = line(&db, &AgentId::Codex, home()).expect("health line");
        assert_eq!(result.text, "!<path> · 0 / 0 managed · 0 in sync");
        assert_eq!(result.diagnostic, Some(HealthDiagnostic::NeedsPath));
    }

    #[test]
    fn copy_deployment_under_link_mode_reports_the_fallback_suffix() {
        let db = Database::open_in_memory().expect("database");
        let connection = db.connection();
        seed_agent(connection, AgentId::ClaudeCode, CLAUDE_SKILLS, true);
        deploy(
            connection,
            AgentId::ClaudeCode,
            "review",
            "copy",
            ItemState::Linked,
            Some(SyncLabel::InSync),
        );
        let result = line(&db, &AgentId::ClaudeCode, home()).expect("health line");
        assert_eq!(
            result.text,
            "~/.claude/skills · 1 / 1 managed · 1 in sync · copy fallback"
        );
        assert!(result.copy_fallback);
    }

    #[test]
    fn copy_only_agent_never_reports_a_fallback() {
        let mut db = Database::open_in_memory().expect("database");
        seed_agent(db.connection(), AgentId::ClaudeCode, CLAUDE_SKILLS, true);
        at_db::settings::set_link_mode(
            db.connection_mut(),
            &AgentId::ClaudeCode,
            LinkMode::Copy,
            at_db::ops::OperationActor::Ui,
        )
        .expect("copy-only agent");
        deploy(
            db.connection(),
            AgentId::ClaudeCode,
            "review",
            "copy",
            ItemState::Linked,
            Some(SyncLabel::InSync),
        );
        let result = line(&db, &AgentId::ClaudeCode, home()).expect("health line");
        assert_eq!(result.text, "~/.claude/skills · 1 / 1 managed · 1 in sync");
        assert!(!result.copy_fallback);
    }

    #[test]
    fn counts_change_after_deploy_and_undeploy_and_are_stable_between_calls() {
        let db = Database::open_in_memory().expect("database");
        let connection = db.connection();
        seed_agent(connection, AgentId::ClaudeCode, CLAUDE_SKILLS, true);
        deploy(
            connection,
            AgentId::ClaudeCode,
            "review",
            "symlink",
            ItemState::Linked,
            Some(SyncLabel::InSync),
        );
        let first = line(&db, &AgentId::ClaudeCode, home()).expect("first");
        let second = line(&db, &AgentId::ClaudeCode, home()).expect("second");
        assert_eq!(first, second);
        assert_eq!(first.text, "~/.claude/skills · 1 / 1 managed · 1 in sync");

        let extra = deploy(
            connection,
            AgentId::ClaudeCode,
            "extra",
            "symlink",
            ItemState::Off,
            Some(SyncLabel::ProjectOnly),
        );
        let after_deploy = line(&db, &AgentId::ClaudeCode, home()).expect("after deploy");
        assert_eq!(
            after_deploy.text,
            "~/.claude/skills · 2 / 2 managed · 1 in sync"
        );

        undeploy(connection, &extra, AgentId::ClaudeCode);
        let after_undeploy = line(&db, &AgentId::ClaudeCode, home()).expect("after undeploy");
        assert_eq!(
            after_undeploy.text,
            "~/.claude/skills · 1 / 1 managed · 1 in sync"
        );
    }

    #[test]
    fn native_only_deployment_is_total_but_not_managed() {
        let db = Database::open_in_memory().expect("database");
        let connection = db.connection();
        seed_agent(connection, AgentId::ClaudeCode, CLAUDE_SKILLS, true);
        deploy(
            connection,
            AgentId::ClaudeCode,
            "native",
            "native",
            ItemState::Native,
            None,
        );
        let result = line(&db, &AgentId::ClaudeCode, home()).expect("health line");
        assert_eq!(result.text, "~/.claude/skills · 0 / 1 managed · 0 in sync");
    }
    #[test]
    fn latest_health_maps_success_failure_and_never_tested() {
        let db = Database::open_in_memory().expect("database");
        let connection = db.connection();
        let item_id = at_model::ItemId::new().to_string();
        let never = latest_health(connection, &item_id, false, false).expect("never tested");
        assert!(
            failing_summary(
                connection,
                at_model::Scope::User,
                &std::collections::HashSet::new()
            )
            .expect("no failures")
            .is_empty()
        );
        assert_eq!(never.dot, LatestHealthDot::NotTested);

        at_db::health::record_check(
            connection,
            &at_db::health::NewHealthCheck::new(
                "server",
                at_model::Scope::User,
                1,
                at_db::health::HealthStatus::Ok,
            )
            .for_item(&item_id)
            .measured(17, 2),
            None,
        )
        .expect("record healthy check");
        let healthy = latest_health(connection, &item_id, false, false).expect("healthy");
        assert_eq!(healthy.dot, LatestHealthDot::Healthy);
        assert_eq!(healthy.latency_ms, Some(17));

        at_db::health::record_check(
            connection,
            &at_db::health::NewHealthCheck::new(
                "server",
                at_model::Scope::User,
                2,
                at_db::health::HealthStatus::Failed,
            )
            .for_item(&item_id)
            .failed_with("Unauthorized401", "401 Unauthorized"),
            None,
        )
        .expect("record failed check");
        let failing = latest_health(connection, &item_id, false, false).expect("failing");
        assert_eq!(failing.dot, LatestHealthDot::Failing);
        assert_eq!(failing.short_reason.as_deref(), Some("401 Unauthorized"));
        assert_eq!(failing.suggested_action.as_deref(), Some("Sign in"));
    }

    #[test]
    fn failure_transition_notifies_once_and_rearms_on_success() {
        let mut transition = FailureTransition::default();
        use at_db::health::HealthStatus::{Failed, Ok};
        assert!(transition.observe(Failed));
        assert!(!transition.observe(Failed));
        assert!(!transition.observe(Failed));
        assert!(!transition.observe(Ok));
        assert!(transition.observe(Failed));
    }

    #[test]
    fn scope_summary_counts_failure_and_excludes_disabled_item() {
        let db = Database::open_in_memory().expect("database");
        let connection = db.connection();
        let first = at_model::ItemId::new().to_string();
        let disabled = at_model::ItemId::new().to_string();
        for (id, name) in [(&first, "first"), (&disabled, "disabled")] {
            at_db::health::record_check(
                connection,
                &at_db::health::NewHealthCheck::new(
                    name,
                    at_model::Scope::User,
                    1,
                    at_db::health::HealthStatus::Failed,
                )
                .for_item(id)
                .failed_with("ServerError", "Unavailable"),
                None,
            )
            .expect("record failed check");
        }
        assert_eq!(
            latest_health(connection, &disabled, true, false)
                .expect("disabled state")
                .dot,
            LatestHealthDot::Disabled
        );
        let disabled = std::collections::HashSet::from([disabled]);
        let summary =
            failing_summary(connection, at_model::Scope::User, &disabled).expect("summary");
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].item_id, first);
        assert_eq!(summary[0].route, format!("/mcp/{}", summary[0].item_id));
    }
}
