//! Test-facing entry points for the at-app command bodies.
//!
//! Every Tauri command delegates to an `_inner`/`_impl` function that takes an injected
//! [`AppState`] instead of a Tauri [`tauri::State`]. Integration tests in `tests/` cannot name
//! those crate-private functions, so this module re-exports them under stable names. Each body
//! here is a one-line delegation: no command gains a new write path, guard or default.
//!
//! This module is not part of the webview contract and is never called by the app shell.

use std::sync::{Arc, Mutex};

use at_db::Database;
use at_store::paths::AppPaths;

use super::bulk_skill::{BulkSkillApplyRequest, BulkSkillBatchView};
use super::commands::CommandError;
use super::drift_commands::{
    BackupPreviewView, BackupRestoreRequest, BackupRestoreResultView, BackupRowView,
    DriftPreviewView, DriftSnapshotView,
};
use super::mcp_form::{McpPlanPreviewRequest, McpSaveRequest, McpSaveResultView};
use super::mcp_servers::{
    McpServerActionOutcome, McpServerActionRequest, McpServerListRequest, McpServerListView,
};
use super::rule_create::{
    InstructionCreateOptions, InstructionCreatePreview, InstructionCreateRequest,
};
use super::skill_agent::{SkillAgentApplyView, SkillAgentPreviewRequest, SkillAgentPreviewView};
use super::skill_delete::{
    SkillDeleteApplyRequest, SkillDeletePreviewRequest, SkillDeletePreviewView,
    SkillDeleteResultView,
};
use super::skill_detail::{
    SkillKeepCopyPreviewView, SkillKeepCopyRequest, SkillKeepCopyResultView,
};
use super::skill_transfer::{
    SkillTransferApplyRequest, SkillTransferApplyView, SkillTransferOptionsRequest,
    SkillTransferOptionsView, SkillTransferPreviewRequest, SkillTransferPreviewView,
};
use super::skill_wizard::{
    SkillAddApplyRequest, SkillAddApplyResultView, SkillAddPreviewRequest, SkillAddPreviewView,
};
use super::state::AppState;
use super::workflows::{InstructionSaveOutcome, InstructionSaveRequest};

/// Builds a [`AppState`] over an injected database and path set.
///
/// Test support only: production code constructs the same value in `app::run`.
#[must_use]
pub fn test_state(database: Database, paths: AppPaths) -> AppState {
    AppState::new(
        Arc::new(super::bridge::JobBridge::new(
            Arc::new(super::bridge::NullSink),
            Arc::new(super::bridge::TracingOperationsLog),
        )),
        None,
    )
    .with_database(database)
    .with_paths(paths)
}

/// Shared index handle used by helpers that need `&Database` rather than `&AppState`.
///
/// Test support only; the app shell owns exactly one such handle.
#[must_use]
pub fn database_handle(database: Database) -> Arc<Mutex<Database>> {
    Arc::new(Mutex::new(database))
}

/// One reviewed skill deployment plan, built from the same planner the webview uses.
///
/// # Errors
/// See `skill_agent_preview`.
pub fn skill_agent_preview(
    database: &rusqlite::Connection,
    paths: &AppPaths,
    request: SkillAgentPreviewRequest,
) -> Result<SkillAgentPreviewView, CommandError> {
    super::skill_agent::skill_agent_preview_impl(database, paths, request)
}

/// Applies one reviewed skill deployment plan.
///
/// # Errors
/// See `skill_agent_apply`.
pub fn skill_agent_apply(
    state: &AppState,
    preview_id: &str,
) -> Result<SkillAgentApplyView, CommandError> {
    super::skill_agent::skill_agent_apply_impl(state, preview_id)
}

/// Builds one reviewed Add Skill plan, including a first-run store creation.
///
/// # Errors
/// See `skill_add_preview`.
pub fn skill_add_preview_inner(
    state: &AppState,
    network: &super::network::NetworkState,
    request: SkillAddPreviewRequest,
) -> Result<SkillAddPreviewView, CommandError> {
    super::skill_wizard::skill_add_preview_impl(state, network, request)
}

/// Applies one reviewed Add Skill plan without a progress job.
///
/// # Errors
/// See `skill_add_apply_start`.
pub fn skill_add_apply_inner(
    state: &AppState,
    request: SkillAddApplyRequest,
) -> Result<SkillAddApplyResultView, CommandError> {
    super::skill_wizard::skill_add_apply_inner(state, request)
}

/// Applies several reviewed skill deployment plans as one atomic batch.
///
/// # Errors
/// See `bulk_skill_apply`.
pub fn bulk_skill_apply(
    state: &AppState,
    request: BulkSkillApplyRequest,
) -> Result<BulkSkillBatchView, CommandError> {
    super::bulk_skill::bulk_skill_apply_inner(state, request)
}

