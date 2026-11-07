//! TU-038: real-binary smoke runs stay inside the fake HOME and preserve read-only state.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{Sandbox, agentuna, exit_code, stderr, stdout};

fn tree_hash(root: &Path) -> [u8; 32] {
    let root = fs::canonicalize(root).expect("canonicalize sandbox root");
    fn visit(root: &Path, current: &Path, digest: &mut Sha256) {
        let mut entries = fs::read_dir(current)
            .expect("read sandbox fixture")
            .map(|entry| entry.expect("read sandbox entry").path())
            .collect::<Vec<PathBuf>>();
        entries.sort();
        for path in entries {
            let relative = path
                .strip_prefix(root)
                .expect("entry remains under sandbox");
            // SQLite creates and removes `-wal`/`-shm` sidecars while reading a WAL database.
            let name = relative
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.ends_with(".db-wal") || name.ends_with(".db-shm") {
                continue;
            }
            digest.update(relative.to_string_lossy().as_bytes());
            let metadata = fs::symlink_metadata(&path).expect("read sandbox metadata");
            if metadata.file_type().is_symlink() {
                digest.update(b"symlink");
                digest.update(
                    fs::read_link(&path)
                        .expect("read sandbox symlink")
                        .to_string_lossy()
                        .as_bytes(),
                );
            } else if metadata.is_dir() {
                digest.update(b"directory");
                visit(root, &path, digest);
            } else if metadata.is_file() {
                digest.update(b"file");
                digest.update(fs::read(&path).expect("read sandbox file"));
            }
        }
    }

    let mut digest = Sha256::new();
    visit(&root, &root, &mut digest);
    digest.finalize().into()
}

fn assert_read_only_case(sandbox: &Sandbox, args: &[&str], expected_code: i32) {
    let before = tree_hash(&sandbox.path(""));
    let output = agentuna(sandbox, args);
    assert_eq!(
        exit_code(&output),
        expected_code,
        "args={args:?}; stdout={}; stderr={}",
        stdout(&output),
        stderr(&output)
    );
    if args.contains(&"--json") {
        let envelope: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("invalid JSON envelope for {args:?}: {error}"));
        assert!(envelope.is_object(), "JSON output is an object: {envelope}");
        for key in [
            "schema_version",
            "command",
            "ok",
            "data",
            "warnings",
            "error",
        ] {
            assert!(
                envelope.get(key).is_some(),
                "envelope lacks {key}: {envelope}"
            );
        }
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["ok"], expected_code == 0);
        match expected_code {
            2 => assert_eq!(envelope["error"]["code"], "usage"),
            4 => assert_eq!(envelope["error"]["code"], "not_found"),
            5 => assert_eq!(envelope["error"]["code"], "locked"),
            _ => {}
        }
    }
    assert_eq!(
        tree_hash(&sandbox.path("")),
        before,
        "read-only CLI invocation changed the fake HOME: {args:?}"
    );
}

fn assert_read_only_mutation(sandbox: &Sandbox, args: &[&str]) {
    let mut full = vec!["--read-only", "--json"];
    full.extend_from_slice(args);
    assert_read_only_case(sandbox, &full, 5);
}

