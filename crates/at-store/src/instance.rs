//! Single-instance advisory lock and CLI coexistence.
//!
//! One advisory lock guards the shared store and SQLite index. The lock file is
//! `<support>/instance.lock` and holds a small JSON record:
//!
//! ```json
//! { "pid": 4711, "version": "0.1.0", "started_at": "2026-11-05T12:00:00Z", "mode": "app" }
//! ```
//!
//! Locking uses `flock` through [`fs4`]: exclusive for writes, shared for reads. The record
//! exists so a second process can tell *who* holds the lock (pid, version, mode) and so a
//! second app launch can focus the running window instead of opening another one.
//!
//! ## What each mode does
//!
//! | Mode | OS lock | Writes the record |
//! | --- | --- | --- |
//! | [`Mode::App`] | exclusive, held for startup | yes |
//! | [`Mode::CliWrite`] | exclusive, per write transaction | only when no live app record exists |
//! | [`Mode::CliRead`] | shared | never |
//! | [`Mode::ReadOnly`] | shared, and the file is not created | never |
//!
//! The app does **not** hold the lock while it sits idle: holding it for the whole lifetime
//! would block every CLI write. Instead the app writes its record at startup, releases the OS
//! lock and keeps the record as its single-instance marker; a second app launch sees the live
//! record and exits after activating the first. CLI writes take the exclusive lock for one
//! transaction and leave a live app record untouched, so the app stays detectable.
//!
//! ## Staleness
//!
//! A record is stale when its pid is dead, or when the pid now belongs to a process that
//! started *after* the record was written (pid reuse; see [`ProcessState`]). Stale records are
//! replaced. A record that cannot be validated because the process probe failed is treated as
//! live: the lock is advisory and stealing it is worse than asking the user to retry.
//! A corrupt file is quarantined as `instance.lock.corrupt-<unix-seconds>` and never crashes
//! startup.
//!
//! ## Read-only
//!
//! [`Mode::ReadOnly`] never creates the lock file, never writes a record and reports every
//! write attempt as [`LockError::ReadOnly`]. Callers must additionally open SQLite read-only
//! with `PRAGMA query_only=1` and skip migrations; the lock module cannot enforce that itself.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// File name of the lock inside the support directory.
pub const LOCK_FILE_NAME: &str = "instance.lock";

/// File name of the activation socket the running app listens on.
pub const SOCKET_FILE_NAME: &str = "instance.sock";

/// Prefix of a quarantined corrupt lock file.
pub const QUARANTINE_PREFIX: &str = "instance.lock.corrupt-";

/// Version recorded in the lock; every workspace crate shares the package version.
pub const LOCK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How much later than the record a process may have started and still be the same process.
///
/// The record is written after the process started, so the process start must never be later
/// than the record time; the tolerance absorbs clock and `ps` rounding.
const PID_REUSE_TOLERANCE: Duration = Duration::from_secs(2);

/// How long [`InstanceLock::read_record`] retries a partially written record.
const READ_ATTEMPTS: u8 = 3;

/// Pause between record read attempts.
const READ_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Who is asking for the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Mode {
    /// The desktop app: exclusive while it starts, then the record stays as its marker.
    App,
    /// A CLI command that only reads; shared lock.
    CliRead,
    /// A CLI command that writes; exclusive lock for one transaction.
    CliWrite,
    /// `--read-only`: shared lock, no file creation, no record, writes rejected.
    ReadOnly,
}

