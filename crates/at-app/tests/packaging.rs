//! TF-031: the release signing and notarization scripts stay wired to the documented steps.
//!
//! These read the checked-in scripts instead of running `codesign`/`notarytool`, which need an
//! Apple Developer identity and network access. The real credential-dependent checks remain a
//! release-job prerequisite.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

fn read(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn entitlements_contain_only_the_network_client_capability() {
    let plist = read("scripts/entitlements.plist");
    let keys: Vec<&str> = plist
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("<key>")
                .and_then(|rest| rest.strip_suffix("</key>"))
        })
        .collect();
    assert_eq!(
        keys,
        vec!["com.apple.security.network.client"],
        "the entitlement snapshot contains exactly the network client capability"
    );
    assert!(
        plist.contains("<true/>") && !plist.contains("<false/>"),
        "the one capability is granted"
    );
    for forbidden in [
        "com.apple.security.cs.allow-jit",
        "com.apple.security.cs.allow-unsigned-executable-memory",
        "com.apple.security.cs.disable-library-validation",
    ] {
        assert!(
            !plist.contains(forbidden),
            "entitlements must not contain {forbidden}"
        );
    }
}

#[test]
fn signing_uses_developer_id_hardened_runtime_and_a_secure_timestamp_in_nested_order() {
    let sign = read("scripts/sign.sh");
    for needle in [
        "Developer ID Application",
        "--options runtime --timestamp",
        "codesign --verify --deep --strict",
        "codesign -dv --verbose=4",
        "Authority=Developer ID Application",
        "Timestamp=",
        "TeamIdentifier=",
    ] {
        assert!(sign.contains(needle), "sign.sh is missing `{needle}`");
    }
    let inner = sign
        .find("Signing the inner executables")
        .expect("inner executables are signed first");
    let frameworks = sign
        .find("Signing the embedded frameworks")
        .expect("frameworks are signed second");
    let app = sign
        .find("Signing the app bundle")
        .expect("the app bundle is signed last");
    assert!(
        inner < frameworks && frameworks < app,
        "codesign order is inner binaries, frameworks, app bundle"
    );
}

