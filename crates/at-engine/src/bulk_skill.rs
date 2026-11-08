//! All-or-nothing aggregation for already preflighted skill plans.

use std::collections::BTreeSet;

use at_model::{AgentId, ChangeSet, FileOp, ItemId, OperationKind, Scope};
use thiserror::Error;

/// Operation represented by a bulk skill request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BulkKind {
    /// Promote skills to a broader scope.
    Promote,
    /// Enable skills for selected agents.
    Enable,
    /// Move skills to another scope.
    Move,
    /// Disable skills for selected agents.
    Disable,
}

/// The selected skills and deployment targets for one bulk operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkIntent {
    /// Requested operation.
    pub kind: BulkKind,
    /// Stable identities selected by the user.
    pub items: Vec<ItemId>,
    /// Agents targeted by enable and disable operations.
    pub targets: Vec<AgentId>,
    /// Scope from which the operation is initiated.
    pub scope: Scope,
    /// Project identity when the operation is project-local.
    pub project_id: Option<String>,
}

/// One item's already-planned operations or its preflight refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkItemPlan {
    /// Stable skill identity.
    pub item_id: ItemId,
    /// File operations produced by the existing single-item planners.
    pub ops: Vec<FileOp>,
    /// Refusal reason from a single-item planner, when planning failed.
    pub refusal: Option<String>,
}

/// A per-item reason why a bulk request was refused before writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkRefusal {
    /// Skill that could not be planned.
    pub item_id: ItemId,
    /// Explanation supplied by the item planner.
    pub reason: String,
}

/// A complete reviewed bulk ChangeSet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkPlan {
    /// One ChangeSet containing every selected item's file operations.
    pub change_set: ChangeSet,
    /// Stable item order represented in the ChangeSet.
    pub items: Vec<ItemId>,
}

/// Why a bulk plan could not be produced.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BulkPlanError {
    /// One or more item planners refused; no ChangeSet is returned.
    #[error("bulk skill preflight refused {0} item(s)")]
    Preflight(usize, Vec<BulkRefusal>),
    /// No item had a valid operation to apply.
    #[error("bulk skill operation has nothing to do")]
    NothingToDo,
    /// An item was omitted, duplicated or not part of the requested selection.
    #[error("bulk item plan set does not match the requested selection")]
    InvalidItems,
    /// Two planned operations target the same path, so they cannot safely share one ChangeSet.
    #[error("bulk skill plans contain duplicate target path: {0}")]
    DuplicateTarget(String),
    /// A ChangeSet could not be validated.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
}

/// Combines item plans from the existing skill planners into one atomic ChangeSet.
///
/// The caller runs each selected item's single-item planner first. Every refusal aborts the whole
/// batch; successful plans are never partially applied. Applying the returned ChangeSet through
/// `at-fs` gives the batch one operation record and one rollback boundary.
///
/// # Errors
/// Returns all item refusals, `NothingToDo`, selection mismatches, duplicate target paths, or
/// ChangeSet validation failures without performing writes.
pub fn plan_bulk(intent: &BulkIntent, plans: Vec<BulkItemPlan>) -> Result<BulkPlan, BulkPlanError> {
    if intent.items.is_empty() || plans.is_empty() {
        return Err(BulkPlanError::NothingToDo);
    }
    let selected: BTreeSet<_> = intent.items.iter().copied().collect();
    let planned: BTreeSet<_> = plans.iter().map(|plan| plan.item_id).collect();
    if selected.len() != intent.items.len() || planned.len() != plans.len() || selected != planned {
        return Err(BulkPlanError::InvalidItems);
    }

    let refusals: Vec<_> = plans
        .iter()
        .filter_map(|plan| {
            plan.refusal.as_ref().map(|reason| BulkRefusal {
                item_id: plan.item_id,
                reason: reason.clone(),
            })
        })
        .collect();
    if !refusals.is_empty() {
        return Err(BulkPlanError::Preflight(refusals.len(), refusals));
    }

    let mut ops = Vec::<FileOp>::new();
    let mut targets = BTreeSet::new();
    for plan in plans {
        if plan.ops.is_empty() {
            continue;
        }
        for op in plan.ops {
            let Some(destination) = op.destination() else {
                ops.push(op);
                continue;
            };
            let path = destination.to_path_buf();
            if !targets.insert(path.clone()) {
                return Err(BulkPlanError::DuplicateTarget(path.display().to_string()));
            }
            ops.push(op);
        }
    }
    if ops.is_empty() {
        return Err(BulkPlanError::NothingToDo);
    }

    let kind = match intent.kind {
        BulkKind::Promote => "promote",
        BulkKind::Enable => "deploy",
        BulkKind::Move => "move",
        BulkKind::Disable => "undeploy",
    };
    let change_set = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: uuid::Uuid::now_v7().to_string(),
        operation_kind: OperationKind::parse(kind)?,
        scope: Some(intent.scope),
        project_id: intent.project_id.clone(),
        summary: format!("Bulk {:?} skills", intent.kind),
        ops,
        created_at_unix_seconds: 0,
    };
    change_set.validate()?;
    Ok(BulkPlan {
        change_set,
        items: intent.items.clone(),
    })
}

