//! Single-instance startup gate and cross-process window activation.
//!
//! [`begin`] is the first thing the desktop binary does. It takes the app mode of the advisory
//! lock from [`at_store::instance`]:
//!
//! - **free or stale** — this process becomes the primary instance, writes
//!   `<support>/instance.lock` and keeps that record while it runs;
//! - **a live app record exists** — [`Startup::AlreadyRunning`]; the caller focuses the running
//!   window through [`activate_existing`] and exits with status 0 without creating a window.
//!
//! The app does not hold the OS lock while it sits idle: the record is the single-instance
//! marker, so CLI writes are never blocked by a running-but-idle GUI. The lock itself is held
//! during startup and each write transaction (see the `at_store::instance` module docs).
//!
//! Activation uses a Unix-domain socket at `<support>/instance.sock`. The running app accepts
//! `activate` on it and calls its focus callback; the second process sends the message and
//! exits. This is the Tauri equivalent of `NSApplication activate` and needs no Apple Events
//! automation permission. On platforms without `std::os::unix` the listener reports
//! [`io::ErrorKind::Unsupported`] and the second instance still exits without opening a window,
//! it just cannot raise the first one.

use std::io;
use std::sync::Mutex;

use at_store::instance::{InstanceLock, InstanceRecord, LockError, Mode};
use at_store::paths::AppPaths;

/// The decision taken at startup.
#[derive(Debug)]
#[non_exhaustive]
pub enum Startup {
    /// This process may open its window and is responsible for the record.
    Primary(Box<PrimaryInstance>),
    /// Another live instance owns the record; do not open a window.
    AlreadyRunning {
        /// The running instance's record, when the file could be read.
        holder: Option<InstanceRecord>,
    },
}

/// The record this process is responsible for until shutdown.
#[derive(Debug, Clone)]
pub struct PrimaryInstance {
    paths: AppPaths,
    record: Option<InstanceRecord>,
}

impl PrimaryInstance {
    /// The record on disk, or `None` when the lock could not be used and protection is degraded.
    #[must_use]
    pub fn record(&self) -> Option<&InstanceRecord> {
        self.record.as_ref()
    }

    /// Moves the record into the managed state that outlives setup.
    #[must_use]
    pub fn into_state(self) -> InstanceState {
        InstanceState {
            paths: self.paths,
            record: self.record,
            server: Mutex::new(None),
        }
    }
}

/// Takes the app lock without waiting.
///
/// A lock I/O failure degrades to "run anyway": a broken support directory must not brick the
/// app, and the log records the reduced protection.
#[must_use]
pub fn begin(paths: &AppPaths) -> Startup {
    let lock = InstanceLock::new(paths.support_dir());
    match lock.try_acquire(Mode::App) {
        Ok(guard) => {
            let record = guard.record().clone();
            guard.release_keeping_record();
            tracing::info!(
                pid = record.pid,
                version = %record.version,
                "this process owns the single-instance record"
            );
            Startup::Primary(Box::new(PrimaryInstance {
                paths: paths.clone(),
                record: Some(record),
            }))
        }
        Err(
            LockError::AlreadyRunning { holder }
            | LockError::Busy {
                holder: Some(holder),
            },
        ) => Startup::AlreadyRunning {
            holder: Some(holder),
        },
        Err(error @ LockError::Busy { holder: None }) => {
            tracing::info!(%error, "the instance lock is held without a readable record");
            Startup::AlreadyRunning { holder: None }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "instance lock unavailable; continuing without single-instance protection"
            );
            Startup::Primary(Box::new(PrimaryInstance {
                paths: paths.clone(),
                record: None,
            }))
        }
    }
}

/// Asks the running instance to focus its window. Returns whether the message was delivered.
#[must_use]
pub fn activate_existing(paths: &AppPaths) -> bool {
    let socket = InstanceLock::new(paths.support_dir()).socket_path();
    let delivered = activation::notify(&socket);
    tracing::info!(
        socket = %socket.display(),
        delivered,
        "asked the running instance to activate"
    );
    delivered
}

