//! A `/`-separated path key for database storage, independent of the host platform (TF-037).
//!
//! SQLite stores a [`PathKey`] as UTF-8 text with forward slashes, so a database written on
//! Windows reads back correctly on macOS and the other way round. Conversion to the
//! platform-native path happens only at the boundary, with [`PathKey::to_native`].
//!
//! Never compare stored paths as strings on a case-insensitive filesystem: use
//! [`PathKey::equals`], which folds case on macOS and Windows and compares exactly on Linux.
//! The macOS rule follows the default APFS volume; a case-sensitive APFS volume would need a
//! separate flag.

use std::path::{Path, PathBuf};

/// Windows paths at or above this length use the `\\?\` extended-length form.
pub const WINDOWS_LONG_PATH_THRESHOLD: usize = 260;

/// Longest path the extended-length form can address.
pub const WINDOWS_MAX_PATH: usize = 32_767;

/// Platform whose path rules apply to a [`PathKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PortablePlatform {
    /// macOS: `/` separators, case-insensitive comparison by default.
    MacOs,
    /// Linux: `/` separators, case-sensitive comparison.
    Linux,
    /// Windows: `\` separators natively, case-insensitive comparison.
    Windows,
    /// Any other platform: Unix rules.
    Other,
}

impl PortablePlatform {
    /// Whether path comparison folds case on this platform.
    #[must_use]
    pub const fn is_case_insensitive(self) -> bool {
        matches!(self, Self::MacOs | Self::Windows)
    }
}

/// Why a path could not become a [`PathKey`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PathKeyError {
    /// The path is not absolute for its platform.
    #[error("{path}: path must be absolute")]
    NotAbsolute {
        /// The rejected text.
        path: String,
    },
    /// The path contains `..`, which could escape a store root.
    #[error("{path}: path must not contain `..`")]
    Traversal {
        /// The rejected text.
        path: String,
    },
    /// A Windows component ends with a dot or space, which Windows silently strips.
    #[error("{path}: Windows path components must not end with a dot or space")]
    TrailingDotOrSpace {
        /// The rejected text.
        path: String,
    },
    /// A Windows component uses a character or reserved name Windows rejects.
    #[error("{path}: not a valid Windows path component: {component}")]
    InvalidComponent {
        /// The rejected text.
        path: String,
        /// The component that is invalid.
        component: String,
    },
    /// The path is longer than the extended-length form can address.
    #[error("{path}: path exceeds {limit} characters")]
    TooLong {
        /// The rejected text.
        path: String,
        /// The platform limit.
        limit: usize,
    },
    /// The path is not valid UTF-8 and cannot be stored as database text.
    #[error("path is not valid UTF-8")]
    NotUtf8,
}

/// A `/`-separated, absolute path stored in the database.
///
/// Construction validates the path for the given platform and normalizes separators, `.`
/// components and repeated separators. The stored text always uses `/`; call
/// [`Self::to_native`] to get a [`PathBuf`] the operating system accepts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathKey(String);

impl PathKey {
    /// Parses a platform-native path string.
    ///
    /// Windows accepts both `\` and `/`; Unix accepts `/` only, because `\` is a legal file-name
    /// character there.
    ///
    /// # Errors
    /// [`PathKeyError`] for a relative path, `..`, an invalid Windows component or a path longer
    /// than [`WINDOWS_MAX_PATH`].
    pub fn from_text(text: &str, platform: PortablePlatform) -> Result<Self, PathKeyError> {
        match platform {
            PortablePlatform::Windows => windows_key(text),
            PortablePlatform::MacOs | PortablePlatform::Linux | PortablePlatform::Other => {
                unix_key(text)
            }
        }
    }

    /// Converts a native path.
    ///
    /// # Errors
    /// [`PathKeyError::NotUtf8`] when the path is not valid UTF-8, plus every error of
    /// [`Self::from_text`].
    pub fn from_path(path: &Path, platform: PortablePlatform) -> Result<Self, PathKeyError> {
        let text = path.to_str().ok_or(PathKeyError::NotUtf8)?;
        Self::from_text(text, platform)
    }

    /// The stored text, always `/`-separated.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The platform-native path.
    ///
    /// On Windows a path at or above [`WINDOWS_LONG_PATH_THRESHOLD`] gets the `\\?\` (or
    /// `\\?\UNC\`) prefix, so the OS never silently truncates it. Components ending in a dot or
    /// space were rejected at construction, so the extended form is safe.
    #[must_use]
    pub fn to_native(&self, platform: PortablePlatform) -> PathBuf {
        match platform {
            PortablePlatform::Windows => {
                if self.0.len() >= WINDOWS_LONG_PATH_THRESHOLD {
                    PathBuf::from(windows_extended_path(&self.0))
                } else {
                    PathBuf::from(self.0.replace('/', "\\"))
                }
            }
            PortablePlatform::MacOs | PortablePlatform::Linux | PortablePlatform::Other => {
                PathBuf::from(&self.0)
            }
        }
    }

