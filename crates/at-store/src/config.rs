//! Runtime configuration file (`agentuna.toml`) and feature flags.
//!
//! Flags let unfinished surfaces ship dark without branching the codebase. They are read
//! **once** at startup into a typed [`FeatureFlags`] value and exposed read-only; changing one
//! therefore requires a restart, which [`ConfigReport::restart_note`] states for the UI.
//!
//! Precedence, lowest first:
//!
//! 1. compiled default ([`FLAGS`]);
//! 2. `[flags]` in `<support>/agentuna.toml`;
//! 3. environment `AGENTUNA_FLAG_<NAME>`;
//! 4. command line `--flag vendor:name=value` (vendor `agentuna` or omitted).
//!
//! Safety is never flaggable. [`SAFETY_FLAGS`] are rejected even when present, with one
//! [`SAFETY_REJECTION`] warning, because a configuration file must not be able to disarm the
//! overwrite, backup or concurrency checks.
//!
//! Loading is deliberately forgiving: an unreadable or malformed file, an unknown key or an
//! invalid value produces a [`ConfigWarning`] and the previous value stays in force, so a typo
//! never prevents the app from starting. The only hard failure is the first-run file creation
//! itself ([`ConfigError::CreateDefault`]); callers fall back to [`RuntimeConfig::defaults`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use toml_edit::{DocumentMut, Item};

/// File name of the runtime configuration inside the support directory.
pub const CONFIG_FILE_NAME: &str = "agentuna.toml";

/// Prefix of environment variables that override a flag.
pub const ENV_FLAG_PREFIX: &str = "AGENTUNA_FLAG_";

/// Statement the UI and `agentuna config` show next to the effective values.
pub const RESTART_NOTE: &str =
    "Feature flags are read once at startup; restart AgenTuna to apply a change.";

/// Exact warning message for a rejected safety flag.
pub const SAFETY_REJECTION: &str = "safety flag cannot be disabled";

/// Every flag this build knows, in display order.
pub const FLAGS: [FlagDefinition; 5] = [
    FlagDefinition {
        key: "marketplace",
        default: false,
        summary: "Marketplace browsing and installation",
    },
    FlagDefinition {
        key: "remote_backup",
        default: false,
        summary: "Remote backup push and pull",
    },
    FlagDefinition {
        key: "auto_update",
        default: false,
        summary: "Automatic update checks",
    },
    FlagDefinition {
        key: "instructions",
        default: true,
        summary: "Generated AGENTS.md and CLAUDE.md instruction files",
    },
    FlagDefinition {
        key: "presets",
        default: true,
        summary: "Presets and preset applications",
    },
];

/// Settings that are never flaggable: they would weaken the safe-write pipeline.
pub const SAFETY_FLAGS: [&str; 4] = [
    "allow_overwrite",
    "skip_backup",
    "disable_concurrency_check",
    "disable_refuse_overwrite",
];

/// The file written on first run. It lists every default and intentionally omits the safety
/// keys: they cannot be enabled, so showing them as toggles would be a lie.
const DEFAULT_FILE: &str = r#"# AgenTuna runtime configuration.
#
# Feature flags are read once at startup; changing one requires restarting AgenTuna.
# Precedence: --flag vendor:name=value on the command line > AGENTUNA_FLAG_<NAME> >
# this file > the compiled default.
#
# Safety is not flaggable. allow_overwrite, skip_backup, disable_concurrency_check and
# disable_refuse_overwrite are always rejected, so they are not listed here.

[flags]
# Marketplace browsing and installation (default: false).
marketplace = false
# Remote backup push and pull (default: false).
remote_backup = false
# Automatic update checks (default: false).
auto_update = false
# Generated AGENTS.md and CLAUDE.md instruction files (default: true).
instructions = true
# Presets and preset applications (default: true).
presets = true

[advanced]
# Log level: error, warn, info, debug or trace.
log_level = "info"
# Jobs the background runtime may run at once (1-64).
max_concurrent_jobs = 4
# Timeout for one MCP connection attempt, in seconds (1-600).
mcp_connect_timeout_seconds = 30
"#;

/// One compiled flag definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagDefinition {
    /// Canonical key, also used in the report and the environment variable.
    pub key: &'static str,
    /// Value compiled into this build.
    pub default: bool,
    /// One-line description shown by `agentuna config` and the settings screen.
    pub summary: &'static str,
}

