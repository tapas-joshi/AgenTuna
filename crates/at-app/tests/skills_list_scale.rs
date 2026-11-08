//! Headless cost of the Skills-screen projection on a full-size index.
//!
//! The GUI mounts `skills_list` on the first click, so its cost is part of the measured
//! click-to-rows budget. This test seeds the real schema with the release fixture's shape
//! (3,000 skills, nine deployment cells each, one lock) and asserts the query stays far below
//! a generous ceiling; the printed best-of-three is the number to compare across changes.

mod support;

use std::time::{Duration, Instant};

use at_app::app::flow_harness;
use at_app::app::skills_list::SkillsListRequest;
use at_app::app::workflows::agents_list_inner;
use at_db::Database;
use at_model::AgentId;
use rusqlite::params;
use support::{Sandbox, TestResult};

const ITEMS: i64 = 3_000;
const AGENTS: [&str; 3] = ["claude_code", "codex", "omp"];

/// Seeds the release fixture's row shape directly, without touching the filesystem.
fn seed(database: &Database) -> TestResult<()> {
    let connection = database.connection();
    connection.execute(
        "INSERT INTO agents (id, display_name, installed, managed) \
         VALUES ('claude_code', 'Claude Code', 1, 1), ('codex', 'Codex', 1, 1), ('omp', 'Oh My Pi', 1, 1)",
        [],
    )?;
    let tx = connection.unchecked_transaction()?;
    {
        let mut items = tx.prepare_cached(
            "INSERT INTO items (id, kind, scope, slug, name, source_path, state, managed, tags_json) \
             VALUES (?1, 'skill', 'user', ?2, ?3, ?4, 'active', 1, '{\"description\":\"Synthetic scale fixture\"}')",
        )?;
        let mut deployments = tx.prepare_cached(
            "INSERT INTO deployments (item_id, agent_id, scope, state, mode, target_path, store_hash, target_hash, baseline_hash) \
             VALUES (?1, ?2, 'user', 'linked', 'symlink', ?3, 'h', 'h', 'h')",
        )?;
        let mut locks = tx.prepare_cached(
            "INSERT INTO locks (item_id, scope, reason) VALUES (?1, 'global', 'scale fixture')",
        )?;
        for index in 0..ITEMS {
            let id = at_model::ItemId::new().to_string();
            let slug = format!("scale-skill-{index:05}");
            items.execute(params![
                id,
                slug,
                format!("Scale Skill {index:05}"),
                format!("/sandbox/home/.agentuna/<store>/{slug}/SKILL.md"),
            ])?;
            for agent in AGENTS {
                deployments.execute(params![
                    id,
                    agent,
                    format!("/sandbox/home/.{agent}/skills/{slug}")
                ])?;
            }
            if index == 0 {
                locks.execute(params![id])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

#[test]
fn skills_list_projection_stays_within_the_click_budget_at_three_thousand_items() -> TestResult<()>
{
    let sandbox = Sandbox::new()?;
    seed(&sandbox.database()?)?;
    let state = sandbox.state()?;
    let request = SkillsListRequest {
        scope: None,
        project_id: None,
    };

    let mut samples = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        let view = flow_harness::skills_list(&state, &request)?;
        samples.push(start.elapsed());
        assert_eq!(
            view.rows.len(),
            ITEMS as usize,
            "every seeded skill row must be projected"
        );
        assert!(
            view.rows.iter().all(|row| row.agents.len() == AGENTS.len()),
            "every row must carry one cell per detected agent"
        );
    }
    let best = samples.iter().min().copied().unwrap_or_default();
    println!("skills_list_ms={:?} samples={samples:?}", best.as_millis());
    // A generous ceiling: the measured miss is the GUI click path, not this query. Debug
    // builds run the projection unoptimized, so the bound only has to catch an N+1 query
    // storm or a quadratic join.
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(2_000)
    } else {
        Duration::from_millis(500)
    };
    assert!(
        best < budget,
        "skills_list projection took {best:?} (budget {budget:?})"
    );
    Ok(())
}

#[test]
fn agents_list_costs_a_few_counts_at_three_thousand_items() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    seed(&sandbox.database()?)?;
    let state = sandbox.state()?;

    let mut samples = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        let cards = agents_list_inner(&state)?;
        samples.push(start.elapsed());
        assert_eq!(cards.len(), AGENTS.len());
        assert!(cards.iter().all(|card| card.total == ITEMS as i32));
        assert!(
            cards
                .iter()
                .all(|card| card.linked_deployments == ITEMS as i32)
        );
    }
    let best = samples.iter().min().copied().unwrap_or_default();

    // What the card used to do for one number: materialise every deployment row of the agent.
    let database = sandbox.database()?;
    let scan = Instant::now();
    for agent in [AgentId::ClaudeCode, AgentId::Codex, AgentId::Omp] {
        let rows = at_db::deployments::list_deployments_for_agent(database.connection(), &agent)?;
        assert_eq!(rows.len(), ITEMS as usize);
    }
    let scan_elapsed = scan.elapsed();
    println!(
        "agents_list_ms={} removed_deployment_scan_ms={}",
        best.as_millis(),
        scan_elapsed.as_millis()
    );

    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(2_000)
    } else {
        Duration::from_millis(500)
    };
    assert!(
        best < budget,
        "agents_list took {best:?} (budget {budget:?})"
    );
    Ok(())
}
