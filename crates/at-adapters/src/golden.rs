//! Golden-file harness for the format-preserving TOML and JSONC adapters.
//!
//! Each [`GoldenCase`] names a fixture under `tests/fixtures`, a function that applies one
//! adapter edit to the fixture copied into a sandboxed home, and a function that re-reads the
//! result as a semantic model. A case passes when the bytes on disk are exactly the bytes in
//! `tests/golden/<id>/expected.<ext>`; anything else — a reordered key, a dropped comment, a
//! changed blank line — is a failure with a unified diff.
//!
//! The harness is test support, not API: it is compiled only for this crate's tests
//! (`#[cfg(test)]`), and `tests/golden.rs` additionally includes this file by path so the
//! integration test builds without a feature flag.
//!
//! # Regenerating expected files
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo test -p at-adapters --test golden
//! cargo test -p at-adapters --test golden
//! ```
//!
//! Regeneration still runs the semantic check and the format reparse, so a broken patch cannot
//! be blessed as a golden file.
//!
//! # Sandboxing
//!
//! Fixtures are copied into an [`at_fs::testkit::TestHome`]; the adapters never see a real
//! home directory, a real agent folder, or the network.

use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use at_fs::testkit::TestHome;
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::CstRootNode;
use similar::TextDiff;
use toml_edit::DocumentMut;

/// Environment variable that switches the runner from compare to rewrite.
pub const UPDATE_ENV: &str = "UPDATE_GOLDEN";

/// Serialisation format of a case's fixture and expected output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// TOML, parsed with `toml_edit`.
    Toml,
    /// JSON with comments and trailing commas, parsed with `jsonc-parser`.
    Jsonc,
}

impl Format {
    /// File extension of an expected file in this format.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Toml => "toml",
            Self::Jsonc => "json",
        }
    }

    /// Parses `text` enough to prove the document is readable by the matching adapter.
    ///
    /// # Errors
    /// The parser message when the document is not valid in this format.
    pub fn parse(self, text: &str) -> Result<(), String> {
        match self {
            Self::Toml => text
                .parse::<DocumentMut>()
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Self::Jsonc => CstRootNode::parse(text, &ParseOptions::default())
                .map(|_| ())
                .map_err(|error| error.to_string()),
        }
    }
}

/// One fixture, one adapter edit, one expected file.
pub struct GoldenCase {
    /// Case id; also the directory name under `tests/golden`.
    pub id: &'static str,
    /// Fixture path relative to `tests/fixtures`, e.g. `codex/golden.toml`.
    pub fixture: &'static str,
    /// Format of the fixture and of the expected file.
    pub format: Format,
    /// Applies the adapter edit to the sandboxed copy of the fixture.
    pub apply: fn(&Path) -> Result<(), String>,
    /// Re-reads the resulting bytes and checks the intended semantic model.
    pub check: fn(&str) -> Result<(), String>,
    /// When set, the output must be byte-identical to the fixture (a no-op case).
    pub unchanged: bool,
}

/// One case that did not match its expected file.
#[derive(Debug)]
pub struct CaseFailure {
    /// Case id.
    pub id: &'static str,
    /// Fixture that was edited.
    pub fixture: PathBuf,
    /// Expected file that was compared against.
    pub expected: PathBuf,
    /// Human-readable explanation, including a unified diff.
    pub detail: String,
}

impl fmt::Display for CaseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "golden case `{}`\n  fixture: {}\n  expected: {}\n{}",
            self.id,
            self.fixture.display(),
            self.expected.display(),
            self.detail
        )
    }
}

/// Every case that failed in one run.
#[derive(Debug)]
pub struct Failures {
    /// Failures in corpus order.
    pub cases: Vec<CaseFailure>,
}

impl fmt::Display for Failures {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, failure) in self.cases.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n\n")?;
            }
            write!(formatter, "{failure}")?;
        }
        Ok(())
    }
}

/// What a passing run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Number of cases that were run.
    pub cases: usize,
    /// Case ids whose expected file was rewritten (empty in compare mode).
    pub updated: Vec<&'static str>,
}