/// Where an effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FlagSource {
    /// Compiled into this build.
    Default,
    /// `[flags]` in `agentuna.toml`.
    File,
    /// `AGENTUNA_FLAG_<NAME>`.
    Environment,
    /// `--flag vendor:name=value`.
    CommandLine,
}

impl FlagSource {
    /// Stable identifier written to JSON and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Environment => "environment",
            Self::CommandLine => "command_line",
        }
    }
}

/// The effective feature flags, one accessor per flag and no setter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureFlags {
    marketplace: bool,
    remote_backup: bool,
    auto_update: bool,
    instructions: bool,
    presets: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            marketplace: false,
            remote_backup: false,
            auto_update: false,
            instructions: true,
            presets: true,
        }
    }
}

impl FeatureFlags {
    /// Whether the marketplace surface is enabled.
    #[must_use]
    pub const fn marketplace(&self) -> bool {
        self.marketplace
    }

    /// Whether remote backup is enabled.
    #[must_use]
    pub const fn remote_backup(&self) -> bool {
        self.remote_backup
    }

    /// Whether automatic update checks are enabled.
    #[must_use]
    pub const fn auto_update(&self) -> bool {
        self.auto_update
    }

    /// Whether generated instruction files are enabled.
    #[must_use]
    pub const fn instructions(&self) -> bool {
        self.instructions
    }

    /// Whether presets are enabled.
    #[must_use]
    pub const fn presets(&self) -> bool {
        self.presets
    }

    /// The value of one flag by canonical key, or `None` for an unknown key.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<bool> {
        match key {
            "marketplace" => Some(self.marketplace),
            "remote_backup" => Some(self.remote_backup),
            "auto_update" => Some(self.auto_update),
            "instructions" => Some(self.instructions),
            "presets" => Some(self.presets),
            _ => None,
        }
    }

    /// Iterates the flags in [`FLAGS`] order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, bool)> + '_ {
        FLAGS.iter().map(|definition| {
            (
                definition.key,
                self.value(definition.key).unwrap_or(definition.default),
            )
        })
    }
}

/// Verbosity written to the log sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// The default.
    Info,
    /// Debug logging.
    Debug,
    /// Trace logging.
    Trace,
}

impl LogLevel {
    /// The lowercase name used in the configuration file and report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// Parses a case-insensitive level name.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

/// Typed `[advanced]` values that are not feature flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancedConfig {
    /// Log verbosity.
    pub log_level: LogLevel,
    /// Jobs the background runtime runs at once (1-64).
    pub max_concurrent_jobs: u16,
    /// Timeout for one MCP connection attempt, in seconds (1-600).
    pub mcp_connect_timeout_seconds: u64,
}

impl AdvancedConfig {
    /// The compiled defaults.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            log_level: LogLevel::Info,
            max_concurrent_jobs: 4,
            mcp_connect_timeout_seconds: 30,
        }
    }
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self::defaults()
    }
}

/// One non-fatal configuration problem, logged as a single `WARN` and carried in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigWarning {
    /// The key the problem is about.
    pub key: String,
    /// Human-readable explanation.
    pub message: String,
    /// Nearest known key, when the problem is an unknown key.
    pub suggestion: Option<String>,
}

/// One flag in the effective report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlagReport {
    /// Effective value.
    pub value: bool,
    /// Value compiled into this build.
    pub default: bool,
    /// Where [`Self::value`] came from.
    pub source: FlagSource,
    /// One-line description.
    pub description: String,
}

/// One advanced value in the effective report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValueReport {
    /// Effective value.
    pub value: serde_json::Value,
    /// Compiled default.
    pub default: serde_json::Value,
    /// Where [`Self::value`] came from.
    pub source: FlagSource,
    /// One-line description.
    pub description: String,
}

/// Machine-readable effective configuration, printed by `agentuna config --json` and embedded
/// verbatim in `diagnostics.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigReport {
    /// Absolute path of the configuration file.
    pub config_path: String,
    /// Always true: the typed flags are immutable for the life of the process.
    pub requires_restart: bool,
    /// Statement the UI shows next to the flags.
    pub restart_note: String,
    /// Every flag with value, compiled default and source.
    pub flags: BTreeMap<String, FlagReport>,
    /// Every advanced value with value, default and source.
    pub advanced: BTreeMap<String, ValueReport>,
    /// Problems found while loading, one entry per key.
    pub warnings: Vec<ConfigWarning>,
}

