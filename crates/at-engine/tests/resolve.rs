//! Scope resolution: precedence, merge keys, shadowing and deterministic ordering.

use std::path::PathBuf;

use at_engine::resolve::{ResolveInput, ResolvedSet, resolve};
use at_model::{Definition, ItemId, ItemKind, LogicalKey, Scope, ScopeSnapshot};

fn definition(kind: ItemKind, name: &str, path: &str) -> Definition {
    Definition {
        kind,
        name: name.to_owned(),
        declared_id: None,
        item_id: None,
        path: PathBuf::from(path),
    }
}

fn skill(name: &str, path: &str) -> Definition {
    definition(ItemKind::Skill, name, path)
}

fn mcp(name: &str, declared_id: Option<&str>, path: &str) -> Definition {
    Definition {
        kind: ItemKind::McpServer,
        name: name.to_owned(),
        declared_id: declared_id.map(str::to_owned),
        item_id: None,
        path: PathBuf::from(path),
    }
}

fn layer(scope: Scope, definitions: Vec<Definition>) -> ScopeSnapshot {
    ScopeSnapshot { scope, definitions }
}

fn resolve_layers(layers: &[ScopeSnapshot]) -> ResolvedSet {
    resolve(ResolveInput::new(layers))
}

fn path(scope: Scope, name: &str) -> PathBuf {
    PathBuf::from(format!("{scope}/{name}/SKILL.md"))
}

fn skill_at(scope: Scope, name: &str) -> Definition {
    skill(name, &path(scope, name).display().to_string())
}

/// One winner per logical id for every non-empty subset of the four scopes, and the
/// loser list names exactly the other scopes, whatever order the layers arrive in.
#[test]
fn scope_matrix_yields_one_winner_and_one_shadowed_record_per_loser() {
    let all = [Scope::Global, Scope::User, Scope::Project, Scope::Local];
    for mask in 1u8..16 {
        let present: Vec<Scope> = (0..4usize)
            .filter(|bit| mask & (1 << bit) != 0)
            .map(|bit| all[bit])
            .collect();
        let layers: Vec<ScopeSnapshot> = present
            .iter()
            .map(|scope| layer(*scope, vec![skill_at(*scope, "review")]))
            .collect();

        let set = resolve_layers(&layers);
        let winner = present
            .iter()
            .copied()
            .max_by_key(|scope| scope.precedence())
            .unwrap();

        assert_eq!(set.items.len(), 1, "mask {mask:#06b} must have one winner");
        assert_eq!(set.items[0].scope, winner, "mask {mask:#06b}");
        assert_eq!(set.items[0].definition.path, path(winner, "review"));
        assert_eq!(set.shadowed.len(), present.len() - 1, "mask {mask:#06b}");

        let winner_path = path(winner, "review");
        let mut losers: Vec<PathBuf> = set
            .shadowed
            .iter()
            .map(|entry| entry.definition.path.clone())
            .collect();
        losers.sort();
        let mut expected: Vec<PathBuf> = present
            .iter()
            .filter(|scope| **scope != winner)
            .map(|scope| path(*scope, "review"))
            .collect();
        expected.sort();
        assert_eq!(losers, expected, "mask {mask:#06b}");
        for entry in &set.shadowed {
            assert_eq!(entry.shadowed_by.scope, winner, "mask {mask:#06b}");
            assert_eq!(entry.shadowed_by.path, winner_path, "mask {mask:#06b}");
        }

        let mut reversed = layers.clone();
        reversed.reverse();
        assert_eq!(resolve_layers(&reversed), set, "mask {mask:#06b} reversed");
    }
}

#[test]
fn empty_input_yields_an_empty_set() {
    let nothing = resolve_layers(&[]);
    assert!(nothing.is_empty());
    assert_eq!(nothing, ResolvedSet::default());

    let empty_layers = [
        layer(Scope::Global, vec![]),
        layer(Scope::User, vec![]),
        layer(Scope::Project, vec![]),
        layer(Scope::Local, vec![]),
    ];
    let set = resolve_layers(&empty_layers);
    assert!(set.items.is_empty());
    assert!(set.shadowed.is_empty());
}

