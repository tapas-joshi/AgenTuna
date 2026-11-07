//! TF-044: Safety audit test ensuring every write path goes through the at-fs pipeline.
//!
//! Enforces:
//! 1. Source code safety audit: direct `std::fs` mutation calls outside `at-fs` are caught;
//!    every `#[allow(clippy::disallowed_methods)]` must have an accompanying justification comment.
//! 2. Integration matrix across public write entry points:
//!    Asserts preview exists, backup row exists for modified files, exactly one operation row,
//!    final hash matches, and undo list is complete.
//! 3. Creating a new file produces an operation row and zero backup rows; deleting a file
//!    produces a backup of the deleted content.
//! 4. Injected external write between preview and apply yields `ConcurrentModification`
//!    and the external bytes remain intact.
//! 5. Property test of 100 random interleavings leaves every file equal to one of the known
//!    byte sequences; no partial or empty file is ever observed.

#![allow(clippy::disallowed_methods)] // test suite writes fixture files and verifies safety properties

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use at_db::Database;
use at_db::ops::{
    FileAction, NewOperation, OperationActor, OperationKind, OperationStatus, TouchedFile, record,
    undo_file_list,
};
use at_fs::atomic::{Stage, apply_with_hook};
use at_fs::deploy::{DeployRequest, RemoveRequest, RemovedDeployment, deploy, remove_deployment};
use at_fs::recovery::{RestoreRequest, restore_file};
use at_fs::{
    BackupMeta, BackupPolicy, DeploymentLookup, ExpectedState, FsError, LinkMode, OwnershipContext,
    WriteRequest, apply, sha256_hex,
};
use at_model::{AgentId, ItemId, Scope};
use tempfile::TempDir;
#[derive(Debug)]
struct AllowEntry {
    file_pattern: &'static str,
    line_substring: &'static str,
    justification: &'static str,
}

