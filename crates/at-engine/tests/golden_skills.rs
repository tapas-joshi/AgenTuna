//! Golden-file suite for `SKILL.md` parsing, format-preserving edits and templates (TS-042).
//!
//! Each directory under `tests/golden/skills/` holds the authorial inputs and the checked-in
//! results:
//!
//! * `SKILL.md` — the document as it would be found on disk (may be binary or oversized);
//! * `case.json` — what the runner should do with it: parse it directly or through the folder
//!   rule, and optionally rewrite its description;
//! * `expected.json` — the parse snapshot (or error variant) and, for edit cases, the unified
//!   diff;
//! * `edited.md` — the exact bytes `set_description` must produce.
//!
//! Regenerate after reviewing a deliberate behaviour change:
//!
//! ```sh
//! cargo test -p at-engine --test golden_skills -- --ignored regenerate
//! ```

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use at_engine::skill_templates::{SkillDraft, SkillTemplate, render_preview};
use at_model::skill::{
    ByteSpan, ParsedSkill, SkillDoc, SkillError, SkillNameMismatch, SkillOrigin,
};
use at_store::skill::{parse_skill_doc, parse_skill_doc_in_folder, set_description};
use serde::Deserialize;
use serde_json::{Value, json};
use support::{describe_differences, json_differences, unified_diff};

/// Directory holding every skill fixture.
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/skills")
}

/// Directory holding the rendered template goldens.
fn template_dir() -> PathBuf {
    golden_dir().join("templates")
}

/// Fixed inputs for the template goldens.
const TEMPLATE_SLUG: &str = "example-skill";
const TEMPLATE_TITLE: &str = "Example Skill";
const TEMPLATE_DESCRIPTION: &str = "Reviews a change and reports findings.";

/// Parser variants every fixture set must exercise.
const PARSER_VARIANTS: &[&str] = &[
    "DocTooLarge",
    "InvalidUtf8",
    "MissingFrontmatter",
    "UnclosedFrontmatter",
    "InvalidFrontmatter",
    "FrontmatterNotMapping",
    "MissingName",
    "NameNotString",
    "NameEmpty",
    "NameTooLong",
    "NameUppercase",
    "NameWhitespace",
    "NameInvalidCharacter",
    "NameLeadingHyphen",
    "NameTrailingHyphen",
    "NameDoubleHyphen",
    "MissingDescription",
    "DescriptionNotString",
    "EmptyDescription",
    "DescriptionTooLong",
];

/// Authorial inputs of one fixture.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoldenCase {
    id: String,
    /// Parse through `parse_skill_doc_in_folder` with this folder name.
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    origin: OriginName,
    /// Rewrite the description to this value and record the result.
    #[serde(default)]
    edit: Option<String>,
    /// This description must be rejected by `set_description`.
    #[serde(default)]
    edit_error: Option<String>,
}

/// Folder rule to apply.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OriginName {
    #[default]
    Create,
    Import,
}

impl OriginName {
    const fn origin(self) -> SkillOrigin {
        match self {
            Self::Create => SkillOrigin::Create,
            Self::Import => SkillOrigin::Import,
        }
    }
}

/// One fixture directory.
struct Fixture {
    dir: PathBuf,
    case: GoldenCase,
}

fn discover() -> Vec<Fixture> {
    let mut fixtures = Vec::new();
    let entries = fs::read_dir(golden_dir()).expect("golden directory exists");
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let case_path = dir.join("case.json");
        if !case_path.is_file() {
            continue;
        }
        let text = fs::read_to_string(&case_path)
            .unwrap_or_else(|error| panic!("{}: {error}", case_path.display()));
        let case: GoldenCase = serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!("{}: invalid case schema: {error}", case_path.display())
        });
        assert_eq!(
            case.id,
            dir.file_name().and_then(|name| name.to_str()).unwrap_or(""),
            "{}: fixture id must match its directory name",
            case_path.display()
        );
        fixtures.push(Fixture { dir, case });
    }
    fixtures
}

fn span_json(span: ByteSpan) -> Value {
    json!([span.start(), span.end()])
}

fn warning_json(warning: Option<&SkillNameMismatch>) -> Value {
    match warning {
        Some(warning) => json!({ "folder": warning.folder, "name": warning.name }),
        None => Value::Null,
    }
}

