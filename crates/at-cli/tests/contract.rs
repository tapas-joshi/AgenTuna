//! TU-033: the machine contract every command group must satisfy.
//!
//! These tests freeze the envelope shape, the error codes and the exit-code table, and
//! prove that every documented exit code is reachable through the shared helpers.

mod support;

use std::collections::BTreeSet;
use std::ffi::OsString;

use at_cli::output::envelope_err;
use at_cli::{
    CliError, Color, CommandGroup, CommandOutput, ErrorCode, GlobalArgs, Registry, Stdio,
};
use at_cli::{ENVELOPE_FIELDS, ERROR_FIELDS, SCHEMA_VERSION, exit_code};
use clap::{ArgMatches, Command};
use serde_json::Value;

use support::{Sandbox, agentuna, exit_code as process_exit, stdout};

/// Version-1 envelope fields, frozen here on purpose.
///
/// Adding a field is allowed within version 1, so a new name does not have to appear in
/// this list. Removing or renaming one does: the envelope would no longer carry the
/// field, this assertion fails, and the only way forward is `schema_version: 2` with a
/// new frozen set.
const FROZEN_V1_ENVELOPE: [&str; 6] = [
    "schema_version",
    "command",
    "ok",
    "data",
    "warnings",
    "error",
];

/// Version-1 `error` object fields, frozen for the same reason.
const FROZEN_V1_ERROR: [&str; 4] = ["code", "message", "details", "hint"];

/// A command group that fails with one error class, used to walk every exit code.
struct Failing {
    noun: &'static str,
    code: ErrorCode,
}

impl CommandGroup for Failing {
    fn name(&self) -> &'static str {
        self.noun
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(Command::new(self.noun).about("Contract test path"))
    }

    fn dispatch(
        &self,
        _globals: &GlobalArgs,
        _matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        Err(CliError::new(
            self.code,
            format!("`{}` failed on purpose for the contract test", self.noun),
            "apply the documented fix and rerun the command",
        ))
    }
}

/// Every error class with the noun that produces it.
const FAILING_NOUNS: [(&str, ErrorCode); 11] = [
    ("usage", ErrorCode::Usage),
    ("validation", ErrorCode::Validation),
    ("not_found", ErrorCode::NotFound),
    ("locked", ErrorCode::Locked),
    ("conflict", ErrorCode::Conflict),
    ("partial", ErrorCode::Partial),
    ("doctor_failure", ErrorCode::DoctorFailure),
    ("confirmation_required", ErrorCode::ConfirmationRequired),
    ("network_required", ErrorCode::NetworkRequired),
    ("not_implemented", ErrorCode::NotImplemented),
    ("internal", ErrorCode::Internal),
];

fn failing_registry() -> Registry {
    let groups: Vec<Box<dyn CommandGroup>> = FAILING_NOUNS
        .iter()
        .map(|(noun, code)| Box::new(Failing { noun, code: *code }) as Box<dyn CommandGroup>)
        .collect();
    Registry::try_new(groups).expect("the contract registry builds")
}

/// Runs the library entry point and captures both streams.
fn run(registry: &Registry, args: &[&str]) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut argv: Vec<OsString> = vec![OsString::from("agentuna")];
    argv.extend(args.iter().map(OsString::from));
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
        String::from_utf8(out).expect("stdout is UTF-8"),
        String::from_utf8(err).expect("stderr is UTF-8"),
    )
}

/// Parses stdout, requiring exactly one JSON value.
fn exactly_one_object(text: &str) -> Value {
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let first = stream
        .next()
        .unwrap_or_else(|| panic!("stdout held no JSON value: {text:?}"))
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {text:?}"));
    assert!(
        stream.next().is_none(),
        "JSON mode must write exactly one object: {text:?}"
    );
    first
}

