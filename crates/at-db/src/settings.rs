//! Typed preferences over the `settings` table (TF-023).
//!
//! Every preference is one row: the canonical key and a JSON value. [`load`]
//! decodes all rows into [`Settings`], which exposes typed fields and the
//! documented defaults for keys that are absent — reading a fresh database never
//! writes a row. A stored value that cannot be decoded falls back to its default
//! with a `tracing::warn`; a row whose key is not canonical is preserved in
//! [`Settings::unknown`] and written back untouched by [`import`].
//!
//! Writes go through [`set_value`] or one of the typed setters. Each write runs
//! in one transaction: the previous value is read, a value that is already
//! current is left alone, and the row is upserted. A change to an audit-sensitive
//! key (anything except navigation state and banner dismissal) records one
//! `operations` row of kind `Settings` through [`crate::ops`], with the old and
//! new value in the summary and no file rows.
//!
//! [`export`] and [`import`] move every row between databases as one document.
//! Import validates every canonical key before writing anything, so one bad
//! value leaves the database unchanged and names the key that failed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use at_model::{AgentId, ItemKind, LinkMode, Scope};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::DbError;
use crate::ops::{OperationActor, OperationKind, record_completed};

pub use crate::retention::UNDO_WINDOW_DAYS;

// The undo window is a compile-time constant, not a preference: no code path can
// read it from the settings table.
const _: () = assert!(UNDO_WINDOW_DAYS == 30, "the undo window is 30 days");

/// Version of the stored preference payloads, reported as [`Settings::schema_version`].
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

/// Default for `health_interval_minutes`: a scheduled check every five minutes.
pub const DEFAULT_HEALTH_INTERVAL_MINUTES: u32 = 5;

/// Smallest accepted `backup_retention_days`; smaller values are clamped up.
pub const MIN_BACKUP_RETENTION_DAYS: u32 = 30;

/// Default for `backup_retention_days`.
pub const DEFAULT_BACKUP_RETENTION_DAYS: u32 = MIN_BACKUP_RETENTION_DAYS;

const SCHEMA_KEY: &str = "schema_version";
const LINK_MODE_PREFIX: &str = "link_mode.";
const BANNER_PREFIX: &str = "native_banner_hidden.";
const LAST_SCOPE_PREFIX: &str = "last_scope.";

const SELECT_ALL_SQL: &str = "SELECT key, value_json FROM settings ORDER BY key ASC";
const SELECT_ONE_SQL: &str = "SELECT value_json FROM settings WHERE key = ?1";
const UPSERT_SQL: &str = "\
INSERT INTO settings (key, value_json, updated_at) \
VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
ON CONFLICT(key) DO UPDATE SET \
    value_json = excluded.value_json, \
    updated_at = excluded.updated_at";
const DELETE_SQL: &str = "DELETE FROM settings WHERE key = ?1";

/// How often the background health scheduler runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum HealthInterval {
    /// Scheduled checks are off.
    Off,
    /// Every five minutes (the default).
    #[default]
    FiveMinutes,
    /// Every fifteen minutes.
    FifteenMinutes,
    /// Every sixty minutes.
    SixtyMinutes,
}

impl HealthInterval {
    /// Every accepted value, in menu order.
    pub const ALL: &'static [Self] = &[
        Self::Off,
        Self::FiveMinutes,
        Self::FifteenMinutes,
        Self::SixtyMinutes,
    ];

    /// Minutes between checks; `0` means off.
    #[must_use]
    pub const fn minutes(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::FiveMinutes => 5,
            Self::FifteenMinutes => 15,
            Self::SixtyMinutes => 60,
        }
    }

    /// The interval stored as `minutes`, or `None` for a value the setting rejects.
    #[must_use]
    pub fn from_minutes(minutes: u32) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|interval| interval.minutes() == minutes)
    }

    /// Whether scheduled checks are disabled.
    #[must_use]
    pub const fn is_off(self) -> bool {
        matches!(self, Self::Off)
    }
}

/// Which MCP servers scheduled background health checks may contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum BackgroundHealth {
    /// No scheduled checks.
    Off,
    /// Stdio servers that were tested once (the default): nothing leaves the machine.
    #[default]
    Local,
    /// Every server, including HTTP endpoints.
    All,
}

impl BackgroundHealth {
    /// Every accepted value, in menu order.
    pub const ALL: &'static [Self] = &[Self::Off, Self::Local, Self::All];

    /// Value stored in `background_health`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Local => "local",
            Self::All => "all",
        }
    }

    /// Parses a stored value; `None` when the setting rejects it.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|health| health.wire_name() == value)
    }
}

