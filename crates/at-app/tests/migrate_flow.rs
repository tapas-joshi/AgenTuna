//! End-to-end migration flow test over the at-app command layer.
//!
//! Uses [`at_migrate::testkit::MigrationFixture`] inside a throwaway sandbox.
//! Exercises `migrate_scan` -> `migrate_conflicts` -> `migrate_plan` -> `migrate_apply` -> `migrate_apply_result`
//! -> `operation_undo_check` -> `operation_undo` through the command-facing API.
//! Verifies that undo restores the original filesystem tree.

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use at_app::app::bridge::{EventSink, JobBridge, JobEvent, JobId, JobState, TracingOperationsLog};
use at_app::app::migrate::{
    MigrateApplyRequest, MigratePlanRequest, MigrateResolutionView, migrate_apply_inner,
    migrate_apply_result_inner, migrate_conflicts_inner, migrate_plan_inner, migrate_scan_inner,
    migrate_scan_result_inner, migrate_scan_start_inner, operation_undo_check_inner,
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
async fn scan_command_keeps_empty_missing_and_unreadable_roots_with_found_items()
-> Result<(), Box<dyn Error>> {
    use at_app::app::migrate::MigrateScanRootStatus;

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let found = home.join(".claude/skills/example");
    let empty = home.join(".codex/skills");
    let missing = home.join(".agents/skills");
    let malformed = home.join(".codex/config.toml");
    std::fs::create_dir_all(&found)?;
    std::fs::create_dir_all(&empty)?;
    std::fs::write(
        found.join("SKILL.md"),
        "---\nname: example\ndescription: fixture\n---\n",
    )?;
    std::fs::write(&malformed, "token = \"fixture-secret\"\ninvalid = [")?;
    let paths = AppPaths::resolve(&PathEnvironment {
        home: HomeSource::Injected(home.clone()),
        overrides: PathOverrides {
            home: Some(home.join("support")),
            user_store: Some(home.join("user-store")),
            global_store: Some(home.join("global-store")),
            ..Default::default()
        },
        project_root: None,
    })?;
    let sink = Arc::new(RecordingSink::default());
    let bridge = Arc::new(JobBridge::new(sink, Arc::new(TracingOperationsLog)));
    let app = AppState::new(bridge, None).with_paths(paths);
    let view = migrate_scan_inner(&app, None).await?;
    let get = |path: &Path| {
        view.roots
            .iter()
            .find(|root| Path::new(&root.path) == path)
            .expect("candidate is present in command result")
    };
    assert!(view.items.iter().any(|item| item.name == "example"));
    assert_eq!(get(&empty).status, MigrateScanRootStatus::Empty);
    assert_eq!(get(&empty).found_items, 0);
    assert_eq!(get(&missing).status, MigrateScanRootStatus::NotPresent);
    assert_eq!(get(&malformed).status, MigrateScanRootStatus::Unreadable);
    assert_eq!(
        get(&malformed).unreadable_reason.as_deref(),
        Some("invalid MCP configuration")
    );
    assert!(!serde_json::to_string(&view)?.contains("fixture-secret"));
    Ok(())
}

#[tokio::test]
async fn migrate_scan_plan_apply_and_undo_restores_original_tree() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    std::fs::create_dir_all(&fixture.paths().support_dir)?;
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
    while start.elapsed() < Duration::from_secs(30) {
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
    // The migration records fresh baselines; the next scan must not report its own writes.
    {
        let database = app.database().expect("database");
        let targets = at_engine::drift::targets_from_index(database.connection())?;
        let notices = at_engine::drift::DriftService {
            database: &database,
            targets: &targets,
        }
        .reconcile()?;
        assert!(
            notices.is_empty(),
            "migrate_apply must leave no open drift: {notices:?}"
        );
    }

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
    // Undo restores the native originals; its own bookkeeping must not look like drift.
    {
        let database = app.database().expect("database");
        let targets = at_engine::drift::targets_from_index(database.connection())?;
        let notices = at_engine::drift::DriftService {
            database: &database,
            targets: &targets,
        }
        .reconcile()?;
        assert!(
            notices.is_empty(),
            "operation_undo must leave no open drift: {notices:?}"
        );
    }

    tokio::task::spawn_blocking(move || {
        bridge.shutdown(Duration::from_secs(1));
        drop(app);
    })
    .await?;
    Ok(())
}

/// App wiring shared by the scan-job tests: state, bridge and the recording event sink.
type ScanApp = (AppState, Arc<JobBridge>, Arc<RecordingSink>);

/// A selection that spans native scopes migrates each item into the store of the scope it was
/// discovered in, as one operation that a single Undo reverses.
#[tokio::test]
async fn a_mixed_scope_selection_lands_in_each_store_and_undoes_as_one()
-> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    std::fs::create_dir_all(&fixture.paths().support_dir)?;
    let paths = fixture.paths();
    // A project rule the scan reports at Project scope.
    let project_rule = paths.project.join("CLAUDE.md");
    std::fs::write(&project_rule, "# Project Guidelines\n")?;

    let filter = fixture.filter();
    let before = filter.capture()?;

    let app_paths = AppPaths::resolve(&PathEnvironment {
        home: HomeSource::Injected(paths.home.clone()),
        overrides: PathOverrides {
            home: Some(paths.support_dir.clone()),
            user_store: Some(paths.user_store.clone()),
            project_store: Some(paths.project_store.clone()),
            ..Default::default()
        },
        project_root: Some(paths.project.clone()),
    })?;
    let database = at_db::Database::open_file(&paths.db_path)?;
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
    let project =
        at_db::projects::add_project(database.connection(), &paths.project, Some("acme"), None)?;
    let project_id = project.id;
    let sink = Arc::new(RecordingSink::default());
    let bridge = Arc::new(JobBridge::new(sink.clone(), Arc::new(TracingOperationsLog)));
    let app = AppState::new(bridge.clone(), None)
        .with_database(database)
        .with_paths(app_paths);

    let scan = migrate_scan_inner(&app, Some(project_id.clone())).await?;
    let user_skill = paths.home.join(".claude/skills").join(NATIVE_SKILL);
    let project_skill = paths
        .project_claude_skill
        .parent()
        .ok_or("the project skill folder")?
        .to_path_buf();
    let user_mcp = paths.home.join(".claude.json");
    let expected_scopes = [
        (&user_skill, "user"),
        (&user_mcp, "user"),
        (&project_skill, "project"),
        (&project_rule, "project"),
    ];
    let mut selected_paths = Vec::new();
    for (path, scope) in &expected_scopes {
        let item = scan
            .items
            .iter()
            .find(|item| Path::new(&item.path) == *path)
            .ok_or_else(|| format!("{} must be discovered by the scan", path.display()))?;
        assert_eq!(
            item.scope,
            *scope,
            "{} is discovered at {scope} scope",
            path.display()
        );
        selected_paths.push(item.path.clone());
    }

    let conflicts =
        migrate_conflicts_inner(&app, Some(project_id.clone()), selected_paths.clone()).await?;
    let resolutions = conflicts
        .conflicts
        .iter()
        .map(|conflict| MigrateResolutionView {
            name: conflict.name.clone(),
            choice: "keep_a".to_owned(),
            names: Vec::new(),
        })
        .collect::<Vec<_>>();

    let plan = migrate_plan_inner(
        &app,
        MigratePlanRequest {
            project_id: Some(project_id.clone()),
            selected_paths: selected_paths.clone(),
            resolutions: resolutions.clone(),
            strategy: "replace_with_links".to_owned(),
        },
    )
    .await?;
    let user_plan = plan
        .scopes
        .iter()
        .find(|entry| entry.scope == "user")
        .ok_or("the plan must name the User store")?;
    let project_plan = plan
        .scopes
        .iter()
        .find(|entry| entry.scope == "project")
        .ok_or("the plan must name the Project store")?;
    assert_eq!(user_plan.store_path, paths.user_store.display().to_string());
    assert_eq!(
        project_plan.store_path,
        paths.project_store.display().to_string()
    );
    assert_eq!(
        user_plan.counts.items, 2,
        "the User store receives the user skill and MCP server"
    );
    assert_eq!(
        project_plan.counts.items, 2,
        "the Project store receives the project skill and rule"
    );

    let job_id = migrate_apply_inner(
        &app,
        MigrateApplyRequest {
            project_id: Some(project_id),
            selected_paths,
            resolutions,
            strategy: "replace_with_links".to_owned(),
            plan_hash: plan.plan_hash.clone(),
        },
    )
    .await?;
    let result = wait_for_apply_result(&app, job_id.as_str()).await?;
    assert_eq!(
        result.scopes.len(),
        2,
        "one result entry per receiving store"
    );

    let user_store_skill = paths.user_store.join("skills").join(NATIVE_SKILL);
    let project_store_skill = paths.project_store.join("skills").join(NATIVE_SKILL);
    assert!(
        user_store_skill.join("SKILL.md").is_file(),
        "the user skill landed in the User store"
    );
    assert!(
        project_store_skill.join("SKILL.md").is_file(),
        "the project skill landed in the Project store"
    );
    assert!(
        paths.user_store.join("mcp.json").is_file(),
        "the MCP server landed in the User store"
    );
    assert!(
        paths
            .project_store
            .join("rules")
            .join("claude.md")
            .is_file(),
        "the project rule landed in the Project store"
    );
    assert!(
        user_skill.is_symlink(),
        "the user native skill is replaced by a link at {}",
        user_skill.display()
    );
    assert!(
        project_skill.is_symlink(),
        "the project native skill is replaced by a link at {}",
        project_skill.display()
    );
    // Links into the store are relative, so the link itself is resolved instead of its target.
    assert_eq!(
        std::fs::canonicalize(&user_skill)?,
        std::fs::canonicalize(&user_store_skill)?,
        "the user native skill links into the User store"
    );
    assert_eq!(
        std::fs::canonicalize(&project_skill)?,
        std::fs::canonicalize(&project_store_skill)?,
        "the project native skill links into the Project store"
    );

    // The migration records fresh baselines; the next scan must not report its own writes.
    {
        let database = app.database().expect("database");
        let targets = at_engine::drift::targets_from_index(database.connection())?;
        let notices = at_engine::drift::DriftService {
            database: &database,
            targets: &targets,
        }
        .reconcile()?;
        assert!(
            notices.is_empty(),
            "a mixed-scope migrate_apply must leave no open drift: {notices:?}"
        );
    }

    let readiness = operation_undo_check_inner(&app, &result.operation_id)?;
    assert!(readiness.known);
    assert!(readiness.conflicted_paths.is_empty());

    operation_undo_inner(&app, &result.operation_id, false)?;
    let after_undo = filter.capture()?;
    let mut bookkeeping = filter.bookkeeping().to_vec();
    bookkeeping.push(paths.support_key.clone());
    bookkeeping.push("home/Library".to_owned());
    after_undo
        .diff(&before)
        .excluding(&bookkeeping)
        .ensure_empty("one Undo must restore both stores and every native original")?;
    {
        let database = app.database().expect("database");
        let targets = at_engine::drift::targets_from_index(database.connection())?;
        let notices = at_engine::drift::DriftService {
            database: &database,
            targets: &targets,
        }
        .reconcile()?;
        assert!(
            notices.is_empty(),
            "undoing a mixed-scope migration must leave no open drift: {notices:?}"
        );
    }

    tokio::task::spawn_blocking(move || {
        bridge.shutdown(Duration::from_secs(1));
        drop(app);
    })
    .await?;
    Ok(())
}

