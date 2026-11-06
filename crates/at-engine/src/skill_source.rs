//! Explicit source checks and persisted update state for imported skills.

use std::path::Path;
use std::str::FromStr;

use at_model::{
    ItemId,
    network::{NetworkEventLog, NetworkPolicy},
};
use at_store::hash_skill_folder;
use rusqlite::Connection;
use time::OffsetDateTime;

/// Persisted or non-persisted outcome of an explicit skill source check.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkillSourceCheck {
    /// Remote head differs from installed commit; update badge is available.
    UpdateAvailable {
        /// Remote commit head resolved for this source.
        remote_sha: String,
        /// Whether the local package differs from the imported content hash.
        local_modified: bool,
    },
    /// Remote head matches installed commit; badge is cleared.
    Current {
        /// Remote commit head resolved for this source.
        remote_sha: String,
        /// Whether the local package differs from the imported content hash.
        local_modified: bool,
    },
    /// The configured branch/ref was deleted; show a warning, not an update badge.
    SourceGone,
    /// Network could not establish source state; prior persisted badge remains unchanged.
    Unknown,
    /// Automatic checking is disabled and the user did not explicitly request a check.
    NotRequested,
}
/// Clock used to make automatic update scheduling deterministic in tests.
pub trait UpdateClock {
    /// Current wall-clock time.
    fn now(&self) -> std::time::SystemTime;
}

/// System wall clock for production scheduling.
pub struct SystemUpdateClock;

impl UpdateClock for SystemUpdateClock {
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
    }
}

/// Trigger for the source-check scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateScheduleTrigger {
    /// The first scheduling pass after application launch.
    Launch,
    /// A configurable periodic scheduling pass.
    Interval,
}

/// Launch/interval gate with editor and wizard deferral.
#[derive(Default)]
pub struct AutoUpdateScheduler {
    last_run: Option<std::time::SystemTime>,
    deferred: Vec<ItemId>,
}

