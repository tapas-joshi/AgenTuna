//! Process logging: one daily rolling file, an `EnvFilter` that reloads at runtime and a
//! redaction layer that keeps secrets out of every line.
//!
//! Both `at-app` and `at-cli` call [`init`] once at startup. The module owns no
//! policy of its own: the log directory comes from [`crate::paths::AppPaths`], the default
//! level from [`crate::config::LogLevel`] and the filter directives from `RUST_LOG`.
//!
//! Layout and rules:
//!
//! - one file per UTC day, `<log_dir>/agentuna-YYYY-MM-DD.log`; the file is opened lazily
//!   on the first event, so installing logging never creates the directory by itself, and
//!   the first event of a new day opens the new file while the old handle is dropped;
//! - files whose name-dated day is more than [`RETENTION_DAYS`] in the past are deleted
//!   once, during [`init`];
//! - compact text by default, one JSON object per line when `AGENTUNA_LOG_JSON=1`;
//! - `RUST_LOG` overrides the settings level entirely; without it the `[advanced]
//!   log_level` value (default `info`) is the filter's default directive. [`Logging::set_level`]
//!   swaps only that default, keeping any target-scoped `RUST_LOG` directives;
//! - every line passes through [`at_model::redact::Redactor`] before it reaches the file or
//!   stderr, so a known secret value, a `NAME=value` assignment for a registered secret name
//!   or a credential shape (`ghp_`, `sk-`, `Bearer …`, `AKIA`, `xoxb-`, JWT, URL password)
//!   is written as `***`. `${VAR}` references are copied verbatim;
//! - `session_id`, `app_version`, `os_version` and `schema_version` are appended to every
//!   line by the writer (the fmt layer has no global-fields concept); `operation_id`,
//!   `job_id`, `agent_id` and `path` are ordinary event/span fields — emit paths through
//!   [`relative_path`] so they stay short and home-relative;
//! - a log directory that cannot be created or opened degrades to stderr only — detected
//!   on the first write — and the app still starts;
//! - panics are reported through `tracing_panic`, which emits one `ERROR` event inside the
//!   panicking thread's current span, so a job panic carries `job_id`/`operation_id` when
//!   the job future is instrumented with [`job_span`].
//!
//! Read-only CLI invocations (`--read-only`, `--dry-run`) set
//! [`LoggingOptions::file_logging`] to `false` so they keep their "writes nothing"
//! contract; their events go to stderr only.
use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use at_model::redact::Redactor;
use time::format_description::well_known::Iso8601;
use time::{Date, OffsetDateTime};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;
use tracing_subscriber::reload::{self, Handle};

use crate::config::LogLevel;
use crate::paths::AppPaths;

/// File-name prefix of the daily logs.
const LOG_FILE_PREFIX: &str = "agentuna-";

/// File-name suffix of the daily logs.
const LOG_FILE_SUFFIX: &str = ".log";

/// How many days of log files are kept; older files are deleted on startup.
pub const RETENTION_DAYS: i64 = 30;

/// Environment variable that switches the format to one JSON object per line.
pub const ENV_JSON: &str = "AGENTUNA_LOG_JSON";

/// Environment variable consulted before the settings level, per `tracing` convention.
pub const ENV_FILTER: &str = "RUST_LOG";

/// Process-wide logging state returned by [`init`].
///
/// Cheap to clone: every handle shares the subscriber's reload handle, the redactor slot and
/// the session id. Keeping one alive is not required for logging to work — the global
/// default subscriber outlives it — but [`Logging::set_level`] and late secret registration
/// need it.
#[derive(Debug, Clone)]
pub struct Logging {
    inner: Arc<LoggingInner>,
}

#[derive(Debug)]
struct LoggingInner {
    session_id: String,
    log_dir: PathBuf,
    redactor: Arc<RedactorSlot>,
    filter: Handle<EnvFilter, Registry>,
    /// Target-scoped `RUST_LOG` directives preserved across [`Logging::set_level`].
    rust_log_targets: Vec<String>,
    /// Whether `RUST_LOG` was set at startup; when it is, settings levels are ignored.
    rust_log_set: bool,
}