/// Polls the apply-result command until the job publishes its reviewed outcome.
async fn wait_for_apply_result(
    app: &AppState,
    job_id: &str,
) -> Result<at_app::app::migrate::MigrateApplyResultView, Box<dyn Error>> {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        match migrate_apply_result_inner(app, job_id) {
            Ok(result) => return Ok(result),
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    Err(format!("migration apply job {job_id} published no result").into())
}

/// Builds the app over one fixture, shared by the scan-job tests.
fn app_for(fixture: &MigrationFixture) -> Result<ScanApp, Box<dyn Error>> {
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
    Ok((app, bridge, sink))
}

/// Waits for the single finished event of one job and returns its terminal state.
async fn finished_state(sink: &RecordingSink, job_id: &JobId) -> Option<JobState> {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        let found = sink
            .events
            .lock()
            .unwrap()
            .iter()
            .find_map(|event| match event {
                JobEvent::Finished(finished) if &finished.id == job_id => Some(finished.state),
                _ => None,
            });
        if found.is_some() {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

#[tokio::test]
async fn the_scan_job_streams_progress_and_records_the_view() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let filter = fixture.filter();
    let (app, bridge, sink) = app_for(&fixture)?;
    let before = filter.capture()?;

    let job_id = migrate_scan_start_inner(&app, None).await?;
    let mut view = None;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        match migrate_scan_result_inner(&app, job_id.as_str()) {
            Ok(Some(found)) => {
                view = Some(found);
                break;
            }
            Ok(None) | Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    let view = view.ok_or("the scan job records a view")?;
    assert!(!view.items.is_empty(), "the fixture has native items");
    assert!(
        view.items.iter().all(|item| matches!(
            item.state.as_str(),
            "in_agentuna" | "conflict" | "not_in_agentuna"
        )),
        "every row carries one authoritative ownership label"
    );
    assert!(
        view.items
            .iter()
            .any(|item| item.state == "not_in_agentuna"),
        "the fixture has unmanaged native copies"
    );

    assert_eq!(
        finished_state(&sink, &job_id).await,
        Some(JobState::Succeeded)
    );
    let events = sink.events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, JobEvent::Started(started) if started.id == job_id)),
        "the bridge announces the scan job"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, JobEvent::Finished(finished) if finished.id == job_id))
            .count(),
        1,
        "the bridge emits exactly one finished event per job"
    );

    let after = filter.capture()?;
    let mut bookkeeping = filter.bookkeeping().to_vec();
    bookkeeping.push(fixture.paths().support_key.clone());
    bookkeeping.push("home/Library".to_owned());
    after
        .diff(&before)
        .excluding(&bookkeeping)
        .ensure_empty("a scan writes nothing")?;
    tokio::task::spawn_blocking(move || {
        bridge.shutdown(Duration::from_secs(1));
        drop(app);
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_cancelled_scan_reaches_cancelled_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let filter = fixture.filter();
    let (app, bridge, sink) = app_for(&fixture)?;
    let before = filter.capture()?;

    let job_id = migrate_scan_start_inner(&app, None).await?;
    let cancelled = app.bridge().cancel_job(&job_id);
    let state = finished_state(&sink, &job_id)
        .await
        .ok_or("the job reaches a terminal state")?;
    if cancelled {
        assert_eq!(state, JobState::Cancelled);
        // A cancelled job never publishes a view, even once its worker stops.
        for _ in 0..20 {
            assert!(
                migrate_scan_result_inner(&app, job_id.as_str()).is_err(),
                "a cancelled scan must not publish a view"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    } else {
        assert_eq!(
            state,
            JobState::Succeeded,
            "the scan finished before the cancel landed"
        );
        assert!(
            migrate_scan_result_inner(&app, job_id.as_str())?.is_some(),
            "a completed scan publishes its view"
        );
    }
    let after = filter.capture()?;
    let mut bookkeeping = filter.bookkeeping().to_vec();
    bookkeeping.push(fixture.paths().support_key.clone());
    bookkeeping.push("home/Library".to_owned());
    after
        .diff(&before)
        .excluding(&bookkeeping)
        .ensure_empty("a cancelled scan writes nothing")?;
    tokio::task::spawn_blocking(move || {
        bridge.shutdown(Duration::from_secs(1));
        drop(app);
    })
    .await?;
    Ok(())
}