/// Lists the transfer destinations offered for one indexed skill.
///
/// # Errors
/// See `skill_transfer_options`.
pub fn skill_transfer_options(
    state: &AppState,
    request: SkillTransferOptionsRequest,
) -> Result<SkillTransferOptionsView, CommandError> {
    super::skill_transfer::skill_transfer_options_impl(state, request)
}

/// Reads the Skills-screen list projection, including lock state.
///
/// # Errors
/// See `skills_list`.
pub fn skills_list(
    state: &AppState,
    request: &super::skills_list::SkillsListRequest,
) -> Result<super::skills_list::SkillsListView, CommandError> {
    super::skills_list::skills_list_inner(state, request)
}

/// Plans one Promote, Move or Duplicate and caches its exact operations.
///
/// # Errors
/// See `skill_transfer_preview`.
pub fn skill_transfer_preview(
    state: &AppState,
    request: SkillTransferPreviewRequest,
) -> Result<SkillTransferPreviewView, CommandError> {
    super::skill_transfer::skill_transfer_preview_impl(state, request)
}

/// Applies one reviewed Promote, Move or Duplicate plan.
///
/// # Errors
/// See `skill_transfer_apply`.
pub fn skill_transfer_apply(
    state: &AppState,
    request: SkillTransferApplyRequest,
) -> Result<SkillTransferApplyView, CommandError> {
    super::skill_transfer::skill_transfer_apply_impl(state, request)
}

/// Plans one skill deletion and caches its ChangeSet.
///
/// # Errors
/// See `skill_delete_preview`.
pub fn skill_delete_preview(
    state: &AppState,
    request: SkillDeletePreviewRequest,
) -> Result<SkillDeletePreviewView, CommandError> {
    super::skill_delete::skill_delete_preview_impl(state, request)
}

/// Applies one reviewed skill deletion after the typed confirmation.
///
/// # Errors
/// See `skill_delete_apply`.
pub fn skill_delete_apply(
    state: &AppState,
    request: SkillDeleteApplyRequest,
) -> Result<SkillDeleteResultView, CommandError> {
    super::skill_delete::skill_delete_apply_impl(state, request)
}

/// Reviews the package overwrite that keeps one shadowed copy.
///
/// # Errors
/// See `skill_keep_copy_preview`.
pub fn skill_keep_copy_preview(
    state: &AppState,
    request: SkillKeepCopyRequest,
) -> Result<SkillKeepCopyPreviewView, CommandError> {
    super::skill_detail::skill_keep_copy_preview_inner(state, request)
}

/// Applies one reviewed keep-copy plan.
///
/// # Errors
/// See `skill_keep_copy_apply`.
pub fn skill_keep_copy_apply(
    state: &AppState,
    preview_id: &str,
) -> Result<SkillKeepCopyResultView, CommandError> {
    super::skill_detail::skill_keep_copy_apply_inner(state, preview_id)
}

/// Plans an MCP add, edit or rename and returns the reviewed ChangeSet view.
///
/// # Errors
/// See `mcp_plan_preview`.
pub fn mcp_plan_preview(
    state: &AppState,
    request: McpPlanPreviewRequest,
) -> Result<super::commands::McpChangeSetPreviewView, CommandError> {
    super::mcp_form::mcp_plan_preview_impl(state, request)
}

/// Applies one reviewed MCP plan, refusing stale previews and unacknowledged drops.
///
/// # Errors
/// See `mcp_save`.
pub fn mcp_save(
    state: &AppState,
    request: McpSaveRequest,
) -> Result<McpSaveResultView, CommandError> {
    super::mcp_form::mcp_save_impl(state, request)
}

/// Lists MCP servers through the same index query the MCP screen renders.
///
/// # Errors
/// See `mcp_server_list`.
pub fn mcp_server_list(
    state: &AppState,
    request: McpServerListRequest,
) -> Result<McpServerListView, CommandError> {
    super::mcp_servers::mcp_server_list_impl(state, request)
}

/// Plans or applies enabling one MCP server for one agent.
///
/// # Errors
/// See `mcp_server_enable`.
pub fn mcp_server_enable(
    state: &AppState,
    request: McpServerActionRequest,
) -> Result<McpServerActionOutcome, CommandError> {
    super::mcp_servers::run_action(state, request, super::mcp_servers::ActionKind::Enable)
}

/// Plans or applies disabling one MCP server for one agent or repository.
///
/// # Errors
/// See `mcp_server_disable`.
pub fn mcp_server_disable(
    state: &AppState,
    request: McpServerActionRequest,
) -> Result<McpServerActionOutcome, CommandError> {
    super::mcp_servers::run_action(state, request, super::mcp_servers::ActionKind::Disable)
}

/// Plans or applies deleting one managed MCP server.
///
/// # Errors
/// See `mcp_server_delete`.
pub fn mcp_server_delete(
    state: &AppState,
    request: McpServerActionRequest,
) -> Result<McpServerActionOutcome, CommandError> {
    super::mcp_servers::run_action(state, request, super::mcp_servers::ActionKind::Delete)
}

