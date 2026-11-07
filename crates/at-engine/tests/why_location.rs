//! TA-070: store definitions report the source line they were parsed from.

use std::fs;
use std::path::Path;

use at_engine::why::store_location;
use at_model::{Definition, ItemId, ItemKind};
use tempfile::TempDir;

const SKILL_DOC: &str = "---\nname: reviewer\ndescription: Reviews changes\n---\n\n# Reviewer\n";

fn definition(kind: ItemKind, path: &Path) -> Definition {
    Definition {
        kind,
        name: "Reviewer".to_owned(),
        declared_id: None,
        item_id: Some(ItemId::new()),
        path: path.to_path_buf(),
    }
}

#[test]
fn a_store_skill_reports_its_frontmatter_name_line_and_span() {
    let temp = TempDir::new().expect("sandbox");
    let path = temp.path().join("reviewer/SKILL.md");
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, SKILL_DOC).expect("write");

    let location = store_location(&definition(ItemKind::Skill, &path), None);
    assert_eq!(location.path, path);
    assert_eq!(
        location.line,
        Some(2),
        "the name entry is on the second line"
    );
    let span = location.byte_span.expect("name byte span");
    assert_eq!(&SKILL_DOC[span], "name: reviewer");
    assert_eq!(location.rendering_path, None);
}

#[test]
fn rules_report_line_one_and_opaque_or_unreadable_files_report_no_line() {
    let temp = TempDir::new().expect("sandbox");
    let rule = temp.path().join("AGENTS.md");
    fs::write(&rule, "# Guidance\n").expect("write");
    let location = store_location(&definition(ItemKind::Rule, &rule), None);
    assert_eq!(location.line, Some(1), "a rule file is its own definition");
    assert_eq!(location.byte_span, None);

    let missing = store_location(
        &definition(ItemKind::Skill, &temp.path().join("missing/SKILL.md")),
        None,
    );
    assert_eq!(missing.line, None, "an unreadable skill reports no line");
    assert_eq!(missing.byte_span, None);

    let mcp = temp.path().join("mcp.json");
    fs::write(&mcp, "{\"servers\":{}}").expect("write");
    let location = store_location(&definition(ItemKind::McpServer, &mcp), None);
    assert_eq!(
        location.line, None,
        "the shared MCP document has no tracked entry span"
    );
    assert_eq!(location.path, mcp);
}