/// Fixture and golden roots plus the sandbox every case runs in.
pub struct Suite {
    fixtures: PathBuf,
    golden: PathBuf,
    sandbox: TestHome,
}

impl Suite {
    /// The repository layout: `tests/fixtures` and `tests/golden` next to this crate.
    ///
    /// # Errors
    /// When the sandbox cannot be created.
    pub fn workspace() -> Result<Self, String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        Self::new(root.join("fixtures"), root.join("golden"))
    }

    /// A suite whose roots the caller supplies; used to test the harness itself.
    ///
    /// # Errors
    /// When the sandbox cannot be created.
    pub fn new(fixtures: impl Into<PathBuf>, golden: impl Into<PathBuf>) -> Result<Self, String> {
        Ok(Self {
            fixtures: fixtures.into(),
            golden: golden.into(),
            sandbox: TestHome::new().map_err(|error| error.to_string())?,
        })
    }

    /// Runs every case, rewriting expected files when [`UPDATE_ENV`] is set.
    ///
    /// # Errors
    /// One [`CaseFailure`] per case that does not match, in corpus order.
    pub fn run(&self, cases: &[GoldenCase]) -> Result<Outcome, Failures> {
        self.run_with(cases, update_requested())
    }

    /// Runs every case in an explicit mode; `update` rewrites expected files.
    ///
    /// # Errors
    /// One [`CaseFailure`] per case that does not match, in corpus order.
    pub fn run_with(&self, cases: &[GoldenCase], update: bool) -> Result<Outcome, Failures> {
        let mut updated = Vec::new();
        let mut failures = Vec::new();
        for case in cases {
            match self.run_case(case, update) {
                Ok(wrote) => {
                    if wrote {
                        updated.push(case.id);
                    }
                }
                Err(failure) => failures.push(failure),
            }
        }
        if failures.is_empty() {
            Ok(Outcome {
                cases: cases.len(),
                updated,
            })
        } else {
            Err(Failures { cases: failures })
        }
    }

    /// Applies one case in the sandbox and returns `(fixture text, result text)`.
    ///
    /// Used by the span tests, which need the bytes before and after an edit without a golden
    /// comparison.
    ///
    /// # Errors
    /// When the fixture cannot be read or seeded, or the adapter edit fails.
    pub fn apply_case(&self, case: &GoldenCase) -> Result<(String, String), String> {
        let source = fs::read_to_string(self.fixtures.join(case.fixture))
            .map_err(|error| format!("cannot read fixture: {error}"))?;
        let path = self.seed(case, &source)?;
        (case.apply)(&path)?;
        let actual =
            fs::read_to_string(&path).map_err(|error| format!("cannot read result: {error}"))?;
        Ok((source, actual))
    }

    /// Writes the fixture into a directory unique to the case.
    fn seed(&self, case: &GoldenCase, source: &str) -> Result<PathBuf, String> {
        let name = Path::new(case.fixture).file_name().map_or_else(
            || format!("fixture.{}", case.format.extension()),
            |name| name.to_string_lossy().into_owned(),
        );
        // `.write` creates the parent directories and returns the absolute sandbox path.
        self.sandbox
            .write(format!("{}/{name}", case.id), source.as_bytes())
            .map_err(|error| format!("cannot seed sandbox: {error}"))
    }

    /// Runs one case. `Ok(true)` means the expected file was (re)written.
    fn run_case(&self, case: &GoldenCase, update: bool) -> Result<bool, CaseFailure> {
        let fixture = self.fixtures.join(case.fixture);
        let expected = self
            .golden
            .join(case.id)
            .join(format!("expected.{}", case.format.extension()));
        let fail = |detail: String| CaseFailure {
            id: case.id,
            fixture: fixture.clone(),
            expected: expected.clone(),
            detail,
        };

        let source = fs::read_to_string(&fixture)
            .map_err(|error| fail(format!("cannot read fixture: {error}")))?;
        let path = self.seed(case, &source).map_err(fail)?;
        (case.apply)(&path).map_err(fail)?;
        let actual = fs::read_to_string(&path)
            .map_err(|error| fail(format!("cannot read result: {error}")))?;

        case.format.parse(&actual).map_err(|error| {
            fail(format!(
                "output is not valid {}: {error}",
                format_name(case.format)
            ))
        })?;
        (case.check)(&actual).map_err(|error| fail(format!("semantic check failed: {error}")))?;

        if case.unchanged && actual != source {
            return Err(fail(format!(
                "no-op case changed bytes: {}\n{}",
                length_delta(&source, &actual),
                unified_diff(&source, &actual, "fixture", "output")
            )));
        }

        if update {
            if let Some(parent) = expected.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| fail(format!("cannot create golden directory: {error}")))?;
            }
            fs::write(&expected, actual.as_bytes())
                .map_err(|error| fail(format!("cannot write expected file: {error}")))?;
            return Ok(true);
        }

        let wanted = match fs::read_to_string(&expected) {
            Ok(wanted) => wanted,
            Err(error) => {
                return Err(fail(format!(
                    "cannot read expected file ({error}); run `{UPDATE_ENV}=1 cargo test -p at-adapters --test golden`"
                )));
            }
        };
        if actual == wanted {
            Ok(false)
        } else {
            Err(fail(format!(
                "{}\n{}",
                length_delta(&wanted, &actual),
                unified_diff(&wanted, &actual, "expected", "actual")
            )))
        }
    }
}

