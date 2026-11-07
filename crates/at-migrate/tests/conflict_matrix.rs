//! TC-034: matrix coverage for native conflict normalization.
//!
//! Every fixture is classified through the same public normalization helpers the scanner uses
//! ([`at_migrate::normalize`]), so a change to what "the same item" means fails here first.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use at_migrate::conflicts::{
    CopyOrigin, ProposedResolution, detect_conflicts, detect_conflicts_with_store, keep_both_names,
    occupied_names,
};
use at_migrate::normalize;
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport, StoreItem};
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
        roots: Vec::new(),
        items: vec![
            item(copies[0].clone(), AgentId::ClaudeCode),
            item(copies[1].clone(), AgentId::Codex),
        ],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
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
        roots: Vec::new(),
        items: vec![
            item(one.clone(), AgentId::ClaudeCode),
            item(two, AgentId::Codex),
        ],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let set = detect_conflicts(&report);
    assert!(set.conflicts.is_empty(), "identical copies need no review");
    assert_eq!(set.equal_groups.len(), 1, "one equal-hash group");
    assert_eq!(set.equal_groups[0].copies.len(), 2);
    assert_eq!(
        set.equal_groups[0]
            .copies
            .iter()
            .filter_map(|copy| copy.agent.as_ref())
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([AgentId::ClaudeCode, AgentId::Codex]),
        "both agents collapse into the one group"
    );

    let single = ScanReport {
        roots: Vec::new(),
        items: vec![item(one, AgentId::ClaudeCode)],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let set = detect_conflicts(&single);
    assert!(set.conflicts.is_empty());
    assert!(set.equal_groups.is_empty());
}

#[test]
fn a_store_env_value_change_is_a_real_conflict_with_a_redacted_diff() {
    let temp = TempDir::new().expect("fixture root");
    let secret = "sk-live-3f4a9b1c";
    let reference = serde_json::json!({
        "command": "server",
        "args": ["--x"],
        "env": {"TOKEN": "${TOKEN}"}
    });
    let literal = serde_json::json!({
        "command": "server",
        "args": ["--x"],
        "env": {"TOKEN": secret}
    });
    let store_path = temp.path().join("store/mcp.json");
    let store_items = vec![StoreItem {
        kind: ItemKind::McpServer,
        name: "example".to_owned(),
        content_hash: normalize::mcp_entry(&reference)
            .expect("canonical store entry")
            .to_string(),
        scope: Scope::User,
        path: store_path.clone(),
    }];

    // One native copy holding a literal value against a store copy holding the reference is a
    // real conflict, never a formatting-only difference.
    let mut native = item(mcp(temp.path(), "native", &literal), AgentId::ClaudeCode);
    native.ownership = Ownership::Conflict;
    let report = ScanReport {
        roots: Vec::new(),
        items: vec![native],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let set = detect_conflicts_with_store(&report, &store_items);
    assert_eq!(set.conflicts.len(), 1, "{set:#?}");
    let conflict = &set.conflicts[0];
    assert!(!conflict.formatting_only);
    assert_eq!(
        conflict.proposed_resolution,
        ProposedResolution::NeedsChoice
    );
    assert_eq!(conflict.copies.len(), 2, "copy A native, copy B store");
    assert_eq!(conflict.copies[0].origin, CopyOrigin::Native);
    assert_eq!(conflict.copies[1].origin, CopyOrigin::Store);
    assert_eq!(conflict.copies[1].agent, None);
    assert_eq!(conflict.copies[1].path, store_path);
    assert!(
        !conflict.unified_diff.contains(secret),
        "the diff must not render the secret: {}",
        conflict.unified_diff
    );

    // A matching `${VAR}` reference in the native copy is formatting-equivalent to the store
    // copy, so the pair collapses instead of waiting for a decision.
    let mut equivalent = item(
        mcp(temp.path(), "equivalent", &reference),
        AgentId::ClaudeCode,
    );
    equivalent.ownership = Ownership::Managed;
    let report = ScanReport {
        roots: Vec::new(),
        items: vec![equivalent],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    assert!(
        detect_conflicts_with_store(&report, &store_items)
            .conflicts
            .is_empty()
    );
}

#[test]
fn keep_both_names_avoid_the_store_and_every_agent_folder() {
    let temp = TempDir::new().expect("fixture root");
    let mut items = Vec::new();
    for (suffix, agent) in [
        ("rules", AgentId::ClaudeCode),
        ("rules-claude-code", AgentId::Codex),
        ("rules-2", AgentId::Omp),
    ] {
        let copy = rule(temp.path(), suffix, rule_bytes_for(suffix));
        let mut row = item(copy, agent);
        row.name = suffix.to_owned();
        items.push(row);
    }
    let scan = ScanReport {
        roots: Vec::new(),
        items,
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let store_items = vec![StoreItem {
        kind: ItemKind::Rule,
        name: "rules-3".to_owned(),
        content_hash: String::new(),
        scope: Scope::User,
        path: temp.path().join("store/rules/rules-3.md"),
    }];

    let occupied = occupied_names(&scan, &store_items, ItemKind::Rule);
    assert_eq!(
        occupied,
        BTreeSet::from([
            "rules".to_owned(),
            "rules-claude-code".to_owned(),
            "rules-2".to_owned(),
            "rules-3".to_owned(),
        ])
    );
    assert_eq!(
        keep_both_names("rules", &AgentId::ClaudeCode, &occupied),
        Some(("rules-claude-code-2".to_owned(), "rules-4".to_owned())),
        "the agent name and both numeric names are already taken"
    );
    assert_eq!(
        keep_both_names("bad/name", &AgentId::ClaudeCode, &BTreeSet::new()),
        None,
        "no valid name exists, so the resolution is blocked"
    );
}

/// Distinct bytes for a fixture file so each copy has its own hash.
fn rule_bytes_for(label: &str) -> &'static [u8] {
    match label {
        "rules" => b"# Rules\nbase\n",
        "rules-claude-code" => b"# Rules\nclaude\n",
        _ => b"# Rules\nother\n",
    }
}