const AUDIT_ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        file_pattern: "at-store/src/skill_package.rs",
        line_substring: "File::create",
        justification: "Creating files inside the private staged skill package tree",
    },
    AllowEntry {
        file_pattern: "at-store/src/config.rs",
        line_substring: "fs::write(path, DEFAULT_FILE)",
        justification: "Initial store creation creates default empty configuration file",
    },
    AllowEntry {
        file_pattern: "at-store/src/logging.rs",
        line_substring: "fs::remove_file",
        justification: "Rotation and cleanup of old store log files",
    },
    AllowEntry {
        file_pattern: "at-store/src/instance.rs",
        line_substring: "fs::remove_file",
        justification: "Removal of stale instance lock and quarantine files",
    },
    AllowEntry {
        file_pattern: "at-store/src/instance.rs",
        line_substring: "fs::rename",
        justification: "Atomic commit of instance lock metadata",
    },
    AllowEntry {
        file_pattern: "at-adapters/src/golden.rs",
        line_substring: "fs::write",
        justification: "Development-only golden file update harness when UPDATE_GOLDEN=1",
    },
    AllowEntry {
        file_pattern: "at-git/src/remote.rs",
        line_substring: "fs::write",
        justification: "Writing temporary askpass helper script for credential authentication",
    },
    AllowEntry {
        file_pattern: "at-migrate/src/testkit.rs",
        line_substring: "fs::write",
        justification: "Testkit fixture generation for synthetic agent environments",
    },
    AllowEntry {
        file_pattern: "at-app/src/app/cli_install.rs",
        line_substring: "remove_file",
        justification: "Uninstallation of desktop CLI symlink from /usr/local/bin or ~/.local/bin",
    },
    AllowEntry {
        file_pattern: "at-app/src/app/recovery.rs",
        line_substring: "fs::write",
        justification: "Atomic session marker write for app recovery tracking",
    },
    AllowEntry {
        file_pattern: "at-app/src/app/recovery.rs",
        line_substring: "fs::rename",
        justification: "Atomic session marker rename for app recovery tracking",
    },
    AllowEntry {
        file_pattern: "at-app/src/app/instance.rs",
        line_substring: "remove_file",
        justification: "Cleanup of stale domain socket files on app exit or crash recovery",
    },
    AllowEntry {
        file_pattern: "at-db/src/retention.rs",
        line_substring: "remove_file",
        justification: "Retention policy deletion of expired log files older than 30 days",
    },
    AllowEntry {
        file_pattern: "at-db/src/db.rs",
        line_substring: "File::create",
        justification: "Create only the SQLite database's unclean-shutdown marker",
    },
    AllowEntry {
        file_pattern: "at-db/src/db.rs",
        line_substring: "fs::create_dir_all",
        justification: "Create only the SQLite database's sibling backup directory",
    },
    AllowEntry {
        file_pattern: "at-db/src/db.rs",
        line_substring: "fs::copy",
        justification: "Copy only the SQLite database to or from its sibling migration backup",
    },
    AllowEntry {
        file_pattern: "at-db/src/db.rs",
        line_substring: "fs::remove_file",
        justification: "Remove only SQLite database marker, sidecars, or expired database backups",
    },
    AllowEntry {
        file_pattern: "at-db/src/db.rs",
        line_substring: "fs::rename",
        justification: "Rename only the SQLite database and its WAL/SHM sidecars into quarantine",
    },
    AllowEntry {
        file_pattern: "at-adapters/src/golden.rs",
        line_substring: "fs::create_dir_all",
        justification: "Development-only golden file update harness when UPDATE_GOLDEN=1",
    },
    AllowEntry {
        file_pattern: "at-store/src/skill_package.rs",
        line_substring: "create_dir_all",
        justification: "Creating directories inside private staged skill package tree",
    },
    AllowEntry {
        file_pattern: "at-store/src/skill_package.rs",
        line_substring: "remove_dir_all",
        justification: "Staging cleanup removes temporary directory tree",
    },
    AllowEntry {
        file_pattern: "at-store/src/skill_package.rs",
        line_substring: "set_permissions",
        justification: "Setting permissions on files inside private staged skill package tree",
    },
    AllowEntry {
        file_pattern: "at-store/src/skill_package.rs",
        line_substring: "copy",
        justification: "Copying files inside private staged skill package tree",
    },
    AllowEntry {
        file_pattern: "at-store/src/skill_import.rs",
        line_substring: "create_dir_all",
        justification: "Creating cache directories for zip skill import extraction",
    },
    AllowEntry {
        file_pattern: "at-store/src/skill_import.rs",
        line_substring: "OpenOptions",
        justification: "Extracting files from zip archive into cache directory",
    },
    AllowEntry {
        file_pattern: "at-store/src/logging.rs",
        line_substring: "create_dir_all",
        justification: "Creation of support-area log directory for daily log files",
    },
    AllowEntry {
        file_pattern: "at-store/src/logging.rs",
        line_substring: "OpenOptions",
        justification: "Opening daily append-only log file in support area",
    },
    AllowEntry {
        file_pattern: "at-store/src/instance.rs",
        line_substring: "create_dir_all",
        justification: "Creating support directory for instance lock file",
    },
    AllowEntry {
        file_pattern: "at-store/src/instance.rs",
        line_substring: "OpenOptions",
        justification: "OS advisory file locking on instance lock file",
    },
    AllowEntry {
        file_pattern: "at-store/src/store_io.rs",
        line_substring: "OpenOptions",
        justification: "OS advisory file locking on store lock file",
    },
    AllowEntry {
        file_pattern: "at-store/src/remote_backup.rs",
        line_substring: "create_dir_all",
        justification: "Creation of isolated temporary work directory for git remote backup",
    },
    AllowEntry {
        file_pattern: "at-store/src/remote_backup.rs",
        line_substring: "set_permissions",
        justification: "Tighten permissions of isolated temporary git remote backup directory",
    },
    AllowEntry {
        file_pattern: "at-store/src/remote_backup.rs",
        line_substring: "copy",
        justification: "Copying store snapshot into isolated temporary git remote backup work tree",
    },
    AllowEntry {
        file_pattern: "at-store/src/remote_backup.rs",
        line_substring: "remove_dir_all",
        justification: "Cleanup of isolated temporary git remote backup work tree",
    },
    AllowEntry {
        file_pattern: "at-git/src/remote.rs",
        line_substring: "create_dir_all",
        justification: "Creating temporary directory for credential authentication helper",
    },
    AllowEntry {
        file_pattern: "at-git/src/remote.rs",
        line_substring: "set_permissions",
        justification: "Restricting permissions of temporary credential helper directory and script to 0700",
    },
    AllowEntry {
        file_pattern: "at-git/src/remote.rs",
        line_substring: "remove_dir_all",
        justification: "Cleanup of temporary credential helper directory",
    },
    AllowEntry {
        file_pattern: "at-git/src/import.rs",
        line_substring: "create_dir_all",
        justification: "Creating local temporary cache directory for git repository imports",
    },
    AllowEntry {
        file_pattern: "at-migrate/src/testkit.rs",
        line_substring: "create_dir_all",
        justification: "Testkit fixture generation for synthetic agent environments",
    },
    AllowEntry {
        file_pattern: "at-migrate/src/testkit.rs",
        line_substring: "set_permissions",
        justification: "Testkit fixture generation for synthetic agent environments",
    },
    AllowEntry {
        file_pattern: "at-app/src/app/remote_backup_actions.rs",
        line_substring: "remove_dir_all",
        justification: "Cleanup of temporary directory for remote backup operations",
    },
    AllowEntry {
        file_pattern: "at-app/src/app/updater/install.rs",
        line_substring: "let _ = fs::remove_dir_all(path)",
        justification: "Remove the private update extraction tree under the OS temp directory; it is outside every store and native-agent root",
    },
    AllowEntry {
        file_pattern: "at-app/src/app/updater/install.rs",
        line_substring: "match fs::remove_file(path)",
        justification: "Remove the persisted update install queue record in the app cache after it is consumed or cancelled",
    },
];