#[test]
fn skill_names_merge_case_insensitively_and_project_wins() {
    let layers = [
        layer(
            Scope::User,
            vec![skill("Code-Review", "/user/review/SKILL.md")],
        ),
        layer(
            Scope::Project,
            vec![skill("code-review", "/project/review/SKILL.md")],
        ),
    ];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 1);
    let winner = &set.items[0];
    assert_eq!(winner.scope, Scope::Project);
    assert_eq!(
        winner.definition.name, "code-review",
        "winning spelling is kept"
    );
    assert_eq!(
        winner.logical_id.key,
        LogicalKey::Name("code-review".to_owned())
    );

    assert_eq!(set.shadowed.len(), 1);
    let loser = &set.shadowed[0];
    assert_eq!(loser.scope, Scope::User);
    assert_eq!(
        loser.definition.name, "Code-Review",
        "losing spelling is kept"
    );
    assert_eq!(
        loser.definition.path,
        PathBuf::from("/user/review/SKILL.md")
    );
    assert_eq!(loser.shadowed_by.scope, Scope::Project);
    assert_eq!(
        loser.shadowed_by.path,
        PathBuf::from("/project/review/SKILL.md")
    );
}

#[test]
fn normalization_trims_and_composes_before_merging() {
    let layers = [
        layer(
            Scope::User,
            vec![skill("  Code-R\u{00e9}view  ", "/user/review/SKILL.md")],
        ),
        layer(
            Scope::Project,
            vec![skill("code-re\u{0301}view", "/project/review/SKILL.md")],
        ),
        layer(
            Scope::Local,
            vec![skill("CODE-R\u{00c9}VIEW", "/local/review/SKILL.md")],
        ),
    ];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].scope, Scope::Local);
    assert_eq!(set.items[0].definition.name, "CODE-R\u{00c9}VIEW");
    assert_eq!(set.shadowed.len(), 2);
    assert!(
        set.shadowed
            .iter()
            .all(|entry| entry.shadowed_by.scope == Scope::Local)
    );
}

#[test]
fn mcp_servers_with_the_same_name_but_different_ids_stay_distinct() {
    let layers = [
        layer(
            Scope::User,
            vec![mcp("sentry", Some("sentry-a"), "/user/a.json")],
        ),
        layer(
            Scope::Project,
            vec![mcp("sentry", Some("sentry-b"), "/project/b.json")],
        ),
    ];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 2);
    assert!(set.shadowed.is_empty());
    assert_eq!(set.items[0].scope, Scope::User);
    assert_eq!(
        set.items[0].logical_id.key,
        LogicalKey::Declared("sentry-a".to_owned())
    );
    assert_eq!(set.items[1].scope, Scope::Project);
    assert_eq!(
        set.items[1].logical_id.key,
        LogicalKey::Declared("sentry-b".to_owned())
    );
}

#[test]
fn mcp_servers_with_the_same_id_merge_and_report_the_loser() {
    let layers = [
        layer(
            Scope::Global,
            vec![mcp("sentry", Some("sentry"), "/global/sentry.json")],
        ),
        layer(
            Scope::Local,
            vec![mcp("sentry-eu", Some("sentry"), "/local/sentry.json")],
        ),
    ];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].scope, Scope::Local);
    assert_eq!(set.items[0].definition.name, "sentry-eu");
    assert_eq!(set.shadowed.len(), 1);
    assert_eq!(set.shadowed[0].scope, Scope::Global);
    assert_eq!(
        set.shadowed[0].definition.path,
        PathBuf::from("/global/sentry.json")
    );
    assert_eq!(set.shadowed[0].shadowed_by.scope, Scope::Local);
    assert_eq!(
        set.shadowed[0].shadowed_by.path,
        PathBuf::from("/local/sentry.json")
    );
}

#[test]
fn mcp_without_a_declared_id_merges_by_name_and_records_the_fallback() {
    let layers = [
        layer(Scope::User, vec![mcp("Files", None, "/user/files.json")]),
        layer(
            Scope::Project,
            vec![mcp(" files ", None, "/project/files.json")],
        ),
    ];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 1);
    assert!(set.items[0].logical_id.is_mcp_name_fallback());
    assert_eq!(
        set.items[0].logical_id.key,
        LogicalKey::Name("files".to_owned())
    );
    assert_eq!(set.shadowed.len(), 1);

    let declared = [layer(
        Scope::User,
        vec![mcp("Files", Some("files"), "/user/files.json")],
    )];
    let set = resolve_layers(&declared);
    assert!(!set.items[0].logical_id.is_mcp_name_fallback());
    assert_eq!(
        set.items[0].logical_id.key,
        LogicalKey::Declared("files".to_owned())
    );
}

#[test]
fn same_name_of_different_kinds_never_merges() {
    let layers = [layer(
        Scope::User,
        vec![
            skill("review", "/user/skill"),
            mcp("review", Some("review"), "/user/mcp"),
        ],
    )];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 2);
    assert_eq!(set.items[0].logical_id.kind, ItemKind::Skill);
    assert_eq!(set.items[1].logical_id.kind, ItemKind::McpServer);
}

