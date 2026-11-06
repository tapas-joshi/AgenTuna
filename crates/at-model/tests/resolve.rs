//! Name normalization and logical identity shared with the add-skill clash check.

use std::path::PathBuf;

use at_model::{Definition, ItemId, ItemKind, LogicalKey, normalized_name};

fn definition(kind: ItemKind, name: &str, declared_id: Option<&str>) -> Definition {
    Definition {
        kind,
        name: name.to_owned(),
        declared_id: declared_id.map(str::to_owned),
        item_id: None,
        path: PathBuf::from("/fixture/SKILL.md"),
    }
}

#[test]
fn trims_surrounding_whitespace() {
    assert_eq!(normalized_name("  Code-Review \n\t"), "code-review");
    assert_eq!(normalized_name("   "), "");
}

#[test]
fn compares_case_insensitively() {
    assert_eq!(
        normalized_name("CODE-Review"),
        normalized_name("code-REVIEW")
    );
    assert_eq!(normalized_name("Straße"), normalized_name("straße"));
    assert_ne!(normalized_name("ss"), normalized_name("ß"));
}

#[test]
fn composes_canonically_equivalent_unicode() {
    let composed = normalized_name("Caf\u{00e9}");
    let decomposed = normalized_name("Cafe\u{0301}");
    assert_eq!(composed, decomposed);
    assert_eq!(composed, "caf\u{00e9}");
}

#[test]
fn keeps_genuinely_different_names_apart() {
    assert_ne!(
        normalized_name("code-review"),
        normalized_name("code_review")
    );
    assert_ne!(normalized_name("review"), normalized_name("reviewer"));
    assert_ne!(normalized_name(""), normalized_name("review"));
}

#[test]
fn skills_keep_authored_spelling_and_share_one_identity() {
    let user = definition(ItemKind::Skill, "Code-Review", None);
    let project = definition(ItemKind::Skill, " code-review ", None);

    assert_eq!(user.name, "Code-Review");
    assert_eq!(user.normalized_name(), "code-review");
    assert_eq!(user.logical_id(), project.logical_id());
    assert_eq!(
        user.logical_id().key,
        LogicalKey::Name("code-review".to_owned())
    );
}

#[test]
fn mcp_identity_prefers_the_declared_id() {
    let declared = definition(ItemKind::McpServer, "Files", Some("files"));
    assert_eq!(
        declared.logical_id().key,
        LogicalKey::Declared("files".to_owned())
    );
    assert!(!declared.logical_id().is_mcp_name_fallback());

    // A declared id is an identifier: case and whitespace are not normalized away.
    let shouted = definition(ItemKind::McpServer, "Files", Some("Files"));
    assert_ne!(declared.logical_id(), shouted.logical_id());

    let fallback = definition(ItemKind::McpServer, " Files ", None);
    assert_eq!(
        fallback.logical_id().key,
        LogicalKey::Name("files".to_owned())
    );
    assert!(fallback.logical_id().is_mcp_name_fallback());
    assert!(
        !definition(ItemKind::Skill, "files", None)
            .logical_id()
            .is_mcp_name_fallback()
    );
}

#[test]
fn the_same_name_in_different_kinds_is_a_different_identity() {
    let skill = definition(ItemKind::Skill, "review", None);
    let mcp = definition(ItemKind::McpServer, "review", Some("review"));
    assert_ne!(skill.logical_id(), mcp.logical_id());
    assert_ne!(skill.logical_id().kind, mcp.logical_id().kind);
}

#[test]
fn item_id_is_carried_but_never_merged_on() {
    let mut user = definition(ItemKind::Skill, "review", None);
    user.item_id = Some(ItemId::new());
    let mut project = definition(ItemKind::Skill, "Review", None);
    project.item_id = Some(ItemId::new());

    assert_ne!(user.item_id, project.item_id);
    assert_eq!(user.logical_id(), project.logical_id());
}