/// One contiguous edit that turns `before` into `after`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSpan {
    /// Byte offset where the edited region starts; every byte before it is unchanged.
    pub start: usize,
    /// Number of bytes removed at [`Self::start`].
    pub removed: usize,
    /// Bytes inserted at [`Self::start`].
    pub inserted: String,
}

impl EditSpan {
    /// `after.len() - before.len()` for this edit.
    #[must_use]
    pub fn length_delta(&self) -> isize {
        self.inserted.len() as isize - self.removed as isize
    }
}

/// Describes how `after` differs from `before` as one contiguous edit.
///
/// The span is the longest common prefix and suffix around the change, so it is exactly the
/// assertion the golden corpus needs: a pure insertion has `removed == 0`, which means every
/// byte before [`EditSpan::start`] is unchanged and the document length grew by exactly
/// `inserted.len()`; a targeted replacement has a non-empty `removed` and an `inserted` text
/// the adapter is allowed to rewrite.
#[must_use]
pub fn single_edit_span(before: &str, after: &str) -> EditSpan {
    let mut start = 0usize;
    for (old, new) in before.chars().zip(after.chars()) {
        if old != new {
            break;
        }
        start += old.len_utf8();
    }
    let before_rest = &before[start..];
    let after_rest = &after[start..];
    let mut suffix = 0usize;
    for (old, new) in before_rest.chars().rev().zip(after_rest.chars().rev()) {
        if old != new {
            break;
        }
        suffix += old.len_utf8();
    }
    let removed = before_rest.len() - suffix;
    let inserted = after_rest[..after_rest.len() - suffix].to_owned();
    EditSpan {
        start,
        removed,
        inserted,
    }
}

/// Whether [`UPDATE_ENV`] asks the runner to rewrite expected files.
#[must_use]
pub fn update_requested() -> bool {
    env::var(UPDATE_ENV).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

fn format_name(format: Format) -> &'static str {
    match format {
        Format::Toml => "TOML",
        Format::Jsonc => "JSONC",
    }
}

fn length_delta(expected: &str, actual: &str) -> String {
    format!(
        "length: expected {} bytes, actual {} bytes (delta {})",
        expected.len(),
        actual.len(),
        actual.len() as isize - expected.len() as isize
    )
}

fn unified_diff(expected: &str, actual: &str, expected_label: &str, actual_label: &str) -> String {
    TextDiff::from_lines(expected, actual)
        .unified_diff()
        .context_radius(3)
        .header(expected_label, actual_label)
        .to_string()
}