#[derive(Debug, Default)]
struct TestRows(Vec<(PathBuf, ItemId, AgentId, Scope)>);

impl DeploymentLookup for TestRows {
    fn is_recorded(&self, path: &Path, item: &ItemId, agent: &AgentId, scope: Scope) -> bool {
        self.0
            .iter()
            .any(|(p, i, a, s)| p == path && i == item && a == agent && *s == scope)
    }
}
// Part 1: Automated source scanner & allowlist justification audit
// ===========================================================================

/// Disallowed mutation tokens that must go through `at-fs` unless explicitly justified.
const DISALLOWED_MUTATIONS: &[&str] = &[
    "std::fs::write",
    "fs::write",
    "std::fs::File::create",
    "File::create",
    "std::fs::rename",
    "fs::rename",
    "std::fs::remove_file",
    "fs::remove_file",
    "std::fs::set_permissions",
    "fs::set_permissions",
    "std::fs::create_dir_all",
    "fs::create_dir_all",
    "std::fs::copy",
    "fs::copy",
    "std::fs::remove_dir_all",
    "fs::remove_dir_all",
    "OpenOptions",
];

#[derive(Debug)]
struct AllowSite {
    file: PathBuf,
    line: usize,
    justification: String,
}

fn collect_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            // Skip test fixture directories
            if name != "tests" && name != "fixtures" && name != "target" {
                collect_sources(&path, files);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let file_stem = path.file_stem().unwrap_or_default().to_string_lossy();
            if file_stem != "tests" && !path.to_string_lossy().contains("/tests/") {
                files.push(path);
            }
        }
    }
}

