//! TC-034: table-driven migration plan matrix.
//!
//! Every strategy and conflict-resolution permutation is exercised against the same builder,
//! and the derived [`PlanCounts`] are compared with the per-action dry-run row counts so a
//! miscount cannot hide behind a green plan.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use at_migrate::conflicts::{detect_conflicts, detect_conflicts_with_store};
use at_migrate::plan::{
    BuildPlanRequest, ConflictResolution, DryRunRow, MigrationAction, MigrationPlan,
    MigrationStrategy, PlanCounts, PlanError, build_plan, dry_run, validate_plan,
};
use at_migrate::scan::{DiscoveryState, Ownership, ParseStatus, ScanItem, ScanReport, StoreItem};
use at_model::{AgentId, ItemKind, Scope};
use tempfile::TempDir;

/// One temporary workspace: a store root, a backup root and native files.
struct Workspace {
    temp: TempDir,
    store: PathBuf,
    backup: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let temp = TempDir::new().expect("temporary workspace");
        let root = fs::canonicalize(temp.path()).expect("canonical workspace");
        let store = root.join("store");
        let backup = root.join("backups");
        at_store::layout::init_store(Scope::User, &store).expect("initialised store");
        Self {
            temp,
            store,
            backup,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(&path, bytes).expect("fixture file");
        path
    }

    fn skill(&self, relative: &str, document: &[u8], agent: AgentId) -> ScanItem {
        let root = self.path(relative);
        fs::create_dir_all(&root).expect("skill root");
        fs::write(root.join("SKILL.md"), document).expect("skill document");
        let normalized = at_store::manifest::hash_skill_folder(&root)
            .expect("skill tree hash")
            .to_string();
        ScanItem {
            kind: ItemKind::Skill,
            name: root
                .file_name()
                .and_then(|name| name.to_str())
                .expect("skill name")
                .to_owned(),
            agents: BTreeSet::from([agent]),
            path: root,
            scope: Scope::User,
            content_hash: Some(at_fs::sha256_hex(document)),
            normalized_hash: Some(normalized),
            mcp_summary: None,
            parse_status: ParseStatus::Parsed,
            store_link: false,
            ownership: Ownership::NotInAgenTuna,
            discovery: DiscoveryState::Valid,
            link_target: None,
            resolved: None,
        }
    }

    fn rule(&self, relative: &str, bytes: &[u8], agent: AgentId) -> ScanItem {
        let path = self.write(relative, bytes);
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("rule name")
            .to_owned();
        ScanItem {
            kind: ItemKind::Rule,
            name,
            agents: BTreeSet::from([agent]),
            path,
            scope: Scope::User,
            content_hash: Some(at_fs::sha256_hex(bytes)),
            normalized_hash: Some(at_fs::sha256_hex(bytes)),
            mcp_summary: None,
            parse_status: ParseStatus::Parsed,
            store_link: false,
            ownership: Ownership::NotInAgenTuna,
            discovery: DiscoveryState::Valid,
            link_target: None,
            resolved: None,
        }
    }

    fn mcp(&self, relative: &str, bytes: &[u8], name: &str, agent: AgentId) -> ScanItem {
        let path = self.write(relative, bytes);
        ScanItem {
            kind: ItemKind::McpServer,
            name: name.to_owned(),
            agents: BTreeSet::from([agent]),
            path,
            scope: Scope::User,
            content_hash: Some(at_fs::sha256_hex(bytes)),
            normalized_hash: Some(at_fs::sha256_hex(bytes)),
            mcp_summary: None,
            parse_status: ParseStatus::Parsed,
            store_link: false,
            ownership: Ownership::NotInAgenTuna,
            discovery: DiscoveryState::Valid,
            link_target: None,
            resolved: None,
        }
    }
}

/// Inputs for one plan permutation.
struct Case {
    items: Vec<ScanItem>,
    resolutions: BTreeMap<String, ConflictResolution>,
    selected: BTreeSet<PathBuf>,
    strategy: MigrationStrategy,
    copy_agents: BTreeSet<AgentId>,
}

impl Case {
    fn new(items: Vec<ScanItem>, strategy: MigrationStrategy) -> Self {
        let selected = items.iter().map(|item| item.path.clone()).collect();
        Self {
            items,
            resolutions: BTreeMap::new(),
            selected,
            strategy,
            copy_agents: BTreeSet::new(),
        }
    }

    fn select(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.selected = paths.into_iter().collect();
        self
    }