#[test]
fn every_read_command_preserves_the_fake_home_in_read_only_mode() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let user_store =
        at_store::layout::StoreLayout::new(at_model::Scope::User, sandbox.user_store());
    at_store::layout::init_store(at_model::Scope::User, user_store.root())
        .expect("initialize user store");
    let project = sandbox.path("home/Projects/read-only-project");
    fs::create_dir_all(&project).expect("create the fixture project");
    let database = at_db::Database::open_file(sandbox.paths().db_path()).expect("database");
    at_db::projects::add_project(database.connection(), &project, None, None)
        .expect("register the fixture project");
    drop(database);

    let missing_item = at_model::ItemId::new().to_string();
    let missing_rule = sandbox.path("home/AGENTS.missing.md");
    let cases: &[(&[&str], i32)] = &[
        (&["projects", "list"], 0),
        (&["scan"], 0),
        (&["doctor"], 0),
        (&["completions", "bash"], 0),
        (&["version"], 0),
        (&["config"], 0),
        (&["skills", "list"], 0),
        (&["skills", "show", "missing-skill"], 4),
        (&["skills", "preset", "list"], 0),
        (&["mcp", "list"], 0),
        (&["mcp", "show", "missing-server"], 4),
        (&["mcp", "test", "missing-server"], 4),
        (&["mcp", "secret", "list"], 0),
        (&["migrate", "scan"], 0),
        (&["migrate", "status"], 0),
        (&["drift", "list"], 0),
        (&["backups", "list"], 0),
        (&["remote", "status"], 0),
        (&["agents", "list"], 0),
        (&["agents", "inspect", "claude_code"], 4),
        (&["effective", "--scope", "user"], 0),
        (&["overview", "--scope", "user"], 0),
        (&["rules", "list", "--scope", "user"], 0),
    ];
    for (args, expected_code) in cases {
        let mut full = vec!["--read-only", "--json"];
        full.extend_from_slice(args);
        assert_read_only_case(&sandbox, &full, *expected_code);
    }

    let native_skill = sandbox.path("home/.claude/skills/native-skill");
    fs::create_dir_all(&native_skill).expect("create native skill fixture");
    fs::write(
        native_skill.join("SKILL.md"),
        "---\nname: native-skill\ndescription: Read-only preview\n---\n\n# Native skill\n",
    )
    .expect("write fixture");
    assert_read_only_case(
        &sandbox,
        &[
            "--read-only",
            "--json",
            "adopt",
            native_skill.to_str().expect("UTF-8 fixture path"),
            "--dry-run",
        ],
        0,
    );

    let project_text = project.to_str().expect("UTF-8 fixture path");
    let missing_item_text = missing_item.as_str();
    let missing_rule_text = missing_rule.to_str().expect("UTF-8 fixture path");
    for (args, expected_code) in [
        (
            vec![
                "--read-only",
                "--json",
                "--project",
                project_text,
                "sync",
                "status",
            ],
            0,
        ),
        (
            vec![
                "--read-only",
                "--json",
                "--scope",
                "user",
                "why",
                missing_item_text,
            ],
            4,
        ),
        (
            vec!["--read-only", "--json", "rules", "show", missing_rule_text],
            4,
        ),
    ] {
        assert_read_only_case(&sandbox, &args, expected_code);
    }
}

