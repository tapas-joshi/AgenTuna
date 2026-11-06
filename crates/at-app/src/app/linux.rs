//! Linux desktop integration: XDG install plan, `.desktop` entry and the Secret Service gate.
//!
//! Everything here is pure: every path is derived from an injected XDG root or home directory,
//! and no function reads the process environment or touches the filesystem. The write side is
//! `scripts/install-linux.sh`, which installs exactly the files [`desktop_install`] describes;
//! packaging as AppImage or `.deb` is a documented follow-up rather than part of this module.
//!
//! Secret handling has no plaintext fallback. [`detect_secret_backend`] classifies what the
//! session offers, and a save path must call [`SecretBackend::require`] *before* writing
//! anything; when no Secret Service provider owns `org.freedesktop.secrets`, the save fails with
//! [`SecretBackendUnavailable`]. Selecting and driving the actual backend stays with the
//! `at-secrets` crate, whose `SecretStore` trait is the only credential writer.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use at_store::{PlatformRootError, PortablePlatform, resolve_platform_roots};

/// Executable the desktop entry launches; `scripts/install-linux.sh` puts it on `PATH`.
pub const APP_BINARY: &str = "agentuna-app";

/// Icon name referenced by the desktop entry and used for every hicolor install name.
pub const ICON_NAME: &str = "agentuna";

/// File name of the desktop entry below `<data>/applications`.
pub const DESKTOP_FILE_NAME: &str = "agentuna.desktop";

/// The desktop entry installed for the app.
///
/// It declares only the fields the launcher and the desktop menu need. `Categories` is the
/// freedesktop `Development` main category; no MIME types or URL handlers are claimed because
/// the app opens its own project rather than files passed on the command line.
pub const DESKTOP_ENTRY: &str = "\
[Desktop Entry]
Type=Application
Version=1.5
Name=AgenTuna
GenericName=AI Configuration Studio
Comment=Manage agent skills, MCP servers and configuration across coding agents
Exec=agentuna-app
Icon=agentuna
Terminal=false
Categories=Development;
Keywords=agent;config;mcp;skills;
StartupNotify=true
";

/// One PNG icon copied into the hicolor theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconAsset {
    /// Icon size in pixels; also the hicolor directory name (`<size>x<size>`).
    pub size: u32,
    /// File name below `crates/at-app/icons`.
    pub source: &'static str,
}

/// Icons shipped with the app, smallest first.
///
/// The 256 px entry uses Tauri's `128x128@2x.png`, which is a 256x256 image.
pub const ICON_ASSETS: [IconAsset; 3] = [
    IconAsset {
        size: 32,
        source: "32x32.png",
    },
    IconAsset {
        size: 128,
        source: "128x128.png",
    },
    IconAsset {
        size: 256,
        source: "128x128@2x.png",
    },
];

/// Files a Linux desktop install writes below one XDG data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopInstall {
    /// `<data>/applications/agentuna.desktop`.
    pub desktop_file: PathBuf,
    /// `<data>/icons/hicolor/<size>x<size>/apps/agentuna.png`, one per [`ICON_ASSETS`] entry.
    pub icons: Vec<PathBuf>,
}

impl DesktopInstall {
    /// Every file the install writes, in the order `--uninstall` removes them.
    #[must_use]
    pub fn files(&self) -> Vec<&Path> {
        let mut files = Vec::with_capacity(self.icons.len() + 1);
        files.push(self.desktop_file.as_path());
        files.extend(self.icons.iter().map(PathBuf::as_path));
        files
    }
}

/// The `.desktop` entry bytes installed by `scripts/install-linux.sh`.
#[must_use]
pub fn desktop_entry() -> &'static str {
    DESKTOP_ENTRY
}

/// Computes the install plan for `data_home`, which is `<XDG_DATA_HOME>/agentuna`.
#[must_use]
pub fn desktop_install(data_home: &Path) -> DesktopInstall {
    let icons = ICON_ASSETS
        .iter()
        .map(|asset| icon_path(data_home, asset.size))
        .collect();
    DesktopInstall {
        desktop_file: data_home.join("applications").join(DESKTOP_FILE_NAME),
        icons,
    }
}

/// `<data>/icons/hicolor/<size>x<size>/apps/agentuna.png`.
#[must_use]
pub fn icon_path(data_home: &Path, size: u32) -> PathBuf {
    data_home
        .join("icons")
        .join("hicolor")
        .join(format!("{size}x{size}"))
        .join("apps")
        .join(format!("{ICON_NAME}.png"))
}

