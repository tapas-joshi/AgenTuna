//! Bulk skill operations as one atomic ChangeSet with one progress event per batch (TS-015).
//!
//! The UI reviews each selected skill through the existing single-item planner, then sends the
//! preview ids here. This command consumes those exact plans, validates their canonical packages
//! are unchanged, combines them into one ChangeSet and applies it atomically. Every item reports
//! progress, and the whole batch produces exactly one Tauri event.

use std::path::Path;

use at_engine::bulk_skill::{
    BulkIntent, BulkItemPlan, BulkKind, BulkProgressEvent, BulkProgressStage, apply_bulk, plan_bulk,
};
use at_model::{AgentId, ItemState};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use ts_rs::TS;

use super::{
    commands::CommandError,
    skill_agent::{ReviewedSkillPlan, take_reviewed_plans},
    state::AppState,
};

/// Event name for the single per-batch progress notification.
pub const EVENT_BULK_PROGRESS: &str = "agentuna:bulk-progress";

/// Input to a reviewed bulk apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct BulkSkillApplyRequest {
    /// Preview ids returned by `skill_agent_preview` for the selected skills.
    pub preview_ids: Vec<String>,
    /// Operation shown in history, for example `Enable 3 skills`.
    pub label: String,
}

/// One per-item progress line delivered in the batch event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct BulkSkillProgressView {
    /// Skill identity.
    pub item_id: String,
    /// `item_started`, `item_done` or `item_failed`.
    pub stage: String,
}

/// The one Tauri event emitted for a completed batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct BulkSkillBatchView {
    /// Stable batch identity for the UI to correlate the event.
    pub batch_id: String,
    /// Progress for every selected item.
    pub events: Vec<BulkSkillProgressView>,
    /// Operation journal identity; empty when the batch was refused before writing.
    pub operation_id: String,
    /// Number of file operations applied.
    pub files_changed: usize,
    /// Whether the batch wrote anything.
    pub applied: bool,
    /// Refusal reason, when the batch did not write.
    pub refusal: Option<String>,
}

/// Applies one reviewed bulk skill selection and emits exactly one progress event.
///
/// # Errors
/// Returns a typed error when a preview expired, the selection mixes scopes or agents in a way
/// the planner refuses, a canonical package changed, or the atomic apply failed.
#[tauri::command]
pub async fn bulk_skill_apply(
    app: AppHandle,
    state: State<'_, AppState>,
    request: BulkSkillApplyRequest,
) -> Result<BulkSkillBatchView, CommandError> {
    if request.preview_ids.is_empty() {
        return Err(CommandError::invalid_argument(
            "preview_ids",
            "select at least one skill",
        ));
    }
    let plans = take_reviewed_plans(&request.preview_ids)?;
    for plan in &plans {
        let current = at_store::manifest::hash_skill_folder(&plan.source)
            .map_err(|_| CommandError::unavailable("the canonical skill package is unavailable"))?
            .to_string();
        if current != plan.source_hash {
            return Err(CommandError::new(
                super::commands::CommandErrorCode::Conflict,
                format!(
                    "the canonical package changed after review: {}",
                    plan.source.display()
                ),
            ));
        }
    }
    let scope = plans
        .first()
        .map(|plan| plan.scope)
        .ok_or_else(|| CommandError::invalid_argument("preview_ids", "no skill plans"))?;
    if plans.iter().any(|plan| plan.scope != scope) {
        return Err(CommandError::invalid_argument(
            "preview_ids",
            "a batch must stay in one scope",
        ));
    }
    let mut targets = Vec::<AgentId>::new();
    for plan in &plans {
        if !targets.contains(&plan.agent) {
            targets.push(plan.agent.clone());
        }
    }
    let items = plans.iter().map(|plan| plan.item_id).collect::<Vec<_>>();
    let item_plans = plans
        .iter()
        .map(|plan| BulkItemPlan {
            item_id: plan.item_id,
            ops: plan
                .set
                .as_ref()
                .map(|set| set.ops.clone())
                .unwrap_or_default(),
            refusal: None,
        })
        .collect::<Vec<_>>();
    let kind = if plans.iter().all(|plan| plan.enabled) {
        BulkKind::Enable
    } else if plans.iter().all(|plan| !plan.enabled) {
        BulkKind::Disable
    } else {
        return Err(CommandError::invalid_argument(
            "preview_ids",
            "a batch enables or disables, never both",
        ));
    };
    let intent = BulkIntent {
        kind,
        items: items.clone(),
        targets,
        scope,
        project_id: plans.first().and_then(|plan| plan.project_id.clone()),
    };
    let plan = match plan_bulk(&intent, item_plans) {
        Ok(plan) => plan,
        Err(at_engine::bulk_skill::BulkPlanError::NothingToDo) => {
            let batch = BulkSkillBatchView {
                batch_id: uuid::Uuid::now_v7().to_string(),
                events: items
                    .iter()
                    .map(|item_id| BulkSkillProgressView {
                        item_id: item_id.to_string(),
                        stage: String::from("item_done"),
                    })
                    .collect(),
                operation_id: String::new(),
                files_changed: 0,
                applied: false,
                refusal: Some(String::from("nothing to do")),
            };
            let _ = app.emit(EVENT_BULK_PROGRESS, &batch);
            return Ok(batch);
        }
        Err(error) => {
            return Err(CommandError::invalid_argument(
                "preview_ids",
                &error.to_string(),
            ));
        }
    };
    let batch_id = plan.change_set.id.clone();
    let roots = at_fs::ApplyRoots::new(
        plans
            .iter()
            .flat_map(|plan| plan.authorized_roots.iter().cloned())
            .collect::<Vec<_>>(),
    )
    .map_err(|_| CommandError::unavailable("authorized skill directories are unavailable"))?;
    let store_root = plans
        .first()
        .map(|plan| plan.store_root.clone())
        .unwrap_or_else(|| Path::new("/").to_path_buf());
    let backup_root = plans
        .first()
        .map(|plan| plan.backup_root.clone())
        .unwrap_or_else(|| store_root.join("backups"));
    let database = state.database().map_err(CommandError::unavailable)?;
    let mut collected = Vec::new();
    let result = {
        let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::ApplyContext {
            store_root: &store_root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };
        apply_bulk(&plan, &mut context, &roots, |event| collected.push(event))
    };
    let batch = match result {
        Ok(outcome) => {
            state.record_writes(&plan.change_set, &store_root);
            update_deployments(database.connection(), &plans, &outcome.operation_id)?;
            BulkSkillBatchView {
                batch_id,
                events: collected.into_iter().map(progress_view).collect(),
                operation_id: outcome.operation_id,
                files_changed: outcome.completed,
                applied: true,
                refusal: None,
            }
        }
        Err(error) => BulkSkillBatchView {
            batch_id,
            events: collected.into_iter().map(progress_view).collect(),
            operation_id: String::new(),
            files_changed: 0,
            applied: false,
            refusal: Some(error.to_string()),
        },
    };
    let _ = app.emit(EVENT_BULK_PROGRESS, &batch);
    Ok(batch)
}

