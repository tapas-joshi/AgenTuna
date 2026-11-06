//! Secret redaction for text that leaves AgenTuna.
//!
//! [`Redactor`] is a pure function object: it holds the values and names it was built
//! with and turns text into text. It has no IO, no clock and no dependency on
//! `at-secrets`, so the same instance runs on log lines, error reports, JSON exports
//! and `Debug` output.
//!
//! One left-to-right pass, no regular expressions, no backtracking. It masks:
//!
//! 1. every known value of at least [`MIN_VALUE_LEN`] bytes;
//! 2. the value in an environment assignment `NAME=value` when `NAME` is a known
//!    secret name — including values shorter than [`MIN_VALUE_LEN`], because the name
//!    proves the text is a secret;
//! 3. token shapes that identify a credential by its prefix: GitHub (`ghp_`, `gho_`,
//!    `ghu_`, `ghs_`, `ghr_`, `github_pat_`), GitLab (`glpat-`), OpenAI and Anthropic
//!    (`sk-`), Stripe (`sk_live_`, `sk_test_`, `rk_live_`, `rk_test_`), Slack (`xoxa-`,
//!    `xoxb-`, `xoxp-`, `xoxr-`, `xoxs-`), AWS (`AKIA`, `ASIA`), Google (`AIza`), npm,
//!    PyPI, Docker Hub, Hugging Face, `Authorization: Bearer …` and JSON Web Tokens;
//! 4. the password in a URL with credentials, `https://user:password@host`.
//!
//! It never changes a `${VAR}` or `${VAR:-default}` reference, the secret name on its
//! own, or text without a match. A reference is copied verbatim, so a value that
//! appears inside one stays: a reference is a placeholder, not a value.
//!
//! # Composition with `at-secrets`
//!
//! `at-secrets::Redactor` holds the session's zeroizing `SecretString` values and
//! masks them wherever they appear; this type is the scanner for everything that is
//! written out. Build one `Redactor` per session from the `secret_refs` names and the
//! resolved values, then pass it as `&dyn SecretRedactor` to
//! [`crate::error::ErrorReport::redact_with`]:
//!
//! ```
//! use at_model::redact::Redactor;
//!
//! let mut redactor = Redactor::new();
//! redactor.insert_name("GITHUB_TOKEN");
//! redactor.insert_value("sk-live-0123456789abcdef");
//!
//! assert_eq!(redactor.redact_text("GITHUB_TOKEN=abc"), "GITHUB_TOKEN=***");
//! assert_eq!(
//!     redactor.redact_text("auth sk-live-0123456789abcdef"),
//!     "auth ***"
//! );
//! assert_eq!(
//!     redactor.redact_text("Use ${GITHUB_TOKEN}"),
//!     "Use ${GITHUB_TOKEN}"
//! );
//! ```
//!
//! The two agree on values of six bytes or more. `at-secrets` additionally masks
//! shorter values because it holds only real secrets; this type masks a short value
//! only in the `NAME=value` form, so it never replaces an ordinary word. When both run
//! over the same text the result is the union of their masks; running this one last
//! adds the shape rules.
//!
//! `at-model` stores the values it is given as plain `String`s. Drop the redactor when
//! the session ends instead of keeping it for the process lifetime.

use core::fmt;

use serde_json::Value;

use crate::error::{REDACTION_MASK, SecretRedactor};

/// Shortest known value that is masked wherever it appears.
///
/// Shorter values are masked only in the `NAME=value` form, where the name proves the
/// text is a secret. Masking a bare short string would replace ordinary words in every
/// log line.
pub const MIN_VALUE_LEN: usize = 6;

/// Longest `${...}` reference copied verbatim, in bytes.
const MAX_REFERENCE_LEN: usize = 512;

/// Longest identifier considered as a secret name, in bytes.
const MAX_NAME_LEN: usize = 255;

/// Longest assignment value scanned before the rule switches to end of line.
const MAX_ASSIGNMENT_VALUE_LEN: usize = 16 * 1024;

