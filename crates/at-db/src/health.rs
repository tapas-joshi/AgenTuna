//! Health-check repository, status mapping and retention over
//! `health_checks` (migration v6, owned by TF-015).
//!
//! One row is one MCP server check: who ran it (`trigger`), when
//! (`checked_at`, unix milliseconds), what it found (`status`, `latency_ms`,
//! `tools_count`) and, on failure, the redacted error (`error_code`,
//! `error_text_redacted` — never raw stderr).
//!
//! [`record_check`] is the only writer. It masks credential shapes a second
//! time through [`at_model::redact::Redactor`] (callers are expected to pass
//! already-redacted text; the shape pass is defence in depth for a forgotten
//! `***`), truncates `error_text_redacted` to [`MAX_ERROR_TEXT_LEN`] and
//! stores `checked_at` exactly as given so the scheduler's clock stays the
//! single source of time.
//!
//! Queries: [`last_n`] (newest rows per server), [`status_of`] (the aggregated
//! latest status — a query, never a stored column), [`failing_servers`] and
//! [`history`]. Each maps the stored status to a [`HealthDot`] the UI renders.
//!
//! Retention lives here and nowhere else: [`prune_health_checks`] keeps the
//! newest [`HEALTH_KEEP_PER_SERVER`] rows per server, batched like
//! [`crate::retention`]. The hourly cadence is [`HEALTH_PRUNE_INTERVAL`]; the
//! background task that calls it belongs to the app layer.
//!
//! This module issues no DDL: every statement is a query or row change against
//! the schema TF-015 created.

use std::time::Duration;

use at_model::{AgentId, Scope, SecretRedactor};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::DbError;

/// Newest rows per server the retention prune keeps.
pub const HEALTH_KEEP_PER_SERVER: usize = 200;

/// How often the background task runs [`prune_health_checks`].
pub const HEALTH_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Longest `error_text_redacted` the writer stores, in bytes of UTF-8.
///
/// Longer text is truncated on a character boundary; the column has no CHECK,
/// so the bound is enforced here — the single writer — and nowhere else.
pub const MAX_ERROR_TEXT_LEN: usize = 512;

/// What a check found.
///
/// The wire names match the `status` CHECK constraint in migration v6.
/// `Unknown` means a check never completed; `SkippedOffline` means an HTTP
/// check the network policy suppressed (TF-036).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HealthStatus {
    /// The server answered and listed its tools.
    Ok,
    /// The check ran and failed; `error_code`/`error_text_redacted` say why.
    Failed,
    /// The check never completed (startup timeout, cancelled before result).
    Unknown,
    /// The network policy skipped the check (offline mode, `background_health`).
    SkippedOffline,
}

impl HealthStatus {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Ok, Self::Failed, Self::Unknown, Self::SkippedOffline];

    /// The value stored in `health_checks.status`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::SkippedOffline => "skipped_offline",
        }
    }

    /// Parses a stored status.
    ///
    /// # Errors
    /// [`DbError::InvalidValue`] when the stored text is not a known status.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.as_str().eq_ignore_ascii_case(value))
            .ok_or_else(|| DbError::InvalidValue {
                field: "health_checks.status",
                reason: format!("unknown status `{value}`"),
            })
    }

    /// The UI dot this status maps to.
    #[must_use]
    pub const fn dot(self) -> HealthDot {
        match self {
            Self::Ok => HealthDot::Healthy,
            Self::Failed => HealthDot::Failing,
            Self::Unknown => HealthDot::Unknown,
            Self::SkippedOffline => HealthDot::SkippedOffline,
        }
    }
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kicked off a check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HealthTrigger {
    /// The user asked for a test.
    Manual,
    /// The health scheduler ran it.
    Scheduled,
    /// The app started and checked configured servers.
    Startup,
}

