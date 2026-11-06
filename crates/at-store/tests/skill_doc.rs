//! Acceptance tests for `SKILL.md` parsing, validation and format-preserving edits.

use at_model::skill::{
    SKILL_DESCRIPTION_MAX_CHARS, SKILL_NAME_MAX_CHARS, SkillError, SkillOrigin,
    validate_skill_description, validate_skill_name,
};
use at_store::skill::{parse_skill_doc, parse_skill_doc_in_folder, set_description};

/// A malformed document and the predicate its error must satisfy.
type MalformedCase = (String, fn(&SkillError) -> bool);

const SIMPLE: &str =
    "---\nname: my-skill\ndescription: Does one thing\nlicense: MIT\n---\n# Body\n";

fn frontmatter(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\nbody\n")
}

#[test]
fn valid_document_exposes_spans_that_map_to_the_source() {
    let doc = parse_skill_doc(SIMPLE.as_bytes()).expect("valid document");
    assert_eq!(doc.name, "my-skill");
    assert_eq!(doc.description, "Does one thing");
    assert_eq!(doc.raw, SIMPLE);
    assert_eq!(
        doc.frontmatter_span.slice(&doc.raw).unwrap(),
        "name: my-skill\ndescription: Does one thing\nlicense: MIT\n"
    );
    assert_eq!(doc.body_span.slice(&doc.raw).unwrap(), "# Body\n");
    assert_eq!(doc.body(), "# Body\n");
    assert_eq!(doc.body_bytes(), b"# Body\n");
    assert_eq!(
        doc.description_span.slice(&doc.raw).unwrap(),
        " Does one thing"
    );
    assert_eq!(doc.unknown_keys.len(), 1);
    assert_eq!(doc.unknown_keys[0].key, "license");
    assert_eq!(doc.unknown_keys[0].raw, "license: MIT");
    assert_eq!(
        doc.unknown_keys[0].span.start(),
        SIMPLE.find("license").unwrap()
    );
}

#[test]
fn unknown_keys_are_preserved_verbatim_in_source_order() {
    let source = "---\nname: my-skill\nlicense: MIT\ndescription: Does one thing\nargument-hint: <path>\nmetadata:\n  author: test\n  version: 1\n---\nbody\n";
    let doc = parse_skill_doc(source.as_bytes()).expect("valid document");
    let keys: Vec<&str> = doc
        .unknown_keys
        .iter()
        .map(|key| key.key.as_str())
        .collect();
    assert_eq!(keys, ["license", "argument-hint", "metadata"]);
    assert_eq!(doc.unknown_keys[0].raw, "license: MIT");
    assert_eq!(doc.unknown_keys[1].raw, "argument-hint: <path>");
    assert_eq!(
        doc.unknown_keys[2].raw,
        "metadata:\n  author: test\n  version: 1"
    );
    for key in &doc.unknown_keys {
        assert_eq!(key.span.slice(&doc.raw).unwrap(), key.raw);
    }
}

