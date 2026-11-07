//! Skill shadow and scope-precedence matrix regressions (TS-043).

use std::path::PathBuf;

use at_engine::resolve::{ResolveInput, resolve};
use at_engine::skill_shadows::shadow_chain;
use at_model::{Definition, ItemKind, Scope, ScopeSnapshot};

fn skill(name: &str, path: &str) -> Definition {
    Definition {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        declared_id: None,
        item_id: None,
        path: PathBuf::from(path),
    }
}

fn scope_snapshot(scope: Scope, name: &str) -> ScopeSnapshot {
    ScopeSnapshot {
        scope,
        definitions: vec![skill("review", name)],
    }
}

#[test]
fn duplicate_skill_matrix_selects_nearest_scope_and_orders_every_shadow() {
    let cases = [
        (Scope::Global, "global"),
        (Scope::User, "user"),
        (Scope::Project, "project"),
        (Scope::Local, "local"),
    ];

    for (winner_scope, winner_path) in cases {
        let layers = [
            scope_snapshot(Scope::Global, "global/review/SKILL.md"),
            scope_snapshot(Scope::User, "user/review/SKILL.md"),
            scope_snapshot(Scope::Project, "project/review/SKILL.md"),
            scope_snapshot(Scope::Local, "local/review/SKILL.md"),
        ];
        let result = resolve(ResolveInput::new(&layers));
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].scope, Scope::Local);
        assert_eq!(
            result.items[0].definition.path,
            PathBuf::from("local/review/SKILL.md")
        );
        assert_eq!(result.shadowed.len(), 3);
        assert!(result.shadowed.iter().all(|shadow| {
            shadow.shadowed_by.scope == Scope::Local
                && shadow.shadowed_by.path == *"local/review/SKILL.md"
                && shadow.blocked.is_none()
        }));
        assert!(
            result
                .shadowed
                .iter()
                .any(|shadow| shadow.scope == Scope::Global)
        );
        assert!(
            result
                .shadowed
                .iter()
                .any(|shadow| shadow.scope == Scope::User)
        );
        assert!(
            result
                .shadowed
                .iter()
                .any(|shadow| shadow.scope == Scope::Project)
        );

        // Restricting the visible layer set to each cutoff proves every precedence step,
        // not only the maximum scope.
        let visible: Vec<_> = layers
            .iter()
            .filter(|layer| layer.scope.precedence() <= winner_scope.precedence())
            .cloned()
            .collect();
        let at_cutoff = resolve(ResolveInput::new(&visible));
        assert_eq!(
            at_cutoff.items[0].scope, winner_scope,
            "cutoff {winner_path}"
        );
        assert_eq!(
            at_cutoff.items[0].definition.path,
            PathBuf::from(format!("{winner_path}/review/SKILL.md"))
        );
    }
}

#[test]
fn all_scope_subsets_are_independent_of_input_layer_order() {
    let scopes = [Scope::Global, Scope::User, Scope::Project, Scope::Local];
    let mut rows = 0;
    for mask in 1_u8..16 {
        let mut layers = scopes
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, scope)| {
                let path = format!("{}/review/SKILL.md", scope.wire_name());
                scope_snapshot(*scope, &path)
            })
            .collect::<Vec<_>>();
        for reverse in [false, true] {
            if reverse {
                layers.reverse();
            }
            let resolved = resolve(ResolveInput::new(&layers));
            let expected_scope = layers
                .iter()
                .map(|layer| layer.scope)
                .max_by_key(|scope| scope.precedence())
                .expect("nonempty scope subset");
            assert_eq!(resolved.items.len(), 1);
            assert_eq!(resolved.items[0].scope, expected_scope);
            assert_eq!(
                resolved.items[0].definition.path,
                PathBuf::from(format!("{}/review/SKILL.md", expected_scope.wire_name()))
            );
            assert_eq!(resolved.shadowed.len(), layers.len() - 1);
            assert!(resolved.shadowed.iter().all(|shadow| {
                shadow.shadowed_by.scope == expected_scope && shadow.blocked.is_none()
            }));
            rows += 1;
            if reverse {
                layers.reverse();
            }
        }
    }
    assert_eq!(rows, 30, "15 nonempty scope subsets in both layer orders");
}

#[test]
fn comparison_chain_orders_shadowed_copies_nearest_to_farthest() {
    let chain = shadow_chain(
        Scope::Local,
        &[Scope::Global, Scope::User, Scope::Project, Scope::Local],
    );
    assert_eq!(
        chain.shadowed,
        vec![Scope::Project, Scope::User, Scope::Global]
    );
}