/// Asserts the frozen contract on one envelope.
fn assert_contract(envelope: &Value) {
    let object = envelope
        .as_object()
        .unwrap_or_else(|| panic!("an envelope is a JSON object: {envelope}"));
    for field in FROZEN_V1_ENVELOPE {
        assert!(
            object.contains_key(field),
            "the envelope lost `{field}`; removing or renaming a field needs schema_version 2"
        );
    }
    assert_eq!(
        ENVELOPE_FIELDS,
        FROZEN_V1_ENVELOPE.as_slice(),
        "the published ENVELOPE_FIELDS must match the frozen version-1 set"
    );
    assert_eq!(
        ERROR_FIELDS,
        FROZEN_V1_ERROR.as_slice(),
        "the published ERROR_FIELDS must match the frozen version-1 set"
    );

    let version = object["schema_version"]
        .as_u64()
        .expect("schema_version is a number");
    assert_eq!(
        version,
        u64::from(SCHEMA_VERSION),
        "the envelope must declare the version this build emits"
    );
    assert_eq!(
        version, 1,
        "schema_version {version} has no frozen field set; add one to this test"
    );

    assert!(object["ok"].is_boolean(), "`ok` is a boolean: {envelope}");
    assert!(
        object["command"].is_string(),
        "`command` is a string: {envelope}"
    );
    let warnings = object["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("`warnings` is an array: {envelope}"));
    for warning in warnings {
        assert!(warning["code"].is_string(), "warnings carry a code");
        assert!(warning["message"].is_string(), "warnings carry a message");
    }

    if object["ok"] == Value::Bool(false) {
        let error = object["error"]
            .as_object()
            .unwrap_or_else(|| panic!("a failed run carries an error object: {envelope}"));
        for field in FROZEN_V1_ERROR {
            assert!(
                error.contains_key(field),
                "the error object lost `{field}`; removing or renaming a field needs schema_version 2"
            );
        }
        assert!(
            error["code"].is_string(),
            "the error carries a machine code"
        );
        assert!(
            error["hint"]
                .as_str()
                .is_some_and(|hint| !hint.trim().is_empty()),
            "every error carries a non-empty hint: {envelope}"
        );
    } else {
        assert!(
            object["error"].is_null(),
            "a successful run carries a null error: {envelope}"
        );
    }
}

#[test]
fn every_documented_exit_code_is_produced_by_a_golden_path() {
    let registry = failing_registry();
    let mut seen = BTreeSet::new();

    for (noun, code) in FAILING_NOUNS {
        let (status, out, err) = run(&registry, &["--json", noun]);
        assert_eq!(
            status,
            code.exit_code(),
            "`{noun}` must exit with {}: {err}",
            code.exit_code()
        );
        assert!(
            !out.contains("error:"),
            "stdout carries the envelope only: {out}"
        );
        assert!(
            err.contains("error:") && err.contains("hint:"),
            "diagnostics with a hint go to stderr: {err}"
        );

        let envelope = exactly_one_object(&out);
        assert_contract(&envelope);
        assert_eq!(envelope["command"], noun);
        assert_eq!(envelope["ok"], Value::Bool(false));
        assert_eq!(envelope["error"]["code"], code.as_str());
        assert!(envelope["data"].is_null(), "an error carries no payload");
        seen.insert(status);
    }

    // Success, through the real `version` command.
    let (status, out, err) = run(&registry, &["--json", "version"]);
    assert_eq!(status, exit_code::SUCCESS);
    assert!(err.is_empty(), "a successful run writes nothing to stderr");
    let envelope = exactly_one_object(&out);
    assert_contract(&envelope);
    assert_eq!(envelope["ok"], Value::Bool(true));
    assert_eq!(envelope["command"], "version");
    seen.insert(status);

    // Usage, through clap's own refusal.
    let (status, out, _) = run(&registry, &["--json", "does-not-exist"]);
    assert_eq!(status, exit_code::USAGE);
    assert_contract(&exactly_one_object(&out));
    seen.insert(status);

    let documented: BTreeSet<i32> = exit_code::TABLE.iter().map(|row| row.code).collect();
    assert_eq!(
        seen, documented,
        "every documented exit code needs a golden path here, and every path needs a documented code"
    );
}

#[test]
fn the_exit_code_table_is_unique_and_ordered() {
    let mut codes = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut previous = -1;
    for row in exit_code::TABLE {
        assert!(
            codes.insert(row.code),
            "exit code {} is listed twice",
            row.code
        );
        assert!(
            names.insert(row.name),
            "exit code name `{}` is reused",
            row.name
        );
        assert!(
            !row.meaning.trim().is_empty(),
            "{} has no meaning",
            row.code
        );
        assert!(
            row.code > previous,
            "the table must ascend by code, so it can be read and diffed"
        );
        previous = row.code;
        assert!(exit_code::is_documented(row.code));
    }
    assert!(!exit_code::is_documented(1), "1 is not a documented code");
    assert!(!exit_code::is_documented(42));
}

#[test]
fn error_codes_are_frozen() {
    let frozen: [(ErrorCode, &str, i32); 11] = [
        (ErrorCode::Usage, "usage", exit_code::USAGE),
        (ErrorCode::Validation, "validation", exit_code::VALIDATION),
        (ErrorCode::NotFound, "not_found", exit_code::NOT_FOUND),
        (ErrorCode::Locked, "locked", exit_code::LOCKED),
        (ErrorCode::Conflict, "conflict", exit_code::CONFLICT),
        (ErrorCode::Partial, "partial", exit_code::PARTIAL),
        (
            ErrorCode::DoctorFailure,
            "doctor_failed",
            exit_code::DOCTOR_FAILURE,
        ),
        (
            ErrorCode::ConfirmationRequired,
            "confirmation_required",
            exit_code::CONFIRMATION_REQUIRED,
        ),
        (
            ErrorCode::NetworkRequired,
            "network_required",
            exit_code::NETWORK_REQUIRED,
        ),
        (
            ErrorCode::NotImplemented,
            "not_implemented",
            exit_code::NOT_IMPLEMENTED,
        ),
        (ErrorCode::Internal, "internal", exit_code::INTERNAL),
    ];

    assert_eq!(
        ErrorCode::ALL.len(),
        frozen.len(),
        "a new error code needs a frozen entry here, and the envelope code string is the contract"
    );
    for (index, (code, name, status)) in frozen.iter().enumerate() {
        assert_eq!(ErrorCode::ALL[index], *code, "ErrorCode::ALL changed order");
        assert_eq!(
            code.as_str(),
            *name,
            "error code `{name}` was renamed; codes are frozen"
        );
        assert_eq!(code.exit_code(), *status);
        assert!(
            exit_code::is_documented(code.exit_code()),
            "`{name}` maps to an undocumented exit code"
        );
    }
}

#[test]
fn every_error_object_has_a_code_and_a_non_empty_hint() {
    for code in ErrorCode::ALL {
        let error = CliError::new(
            *code,
            "a message for the contract test",
            "do the documented thing",
        );
        assert!(!error.hint().trim().is_empty());
        let envelope = envelope_err("probe", &error);
        assert_contract(&envelope);
        assert_eq!(envelope["error"]["code"], code.as_str());
        assert_eq!(envelope["error"]["message"], error.message());
        assert!(envelope["error"]["details"].is_null());
    }

    // An empty hint cannot reach the envelope; the constructor substitutes one.
    let empty = CliError::new(ErrorCode::Internal, "no hint given", "");
    assert!(
        !empty.hint().trim().is_empty(),
        "the constructor must substitute a hint so the contract cannot be violated"
    );
}

#[test]
fn a_failed_report_still_carries_its_payload() {
    let error = CliError::new(ErrorCode::DoctorFailure, "one check failed", "fix it")
        .with_data(serde_json::json!({ "findings": [{ "code": "database_corrupt" }] }))
        .with_report("report body\n");
    let envelope = envelope_err("doctor", &error);
    assert_contract(&envelope);
    assert_eq!(envelope["ok"], Value::Bool(false));
    assert_eq!(
        envelope["data"]["findings"][0]["code"], "database_corrupt",
        "a report-style command keeps its payload in `data` even when it fails"
    );
    assert_eq!(error.report(), Some("report body\n"));
}

#[test]
fn every_registered_command_answers_help_and_a_smoke_call() {
    let sandbox = Sandbox::new();
    let registry = at_cli::production_registry().expect("the shipped registry builds");

    // Out of tree, the smoke invocation per built-in command that needs arguments.
    let mut cases: Vec<(&str, Vec<&str>, i32)> = vec![
        ("version", vec!["version"], exit_code::SUCCESS),
        (
            "completions",
            vec!["completions", "zsh"],
            exit_code::SUCCESS,
        ),
        (
            "projects",
            vec!["projects", "list"],
            exit_code::NOT_IMPLEMENTED,
        ),
        ("scan", vec!["scan"], exit_code::NOT_IMPLEMENTED),
        ("doctor", vec!["doctor"], exit_code::SUCCESS),
    ];
    for noun in registry.names() {
        cases.push((noun, vec![noun], -1));
    }

    for (noun, args, expected) in cases {
        let help = agentuna(&sandbox, &[noun, "--help"]);
        assert_eq!(
            process_exit(&help),
            exit_code::SUCCESS,
            "`{noun} --help` must succeed: {}",
            support::stderr(&help)
        );
        assert!(!stdout(&help).is_empty(), "`{noun} --help` printed nothing");

        let mut with_json = vec!["--json"];
        with_json.extend(args.iter().copied());
        let smoke = agentuna(&sandbox, &with_json);
        let status = process_exit(&smoke);
        assert!(
            exit_code::is_documented(status),
            "`{args:?}` exited with the undocumented code {status}: {}",
            support::stderr(&smoke)
        );
        if expected >= 0 {
            assert_eq!(
                status,
                expected,
                "`{args:?}` must exit with {expected}: {}",
                support::stderr(&smoke)
            );
        }

        let envelope = exactly_one_object(&stdout(&smoke));
        assert_contract(&envelope);
        assert!(
            envelope["command"]
                .as_str()
                .is_some_and(|command| command.starts_with(noun)),
            "the envelope names the command that ran: {envelope}"
        );
    }
}

#[test]
fn json_mode_keeps_stdout_parseable_for_every_built_in() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["--json", "version"],
        vec!["--json", "doctor"],
        vec!["--json", "scan"],
        vec!["--json", "projects", "list"],
        vec!["--json", "completions", "bash"],
        vec!["--json", "nope"],
    ] {
        let output = agentuna(&sandbox, &args);
        let text = stdout(&output);
        let envelope = exactly_one_object(&text);
        assert_contract(&envelope);
        assert!(
            exit_code::is_documented(process_exit(&output)),
            "`{args:?}` exited with an undocumented code"
        );
    }
}

#[test]
fn help_is_text_for_humans_and_one_envelope_in_json_mode() {
    let sandbox = Sandbox::new();

    // clap renders help itself; a human gets the text and exit 0.
    let human = agentuna(&sandbox, &["--help"]);
    assert_eq!(process_exit(&human), exit_code::SUCCESS);
    assert!(stdout(&human).contains("Usage:"));
    assert!(
        serde_json::from_str::<Value>(&stdout(&human)).is_err(),
        "help is text, not an envelope"
    );

    // JSON mode promises exactly one parseable object, help included.
    let machine = agentuna(&sandbox, &["--json", "--help"]);
    assert_eq!(process_exit(&machine), exit_code::SUCCESS);
    let envelope = exactly_one_object(&stdout(&machine));
    assert_contract(&envelope);
    assert_eq!(envelope["ok"], Value::Bool(true));
    assert!(
        envelope["data"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Usage:")),
        "the help text travels in `data.text`: {envelope}"
    );

    let version = agentuna(&sandbox, &["--json", "--version"]);
    assert_eq!(process_exit(&version), exit_code::SUCCESS);
    let envelope = exactly_one_object(&stdout(&version));
    assert_contract(&envelope);
    assert_eq!(envelope["command"], "version");
}
