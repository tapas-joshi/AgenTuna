//! Skill deployment policy: mode choice, cross-volume handling and error translation (TS-027).
//!
//! This is a thin policy layer on top of [`crate::deploy::deploy`], TF-006's primitive. It never
//! creates a link, junction or copy itself: [`deploy_skill`] chooses the mode, refuses a
//! cross-volume link unless copy fallback is enabled, names a foreign symlink's target in the
//! refusal, and normalizes `EPERM`, `EACCES`, `ENOSPC` and `EDQUOT` into typed [`FsError`]
//! values with remediation text.
//!
//! The per-agent mode follows the settings contract: `link_mode.<agent>` wins over
//! `default_link_mode`.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use at_model::LinkMode;

use crate::deploy::{self, CopyOutcome, DeployRequest, LinkOutcome};
use crate::error::FsError;
use crate::guard::{self, OverwriteGuard};

/// The mode a skill deployment should use for one agent.
///
/// `agent_override` is the `link_mode.<agent>` setting; it wins over `default_mode`
/// (`default_link_mode`). Copy fallback is opt-in, so a link is never silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillStrategy {
    /// Selected mode.
    pub mode: LinkMode,
    /// Copy when a link is refused or would cross volumes.
    pub copy_fallback: bool,
}

impl SkillStrategy {
    /// Resolves the per-agent setting.
    #[must_use]
    pub fn new(default_mode: LinkMode, agent_override: Option<LinkMode>) -> Self {
        Self {
            mode: agent_override.unwrap_or(default_mode),
            copy_fallback: false,
        }
    }

    /// Allows the policy to fall back to a copy. The fallback is reported as a warning.
    #[must_use]
    pub fn with_copy_fallback(mut self) -> Self {
        self.copy_fallback = true;
        self
    }
}

/// The per-agent mode for a skill deployment.
#[must_use]
pub fn skill_mode(default_mode: LinkMode, agent_override: Option<LinkMode>) -> LinkMode {
    agent_override.unwrap_or(default_mode)
}

/// Answers whether two paths are on one volume.
///
/// Injected so cross-volume policy can be tested on a single-volume machine.
pub trait VolumeLookup: fmt::Debug {
    /// Whether a link between `source` and `destination` stays on one volume.
    fn same_volume(&self, source: &Path, destination: &Path) -> bool;
}

/// The real volume check: compares `st_dev` of the nearest existing ancestors.
#[derive(Debug, Clone, Copy)]
pub struct SystemVolumes;

impl VolumeLookup for SystemVolumes {
    fn same_volume(&self, source: &Path, destination: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Some(source) = nearest_existing(source) else {
                return false;
            };
            let Some(destination) = nearest_existing(destination) else {
                return false;
            };
            match (fs::metadata(&source), fs::metadata(&destination)) {
                (Ok(a), Ok(b)) => a.dev() == b.dev(),
                _ => false,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (source, destination);
            false
        }
    }
}

/// One skill deployment request.
#[derive(Debug, Clone)]
pub struct SkillDeployRequest {
    /// Canonical store folder of the skill.
    pub source: PathBuf,
    /// `<agent>/<slug>` path.
    pub target: PathBuf,
    /// Mode and fallback policy.
    pub strategy: SkillStrategy,
    /// Refuse-to-overwrite guard, when the caller has one.
    pub guard: Option<Arc<dyn OverwriteGuard>>,
}

impl SkillDeployRequest {
    /// A request with no guard.
    #[must_use]
    pub fn new(
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        strategy: SkillStrategy,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            strategy,
            guard: None,
        }
    }

    /// Attaches a refuse-to-overwrite guard.
    #[must_use]
    pub fn with_guard(mut self, guard: Arc<dyn OverwriteGuard>) -> Self {
        self.guard = Some(guard);
        self
    }
}

/// A fallback the policy had to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SkillWarning {
    /// The link crossed volumes, so a copy was written instead.
    CopyFallbackCrossVolume,
    /// The platform refused the link, so a copy was written instead.
    CopyFallbackLinkUnsupported,
}

/// What the deployment did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SkillDeployOutcome {
    /// Mode actually used.
    pub mode: LinkMode,
    /// Deployment path.
    pub target: PathBuf,
    /// Present when a link was written.
    pub link: Option<LinkOutcome>,
    /// Present when a copy was written.
    pub copy: Option<CopyOutcome>,
    /// Fallbacks used, in order.
    pub warnings: Vec<SkillWarning>,
}