#[test]
fn every_mutating_command_refuses_read_only_with_a_typed_error() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let project = sandbox.path("home/Projects/read-only-project");
    fs::create_dir_all(&project).expect("create the fixture project");
    let database = at_db::Database::open_file(sandbox.paths().db_path()).expect("database");
    at_db::projects::add_project(database.connection(), &project, None, None)
        .expect("register the fixture project");
    drop(database);

    let project_text = project.to_str().expect("UTF-8 fixture path").to_owned();
    let native_skill = sandbox.path("home/.claude/skills/native-skill");
    fs::create_dir_all(&native_skill).expect("create native skill fixture");
    fs::write(native_skill.join("SKILL.md"), "# native skill\n").expect("write fixture");
    let native_text = native_skill
        .to_str()
        .expect("UTF-8 fixture path")
        .to_owned();
    let rule_path = sandbox.path("home/AGENTS.md");
    fs::write(&rule_path, "# existing rule\n").expect("write rule fixture");
    let rule_text = rule_path.to_str().expect("UTF-8 fixture path").to_owned();
    let missing_item = at_model::ItemId::new().to_string();

    let cases: Vec<Vec<String>> = vec![
        vec!["projects".into(), "add".into(), project_text.clone()],
        vec!["projects".into(), "remove".into(), project_text.clone()],
        vec!["projects".into(), "use".into(), project_text.clone()],
        vec!["doctor".into(), "--fix".into()],
        vec!["mcp".into(), "add".into()],
        vec!["mcp".into(), "edit".into(), "missing-server".into()],
        vec![
            "mcp".into(),
            "remove".into(),
            "missing-server".into(),
            "--force".into(),
        ],
        vec![
            "mcp".into(),
            "enable".into(),
            "missing-server".into(),
            "--agent".into(),
            "codex".into(),
        ],
        vec![
            "mcp".into(),
            "disable".into(),
            "missing-server".into(),
            "--agent".into(),
            "codex".into(),
        ],
        vec![
            "mcp".into(),
            "secret".into(),
            "set".into(),
            "test-secret".into(),
        ],
        vec![
            "mcp".into(),
            "secret".into(),
            "rm".into(),
            "test-secret".into(),
            "--force".into(),
        ],
        vec![
            "skills".into(),
            "install".into(),
            "/missing-skill-source".into(),
        ],
        vec![
            "skills".into(),
            "add".into(),
            "--from".into(),
            "/missing-skill-source".into(),
        ],
        vec![
            "skills".into(),
            "enable".into(),
            "missing-skill".into(),
            "--agent".into(),
            "codex".into(),
        ],
        vec![
            "skills".into(),
            "deploy".into(),
            "missing-skill".into(),
            "--agent".into(),
            "codex".into(),
        ],
        vec![
            "skills".into(),
            "disable".into(),
            "missing-skill".into(),
            "--agent".into(),
            "codex".into(),
        ],
        vec![
            "skills".into(),
            "remove".into(),
            "missing-skill".into(),
            "--yes".into(),
        ],
        vec![
            "skills".into(),
            "preset".into(),
            "apply".into(),
            "missing-preset".into(),
            "--agent".into(),
            "codex".into(),
        ],
        vec!["migrate".into(), "plan".into()],
        vec![
            "migrate".into(),
            "apply".into(),
            "--plan".into(),
            "missing-plan".into(),
            "--yes".into(),
        ],
        vec![
            "migrate".into(),
            "undo".into(),
            "missing-operation".into(),
            "--yes".into(),
        ],
        vec!["adopt".into(), native_text, "--yes".into()],
        vec!["drift".into(), "adopt".into(), "1".into(), "--yes".into()],
        vec!["drift".into(), "restore".into(), "1".into(), "--yes".into()],
        vec![
            "backups".into(),
            "restore".into(),
            "missing-backup".into(),
            "--yes".into(),
        ],
        vec![
            "sync".into(),
            "push".into(),
            "missing-item".into(),
            "--project".into(),
            project_text.clone(),
            "--yes".into(),
        ],
        vec![
            "sync".into(),
            "pull".into(),
            "missing-item".into(),
            "--project".into(),
            project_text,
            "--yes".into(),
        ],
        vec!["remote".into(), "backup".into(), "--yes".into()],
        vec![
            "remote".into(),
            "restore".into(),
            "0123456789abcdef0123456789abcdef01234567".into(),
            "--yes".into(),
        ],
        vec![
            "--scope".into(),
            "global".into(),
            "effective".into(),
            "lock".into(),
            missing_item.clone(),
            "--reason".into(),
            "read-only test".into(),
        ],
        vec![
            "--scope".into(),
            "global".into(),
            "effective".into(),
            "unlock".into(),
            missing_item,
        ],
        vec![
            "rules".into(),
            "save".into(),
            rule_text,
            "--scope".into(),
            "user".into(),
            "--stdin".into(),
        ],
        vec![
            "rules".into(),
            "new".into(),
            "--scope".into(),
            "user".into(),
            "--type".into(),
            "AGENTS.md".into(),
        ],
    ];
    for args in cases {
        let references = args.iter().map(String::as_str).collect::<Vec<_>>();
        assert_read_only_mutation(&sandbox, &references);
    }
}

#[test]
fn malformed_and_unknown_invocations_preserve_fake_home_hash() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    assert_read_only_case(&sandbox, &["--read-only", "not-a-command"], 2);
    assert_read_only_case(&sandbox, &["--read-only", "--json", "not-a-command"], 2);
    assert_read_only_case(&sandbox, &["--read-only", "projects", "unknown"], 2);
    assert_read_only_case(&sandbox, &["--read-only", "doctor", "--unknown"], 2);
}