/// Inputs the process contributes to a load, injectable so tests never read the real environment.
#[derive(Debug, Clone)]
pub struct ConfigInput {
    /// Raw `--flag` values, in command-line order.
    pub cli_flags: Vec<String>,
    /// `(name, value)` pairs, normally only the `AGENTUNA_FLAG_*` variables.
    pub environment: Vec<(String, String)>,
    /// Create the file on first run; false for read-only callers.
    pub create: bool,
}

impl Default for ConfigInput {
    fn default() -> Self {
        Self {
            cli_flags: Vec::new(),
            environment: Vec::new(),
            create: true,
        }
    }
}

impl ConfigInput {
    /// Reads the `AGENTUNA_FLAG_*` variables from the process environment.
    #[must_use]
    pub fn production(cli_flags: Vec<String>) -> Self {
        let mut environment: Vec<(String, String)> = std::env::vars_os()
            .filter_map(|(key, value)| {
                let key = key.into_string().ok()?;
                if !key.starts_with(ENV_FLAG_PREFIX) {
                    return None;
                }
                Some((key, value.into_string().ok()?))
            })
            .collect();
        environment.sort_unstable();
        Self {
            cli_flags,
            environment,
            create: true,
        }
    }

    /// Replaces the environment pairs.
    #[must_use]
    pub fn with_environment(mut self, pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        self.environment = pairs.into_iter().collect();
        self
    }
}

/// A hard failure while loading the runtime configuration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The default file could not be created on first run.
    #[error(
        "{}: could not create the runtime configuration file: {source}",
        path.display()
    )]
    CreateDefault {
        /// File that could not be written.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Effective runtime configuration: typed flags, advanced values and the serializable report.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    path: PathBuf,
    flags: FeatureFlags,
    advanced: AdvancedConfig,
    report: ConfigReport,
}

impl RuntimeConfig {
    /// Loads the file, overlays the environment and command line, logs one `WARN` per problem
    /// and returns the effective configuration.
    ///
    /// The file is created with comments and all defaults when it does not exist and
    /// [`ConfigInput::create`] is set. Values written by that first-run creation are *not*
    /// applied to this load: the run reports the compiled defaults, and the next process
    /// reads the file like any other.
    ///
    /// # Errors
    /// [`ConfigError::CreateDefault`] when the support directory or file cannot be written.
    pub fn load(config_path: impl Into<PathBuf>, input: ConfigInput) -> Result<Self, ConfigError> {
        let path = config_path.into();
        let mut warnings = WarningLog::default();

        let mut flags: BTreeMap<&'static str, (bool, FlagSource)> = FLAGS
            .iter()
            .map(|definition| (definition.key, (definition.default, FlagSource::Default)))
            .collect();
        let mut advanced = AdvancedConfig::defaults();
        let mut advanced_sources: BTreeMap<&'static str, FlagSource> = [
            "log_level",
            "max_concurrent_jobs",
            "mcp_connect_timeout_seconds",
        ]
        .into_iter()
        .map(|key| (key, FlagSource::Default))
        .collect();

        if let Some(text) = read_or_create(&path, input.create, &mut warnings)? {
            apply_file(
                &text,
                &mut flags,
                &mut advanced,
                &mut advanced_sources,
                &mut warnings,
            );
        }
        apply_environment(&input.environment, &mut flags, &mut warnings);
        apply_command_line(&input.cli_flags, &mut flags, &mut warnings);

        let feature_flags = FeatureFlags {
            marketplace: flag_value(&flags, "marketplace"),
            remote_backup: flag_value(&flags, "remote_backup"),
            auto_update: flag_value(&flags, "auto_update"),
            instructions: flag_value(&flags, "instructions"),
            presets: flag_value(&flags, "presets"),
        };
        let report = build_report(
            &path,
            &flags,
            &advanced,
            &advanced_sources,
            warnings.entries(),
        );
        for warning in &report.warnings {
            match &warning.suggestion {
                Some(suggestion) => {
                    tracing::warn!(key = %warning.key, suggestion = %suggestion, "{}", warning.message);
                }
                None => tracing::warn!(key = %warning.key, "{}", warning.message),
            }
        }