/// One canonical settings key.
///
/// Keys with a suffix (`link_mode.<agent>`, `native_banner_hidden.<kind>`,
/// `last_scope.<project_id>`) are stored as `prefix.suffix` and never as a
/// nested object, per decision 9 of the technical-story contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsKey {
    /// Version of the stored payloads.
    SchemaVersion,
    /// `health_interval_minutes`.
    HealthIntervalMinutes,
    /// `background_health`.
    BackgroundHealth,
    /// `backup_retention_days`.
    BackupRetentionDays,
    /// `default_link_mode`.
    DefaultLinkMode,
    /// `link_mode.<agent>`.
    LinkMode(AgentId),
    /// `native_banner_hidden.<kind>`.
    NativeBannerHidden(ItemKind),
    /// `global_store_path`.
    GlobalStorePath,
    /// `network.marketplace_enabled`.
    MarketplaceEnabled,
    /// `auto_update_skills`.
    AutoUpdateSkills,
    /// `last_scope.<project_id>`.
    LastScope(String),
    /// `last_project_id`.
    LastProjectId,
}

impl SettingsKey {
    /// Every fixed-name key, in declaration order.
    pub const FIXED: &'static [Self] = &[
        Self::SchemaVersion,
        Self::HealthIntervalMinutes,
        Self::BackgroundHealth,
        Self::BackupRetentionDays,
        Self::DefaultLinkMode,
        Self::GlobalStorePath,
        Self::MarketplaceEnabled,
        Self::AutoUpdateSkills,
        Self::LastProjectId,
    ];

    /// The exact string stored in `settings.key`.
    #[must_use]
    pub fn stored_key(&self) -> String {
        match self {
            Self::SchemaVersion => SCHEMA_KEY.to_owned(),
            Self::HealthIntervalMinutes => "health_interval_minutes".to_owned(),
            Self::BackgroundHealth => "background_health".to_owned(),
            Self::BackupRetentionDays => "backup_retention_days".to_owned(),
            Self::DefaultLinkMode => "default_link_mode".to_owned(),
            Self::LinkMode(agent) => format!("{LINK_MODE_PREFIX}{agent}"),
            Self::NativeBannerHidden(kind) => format!("{BANNER_PREFIX}{}", kind.wire_name()),
            Self::GlobalStorePath => "global_store_path".to_owned(),
            Self::MarketplaceEnabled => "network.marketplace_enabled".to_owned(),
            Self::AutoUpdateSkills => "auto_update_skills".to_owned(),
            Self::LastScope(project_id) => format!("{LAST_SCOPE_PREFIX}{project_id}"),
            Self::LastProjectId => "last_project_id".to_owned(),
        }
    }

    /// Parses a stored key.
    ///
    /// An unrecognised key is [`SettingsKeyError::Unknown`] and is preserved by
    /// callers; a key with a canonical prefix but an invalid suffix is
    /// [`SettingsKeyError::Malformed`] and is reported instead of preserved.
    ///
    /// # Errors
    /// [`SettingsKeyError::Unknown`] for a key outside the canonical set, and
    /// [`SettingsKeyError::Malformed`] for a canonical prefix with an invalid
    /// suffix.
    pub fn parse(stored: &str) -> Result<Self, SettingsKeyError> {
        let fixed = Self::FIXED
            .iter()
            .find(|key| key.stored_key() == stored)
            .cloned();
        if let Some(key) = fixed {
            return Ok(key);
        }
        if let Some(suffix) = stored.strip_prefix(LINK_MODE_PREFIX) {
            return match suffix.parse::<AgentId>() {
                Ok(agent) => Ok(Self::LinkMode(agent)),
                Err(error) => Err(SettingsKeyError::Malformed {
                    key: stored.to_owned(),
                    reason: error.to_string(),
                }),
            };
        }
        if let Some(suffix) = stored.strip_prefix(BANNER_PREFIX) {
            return match suffix.parse::<ItemKind>() {
                Ok(kind) => Ok(Self::NativeBannerHidden(kind)),
                Err(_) => Err(SettingsKeyError::Malformed {
                    key: stored.to_owned(),
                    reason: "expected skill, mcp_server or rule".to_owned(),
                }),
            };
        }
        if let Some(suffix) = stored.strip_prefix(LAST_SCOPE_PREFIX) {
            if suffix.is_empty() || suffix.chars().any(char::is_control) {
                return Err(SettingsKeyError::Malformed {
                    key: stored.to_owned(),
                    reason: "the project id must be non-empty and free of control characters"
                        .to_owned(),
                });
            }
            return Ok(Self::LastScope(suffix.to_owned()));
        }
        Err(SettingsKeyError::Unknown {
            key: stored.to_owned(),
        })
    }

    /// Whether a change to this key belongs in the operations log.
    ///
    /// Navigation state (`last_scope.*`, `last_project_id`), banner dismissal
    /// (`native_banner_hidden.*`) and the payload version do not: they change how
    /// the UI reopens, not what AgenTuna does.
    #[must_use]
    pub fn is_audited(&self) -> bool {
        !matches!(
            self,
            Self::SchemaVersion
                | Self::NativeBannerHidden(_)
                | Self::LastScope(_)
                | Self::LastProjectId
        )
    }
}