impl HealthTrigger {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Manual, Self::Scheduled, Self::Startup];

    /// The value stored in `health_checks.trigger`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::Startup => "startup",
        }
    }

    /// Parses a stored trigger.
    ///
    /// # Errors
    /// [`DbError::InvalidValue`] when the stored text is not a known trigger.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        Self::ALL
            .iter()
            .copied()
            .find(|trigger| trigger.as_str().eq_ignore_ascii_case(value))
            .ok_or_else(|| DbError::InvalidValue {
                field: "health_checks.trigger",
                reason: format!("unknown trigger `{value}`"),
            })
    }
}

impl std::fmt::Display for HealthTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Transport the checked server speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HealthTransport {
    /// A child process over stdio.
    Stdio,
    /// Streamable HTTP.
    Http,
    /// Server-sent events endpoint.
    Sse,
}

impl HealthTransport {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Stdio, Self::Http, Self::Sse];

    /// The value stored in `health_checks.transport`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Sse => "sse",
        }
    }

    /// Parses a stored transport.
    ///
    /// # Errors
    /// [`DbError::InvalidValue`] when the stored text is not a known transport.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        Self::ALL
            .iter()
            .copied()
            .find(|transport| transport.as_str().eq_ignore_ascii_case(value))
            .ok_or_else(|| DbError::InvalidValue {
                field: "health_checks.transport",
                reason: format!("unknown transport `{value}`"),
            })
    }
}

impl std::fmt::Display for HealthTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The dot the UI renders for a server's latest status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HealthDot {
    /// `ok`: the server answered.
    Healthy,
    /// `failed`: the newest check failed.
    Failing,
    /// `unknown`: a check never completed.
    Unknown,
    /// `skipped_offline`: the policy suppressed the check.
    SkippedOffline,
}

impl HealthDot {
    /// The status this dot maps back to, for round-trips.
    #[must_use]
    pub const fn status(self) -> HealthStatus {
        match self {
            Self::Healthy => HealthStatus::Ok,
            Self::Failing => HealthStatus::Failed,
            Self::Unknown => HealthStatus::Unknown,
            Self::SkippedOffline => HealthStatus::SkippedOffline,
        }
    }
}

/// One `health_checks` row as read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthCheck {
    /// Row id.
    pub id: i64,
    /// Server identity — the canonical item id when the server is managed,
    /// `None` for a native-only server (identified by `server_name`+`scope`).
    pub item_id: Option<String>,
    /// Display name of the server.
    pub server_name: String,
    /// Scope the server was checked under.
    pub scope: Scope,
    /// Project the check ran in, for project-scope servers.
    pub project_id: Option<String>,
    /// Agent the server deploys to, when recorded.
    pub agent_id: Option<AgentId>,
    /// Transport, when recorded.
    pub transport: Option<HealthTransport>,
    /// When the check ran, unix milliseconds.
    pub checked_at: i64,
    /// What the check found.
    pub status: HealthStatus,
    /// What kicked it off.
    pub trigger: HealthTrigger,
    /// Round-trip time for `ok` checks.
    pub latency_ms: Option<i64>,
    /// Tools the server listed for `ok` checks.
    pub tools_count: Option<i64>,
    /// Stable error code for `failed` checks.
    pub error_code: Option<String>,
    /// Redacted error text for `failed` checks.
    pub error_text_redacted: Option<String>,
}

/// One check result, as [`record_check`] consumes it.
#[derive(Clone, Debug)]
pub struct NewHealthCheck<'a> {
    /// Server identity; `None` for native-only servers.
    pub item_id: Option<&'a str>,
    /// Display name of the server.
    pub server_name: &'a str,
    /// Scope the server was checked under.
    pub scope: Scope,
    /// Project the check ran in.
    pub project_id: Option<&'a str>,
    /// Agent the server deploys to.
    pub agent_id: Option<&'a AgentId>,
    /// Transport used.
    pub transport: Option<HealthTransport>,
    /// When the check ran, unix milliseconds.
    pub checked_at_unix_ms: i64,
    /// What the check found.
    pub status: HealthStatus,
    /// What kicked it off.
    pub trigger: HealthTrigger,
    /// Round-trip time.
    pub latency_ms: Option<i64>,
    /// Tools listed.
    pub tools_count: Option<i64>,
    /// Stable error code.
    pub error_code: Option<&'a str>,
    /// Error text, already redacted by the caller (the column name says so);
    /// the writer still masks credential shapes and truncates to
    /// [`MAX_ERROR_TEXT_LEN`].
    pub error_text_redacted: Option<&'a str>,
}