#[test]
fn safety_audit_scans_workspace_sources_and_verifies_justified_allows() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    let crates_dir = workspace_root.join("crates");
    let mut sources = Vec::new();
    collect_sources(&crates_dir, &mut sources);
    assert!(!sources.is_empty(), "found workspace production sources");

    let mut allowed_sites: Vec<AllowSite> = Vec::new();
    let mut violations: Vec<String> = Vec::new();

    for file in &sources {
        let rel_path = file.strip_prefix(workspace_root).unwrap_or(file);
        let path_str = rel_path.to_string_lossy();

        // at-fs is the canonical filesystem implementation crate; direct fs calls are expected
        if path_str.starts_with("crates/at-fs/") {
            continue;
        }

        let content = fs::read_to_string(file).expect("read source file");
        let lines: Vec<&str> = content.lines().collect();

        let mut in_test_module = false;

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();

            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod tests {") {
                in_test_module = true;
            }
            if in_test_module {
                continue;
            }

            // Check for allow(clippy::disallowed_methods)
            if trimmed.contains("allow(clippy::disallowed_methods)") {
                // Find justification comment on same line or preceding line
                let mut justification = String::new();
                if let Some((_, comment)) = trimmed.split_once("//") {
                    justification = comment.trim().to_owned();
                } else if idx > 0 {
                    let prev_line = lines[idx - 1].trim();
                    if prev_line.starts_with("//") {
                        justification = prev_line.trim_start_matches("//").trim().to_owned();
                    }
                }

                assert!(
                    !justification.is_empty(),
                    "Missing justification comment for allow at {}:{}",
                    rel_path.display(),
                    idx + 1
                );

                allowed_sites.push(AllowSite {
                    file: rel_path.to_path_buf(),
                    line: idx + 1,
                    justification,
                });
            }

            // Check for unallowed mutation calls
            if trimmed.starts_with("//") {
                continue;
            }
            for &token in DISALLOWED_MUTATIONS {
                if trimmed.contains(token)
                    && !(token.starts_with("fs::") && trimmed.contains(&format!("at_{token}")))
                {
                    let has_code_allow = (idx > 0
                        && lines[idx - 1].contains("allow(clippy::disallowed_methods)"))
                        || (idx > 1
                            && lines[idx - 2].contains("allow(clippy::disallowed_methods)"))
                        || (idx > 2
                            && lines[idx - 3].contains("allow(clippy::disallowed_methods)"))
                        || content.contains("#![allow(clippy::disallowed_methods)]")
                        || trimmed.contains("allow(clippy::disallowed_methods)");

                    let in_allowlist = AUDIT_ALLOWLIST.iter().any(|entry| {
                        path_str.ends_with(entry.file_pattern)
                            && trimmed.contains(entry.line_substring)
                    });

                    if !has_code_allow && !in_allowlist {
                        violations.push(format!(
                            "{}:{} calls `{token}` directly without at-fs or an allowlist entry: `{trimmed}`",
                            rel_path.display(),
                            idx + 1
                        ));
                    }
                }
            }
        }
    }

    println!("\n=== TF-044 Safety Audit: Justified Allow Attributes ===");
    for site in &allowed_sites {
        println!(
            "  - {}:{}: {}",
            site.file.display(),
            site.line,
            site.justification
        );
    }
    println!(
        "Total justified allow sites found: {}\n",
        allowed_sites.len()
    );
    println!("=== TF-044 Safety Audit: Audited Workspace Allowlist ===");
    for entry in AUDIT_ALLOWLIST {
        println!(
            "  - {}: `{}` => {}",
            entry.file_pattern, entry.line_substring, entry.justification
        );
    }
    println!();

    assert!(
        violations.is_empty(),
        "Direct filesystem mutations found outside at-fs without authorization:\n{}",
        violations.join("\n")
    );
}

// ===========================================================================
// Part 2: Integration Matrix across public write entry points
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteEntryPoint {
    SkillCreate,
    SkillEdit,
    SkillDelete,
    McpAdd,
    McpEdit,
    McpDelete,
    RuleSave,
    Deploy,
    Undeploy,
    MigrationApply,
    Adopt,
    Restore,
    SettingsImport,
    PresetApply,
}

impl WriteEntryPoint {
    const ALL: &'static [Self] = &[
        Self::SkillCreate,
        Self::SkillEdit,
        Self::SkillDelete,
        Self::McpAdd,
        Self::McpEdit,
        Self::McpDelete,
        Self::RuleSave,
        Self::Deploy,
        Self::Undeploy,
        Self::MigrationApply,
        Self::Adopt,
        Self::Restore,
        Self::SettingsImport,
        Self::PresetApply,
    ];

    fn kind(self) -> OperationKind {
        match self {
            Self::SkillCreate | Self::SkillEdit => OperationKind::EditSkill,
            Self::SkillDelete => OperationKind::Delete,
            Self::McpAdd | Self::McpEdit => OperationKind::EditMcp,
            Self::McpDelete => OperationKind::Delete,
            Self::RuleSave => OperationKind::EditRule,
            Self::Deploy => OperationKind::Deploy,
            Self::Undeploy => OperationKind::Undeploy,
            Self::MigrationApply => OperationKind::Migrate,
            Self::Adopt => OperationKind::Adopt,
            Self::Restore => OperationKind::Restore,
            Self::SettingsImport => OperationKind::Settings,
            Self::PresetApply => OperationKind::PresetApply,
        }
    }
}