/// Why a stored key is not a canonical settings key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsKeyError {
    /// The key has no canonical name and is preserved rather than interpreted.
    #[error("`{key}` is not a canonical settings key")]
    Unknown {
        /// The stored key.
        key: String,
    },
    /// The key has a canonical prefix but an invalid suffix.
    #[error("`{key}` is malformed: {reason}")]
    Malformed {
        /// The stored key.
        key: String,
        /// What is wrong with the suffix.
        reason: String,
    },
}

/// Every preference, decoded with defaults applied.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Version of the stored payloads; [`SETTINGS_SCHEMA_VERSION`] when no row exists.
    pub schema_version: u32,
    /// How often scheduled health checks run.
    pub health_interval: HealthInterval,
    /// Which servers scheduled checks may contact.
    pub background_health: BackgroundHealth,
    /// Backups older than this many days are pruned; never below 30.
    pub backup_retention_days: u32,
    /// Deployment mode for an agent without a `link_mode.<agent>` row.
    pub default_link_mode: LinkMode,
    /// Per-agent deployment modes, keyed by the stored agent id.
    pub link_modes: BTreeMap<AgentId, LinkMode>,
    /// Per-kind "native item banner dismissed" flags; only `true` entries are stored.
    pub native_banner_hidden: BTreeMap<ItemKind, bool>,
    /// Configured Global store; `None` means "use the default".
    pub global_store_path: Option<PathBuf>,
    /// Whether marketplace requests are allowed.
    pub marketplace_enabled: bool,
    /// Whether installed skills may update themselves.
    pub auto_update_skills: bool,
    /// Last scope chosen per project id.
    pub last_scope: BTreeMap<String, Scope>,
    /// Last project opened, when one was.
    pub last_project_id: Option<String>,
    /// Stored rows whose key is not canonical, as raw JSON text, preserved verbatim.
    pub unknown: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            health_interval: HealthInterval::default(),
            background_health: BackgroundHealth::default(),
            backup_retention_days: DEFAULT_BACKUP_RETENTION_DAYS,
            default_link_mode: LinkMode::Link,
            link_modes: BTreeMap::new(),
            native_banner_hidden: BTreeMap::new(),
            global_store_path: None,
            marketplace_enabled: false,
            auto_update_skills: false,
            last_scope: BTreeMap::new(),
            last_project_id: None,
            unknown: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Deployment mode for `agent`: its `link_mode.<agent>` row, otherwise
    /// [`Settings::default_link_mode`].
    ///
    /// A custom agent's Copy default is written as an explicit row when the agent
    /// is created (TA-014), so this accessor needs no special case.
    #[must_use]
    pub fn link_mode(&self, agent: &AgentId) -> LinkMode {
        self.link_modes
            .get(agent)
            .copied()
            .unwrap_or(self.default_link_mode)
    }

    /// Whether the native-item banner is dismissed for `kind`.
    #[must_use]
    pub fn native_banner_hidden(&self, kind: ItemKind) -> bool {
        self.native_banner_hidden
            .get(&kind)
            .copied()
            .unwrap_or(false)
    }

    /// Last scope chosen for `project_id`, when one was stored.
    #[must_use]
    pub fn last_scope(&self, project_id: &str) -> Option<Scope> {
        self.last_scope.get(project_id).copied()
    }

    /// The configured Global store path, when one is stored.
    #[must_use]
    pub fn global_store_path(&self) -> Option<&Path> {
        self.global_store_path.as_deref()
    }
}