/// Managed state that owns this process's instance record and activation listener.
///
/// Stored in [`super::state::AppState`], so it lives until the app exits; [`Self::shutdown`]
/// stops the listener and removes the record, and only a record that still matches this
/// process is removed.
#[derive(Debug)]
pub struct InstanceState {
    paths: AppPaths,
    record: Option<InstanceRecord>,
    server: Mutex<Option<activation::ActivationServer>>,
}

impl InstanceState {
    /// Starts the activation listener. Idempotent: a second call leaves the first listener in
    /// place.
    ///
    /// # Errors
    /// The socket could not be bound; the app keeps running without activation support.
    pub fn start_activation<F>(&self, on_activate: F) -> io::Result<()>
    where
        F: Fn() + Send + 'static,
    {
        let mut slot = match self.server.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot.is_some() {
            return Ok(());
        }
        let path = InstanceLock::new(self.paths.support_dir()).socket_path();
        *slot = Some(activation::ActivationServer::start(path, on_activate)?);
        Ok(())
    }

    /// The record this process wrote, when single-instance protection is active.
    #[must_use]
    pub fn record(&self) -> Option<&InstanceRecord> {
        self.record.as_ref()
    }

    /// Stops the listener and removes the record. Safe to call more than once.
    pub fn shutdown(&self) {
        if let Ok(mut slot) = self.server.lock()
            && let Some(server) = slot.take()
        {
            drop(server);
        }
        if let Some(record) = &self.record {
            let removed = InstanceLock::new(self.paths.support_dir()).release_record(record);
            tracing::info!(removed, "released the single-instance record");
        }
    }
}

impl Drop for InstanceState {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(unix)]
mod activation {
    use std::io::{self, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use at_store::instance::{ACTIVATION_MESSAGE, read_activation_message};

    /// Poll interval of the non-blocking accept loop.
    const ACCEPT_POLL: Duration = Duration::from_millis(25);

    /// The listener a primary instance runs for its whole lifetime.
    #[derive(Debug)]
    pub struct ActivationServer {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl ActivationServer {
        pub fn start(path: PathBuf, on_activate: impl Fn() + Send + 'static) -> io::Result<Self> {
            if path.exists() {
                match UnixStream::connect(&path) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            format!(
                                "{}: another activation listener is already running",
                                path.display()
                            ),
                        ));
                    }
                    Err(_) => {
                        // A dead process left the socket behind; replace it.
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
            let listener = UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop);
            let handle = thread::Builder::new()
                .name("agentuna-activation".to_owned())
                .spawn(move || {
                    while !stop_flag.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((mut stream, _)) => match read_activation_message(&mut stream) {
                                Ok(message) if message == ACTIVATION_MESSAGE => on_activate(),
                                Ok(message) => tracing::debug!(
                                    message,
                                    "ignored an unknown activation message"
                                ),
                                Err(error) => {
                                    tracing::debug!(%error, "activation message unreadable")
                                }
                            },
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                thread::sleep(ACCEPT_POLL);
                            }
                            Err(error) => {
                                tracing::warn!(%error, "activation listener stopped");
                                break;
                            }
                        }
                    }
                })?;
            Ok(Self {
                path,
                stop,
                handle: Some(handle),
            })
        }

        fn stop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    impl Drop for ActivationServer {
        fn drop(&mut self) {
            self.stop();
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Sends the activation message to `path`; false when nobody is listening.
    pub fn notify(path: &Path) -> bool {
        let Ok(mut stream) = UnixStream::connect(path) else {
            return false;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
        stream
            .write_all(ACTIVATION_MESSAGE.as_bytes())
            .and_then(|()| stream.write_all(b"\n"))
            .is_ok()
    }
}

#[cfg(not(unix))]
mod activation {
    use std::io;
    use std::path::{Path, PathBuf};

    /// Activation channels are a Unix-domain-socket feature; other targets still refuse to
    /// open a second window but cannot raise the first one.
    #[derive(Debug)]
    pub struct ActivationServer;

    impl ActivationServer {
        pub fn start(_path: PathBuf, _on_activate: impl Fn() + Send + 'static) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "window activation needs an activation channel this platform does not provide",
            ))
        }
    }

    pub fn notify(_path: &Path) -> bool {
        false
    }
}

#[cfg(test)]
mod tests;