fn doc_snapshot(doc: &SkillDoc, warning: Option<&SkillNameMismatch>) -> Value {
    json!({
        "name": doc.name,
        "description": doc.description,
        "frontmatter": doc.frontmatter(),
        "body": doc.body(),
        "raw_len": doc.raw.len(),
        "frontmatter_span": span_json(doc.frontmatter_span),
        "body_span": span_json(doc.body_span),
        "description_span": span_json(doc.description_span),
        "unknown_keys": doc
            .unknown_keys
            .iter()
            .map(|key| json!({ "key": key.key, "raw": key.raw, "span": span_json(key.span) }))
            .collect::<Vec<_>>(),
        "warning": warning_json(warning),
    })
}

fn error_snapshot(error: &SkillError) -> Value {
    match error {
        SkillError::DocTooLarge { size, limit } => {
            json!({ "variant": "DocTooLarge", "size": size, "limit": limit })
        }
        SkillError::InvalidUtf8 { valid_up_to } => {
            json!({ "variant": "InvalidUtf8", "valid_up_to": valid_up_to })
        }
        SkillError::MissingFrontmatter => json!({ "variant": "MissingFrontmatter" }),
        SkillError::UnclosedFrontmatter => json!({ "variant": "UnclosedFrontmatter" }),
        SkillError::InvalidFrontmatter {
            line,
            column,
            message,
        } => json!({
            "variant": "InvalidFrontmatter",
            "line": line,
            "column": column,
            "message": message,
        }),
        SkillError::FrontmatterNotMapping => json!({ "variant": "FrontmatterNotMapping" }),
        SkillError::MissingName => json!({ "variant": "MissingName" }),
        SkillError::NameNotString => json!({ "variant": "NameNotString" }),
        SkillError::NameEmpty => json!({ "variant": "NameEmpty" }),
        SkillError::NameTooLong { name, len, limit } => {
            json!({ "variant": "NameTooLong", "name": name, "len": len, "limit": limit })
        }
        SkillError::NameUppercase { name } => {
            json!({ "variant": "NameUppercase", "name": name })
        }
        SkillError::NameWhitespace { name } => {
            json!({ "variant": "NameWhitespace", "name": name })
        }
        SkillError::NameInvalidCharacter { name, character } => json!({
            "variant": "NameInvalidCharacter",
            "name": name,
            "character": character.to_string(),
        }),
        SkillError::NameLeadingHyphen { name } => {
            json!({ "variant": "NameLeadingHyphen", "name": name })
        }
        SkillError::NameTrailingHyphen { name } => {
            json!({ "variant": "NameTrailingHyphen", "name": name })
        }
        SkillError::NameDoubleHyphen { name } => {
            json!({ "variant": "NameDoubleHyphen", "name": name })
        }
        SkillError::MissingDescription => json!({ "variant": "MissingDescription" }),
        SkillError::DescriptionNotString => json!({ "variant": "DescriptionNotString" }),
        SkillError::EmptyDescription => json!({ "variant": "EmptyDescription" }),
        SkillError::DescriptionTooLong { len, limit } => {
            json!({ "variant": "DescriptionTooLong", "len": len, "limit": limit })
        }
        SkillError::DescriptionNotRepresentable { value } => {
            json!({ "variant": "DescriptionNotRepresentable", "value": value })
        }
        SkillError::NameMismatch { name, folder } => {
            json!({ "variant": "NameMismatch", "name": name, "folder": folder })
        }
        _ => json!({ "variant": "Unknown" }),
    }
}

/// Parses the fixture exactly as its case describes.
fn parse_fixture(fixture: &Fixture) -> Result<ParsedSkill, SkillError> {
    let bytes = fs::read(fixture.dir.join("SKILL.md"))
        .unwrap_or_else(|error| panic!("{}: {error}", fixture.dir.display()));
    match &fixture.case.folder {
        Some(folder) => parse_skill_doc_in_folder(&bytes, folder, fixture.case.origin.origin()),
        None => parse_skill_doc(&bytes).map(|doc| ParsedSkill { doc, warning: None }),
    }
}

