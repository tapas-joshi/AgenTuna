//! Zeroizing secret value wrapper.

use core::fmt;

use zeroize::Zeroize;

/// A secret value that zeroes its bytes on drop and never renders them.
///
/// `Debug` and `Display` always print `<redacted>`, so values cannot leak through
/// tracing, error chains, panic messages or accidental formatting. Read the plaintext
/// only at the point of use with [`SecretString::expose_secret`], and pass it through
/// an environment variable or request header, never a process argument.
pub struct SecretString(String);

impl SecretString {
    /// Wraps `value`.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the plaintext for the one operation that needs it.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Zeroize for SecretString {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "sk-live-sentinel-value";

    #[test]
    fn debug_and_display_are_redacted() {
        let secret = SecretString::new(SENTINEL);
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{secret}"), "<redacted>");
        assert!(!format!("{secret:?}{secret}").contains(SENTINEL));
    }

    #[test]
    fn expose_secret_returns_the_value() {
        let secret = SecretString::new(SENTINEL);
        assert_eq!(secret.expose_secret(), SENTINEL);
    }

    #[test]
    fn zeroize_clears_the_value() {
        let mut secret = SecretString::new(SENTINEL);
        secret.zeroize();
        assert!(secret.expose_secret().is_empty());
    }
}