/// Deploys a skill folder with the policy in `request`.
///
/// # Errors
/// [`FsError::CrossVolume`] when a link was requested across volumes and copy fallback is off;
/// [`FsError::ForeignLink`] when the target is a symlink pointing elsewhere (the error names the
/// target); [`FsError::RefusedOverwrite`] for any other unmanaged entry; typed
/// [`FsError::PermissionDenied`], [`FsError::NoSpaceLeft`] and [`FsError::QuotaExceeded`] for the
/// corresponding platform failures.
pub fn deploy_skill(request: &SkillDeployRequest) -> Result<SkillDeployOutcome, FsError> {
    deploy_skill_with(request, &SystemVolumes)
}

/// [`deploy_skill`] with an injected volume check. For tests.
#[doc(hidden)]
pub fn deploy_skill_with(
    request: &SkillDeployRequest,
    volumes: &dyn VolumeLookup,
) -> Result<SkillDeployOutcome, FsError> {
    let source = guard::clean_absolute(&request.source)?;
    let target = guard::clean_absolute(&request.target)?;
    let source_real = fs::canonicalize(&source).map_err(|e| normalize_io(&source, e))?;
    check_existing_target(&target, &source_real)?;

    let mut warnings = Vec::new();
    let mut mode = request.strategy.mode;
    if mode == LinkMode::Link {
        let parent = target.parent().ok_or_else(|| FsError::NoParent {
            path: target.clone(),
        })?;
        if !volumes.same_volume(&source_real, parent) {
            if request.strategy.copy_fallback {
                warnings.push(SkillWarning::CopyFallbackCrossVolume);
                mode = LinkMode::Copy;
            } else {
                return Err(FsError::CrossVolume {
                    from: source_real,
                    to: target,
                });
            }
        }
    }

    let mut deploy_request = DeployRequest::new(&source_real, &target, mode);
    deploy_request.fallback_to_copy = request.strategy.copy_fallback;
    if let Some(guard) = &request.guard {
        deploy_request = deploy_request.with_guard(Arc::clone(guard));
    }
    let outcome = deploy::deploy(&deploy_request)
        .map_err(|error| normalize_deploy_error(error, &source_real, &target))?;
    if request.strategy.mode == LinkMode::Link
        && outcome.mode == LinkMode::Copy
        && !warnings.contains(&SkillWarning::CopyFallbackCrossVolume)
    {
        warnings.push(SkillWarning::CopyFallbackLinkUnsupported);
    }

    Ok(SkillDeployOutcome {
        mode: outcome.mode,
        target: outcome.target,
        link: outcome.link,
        copy: outcome.copy,
        warnings,
    })
}

/// Refuses a foreign symlink before the primitive replaces anything, naming its target.
fn check_existing_target(target: &Path, source_real: &Path) -> Result<(), FsError> {
    match fs::symlink_metadata(target) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(normalize_io(target, e)),
        Ok(meta) if meta.file_type().is_symlink() => {
            let raw = fs::read_link(target).map_err(|e| normalize_io(target, e))?;
            let joined = if raw.is_absolute() {
                raw
            } else {
                target
                    .parent()
                    .ok_or_else(|| FsError::NoParent {
                        path: target.to_path_buf(),
                    })?
                    .join(raw)
            };
            let resolved = fs::canonicalize(&joined).unwrap_or(joined);
            if resolved == source_real {
                Ok(())
            } else {
                Err(FsError::ForeignLink {
                    path: target.to_path_buf(),
                    target: resolved,
                })
            }
        }
        Ok(_) => Ok(()),
    }
}

/// Normalizes a platform error on `path` into a typed [`FsError`].
///
/// `EACCES` and `EPERM` become [`FsError::PermissionDenied`], `ENOSPC` becomes
/// [`FsError::NoSpaceLeft`] and `EDQUOT` becomes [`FsError::QuotaExceeded`]; everything else
/// keeps the crate's standard IO mapping.
#[must_use]
pub fn normalize_io(path: &Path, error: io::Error) -> FsError {
    FsError::from_platform_io(path, error)
}

/// Applies [`normalize_io`] to the IO variant of an error a lower layer already mapped.
#[must_use]
pub fn normalize_error(error: FsError) -> FsError {
    match error {
        FsError::Io { path, source } => FsError::from_platform_io(&path, source),
        other => other,
    }
}

/// A failure while staging names the staging sibling; report the deployment folder instead, so
/// `PermissionDenied` names the directory the caller must unlock.
fn normalize_deploy_error(error: FsError, _source: &Path, target: &Path) -> FsError {
    match normalize_error(error) {
        FsError::PermissionDenied { path } if path.parent() == target.parent() => {
            FsError::PermissionDenied {
                path: target
                    .parent()
                    .map_or_else(|| target.to_path_buf(), Path::to_path_buf),
            }
        }
        other => other,
    }
}

/// The nearest existing ancestor of `path` (or `path` itself).
fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut probe = path.to_path_buf();
    loop {
        if fs::metadata(&probe).is_ok() {
            return Some(probe);
        }
        if !probe.pop() {
            return None;
        }
    }
}
