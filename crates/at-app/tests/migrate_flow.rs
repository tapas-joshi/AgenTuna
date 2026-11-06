//! End-to-end migration flow test over the at-app command layer.
//!
//! Uses [`at_migrate::testkit::MigrationFixture`] inside a throwaway sandbox.
//! Exercises `migrate_scan` -> `migrate_conflicts` -> `migrate_plan` -> `migrate_apply` -> `migrate_apply_result`
//! -> `operation_undo_check` -> `operation_undo` through the command-facing API.
//! Verifies that undo restores the original filesystem tree.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use at_app::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
use at_app::app::migrate::{
    MigrateApplyRequest, MigratePlanRequest, MigrateResolutionView, migrate_apply_inner,
    migrate_apply_result_inner, migrate_conflicts_inner, migrate_plan_inner, migrate_scan_inner,
    operation_undo_check_inner,
};
use at_app::app::overwrite::operation_undo_inner;
use at_app::app::state::AppState;
use at_migrate::testkit::{MigrationFixture, NATIVE_SKILL};
use at_model::{Agent, AgentId, LinkMode};
use at_store::paths::{AppPaths, HomeSource, PathEnvironment, PathOverrides};

#[derive(Default)]
struct RecordingSink {
    events: std::sync::Mutex<Vec<JobEvent>>,
}

impl EventSink for RecordingSink {
    fn emit(&self, event: JobEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn migrate_scan_plan_apply_and_undo_restores_original_tree() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let filter = fixture.filter();
    let before = filter.capture()?;

    let paths = AppPaths::resolve(&PathEnvironment {
        home: HomeSource::Injected(fixture.paths().home.clone()),
        overrides: PathOverrides {
            home: Some(fixture.paths().support_dir.clone()),
            user_store: Some(fixture.paths().user_store.clone()),
            project_store: Some(fixture.paths().project_store.clone()),
            ..Default::default()
        },
        project_root: Some(fixture.paths().project.clone()),
    })?;

    std::fs::create_dir_all(&fixture.paths().support_dir)?;
    let database = at_db::Database::open_file(&fixture.paths().db_path)?;

    // Register detected agents in DB so migration_inputs recognizes them.
    for agent in [AgentId::ClaudeCode, AgentId::Codex, AgentId::Omp] {
        at_db::agents::upsert_agent(
            database.connection(),
            &Agent {
                id: agent.clone(),
                display_name: agent.to_string(),
                version: None,
                detected: true,
            },
        )?;
    }

    let sink = Arc::new(RecordingSink::default());
    let bridge = Arc::new(JobBridge::new(sink.clone(), Arc::new(TracingOperationsLog)));

    let app = AppState::new(bridge.clone(), None)
        .with_database(database)
        .with_paths(paths);

    // 1. Scan
    let scan = migrate_scan_inner(&app, None).await?;
    assert!(!scan.items.is_empty(), "native scan must discover items");

    // Select the native skill copies that conflict in User scope
    let home_str = fixture.paths().home.display().to_string();
    let conflict_items = scan
        .items
        .iter()
        .filter(|item| item.name == NATIVE_SKILL && item.path.starts_with(&home_str))
        .collect::<Vec<_>>();
    assert!(!conflict_items.is_empty(), "expected native skill in scan");

    let selected_paths = vec![conflict_items[0].path.clone()];

    // 2. Conflicts
    let conflicts = migrate_conflicts_inner(&app, None, selected_paths.clone()).await?;
    let resolutions = conflicts
        .conflicts
        .into_iter()
        .map(|conflict| MigrateResolutionView {
            name: conflict.name,
            choice: "keep_a".to_owned(),
            names: Vec::new(),
        })
        .collect::<Vec<_>>();
    // 3. Plan
    let plan = migrate_plan_inner(
        &app,
        MigratePlanRequest {
            project_id: None,
            selected_paths: selected_paths.clone(),
            resolutions: resolutions.clone(),
            strategy: "replace_with_links".to_owned(),
        },
    )
    .await?;
    assert!(plan.counts.items > 0, "plan must include selected items");

    // 4. Apply
    let job_id = migrate_apply_inner(
        &app,
        MigrateApplyRequest {
            project_id: None,
            selected_paths: selected_paths.clone(),
            resolutions,
            strategy: "replace_with_links".to_owned(),
            plan_hash: plan.plan_hash,
        },
    )
    .await?;

    // Poll for job completion
    let start = std::time::Instant::now();
    let mut apply_result = None;
    let mut last_err = None;
    while start.elapsed() < Duration::from_secs(5) {
        match migrate_apply_result_inner(&app, job_id.as_str()) {
            Ok(result) => {
                apply_result = Some(result);
                break;
            }
            Err(err) => {
                last_err = Some(err);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    if apply_result.is_none() {
        let events = sink.events.lock().unwrap().clone();
        eprintln!("RECORDED EVENTS: {:?}", events);
        eprintln!("LAST RESULT ERR: {:?}", last_err);
        panic!("migrate_apply job must complete");
    }
    let apply_result = apply_result.unwrap();
    assert_eq!(apply_result.counts.items, plan.counts.items);

    // Verify tree changed after apply
    let after_apply = filter.capture()?;
    assert_ne!(before, after_apply, "apply must modify the tree");

    // 5. Undo readiness check
    let readiness = operation_undo_check_inner(&app, &apply_result.operation_id)?;
    assert!(readiness.known);
    assert!(!readiness.already_undone);
    assert!(!readiness.expired);
    assert!(readiness.conflicted_paths.is_empty());

    // 6. Undo
    let undo_view = operation_undo_inner(&app, &apply_result.operation_id, false)?;
    assert!(!undo_view.undo_operation_id.is_empty());

    // 7. Verify undo restored original tree (excluding SQLite bookkeeping)
    let after_undo = filter.capture()?;
    let mut bookkeeping = filter.bookkeeping().to_vec();
    bookkeeping.push(fixture.paths().support_key.clone());
    bookkeeping.push("home/Library".to_owned());
    after_undo
        .diff(&before)
        .excluding(&bookkeeping)
        .ensure_empty("undo must restore the original tree")?;

    tokio::task::spawn_blocking(move || {
        bridge.shutdown(Duration::from_secs(1));
        drop(app);
    })
    .await?;
    Ok(())
}
