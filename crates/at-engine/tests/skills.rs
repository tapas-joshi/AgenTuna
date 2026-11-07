//! Skills index composition tests using an in-memory DB and injected agent folders.

use std::collections::BTreeMap;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::Database;
use at_db::skills_index::SkillFilter;
use at_engine::skills::{SkillBadge, SkillCellState, index};
use at_model::{AgentId, ItemId, Scope};
use rusqlite::params;

fn add_skill(db: &Database, scope: Scope, slug: &str, name: &str) -> ItemId {
    let id = ItemId::new();
    db.connection()
        .execute(
            "INSERT INTO items (id,kind,scope,slug,name,state,managed) VALUES (?1,'skill',?2,?3,?4,'active',1)",
            params![id.to_string(), scope.wire_name(), slug, name],
        )
        .expect("insert canonical skill");
    id
}

#[test]
fn index_merges_canonical_rows_with_live_native_skills_and_counts() {
    let db = Database::open_in_memory().expect("database");
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('claude_code','Claude Code',1)",
            [],
        )
        .expect("insert agent");
    let locked = add_skill(&db, Scope::Global, "shared", "Shared");
    add_skill(&db, Scope::Project, "shared", "Shared");
    add_skill(&db, Scope::User, "other", "Other");
    add_skill(&db, Scope::User, "third", "Third");
    at_db::locks::upsert_lock(
        db.connection(),
        &at_db::locks::LockSpec {
            item_id: &locked,
            scope: Scope::Global,
            reason: Some("organization policy"),
            locked_by: Some("admin"),
        },
    )
    .expect("insert lock");

    let temp = tempfile::tempdir().expect("temporary root");
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    for slug in ["native-one", "native-two"] {
        let folder = home.join(".claude/skills").join(slug);
        std::fs::create_dir_all(&folder).expect("native skill folder");
        std::fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {slug}\ndescription: fixture\n---\n"),
        )
        .expect("native skill file");
    }
    let env = AgentEnvironment {
        home,
        project: Some(project),
        variables: BTreeMap::new(),
    };
    let registry = AgentAdapterRegistry::register();
    let result = index(
        db.connection(),
        &SkillFilter::default(),
        &registry,
        &env,
        &[],
    )
    .expect("compose skills index");

    assert_eq!(result.counts.all, 6);
    assert_eq!(result.counts.not_in_agentuna, 2);
    assert_eq!(result.counts.shadowed, 1);
    let native = result
        .rows
        .iter()
        .find(|row| row.skill.name == "native-one")
        .expect("native row");
    assert!(matches!(
        native.agents.get(&AgentId::ClaudeCode),
        Some(SkillCellState::Native { path }) if path.ends_with("native-one")
    ));
    let locked_row = result
        .rows
        .iter()
        .find(|row| row.skill.item_id == Some(locked))
        .expect("locked canonical row");
    assert!(matches!(
        locked_row.agents.get(&AgentId::ClaudeCode),
        Some(SkillCellState::Locked { reason }) if reason == "organization policy"
    ));
}