fn progress_view(event: BulkProgressEvent) -> BulkSkillProgressView {
    BulkSkillProgressView {
        item_id: event.item_id.to_string(),
        stage: match event.stage {
            BulkProgressStage::ItemStarted => "item_started",
            BulkProgressStage::ItemDone => "item_done",
            BulkProgressStage::ItemFailed => "item_failed",
            _ => "unknown",
        }
        .to_owned(),
    }
}

/// Records the per-item deployment cells for an applied batch.
fn update_deployments(
    connection: &rusqlite::Connection,
    plans: &[ReviewedSkillPlan],
    operation_id: &str,
) -> Result<(), CommandError> {
    for plan in plans {
        if plan.no_op {
            continue;
        }
        let state = if plan.enabled {
            ItemState::Linked
        } else {
            ItemState::Off
        };
        let mode = if plan.enabled { "symlink" } else { "none" };
        let target = plan.target.to_string_lossy();
        let spec =
            at_db::deployments::DeploymentSpec::new(&plan.item_id, &plan.agent, plan.scope, state)
                .with_mode(mode)
                .with_target_path(target.as_ref())
                .with_operation(operation_id);
        let spec = match plan.project_id.as_deref() {
            Some(project_id) => spec.with_project(project_id),
            None => spec,
        };
        at_db::deployments::upsert_deployment(connection, &spec).map_err(|error| {
            CommandError::internal(format!("cannot record deployments: {error}"))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_progress_stage_maps_to_a_stable_wire_name() {
        let item = at_model::ItemId::new();
        for (stage, expected) in [
            (BulkProgressStage::ItemStarted, "item_started"),
            (BulkProgressStage::ItemDone, "item_done"),
            (BulkProgressStage::ItemFailed, "item_failed"),
        ] {
            let view = progress_view(BulkProgressEvent {
                item_id: item,
                stage,
            });
            assert_eq!(view.item_id, item.to_string());
            assert_eq!(view.stage, expected);
        }
    }

    #[test]
    fn the_batch_event_name_is_the_one_the_ui_listens_for() {
        assert_eq!(EVENT_BULK_PROGRESS, "agentuna:bulk-progress");
    }
}