/// Longest URL userinfo scanned for `user:password@`, in bytes.
const MAX_USERINFO_LEN: usize = 256;

/// Longest JSON Web Token scanned, in bytes.
const MAX_JWT_LEN: usize = 4096;

/// JSON nesting walked before the rest of a subtree is masked.
const MAX_JSON_DEPTH: usize = 64;

/// Shortest token body accepted after a credential prefix.
const MIN_TOKEN_BODY: usize = 8;

/// Masks known values, secret names and credential shapes in text.
///
/// # Examples
///
/// ```
/// use at_model::redact::Redactor;
///
/// let redactor = Redactor::new();
/// assert_eq!(redactor.redact_text("ghp_0123456789abcdef"), "***");
/// assert_eq!(redactor.redact_text("no secrets here"), "no secrets here");
/// ```
pub struct Redactor {
    /// Known values, at least [`MIN_VALUE_LEN`] bytes each, longest first.
    values: Vec<String>,
    /// Secret names whose `NAME=value` assignments are masked.
    names: Vec<String>,
    /// Bytes that can start a match; everything else is copied in runs.
    interesting: [bool; 256],
    /// First bytes of the known values.
    value_starts: [bool; 256],
    /// First bytes of the secret names.
    name_starts: [bool; 256],
}

/// One mask to apply: copy `text[cursor..keep]`, write the mask, resume at `end`.
#[derive(Debug, Clone, Copy)]
struct Mask {
    /// End of the text copied verbatim before the mask.
    keep: usize,
    /// First byte after the masked region.
    end: usize,
}

impl Redactor {
    /// A redactor that knows no values or names but still masks credential shapes.
    #[must_use]
    pub fn new() -> Self {
        let mut interesting = [false; 256];
        interesting[usize::from(b'$')] = true;
        interesting[usize::from(b':')] = true;
        for byte in SHAPE_STARTS {
            interesting[usize::from(*byte)] = true;
        }
        Self {
            values: Vec::new(),
            names: Vec::new(),
            interesting,
            value_starts: [false; 256],
            name_starts: [false; 256],
        }
    }

