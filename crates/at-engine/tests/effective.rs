//! Effective configuration query API regression tests.

use std::collections::BTreeMap;
use std::path::PathBuf;

use at_engine::effective::{AgentLoads, EffectiveQuery, query};
use at_model::{AgentId, Definition, ItemKind, ItemState, Scope, ScopeSnapshot};
use serde_json::json;

fn skill(name: &str, path: &str) -> Definition {
    Definition {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        declared_id: None,
        item_id: None,
        path: PathBuf::from(path),
    }
}

#[test]
fn query_resolves_nearest_scope_and_counts_only_installed_loaders() {
    let layers = [
        ScopeSnapshot {
            scope: Scope::User,
            definitions: vec![skill("review", "user/review/SKILL.md")],
        },
        ScopeSnapshot {
            scope: Scope::Project,
            definitions: vec![skill("review", "project/review/SKILL.md")],
        },
        ScopeSnapshot {
            scope: Scope::Local,
            definitions: vec![skill("local-only", "local/local-only/SKILL.md")],
        },
    ];
    let review = skill("review", "unused").logical_id();
    let agents = [
        AgentLoads {
            agent: AgentId::ClaudeCode,
            installed: true,
            loaded: [(review.clone(), ItemState::Linked)].into(),
        },
        AgentLoads {
            agent: AgentId::Codex,
            installed: true,
            loaded: BTreeMap::new(),
        },
        AgentLoads {
            agent: AgentId::Omp,
            installed: false,
            loaded: [(review.clone(), ItemState::Linked)].into(),
        },
    ];
    let result = query(EffectiveQuery {
        scope: Scope::Project,
        layers: &layers,
        agent_filter: None,
        kind_filter: Some(ItemKind::Skill),
        agents: &agents,
        locks: None,
        tombstones: None,
    });

    assert_eq!(result.items.len(), 1);
    let row = &result.items[0];
    assert_eq!(row.winner_scope, Scope::Project);
    assert_eq!(row.source_path, PathBuf::from("project/review/SKILL.md"));
    assert_eq!(row.loads_in_agents, 1);
    assert_eq!(row.agents.len(), 2);
    assert!(row.agents[0].loads);
    assert_eq!(row.agents[1].state, ItemState::Off);
    let serialized = serde_json::to_value(&result).expect("effective config serializes");
    assert_eq!(serialized["items"][0]["loads_in_agents"], json!(1));
}

#[test]
fn agent_filter_returns_only_that_installed_agents_row() {
    let layers = [ScopeSnapshot {
        scope: Scope::User,
        definitions: vec![skill("review", "user/review/SKILL.md")],
    }];
    let agents = [
        AgentLoads {
            agent: AgentId::ClaudeCode,
            installed: true,
            loaded: BTreeMap::new(),
        },
        AgentLoads {
            agent: AgentId::Codex,
            installed: true,
            loaded: BTreeMap::new(),
        },
    ];
    let result = query(EffectiveQuery {
        scope: Scope::User,
        layers: &layers,
        agent_filter: Some(&AgentId::Codex),
        kind_filter: None,
        agents: &agents,
        locks: None,
        tombstones: None,
    });

    assert_eq!(result.items[0].agents.len(), 1);
    assert_eq!(result.items[0].agents[0].agent, AgentId::Codex);
}
