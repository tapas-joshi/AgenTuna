//! Local loopback fixture tests for the skills.sh marketplace client.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use at_git::marketplace::MarketplaceClient;
use at_model::network::{NetworkEventLog, NetworkPolicy};

#[test]
fn searches_local_fixture_then_serves_cached_rows_offline() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept fixture request");
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).expect("read fixture request");
        let body = br#"{"data":[{"id":"octo/tools/plan","name":"Plan","description":"A test skill","installs":42,"installUrl":"https://github.com/octo/tools"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("write response headers");
        stream.write_all(body).expect("write response body");
    });
    let directory = tempfile::tempdir().expect("temp cache");
    let cache = directory.path().join("marketplace.json");
    let client =
        MarketplaceClient::with_endpoint(&format!("http://{address}/api/v1/skills/search"))
            .expect("fixture client");
    let policy = NetworkPolicy::default().with_marketplace_enabled(true);
    let mut log = NetworkEventLog::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let page = runtime
        .block_on(client.search("  Plan  ", &cache, &policy, &mut log))
        .expect("marketplace response");
    assert_eq!(page.skills.len(), 1);
    assert_eq!(page.skills[0].installs, 42);
    assert_eq!(page.skills[0].source_url, "https://github.com/octo/tools");
    assert_eq!(log.entries().len(), 1);
    server.join().expect("server thread");

    let offline = NetworkPolicy::default()
        .with_marketplace_enabled(true)
        .with_offline(true);
    let cached = runtime
        .block_on(client.search("plan", &cache, &offline, &mut log))
        .expect("offline cache");
    assert!(cached.offline);
    assert_eq!(cached.skills, page.skills);
    assert_eq!(log.entries().len(), 1);
}