#[test]
fn adding_a_deeper_skill_definition_does_not_change_other_logical_ids() {
    let before = [ScopeSnapshot {
        scope: Scope::User,
        definitions: vec![
            skill("review", "user/review/SKILL.md"),
            skill("format", "user/format/SKILL.md"),
        ],
    }];
    let after = [
        ScopeSnapshot {
            scope: Scope::User,
            definitions: vec![
                skill("review", "user/review/SKILL.md"),
                skill("format", "user/format/SKILL.md"),
            ],
        },
        ScopeSnapshot {
            scope: Scope::Local,
            definitions: vec![skill("review", "local/review/SKILL.md")],
        },
    ];
    let before = resolve(ResolveInput::new(&before));
    let after = resolve(ResolveInput::new(&after));
    let format_before = before
        .items
        .iter()
        .find(|item| item.definition.name == "format")
        .expect("format winner");
    let format_after = after
        .items
        .iter()
        .find(|item| item.definition.name == "format")
        .expect("format winner");
    assert_eq!(format_after.scope, format_before.scope);
    assert_eq!(format_after.definition.path, format_before.definition.path);
    let review = after
        .items
        .iter()
        .find(|item| item.definition.name == "review")
        .expect("review winner");
    assert_eq!(review.scope, Scope::Local);
}

// --- TS-043: list and compare presentation matrix ---

use at_engine::locks::{LockId, LockRecord, LockStore, MemoryLockStore};
use at_engine::tombstones::MemoryTombstones;
use at_engine::why::{StepStatus, TraceEvidence, trace};
use at_model::{AgentId, ItemId, Lock};
use at_store::manifest::{DisabledAgents, DisabledEntry};

fn owned_skill(name: &str, path: &str, item_id: ItemId) -> Definition {
    Definition {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        declared_id: None,
        item_id: Some(item_id),
        path: PathBuf::from(path),
    }
}

fn owned_snapshot(scope: Scope, definition: Definition) -> ScopeSnapshot {
    ScopeSnapshot {
        scope,
        definitions: vec![definition],
    }
}

fn lock_record(item_id: ItemId, reason: &str) -> LockRecord {
    LockRecord {
        id: LockId::new(),
        lock: Lock {
            scope: Scope::Global,
            item_id,
            reason: reason.to_owned(),
            locked_by: "admin".to_owned(),
            locked_at: time::OffsetDateTime::UNIX_EPOCH,
        },
    }
}

fn tombstone(item_id: ItemId, agents: DisabledAgents) -> DisabledEntry {
    DisabledEntry {
        item_id,
        kind: ItemKind::Skill,
        agents,
    }
}

fn evidence(
    defined: bool,
    winning: bool,
    shadowed: bool,
    lock_reason: Option<&str>,
) -> TraceEvidence {
    TraceEvidence {
        defined,
        disabled: false,
        winning,
        shadowed,
        lock_reason: lock_reason.map(str::to_owned),
        unreadable: None,
        location: None,
    }
}

