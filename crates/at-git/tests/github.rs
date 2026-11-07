//! Integration tests for GitHub REST client and loopback fixture server.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use at_git::github::*;

struct Reply {
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: &'static str,
}

fn reply(status: u16, headers: Vec<(&'static str, &'static str)>, body: &'static str) -> Reply {
    Reply {
        status,
        headers,
        body,
    }
}

/// Serves the canned `replies` in order on loopback and records each request.
fn fixture_server(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let handle = std::thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let expected = loop {
                let read = stream.read(&mut buffer).expect("read");
                if read == 0 {
                    break bytes.len();
                }
                bytes.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&bytes).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let head = &text[..end];
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    break end + 4 + length;
                }
            };
            while bytes.len() < expected {
                let read = stream.read(&mut buffer).expect("read body");
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            captured
                .lock()
                .expect("requests lock")
                .push(String::from_utf8_lossy(&bytes).to_string());
            let mut response = format!("HTTP/1.1 {} fixture\r\n", reply.status);
            for (name, value) in reply.headers {
                response.push_str(name);
                response.push_str(": ");
                response.push_str(value);
                response.push_str("\r\n");
            }
            response.push_str(&format!(
                "content-length: {}\r\nconnection: close\r\n\r\n{}",
                reply.body.len(),
                reply.body
            ));
            stream.write_all(response.as_bytes()).expect("write");
        }
    });
    (format!("http://{address}"), requests, handle)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn token_without_repo_scope_is_refused_before_any_create_request() {
    let (endpoint, requests, handle) = fixture_server(vec![reply(
        200,
        vec![("x-oauth-scopes", "read:user, gist")],
        "{}",
    )]);
    let token = concat!("ghp_", "scope-sentinel-0123456789");
    let client = GitHubClient::with_endpoint(&endpoint).expect("client");
    let error = runtime()
        .block_on(client.create_private_repo(token, "agentuna-backup", None))
        .expect_err("scope refusal");
    assert_eq!(error, GitHubError::MissingRepoScope);
    assert!(!error.to_string().contains(token));
    let captured = requests.lock().expect("requests");
    assert_eq!(captured.len(), 1, "only the scope check ran");
    assert!(captured[0].starts_with("GET /user "));
    assert!(captured[0].contains("authorization: Bearer "));
    assert!(!captured[0].contains(&format!("?{token}")));
    drop(captured);
    handle.join().expect("server thread");
}

#[test]
fn missing_scope_header_is_refused_rather_than_assumed() {
    let (endpoint, _, handle) = fixture_server(vec![reply(200, vec![], "{\"login\":\"octo\"}")]);
    let client = GitHubClient::with_endpoint(&endpoint).expect("client");
    let error = runtime()
        .block_on(client.validate_repo_scope(concat!("ghp_", "abcdefghijklmnopqrstuvwxyz")))
        .expect_err("unreported scopes");
    assert_eq!(error, GitHubError::RepoScopeUnreported);
    handle.join().expect("server thread");
}

#[test]
fn repo_scope_creates_a_private_repository_without_putting_the_token_in_the_request_line() {
    let (endpoint, requests, handle) = fixture_server(vec![
        reply(200, vec![("x-oauth-scopes", "repo, read:user")], "{}"),
        reply(
            201,
            vec![("content-type", "application/json")],
            "{\"clone_url\":\"https://github.com/acme/agentuna-backup.git\",\"private\":true}",
        ),
    ]);
    let token = concat!("github_pat_", "create-sentinel-0123456789");
    let client = GitHubClient::with_endpoint(&endpoint).expect("client");
    let created = runtime()
        .block_on(client.create_private_repo(token, "agentuna-backup", Some("Backup")))
        .expect("create repository");
    assert_eq!(created.url, "https://github.com/acme/agentuna-backup.git");
    let captured = requests.lock().expect("requests");
    assert_eq!(captured.len(), 2);
    assert!(captured[1].starts_with("POST /user/repos "));
    assert!(captured[1].contains("\"private\":true"));
    assert!(
        captured[1].contains(token),
        "token travels in the authorization header"
    );
    assert!(
        !captured[1]
            .lines()
            .next()
            .unwrap_or_default()
            .contains(token),
        "the request line never carries the token"
    );
    drop(captured);
    handle.join().expect("server thread");
}

#[test]
fn rejected_token_names_the_cause_without_echoing_the_secret() {
    let (endpoint, _, handle) = fixture_server(vec![reply(401, vec![], "{}")]);
    let token = concat!("ghp_", "rejected-sentinel-0123456789");
    let client = GitHubClient::with_endpoint(&endpoint).expect("client");
    let error = runtime()
        .block_on(client.validate_repo_scope(token))
        .expect_err("auth rejection");
    assert_eq!(error, GitHubError::AuthRejected);
    assert!(error.to_string().contains("rejected"));
    assert!(!error.to_string().contains(token));
    assert!(!format!("{error:?}").contains(token));
    handle.join().expect("server thread");
}

#[test]
fn unreachable_endpoint_is_offline_and_never_echoes_the_token() {
    let token = concat!("ghp_", "offline-sentinel-0123456789");
    let client = GitHubClient::with_endpoint("http://127.0.0.1:1").expect("client");
    let error = runtime()
        .block_on(client.validate_repo_scope(token))
        .expect_err("offline");
    assert_eq!(error, GitHubError::Offline);
    assert!(!error.to_string().contains(token));
}

#[test]
fn invalid_names_are_refused_before_the_scope_check_runs() {
    let (endpoint, requests, handle) =
        fixture_server(vec![reply(200, vec![("x-oauth-scopes", "repo")], "{}")]);
    let client = GitHubClient::with_endpoint(&endpoint).expect("client");
    let error = runtime()
        .block_on(client.create_private_repo(
            concat!("ghp_", "abcdefghijklmnopqrstuvwxyz"),
            "bad name",
            None,
        ))
        .expect_err("invalid name");
    assert_eq!(error, GitHubError::InvalidRepoName);
    assert!(requests.lock().expect("requests").is_empty());
    drop(requests);
    // No request was served, so the fixture thread still waits; stop it by connecting once.
    let address = endpoint.trim_start_matches("http://");
    let _ = std::net::TcpStream::connect(address);
    handle.join().expect("server thread");
}