impl Mode {
    /// Stable identifier written to the record and JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::CliRead => "cli_read",
            Self::CliWrite => "cli_write",
            Self::ReadOnly => "read_only",
        }
    }

    /// Whether this mode takes a shared lock.
    #[must_use]
    pub const fn is_shared(self) -> bool {
        matches!(self, Self::CliRead | Self::ReadOnly)
    }

    /// Whether this mode may write at all.
    #[must_use]
    pub const fn allows_writes(self) -> bool {
        matches!(self, Self::App | Self::CliWrite)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The JSON content of the lock file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRecord {
    /// Process id that wrote the record.
    pub pid: u32,
    /// Version of the binary that wrote it.
    pub version: String,
    /// RFC 3339 time the record was written.
    pub started_at: String,
    /// What the process was doing when it took the lock.
    pub mode: Mode,
}

impl InstanceRecord {
    /// A record for the current process and the given mode.
    #[must_use]
    pub fn for_current_process(mode: Mode) -> Self {
        Self {
            pid: std::process::id(),
            version: LOCK_VERSION.to_owned(),
            started_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            mode,
        }
    }

    /// The record time parsed back into a timestamp.
    #[must_use]
    pub fn started_at(&self) -> Option<OffsetDateTime> {
        OffsetDateTime::parse(&self.started_at, &Rfc3339).ok()
    }
}

/// What reading the lock file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordState {
    /// No lock file exists.
    Missing,
    /// The file exists but is not a valid record.
    Invalid {
        /// Why it could not be parsed.
        reason: String,
    },
    /// A well-formed record.
    Valid(InstanceRecord),
}

/// Best-effort answer to "is this pid still the process that wrote the record?".
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProcessState {
    /// The pid exists; the start time is present when the probe could determine it.
    Alive {
        /// Process start time, when known.
        started_at: Option<OffsetDateTime>,
    },
    /// The pid does not exist.
    Dead,
    /// The probe could not determine anything (for example `ps` is unavailable).
    Unknown,
}

/// Injected process inspection, so lock tests never depend on the developer's process table.
pub trait ProcessProbe: Send + Sync {
    /// Inspects one pid.
    fn inspect(&self, pid: u32) -> ProcessState;
}

/// The production probe: `ps -p <pid> -o etime=` (macOS and Linux).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProbe;

impl ProcessProbe for SystemProbe {
    fn inspect(&self, pid: u32) -> ProcessState {
        if pid == 0 || pid == u32::MAX {
            return ProcessState::Dead;
        }
        let output = match Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "etime="])
            .output()
        {
            Ok(output) => output,
            Err(_) => return ProcessState::Unknown,
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() || stdout.trim().is_empty() {
            return ProcessState::Dead;
        }
        match parse_elapsed_time(&stdout) {
            Some(elapsed) => ProcessState::Alive {
                started_at: Some(OffsetDateTime::now_utc() - elapsed),
            },
            None => ProcessState::Alive { started_at: None },
        }
    }
}

/// How long a caller waits for the lock before giving up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockPolicy {
    /// Total time to keep retrying a blocking lock.
    pub wait: Duration,
    /// Pause between attempts.
    pub poll: Duration,
}

impl LockPolicy {
    /// Try once and report [`LockError::Busy`] immediately.
    pub const NONBLOCKING: Self = Self {
        wait: Duration::ZERO,
        poll: Duration::from_millis(10),
    };

    /// The CLI transaction policy: five seconds, matching the SQLite busy timeout.
    pub const CLI_TRANSACTION: Self = Self {
        wait: Duration::from_secs(5),
        poll: Duration::from_millis(50),
    };

    /// A policy with the given total wait.
    #[must_use]
    pub const fn waiting(wait: Duration) -> Self {
        Self {
            wait,
            poll: Duration::from_millis(50),
        }
    }
}

/// Why the lock could not be taken.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LockError {
    /// Another process held the lock for the whole policy window.
    Busy {
        /// Record of the holder when the file names one.
        holder: Option<InstanceRecord>,
    },
    /// A second app launch found a live instance and must not open a window.
    AlreadyRunning {
        /// Record of the running instance.
        holder: InstanceRecord,
    },
    /// The caller asked for a write through a read-only guard.
    ReadOnly,
    /// A filesystem operation failed.
    Io {
        /// Path the operation targeted.
        path: PathBuf,
        /// OS error text.
        message: String,
    },
}

