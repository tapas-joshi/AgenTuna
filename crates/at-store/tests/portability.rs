//! TF-037: `/`-separated path keys and per-platform root resolution, with injected env.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use at_store::{
    PathKey, PathKeyError, PlatformRootError, PlatformRoots, PortablePlatform,
    resolve_platform_roots,
};

fn env(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<OsString> {
    let pairs: Vec<(String, OsString)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

#[test]
fn a_windows_path_round_trips_through_the_forward_slash_key() {
    let key =
        PathKey::from_text(r"C:\Users\a\.agentuna\skills\x", PortablePlatform::Windows).unwrap();
    assert_eq!(key.as_str(), "C:/Users/a/.agentuna/skills/x");
    assert_eq!(
        key.to_native(PortablePlatform::Windows).to_string_lossy(),
        r"C:\Users\a\.agentuna\skills\x"
    );
    // The same key converted with Unix rules is a plain path with forward slashes.
    assert_eq!(
        key.to_native(PortablePlatform::Linux),
        PathBuf::from("C:/Users/a/.agentuna/skills/x")
    );

    let root = PathKey::from_text(r"C:\", PortablePlatform::Windows).unwrap();
    assert_eq!(root.as_str(), "C:/");
    assert_eq!(
        root.to_native(PortablePlatform::Windows).to_string_lossy(),
        r"C:\"
    );
    assert_eq!(root.file_name(), None);
}

#[test]
fn a_unix_path_round_trips_unchanged() {
    let key = PathKey::from_text("/Users/a/.agentuna/skills/x", PortablePlatform::MacOs).unwrap();
    assert_eq!(key.as_str(), "/Users/a/.agentuna/skills/x");
    assert_eq!(
        key.to_native(PortablePlatform::MacOs),
        PathBuf::from("/Users/a/.agentuna/skills/x")
    );
}

#[test]
fn path_comparison_follows_the_platform_case_rules() {
    struct Case {
        platform: PortablePlatform,
        left: &'static str,
        right: &'static str,
        equal: bool,
    }
    let cases = [
        Case {
            platform: PortablePlatform::Windows,
            left: "C:/Users/a/.agentuna",
            right: "C:/USERS/A/.agentuna",
            equal: true,
        },
        Case {
            platform: PortablePlatform::MacOs,
            left: "/Users/a/.agentuna",
            right: "/Users/A/.agentuna",
            equal: true,
        },
        Case {
            platform: PortablePlatform::Linux,
            left: "/Users/a/.agentuna",
            right: "/Users/A/.agentuna",
            equal: false,
        },
    ];
    for case in cases {
        let left = PathKey::from_text(case.left, case.platform).unwrap();
        let right = PathKey::from_text(case.right, case.platform).unwrap();
        assert_eq!(
            left.equals(&right, case.platform),
            case.equal,
            "{:?}: {} vs {}",
            case.platform,
            case.left,
            case.right
        );
    }
}

#[test]
fn invalid_windows_paths_are_rejected() {
    assert!(matches!(
        PathKey::from_text(r"relative\path", PortablePlatform::Windows),
        Err(PathKeyError::NotAbsolute { .. })
    ));
    assert!(matches!(
        PathKey::from_text(r"C:\Users\..\etc", PortablePlatform::Windows),
        Err(PathKeyError::Traversal { .. })
    ));
    assert!(matches!(
        PathKey::from_text(r"C:\Users\name.", PortablePlatform::Windows),
        Err(PathKeyError::TrailingDotOrSpace { .. })
    ));
    assert!(matches!(
        PathKey::from_text(r"C:\Users\name ", PortablePlatform::Windows),
        Err(PathKeyError::TrailingDotOrSpace { .. })
    ));
    assert!(matches!(
        PathKey::from_text(r"C:\Users\CON\skills", PortablePlatform::Windows),
        Err(PathKeyError::InvalidComponent { .. })
    ));
    assert!(matches!(
        PathKey::from_text(r"C:\Users\a?b", PortablePlatform::Windows),
        Err(PathKeyError::InvalidComponent { .. })
    ));
    // A trailing dot is legal on Unix and must not be rejected there.
    let unix = PathKey::from_text("/Users/a./skills", PortablePlatform::Linux).unwrap();
    assert_eq!(unix.as_str(), "/Users/a./skills");
}

#[test]
fn relative_unix_paths_and_traversal_are_rejected() {
    assert!(matches!(
        PathKey::from_text("Users/a", PortablePlatform::Linux),
        Err(PathKeyError::NotAbsolute { .. })
    ));
    assert!(matches!(
        PathKey::from_text("/Users/../etc", PortablePlatform::Linux),
        Err(PathKeyError::Traversal { .. })
    ));
}

#[test]
fn long_windows_paths_use_the_extended_length_prefix() {
    let long = format!("C:/Users/{}", "a".repeat(300));
    let key = PathKey::from_text(&long, PortablePlatform::Windows).unwrap();
    let native_path = key.to_native(PortablePlatform::Windows);
    let native = native_path.to_string_lossy();
    assert!(native.starts_with(r"\\?\C:\"), "{native}");
    assert!(native.ends_with(&"a".repeat(300)));

    let short = PathKey::from_text(r"C:\Users\a", PortablePlatform::Windows).unwrap();
    assert_eq!(
        short.to_native(PortablePlatform::Windows).to_string_lossy(),
        r"C:\Users\a"
    );
}

#[test]
fn unc_paths_keep_their_prefix_and_gain_unc_when_long() {
    let key = PathKey::from_text(r"\\server\share\skills\x", PortablePlatform::Windows).unwrap();
    assert_eq!(key.as_str(), "//server/share/skills/x");
    assert_eq!(
        key.to_native(PortablePlatform::Windows).to_string_lossy(),
        r"\\server\share\skills\x"
    );

    let long = format!("\\\\server\\share\\{}", "b".repeat(300));
    let key = PathKey::from_text(&long, PortablePlatform::Windows).unwrap();
    let native_path = key.to_native(PortablePlatform::Windows);
    let native = native_path.to_string_lossy();
    assert!(native.starts_with(r"\\?\UNC\server\share\"), "{native}");
}

#[test]
fn parent_and_file_name_work_on_the_stored_text() {
    let key = PathKey::from_text(r"C:\Users\a\skills\x", PortablePlatform::Windows).unwrap();
    assert_eq!(key.file_name(), Some("x"));
    assert_eq!(key.parent().unwrap().as_str(), "C:/Users/a/skills");
}

#[test]
fn macos_roots_match_the_documented_contract() {
    let roots = resolve_platform_roots(
        PortablePlatform::MacOs,
        Some(Path::new("/Users/a")),
        env(&[]),
    )
    .unwrap();
    assert_eq!(
        roots,
        PlatformRoots {
            home: PathBuf::from("/Users/a"),
            support_dir: PathBuf::from("/Users/a/Library/Application Support/AgenTuna"),
            local_state_dir: PathBuf::from("/Users/a/Library/Application Support/AgenTuna"),
            cache_dir: PathBuf::from("/Users/a/Library/Application Support/AgenTuna/cache"),
        }
    );
}

#[test]
fn linux_honours_xdg_overrides() {
    let roots = resolve_platform_roots(
        PortablePlatform::Linux,
        Some(Path::new("/home/a")),
        env(&[
            ("XDG_CONFIG_HOME", "/custom/config"),
            ("XDG_DATA_HOME", "/custom/data"),
            ("XDG_CACHE_HOME", "/custom/cache"),
        ]),
    )
    .unwrap();
    assert_eq!(roots.support_dir, PathBuf::from("/custom/config/agentuna"));
    assert_eq!(
        roots.local_state_dir,
        PathBuf::from("/custom/data/agentuna")
    );
    assert_eq!(roots.cache_dir, PathBuf::from("/custom/cache/agentuna"));
}

#[test]
fn linux_falls_back_to_the_home_dot_directories() {
    let roots = resolve_platform_roots(
        PortablePlatform::Linux,
        Some(Path::new("/home/a")),
        env(&[("XDG_CONFIG_HOME", "relative/ignored")]),
    )
    .unwrap();
    assert_eq!(roots.support_dir, PathBuf::from("/home/a/.config/agentuna"));
    assert_eq!(
        roots.local_state_dir,
        PathBuf::from("/home/a/.local/share/agentuna")
    );
    assert_eq!(roots.cache_dir, PathBuf::from("/home/a/.cache/agentuna"));
}

#[test]
fn windows_uses_appdata_and_localappdata() {
    let roots = resolve_platform_roots(
        PortablePlatform::Windows,
        None,
        env(&[
            ("USERPROFILE", r"C:\Users\a"),
            ("APPDATA", r"C:\Users\a\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\a\AppData\Local"),
        ]),
    )
    .unwrap();
    assert_eq!(roots.home, PathBuf::from(r"C:\Users\a"));
    assert_eq!(
        roots.support_dir,
        PathBuf::from(r"C:\Users\a\AppData\Roaming\AgenTuna")
    );
    assert_eq!(
        roots.local_state_dir,
        PathBuf::from(r"C:\Users\a\AppData\Local\AgenTuna")
    );
    assert_eq!(
        roots.cache_dir,
        PathBuf::from(r"C:\Users\a\AppData\Local\AgenTuna\cache")
    );
}

#[test]
fn windows_requires_appdata_and_localappdata() {
    let error = resolve_platform_roots(
        PortablePlatform::Windows,
        Some(Path::new(r"C:\Users\a")),
        env(&[("LOCALAPPDATA", r"C:\Users\a\AppData\Local")]),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PlatformRootError::MissingVariable {
            variable: "APPDATA"
        }
    ));

    let error = resolve_platform_roots(
        PortablePlatform::Windows,
        Some(Path::new(r"C:\Users\a")),
        env(&[("APPDATA", r"C:\Users\a\AppData\Roaming")]),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PlatformRootError::MissingVariable {
            variable: "LOCALAPPDATA"
        }
    ));
}

#[test]
fn a_missing_home_is_a_typed_error() {
    let error = resolve_platform_roots(PortablePlatform::Linux, None, env(&[])).unwrap_err();
    assert!(matches!(error, PlatformRootError::HomeUnavailable));
}
