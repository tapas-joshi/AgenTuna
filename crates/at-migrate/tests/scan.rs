//! TC-006 native scan regression coverage.

use std::collections::BTreeSet;
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::scan::{Ownership, ParseStatus, ScanRequest, scan};
use at_migrate::testkit::{MigrationFixture, NATIVE_SKILL};
use at_model::{AgentId, LinkMode};

#[test]
fn scans_skills_mcp_and_project_rules_with_scopes_and_agents()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let paths = fixture.paths();
    let env = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    let result = scan(ScanRequest {
        environment: &env,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    assert!(
        result
            .items
            .iter()
            .any(|item| item.kind == at_model::ItemKind::Skill
                && item.name == NATIVE_SKILL
                && item.scope == at_model::Scope::Project)
    );
    assert!(
        result
            .items
            .iter()
            .any(|item| item.kind == at_model::ItemKind::McpServer
                && item.name == "example"
                && item.scope == at_model::Scope::User)
    );
    assert!(
        result.items.iter().any(|item| {
            item.kind == at_model::ItemKind::Rule
                && item.path == paths.project.join("AGENTS.md")
                && item.scope == at_model::Scope::Project
        }),
        "{:?}",
        result.items
    );
    let conflicts = detect_conflicts(&result);
    assert!(
        conflicts
            .conflicts
            .iter()
            .any(|conflict| conflict.name == NATIVE_SKILL)
    );
    assert!(
        !result
            .items
            .iter()
            .any(|item| item.path == paths.managed_deployment)
    );
    Ok(())
}

#[test]
fn overlapping_roots_deduplicate_skills_and_merge_agent_owners()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let paths = fixture.paths();
    let env = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let mut stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    stores.push(PathBuf::from("/no-store-root"));
    let result = scan(ScanRequest {
        environment: &env,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    let shared = result
        .items
        .iter()
        .find(|item| {
            item.name == NATIVE_SKILL
                && item.scope == at_model::Scope::Project
                && item.path.starts_with(paths.project.join(".agents"))
        })
        .ok_or("shared skill not found")?;
    assert_eq!(
        shared.agents,
        BTreeSet::from([AgentId::Codex, AgentId::Omp])
    );
    Ok(())
}

#[test]
fn malformed_entries_are_item_errors_and_scan_continues() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = MigrationFixture::new(LinkMode::Copy)?;
    let paths = fixture.paths();
    std::fs::create_dir_all(paths.project.join(".agents/skills/broken"))?;
    std::fs::write(
        paths.project.join(".agents/skills/broken/SKILL.md"),
        b"not a skill document",
    )?;
    let env = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    let result = scan(ScanRequest {
        environment: &env,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    assert!(result.partial);
    assert!(
        result
            .items
            .iter()
            .any(|item| item.kind == at_model::ItemKind::McpServer && item.name == "example")
    );
    assert!(result.items.iter().any(|item| {
        item.path.ends_with("broken") && matches!(item.parse_status, ParseStatus::Error(_))
    }));
    assert!(
        result
            .items
            .iter()
            .all(|item| item.ownership == Ownership::NotInAgenTuna)
    );
    Ok(())
}
