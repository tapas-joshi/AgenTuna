//! `${NAME}` secret references: syntax, import conversion and resolution.
//!
//! An env value or an HTTP header may hold a reference to a secret. AgenTuna accepts
//! exactly `${NAME}`, where `NAME` matches `^[A-Za-z_][A-Za-z0-9_]*$`; text around the
//! reference is allowed, so `Bearer ${TOKEN}` is a template with one reference.
//!
//! | input        | outcome                                                        |
//! | ------------ | -------------------------------------------------------------- |
//! | `${A}`       | one reference to `A`                                            |
//! | `${A_B1}`    | one reference to `A_B1`                                         |
//! | `$A`         | literal text; no braces, so nothing to reference                |
//! | `${}`        | [`RefError::EmptyName`]                                         |
//! | `${A B}`     | [`RefError::InvalidName`]                                       |
//! | `${A${B}}`   | [`RefError::NestedReference`]                                   |
//! | `${A`        | [`RefError::Unterminated`]                                      |
//! | `$${A}`      | [`RefError::AmbiguousReference`]                                |
//! | `${A:-x}`    | [`RefError::DefaultNotAllowed`]; on import `x` plus a warning    |
//!
//! A value that starts with `${` and is not one of the accepted forms is rejected rather
//! than stored as text: a malformed reference would still reach an agent that may expand
//! it differently from what the user sees. `$${A}` is rejected for the same reason — the
//! literal `${A}` it would produce is a reference again for the agent.
//!
//! Importing is the one place that accepts more: `${NAME:-default}` becomes the literal
//! `default` with a [`RefWarning`], because a default that is not a secret must not
//! masquerade as one.
//!
//! Nothing here runs at save time. [`VarTemplate::resolve`] is called only when a child
//! process is spawned or a request header is built; a missing secret fails that
//! operation and never leaves a partly substituted value behind.

use core::fmt;

use zeroize::Zeroizing;

use crate::error::SecretsError;
use crate::name::{KeychainId, SecretName};
use crate::secret::SecretString;
use crate::store::SecretStore;

/// Longest accepted secret name, matching the Keychain account limit.
const MAX_NAME_BYTES: usize = 255;

/// Longest offending text echoed in an error message.
const MAX_SNIPPET_CHARS: usize = 40;

/// Why a value is not a valid `${NAME}` reference.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RefError {
    /// The braces hold no name: `${}`.
    #[error("`${{}}` is missing a secret name")]
    EmptyName,

    /// The name uses characters the grammar does not allow.
    #[error(
        "`{snippet}` is not a secret name: use letters, digits and underscores, starting with a letter or an underscore"
    )]
    InvalidName {
        /// Offending text, shortened for display.
        snippet: String,
    },

    /// A reference was opened but never closed.
    #[error("`{snippet}` is missing its closing `}}`")]
    Unterminated {
        /// Offending text, shortened for display.
        snippet: String,
    },

    /// One reference sits inside another.
    #[error("`{snippet}` must not contain another reference")]
    NestedReference {
        /// Offending text, shortened for display.
        snippet: String,
    },

    /// `$${...}` looks like an escape AgenTuna does not support.
    #[error(
        "`{snippet}` is not supported: write `${{NAME}}` to reference a secret, or the text without `$` before `{{`"
    )]
    AmbiguousReference {
        /// Offending text, shortened for display.
        snippet: String,
    },

    /// `${NAME:-default}` outside import: a stored value keeps no default.
    #[error("`${{{name}:-...}}` is not supported in a stored value: write a literal value instead")]
    DefaultNotAllowed {
        /// Name in the rejected reference.
        name: SecretName,
    },

    /// A reference names a secret the Keychain does not hold.
    #[error("secret `{name}` is not in the Keychain")]
    MissingSecret {
        /// Name that could not be resolved.
        name: String,
    },

    /// The credential store failed for a reason other than a missing entry.
    #[error(transparent)]
    Store(#[from] SecretsError),
}

/// One piece of a value.
#[derive(Clone, PartialEq, Eq)]
enum Segment {
    /// Literal text, stored and rendered as-is.
    Text(String),
    /// A reference resolved just before use.
    Reference(SecretName),
}

/// A stored value with zero or more `${NAME}` references.
///
/// Equality compares the reference names and the literal text; no resolved value is ever
/// part of a template.
#[derive(Clone, PartialEq, Eq)]
pub struct VarTemplate {
    segments: Vec<Segment>,
}

