//! Generative robustness tests for the managed-block renderer used by rule files.
//!
//! A rule file may document the marker syntax: a fenced example, an inline-code example or a
//! lookalike comment must stay user text. The renderer may only rewrite the bytes between the
//! real marker pair; every other byte is asserted unchanged.

use std::path::Path;

use at_fs::managed_block::{
    MANAGED_BEGIN, MANAGED_END, ManagedBlockError, managed_span, render_managed_block,
};
use at_fs::testkit::generative::Rng;

/// Seeds per property. Each seed runs the full builder, so the real case count is higher.
const CASES: u64 = 320;

const MARKER_BEGIN: &str = "<!-- agentuna:begin -->";
const MARKER_END: &str = "<!-- agentuna:end -->";

/// One generated document plus the byte offsets of its real managed block.
struct Fixture {
    text: String,
    /// Bytes up to here (through the begin marker) must never change.
    body_start: usize,
    /// Bytes from here (the end marker's line, indentation included) must never change.
    end_line_start: usize,
    /// Marker examples that must survive byte for byte.
    examples: Vec<String>,
    /// Whether the document uses CRLF line endings.
    crlf: bool,
}

fn chunk(rng: &mut Rng, nl: &str, examples: &mut Vec<String>) -> String {
    match rng.below(7) {
        0 => format!("plain {}{nl}", rng.unicode_word()),
        1 => {
            let example = format!("```markdown{nl}{MARKER_BEGIN}{nl}{MARKER_END}{nl}```{nl}");
            examples.push(example.clone());
            example
        }
        2 => {
            let example = format!("~~~{nl}{MARKER_END}{nl}~~~{nl}");
            examples.push(example.clone());
            example
        }
        3 => {
            let example = format!("  ```{nl}{MARKER_BEGIN}{nl}```{nl}");
            examples.push(example.clone());
            example
        }
        4 => {
            let example = format!("Use `{MARKER_BEGIN}` to open and `{MARKER_END}` to close.{nl}");
            examples.push(example.clone());
            example
        }
        5 => {
            let example =
                format!("<!-- agentuna:beginning --> and <!--agentuna:end--> are lookalikes.{nl}");
            examples.push(example.clone());
            example
        }
        _ => {
            let example =
                format!("```text{nl}not a marker: {MARKER_BEGIN}{nl}still inside{nl}```{nl}");
            examples.push(example.clone());
            example
        }
    }
}

fn build(seed: u64) -> Fixture {
    let mut rng = Rng::new(seed ^ 0x10C_0FFEE);
    let crlf = rng.chance(3);
    let nl = if crlf { "\r\n" } else { "\n" };
    let mut examples = Vec::new();
    let mut text = String::new();
    for _ in 0..rng.below(5) {
        text.push_str(&chunk(&mut rng, nl, &mut examples));
    }
    let indent = if rng.chance(3) { "" } else { "  " };
    text.push_str(indent);
    text.push_str(MARKER_BEGIN);
    let body_start = text.len();
    text.push_str(nl);
    for _ in 0..rng.below(4) {
        text.push_str(&format!("old line {}{nl}", rng.unicode_word()));
    }
    let end_line_start = text.len();
    text.push_str(indent);
    text.push_str(MARKER_END);
    text.push_str(nl);
    for _ in 0..rng.below(5) {
        text.push_str(&chunk(&mut rng, nl, &mut examples));
    }
    text.push_str(nl);
    if rng.chance(2) {
        // Drop only the separator line's ending: the last user chunk keeps its own newline,
        // so a recorded example always remains an exact substring.
        text.truncate(text.len() - nl.len());
    }
    Fixture {
        text,
        body_start,
        end_line_start,
        examples,
        crlf,
    }
}

fn render(text: &str, content: &str) -> Vec<u8> {
    render_managed_block(
        Path::new("AGENTS.md"),
        Some(text.as_bytes()),
        content.as_bytes(),
    )
    .expect("generated document must render")
}

#[test]
fn marker_strings_match_the_public_constants() {
    assert_eq!(MARKER_BEGIN.as_bytes(), MANAGED_BEGIN);
    assert_eq!(MARKER_END.as_bytes(), MANAGED_END);
}

