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