impl fmt::Display for LockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy {
                holder: Some(holder),
            } => write!(
                formatter,
                "another AgenTuna process holds the lock (pid {}, version {}, mode {})",
                holder.pid, holder.version, holder.mode
            ),
            Self::Busy { holder: None } => {
                formatter.write_str("another AgenTuna process holds the lock")
            }
            Self::AlreadyRunning { holder } => write!(
                formatter,
                "another AgenTuna instance is running (pid {}, version {})",
                holder.pid, holder.version
            ),
            Self::ReadOnly => formatter.write_str("read-only mode: writes are disabled"),
            Self::Io { path, message } => write!(formatter, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for LockError {}

/// Everything known about the current lock, for diagnostics and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LockStatus {
    /// No record exists.
    Free,
    /// A record exists; `alive` says whether its process still runs.
    Recorded {
        /// The record.
        record: InstanceRecord,
        /// Whether the recorded process is alive and was not replaced by a pid-reuse.
        alive: bool,
    },
    /// The file exists but is not a valid record.
    Corrupt {
        /// Parse error.
        reason: String,
        /// Path of the quarantined copy, when one was made.
        quarantined: Option<PathBuf>,
    },
}

/// The lock for one support directory. Cheap to clone; every operation reopens the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceLock {
    support_dir: PathBuf,
}

