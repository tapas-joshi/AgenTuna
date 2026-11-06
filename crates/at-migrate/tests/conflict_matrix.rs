//! TC-034: matrix coverage for native conflict normalization.
//!
//! Every fixture is classified through the same public normalization helpers the scanner uses
//! ([`at_migrate::normalize`]), so a change to what "the same item" means fails here first.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use at_migrate::conflicts::{ProposedResolution, detect_conflicts};
use at_migrate::normalize;
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};
use at_model::{AgentId, ItemKind, Scope};
use tempfile::TempDir;

#[derive(Clone)]
struct FixtureCopy {
    kind: ItemKind,
    name: &'static str,
    path: PathBuf,
    raw_hash: String,
    normalized_hash: String,
}

fn item(copy: FixtureCopy, agent: AgentId) -> ScanItem {
    ScanItem {
        kind: copy.kind,
        name: copy.name.to_owned(),
        agents: BTreeSet::from([agent]),
        path: copy.path,
        scope: Scope::User,
        content_hash: Some(copy.raw_hash),
        normalized_hash: Some(copy.normalized_hash),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    }
}

fn classify(copies: [FixtureCopy; 2]) -> (bool, bool, ProposedResolution) {
    let report = ScanReport {
        items: vec![
            item(copies[0].clone(), AgentId::ClaudeCode),
            item(copies[1].clone(), AgentId::Codex),
        ],
        partial: false,
        skipped_roots: Vec::new(),
    };
    let conflicts = detect_conflicts(&report);
    let conflict = conflicts.conflicts.first().expect("fixture conflict");
    (
        conflict.formatting_only,
        !conflict.unified_diff.is_empty(),
        conflict.proposed_resolution.clone(),
    )
}

/// Asserts the classification of one fixture pair.
fn expect(case: &str, left: FixtureCopy, right: FixtureCopy, formatting_only: bool) {
    let (formatting, has_diff, resolution) = classify([left, right]);
    assert_eq!(formatting, formatting_only, "{case}: formatting_only");
    assert_eq!(has_diff, !formatting_only, "{case}: diff presence");
    assert_eq!(
        resolution,
        if formatting_only {
            ProposedResolution::KeepA
        } else {
            ProposedResolution::NeedsChoice
        },
        "{case}: proposed resolution"
    );
}

fn skill(root: &Path, document: &[u8], asset: Option<&[u8]>) -> FixtureCopy {
    fs::create_dir_all(root).expect("skill folder");
    fs::write(root.join("SKILL.md"), document).expect("skill document");
    if let Some(asset) = asset {
        fs::write(root.join("asset.bin"), asset).expect("skill asset");
    }
    FixtureCopy {
        kind: ItemKind::Skill,
        name: "sample-skill",
        path: root.to_path_buf(),
        raw_hash: at_fs::sha256_hex(document),
        normalized_hash: normalize::skill_tree(root)
            .expect("normalized skill hash")
            .to_string(),
    }
}

fn rule(temp: &Path, suffix: &str, bytes: &[u8]) -> FixtureCopy {
    let path = temp.join(format!("rule-{suffix}.md"));
    fs::write(&path, bytes).expect("rule fixture");
    FixtureCopy {
        kind: ItemKind::Rule,
        name: "rules",
        path,
        raw_hash: at_fs::sha256_hex(bytes),
        normalized_hash: normalize::rule_bytes(bytes).to_string(),
    }
}

fn mcp(temp: &Path, suffix: &str, value: &serde_json::Value) -> FixtureCopy {
    let path = temp.join(format!("mcp-{suffix}.json"));
    let bytes = serde_json::to_vec(value).expect("json bytes");
    fs::write(&path, &bytes).expect("json fixture");
    FixtureCopy {
        kind: ItemKind::McpServer,
        name: "example",
        path,
        raw_hash: suffix.to_owned(),
        normalized_hash: normalize::mcp_entry(value)
            .expect("canonical MCP hash")
            .to_string(),
    }
}