/// Which writer the subscriber emits to.
#[derive(Clone)]
enum Sink {
    /// `<log_dir>/agentuna-YYYY-MM-DD.log`, rolled when the UTC date changes.
    Daily(DailyAppender),
    /// The log file could not be opened; everything goes to stderr.
    Stderr,
}

/// The redactor shared between the writer and callers that learn secrets later.
///
/// Interior mutability lets the secrets store register Keychain-resolved values after the
/// subscriber is installed; the writer re-reads the slot on every line.
#[derive(Debug, Default)]
pub struct RedactorSlot {
    inner: RwLock<Redactor>,
}

impl RedactorSlot {
    /// An empty slot; masks only the built-in credential shapes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A slot preloaded with known values and names.
    #[must_use]
    pub fn with_known(values: &[&str], names: &[&str]) -> Self {
        let slot = Self::new();
        {
            let mut redactor = slot.write();
            for value in values {
                redactor.insert_value(value);
            }
            for name in names {
                redactor.insert_name(name);
            }
        }
        slot
    }

    /// Registers a secret value resolved after startup (for example from the Keychain).
    ///
    /// Returns whether the value was accepted (non-empty, not a `${...}` reference).
    pub fn add_value(&self, value: &str) -> bool {
        self.write().insert_value(value)
    }

    /// Registers a secret name so `NAME=value` assignments are masked.
    pub fn add_name(&self, name: &str) -> bool {
        self.write().insert_name(name)
    }

    /// Redacts `text` with the current redactor contents.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        self.read().redact_text(text)
    }

    fn read(&self) -> RwLockReadGuard<'_, Redactor> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Redactor> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Everything [`init`] needs that is not already in [`AppPaths`].
#[derive(Clone)]
pub struct LoggingOptions {
    /// Directory the daily files live in; normally `paths.log_dir()`.
    pub log_dir: PathBuf,
    /// Default filter level used when `RUST_LOG` is unset or carries no default directive.
    pub level: LogLevel,
    /// `AGENTUNA_LOG_JSON=1` equivalent; tests set this directly instead of mutating env.
    pub json: bool,
    /// `RUST_LOG` equivalent; `None` reads the process environment.
    pub rust_log: Option<String>,
    /// Known secret values (at least six bytes each are masked wherever they appear).
    pub secret_values: Vec<String>,
    /// Secret names whose `NAME=value` assignments are masked.
    pub secret_names: Vec<String>,
    /// Redactor slot to share; `None` builds one from `secret_values`/`secret_names`.
    pub redactor: Option<Arc<RedactorSlot>>,
    /// Session id for the startup line and spans; `None` generates a UUIDv7.
    pub session_id: Option<String>,
    /// App version written to the startup line; `None` uses this crate's `CARGO_PKG_VERSION`.
    pub app_version: Option<String>,
    /// Schema version written to the startup line; `None` uses the shipped migrations' latest.
    pub schema_version: Option<u32>,
    /// Operating-system version for the startup line; `None` probes `sw_vers` on macOS.
    pub os_version: Option<String>,
    /// Clock for the daily file name and the retention sweep; `None` uses the UTC clock.
    pub now: Option<Arc<dyn Fn() -> OffsetDateTime + Send + Sync>>,
    /// Write target for tests that need to observe stderr; production uses `io::stderr`.
    pub stderr: Option<Arc<dyn Fn() -> Box<dyn Write + Send> + Send + Sync>>,
    /// When `false`, events go to stderr only and the log directory is never touched.
    /// Read-only CLI invocations (`--read-only`, `--dry-run`) use this so they keep their
    /// "writes nothing" contract; the default is `true`.
    pub file_logging: bool,
}

impl LoggingOptions {
    /// Options for the real process: log dir from `paths`, level from settings, env read.
    #[must_use]
    pub fn for_paths(paths: &AppPaths, level: LogLevel) -> Self {
        let (secret_values, secret_names) = harvest_env_secrets();
        Self {
            log_dir: paths.log_dir().to_path_buf(),
            level,
            json: std::env::var(ENV_JSON).is_ok_and(|value| value == "1"),
            rust_log: None,
            secret_values,
            secret_names,
            redactor: None,
            session_id: None,
            app_version: None,
            schema_version: None,
            os_version: None,
            now: None,
            stderr: None,
            file_logging: true,
        }
    }
}