/// Why a settings read or write did not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsError {
    /// SQLite refused the read or write.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The value is not accepted for its key.
    #[error("settings key `{key}`: {reason}")]
    InvalidValue {
        /// The stored key.
        key: String,
        /// What the key accepts.
        reason: String,
    },
    /// The key has a canonical prefix but an invalid suffix.
    #[error("settings key `{key}`: {reason}")]
    InvalidKey {
        /// The rejected key.
        key: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A document was written by a newer build.
    #[error("settings document schema v{found} is newer than this build supports (v{supported})")]
    UnsupportedSchemaVersion {
        /// Version found in the document or row.
        found: u32,
        /// Highest version this build understands.
        supported: u32,
    },
    /// A value could not be encoded as JSON.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<rusqlite::Error> for SettingsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

/// A decoded value: what [`decode`] produces and [`encode`] and [`assign`] consume.
#[derive(Debug, Clone)]
enum Decoded {
    Schema(u32),
    HealthInterval(HealthInterval),
    BackgroundHealth(BackgroundHealth),
    RetentionDays(u32),
    LinkMode(LinkMode),
    BannerHidden(bool),
    StorePath(Option<PathBuf>),
    Flag(bool),
    Scope(Option<Scope>),
    Text(Option<String>),
}

/// Reads every stored preference, applying defaults and warnings.
///
/// Invalid values fall back to their default with a `tracing::warn`; the row is
/// left in the database so the next write can replace it. A fresh database reads
/// [`Settings::default`] without writing a row.
///
/// # Errors
/// [`SettingsError::Db`] when the table cannot be read.
pub fn load(connection: &Connection) -> Result<Settings, SettingsError> {
    let mut settings = Settings::default();
    let mut statement = connection.prepare(SELECT_ALL_SQL)?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (stored_key, value_json) = row?;
        let key = match SettingsKey::parse(&stored_key) {
            Ok(key) => key,
            Err(SettingsKeyError::Unknown { .. }) => {
                settings.unknown.insert(stored_key, value_json);
                continue;
            }
            Err(SettingsKeyError::Malformed { reason, .. }) => {
                tracing::warn!(
                    key = %stored_key,
                    %reason,
                    "ignoring a malformed settings key; the row is preserved"
                );
                continue;
            }
        };
        match decode(&key, &value_json) {
            Ok(decoded) => assign(&mut settings, &key, decoded),
            Err(error) => tracing::warn!(
                key = %stored_key,
                %error,
                "stored settings value is invalid; using the default"
            ),
        }
    }
    Ok(settings)
}

/// Writes one preference, validating and normalising it first.
///
/// The value is JSON: a number for `health_interval_minutes` and
/// `backup_retention_days`, a string for `background_health`,
/// `default_link_mode`, `link_mode.*`, `global_store_path`, `last_scope.*` and
/// `last_project_id`, and `true`/`false` for the flags and banners.
///
/// A change to an audited key ([`SettingsKey::is_audited`]) records one
/// `Settings` operations row with the old and new value in its summary. Writing a
/// value that is already current writes nothing, not even an audit row.
///
/// # Errors
/// [`SettingsError::InvalidValue`] when the key rejects the value,
/// [`SettingsError::InvalidKey`] for a malformed key, or [`SettingsError::Db`]
/// when the transaction fails.
pub fn set_value(
    connection: &mut Connection,
    key: &SettingsKey,
    value_json: &str,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    let normalized = encode(&decode(key, value_json)?)?;
    write(connection, key, Some(&normalized), actor)
}

/// Sets `health_interval_minutes`.
///
/// # Errors
/// A SQLite or audit failure.
pub fn set_health_interval(
    connection: &mut Connection,
    interval: HealthInterval,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    set_value(
        connection,
        &SettingsKey::HealthIntervalMinutes,
        &encode(&Decoded::HealthInterval(interval))?,
        actor,
    )
}

/// Sets `background_health`.
///
/// # Errors
/// A SQLite or audit failure.
pub fn set_background_health(
    connection: &mut Connection,
    health: BackgroundHealth,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    set_value(
        connection,
        &SettingsKey::BackgroundHealth,
        &encode(&Decoded::BackgroundHealth(health))?,
        actor,
    )
}

/// Sets `backup_retention_days` and returns the stored value.
///
/// A value below [`MIN_BACKUP_RETENTION_DAYS`] is clamped up to it with a
/// warning, so the returned value is what retention will use.
///
/// # Errors
/// A SQLite or audit failure.
pub fn set_backup_retention_days(
    connection: &mut Connection,
    days: u32,
    actor: OperationActor,
) -> Result<u32, SettingsError> {
    let stored = days.max(MIN_BACKUP_RETENTION_DAYS);
    if stored != days {
        tracing::warn!(
            requested = days,
            stored,
            "backup_retention_days is below the minimum; clamping"
        );
    }
    set_value(
        connection,
        &SettingsKey::BackupRetentionDays,
        &encode(&Decoded::RetentionDays(stored))?,
        actor,
    )?;
    Ok(stored)
}

/// Sets `default_link_mode`.
///
/// # Errors
/// A SQLite or audit failure.
pub fn set_default_link_mode(
    connection: &mut Connection,
    mode: LinkMode,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    set_value(
        connection,
        &SettingsKey::DefaultLinkMode,
        &encode(&Decoded::LinkMode(mode))?,
        actor,
    )
}