impl<'a> NewHealthCheck<'a> {
    /// A manual check of `server_name` at `checked_at_unix_ms`.
    #[must_use]
    pub fn new(
        server_name: &'a str,
        scope: Scope,
        checked_at_unix_ms: i64,
        status: HealthStatus,
    ) -> Self {
        Self {
            item_id: None,
            server_name,
            scope,
            project_id: None,
            agent_id: None,
            transport: None,
            checked_at_unix_ms,
            status,
            trigger: HealthTrigger::Manual,
            latency_ms: None,
            tools_count: None,
            error_code: None,
            error_text_redacted: None,
        }
    }

    /// Sets the server identity.
    #[must_use]
    pub fn for_item(mut self, item_id: &'a str) -> Self {
        self.item_id = Some(item_id);
        self
    }

    /// Sets scope extras: project, agent and transport.
    #[must_use]
    pub fn in_context(
        mut self,
        project_id: Option<&'a str>,
        agent_id: Option<&'a AgentId>,
        transport: Option<HealthTransport>,
    ) -> Self {
        self.project_id = project_id;
        self.agent_id = agent_id;
        self.transport = transport;
        self
    }

    /// Marks the check as scheduler- or startup-driven.
    #[must_use]
    pub const fn triggered_by(mut self, trigger: HealthTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    /// Sets the latency and tool count of a successful check.
    #[must_use]
    pub const fn measured(mut self, latency_ms: i64, tools_count: i64) -> Self {
        self.latency_ms = Some(latency_ms);
        self.tools_count = Some(tools_count);
        self
    }

    /// Sets the error code and redacted text of a failed check.
    #[must_use]
    pub fn failed_with(mut self, error_code: &'a str, error_text: &'a str) -> Self {
        self.error_code = Some(error_code);
        self.error_text_redacted = Some(error_text);
        self
    }
}

/// Column list every `health_checks` read selects, in [`health_check_from_row`] order:
/// id, item_id, scope, project_id, agent_id, server_name, transport,
/// checked_at, status, trigger, latency_ms, tools_count, error_code,
/// error_text_redacted.
const HEALTH_COLUMNS: &str = "id, item_id, scope, project_id, agent_id, server_name, transport, \
     checked_at, status, trigger, latency_ms, tools_count, error_code, error_text_redacted";

/// Records one health check and returns its row id.
///
/// `error_text_redacted` is masked again with `redactor` when one is given —
/// pass the session redactor so resolved secret *values* are covered, not only
/// credential shapes — then credential shapes are masked with a fresh
/// [`at_model::redact::Redactor`] and the result truncated to
/// [`MAX_ERROR_TEXT_LEN`] on a character boundary. Other columns are stored
/// as given.
///
/// # Errors
///
/// [`DbError`] when the row cannot be inserted.
pub fn record_check(
    connection: &Connection,
    check: &NewHealthCheck<'_>,
    redactor: Option<&dyn SecretRedactor>,
) -> Result<i64, DbError> {
    let error_text = check
        .error_text_redacted
        .map(|text| sanitize_error_text(text, redactor));
    connection.execute(
        "INSERT INTO health_checks \
             (item_id, scope, project_id, agent_id, server_name, transport, checked_at, \
              status, trigger, latency_ms, tools_count, error_code, error_text_redacted) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            check.item_id,
            check.scope.wire_name(),
            check.project_id,
            check.agent_id.as_ref().map(ToString::to_string),
            check.server_name,
            check.transport.map(HealthTransport::as_str),
            check.checked_at_unix_ms,
            check.status.as_str(),
            check.trigger.as_str(),
            check.latency_ms,
            check.tools_count,
            check.error_code,
            error_text,
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

/// The `n` newest checks of `item_id`, descending `checked_at`.
///
/// Tolerates fewer than `n` rows; `n = 0` returns an empty list. `item_id` is
/// the server identity — for a native-only server pass the `native_items` id
/// the writer recorded.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn last_n(
    connection: &Connection,
    item_id: &str,
    n: usize,
) -> Result<Vec<HealthCheck>, DbError> {
    let limit = i64::try_from(n).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(&format!(
        "SELECT {HEALTH_COLUMNS} FROM health_checks \
          WHERE item_id = ?1 \
          ORDER BY checked_at DESC, id DESC \
          LIMIT ?2"
    ))?;
    let rows = statement
        .query_map(params![item_id, limit], health_check_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The aggregated latest status of `item_id` — a query, not a stored column.
///
/// `None` means the server was never checked; the UI renders that as
/// "not tested", distinct from a stored `unknown` (a check that never
/// completed).
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn status_of(connection: &Connection, item_id: &str) -> Result<Option<HealthStatus>, DbError> {
    let status: Option<String> = connection
        .query_row(
            "SELECT status FROM health_checks \
              WHERE item_id = ?1 \
              ORDER BY checked_at DESC, id DESC \
              LIMIT 1",
            [item_id],
            |row| row.get(0),
        )
        .optional()?;
    status.map(|value| HealthStatus::parse(&value)).transpose()
}

/// The newest check row of `item_id`, when one exists.
///
/// Convenience for callers that need latency/tools alongside the status;
/// [`status_of`] is the cheap path when only the dot is needed.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn latest_check(
    connection: &Connection,
    item_id: &str,
) -> Result<Option<HealthCheck>, DbError> {
    connection
        .query_row(
            &format!(
                "SELECT {HEALTH_COLUMNS} FROM health_checks \
                  WHERE item_id = ?1 \
                  ORDER BY checked_at DESC, id DESC \
                  LIMIT 1"
            ),
            [item_id],
            health_check_from_row,
        )
        .optional()
        .map_err(DbError::from)
}

/// One failing server: its newest check has `status = 'failed'`.
///
/// Servers never checked and servers whose newest row is `unknown` or
/// `skipped_offline` are excluded — only a *latest* failure counts, so a
/// server that recovered drops out of the list.
///
/// "Server" identity: `item_id` when the server is managed; for native-only
/// servers (`item_id` NULL) the `(agent_id, scope, server_name)` triple, so
/// two different native servers with the same display name do not merge.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn failing_servers(connection: &Connection, scope: Scope) -> Result<Vec<HealthCheck>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {HEALTH_COLUMNS} FROM ( \
            SELECT h.*, ROW_NUMBER() OVER ( \
                PARTITION BY COALESCE( \
                    h.item_id, \
                    'native:' || COALESCE(h.agent_id, '') || ':' || h.scope || ':' || h.server_name \
                ) ORDER BY h.checked_at DESC, h.id DESC \
            ) AS rn \
              FROM health_checks h \
             WHERE h.scope = ?1 \
        ) WHERE rn = 1 AND status = 'failed' \
        ORDER BY checked_at DESC, id DESC"
    ))?;
    let rows = statement
        .query_map([scope.wire_name()], health_check_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every check of `item_id` inside `[from_ms, to_ms)`, newest first.
///
/// Bounds are unix milliseconds, matching `checked_at`. `None` leaves that
/// end open.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn history(
    connection: &Connection,
    item_id: &str,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> Result<Vec<HealthCheck>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {HEALTH_COLUMNS} FROM health_checks \
          WHERE item_id = ?1 \
            AND (?2 IS NULL OR checked_at >= ?2) \
            AND (?3 IS NULL OR checked_at < ?3) \
          ORDER BY checked_at DESC, id DESC"
    ))?;
    let rows = statement
        .query_map(params![item_id, from_ms, to_ms], health_check_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Deletes all but the newest [`HEALTH_KEEP_PER_SERVER`] rows per server.
///
/// The single owner of `health_checks` retention (TM-009; TF-050's
/// [`crate::retention`] deliberately never touches this table). "Per server"
/// uses the same identity as [`failing_servers`]: `item_id` when set, else the
/// `(agent_id, scope, server_name)` triple — so native-only servers each keep
/// their own window instead of sharing one.
///
/// Deletes run in [`crate::retention::PRUNE_BATCH_SIZE`] batches, each its own
/// transaction, so an hourly prune never holds the write lock for long.
/// Returns the number of rows deleted.
///
/// # Errors
///
/// [`DbError`] when a statement fails.
pub fn prune_health_checks(connection: &Connection) -> Result<u64, DbError> {
    let mut deleted = 0_u64;
    loop {
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "DELETE FROM health_checks WHERE id IN ( \
                SELECT id FROM ( \
                    SELECT id, ROW_NUMBER() OVER ( \
                        PARTITION BY COALESCE( \
                            item_id, \
                            'native:' || COALESCE(agent_id, '') || ':' || scope || ':' || server_name \
                        ) ORDER BY checked_at DESC, id DESC \
                    ) AS rn \
                      FROM health_checks \
                ) WHERE rn > ?1 \
                LIMIT ?2 \
            )",
            params![
                i64::try_from(HEALTH_KEEP_PER_SERVER).unwrap_or(i64::MAX),
                i64::try_from(crate::retention::PRUNE_BATCH_SIZE).unwrap_or(i64::MAX),
            ],
        )?;
        transaction.commit()?;
        if changed == 0 {
            break;
        }
        deleted += u64::try_from(changed).unwrap_or(u64::MAX);
        if changed < crate::retention::PRUNE_BATCH_SIZE {
            break;
        }
    }
    Ok(deleted)
}

