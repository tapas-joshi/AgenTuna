//! Spawning an executable the caller may have written a moment earlier.
//!
//! Linux refuses to `execve` a file that is still open for writing and answers `ETXTBSY`
//! ([`std::io::ErrorKind::ExecutableFileBusy`]). Ordinary situations produce that state for this
//! workspace: a server command or login shell that was just written, a CLI that is being replaced
//! while it is probed, and a descriptor that an unrelated thread's `fork` inherited while the
//! write handle was still open. All of them clear on their own, so every spawn goes through
//! [`spawn_retrying`] and retries a bounded number of times before reporting the typed error.
//!
//! The retry is the only behaviour here: executable resolution, consent and timeouts belong to
//! the caller. On platforms whose `exec` never answers `ETXTBSY` the first attempt succeeds and
//! no retry happens.

use std::process::Child;
use std::time::Duration;

use tokio::process::{Child as AsyncChild, Command as AsyncCommand};

/// How many times one spawn is attempted while the kernel reports the executable as busy.
const SPAWN_ATTEMPTS: usize = 8;

/// Wait between two attempts of the same spawn.
const SPAWN_BACKOFF: Duration = Duration::from_millis(20);

/// Spawns a prepared `command`, retrying while the kernel reports its executable as busy.
///
/// The builder stays reusable across attempts, so one failed `spawn` does not consume `command`.
/// A busy executable is retried up to 8 times, 20 ms apart (140 ms in total): long enough for a
/// write handle held by another thread to close, short enough not to delay a genuinely broken
/// installation. Every other error, and a busy executable still busy after the last attempt, is
/// returned unchanged, so callers keep their own error mapping.
///
/// # Errors
/// The [`std::io::Error`] from the last spawn attempt, for example
/// [`std::io::ErrorKind::NotFound`] when the executable does not exist or
/// [`std::io::ErrorKind::PermissionDenied`] when it is not executable.
pub fn spawn_retrying(command: &mut std::process::Command) -> std::io::Result<Child> {
    retry_while_busy(|| command.spawn())
}

/// Spawns an asynchronous `command`, retrying while the kernel reports its executable as busy.
///
/// Same policy as [`spawn_retrying`]; the backoff sleeps on the runtime instead of the thread.
pub(crate) async fn spawn_retrying_async(
    command: &mut AsyncCommand,
) -> std::io::Result<AsyncChild> {
    retry_while_busy_async(|| command.spawn()).await
}

/// Calls `try_spawn` up to [`SPAWN_ATTEMPTS`] times while the executable is reported busy.
///
/// Split from the command so the policy is testable on any platform: only a busy executable is
/// retried, and only until the attempt bound.
fn retry_while_busy<T>(mut try_spawn: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        match try_spawn() {
            Ok(value) => return Ok(value),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt < SPAWN_ATTEMPTS =>
            {
                std::thread::sleep(SPAWN_BACKOFF);
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// [`retry_while_busy`] with the backoff on the async runtime.
async fn retry_while_busy_async<T>(
    mut try_spawn: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        match try_spawn() {
            Ok(value) => return Ok(value),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt < SPAWN_ATTEMPTS =>
            {
                tokio::time::sleep(SPAWN_BACKOFF).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_executable_is_retried_until_it_spawns() {
        let mut calls = 0_u32;
        let result = retry_while_busy(|| {
            calls += 1;
            if calls <= 3 {
                Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result.expect("the fourth attempt spawns"), 4);
    }

    #[test]
    fn a_busy_executable_that_never_clears_reports_the_error_at_the_bound() {
        let mut calls = 0_u32;
        let error = retry_while_busy::<()>(|| {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ExecutableFileBusy);
        assert_eq!(calls as usize, SPAWN_ATTEMPTS);
    }

    #[test]
    fn other_spawn_errors_are_not_retried() {
        let mut calls = 0_u32;
        let error = retry_while_busy::<()>(|| {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn the_async_retry_follows_the_same_policy() {
        let mut calls = 0_u32;
        let result = retry_while_busy_async(|| {
            calls += 1;
            if calls <= 2 {
                Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
            } else {
                Ok(calls)
            }
        })
        .await;
        assert_eq!(result.expect("the third attempt spawns"), 3);
    }
}