/// Sets `link_mode.<agent>`.
///
/// # Errors
/// A SQLite or audit failure.
pub fn set_link_mode(
    connection: &mut Connection,
    agent: &AgentId,
    mode: LinkMode,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    set_value(
        connection,
        &SettingsKey::LinkMode(agent.clone()),
        &encode(&Decoded::LinkMode(mode))?,
        actor,
    )
}

/// Sets or clears `native_banner_hidden.<kind>`.
///
/// `hidden = false` removes the row, which is what un-hiding a banner does.
///
/// # Errors
/// A SQLite failure.
pub fn set_native_banner_hidden(
    connection: &mut Connection,
    kind: ItemKind,
    hidden: bool,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    let key = SettingsKey::NativeBannerHidden(kind);
    if hidden {
        set_value(
            connection,
            &key,
            &encode(&Decoded::BannerHidden(true))?,
            actor,
        )
    } else {
        write(connection, &key, None, actor)
    }
}

/// Sets or clears `global_store_path`.
///
/// A relative path is rejected: TF-023 stores an absolute path or nothing.
/// `None` removes the row so AppPaths falls back to its default.
///
/// # Errors
/// [`SettingsError::InvalidValue`] for a relative or non-UTF-8 path, or a SQLite
/// or audit failure.
pub fn set_global_store_path(
    connection: &mut Connection,
    path: Option<&Path>,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    let key = SettingsKey::GlobalStorePath;
    match path {
        Some(path) => {
            let text = path.to_str().ok_or(SettingsError::InvalidValue {
                key: key.stored_key(),
                reason: "the path must be valid UTF-8".to_owned(),
            })?;
            set_value(
                connection,
                &key,
                &encode(&Decoded::StorePath(Some(PathBuf::from(text))))?,
                actor,
            )
        }
        None => write(connection, &key, None, actor),
    }
}

/// Sets `network.marketplace_enabled`.
///
/// # Errors
/// A SQLite or audit failure.
pub fn set_marketplace_enabled(
    connection: &mut Connection,
    enabled: bool,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    set_value(
        connection,
        &SettingsKey::MarketplaceEnabled,
        &encode(&Decoded::Flag(enabled))?,
        actor,
    )
}

/// Sets `auto_update_skills`.
///
/// # Errors
/// A SQLite or audit failure.
pub fn set_auto_update_skills(
    connection: &mut Connection,
    enabled: bool,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    set_value(
        connection,
        &SettingsKey::AutoUpdateSkills,
        &encode(&Decoded::Flag(enabled))?,
        actor,
    )
}

/// Sets or clears `last_scope.<project_id>`.
///
/// `None` removes the row: the project reopens on the default scope.
///
/// # Errors
/// [`SettingsError::InvalidValue`] for a project id that cannot address a row, or
/// a SQLite failure.
pub fn set_last_scope(
    connection: &mut Connection,
    project_id: &str,
    scope: Option<Scope>,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    let key = SettingsKey::LastScope(project_id.to_owned());
    // Reject an id that would produce a malformed key instead of writing it.
    SettingsKey::parse(&key.stored_key()).map_err(|error| SettingsError::InvalidKey {
        key: key.stored_key(),
        reason: error.to_string(),
    })?;
    match scope {
        Some(scope) => set_value(
            connection,
            &key,
            &encode(&Decoded::Scope(Some(scope)))?,
            actor,
        ),
        None => write(connection, &key, None, actor),
    }
}

/// Sets or clears `last_project_id`.
///
/// `None` removes the row.
///
/// # Errors
/// [`SettingsError::InvalidValue`] for a project id that cannot be stored, or a
/// SQLite failure.
pub fn set_last_project_id(
    connection: &mut Connection,
    project_id: Option<&str>,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    let key = SettingsKey::LastProjectId;
    match project_id {
        Some(project_id) => set_value(
            connection,
            &key,
            &encode(&Decoded::Text(Some(project_id.to_owned())))?,
            actor,
        ),
        None => write(connection, &key, None, actor),
    }
}

/// Every stored row as one versioned document, for export and CLI parity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsDocument {
    /// Version of the document layout; [`SETTINGS_SCHEMA_VERSION`] when absent.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Raw JSON text per stored key, in key order.
    pub values: BTreeMap<String, Box<RawValue>>,
}

const fn default_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

/// What [`import`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportReport {
    /// Canonical rows written.
    pub written: usize,
    /// Non-canonical rows preserved verbatim.
    pub preserved: usize,
}

