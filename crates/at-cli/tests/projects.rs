//! TU-027: `agentuna projects` and `agentuna scan` against a sandboxed HOME.
//!
//! Every case spawns the real binary with `AGENTUNA_*` roots redirected into a
//! temp dir, so no test touches the real home, agent folders or Keychain.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use support::{Sandbox, agentuna, exit_code, stderr, stdout};

/// Parses stdout, requiring exactly one JSON value.
fn envelope(output: &std::process::Output) -> Value {
    let text = stdout(output);
    assert_eq!(
        text.lines().count(),
        1,
        "JSON mode must write exactly one line, got: {text}"
    );
    serde_json::from_str(&text).expect("stdout is one JSON object")
}

/// Creates `<root>/<name>/.git` so the scanner detects a project there.
fn make_project(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join(".git")).expect("create the fixture project");
    dir
}

#[test]
fn projects_list_on_a_fresh_install_is_an_empty_envelope() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    let output = agentuna(&sandbox, &["projects", "list", "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["command"], "projects list");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["error"], Value::Null);
    assert_eq!(envelope["data"]["projects"], serde_json::json!([]));
    assert_eq!(envelope["data"]["active_project_id"], Value::Null);
    assert_eq!(envelope["warnings"], serde_json::json!([]));
}

#[test]
fn projects_list_reports_counts_timestamps_hidden_and_order() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    let alpha = sandbox.path("alpha");
    let beta = sandbox.path("beta");
    fs::create_dir_all(&alpha).expect("alpha dir");
    fs::create_dir_all(&beta).expect("beta dir");

    let added = agentuna(
        &sandbox,
        &["projects", "add", alpha.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&added), 0, "stderr: {}", stderr(&added));
    let added_beta = agentuna(
        &sandbox,
        &["projects", "add", beta.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&added_beta), 0);

    // `use` makes beta the most recently used project, so it must sort first.
    let used = agentuna(
        &sandbox,
        &["projects", "use", beta.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&used), 0, "stderr: {}", stderr(&used));
    assert_eq!(envelope(&used)["data"]["changed"], true);

    // Hide alpha; it stays listed with hidden = true.
    let removed = agentuna(
        &sandbox,
        &["projects", "remove", alpha.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&removed), 0, "stderr: {}", stderr(&removed));

    let output = agentuna(&sandbox, &["projects", "list", "--json"]);
    assert_eq!(exit_code(&output), 0);
    let data = envelope(&output)["data"].clone();
    let projects = data["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 2);

    // Ordering: last_used_at desc, then name. Beta was used last.
    assert_eq!(
        projects[0]["path"].as_str().unwrap(),
        fs::canonicalize(&beta).unwrap().to_str().unwrap()
    );
    assert_eq!(projects[1]["hidden"], true);
    assert_eq!(projects[0]["hidden"], false);

    // Golden fields on every row.
    for project in projects {
        for field in [
            "id",
            "name",
            "path",
            "last_used_at",
            "hidden",
            "skill_count",
            "server_count",
            "rule_count",
        ] {
            assert!(
                project.get(field).is_some(),
                "project row is missing `{field}`: {project}"
            );
        }
        assert!(project["id"].as_str().unwrap().starts_with("prj_"));
        assert!(project["last_used_at"].is_string());
    }
    assert_eq!(
        data["active_project_id"].as_str().unwrap(),
        projects[0]["id"].as_str().unwrap()
    );
}

#[test]
fn projects_add_registers_and_reports_the_id() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let dir = make_project(&sandbox.path("projects"), "demo");

    let output = agentuna(
        &sandbox,
        &["projects", "add", dir.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let data = envelope(&output)["data"].clone();
    assert_eq!(data["inserted"], true);
    assert!(data["project"]["id"].as_str().unwrap().starts_with("prj_"));
    assert_eq!(
        data["project"]["path"].as_str().unwrap(),
        fs::canonicalize(&dir).unwrap().to_str().unwrap()
    );
    assert_eq!(data["project"]["name"], "demo");
    assert_eq!(data["project"]["hidden"], false);
}

#[test]
fn projects_add_through_a_symlink_selects_the_existing_row() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let real = make_project(&sandbox.path("real"), "app");
    let link = sandbox.path("link-to-app");
    std::os::unix::fs::symlink(&real, &link).expect("create the symlink");

    let first = agentuna(
        &sandbox,
        &["projects", "add", real.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&first), 0);
    let first_id = envelope(&first)["data"]["project"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let second = agentuna(
        &sandbox,
        &["projects", "add", link.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&second), 0, "stderr: {}", stderr(&second));
    let second_env = envelope(&second);
    assert_eq!(second_env["ok"], true);
    assert_eq!(second_env["data"]["inserted"], false);
    assert_eq!(
        second_env["data"]["project"]["id"].as_str().unwrap(),
        first_id
    );
    let warnings = second_env["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w["code"] == "already_registered"),
        "expected an already_registered warning: {second_env}"
    );

    // Still exactly one row.
    let list = agentuna(&sandbox, &["projects", "list", "--json"]);
    assert_eq!(
        envelope(&list)["data"]["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn projects_add_rejects_a_missing_or_non_directory_path() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    // A path that does not exist exits 4 (not_found).
    let missing = sandbox.path("no-such-dir");
    let output = agentuna(
        &sandbox,
        &["projects", "add", missing.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&output), 4, "stderr: {}", stderr(&output));
    assert_eq!(envelope(&output)["error"]["code"], "not_found");

    // An existing file exits 3 (validation): projects are folders.
    let file = sandbox.path("a-file.txt");
    fs::write(&file, "x").expect("write the fixture file");
    let output = agentuna(
        &sandbox,
        &["projects", "add", file.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&output), 3, "stderr: {}", stderr(&output));
    assert_eq!(envelope(&output)["error"]["code"], "validation");
}

#[test]
fn projects_remove_and_use_accept_ids_and_paths() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let dir = make_project(&sandbox.path("projects"), "demo");

    let added = agentuna(
        &sandbox,
        &["projects", "add", dir.to_str().unwrap(), "--json"],
    );
    let id = envelope(&added)["data"]["project"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // `use` by id.
    let used = agentuna(&sandbox, &["projects", "use", id.as_str(), "--json"]);
    assert_eq!(exit_code(&used), 0, "stderr: {}", stderr(&used));
    assert_eq!(
        envelope(&used)["data"]["active_project_id"]
            .as_str()
            .unwrap(),
        id.as_str()
    );

    // `remove` by path hides but keeps the row.
    let removed = agentuna(
        &sandbox,
        &["projects", "remove", dir.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&removed), 0, "stderr: {}", stderr(&removed));
    assert_eq!(envelope(&removed)["data"]["hidden"], true);

    let list = agentuna(&sandbox, &["projects", "list", "--json"]);
    let list_env = envelope(&list);
    let projects = list_env["data"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "remove hides, it does not delete");
    assert_eq!(projects[0]["hidden"], true);
    // last_used_at survives hiding.
    assert!(projects[0]["last_used_at"].is_string());
}

#[test]
fn projects_remove_and_use_unknown_target_exit_not_found() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    for args in [
        vec!["projects", "remove", "prj_missing", "--json"],
        vec!["projects", "use", "/definitely/not/registered", "--json"],
    ] {
        let output = agentuna(&sandbox, &args);
        assert_eq!(exit_code(&output), 4, "args {args:?}: {}", stderr(&output));
        let envelope = envelope(&output);
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], "not_found");
        assert!(!envelope["error"]["hint"].as_str().unwrap().is_empty());
    }
}

#[test]
fn write_commands_refuse_read_only_mode() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let dir = make_project(&sandbox.path("projects"), "demo");

    for args in [
        vec!["projects", "add", dir.to_str().unwrap()],
        vec!["projects", "remove", dir.to_str().unwrap()],
        vec!["projects", "use", dir.to_str().unwrap()],
    ] {
        let mut full = vec!["--read-only", "--json"];
        full.extend(args.iter().copied());
        let output = agentuna(&sandbox, &full);
        assert_eq!(exit_code(&output), 5, "args {args:?}: {}", stderr(&output));
        assert_eq!(envelope(&output)["error"]["code"], "locked");
    }
}