impl Default for LoggingOptions {
    fn default() -> Self {
        Self {
            log_dir: PathBuf::from("agentuna-logs"),
            level: LogLevel::Info,
            json: false,
            rust_log: None,
            secret_values: Vec::new(),
            secret_names: Vec::new(),
            redactor: None,
            session_id: None,
            app_version: None,
            schema_version: None,
            os_version: None,
            now: None,
            stderr: None,
            file_logging: true,
        }
    }
}

/// The installed subscriber. Returned by [`init`]; clone it into state holders
/// when commands need [`Logging::set_level`] or [`Logging::redactor`].
#[derive(Debug)]
pub struct LoggingGuard {
    logging: Logging,
}

impl LoggingGuard {
    /// The shared logging handle.
    #[must_use]
    pub fn logging(&self) -> &Logging {
        &self.logging
    }
}

impl Logging {
    /// Directory the daily files are written to.
    #[must_use]
    pub fn log_dir(&self) -> &Path {
        &self.inner.log_dir
    }

    /// The session id carried by the startup line and available to spans.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    /// The shared redactor slot; register Keychain-resolved values here as they arrive.
    #[must_use]
    pub fn redactor(&self) -> &Arc<RedactorSlot> {
        &self.inner.redactor
    }

    /// Changes the default filter level at runtime.
    ///
    /// The new `EnvFilter` keeps the target-scoped directives `RUST_LOG` contributed at
    /// startup and replaces only the default level. When `RUST_LOG` was set at startup the
    /// call is a no-op: the operator's explicit environment owns the filter, matching the
    /// documented env-over-settings precedence.
    ///
    /// # Errors
    /// [`LoggingError::Reload`] when the subscriber is already gone (process shutdown).
    pub fn set_level(&self, level: LogLevel) -> Result<(), LoggingError> {
        if self.inner.rust_log_set {
            return Ok(());
        }
        let filter = build_reload_filter(&self.inner.rust_log_targets, level);
        self.inner
            .filter
            .reload(filter)
            .map_err(|error| LoggingError::Reload {
                reason: error.to_string(),
            })
    }
}

/// Installs the process subscriber and returns its guard.
///
/// Order of operations: sweep expired log files, open today's file (stderr on failure),
/// install the panic hook, emit the startup line. Safe to call once; a second call returns
/// [`LoggingError::AlreadyInitialized`] rather than double-subscribing.
///
/// # Errors
/// [`LoggingError::AlreadyInitialized`] when a global default exists.
pub fn init(options: &LoggingOptions) -> Result<LoggingGuard, LoggingError> {
    init_inner(options, true)
}

/// [`init`] without touching the process panic hook — for tests that exercise the writer.
pub fn init_for_test(options: &LoggingOptions) -> Result<LoggingGuard, LoggingError> {
    init_inner(options, false)
}

/// Builds the subscriber for `options` without installing it globally.
///
/// Tests wrap their assertions in [`tracing::subscriber::with_default`] so parallel tests
/// never race on `set_global_default`.
pub fn subscriber_for_test(
    options: &LoggingOptions,
) -> Result<(tracing::Dispatch, LoggingGuard), LoggingError> {
    build(options, false)
}

fn init_inner(
    options: &LoggingOptions,
    install_panic_hook: bool,
) -> Result<LoggingGuard, LoggingError> {
    let (dispatch, guard) = build(options, install_panic_hook)?;
    // `Dispatch` is not itself a `Subscriber`; rebuild one from the same subscriber is not
    // possible, so install through `set_global_default` on the dispatch's inner subscriber
    // is unavailable — use the dispatch-based setter instead.
    tracing::dispatcher::set_global_default(dispatch).map_err(|error| {
        LoggingError::AlreadyInitialized {
            reason: error.to_string(),
        }
    })?;
    Ok(guard)
}