#[test]
fn shadow_lock_and_disable_matrix_reports_states_and_reason_strings() {
    let scopes = [Scope::Global, Scope::User, Scope::Project, Scope::Local];
    let mut rows = 0usize;

    // (1) All sixteen scope pairs: nearest scope wins and the loser is reported.
    for &first in &scopes {
        for &second in &scopes {
            let first_id = ItemId::new();
            let second_id = ItemId::new();
            let first_path = format!("{}/review/SKILL.md", first.wire_name());
            let second_path = format!("{}/review/SKILL.md", second.wire_name());
            let mut layers = vec![owned_snapshot(
                first,
                owned_skill("review", &first_path, first_id),
            )];
            if second != first {
                layers.push(owned_snapshot(
                    second,
                    owned_skill("review", &second_path, second_id),
                ));
            }
            let expected = if second.precedence() > first.precedence() {
                second
            } else {
                first
            };
            let resolved = resolve(ResolveInput::new(&layers));
            let logical = owned_skill("review", &first_path, first_id).logical_id();
            let winner = resolved.get(&logical).expect("winner row");
            assert_eq!(winner.scope, expected, "pair {first:?}/{second:?}");
            if first != second {
                let loser_scope = if expected == first { second } else { first };
                let loser_path = if loser_scope == first {
                    &first_path
                } else {
                    &second_path
                };
                let winner_path = if expected == first {
                    &first_path
                } else {
                    &second_path
                };
                let loser = resolved
                    .shadowed
                    .iter()
                    .find(|shadowed| shadowed.scope == loser_scope)
                    .expect("losing copy is reported");
                assert_eq!(loser.shadowed_by.scope, expected);
                assert_eq!(loser.definition.path, PathBuf::from(loser_path));
                assert_eq!(loser.shadowed_by.path, PathBuf::from(winner_path));
                assert!(!loser.is_blocked(), "precedence alone is not a lock");
                let chain = shadow_chain(expected, &[first, second]);
                assert_eq!(chain.winner, expected);
                assert_eq!(chain.shadowed, vec![loser_scope]);
            }
            rows += 1;
        }
    }

    // (2) Lock variations: a locked Global copy outranks and cannot be disabled.
    for &other in &[Scope::User, Scope::Project, Scope::Local] {
        for global_first in [true, false] {
            let global_id = ItemId::new();
            let other_id = ItemId::new();
            let global_path = "global/review/SKILL.md";
            let other_path = format!("{}/review/SKILL.md", other.wire_name());
            let mut layers = Vec::new();
            if global_first {
                layers.push(owned_snapshot(
                    Scope::Global,
                    owned_skill("review", global_path, global_id),
                ));
                layers.push(owned_snapshot(
                    other,
                    owned_skill("review", &other_path, other_id),
                ));
            } else {
                layers.push(owned_snapshot(
                    other,
                    owned_skill("review", &other_path, other_id),
                ));
                layers.push(owned_snapshot(
                    Scope::Global,
                    owned_skill("review", global_path, global_id),
                ));
            }
            let locks = MemoryLockStore::new();
            locks
                .put(lock_record(global_id, "organization policy"))
                .expect("lock store");
            let logical = owned_skill("review", global_path, global_id).logical_id();

            let resolved = resolve(ResolveInput::new(&layers).with_locks(&locks));
            let winner = resolved.get(&logical).expect("locked winner");
            assert_eq!(winner.scope, Scope::Global, "a lock outranks {other:?}");
            assert_eq!(winner.lock_reason(), Some("organization policy"));
            let blocked = resolved
                .shadowed
                .iter()
                .find(|shadowed| shadowed.scope == other)
                .expect("nearer copy is reported as blocked");
            assert!(
                blocked.is_blocked(),
                "the nearer copy cannot shadow the lock"
            );
            assert_eq!(
                blocked.blocking_lock().expect("blocking lock").reason,
                "organization policy"
            );

            // A disable-here tombstone cannot remove a locked item.
            let tombstones = MemoryTombstones::new();
            tombstones
                .add(Scope::Local, tombstone(global_id, DisabledAgents::All))
                .expect("tombstone");
            let guarded = resolve(
                ResolveInput::new(&layers)
                    .with_locks(&locks)
                    .with_tombstones(&tombstones),
            );
            assert!(
                guarded.get(&logical).is_some(),
                "a locked item cannot be disabled"
            );
            assert!(guarded.disabled.is_empty());

            let trace_row = trace(
                global_id,
                Scope::Local,
                AgentId::ClaudeCode,
                [
                    (
                        Scope::Global,
                        evidence(true, true, false, Some("organization policy")),
                    ),
                    (Scope::User, evidence(false, false, false, None)),
                    (Scope::Project, evidence(false, false, false, None)),
                    (Scope::Local, evidence(false, false, false, None)),
                ],
            );
            assert_eq!(
                trace_row.steps[0].status,
                StepStatus::Locked {
                    reason: "organization policy".to_owned()
                }
            );
            assert_eq!(
                trace_row.steps[0].reason, "Pinned by Global lock: organization policy",
                "the detail reason names the lock"
            );
            rows += 1;
        }
    }

    // (3) Disable-here variations for every scope and agent selection.
    for &winner_scope in &scopes {
        for (agents, agent, removed) in [
            (DisabledAgents::All, AgentId::ClaudeCode, true),
            (
                DisabledAgents::Only(vec![AgentId::Codex]),
                AgentId::Codex,
                true,
            ),
            (
                DisabledAgents::Only(vec![AgentId::Codex]),
                AgentId::Omp,
                false,
            ),
            (
                DisabledAgents::Only(vec![AgentId::Omp]),
                AgentId::Codex,
                false,
            ),
        ] {
            let item_id = ItemId::new();
            let path = format!("{}/review/SKILL.md", winner_scope.wire_name());
            let layers = [owned_snapshot(
                winner_scope,
                owned_skill("review", &path, item_id),
            )];
            let tombstones = MemoryTombstones::new();
            tombstones
                .add(winner_scope, tombstone(item_id, agents))
                .expect("tombstone");
            let resolved = resolve(
                ResolveInput::new(&layers)
                    .with_tombstones(&tombstones)
                    .for_agent(&agent),
            );
            let logical = owned_skill("review", &path, item_id).logical_id();
            if removed {
                assert!(
                    resolved.get(&logical).is_none(),
                    "{agent:?} at {winner_scope:?} is disabled"
                );
                let disabled = resolved
                    .disabled
                    .iter()
                    .find(|entry| entry.item_id == item_id)
                    .expect("disable record keeps the copy identifiable");
                assert_eq!(disabled.scope, winner_scope);
                assert_eq!(disabled.definition.path, PathBuf::from(&path));
                let comparison = resolve(ResolveInput::new(&layers))
                    .get(&logical)
                    .cloned()
                    .expect("the copy stays available for comparison");
                assert_eq!(comparison.definition.path, PathBuf::from(&path));
            } else {
                assert!(
                    resolved.get(&logical).is_some(),
                    "{agent:?} is not listed by the tombstone, so the item stays effective"
                );
            }
            rows += 1;
        }
    }

    assert!(rows >= 32, "the matrix covers {rows} rows");
}