/// Resolves `<XDG_CONFIG_HOME>/agentuna`, the Linux support directory with the database.
///
/// The XDG rules themselves live in `at_store` (TF-037); this wrapper only selects the Linux
/// platform so callers cannot accidentally resolve macOS paths on a Linux host.
///
/// # Errors
/// [`PlatformRootError`] when neither an absolute `XDG_CONFIG_HOME` nor a home directory is
/// available.
pub fn xdg_config_dir(
    home: &Path,
    lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf, PlatformRootError> {
    resolve_platform_roots(PortablePlatform::Linux, Some(home), lookup)
        .map(|roots| roots.support_dir)
}

/// Resolves `<XDG_DATA_HOME>/agentuna`, the Linux local-state directory.
///
/// # Errors
/// [`PlatformRootError`] when neither an absolute `XDG_DATA_HOME` nor a home directory is
/// available.
pub fn xdg_data_dir(
    home: &Path,
    lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf, PlatformRootError> {
    resolve_platform_roots(PortablePlatform::Linux, Some(home), lookup)
        .map(|roots| roots.local_state_dir)
}

/// The credential store available to this Linux session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretBackend {
    /// A Secret Service provider (gnome-keyring, KWallet) owns `org.freedesktop.secrets`.
    SecretService,
    /// Nothing can hold a secret; callers must refuse the save.
    Unavailable(SecretBackendGap),
}

impl SecretBackend {
    /// Whether a credential store exists, so a secret may be written at all.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::SecretService)
    }

    /// The gate a save path calls before writing anything.
    ///
    /// # Errors
    /// [`SecretBackendUnavailable`] when no Secret Service provider is reachable. AgenTuna has
    /// no plaintext fallback, so the caller must abort rather than write elsewhere.
    pub fn require(self) -> Result<(), SecretBackendUnavailable> {
        match self {
            Self::SecretService => Ok(()),
            Self::Unavailable(gap) => Err(SecretBackendUnavailable { gap }),
        }
    }
}

/// Why the session cannot store secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SecretBackendGap {
    /// `DBUS_SESSION_BUS_ADDRESS` is unset or empty.
    #[error("DBUS_SESSION_BUS_ADDRESS is not set")]
    NoSessionBus,
    /// The session bus exists but nothing owns `org.freedesktop.secrets`.
    #[error("no process owns org.freedesktop.secrets on the session bus")]
    NoSecretServiceOwner,
}

/// No credential store is available, so no secret may be stored.
///
/// The `at-secrets` backend selection maps this gate to its own typed error (currently
/// `SecretsError::KeychainUnavailable`) when it runs on Linux; the important invariant here is
/// that no write happens before the gate passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "no Linux credential store is available ({gap}); install and start a Secret Service provider such as gnome-keyring or KWallet, then retry. AgenTuna never writes secrets to disk"
)]
#[non_exhaustive]
pub struct SecretBackendUnavailable {
    /// Which part of the Secret Service handshake failed.
    pub gap: SecretBackendGap,
}

