//! TF-032: a manifest that fails signature verification must cause no archive request.
//!
//! The fixture server binds loopback only; nothing here touches the real network.

use at_app::app::network::NetworkState;
use at_app::app::updater::{UpdateError, UpdateManifestError, check_and_stage_with_key};
use at_model::network::NetworkAction;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Builds a manifest signed by a throwaway key, returning the bytes and the matching public key.
fn signed_fixture(archive_url: &str) -> (Vec<u8>, [u8; 32]) {
    let mut secret = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut secret))
        .expect("test-only OS randomness");
    let signing = SigningKey::from_bytes(&secret);
    let mut value = json!({
        "version": "2.0.0",
        "min_os": "14.0",
        "channel": "stable",
        "url": archive_url,
        "sha256": "a".repeat(64),
        "notes_url": null,
        "published_at": "2026-11-06T00:00:00Z"
    });
    // serde_json maps are key-sorted, so this is the canonical compact form.
    let canonical = serde_json::to_vec(&value).expect("canonical fixture");
    let signature = signing.sign(&canonical);
    value.as_object_mut().expect("object fixture").insert(
        "signature".to_owned(),
        Value::String(
            signature
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        ),
    );
    (
        serde_json::to_vec(&value).expect("fixture JSON"),
        signing.verifying_key().to_bytes(),
    )
}

#[tokio::test]
async fn a_tampered_manifest_makes_only_the_manifest_request() {
    let (fixture, public_key) = signed_fixture("http://127.0.0.1:9/payload.zip");
    let mut tampered: Value = serde_json::from_slice(&fixture).expect("fixture JSON");
    tampered["version"] = Value::String("9.9.9".to_owned());
    let tampered = serde_json::to_vec(&tampered).expect("tampered JSON");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("bound address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("manifest request");
        let mut request = [0_u8; 1024];
        let count = socket.read(&mut request).await.expect("read request");
        let request = String::from_utf8_lossy(&request[..count]).to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            tampered.len()
        );
        socket
            .write_all(reply.as_bytes())
            .await
            .expect("write headers");
        socket.write_all(&tampered).await.expect("write manifest");
        request
    });

    at_mcp::ensure_crypto_provider();
    let network = NetworkState::default();
    let staging = tempfile::tempdir().expect("temporary staging directory");
    let endpoint = format!("http://{address}/manifest.json");
    let result = check_and_stage_with_key(
        &network,
        &reqwest::Client::new(),
        &endpoint,
        "1.0.0",
        false,
        staging.path(),
        &public_key,
    )
    .await;

    assert!(matches!(
        result,
        Err(UpdateError::Manifest(UpdateManifestError::SignatureInvalid))
    ));
    assert!(
        server
            .await
            .expect("fixture server task")
            .starts_with("GET /manifest.json ")
    );
    assert!(
        network
            .events()
            .iter()
            .all(|event| event.action == NetworkAction::UpdateCheck),
        "no archive download may be authorised after a bad signature"
    );
    assert_eq!(
        std::fs::read_dir(staging.path())
            .expect("staging dir")
            .count(),
        0,
        "nothing may be staged"
    );
}