impl InstanceLock {
    /// A lock rooted at `support_dir`.
    ///
    /// There is no `Default`: a lock relative to the working directory would silently guard
    /// the wrong store.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new(support_dir: impl Into<PathBuf>) -> Self {
        Self {
            support_dir: support_dir.into(),
        }
    }

    /// The support directory the lock lives in.
    #[must_use]
    pub fn support_dir(&self) -> &Path {
        &self.support_dir
    }

    /// `<support>/instance.lock`.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.support_dir.join(LOCK_FILE_NAME)
    }

    /// `<support>/instance.sock`, the app's activation channel.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.support_dir.join(SOCKET_FILE_NAME)
    }

    /// Reads the record once, without waiting for a partially written file.
    #[must_use]
    pub fn read_record(&self) -> RecordState {
        read_record_at(&self.lock_path(), 1)
    }

    /// Reads the record, retrying briefly so a concurrent writer is not mistaken for corruption.
    #[must_use]
    pub fn read_record_settled(&self) -> RecordState {
        read_record_at(&self.lock_path(), READ_ATTEMPTS)
    }

    /// Reports the lock state without taking the lock.
    #[must_use]
    pub fn status(&self, probe: &dyn ProcessProbe) -> LockStatus {
        match self.read_record_settled() {
            RecordState::Missing => LockStatus::Free,
            RecordState::Invalid { reason } => LockStatus::Corrupt {
                reason,
                quarantined: None,
            },
            RecordState::Valid(record) => LockStatus::Recorded {
                alive: record_is_live(&record, probe),
                record,
            },
        }
    }

    /// Takes the lock without waiting.
    ///
    /// # Errors
    /// [`LockError::Busy`], [`LockError::AlreadyRunning`] or [`LockError::Io`].
    pub fn try_acquire(&self, mode: Mode) -> Result<InstanceGuard, LockError> {
        self.acquire_with(mode, LockPolicy::NONBLOCKING, &SystemProbe)
    }

    /// Takes the lock, retrying for the policy's wait window.
    ///
    /// # Errors
    /// [`LockError::Busy`] when the window expires, [`LockError::AlreadyRunning`] for a second
    /// app launch, or [`LockError::Io`].
    pub fn acquire(&self, mode: Mode, policy: LockPolicy) -> Result<InstanceGuard, LockError> {
        self.acquire_with(mode, policy, &SystemProbe)
    }

    /// Takes the lock with an injected process probe.
    ///
    /// # Errors
    /// Same as [`Self::acquire`].
    pub fn acquire_with(
        &self,
        mode: Mode,
        policy: LockPolicy,
        probe: &dyn ProcessProbe,
    ) -> Result<InstanceGuard, LockError> {
        self.acquire_attempt(mode, policy, probe, READ_ATTEMPTS)
    }

    fn acquire_attempt(
        &self,
        mode: Mode,
        policy: LockPolicy,
        probe: &dyn ProcessProbe,
        attempts_left: u8,
    ) -> Result<InstanceGuard, LockError> {
        let path = self.lock_path();
        if mode.is_shared() && !path.exists() {
            // Nothing holds a lock and this mode must not create the file or its directory.
            return Ok(InstanceGuard::unlocked(self.support_dir.clone(), mode));
        }
        if !mode.is_shared() {
            fs::create_dir_all(&self.support_dir)
                .map_err(|error| io_error(&self.support_dir, error))?;
        }
        // Readers only need read access: a read-only support directory still permits
        // shared locks, and this mode never creates or writes the file.
        let mut file = if mode.is_shared() {
            OpenOptions::new().read(true).open(&path)
        } else {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
        }
        .map_err(|error| io_error(&path, error))?;

        let deadline = Instant::now() + policy.wait;
        loop {
            let attempt = if mode.is_shared() {
                FileExt::try_lock_shared(&file)
            } else {
                FileExt::try_lock(&file)
            };
            match attempt {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        let holder = match read_record_at(&path, READ_ATTEMPTS) {
                            RecordState::Valid(record) => Some(record),
                            _ => None,
                        };
                        return Err(LockError::Busy { holder });
                    }
                    sleep(policy.poll);
                }
                Err(TryLockError::Error(error)) => return Err(io_error(&path, error)),
            }
        }

        if mode.is_shared() {
            let record = match read_record_at(&path, READ_ATTEMPTS) {
                RecordState::Valid(record) => record,
                _ => InstanceRecord::for_current_process(mode),
            };
            return Ok(InstanceGuard {
                dir: self.support_dir.clone(),
                file: Some(file),
                record,
                mode,
                owns_record: false,
            });
        }

        let state = read_record_at(&path, READ_ATTEMPTS);
        match state {
            RecordState::Valid(record) if record_is_live(&record, probe) => {
                match mode {
                    Mode::App => {
                        let _ = FileExt::unlock(&file);
                        Err(LockError::AlreadyRunning { holder: record })
                    }
                    Mode::CliWrite if record.mode == Mode::App => {
                        // The app is running but idle; a write transaction may proceed without
                        // taking over its record.
                        Ok(InstanceGuard {
                            dir: self.support_dir.clone(),
                            file: Some(file),
                            record,
                            mode,
                            owns_record: false,
                        })
                    }
                    _ => {
                        let ours = InstanceRecord::for_current_process(mode);
                        write_record(&mut file, &path, &ours)?;
                        Ok(InstanceGuard {
                            dir: self.support_dir.clone(),
                            file: Some(file),
                            record: ours,
                            mode,
                            owns_record: true,
                        })
                    }
                }
            }
            RecordState::Invalid { reason } => {
                if attempts_left == 0 {
                    tracing::warn!(
                        path = %path.display(),
                        %reason,
                        "lock file is corrupt and could not be quarantined; replacing it in place"
                    );
                    let ours = InstanceRecord::for_current_process(mode);
                    write_record(&mut file, &path, &ours)?;
                    return Ok(InstanceGuard {
                        dir: self.support_dir.clone(),
                        file: Some(file),
                        record: ours,
                        mode,
                        owns_record: true,
                    });
                }
                tracing::warn!(
                    path = %path.display(),
                    %reason,
                    "quarantining the corrupt instance lock"
                );
                if let Some(quarantined) = quarantine(&self.support_dir, &path) {
                    tracing::warn!(path = %quarantined.display(), "corrupt lock quarantined");
                }
                // The lock above is on the old inode; start over with a fresh file.
                drop(file);
                self.acquire_attempt(mode, policy, probe, attempts_left - 1)
            }
            RecordState::Missing | RecordState::Valid(_) => {
                let ours = InstanceRecord::for_current_process(mode);
                write_record(&mut file, &path, &ours)?;
                Ok(InstanceGuard {
                    dir: self.support_dir.clone(),
                    file: Some(file),
                    record: ours,
                    mode,
                    owns_record: true,
                })
            }
        }
    }

    /// Removes the record when it still belongs to `record`.
    ///
    /// Used on clean exit. Never deletes another instance's lock: the file is re-read and both
    /// pid and `started_at` must match before it is removed.
    pub fn release_record(&self, record: &InstanceRecord) -> bool {
        let path = self.lock_path();
        match read_record_at(&path, 1) {
            RecordState::Valid(current)
                if current.pid == record.pid && current.started_at == record.started_at =>
            {
                match fs::remove_file(&path) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            %error,
                            "could not remove this instance's lock record"
                        );
                        false
                    }
                }
            }
            _ => false,
        }
    }
}

