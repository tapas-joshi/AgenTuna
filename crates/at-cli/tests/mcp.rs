//! TM-027: the MCP command surface is registered and secret values are not arguments.

use std::ffi::OsString;

use at_cli::{Color, Stdio, production_registry, run};

#[test]
fn production_tree_contains_all_mcp_and_secret_operations() {
    let registry = production_registry().expect("production command tree");
    let command = registry.command();
    let mcp = command
        .get_subcommands()
        .find(|command| command.get_name() == "mcp")
        .expect("mcp group registered");
    for name in ["list", "show", "add", "edit", "remove", "test", "secret"] {
        assert!(
            mcp.get_subcommands()
                .any(|command| command.get_name() == name),
            "missing mcp {name} command"
        );
    }
    let secret = mcp
        .get_subcommands()
        .find(|command| command.get_name() == "secret")
        .expect("nested secret group");
    for name in ["list", "set", "rm"] {
        assert!(
            secret
                .get_subcommands()
                .any(|command| command.get_name() == name),
            "missing mcp secret {name} command"
        );
    }
}

#[test]
fn secret_set_rejects_and_redacts_a_value_passed_as_an_argument() {
    let registry = production_registry().expect("production command tree");
    let args = [
        OsString::from("agentuna"),
        OsString::from("mcp"),
        OsString::from("secret"),
        OsString::from("set"),
        OsString::from("OPENAI_API_KEY"),
        OsString::from("sk-sentinel-must-never-be-argv"),
        OsString::from("--json"),
    ];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = {
        let mut stdio = Stdio {
            out: &mut out,
            err: &mut err,
            color: Color::Plain,
        };
        run(&registry, &args, &mut stdio)
    };
    assert_eq!(status, 2);
    let stdout = String::from_utf8(out).expect("JSON stdout");
    let stderr = String::from_utf8(err).expect("diagnostic stderr");
    assert!(!stdout.contains("sk-sentinel-must-never-be-argv"));
    assert!(!stderr.contains("sk-sentinel-must-never-be-argv"));
    let envelope: serde_json::Value = serde_json::from_str(&stdout).expect("error envelope");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "usage");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stdin")
    );
}

#[test]
fn mcp_add_accepts_dry_run_only_through_shared_global_flag() {
    let command = production_registry()
        .expect("production command tree")
        .command();
    let matches = command
        .try_get_matches_from(["agentuna", "mcp", "add", "--dry-run", "--json"])
        .expect("shared global flags propagate to MCP commands");
    let (noun, matches) = matches.subcommand().expect("noun");
    assert_eq!(noun, "mcp");
    let (subcommand, _) = matches.subcommand().expect("MCP operation");
    assert_eq!(subcommand, "add");
}