/// Exports every stored row, `unknown` keys included.
///
/// Values are copied as stored, so an export followed by an import into an empty
/// database reproduces the rows byte for byte.
///
/// # Errors
/// [`SettingsError::Db`] when the table cannot be read, or
/// [`SettingsError::UnsupportedSchemaVersion`] when the stored payload version is
/// newer than this build.
pub fn export(connection: &Connection) -> Result<SettingsDocument, SettingsError> {
    let settings = load(connection)?;
    let mut values = BTreeMap::new();
    let mut statement = connection.prepare(SELECT_ALL_SQL)?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (key, value_json) = row?;
        values.insert(key, RawValue::from_string(value_json)?);
    }
    Ok(SettingsDocument {
        schema_version: settings.schema_version,
        values,
    })
}

/// Writes a document, replacing the rows it names.
///
/// Every canonical key is validated before the transaction opens, so a document
/// with one invalid value writes nothing and reports that key. Non-canonical keys
/// are preserved verbatim, and a document with a newer payload version is
/// refused.
///
/// When an audited key changes, one `Settings` operations row summarises the
/// import.
///
/// # Errors
/// [`SettingsError::InvalidValue`] / [`SettingsError::InvalidKey`] for the first
/// bad entry, [`SettingsError::UnsupportedSchemaVersion`] for a newer document,
/// or [`SettingsError::Db`] when the transaction fails.
pub fn import(
    connection: &mut Connection,
    document: &SettingsDocument,
    actor: OperationActor,
) -> Result<ImportReport, SettingsError> {
    if document.schema_version > SETTINGS_SCHEMA_VERSION {
        return Err(SettingsError::UnsupportedSchemaVersion {
            found: document.schema_version,
            supported: SETTINGS_SCHEMA_VERSION,
        });
    }
    let mut canonical: Vec<(String, String)> = Vec::new();
    let mut preserved: Vec<(String, String)> = Vec::new();
    for (stored_key, raw) in &document.values {
        match SettingsKey::parse(stored_key) {
            Ok(key) => {
                let normalized = encode(&decode(&key, raw.get())?)?;
                canonical.push((stored_key.clone(), normalized));
            }
            Err(SettingsKeyError::Unknown { .. }) => {
                preserved.push((stored_key.clone(), raw.get().to_owned()));
            }
            Err(SettingsKeyError::Malformed { reason, .. }) => {
                return Err(SettingsError::InvalidKey {
                    key: stored_key.clone(),
                    reason,
                });
            }
        }
    }

    let transaction = connection.transaction()?;
    let mut written = 0_usize;
    let mut audited = 0_usize;
    for (stored_key, value_json) in canonical.iter().chain(preserved.iter()) {
        let previous: Option<String> = transaction
            .query_row(SELECT_ONE_SQL, [stored_key], |row| row.get(0))
            .optional()?;
        if previous.as_deref() == Some(value_json.as_str()) {
            continue;
        }
        if SettingsKey::parse(stored_key).is_ok_and(|key| key.is_audited()) {
            audited += 1;
        }
        transaction.execute(UPSERT_SQL, params![stored_key, value_json])?;
        written += 1;
    }
    if audited > 0 {
        record_completed(
            &transaction,
            OperationKind::Settings,
            actor,
            Some(&format!("settings import: {audited} key(s) changed")),
        )?;
    }
    transaction.commit()?;
    Ok(ImportReport {
        written,
        preserved: preserved.len(),
    })
}