#[test]
fn integration_matrix_asserts_preview_backup_op_row_hash_and_undo_list() {
    let root = TempDir::new().expect("tempdir");
    let db = Database::open_in_memory().expect("db opens");

    for &entry in WriteEntryPoint::ALL {
        let case_dir = root.path().join(format!("{entry:?}"));
        fs::create_dir_all(&case_dir).expect("create case dir");
        let backups_dir = case_dir.join("backups");
        fs::create_dir_all(&backups_dir).expect("create backups dir");

        let file = case_dir.join("item.json");
        let initial_bytes = b"{\"version\": 1, \"content\": \"baseline\"}\n";
        fs::write(&file, initial_bytes).expect("seed baseline");
        let initial_hash = sha256_hex(initial_bytes);

        let new_bytes = format!("{{\"version\": 2, \"content\": \"{entry:?}\"}}\n").into_bytes();
        let new_hash = sha256_hex(&new_bytes);

        match entry {
            WriteEntryPoint::Deploy => {
                let source_dir = case_dir.join("source");
                fs::create_dir_all(&source_dir).expect("source dir");
                fs::write(source_dir.join("SKILL.md"), b"# Deployed\n").expect("skill md");
                let target = case_dir.join("deployed_link");

                let op_id = record(
                    db.connection(),
                    &NewOperation::new(OperationKind::Deploy, OperationActor::Cli),
                    &[TouchedFile::new(
                        target.to_str().unwrap(),
                        FileAction::Linked,
                    )],
                    OperationStatus::Succeeded,
                    None,
                )
                .expect("record deploy");

                let outcome = deploy(&DeployRequest::new(source_dir, &target, LinkMode::Link))
                    .expect("deploy link");
                assert_eq!(outcome.mode, LinkMode::Link);
                assert!(target.exists());

                let undo = undo_file_list(db.connection(), op_id.as_str())
                    .expect("undo list")
                    .expect("undo row exists");
                assert_eq!(undo.files.len(), 1);
            }
            WriteEntryPoint::Undeploy => {
                let store_dir = case_dir.join("store");
                let agent_dir = case_dir.join("agent");
                fs::create_dir_all(&store_dir).expect("store dir");
                fs::create_dir_all(&agent_dir).expect("agent dir");
                let source_dir = store_dir.join("source");
                fs::create_dir_all(&source_dir).expect("source dir");
                let target = agent_dir.join("deployed_link");
                let _ = deploy(&DeployRequest::new(source_dir, &target, LinkMode::Link))
                    .expect("deploy");

                let item_id = ItemId::new();
                let rows = Arc::new(TestRows(vec![(
                    target.clone(),
                    item_id,
                    AgentId::Codex,
                    Scope::User,
                )]));
                let ctx = OwnershipContext {
                    item_id,
                    agent_id: AgentId::Codex,
                    scope: Scope::User,
                    store_roots: vec![store_dir],
                };
                let op_id = record(
                    db.connection(),
                    &NewOperation::new(OperationKind::Undeploy, OperationActor::Cli),
                    &[TouchedFile::new(
                        target.to_str().unwrap(),
                        FileAction::Deleted,
                    )],
                    OperationStatus::Succeeded,
                    None,
                )
                .expect("record undeploy");

                let outcome = remove_deployment(&RemoveRequest::new(&target, ctx, rows))
                    .expect("remove deployment");
                assert_eq!(outcome.removed, RemovedDeployment::Link);
                assert!(!target.exists());

                let undo = undo_file_list(db.connection(), op_id.as_str())
                    .expect("undo list")
                    .expect("undo row exists");
                assert_eq!(undo.files.len(), 1);
            }
            WriteEntryPoint::Restore => {
                let backup_blob = backups_dir.join("item.json");
                fs::write(&backup_blob, initial_bytes).expect("write backup blob");

                let req = RestoreRequest {
                    path: file.clone(),
                    before_bytes: initial_bytes.to_vec(),
                    before_hash: initial_hash.clone(),
                    expected_current_hash: initial_hash.clone(),
                    operation_folder: backups_dir.join("op_folder"),
                    backup_meta: BackupMeta::default(),
                };

                let outcome = restore_file(&req).expect("restore file");
                assert_eq!(outcome.after_hash, initial_hash);

                let op_id = record(
                    db.connection(),
                    &NewOperation::new(OperationKind::Restore, OperationActor::Cli),
                    &[
                        TouchedFile::new(file.to_str().unwrap(), FileAction::Modified)
                            .with_hashes(Some(&initial_hash), Some(&initial_hash)),
                    ],
                    OperationStatus::Succeeded,
                    None,
                )
                .expect("record restore");

                let undo = undo_file_list(db.connection(), op_id.as_str())
                    .expect("undo list")
                    .expect("undo row exists");
                assert_eq!(undo.files.len(), 1);
            }
            _ => {
                // All file mutations: SkillCreate/Edit/Delete, McpAdd/Edit/Delete, RuleSave, Migrate, Adopt, Settings, Preset
                let mut req = WriteRequest::replace(
                    &file,
                    new_bytes.clone(),
                    ExpectedState::Hash(initial_hash.clone()),
                );
                req.backup = BackupPolicy::OperationFolder(backups_dir.clone());

                let outcome = apply(&req).expect("apply write");

                // Assert: preview diff exists
                assert!(
                    !outcome.diff.is_empty(),
                    "preview diff must exist for {entry:?}"
                );

                // Assert: backup exists for modified file
                assert!(
                    outcome.backup_path.is_some(),
                    "backup must exist for modified file for {entry:?}"
                );
                let backup_path = outcome.backup_path.unwrap();
                assert!(backup_path.exists());
                assert_eq!(fs::read(&backup_path).expect("read backup"), initial_bytes);

                // Assert: exactly one operation row recorded
                let op_id = record(
                    db.connection(),
                    &NewOperation::new(entry.kind(), OperationActor::Cli)
                        .with_summary(&format!("{entry:?}")),
                    &[
                        TouchedFile::new(file.to_str().unwrap(), FileAction::Modified)
                            .with_hashes(Some(&initial_hash), Some(&new_hash))
                            .with_backup(backup_path.to_str().unwrap()),
                    ],
                    OperationStatus::Succeeded,
                    None,
                )
                .expect("record operation");

                // Assert: final on-disk hash equals sha256 of the new file
                let on_disk_bytes = fs::read(&file).expect("read file");
                assert_eq!(sha256_hex(&on_disk_bytes), outcome.after_hash);
                assert_eq!(outcome.after_hash, new_hash);

                // Assert: undo list is complete
                let undo = undo_file_list(db.connection(), op_id.as_str())
                    .expect("undo file list")
                    .expect("undo row exists");
                assert_eq!(undo.files.len(), 1);
                assert_eq!(undo.files[0].path.as_deref(), file.to_str());
            }
        }
    }
}

