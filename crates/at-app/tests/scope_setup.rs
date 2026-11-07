//! TA-050: the Overview "Set up this scope" action creates the scope store through the
//! reviewed at-fs ChangeSet pipeline and stays undoable through `operation_undo`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use at_app::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
use at_app::app::overview::{
    overview_load_inner, overview_setup_apply_inner, overview_setup_preview_inner,
};
use at_app::app::overwrite::operation_undo_inner;
use at_app::app::state::AppState;
use at_db::Database;
use at_model::Scope;
use at_store::layout::{STORE_DIR_MODE, StoreLayout};
use at_store::paths::{AppPaths, HomeSource, PathEnvironment, PathOverrides};
use tempfile::TempDir;

#[derive(Default)]
struct Sink;

impl EventSink for Sink {
    fn emit(&self, _event: JobEvent) {}
}

struct Sandbox {
    _dir: TempDir,
    home: PathBuf,
    support: PathBuf,
    user_store: PathBuf,
    global: PathBuf,
    project: PathBuf,
}

impl Sandbox {
    fn new(create_global: bool) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let home = dir.path().join("home");
        let support = dir.path().join("support");
        let user_store = home.join(".agentuna");
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        // The user store stays absent so the test proves loading does not create it.
        for directory in [&home, &support, &project] {
            fs::create_dir_all(directory).expect("sandbox directory");
        }
        if create_global {
            fs::create_dir_all(&global).expect("global directory");
        }
        Self {
            _dir: dir,
            home,
            support,
            user_store,
            global,
            project,
        }
    }

    fn paths(&self) -> AppPaths {
        self.paths_with_global(&self.global)
    }

    fn paths_with_global(&self, global: &Path) -> AppPaths {
        AppPaths::resolve(&PathEnvironment {
            home: HomeSource::Injected(self.home.clone()),
            overrides: PathOverrides {
                home: Some(self.support.clone()),
                user_store: Some(self.user_store.clone()),
                project_store: None,
                global_store: Some(global.to_path_buf()),
            },
            project_root: Some(self.project.clone()),
        })
        .expect("paths resolve")
    }

    fn database(&self) -> Database {
        Database::open_file(self.support.join("agentuna.db")).expect("database")
    }

    fn state(&self) -> AppState {
        self.state_with_paths(self.paths())
    }

    fn state_with_paths(&self, paths: AppPaths) -> AppState {
        let bridge = Arc::new(JobBridge::new(
            Arc::new(Sink),
            Arc::new(TracingOperationsLog),
        ));
        AppState::new(bridge, None)
            .with_database(self.database())
            .with_paths(paths)
    }

    fn register_active_project(&self, database: &mut Database) -> String {
        let project = at_db::projects::add_project(
            database.connection(),
            &self.project,
            Some("Project"),
            None,
        )
        .expect("register project");
        at_db::settings::set_last_project_id(
            database.connection_mut(),
            Some(&project.id),
            at_db::ops::OperationActor::Ui,
        )
        .expect("select project");
        project.id
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn user_store_setup_is_previewed_applied_and_undone() {
    let sandbox = Sandbox::new(true);
    let state = sandbox.state();
    let layout = StoreLayout::new(Scope::User, sandbox.paths().user_store());

    let before = overview_load_inner(&state, "user").expect("overview before setup");
    assert!(
        !before
            .scopes
            .iter()
            .find(|card| card.scope == "user")
            .expect("user card")
            .store_ready,
        "an absent store is not ready"
    );
    assert!(!layout.root().exists(), "loading must not create the store");

    let preview = overview_setup_preview_inner(&state, "user", None).expect("preview");
    assert!(!preview.already_ready);
    assert!(
        preview.files.iter().any(|file| file.path == "store.json"),
        "the manifest is in the reviewed plan"
    );
    assert!(
        preview.files.iter().any(|file| file.path == "skills"),
        "the skills folder is in the reviewed plan"
    );
    assert!(!layout.root().exists(), "preview must not write");
    let preview_id = preview.preview_id.expect("preview identity");

    let applied = overview_setup_apply_inner(&state, &preview_id, true).expect("apply");
    assert_eq!(applied.files_changed, preview.files.len());
    assert!(layout.is_ready());
    let header = layout.read_header().expect("store manifest");
    assert_eq!(header.scope, Scope::User);
    for directory in [
        layout.skills_dir(),
        layout.rules_dir(),
        layout.backups_dir(),
        layout.journals_dir(),
        layout.staging_dir(),
        layout.tmp_dir(),
    ] {
        assert!(directory.is_dir(), "{} must exist", directory.display());
    }
    assert_eq!(mode(layout.root()), STORE_DIR_MODE);
    assert_eq!(mode(&layout.state_dir()), STORE_DIR_MODE);

    let after = overview_load_inner(&state, "user").expect("overview after setup");
    assert!(
        after
            .scopes
            .iter()
            .find(|card| card.scope == "user")
            .expect("user card")
            .store_ready,
        "the created store is reported ready"
    );

    let undone = operation_undo_inner(&state, &applied.operation_id, false).expect("undo setup");
    assert!(undone.summary.contains("was undone"));
    assert!(!layout.is_ready(), "undo removes store.json");
    assert!(!layout.mcp_file().exists());
    assert!(!layout.skills_dir().exists());
    assert!(!layout.rules_dir().exists());
    let reopened = overview_load_inner(&state, "user").expect("overview after undo");
    assert!(
        !reopened
            .scopes
            .iter()
            .find(|card| card.scope == "user")
            .expect("user card")
            .store_ready
    );
}

#[test]
fn project_setup_requires_a_registered_existing_root_and_creates_local_scope() {
    let sandbox = Sandbox::new(true);
    let mut database = sandbox.database();
    let project_id = sandbox.register_active_project(&mut database);
    drop(database);
    let state = sandbox.state();

    let missing_project = overview_setup_preview_inner(&state, "project", None);
    assert!(missing_project.is_err(), "project id is required");

    let preview =
        overview_setup_preview_inner(&state, "project", Some(&project_id)).expect("preview");
    let expected_store = fs::canonicalize(&sandbox.project)
        .expect("canonical project")
        .join(".agentuna");
    assert_eq!(PathBuf::from(&preview.path), expected_store);
    assert!(!preview.already_ready);
    let applied =
        overview_setup_apply_inner(&state, &preview.preview_id.expect("preview identity"), true)
            .expect("apply");
    assert!(applied.files_changed > 0);

    let layout = StoreLayout::new(Scope::Project, sandbox.project.join(".agentuna"));
    assert!(layout.is_ready());
    assert!(
        layout.local_dir().expect("project local dir").is_dir(),
        "the project store owns local/"
    );
    assert!(
        overview_load_inner(&state, "project")
            .expect("overview")
            .scopes
            .iter()
            .find(|card| card.scope == "project")
            .expect("project card")
            .store_ready
    );
}

#[test]
fn project_setup_refuses_a_registered_folder_that_no_longer_exists() {
    let sandbox = Sandbox::new(true);
    let mut database = sandbox.database();
    let project_id = sandbox.register_active_project(&mut database);
    drop(database);
    let state = sandbox.state();
    fs::remove_dir_all(&sandbox.project).expect("remove project folder");

    let error = overview_setup_preview_inner(&state, "project", Some(&project_id))
        .expect_err("missing project folder must be refused");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::InvalidArgument
    );
}

#[test]
fn global_setup_uses_the_existing_consent_and_records_a_global_write() {
    let sandbox = Sandbox::new(true);
    let state = sandbox.state();
    let preview = overview_setup_preview_inner(&state, "global", None).expect("preview");
    let applied =
        overview_setup_apply_inner(&state, &preview.preview_id.expect("preview identity"), true)
            .expect("apply");
    let layout = StoreLayout::new(Scope::Global, sandbox.global.clone());
    assert!(layout.is_ready());
    let database = state.database().expect("database");
    let kind: String = database
        .connection()
        .query_row(
            "SELECT kind FROM operations WHERE id = ?1",
            [&applied.operation_id],
            |row| row.get(0),
        )
        .expect("operation row");
    assert_eq!(kind, "global_write");
}

#[test]
fn global_setup_is_refused_without_prior_write_consent() {
    let sandbox = Sandbox::new(false);
    let paths = sandbox.paths_with_global(&sandbox.global);
    let state = sandbox.state_with_paths(paths);

    let error = overview_setup_preview_inner(&state, "global", None)
        .expect_err("an absent Global store has no consent yet");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::Conflict
    );
    assert!(!sandbox.global.exists(), "the refusal writes nothing");
}