#[test]
fn native_copy_never_silently_wins_and_the_conflict_names_its_path() {
    use std::collections::BTreeMap;

    use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
    use at_db::skills_index::SkillFilter;
    use at_engine::deploy::{DeployPlanError, PlanRequest, plan};
    use at_engine::skills::{SkillCellState, index};
    use at_model::LinkMode;

    let temp = tempfile::tempdir().expect("temporary root");
    let root = std::fs::canonicalize(temp.path()).expect("canonical temp root");
    let home = root.join("home");
    let project = root.join("project");
    let native_dir = home.join(".claude/skills/review");
    std::fs::create_dir_all(&native_dir).expect("native skill folder");
    std::fs::write(
        native_dir.join("SKILL.md"),
        "---\nname: review\ndescription: unmanaged copy\n---\n",
    )
    .expect("native skill document");

    let database = at_db::Database::open_in_memory().expect("database");
    let item_id = ItemId::new();
    database
        .connection()
        .execute(
            "INSERT INTO items (id,kind,scope,slug,name,state,managed) \
             VALUES (?1,'skill','user','review','review','active',1)",
            [item_id.to_string()],
        )
        .expect("managed item");

    let env = AgentEnvironment {
        home,
        project: Some(project),
        variables: BTreeMap::new(),
    };
    let registry = AgentAdapterRegistry::register();
    let listed = index(
        database.connection(),
        &SkillFilter::default(),
        &registry,
        &env,
        &[],
    )
    .expect("skills index");

    let native_row = listed
        .rows
        .iter()
        .find(|row| row.skill.native_path.as_deref() == native_dir.to_str())
        .expect("the unmanaged copy has its own row");
    assert_eq!(
        native_row.skill.item_id, None,
        "native row is not canonical"
    );
    assert_eq!(native_row.skill.state.as_deref(), Some("native"));
    assert!(
        matches!(
            native_row.agents.get(&AgentId::ClaudeCode),
            Some(SkillCellState::Native { path }) if path == &native_dir
        ),
        "the row reports the unmanaged path instead of silently adopting it"
    );
    let managed_row = listed
        .rows
        .iter()
        .find(|row| row.skill.item_id == Some(item_id))
        .expect("managed row stays separate");
    assert_eq!(managed_row.skill.native_path, None);
    assert_ne!(managed_row.skill.name, "");

    let store_root = root.join("store");
    let source = store_root.join("skills/review");
    std::fs::create_dir_all(&source).expect("canonical package folder");
    let base = PlanRequest {
        item_id,
        scope: Scope::User,
        agent: AgentId::ClaudeCode,
        store_root: store_root.clone(),
        source: source.clone(),
        destination: native_dir.clone(),
        mode: LinkMode::Link,
        link_supported: true,
        managed: true,
        destination_managed: false,
        shared_agents: Vec::new(),
        project_id: None,
        summary: String::from("deploy review"),
    };
    let conflict = plan(base.clone()).expect_err("an unmanaged destination is refused");
    match conflict {
        DeployPlanError::NativeConflict { path } => {
            assert_eq!(path, native_dir, "the row names the path")
        }
        other => panic!("unexpected deploy error: {other:?}"),
    }
    let managed = plan(PlanRequest {
        destination_managed: true,
        ..base
    })
    .expect("a managed destination is allowed");
    assert!(managed.change_set.is_some());
}