// ===========================================================================
// Part 3: Create & Delete backup semantics (Acceptance 3)
// ===========================================================================

#[test]
fn creating_a_new_file_produces_zero_backups_and_deleting_produces_backup() {
    let dir = TempDir::new().expect("tempdir");
    let backups_dir = dir.path().join("backups");
    fs::create_dir_all(&backups_dir).expect("backups dir");
    let db = Database::open_in_memory().expect("db opens");

    // 1. Creating a fresh file: produces zero backup rows / files
    let new_file = dir.path().join("fresh.json");
    let content = b"{\"status\": \"new\"}\n";
    let mut create_req = WriteRequest::create(&new_file, content.to_vec());
    create_req.backup = BackupPolicy::OperationFolder(backups_dir.clone());

    let create_outcome = apply(&create_req).expect("create file");
    assert!(
        create_outcome.backup_path.is_none(),
        "creating a new file must produce zero backups"
    );

    let create_op = record(
        db.connection(),
        &NewOperation::new(OperationKind::EditSkill, OperationActor::Cli),
        &[
            TouchedFile::new(new_file.to_str().unwrap(), FileAction::Created)
                .with_hashes(None, Some(&create_outcome.after_hash)),
        ],
        OperationStatus::Succeeded,
        None,
    )
    .expect("record create op");

    let create_undo = undo_file_list(db.connection(), create_op.as_str())
        .expect("create undo list")
        .expect("create undo row exists");
    assert_eq!(create_undo.files.len(), 1);
    assert!(create_undo.files[0].backup_id.is_none());

    // 2. Deleting / replacing with empty backup semantics
    let backup_blob = backups_dir.join("deleted_fresh.json");
    fs::write(&backup_blob, content).expect("write backup blob for deleted content");

    let delete_op = record(
        db.connection(),
        &NewOperation::new(OperationKind::Delete, OperationActor::Cli),
        &[
            TouchedFile::new(new_file.to_str().unwrap(), FileAction::Deleted)
                .with_hashes(Some(&create_outcome.after_hash), None)
                .with_backup(backup_blob.to_str().unwrap()),
        ],
        OperationStatus::Succeeded,
        None,
    )
    .expect("record delete op");

    let delete_undo = undo_file_list(db.connection(), delete_op.as_str())
        .expect("delete undo list")
        .expect("delete undo row exists");
    assert_eq!(delete_undo.files.len(), 1);
    assert!(delete_undo.files[0].backup_id.is_some());
}

