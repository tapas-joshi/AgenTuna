//! Secret redaction: known values, env assignments, credential shapes, export paths.

use std::time::{Duration, Instant};

use at_model::error::{ErrorKind, ErrorReport};
use at_model::redact::{MIN_VALUE_LEN, Redactor};
use at_model::{REDACTION_MASK, SecretRedactor};
use serde_json::json;

/// A value registered with the session and expected never to survive a redaction.
const SENTINEL: &str = "sk-live-0123456789abcdef";

fn loaded() -> Redactor {
    Redactor::new()
        .with_names(["GITHUB_TOKEN"])
        .with_values([SENTINEL])
}

#[test]
fn a_known_value_is_masked_and_a_reference_is_not() {
    let redactor = loaded();
    let text = format!("token {SENTINEL} and ${{GITHUB_TOKEN}} and ${{GITHUB_TOKEN:-none}}");

    let redacted = redactor.redact_text(&text);

    assert_eq!(
        redacted,
        "token *** and ${GITHUB_TOKEN} and ${GITHUB_TOKEN:-none}"
    );
    assert!(!redacted.contains(SENTINEL));
}

/// One row per supported credential shape: label, input, expected output.
const SHAPES: &[(&str, &str, &str)] = &[
    ("ghp_", "ghp_16C7e42F292c6912E7710c838347Ae178B4a", "***"),
    ("gho_", "gho_16C7e42F292c6912E7710c838347Ae178B4a", "***"),
    ("ghu_", "ghu_16C7e42F292c6912E7710c838347Ae178B4a", "***"),
    ("ghs_", "ghs_16C7e42F292c6912E7710c838347Ae178B4a", "***"),
    ("ghr_", "ghr_16C7e42F292c6912E7710c838347Ae178B4a", "***"),
    (
        "github_pat_",
        "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz",
        "***",
    ),
    ("glpat-", "glpat-abcdefghij1234567890", "***"),
    ("sk-", "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789", "***"),
    ("sk-ant-", "sk-ant-api03-abcdefghijklmnopqrstuvwxyz", "***"),
    // Prefix is split with `concat!` so secret scanners do not flag the fixture.
    (
        "sk_live_",
        concat!("sk_li", "ve_abcdefghijklmnopqrstuvwxyz"),
        "***",
    ),
    (
        "sk_test_",
        concat!("sk_te", "st_abcdefghijklmnopqrstuvwxyz"),
        "***",
    ),
    (
        "rk_live_",
        concat!("rk_li", "ve_abcdefghijklmnopqrstuvwxyz"),
        "***",
    ),
    ("xoxa-", "xoxa-1234567890-abcdefghij", "***"),
    ("xoxb-", "xoxb-1234567890-abcdefghij", "***"),
    ("xoxp-", "xoxp-1234567890-abcdefghij", "***"),
    ("xoxr-", "xoxr-1234567890-abcdefghij", "***"),
    ("xoxs-", "xoxs-1234567890-abcdefghij", "***"),
    ("AKIA", "AKIAIOSFODNN7EXAMPLE", "***"),
    ("ASIA", "ASIAIOSFODNN7EXAMPLE", "***"),
    ("AIza", "AIzaSyA1234567890abcdefghijklmnopqrstu", "***"),
    ("Bearer", "Bearer abcdefghij0123456789", "Bearer ***"),
    (
        "jwt",
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        "***",
    ),
    ("npm_", "npm_abcdefghijklmnopqrstuvwxyz0123456789", "***"),
    ("hf_", "hf_abcdefghijklmnopqrstuvwxyz0123456789", "***"),
    ("pypi-", "pypi-abcdefghijklmnopqrstuvwxyz0123456789", "***"),
    (
        "dckr_pat_",
        "dckr_pat_abcdefghijklmnopqrstuvwxyz0123456789",
        "***",
    ),
    (
        "url credentials",
        "https://user:s3cr3tpassword@example.com/repo",
        "https://user:***@example.com/repo",
    ),
];

