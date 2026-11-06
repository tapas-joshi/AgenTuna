//! Filesystem behaviour of reading a `SKILL.md`: bounds, folder-name policy and errors.

use std::fs;
use std::path::{Path, PathBuf};

use at_model::skill::{SkillError, SkillOrigin};
use at_store::skill::{SkillReadError, read_skill_doc, read_skill_doc_in_folder};

const DOCUMENT: &str = "---\nname: my-skill\ndescription: Does one thing\n---\n# Body\n";

fn write_skill(root: &Path, folder: &str) -> PathBuf {
    let directory = root.join(folder);
    fs::create_dir_all(&directory).expect("skill directory");
    let path = directory.join("SKILL.md");
    fs::write(&path, DOCUMENT).expect("SKILL.md");
    path
}

#[test]
fn reads_a_document_from_its_folder() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let path = write_skill(sandbox.path(), "my-skill");
    let doc = read_skill_doc(&path).expect("valid document");
    assert_eq!(doc.name, "my-skill");
    assert_eq!(doc.description, "Does one thing");

    let created = read_skill_doc_in_folder(&path, SkillOrigin::Create).expect("matching folder");
    assert!(created.warning.is_none());
    assert_eq!(created.path, path);
}

#[test]
fn folder_name_mismatch_blocks_create_and_warns_import() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let path = write_skill(sandbox.path(), "other-folder");

    let error = read_skill_doc_in_folder(&path, SkillOrigin::Create)
        .expect_err("mismatch must block creation");
    assert!(matches!(
        error,
        SkillReadError::Invalid {
            source: SkillError::NameMismatch { .. },
            ..
        }
    ));

    let imported = read_skill_doc_in_folder(&path, SkillOrigin::Import).expect("import");
    let warning = imported.warning.expect("warning");
    assert_eq!(warning.name, "my-skill");
    assert_eq!(warning.folder, "other-folder");
}

#[test]
fn oversized_documents_are_refused_before_parsing() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let path = write_skill(sandbox.path(), "my-skill");
    let huge = vec![b'a'; at_model::skill::MAX_SKILL_DOC_BYTES + 1];
    fs::write(&path, huge).expect("oversized file");
    let error = read_skill_doc(&path).expect_err("oversized");
    assert!(matches!(
        error,
        SkillReadError::Invalid {
            source: SkillError::DocTooLarge { .. },
            ..
        }
    ));
}

#[test]
fn directories_and_invalid_bytes_report_distinct_errors() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let directory = sandbox.path().join("my-skill");
    fs::create_dir(&directory).expect("directory");
    let error = read_skill_doc(&directory).expect_err("directory is not a file");
    assert!(matches!(error, SkillReadError::NotRegularFile { .. }));

    let path = directory.join("SKILL.md");
    fs::write(&path, [0xFF_u8, 0xFE, 0x00]).expect("invalid utf-8");
    let error = read_skill_doc(&path).expect_err("invalid utf-8");
    assert!(matches!(
        error,
        SkillReadError::Invalid {
            source: SkillError::InvalidUtf8 { .. },
            ..
        }
    ));

    let missing = sandbox.path().join("missing").join("SKILL.md");
    let error = read_skill_doc(&missing).expect_err("missing file");
    assert!(matches!(error, SkillReadError::Io { .. }));
}