    fn resolve(mut self, name: &str, resolution: ConflictResolution) -> Self {
        self.resolutions.insert(name.to_owned(), resolution);
        self
    }

    fn copy_agents(mut self, agents: impl IntoIterator<Item = AgentId>) -> Self {
        self.copy_agents = agents.into_iter().collect();
        self
    }
}

fn plan_for(workspace: &Workspace, case: &Case) -> Result<MigrationPlan, PlanError> {
    let scan = ScanReport {
        roots: Vec::new(),
        items: case.items.clone(),
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts(&scan);
    build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &case.resolutions,
        selected_paths: &case.selected,
        store_root: workspace.store.clone(),
        copy_agents: &case.copy_agents,
        strategy: case.strategy,
        scope: Scope::User,
        backup_root: workspace.backup.clone(),
    })
}

fn rows_for(rows: &[DryRunRow], action: MigrationAction) -> usize {
    rows.iter().filter(|row| row.action == action).count()
}

/// Asserts the derived counts equal the per-action dry-run row counts.
fn assert_counts_match_rows(plan: &MigrationPlan) {
    let rows = dry_run(plan);
    assert_eq!(rows.len(), plan.steps.len(), "one row per step");
    assert_eq!(
        rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        (1..=rows.len()).collect::<Vec<_>>(),
        "rows are numbered in plan order"
    );
    let created_actions = [
        MigrationAction::CreateStoreItem,
        MigrationAction::RenderMcp,
        MigrationAction::RenderRule,
        MigrationAction::UpdateManifest,
        MigrationAction::WriteLink,
        MigrationAction::WriteCopy,
    ];
    let created = created_actions
        .iter()
        .map(|action| rows_for(&rows, action.clone()))
        .sum::<usize>();
    assert_eq!(
        plan.counts.files_created, created,
        "files_created equals created rows: {rows:#?}"
    );
    assert_eq!(
        plan.counts.files_backed_up,
        rows_for(&rows, MigrationAction::BackupOriginal),
        "files_backed_up equals backup rows"
    );
    assert_eq!(
        plan.counts.links_written,
        rows_for(&rows, MigrationAction::WriteLink),
        "links_written equals link rows"
    );
    let left_in_place = rows
        .iter()
        .filter(|row| row.action == MigrationAction::RecordWatched)
        .filter_map(|row| row.source.as_ref())
        .collect::<BTreeSet<_>>()
        .len();
    assert_eq!(
        plan.counts.files_left_in_place, left_in_place,
        "files_left_in_place equals unique watched sources"
    );
    assert_eq!(
        plan.counts.items,
        plan.steps
            .iter()
            .filter(|step| {
                matches!(
                    step.action,
                    MigrationAction::CreateStoreItem
                        | MigrationAction::RenderMcp
                        | MigrationAction::RenderRule
                )
            })
            .filter_map(|step| step.item_name.as_ref())
            .collect::<BTreeSet<_>>()
            .len(),
        "items equals distinct store item names"
    );
}

fn assert_counts(plan: &MigrationPlan, expected: PlanCounts) {
    assert_eq!(plan.counts, expected, "counts for {:?}", plan.steps);
    assert_counts_match_rows(plan);
}

fn mixed_workspace(workspace: &Workspace) -> Vec<ScanItem> {
    vec![
        workspace.skill(
            "native/skill/sample-skill",
            b"# Sample\n",
            AgentId::ClaudeCode,
        ),
        workspace.mcp(
            "native/config/mcp.json",
            br#"{"mcpServers":{"example":{"command":"node","args":["server.js"]}}}"#,
            "example",
            AgentId::ClaudeCode,
        ),
        workspace.rule(
            "native/rules/team-rules.md",
            b"# Team rules\n",
            AgentId::ClaudeCode,
        ),
    ]
}