#[test]
fn projects_dry_run_reports_without_writing() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let dir = make_project(&sandbox.path("projects"), "demo");

    let output = agentuna(
        &sandbox,
        &[
            "--dry-run",
            "--json",
            "projects",
            "add",
            dir.to_str().unwrap(),
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(envelope(&output)["data"]["applied"], false);

    let list = agentuna(&sandbox, &["projects", "list", "--json"]);
    assert_eq!(
        envelope(&list)["data"]["projects"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn scan_reports_discovered_new_missing_and_unchanged() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    // ~/Projects under the sandbox home holds two projects; one is registered.
    let home_projects = sandbox.path("home").join("Projects");
    let known = make_project(&home_projects, "known");
    let fresh = make_project(&home_projects, "fresh");

    let added = agentuna(
        &sandbox,
        &["projects", "add", known.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&added), 0);

    let output = agentuna(&sandbox, &["scan", "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let scan_env = envelope(&output);
    assert_eq!(scan_env["ok"], true);
    let data = &scan_env["data"];

    assert_eq!(data["depth"], 4);
    assert!(
        data["roots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|root| { root.as_str().unwrap().ends_with("Projects") })
    );

    let discovered = data["discovered"].as_array().unwrap();
    assert_eq!(discovered.len(), 2, "data: {data}");

    let new_paths: Vec<&str> = data["new"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();
    assert!(new_paths.contains(&fs::canonicalize(&fresh).unwrap().to_str().unwrap()));

    let registered = agentuna(&sandbox, &["projects", "list", "--json"]);
    assert_eq!(exit_code(&registered), 0, "{}", stderr(&registered));
    let registered_envelope = envelope(&registered);
    let registered_projects = registered_envelope["data"]["projects"]
        .as_array()
        .expect("project registry rows");
    let discovered_row = registered_projects
        .iter()
        .find(|project| project["path"] == fs::canonicalize(&fresh).unwrap().to_str().unwrap())
        .expect("scan upserts newly discovered projects");
    assert_eq!(
        discovered_row["source_root"],
        fs::canonicalize(&home_projects).unwrap().to_str().unwrap()
    );

    let unchanged_paths: Vec<&str> = data["unchanged"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();
    assert!(unchanged_paths.contains(&fs::canonicalize(&known).unwrap().to_str().unwrap()));

    // A registered project whose folder vanished lands in `missing`.
    let gone = sandbox.path("gone-project");
    fs::create_dir_all(&gone).expect("gone dir");
    let added_gone = agentuna(
        &sandbox,
        &["projects", "add", gone.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&added_gone), 0);
    fs::remove_dir_all(&gone).expect("remove the project folder");

    let rescan = agentuna(&sandbox, &["scan", "--json"]);
    assert_eq!(exit_code(&rescan), 0);
    let rescan_env = envelope(&rescan);
    let missing = rescan_env["data"]["missing"].as_array().unwrap();
    assert!(
        missing
            .iter()
            .any(|p| p.as_str().unwrap().contains("gone-project")),
        "missing must name the vanished project: {missing:?}"
    );
    let after_missing = agentuna(&sandbox, &["projects", "list", "--json"]);
    let missing_row = envelope(&after_missing)["data"]["projects"]
        .as_array()
        .expect("project registry rows")
        .iter()
        .find(|project| {
            project["path"]
                .as_str()
                .is_some_and(|path| path.contains("gone-project"))
        })
        .cloned()
        .expect("missing project remains in the registry");
    assert_eq!(missing_row["hidden"], true);
}

#[test]
fn scan_honours_depth_and_reports_warnings_without_failing() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    // A project three levels below ~/Projects is only found with enough depth.
    let deep = sandbox
        .path("home")
        .join("Projects")
        .join("group")
        .join("sub")
        .join("deep-app");
    fs::create_dir_all(deep.join(".git")).expect("deep project");

    let shallow = agentuna(&sandbox, &["scan", "--depth", "1", "--json"]);
    assert_eq!(exit_code(&shallow), 0);
    assert_eq!(
        envelope(&shallow)["data"]["discovered"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let deep_scan = agentuna(&sandbox, &["scan", "--depth", "4", "--json"]);
    assert_eq!(exit_code(&deep_scan), 0);
    let data = envelope(&deep_scan)["data"].clone();
    assert_eq!(data["discovered"].as_array().unwrap().len(), 1);
    assert_eq!(data["depth"], 4);
}

#[test]
fn scan_skips_a_missing_default_root_without_a_warning() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    // The sandbox home has no Projects folder at all: the documented default root is a
    // convenience, so its absence is skipped silently instead of warning on every run.

    let output = agentuna(&sandbox, &["scan", "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["discovered"].as_array().unwrap().len(), 0);
    assert_eq!(
        envelope["warnings"].as_array().unwrap().len(),
        0,
        "a missing default root is not a warning: {envelope}"
    );
    assert_eq!(
        envelope["data"]["warnings"].as_array().unwrap().len(),
        0,
        "the report lists no scan warning either: {envelope}"
    );
    assert!(
        !stderr(&output).contains("warning:"),
        "nothing is written to stderr: {}",
        stderr(&output)
    );

    let human = agentuna(&sandbox, &["scan"]);
    assert_eq!(exit_code(&human), 0);
    assert!(
        !stdout(&human).contains("missing_root"),
        "the human report omits the absent default root: {}",
        stdout(&human)
    );
}

#[test]
fn scan_permission_denied_root_is_a_warning_and_still_exits_zero() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new();
    sandbox.healthy();

    // ~/Projects exists but cannot be listed.
    let projects = sandbox.path("home").join("Projects");
    fs::create_dir_all(&projects).expect("create the scan root");
    let mut permissions = fs::metadata(&projects)
        .expect("root metadata")
        .permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&projects, permissions).expect("chmod the scan root");

    let output = agentuna(&sandbox, &["scan", "--json"]);
    // Restore permissions so the temp dir cleans up.
    let mut permissions = fs::metadata(&projects)
        .expect("root metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&projects, permissions).expect("restore the scan root");

    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(envelope["ok"], true);
    let warnings = envelope["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "scan_permission_denied"),
        "expected a scan_permission_denied warning: {envelope}"
    );
    assert!(
        envelope["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["kind"] == "permission_denied")
    );
}

#[test]
fn scan_nested_repos_collapse_to_shallowest_match() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    let projects_root = sandbox.path("home").join("Projects");
    let outer = make_project(&projects_root, "outer");
    let _inner = make_project(&outer, "inner");

    let output = agentuna(&sandbox, &["scan", "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(envelope["ok"], true);
    let discovered = envelope["data"]["discovered"].as_array().unwrap();
    let paths: Vec<&str> = discovered
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();

    let canonical_outer = fs::canonicalize(&outer).unwrap();
    assert_eq!(paths.len(), 1, "nested inner repo must collapse into outer");
    assert_eq!(paths[0], canonical_outer.to_str().unwrap());
}

#[cfg(unix)]
#[test]
fn scan_symlink_cycle_does_not_loop_and_reports_cleanly() {
    use std::os::unix::fs::symlink;

    let sandbox = Sandbox::new();
    sandbox.healthy();

    let projects_root = sandbox.path("home").join("Projects");
    let _repo = make_project(&projects_root, "real_repo");
    symlink(&projects_root, projects_root.join("loop")).expect("cycle symlink");

    let output = agentuna(&sandbox, &["scan", "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(envelope["ok"], true);
    let discovered = envelope["data"]["discovered"].as_array().unwrap();
    assert_eq!(
        discovered.len(),
        1,
        "symlink loop must not duplicate or recurse"
    );
}

#[test]
fn scan_never_modifies_last_used_at_on_existing_projects() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    let projects_root = sandbox.path("home").join("Projects");
    let known = make_project(&projects_root, "known_project");

    // Register project and touch it with `projects use` so last_used_at is recorded
    let add_out = agentuna(
        &sandbox,
        &["projects", "add", known.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&add_out), 0);
    let use_out = agentuna(
        &sandbox,
        &["projects", "use", known.to_str().unwrap(), "--json"],
    );
    assert_eq!(exit_code(&use_out), 0);

    let list_before = agentuna(&sandbox, &["projects", "list", "--json"]);
    let env_before = envelope(&list_before);
    let project_before = env_before["data"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "known_project")
        .expect("found known_project");
    let last_used_before = project_before["last_used_at"]
        .as_str()
        .expect("has last_used_at");

    // Create a new project so scan has a new project to discover and diff
    let _fresh = make_project(&projects_root, "fresh_project");

    // Run scan
    let scan_out = agentuna(&sandbox, &["scan", "--json"]);
    assert_eq!(exit_code(&scan_out), 0, "stderr: {}", stderr(&scan_out));

    let list_after = agentuna(&sandbox, &["projects", "list", "--json"]);
    let env_after = envelope(&list_after);
    let project_after = env_after["data"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "known_project")
        .expect("found known_project after scan");
    let last_used_after = project_after["last_used_at"]
        .as_str()
        .expect("has last_used_at");

    assert_eq!(
        last_used_before, last_used_after,
        "applying scan diff must never modify last_used_at on existing projects"
    );
}
#[test]
fn json_mode_keeps_stdout_to_exactly_one_object() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let dir = make_project(&sandbox.path("projects"), "demo");

    for args in [
        vec!["projects", "list", "--json"],
        vec!["projects", "add", dir.to_str().unwrap(), "--json"],
        vec!["projects", "remove", dir.to_str().unwrap(), "--json"],
        vec!["projects", "use", dir.to_str().unwrap(), "--json"],
        vec!["scan", "--json"],
        vec!["scan", "--depth", "2", "--json"],
    ] {
        let output = agentuna(&sandbox, &args);
        let text = stdout(&output);
        let parsed: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("args {args:?} stdout is not JSON ({error}): {text}"));
        assert!(parsed.is_object(), "args {args:?}: one JSON object");
        assert!(parsed.get("schema_version").is_some());
        // Diagnostics may exist on stderr but never corrupt stdout.
        let _ = stderr(&output);
    }
}

#[test]
fn human_mode_renders_readable_output() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let dir = make_project(&sandbox.path("projects"), "demo");

    let added = agentuna(&sandbox, &["projects", "add", dir.to_str().unwrap()]);
    assert_eq!(exit_code(&added), 0);
    assert!(
        stdout(&added).contains("registered"),
        "stdout: {}",
        stdout(&added)
    );

    let list = agentuna(&sandbox, &["projects", "list"]);
    assert_eq!(exit_code(&list), 0);
    assert!(stdout(&list).contains("demo"), "stdout: {}", stdout(&list));

    let scan = agentuna(&sandbox, &["scan"]);
    assert_eq!(exit_code(&scan), 0, "stderr: {}", stderr(&scan));
    assert!(stdout(&scan).contains("scan:"), "stdout: {}", stdout(&scan));
}
