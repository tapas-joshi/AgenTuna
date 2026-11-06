//! Safe rule editing and create-only rule-file intents.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_model::{AgentId, ItemId, ItemKind, Scope};

use crate::rules::{MANAGED_BEGIN, MANAGED_END, RuleSource};

/// Native rule-file names supported by the create intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleFileType {
    /// Claude Code's `CLAUDE.md`.
    Claude,
    /// Codex/omp's `AGENTS.md`.
    Agents,
    /// Claude Code's project-local `CLAUDE.local.md`.
    ClaudeLocal,
}

impl RuleFileType {
    fn file_name(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE.md",
            Self::Agents => "AGENTS.md",
            Self::ClaudeLocal => "CLAUDE.local.md",
        }
    }
}

/// A reviewed create-only intent for a native rule file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRuleIntent {
    /// Resolved absolute target path.
    pub path: PathBuf,
    /// Exact template bytes: one H1 and its final newline.
    pub template: Vec<u8>,
    /// One create-only operation. It must be applied under authorized roots.
    pub change_set: at_model::ChangeSet,
    /// Local rules are project-local and must never be staged for git.
    pub local_only: bool,
    scope: Scope,
}

/// Rule save and creation failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RuleEditorError {
    /// The editor changed bytes outside the managed body.
    #[error("hand edit inside AgenTuna markers at line {line}")]
    HandEditInsideMarkers {
        /// First line whose bytes differ from the original managed-region boundaries.
        line: usize,
    },
    /// A filesystem permission check failed.
    #[error("permission denied: {path} (OS error {os_error:?})")]
    PermissionDenied {
        /// Path whose permissions prevented writing.
        path: PathBuf,
        /// Platform error code, when provided by the operating system.
        os_error: Option<i32>,
    },
    /// A filesystem read failed before the write could begin.
    #[error("cannot access rule file {path}: {message}")]
    Io {
        /// File that could not be accessed.
        path: PathBuf,
        /// Human-readable filesystem error.
        message: String,
    },
    /// A managed block could not be parsed or rendered.
    #[error(transparent)]
    ManagedBlock(#[from] at_fs::managed_block::ManagedBlockError),
    /// The save-on-open version no longer matches disk.
    #[error("rule file changed since it was opened: {path}; {diff}")]
    Conflict {
        /// File whose contents changed.
        path: PathBuf,
        /// Diff between the disk and editor buffers.
        diff: String,
    },
    /// This Global target is not writable.
    #[error("Global rule path is read-only: {path}")]
    ReadOnly {
        /// Read-only target path.
        path: PathBuf,
    },
    /// The target already exists; creation never overwrites.
    #[error("rule file already exists: {path}")]
    AlreadyExists {
        /// Existing path that was not overwritten.
        path: PathBuf,
    },
    /// No native path matching the requested file type exists for this adapter/scope.
    #[error("no {file_type} path for {agent} at {scope:?}")]
    NoPath {
        /// Native rule filename kind requested.
        file_type: &'static str,
        /// Agent whose native path was requested.
        agent: AgentId,
        /// Scope in which the path was requested.
        scope: Scope,
    },
    /// Adapter path resolution failed.
    #[error("cannot resolve rule path: {0}")]
    Adapter(String),
    /// A safe filesystem/operation-history write failed.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// Version history could not be recorded.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// A file size could not be represented in SQLite.
    #[error("rule file is too large to record in version history")]
    VersionSize,
    /// ChangeSet construction failed.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// The selected scope has no project identity/path.
    #[error("{scope:?} scope requires a project path")]
    MissingProject {
        /// Scope that requires a project path.
        scope: Scope,
    },
}

/// Outcome of saving the editor's full-file buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSaveOutcome {
    /// Whether a write was applied.
    pub changed: bool,
    /// Apply outcome when a write was necessary.
    pub apply: Option<at_fs::ApplyOutcome>,
}