/// Shared construction: sweep, sink, filter, fmt layer, panic hook, startup line.
fn build(
    options: &LoggingOptions,
    install_panic_hook: bool,
) -> Result<(tracing::Dispatch, LoggingGuard), LoggingError> {
    let now = options
        .now
        .clone()
        .unwrap_or_else(|| Arc::new(OffsetDateTime::now_utc));

    // Retention first: a sweep failure is reported on stderr, never fatal.
    if let Err(error) = sweep_expired(&options.log_dir, &now) {
        let _ = writeln!(
            io::stderr(),
            "agentuna: could not sweep old logs in {}: {error}",
            options.log_dir.display()
        );
    }

    let redactor = match &options.redactor {
        Some(slot) => Arc::clone(slot),
        None => Arc::new(RedactorSlot::with_known(
            &options
                .secret_values
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            &options
                .secret_names
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )),
    };

    // The appender opens its file lazily on the first write and falls back to stderr
    // on its own when the directory cannot be created, so building the subscriber never
    // touches the file system. `file_logging: false` (read-only CLI runs) skips the file
    // sink entirely.
    let stderr_factory: Arc<dyn Fn() -> Box<dyn Write + Send> + Send + Sync> = options
        .stderr
        .clone()
        .unwrap_or_else(|| Arc::new(|| Box::new(io::stderr()) as Box<dyn Write + Send>));
    let sink = if options.file_logging {
        Sink::Daily(DailyAppender::new(
            &options.log_dir,
            now.clone(),
            Arc::clone(&stderr_factory),
        ))
    } else {
        Sink::Stderr
    };

    let rust_log = options
        .rust_log
        .clone()
        .or_else(|| std::env::var(ENV_FILTER).ok());
    let (filter, rust_log_targets, rust_log_set) = build_filter(rust_log.as_deref(), options.level);
    let (reloadable, reload_handle) = reload::Layer::new(filter);

    let session_id = options
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::now_v7().simple().to_string());
    let app_version = options
        .app_version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    let schema_version = options
        .schema_version
        .unwrap_or(crate::layout::SCHEMA_VERSION);
    let os_version = options.os_version.clone().unwrap_or_else(detect_os_version);
    let decorate = Decorate::new(
        options.json,
        &session_id,
        &app_version,
        &os_version,
        schema_version,
    );

    let make_writer = RedactingMakeWriter {
        sink,
        redactor: Arc::clone(&redactor),
        decorate,
        stderr: options.stderr.clone(),
    };

    let fmt_layer: Box<dyn Layer<Registry> + Send + Sync> = if options.json {
        Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_writer(make_writer),
        )
    } else {
        Box::new(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(make_writer),
        )
    };

    let subscriber = Registry::default().with(fmt_layer.with_filter(reloadable));
    let dispatch = tracing::Dispatch::new(subscriber);

    if install_panic_hook {
        self::install_panic_hook();
    }

    let logging = Logging {
        inner: Arc::new(LoggingInner {
            session_id,
            log_dir: options.log_dir.clone(),
            redactor,
            filter: reload_handle,
            rust_log_targets,
            rust_log_set,
        }),
    };

    // Emit the startup line through this dispatch explicitly: for `init` the global
    // default is not installed yet at this point, and for tests the caller wraps its own
    // assertions in `with_default`. Either way the line lands in our sink exactly once.
    // The session fields are appended by the writer's decorator on every line, so the
    // event itself only carries what is specific to this record.
    let log_dir = logging.log_dir().to_path_buf();
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(
            log_dir = %log_dir.display(),
            json = options.json,
            "AgenTuna started"
        );
    });

    Ok((dispatch, LoggingGuard { logging }))
}