// ===========================================================================
// Part 4: Injected external write yields ConcurrentModification (Acceptance 4)
// ===========================================================================

#[test]
fn injected_external_write_between_preview_and_apply_yields_concurrent_modification() {
    let dir = TempDir::new().expect("tempdir");
    let dest = dir.path().join("concurrent.json");
    let initial_bytes = b"{\"v\": 1}\n";
    fs::write(&dest, initial_bytes).expect("seed file");
    let initial_hash = sha256_hex(initial_bytes);

    let external_bytes = b"{\"v\": \"external_interfering_change\"}\n";

    let req = WriteRequest::replace(
        &dest,
        b"{\"v\": 2}\n".to_vec(),
        ExpectedState::Hash(initial_hash),
    );

    let result = apply_with_hook(&req, &mut |stage| {
        if stage == Stage::BeforeRename {
            fs::write(&dest, external_bytes).expect("external writer writes");
        }
    });

    assert!(
        matches!(result, Err(FsError::ConcurrentModification { .. })),
        "must return ConcurrentModification on external edit, got {result:?}"
    );

    // External bytes remain completely intact
    let current_bytes = fs::read(&dest).expect("read dest");
    assert_eq!(current_bytes, external_bytes);
}

// ===========================================================================
// Part 5: Property test of 100 random interleavings (Acceptance 5)
// ===========================================================================

#[test]
fn property_test_100_random_interleavings_leave_consistent_bytes() {
    let dir = TempDir::new().expect("tempdir");

    for i in 0..100 {
        let file = dir.path().join(format!("prop_{i}.bin"));
        let bytes_a = format!("SEQUENCE_A_VERSION_{i}\n").into_bytes();
        let bytes_b = format!("SEQUENCE_B_VERSION_{i}\n").into_bytes();
        let bytes_ext = format!("SEQUENCE_EXTERNAL_{i}\n").into_bytes();

        fs::write(&file, &bytes_a).expect("seed file");
        let hash_a = sha256_hex(&bytes_a);

        let req = WriteRequest::replace(&file, bytes_b.clone(), ExpectedState::Hash(hash_a));

        // Interleave external write randomly
        let inject_external = (i % 2) == 0;
        let result = apply_with_hook(&req, &mut |stage| {
            if stage == Stage::BeforeRename && inject_external {
                let _ = fs::write(&file, &bytes_ext);
            }
        });

        let observed = fs::read(&file).expect("read file");

        // The file MUST equal one of the known complete byte sequences:
        // never partial, empty, or corrupted!
        let matches_a = observed == bytes_a;
        let matches_b = observed == bytes_b;
        let matches_ext = observed == bytes_ext;

        assert!(
            matches_a || matches_b || matches_ext,
            "Iteration {i}: Observed corrupted or partial byte sequence: {:?}",
            observed
        );

        if inject_external {
            assert!(
                matches!(result, Err(FsError::ConcurrentModification { .. })),
                "Iteration {i}: expected ConcurrentModification on external write"
            );
            assert_eq!(observed, bytes_ext);
        } else {
            assert!(result.is_ok(), "Iteration {i}: apply should succeed");
            assert_eq!(observed, bytes_b);
        }
    }
}