#[test]
fn leave_and_watch_and_replace_counts_match_rows_for_every_selection() {
    let workspace = Workspace::new();
    let items = mixed_workspace(&workspace);
    let skill = items[0].clone();
    let mcp = items[1].clone();
    let rule = items[2].clone();

    // A single skill, both strategies.
    let plan = plan_for(
        &workspace,
        &Case::new(vec![skill.clone()], MigrationStrategy::LeaveAndWatch),
    )
    .expect("leave-and-watch skill plan");
    assert_counts(
        &plan,
        PlanCounts {
            items: 1,
            files_created: 2,
            files_backed_up: 0,
            links_written: 0,
            files_left_in_place: 1,
        },
    );
    validate_plan(&plan).expect("leave-and-watch plan validates");

    let plan = plan_for(
        &workspace,
        &Case::new(vec![skill.clone()], MigrationStrategy::ReplaceWithLinks),
    )
    .expect("replacement skill plan");
    assert_counts(
        &plan,
        PlanCounts {
            items: 1,
            files_created: 3,
            files_backed_up: 1,
            links_written: 1,
            files_left_in_place: 0,
        },
    );
    assert!(
        plan.steps
            .iter()
            .any(|step| step.action == MigrationAction::WriteLink)
    );

    // The same skill in copy mode writes a manifest-bearing copy instead of a link.
    let plan = plan_for(
        &workspace,
        &Case::new(vec![skill.clone()], MigrationStrategy::ReplaceWithLinks)
            .copy_agents([AgentId::ClaudeCode]),
    )
    .expect("copy-mode replacement plan");
    assert_counts(
        &plan,
        PlanCounts {
            items: 1,
            files_created: 3,
            files_backed_up: 1,
            links_written: 0,
            files_left_in_place: 0,
        },
    );
    assert!(
        plan.steps
            .iter()
            .any(|step| step.action == MigrationAction::WriteCopy)
    );

    // Mixed skill + MCP + rule, both strategies.
    let plan = plan_for(
        &workspace,
        &Case::new(items.clone(), MigrationStrategy::LeaveAndWatch),
    )
    .expect("mixed leave-and-watch plan");
    assert_counts(
        &plan,
        PlanCounts {
            items: 3,
            files_created: 4,
            files_backed_up: 0,
            links_written: 0,
            // One watched deployment per native source: the skill folder, the MCP config and
            // the rule file.
            files_left_in_place: 3,
        },
    );
    assert!(plan.steps.iter().any(|step| {
        step.action == MigrationAction::RecordWatched
            && step.item_name.as_deref() == Some("example")
    }));
    assert!(plan.steps.iter().any(|step| {
        step.action == MigrationAction::RecordWatched
            && step.item_name.as_deref() == Some("team-rules")
    }));
    assert!(plan.steps.iter().any(|step| {
        step.action == MigrationAction::RenderMcp && step.item_name.as_deref() == Some("example")
    }));
    assert!(plan.steps.iter().any(|step| {
        step.action == MigrationAction::RenderRule
            && step.item_name.as_deref() == Some("team-rules")
    }));

    let plan = plan_for(
        &workspace,
        &Case::new(items, MigrationStrategy::ReplaceWithLinks),
    )
    .expect("mixed replacement plan");
    assert_counts(
        &plan,
        PlanCounts {
            items: 3,
            files_created: 5,
            files_backed_up: 1,
            links_written: 1,
            files_left_in_place: 0,
        },
    );

    // Only the skill can be replaced: MCP and rule sources stay watched.
    let plan = plan_for(
        &workspace,
        &Case::new(vec![mcp, rule], MigrationStrategy::ReplaceWithLinks),
    )
    .expect("non-skill replacement plan");
    assert_counts(
        &plan,
        PlanCounts {
            items: 2,
            files_created: 3,
            files_backed_up: 0,
            links_written: 0,
            files_left_in_place: 0,
        },
    );
}

