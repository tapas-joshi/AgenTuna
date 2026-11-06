//! Guard: network egress lives only in the crates that consult the policy.
//!
//! The privacy promise is architectural: `at-model` decides, and only the gated crates may own a
//! socket or a network client. This scan fails when a crate outside that set declares an HTTP,
//! TLS, SSH or git client, when a crate other than `at-mcp` mentions the socket API, or when a
//! webview capability grants a network plugin. It reads the workspace manifests and sources,
//! which is what keeps a reviewer from adding a quiet `reqwest` later.

use std::fs;
use std::path::{Path, PathBuf};

/// Crates allowed to own network clients because they call `NetworkPolicy` first.
const GATED: &[&str] = &["at-app", "at-git", "at-mcp"];

/// The one crate allowed to open a socket.
const SOCKET_CRATE: &str = "at-mcp";

/// Crates that speak HTTP, TLS, SSH or git over the wire.
const NETWORK_CLIENT_CRATES: &[&str] = &[
    "attohttpc",
    "curl",
    "curl-sys",
    "git2",
    "h2",
    "hyper",
    "hyper-util",
    "isahc",
    "libgit2",
    "native-tls",
    "octocrab",
    "openssl",
    "reqwest",
    "rustls",
    "ssh2",
    "surf",
    "tungstenite",
    "tokio-tungstenite",
    "tonic",
    "ureq",
    "webpki-roots",
];

/// APIs that create or resolve sockets.
const SOCKET_APIS: &[&str] = &[
    "std::net::",
    "TcpListener",
    "TcpStream",
    "ToSocketAddrs",
    "UdpSocket",
];

/// Capability permissions that would let the webview reach the network behind the policy.
const FORBIDDEN_CAPABILITIES: &[&str] = &["\"http:", "\"shell:", "\"fs:", "\"updater:"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("at-app lives in <workspace>/crates/at-app")
        .to_path_buf()
}

/// Every workspace crate, as `(directory name, directory)`.
fn crate_dirs() -> Vec<(String, PathBuf)> {
    let root = workspace_root().join("crates");
    let mut crates = Vec::new();
    for entry in fs::read_dir(&root).expect("the workspace has a crates directory") {
        let path = entry.expect("a readable directory entry").path();
        if !path.join("Cargo.toml").is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a UTF-8 crate directory name")
            .to_owned();
        crates.push((name, path));
    }
    crates.sort();
    assert!(
        crates.len() >= 12,
        "the scan must see every workspace crate, saw {}",
        crates.len()
    );
    crates
}

/// Dependency names declared in a manifest's dependency tables.
fn declared_dependencies(manifest: &str) -> Vec<String> {
    let mut dependencies = Vec::new();
    let mut in_table = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_table = line.starts_with("[dependencies]")
                || line.starts_with("[dev-dependencies]")
                || line.starts_with("[build-dependencies]");
            continue;
        }
        if !in_table {
            continue;
        }
        let name = line.split(['=', '.']).next().unwrap_or_default().trim();
        if !name.is_empty() {
            dependencies.push(name.to_owned());
        }
    }
    dependencies
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("a readable source directory") {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_gated_crates_declare_a_network_client() {
    let mut inspected = 0;
    for (name, dir) in crate_dirs() {
        let manifest =
            fs::read_to_string(dir.join("Cargo.toml")).expect("a readable crate manifest");
        inspected += 1;
        let dependencies = declared_dependencies(&manifest);
        if GATED.contains(&name.as_str()) {
            assert!(
                dependencies
                    .iter()
                    .any(|dependency| dependency == "at-model"),
                "{name} may use the network, so it must depend on the policy in at-model"
            );
            continue;
        }
        for dependency in dependencies {
            assert!(
                !NETWORK_CLIENT_CRATES.contains(&dependency.as_str()),
                "{name} declares `{dependency}`, a network client outside the policy gate; \
                 route it through one of {GATED:?}"
            );
        }
    }
    assert!(
        inspected >= 12,
        "the scan inspected only {inspected} crates"
    );
}

#[test]
fn only_the_mcp_crate_mentions_the_socket_api() {
    for (name, dir) in crate_dirs() {
        if name == SOCKET_CRATE {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&dir.join("src"), &mut files);
        assert!(!files.is_empty(), "{name} has no source files to scan");
        for file in files {
            let text = fs::read_to_string(&file).expect("a readable source file");
            for api in SOCKET_APIS {
                assert!(
                    !text.contains(api),
                    "{} mentions `{api}`; sockets belong in {SOCKET_CRATE}, behind the policy",
                    file.display()
                );
            }
        }
    }
}

#[test]
fn the_webview_capabilities_grant_no_network_plugin() {
    let capabilities = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    let mut seen = 0;
    for entry in fs::read_dir(&capabilities).expect("the app ships capability files") {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        seen += 1;
        let text = fs::read_to_string(&path).expect("a readable capability file");
        for line in text.lines() {
            if line.contains("$schema") {
                continue;
            }
            for forbidden in FORBIDDEN_CAPABILITIES {
                assert!(
                    !line.contains(forbidden),
                    "{} grants `{forbidden}`; the webview is presentation only",
                    path.display()
                );
            }
        }
    }
    assert!(seen > 0, "the app must ship at least one capability file");
}

#[test]
fn the_registry_stays_owned_by_at_model() {
    // A second action list in another crate would drift from the policy; only at-model may
    // define the enum, and every gated crate must import it.
    let root = workspace_root().join("crates");
    let mut definitions = Vec::new();
    for (name, dir) in crate_dirs() {
        let mut files = Vec::new();
        rust_files(&dir.join("src"), &mut files);
        for file in files {
            let text = fs::read_to_string(&file).expect("a readable source file");
            if text.contains("pub enum NetworkAction {") {
                definitions.push(name.clone());
            }
        }
    }
    assert_eq!(
        definitions,
        ["at-model"],
        "the NetworkAction registry must be defined once, in at-model (scanned {root:?})"
    );
}