/// Plans one copy of an MCP server into another scope and caches its operations.
///
/// # Errors
/// See `mcp_server_copy_preview`.
pub fn mcp_server_copy_preview(
    state: &AppState,
    request: super::mcp_copy::McpCopyPreviewRequest,
) -> Result<super::mcp_copy::McpCopyPreviewView, CommandError> {
    super::mcp_copy::mcp_server_copy_preview_impl(state, request)
}

/// Applies one reviewed MCP copy plan.
///
/// # Errors
/// See `mcp_server_copy_apply`.
pub fn mcp_server_copy_apply(
    state: &AppState,
    request: super::mcp_copy::McpCopyApplyRequest,
) -> Result<super::mcp_copy::McpCopyApplyView, CommandError> {
    super::mcp_copy::mcp_server_copy_apply_impl(state, request)
}

/// Reports the scope availability the rule-create dialog renders.
///
/// # Errors
/// See `instruction_create_options`.
pub fn instruction_create_options(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<InstructionCreateOptions, CommandError> {
    super::rule_create::instruction_create_options_impl(state, project_id)
}

/// Resolves the target path for one rule file type and scope without writing.
///
/// # Errors
/// See `instruction_create_preview`.
pub fn instruction_create_preview(
    state: &AppState,
    file_type: &str,
    scope: &str,
    project_id: Option<&str>,
) -> Result<InstructionCreatePreview, CommandError> {
    super::rule_create::instruction_create_preview_impl(state, file_type, scope, project_id)
}

/// Creates the reviewed rule file and returns its absolute path.
///
/// # Errors
/// See `instruction_create`.
pub fn instruction_create(
    state: &AppState,
    request: InstructionCreateRequest,
    project_id: Option<&str>,
) -> Result<String, CommandError> {
    super::rule_create::instruction_create_impl(state, request, project_id)
}

/// Saves an instruction buffer through the guarded, managed-block pipeline.
///
/// # Errors
/// See `instruction_save`.
pub fn instruction_save(
    state: &AppState,
    request: InstructionSaveRequest,
) -> Result<InstructionSaveOutcome, CommandError> {
    super::workflows::instruction_save_inner(state, request)
}

/// Loads the redacted drift preview for one open event.
///
/// # Errors
/// See `drift_preview`.
pub fn drift_preview(state: &AppState, event_id: i64) -> Result<DriftPreviewView, CommandError> {
    super::drift_commands::drift_preview_inner(state, event_id)
}

/// Reads the Sync & drift rows with their live on-disk classification.
///
/// # Errors
/// See `drift_snapshot`.
pub fn drift_snapshot(state: &AppState) -> Result<DriftSnapshotView, CommandError> {
    super::drift_commands::drift_snapshot_inner(state)
}

/// Adopts one drifted deployment into the canonical store.
///
/// # Errors
/// See `drift_adopt`.
pub fn drift_adopt(state: &AppState, event_id: i64) -> Result<(), CommandError> {
    super::drift_commands::drift_adopt_inner(state, event_id)
}

/// Restores canonical content over one drifted deployment.
///
/// # Errors
/// See `drift_restore`.
pub fn drift_restore(state: &AppState, event_id: i64) -> Result<(), CommandError> {
    super::drift_commands::drift_restore_inner(state, event_id)
}

/// Lists the most recent safe-to-inspect backups.
///
/// # Errors
/// See `backups_list`.
pub fn backups_list(state: &AppState) -> Result<Vec<BackupRowView>, CommandError> {
    super::drift_commands::backups_list_inner(state)
}

/// Previews restoring one catalogue backup.
///
/// # Errors
/// See `backup_preview`.
pub fn backup_preview(
    state: &AppState,
    backup_id: &str,
) -> Result<BackupPreviewView, CommandError> {
    super::drift_commands::backup_preview_inner(state, backup_id)
}

/// Restores one verified catalogue backup through a journaled ChangeSet.
///
/// # Errors
/// See `backup_restore`.
pub fn backup_restore(
    state: &AppState,
    request: &BackupRestoreRequest,
) -> Result<BackupRestoreResultView, CommandError> {
    super::drift_commands::backup_restore_inner(state, request)
}

/// Reconciles every indexed deployment with its recorded baseline.
///
/// This is the same call the app watcher and `drift_rescan` make; tests use it to prove a
/// command's own write did not open a drift event.
///
/// # Errors
/// Returns the database error from the reconcile pass.
pub fn reconcile_drift(
    database: &Database,
) -> Result<Vec<at_engine::drift::DriftNotice>, at_db::DbError> {
    at_engine::drift::reconcile_indexed(database)
}

/// Reads the open drift events without reconciling.
///
/// # Errors
/// Returns the database error from the query.
pub fn open_drift_events(
    database: &Database,
) -> Result<Vec<at_db::drift::DriftEvent>, at_db::DbError> {
    at_db::drift::open_drift_events(database.connection(), None)
}