        Ok(Self {
            path,
            flags: feature_flags,
            advanced,
            report,
        })
    }

    /// The compiled defaults with no file access, for callers recovering from [`ConfigError`].
    #[must_use]
    pub fn defaults(config_path: impl Into<PathBuf>) -> Self {
        let path = config_path.into();
        let flags: BTreeMap<&'static str, (bool, FlagSource)> = FLAGS
            .iter()
            .map(|definition| (definition.key, (definition.default, FlagSource::Default)))
            .collect();
        let advanced = AdvancedConfig::defaults();
        let advanced_sources: BTreeMap<&'static str, FlagSource> = [
            "log_level",
            "max_concurrent_jobs",
            "mcp_connect_timeout_seconds",
        ]
        .into_iter()
        .map(|key| (key, FlagSource::Default))
        .collect();
        let report = build_report(&path, &flags, &advanced, &advanced_sources, Vec::new());
        Self {
            path,
            flags: FeatureFlags {
                marketplace: flag_value(&flags, "marketplace"),
                remote_backup: flag_value(&flags, "remote_backup"),
                auto_update: flag_value(&flags, "auto_update"),
                instructions: flag_value(&flags, "instructions"),
                presets: flag_value(&flags, "presets"),
            },
            advanced,
            report,
        }
    }

    /// Path of the configuration file.
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.path
    }

    /// The read-only typed flags.
    #[must_use]
    pub fn flags(&self) -> &FeatureFlags {
        &self.flags
    }

    /// The typed advanced values.
    #[must_use]
    pub fn advanced(&self) -> &AdvancedConfig {
        &self.advanced
    }

    /// The serializable effective report.
    #[must_use]
    pub fn report(&self) -> &ConfigReport {
        &self.report
    }

    /// The exact object `agentuna config --json` prints and `diagnostics.json` embeds.
    #[must_use]
    pub fn diagnostics_section(&self) -> serde_json::Value {
        serde_json::to_value(&self.report).unwrap_or(serde_json::Value::Null)
    }
}

/// Reads the file, creating it on first run. Returns `None` when there is nothing to apply.
fn read_or_create(
    path: &Path,
    create: bool,
    warnings: &mut WarningLog,
) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            if let Some(parent) = path.parent()
                && let Err(source) = at_fs::ensure_dir(parent)
            {
                return Err(ConfigError::CreateDefault {
                    path: path.to_path_buf(),
                    source: match source {
                        at_fs::FsError::Io { source, .. } => source,
                        other => std::io::Error::other(other.to_string()),
                    },
                });
            }
            fs::write(path, DEFAULT_FILE).map_err(|source| ConfigError::CreateDefault {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(None)
        }
        Err(error) => {
            warnings.problem(
                CONFIG_FILE_NAME,
                format!("{CONFIG_FILE_NAME} is unreadable ({error}); using the compiled defaults"),
                None,
            );
            Ok(None)
        }
    }
}

/// Applies `[flags]` and `[advanced]`.
fn apply_file(
    text: &str,
    flags: &mut BTreeMap<&'static str, (bool, FlagSource)>,
    advanced: &mut AdvancedConfig,
    advanced_sources: &mut BTreeMap<&'static str, FlagSource>,
    warnings: &mut WarningLog,
) {
    let document = match text.parse::<DocumentMut>() {
        Ok(document) => document,
        Err(error) => {
            warnings.problem(
                CONFIG_FILE_NAME,
                format!(
                    "{CONFIG_FILE_NAME} is not valid TOML ({error}); using the compiled defaults"
                ),
                None,
            );
            return;
        }
    };
    let root = document.as_table();

    match root.get("flags") {
        None => {}
        Some(item) => match item.as_table() {
            None => warnings.problem("flags", "`flags` must be a table; ignoring it", None),
            Some(table) => {
                for (key, item) in table {
                    apply_flag_item(key, item, flags, warnings);
                }
            }
        },
    }

    match root.get("advanced") {
        None => {}
        Some(item) => match item.as_table() {
            None => warnings.problem("advanced", "`advanced` must be a table; ignoring it", None),
            Some(table) => {
                for (key, item) in table {
                    apply_advanced_item(key, item, advanced, advanced_sources, warnings);
                }
            }
        },
    }
}

/// Applies one `[flags]` entry.
fn apply_flag_item(
    key: &str,
    item: &Item,
    flags: &mut BTreeMap<&'static str, (bool, FlagSource)>,
    warnings: &mut WarningLog,
) {
    let key = normalize_key(key);
    if SAFETY_FLAGS.contains(&key.as_str()) {
        warnings.safety(&key);
        return;
    }
    let Some(value) = item.as_bool() else {
        warnings.problem(
            &key,
            format!("`{key}` must be true or false; ignoring the file value"),
            None,
        );
        return;
    };
    set_flag(&key, value, FlagSource::File, flags, warnings);
}