    /// Registers every value in `values` that is long enough to mask.
    #[must_use]
    pub fn with_values<'a>(mut self, values: impl IntoIterator<Item = &'a str>) -> Self {
        for value in values {
            self.insert_value(value);
        }
        self
    }

    /// Registers every name in `names`.
    #[must_use]
    pub fn with_names<'a>(mut self, names: impl IntoIterator<Item = &'a str>) -> Self {
        for name in names {
            self.insert_name(name);
        }
        self
    }

    /// Registers one known value.
    ///
    /// Returns whether the value was stored. Empty values and values shorter than
    /// [`MIN_VALUE_LEN`] are rejected: masking them everywhere would replace ordinary
    /// words. Such a value is still masked in the `NAME=value` form when its name is
    /// registered with [`Redactor::insert_name`].
    pub fn insert_value(&mut self, value: &str) -> bool {
        if value.len() < MIN_VALUE_LEN || self.values.iter().any(|known| known == value) {
            return false;
        }
        // A valid `str` never starts with a UTF-8 continuation byte, so this index is
        // always the start of a character and the byte run in `scan` stays on a
        // boundary.
        self.value_starts[usize::from(value.as_bytes()[0])] = true;
        self.interesting[usize::from(value.as_bytes()[0])] = true;
        self.values.push(value.to_owned());
        self.values
            .sort_unstable_by_key(|known| core::cmp::Reverse(known.len()));
        true
    }

    /// Registers one secret name.
    ///
    /// Returns whether the name was stored. The name is compared byte for byte, so a
    /// name must start with an ASCII letter or underscore to match an assignment; an
    /// environment name always does.
    pub fn insert_name(&mut self, name: &str) -> bool {
        if name.is_empty() || self.names.iter().any(|known| known == name) {
            return false;
        }
        self.interesting[usize::from(name.as_bytes()[0])] = true;
        self.name_starts[usize::from(name.as_bytes()[0])] = true;
        self.names.push(name.to_owned());
        true
    }

    /// How many distinct values are masked.
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    /// How many secret names are registered.
    #[must_use]
    pub fn name_count(&self) -> usize {
        self.names.len()
    }

    /// Whether no value and no name is registered.
    ///
    /// Credential shapes are masked regardless, so an empty redactor still protects
    /// text that contains a token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.names.is_empty()
    }

    /// The registered secret names, for a developer-mode diagnostic.
    ///
    /// Names are labels, not values, so they may be listed. Values are not exposed.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.names.iter().map(String::as_str)
    }

    /// Returns `text` with every match replaced by [`REDACTION_MASK`].
    #[must_use]
    pub fn redact_text(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let _masked = self.scan(text, Some(&mut out));
        out
    }

    /// Whether `text` contains a known value, a named assignment or a token shape.
    ///
    /// A `${VAR}` reference alone is not a secret, so it returns `false`.
    #[must_use]
    pub fn contains_secret(&self, text: &str) -> bool {
        self.scan(text, None)
    }

    /// Redacts every string in `value`, returning a new document.
    ///
    /// An object member whose key is a registered secret name is masked whole — the
    /// JSON form of `NAME=value`. A string that is exactly a `${VAR}` reference is
    /// kept, because it holds no value.
    #[must_use]
    pub fn redact_json(&self, value: &Value) -> Value {
        let mut redacted = value.clone();
        self.redact_json_in_place(&mut redacted);
        redacted
    }

    /// Redacts every string in `value` in place.
    ///
    /// Keys are left alone: they are labels, and rewriting one would break the shape
    /// of the document for its consumer. A subtree nested deeper than 64 levels is
    /// replaced by [`REDACTION_MASK`] instead of being walked.
    pub fn redact_json_in_place(&self, value: &mut Value) {
        self.redact_value(value, 0);
    }

    /// Formats `value` with `Debug` and redacts the result.
    ///
    /// Use it instead of `format!("{value:?}")` for any value that may hold a secret:
    /// a derived `Debug` prints every field.
    #[must_use]
    pub fn redact_debug<T>(&self, value: &T) -> String
    where
        T: fmt::Debug + ?Sized,
    {
        self.redact_text(&format!("{value:?}"))
    }

    /// Whether `text` matches at `cursor` and where the mask starts and ends.
    fn match_at(&self, text: &[u8], cursor: usize) -> Option<Mask> {
        let byte = text[cursor];
        if byte == b':' {
            return url_credentials(text, cursor);
        }
        if self.name_starts[usize::from(byte)]
            && let Some(mask) = self.env_assignment(text, cursor)
        {
            return Some(mask);
        }
        if self.value_starts[usize::from(byte)] {
            let rest = &text[cursor..];
            for value in &self.values {
                let candidate = value.as_bytes();
                // The first byte is known to match from the table; reject on the
                // second before the slice comparison.
                if rest.get(1) != candidate.get(1) || !rest.starts_with(candidate) {
                    continue;
                }
                return Some(Mask {
                    keep: cursor,
                    end: cursor + value.len(),
                });
            }
        }
        if matches!(byte, b'B' | b'b')
            && let Some(mask) = bearer_at(text, cursor)
        {
            return Some(mask);
        }
        shape_at(text, cursor)
    }

    /// The `NAME=value` rule, when `NAME` is a registered secret name.
    fn env_assignment(&self, text: &[u8], cursor: usize) -> Option<Mask> {
        if self.names.is_empty() || !is_ident_start(text[cursor]) {
            return None;
        }
        if cursor > 0 && is_ident_byte(text[cursor - 1]) {
            return None;
        }
        let name_limit = (cursor + MAX_NAME_LEN).min(text.len());
        let mut name_end = cursor + 1;
        while name_end < name_limit && is_ident_byte(text[name_end]) {
            name_end += 1;
        }
        if name_end >= text.len()
            || text[name_end] != b'='
            || !self.is_secret_name(&text[cursor..name_end])
        {
            return None;
        }
        // `NAME==other` is a comparison, not an assignment.
        if text.get(name_end + 1) == Some(&b'=') {
            return None;
        }
        let value_start = name_end + 1;
        let quoted = text
            .get(value_start)
            .is_some_and(|byte| matches!(byte, b'"' | b'\''));
        let keep = if quoted { value_start + 1 } else { value_start };
        let ends = |byte: u8| {
            if quoted {
                matches!(byte, b'"' | b'\'' | b'\n' | b'\r')
            } else {
                is_value_terminator(byte)
            }
        };
        let limit = (keep + MAX_ASSIGNMENT_VALUE_LEN).min(text.len());
        let mut value_end = keep;
        while value_end < limit && !ends(text[value_end]) {
            value_end += 1;
        }
        if value_end == limit && limit < text.len() && !ends(text[value_end]) {
            // A value longer than the working window: mask to the end of the line in
            // one scan. Templates are short, so no reference is expected past the
            // window; extending keeps the work linear and the tail masked.
            while value_end < text.len() && !matches!(text[value_end], b'\n' | b'\r') {
                value_end += 1;
            }
        }
        if value_end <= keep {
            return None;
        }
        if value_end <= limit && contains_reference(&text[keep..value_end]) {
            return None;
        }
        Some(Mask {
            keep,
            end: value_end,
        })
    }

    /// Whether `name` is one of the registered secret names.
    fn is_secret_name(&self, name: &[u8]) -> bool {
        self.names.iter().any(|known| known.as_bytes() == name)
    }

    /// Walks `text` once, writing the redacted form to `out` when one is given.
    ///
    /// Returns whether any match was found. Every byte is examined a bounded number of
    /// times, so the walk is O(n) with a small constant and no backtracking.
    fn scan(&self, text: &str, mut out: Option<&mut String>) -> bool {
        let bytes = text.as_bytes();
        let mut cursor = 0usize;
        // Start of the bytes not written yet. A candidate that does not match simply
        // stays inside this range, so clean text is copied in runs instead of a
        // character at a time.
        let mut pending = 0usize;
        let mut masked = false;
        while cursor < bytes.len() {
            while cursor < bytes.len() && !self.interesting[usize::from(bytes[cursor])] {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                break;
            }
            if let Some(end) = reference_end(bytes, cursor) {
                if let Some(buffer) = out.as_mut() {
                    buffer.push_str(&text[pending..end]);
                }
                pending = end;
                cursor = end;
                continue;
            }
            if let Some(mask) = self.match_at(bytes, cursor) {
                masked = true;
                if out.is_none() {
                    return true;
                }
                if let Some(buffer) = out.as_mut() {
                    buffer.push_str(&text[pending..mask.keep]);
                    buffer.push_str(REDACTION_MASK);
                }
                cursor = mask.end;
                pending = cursor;
                continue;
            }
            // Not a match: the byte stays in the verbatim range. Every byte that is
            // marked interesting starts a character, so moving on keeps the range
            // boundaries valid.
            cursor += 1;
        }
        if let Some(buffer) = out.as_mut() {
            buffer.push_str(&text[pending..]);
        }
        masked
    }

    /// Recursive half of [`Redactor::redact_json_in_place`].
    fn redact_value(&self, value: &mut Value, depth: usize) {
        if depth > MAX_JSON_DEPTH && matches!(value, Value::Array(_) | Value::Object(_)) {
            *value = Value::String(REDACTION_MASK.to_owned());
            return;
        }
        match value {
            Value::String(text) => {
                let redacted = self.redact_text(text);
                if redacted != *text {
                    *text = redacted;
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.redact_value(item, depth + 1);
                }
            }
            Value::Object(members) => {
                for (key, member) in members.iter_mut() {
                    if self.is_secret_name(key.as_bytes()) {
                        match member {
                            Value::String(text) if is_reference(text) => {}
                            other => *other = Value::String(REDACTION_MASK.to_owned()),
                        }
                    } else {
                        self.redact_value(member, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Redactor {
    /// Prints how many values are masked and which names are known, never a value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("values", &self.values.len())
            .field("names", &self.names)
            .finish()
    }
}

impl SecretRedactor for Redactor {
    fn redact(&self, text: &str) -> String {
        Self::redact_text(self, text)
    }
}

/// First bytes of every credential shape, used to seed the interesting-byte table.
const SHAPE_STARTS: &[u8] = b"gsrxnhpdAeBb";

/// Which byte set a token body uses.
#[derive(Clone, Copy)]
enum Body {
    /// Letters, digits and the URL-safe punctuation a token may use.
    Token,
    /// Upper-case letters and digits only, the AWS key id shape.
    Upper,
}

/// Matches a prefixed token at `cursor`.
///
/// The prefixes are slice patterns, so each candidate compiles to a length check and a
/// few byte comparisons: no regular expression and no `memcmp` call per candidate.
fn shape_at(text: &[u8], cursor: usize) -> Option<Mask> {
    let rest = &text[cursor..];
    let (prefix_len, body_kind) = match rest {
        [b'g', b'h', b'p', b'_', ..]
        | [b'g', b'h', b'o', b'_', ..]
        | [b'g', b'h', b'u', b'_', ..]
        | [b'g', b'h', b's', b'_', ..]
        | [b'g', b'h', b'r', b'_', ..] => (4, Body::Token),
        [
            b'g',
            b'i',
            b't',
            b'h',
            b'u',
            b'b',
            b'_',
            b'p',
            b'a',
            b't',
            b'_',
            ..,
        ] => (11, Body::Token),
        [b'g', b'l', b'p', b'a', b't', b'-', ..] => (6, Body::Token),
        [b's', b'k', b'_', b'l', b'i', b'v', b'e', b'_', ..]
        | [b's', b'k', b'_', b't', b'e', b's', b't', b'_', ..] => (8, Body::Token),
        [b's', b'k', b'-', ..] => (3, Body::Token),
        [b'r', b'k', b'_', b'l', b'i', b'v', b'e', b'_', ..]
        | [b'r', b'k', b'_', b't', b'e', b's', b't', b'_', ..] => (8, Body::Token),
        [b'x', b'o', b'x', b'a' | b'b' | b'p' | b'r' | b's', b'-', ..] => (5, Body::Token),
        [b'n', b'p', b'm', b'_', ..] => (4, Body::Token),
        [b'h', b'f', b'_', ..] => (3, Body::Token),
        [b'p', b'y', b'p', b'i', b'-', ..] => (5, Body::Token),
        [b'd', b'c', b'k', b'r', b'_', b'p', b'a', b't', b'_', ..] => (9, Body::Token),
        [b'A', b'K' | b'S', b'I', b'A', ..] => (4, Body::Upper),
        [b'A', b'I', b'z', b'a', ..] => (4, Body::Token),
        // A JWT starts at a token boundary: inside a longer base64url run it is a
        // fragment, and scanning every offset of such a run would cost more than the
        // match is worth.
        [b'e', b'y', b'J', ..] if cursor == 0 || !is_base64url(text[cursor - 1]) => {
            return jwt_at(rest, cursor);
        }
        _ => return None,
    };
    let body = &rest[prefix_len..];
    let len = match body_kind {
        Body::Token => token_len(body),
        Body::Upper => upper_token_len(body),
    };
    if len < MIN_TOKEN_BODY {
        return None;
    }
    Some(Mask {
        keep: cursor,
        end: cursor + prefix_len + len,
    })
}

/// Matches `Bearer <token>`, keeping the scheme visible.
fn bearer_at(text: &[u8], cursor: usize) -> Option<Mask> {
    let rest = &text[cursor..];
    if rest.len() < 7 || !rest[..6].eq_ignore_ascii_case(b"bearer") {
        return None;
    }
    if !matches!(rest[6], b' ' | b'\t') {
        return None;
    }
    let mut start = 6;
    while start < rest.len() && matches!(rest[start], b' ' | b'\t') {
        start += 1;
    }
    let len = token_len(&rest[start..]);
    if len < MIN_TOKEN_BODY {
        return None;
    }
    Some(Mask {
        keep: cursor + start,
        end: cursor + start + len,
    })
}

/// Matches a JSON Web Token: three base64url segments, header starting with `eyJ`.
fn jwt_at(rest: &[u8], cursor: usize) -> Option<Mask> {
    if rest.len() < 3 || rest[0] != b'e' || rest[1] != b'y' || rest[2] != b'J' {
        return None;
    }
    let limit = rest.len().min(MAX_JWT_LEN);
    let mut index = 0usize;
    let mut dots = 0usize;
    let mut segment = 0usize;
    while index < limit {
        let byte = rest[index];
        if is_base64url(byte) {
            segment += 1;
            index += 1;
        } else if byte == b'.' {
            if segment < 4 {
                return None;
            }
            dots += 1;
            segment = 0;
            index += 1;
        } else {
            break;
        }
    }
    if dots < 2 || segment < 4 {
        return None;
    }
    Some(Mask {
        keep: cursor,
        end: cursor + index,
    })
}

/// Masks the password of `scheme://user:password@host`.
fn url_credentials(text: &[u8], cursor: usize) -> Option<Mask> {
    if text.get(cursor + 1) != Some(&b'/') || text.get(cursor + 2) != Some(&b'/') {
        return None;
    }
    let start = cursor + 3;
    let limit = (start + MAX_USERINFO_LEN).min(text.len());
    let mut index = start;
    let mut colon = None;
    while index < limit {
        match text[index] {
            b':' if colon.is_none() => colon = Some(index),
            b'@' => {
                let separator = colon?;
                if separator > start && index > separator + 1 {
                    return Some(Mask {
                        keep: separator + 1,
                        end: index,
                    });
                }
                return None;
            }
            b'/' | b'?' | b'#' | b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'\\' => {
                return None;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// End of the `${...}` reference that starts at `cursor`, or `None`.
///
/// A reference is copied verbatim, so no rule can mask inside one.
fn reference_end(text: &[u8], cursor: usize) -> Option<usize> {
    if text[cursor] != b'$' || text.get(cursor + 1) != Some(&b'{') {
        return None;
    }
    let limit = (cursor + 2 + MAX_REFERENCE_LEN).min(text.len());
    let mut index = cursor + 2;
    while index < limit {
        match text[index] {
            b'}' => return Some(index + 1),
            b'\n' | b'\r' => return None,
            _ => index += 1,
        }
    }
    None
}

/// Whether `text` is exactly one `${...}` reference.
fn is_reference(text: &str) -> bool {
    let bytes = text.as_bytes();
    reference_end(bytes, 0) == Some(bytes.len())
}

/// Whether `value` contains the start of a reference, and so must not be masked whole.
fn contains_reference(value: &[u8]) -> bool {
    value.windows(2).any(|pair| pair == b"${")
}

/// Length of the leading run of token bytes, without trailing separators.
fn token_len(body: &[u8]) -> usize {
    let mut len = 0usize;
    while len < body.len() && is_token_byte(body[len]) {
        len += 1;
    }
    while len > 0 && body[len - 1] == b'-' {
        len -= 1;
    }
    len
}

/// Length of the leading run of upper-case letters and digits, for AWS key ids.
fn upper_token_len(body: &[u8]) -> usize {
    let mut len = 0usize;
    while len < body.len() && (body[len].is_ascii_uppercase() || body[len].is_ascii_digit()) {
        len += 1;
    }
    len
}

/// Bytes that can appear inside a token.
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'/' | b'=' | b'~')
}

/// Bytes that can appear inside a base64url segment.
fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// Bytes that end an unquoted assignment value.
fn is_value_terminator(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'"' | b'\''
                | b'&'
                | b'#'
                | b','
                | b';'
                | b')'
                | b']'
                | b'}'
                | b'<'
                | b'>'
                | b'|'
                | b'`'
        )
}

/// Whether `byte` can start an identifier.
fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Whether `byte` can appear inside an identifier.
fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
