//! Canonical MCP server model: golden round trips, transport table, opaque keys.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use at_model::{
    EnvValue, McpServer, McpServerDef, McpServerId, McpServerIdError, McpTransport, OAuthProfile,
    Scope, SecretRef,
};
use serde_json::json;

const STDIO_GOLDEN: &str = include_str!("golden/mcp_stdio.json");
const HTTP_GOLDEN: &str = include_str!("golden/mcp_http.json");

fn pretty(server: &McpServer) -> String {
    let mut text = serde_json::to_string_pretty(server).unwrap();
    text.push('\n');
    text
}

fn round_trip_def(def: &McpServerDef) -> McpServerDef {
    let json = serde_json::to_string(def).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn stdio(
    command: &str,
    args: &[&str],
    cwd: Option<&str>,
    env: Vec<(&str, EnvValue)>,
) -> McpTransport {
    McpTransport::Stdio {
        command: command.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: cwd.map(PathBuf::from),
        env: env
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    }
}

fn def(transport: McpTransport) -> McpServerDef {
    McpServerDef {
        display_name: "Fixture".to_owned(),
        description: None,
        transport,
        extra: BTreeMap::new(),
    }
}

#[test]
fn golden_stdio_server_re_serialises_byte_identically() {
    let server: McpServer = serde_json::from_str(STDIO_GOLDEN).unwrap();
    assert_eq!(
        server.id.to_string(),
        "mcp_0192f5c4-1b2c-7e3f-9a4b-5c6d7e8f9a0b"
    );
    assert_eq!(server.scope, Scope::User);
    assert!(server.transport.is_stdio());
    assert!(!server.transport.is_http());
    assert_eq!(pretty(&server), STDIO_GOLDEN);
}

#[test]
fn golden_http_server_re_serialises_byte_identically() {
    let server: McpServer = serde_json::from_str(HTTP_GOLDEN).unwrap();
    assert_eq!(
        server.id.to_string(),
        "mcp_0192f5c4-1b2d-7f40-8b5c-6d7e8f9a0b1c"
    );
    assert_eq!(server.scope, Scope::Project);
    assert!(server.transport.is_http());
    assert!(!server.transport.is_stdio());
    assert_eq!(pretty(&server), HTTP_GOLDEN);
}

#[test]
fn unknown_keys_survive_load_and_save_in_stable_order() {
    let server: McpServer = serde_json::from_str(STDIO_GOLDEN).unwrap();
    assert_eq!(server.extra.len(), 2);
    assert_eq!(server.extra.get("enabled"), Some(&json!(true)));
    assert_eq!(
        server.extra.get("vendor_extension"),
        Some(&json!({"keep": true}))
    );

    let saved = pretty(&server);
    let reloaded: McpServer = serde_json::from_str(&saved).unwrap();
    assert_eq!(reloaded.extra, server.extra);
    assert_eq!(pretty(&reloaded), saved, "second save is byte-identical");

    let enabled = saved.find("\"enabled\"").unwrap();
    let vendor = saved.find("\"vendor_extension\"").unwrap();
    assert!(enabled < vendor, "unknown keys keep their relative order");

    // Arbitrary opaque content, including nested objects and arrays, survives too.
    let mut with_nested = def(stdio("npx", &[], None, Vec::new()));
    with_nested
        .extra
        .insert("nested".to_owned(), json!({"b": [1, null, true], "a": "x"}));
    with_nested.extra.insert("flag".to_owned(), json!(false));
    let back = round_trip_def(&with_nested);
    assert_eq!(back.extra, with_nested.extra);
}

#[test]
fn transport_table_round_trips() {
    let secret = EnvValue::Secret(SecretRef {
        env_name: "API_TOKEN".to_owned(),
        keychain_account: "agentuna/api".to_owned(),
    });

    struct Case {
        name: &'static str,
        transport: McpTransport,
        kind: &'static str,
        args: Vec<&'static str>,
        cwd: Option<&'static str>,
        env_len: usize,
    }

    let cases = vec![
        Case {
            name: "stdio with empty args, empty env and no cwd",
            transport: stdio("uvx", &[], None, Vec::new()),
            kind: "stdio",
            args: vec![],
            cwd: None,
            env_len: 0,
        },
        Case {
            name: "stdio args with spaces, unicode and newlines",
            transport: stdio(
                "node",
                &["server.js", "hello world", "日本語", "line one\nline two"],
                None,
                Vec::new(),
            ),
            kind: "stdio",
            args: vec!["server.js", "hello world", "日本語", "line one\nline two"],
            cwd: None,
            env_len: 0,
        },
        Case {
            name: "stdio with cwd and a secret env entry",
            transport: stdio(
                "./bin/server",
                &["--port", "8080"],
                Some("/opt/agentuna"),
                vec![("API_TOKEN", secret.clone())],
            ),
            kind: "stdio",
            args: vec!["--port", "8080"],
            cwd: Some("/opt/agentuna"),
            env_len: 1,
        },
        Case {
            name: "http without headers or oauth",
            transport: McpTransport::Http {
                url: "https://mcp.example.com".parse().unwrap(),
                headers: BTreeMap::new(),
                oauth: None,
            },
            kind: "http",
            args: vec![],
            cwd: None,
            env_len: 0,
        },
        Case {
            name: "http with a secret header and oauth",
            transport: McpTransport::Http {
                url: "https://mcp.example.com/v1".parse().unwrap(),
                headers: BTreeMap::from([("Authorization".to_owned(), secret.clone())]),
                oauth: Some(OAuthProfile {
                    issuer: "https://auth.example.com".to_owned(),
                    client_id: "agentuna".to_owned(),
                    scopes: vec!["mcp.read".to_owned(), "mcp.write".to_owned()],
                    token_ref: "agentuna/api-oauth".to_owned(),
                }),
            },
            kind: "http",
            args: vec![],
            cwd: None,
            env_len: 1,
        },
    ];

    for case in cases {
        let original = def(case.transport);
        let back = round_trip_def(&original);
        assert_eq!(back, original, "{}", case.name);
        assert_eq!(back.transport.kind(), case.kind, "{}", case.name);

        match &back.transport {
            McpTransport::Stdio { args, cwd, env, .. } => {
                assert!(back.transport.is_stdio(), "{}", case.name);
                assert_eq!(
                    args.iter().map(String::as_str).collect::<Vec<_>>(),
                    case.args,
                    "{}",
                    case.name
                );
                assert_eq!(
                    cwd.as_deref(),
                    case.cwd.map(Path::new),
                    "{} cwd None vs Some must round-trip",
                    case.name
                );
                assert_eq!(env.len(), case.env_len, "{}", case.name);
            }
            McpTransport::Http { headers, .. } => {
                assert!(back.transport.is_http(), "{}", case.name);
                assert_eq!(headers.len(), case.env_len, "{}", case.name);
            }
        }
    }
}

#[test]
fn secrets_are_stored_as_references_only() {
    let secret = EnvValue::Secret(SecretRef {
        env_name: "GITHUB_TOKEN".to_owned(),
        keychain_account: "agentuna/github".to_owned(),
    });
    let value = serde_json::to_value(&secret).unwrap();
    let object = value.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["env_name", "keychain_account"]);
    assert_eq!(object.len(), 2, "a secret holds exactly the two references");

    let debug = format!("{secret:?}");
    assert!(debug.contains("GITHUB_TOKEN"));
    assert!(debug.contains("agentuna/github"));

    // A literal may hold a pasted credential. Neither the value's Debug nor the
    // server's Debug may print it.
    let pasted = EnvValue::Literal("sk-live-SENTINEL".to_owned());
    assert!(!format!("{pasted:?}").contains("sk-live-SENTINEL"));
    let server_def = McpServerDef {
        display_name: "Pasted".to_owned(),
        description: None,
        transport: stdio("node", &["server.js"], None, vec![("TOKEN", pasted)]),
        extra: BTreeMap::new(),
    };
    assert!(!format!("{server_def:?}").contains("sk-live-SENTINEL"));
    assert!(
        serde_json::to_string(&server_def)
            .unwrap()
            .contains("sk-live-SENTINEL")
    );
}

