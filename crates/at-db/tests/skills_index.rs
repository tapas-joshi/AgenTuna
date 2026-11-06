//! Behaviour tests for the skills index projection.

mod common;

use at_db::Database;
use at_db::skills_index::{SkillFilter, list_skills, search_skills};
use at_model::{AgentId, ItemId, Scope};
use rusqlite::params;

fn add_skill(db: &Database, scope: Scope, slug: &str, name: &str) -> ItemId {
    let id = ItemId::new();
    db.connection().execute(
        "INSERT INTO items (id,kind,scope,slug,name,state,managed) VALUES (?1,'skill',?2,?3,?4,'active',1)",
        params![id.to_string(), scope.wire_name(), slug, name],
    ).expect("insert test skill");
    id
}

#[test]
fn skills_projection_reports_agent_state_shadowing_and_filter_results() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection().execute("INSERT INTO agents(id,display_name,installed) VALUES ('codex','Codex',1),('cursor','Cursor',0)", []).expect("insert agents");
    let low = add_skill(&db, Scope::Global, "code-review", "Code Review");
    add_skill(&db, Scope::User, "code-review", "Code Review");
    db.connection().execute(
        "INSERT INTO deployments(item_id,agent_id,scope,state,target_path) VALUES (?1,'codex','global','linked','/sandbox/codex/skills/code-review')",
        [low.to_string()],
    ).expect("insert deployment");
    let rows = list_skills(
        db.connection(),
        &SkillFilter {
            shadowed_only: true,
            ..SkillFilter::default()
        },
    )
    .expect("query skills");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item_id, Some(low));
    assert_eq!(rows[0].shadowed_by, Some(Scope::User));
    assert_eq!(
        rows[0].deployed.get(&AgentId::Codex),
        Some(&at_model::DeploymentState::Linked)
    );
    assert_eq!(
        rows[0].deployed.get(&AgentId::Cursor),
        Some(&at_model::DeploymentState::NotInstalled)
    );
    assert_eq!(
        search_skills(db.connection(), "rev").expect("search")[0].name,
        "Code Review"
    );
}

#[test]
fn not_in_agentuna_returns_only_unadopted_native_skills() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('omp','omp',1)",
            [],
        )
        .expect("insert agent");
    db.connection().execute(
        "INSERT INTO native_items(id,agent_id,scope,kind,name,path,status) VALUES ('native-a','omp','user','skill','other-skill','/sandbox/skills/other-skill','new'),('native-b','omp','user','skill','adopted-skill','/sandbox/skills/adopted-skill','adopted')",
        [],
    ).expect("insert native rows");
    let rows = list_skills(
        db.connection(),
        &SkillFilter {
            not_in_agentuna: true,
            ..SkillFilter::default()
        },
    )
    .expect("query native skills");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "other-skill");
    assert_eq!(
        rows[0].native_path.as_deref(),
        Some("/sandbox/skills/other-skill")
    );
    assert!(rows[0].item_id.is_none());
}

#[test]
fn search_ranks_code_review_before_review_standards_case_insensitively() {
    let db = common::open_with(common::ALL_VERSIONS);
    add_skill(&db, Scope::User, "review-standards", "Review Standards");
    add_skill(&db, Scope::User, "code-review", "Code Review");
    let rows = search_skills(db.connection(), "REV").expect("search skills");
    assert_eq!(rows[0].name, "Code Review");
    assert_eq!(rows[1].name, "Review Standards");
}