/// The complete expectation for one fixture.
fn build_expectation(fixture: &Fixture) -> (Value, Option<String>) {
    let parsed = parse_fixture(fixture);
    let mut expected = serde_json::Map::new();
    match &parsed {
        Ok(parsed) => {
            expected.insert(
                "parse".to_owned(),
                json!({ "ok": doc_snapshot(&parsed.doc, parsed.warning.as_ref()) }),
            );
            if let Some(description) = &fixture.case.edit {
                let edited = set_description(&parsed.doc, description)
                    .unwrap_or_else(|error| panic!("fixture `{}`: {error}", fixture.case.id));
                let diff = unified_diff("SKILL.md", &parsed.doc.raw, &edited);
                expected.insert(
                    "edit".to_owned(),
                    json!({ "diff": diff, "description": description.trim() }),
                );
                return (Value::Object(expected), Some(edited));
            }
            if let Some(description) = &fixture.case.edit_error {
                let error = set_description(&parsed.doc, description)
                    .expect_err("the fixture declares that this description cannot be represented");
                expected.insert("edit_error".to_owned(), error_snapshot(&error));
            }
        }
        Err(error) => {
            assert!(
                fixture.case.edit.is_none() && fixture.case.edit_error.is_none(),
                "fixture `{}` cannot edit a document that does not parse",
                fixture.case.id
            );
            expected.insert(
                "parse".to_owned(),
                json!({ "error": error_snapshot(error) }),
            );
        }
    }
    (Value::Object(expected), None)
}

fn read_expected(fixture: &Fixture) -> Value {
    let path = fixture.dir.join("expected.json");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{}: invalid JSON: {error}", path.display()))
}