#[test]
fn skill_normalization_fixtures_classify_formatting_and_content() {
    let temp = TempDir::new().expect("fixture root");
    // Frontmatter field order is formatting; the body is content.
    expect(
        "frontmatter key order",
        skill(
            &temp.path().join("fm-a"),
            b"---\nname: x\ndescription: y\n---\nBody\n",
            None,
        ),
        skill(
            &temp.path().join("fm-b"),
            b"---\ndescription: y\nname: x\n---\nBody\n",
            None,
        ),
        true,
    );
    // CRLF and trailing spaces in SKILL.md are formatting.
    expect(
        "CRLF and trailing spaces",
        skill(
            &temp.path().join("ws-a"),
            b"---\nname: x\n---\nBody  \n",
            None,
        ),
        skill(
            &temp.path().join("ws-b"),
            b"---\r\nname: x\r\n---\r\nBody\r\n",
            None,
        ),
        true,
    );
    // An asset difference is content even when SKILL.md is identical.
    expect(
        "asset-only change",
        skill(
            &temp.path().join("asset-a"),
            b"---\nname: x\n---\nBody\n",
            Some(b"one"),
        ),
        skill(
            &temp.path().join("asset-b"),
            b"---\nname: x\n---\nBody\n",
            Some(b"two"),
        ),
        false,
    );
    // A changed SKILL.md body is content.
    expect(
        "changed skill body",
        skill(
            &temp.path().join("body-a"),
            b"---\nname: x\n---\nBody one\n",
            None,
        ),
        skill(
            &temp.path().join("body-b"),
            b"---\nname: x\n---\nBody two\n",
            None,
        ),
        false,
    );
    // An added asset is content.
    expect(
        "added asset",
        skill(
            &temp.path().join("added-a"),
            b"---\nname: x\n---\nBody\n",
            None,
        ),
        skill(
            &temp.path().join("added-b"),
            b"---\nname: x\n---\nBody\n",
            Some(b"asset"),
        ),
        false,
    );
}

#[test]
fn rule_normalization_fixtures_classify_line_endings_and_content() {
    let temp = TempDir::new().expect("fixture root");
    expect(
        "rule line endings",
        rule(temp.path(), "lf", b"# Rule\ncontent\n"),
        rule(temp.path(), "crlf", b"# Rule\r\ncontent\r\n"),
        true,
    );
    expect(
        "rule trailing spaces",
        rule(temp.path(), "spaces", b"# Rule  \ncontent\t\n"),
        rule(temp.path(), "plain", b"# Rule\ncontent\n"),
        true,
    );
    expect(
        "rule trailing blank lines",
        rule(temp.path(), "blank", b"# Rule\ncontent\n\n\n"),
        rule(temp.path(), "single", b"# Rule\ncontent\n"),
        true,
    );
    expect(
        "changed rule content",
        rule(temp.path(), "one", b"# Rule\none\n"),
        rule(temp.path(), "two", b"# Rule\ntwo\n"),
        false,
    );
    expect(
        "lone carriage return is content",
        rule(temp.path(), "cr", b"# Rule\rone\n"),
        rule(temp.path(), "lf-two", b"# Rule\none\n"),
        false,
    );
}

#[test]
fn mcp_normalization_fixtures_classify_key_order_and_variable_references() {
    let temp = TempDir::new().expect("fixture root");
    let key_order_a =
        serde_json::json!({"command":"server", "args":["--x"], "env":{"TOKEN":"${TOKEN}"}});
    let key_order_b =
        serde_json::json!({"env":{"TOKEN":"${TOKEN}"}, "args":["--x"], "command":"server"});
    let env_changed =
        serde_json::json!({"command":"server", "args":["--x"], "env":{"TOKEN":"${OTHER}"}});
    let args_changed =
        serde_json::json!({"command":"server", "args":["--y"], "env":{"TOKEN":"${TOKEN}"}});

    expect(
        "MCP key order",
        mcp(temp.path(), "a", &key_order_a),
        mcp(temp.path(), "b", &key_order_b),
        true,
    );
    expect(
        "equivalent variable reference",
        mcp(temp.path(), "c", &key_order_a),
        mcp(temp.path(), "d", &key_order_b),
        true,
    );
    expect(
        "changed variable reference",
        mcp(temp.path(), "e", &key_order_a),
        mcp(temp.path(), "f", &env_changed),
        false,
    );
    expect(
        "changed argument",
        mcp(temp.path(), "g", &key_order_a),
        mcp(temp.path(), "h", &args_changed),
        false,
    );
}

#[test]
fn identical_copies_and_single_copies_are_not_conflicts() {
    let temp = TempDir::new().expect("fixture root");
    let one = rule(temp.path(), "same-a", b"# Rule\ncontent\n");
    let two = rule(temp.path(), "same-b", b"# Rule\ncontent\n");
    let report = ScanReport {
        items: vec![
            item(one.clone(), AgentId::ClaudeCode),
            item(two, AgentId::Codex),
        ],
        partial: false,
        skipped_roots: Vec::new(),
    };
    assert!(detect_conflicts(&report).conflicts.is_empty());

    let single = ScanReport {
        items: vec![item(one, AgentId::ClaudeCode)],
        partial: false,
        skipped_roots: Vec::new(),
    };
    assert!(detect_conflicts(&single).conflicts.is_empty());
}