/// Builds the initial [`EnvFilter`] and the pieces [`Logging::set_level`] needs later.
///
/// `RUST_LOG` wins over the settings level: a bare directive in it becomes the env default;
/// target-scoped directives (`target=level`) are preserved for later reloads. Returns the
/// filter, the preserved targets and whether the env var was set.
fn build_filter(rust_log: Option<&str>, level: LogLevel) -> (EnvFilter, Vec<String>, bool) {
    let Some(raw) = rust_log else {
        return (EnvFilter::new(level.as_str()), Vec::new(), false);
    };
    if raw.trim().is_empty() {
        // Set-but-empty counts as unset: the settings level owns the filter.
        return (EnvFilter::new(level.as_str()), Vec::new(), false);
    }
    let mut targets = Vec::new();
    let mut env_default: Option<String> = None;
    for directive in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        let directive = directive.trim();
        if directive.is_empty() {
            continue;
        }
        if directive.contains('=') {
            targets.push(directive.to_owned());
        } else {
            // A bare directive is a level (or `off`); the last one wins, matching EnvFilter.
            env_default = Some(directive.to_owned());
        }
    }
    let filter = build_reload_filter_with_default(env_default, &targets, level);
    (filter, targets, true)
}

/// Rebuilds the filter for [`Logging::set_level`]: settings level as default, `RUST_LOG`
/// target directives preserved.
fn build_reload_filter(targets: &[String], level: LogLevel) -> EnvFilter {
    build_reload_filter_with_default(None, targets, level)
}

fn build_reload_filter_with_default(
    env_default: Option<String>,
    targets: &[String],
    level: LogLevel,
) -> EnvFilter {
    let default = env_default.unwrap_or_else(|| level.as_str().to_owned());
    let mut spec = default;
    if !targets.is_empty() {
        spec.push(',');
        spec.push_str(&targets.join(","));
    }
    EnvFilter::new(spec)
}

/// Installs the `tracing_panic` hook, preserving the previous hook (the default stderr
/// hook) so panic messages still reach the terminal.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing_panic::panic_hook(info);
        previous(info);
    }));
}

/// Environment variable names whose values are treated as secrets at startup.
///
/// A name qualifies when one of its `_`/`-` separated segments is a credential word, so
/// `GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY` and `NPM_KEY` match while `MONKEY` and `KEYNOTE`
/// do not. Matching names are registered with the redactor twice: as a name (masking
/// `NAME=value` assignments of any length) and as a value (masking the resolved secret
/// wherever it appears in a line).
const SECRET_NAME_SEGMENTS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "KEY",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "PRIVATE",
    "AUTH",
];

/// Whether `name` looks like a credential variable.
fn is_secret_env_name(name: &str) -> bool {
    name.split(['_', '-'])
        .any(|segment| SECRET_NAME_SEGMENTS.contains(&segment.to_ascii_uppercase().as_str()))
}

/// Scans the process environment for credential-shaped variables.
///
/// Returns `(values, names)`: every qualifying variable contributes its name, and its
/// value too when the value is at least [`at_model::redact::MIN_VALUE_LEN`] bytes (shorter
/// values are still masked through the `NAME=value` rule).
fn harvest_env_secrets() -> (Vec<String>, Vec<String>) {
    let mut values = Vec::new();
    let mut names = Vec::new();
    // `vars_os` + lossy name check: `vars()` would panic on a non-UTF-8 variable, and a
    // non-UTF-8 value cannot be represented to the redactor anyway.
    for (name, value) in std::env::vars_os() {
        let Some(name) = name.into_string().ok() else {
            continue;
        };
        if !is_secret_env_name(&name) {
            continue;
        }
        names.push(name);
        // Only UTF-8 values can be registered; the `NAME=value` rule still masks the
        // assignment itself for non-UTF-8 secrets.
        if let Ok(value) = value.into_string()
            && value.len() >= at_model::redact::MIN_VALUE_LEN
        {
            values.push(value);
        }
    }
    values.sort_unstable();
    values.dedup();
    names.sort_unstable();
    names.dedup();
    (values, names)
}