#[test]
fn conflict_resolution_permutations_produce_one_item_per_selected_copy() {
    let workspace = Workspace::new();
    let first = workspace.rule(
        "native/claude/rules.md",
        b"# Rules\none\n",
        AgentId::ClaudeCode,
    );
    let second = workspace.rule("native/codex/rules.md", b"# Rules\ntwo\n", AgentId::Codex);
    let name = "rules";

    for (resolution, expected_items, expected_created) in [
        (ConflictResolution::KeepA, 1usize, 2usize),
        (ConflictResolution::KeepB, 1, 2),
        (
            ConflictResolution::KeepBoth {
                names: vec!["rules-claude-code".to_owned(), "rules-2".to_owned()],
            },
            2,
            3,
        ),
    ] {
        for strategy in [
            MigrationStrategy::LeaveAndWatch,
            MigrationStrategy::ReplaceWithLinks,
        ] {
            let case = Case::new(vec![first.clone(), second.clone()], strategy)
                .resolve(name, resolution.clone());
            let plan = plan_for(&workspace, &case).expect("resolved conflict plan");
            // Leave-and-watch records one watched deployment per migrated rule copy; replacing
            // is only possible for skills, so rule items are never linked.
            let expected_watched = if strategy == MigrationStrategy::LeaveAndWatch {
                expected_items
            } else {
                0
            };
            assert_counts(
                &plan,
                PlanCounts {
                    items: expected_items,
                    files_created: expected_created,
                    files_backed_up: 0,
                    links_written: 0,
                    files_left_in_place: expected_watched,
                },
            );
            validate_plan(&plan).expect("resolved plan validates");
            let store_names = plan
                .steps
                .iter()
                .filter(|step| step.action == MigrationAction::RenderRule)
                .filter_map(|step| step.item_name.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(store_names.len(), expected_items);
            if let ConflictResolution::KeepBoth { names } = &resolution {
                assert_eq!(
                    store_names,
                    names.iter().cloned().collect::<BTreeSet<_>>(),
                    "keep-both names are used verbatim"
                );
            }
        }
    }
}

#[test]
fn unresolved_invalid_and_empty_selections_are_refused() {
    let workspace = Workspace::new();
    let first = workspace.rule(
        "native/claude/rules.md",
        b"# Rules\none\n",
        AgentId::ClaudeCode,
    );
    let second = workspace.rule("native/codex/rules.md", b"# Rules\ntwo\n", AgentId::Codex);

    let case = Case::new(
        vec![first.clone(), second.clone()],
        MigrationStrategy::LeaveAndWatch,
    );
    assert!(matches!(
        plan_for(&workspace, &case),
        Err(PlanError::UnresolvedConflict { .. })
    ));

    let case = Case::new(
        vec![first.clone(), second.clone()],
        MigrationStrategy::LeaveAndWatch,
    )
    .resolve(
        "rules",
        ConflictResolution::KeepBoth {
            names: vec!["only-one".to_owned()],
        },
    );
    assert!(matches!(
        plan_for(&workspace, &case),
        Err(PlanError::InvalidKeepBoth { .. })
    ));

    let case = Case::new(vec![first], MigrationStrategy::LeaveAndWatch).select(Vec::new());
    assert!(matches!(
        plan_for(&workspace, &case),
        Err(PlanError::EmptySelection)
    ));

    // A path outside the scan contributes no item, so the selection is effectively empty.
    let case = Case::new(Vec::new(), MigrationStrategy::LeaveAndWatch)
        .select([workspace.path("native/unknown")]);
    assert!(matches!(
        plan_for(&workspace, &case),
        Err(PlanError::EmptySelection)
    ));
}

#[test]
fn plan_hash_and_serialization_are_stable_across_rebuilds() {
    let workspace = Workspace::new();
    let items = mixed_workspace(&workspace);
    let first = plan_for(
        &workspace,
        &Case::new(items.clone(), MigrationStrategy::LeaveAndWatch),
    )
    .expect("first plan");
    let second = plan_for(
        &workspace,
        &Case::new(items, MigrationStrategy::LeaveAndWatch),
    )
    .expect("second plan");
    assert_eq!(
        first.plan_hash, second.plan_hash,
        "the reviewed payload hash is deterministic"
    );
    assert_eq!(first.steps, second.steps);
    assert_eq!(first.counts, second.counts);
    assert_ne!(first.id, second.id, "plan identity is fresh per build");

    // Changing the strategy changes the hash even when the item set is identical.
    let replaced = plan_for(
        &workspace,
        &Case::new(
            vec![workspace.skill(
                "native/skill/sample-skill",
                b"# Sample\n",
                AgentId::ClaudeCode,
            )],
            MigrationStrategy::ReplaceWithLinks,
        ),
    )
    .expect("replacement plan");
    assert_ne!(replaced.plan_hash, first.plan_hash);
}

#[test]
fn store_destinations_are_kind_specific_and_inside_the_store() {
    let workspace = Workspace::new();
    let items = mixed_workspace(&workspace);
    let plan = plan_for(
        &workspace,
        &Case::new(items, MigrationStrategy::LeaveAndWatch),
    )
    .expect("mixed plan");
    let destinations = plan
        .steps
        .iter()
        .filter_map(|step| step.destination.clone())
        .collect::<BTreeSet<PathBuf>>();
    assert!(destinations.contains(&workspace.store.join("skills/sample-skill")));
    assert!(destinations.contains(&workspace.store.join("mcp.json")));
    assert!(destinations.contains(&workspace.store.join("rules/team-rules.md")));
    assert!(destinations.contains(&workspace.store.join("store.json")));
    for destination in destinations {
        assert!(
            destination.starts_with(&workspace.store),
            "{} escapes the store",
            destination.display()
        );
    }
    assert_counts_match_rows(&plan);
}

#[test]
fn plan_keeps_backup_paths_for_replaced_originals_only() {
    let workspace = Workspace::new();
    let skill = workspace.skill(
        "native/skill/sample-skill",
        b"# Sample\n",
        AgentId::ClaudeCode,
    );
    let plan = plan_for(
        &workspace,
        &Case::new(vec![skill], MigrationStrategy::ReplaceWithLinks),
    )
    .expect("replacement plan");
    let rows = dry_run(&plan);
    let backups = rows
        .iter()
        .filter(|row| row.action == MigrationAction::BackupOriginal)
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        backups[0].backup_path,
        Some(workspace.backup.join("sample-skill"))
    );
    assert!(
        rows.iter()
            .filter(|row| row.action != MigrationAction::BackupOriginal)
            .all(|row| row.backup_path.is_none())
    );
    assert_counts_match_rows(&plan);
}