#[test]
fn block_edits_preserve_every_byte_outside_the_block() {
    for seed in 0..CASES {
        let fixture = build(seed);
        let mut rng = Rng::new(seed ^ 0xB10C);
        let content = match rng.below(4) {
            0 => String::new(),
            1 => format!("new {}", rng.unicode_word()),
            2 => format!("first line\nsecond {} line\n", rng.unicode_word()),
            _ => format!("{}\n{}\n", rng.unicode_word(), rng.token()),
        };
        let output = render(&fixture.text, &content);
        let output = String::from_utf8(output).expect("utf-8 output");

        assert!(
            output.starts_with(&fixture.text[..fixture.body_start]),
            "seed={seed}: bytes before the block changed\nbefore: {:?}\nafter: {:?}",
            &fixture.text[..fixture.body_start.min(fixture.text.len())],
            &output[..fixture.body_start.min(output.len())]
        );
        assert!(
            output.ends_with(&fixture.text[fixture.end_line_start..]),
            "seed={seed}: bytes from the end marker's line onward changed\nbefore: {:?}\nafter: {:?}",
            &fixture.text[fixture.end_line_start..],
            &output[output
                .len()
                .saturating_sub(fixture.text.len() - fixture.end_line_start)..]
        );

        for example in &fixture.examples {
            assert_eq!(
                output.matches(example).count(),
                fixture.text.matches(example).count(),
                "seed={seed}: marker example was rewritten or removed: {example:?}\nfull output: {output:?}"
            );
        }

        let span = managed_span(Path::new("AGENTS.md"), output.as_bytes())
            .expect("rendered block must be detectable");
        assert_eq!(&output[span.begin..span.body_start], MARKER_BEGIN);
        assert_eq!(&output[span.end..span.end + MARKER_END.len()], MARKER_END);
        let ending = if fixture.crlf { "\r\n" } else { "\n" };
        let normalized: String = content.replace("\r\n", "\n").replace('\n', ending);
        let tail = if normalized.is_empty() || normalized.ends_with(ending) {
            ""
        } else {
            ending
        };
        assert_eq!(
            &output[span.body_start..span.body_end],
            format!("{ending}{normalized}{tail}"),
            "seed={seed}: the block body is not the requested content"
        );

        if fixture.crlf {
            let body = &output[span.body_start..span.body_end];
            let bytes = body.as_bytes();
            for (index, byte) in bytes.iter().enumerate() {
                if *byte == b'\n' {
                    assert!(
                        index > 0 && bytes[index - 1] == b'\r',
                        "seed={seed}: a CRLF file gained a bare LF inside the block: {body:?}"
                    );
                }
            }
        }

        let again = render(&output, &content);
        assert_eq!(
            again.as_slice(),
            output.as_bytes(),
            "seed={seed}: rendering the same content twice is not idempotent"
        );
    }
}

#[test]
fn marker_examples_are_never_mistaken_for_the_block() {
    for seed in 0..CASES {
        let fixture = build(seed);
        if fixture.examples.is_empty() {
            continue;
        }
        let output = render(&fixture.text, "replaced");
        let output = String::from_utf8(output).expect("utf-8 output");
        for example in &fixture.examples {
            assert!(
                output.contains(example),
                "seed={seed}: example lost: {example:?}\noutput: {output:?}"
            );
        }
    }
}

#[test]
fn malformed_marker_sets_are_typed_errors() {
    let path = Path::new("AGENTS.md");
    type Expectation = fn(&ManagedBlockError) -> bool;
    let cases: &[(&str, Expectation)] = &[
        ("header\n<!-- agentuna:begin -->\nbody\n", |error| {
            matches!(error, ManagedBlockError::MalformedManagedBlock { .. })
        }),
        ("<!-- agentuna:end -->\nbody\n", |error| {
            matches!(error, ManagedBlockError::MalformedManagedBlock { .. })
        }),
        (
            "<!-- agentuna:begin -->\n<!-- agentuna:begin -->\n<!-- agentuna:end -->\n",
            |error| matches!(error, ManagedBlockError::MalformedManagedBlock { .. }),
        ),
        (
            "<!-- agentuna:begin -->\n<!-- agentuna:end -->\n<!-- agentuna:end -->\n",
            |error| matches!(error, ManagedBlockError::MalformedManagedBlock { .. }),
        ),
        (
            "<!-- agentuna:end -->\n<!-- agentuna:begin -->\nbody\n<!-- agentuna:end -->\n",
            |error| matches!(error, ManagedBlockError::MalformedManagedBlock { .. }),
        ),
        (
            "```\n<!-- agentuna:begin -->\n<!-- agentuna:end -->\n```\n",
            |error| matches!(error, ManagedBlockError::AdoptRequired { .. }),
        ),
        (
            "<!-- agentuna:beginning -->\n<!--agentuna:end-->\n",
            |error| matches!(error, ManagedBlockError::AdoptRequired { .. }),
        ),
        ("# plain rules\n", |error| {
            matches!(error, ManagedBlockError::AdoptRequired { .. })
        }),
    ];
    for (text, matches_expected) in cases {
        let error = render_managed_block(path, Some(text.as_bytes()), b"content")
            .expect_err("malformed document must be refused");
        assert!(
            matches_expected(&error),
            "{text:?} produced the wrong error: {error}"
        );
    }
}

#[test]
fn multi_megabyte_user_text_is_preserved() {
    let mut rng = Rng::new(0x5AFE);
    let mut prefix = String::new();
    while prefix.len() < 5 * 1024 * 1024 {
        prefix.push_str(&format!(
            "line {} {}\n",
            rng.next_u64() % 1_000_000,
            rng.unicode_word()
        ));
    }
    let mut text = prefix.clone();
    text.push_str(MARKER_BEGIN);
    text.push('\n');
    text.push_str("old\n");
    text.push_str(MARKER_END);
    text.push('\n');
    let mut suffix = String::new();
    while suffix.len() < 1024 * 1024 {
        suffix.push_str(&format!("tail {}\n", rng.next_u64()));
    }
    text.push_str(&suffix);

    let output = render(&text, "new body");
    let output = String::from_utf8(output).expect("utf-8 output");
    assert!(output.starts_with(&prefix));
    assert!(output.ends_with(&suffix));
    assert!(output.contains("\nnew body\n"));
    assert_eq!(output.matches(MARKER_BEGIN).count(), 1);
}
