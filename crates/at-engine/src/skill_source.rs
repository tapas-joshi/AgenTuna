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
}