/// Applies one `[advanced]` entry.
fn apply_advanced_item(
    key: &str,
    item: &Item,
    advanced: &mut AdvancedConfig,
    advanced_sources: &mut BTreeMap<&'static str, FlagSource>,
    warnings: &mut WarningLog,
) {
    let key = normalize_key(key);
    match key.as_str() {
        "log_level" => match item.as_str().and_then(LogLevel::parse) {
            Some(level) => {
                advanced.log_level = level;
                advanced_sources.insert("log_level", FlagSource::File);
            }
            None => warnings.problem(
                "log_level",
                "`log_level` must be error, warn, info, debug or trace; ignoring the file value",
                None,
            ),
        },
        "max_concurrent_jobs" => match item.as_integer().filter(|value| (1..=64).contains(value)) {
            Some(value) => {
                advanced.max_concurrent_jobs = u16::try_from(value).unwrap_or(4);
                advanced_sources.insert("max_concurrent_jobs", FlagSource::File);
            }
            None => warnings.problem(
                "max_concurrent_jobs",
                "`max_concurrent_jobs` must be an integer from 1 to 64; ignoring the file value",
                None,
            ),
        },
        "mcp_connect_timeout_seconds" => {
            match item
                .as_integer()
                .filter(|value| (1..=600).contains(value))
            {
                Some(value) => {
                    advanced.mcp_connect_timeout_seconds = value.unsigned_abs();
                    advanced_sources.insert("mcp_connect_timeout_seconds", FlagSource::File);
                }
                None => warnings.problem(
                    "mcp_connect_timeout_seconds",
                    "`mcp_connect_timeout_seconds` must be an integer from 1 to 600; ignoring the file value",
                    None,
                ),
            }
        }
        _ => warnings.unknown(&key, "advanced"),
    }
}

/// Applies `AGENTUNA_FLAG_<NAME>` pairs.
fn apply_environment(
    environment: &[(String, String)],
    flags: &mut BTreeMap<&'static str, (bool, FlagSource)>,
    warnings: &mut WarningLog,
) {
    for (name, value) in environment {
        let Some(raw_key) = name.strip_prefix(ENV_FLAG_PREFIX) else {
            continue;
        };
        if raw_key.is_empty() {
            continue;
        }
        let key = normalize_key(raw_key);
        if SAFETY_FLAGS.contains(&key.as_str()) {
            warnings.safety(&key);
            continue;
        }
        let Some(parsed) = parse_bool(value) else {
            warnings.problem(
                &key,
                format!("{name} must be true or false; ignoring it"),
                None,
            );
            continue;
        };
        set_flag(&key, parsed, FlagSource::Environment, flags, warnings);
    }
}

/// Applies `--flag vendor:name=value` values.
fn apply_command_line(
    cli_flags: &[String],
    flags: &mut BTreeMap<&'static str, (bool, FlagSource)>,
    warnings: &mut WarningLog,
) {
    for raw in cli_flags {
        let (key, value) = match parse_cli_flag(raw) {
            Ok(parsed) => parsed,
            Err(message) => {
                warnings.problem(raw, message, None);
                continue;
            }
        };
        if SAFETY_FLAGS.contains(&key.as_str()) {
            warnings.safety(&key);
            continue;
        }
        let Some(parsed) = parse_bool(&value) else {
            warnings.problem(
                &key,
                format!("`--flag {raw}` must end in true or false; ignoring it"),
                None,
            );
            continue;
        };
        set_flag(&key, parsed, FlagSource::CommandLine, flags, warnings);
    }
}

/// Parses `[vendor:]name=value` into `(normalized name, raw value)`.
fn parse_cli_flag(raw: &str) -> Result<(String, String), String> {
    let (head, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("`--flag {raw}` is not of the form vendor:name=value"))?;
    let name = match head.split_once(':') {
        Some((vendor, name)) => {
            if !vendor.eq_ignore_ascii_case("agentuna") {
                return Err(format!(
                    "`--flag {raw}` names the unknown vendor `{vendor}`; only agentuna flags exist"
                ));
            }
            name
        }
        None => head,
    };
    if name.trim().is_empty() {
        return Err(format!("`--flag {raw}` has an empty flag name"));
    }
    Ok((normalize_key(name), value.to_owned()))
}

/// Stores one accepted flag value and records its source.
fn set_flag(
    key: &str,
    value: bool,
    source: FlagSource,
    flags: &mut BTreeMap<&'static str, (bool, FlagSource)>,
    warnings: &mut WarningLog,
) {
    match flags.get_mut(key) {
        Some(slot) => *slot = (value, source),
        None => warnings.unknown(key, "flags"),
    }
}

