//! TF-020 shell integration: a quarantined index is rebuilt and reported, never ignored.
//!
//! Every filesystem root is a temp directory. The store is seeded through the canonical manifest
//! APIs so the rebuild scans realistic state.

use std::fs;
use std::path::{Path, PathBuf};

use at_app::app::startup::{self, OpenedDatabase, StartupWarningView};
use at_db::rebuild::rebuild;
use at_model::{ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::paths::{AppPaths, HomeSource, PathEnvironment, PathOverrides};
use tempfile::TempDir;

const SKILL_DOC: &str = "---\nname: review\ndescription: Code review skill\n---\n\n# Review\n";

struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: TempDir::new().expect("temp dir"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn paths(&self) -> AppPaths {
        let support = self.path("support");
        let user_store = self.path("user-store");
        let global = self.path("global");
        let home = self.path("home");
        for directory in [&support, &user_store, &global, &home] {
            fs::create_dir_all(directory).expect("sandbox directory");
        }
        AppPaths::resolve(&PathEnvironment {
            home: HomeSource::Injected(home),
            overrides: PathOverrides {
                home: Some(support),
                user_store: Some(user_store),
                project_store: None,
                global_store: Some(global),
            },
            project_root: None,
        })
        .expect("paths resolve")
    }

    /// Creates the User store with one skill that a rebuild must find again.
    fn seed_skill(&self, paths: &AppPaths) {
        let store = StoreLayout::new(Scope::User, paths.user_store());
        init_store(Scope::User, store.root()).expect("init store");
        let slug = Slug::parse("review").expect("slug");
        let dir = store.skill_dir(&slug);
        fs::create_dir_all(&dir).expect("skill dir");
        fs::write(dir.join("SKILL.md"), SKILL_DOC).expect("skill doc");
        let rebuilt = at_store::manifest_rebuild::plan_manifest_rebuild(&store)
            .expect("plan manifest rebuild");
        let token = rebuilt.token().clone();
        at_store::manifest_rebuild::apply_manifest_rebuild(&store, &rebuilt, &token)
            .expect("write manifest");
    }
}

fn rebuild_now(paths: &AppPaths) {
    let mut database = at_db::Database::open_file(paths.db_path()).expect("database");
    rebuild(
        database.connection_mut(),
        &startup::rebuild_paths(paths),
        None,
    )
    .expect("rebuild");
}

fn item_count(opened: &OpenedDatabase) -> i64 {
    let database = opened.database.as_ref().expect("database handle");
    let counts = at_db::overview::counts_for_scope(database.connection()).expect("counts");
    counts
        .into_iter()
        .map(|(_, counts)| counts.skills)
        .sum::<i64>()
}

#[test]
fn a_healthy_index_starts_with_no_warning_and_no_rebuild() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    sandbox.seed_skill(&paths);
    rebuild_now(&paths);

    let opened = startup::open_database(&paths);
    assert!(opened.warnings.is_empty(), "{:?}", opened.warnings);
    assert!(opened.rebuild.is_none(), "a healthy index is not rebuilt");
    assert_eq!(item_count(&opened), 1);
}

#[test]
fn a_corrupt_index_is_quarantined_rebuilt_and_reported() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    sandbox.seed_skill(&paths);
    rebuild_now(&paths);

    let corrupt = b"this is not a sqlite database";
    fs::write(paths.db_path(), corrupt).expect("corrupt the index");

    let opened = startup::open_database(&paths);
    assert!(opened.database.is_some(), "the app starts on a fresh index");
    assert_eq!(opened.warnings.len(), 1, "{:?}", opened.warnings);
    let warning = &opened.warnings[0];
    assert_eq!(warning.kind, "database_quarantined");
    let quarantine = PathBuf::from(warning.path.as_deref().expect("quarantine path"));
    assert!(
        quarantine
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("agentuna.db.corrupt-")),
        "quarantine name is timestamped: {}",
        quarantine.display()
    );
    assert_eq!(fs::read(&quarantine).expect("quarantine bytes"), corrupt);
    assert!(
        warning.message.contains(&quarantine.display().to_string()),
        "the warning names the quarantine file: {}",
        warning.message
    );

    let report = opened.rebuild.as_ref().expect("derived data was rebuilt");
    assert_eq!(report.counts.items, 1, "the seeded skill is indexed again");
    assert_eq!(
        report.counts.agent_paths, 0,
        "no agent folders in the sandbox"
    );
    assert_eq!(item_count(&opened), 1);

    // The fresh index is usable and the quarantine file is left alone.
    let database = opened.database.as_ref().expect("database");
    assert_eq!(database.schema_version().expect("schema"), 9);
    assert!(quarantine.exists());
}

#[test]
fn an_unreadable_index_starts_without_one_and_leaves_the_file_untouched() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    sandbox.seed_skill(&paths);
    // A directory where the database file belongs cannot be opened as SQLite.
    fs::create_dir_all(paths.db_path()).expect("blocking directory");
    let before = fs::read_dir(paths.db_path()).expect("directory").count();

    let opened = startup::open_database(&paths);
    assert!(
        opened.database.is_none(),
        "no handle on an unreadable index"
    );
    assert!(opened.rebuild.is_none());
    assert_eq!(opened.warnings.len(), 1, "{:?}", opened.warnings);
    let warning = &opened.warnings[0];
    assert_eq!(warning.kind, "index_unavailable");
    assert_eq!(
        warning.path.as_deref(),
        Some(paths.db_path().display().to_string().as_str())
    );
    assert!(
        warning.message.contains("could not be opened"),
        "{}",
        warning.message
    );
    // Nothing was replaced and nothing was quarantined.
    assert!(paths.db_path().is_dir());
    assert_eq!(
        fs::read_dir(paths.db_path()).expect("directory").count(),
        before
    );
    let quarantined: Vec<PathBuf> = fs::read_dir(paths.support_dir())
        .expect("support dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().contains(".corrupt-"))
        .collect();
    assert!(quarantined.is_empty(), "{quarantined:?}");
}