/// Stage reported for one selected skill during batch application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BulkProgressStage {
    /// The batch is about to apply this item's operations.
    ItemStarted,
    /// The batch applied this item's operations.
    ItemDone,
    /// The atomic ChangeSet failed; this item's operations were rolled back.
    ItemFailed,
}

/// One per-item progress notification emitted during batch application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct BulkProgressEvent {
    /// Stable skill identity.
    pub item_id: ItemId,
    /// Current application stage.
    pub stage: BulkProgressStage,
}

/// Failure while applying a complete bulk plan.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BulkApplyError {
    /// The atomic ChangeSet failed and at-fs restored its pre-state.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
}

/// Applies one reviewed batch ChangeSet and reports progress for every item.
///
/// `at-fs` owns the atomic rollback boundary: a failed operation restores all operations from the
/// ChangeSet before this function returns.
///
/// # Errors
/// Returns the ChangeSet application error after reporting failure for every selected item.
pub fn apply_bulk(
    plan: &BulkPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
    progress: impl FnMut(BulkProgressEvent),
) -> Result<at_fs::ApplyOutcome, BulkApplyError> {
    apply_bulk_inner(plan, context, roots, None, progress)
}

/// Like [`apply_bulk`], registering expected file writes before atomic replacement.
pub fn apply_bulk_with_registry(
    plan: &BulkPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
    registry: &at_fs::RecentWrites,
    progress: impl FnMut(BulkProgressEvent),
) -> Result<at_fs::ApplyOutcome, BulkApplyError> {
    apply_bulk_inner(plan, context, roots, Some(registry), progress)
}