    /// Platform-aware path equality.
    ///
    /// Folds case on macOS and Windows and compares exactly on Linux. Use this instead of `==`
    /// on the stored text.
    #[must_use]
    pub fn equals(&self, other: &Self, platform: PortablePlatform) -> bool {
        if platform.is_case_insensitive() {
            self.0.to_lowercase() == other.0.to_lowercase()
        } else {
            self.0 == other.0
        }
    }

    /// The last component; `None` for a root.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        if self.0.ends_with('/') {
            return None;
        }
        self.0.rsplit('/').find(|part| !part.is_empty())
    }

    /// The parent key, or `None` at a root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let trimmed = self.0.trim_end_matches('/');
        let index = trimmed.rfind('/')?;
        if index == 0 {
            Some(Self("/".to_owned()))
        } else {
            Some(Self(trimmed[..index].to_owned()))
        }
    }
}

fn unix_key(text: &str) -> Result<PathKey, PathKeyError> {
    if !text.starts_with('/') {
        return Err(PathKeyError::NotAbsolute {
            path: text.to_owned(),
        });
    }
    let mut parts: Vec<&str> = Vec::new();
    for segment in text.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                return Err(PathKeyError::Traversal {
                    path: text.to_owned(),
                });
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    Ok(PathKey(format!("/{joined}")))
}

fn windows_key(text: &str) -> Result<PathKey, PathKeyError> {
    let normalized = text.replace('\\', "/");
    if normalized.len() > WINDOWS_MAX_PATH {
        return Err(PathKeyError::TooLong {
            path: text.to_owned(),
            limit: WINDOWS_MAX_PATH,
        });
    }
    if normalized.starts_with("//") {
        return windows_unc_key(&normalized, text);
    }
    let mut segments = normalized.split('/').filter(|part| !part.is_empty());
    let Some(first) = segments.next() else {
        return Err(PathKeyError::NotAbsolute {
            path: text.to_owned(),
        });
    };
    if !(first.len() == 2 && first.as_bytes()[0].is_ascii_alphabetic() && first.ends_with(':')) {
        return Err(PathKeyError::NotAbsolute {
            path: text.to_owned(),
        });
    }
    let mut parts = vec![first.to_owned()];
    for segment in segments {
        push_windows_segment(&mut parts, segment, text)?;
    }
    // `C:` alone is drive-relative on Windows; the absolute key is `C:/`.
    if parts.len() == 1 {
        return Ok(PathKey(format!("{}/", parts[0])));
    }
    Ok(PathKey(parts.join("/")))
}

fn windows_unc_key(normalized: &str, text: &str) -> Result<PathKey, PathKeyError> {
    let mut segments = normalized.trim_start_matches('/').split('/');
    let (Some(server), Some(share)) = (segments.next(), segments.next()) else {
        return Err(PathKeyError::NotAbsolute {
            path: text.to_owned(),
        });
    };
    if server.is_empty() || share.is_empty() {
        return Err(PathKeyError::NotAbsolute {
            path: text.to_owned(),
        });
    }
    let mut parts = vec![format!("//{server}"), share.to_owned()];
    for segment in segments {
        push_windows_segment(&mut parts, segment, text)?;
    }
    Ok(PathKey(parts.join("/")))
}

fn push_windows_segment(
    parts: &mut Vec<String>,
    segment: &str,
    text: &str,
) -> Result<(), PathKeyError> {
    match segment {
        "" | "." => return Ok(()),
        ".." => {
            return Err(PathKeyError::Traversal {
                path: text.to_owned(),
            });
        }
        _ => {}
    }
    if segment.ends_with('.') || segment.ends_with(' ') {
        return Err(PathKeyError::TrailingDotOrSpace {
            path: text.to_owned(),
        });
    }
    if !segment
        .chars()
        .all(|c| !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') && !c.is_control())
    {
        return Err(PathKeyError::InvalidComponent {
            path: text.to_owned(),
            component: segment.to_owned(),
        });
    }
    if is_reserved_windows_name(segment) {
        return Err(PathKeyError::InvalidComponent {
            path: text.to_owned(),
            component: segment.to_owned(),
        });
    }
    parts.push(segment.to_owned());
    Ok(())
}

fn is_reserved_windows_name(segment: &str) -> bool {
    let stem = segment
        .split_once('.')
        .map_or(segment, |(stem, _extension)| stem);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// `\\?\C:\...` or `\\?\UNC\server\share\...` for a long `/`-separated Windows key.
fn windows_extended_path(key: &str) -> String {
    let backslashes = key.replace('/', "\\");
    if let Some(unc) = backslashes.strip_prefix("\\\\") {
        format!("\\\\?\\UNC\\{unc}")
    } else {
        format!("\\\\?\\{backslashes}")
    }
}
