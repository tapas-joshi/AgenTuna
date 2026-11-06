//! Matrix coverage for native conflict normalization.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use at_migrate::conflicts::{ProposedResolution, detect_conflicts};
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
        normalized_hash: at_store::manifest::hash_skill_folder(root)
            .expect("normalized skill hash")
            .to_string(),
    }
}

#[test]
fn normalization_fixture_matrix_classifies_formatting_and_content() {
    let temp = TempDir::new().expect("fixture root");
    let cases = [
        (
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
        ),
        (
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
        ),
        (
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
        ),
    ];
    for (fixture_name, left, right, formatting_only) in cases {
        let (formatting, has_diff, resolution) = classify([left, right]);
        assert_eq!(formatting, formatting_only, "{fixture_name}");
        assert_eq!(has_diff, !formatting_only, "{fixture_name}");
        assert_eq!(
            resolution,
            if formatting_only {
                ProposedResolution::KeepA
            } else {
                ProposedResolution::NeedsChoice
            },
            "{fixture_name}"
        );
    }

    let key_order_a =
        serde_json::json!({"command":"server", "args":["--x"], "env":{"TOKEN":"${TOKEN}"}});
    let key_order_b =
        serde_json::json!({"env":{"TOKEN":"${TOKEN}"}, "args":["--x"], "command":"server"});
    let env_changed =
        serde_json::json!({"command":"server", "args":["--x"], "env":{"TOKEN":"${OTHER}"}});
    let mcp_pair = |value: &serde_json::Value, suffix: &str| {
        let path = temp.path().join(format!("mcp-{suffix}.json"));
        let bytes = serde_json::to_vec(value).expect("json bytes");
        fs::write(&path, &bytes).expect("json fixture");
        FixtureCopy {
            kind: ItemKind::McpServer,
            name: "example",
            path,
            raw_hash: suffix.to_owned(),
            normalized_hash: at_store::manifest::hash_mcp_entry(value)
                .expect("canonical MCP hash")
                .to_string(),
        }
    };
    for (fixture_name, right, formatting_only) in [
        ("MCP key order", &key_order_b, true),
        ("equivalent variable reference", &key_order_a, true),
        ("changed variable reference", &env_changed, false),
    ] {
        let (formatting, has_diff, _) =
            classify([mcp_pair(&key_order_a, "a"), mcp_pair(right, "b")]);
        assert_eq!(formatting, formatting_only, "{fixture_name}");
        assert_eq!(has_diff, !formatting_only, "{fixture_name}");
    }

    let rule = |suffix: &str, bytes: &[u8]| {
        let path = temp.path().join(format!("rule-{suffix}.md"));
        fs::write(&path, bytes).expect("rule fixture");
        FixtureCopy {
            kind: ItemKind::Rule,
            name: "rules",
            path,
            raw_hash: at_fs::sha256_hex(bytes),
            normalized_hash: at_store::manifest::hash_rule_bytes(bytes).to_string(),
        }
    };
    let (formatting, has_diff, _) = classify([
        rule("lf", b"# Rule\ncontent\n"),
        rule("crlf", b"# Rule\r\ncontent\r\n"),
    ]);
    assert!(formatting);
    assert!(!has_diff);
}