/// Upserts (`Some`) or removes (`None`) one row and records the audit entry.
fn write(
    connection: &mut Connection,
    key: &SettingsKey,
    value_json: Option<&str>,
    actor: OperationActor,
) -> Result<(), SettingsError> {
    let stored_key = key.stored_key();
    let transaction = connection.transaction()?;
    let previous: Option<String> = transaction
        .query_row(SELECT_ONE_SQL, [stored_key.as_str()], |row| row.get(0))
        .optional()?;
    let changed = previous.as_deref() != value_json;
    if changed {
        match value_json {
            Some(value_json) => {
                transaction.execute(UPSERT_SQL, params![stored_key, value_json])?;
            }
            None => {
                transaction.execute(DELETE_SQL, [stored_key.as_str()])?;
            }
        }
        if key.is_audited() {
            let before = previous
                .clone()
                .or_else(|| effective_default(key))
                .unwrap_or_else(|| "unset".to_owned());
            let after = value_json
                .map(str::to_owned)
                .or_else(|| effective_default(key))
                .unwrap_or_else(|| "unset".to_owned());
            record_completed(
                &transaction,
                OperationKind::Settings,
                actor,
                Some(&format!("{stored_key}: {before} -> {after}")),
            )?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Parses and validates the JSON stored for `key`.
fn decode(key: &SettingsKey, value_json: &str) -> Result<Decoded, SettingsError> {
    let value: serde_json::Value = serde_json::from_str(value_json)?;
    match key {
        SettingsKey::SchemaVersion => {
            let found = integer(key, &value)?;
            if found > SETTINGS_SCHEMA_VERSION {
                return Err(SettingsError::UnsupportedSchemaVersion {
                    found,
                    supported: SETTINGS_SCHEMA_VERSION,
                });
            }
            Ok(Decoded::Schema(found))
        }
        SettingsKey::HealthIntervalMinutes => {
            let minutes = integer(key, &value)?;
            HealthInterval::from_minutes(minutes)
                .map(Decoded::HealthInterval)
                .ok_or_else(|| invalid(key, "expected 0 (off), 5, 15 or 60 minutes"))
        }
        SettingsKey::BackgroundHealth => {
            let text = string(key, &value)?;
            BackgroundHealth::parse(text)
                .map(Decoded::BackgroundHealth)
                .ok_or_else(|| invalid(key, "expected off, local or all"))
        }
        SettingsKey::BackupRetentionDays => {
            let days = integer(key, &value)?;
            if days < MIN_BACKUP_RETENTION_DAYS {
                tracing::warn!(
                    stored = days,
                    minimum = MIN_BACKUP_RETENTION_DAYS,
                    "backup_retention_days is below the minimum; using the minimum"
                );
            }
            Ok(Decoded::RetentionDays(days.max(MIN_BACKUP_RETENTION_DAYS)))
        }
        SettingsKey::DefaultLinkMode | SettingsKey::LinkMode(_) => {
            let text = string(key, &value)?;
            text.parse::<LinkMode>()
                .ok()
                .map(Decoded::LinkMode)
                .ok_or_else(|| invalid(key, "expected link or copy"))
        }
        SettingsKey::NativeBannerHidden(_) => match value {
            serde_json::Value::Bool(hidden) => Ok(Decoded::BannerHidden(hidden)),
            _ => Err(invalid(key, "expected true or false")),
        },
        SettingsKey::GlobalStorePath => {
            let text = string(key, &value)?;
            if text.is_empty() {
                return Ok(Decoded::StorePath(None));
            }
            let path = PathBuf::from(text);
            if !path.is_absolute() {
                return Err(invalid(key, "expected an absolute path or an empty string"));
            }
            Ok(Decoded::StorePath(Some(path)))
        }
        SettingsKey::MarketplaceEnabled | SettingsKey::AutoUpdateSkills => match value {
            serde_json::Value::Bool(enabled) => Ok(Decoded::Flag(enabled)),
            _ => Err(invalid(key, "expected true or false")),
        },
        SettingsKey::LastScope(_) => {
            let text = string(key, &value)?;
            if text.is_empty() {
                return Ok(Decoded::Scope(None));
            }
            Scope::ALL
                .iter()
                .copied()
                .find(|scope| scope.wire_name() == text)
                .map(|scope| Decoded::Scope(Some(scope)))
                .ok_or_else(|| invalid(key, "expected global, user, project, local or empty"))
        }
        SettingsKey::LastProjectId => Ok(Decoded::Text(Some(string(key, &value)?.to_owned()))),
    }
}

/// The effective default for an audited key, as the JSON text it would store.
fn effective_default(key: &SettingsKey) -> Option<String> {
    let defaults = Settings::default();
    let decoded = match key {
        SettingsKey::HealthIntervalMinutes => Decoded::HealthInterval(defaults.health_interval),
        SettingsKey::BackgroundHealth => Decoded::BackgroundHealth(defaults.background_health),
        SettingsKey::BackupRetentionDays => Decoded::RetentionDays(defaults.backup_retention_days),
        SettingsKey::DefaultLinkMode => Decoded::LinkMode(defaults.default_link_mode),
        SettingsKey::LinkMode(agent) => Decoded::LinkMode(defaults.link_mode(agent)),
        SettingsKey::GlobalStorePath => Decoded::StorePath(defaults.global_store_path.clone()),
        SettingsKey::MarketplaceEnabled => Decoded::Flag(defaults.marketplace_enabled),
        SettingsKey::AutoUpdateSkills => Decoded::Flag(defaults.auto_update_skills),
        _ => return None,
    };
    encode(&decoded).ok()
}

/// Serialises a decoded value to the exact JSON text that is stored.
fn encode(decoded: &Decoded) -> Result<String, SettingsError> {
    let text = match decoded {
        Decoded::Schema(version) | Decoded::RetentionDays(version) => version.to_string(),
        Decoded::HealthInterval(interval) => interval.minutes().to_string(),
        Decoded::BackgroundHealth(health) => quoted(health.wire_name()),
        Decoded::LinkMode(mode) => quoted(mode.wire_name()),
        Decoded::BannerHidden(hidden) | Decoded::Flag(hidden) => hidden.to_string(),
        Decoded::StorePath(None) | Decoded::Scope(None) | Decoded::Text(None) => quoted(""),
        Decoded::StorePath(Some(path)) => {
            let text = path.to_str().ok_or_else(|| {
                SettingsError::Json(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the path is not valid UTF-8",
                )))
            })?;
            quoted(text)
        }
        Decoded::Scope(Some(scope)) => quoted(scope.wire_name()),
        Decoded::Text(Some(text)) => quoted(text),
    };
    Ok(text)
}

/// Stores a decoded value in the snapshot.
fn assign(settings: &mut Settings, key: &SettingsKey, decoded: Decoded) {
    match (key, decoded) {
        (SettingsKey::SchemaVersion, Decoded::Schema(version)) => {
            settings.schema_version = version;
        }
        (SettingsKey::HealthIntervalMinutes, Decoded::HealthInterval(interval)) => {
            settings.health_interval = interval;
        }
        (SettingsKey::BackgroundHealth, Decoded::BackgroundHealth(health)) => {
            settings.background_health = health;
        }
        (SettingsKey::BackupRetentionDays, Decoded::RetentionDays(days)) => {
            settings.backup_retention_days = days;
        }
        (SettingsKey::DefaultLinkMode, Decoded::LinkMode(mode)) => {
            settings.default_link_mode = mode;
        }
        (SettingsKey::LinkMode(agent), Decoded::LinkMode(mode)) => {
            settings.link_modes.insert(agent.clone(), mode);
        }
        (SettingsKey::NativeBannerHidden(kind), Decoded::BannerHidden(hidden)) => {
            settings.native_banner_hidden.insert(*kind, hidden);
        }
        (SettingsKey::GlobalStorePath, Decoded::StorePath(path)) => {
            settings.global_store_path = path;
        }
        (SettingsKey::MarketplaceEnabled, Decoded::Flag(enabled)) => {
            settings.marketplace_enabled = enabled;
        }
        (SettingsKey::AutoUpdateSkills, Decoded::Flag(enabled)) => {
            settings.auto_update_skills = enabled;
        }
        (SettingsKey::LastScope(project_id), Decoded::Scope(Some(scope))) => {
            settings.last_scope.insert(project_id.clone(), scope);
        }
        (SettingsKey::LastProjectId, Decoded::Text(project_id)) => {
            settings.last_project_id = project_id;
        }
        // `decode` never produces a value for a different key.
        _ => {}
    }
}

fn integer(key: &SettingsKey, value: &serde_json::Value) -> Result<u32, SettingsError> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| invalid(key, "expected a non-negative integer"))
}