/// A held lock. Dropping it unlocks the file and removes the record only if this guard wrote it.
pub struct InstanceGuard {
    dir: PathBuf,
    file: Option<File>,
    record: InstanceRecord,
    mode: Mode,
    owns_record: bool,
}

impl InstanceGuard {
    fn unlocked(dir: PathBuf, mode: Mode) -> Self {
        Self {
            dir,
            file: None,
            record: InstanceRecord::for_current_process(mode),
            mode,
            owns_record: false,
        }
    }

    /// The record this guard read or wrote.
    #[must_use]
    pub fn record(&self) -> &InstanceRecord {
        &self.record
    }

    /// The mode the guard was taken with.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the guard holds an OS lock at all (false for a lock-free read-only guard).
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.file.is_some()
    }

    /// Path of the lock file.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.dir.join(LOCK_FILE_NAME)
    }

    /// Rejects writes for read-only and read modes.
    ///
    /// # Errors
    /// [`LockError::ReadOnly`] for [`Mode::ReadOnly`] and [`Mode::CliRead`].
    pub fn ensure_writable(&self) -> Result<(), LockError> {
        if self.mode().allows_writes() {
            Ok(())
        } else {
            Err(LockError::ReadOnly)
        }
    }

    /// Releases the OS lock and the record, as dropping would.
    pub fn release(mut self) {
        self.release_inner();
    }

    /// Releases the OS lock but leaves the record behind as this process's marker.
    pub fn release_keeping_record(mut self) {
        self.owns_record = false;
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
            drop(file);
        }
        if self.owns_record {
            self.owns_record = false;
            let path = self.dir.join(LOCK_FILE_NAME);
            if let RecordState::Valid(current) = read_record_at(&path, 1)
                && current.pid == self.record.pid
                && current.started_at == self.record.started_at
                && let Err(error) = fs::remove_file(&path)
            {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "could not remove this instance's lock record"
                );
            }
        }
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        self.release_inner();
    }
}

impl fmt::Debug for InstanceGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstanceGuard")
            .field("record", &self.record)
            .field("mode", &self.mode)
            .field("owns_record", &self.owns_record)
            .field("locked", &self.file.is_some())
            .finish()
    }
}

/// Reads the record at `path`, retrying `attempts` times when the file is mid-write.
fn read_record_at(path: &Path, attempts: u8) -> RecordState {
    let mut last = RecordState::Missing;
    for attempt in 0..attempts.max(1) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return RecordState::Missing,
            Err(error) => {
                return RecordState::Invalid {
                    reason: error.to_string(),
                };
            }
        };
        // A freshly created (or just-truncated) lock file is empty, not corrupt. Quarantining it
        // would move a healthy new file aside on every start after a real quarantine.
        if text.trim().is_empty() {
            return RecordState::Missing;
        }
        match serde_json::from_str::<InstanceRecord>(text.trim()) {
            Ok(record) => return RecordState::Valid(record),
            Err(error) => {
                last = RecordState::Invalid {
                    reason: error.to_string(),
                };
                if attempt + 1 < attempts {
                    sleep(READ_RETRY_DELAY);
                }
            }
        }
    }
    last
}

