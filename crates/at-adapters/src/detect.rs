//! Bounded asynchronous version probes composed with pure adapter detection.

use std::path::{Path, PathBuf};
use std::time::Duration;

use at_model::{AgentId, ItemKind, Scope};
use tokio::process::Command;
use tokio::time::timeout;

use crate::adapter::{
    AgentAdapterRegistry, AgentDetectionWithVersion, AgentEnvironment, parse_probe_version,
};

/// Maximum combined stdout/stderr bytes retained from a version command.
pub const MAX_VERSION_OUTPUT_BYTES: usize = 16 * 1024;
/// Maximum time allowed for one version probe.
pub const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Executable and argument vector used to query an agent version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionProbe {
    /// Executable path; never parsed or run through a shell.
    pub executable: PathBuf,
    /// Explicit arguments passed to the executable.
    pub args: Vec<String>,
}

/// Result of detecting one adapter and optionally probing its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDetectionResult {
    /// Stable agent identity.
    pub agent: AgentId,
    /// Adapter detection combined with parsed version output.
    pub detection: AgentDetectionWithVersion,
    /// Executable used by the probe, if one was found.
    pub binary: Option<PathBuf>,
}

/// Finds the first executable matching `name` in an injected PATH value.
#[must_use]
pub fn find_executable(path_value: Option<&str>, name: &str) -> Option<PathBuf> {
    let paths = path_value?;
    std::env::split_paths(paths)
        .map(|directory| directory.join(name))
        .find(|candidate| std::fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file()))
}

/// Runs an executable with explicit arguments, bounded output, and a deadline.
///
/// A successful output may be empty or unparseable; the caller then reports an unknown version.
pub async fn probe_version(probe: &VersionProbe) -> (bool, Option<String>) {
    use tokio::io::AsyncReadExt;
    let Ok(mut child) = Command::new(&probe.executable)
        .args(&probe.args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    else {
        return (false, None);
    };
    let Some(mut stdout) = child.stdout.take() else {
        return (false, None);
    };
    let Some(mut stderr) = child.stderr.take() else {
        return (false, None);
    };
    let read_stdout = tokio::spawn(async move {
        let mut bytes = vec![0; MAX_VERSION_OUTPUT_BYTES / 2];
        let result = stdout.read(&mut bytes).await;
        (result, bytes)
    });
    let read_stderr = tokio::spawn(async move {
        let mut bytes = vec![0; MAX_VERSION_OUTPUT_BYTES / 2];
        let result = stderr.read(&mut bytes).await;
        (result, bytes)
    });
    let Ok(Ok(status)) = timeout(VERSION_PROBE_TIMEOUT, child.wait()).await else {
        return (false, None);
    };
    let Ok((Ok(stdout_len), mut stdout_bytes)) = read_stdout.await else {
        return (false, None);
    };
    let Ok((Ok(stderr_len), stderr_bytes)) = read_stderr.await else {
        return (false, None);
    };
    stdout_bytes.truncate(stdout_len);
    stdout_bytes.extend_from_slice(&stderr_bytes[..stderr_len]);
    (
        status.success(),
        parse_probe_version(&String::from_utf8_lossy(&stdout_bytes)),
    )
}

/// Detects one adapter from injected paths and adds the parsed probe version.
///
/// This stays independent of machine-global PATH and uses the adapter's established pure
/// detection semantics. Probe policy and database persistence are owned by the caller.
pub async fn detect_one(
    registry: &AgentAdapterRegistry,
    agent: &AgentId,
    environment: &AgentEnvironment,
    probe: Option<&VersionProbe>,
) -> Result<AgentDetectionResult, crate::adapter::AgentError> {
    let adapter = registry
        .get(agent)?
        .ok_or_else(|| crate::adapter::AgentError::Unsupported {
            agent: agent.clone(),
            kind: String::from("agent detection"),
        })?;
    let version = match probe {
        Some(probe) => probe_version(probe).await.1,
        None => None,
    };
    let mut presence = adapter.detect(environment);
    if probe.is_some() {
        presence.state = crate::adapter::DetectionState::Installed;
    }

    let binary = probe.map(|probe| probe.executable.clone());
    Ok(AgentDetectionResult {
        agent: agent.clone(),
        detection: AgentDetectionWithVersion { presence, version },
        binary,
    })
}
/// Builds the standard skill-root candidate for a detected adapter.
#[must_use]
pub fn primary_skill_root(
    registry: &AgentAdapterRegistry,
    agent: &AgentId,
    environment: &AgentEnvironment,
) -> Option<PathBuf> {
    registry
        .get(agent)
        .ok()
        .flatten()?
        .paths(ItemKind::Skill, Scope::User, environment)
        .into_iter()
        .find(|candidate| candidate.primary)
        .map(|candidate| candidate.path)
}

/// Whether a path is a regular executable file.
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_lookup_uses_only_the_injected_path() {
        let root = tempfile::tempdir().expect("temporary directory");
        let bin = root.path().join("agent-cli");
        std::fs::write(&bin, b"fixture").expect("write fixture executable");
        let path = std::env::join_paths([root.path()]).expect("join injected path");
        assert_eq!(find_executable(path.to_str(), "agent-cli"), Some(bin));
        assert_eq!(find_executable(Some(""), "agent-cli"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_uses_explicit_args_and_parses_output() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("temporary directory");
        let binary = root.path().join("version");
        std::fs::write(&binary, "#!/bin/sh\nprintf 'agent v2.3.4\\n'\n").expect("write script");
        let mut permissions = std::fs::metadata(&binary).expect("metadata").permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&binary, permissions).expect("set executable bit");
        assert_eq!(
            probe_version(&VersionProbe {
                executable: binary,
                args: vec![String::from("--version")],
            })
            .await
            .1
            .as_deref(),
            Some("2.3.4")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hung_version_probe_is_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("temporary directory");
        let binary = root.path().join("hang");
        std::fs::write(&binary, "#!/bin/sh\nsleep 5\n").expect("write script");
        let mut permissions = std::fs::metadata(&binary).expect("metadata").permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&binary, permissions).expect("set executable bit");
        let start = tokio::time::Instant::now();
        assert_eq!(
            probe_version(&VersionProbe {
                executable: binary,
                args: Vec::new()
            })
            .await,
            (false, None)
        );
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