#[test]
fn server_ids_round_trip_and_reject_foreign_values() {
    let ids: Vec<McpServerId> = (0..200).map(|_| McpServerId::new()).collect();
    assert!(
        ids.windows(2).all(|window| window[0] < window[1]),
        "ids must be strictly increasing"
    );

    let text = ids[0].to_string();
    assert!(text.starts_with("mcp_"));
    assert_eq!(text.parse::<McpServerId>().unwrap(), ids[0]);
    let json = serde_json::to_string(&ids[0]).unwrap();
    assert_eq!(serde_json::from_str::<McpServerId>(&json).unwrap(), ids[0]);

    assert!(matches!(
        "mcp_not-a-uuid".parse::<McpServerId>(),
        Err(McpServerIdError::InvalidUuid(_))
    ));
    assert!(matches!(
        "itm_0192f5c4-1b2c-7e3f-9a4b-5c6d7e8f9a0b".parse::<McpServerId>(),
        Err(McpServerIdError::MissingPrefix(_))
    ));
    assert!(matches!(
        "mcp_67e55044-10b1-426f-9247-bb680e5fe0c8".parse::<McpServerId>(),
        Err(McpServerIdError::NotVersion7(_))
    ));
    // A store item id never parses as a server id and vice versa.
    assert!(
        at_model::ItemId::new()
            .to_string()
            .parse::<McpServerId>()
            .is_err()
    );
    assert!(text.parse::<at_model::ItemId>().is_err());
}

#[test]
fn def_conversion_preserves_content() {
    let server: McpServer = serde_json::from_str(STDIO_GOLDEN).unwrap();
    let def = server.clone().into_def();
    assert_eq!(def.display_name, server.display_name);
    assert_eq!(def.extra, server.extra);
    let rebuilt = McpServer::from_def(server.id, server.scope, def);
    assert_eq!(rebuilt, server);
}
