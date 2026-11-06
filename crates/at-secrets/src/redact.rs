//! Session redaction of resolved secret values.
//!
//! [`Redactor`] holds the values resolved during the current session and masks them as
//! [`REDACTION_MASK`] wherever text leaves AgenTuna: captured stderr, error chains,
//! toasts, log lines and the panic hook. It implements [`SecretRedactor`], so
//! [`at_model::ErrorReport::redact_with`] applies it to a whole report.
//!
//! Redaction is defence in depth. The primary guarantee is that a value is never written
//! to disk or passed in a process argument in the first place; this catches the paths
//! where a child process, an error message or a log line echoes one back.

use core::fmt;

use at_model::error::{REDACTION_MASK, SecretRedactor};

use crate::secret::SecretString;

/// Masks the secret values resolved in this session.
///
/// Values are matched longest first, so a secret that contains another secret is masked
/// whole instead of leaving its tail visible. Entries are [`SecretString`]s and are
/// zeroed when the redactor drops; the type prints no value through `Debug`.
#[derive(Default)]
pub struct Redactor {
    /// Distinct non-empty values, longest first.
    values: Vec<SecretString>,
}

impl Redactor {
    /// A redactor that knows no values yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one resolved value.
    ///
    /// Empty values are ignored: masking an empty match would insert a mask at every
    /// position. Registering the same value twice keeps one entry.
    pub fn insert(&mut self, value: SecretString) {
        let plain = value.expose_secret();
        if plain.is_empty()
            || self
                .values
                .iter()
                .any(|known| known.expose_secret() == plain)
        {
            return;
        }
        self.values.push(value);
        self.values
            .sort_by_key(|value| core::cmp::Reverse(value.expose_secret().len()));
    }

    /// How many distinct values the redactor masks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the redactor knows no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Forgets every value, for example when the session ends.
    pub fn clear(&mut self) {
        self.values.clear();
    }

    /// Returns `text` with every known value replaced by [`REDACTION_MASK`].
    ///
    /// Text without a match is returned byte for byte, so redacting a clean log line
    /// changes nothing. The scan is single pass and left-to-right; at each position the
    /// longest matching value wins.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        if self.values.is_empty() || text.is_empty() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;
        while cursor < text.len() {
            let rest = &text[cursor..];
            let matched = self
                .values
                .iter()
                .find(|value| rest.starts_with(value.expose_secret()));
            if let Some(value) = matched {
                out.push_str(REDACTION_MASK);
                cursor += value.expose_secret().len();
                continue;
            }
            let Some(first) = rest.chars().next() else {
                break;
            };
            out.push(first);
            cursor += first.len_utf8();
        }
        out
    }
}

impl fmt::Debug for Redactor {
    /// Prints how many values are masked, never the values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("values", &self.values.len())
            .finish()
    }
}

impl SecretRedactor for Redactor {
    fn redact(&self, text: &str) -> String {
        Self::redact(self, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretString;
    use at_model::error::{ErrorKind, ErrorReport};

    const SENTINEL: &str = "sk-live-0123456789abcdef0123456789abcdef";

    fn redactor_with(values: &[&str]) -> Redactor {
        let mut redactor = Redactor::new();
        for value in values {
            redactor.insert(SecretString::new(*value));
        }
        redactor
    }

    #[test]
    fn masks_a_40_character_sentinel_three_times_in_a_64_kib_buffer() {
        assert_eq!(SENTINEL.len(), 40);
        let filler = "a".repeat(64 * 1024);
        let mut buffer = String::with_capacity(filler.len() + 3 * SENTINEL.len());
        buffer.push_str(&filler[..1024]);
        buffer.push_str(SENTINEL);
        buffer.push_str(&filler[1024..40_000]);
        buffer.push_str(SENTINEL);
        buffer.push_str(&filler[40_000..]);
        buffer.push_str(SENTINEL);

        let redactor = redactor_with(&[SENTINEL]);
        let redacted = redactor.redact(&buffer);

        assert!(!redacted.contains(SENTINEL));
        assert_eq!(redacted.matches(REDACTION_MASK).count(), 3);
        assert_eq!(
            redacted.len(),
            buffer.len() - 3 * (SENTINEL.len() - REDACTION_MASK.len())
        );
        let expected = buffer.replace(SENTINEL, REDACTION_MASK);
        assert_eq!(redacted, expected);
    }

    #[test]
    fn longest_value_wins_so_a_prefix_is_not_left_visible() {
        let redactor = redactor_with(&["secret", "secret-part"]);
        assert_eq!(redactor.redact("x secret-part y"), "x *** y");

        // The shorter value is still a value and is masked on its own.
        assert_eq!(redactor.redact("secret"), "***");
    }

    #[test]
    fn clean_text_keeps_its_bytes() {
        let redactor = redactor_with(&["sk-hidden"]);
        let text = "plain stderr line\nwith two lines ✓";
        assert_eq!(redactor.redact(text), text);
        assert!(Redactor::new().redact(text) == text);
    }

    #[test]
    fn empty_and_duplicate_values_are_ignored() {
        let mut redactor = Redactor::new();
        redactor.insert(SecretString::new(""));
        assert!(redactor.is_empty());
        redactor.insert(SecretString::new("value"));
        redactor.insert(SecretString::new("value"));
        assert_eq!(redactor.len(), 1);
        assert_eq!(redactor.redact("a value b"), "a *** b");
    }

    #[test]
    fn debug_never_prints_a_value() {
        let redactor = redactor_with(&[SENTINEL]);
        let printed = format!("{redactor:?}");
        assert!(!printed.contains(SENTINEL), "{printed}");
        assert!(printed.contains("values"), "{printed}");
    }

    #[test]
    fn redacts_an_error_report_through_the_trait() {
        let redactor = redactor_with(&[SENTINEL]);
        let mut report = ErrorReport::new(
            ErrorKind::Network,
            format!("request failed with token {SENTINEL}"),
        )
        .with_source(format!("header Authorization: Bearer {SENTINEL}"));

        report.redact_with(&redactor);

        let printed = format!("{report:?}");
        assert!(!printed.contains(SENTINEL), "{printed}");
        assert!(report.message.contains(REDACTION_MASK));
        assert!(report.source_chain[0].contains(REDACTION_MASK));
    }

    #[test]
    fn masks_across_utf8_boundaries_safely() {
        let redactor = redactor_with(&["ключ", "秘密"]);
        assert_eq!(redactor.redact("a ключ b 秘密 c"), "a *** b *** c");
    }
}