/// Deletes `agentuna-*.log` files whose name-dated day is more than [`RETENTION_DAYS`] old.
///
/// A file whose name does not parse falls back to its modification time. A missing
/// directory is not an error (first run); individual delete failures are skipped —
/// retention is best-effort and must never stop startup.
pub fn sweep_expired(
    log_dir: &Path,
    now: &Arc<dyn Fn() -> OffsetDateTime + Send + Sync>,
) -> io::Result<()> {
    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let cutoff = now().date() - time::Duration::days(RETENTION_DAYS);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let expired = match log_file_date(name) {
            Some(date) => date < cutoff,
            None => entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .map(OffsetDateTime::from)
                .is_some_and(|mtime| mtime.date() < cutoff),
        };
        if expired {
            // Best effort: a read-only or busy file is retried on the next start.
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Parses `agentuna-YYYY-MM-DD.log` into its date; `None` for any other name.
fn log_file_date(name: &str) -> Option<Date> {
    let stem = name
        .strip_prefix(LOG_FILE_PREFIX)?
        .strip_suffix(LOG_FILE_SUFFIX)?;
    Date::parse(stem, &Iso8601::DATE).ok()
}

/// Opens (creating if needed) today's log file inside `dir`.
fn open_daily_file(dir: &Path, date: Date) -> io::Result<File> {
    fs::create_dir_all(dir)?;
    let name = format!(
        "{LOG_FILE_PREFIX}{:04}-{:02}-{:02}{LOG_FILE_SUFFIX}",
        date.year(),
        u8::from(date.month()),
        date.day()
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))
}

/// The `MakeWriter` handed to the fmt layer: resolves to a [`RedactingWriter`] over either
/// today's daily file or stderr.
struct RedactingMakeWriter {
    sink: Sink,
    redactor: Arc<RedactorSlot>,
    decorate: Decorate,
    stderr: Option<Arc<dyn Fn() -> Box<dyn Write + Send> + Send + Sync>>,
}

impl<'a> MakeWriter<'a> for RedactingMakeWriter {
    type Writer = RedactingWriter<SinkWriter>;

    fn make_writer(&'a self) -> Self::Writer {
        let inner = match &self.sink {
            Sink::Daily(appender) => SinkWriter::Daily(appender.clone()),
            Sink::Stderr => SinkWriter::Stderr(self.stderr_writer()),
        };
        RedactingWriter::new(inner, Arc::clone(&self.redactor), self.decorate.clone())
    }
}

impl RedactingMakeWriter {
    fn stderr_writer(&self) -> Box<dyn Write + Send> {
        match &self.stderr {
            Some(factory) => factory(),
            None => Box::new(io::stderr()),
        }
    }
}

/// What one event writes through to.
enum SinkWriter {
    /// The daily appender (rolls the file when the UTC date changes).
    Daily(DailyAppender),
    /// Stderr fallback.
    Stderr(Box<dyn Write + Send>),
}

impl Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Daily(appender) => appender.write(buf),
            Self::Stderr(writer) => writer.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Daily(appender) => appender.flush(),
            Self::Stderr(writer) => writer.flush(),
        }
    }
}

/// Constant per-process fields appended to every emitted line.
///
/// `tracing_subscriber::fmt` has no "global fields" concept, so the session fields are
/// injected at the writer: text lines get a ` k="v"` suffix and JSON lines get the entries
/// merged into the top-level object (consistent with `flatten_event`). Values are escaped
/// for their format; they are constants, never secrets, so they skip the redactor.
#[derive(Clone)]
struct Decorate {
    kind: DecorateKind,
}

#[derive(Clone)]
enum DecorateKind {
    /// ` session_id="…" app_version="…" os_version="…" schema_version=N` appended verbatim.
    Text(String),
    /// `"session_id":"…",…` merged before the closing `}` of the single-line JSON object.
    Json(String),
}