fn string<'a>(key: &SettingsKey, value: &'a serde_json::Value) -> Result<&'a str, SettingsError> {
    value
        .as_str()
        .ok_or_else(|| invalid(key, "expected a JSON string"))
}

fn invalid(key: &SettingsKey, reason: &str) -> SettingsError {
    SettingsError::InvalidValue {
        key: key.stored_key(),
        reason: reason.to_owned(),
    }
}

fn quoted(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_window_is_a_constant_with_no_settings_row() {
        assert_eq!(UNDO_WINDOW_DAYS, 30);
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        let settings = load(connection).expect("fresh database loads");
        assert!(!settings.unknown.contains_key("undo_window_days"));
    }

    #[test]
    fn rejects_values_outside_the_accepted_sets() {
        for (key, value) in [
            (SettingsKey::HealthIntervalMinutes, "7"),
            (SettingsKey::HealthIntervalMinutes, "30"),
            (SettingsKey::BackgroundHealth, "\"sometimes\""),
            (
                SettingsKey::LastScope("prj_one".to_owned()),
                "\"workspace\"",
            ),
            (SettingsKey::GlobalStorePath, "\"relative/path\""),
        ] {
            assert!(
                matches!(decode(&key, value), Err(SettingsError::InvalidValue { .. })),
                "{key:?} must reject {value}"
            );
        }
    }

    #[test]
    fn reported_schema_version_is_the_document_version() {
        let document: SettingsDocument =
            serde_json::from_str(r#"{"values":{"health_interval_minutes":5}}"#)
                .expect("document parses");
        assert_eq!(document.schema_version, SETTINGS_SCHEMA_VERSION);
    }
}
