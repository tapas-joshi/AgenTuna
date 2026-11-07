//! Skill templates, slug generation and live preview.

use std::time::{Duration, Instant};

use at_engine::skill_templates::{
    DescriptionIssue, PLACEHOLDER_SLUG, SkillDraft, SkillPreview, SkillTemplate, SlugError,
    render_preview, slugify,
};
use at_model::skill::SkillDoc;
use at_store::skill::parse_skill_doc;

fn preview(display_name: &str, description: &str, template: SkillTemplate) -> SkillPreview {
    render_preview(&SkillDraft::new(display_name, description, template))
}

fn parsed(preview: &SkillPreview) -> SkillDoc {
    parse_skill_doc(preview.markdown.as_bytes()).expect("rendered document parses")
}

#[test]
fn slugify_matches_the_canonical_examples() {
    assert_eq!(slugify("My Great Skill!").expect("slug"), "my-great-skill");
    assert_eq!(slugify("Étude").expect("slug"), "etude");
    assert!(matches!(slugify("***"), Err(SlugError::Empty)));
}

#[test]
fn slugify_normalizes_whitespace_punctuation_and_case() {
    assert_eq!(
        slugify("  --Hello--World--  ").expect("slug"),
        "hello-world"
    );
    assert_eq!(slugify("C++ & Rust").expect("slug"), "c-rust");
    assert_eq!(slugify("2026 Q1 Plan").expect("slug"), "2026-q1-plan");
    assert_eq!(slugify("ümlaut Ölçüm").expect("slug"), "umlaut-olcum");
    assert_eq!(slugify("e\u{0301}tude").expect("slug"), "etude");
    assert!(matches!(slugify(""), Err(SlugError::Empty)));
    assert!(matches!(slugify("   "), Err(SlugError::Empty)));
    assert!(matches!(slugify("ー"), Err(SlugError::Empty)));
    assert_eq!(slugify("ﬁle").expect("ligature"), "file");
}

#[test]
fn slugify_reports_a_slug_over_the_name_limit() {
    let long = "a".repeat(65);
    assert!(matches!(
        slugify(&long),
        Err(SlugError::TooLong {
            len: 65,
            limit: 64,
            ..
        })
    ));
    let exactly = "a".repeat(64);
    assert_eq!(slugify(&exactly).expect("64 chars are allowed"), exactly);
}

#[test]
fn blank_renders_minimal_frontmatter_and_a_usage_section() {
    let preview = preview(
        "My Great Skill!",
        "Summarizes a change.",
        SkillTemplate::Blank,
    );
    let document = &preview.markdown;
    assert!(document.starts_with("---\nname: my-great-skill\ndescription: "));
    assert!(document.contains("\n---\n\n# My Great Skill!\n"));
    assert!(document.contains("\nUse when: Summarizes a change.\n"));
    assert!(document.contains("## Usage\n"));
    assert!(!document.contains("- [ ]"));
    assert!(!document.contains("## Inputs"));
    assert!(preview.is_valid());

    let doc = parsed(&preview);
    assert_eq!(doc.name, "my-great-skill");
    assert_eq!(doc.description, "Summarizes a change.");
}

#[test]
fn checklist_renders_markdown_checkboxes() {
    let preview = preview(
        "Release Check",
        "Run before publishing.",
        SkillTemplate::Checklist,
    );
    let document = &preview.markdown;
    assert!(document.contains("## Checklist\n"));
    assert_eq!(document.matches("- [ ] ").count(), 3);
    assert!(document.contains("- [ ] First item"));
    assert!(!document.contains("## Inputs"));
    assert!(preview.is_valid());

    let doc = parsed(&preview);
    assert_eq!(doc.name, "release-check");
    assert_eq!(doc.description, "Run before publishing.");
}

#[test]
fn workflow_renders_numbered_steps_inputs_and_outputs() {
    let preview = preview(
        "Review Flow",
        "Review a pull request.",
        SkillTemplate::Workflow,
    );
    let document = &preview.markdown;
    assert!(document.contains("## Workflow\n"));
    assert!(document.contains("\n1. First step."));
    assert!(document.contains("\n2. Second step."));
    assert!(document.contains("\n3. Third step."));
    assert!(document.contains("## Inputs\n"));
    assert!(document.contains("## Outputs\n"));
    assert!(preview.is_valid());

    let doc = parsed(&preview);
    assert_eq!(doc.name, "review-flow");
    assert_eq!(doc.description, "Review a pull request.");
}