fn apply_bulk_inner(
    plan: &BulkPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
    registry: Option<&at_fs::RecentWrites>,
    mut progress: impl FnMut(BulkProgressEvent),
) -> Result<at_fs::ApplyOutcome, BulkApplyError> {
    for item_id in &plan.items {
        progress(BulkProgressEvent {
            item_id: *item_id,
            stage: BulkProgressStage::ItemStarted,
        });
    }
    let result = match registry {
        Some(registry) => {
            at_fs::apply_with_roots_and_registry(&plan.change_set, context, roots, registry)
        }
        None => at_fs::apply_with_roots(&plan.change_set, context, roots),
    };
    match result {
        Ok(outcome) => {
            for item_id in &plan.items {
                progress(BulkProgressEvent {
                    item_id: *item_id,
                    stage: BulkProgressStage::ItemDone,
                });
            }
            Ok(outcome)
        }
        Err(error) => {
            for item_id in &plan.items {
                progress(BulkProgressEvent {
                    item_id: *item_id,
                    stage: BulkProgressStage::ItemFailed,
                });
            }
            Err(BulkApplyError::Apply(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_model::{ExpectedState, Scope};

    fn intent(items: Vec<ItemId>, kind: BulkKind) -> BulkIntent {
        BulkIntent {
            kind,
            items,
            targets: vec![AgentId::Codex, AgentId::ClaudeCode],
            scope: Scope::User,
            project_id: None,
        }
    }

    fn write(path: &str) -> FileOp {
        FileOp::Write {
            path: path.into(),
            bytes: b"skill".to_vec(),
            expected: ExpectedState::Absent,
            follow_symlink: false,
            backup: true,
        }
    }

    #[test]
    fn preflight_refusal_returns_item_reason_and_no_changeset() {
        let item = ItemId::new();
        let error = plan_bulk(
            &intent(vec![item], BulkKind::Enable),
            vec![BulkItemPlan {
                item_id: item,
                ops: vec![write("/sandbox/skill")],
                refusal: Some("locked by policy".to_owned()),
            }],
        )
        .expect_err("locked item refuses the entire batch");
        assert!(matches!(
            error,
            BulkPlanError::Preflight(1, ref refusals)
                if refusals[0].item_id == item && refusals[0].reason == "locked by policy"
        ));
    }

    #[test]
    fn six_target_operations_form_one_reviewed_changeset() {
        let items = [ItemId::new(), ItemId::new(), ItemId::new()];
        let plans = items
            .iter()
            .enumerate()
            .map(|(index, item)| BulkItemPlan {
                item_id: *item,
                ops: vec![
                    write(&format!("/sandbox/{index}/codex")),
                    write(&format!("/sandbox/{index}/claude")),
                ],
                refusal: None,
            })
            .collect();
        let plan = plan_bulk(&intent(items.to_vec(), BulkKind::Enable), plans).expect("batch plan");
        assert_eq!(plan.change_set.ops.len(), 6);
        assert_eq!(plan.change_set.operation_kind.as_str(), "deploy");
        assert_eq!(plan.items, items);
    }

    #[test]
    fn no_valid_file_operations_returns_nothing_to_do() {
        let item = ItemId::new();
        let error = plan_bulk(
            &intent(vec![item], BulkKind::Enable),
            vec![BulkItemPlan {
                item_id: item,
                ops: Vec::new(),
                refusal: None,
            }],
        )
        .expect_err("empty plan");
        assert!(matches!(error, BulkPlanError::NothingToDo));
    }
    #[test]
    fn batch_apply_records_one_operation_six_files_and_per_item_progress() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let root = temporary.path().canonicalize().expect("canonical root");
        let backup_folder = root.join("backups");
        std::fs::create_dir_all(&backup_folder).expect("backup directory");
        let database = at_db::Database::open_in_memory().expect("database");
        let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::ApplyContext {
            store_root: &root,
            backup_folder: &backup_folder,
            recorder: &mut recorder,
        };
        let roots = at_fs::ApplyRoots::new([root.clone()]).expect("authorized root");
        let items = [ItemId::new(), ItemId::new(), ItemId::new()];
        let plans = items
            .iter()
            .enumerate()
            .map(|(index, item)| BulkItemPlan {
                item_id: *item,
                ops: vec![
                    write(&root.join(format!("{index}/codex")).to_string_lossy()),
                    write(&root.join(format!("{index}/claude")).to_string_lossy()),
                ],
                refusal: None,
            })
            .collect();
        let plan = plan_bulk(&intent(items.to_vec(), BulkKind::Enable), plans).expect("batch plan");
        let mut events = Vec::new();
        apply_bulk(&plan, &mut context, &roots, |event| events.push(event)).expect("atomic apply");
        assert_eq!(events.len(), 6);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.stage == BulkProgressStage::ItemStarted)
                .count(),
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.stage == BulkProgressStage::ItemDone)
                .count(),
            3
        );
        assert_eq!(
            database
                .connection()
                .query_row("SELECT COUNT(*) FROM operations", [], |row| row
                    .get::<_, i64>(0))
                .expect("operation count"),
            1
        );
        assert_eq!(
            database
                .connection()
                .query_row("SELECT COUNT(*) FROM operation_files", [], |row| row
                    .get::<_, i64>(0))
                .expect("file count"),
            6
        );
    }
}