/// Saves editor content by replacing only the managed block.
///
/// `source` must be the exact read-on-open snapshot. The prepared ChangeSet carries its hash and
/// modification time, and the atomic write pipeline rechecks both before installation.
pub fn save_rule(
    source: &RuleSource,
    editor_buffer: &[u8],
    item_id: &ItemId,
    connection: &rusqlite::Connection,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
    recent_writes: &at_fs::RecentWrites,
) -> Result<RuleSaveOutcome, RuleEditorError> {
    if editor_buffer == source.bytes {
        return Ok(RuleSaveOutcome {
            changed: false,
            apply: None,
        });
    }

    let content = managed_content(&source.path, &source.bytes, editor_buffer)?;
    let rendered =
        at_fs::managed_block::render_managed_block(&source.path, Some(&source.bytes), content)?;
    if rendered == source.bytes {
        return Ok(RuleSaveOutcome {
            changed: false,
            apply: None,
        });
    }

    let current = std::fs::read(&source.path).map_err(|error| io_error(&source.path, error))?;
    let metadata =
        std::fs::metadata(&source.path).map_err(|error| io_error(&source.path, error))?;
    let mtime = metadata.modified().ok();
    let current_hash = at_fs::sha256_hex(&current);
    if current_hash != source.sha256 || mtime != source.mtime {
        return Err(RuleEditorError::Conflict {
            path: source.path.clone(),
            diff: similar::TextDiff::from_lines(
                String::from_utf8_lossy(&current),
                String::from_utf8_lossy(editor_buffer),
            )
            .unified_diff()
            .to_string(),
        });
    }
    if metadata.permissions().mode() & 0o222 == 0 {
        return Err(RuleEditorError::PermissionDenied {
            path: source.path.clone(),
            os_error: std::io::Error::from(std::io::ErrorKind::PermissionDenied).raw_os_error(),
        });
    }

    let expected = match source.mtime {
        Some(mtime) => at_model::ExpectedState::hash_and_mtime(&source.sha256, mtime)?,
        None => at_model::ExpectedState::hash(&source.sha256)?,
    };
    let change_set = rule_change_set(
        "edit_rule",
        "Edit managed rule content",
        source.scope,
        &source.path,
        rendered.clone(),
        expected,
        true,
    )?;
    let apply = at_fs::apply_with_roots(&change_set, context, roots)
        .map_err(|error| apply_error(error, &source.path, editor_buffer))?;
    let saved_hash = at_fs::sha256_hex(&rendered);
    recent_writes.record(&source.path, saved_hash.clone());
    record_version(
        item_id,
        source.scope,
        &source.path,
        &saved_hash,
        rendered.len(),
        &apply.operation_id,
        connection,
    )?;
    Ok(RuleSaveOutcome {
        changed: true,
        apply: Some(apply),
    })
}

/// Resolves a new rule intent using the standard adapter registry and Claude Code's native path.
///
/// # Errors
/// Returns the same errors as [`new_rule`] when the path is unsupported, occupied or read-only.
pub fn new_registered_rule(
    file_type: RuleFileType,
    scope: Scope,
    home: &Path,
    project: Option<&Path>,
    global_writable: bool,
) -> Result<NewRuleIntent, RuleEditorError> {
    let registry = AgentAdapterRegistry::register();
    let env = AgentEnvironment {
        home: home.to_path_buf(),
        project: project.map(Path::to_path_buf),
        variables: Default::default(),
    };
    new_rule(
        file_type,
        scope,
        &AgentId::ClaudeCode,
        &registry,
        &env,
        global_writable,
    )
}

