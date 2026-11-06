//! Test-only fault injection for the write pipeline.
//!
//! The injection machinery — [`FailInjector`], [`TestInjector`], [`with_injector`]
//! and the armed-fault registry — is compiled only in tests or with the `faults` or
//! `testkit` cargo features, which no production dependency enables, so release builds
//! contain no failpoint state and [`hit`] folds to a no-op. [`FaultPoint`] stays compiled
//! so pipeline call sites share one spelling; it is inert data.
//!
//! A `Fail` fault is consumed when hit (fire-once), so a table row arms the exact
//! point it wants to break and later stages of the same `apply` run normally.

use std::path::Path;

/// A point in the write pipeline where a fault can be injected.
///
/// The names are stable: tests arm them by variant, and the pipeline calls
/// [`hit`] at each one. Without the `faults` or `testkit` feature [`hit`] returns
/// `Ok(())` immediately and these variants cost nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
#[non_exhaustive]
pub enum FaultPoint {
    /// Creating the staging temp file (`OpenOptions` in the destination directory).
    TempCreate,
    /// Writing the new bytes into the temp file.
    TempWrite,
    /// `fsync` of the temp file before it is installed.
    TempSync,
    /// Writing the backup blob inside the operation folder.
    BackupWrite,
    /// `fsync` of the backup blob.
    BackupSync,
    /// `fsync` of the destination directory after the rename.
    DirSync,
    /// Immediately before the destination is re-checked and the temp installed —
    /// the point an external writer races the pipeline.
    BeforeRename,
    /// The install step itself (`rename` for replace, `hard_link` for create).
    Rename,
    /// Creating the staged symlink of a link deployment.
    DeploySymlink,
    /// The install rename of a link or copy deployment.
    DeployRename,
    /// Writing one file of a staged copy tree.
    DeployCopyWrite,
}

/// What an injector does when its point is hit. Test and `faults`/`testkit` features only.
#[cfg(any(test, feature = "faults", feature = "testkit"))]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Fault {
    /// The operation fails with this raw OS error (`libc::ENOSPC`, `EACCES`, …).
    /// The pipeline must surface its typed [`crate::FsError`] and leave no debris.
    Fail(i32),
    /// An external writer lands `bytes` at the path the step touches, then the
    /// pipeline continues — the optimistic-concurrency race the re-check exists for.
    ExternalWrite(Vec<u8>),
}

/// The seam every fault source implements. Test and `faults`/`testkit` features only.
///
/// Called once per [`FaultPoint`] crossing; `Some` injects the fault, `None`
/// lets the real operation proceed. Implementations consume fire-once faults.
#[cfg(any(test, feature = "faults", feature = "testkit"))]
pub trait FailInjector: Send + Sync {
    /// The fault armed for `point`, if any. `path` is the file the step touches.
    fn fault(&self, point: FaultPoint, path: &Path) -> Option<Fault>;
}

/// A table-driven injector: `(point, fault)` pairs consumed on first hit.
/// Test and `faults`/`testkit` features only.
#[cfg(any(test, feature = "faults", feature = "testkit"))]
#[derive(Debug, Default)]
pub struct TestInjector {
    armed: std::sync::Mutex<Vec<(FaultPoint, Fault)>>,
}

#[cfg(any(test, feature = "faults", feature = "testkit"))]
impl TestInjector {
    /// An injector with nothing armed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Arms `fault` at `point`. Arms queue per point and are consumed in order.
    pub fn arm(&self, point: FaultPoint, fault: Fault) {
        let mut armed = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        armed.push((point, fault));
    }
}

#[cfg(any(test, feature = "faults", feature = "testkit"))]
impl FailInjector for TestInjector {
    fn fault(&self, point: FaultPoint, _path: &Path) -> Option<Fault> {
        let mut armed = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = armed.iter().position(|(p, _)| *p == point)?;
        Some(armed.remove(index).1)
    }
}

#[cfg(any(test, feature = "faults", feature = "testkit"))]
thread_local! {
    /// Armed injectors for this thread, oldest first. Tests arm and clear
    /// explicitly so parallel tests in one process never see each other's faults.
    static INJECTORS: std::cell::RefCell<Vec<std::sync::Arc<dyn FailInjector>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs `f` with `injector` armed for the current thread. Test and `faults`/`testkit` features only.
///
/// The injector is removed again when `f` returns or unwinds, so a panicking
/// test cannot leak faults into the next test on the same thread.
#[cfg(any(test, feature = "faults", feature = "testkit"))]
pub fn with_injector<R>(injector: std::sync::Arc<dyn FailInjector>, f: impl FnOnce() -> R) -> R {
    INJECTORS.with(|cell| cell.borrow_mut().push(injector));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    INJECTORS.with(|cell| {
        cell.borrow_mut().pop();
    });
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
/// A failure the injector asked the pipeline to surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum InjectedFailure {
    /// `io::Error::from_raw_os_error(code)` should be returned to the caller.
    Os(i32),
}

impl InjectedFailure {
    /// The `io::Error` the pipeline reports for this failure.
    #[must_use]
    pub(crate) fn into_io(self) -> std::io::Error {
        match self {
            Self::Os(code) => std::io::Error::from_raw_os_error(code),
        }
    }
}

/// Consults the failpoint registry at `point`.
///
/// With `faults`/`testkit` compiled in or in test builds, armed injectors are consulted in
/// order and the first fault wins: [`Fault::Fail`] becomes [`InjectedFailure::Os`], and
/// [`Fault::ExternalWrite`] performs a real `std::fs::write` — the injector
/// stands in for *another process*, exactly the writer the pipeline must not
/// overwrite.
///
/// Without `faults` this is an unconditional `Ok(())` and optimizes to nothing.
pub(crate) fn hit(point: FaultPoint, path: &Path) -> Result<(), InjectedFailure> {
    #[cfg(any(test, feature = "faults", feature = "testkit"))]
    {
        let fault = INJECTORS.with(|cell| {
            let injectors = cell.borrow();
            injectors
                .iter()
                .find_map(|injector| injector.fault(point, path))
        });
        match fault {
            Some(Fault::Fail(code)) => return Err(InjectedFailure::Os(code)),
            Some(Fault::ExternalWrite(bytes)) => {
                let _ = std::fs::write(path, &bytes);
            }
            None => {}
        }
    }
    #[cfg(not(any(test, feature = "faults", feature = "testkit")))]
    let _ = (point, path);
    Ok(())
}
