//! Error model: stable codes, IO mapping, remedies, redaction and formatting.

use std::io;
use std::path::Path;

use at_model::{ErrorKind, ErrorReport, REDACTION_MASK, Remedy};

const CODE_SNAPSHOT: &str = include_str!("snapshots/error_codes.txt");
const REMEDY_SNAPSHOT: &str = include_str!("snapshots/remedy_ids.txt");

#[test]
fn codes_round_trip_and_are_locked_by_snapshot() {
    assert_eq!(ErrorKind::ALL.len(), 15);
    let mut unique: Vec<&str> = ErrorKind::ALL.iter().map(|kind| kind.code()).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 15, "every kind must have its own code");

    let declared: Vec<&str> = ErrorKind::ALL.iter().map(|kind| kind.code()).collect();
    let snapshot: Vec<&str> = CODE_SNAPSHOT.lines().collect();
    assert_eq!(declared, snapshot, "codes are a published contract");

    for &kind in ErrorKind::ALL {
        assert_eq!(ErrorKind::from_code(kind.code()), Some(kind));
        assert_eq!(kind.code().parse::<ErrorKind>().unwrap(), kind);
        let wire = serde_json::to_string(&kind).unwrap();
        assert_eq!(wire, format!("\"{}\"", kind.code()));
        assert_eq!(serde_json::from_str::<ErrorKind>(&wire).unwrap(), kind);
        assert!(!kind.title().is_empty());
    }
    assert_eq!(ErrorKind::from_code("E_UNKNOWN"), None);
    assert!("E_UNKNOWN".parse::<ErrorKind>().is_err());
}

#[test]
fn io_kinds_map_to_error_kinds_with_the_path_populated() {
    let path = Path::new("/tmp/agentuna/config/mcp.json");
    let cases = [
        (io::ErrorKind::PermissionDenied, ErrorKind::PermissionDenied),
        (io::ErrorKind::NotFound, ErrorKind::NotFound),
        (io::ErrorKind::AlreadyExists, ErrorKind::Conflict),
        (io::ErrorKind::InvalidData, ErrorKind::InvalidFormat),
        (io::ErrorKind::ConnectionRefused, ErrorKind::Network),
        (io::ErrorKind::Other, ErrorKind::Io),
    ];
    for (io_kind, expected) in cases {
        let report = ErrorReport::from_io(path, io::Error::from(io_kind));
        assert_eq!(report.kind, expected, "{io_kind:?}");
        assert_eq!(report.path.as_deref(), Some(path), "{io_kind:?}");
        assert_eq!(report.remedy, expected.default_remedies(), "{io_kind:?}");
    }
}

#[test]
fn permission_denied_names_the_path_and_the_read_only_folder() {
    let path = Path::new("/tmp/agentuna/config/mcp.json");
    let report = ErrorReport::from_io(path, io::Error::from(io::ErrorKind::PermissionDenied));

    assert_eq!(report.kind, ErrorKind::PermissionDenied);
    assert_eq!(report.path.as_deref(), Some(path));
    assert!(!report.retryable);
    assert_eq!(
        report.user_message(),
        "Cannot write /tmp/agentuna/config/mcp.json because the folder is read-only."
    );
    assert!(report.to_string().starts_with("E_PERMISSION_DENIED: "));

    // The plain `From` conversion has no path but still renders a sentence.
    let bare = ErrorReport::from(io::Error::from(io::ErrorKind::PermissionDenied));
    assert_eq!(bare.path, None);
    assert_eq!(
        bare.user_message(),
        "Cannot write because the folder is read-only."
    );
}

#[test]
fn display_and_debug_never_panic_and_debug_keeps_the_source_chain() {
    let report = ErrorReport::new(ErrorKind::ConcurrentModification, "could not apply plan")
        .with_path("/tmp/x.json")
        .with_operation_id("op_123")
        .with_source("expected hash aa, found bb")
        .with_source("caused by: file replaced");

    let display = report.to_string();
    let debug = format!("{report:?}");
    let user = report.user_message();

    assert!(display.starts_with("E_CONCURRENT_MODIFICATION: could not apply plan"));
    assert!(
        !display.contains("expected hash aa"),
        "Display omits the chain"
    );
    assert!(debug.contains("expected hash aa"));
    assert!(debug.contains("caused by: file replaced"));
    assert!(!user.contains("expected hash"));
    assert!(!user.contains("E_CONCURRENT_MODIFICATION"));
    assert_eq!(user, "/tmp/x.json changed on disk after AgenTuna read it.");

    // Hostile and deeply nested source strings format without panicking.
    let hostile = ErrorReport::new(ErrorKind::Internal, "")
        .with_source("\u{0}\u{1b}[31m")
        .with_source("x".repeat(10_000));
    let _ = hostile.to_string();
    let _ = format!("{hostile:?}");
    let _ = hostile.user_message();
    assert!(hostile.to_string().contains("Internal error"));
}