/// Applies the caller's redactor, then the shape pass, then the length bound.
fn sanitize_error_text(text: &str, redactor: Option<&dyn SecretRedactor>) -> String {
    let masked = match redactor {
        Some(redactor) => redactor.redact(text),
        None => text.to_owned(),
    };
    let masked = at_model::redact::Redactor::new().redact_text(&masked);
    if masked.len() <= MAX_ERROR_TEXT_LEN {
        return masked;
    }
    // Truncate on a char boundary; `MAX_ERROR_TEXT_LEN` is a byte budget.
    let mut end = MAX_ERROR_TEXT_LEN;
    while !masked.is_char_boundary(end) {
        end -= 1;
    }
    masked[..end].to_owned()
}

/// Maps one `health_checks` row; every query above selects [`HEALTH_COLUMNS`].
fn health_check_from_row(row: &Row<'_>) -> rusqlite::Result<HealthCheck> {
    // HEALTH_COLUMNS order: id(0), item_id(1), scope(2), project_id(3),
    // agent_id(4), server_name(5), transport(6), checked_at(7), status(8),
    // trigger(9), latency_ms(10), tools_count(11), error_code(12),
    // error_text_redacted(13).
    let scope: String = row.get(2)?;
    let agent_id: Option<String> = row.get(4)?;
    let transport: Option<String> = row.get(6)?;
    let status: String = row.get(8)?;
    let trigger: String = row.get(9)?;
    Ok(HealthCheck {
        id: row.get(0)?,
        item_id: row.get(1)?,
        scope: scope.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        project_id: row.get(3)?,
        agent_id: agent_id
            .map(|value| {
                value.parse::<AgentId>().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        server_name: row.get(5)?,
        transport: transport
            .map(|value| {
                HealthTransport::ALL
                    .iter()
                    .copied()
                    .find(|candidate| candidate.as_str().eq_ignore_ascii_case(&value))
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(UnknownHealthTransport {
                                value: value.clone(),
                            }),
                        )
                    })
            })
            .transpose()?,
        checked_at: row.get::<_, i64>(7)?,
        status: HealthStatus::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(&status))
            .ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(UnknownHealthStatus {
                        value: status.clone(),
                    }),
                )
            })?,
        trigger: HealthTrigger::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(&trigger))
            .ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(UnknownHealthTrigger {
                        value: trigger.clone(),
                    }),
                )
            })?,
        latency_ms: row.get(10)?,
        tools_count: row.get(11)?,
        error_code: row.get(12)?,
        error_text_redacted: row.get(13)?,
    })
}