impl VarTemplate {
    /// Parses one env value or header value.
    ///
    /// # Errors
    /// [`RefError::EmptyName`], [`RefError::InvalidName`], [`RefError::Unterminated`],
    /// [`RefError::NestedReference`], [`RefError::AmbiguousReference`] or
    /// [`RefError::DefaultNotAllowed`].
    pub fn parse(raw: &str) -> Result<Self, RefError> {
        let mut segments = Vec::new();
        let mut text_start = 0usize;
        let mut cursor = 0usize;

        while cursor < raw.len() {
            let Some(offset) = raw[cursor..].find('$') else {
                break;
            };
            let dollar = cursor + offset;
            let after = dollar + 1;
            match raw[after..].chars().next() {
                Some('$') => {
                    if raw[after + 1..].starts_with('{') {
                        return Err(RefError::AmbiguousReference {
                            snippet: snippet(&raw[dollar..]),
                        });
                    }
                    // `$$` anywhere else is two literal dollars.
                    cursor = after + 1;
                }
                Some('{') => {
                    let inner_start = after + 1;
                    let Some(close) = raw[inner_start..].find('}') else {
                        return Err(RefError::Unterminated {
                            snippet: snippet(&raw[dollar..]),
                        });
                    };
                    let inner = &raw[inner_start..inner_start + close];
                    let name = parse_name(inner)?;
                    if text_start < dollar {
                        segments.push(Segment::Text(raw[text_start..dollar].to_owned()));
                    }
                    segments.push(Segment::Reference(name));
                    cursor = inner_start + close + 1;
                    text_start = cursor;
                }
                _ => {
                    // A lone `$` is literal text.
                    cursor = after;
                }
            }
        }

        if text_start < raw.len() {
            segments.push(Segment::Text(raw[text_start..].to_owned()));
        }
        Ok(Self { segments })
    }

    /// Every reference in the value, in the order it appears.
    pub fn references(&self) -> impl Iterator<Item = &SecretName> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Reference(name) => Some(name),
            Segment::Text(_) => None,
        })
    }

    /// Whether the value is exactly one reference and nothing else.
    #[must_use]
    pub fn is_reference(&self) -> bool {
        self.as_reference().is_some()
    }

    /// The single reference when the value is exactly one, otherwise `None`.
    #[must_use]
    pub fn as_reference(&self) -> Option<&SecretName> {
        match self.segments.as_slice() {
            [Segment::Reference(name)] => Some(name),
            _ => None,
        }
    }

    /// The literal text when the value holds no reference.
    #[must_use]
    pub fn as_literal(&self) -> Option<&str> {
        match self.segments.as_slice() {
            [] => Some(""),
            [Segment::Text(text)] => Some(text),
            _ => None,
        }
    }

    /// Whether the value holds at least one reference.
    #[must_use]
    pub fn has_reference(&self) -> bool {
        self.references().next().is_some()
    }

    /// Replaces every reference with its stored value, at the moment of use.
    ///
    /// A reference is looked up by its [`SecretName`] in the default Keychain service.
    /// When any reference is missing the call returns [`RefError::MissingSecret`] and
    /// nothing is substituted: the partly built value lives in a buffer that zeroes on
    /// drop, so a caller that blocks the operation has nothing to write.
    ///
    /// # Errors
    /// [`RefError::MissingSecret`] when a referenced secret is not in the store, or
    /// [`RefError::Store`] for another credential-store failure.
    pub fn resolve(&self, store: &dyn SecretStore) -> Result<SecretString, RefError> {
        self.render(store, None)
    }

    /// Resolves like [`VarTemplate::resolve`] and registers every value it read with
    /// `redactor`, so a value echoed by the child process is masked.
    ///
    /// This is the call a spawn path should use: resolving without registering leaves
    /// the value unknown to [`crate::Redactor`] and therefore printable.
    ///
    /// # Errors
    /// The same errors as [`VarTemplate::resolve`].
    pub fn resolve_tracked(
        &self,
        store: &dyn SecretStore,
        redactor: &mut crate::redact::Redactor,
    ) -> Result<SecretString, RefError> {
        self.render(store, Some(redactor))
    }

    fn render(
        &self,
        store: &dyn SecretStore,
        mut redactor: Option<&mut crate::redact::Redactor>,
    ) -> Result<SecretString, RefError> {
        let mut out = Zeroizing::new(String::with_capacity(self.capacity_hint()));
        for segment in &self.segments {
            match segment {
                Segment::Text(text) => out.push_str(text),
                Segment::Reference(name) => {
                    let id = KeychainId::from(name);
                    match store.read(&id) {
                        Ok(value) => {
                            if let Some(redactor) = redactor.as_deref_mut() {
                                redactor.insert(SecretString::new(value.expose_secret()));
                            }
                            out.push_str(value.expose_secret());
                        }
                        Err(SecretsError::NotFound { .. }) => {
                            return Err(RefError::MissingSecret {
                                name: name.as_str().to_owned(),
                            });
                        }
                        Err(other) => return Err(RefError::Store(other)),
                    }
                }
            }
        }
        Ok(SecretString::new(out.as_str()))
    }

    fn capacity_hint(&self) -> usize {
        self.segments
            .iter()
            .map(|segment| match segment {
                Segment::Text(text) => text.len(),
                Segment::Reference(_) => MAX_NAME_BYTES.min(64),
            })
            .sum()
    }
}