/// Resolves and plans a create-only native rule file using the selected adapter's verified path.
///
/// # Errors
/// Returns an error when the path is unsupported, occupied or read-only.
pub fn new_rule(
    file_type: RuleFileType,
    scope: Scope,
    agent: &AgentId,
    registry: &AgentAdapterRegistry,
    env: &AgentEnvironment,
    global_writable: bool,
) -> Result<NewRuleIntent, RuleEditorError> {
    if matches!(scope, Scope::Project | Scope::Local) && env.project.is_none() {
        return Err(RuleEditorError::MissingProject { scope });
    }
    let adapter = registry
        .get(agent)
        .map_err(|error| RuleEditorError::Adapter(error.to_string()))?
        .ok_or_else(|| RuleEditorError::NoPath {
            file_type: file_type.file_name(),
            agent: agent.clone(),
            scope,
        })?;
    let path = adapter
        .paths(ItemKind::Rule, scope, env)
        .into_iter()
        .find(|candidate| {
            candidate.kind == "rules_file"
                && candidate.path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .eq_ignore_ascii_case(file_type.file_name())
                })
        })
        .map(|candidate| candidate.path)
        .ok_or_else(|| RuleEditorError::NoPath {
            file_type: file_type.file_name(),
            agent: agent.clone(),
            scope,
        })?;
    if scope == Scope::Global && !global_writable {
        return Err(RuleEditorError::ReadOnly { path });
    }
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(RuleEditorError::AlreadyExists { path });
    }
    let template = format!("# {}\n", file_type.file_name()).into_bytes();
    let change_set = rule_change_set(
        "edit_rule",
        "Create native rule file",
        scope,
        &path,
        template.clone(),
        at_model::ExpectedState::absent(),
        false,
    )?;
    Ok(NewRuleIntent {
        path,
        template,
        change_set,
        local_only: file_type == RuleFileType::ClaudeLocal || scope == Scope::Local,
        scope,
    })
}

/// Applies a new-rule intent and returns its source immediately, without rescanning adapters.
pub fn apply_new_rule(
    intent: &NewRuleIntent,
    item_id: &ItemId,
    connection: &rusqlite::Connection,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
    recent_writes: &at_fs::RecentWrites,
) -> Result<(RuleSource, at_fs::ApplyOutcome), RuleEditorError> {
    let outcome =
        at_fs::apply_with_roots(&intent.change_set, context, roots).map_err(
            |error| match error {
                at_fs::ChangeSetApplyError::Fs(at_fs::FsError::AlreadyExists { path }) => {
                    RuleEditorError::AlreadyExists { path }
                }
                other => RuleEditorError::Apply(other),
            },
        )?;
    let hash = at_fs::sha256_hex(&intent.template);
    recent_writes.record(&intent.path, hash.clone());
    record_version(
        item_id,
        intent.scope,
        &intent.path,
        &hash,
        intent.template.len(),
        &outcome.operation_id,
        connection,
    )?;
    let mtime = std::fs::metadata(&intent.path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    Ok((
        RuleSource {
            scope: intent.scope,
            agent_consumers: Vec::new(),
            path: intent.path.clone(),
            bytes: intent.template.clone(),
            mtime,
            sha256: hash,
            managed: false,
            link_target: None,
        },
        outcome,
    ))
}

fn rule_change_set(
    operation: &str,
    summary: &str,
    scope: Scope,
    path: &Path,
    bytes: Vec<u8>,
    expected: at_model::ExpectedState,
    backup: bool,
) -> Result<at_model::ChangeSet, RuleEditorError> {
    let id = uuid::Uuid::now_v7().to_string();
    let change_set = at_model::ChangeSet {
        id: id.clone(),
        preview_id: id,
        operation_kind: at_model::OperationKind::parse(operation)?,
        scope: Some(scope),
        project_id: None,
        summary: summary.to_owned(),
        ops: vec![at_model::FileOp::Write {
            path: path.to_path_buf(),
            bytes,
            expected,
            follow_symlink: false,
            backup,
        }],
        created_at_unix_seconds: 0,
    };
    change_set.validate()?;
    Ok(change_set)
}
fn io_error(path: &Path, error: std::io::Error) -> RuleEditorError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        RuleEditorError::PermissionDenied {
            path: path.to_path_buf(),
            os_error: error.raw_os_error(),
        }
    } else {
        RuleEditorError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    }
}
fn apply_error(error: at_fs::ChangeSetApplyError, path: &Path, editor: &[u8]) -> RuleEditorError {
    match error {
        at_fs::ChangeSetApplyError::Fs(at_fs::FsError::PermissionDenied { path }) => {
            RuleEditorError::PermissionDenied {
                path,
                os_error: std::io::Error::from(std::io::ErrorKind::PermissionDenied).raw_os_error(),
            }
        }
        at_fs::ChangeSetApplyError::Stale(stale_path) if stale_path.as_path() == path => {
            conflict_from_disk(path, editor)
        }
        at_fs::ChangeSetApplyError::Fs(at_fs::FsError::ConcurrentModification {
            path: changed_path,
            ..
        }) if changed_path.as_path() == path => conflict_from_disk(path, editor),
        other => RuleEditorError::Apply(other),
    }
}