#[test]
fn refused_overwrite_carries_adopt_move_aside_and_cancel() {
    let report = ErrorReport::new(
        ErrorKind::RefusedOverwrite,
        "destination is not managed by AgenTuna",
    )
    .with_path("/tmp/unmanaged.json");

    assert_eq!(
        report.remedy,
        [Remedy::Adopt, Remedy::MoveAside, Remedy::Cancel]
    );
    assert!(!report.retryable);

    let ids: Vec<&str> = Remedy::ALL.iter().map(|remedy| remedy.id()).collect();
    assert_eq!(ids, REMEDY_SNAPSHOT.lines().collect::<Vec<_>>());
    for remedy in &report.remedy {
        assert!(!remedy.label().is_empty());
    }

    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    assert_eq!(json["code"], "E_REFUSED_OVERWRITE");
    assert_eq!(
        json["remedy"],
        serde_json::json!(["adopt", "move_aside", "cancel"]),
        "the app and CLI dispatch these exact ids"
    );
    assert_eq!(json["path"], "/tmp/unmanaged.json");
    assert_eq!(
        json["user_message"],
        "/tmp/unmanaged.json already exists and is not managed by AgenTuna, so it was not overwritten."
    );
}

#[test]
fn cancelled_reports_offer_no_action() {
    let report = ErrorReport::cancelled();
    assert_eq!(report.kind, ErrorKind::Cancelled);
    assert!(report.remedy.is_empty());
    assert!(!report.retryable);
    assert_eq!(report.user_message(), "The operation was cancelled.");
}

const SENTINEL: &str = "sk-live-9f8e7d6c5b4a";

#[test]
fn secrets_never_reach_display_debug_or_json() {
    let report = ErrorReport::new(
        ErrorKind::PermissionDenied,
        format!("failed with {SENTINEL}"),
    )
    .with_path(format!("/tmp/{SENTINEL}/config.json"))
    .with_operation_id(format!("op-{SENTINEL}"))
    .with_source(format!("cause: token {SENTINEL} rejected"))
    .redacted_with([SENTINEL]);

    let surfaces = [
        report.to_string(),
        format!("{report:?}"),
        report.user_message(),
        serde_json::to_string(&report).unwrap(),
    ];
    for surface in &surfaces {
        assert!(
            !surface.contains(SENTINEL),
            "a rendered surface leaked the secret: {surface}"
        );
    }
    for surface in &surfaces[..3] {
        assert!(
            surface.contains(REDACTION_MASK),
            "masked value is visible: {surface}"
        );
    }
    assert!(surfaces[3].contains(REDACTION_MASK), "{}", surfaces[3]);

    // Longest first: a secret that contains another one is masked as a whole.
    let mut overlapping = ErrorReport::new(ErrorKind::Auth, "supersecret and secret");
    overlapping.redact_values(["secret", "supersecret"]);
    assert_eq!(overlapping.message, "*** and ***");

    // Empty values are ignored rather than masking nothing or panicking.
    let mut untouched = ErrorReport::new(ErrorKind::Auth, "unchanged");
    untouched.redact_values(["", ""]);
    assert_eq!(untouched.message, "unchanged");
}

#[test]
fn serde_json_errors_map_to_invalid_format_with_line_information() {
    let source = serde_json::from_str::<serde_json::Value>("{\"a\": }").unwrap_err();
    let report = ErrorReport::from(source);
    assert_eq!(report.kind, ErrorKind::InvalidFormat);
    assert!(
        report.message.contains("line"),
        "line and column survive the conversion: {}",
        report.message
    );
    assert!(!report.retryable);
}

#[test]
fn a_redactor_masks_every_visible_field() {
    struct HidingRedactor;

    impl at_model::SecretRedactor for HidingRedactor {
        fn redact(&self, text: &str) -> String {
            text.replace("hunter2", "[hidden]")
        }
    }

    let mut report = ErrorReport::new(ErrorKind::Auth, "sign-in with hunter2 failed")
        .with_path("/tmp/hunter2/session")
        .with_operation_id("op-hunter2")
        .with_source("rejected hunter2");
    report.redact_with(&HidingRedactor);

    assert_eq!(report.message, "sign-in with [hidden] failed");
    assert_eq!(
        report.path.as_deref(),
        Some(Path::new("/tmp/[hidden]/session"))
    );
    assert_eq!(report.operation_id.as_deref(), Some("op-[hidden]"));
    assert_eq!(report.source_chain, ["rejected [hidden]"]);
    let rendered = format!(
        "{report:?}{}{report}",
        serde_json::to_string(&report).unwrap()
    );
    assert!(!rendered.contains("hunter2"));
}