/// Writes `record` into the locked file in place, so the lock keeps guarding the same inode.
fn write_record(file: &mut File, path: &Path, record: &InstanceRecord) -> Result<(), LockError> {
    let json = serde_json::to_string(record).map_err(|error| LockError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    file.set_len(0).map_err(|error| io_error(path, error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_error(path, error))?;
    file.write_all(json.as_bytes())
        .map_err(|error| io_error(path, error))?;
    file.write_all(b"\n")
        .map_err(|error| io_error(path, error))?;
    file.flush().map_err(|error| io_error(path, error))?;
    file.sync_data().map_err(|error| io_error(path, error))?;
    Ok(())
}

/// Moves the corrupt file aside. Returns the quarantined path when the rename succeeded.
fn quarantine(support_dir: &Path, path: &Path) -> Option<PathBuf> {
    let stamp = OffsetDateTime::now_utc().unix_timestamp();
    let target = support_dir.join(format!("{QUARANTINE_PREFIX}{stamp}"));
    match fs::rename(path, &target) {
        Ok(()) => Some(target),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "could not quarantine the corrupt instance lock"
            );
            None
        }
    }
}

/// Whether the recorded process is still the one that wrote the record.
fn record_is_live(record: &InstanceRecord, probe: &dyn ProcessProbe) -> bool {
    match probe.inspect(record.pid) {
        ProcessState::Alive { started_at } => match (started_at, record.started_at()) {
            (Some(process_start), Some(recorded)) => {
                process_start <= recorded + PID_REUSE_TOLERANCE
            }
            _ => true,
        },
        ProcessState::Dead => false,
        ProcessState::Unknown => true,
    }
}

/// Parses `ps -o etime=` output: `[[dd-]hh:]mm:ss`.
fn parse_elapsed_time(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (days, rest) = match raw.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, raw),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
        ),
        [minutes, seconds] => (
            0,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
        ),
        _ => return None,
    };
    Some(Duration::from_secs(
        days * 86_400 + hours * 3_600 + minutes * 60 + seconds,
    ))
}

/// Wraps an OS error with the path it targeted.
fn io_error(path: &Path, error: io::Error) -> LockError {
    LockError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// A probe whose answer every test controls.
#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct FixedProbe {
    states: std::collections::BTreeMap<u32, ProcessState>,
    unknown: bool,
}

#[cfg(test)]
impl FixedProbe {
    /// A probe that reports exactly `pid` as alive with the given start time.
    pub fn alive(pid: u32, started_at: Option<OffsetDateTime>) -> Self {
        let mut states = std::collections::BTreeMap::new();
        states.insert(pid, ProcessState::Alive { started_at });
        Self {
            states,
            unknown: false,
        }
    }

    /// A probe that reports exactly `pid` as dead.
    pub fn dead(pid: u32) -> Self {
        let mut states = std::collections::BTreeMap::new();
        states.insert(pid, ProcessState::Dead);
        Self {
            states,
            unknown: false,
        }
    }

    /// A probe that answers [`ProcessState::Unknown`] for every pid.
    pub fn unknown() -> Self {
        Self {
            states: std::collections::BTreeMap::new(),
            unknown: true,
        }
    }

    /// Adds another pid to the probe's table.
    pub fn with(mut self, pid: u32, state: ProcessState) -> Self {
        self.states.insert(pid, state);
        self
    }
}

#[cfg(test)]
impl ProcessProbe for FixedProbe {
    fn inspect(&self, pid: u32) -> ProcessState {
        self.states.get(&pid).cloned().unwrap_or(if self.unknown {
            ProcessState::Unknown
        } else {
            ProcessState::Dead
        })
    }
}

/// Reads the activation message from a connected client.
pub fn read_activation_message(stream: &mut impl Read) -> io::Result<String> {
    let mut buffer = [0_u8; 64];
    let read = stream.read(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..read]).trim().to_owned())
}

/// The message a second instance sends to the running app.
pub const ACTIVATION_MESSAGE: &str = "activate";

#[cfg(test)]
mod tests;