/// Reads a flag value from the resolved map, falling back to the compiled default.
fn flag_value(values: &BTreeMap<&'static str, (bool, FlagSource)>, key: &str) -> bool {
    values.get(key).map_or_else(
        || {
            FLAGS
                .iter()
                .find(|definition| definition.key == key)
                .is_some_and(|definition| definition.default)
        },
        |(value, _)| *value,
    )
}

/// Builds the serializable report from the resolved values.
fn build_report(
    path: &Path,
    flags: &BTreeMap<&'static str, (bool, FlagSource)>,
    advanced: &AdvancedConfig,
    advanced_sources: &BTreeMap<&'static str, FlagSource>,
    warnings: Vec<ConfigWarning>,
) -> ConfigReport {
    let flags = flags
        .iter()
        .filter_map(|(key, (value, source))| {
            let definition = FLAGS.iter().find(|definition| definition.key == *key)?;
            Some((
                (*key).to_owned(),
                FlagReport {
                    value: *value,
                    default: definition.default,
                    source: *source,
                    description: definition.summary.to_owned(),
                },
            ))
        })
        .collect();
    let source = |key: &str| {
        advanced_sources
            .get(key)
            .copied()
            .unwrap_or(FlagSource::Default)
    };
    let advanced = BTreeMap::from([
        (
            "log_level".to_owned(),
            ValueReport {
                value: serde_json::Value::String(advanced.log_level.as_str().to_owned()),
                default: serde_json::Value::String(LogLevel::Info.as_str().to_owned()),
                source: source("log_level"),
                description: "Log verbosity: error, warn, info, debug or trace".to_owned(),
            },
        ),
        (
            "max_concurrent_jobs".to_owned(),
            ValueReport {
                value: serde_json::Value::from(advanced.max_concurrent_jobs),
                default: serde_json::Value::from(AdvancedConfig::defaults().max_concurrent_jobs),
                source: source("max_concurrent_jobs"),
                description: "Jobs the background runtime runs at once (1-64)".to_owned(),
            },
        ),
        (
            "mcp_connect_timeout_seconds".to_owned(),
            ValueReport {
                value: serde_json::Value::from(advanced.mcp_connect_timeout_seconds),
                default: serde_json::Value::from(
                    AdvancedConfig::defaults().mcp_connect_timeout_seconds,
                ),
                source: source("mcp_connect_timeout_seconds"),
                description: "Timeout for one MCP connection attempt, in seconds (1-600)"
                    .to_owned(),
            },
        ),
    ]);
    ConfigReport {
        config_path: path.display().to_string(),
        requires_restart: true,
        restart_note: RESTART_NOTE.to_owned(),
        flags,
        advanced,
        warnings,
    }
}

/// Normalizes a key written by a human: trimmed, lowercase, dashes as underscores.
fn normalize_key(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('-', "_")
}

/// Parses the accepted boolean spellings.
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Collects warnings, at most one per key and problem, and finds a nearest-known suggestion.
#[derive(Debug, Default)]
struct WarningLog {
    entries: Vec<ConfigWarning>,
    seen: BTreeSet<String>,
}

impl WarningLog {
    fn entries(self) -> Vec<ConfigWarning> {
        self.entries
    }

    fn problem(&mut self, key: &str, message: impl Into<String>, suggestion: Option<String>) {
        let fingerprint = format!("problem:{key}");
        if !self.seen.insert(fingerprint) {
            return;
        }
        self.entries.push(ConfigWarning {
            key: key.to_owned(),
            message: message.into(),
            suggestion,
        });
    }

    fn unknown(&mut self, key: &str, section: &str) {
        self.problem(
            key,
            format!("unknown `{section}` key `{key}`; ignoring it"),
            nearest_known(key),
        );
    }

    fn safety(&mut self, key: &str) {
        self.problem(
            key,
            format!("{SAFETY_REJECTION}: `{key}` was ignored"),
            None,
        );
    }
}

/// The closest known flag key, when it is close enough to be a likely typo.
fn nearest_known(key: &str) -> Option<String> {
    let mut candidates: Vec<&str> = FLAGS
        .iter()
        .map(|definition| definition.key)
        .chain(SAFETY_FLAGS)
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(key, candidate), candidate))
        .filter(|(distance, _)| *distance <= 3)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate.to_owned())
}

/// Levenshtein distance with the two-row dynamic program.
fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_char != right_char);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests;