impl Decorate {
    /// Builds the decorator for `json` from the resolved session fields.
    fn new(
        json: bool,
        session_id: &str,
        app_version: &str,
        os_version: &str,
        schema_version: u32,
    ) -> Self {
        let fields = [
            DecorateField::Str("session_id", session_id.to_owned()),
            DecorateField::Str("app_version", app_version.to_owned()),
            DecorateField::Str("os_version", os_version.to_owned()),
            DecorateField::UInt("schema_version", u64::from(schema_version)),
        ];
        if json {
            let entries = fields
                .iter()
                .map(|field| match field {
                    DecorateField::Str(name, value) => {
                        let encoded =
                            serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned());
                        format!("\"{name}\":{encoded}")
                    }
                    DecorateField::UInt(name, value) => format!("\"{name}\":{value}"),
                })
                .collect::<Vec<_>>()
                .join(",");
            Self {
                kind: DecorateKind::Json(entries),
            }
        } else {
            let suffix = fields
                .iter()
                .map(|field| match field {
                    DecorateField::Str(name, value) => {
                        format!(" {name}=\"{}\"", escape_text_field(value))
                    }
                    DecorateField::UInt(name, value) => format!(" {name}={value}"),
                })
                .collect::<String>();
            Self {
                kind: DecorateKind::Text(suffix),
            }
        }
    }

    /// Applies the decoration to one complete, already-redacted line (no `\n`).
    fn apply(&self, line: &str) -> String {
        match &self.kind {
            DecorateKind::Text(suffix) => {
                let mut out = String::with_capacity(line.len() + suffix.len());
                out.push_str(line);
                out.push_str(suffix);
                out
            }
            DecorateKind::Json(entries) => {
                let Some(body) = line.strip_suffix('}') else {
                    return line.to_owned();
                };
                let body = body.trim_end();
                let mut out = String::with_capacity(body.len() + entries.len() + 2);
                out.push_str(body);
                if !body.ends_with('{') {
                    out.push(',');
                }
                out.push_str(entries);
                out.push('}');
                out
            }
        }
    }
}

/// One constant field: a string (quoted in both formats) or an unsigned integer.
enum DecorateField {
    /// A string value, rendered `"v"` in JSON and `"v"` (escaped) in text.
    Str(&'static str, String),
    /// An unsigned integer, rendered bare in both formats.
    UInt(&'static str, u64),
}

/// Escapes a value for the `k="v"` text field syntax (backslash and quote only).
fn escape_text_field(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Buffers bytes until a newline, redacts each complete line, appends the session fields
/// and forwards it.
///
/// `tracing_subscriber::fmt` emits one record per event and always terminates it with `\n`,
/// so the buffer rarely holds more than one line. A partial line at `flush` is still
/// processed before it is written, so a truncated final line can neither leak a secret nor
/// skip the session fields.
struct RedactingWriter<W: Write> {
    inner: W,
    redactor: Arc<RedactorSlot>,
    decorate: Decorate,
    pending: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    fn new(inner: W, redactor: Arc<RedactorSlot>, decorate: Decorate) -> Self {
        Self {
            inner,
            redactor,
            decorate,
            pending: Vec::with_capacity(512),
        }
    }

    /// Redacts `bytes` (a complete line without its terminator), decorates it and writes
    /// it plus `\n`.
    fn emit_line(&mut self, bytes: &[u8]) -> io::Result<()> {
        // Log output is UTF-8 by construction (fmt writes `&str`); a stray invalid byte is
        // replaced rather than dropping the line.
        let text = String::from_utf8_lossy(bytes);
        let redacted = self.redactor.redact(&text);
        let decorated = self.decorate.apply(&redacted);
        self.inner.write_all(decorated.as_bytes())?;
        self.inner.write_all(b"\n")
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        // Drain one complete line at a time so the redactor sees whole records; the drain
        // ends its borrow before `emit_line` takes `&mut self`.
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
            line.pop(); // the '\n' itself
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.emit_line(&line)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.emit_line(&pending)?;
        }
        self.inner.flush()
    }
}

/// Daily rolling file appender: one open `File` per UTC day, rolled lazily on write.
///
/// The appender is `Clone`-cheap (an `Arc` around the shared state) so each event's
/// `MakeWriter` call clones instead of re-opening anything; the date check is one
/// `OffsetDateTime::now_utc()` per event.
#[derive(Clone)]
struct DailyAppender {
    state: Arc<Mutex<DailyState>>,
}

struct DailyState {
    dir: PathBuf,
    now: Arc<dyn Fn() -> OffsetDateTime + Send + Sync>,
    /// Factory for the stderr sink used once file logging has failed.
    stderr: Arc<dyn Fn() -> Box<dyn Write + Send> + Send + Sync>,
    file: Option<File>,
    date: Option<Date>,
    /// Set when the file cannot be opened; the writer degrades to stderr writes.
    failed: bool,
    /// The failure warning is emitted once, on the first failed open.
    warned: bool,
    /// Lazily created stderr sink; `None` until the first fallback write.
    stderr_writer: Option<Box<dyn Write + Send>>,
}

impl DailyAppender {
    /// Creates the appender without touching the file system.
    ///
    /// The directory and today's file are created lazily on the first write, so merely
    /// installing logging (or a process that never logs) leaves no trace. When the open
    /// fails — a read-only or missing directory — the appender warns once and forwards
    /// every write to the stderr factory instead.
    fn new(
        dir: &Path,
        now: Arc<dyn Fn() -> OffsetDateTime + Send + Sync>,
        stderr: Arc<dyn Fn() -> Box<dyn Write + Send> + Send + Sync>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(DailyState {
                dir: dir.to_path_buf(),
                now,
                stderr,
                file: None,
                date: None,
                failed: false,
                warned: false,
                stderr_writer: None,
            })),
        }
    }

