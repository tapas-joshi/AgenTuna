//! The extension seam other slices use: attach a group, dispatch through the root.

use std::ffi::OsString;

use at_cli::error::CliError;
use at_cli::globals::GlobalArgs;
use at_cli::output::{Color, CommandOutput, Stdio};
use at_cli::registry::{CommandGroup, Registry, RegistryError};
use clap::{ArgMatches, Command};

/// A stand-in for an owning slice: one noun, an optional nested noun, no engine.
struct FakeGroup {
    noun: &'static str,
    nested: bool,
}

impl CommandGroup for FakeGroup {
    fn name(&self) -> &'static str {
        self.noun
    }

    fn attach(&self, root: Command) -> Command {
        let mut group = Command::new(self.noun)
            .about("Fake group")
            .subcommand(Command::new("list").about("List items"));
        if self.nested {
            group = group.subcommand(
                Command::new("secret")
                    .about("Nested nouns")
                    .subcommand(Command::new("list").about("List secrets")),
            );
        }
        root.subcommand(group)
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let verb = matches.subcommand_name().unwrap_or("none");
        let scope = globals.scope.map(|scope| scope.as_str());
        Ok(CommandOutput::new(
            serde_json::json!({
                "verb": verb,
                "json": globals.json,
                "scope": scope,
                "project": globals.project.as_ref().map(|path| path.display().to_string()),
            }),
            format!("{} {verb} json={}\n", self.noun, globals.json),
        ))
    }
}

fn registry_with(groups: Vec<FakeGroup>) -> Registry {
    let boxed: Vec<Box<dyn CommandGroup>> = groups
        .into_iter()
        .map(|group| Box::new(group) as Box<dyn CommandGroup>)
        .collect();
    Registry::try_new(boxed).expect("registry accepts the fake groups")
}

/// Runs the CLI in-process and returns the exit code with both captured streams.
fn run_cli(registry: &Registry, args: &[&str]) -> (i32, String, String) {
    let argv: Vec<OsString> = std::iter::once(OsString::from("agentuna"))
        .chain(args.iter().map(OsString::from))
        .collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut stdio = Stdio {
            out: &mut out,
            err: &mut err,
            color: Color::Plain,
        };
        at_cli::run(registry, &argv, &mut stdio)
    };
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

fn command_names(registry: &Registry) -> Vec<String> {
    registry
        .command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect()
}