fn conflict_from_disk(path: &Path, editor: &[u8]) -> RuleEditorError {
    let disk = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return io_error(path, error),
    };
    RuleEditorError::Conflict {
        path: path.to_path_buf(),
        diff: similar::TextDiff::from_lines(
            String::from_utf8_lossy(&disk),
            String::from_utf8_lossy(editor),
        )
        .unified_diff()
        .to_string(),
    }
}
fn record_version(
    item_id: &ItemId,
    scope: Scope,
    path: &Path,
    hash: &str,
    bytes: usize,
    operation_id: &str,
    connection: &rusqlite::Connection,
) -> Result<(), RuleEditorError> {
    let size = i64::try_from(bytes).map_err(|_| RuleEditorError::VersionSize)?;
    let path_text = path.to_string_lossy();
    let mut version = at_db::versions::NewVersion::new(item_id, scope, hash)
        .with_path(&path_text)
        .with_size_mtime(size, 0)
        .with_operation(operation_id);
    version.mtime = std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok());
    at_db::versions::record_skill_version(connection, &version)?;
    Ok(())
}

fn managed_content<'a>(
    path: &Path,
    original: &'a [u8],
    editor: &'a [u8],
) -> Result<&'a [u8], RuleEditorError> {
    let (original_prefix, original_body, original_suffix) = split_managed(path, original)?;
    let (editor_prefix, editor_body, editor_suffix) = split_managed(path, editor)?;
    if original_prefix != editor_prefix {
        return Err(RuleEditorError::HandEditInsideMarkers {
            line: differing_line(original_prefix, editor_prefix),
        });
    }
    if original_suffix != editor_suffix {
        return Err(RuleEditorError::HandEditInsideMarkers {
            line: original_prefix
                .iter()
                .filter(|byte| **byte == b'\n')
                .count()
                + 1,
        });
    }
    let _ = original_body;
    let content_start = if editor_body.starts_with(b"\r\n") {
        2
    } else if editor_body.starts_with(b"\n") {
        1
    } else {
        0
    };
    let content_end = if editor_body.ends_with(b"\r\n") {
        editor_body.len() - 2
    } else if editor_body.ends_with(b"\n") {
        editor_body.len() - 1
    } else {
        editor_body.len()
    };
    Ok(&editor_body[content_start..content_end])
}

type ManagedParts<'a> = (&'a [u8], &'a [u8], &'a [u8]);

fn split_managed<'a>(path: &Path, bytes: &'a [u8]) -> Result<ManagedParts<'a>, RuleEditorError> {
    let begin = bytes
        .windows(MANAGED_BEGIN.len())
        .position(|window| window == MANAGED_BEGIN)
        .ok_or_else(|| at_fs::managed_block::ManagedBlockError::AdoptRequired {
            path: path.to_path_buf(),
        })?;
    let body_start = begin + MANAGED_BEGIN.len();
    let end_rel = bytes[body_start..]
        .windows(MANAGED_END.len())
        .position(|window| window == MANAGED_END)
        .ok_or_else(
            || at_fs::managed_block::ManagedBlockError::MalformedManagedBlock {
                path: path.to_path_buf(),
                line: bytes[..begin].iter().filter(|byte| **byte == b'\n').count() + 1,
            },
        )?;
    let end = body_start + end_rel;
    Ok((&bytes[..body_start], &bytes[body_start..end], &bytes[end..]))
}