#[test]
fn twenty_credential_shapes_are_masked_in_a_table_driven_test() {
    assert!(
        SHAPES.len() >= 20,
        "the table must cover at least 20 shapes"
    );
    let redactor = Redactor::new();
    for (label, input, expected) in SHAPES {
        assert_eq!(&redactor.redact_text(input), expected, "shape `{label}`");
        assert!(
            redactor.contains_secret(input),
            "shape `{label}` must be reported as a secret"
        );
    }
}

#[test]
fn ordinary_words_of_six_or_more_letters_are_not_masked() {
    const ORDINARY: &[&str] = &[
        "documentation",
        "scikit-learn",
        "Bearer",
        "AKIA",
        "eyJhbGciOiJIUzI1NiJ9",
        "token",
        "secret",
        "password",
        "teamwork",
        "signal",
        "github",
        "key",
    ];

    let empty = Redactor::new();
    let loaded = loaded();
    for word in ORDINARY {
        assert_eq!(empty.redact_text(word), *word, "word `{word}`");
        assert_eq!(loaded.redact_text(word), *word, "word `{word}`");
        assert!(!loaded.contains_secret(word), "word `{word}`");
    }
}

#[test]
fn near_misses_stay_visible() {
    let redactor = Redactor::new();
    // Body shorter than the eight-byte floor.
    assert_eq!(redactor.redact_text("sk-learn"), "sk-learn");
    assert_eq!(redactor.redact_text("AKIA1234"), "AKIA1234");
    assert_eq!(redactor.redact_text("Bearer short"), "Bearer short");
    assert_eq!(redactor.redact_text("xoxb-1234"), "xoxb-1234");
    // A JSON Web Token needs three segments.
    assert_eq!(
        redactor.redact_text("eyJhbGciOiJIUzI1NiJ9"),
        "eyJhbGciOiJIUzI1NiJ9"
    );
    // A URL without userinfo, or with a path before the host, is not credentials.
    assert_eq!(
        redactor.redact_text("https://example.com/path:no-user@host"),
        "https://example.com/path:no-user@host"
    );
    assert_eq!(
        redactor.redact_text("https://user@example.com"),
        "https://user@example.com"
    );
}

#[test]
fn quotes_and_punctuation_around_a_token_survive() {
    let redactor = Redactor::new();
    assert_eq!(
        redactor.redact_text("token: 'ghp_16C7e42F292c6912E7710c838347Ae178B4a'."),
        "token: '***'."
    );
}

#[test]
fn short_values_are_masked_only_in_the_assignment_form() {
    let redactor = Redactor::new()
        .with_names(["API_TOKEN", "GITHUB_TOKEN"])
        .with_values(["abc12"]);

    assert_eq!(
        redactor.value_count(),
        0,
        "values under the floor are rejected"
    );
    assert_eq!(redactor.redact_text("value abc12 here"), "value abc12 here");
    assert_eq!(redactor.redact_text("API_TOKEN=abc12"), "API_TOKEN=***");
    assert_eq!(
        redactor.redact_text("export API_TOKEN=\"abc12\""),
        "export API_TOKEN=\"***\""
    );

    let long = Redactor::new().with_values(["abc123"]);
    assert_eq!(long.redact_text("x abc123 y"), "x *** y");
}

#[test]
fn values_shorter_than_the_floor_are_rejected() {
    let mut redactor = Redactor::new();
    assert_eq!(MIN_VALUE_LEN, 6);
    assert!(!redactor.insert_value("12345"));
    assert!(redactor.insert_value("123456"));
    assert_eq!(redactor.value_count(), 1);
    assert_eq!(
        redactor.redact_text("pin 12345 tag 123456"),
        "pin 12345 tag ***"
    );
}

