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

/// How many times a spawn is attempted while the kernel reports the executable as busy.
///
/// A script written moments ago can still be open for writing in another thread's `fork` child,
/// and Linux answers `execve` with `ETXTBSY` until that descriptor closes — which also happens to
/// an agent CLI that is being replaced while it is probed. Eight attempts at [`SPAWN_BACKOFF`]
/// cover such a leak for 140 ms, well inside [`VERSION_PROBE_TIMEOUT`], and a genuinely unusable
/// executable still fails with its own typed error.
const SPAWN_ATTEMPTS: usize = 8;

/// Wait between two spawns of an executable the kernel reports as busy.
const SPAWN_BACKOFF: Duration = Duration::from_millis(20);

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
        .find(|candidate| is_executable_file(candidate))
}

/// Runs an executable with explicit arguments, bounded output, and a deadline.
///
/// A successful output may be empty or unparseable; the caller then reports an unknown version.
pub async fn probe_version(probe: &VersionProbe) -> (bool, Option<String>) {
    use tokio::io::AsyncReadExt;
    let mut command = Command::new(&probe.executable);
    command
        .args(&probe.args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let Ok(mut child) = spawn_retrying(&mut command, SPAWN_ATTEMPTS).await else {
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

/// Spawns `command`, retrying while the kernel reports its executable as busy.
///
/// `command` is a builder, so a failed spawn leaves it reusable for the next attempt. Retries
/// happen before the caller's deadline starts and are bounded by `attempts`; the last error is
/// returned unchanged, so callers keep distinguishing permission, missing and other failures.
async fn spawn_retrying(
    command: &mut Command,
    attempts: usize,
) -> std::io::Result<tokio::process::Child> {
    retry_while_busy(attempts, || command.spawn()).await
}

/// Calls `try_spawn` up to `attempts` times while the kernel reports a busy executable.
///
/// The policy is split from the command so it stays testable on platforms whose `exec` never
/// answers `ETXTBSY`; every other error ends the loop on its first occurrence.
async fn retry_while_busy<T>(
    attempts: usize,
    mut try_spawn: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        match try_spawn() {
            Ok(value) => return Ok(value),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < attempts =>
            {
                tokio::time::sleep(SPAWN_BACKOFF).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
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
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
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
        #[cfg(unix)]
        assert_eq!(find_executable(path.to_str(), "agent-cli"), None);
        at_fs::testkit::write_executable(&bin, b"fixture").expect("make the fixture executable");
        assert_eq!(find_executable(path.to_str(), "agent-cli"), Some(bin));
        assert_eq!(find_executable(Some(""), "agent-cli"), None);
    }

    /// The standard probe for a fixture on the `--version` contract.
    #[cfg(unix)]
    fn probe(binary: &Path) -> VersionProbe {
        VersionProbe {
            executable: binary.to_path_buf(),
            args: vec![String::from("--version")],
        }
    }

    /// Opens `path` for writing and keeps the handle until the returned value is dropped.
    ///
    /// Linux refuses to `execve` a file that is open for writing, which is exactly the state a
    /// concurrent `fork` leaves behind when it inherits a write handle to a freshly written
    /// script.
    #[cfg(unix)]
    fn hold_open_for_writing(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open the fixture for writing")
    }

    #[cfg(unix)]
    fn probe_command(binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_uses_explicit_args_and_parses_output() {
        let root = tempfile::tempdir().expect("temporary directory");
        let binary = root.path().join("version");
        at_fs::testkit::write_executable(&binary, "#!/bin/sh\nprintf 'agent v2.3.4\\n'\n")
            .expect("write script");
        assert_eq!(
            probe_version(&probe(&binary)).await.1.as_deref(),
            Some("2.3.4")
        );
    }

    /// A script that another thread still has open for writing must not turn into a lost probe.
    ///
    /// Linux answers `execve` with `ETXTBSY` until the write handle closes, so the first attempt
    /// legitimately fails while the handle below is alive; the bounded retry covers the 100 ms
    /// release. macOS does not reject `exec` of an open file: there the same code path must
    /// simply succeed, which the shared assertions below also check.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_retries_while_another_thread_holds_the_script_open() {
        let root = tempfile::tempdir().expect("temporary directory");
        let binary = root.path().join("version");
        at_fs::testkit::write_executable(&binary, "#!/bin/sh\nprintf 'agent v7.0.0\\n'\n")
            .expect("write script");

        // The single attempt is the regression proof: without the retry this is the attempt the
        // probe would have made, and on Linux it fails with `ExecutableFileBusy`.
        let busy = hold_open_for_writing(&binary);
        match spawn_retrying(&mut probe_command(&binary), 1).await {
            Ok(mut child) => {
                let _ = child.wait().await;
                drop(busy);
                assert_eq!(
                    probe_version(&probe(&binary)).await.1.as_deref(),
                    Some("7.0.0"),
                    "this platform does not report a busy executable, so only success is asserted"
                );
                return;
            }
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ExecutableFileBusy),
        }

        let released = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(busy);
        });
        assert_eq!(
            probe_version(&probe(&binary)).await.1.as_deref(),
            Some("7.0.0"),
            "the retry must outlive the write handle the other thread holds"
        );
        released.join().expect("release thread");
    }

    #[tokio::test]
    async fn the_busy_retry_recovers_within_its_bound() {
        // Three busy attempts then a success is what a write handle held by another thread
        // produces for a moment; the fourth attempt must be the one that spawns.
        let mut calls = 0_u32;
        let result = retry_while_busy(SPAWN_ATTEMPTS, || {
            calls += 1;
            if calls <= 3 {
                Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
            } else {
                Ok(())
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "the retry must outlast a transient busy executable"
        );
        assert_eq!(calls, 4);

        // The single attempt the probe made before the retry existed: the busy window is fatal.
        let mut calls = 0_u32;
        let error = retry_while_busy::<()>(1, || {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ExecutableFileBusy);
        assert_eq!(calls, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_errors_other_than_busy_are_returned_typed() {
        let root = tempfile::tempdir().expect("temporary directory");
        let missing = root.path().join("missing");
        let error = spawn_retrying(&mut probe_command(&missing), SPAWN_ATTEMPTS)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hung_version_probe_is_bounded() {
        let root = tempfile::tempdir().expect("temporary directory");
        let binary = root.path().join("hang");
        // The child hangs far longer than any sane probe deadline, so a generous wall-clock
        // bound still proves the probe timed out instead of waiting for the process.
        at_fs::testkit::write_executable(&binary, "#!/bin/sh\nsleep 60\n").expect("write script");
        let start = tokio::time::Instant::now();
        assert_eq!(
            probe_version(&VersionProbe {
                executable: binary,
                args: Vec::new()
            })
            .await,
            (false, None)
        );
        assert!(start.elapsed() < Duration::from_secs(20));
    }
}