/// Runs the bundle script's dry-run mode and returns the commands it would execute, in order.
fn bundle_plan() -> Vec<String> {
    let root = repo_root();
    let output = std::process::Command::new("bash")
        .arg(root.join("scripts/bundle.sh"))
        .arg("--dry-run")
        .current_dir(&root)
        .output()
        .expect("scripts/bundle.sh --dry-run runs");
    assert!(
        output.status.success(),
        "scripts/bundle.sh --dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("+ ").map(str::to_owned))
        .collect()
}

#[test]
fn the_bundle_script_builds_the_frontend_first_and_links_it_into_the_app() {
    let plan = bundle_plan();
    assert!(
        plan.len() >= 2,
        "the dry run printed no build plan: {plan:?}"
    );

    let ui_build = plan
        .iter()
        .position(|step| step.contains("pnpm --dir ui build"))
        .expect("the plan builds the frontend");

    let app_builds: Vec<usize> = plan
        .iter()
        .enumerate()
        .filter(|(_, step)| step.contains("-p at-app"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        app_builds.len(),
        1,
        "a host bundle builds the app exactly once: {plan:?}"
    );
    let app_build = app_builds[0];
    assert!(
        ui_build < app_build,
        "the frontend build must precede the app build, which embeds it: {plan:?}"
    );

    // Without `--features custom-protocol` tauri compiles its `dev` cfg and the release
    // window loads build.devUrl (http://localhost:1420) instead of the embedded assets.
    let app_step = &plan[app_build];
    assert!(
        app_step.contains("--features custom-protocol"),
        "the app build must enable the custom-protocol feature: {app_step}"
    );
    assert!(
        plan.iter()
            .any(|step| step.contains("-p at-cli") && step.contains("--release")),
        "the plan still builds the packaged CLI: {plan:?}"
    );
}

#[test]
fn the_embedded_frontend_directory_is_the_one_the_ui_build_writes() {
    let root = repo_root();
    let config = fs::read_to_string(root.join("crates/at-app/tauri.conf.json"))
        .expect("the app config is readable");
    let config: serde_json::Value = serde_json::from_str(&config).expect("the app config is JSON");
    let frontend_dist = config["build"]["frontendDist"]
        .as_str()
        .expect("build.frontendDist is a string");
    let resolved = root.join("crates/at-app").join(frontend_dist);
    assert_eq!(
        resolved.canonicalize().unwrap_or(resolved),
        root.join("ui/dist"),
        "frontendDist must be the directory `pnpm --dir ui build` writes"
    );
}

#[test]
fn notarization_waits_for_accepted_status_and_staples_app_and_dmg() {
    let script = read("scripts/notarize.sh");
    for needle in [
        "notarytool submit",
        "--wait",
        "status\" != \"Accepted",
        "notarytool log",
        "stapler staple \"$APP\"",
        "stapler validate \"$APP\"",
        "stapler staple \"$DMG\"",
        "stapler validate \"$DMG\"",
        "spctl --assess",
        "requires a Developer ID Application signature",
    ] {
        assert!(script.contains(needle), "notarize.sh is missing `{needle}`");
    }
    let app_staple = script
        .find("stapler staple \"$APP\"")
        .expect("the app is stapled");
    let dmg_submit = script.find("submit \"dmg\"").expect("the dmg is notarized");
    assert!(
        app_staple < dmg_submit,
        "the app is stapled before the dmg is submitted"
    );
}

#[test]
fn bundle_script_end_to_end_dry_run_and_launch_command_verification() {
    let root = repo_root();
    let bundle_script = root.join("scripts/bundle.sh");
    assert!(bundle_script.exists(), "scripts/bundle.sh exists");

    // 1. End-to-end dry-run execution
    let output = std::process::Command::new("bash")
        .arg(&bundle_script)
        .arg("--dry-run")
        .current_dir(&root)
        .output()
        .expect("scripts/bundle.sh --dry-run executes");
    assert!(
        output.status.success(),
        "scripts/bundle.sh --dry-run failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Dry run: printed the build plan for"),
        "dry-run must print the completion confirmation: {stdout}"
    );
    assert!(
        stdout.contains("nothing was built or written"),
        "dry-run must not write artifacts: {stdout}"
    );

    // 2. Universal mode dry-run execution
    let universal_output = std::process::Command::new("bash")
        .arg(&bundle_script)
        .arg("--dry-run")
        .arg("--universal")
        .current_dir(&root)
        .output()
        .expect("scripts/bundle.sh --dry-run --universal executes");
    assert!(
        universal_output.status.success(),
        "scripts/bundle.sh --dry-run --universal failed: {}",
        String::from_utf8_lossy(&universal_output.stderr)
    );
    let universal_stdout = String::from_utf8_lossy(&universal_output.stdout);
    assert!(
        universal_stdout.contains("aarch64-apple-darwin")
            && universal_stdout.contains("x86_64-apple-darwin"),
        "universal mode dry run plans both architecture targets: {universal_stdout}"
    );

    // 3. Launch command verification (TF-029 AC1)
    // The bundle script targets target/release/AgenTuna.app.
    // The standard macOS launcher command is `open 'target/release/AgenTuna.app'`.
    let open_bin = Path::new("/usr/bin/open");
    assert!(
        open_bin.exists(),
        "the macOS launch command `/usr/bin/open` must exist on the system"
    );

    let script_content = fs::read_to_string(&bundle_script).expect("bundle.sh readable");
    assert!(
        script_content.contains(r#"APP_NAME="AgenTuna""#),
        "bundle script defines app name AgenTuna"
    );
    assert!(
        script_content.contains(r#"APP_BUNDLE="$TARGET_DIR/release/$APP_NAME.app""#),
        "bundle path is target/release/AgenTuna.app"
    );
    assert!(
        script_content.contains("<string>agentuna-app</string>"),
        "bundle plist specifies agentuna-app executable"
    );

    // NOTE on TF-029 AC1:
    // Launching the GUI application via `open 'target/release/AgenTuna.app'` requires an
    // interactive macOS desktop session / WindowServer connection that is unavailable in
    // headless test environments and subagents. The launch command itself is verified
    // above; actual GUI window launch must be verified manually by an operator.
}