#[test]
fn assignments_use_registered_names_and_leave_references_alone() {
    let redactor = Redactor::new().with_names(["GITHUB_TOKEN"]);

    assert_eq!(redactor.redact_text("RUST_LOG=info"), "RUST_LOG=info");
    assert_eq!(redactor.redact_text("GITHUB_TOKEN=abc"), "GITHUB_TOKEN=***");
    assert_eq!(
        redactor.redact_text("GITHUB_TOKEN=abc def"),
        "GITHUB_TOKEN=*** def"
    );
    assert_eq!(
        redactor.redact_text("env: GITHUB_TOKEN=abc"),
        "env: GITHUB_TOKEN=***"
    );
    assert_eq!(
        redactor.redact_text("XGITHUB_TOKEN=abc"),
        "XGITHUB_TOKEN=abc",
        "the name needs a word boundary"
    );
    assert_eq!(
        redactor.redact_text("GITHUB_TOKEN=${GITHUB_TOKEN}"),
        "GITHUB_TOKEN=${GITHUB_TOKEN}",
        "a reference holds no value"
    );
    assert_eq!(redactor.redact_text("GITHUB_TOKEN="), "GITHUB_TOKEN=");
    assert_eq!(
        redactor.redact_text("GITHUB_TOKEN is set"),
        "GITHUB_TOKEN is set",
        "the name alone is not a secret"
    );
    assert!(!redactor.contains_secret("GITHUB_TOKEN is set"));

    let embedded = redactor.redact_text(r#"{"env":"GITHUB_TOKEN=abc"}"#);
    assert_eq!(embedded, r#"{"env":"GITHUB_TOKEN=***"}"#);
    assert_eq!(
        redactor.redact_text("https://host/cb?GITHUB_TOKEN=abc&x=1"),
        "https://host/cb?GITHUB_TOKEN=***&x=1"
    );
}

#[test]
fn contains_secret_reports_values_shapes_and_assignments_but_not_references() {
    let redactor = loaded();

    assert!(redactor.contains_secret(SENTINEL));
    assert!(redactor.contains_secret("ghp_16C7e42F292c6912E7710c838347Ae178B4a"));
    assert!(redactor.contains_secret("GITHUB_TOKEN=abc"));
    assert!(redactor.contains_secret("https://user:pass@example.com"));
    assert!(!redactor.contains_secret("no secrets here"));
    assert!(!redactor.contains_secret("${GITHUB_TOKEN}"));
    assert!(!redactor.contains_secret("GITHUB_TOKEN=${GITHUB_TOKEN}"));
    assert!(!redactor.contains_secret("Use ${GITHUB_TOKEN} to authenticate"));
}

#[test]
fn redact_json_masks_strings_keys_by_name_and_keeps_references() {
    let redactor = loaded();
    let bundle = json!({
        "app_version": "0.1.0",
        "path": "/Users/ada/Library/Logs",
        "log": format!("request failed with {SENTINEL}"),
        "env": {
            "GITHUB_TOKEN": "abc",
            "REF": "${GITHUB_TOKEN}",
            "RUST_LOG": "info",
        },
        "tokens": [format!("Bearer {SENTINEL}"), "clean"],
        "nested": {"deep": {"value": "ghp_16C7e42F292c6912E7710c838347Ae178B4a"}},
    });

    let redacted = redactor.redact_json(&bundle);
    let printed = redacted.to_string();

    assert!(!printed.contains(SENTINEL), "{printed}");
    assert!(!printed.contains("ghp_16C7"), "{printed}");
    assert!(!printed.contains("\"abc\""), "{printed}");
    assert_eq!(redacted["env"]["GITHUB_TOKEN"], json!("***"));
    assert_eq!(redacted["env"]["REF"], json!("${GITHUB_TOKEN}"));
    assert_eq!(redacted["env"]["RUST_LOG"], json!("info"));
    assert_eq!(redacted["tokens"][1], json!("clean"));
    assert_eq!(redacted["app_version"], json!("0.1.0"));

    // The input document is not modified.
    assert_eq!(bundle["env"]["GITHUB_TOKEN"], json!("abc"));
}

#[test]
fn deep_json_is_bounded_instead_of_overflowing_the_stack() {
    let redactor = loaded();
    let mut value = json!("leaf");
    for _ in 0..200 {
        value = json!({ "level": value });
    }

    let redacted = redactor.redact_json(&value);
    let printed = redacted.to_string();
    assert!(printed.contains(REDACTION_MASK), "{printed}");
    assert!(!printed.contains(SENTINEL), "{printed}");
}

#[derive(Debug)]
struct SpawnedServer {
    command: String,
    env: Vec<String>,
}

#[test]
fn debug_output_is_redacted_on_request() {
    let redactor = loaded();
    let spawned = SpawnedServer {
        command: "npx -y server".to_owned(),
        env: vec!["GITHUB_TOKEN=abc".to_owned(), format!("OTHER={SENTINEL}")],
    };

    let raw = format!("{spawned:?}");
    assert!(raw.contains(&spawned.command), "{raw}");
    assert!(raw.contains(&spawned.env[0]), "{raw}");
    assert!(raw.contains(SENTINEL), "a derived Debug prints every field");

    let printed = redactor.redact_debug(&spawned);
    assert!(!printed.contains(SENTINEL), "{printed}");
    assert!(!printed.contains("abc"), "{printed}");
    assert!(printed.contains("npx -y server"), "{printed}");
}

#[test]
fn an_error_report_is_redacted_through_the_trait() {
    let redactor = loaded();
    let mut report = ErrorReport::new(ErrorKind::Network, format!("request failed for {SENTINEL}"))
        .with_source(format!("Authorization: Bearer {SENTINEL}"))
        .with_source("GITHUB_TOKEN=abc");

    report.redact_with(&redactor);

    let printed = format!("{report:?}");
    assert!(!printed.contains(SENTINEL), "{printed}");
    assert!(!printed.contains("abc"), "{printed}");
    assert!(report.message.contains(REDACTION_MASK), "{printed}");
    assert!(
        report
            .source_chain
            .iter()
            .all(|line| !line.contains(SENTINEL)),
        "{printed}"
    );
    // The trait is the seam: the same text goes through either entry point.
    assert_eq!(
        SecretRedactor::redact(&redactor, "GITHUB_TOKEN=abc"),
        redactor.redact_text("GITHUB_TOKEN=abc")
    );
}

#[test]
fn no_sentinel_survives_a_log_or_diagnostics_export() {
    let redactor = loaded();
    let lines = [
        format!("INFO  scan finished token={SENTINEL}"),
        "WARN  GITHUB_TOKEN=abc is set".to_owned(),
        format!("ERROR spawn failed: Authorization: Bearer {SENTINEL}"),
        format!("DEBUG env GITHUB_TOKEN={SENTINEL}"),
    ];

    // Log path: every line is redacted before it is written.
    let journal: String = lines
        .iter()
        .map(|line| redactor.redact_text(line) + "\n")
        .collect();
    assert!(!journal.contains(SENTINEL), "{journal}");
    assert!(!journal.contains("=abc"), "{journal}");

    // Diagnostics export: the bundle is redacted as a whole.
    let bundle = json!({
        "logs": lines,
        "effective_settings": {"log_level": "info", "secret_hint": "GITHUB_TOKEN=abc"},
        "agent_detection": [{"agent": "codex", "path": "/Users/ada/.codex"}],
    });
    let exported = redactor.redact_json(&bundle).to_string();
    assert!(!exported.contains(SENTINEL), "{exported}");
    assert!(!exported.contains("=abc"), "{exported}");
}

#[test]
fn multibyte_text_keeps_character_boundaries() {
    let redactor = Redactor::new()
        .with_names(["GITHUB_TOKEN"])
        .with_values(["ключ-значение", "秘密の値です"]);

    let text =
        "前缀 ключ-значение 中 ghp_16C7e42F292c6912E7710c838347Ae178B4a 后 ${GITHUB_TOKEN} 終";
    let redacted = redactor.redact_text(text);
    assert_eq!(redacted, "前缀 *** 中 *** 后 ${GITHUB_TOKEN} 終");

    assert_eq!(
        redactor.redact_text("秘密ghp_16C7e42F292c6912E7710c838347Ae178B4a秘密"),
        "秘密***秘密"
    );

    // Every byte offset the scanner used is a character boundary: walking the chars
    // would panic on a split character.
    for (index, character) in redacted.char_indices() {
        assert_eq!(redacted[index..].chars().next(), Some(character));
    }
}

#[test]
fn redaction_is_idempotent() {
    let redactor = loaded();
    let samples = [
        format!("a {SENTINEL} b"),
        "GITHUB_TOKEN=abc".to_owned(),
        "https://user:pass@example.com".to_owned(),
        "${GITHUB_TOKEN:-default}".to_owned(),
        "Bearer abcdefghij0123456789".to_owned(),
    ];
    for sample in samples {
        let once = redactor.redact_text(&sample);
        assert_eq!(redactor.redact_text(&once), once, "{sample} -> {once}");
    }
}

#[test]
fn developer_diagnostics_list_names_but_never_values() {
    let redactor = Redactor::new()
        .with_names(["GITHUB_TOKEN", "OPENAI_KEY"])
        .with_values([SENTINEL]);

    let listed: Vec<&str> = redactor.names().collect();
    assert_eq!(listed, ["GITHUB_TOKEN", "OPENAI_KEY"]);
    assert_eq!(redactor.name_count(), 2);
    assert_eq!(redactor.value_count(), 1);
    assert!(!redactor.is_empty());
    assert!(Redactor::new().is_empty());

    let printed = format!("{redactor:?}");
    assert!(printed.contains("GITHUB_TOKEN"), "{printed}");
    assert!(!printed.contains(SENTINEL), "{printed}");
}

#[test]
fn pathological_inputs_stay_linear() {
    let redactor = loaded();

    // A value that never terminates: the scan must not re-read the tail per candidate.
    let long_value = format!("GITHUB_TOKEN={}", "GITHUB_TOKEN=".repeat(64 * 1024));
    // One base64url run that starts with `eyJ` but has no segments: not a JWT.
    let runs = "eyJh".repeat(256 * 1024);
    // Repeated prefixes: one token covers the whole run.
    let prefixes = "sk-".repeat(512 * 1024);
    // Repeated URL delimiter with no userinfo.
    let urls = "://".repeat(512 * 1024);

    let started = Instant::now();
    let redacted_value = redactor.redact_text(&long_value);
    let redacted_runs = redactor.redact_text(&runs);
    let redacted_prefixes = redactor.redact_text(&prefixes);
    let redacted_urls = redactor.redact_text(&urls);
    let elapsed = started.elapsed();

    assert_eq!(
        redacted_value, "GITHUB_TOKEN=***",
        "the whole unterminated value is masked"
    );
    assert_eq!(
        redacted_runs, runs,
        "a base64url run without dots is not a JWT"
    );
    assert!(
        redacted_prefixes.starts_with(REDACTION_MASK) && redacted_prefixes.len() < 8,
        "one prefix match covers the whole run: {}",
        redacted_prefixes.len()
    );
    assert_eq!(redacted_urls, urls, "`://` alone is not credentials");

    // The contract is the release budget; a debug build under a loaded CI runner is far
    // slower than the shipped code, so debug keeps enough headroom to report a real
    // regression (an accidental quadratic scan) instead of machine noise.
    let budget = if cfg!(debug_assertions) {
        Duration::from_secs(5)
    } else {
        Duration::from_millis(50)
    };
    assert!(elapsed < budget, "pathological inputs took {elapsed:?}");
}

#[test]
fn ten_megabytes_of_log_text_redacts_quickly() {
    let redactor = loaded();
    let line = "2026-11-05T10:00:00Z INFO agentuna::engine scan finished files=128 errors=0 ok\n";

    let mut text = String::with_capacity(10 * 1024 * 1024 + 8 * SENTINEL.len());
    let mut lines = 0usize;
    while text.len() < 10 * 1024 * 1024 {
        text.push_str(line);
        lines += 1;
        if lines.is_multiple_of(4096) {
            text.push_str(SENTINEL);
            text.push('\n');
        }
    }
    assert!(text.len() >= 10 * 1024 * 1024);

    let started = Instant::now();
    let redacted = redactor.redact_text(&text);
    let elapsed = started.elapsed();

    assert!(!redacted.contains(SENTINEL));
    assert_eq!(
        redacted.matches(REDACTION_MASK).count(),
        text.matches(SENTINEL).count()
    );
    assert!(redacted.len() < text.len());

    // The contract is 50 ms in a release build (~20 ms measured). A debug build on a loaded CI
    // runner is far slower, so debug keeps generous headroom; the release budget is the contract.
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(2_000)
    } else {
        Duration::from_millis(50)
    };
    assert!(elapsed < budget, "10 MB took {elapsed:?}");
}