impl AutoUpdateScheduler {
    /// Returns enabled Git/marketplace item IDs when this trigger is due.
    ///
    /// The interval is supplied by the caller and the clock is injected. A
    /// default-off setting with no per-item opt-in returns no work.
    ///
    /// # Errors
    /// Returns a database error while enumerating sources or resolving policy.
    pub fn due_items(
        &mut self,
        connection: &Connection,
        trigger: UpdateScheduleTrigger,
        interval: std::time::Duration,
        editor_or_wizard_open: bool,
        clock: &impl UpdateClock,
    ) -> Result<Vec<ItemId>, at_db::DbError> {
        let now = clock.now();
        let due = match trigger {
            UpdateScheduleTrigger::Launch => self.last_run.is_none(),
            UpdateScheduleTrigger::Interval => self.last_run.is_some_and(|last| {
                now.duration_since(last)
                    .is_ok_and(|elapsed| elapsed >= interval)
            }),
        };
        if !due {
            return Ok(Vec::new());
        }
        self.last_run = Some(now);
        let mut statement = connection.prepare(
            "SELECT DISTINCT item_id FROM sources WHERE deleted_at IS NULL \
             AND kind IN ('git', 'marketplace') ORDER BY item_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut enabled = Vec::new();
        for row in rows {
            let item_id = row?
                .parse::<ItemId>()
                .map_err(|error| at_db::DbError::InvalidValue {
                    field: "item_id",
                    reason: error.to_string(),
                })?;
            if at_db::sources::effective_auto_update(connection, &item_id)? {
                enabled.push(item_id);
            }
        }
        if editor_or_wizard_open {
            for item_id in enabled {
                if !self.deferred.contains(&item_id) {
                    self.deferred.push(item_id);
                }
            }
            Ok(Vec::new())
        } else {
            Ok(enabled)
        }
    }

    /// Releases paused items after the wizard/editor is saved or discarded.
    #[must_use]
    pub fn resume_after_editor_close(&mut self) -> Vec<ItemId> {
        std::mem::take(&mut self.deferred)
    }
}

/// Failure during source check or persistence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SkillSourceError {
    /// The item id is malformed.
    #[error("invalid skill item id")]
    InvalidItemId,
    /// No Git source is recorded for the item.
    #[error("skill has no live Git source")]
    MissingGitSource,
    /// Source path could not be hashed safely.
    #[error("could not hash the local skill package: {0}")]
    Hash(#[from] at_store::HashError),
    /// Git source check failed.
    #[error(transparent)]
    Git(#[from] at_git::source_check::SourceCheckError),
    /// Source database operation failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// The system clock could not be represented as an RFC 3339 timestamp.
    #[error("could not format source check time")]
    Timestamp,
    /// Marketplace listing metadata is invalid for install recording.
    #[error(transparent)]
    Marketplace(#[from] at_git::marketplace::MarketplaceError),
}
/// Result of preparing an auto-update for user review.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkillUpdateDecision {
    /// Content changed locally and automatic update must not replace it.
    SkippedLocalModified,
    /// The incoming package is byte-identical to the installed package.
    UpToDate,
    /// Reviewed plan and preview for a clean package update.
    Review(Box<SkillUpdatePlan>),
    /// An open skill editor has unsaved changes; the update is deferred.
    PausedEditorUnsavedChanges,
}

/// Reviewed package update plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillUpdatePlan {
    /// Exact filesystem operations to apply.
    pub change_set: at_model::ChangeSet,
    /// Preview bound to the current file hashes.
    pub preview: crate::preview::Preview,
    /// Source row identity to update after successful apply.
    pub source_id: String,
    /// New upstream commit SHA.
    pub commit_sha: String,
    /// Package path relative to the store root.
    pub package_path: std::path::PathBuf,
}

/// Errors while planning or applying a reviewed skill update.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SkillUpdateError {
    /// Invalid item identity.
    #[error("invalid skill item id")]
    InvalidItemId,
    /// No source row exists.
    #[error("skill has no source row")]
    MissingSource,
    /// The source is not a Git skill.
    #[error("skill source is not Git")]
    NotGit,
    /// The update plan is invalid or cannot be previewed.
    #[error(transparent)]
    Preview(#[from] crate::preview::PreviewError),
    /// A package could not be read or hashed.
    #[error(transparent)]
    Package(#[from] at_store::SkillPackageError),
    /// The source metadata could not be updated.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// The reviewed apply succeeded but the updated package could not be hashed.
    #[error(transparent)]
    Hash(#[from] at_store::HashError),
    /// A ChangeSet contains an invalid hash or path.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
}

/// Inputs for building a skill update plan.
#[derive(Clone, Copy)]
pub struct SkillUpdateRequest<'a> {
    /// Stable skill item id.
    pub item_id: &'a str,
    /// Installed package folder.
    pub local_package: &'a Path,
    /// Store-relative package path.
    pub package_path: &'a Path,
    /// Store root used for preview path resolution.
    pub store_root: &'a Path,
    /// Validated upstream package.
    pub remote_package: &'a at_store::SkillPackage,
    /// Upstream commit id.
    pub remote_sha: &'a str,
    /// Owning store scope.
    pub scope: at_model::Scope,
    /// Project id for Project or Local scope.
    pub project_id: Option<&'a str>,
    /// Whether a skill editor currently has unsaved changes.
    pub editor_has_unsaved_changes: bool,
    /// Explicitly replace local edits after review.
    pub override_local_modified: bool,
}

/// Builds a content-bound update plan from a fetched upstream package.
///
/// The local package is never changed here. Local edits produce `SkippedLocalModified`
/// unless the caller explicitly requests an override; that override still returns a review.
///
/// # Errors
/// Returns a typed item/source/database/package/preview error.
pub fn plan_skill_update(
    connection: &Connection,
    request: SkillUpdateRequest<'_>,
) -> Result<SkillUpdateDecision, SkillUpdateError> {
    let SkillUpdateRequest {
        item_id,
        local_package,
        package_path,
        store_root,
        remote_package,
        remote_sha,
        scope,
        project_id,
        override_local_modified,
        editor_has_unsaved_changes,
    } = request;
    if editor_has_unsaved_changes {
        return Ok(SkillUpdateDecision::PausedEditorUnsavedChanges);
    }
    let id = ItemId::from_str(item_id).map_err(|_| SkillUpdateError::InvalidItemId)?;
    let source =
        at_db::sources::source_for(connection, &id)?.ok_or(SkillUpdateError::MissingSource)?;
    if source.kind != at_db::sources::SourceKind::Git {
        return Err(SkillUpdateError::NotGit);
    }
    let local = at_store::SkillPackage::read(local_package)?;
    let local_modified = source
        .content_hash
        .as_deref()
        .is_some_and(|hash| hash != local.hash.to_string());
    if local_modified && !override_local_modified {
        at_db::sources::set_local_modified(connection, &source.id, true)?;
        return Ok(SkillUpdateDecision::SkippedLocalModified);
    }

    let mut operations = Vec::new();
    let mut paths = std::collections::BTreeSet::new();
    paths.extend(local.package.paths().map(str::to_owned));
    paths.extend(remote_package.paths().map(str::to_owned));
    for relative in paths {
        let old = local.package.get(&relative);
        let new = remote_package.get(&relative);
        if old
            .zip(new)
            .is_some_and(|(old, new)| old.bytes == new.bytes)
        {
            continue;
        }
        let path = package_path.join(&relative);
        let expected = match old {
            Some(old) => at_model::ExpectedState::hash(at_fs::sha256_hex(&old.bytes))?,
            None => at_model::ExpectedState::absent(),
        };
        match (old, new) {
            (_, Some(new)) => operations.push(at_model::FileOp::Write {
                path,
                bytes: new.bytes.clone(),
                expected,
                follow_symlink: false,
                backup: true,
            }),
            (Some(_), None) => operations.push(at_model::FileOp::Delete { path, expected }),
            (None, None) => {}
        }
    }
    if operations.is_empty() {
        at_db::sources::set_local_modified(connection, &source.id, false)?;
        return Ok(SkillUpdateDecision::UpToDate);
    }
    let mut change_set = at_model::ChangeSet {
        id: format!("op_{}", uuid::Uuid::now_v7()),
        preview_id: String::new(),
        operation_kind: at_model::OperationKind::parse("update")
            .map_err(|_| SkillUpdateError::MissingSource)?,
        scope: Some(scope),
        project_id: project_id.map(str::to_owned),
        summary: "Update skill package".to_owned(),
        ops: operations,
        created_at_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
    };
    change_set
        .validate()
        .map_err(crate::preview::PreviewError::from)?;
    let preview = crate::preview::preview(&change_set, store_root)?;
    change_set.preview_id.clone_from(&preview.id);
    Ok(SkillUpdateDecision::Review(Box::new(SkillUpdatePlan {
        change_set,
        preview,
        source_id: source.id,
        commit_sha: remote_sha.to_owned(),
        package_path: package_path.to_path_buf(),
    })))
}

/// Applies the exact reviewed update and advances source metadata only after success.
///
/// # Errors
/// Returns a stale-plan, filesystem, package-hash or database error.
pub fn apply_skill_update(
    connection: &Connection,
    plan: &SkillUpdatePlan,
    context: &mut at_fs::changeset::ApplyContext<'_>,
) -> Result<at_fs::changeset::ApplyOutcome, SkillUpdateError> {
    let outcome = match crate::preview::apply_reviewed(&plan.change_set, context) {
        Ok(outcome) => outcome,
        Err(error) => {
            let mut operation = at_db::ops::NewOperation::new(
                at_db::ops::OperationKind::Update,
                at_db::ops::OperationActor::Updater,
            );
            operation.scope = plan.change_set.scope;
            operation.summary = Some("Skill update failed");
            at_db::ops::record(
                connection,
                &operation,
                &[],
                at_db::ops::OperationStatus::Failed,
                Some("apply_failed"),
            )?;
            return Err(SkillUpdateError::Preview(error));
        }
    };
    let installed = context.store_root.join(&plan.package_path);
    let content_hash = hash_skill_folder(&installed)?.to_string();
    at_db::sources::set_installed_commit(
        connection,
        &plan.source_id,
        &plan.commit_sha,
        Some(&content_hash),
    )?;
    at_db::sources::set_local_modified(connection, &plan.source_id, false)?;
    Ok(outcome)
}

/// Runs a check only for an explicit request or when effective auto-update is enabled.
///
/// With auto-update off and no user request, returns `NotRequested` without invoking git.
///
/// # Errors
/// Returns an error for invalid IDs, source, policy, hashing or database failures.
pub fn check_skill_source_if_requested(
    connection: &Connection,
    item_id: &str,
    local_package: &Path,
    explicit_request: bool,
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
) -> Result<SkillSourceCheck, SkillSourceError> {
    let id = ItemId::from_str(item_id).map_err(|_| SkillSourceError::InvalidItemId)?;
    if !explicit_request && !at_db::sources::effective_auto_update(connection, &id)? {
        return Ok(SkillSourceCheck::NotRequested);
    }
    check_skill_source(connection, item_id, local_package, policy, log)
}
/// Checks a git-backed skill on explicit request and updates its source row.
///
/// The local package is hashed with the same canonical bounded hash used at import. `Unknown`
/// leaves the prior remote state untouched.
///
/// # Errors
/// Returns an error for invalid IDs, missing/non-Git sources, unsafe packages, persistence errors,
/// or policy refusal.
pub fn check_skill_source(
    connection: &Connection,
    item_id: &str,
    local_package: &Path,
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
) -> Result<SkillSourceCheck, SkillSourceError> {
    let id = ItemId::from_str(item_id).map_err(|_| SkillSourceError::InvalidItemId)?;
    let source =
        at_db::sources::source_for(connection, &id)?.ok_or(SkillSourceError::MissingGitSource)?;
    if source.kind != at_db::sources::SourceKind::Git {
        return Err(SkillSourceError::MissingGitSource);
    }
    let Some(url) = source.url.as_deref() else {
        return Err(SkillSourceError::MissingGitSource);
    };
    let local_hash = hash_skill_folder(local_package)?.to_string();
    let local_modified = source
        .content_hash
        .as_deref()
        .is_some_and(|imported| imported != local_hash);
    let check = at_git::source_check::check_source_with_process(
        url,
        source.git_ref.as_deref(),
        policy,
        log,
    )?;
    match check {
        at_git::source_check::SourceCheck::Unknown => Ok(SkillSourceCheck::Unknown),
        at_git::source_check::SourceCheck::SourceGone => {
            let checked_at = checked_at()?;
            at_db::sources::set_local_modified(connection, &source.id, local_modified)?;
            at_db::sources::record_update_check(
                connection,
                &source.id,
                &at_db::sources::CheckOutcome {
                    remote_sha: None,
                    etag: None,
                    checked_at: Some(&checked_at),
                    update_available: false,
                },
            )?;
            Ok(SkillSourceCheck::SourceGone)
        }
        at_git::source_check::SourceCheck::Found { remote_sha } => {
            let changed = source.commit_sha.as_deref() != Some(remote_sha.as_str());
            let checked_at = checked_at()?;
            at_db::sources::set_local_modified(connection, &source.id, local_modified)?;
            at_db::sources::record_update_check(
                connection,
                &source.id,
                &at_db::sources::CheckOutcome {
                    remote_sha: Some(&remote_sha),
                    etag: None,
                    checked_at: Some(&checked_at),
                    update_available: changed,
                },
            )?;
            if changed {
                Ok(SkillSourceCheck::UpdateAvailable {
                    remote_sha,
                    local_modified,
                })
            } else {
                Ok(SkillSourceCheck::Current {
                    remote_sha,
                    local_modified,
                })
            }
        }
        _ => Ok(SkillSourceCheck::Unknown),
    }
}

fn checked_at() -> Result<String, SkillSourceError> {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| SkillSourceError::Timestamp)
}

/// Records provenance after a marketplace skill was installed through TS-020.
///
/// # Errors
/// Returns an error for an invalid marketplace source or SQLite persistence failure.
pub fn record_marketplace_install(
    connection: &mut Connection,
    item_id: &ItemId,
    skill: &at_git::marketplace::MarketplaceSkill,
    commit_sha: &str,
    subpath: &str,
    content_hash: &str,
) -> Result<String, SkillSourceError> {
    at_git::marketplace::MarketplaceClient::install_source(skill)?;
    let mut source =
        at_db::sources::SourceSpec::new(item_id, at_db::sources::SourceKind::Marketplace);
    source.url = Some(&skill.source_url);
    source.subpath = Some(subpath);
    source.commit_sha = Some(commit_sha);
    source.content_hash = Some(content_hash);
    source.marketplace_id = Some(&skill.id);
    source.install_count = i64::try_from(skill.installs).ok();
    Ok(at_db::sources::upsert_source(connection, &source)?)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(directory)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .expect("run local Git fixture");
        assert!(status.success(), "fixture git command failed: {args:?}");
    }

