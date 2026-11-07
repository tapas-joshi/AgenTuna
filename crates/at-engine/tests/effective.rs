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
        AgentLoads::new(
            AgentId::ClaudeCode,
            true,
            [(review.clone(), ItemState::Linked)].into(),
        ),
        AgentLoads::new(AgentId::Codex, true, BTreeMap::new()),
        AgentLoads::new(
            AgentId::Omp,
            false,
            [(review.clone(), ItemState::Linked)].into(),
        ),
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
fn inherited_effective_configuration_serializes_as_a_stable_snapshot() {
    let layers = [
        ScopeSnapshot {
            scope: Scope::Global,
            definitions: vec![skill("review", "global/review/SKILL.md")],
        },
        ScopeSnapshot {
            scope: Scope::User,
            definitions: Vec::new(),
        },
        ScopeSnapshot {
            scope: Scope::Project,
            definitions: Vec::new(),
        },
    ];
    let review = skill("review", "unused").logical_id();
    let agents = [AgentLoads::new(
        AgentId::Codex,
        true,
        [(review, ItemState::Linked)].into(),
    )];
    let result = query(EffectiveQuery {
        scope: Scope::Project,
        layers: &layers,
        agent_filter: None,
        kind_filter: Some(ItemKind::Skill),
        agents: &agents,
        locks: None,
        tombstones: None,
    });
    let serialized = serde_json::to_value(result).expect("effective config serializes");
    assert_eq!(serialized["items"][0]["winner_scope"], json!("global"));
    assert_eq!(
        serialized["items"][0]["source_path"],
        json!("global/review/SKILL.md")
    );
    assert_eq!(serialized["items"][0]["agents"][0]["agent"], json!("codex"));
    assert_eq!(
        serialized["items"][0]["agents"][0]["state"],
        json!("linked")
    );
    assert_eq!(serialized["items"][0]["loads_in_agents"], json!(1));
    assert_eq!(serialized["disabled_here"], json!([]));
    assert_eq!(serialized["diagnostics"], json!([]));
}