    /// Ensures `state.file` points at a file for `state.now`'s UTC date, opening it
    /// lazily. On failure the state degrades to stderr writes.
    fn ensure_today(state: &mut DailyState) {
        if state.failed {
            return;
        }
        let today = (state.now)().date();
        if state.date == Some(today) && state.file.is_some() {
            return;
        }
        match open_daily_file(&state.dir, today) {
            Ok(file) => {
                state.file = Some(file);
                state.date = Some(today);
            }
            Err(error) => {
                state.failed = true;
                state.file = None;
                if !state.warned {
                    state.warned = true;
                    let dir = state.dir.display().to_string();
                    let _ = writeln!(
                        Self::stderr_writer(state),
                        "agentuna: cannot open the log directory {dir} ({error}); \
                         logging to stderr only"
                    );
                }
            }
        }
    }

    /// The lazily created stderr sink used once file logging has failed.
    fn stderr_writer(state: &mut DailyState) -> &mut dyn Write {
        state
            .stderr_writer
            .get_or_insert_with(|| (state.stderr)())
            .as_mut()
    }
}

impl Write for DailyAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        Self::ensure_today(&mut state);
        match &mut state.file {
            Some(file) => file.write(buf),
            None => Self::stderr_writer(&mut state).write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut state.file {
            Some(file) => file.flush(),
            None => Self::stderr_writer(&mut state).flush(),
        }
    }
}

/// A span for one bridge job; instrument the job future with it so panics and progress
/// events carry `job_id`, `operation_id` and `job_name` in their span context.
#[must_use]
pub fn job_span(id: &str, name: &str) -> tracing::Span {
    tracing::info_span!("job", job_id = %id, operation_id = %id, job_name = %name)
}

/// Renders `path` relative to `home` when it sits underneath it, else the full path.
///
/// Log fields should use this so lines stay readable and do not leak the home directory
/// layout more than necessary. The result keeps a leading `~/` so consumers can tell a
/// relative rendering from a literal path.
#[must_use]
pub fn relative_path<'a>(path: &'a Path, home: Option<&Path>) -> Cow<'a, str> {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        if rest.as_os_str().is_empty() {
            return Cow::Borrowed("~");
        }
        return Cow::Owned(format!("~/{}", rest.display()));
    }
    Cow::Owned(path.display().to_string())
}

/// Why [`init`] could not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoggingError {
    /// A global default subscriber already exists (init called twice, or a test set one).
    #[error("logging is already initialized: {reason}")]
    AlreadyInitialized {
        /// The underlying `set_global_default` error.
        reason: String,
    },
    /// The reload handle lost its subscriber during shutdown.
    #[error("the log filter subscriber is gone: {reason}")]
    Reload {
        /// The reload error.
        reason: String,
    },
}

/// macOS product version via `sw_vers`; a generic `os arch` string elsewhere or on failure.
fn detect_os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                let version = text.trim();
                if version.is_empty() {
                    "macos".to_owned()
                } else {
                    format!("macos {version}")
                }
            }
            _ => "macos".to_owned(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    }
}

#[cfg(test)]
mod tests;