    fn git_output(directory: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(directory)
            .args(args)
            .output()
            .expect("read fixture commit");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("fixture sha utf8")
            .trim()
            .to_owned()
    }

    fn insert_item(database: &at_db::Database, item_id: &ItemId) {
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name, managed) \
                 VALUES (?1, 'skill', 'user', ?2, 'Fixture skill', 1)",
                rusqlite::params![item_id.to_string(), item_id.to_string()],
            )
            .expect("insert fixture item");
    }

    #[test]
    fn explicit_check_persists_remote_state_and_detects_local_edits() {
        let fixture = tempfile::tempdir().expect("fixture tempdir");
        let bare = fixture.path().join("remote.git");
        let work = fixture.path().join("work");
        let package = fixture.path().join("installed-skill");
        std::fs::create_dir(&work).expect("work directory");
        std::fs::create_dir(&package).expect("installed package");
        std::fs::write(package.join("SKILL.md"), "installed").expect("package content");
        git(
            fixture.path(),
            &["init", "--bare", bare.to_str().expect("bare path")],
        );
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "fixture@example.invalid"]);
        git(&work, &["config", "user.name", "Fixture"]);
        std::fs::write(work.join("SKILL.md"), "one").expect("first source revision");
        git(&work, &["add", "SKILL.md"]);
        git(&work, &["commit", "-m", "first"]);
        git(
            &work,
            &["remote", "add", "origin", bare.to_str().expect("bare path")],
        );
        git(&work, &["push", "origin", "main"]);
        let imported_sha = git_output(&work, &["rev-parse", "HEAD"]);
        std::fs::write(work.join("SKILL.md"), "two").expect("second source revision");
        git(&work, &["add", "SKILL.md"]);
        git(&work, &["commit", "-m", "second"]);
        git(&work, &["push", "origin", "main"]);

        let mut database = at_db::Database::open_in_memory().expect("database");
        let item_id = ItemId::new();
        insert_item(&database, &item_id);
        let package_hash = hash_skill_folder(&package)
            .expect("package hash")
            .to_string();
        let source_url = format!("file://{}", bare.display());
        let mut spec = at_db::sources::SourceSpec::new(&item_id, at_db::sources::SourceKind::Git);
        spec.url = Some(&source_url);
        spec.git_ref = Some("main");
        spec.commit_sha = Some(&imported_sha);
        spec.content_hash = Some(&package_hash);
        at_db::sources::upsert_source(database.connection_mut(), &spec).expect("source row");

        let mut log = NetworkEventLog::new();
        let result = check_skill_source(
            database.connection(),
            &item_id.to_string(),
            &package,
            &NetworkPolicy::default(),
            &mut log,
        )
        .expect("explicit update check");
        let remote_sha = git_output(&work, &["rev-parse", "HEAD"]);
        assert_eq!(
            result,
            SkillSourceCheck::UpdateAvailable {
                remote_sha: remote_sha.clone(),
                local_modified: false,
            }
        );
        let row = at_db::sources::source_for(database.connection(), &item_id)
            .expect("read source")
            .expect("live source");
        assert_eq!(row.remote_sha.as_deref(), Some(remote_sha.as_str()));
        assert!(row.update_available);
        assert!(!row.local_modified);

        std::fs::write(package.join("SKILL.md"), "local edit").expect("local edit");
        let modified = check_skill_source(
            database.connection(),
            &item_id.to_string(),
            &package,
            &NetworkPolicy::default(),
            &mut log,
        )
        .expect("check locally modified package");
        assert_eq!(
            modified,
            SkillSourceCheck::UpdateAvailable {
                remote_sha,
                local_modified: true,
            }
        );
    }

    #[test]
    fn auto_update_off_without_user_request_does_not_run_git() {
        let database = at_db::Database::open_in_memory().expect("database");
        let item_id = ItemId::new();
        let mut log = NetworkEventLog::new();
        let result = check_skill_source_if_requested(
            database.connection(),
            &item_id.to_string(),
            Path::new("missing-local-path"),
            false,
            &NetworkPolicy::default(),
            &mut log,
        )
        .expect("disabled automatic check");
        assert_eq!(result, SkillSourceCheck::NotRequested);
        assert_eq!(log.entries().len(), 0);
    }

    #[test]
    fn scheduler_uses_injected_clock_interval_and_resumes_deferred_editor_work() {
        struct FixedClock(std::time::SystemTime);
        impl UpdateClock for FixedClock {
            fn now(&self) -> std::time::SystemTime {
                self.0
            }
        }

        let mut database = at_db::Database::open_in_memory().expect("database");
        let item_id = ItemId::new();
        insert_item(&database, &item_id);
        let mut source = at_db::sources::SourceSpec::new(&item_id, at_db::sources::SourceKind::Git);
        source.auto_update = Some(at_db::sources::AutoUpdate::On);
        at_db::sources::upsert_source(database.connection_mut(), &source).expect("source");

        let start = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let mut scheduler = AutoUpdateScheduler::default();
        assert!(
            scheduler
                .due_items(
                    database.connection(),
                    UpdateScheduleTrigger::Launch,
                    std::time::Duration::from_secs(30),
                    true,
                    &FixedClock(start),
                )
                .expect("paused launch")
                .is_empty()
        );
        assert_eq!(scheduler.resume_after_editor_close(), vec![item_id]);
        assert!(
            scheduler
                .due_items(
                    database.connection(),
                    UpdateScheduleTrigger::Interval,
                    std::time::Duration::from_secs(30),
                    false,
                    &FixedClock(start + std::time::Duration::from_secs(29)),
                )
                .expect("not due")
                .is_empty()
        );
        assert_eq!(
            scheduler
                .due_items(
                    database.connection(),
                    UpdateScheduleTrigger::Interval,
                    std::time::Duration::from_secs(30),
                    false,
                    &FixedClock(start + std::time::Duration::from_secs(30)),
                )
                .expect("due interval"),
            vec![item_id]
        );
    }
    #[test]
    fn marketplace_install_records_source_and_install_snapshot() {
        let mut database = at_db::Database::open_in_memory().expect("database");
        let item_id = ItemId::new();
        insert_item(&database, &item_id);
        let skill = at_git::marketplace::MarketplaceSkill {
            id: "octo/tools/plan".to_owned(),
            name: "Plan".to_owned(),
            description: "fixture".to_owned(),
            author: "octo".to_owned(),
            installs: 73,
            source_url: "https://github.com/octo/tools".to_owned(),
            updated_at: None,
        };
        record_marketplace_install(
            database.connection_mut(),
            &item_id,
            &skill,
            "0123456789012345678901234567890123456789",
            "skills/plan",
            "fixture-hash",
        )
        .expect("record installed marketplace skill");
        let source = at_db::sources::source_for(database.connection(), &item_id)
            .expect("read marketplace source")
            .expect("source row exists");
        assert_eq!(source.kind, at_db::sources::SourceKind::Marketplace);
        assert_eq!(source.marketplace_id.as_deref(), Some("octo/tools/plan"));
        assert_eq!(source.install_count, Some(73));
        assert_eq!(
            source.commit_sha.as_deref(),
            Some("0123456789012345678901234567890123456789")
        );
    }
    #[test]
    fn offline_check_preserves_existing_badge_state() {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let package = fixture.path().join("installed-skill");
        std::fs::create_dir(&package).expect("package directory");
        std::fs::write(package.join("SKILL.md"), "fixture").expect("package file");
        let mut database = at_db::Database::open_in_memory().expect("database");
        let item_id = ItemId::new();
        insert_item(&database, &item_id);
        let content_hash = hash_skill_folder(&package)
            .expect("content hash")
            .to_string();
        let mut source = at_db::sources::SourceSpec::new(&item_id, at_db::sources::SourceKind::Git);
        source.url = Some("https://example.invalid/skill.git");
        source.git_ref = Some("main");
        source.commit_sha = Some("installed-sha");
        source.content_hash = Some(&content_hash);
        at_db::sources::upsert_source(database.connection_mut(), &source).expect("source row");
        let row = at_db::sources::source_for(database.connection(), &item_id)
            .expect("read source")
            .expect("live source");
        at_db::sources::record_update_check(
            database.connection(),
            &row.id,
            &at_db::sources::CheckOutcome {
                remote_sha: Some("previous-remote-sha"),
                checked_at: Some("2026-11-06T00:00:00Z"),
                update_available: true,
                ..Default::default()
            },
        )
        .expect("record previous update state");
        let mut log = NetworkEventLog::new();
        let result = check_skill_source(
            database.connection(),
            &item_id.to_string(),
            &package,
            &NetworkPolicy::default().with_offline(true),
            &mut log,
        )
        .expect("offline result");
        assert_eq!(result, SkillSourceCheck::Unknown);
        let after = at_db::sources::source_for(database.connection(), &item_id)
            .expect("read source after offline check")
            .expect("live source remains");
        assert_eq!(after.remote_sha.as_deref(), Some("previous-remote-sha"));
        assert!(after.update_available);
        assert_eq!(log.entries().len(), 0);
    }
    #[derive(Default)]
    struct UpdateRecorder;

    impl at_fs::changeset::OperationRecorder for UpdateRecorder {
        fn begin(&mut self, _: &at_model::ChangeSet) -> Result<String, String> {
            Ok("update-test-op".to_owned())
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
    fn clean_update_is_reviewed_then_applied_with_backup_and_source_commit_bump() {
        let temp = tempfile::tempdir().expect("fixture");
        let store =
            at_store::layout::StoreLayout::new(at_model::Scope::User, temp.path().join("store"));
        store.init().expect("store");
        let slug = at_store::layout::Slug::parse("alpha").expect("slug");
        let package_dir = store.skill_dir(&slug);
        std::fs::create_dir_all(&package_dir).expect("package folder");
        std::fs::write(package_dir.join("SKILL.md"), "old").expect("old package");
        let local_hash = hash_skill_folder(&package_dir)
            .expect("local hash")
            .to_string();
        let mut database = at_db::Database::open_in_memory().expect("database");
        let item_id = ItemId::new();
        insert_item(&database, &item_id);
        let mut source = at_db::sources::SourceSpec::new(&item_id, at_db::sources::SourceKind::Git);
        source.url = Some("https://example.invalid/skill.git");
        source.git_ref = Some("main");
        source.commit_sha = Some("old-commit");
        source.content_hash = Some(&local_hash);
        let source_id =
            at_db::sources::upsert_source(database.connection_mut(), &source).expect("source");

        let mut remote = at_store::SkillPackage::new();
        remote
            .insert("SKILL.md", b"new".to_vec(), 0o644)
            .expect("remote document");
        let relative_package = package_dir
            .strip_prefix(store.root())
            .expect("relative package path");
        let decision = plan_skill_update(
            database.connection(),
            SkillUpdateRequest {
                item_id: &item_id.to_string(),
                local_package: &package_dir,
                package_path: relative_package,
                store_root: store.root(),
                remote_package: &remote,
                remote_sha: "new-commit",
                scope: at_model::Scope::User,
                project_id: None,
                editor_has_unsaved_changes: false,
                override_local_modified: false,
            },
        )
        .expect("plan");
        let SkillUpdateDecision::Review(plan) = decision else {
            panic!("clean changed package must require review");
        };
        assert_eq!(plan.preview.files.len(), 1);
        assert!(plan.preview.files[0].backup_required);
        assert!(matches!(
            plan.preview.files[0].diff,
            crate::preview::DiffKind::Unified { .. }
        ));

        let backups = temp.path().join("backups");
        std::fs::create_dir(&backups).expect("backup directory");
        let mut recorder = UpdateRecorder;
        let mut context = at_fs::changeset::ApplyContext {
            store_root: store.root(),
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        apply_skill_update(database.connection(), &plan, &mut context).expect("apply");
        assert_eq!(
            std::fs::read(package_dir.join("SKILL.md")).expect("updated file"),
            b"new"
        );
        let after = at_db::sources::source_for(database.connection(), &item_id)
            .expect("read source")
            .expect("source row");
        assert_eq!(after.commit_sha.as_deref(), Some("new-commit"));
        assert!(!after.update_available);
        assert!(!after.local_modified);
        assert_ne!(after.content_hash.as_deref(), Some(local_hash.as_str()));
        assert_eq!(after.id, source_id);
        assert_eq!(
            plan_skill_update(
                database.connection(),
                SkillUpdateRequest {
                    item_id: &item_id.to_string(),
                    local_package: &package_dir,
                    package_path: relative_package,
                    store_root: store.root(),
                    remote_package: &remote,
                    remote_sha: "deferred-commit",
                    scope: at_model::Scope::User,
                    project_id: None,
                    editor_has_unsaved_changes: true,
                    override_local_modified: false,
                },
            )
            .expect("unsaved editor defers update"),
            SkillUpdateDecision::PausedEditorUnsavedChanges
        );
        let mut stale_remote = at_store::SkillPackage::new();
        stale_remote
            .insert("SKILL.md", b"stale-plan target".to_vec(), 0o644)
            .expect("stale test package");
        let stale_decision = plan_skill_update(
            database.connection(),
            SkillUpdateRequest {
                item_id: &item_id.to_string(),
                local_package: &package_dir,
                package_path: relative_package,
                store_root: store.root(),
                remote_package: &stale_remote,
                remote_sha: "stale-commit",
                scope: at_model::Scope::User,
                project_id: None,
                editor_has_unsaved_changes: false,
                override_local_modified: false,
            },
        )
        .expect("stale test plan");
        let SkillUpdateDecision::Review(stale_plan) = stale_decision else {
            panic!("changed remote package needs a review");
        };
        std::fs::write(package_dir.join("SKILL.md"), "external edit").expect("external edit");
        let mut stale_context = at_fs::changeset::ApplyContext {
            store_root: store.root(),
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        assert!(matches!(
            apply_skill_update(database.connection(), &stale_plan, &mut stale_context),
            Err(SkillUpdateError::Preview(
                crate::preview::PreviewError::ConcurrentModification
            ))
        ));
        assert_eq!(
            std::fs::read(package_dir.join("SKILL.md")).expect("preserved external edit"),
            b"external edit"
        );
        assert_eq!(
            at_db::sources::source_for(database.connection(), &item_id)
                .expect("read source")
                .and_then(|source| source.commit_sha),
            Some("new-commit".to_owned())
        );
        assert_eq!(
            database
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM operations WHERE kind = 'update' AND status = 'failed'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("failed update operation"),
            1
        );
        std::fs::write(package_dir.join("SKILL.md"), "local edit").expect("local edit");
        let mut newer_remote = at_store::SkillPackage::new();
        newer_remote
            .insert("SKILL.md", b"third".to_vec(), 0o644)
            .expect("newer remote document");
        let request = SkillUpdateRequest {
            item_id: &item_id.to_string(),
            local_package: &package_dir,
            package_path: relative_package,
            store_root: store.root(),
            remote_package: &newer_remote,
            remote_sha: "third-commit",
            scope: at_model::Scope::User,
            project_id: None,
            editor_has_unsaved_changes: false,
            override_local_modified: false,
        };
        assert_eq!(
            plan_skill_update(database.connection(), request).expect("local modification check"),
            SkillUpdateDecision::SkippedLocalModified
        );
        assert_eq!(
            std::fs::read(package_dir.join("SKILL.md")).expect("preserved local edit"),
            b"local edit"
        );
        let override_decision = plan_skill_update(
            database.connection(),
            SkillUpdateRequest {
                override_local_modified: true,
                ..request
            },
        )
        .expect("override plan");
        let SkillUpdateDecision::Review(override_plan) = override_decision else {
            panic!("explicit local-edit override must require review");
        };
        assert!(override_plan.preview.files[0].backup_required);
        let mut override_context = at_fs::changeset::ApplyContext {
            store_root: store.root(),
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        apply_skill_update(database.connection(), &override_plan, &mut override_context)
            .expect("apply override");
        assert_eq!(
            std::fs::read(package_dir.join("SKILL.md")).expect("updated package"),
            b"third"
        );
    }
}
