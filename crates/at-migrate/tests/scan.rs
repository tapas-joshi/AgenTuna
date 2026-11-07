//! TC-006 native scan regression coverage.

use std::collections::BTreeSet;
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::scan::read_mcp_entries;
use at_migrate::scan::{
    Ownership, ParseStatus, ScanError, ScanObserver, ScanProgress, ScanRequest, scan, scan_with,
};
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
    let managed = result
        .items
        .iter()
        .find(|item| item.path == paths.managed_deployment)
        .expect("a store deployment is reported as managed");
    assert!(managed.store_link, "the scan records the store link");
    assert_eq!(
        managed.ownership,
        Ownership::Managed,
        "a store deployment is classified Managed"
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
    assert_eq!(
        result
            .items
            .iter()
            .filter(|item| item.path == shared.path)
            .count(),
        1,
        "the physical skill folder appears once"
    );
    Ok(())
}

#[test]
fn reads_jsonc_mcp_configs_with_comments() -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = PathBuf::from("claude.json");
    let claude = br#"{
      // Preserve comments and unrelated keys.
      "mcpServers": {
        "claude_alpha": { "command": "node", "args": ["alpha.js"] },
        "claude_beta": { "command": "python", "args": ["beta.py"] }
      },
      "unrelated": { "keep": true }
    }"#;
    let claude_entries = read_mcp_entries(&claude_path, claude)?;
    assert_eq!(claude_entries.len(), 2);
    assert_eq!(claude_entries["claude_alpha"]["command"], "node");
    assert_eq!(claude_entries["claude_beta"]["command"], "python");

    let omp_path = PathBuf::from("mcp.json");
    let omp = br#"{
      // Preserve the omp schema and its unrelated property.
      "$schema": "https://example.invalid/schema.json",
      "mcpServers": {
        "omp_alpha": { "command": "node", "args": ["alpha.js"] },
        "omp_beta": { "command": "python", "args": ["beta.py"] }
      },
      "unrelated": { "keep": true }
    }"#;
    let omp_entries = read_mcp_entries(&omp_path, omp)?;
    assert_eq!(omp_entries.len(), 2);
    assert_eq!(omp_entries["omp_alpha"]["command"], "node");
    assert_eq!(omp_entries["omp_beta"]["command"], "python");
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

/// Records every discovery step and can stop the scan after a chosen number of them.
struct Recorder {
    seen: Vec<ScanProgress>,
    stop_after: usize,
}

impl ScanObserver for Recorder {
    fn step(&mut self, progress: &ScanProgress) -> bool {
        self.seen.push(progress.clone());
        self.stop_after == 0 || self.seen.len() < self.stop_after
    }
}

#[test]
fn scan_reports_every_step_and_stops_cleanly_on_cancel() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let paths = fixture.paths();
    let environment = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];

    // One step per adapter, scope and phase: three scopes, skills plus two configuration
    // kinds, for every registered adapter.
    let total = adapters.adapters()?.len() * 3 * 3;
    let mut recorder = Recorder {
        seen: Vec::new(),
        stop_after: 0,
    };
    let report = scan_with(
        ScanRequest {
            environment: &environment,
            adapters: &adapters,
            store_roots: &stores,
            store_items: &[],
        },
        &mut recorder,
    )?;
    assert_eq!(recorder.seen.len(), total, "one progress step per unit");
    assert!(
        recorder.seen.iter().all(|step| step.total == total),
        "every step knows the total"
    );
    assert_eq!(
        recorder
            .seen
            .iter()
            .map(|step| step.completed)
            .collect::<Vec<_>>(),
        (0..total).collect::<Vec<_>>(),
        "steps count up without gaps"
    );
    let mut units = BTreeSet::new();
    for step in &recorder.seen {
        units.insert((step.agent.clone(), step.scope, step.kind));
    }
    assert_eq!(units.len(), total, "no unit is reported twice");
    // The same request without an observer produces the same result.
    let plain = scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    assert_eq!(report, plain);

    let mut cancelled = Recorder {
        seen: Vec::new(),
        stop_after: 2,
    };
    let outcome = scan_with(
        ScanRequest {
            environment: &environment,
            adapters: &adapters,
            store_roots: &stores,
            store_items: &[],
        },
        &mut cancelled,
    );
    assert!(matches!(outcome, Err(ScanError::Cancelled)));
    assert_eq!(
        cancelled.seen.len(),
        2,
        "a cancel stops before the third step"
    );
    Ok(())
}