#[test]
fn ordering_is_kind_then_name_then_scope_depth() {
    let layers = [
        layer(
            Scope::Local,
            vec![mcp("alpha", Some("alpha-local"), "/local/alpha")],
        ),
        layer(
            Scope::Project,
            vec![
                mcp("alpha", Some("alpha-project"), "/project/alpha"),
                skill("zeta", "/project/zeta"),
            ],
        ),
        layer(
            Scope::User,
            vec![skill("alpha", "/user/alpha"), skill("Beta", "/user/beta")],
        ),
    ];
    let set = resolve_layers(&layers);

    let order: Vec<(ItemKind, &str, Scope)> = set
        .items
        .iter()
        .map(|item| {
            (
                item.definition.kind,
                item.definition.name.as_str(),
                item.scope,
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            (ItemKind::Skill, "alpha", Scope::User),
            (ItemKind::Skill, "Beta", Scope::User),
            (ItemKind::Skill, "zeta", Scope::Project),
            (ItemKind::McpServer, "alpha", Scope::Project),
            (ItemKind::McpServer, "alpha", Scope::Local),
        ]
    );
}

#[test]
fn removing_a_deeper_layer_restores_the_inherited_winner() {
    let global = layer(
        Scope::Global,
        vec![skill("review", "/global/review/SKILL.md")],
    );
    let project = layer(
        Scope::Project,
        vec![skill("review", "/project/review/SKILL.md")],
    );

    let both = resolve_layers(&[global.clone(), project]);
    assert_eq!(both.items.len(), 1);
    assert_eq!(both.items[0].scope, Scope::Project);
    assert_eq!(both.shadowed.len(), 1);
    assert_eq!(
        both.shadowed[0].definition.path,
        PathBuf::from("/global/review/SKILL.md")
    );

    let inherited = resolve_layers(&[global]);
    assert_eq!(inherited.items.len(), 1);
    assert_eq!(inherited.items[0].scope, Scope::Global);
    assert!(inherited.shadowed.is_empty());
}

#[test]
fn duplicate_definitions_in_one_scope_shadow_deterministically() {
    let layers = [
        layer(
            Scope::Project,
            vec![skill("review", "/project/first/SKILL.md")],
        ),
        layer(
            Scope::Project,
            vec![skill("Review", "/project/second/SKILL.md")],
        ),
    ];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 1);
    assert_eq!(
        set.items[0].definition.path,
        PathBuf::from("/project/first/SKILL.md")
    );
    assert_eq!(set.shadowed.len(), 1);
    assert_eq!(set.shadowed[0].scope, Scope::Project);
    assert_eq!(
        set.shadowed[0].definition.path,
        PathBuf::from("/project/second/SKILL.md")
    );
    assert_eq!(
        set.shadowed[0].shadowed_by.path,
        PathBuf::from("/project/first/SKILL.md")
    );
}

#[test]
fn managed_winner_item_id_reaches_shadowed_by() {
    let id = ItemId::new();
    let mut managed = skill("review", "/user/review/SKILL.md");
    managed.item_id = Some(id);
    let layers = [
        layer(
            Scope::Global,
            vec![skill("review", "/global/review/SKILL.md")],
        ),
        layer(Scope::User, vec![managed]),
    ];
    let set = resolve_layers(&layers);

    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].scope, Scope::User);
    assert_eq!(set.shadowed.len(), 1);
    assert_eq!(set.shadowed[0].scope, Scope::Global);
    assert_eq!(set.shadowed[0].shadowed_by.item_id, Some(id));
    assert_eq!(set.get(&set.items[0].logical_id), Some(&set.items[0]));
}

#[test]
fn adding_a_deeper_layer_leaves_other_logical_ids_untouched() {
    let review = skill("review", "/global/review/SKILL.md");
    let deploy = skill("deploy", "/global/deploy/SKILL.md");
    let base = [layer(Scope::Global, vec![review.clone(), deploy.clone()])];
    let extended = [
        layer(Scope::Global, vec![review.clone(), deploy.clone()]),
        layer(
            Scope::Local,
            vec![skill("review", "/local/review/SKILL.md")],
        ),
    ];

    let before = resolve_layers(&base);
    let after = resolve_layers(&extended);

    assert_eq!(after.items.len(), 2);
    assert_eq!(before.items.len(), 2);
    assert_eq!(
        after.get(&deploy.logical_id()),
        before.get(&deploy.logical_id())
    );
    assert_eq!(
        before.get(&review.logical_id()).map(|item| item.scope),
        Some(Scope::Global)
    );
    assert_eq!(
        after.get(&review.logical_id()).map(|item| item.scope),
        Some(Scope::Local)
    );
    assert_eq!(after.shadowed.len(), 1);
    assert_eq!(after.shadowed[0].logical_id, review.logical_id());
    assert_eq!(after.shadowed[0].scope, Scope::Global);
}