#[test]
fn agent_filter_returns_only_that_installed_agents_row() {
    let layers = [ScopeSnapshot {
        scope: Scope::User,
        definitions: vec![skill("review", "user/review/SKILL.md")],
    }];
    let agents = [
        AgentLoads::new(AgentId::ClaudeCode, true, BTreeMap::new()),
        AgentLoads::new(AgentId::Codex, true, BTreeMap::new()),
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

// =========================================================================
// TA-006 acceptance: kind/agent filters, disabled and locked rows, one shape.
// =========================================================================

use std::collections::BTreeMap as _Map;
use std::time::Instant;

use at_engine::effective::EffectiveQueryView;
use at_engine::locks::{LockId, LockRecord, LockStore, MemoryLockStore};
use at_engine::tombstones::{TombstoneError, TombstoneMetadata, TombstoneSource};
use at_model::ItemId;
use at_store::manifest::{DisabledAgents, DisabledEntry};

fn definition(kind: ItemKind, name: &str, path: &str, item_id: Option<ItemId>) -> Definition {
    Definition {
        kind,
        name: name.to_owned(),
        declared_id: None,
        item_id,
        path: PathBuf::from(path),
    }
}

/// Tombstone source that reports manifest metadata, which `MemoryTombstones` does not.
#[derive(Debug, Default)]
struct MetadataTombstones {
    user_entries: Vec<DisabledEntry>,
    reason: Option<String>,
    path: Option<PathBuf>,
}

impl TombstoneSource for MetadataTombstones {
    fn disabled(&self, scope: Scope) -> Result<Vec<DisabledEntry>, TombstoneError> {
        Ok(if scope == Scope::User {
            self.user_entries.clone()
        } else {
            Vec::new()
        })
    }

    fn metadata(&self, scope: Scope) -> Option<TombstoneMetadata> {
        (scope == Scope::User).then(|| TombstoneMetadata {
            reason: self.reason.clone(),
            path: self.path.clone(),
        })
    }
}

fn locked_store(scope: Scope, item_id: ItemId, reason: &str) -> MemoryLockStore {
    let store = MemoryLockStore::new();
    store
        .put(LockRecord {
            id: LockId::new(),
            lock: at_model::Lock {
                scope,
                item_id,
                locked_at: time::OffsetDateTime::now_utc(),
                reason: reason.to_owned(),
                locked_by: "ui".to_owned(),
            },
        })
        .expect("lock store accepts the record");
    store
}

#[test]
fn mcp_kind_filter_returns_one_row_per_server_with_the_installed_loader_count() {
    let github = ItemId::new();
    let layers = [ScopeSnapshot {
        scope: Scope::User,
        definitions: vec![
            definition(ItemKind::McpServer, "github", "user/mcp.json", Some(github)),
            definition(ItemKind::McpServer, "postgres", "user/mcp.json", None),
            definition(ItemKind::Skill, "review", "user/review/SKILL.md", None),
        ],
    }];
    let github_id = definition(ItemKind::McpServer, "github", "x", Some(github)).logical_id();
    let agents = [
        AgentLoads::new(
            AgentId::ClaudeCode,
            true,
            [(github_id.clone(), ItemState::Linked)].into(),
        ),
        AgentLoads::new(
            AgentId::Codex,
            true,
            [(github_id, ItemState::Linked)].into(),
        ),
        AgentLoads::new(AgentId::Omp, false, _Map::new()),
    ];
    let result = query(EffectiveQuery {
        scope: Scope::User,
        layers: &layers,
        agent_filter: None,
        kind_filter: Some(ItemKind::McpServer),
        agents: &agents,
        locks: None,
        tombstones: None,
    });

    assert_eq!(result.items.len(), 2, "one row per resolved server");
    assert!(
        result
            .items
            .iter()
            .all(|row| row.kind == ItemKind::McpServer)
    );
    let github_row = result
        .items
        .iter()
        .find(|row| row.name == "github")
        .expect("github row");
    assert_eq!(
        github_row.loads_in_agents, 2,
        "the uninstalled agent is not a loader"
    );
    assert_eq!(
        result
            .items
            .iter()
            .find(|row| row.name == "postgres")
            .expect("postgres row")
            .loads_in_agents,
        0
    );
}

#[test]
fn agent_filter_reports_a_disabled_entry_in_disabled_here_with_reason_and_tombstone_path() {
    let review = ItemId::new();
    let layers = [ScopeSnapshot {
        scope: Scope::User,
        definitions: vec![definition(
            ItemKind::Skill,
            "review",
            "user/review/SKILL.md",
            Some(review),
        )],
    }];
    let tombstones = MetadataTombstones {
        user_entries: vec![DisabledEntry {
            item_id: review,
            kind: ItemKind::Skill,
            agents: DisabledAgents::All,
        }],
        reason: Some("superseded by the org preset".to_owned()),
        path: Some(PathBuf::from("user/store.json")),
    };
    let agents = [AgentLoads::new(AgentId::Codex, true, _Map::new())];

    let result = query(EffectiveQuery {
        scope: Scope::User,
        layers: &layers,
        agent_filter: Some(&AgentId::Codex),
        kind_filter: Some(ItemKind::Skill),
        agents: &agents,
        locks: None,
        tombstones: Some(&tombstones),
    });

    assert!(result.items.is_empty(), "a disabled item is not effective");
    assert_eq!(result.disabled_here.len(), 1);
    let disabled = &result.disabled_here[0];
    assert_eq!(disabled.agent, Some(AgentId::Codex));
    assert_eq!(disabled.item_id, review);
    assert_eq!(disabled.scope, Scope::User);
    assert_eq!(
        disabled.reason.as_deref(),
        Some("superseded by the org preset")
    );
    assert_eq!(
        disabled.tombstone_path.as_deref(),
        Some(std::path::Path::new("user/store.json"))
    );
    assert_eq!(disabled.name, "review");
}

#[test]
fn locked_row_carries_the_lock_identity_and_reason() {
    let review = ItemId::new();
    let layers = [ScopeSnapshot {
        scope: Scope::Global,
        definitions: vec![definition(
            ItemKind::Skill,
            "review",
            "global/review/SKILL.md",
            Some(review),
        )],
    }];
    let locks = locked_store(Scope::Global, review, "Managed by organization policy");
    let agents = [AgentLoads::new(AgentId::Codex, true, _Map::new())];

    let result = query(EffectiveQuery {
        scope: Scope::Global,
        layers: &layers,
        agent_filter: None,
        kind_filter: None,
        agents: &agents,
        locks: Some(&locks),
        tombstones: None,
    });

    let lock = result.items[0].lock.as_ref().expect("locked row");
    assert!(lock.id.starts_with("lck_"), "lock id is `{}`", lock.id);
    assert_eq!(lock.reason, "Managed by organization policy");
}

#[test]
fn five_hundred_items_across_five_agents_sort_by_kind_then_name_under_the_budget() {
    let mut definitions = Vec::with_capacity(500);
    for index in 0..500 {
        let kind = match index % 3 {
            0 => ItemKind::Skill,
            1 => ItemKind::McpServer,
            _ => ItemKind::Rule,
        };
        definitions.push(definition(
            kind,
            &format!("item-{index:03}"),
            &format!("user/item-{index:03}"),
            None,
        ));
    }
    definitions.reverse();
    let layers = [ScopeSnapshot {
        scope: Scope::User,
        definitions,
    }];
    let agents: Vec<AgentLoads> = [
        AgentId::ClaudeCode,
        AgentId::Codex,
        AgentId::Omp,
        AgentId::Cursor,
        AgentId::Custom("windsurf".to_owned()),
    ]
    .into_iter()
    .map(|agent| AgentLoads::new(agent, true, _Map::new()))
    .collect();

    let started = Instant::now();
    let result = query(EffectiveQuery {
        scope: Scope::User,
        layers: &layers,
        agent_filter: None,
        kind_filter: None,
        agents: &agents,
        locks: None,
        tombstones: None,
    });
    let elapsed = started.elapsed();

    assert_eq!(result.items.len(), 500);
    assert!(
        result
            .items
            .windows(2)
            .all(|pair| (pair[0].kind, pair[0].name.as_str())
                <= (pair[1].kind, pair[1].name.as_str())),
        "items must be sorted by kind then display name"
    );
    assert!(result.items.iter().all(|row| row.agents.len() == 5));
    let budget = if cfg!(debug_assertions) {
        std::time::Duration::from_secs(5)
    } else {
        std::time::Duration::from_millis(200)
    };
    assert!(
        elapsed < budget,
        "cold query took {elapsed:?}, above {budget:?}"
    );
}

#[test]
fn query_view_serializes_once_for_both_frontends() {
    let result = query(EffectiveQuery {
        scope: Scope::User,
        layers: &[ScopeSnapshot {
            scope: Scope::User,
            definitions: vec![definition(ItemKind::Skill, "review", "user/review", None)],
        }],
        agent_filter: None,
        kind_filter: None,
        agents: &[],
        locks: None,
        tombstones: None,
    });
    let view = EffectiveQueryView::new(result, Vec::new());
    let json = serde_json::to_value(&view).expect("view serializes");
    assert_eq!(json["items"][0]["name"], serde_json::json!("review"));
    assert_eq!(json["disabled_here"], serde_json::json!([]));
    assert_eq!(json["diagnostics"], serde_json::json!([]));
    assert_eq!(json["load_diagnostics"], serde_json::json!([]));
    let decoded: EffectiveQueryView = serde_json::from_value(json.clone()).expect("round trip");
    assert_eq!(decoded, view);
}