/// A native copy that differs from the canonical store item is a conflict group whose second
/// copy is the store, so overwriting reviewed store content needs an explicit decision.
#[test]
fn a_store_copy_conflict_requires_a_choice_before_the_store_is_overwritten() {
    let workspace = Workspace::new();
    let mut native = workspace.rule(
        "native/claude/rules.md",
        b"# Rules\nnative\n",
        AgentId::ClaudeCode,
    );
    native.ownership = Ownership::Conflict;
    let store_path = workspace.store.join("rules/rules.md");
    fs::create_dir_all(store_path.parent().expect("store rules dir")).expect("store rules dir");
    fs::write(&store_path, b"# Rules\nstore\n").expect("store rule");
    let store_items = vec![StoreItem {
        kind: ItemKind::Rule,
        name: "rules".to_owned(),
        content_hash: at_migrate::normalize::rule_bytes(b"# Rules\nstore\n").to_string(),
        scope: Scope::User,
        path: store_path.clone(),
    }];
    let scan = ScanReport {
        roots: Vec::new(),
        items: vec![native.clone()],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts_with_store(&scan, &store_items);
    assert_eq!(conflicts.conflicts.len(), 1, "the store copy is compared");
    assert_eq!(
        conflicts.conflicts[0].copies[1].origin,
        at_migrate::conflicts::CopyOrigin::Store
    );
    assert_eq!(conflicts.conflicts[0].copies[1].path, store_path);

    let plan = |resolution: Option<ConflictResolution>| {
        let resolutions = resolution
            .map(|resolution| BTreeMap::from([("rules".to_owned(), resolution)]))
            .unwrap_or_default();
        build_plan(BuildPlanRequest {
            scan: &scan,
            conflicts: &conflicts,
            resolutions: &resolutions,
            selected_paths: &BTreeSet::from([native.path.clone()]),
            store_root: workspace.store.clone(),
            copy_agents: &BTreeSet::new(),
            strategy: MigrationStrategy::LeaveAndWatch,
            scope: Scope::User,
            backup_root: workspace.backup.clone(),
        })
    };

    assert!(matches!(
        plan(None),
        Err(PlanError::UnresolvedConflict { .. })
    ));
    let kept_native = plan(Some(ConflictResolution::KeepA)).expect("keeping the native copy");
    let rendered = kept_native
        .steps
        .iter()
        .find(|step| step.action == MigrationAction::RenderRule)
        .expect("a rendered rule step");
    assert_eq!(rendered.item_name.as_deref(), Some("rules"));
    assert_eq!(rendered.source.as_deref(), Some(native.path.as_path()));
    validate_plan(&kept_native).expect("keeping the native copy validates");

    // Keeping the store copy migrates nothing, so no applicable plan exists for that choice.
    assert!(matches!(
        plan(Some(ConflictResolution::KeepB)),
        Err(PlanError::EmptySelection)
    ));

    let renamed = plan(Some(ConflictResolution::KeepBoth {
        names: vec!["rules-native".to_owned(), "rules-store".to_owned()],
    }))
    .expect("keeping both copies");
    let rendered = renamed
        .steps
        .iter()
        .find(|step| step.action == MigrationAction::RenderRule)
        .expect("a rendered rule step");
    assert_eq!(rendered.item_name.as_deref(), Some("rules-native"));
}