impl fmt::Debug for VarTemplate {
    /// Prints the reference names and the number of literal segments; literal text can
    /// hold a pasted credential, so it never reaches a log through `Debug`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let references = self
            .references()
            .map(SecretName::as_str)
            .collect::<Vec<_>>();
        f.debug_struct("VarTemplate")
            .field("references", &references)
            .field("segments", &self.segments.len())
            .finish()
    }
}

/// What an imported value becomes in a stored definition.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Imported {
    /// Store the text as a literal value.
    Literal(String),
    /// Store a reference to this secret.
    Secret(SecretName),
}

impl fmt::Debug for Imported {
    /// A literal can hold a pasted credential, so it never reaches a log through `Debug`;
    /// the reference variant prints its name, which is a label.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(_) => f.write_str("Literal(***)"),
            Self::Secret(name) => f.debug_tuple("Secret").field(name).finish(),
        }
    }
}

/// Something the import changed and the import summary must show.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefWarning {
    /// `${NAME:-default}` was replaced by the literal `default`.
    DefaultDiscarded {
        /// Name in the reference.
        name: SecretName,
        /// Default text now stored as the literal value.
        default: String,
    },
}

impl RefWarning {
    /// Plain sentence for the import summary.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::DefaultDiscarded { name, default } => format!(
                "`${{{}:-{default}}}` was imported as the literal `{default}`: a default is not a secret.",
                name.as_str()
            ),
        }
    }
}

/// Result of importing one raw env or header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    /// What to store for the value.
    pub value: Imported,
    /// Changes the import made, for the summary.
    pub warnings: Vec<RefWarning>,
}

/// Converts one imported value, applying the import-only rules.
///
/// `${NAME}` becomes [`Imported::Secret`]; `${NAME:-default}` becomes
/// [`Imported::Literal`] with a warning, so the stored value carries no `${}` wrapper;
/// anything else is validated as a template and kept verbatim for the agent to expand.
///
/// # Errors
/// The syntax errors of [`VarTemplate::parse`], plus [`RefError::InvalidName`] or
/// [`RefError::NestedReference`] for a malformed `${NAME:-default}`.
pub fn import_value(raw: &str) -> Result<ImportOutcome, RefError> {
    if let Some(inner) = whole_reference(raw) {
        if let Some((name, default)) = inner.split_once(":-") {
            let name = parse_name(name)?;
            check_default(default)?;
            let literal = default.to_owned();
            return Ok(ImportOutcome {
                warnings: vec![RefWarning::DefaultDiscarded {
                    name,
                    default: literal.clone(),
                }],
                value: Imported::Literal(literal),
            });
        }
        return Ok(ImportOutcome {
            value: Imported::Secret(parse_name(inner)?),
            warnings: Vec::new(),
        });
    }

    // A template or a plain literal: validate the syntax, keep the text byte for byte.
    VarTemplate::parse(raw)?;
    Ok(ImportOutcome {
        value: Imported::Literal(raw.to_owned()),
        warnings: Vec::new(),
    })
}

/// The inside of `raw` when the whole value is one `${...}` expression.
fn whole_reference(raw: &str) -> Option<&str> {
    let inner = raw.strip_prefix("${")?.strip_suffix('}')?;
    // `${A}${B}` strips to `A}${B`, which still holds a brace.
    if inner.contains('}') {
        return None;
    }
    Some(inner)
}