#[test]
fn a_registered_group_joins_the_tree_and_dispatches() {
    let registry = registry_with(vec![FakeGroup {
        noun: "skills",
        nested: false,
    }]);

    let names = command_names(&registry);
    assert!(names.contains(&"skills".to_owned()), "tree was {names:?}");
    assert!(names.contains(&"version".to_owned()));

    let help = run_cli(&registry, &["--help"]);
    assert_eq!(help.0, 0);
    assert!(
        help.1
            .lines()
            .any(|line| line.trim_start().starts_with("skills")),
        "help must list the attached group: {}",
        help.1
    );

    let (code, out, err) = run_cli(&registry, &["skills", "list"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out, "skills list json=false\n");
    assert!(err.is_empty());
}

#[test]
fn nested_nouns_and_globals_after_them_are_parsed() {
    let registry = registry_with(vec![FakeGroup {
        noun: "mcp",
        nested: true,
    }]);

    let (code, out, err) = run_cli(&registry, &["mcp", "secret", "list", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");

    let envelope: serde_json::Value = serde_json::from_str(&out).expect("one JSON object");
    assert_eq!(envelope["command"], "mcp secret list");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(true));
    assert_eq!(envelope["data"]["verb"], "secret");
    assert_eq!(envelope["data"]["json"], serde_json::Value::Bool(true));
    assert!(err.is_empty(), "diagnostics only, got {err}");
}

#[test]
fn global_scope_reaches_a_foreign_group_before_and_after_the_noun() {
    let registry = registry_with(vec![FakeGroup {
        noun: "skills",
        nested: false,
    }]);

    for args in [
        vec!["--scope", "local", "skills", "list", "--json"],
        vec!["skills", "list", "--scope", "local", "--json"],
        vec!["skills", "--scope", "local", "list", "--json"],
    ] {
        let (code, out, err) = run_cli(&registry, &args);
        assert_eq!(code, 0, "args {args:?} stderr: {err}");
        let envelope: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(
            envelope["data"]["scope"], "local",
            "args {args:?} lost the scope flag: {out}"
        );
    }
}

#[test]
fn an_unregistered_group_is_absent_rather_than_a_stub() {
    let registry = registry_with(Vec::new());
    let names = command_names(&registry);
    assert!(!names.contains(&"skills".to_owned()), "tree was {names:?}");

    let help = run_cli(&registry, &["--help"]);
    assert_eq!(help.0, 0);
    assert!(
        !help
            .1
            .lines()
            .any(|line| line.trim_start().starts_with("skills")),
        "unregistered nouns must not be listed: {}",
        help.1
    );

    let (code, out, err) = run_cli(&registry, &["skills", "list"]);
    assert_eq!(code, 2, "an absent group is a usage error");
    assert!(out.is_empty());
    assert!(err.contains("unrecognized subcommand"), "stderr: {err}");
}

#[test]
fn duplicate_group_names_are_rejected() {
    let result = Registry::try_new(vec![
        Box::new(FakeGroup {
            noun: "skills",
            nested: false,
        }),
        Box::new(FakeGroup {
            noun: "skills",
            nested: true,
        }),
    ]);
    match result {
        Err(RegistryError::Duplicate { name }) => assert_eq!(name, "skills"),
        Err(other) => panic!("expected a duplicate error, got {other:?}"),
        Ok(_) => panic!("duplicate nouns must not build a registry"),
    }
}

#[test]
fn a_group_cannot_shadow_a_built_in_command() {
    for noun in [
        "version",
        "projects",
        "scan",
        "doctor",
        "completions",
        "help",
    ] {
        let result = Registry::try_new(vec![Box::new(FakeGroup {
            noun,
            nested: false,
        })]);
        match result {
            Err(RegistryError::Reserved { name }) => assert_eq!(name, noun),
            Err(other) => panic!("expected a reserved error for {noun}, got {other:?}"),
            Ok(_) => panic!("`{noun}` is a built-in command and must stay reserved"),
        }
    }
}

#[test]
fn a_group_error_keeps_its_typed_exit_code() {
    struct Failing;

    impl CommandGroup for Failing {
        fn name(&self) -> &'static str {
            "broken"
        }

        fn attach(&self, root: Command) -> Command {
            root.subcommand(
                Command::new("broken")
                    .about("Always fails")
                    .subcommand(Command::new("run")),
            )
        }

        fn dispatch(
            &self,
            _globals: &GlobalArgs,
            _matches: &ArgMatches,
        ) -> Result<CommandOutput, CliError> {
            Err(CliError::not_implemented("broken", "TU-000"))
        }
    }

    let registry =
        Registry::try_new(vec![Box::new(Failing)]).expect("registry accepts the failing group");

    let (code, out, err) = run_cli(&registry, &["broken", "run"]);
    assert_eq!(code, 11);
    assert!(out.is_empty());
    assert!(err.contains("not implemented"), "stderr: {err}");

    let (code, out, err) = run_cli(&registry, &["broken", "run", "--json"]);
    assert_eq!(code, 11);
    let envelope: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(envelope["error"]["code"], "not_implemented");
    assert_eq!(envelope["command"], "broken run");
    assert!(err.contains("error:"));
}

#[test]
fn warnings_reach_the_stream_the_mode_expects() {
    struct Noisy;

    impl CommandGroup for Noisy {
        fn name(&self) -> &'static str {
            "noisy"
        }

        fn attach(&self, root: Command) -> Command {
            root.subcommand(
                Command::new("noisy")
                    .about("Always warns")
                    .subcommand(Command::new("run")),
            )
        }

        fn dispatch(
            &self,
            _globals: &GlobalArgs,
            _matches: &ArgMatches,
        ) -> Result<CommandOutput, CliError> {
            Ok(CommandOutput::human_only("done\n").with_warnings(vec![
                at_cli::output::Warning::new(
                    "already_registered",
                    "the path was already registered",
                ),
            ]))
        }
    }

    let registry = Registry::try_new(vec![Box::new(Noisy)]).expect("registry");

    let (code, out, err) = run_cli(&registry, &["noisy", "run"]);
    assert_eq!(code, 0);
    assert_eq!(out, "done\n", "stdout stays the command's own text");
    assert!(
        err.contains("warning: the path was already registered"),
        "stderr: {err}"
    );

    let (code, out, err) = run_cli(&registry, &["noisy", "run", "--json"]);
    assert_eq!(code, 0);
    let envelope: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(envelope["warnings"][0]["code"], "already_registered");
    assert_eq!(envelope["data"], serde_json::Value::Null);
    assert!(err.is_empty(), "warnings live in the envelope in JSON mode");
}

#[test]
fn no_command_shadows_a_global_argument_id() {
    // The global flags are propagated into every subcommand; an argument with the same
    // id but a different type panics when the value is read back.
    const GLOBAL_IDS: &[&str] = &["json", "project", "scope", "yes", "verbose", "no_color"];

    fn walk(command: &clap::Command, path: &str) {
        for sub in command.get_subcommands() {
            let name = format!("{path} {}", sub.get_name());
            for id in GLOBAL_IDS {
                if let Some(arg) = sub.get_arguments().find(|arg| arg.get_id() == *id) {
                    assert!(
                        arg.is_global_set(),
                        "`{name}` declares `{id}`, which is reserved by a global flag"
                    );
                }
            }
            walk(sub, &name);
        }
    }

    let registry = registry_with(vec![FakeGroup {
        noun: "skills",
        nested: true,
    }]);
    walk(&registry.command(), "agentuna");
    walk(
        &at_cli::production_registry().expect("registry").command(),
        "agentuna",
    );
}
