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

#[test]
fn search_matches_description_substrings_case_insensitively() {
    let db = common::open_with(common::ALL_VERSIONS);
    let described = add_skill(&db, Scope::User, "deploy-guide", "Deploy Guide");
    db.connection()
        .execute(
            "UPDATE items SET tags_json = ?1 WHERE id = ?2",
            params![
                r#"{"description":"Code review helpers for pull requests"}"#,
                described.to_string()
            ],
        )
        .expect("store description");
    add_skill(&db, Scope::User, "release-notes", "Release Notes");

    let rows = search_skills(db.connection(), "REV").expect("search skills");
    assert_eq!(rows.len(), 1, "only the described skill matches");
    assert_eq!(rows[0].name, "Deploy Guide");
    assert_eq!(
        rows[0].description.as_deref(),
        Some("Code review helpers for pull requests"),
        "the projection carries the stored description"
    );

    let lowered = list_skills(
        db.connection(),
        &SkillFilter {
            text: Some(String::from("code review")),
            ..SkillFilter::default()
        },
    )
    .expect("filter by description text");
    assert_eq!(lowered.len(), 1);
    assert_eq!(lowered[0].item_id, Some(described));
}

#[test]
fn list_skills_over_500_seeded_skills_returns_within_the_ci_bound() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('codex','Codex',1)",
            [],
        )
        .expect("insert agent");
    let mut statements = Vec::new();
    for index in 0..500 {
        let id = ItemId::new().to_string();
        statements.push((id, format!("skill-{index:03}"), format!("Skill {index:03}")));
    }
    db.connection().execute_batch("BEGIN").expect("begin batch");
    for (id, slug, name) in &statements {
        db.connection()
            .execute(
                "INSERT INTO items (id,kind,scope,slug,name,state,managed,tags_json) VALUES (?1,'skill','user',?2,?3,'active',1,?4)",
                params![id, slug, name, r#"{"description":"Fixture skill for the timed projection"}"#],
            )
            .expect("insert skill");
    }
    db.connection()
        .execute_batch("COMMIT")
        .expect("commit batch");

    let filter = SkillFilter {
        include_native: true,
        ..SkillFilter::default()
    };
    let started = std::time::Instant::now();
    let rows = list_skills(db.connection(), &filter).expect("list 500 skills");
    let elapsed = started.elapsed();
    assert_eq!(rows.len(), 500, "one row per seeded skill");
    assert!(
        rows.iter().all(|row| row.description.is_some()),
        "every seeded row carries its stored description"
    );
    // Generous CI bound: the dev Mac measures well under 50 ms; the assertion
    // only fails when the projection degrades catastrophically.
    assert!(
        elapsed < std::time::Duration::from_millis(2_000),
        "list_skills over 500 skills took {elapsed:?}"
    );
}

#[test]
fn ignored_upstream_commit_hides_the_update_badge_until_a_newer_one_is_checked() {
    let mut db = common::open_with(common::ALL_VERSIONS);
    let id = add_skill(&db, Scope::User, "code-review", "Code Review");
    let mut spec = at_db::sources::SourceSpec::new(&id, at_db::sources::SourceKind::Git);
    spec.commit_sha = Some("installed-sha");
    let source_id = at_db::sources::upsert_source(db.connection_mut(), &spec).expect("source row");
    at_db::sources::record_update_check(
        db.connection(),
        &source_id,
        &at_db::sources::CheckOutcome {
            remote_sha: Some("checked-sha"),
            update_available: true,
            ..Default::default()
        },
    )
    .expect("record check");

    let badge = |db: &Database| {
        list_skills(db.connection(), &SkillFilter::default())
            .expect("query skills")
            .into_iter()
            .find(|row| row.item_id == Some(id))
            .expect("skill row")
            .update_available
    };
    assert!(badge(&db));
    at_db::sources::set_ignored_sha(db.connection(), &source_id, Some("checked-sha"))
        .expect("ignore checked sha");
    assert!(
        !badge(&db),
        "the ignored commit must not advertise an update"
    );
    assert!(
        at_db::sources::update_candidates(db.connection())
            .expect("candidates")
            .is_empty()
    );

    at_db::sources::record_update_check(
        db.connection(),
        &source_id,
        &at_db::sources::CheckOutcome {
            remote_sha: Some("newer-sha"),
            update_available: true,
            ..Default::default()
        },
    )
    .expect("record newer check");
    assert!(badge(&db), "a newer remote sha must show the badge again");
}