#[derive(Clone)]
struct CountingAdapter {
    id: AgentId,
    path_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl at_adapters::adapter::AgentAdapter for CountingAdapter {
    fn id(&self) -> AgentId {
        self.id.clone()
    }

    fn capabilities(&self, _env: &AgentEnvironment) -> at_adapters::adapter::AgentCapabilities {
        at_adapters::adapter::AgentCapabilities {
            mcp: None,
            supports_symlink: true,
            supports_rules: false,
        }
    }

    fn detect(&self, _env: &AgentEnvironment) -> at_adapters::adapter::AgentDetection {
        at_adapters::adapter::AgentDetection {
            state: at_adapters::adapter::DetectionState::ConfiguredOnly,
            candidates: Vec::new(),
        }
    }

    fn paths(
        &self,
        kind: at_model::ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<at_adapters::adapter::AgentPathCandidate> {
        self.path_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if kind != at_model::ItemKind::Skill {
            return Vec::new();
        }
        let path = match scope {
            Scope::Global | Scope::User => env.home.join(".counter/skills"),
            Scope::Project | Scope::Local => env
                .project
                .as_ref()
                .map(|project| project.join(".counter/skills"))
                .unwrap_or_else(|| env.home.join(".counter/skills")),
        };
        vec![at_adapters::adapter::AgentPathCandidate {
            kind: String::from("skills_dir"),
            path,
            scope,
            format: None,
            primary: true,
        }]
    }
}

#[test]
fn native_path_discovery_is_independent_of_canonical_row_count() {
    let db = Database::open_in_memory().expect("database");
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('custom:counter','Counter',1)",
            [],
        )
        .expect("insert agent");
    for index in 0..4 {
        add_skill(
            &db,
            Scope::User,
            &format!("skill-{index}"),
            &format!("Skill {index}"),
        );
    }
    let path_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = CountingAdapter {
        id: AgentId::custom("counter").expect("custom id"),
        path_calls: path_calls.clone(),
    };
    let registry = AgentAdapterRegistry::register();
    registry
        .insert(std::sync::Arc::new(adapter))
        .expect("register adapter");
    let temp = tempfile::tempdir().expect("temporary root");
    let env = AgentEnvironment {
        home: temp.path().join("home"),
        project: Some(temp.path().join("project")),
        variables: BTreeMap::new(),
    };
    let result = index(
        db.connection(),
        &SkillFilter::default(),
        &registry,
        &env,
        &[],
    )
    .expect("compose index");
    assert_eq!(result.rows.len(), 4);
    assert_eq!(path_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}
#[test]
fn shadowed_badge_wins_over_available_update() {
    let item = at_db::skills_index::SkillRow {
        item_id: Some(ItemId::new()),
        name: String::from("review"),
        scope: Scope::User,
        state: Some(String::from("active")),
        shadowed_by: Some(Scope::Project),
        deployed: BTreeMap::new(),
        native_path: None,
        native_paths: BTreeMap::new(),
        update_available: true,
        updated_at: String::new(),
        description: None,
    };
    let row = at_engine::skills::SkillIndexRow {
        skill: item,
        agents: BTreeMap::new(),
    };
    assert_eq!(row.badge(), SkillBadge::Shadowed);
}

/// An adapter whose skills root disappeared after installation: listing it now fails with
/// `NotInstalled` instead of returning an empty set.
struct DisappearedAdapter {
    id: AgentId,
    missing: std::path::PathBuf,
}

impl at_adapters::adapter::AgentAdapter for DisappearedAdapter {
    fn id(&self) -> AgentId {
        self.id.clone()
    }

    fn capabilities(&self, _env: &AgentEnvironment) -> at_adapters::adapter::AgentCapabilities {
        at_adapters::adapter::AgentCapabilities {
            mcp: None,
            supports_symlink: true,
            supports_rules: false,
        }
    }

    fn detect(&self, _env: &AgentEnvironment) -> at_adapters::adapter::AgentDetection {
        at_adapters::adapter::AgentDetection {
            state: at_adapters::adapter::DetectionState::ConfiguredOnly,
            candidates: Vec::new(),
        }
    }

    fn paths(
        &self,
        _kind: at_model::ItemKind,
        _scope: Scope,
        _env: &AgentEnvironment,
    ) -> Vec<at_adapters::adapter::AgentPathCandidate> {
        Vec::new()
    }

    fn list_native_skills(
        &self,
        _env: &AgentEnvironment,
        _store_roots: &[std::path::PathBuf],
    ) -> Result<Vec<at_adapters::native_skills::NativeSkill>, at_adapters::adapter::AgentError>
    {
        Err(at_adapters::adapter::AgentError::NotInstalled {
            agent: self.id.clone(),
            path: self.missing.clone(),
        })
    }
}

#[test]
fn a_disappeared_source_folder_yields_an_error_cell_with_remediation() {
    let db = Database::open_in_memory().expect("database");
    let temp = tempfile::tempdir().expect("temporary root");
    let missing = temp.path().join("gone/skills");
    let agent = AgentId::custom("gone-tool").expect("custom id");
    let registry = AgentAdapterRegistry::register();
    registry
        .insert(std::sync::Arc::new(DisappearedAdapter {
            id: agent.clone(),
            missing: missing.clone(),
        }))
        .expect("register adapter");
    let env = AgentEnvironment {
        home: temp.path().join("home"),
        project: Some(temp.path().join("project")),
        variables: BTreeMap::new(),
    };
    let result = index(
        db.connection(),
        &SkillFilter::default(),
        &registry,
        &env,
        &[],
    )
    .expect("a failed source is a row, not a panic");
    let row = result
        .rows
        .iter()
        .find(|row| row.skill.state.as_deref() == Some("error"))
        .expect("the disappeared source produces an error row");
    assert_eq!(
        row.skill.native_path.as_deref(),
        Some(missing.to_string_lossy().as_ref())
    );
    match row.agents.get(&agent) {
        Some(SkillCellState::Error {
            message,
            remediation,
        }) => {
            assert!(
                message.contains(&missing.display().to_string()),
                "the error names the missing path: {message}"
            );
            assert!(!remediation.is_empty(), "the error carries a remediation");
        }
        other => panic!("expected an Error cell, got {other:?}"),
    }
}
