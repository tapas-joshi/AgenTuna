//! Integration tests for remote backup repository creation and loopback fixture server.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use at_app::app::network::NetworkState;
use at_app::app::remote_backup::actions;
use at_app::app::remote_backup::{create_private_repo_and_configure, create_repo_token};
use at_db::Database;
use at_model::network::{NetworkAction, NetworkHost, NetworkTrigger};

struct Reply {
    status: u16,
    scopes: Option<&'static str>,
    body: &'static str,
}

fn fixture_server(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback fixture");
    let address = listener.local_addr().expect("fixture address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let handle = std::thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().expect("accept fixture request");
            let request = read_request_headers(&mut stream);
            captured.lock().expect("request lock").push(request);
            let scope_header = reply
                .scopes
                .map_or_else(String::new, |scope| format!("x-oauth-scopes: {scope}\r\n"));
            let response = format!(
                "HTTP/1.1 {} fixture\r\n{scope_header}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                reply.status,
                reply.body.len(),
                reply.body,
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fixture response");
        }
    });
    (format!("http://{address}"), requests, handle)
}

fn read_request_headers(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).expect("read stream");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).to_lowercase()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

fn authorized_host(network: &NetworkState) -> NetworkHost {
    network
        .authorize(
            NetworkAction::RemoteBackupPush,
            "api.github.com",
            NetworkTrigger::User,
        )
        .expect("user-authorized GitHub request")
}

#[test]
fn private_repo_creation_persists_url_through_remote_backup_configuration() {
    at_mcp::ensure_crypto_provider();
    let (endpoint, requests, server) = fixture_server(vec![
        Reply {
            status: 200,
            scopes: Some("repo, read:user"),
            body: "{}",
        },
        Reply {
            status: 201,
            scopes: None,
            body: r#"{"clone_url":"https://github.com/acme/agentuna-backup.git","private":true}"#,
        },
    ]);
    let mut database = Database::open_in_memory().expect("in-memory database");
    at_store::remote_backup::set_device_name(&mut database, Some("fixture-device"))
        .expect("fixture device name");
    let database = Arc::new(Mutex::new(database));
    let network = NetworkState::default();
    let host = authorized_host(&network);
    let client =
        at_git::github::GitHubClient::with_endpoint(&endpoint).expect("loopback GitHub client");
    let token_text = concat!("ghp_", "repo-create-fixture-0123456789");
    let secrets = at_secrets::MemoryStore::new();
    at_secrets::save_token(&secrets, token_text).expect("save token to memory-only store");
    let token = create_repo_token(&secrets).expect("read saved token");
    assert!(at_secrets::token_exists(&secrets).expect("token presence"));

    runtime()
        .block_on(create_private_repo_and_configure(
            Arc::clone(&database),
            client,
            token,
            "agentuna-backup".to_owned(),
            "fixture-device".to_owned(),
            network.clone(),
            host,
        ))
        .expect("create and configure private repo");
    server.join().expect("fixture server");

    let database = database.lock().expect("database lock");
    let saved = at_db::remote_backup::remote_backup_state(database.connection())
        .expect("saved remote-backup state");
    assert_eq!(
        saved.repo_url.as_deref(),
        Some("https://github.com/acme/agentuna-backup.git")
    );
    assert_eq!(saved.status, at_db::remote_backup::RemoteBackupStatus::Idle);
    let stored_secret_values: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE instr(value_json, ?1) > 0",
            [token_text],
            |row| row.get(0),
        )
        .expect("inspect settings for secret leakage");
    assert_eq!(stored_secret_values, 0);
    let requests = requests.lock().expect("request lock");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("get /user "));
    assert!(requests[1].starts_with("post /user/repos "));
    assert!(requests.iter().all(|request| request.contains(token_text)));
    assert!(requests.iter().all(|request| {
        !request
            .lines()
            .next()
            .unwrap_or_default()
            .contains(token_text)
    }));
    assert_eq!(network.event_count(), 1);
}

#[test]
fn missing_repo_scope_returns_typed_remedy_without_creating_or_configuring_a_repo() {
    at_mcp::ensure_crypto_provider();
    let (endpoint, requests, server) = fixture_server(vec![Reply {
        status: 200,
        scopes: Some("read:user, gist"),
        body: "{}",
    }]);
    let mut database = Database::open_in_memory().expect("in-memory database");
    at_store::remote_backup::set_device_name(&mut database, Some("fixture-device"))
        .expect("fixture device name");
    let database = Arc::new(Mutex::new(database));
    let network = NetworkState::default();
    let host = authorized_host(&network);
    let client =
        at_git::github::GitHubClient::with_endpoint(&endpoint).expect("loopback GitHub client");
    let token_text = concat!("ghp_", "scope-refused-fixture-012345");
    let secrets = at_secrets::MemoryStore::new();
    at_secrets::save_token(&secrets, token_text).expect("save token to memory-only store");
    let token = create_repo_token(&secrets).expect("read saved token");

    let failure = runtime()
        .block_on(create_private_repo_and_configure(
            Arc::clone(&database),
            client,
            token,
            "agentuna-backup".to_owned(),
            "fixture-device".to_owned(),
            network.clone(),
            host,
        ))
        .expect_err("scope refusal");
    server.join().expect("fixture server");

    assert_eq!(
        failure.code,
        actions::RemoteBackupFailureCode::RepoScopeMissing
    );
    assert_eq!(failure.remedy, actions::RemoteBackupRemedy::AddRepoScope);
    assert!(!failure.to_string().contains(token_text));
    assert!(!format!("{failure:?}").contains(token_text));
    let database = database.lock().expect("database lock");
    let saved = at_db::remote_backup::remote_backup_state(database.connection())
        .expect("unchanged remote-backup state");
    assert_eq!(saved.repo_url, None);
    assert_eq!(
        saved.status,
        at_db::remote_backup::RemoteBackupStatus::Unconfigured
    );
    let stored_secret_values: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE instr(value_json, ?1) > 0",
            [token_text],
            |row| row.get(0),
        )
        .expect("inspect settings for secret leakage");
    assert_eq!(stored_secret_values, 0);
    let requests = requests.lock().expect("request lock");
    assert_eq!(
        requests.len(),
        1,
        "scope refusal must prevent repo creation"
    );
    assert!(requests[0].starts_with("get /user "));
    assert_eq!(network.event_count(), 1);
}