fn render_template(template: SkillTemplate) -> String {
    render_preview(&SkillDraft::new(
        TEMPLATE_TITLE,
        TEMPLATE_DESCRIPTION,
        template,
    ))
    .markdown
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn every_skill_fixture_matches_its_checked_in_snapshot() {
    let fixtures = discover();
    assert!(
        fixtures.len() >= 20,
        "the suite must keep at least 20 fixtures; found {}",
        fixtures.len()
    );
    for fixture in &fixtures {
        let expected = read_expected(fixture);
        let (actual, edited) = build_expectation(fixture);
        let parse_differences = json_differences(&expected["parse"], &actual["parse"]);
        assert!(
            parse_differences.is_empty(),
            "fixture `{}` parse snapshot differs:\n{}",
            fixture.case.id,
            describe_differences(&parse_differences)
        );
        if let Some(edited) = edited {
            let path = fixture.dir.join("edited.md");
            let checked_in =
                fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert_eq!(
                checked_in,
                edited.as_bytes(),
                "fixture `{}`: edited.md is not the exact rewrite",
                fixture.case.id
            );
            let diff_differences = json_differences(&expected["edit"], &actual["edit"]);
            assert!(
                diff_differences.is_empty(),
                "fixture `{}` edit snapshot differs:\n{}",
                fixture.case.id,
                describe_differences(&diff_differences)
            );
        }
        if actual.get("edit_error").is_some() {
            let error_differences =
                json_differences(&expected["edit_error"], &actual["edit_error"]);
            assert!(
                error_differences.is_empty(),
                "fixture `{}` edit error differs:\n{}",
                fixture.case.id,
                describe_differences(&error_differences)
            );
        }
    }
}

#[test]
fn editing_the_description_leaves_every_other_byte_untouched() {
    let fixtures: Vec<Fixture> = discover()
        .into_iter()
        .filter(|fixture| fixture.case.edit.is_some())
        .collect();
    assert!(
        !fixtures.is_empty(),
        "at least one fixture must exercise a description edit"
    );
    for fixture in fixtures {
        let parsed = parse_fixture(&fixture).expect("edit fixtures parse");
        let description = fixture.case.edit.as_deref().expect("edit description");
        let edited = set_description(&parsed.doc, description).expect("the edit succeeds");
        let reparsed = parse_skill_doc(edited.as_bytes()).expect("the edit re-parses");

        let original_span = parsed.doc.description_span;
        let new_span = reparsed.description_span;
        assert_eq!(
            &parsed.doc.raw[..original_span.start()],
            &edited[..new_span.start()],
            "fixture `{}`: bytes before the description changed",
            fixture.case.id
        );
        assert_eq!(
            &parsed.doc.raw[original_span.end()..],
            &edited[new_span.end()..],
            "fixture `{}`: bytes after the description changed",
            fixture.case.id
        );
        assert_eq!(reparsed.description, description.trim());

        let diff = unified_diff("SKILL.md", &parsed.doc.raw, &edited);
        let hunks = diff.lines().filter(|line| line.starts_with("@@")).count();
        assert_eq!(
            hunks, 1,
            "fixture `{}`: the description rewrite must be one contiguous hunk, not {hunks}:\n{diff}",
            fixture.case.id
        );
        assert!(
            diff.lines().any(|line| line.starts_with('-'))
                && diff.lines().any(|line| line.starts_with('+')),
            "fixture `{}`: the diff must show the description being replaced:\n{diff}",
            fixture.case.id
        );
    }
}

#[test]
fn unknown_frontmatter_keys_and_their_order_survive_an_edit() {
    let fixture = discover()
        .into_iter()
        .find(|fixture| fixture.case.id == "key-order")
        .expect("the key-order fixture exists");
    let parsed = parse_fixture(&fixture).expect("the fixture parses").doc;
    let keys: Vec<&str> = parsed
        .unknown_keys
        .iter()
        .map(|key| key.key.as_str())
        .collect();
    assert_eq!(keys, vec!["license", "metadata", "argument-hint"]);
    let edited = set_description(
        &parsed,
        "A rewritten description for the key-order fixture.",
    )
    .expect("the edit succeeds");
    let reparsed = parse_skill_doc(edited.as_bytes()).expect("the edit re-parses");
    let reparsed_keys: Vec<&str> = reparsed
        .unknown_keys
        .iter()
        .map(|key| key.key.as_str())
        .collect();
    assert_eq!(reparsed_keys, keys);
    for (before, after) in parsed.unknown_keys.iter().zip(&reparsed.unknown_keys) {
        assert_eq!(before.raw, after.raw, "unknown key text must be preserved");
    }
}

#[test]
fn every_parser_error_variant_is_exercised_by_a_fixture() {
    let mut variants: Vec<String> = Vec::new();
    for fixture in discover() {
        let expected = read_expected(&fixture);
        if let Some(variant) = expected["parse"]["error"]["variant"].as_str() {
            variants.push(variant.to_owned());
        }
    }
    for required in PARSER_VARIANTS {
        assert!(
            variants.iter().any(|variant| variant == required),
            "no fixture exercises parser error `{required}`; covered: {variants:?}"
        );
    }
}

#[test]
fn all_three_template_renders_match_their_golden_files_and_reparse() {
    for template in SkillTemplate::ALL {
        let golden_path = template_dir().join(format!("{}.md", template.as_str()));
        let golden = fs::read_to_string(&golden_path)
            .unwrap_or_else(|error| panic!("{}: {error}", golden_path.display()));
        let rendered = render_template(template);
        assert_eq!(
            rendered,
            golden,
            "template `{}` no longer matches {}",
            template.as_str(),
            golden_path.display()
        );
        let parsed = parse_skill_doc(golden.as_bytes()).unwrap_or_else(|error| {
            panic!("template `{}` does not parse: {error}", template.as_str())
        });
        assert_eq!(parsed.name, TEMPLATE_SLUG);
        assert_eq!(parsed.description, TEMPLATE_DESCRIPTION);
        assert!(
            parsed
                .body()
                .contains(template.body().lines().next().unwrap_or("")),
            "template `{}` body is missing from the rendered document",
            template.as_str()
        );
    }
}

#[test]
#[ignore = "rewrites checked-in goldens; run manually after reviewing a behaviour change"]
fn regenerate() {
    fs::create_dir_all(template_dir()).expect("template golden directory");
    for template in SkillTemplate::ALL {
        fs::write(
            template_dir().join(format!("{}.md", template.as_str())),
            render_template(template),
        )
        .expect("write template golden");
    }
    for fixture in discover() {
        let (expected, edited) = build_expectation(&fixture);
        let mut document = serde_json::Map::new();
        document.insert("schema_version".to_owned(), json!(1));
        document.insert("id".to_owned(), json!(fixture.case.id));
        for (key, value) in expected.as_object().expect("expectation is an object") {
            document.insert(key.clone(), value.clone());
        }
        fs::write(
            fixture.dir.join("expected.json"),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&Value::Object(document)).expect("serialize golden")
            ),
        )
        .expect("write expected.json");
        if let Some(edited) = edited {
            fs::write(fixture.dir.join("edited.md"), edited).expect("write edited.md");
        }
    }
}