fn parse_name(raw: &str) -> Result<SecretName, RefError> {
    if raw.is_empty() {
        return Err(RefError::EmptyName);
    }
    if raw.contains('{') || raw.contains('}') {
        return Err(RefError::NestedReference {
            snippet: snippet(raw),
        });
    }
    if let Some((name, _)) = raw.split_once(":-") {
        let name = parse_name(name)?;
        return Err(RefError::DefaultNotAllowed { name });
    }
    if !is_valid_name(raw) {
        return Err(RefError::InvalidName {
            snippet: snippet(raw),
        });
    }
    SecretName::new(raw).map_err(|_| RefError::InvalidName {
        snippet: snippet(raw),
    })
}

/// `^[A-Za-z_][A-Za-z0-9_]*$` with the Keychain account length bound.
fn is_valid_name(name: &str) -> bool {
    if name.len() > MAX_NAME_BYTES {
        return false;
    }
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A default becomes a literal, so it must not need expansion of its own.
fn check_default(default: &str) -> Result<(), RefError> {
    if default.contains('{') || default.contains('}') || default.contains('$') {
        return Err(RefError::NestedReference {
            snippet: snippet(default),
        });
    }
    Ok(())
}

fn snippet(raw: &str) -> String {
    if raw.chars().count() <= MAX_SNIPPET_CHARS {
        return raw.to_owned();
    }
    let mut out = raw.chars().take(MAX_SNIPPET_CHARS).collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;
    use crate::store::SecretStore;

    const SENTINEL: &str = "sk-live-0123456789abcdef0123456789abcdef";

    fn name(raw: &str) -> SecretName {
        SecretName::new(raw).expect("valid test secret name")
    }

    fn reference(raw: &str) -> String {
        VarTemplate::parse(raw)
            .expect("parses")
            .references()
            .map(|name| name.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(",")
    }

    #[test]
    fn parser_table_matches_exact_outcomes() {
        // Accepted single references.
        assert_eq!(reference("${A}"), "A");
        assert_eq!(reference("${A_B1}"), "A_B1");
        assert_eq!(reference("Bearer ${TOKEN}"), "TOKEN");
        assert_eq!(reference("${A}${B}"), "A,B");

        // A bare dollar is literal text, not a reference.
        let template = VarTemplate::parse("$A").expect("literal");
        assert_eq!(template.as_literal(), Some("$A"));
        assert!(!template.has_reference());
        let template = VarTemplate::parse("cost: 5$").expect("trailing dollar is literal");
        assert_eq!(template.as_literal(), Some("cost: 5$"));
        // `$$` not before a brace is two literal dollars.
        let template = VarTemplate::parse("a$$b").expect("literal");
        assert_eq!(template.as_literal(), Some("a$$b"));

        // Rejected forms, with their exact error.
        assert!(matches!(
            VarTemplate::parse("${}"),
            Err(RefError::EmptyName)
        ));
        assert!(matches!(
            VarTemplate::parse("${A B}"),
            Err(RefError::InvalidName { .. })
        ));
        assert!(matches!(
            VarTemplate::parse("${1A}"),
            Err(RefError::InvalidName { .. })
        ));
        assert!(matches!(
            VarTemplate::parse("${A${B}}"),
            Err(RefError::NestedReference { .. })
        ));
        assert!(matches!(
            VarTemplate::parse("${A"),
            Err(RefError::Unterminated { .. })
        ));
        assert!(matches!(
            VarTemplate::parse("$${A}"),
            Err(RefError::AmbiguousReference { .. })
        ));
        assert!(matches!(
            VarTemplate::parse("${A:-x}"),
            Err(RefError::DefaultNotAllowed { .. })
        ));
    }

    #[test]
    fn a_whole_value_reference_is_recognised() {
        let template = VarTemplate::parse("${TOKEN}").expect("parses");
        assert!(template.is_reference());
        assert_eq!(
            template.as_reference().map(SecretName::as_str),
            Some("TOKEN")
        );
        assert_eq!(template.as_literal(), None);

        let template = VarTemplate::parse("Bearer ${TOKEN}").expect("parses");
        assert!(!template.is_reference());
        assert!(template.has_reference());
        assert_eq!(template.as_literal(), None);
    }

    #[test]
    fn resolution_substitutes_every_reference() {
        let store = MemoryStore::new();
        store
            .create(
                &KeychainId::from(&name("GITHUB_TOKEN")),
                SecretString::new("ghp_value"),
            )
            .expect("stored");

        let template = VarTemplate::parse("Bearer ${GITHUB_TOKEN}").expect("parses");
        let resolved = template.resolve(&store).expect("resolves");
        assert_eq!(resolved.expose_secret(), "Bearer ghp_value");
    }

    #[test]
    fn missing_secret_blocks_and_returns_no_partial_value() {
        let store = MemoryStore::new();
        let template = VarTemplate::parse("prefix-${MISSING}-suffix").expect("parses");
        let error = template.resolve(&store).expect_err("must fail");
        match error {
            RefError::MissingSecret { name } => assert_eq!(name, "MISSING"),
            other => panic!("expected MissingSecret, got {other:?}"),
        }
    }

    #[test]
    fn tracked_resolution_registers_the_value_for_redaction() {
        use crate::redact::Redactor;

        let store = MemoryStore::new();
        store
            .create(
                &KeychainId::from(&name("GITHUB_TOKEN")),
                SecretString::new(SENTINEL),
            )
            .expect("stored");

        let mut redactor = Redactor::new();
        let template = VarTemplate::parse("Bearer ${GITHUB_TOKEN}").expect("parses");
        template
            .resolve_tracked(&store, &mut redactor)
            .expect("resolves");

        assert_eq!(redactor.len(), 1);
        let echoed = format!("child said: {SENTINEL}");
        assert!(!redactor.redact(&echoed).contains(SENTINEL));
    }

    #[test]
    fn import_converts_a_default_to_a_literal_with_a_warning() {
        let outcome = import_value("${GITHUB_TOKEN:-public}").expect("imports");
        assert_eq!(outcome.value, Imported::Literal("public".to_owned()));
        assert_eq!(outcome.warnings.len(), 1);
        let message = outcome.warnings[0].message();
        assert!(message.contains("public"), "{message}");
        match &outcome.value {
            Imported::Literal(text) => assert!(!text.contains("${")),
            Imported::Secret(_) => panic!("a default must not become a secret reference"),
        }
    }

    #[test]
    fn import_keeps_a_plain_reference_and_verbatim_text() {
        let outcome = import_value("${GITHUB_TOKEN}").expect("imports");
        assert_eq!(outcome.value, Imported::Secret(name("GITHUB_TOKEN")));
        assert!(outcome.warnings.is_empty());

        let outcome = import_value("Bearer ${GITHUB_TOKEN}").expect("imports");
        assert_eq!(
            outcome.value,
            Imported::Literal("Bearer ${GITHUB_TOKEN}".to_owned())
        );

        let outcome = import_value("$A").expect("imports");
        assert_eq!(outcome.value, Imported::Literal("$A".to_owned()));
    }

    #[test]
    fn import_rejects_malformed_references_instead_of_storing_them() {
        assert!(import_value("${}").is_err());
        assert!(import_value("${A B}").is_err());
        assert!(import_value("${A:-${B}}").is_err());
        assert!(import_value("$${A}").is_err());
    }

    #[test]
    fn debug_never_prints_literal_text() {
        let template = VarTemplate::parse("Bearer ghp_pasted_credential ${TOKEN}").expect("parses");
        let printed = format!("{template:?}");
        assert!(!printed.contains("ghp_pasted_credential"), "{printed}");
        assert!(printed.contains("TOKEN"), "{printed}");
    }

    #[test]
    fn debug_of_every_type_never_prints_a_resolved_value() {
        use at_model::{EnvValue, SecretRef};

        use crate::redact::Redactor;

        let mut redactor = Redactor::new();
        redactor.insert(SecretString::new(SENTINEL));

        let warnings = import_value("${TOKEN:-fallback}")
            .expect("imports")
            .warnings;
        let error = VarTemplate::parse("${A B}").expect_err("rejected");
        let missing = VarTemplate::parse("${MISSING}")
            .expect("parses")
            .resolve(&MemoryStore::new())
            .expect_err("missing");

        let printed = format!(
            "{redactor:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
            SecretString::new(SENTINEL),
            KeychainId::from(&name("TOKEN")),
            name("TOKEN"),
            warnings,
            VarTemplate::parse("Bearer ${TOKEN}").expect("parses"),
            error,
            missing,
            ImportOutcome {
                value: Imported::Literal(SENTINEL.to_owned()),
                warnings: Vec::new(),
            },
            EnvValue::Literal(SENTINEL.to_owned()),
            EnvValue::Secret(SecretRef {
                env_name: "TOKEN".to_owned(),
                keychain_account: "TOKEN".to_owned(),
            }),
        );
        assert!(!printed.contains(SENTINEL), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
    }

    #[test]
    fn error_messages_are_bounded() {
        let long = "A".repeat(10_000);
        let error = VarTemplate::parse(&format!("${{{long} B}}")).expect_err("rejected");
        let message = error.to_string();
        assert!(message.chars().count() < 200, "{}", message.len());
    }
}