fn differing_line(a: &[u8], b: &[u8]) -> usize {
    let offset = a
        .iter()
        .zip(b)
        .position(|(left, right)| left != right)
        .unwrap_or(a.len().min(b.len()));
    a[..offset].iter().filter(|byte| **byte == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_buffer_must_leave_marker_regions_untouched() {
        let path = Path::new("CLAUDE.md");
        let original = b"hand\n<!-- agentuna:begin -->\nold\n<!-- agentuna:end -->\ntail\n";
        let edited = b"hand\n<!-- agentuna:begin -->\nnew\n<!-- agentuna:end -->\ntail\n";
        assert_eq!(
            managed_content(path, original, edited).expect("managed body"),
            b"new"
        );
        let marker_edit = b"hand\n<!-- agentuna:begin -->\nnew\n<!-- agentuna:end -->\nchanged\n";
        assert!(matches!(
            managed_content(path, original, marker_edit),
            Err(RuleEditorError::HandEditInsideMarkers { .. })
        ));
    }

    #[test]
    fn new_rule_template_has_one_h1_and_final_newline() {
        assert_eq!(RuleFileType::ClaudeLocal.file_name(), "CLAUDE.local.md");
        assert_eq!(
            format!("# {}\n", RuleFileType::ClaudeLocal.file_name()).as_bytes(),
            b"# CLAUDE.local.md\n"
        );
    }

    #[test]
    fn new_local_rule_uses_project_local_target_and_create_only_change() {
        let temporary = tempfile::tempdir().expect("temporary project");
        let project = temporary.path().join("project");
        std::fs::create_dir_all(&project).expect("project directory");
        let env = AgentEnvironment {
            home: temporary.path().join("home"),
            project: Some(project.clone()),
            variables: Default::default(),
        };
        let registry = AgentAdapterRegistry::register();
        let intent = new_rule(
            RuleFileType::ClaudeLocal,
            Scope::Project,
            &AgentId::ClaudeCode,
            &registry,
            &env,
            true,
        )
        .expect("new rule intent");
        assert_eq!(intent.path, project.join("CLAUDE.local.md"));
        assert!(intent.local_only);
        assert_eq!(intent.template, b"# CLAUDE.local.md\n");
        assert!(matches!(
            intent.change_set.ops.as_slice(),
            [at_model::FileOp::Write {
                expected: at_model::ExpectedState::Absent,
                backup: false,
                ..
            }]
        ));
    }

    #[test]
    fn applied_new_rule_has_one_trailing_newline_and_appears_in_next_scan() {
        let temporary = tempfile::tempdir().expect("temporary project");
        let project = temporary.path().join("project");
        let store = temporary.path().join("store");
        let backups = temporary.path().join("backups");
        std::fs::create_dir_all(&project).expect("project directory");
        std::fs::create_dir_all(&store).expect("store");
        let env = AgentEnvironment {
            home: temporary.path().join("home"),
            project: Some(project.clone()),
            variables: Default::default(),
        };
        let registry = AgentAdapterRegistry::register();
        let intent = new_rule(
            RuleFileType::ClaudeLocal,
            Scope::Project,
            &AgentId::ClaudeCode,
            &registry,
            &env,
            true,
        )
        .expect("new rule intent");
        let migrated = at_db::Database::open_in_memory().expect("migrated database");
        let database = migrated.connection();
        let item = ItemId::new();
        database
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name) \
                 VALUES (?1, 'rule', 'project', 'claude-local', 'CLAUDE.local.md')",
                [item.to_string()],
            )
            .expect("rule item row");
        let mut recorder = CountingRecorder::default();
        let mut context = at_fs::ApplyContext {
            store_root: &store,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let roots = at_fs::ApplyRoots::new([temporary.path().to_path_buf()]).expect("roots");

        let (created, _) = apply_new_rule(
            &intent,
            &item,
            database,
            &mut context,
            &roots,
            &at_fs::RecentWrites::new(),
        )
        .expect("apply rule");
        assert_eq!(
            std::fs::read(&created.path).expect("read new rule"),
            b"# CLAUDE.local.md\n"
        );
        let scan = crate::rules::scan(&registry, &env, &[AgentId::ClaudeCode], &[Scope::Project]);
        assert!(
            scan.sources
                .iter()
                .any(|source| source.path == created.path)
        );
    }

    #[test]
    fn global_rule_creation_refuses_read_only_store_and_existing_files() {
        let temporary = tempfile::tempdir().expect("temporary home");
        let env = AgentEnvironment {
            home: temporary.path().to_path_buf(),
            project: None,
            variables: Default::default(),
        };
        let registry = AgentAdapterRegistry::register();
        let read_only = new_rule(
            RuleFileType::Claude,
            Scope::Global,
            &AgentId::ClaudeCode,
            &registry,
            &env,
            false,
        );
        assert!(matches!(read_only, Err(RuleEditorError::ReadOnly { .. })));

        let path = temporary.path().join(".claude/CLAUDE.md");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("Claude directory");
        std::fs::write(&path, b"hand authored").expect("existing rule");
        let existing = new_rule(
            RuleFileType::Claude,
            Scope::Global,
            &AgentId::ClaudeCode,
            &registry,
            &env,
            true,
        );
        assert!(matches!(
            existing,
            Err(RuleEditorError::AlreadyExists { path: found }) if found == path
        ));
    }

    #[derive(Default)]
    struct CountingRecorder(usize);

    impl at_fs::OperationRecorder for CountingRecorder {
        fn begin(&mut self, _: &at_model::ChangeSet) -> Result<String, String> {
            self.0 += 1;
            Ok("test-rule-op".to_owned())
        }

        fn file_completed(
            &mut self,
            _: &str,
            _: &Path,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&Path>,
        ) -> Result<(), String> {
            Ok(())
        }

        fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn unchanged_editor_buffer_does_not_start_an_operation_or_backup() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let store = temporary.path().join("store");
        std::fs::create_dir_all(&store).expect("store");
        let backup = temporary.path().join("backups");
        let mut recorder = CountingRecorder::default();
        let mut context = at_fs::ApplyContext {
            store_root: &store,
            backup_folder: &backup,
            recorder: &mut recorder,
        };
        let roots = at_fs::ApplyRoots::new([temporary.path().to_path_buf()]).expect("roots");
        let source = RuleSource {
            scope: Scope::Project,
            agent_consumers: Vec::new(),
            path: temporary.path().join("CLAUDE.md"),
            bytes: b"unchanged".to_vec(),
            mtime: None,
            sha256: at_fs::sha256_hex(b"unchanged"),
            managed: true,
            link_target: None,
        };
        let db = rusqlite::Connection::open_in_memory().expect("database");
        let result = save_rule(
            &source,
            &source.bytes,
            &ItemId::new(),
            &db,
            &mut context,
            &roots,
            &at_fs::RecentWrites::new(),
        )
        .expect("unchanged save");
        assert!(!result.changed);
        assert!(result.apply.is_none());
        assert_eq!(recorder.0, 0);
    }

    #[test]
    fn external_edit_after_open_returns_diff_conflict_without_overwriting_disk() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let store = temporary.path().join("store");
        std::fs::create_dir_all(&store).expect("store");
        let backup = temporary.path().join("backups");
        let path = temporary.path().join("CLAUDE.md");
        let original = b"hand\n<!-- agentuna:begin -->\nold\n<!-- agentuna:end -->\ntail\n";
        std::fs::write(&path, original).expect("rule file");
        let metadata = std::fs::metadata(&path).expect("metadata");
        let source = RuleSource {
            scope: Scope::Project,
            agent_consumers: Vec::new(),
            path: path.clone(),
            bytes: original.to_vec(),
            mtime: metadata.modified().ok(),
            sha256: at_fs::sha256_hex(original),
            managed: true,
            link_target: None,
        };
        let external = b"hand\n<!-- agentuna:begin -->\nexternal\n<!-- agentuna:end -->\ntail\n";
        std::fs::write(&path, external).expect("external edit");
        let mut recorder = CountingRecorder::default();
        let mut context = at_fs::ApplyContext {
            store_root: &store,
            backup_folder: &backup,
            recorder: &mut recorder,
        };
        let roots = at_fs::ApplyRoots::new([temporary.path().to_path_buf()]).expect("roots");
        let db = rusqlite::Connection::open_in_memory().expect("database");
        let editor = b"hand\n<!-- agentuna:begin -->\nedited\n<!-- agentuna:end -->\ntail\n";
        let result = save_rule(
            &source,
            editor,
            &ItemId::new(),
            &db,
            &mut context,
            &roots,
            &at_fs::RecentWrites::new(),
        );
        assert!(
            matches!(result, Err(RuleEditorError::Conflict { diff, .. }) if diff.contains("external"))
        );
        assert_eq!(
            std::fs::read(path).expect("disk remains external"),
            external
        );
        assert_eq!(recorder.0, 0);
    }
}