#[test]
fn every_template_round_trips_through_the_skill_parser() {
    let descriptions = [
        "Plain description.",
        "Quotes \"like this\" and a colon: yes, plus a # hash.",
        "- starts with a dash",
        "true",
        "123",
        "Line one.\nLine two.",
        "Trailing colon:",
        "Unicode: Étude — naïve",
        "A single ' quote and a \\ backslash.",
    ];
    for template in SkillTemplate::ALL {
        for description in descriptions {
            let preview = preview("Round Trip", description, template);
            assert!(
                preview.is_valid(),
                "{template:?} preview invalid for {description:?}: {:?}",
                preview.description_error
            );
            let doc = parsed(&preview);
            assert_eq!(doc.name, "round-trip", "{template:?}");
            assert_eq!(doc.description, description, "{template:?}");
        }
    }
}

#[test]
fn preview_escapes_yaml_scalars_that_would_change_meaning() {
    let tricky = "Quotes \"x\", a colon: value, and a newline\nsecond line.";
    let rendered = preview("Tricky", tricky, SkillTemplate::Blank);
    let doc = parsed(&rendered);
    assert_eq!(doc.description, tricky);
    assert!(rendered.markdown.contains("description: \""));

    for description in [
        "- leading dash",
        "? question",
        "true",
        "false",
        "null",
        "~",
        "42",
        "3.5",
        "key: value",
        "suffix #tag",
        "ends with colon:",
        "'single quoted'",
        "\"double quoted\"",
    ] {
        let preview = preview("Scalars", description, SkillTemplate::Blank);
        let doc = parsed(&preview);
        assert_eq!(doc.description, description, "description {description:?}");
    }
}

#[test]
fn preview_keeps_rendering_while_inputs_are_invalid() {
    let empty_name = preview("***", "Still typing.", SkillTemplate::Blank);
    assert!(matches!(empty_name.name_error, Some(SlugError::Empty)));
    assert_eq!(empty_name.slug, None);
    assert!(!empty_name.is_valid());
    assert!(
        empty_name
            .markdown
            .contains(&format!("name: {PLACEHOLDER_SLUG}"))
    );
    assert!(empty_name.markdown.contains("## Usage"));

    let empty_description = preview("Typing", "", SkillTemplate::Workflow);
    assert_eq!(
        empty_description.description_error,
        Some(DescriptionIssue::Empty)
    );
    assert!(empty_description.markdown.contains("## Workflow"));
    assert!(!empty_description.is_valid());

    let long_description = "a".repeat(1025);
    let too_long = preview("Typing", &long_description, SkillTemplate::Blank);
    assert!(matches!(
        too_long.description_error,
        Some(DescriptionIssue::TooLong {
            len: 1025,
            limit: 1024
        })
    ));
    assert!(too_long.markdown.len() > long_description.len());
    assert!(!too_long.is_valid());
}

#[test]
fn title_falls_back_to_the_slug_for_an_unnamed_draft() {
    let preview = preview("", "Draft.", SkillTemplate::Blank);
    assert!(preview.markdown.contains("\n# untitled\n"));
    assert!(preview.markdown.contains("name: untitled"));
    assert!(!preview.is_valid());
}

#[test]
fn preview_is_valid_for_a_normal_draft() {
    let preview = preview(
        "  My Great Skill!  ",
        "  Summarizes a change.  ",
        SkillTemplate::Checklist,
    );
    assert_eq!(preview.slug.as_deref(), Some("my-great-skill"));
    assert!(preview.is_valid());
    let doc = parsed(&preview);
    assert_eq!(doc.description, "Summarizes a change.");
}

#[test]
fn a_twenty_kilobyte_description_renders_far_below_the_interactive_budget() {
    let description = "word ".repeat(4 * 1024);
    assert!(description.len() >= 20 * 1024);
    let draft = SkillDraft::new("Large Preview", &description, SkillTemplate::Workflow);

    // Warm up allocator and code paths once, then measure the pure render.
    let warm = render_preview(&draft);
    assert!(warm.markdown.len() > description.len());

    const RUNS: u32 = 100;
    let start = Instant::now();
    for _ in 0..RUNS {
        std::hint::black_box(render_preview(&draft));
    }
    let per_call = start.elapsed() / RUNS;
    // 50 ms is the interactive budget in the shipped (release) build. A debug build is much
    // slower, and the assertion is an average over 100 renders, so the debug ceiling only has
    // to catch a catastrophic regression.
    let budget = if cfg!(debug_assertions) {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(50)
    };
    assert!(
        per_call < budget,
        "20 KB preview averaged {per_call:?}, above {budget:?}"
    );
}