#[test]
fn every_malformed_case_returns_its_own_error() {
    let long_name = "a".repeat(SKILL_NAME_MAX_CHARS + 1);
    let long_description = "x".repeat(SKILL_DESCRIPTION_MAX_CHARS + 1);
    let cases: Vec<MalformedCase> = vec![
        ("name: my-skill\ndescription: x\n".to_owned(), |e| {
            matches!(e, SkillError::MissingFrontmatter)
        }),
        ("---\nname: my-skill\ndescription: x\n".to_owned(), |e| {
            matches!(e, SkillError::UnclosedFrontmatter)
        }),
        ("---\nname: [oops\ndescription: x\n---\n".to_owned(), |e| {
            matches!(e, SkillError::InvalidFrontmatter { .. })
        }),
        ("---\ndescription: x\n---\n".to_owned(), |e| {
            matches!(e, SkillError::MissingName)
        }),
        ("---\nname: my-skill\n---\n".to_owned(), |e| {
            matches!(e, SkillError::MissingDescription)
        }),
        ("---\nname: true\ndescription: x\n---\n".to_owned(), |e| {
            matches!(e, SkillError::NameNotString)
        }),
        (
            "---\nname: my-skill\ndescription: true\n---\n".to_owned(),
            |e| matches!(e, SkillError::DescriptionNotString),
        ),
        ("---\nname: \"\"\ndescription: x\n---\n".to_owned(), |e| {
            matches!(e, SkillError::NameEmpty)
        }),
        (frontmatter("MySkill", "x"), |e| {
            matches!(e, SkillError::NameUppercase { .. })
        }),
        (frontmatter("my skill", "x"), |e| {
            matches!(e, SkillError::NameWhitespace { .. })
        }),
        (frontmatter(&long_name, "x"), |e| {
            matches!(e, SkillError::NameTooLong { .. })
        }),
        (frontmatter("-my-skill", "x"), |e| {
            matches!(e, SkillError::NameLeadingHyphen { .. })
        }),
        (frontmatter("my-skill-", "x"), |e| {
            matches!(e, SkillError::NameTrailingHyphen { .. })
        }),
        (frontmatter("my--skill", "x"), |e| {
            matches!(e, SkillError::NameDoubleHyphen { .. })
        }),
        (frontmatter("my_skill", "x"), |e| {
            matches!(e, SkillError::NameInvalidCharacter { .. })
        }),
        (
            "---\nname: my-skill\ndescription: \"   \"\n---\n".to_owned(),
            |e| matches!(e, SkillError::EmptyDescription),
        ),
        (frontmatter("my-skill", &long_description), |e| {
            matches!(e, SkillError::DescriptionTooLong { .. })
        }),
    ];
    for (source, expected) in cases {
        let error = parse_skill_doc(source.as_bytes())
            .expect_err(&format!("expected an error for {source:?}"));
        assert!(
            expected(&error),
            "unexpected error for {source:?}: {error:?}"
        );
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn name_and_description_rules_are_exposed_for_wizards() {
    assert!(validate_skill_name("my-skill-2").is_ok());
    assert!(validate_skill_name(&"a".repeat(SKILL_NAME_MAX_CHARS)).is_ok());
    assert!(matches!(
        validate_skill_name(&"a".repeat(SKILL_NAME_MAX_CHARS + 1)),
        Err(SkillError::NameTooLong { .. })
    ));
    assert!(validate_skill_description("Use when reviewing a change").is_ok());
    assert!(matches!(
        validate_skill_description(""),
        Err(SkillError::EmptyDescription)
    ));
    assert!(matches!(
        validate_skill_description(&"x".repeat(SKILL_DESCRIPTION_MAX_CHARS + 1)),
        Err(SkillError::DescriptionTooLong { .. })
    ));
}

#[test]
fn crlf_and_bom_documents_parse_with_exact_offsets() {
    let source = "\u{feff}---\r\nname: my-skill\r\ndescription: Handles files\r\nlicense: MIT\r\n---\r\n# Body\r\n";
    let doc = parse_skill_doc(source.as_bytes()).expect("BOM + CRLF document");
    assert_eq!(doc.name, "my-skill");
    assert_eq!(doc.description, "Handles files");
    assert_eq!(doc.raw, source);
    assert_eq!(
        doc.frontmatter_span.slice(&doc.raw).unwrap(),
        "name: my-skill\r\ndescription: Handles files\r\nlicense: MIT\r\n"
    );
    assert_eq!(doc.body_span.slice(&doc.raw).unwrap(), "# Body\r\n");
    assert_eq!(
        doc.description_span.slice(&doc.raw).unwrap(),
        " Handles files"
    );
}

#[test]
fn folded_and_literal_descriptions_resolve_to_their_text() {
    let folded = "---\nname: my-skill\ndescription: >-\n  line one\n  line two\n---\n";
    let doc = parse_skill_doc(folded.as_bytes()).expect("folded description");
    assert_eq!(doc.description, "line one line two");
    assert_eq!(
        doc.description_span.slice(&doc.raw).unwrap(),
        " >-\n  line one\n  line two"
    );

    let literal = "---\nname: my-skill\ndescription: |\n  first\n  second\n---\n";
    let doc = parse_skill_doc(literal.as_bytes()).expect("literal description");
    assert_eq!(doc.description, "first\nsecond");
}

#[test]
fn description_edit_rewrites_only_the_description_scalar() {
    let doc = parse_skill_doc(SIMPLE.as_bytes()).expect("valid document");
    let edited = set_description(&doc, "Does another thing").expect("edit");
    assert_eq!(
        edited,
        "---\nname: my-skill\ndescription: Does another thing\nlicense: MIT\n---\n# Body\n"
    );
    assert_eq!(
        edited.len(),
        SIMPLE.len() - "Does one thing".len() + "Does another thing".len()
    );
}

#[test]
fn description_edit_keeps_bom_crlf_quotes_and_blocks() {
    let source = "\u{feff}---\r\nname: my-skill\r\ndescription: \"Uses quotes\"\r\nmetadata:\r\n  owner: team\r\n---\r\nBody line\r\n";
    let doc = parse_skill_doc(source.as_bytes()).expect("BOM + CRLF document");
    let edited = set_description(&doc, "New text").expect("edit");
    assert_eq!(
        edited,
        "\u{feff}---\r\nname: my-skill\r\ndescription: New text\r\nmetadata:\r\n  owner: team\r\n---\r\nBody line\r\n"
    );

    let folded =
        "---\nname: my-skill\ndescription: >-\n  line one\n  line two\nlicense: MIT\n---\nbody\n";
    let doc = parse_skill_doc(folded.as_bytes()).expect("folded description");
    let edited = set_description(&doc, "Replaced").expect("edit");
    assert_eq!(
        edited,
        "---\nname: my-skill\ndescription: Replaced\nlicense: MIT\n---\nbody\n"
    );

    let literal = "---\nname: my-skill\ndescription: |\n  first\n  second\n---\n";
    let doc = parse_skill_doc(literal.as_bytes()).expect("literal description");
    let edited = set_description(&doc, "first\nsecond").expect("edit");
    assert_eq!(
        edited,
        "---\nname: my-skill\ndescription: \"first\\nsecond\"\n---\n"
    );
}

#[test]
fn description_edit_handles_continuations_and_unicode() {
    let continued = "---\nname: my-skill\ndescription:\n  split over\n  two lines\n---\n";
    let doc = parse_skill_doc(continued.as_bytes()).expect("continuation description");
    assert_eq!(doc.description, "split over two lines");
    let edited = set_description(&doc, "one line").expect("edit");
    assert_eq!(edited, "---\nname: my-skill\ndescription: one line\n---\n");

    let unicode = "---\nname: my-skill\ndescription: Caf\u{e9} au lait\n---\n";
    let doc = parse_skill_doc(unicode.as_bytes()).expect("unicode description");
    assert_eq!(doc.description, "Caf\u{e9} au lait");
    let edited = set_description(&doc, "Th\u{e9} noir").expect("edit");
    assert_eq!(
        edited,
        "---\nname: my-skill\ndescription: Th\u{e9} noir\n---\n"
    );
}

#[test]
fn description_edit_rejects_values_outside_the_limit() {
    let doc = parse_skill_doc(SIMPLE.as_bytes()).expect("valid document");
    assert!(matches!(
        set_description(&doc, "   "),
        Err(SkillError::EmptyDescription)
    ));
    assert!(matches!(
        set_description(&doc, &"x".repeat(SKILL_DESCRIPTION_MAX_CHARS + 1)),
        Err(SkillError::DescriptionTooLong { .. })
    ));
}

#[test]
fn folder_name_mismatch_blocks_creation_and_warns_on_import() {
    let source = frontmatter("my-skill", "Does one thing");
    let created = parse_skill_doc_in_folder(source.as_bytes(), "my-skill", SkillOrigin::Create)
        .expect("matching folder");
    assert!(created.warning.is_none());

    let blocked = parse_skill_doc_in_folder(source.as_bytes(), "other-folder", SkillOrigin::Create)
        .expect_err("mismatch must block creation");
    assert!(matches!(blocked, SkillError::NameMismatch { .. }));

    let imported =
        parse_skill_doc_in_folder(source.as_bytes(), "other-folder", SkillOrigin::Import)
            .expect("mismatch is allowed on import");
    let warning = imported.warning.expect("import warning");
    assert_eq!(warning.name, "my-skill");
    assert_eq!(warning.folder, "other-folder");
}

#[test]
fn oversized_and_non_utf8_documents_are_rejected() {
    let oversized = vec![b'a'; at_model::skill::MAX_SKILL_DOC_BYTES + 1];
    assert!(matches!(
        parse_skill_doc(&oversized),
        Err(SkillError::DocTooLarge { .. })
    ));

    let mut invalid = SIMPLE.as_bytes().to_vec();
    invalid.extend_from_slice(&[0xFF, 0xFE]);
    assert!(matches!(
        parse_skill_doc(&invalid),
        Err(SkillError::InvalidUtf8 { .. })
    ));
}

#[test]
fn frontmatter_must_be_a_block_mapping() {
    let flow = "---\n{name: my-skill, description: x}\n---\nbody\n";
    assert!(matches!(
        parse_skill_doc(flow.as_bytes()),
        Err(SkillError::FrontmatterNotMapping)
    ));
}

#[test]
fn two_hundred_representative_documents_parse() {
    for index in 0..200_u32 {
        let name = format!("skill-{index}");
        let words = format!("Skill number {index} handles the case");
        let description = match index % 4 {
            0 => words.clone(),
            1 => format!("\"{words}\""),
            2 => format!(">-\n  Skill number {index}\n  handles the case"),
            _ => format!("|\n  Skill number {index}\n  handles the case"),
        };
        let folded = index % 4 == 2;
        let extra = match index % 4 {
            0 => "license: MIT\n".to_owned(),
            1 => "argument-hint: <path>\n".to_owned(),
            2 => "metadata:\n  author: test\n  tags: [a, b]\n".to_owned(),
            _ => "allowed-tools: Bash(git:*) Read\nversion: 1.2.3\n".to_owned(),
        };
        let body = format!("# {name}\n\nBody for skill {index}.\n");
        let front = format!("---\nname: {name}\ndescription: {description}\n{extra}---\n");
        let document = if index % 3 == 0 {
            format!("\u{feff}{}", front.replace('\n', "\r\n")) + &body
        } else {
            front + &body
        };
        let doc = parse_skill_doc(document.as_bytes())
            .unwrap_or_else(|error| panic!("document {index} failed: {error}"));
        assert_eq!(doc.name, name);
        if folded {
            assert_eq!(doc.description, words);
        } else if index % 4 == 3 {
            assert_eq!(
                doc.description,
                format!("Skill number {index}\nhandles the case")
            );
        } else {
            assert_eq!(doc.description, words);
        }
        assert_eq!(doc.body(), body);
    }
}