/// Error for a stored `health_checks.status` outside the CHECK constraint.
#[derive(Debug, thiserror::Error)]
#[error("stored health status `{value}` is not one of ok, failed, unknown, skipped_offline")]
struct UnknownHealthStatus {
    value: String,
}

/// Error for a stored `health_checks.trigger` outside the CHECK constraint.
#[derive(Debug, thiserror::Error)]
#[error("stored health trigger `{value}` is not one of manual, scheduled, startup")]
struct UnknownHealthTrigger {
    value: String,
}

/// Error for a stored `health_checks.transport` outside the CHECK constraint.
#[derive(Debug, thiserror::Error)]
#[error("stored health transport `{value}` is not one of stdio, http, sse")]
struct UnknownHealthTransport {
    value: String,
}

#[cfg(test)]
mod tests {

    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        let statements = [
            "INSERT INTO health_checks (item_id, scope, project_id, agent_id, server_name, transport, checked_at, status, trigger, latency_ms, tools_count, error_code, error_text_redacted) VALUES ('itm_x', 'user', NULL, NULL, 'srv', 'stdio', 1000, 'ok', 'manual', 10, 2, NULL, NULL)",
            "SELECT id, item_id, scope, project_id, agent_id, server_name, transport, checked_at, status, trigger, latency_ms, tools_count, error_code, error_text_redacted FROM health_checks WHERE item_id = ?1 ORDER BY checked_at DESC, id DESC LIMIT ?2",
            "SELECT status FROM health_checks WHERE item_id = ?1 ORDER BY checked_at DESC, id DESC LIMIT 1",
            "SELECT id, item_id, scope, project_id, agent_id, server_name, transport, checked_at, status, trigger, latency_ms, tools_count, error_code, error_text_redacted FROM health_checks WHERE item_id = ?1 ORDER BY checked_at DESC, id DESC LIMIT 1",
            "SELECT id, item_id, scope, project_id, agent_id, server_name, transport, checked_at, status, trigger, latency_ms, tools_count, error_code, error_text_redacted FROM (SELECT h.*, ROW_NUMBER() OVER (PARTITION BY COALESCE(h.item_id, 'native:' || COALESCE(h.agent_id, '') || ':' || h.scope || ':' || h.server_name) ORDER BY h.checked_at DESC, h.id DESC) AS rn FROM health_checks h WHERE h.scope = ?1) WHERE rn = 1 AND status = 'failed' ORDER BY checked_at DESC, id DESC",
            "SELECT id, item_id, scope, project_id, agent_id, server_name, transport, checked_at, status, trigger, latency_ms, tools_count, error_code, error_text_redacted FROM health_checks WHERE item_id = ?1 AND (?2 IS NULL OR checked_at >= ?2) AND (?3 IS NULL OR checked_at < ?3) ORDER BY checked_at DESC, id DESC",
            "DELETE FROM health_checks WHERE id IN (SELECT id FROM (SELECT id, ROW_NUMBER() OVER (PARTITION BY COALESCE(item_id, 'native:' || COALESCE(agent_id, '') || ':' || scope || ':' || server_name) ORDER BY checked_at DESC, id DESC) AS rn FROM health_checks) WHERE rn > ?1 LIMIT ?2)",
        ];
        for statement in statements {
            connection
                .prepare(statement)
                .unwrap_or_else(|error| panic!("statement does not compile: {error}\n{statement}"));
            let lowered = statement.to_ascii_lowercase();
            for keyword in ["create ", "alter ", "drop "] {
                assert!(
                    !lowered.contains(keyword),
                    "statement must not change the schema: {statement}"
                );
            }
        }
    }
}