#[test]
fn a_warning_carries_its_kind_message_and_path_for_the_banner() {
    let quarantine = Path::new("/tmp/agentuna.db.corrupt-20261106T101500Z");
    let warning = StartupWarningView::quarantined(quarantine);
    assert_eq!(warning.kind, "database_quarantined");
    assert!(warning.message.contains("rebuilt it from your stores"));
    assert_eq!(
        warning.path.as_deref(),
        Some("/tmp/agentuna.db.corrupt-20261106T101500Z")
    );
    let serialized = serde_json::to_value(&warning).expect("serializes");
    assert_eq!(serialized["kind"], "database_quarantined");
    assert_eq!(
        serialized["path"],
        "/tmp/agentuna.db.corrupt-20261106T101500Z"
    );
}

#[test]
fn rebuild_paths_cover_the_user_and_global_stores_and_the_selected_project() {
    let sandbox = Sandbox::new();
    let project_root = sandbox.path("project");
    fs::create_dir_all(project_root.join(".agentuna")).expect("project store");
    let paths = AppPaths::resolve(&PathEnvironment {
        home: HomeSource::Injected(sandbox.path("home")),
        overrides: PathOverrides {
            home: Some(sandbox.path("support")),
            user_store: Some(sandbox.path("user-store")),
            project_store: None,
            global_store: Some(sandbox.path("global")),
        },
        project_root: Some(project_root.clone()),
    })
    .expect("paths resolve");

    let roots = startup::rebuild_paths(&paths);
    assert_eq!(roots.user_store, paths.user_store());
    assert_eq!(roots.global_store.as_deref(), Some(paths.global_store()));
    assert_eq!(
        roots.projects,
        vec![(project_root.clone(), project_root.join(".agentuna"))]
    );
}
#[test]
fn fresh_db_at_startup_rebuilds_derived_data_and_indexes_native_items() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    let native_skill_dir = sandbox.path("home").join(".claude/skills/fixture-skill");
    fs::create_dir_all(&native_skill_dir).expect("native skill dir");
    fs::write(
        native_skill_dir.join("SKILL.md"),
        "---\nname: fixture-skill\ndescription: A native skill\n---\n# Fixture\n",
    )
    .expect("write SKILL.md");

    let opened = startup::open_database(&paths);
    assert!(
        opened.warnings.is_empty(),
        "fresh startup has no quarantine warnings: {:?}",
        opened.warnings
    );
    assert!(
        opened.rebuild.is_some(),
        "fresh startup rebuilds derived data"
    );
    let report = opened.rebuild.as_ref().unwrap();
    assert_eq!(
        report.counts.native_items, 1,
        "native skill was discovered and indexed"
    );

    let db = opened.database.as_ref().expect("database handle");
    let count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM native_items WHERE name = 'fixture-skill' AND kind = 'skill' AND status = 'new'",
            [],
            |row| row.get(0),
        )
        .expect("query native_items");
    assert_eq!(count, 1, "native item row is present in native_items table");
}

#[test]
fn startup_discovery_indexes_native_mcp_servers_and_rule_files() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    let home = sandbox.path("home");
    fs::write(
        home.join(".claude.json"),
        br#"{"mcpServers": {"context7": {"command": "npx"}, "github": {"type": "http", "url": "https://example.invalid/mcp"}}}"#,
    )
    .expect("claude config");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    fs::write(
        home.join(".codex/config.toml"),
        b"[mcp_servers.alpha]\ncommand = \"alpha\"\n",
    )
    .expect("codex config");
    fs::create_dir_all(home.join(".claude")).expect("claude dir");
    fs::write(home.join(".claude/CLAUDE.md"), b"# Rules\n").expect("rule file");

    let opened = startup::open_database(&paths);
    assert!(opened.warnings.is_empty(), "{:?}", opened.warnings);
    let database = opened.database.as_ref().expect("database handle");
    let rows =
        at_db::mcp_index::mcp_list_rows(database.connection(), None, None).expect("mcp rows");
    let mut names: Vec<&str> = rows
        .iter()
        .filter(|row| row.native_only)
        .map(|row| row.display_name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["alpha", "context7", "github"],
        "the MCP screen gets one native row per server: {rows:#?}"
    );
    let counts =
        at_db::mcp_index::mcp_list_counts(database.connection(), None, None).expect("chip counts");
    assert_eq!(
        counts.not_in_agentuna, 3,
        "the banner counts the same rows the table renders"
    );
    let rules = at_db::native::list_native_items(
        database.connection(),
        &at_db::native::NativeItemFilter::default(),
    )
    .expect("native rows")
    .into_iter()
    .filter(|item| item.kind == ItemKind::Rule)
    .count();
    assert_eq!(rules, 1, "the rule file is a finding");
}