/// Classifies the session from an injected bus address and the bus's owner of
/// `org.freedesktop.secrets`.
///
/// `secrets_owner` is whatever the D-Bus name-owner lookup returned; `None` means the name has
/// no owner. Empty or whitespace-only values count as absent.
#[must_use]
pub fn detect_secret_backend(
    session_bus: Option<&str>,
    secrets_owner: Option<&str>,
) -> SecretBackend {
    let bus = non_empty(session_bus);
    let owner = non_empty(secrets_owner);
    match (bus, owner) {
        (None, _) => SecretBackend::Unavailable(SecretBackendGap::NoSessionBus),
        (Some(_), None) => SecretBackend::Unavailable(SecretBackendGap::NoSecretServiceOwner),
        (Some(_), Some(_)) => SecretBackend::SecretService,
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(values: &'a [(&'a str, &'a str)]) -> impl FnMut(&str) -> Option<OsString> + 'a {
        move |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
        }
    }

    fn entry_value<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
        entry.lines().find_map(|line| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
        })
    }

    #[test]
    fn desktop_entry_declares_the_launcher_keys_the_menu_needs() {
        let entry = desktop_entry();
        assert!(entry.starts_with("[Desktop Entry]\n"));
        for line in entry.lines().skip(1) {
            assert!(!line.trim().is_empty(), "blank line in {entry:?}");
            assert!(line.contains('='), "line without `=`: {line:?}");
            assert!(!line.ends_with(' '), "trailing space in {line:?}");
        }

        assert_eq!(entry_value(entry, "Type"), Some("Application"));
        assert_eq!(entry_value(entry, "Name"), Some("AgenTuna"));
        assert_eq!(entry_value(entry, "Exec"), Some(APP_BINARY));
        assert_eq!(entry_value(entry, "Icon"), Some(ICON_NAME));
        assert_eq!(entry_value(entry, "Terminal"), Some("false"));
        assert_eq!(entry_value(entry, "Categories"), Some("Development;"));

        let keywords = entry_value(entry, "Keywords").expect("keywords");
        assert!(keywords.ends_with(';'), "Keywords must end with `;`");
        assert!(
            !entry_value(entry, "Exec").expect("exec").contains('/'),
            "the launcher resolves the binary through PATH"
        );
        for forbidden in [
            "allow-jit",
            "disable-library-validation",
            "allow-unsigned-executable-memory",
        ] {
            assert!(!entry.contains(forbidden));
        }
    }

    #[test]
    fn install_plan_lives_under_the_data_home() {
        let data_home = Path::new("/custom/data/agentuna");
        let plan = desktop_install(data_home);

        assert_eq!(
            plan.desktop_file,
            Path::new("/custom/data/agentuna/applications/agentuna.desktop")
        );
        assert_eq!(
            plan.icons,
            vec![
                PathBuf::from("/custom/data/agentuna/icons/hicolor/32x32/apps/agentuna.png"),
                PathBuf::from("/custom/data/agentuna/icons/hicolor/128x128/apps/agentuna.png"),
                PathBuf::from("/custom/data/agentuna/icons/hicolor/256x256/apps/agentuna.png"),
            ]
        );
        assert_eq!(plan.files().len(), ICON_ASSETS.len() + 1);
    }

    #[test]
    fn icon_assets_are_distinct_and_present_in_the_crate() {
        let icons = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons");
        let mut sizes: Vec<u32> = Vec::new();
        let mut sources: Vec<&str> = Vec::new();
        for asset in ICON_ASSETS {
            assert!(
                icons.join(asset.source).is_file(),
                "missing {}",
                asset.source
            );
            assert!(
                !sizes.contains(&asset.size),
                "duplicate size {}",
                asset.size
            );
            assert!(
                !sources.contains(&asset.source),
                "duplicate {}",
                asset.source
            );
            sizes.push(asset.size);
            sources.push(asset.source);
        }
        assert!(sizes.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(sizes, vec![32, 128, 256]);
    }

    #[test]
    fn xdg_directories_honour_absolute_overrides() {
        let home = Path::new("/home/tester");
        let data = xdg_data_dir(
            home,
            lookup(&[
                ("XDG_DATA_HOME", "/xdg/data"),
                ("XDG_CONFIG_HOME", "/xdg/config"),
            ]),
        )
        .expect("data dir");
        let config = xdg_config_dir(
            home,
            lookup(&[
                ("XDG_DATA_HOME", "/xdg/data"),
                ("XDG_CONFIG_HOME", "/xdg/config"),
            ]),
        )
        .expect("config dir");

        assert_eq!(data, Path::new("/xdg/data/agentuna"));
        assert_eq!(config, Path::new("/xdg/config/agentuna"));
    }

    #[test]
    fn xdg_directories_fall_back_to_the_home_relative_defaults() {
        let home = Path::new("/home/tester");
        let data = xdg_data_dir(home, lookup(&[])).expect("data dir");
        let config = xdg_config_dir(home, lookup(&[])).expect("config dir");

        assert_eq!(data, Path::new("/home/tester/.local/share/agentuna"));
        assert_eq!(config, Path::new("/home/tester/.config/agentuna"));
    }

    #[test]
    fn relative_xdg_values_are_ignored_as_the_spec_requires() {
        let home = Path::new("/home/tester");
        let data =
            xdg_data_dir(home, lookup(&[("XDG_DATA_HOME", "relative/data")])).expect("data dir");
        assert_eq!(data, Path::new("/home/tester/.local/share/agentuna"));
    }

    #[test]
    fn secret_service_requires_a_session_bus_and_an_owner() {
        let bus = Some("unix:path=/run/user/1000/bus");
        let owner = Some(":1.42");

        assert_eq!(
            detect_secret_backend(bus, owner),
            SecretBackend::SecretService
        );
        assert_eq!(
            detect_secret_backend(None, owner),
            SecretBackend::Unavailable(SecretBackendGap::NoSessionBus)
        );
        assert_eq!(
            detect_secret_backend(bus, None),
            SecretBackend::Unavailable(SecretBackendGap::NoSecretServiceOwner)
        );
        assert_eq!(
            detect_secret_backend(Some("   "), owner),
            SecretBackend::Unavailable(SecretBackendGap::NoSessionBus)
        );
        assert_eq!(
            detect_secret_backend(bus, Some("")),
            SecretBackend::Unavailable(SecretBackendGap::NoSecretServiceOwner)
        );
    }

    #[test]
    fn unavailable_backend_refuses_the_save_before_any_write() {
        let backend = detect_secret_backend(None, None);
        assert!(!backend.is_available());
        let error = backend.require().expect_err("no backend must refuse");
        assert_eq!(error.gap, SecretBackendGap::NoSessionBus);
        assert!(!error.to_string().is_empty());

        assert!(
            detect_secret_backend(Some("bus"), Some("owner"))
                .require()
                .is_ok()
        );
    }

    #[test]
    fn install_script_writes_the_same_desktop_entry() {
        const SCRIPT: &str = include_str!("../../../../scripts/install-linux.sh");
        const OPEN: &str = "<<'AGENTUNA_DESKTOP_ENTRY'\n";
        const CLOSE: &str = "\nAGENTUNA_DESKTOP_ENTRY\n";

        let start = SCRIPT.find(OPEN).expect("desktop-entry heredoc") + OPEN.len();
        let end = SCRIPT[start..]
            .find(CLOSE)
            .map(|offset| start + offset)
            .expect("desktop-entry heredoc terminator");
        let written = format!("{}\n", &SCRIPT[start..end]);

        assert_eq!(
            written, DESKTOP_ENTRY,
            "scripts/install-linux.sh and DESKTOP_ENTRY drifted apart"
        );
    }
}
