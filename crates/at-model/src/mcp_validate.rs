//! Validation rules for MCP server definitions.
//!
//! The rules are pure: the only outside input is the [`CommandResolver`], injected so
//! `at-model` stays free of filesystem access. The form, the CLI and the engine call
//! the same functions and therefore report the same problems for the same definition.
//!
//! Three kinds of result come back:
//!
//! - [`Validation::errors`] are hard errors. Saving is blocked and the form renders each
//!   one inline at [`FieldError::field`].
//! - [`Validation::warnings`] do not block saving. [`FieldCode::UnresolvedCommand`] is a
//!   warning because a server may be saved before its binary is installed; testing the
//!   connection is what fails at that point.
//! - [`Validation::shadows`] are notices. The same server name in two scopes is allowed;
//!   the higher-precedence scope wins and the notice names it.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::Scope;
use crate::mcp::{EnvValue, McpTransport, SecretRef};
use crate::wire::wire_enum;

/// Field path of the server name.
pub const FIELD_NAME: &str = "name";
/// Field path of the HTTP endpoint URL.
pub const FIELD_URL: &str = "url";
/// Field path of the stdio command.
pub const FIELD_COMMAND: &str = "command";
/// Field path prefix of the stdio arguments; one error per index is `args.<index>`.
pub const FIELD_ARGS: &str = "args";
/// Field path prefix of environment entries; one error per entry is `env.<NAME>`.
pub const FIELD_ENV: &str = "env";
/// Field path prefix of HTTP headers; one error per entry is `headers.<NAME>`.
pub const FIELD_HEADERS: &str = "headers";

/// Longest accepted server name, in bytes, matching `^[a-z0-9][a-z0-9._-]{0,62}$`.
pub const MAX_NAME_LEN: usize = 63;

wire_enum! {
    /// Stable code for one validation problem.
    ///
    /// Codes are part of the contract: the form, the CLI `--json` output and the tests
    /// compare them, so a code never changes meaning once shipped.
    #[non_exhaustive]
    pub enum FieldCode {
        /// The name is empty.
        NameRequired => "name_required",
        /// The name does not match `^[a-z0-9][a-z0-9._-]{0,62}$`.
        NameInvalid => "name_invalid",
        /// The name ends in a dot.
        NameTrailingDot => "name_trailing_dot",
        /// A server with this name already exists in the same scope.
        DuplicateId => "duplicate_id",
        /// The URL is empty.
        UrlRequired => "url_required",
        /// The URL does not parse as an absolute URL.
        UrlInvalid => "url_invalid",
        /// The URL scheme is not `http` or `https`.
        UrlScheme => "url_scheme",
        /// The URL has no host.
        UrlHostMissing => "url_host_missing",
        /// The URL carries a user name or password.
        CredentialsInUrl => "credentials_in_url",
        /// The URL has a fragment.
        UrlFragment => "url_fragment",
        /// The command is empty or whitespace only.
        CommandRequired => "command_required",
        /// The command contains a NUL character.
        CommandNul => "command_nul",
        /// The command does not resolve on `PATH`. A warning, not an error.
        UnresolvedCommand => "unresolved_command",
        /// An argument contains a NUL character.
        ArgumentNul => "argument_nul",
        /// An environment variable name is not `^[A-Za-z_][A-Za-z0-9_]*$`.
        EnvNameInvalid => "env_name_invalid",
        /// The same environment variable name appears twice.
        EnvNameDuplicate => "env_name_duplicate",
        /// A header name is empty or contains control or zero-width characters.
        HeaderNameInvalid => "header_name_invalid",
        /// A value contains a control character.
        ValueControl => "value_control",
        /// A value contains a zero-width character.
        ValueZeroWidth => "value_zero_width",
        /// The Keychain account of a secret reference is empty.
        SecretRefAccountRequired => "secret_ref_account_required",
        /// The Keychain account of a secret reference contains a control character.
        SecretRefAccountControl => "secret_ref_account_control",
    }
}

/// One validation problem, shaped so the form can render it next to a control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    /// Path of the field the problem belongs to, for example `name`, `url`,
    /// `args.0`, `env.GITHUB_TOKEN` or `env.GITHUB_TOKEN.keychain_account`.
    pub field: String,
    /// Stable code the UI and the CLI branch on.
    pub code: FieldCode,
    /// Sentence shown to the user under the field.
    pub message: String,
}

impl FieldError {
    /// Builds one field problem.
    #[must_use]
    pub fn new(field: impl Into<String>, code: FieldCode, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code,
            message: message.into(),
        }
    }
}

/// A server definition as the editor currently holds it.
///
/// `name` is the one name every agent file uses as its key (the Codex table name and
/// the omp JSON key), not a free-form display label.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerCandidate<'a> {
    /// Server name: lower-case, used verbatim as the key in each agent document.
    pub name: &'a str,
    /// Transport and its settings.
    pub transport: &'a McpTransport,
}

/// A server that already exists in some scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingServer<'a> {
    /// Name the existing server was saved under.
    pub name: &'a str,
    /// Scope it lives in.
    pub scope: Scope,
}

/// Decides whether a stdio command can be started.
///
/// Production code wraps `at-mcp`'s `PathResolver`:
/// `let commands = |command: &str| resolver.which(command).is_ok();` — `at-model`
/// cannot depend on `at-mcp`, so the check arrives as this trait and any
/// `Fn(&str) -> bool` implements it.
pub trait CommandResolver {
    /// Whether `command` names an executable a child process can start.
    fn resolves(&self, command: &str) -> bool;
}

impl<F> CommandResolver for F
where
    F: Fn(&str) -> bool,
{
    fn resolves(&self, command: &str) -> bool {
        self(command)
    }
}

/// Everything validation needs from outside the definition itself.
#[derive(Clone, Copy)]
pub struct ValidationContext<'a> {
    /// Scope the definition is being saved into.
    pub scope: Scope,
    /// Every other server that exists today, in every scope.
    ///
    /// The definition being edited must not be in this slice; that is what makes an
    /// unchanged name valid while a second server with the same name is a duplicate.
    pub existing: &'a [ExistingServer<'a>],
    /// PATH lookup for stdio commands.
    pub commands: &'a dyn CommandResolver,
}

/// A server name that exists in more than one scope.
///
/// The definition is still valid; only the winning scope takes effect. `winning_scope`
/// is the scope with the highest precedence among the copies, so when it differs from
/// [`ShadowNotice::scope`] the definition being edited is the shadowed one, and when it
/// matches, [`ShadowNotice::other_scope`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowNotice {
    /// Name the copies share.
    pub name: String,
    /// Scope being validated.
    pub scope: Scope,
    /// The other scope holding a server with the same name.
    pub other_scope: Scope,
    /// Scope whose definition takes effect.
    pub winning_scope: Scope,
}

/// Result of validating one server definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    /// Hard problems; saving is blocked while this is non-empty.
    pub errors: Vec<FieldError>,
    /// Problems worth showing that do not block saving.
    pub warnings: Vec<FieldError>,
    /// Names that also exist in another scope.
    pub shadows: Vec<ShadowNotice>,
}

impl Validation {
    /// Whether saving may proceed, that is there are no hard errors.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Whether nothing at all was reported.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty() && self.warnings.is_empty() && self.shadows.is_empty()
    }
}

/// Validates a whole server definition.
///
/// Returns every problem found, not just the first, so the form can mark each control.
#[must_use]
pub fn validate_server(candidate: &ServerCandidate<'_>, ctx: &ValidationContext<'_>) -> Validation {
    let mut report = Validation::default();

    let name_error = validate_name(candidate.name);
    let name_is_valid = name_error.is_none();
    report.errors.extend(name_error);
    if name_is_valid {
        validate_identity(candidate.name, ctx, &mut report);
    }

    match candidate.transport {
        McpTransport::Stdio {
            command, args, env, ..
        } => {
            validate_command_field(command, ctx.commands, &mut report);
            for (index, arg) in args.iter().enumerate() {
                if arg.contains('\0') {
                    report.errors.push(FieldError::new(
                        format!("{FIELD_ARGS}.{index}"),
                        FieldCode::ArgumentNul,
                        "An argument cannot contain a NUL character.",
                    ));
                }
            }
            report.errors.extend(validate_env_entries(
                env.iter().map(|(k, v)| (k.as_str(), v)),
            ));
        }
        McpTransport::Http { url, headers, .. } => {
            report.errors.extend(validate_parsed_url(url));
            report.errors.extend(validate_header_entries(
                headers.iter().map(|(k, v)| (k.as_str(), v)),
            ));
        }
    }

    report
}

/// Validates the server name against `^[a-z0-9][a-z0-9._-]{0,62}$` and the trailing-dot rule.
///
/// The name is lower-case because it is a key in per-agent documents that compare keys
/// case-sensitively.
#[must_use]
pub fn validate_name(name: &str) -> Option<FieldError> {
    if name.is_empty() {
        return Some(FieldError::new(
            FIELD_NAME,
            FieldCode::NameRequired,
            "Enter a name for this server.",
        ));
    }
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !first_ok || !rest_ok || name.len() > MAX_NAME_LEN {
        return Some(FieldError::new(
            FIELD_NAME,
            FieldCode::NameInvalid,
            "Use lower-case letters, digits, dots, dashes and underscores, starting with a letter or a digit, at most 63 characters.",
        ));
    }
    if name.ends_with('.') {
        return Some(FieldError::new(
            FIELD_NAME,
            FieldCode::NameTrailingDot,
            "A name cannot end with a dot.",
        ));
    }
    None
}

/// Validates an environment variable name against `^[A-Za-z_][A-Za-z0-9_]*$`.
#[must_use]
pub fn validate_env_name(name: &str) -> Option<FieldError> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !first_ok || !rest_ok {
        return Some(FieldError::new(
            format!("{FIELD_ENV}.{name}"),
            FieldCode::EnvNameInvalid,
            "Use letters, digits and underscores, starting with a letter or an underscore.",
        ));
    }
    None
}

/// Parses `raw` and validates the result as an HTTP endpoint URL.
///
/// The form calls this while the URL is still text; [`validate_parsed_url`] is the same
/// rule for an already parsed [`Url`].
#[must_use]
pub fn validate_url(raw: &str) -> Vec<FieldError> {
    if raw.trim().is_empty() {
        return vec![FieldError::new(
            FIELD_URL,
            FieldCode::UrlRequired,
            "Enter the server URL.",
        )];
    }
    match Url::parse(raw) {
        Ok(url) => validate_parsed_url(&url),
        Err(_) => vec![FieldError::new(
            FIELD_URL,
            FieldCode::UrlInvalid,
            "Enter a complete URL, for example https://example.com/mcp.",
        )],
    }
}

/// Validates an already parsed endpoint URL.
///
/// Requires `http` or `https`, a host, no user name or password and no fragment.
#[must_use]
pub fn validate_parsed_url(url: &Url) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if !matches!(url.scheme(), "http" | "https") {
        errors.push(FieldError::new(
            FIELD_URL,
            FieldCode::UrlScheme,
            "Only http and https URLs are supported.",
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        errors.push(FieldError::new(
            FIELD_URL,
            FieldCode::UrlHostMissing,
            "The URL needs a host name.",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        errors.push(FieldError::new(
            FIELD_URL,
            FieldCode::CredentialsInUrl,
            "A URL cannot carry credentials. Remove them from the URL and store them as a header secret instead.",
        ));
    }
    if url.fragment().is_some() {
        errors.push(FieldError::new(
            FIELD_URL,
            FieldCode::UrlFragment,
            "Remove the fragment: the server address ends before the #.",
        ));
    }
    errors
}

/// Validates one environment entry.
///
/// Reports the entry name, control characters and zero-width characters in the value,
/// and the Keychain account of a secret reference. Duplicates are the caller's to pass
/// as a collection, see [`validate_env_entries`].
#[must_use]
pub fn validate_env_entry(name: &str, value: &EnvValue) -> Vec<FieldError> {
    let mut errors = validate_env_name(name).into_iter().collect::<Vec<_>>();
    errors.extend(validate_entry_value(&format!("{FIELD_ENV}.{name}"), value));
    errors
}

/// Validates an ordered list of environment entries, including duplicate names.
///
/// The stored form is a map and cannot hold duplicates; the form edits a list, so the
/// duplicate check runs here before the list is folded into a map.
#[must_use]
pub fn validate_env_entries<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a EnvValue)>,
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (name, value) in entries {
        errors.extend(validate_env_entry(name, value));
        if seen.contains(&name) {
            errors.push(FieldError::new(
                format!("{FIELD_ENV}.{name}"),
                FieldCode::EnvNameDuplicate,
                "This variable is already set for this server.",
            ));
        } else {
            seen.push(name);
        }
    }
    errors
}

/// Validates HTTP headers: non-empty names without control or zero-width characters,
/// and values without control or zero-width characters.
#[must_use]
pub fn validate_header_entries<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a EnvValue)>,
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    for (name, value) in headers {
        let field = format!("{FIELD_HEADERS}.{name}");
        if name.is_empty() || has_forbidden_char(name).is_some() {
            errors.push(FieldError::new(
                field.clone(),
                FieldCode::HeaderNameInvalid,
                "A header name cannot be empty and cannot contain control or zero-width characters.",
            ));
        }
        errors.extend(validate_entry_value(&field, value));
    }
    errors
}

fn validate_command_field(command: &str, commands: &dyn CommandResolver, report: &mut Validation) {
    if command.trim().is_empty() {
        report.errors.push(FieldError::new(
            FIELD_COMMAND,
            FieldCode::CommandRequired,
            "Enter the executable to run.",
        ));
        return;
    }
    if command.contains('\0') {
        report.errors.push(FieldError::new(
            FIELD_COMMAND,
            FieldCode::CommandNul,
            "The command cannot contain a NUL character.",
        ));
        return;
    }
    if !commands.resolves(command) {
        report.warnings.push(FieldError::new(
            FIELD_COMMAND,
            FieldCode::UnresolvedCommand,
            format!(
                "`{command}` is not on PATH. You can save now and install it later, but testing the connection will fail until it exists."
            ),
        ));
    }
}

/// Reports duplicates in the same scope and notice for copies in other scopes.
fn validate_identity(name: &str, ctx: &ValidationContext<'_>, report: &mut Validation) {
    let mut scopes: Vec<Scope> = vec![ctx.scope];
    let mut duplicate = false;
    for other in ctx.existing {
        if other.name != name {
            continue;
        }
        if other.scope == ctx.scope {
            duplicate = true;
        } else if !scopes.contains(&other.scope) {
            scopes.push(other.scope);
        }
    }

    if duplicate {
        report.errors.push(FieldError::new(
            FIELD_NAME,
            FieldCode::DuplicateId,
            format!("A server named `{name}` already exists in this scope."),
        ));
    }

    if scopes.len() < 2 {
        return;
    }
    // Precedence is ordered, so the maximum is the scope that takes effect.
    let winning_scope = scopes
        .iter()
        .copied()
        .max_by_key(|scope| scope.precedence())
        .unwrap_or(ctx.scope);
    for scope in scopes {
        if scope == ctx.scope {
            continue;
        }
        report.shadows.push(ShadowNotice {
            name: name.to_owned(),
            scope: ctx.scope,
            other_scope: scope,
            winning_scope,
        });
    }
}

/// Validates the value of an environment entry or header.
fn validate_entry_value(field: &str, value: &EnvValue) -> Vec<FieldError> {
    match value {
        EnvValue::Literal(text) => validate_text(field, text),
        EnvValue::Secret(secret) => validate_secret_ref(field, secret),
    }
}

fn validate_secret_ref(field: &str, secret: &SecretRef) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let account_field = format!("{field}.keychain_account");
    if secret.keychain_account.is_empty() {
        errors.push(FieldError::new(
            account_field,
            FieldCode::SecretRefAccountRequired,
            "Enter the Keychain account that holds this secret.",
        ));
    } else if secret.keychain_account.chars().any(char::is_control) {
        errors.push(FieldError::new(
            account_field,
            FieldCode::SecretRefAccountControl,
            "A Keychain account cannot contain control characters.",
        ));
    }
    errors
}

/// Rejects control characters and zero-width characters in a stored value.
///
/// A newline would break an HTTP header, and a zero-width character would make two
/// visually identical values compare differently.
#[must_use]
pub fn validate_text(field: &str, text: &str) -> Vec<FieldError> {
    match has_forbidden_char(text) {
        Some(ForbiddenChar::Control) => vec![FieldError::new(
            field,
            FieldCode::ValueControl,
            "Remove control characters from this value.",
        )],
        Some(ForbiddenChar::ZeroWidth) => vec![FieldError::new(
            field,
            FieldCode::ValueZeroWidth,
            "Remove zero-width characters from this value.",
        )],
        None => Vec::new(),
    }
}

/// The first character class a value may not contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForbiddenChar {
    Control,
    ZeroWidth,
}

fn has_forbidden_char(text: &str) -> Option<ForbiddenChar> {
    for c in text.chars() {
        if c.is_control() {
            return Some(ForbiddenChar::Control);
        }
        if is_zero_width(c) {
            return Some(ForbiddenChar::ZeroWidth);
        }
    }
    None
}

fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{180E}'
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    const KNOWN: &[&str] = &["github-mcp-server", "/usr/local/bin/known"];

    struct FakeCommands;

    impl CommandResolver for FakeCommands {
        fn resolves(&self, command: &str) -> bool {
            KNOWN.contains(&command)
        }
    }

    fn context<'a>(scope: Scope, existing: &'a [ExistingServer<'a>]) -> ValidationContext<'a> {
        ValidationContext {
            scope,
            existing,
            commands: &FakeCommands,
        }
    }

    fn stdio(command: &str, args: &[&str], env: &[(&str, &str)]) -> McpTransport {
        McpTransport::Stdio {
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: None,
            env: env
                .iter()
                .map(|(name, value)| ((*name).to_owned(), EnvValue::Literal((*value).to_owned())))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn http(url: &str) -> McpTransport {
        McpTransport::Http {
            url: Url::parse(url).expect("test URL parses"),
            headers: BTreeMap::new(),
            oauth: None,
        }
    }

    fn name_codes(name: &str) -> Vec<FieldCode> {
        validate_name(name)
            .map(|error| error.code)
            .into_iter()
            .collect()
    }

    fn url_codes(raw: &str) -> Vec<FieldCode> {
        validate_url(raw)
            .into_iter()
            .map(|error| error.code)
            .collect()
    }

    fn env_name_codes(name: &str) -> Vec<FieldCode> {
        validate_env_entry(name, &EnvValue::Literal("x".to_owned()))
            .into_iter()
            .map(|error| error.code)
            .collect()
    }

    fn command_result(command: &str) -> (Vec<FieldCode>, Vec<FieldCode>) {
        let transport = stdio(command, &[], &[]);
        let candidate = ServerCandidate {
            name: "server",
            transport: &transport,
        };
        let report = validate_server(&candidate, &context(Scope::User, &[]));
        (
            report.errors.into_iter().map(|error| error.code).collect(),
            report
                .warnings
                .into_iter()
                .map(|error| error.code)
                .collect(),
        )
    }

    #[test]
    fn validation_table_matches_exact_codes() {
        let long_ok = "a".repeat(MAX_NAME_LEN);
        let long_bad = "a".repeat(MAX_NAME_LEN + 1);

        // 24 cases across names, URLs, environment names and commands.
        let cases: Vec<(&str, Vec<FieldCode>, Vec<FieldCode>)> = vec![
            ("name: plain", name_codes("github"), vec![]),
            ("name: single char", name_codes("a"), vec![]),
            ("name: dotted", name_codes("my.server_1-2"), vec![]),
            ("name: empty", name_codes(""), vec![FieldCode::NameRequired]),
            (
                "name: upper case",
                name_codes("GitHub"),
                vec![FieldCode::NameInvalid],
            ),
            (
                "name: leading dash",
                name_codes("-x"),
                vec![FieldCode::NameInvalid],
            ),
            (
                "name: space",
                name_codes("my server"),
                vec![FieldCode::NameInvalid],
            ),
            (
                "name: trailing dot",
                name_codes("github."),
                vec![FieldCode::NameTrailingDot],
            ),
            ("name: 63 chars", name_codes(&long_ok), vec![]),
            (
                "name: 64 chars",
                name_codes(&long_bad),
                vec![FieldCode::NameInvalid],
            ),
            ("url: https", url_codes("https://example.com/mcp"), vec![]),
            (
                "url: local http",
                url_codes("http://127.0.0.1:3000/mcp"),
                vec![],
            ),
            (
                "url: query",
                url_codes("https://example.com/mcp?token=1"),
                vec![],
            ),
            (
                "url: bad scheme",
                url_codes("ftp://example.com/x"),
                vec![FieldCode::UrlScheme],
            ),
            (
                "url: garbage",
                url_codes("not a url"),
                vec![FieldCode::UrlInvalid],
            ),
            (
                "url: user and password",
                url_codes("https://user:pass@host/mcp"),
                vec![FieldCode::CredentialsInUrl],
            ),
            (
                "url: user only",
                url_codes("https://user@host/mcp"),
                vec![FieldCode::CredentialsInUrl],
            ),
            (
                "url: fragment",
                url_codes("https://example.com/mcp#frag"),
                vec![FieldCode::UrlFragment],
            ),
            (
                "url: file scheme without host",
                url_codes("file:///tmp/mcp"),
                vec![FieldCode::UrlScheme, FieldCode::UrlHostMissing],
            ),
            ("env: upper snake", env_name_codes("GITHUB_TOKEN"), vec![]),
            ("env: underscore prefix", env_name_codes("_x1"), vec![]),
            (
                "env: leading digit",
                env_name_codes("1BAD"),
                vec![FieldCode::EnvNameInvalid],
            ),
            (
                "env: dash",
                env_name_codes("BAD-NAME"),
                vec![FieldCode::EnvNameInvalid],
            ),
            (
                "env: zero width",
                env_name_codes("BAD\u{200B}NAME"),
                vec![FieldCode::EnvNameInvalid],
            ),
            (
                "command: known",
                command_result("github-mcp-server").0,
                vec![],
            ),
            (
                "command: empty",
                command_result("").0,
                vec![FieldCode::CommandRequired],
            ),
            (
                "command: spaces",
                command_result("   ").0,
                vec![FieldCode::CommandRequired],
            ),
            (
                "command: NUL",
                command_result("bad\0cmd").0,
                vec![FieldCode::CommandNul],
            ),
            (
                "command: unknown is a warning",
                command_result("not-installed").0,
                vec![],
            ),
        ];

        for (label, got, want) in cases {
            assert_eq!(got, want, "case: {label}");
        }
        assert_eq!(
            command_result("not-installed").1,
            vec![FieldCode::UnresolvedCommand],
            "an unresolved command must be a warning"
        );
    }

    #[test]
    fn valid_stdio_and_http_servers_pass() {
        let transport = stdio(
            "github-mcp-server",
            &["--stdio"],
            &[("GITHUB_TOKEN", "info")],
        );
        let candidate = ServerCandidate {
            name: "github",
            transport: &transport,
        };
        let report = validate_server(&candidate, &context(Scope::User, &[]));
        assert!(report.is_ok());
        assert!(report.is_empty());

        let transport = http("https://example.com/mcp");
        let candidate = ServerCandidate {
            name: "github",
            transport: &transport,
        };
        let report = validate_server(&candidate, &context(Scope::User, &[]));
        assert!(report.is_ok());
        assert!(report.is_empty());
    }

    #[test]
    fn duplicate_in_same_scope_is_an_error() {
        let existing = [ExistingServer {
            name: "github",
            scope: Scope::User,
        }];
        let transport = http("https://example.com/mcp");
        let candidate = ServerCandidate {
            name: "github",
            transport: &transport,
        };
        let report = validate_server(&candidate, &context(Scope::User, &existing));
        assert!(!report.is_ok());
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].code, FieldCode::DuplicateId);
        assert_eq!(report.errors[0].field, FIELD_NAME);
        assert!(report.shadows.is_empty());
    }

    #[test]
    fn same_name_in_user_and_project_is_allowed_with_a_shadow_notice() {
        let existing = [ExistingServer {
            name: "github",
            scope: Scope::Project,
        }];
        let transport = http("https://example.com/mcp");
        let candidate = ServerCandidate {
            name: "github",
            transport: &transport,
        };

        let report = validate_server(&candidate, &context(Scope::User, &existing));
        assert!(report.is_ok());
        assert!(report.errors.is_empty());
        assert_eq!(report.shadows.len(), 1);
        assert_eq!(report.shadows[0].name, "github");
        assert_eq!(report.shadows[0].scope, Scope::User);
        assert_eq!(report.shadows[0].winning_scope, Scope::Project);

        // The reverse direction names the same winner.
        let existing = [ExistingServer {
            name: "github",
            scope: Scope::User,
        }];
        let report = validate_server(&candidate, &context(Scope::Project, &existing));
        assert!(report.is_ok());
        assert_eq!(report.shadows.len(), 1);
        assert_eq!(report.shadows[0].winning_scope, Scope::Project);
    }

    #[test]
    fn local_beats_project_user_and_global() {
        let existing = [
            ExistingServer {
                name: "github",
                scope: Scope::Global,
            },
            ExistingServer {
                name: "github",
                scope: Scope::User,
            },
            ExistingServer {
                name: "github",
                scope: Scope::Project,
            },
        ];
        let transport = http("https://example.com/mcp");
        let candidate = ServerCandidate {
            name: "github",
            transport: &transport,
        };
        let report = validate_server(&candidate, &context(Scope::Local, &existing));
        assert!(report.is_ok());
        assert_eq!(report.shadows.len(), 3);
        for notice in &report.shadows {
            assert_eq!(notice.winning_scope, Scope::Local);
        }
    }

    #[test]
    fn credentials_in_url_is_rejected_with_a_form_ready_message() {
        let errors = validate_url("https://user:pass@host/mcp");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, FieldCode::CredentialsInUrl);
        assert_eq!(errors[0].field, FIELD_URL);
        assert!(
            errors[0].message.contains("header secret"),
            "message must point at the remedy: {}",
            errors[0].message
        );
    }

    #[test]
    fn missing_command_saves_with_a_warning() {
        let transport = stdio("ghost-server", &[], &[]);
        let candidate = ServerCandidate {
            name: "ghost",
            transport: &transport,
        };
        let report = validate_server(&candidate, &context(Scope::User, &[]));
        assert!(report.is_ok(), "a missing binary must not block saving");
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].code, FieldCode::UnresolvedCommand);
        assert_eq!(report.warnings[0].field, FIELD_COMMAND);
    }

    #[test]
    fn environment_entries_reject_control_and_zero_width_characters() {
        let value = EnvValue::Literal("line\nbreak".to_owned());
        let errors = validate_env_entry("TOKEN", &value);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, FieldCode::ValueControl);
        assert_eq!(errors[0].field, "env.TOKEN");

        let value = EnvValue::Literal("zero\u{200B}width".to_owned());
        let errors = validate_env_entry("TOKEN", &value);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, FieldCode::ValueZeroWidth);

        let value = EnvValue::Literal("zero\u{FEFF}bom".to_owned());
        let errors = validate_env_entry("TOKEN", &value);
        assert_eq!(errors[0].code, FieldCode::ValueZeroWidth);
    }

    #[test]
    fn duplicate_environment_names_are_rejected_in_the_ordered_form() {
        let first = EnvValue::Literal("a".to_owned());
        let second = EnvValue::Literal("b".to_owned());
        let entries = [("TOKEN", &first), ("TOKEN", &second)];
        let errors = validate_env_entries(entries);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, FieldCode::EnvNameDuplicate);
        assert_eq!(errors[0].field, "env.TOKEN");
    }

    #[test]
    fn arguments_reject_nul_at_their_index() {
        let transport = stdio("github-mcp-server", &["ok", "bad\0arg"], &[]);
        let candidate = ServerCandidate {
            name: "github",
            transport: &transport,
        };
        let report = validate_server(&candidate, &context(Scope::User, &[]));
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].code, FieldCode::ArgumentNul);
        assert_eq!(report.errors[0].field, "args.1");
    }

    #[test]
    fn secret_ref_account_must_be_present_and_clean() {
        let empty = EnvValue::Secret(SecretRef {
            env_name: "GITHUB_TOKEN".to_owned(),
            keychain_account: String::new(),
        });
        let errors = validate_env_entry("GITHUB_TOKEN", &empty);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, FieldCode::SecretRefAccountRequired);
        assert_eq!(errors[0].field, "env.GITHUB_TOKEN.keychain_account");

        let dirty = EnvValue::Secret(SecretRef {
            env_name: "GITHUB_TOKEN".to_owned(),
            keychain_account: "bad\naccount".to_owned(),
        });
        let errors = validate_env_entry("GITHUB_TOKEN", &dirty);
        assert_eq!(errors[0].code, FieldCode::SecretRefAccountControl);

        let good = EnvValue::Secret(SecretRef {
            env_name: "GITHUB_TOKEN".to_owned(),
            keychain_account: "GITHUB_TOKEN".to_owned(),
        });
        assert!(validate_env_entry("GITHUB_TOKEN", &good).is_empty());
    }

    #[test]
    fn headers_reject_control_characters_and_bad_names() {
        let value = EnvValue::Literal("Bearer\nabc".to_owned());
        let entries = [("Authorization", &value)];
        let errors = validate_header_entries(entries);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, FieldCode::ValueControl);
        assert_eq!(errors[0].field, "headers.Authorization");

        let value = EnvValue::Literal("ok".to_owned());
        let entries = [("Bad\nHeader", &value)];
        let errors = validate_header_entries(entries);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, FieldCode::HeaderNameInvalid);
    }

    #[test]
    fn candidate_closures_act_as_command_resolvers() {
        let resolver = |command: &str| command == "known";
        assert!(resolver.resolves("known"));
        assert!(!resolver.resolves("other"));

        let transport = stdio("known", &[], &[]);
        let candidate = ServerCandidate {
            name: "server",
            transport: &transport,
        };
        let ctx = ValidationContext {
            scope: Scope::User,
            existing: &[],
            commands: &resolver,
        };
        assert!(validate_server(&candidate, &ctx).is_ok());
    }

    #[test]
    fn stdio_cwd_is_not_part_of_the_rules() {
        let transport = McpTransport::Stdio {
            command: "github-mcp-server".to_owned(),
            args: Vec::new(),
            cwd: Some(PathBuf::from("/tmp")),
            env: BTreeMap::new(),
        };
        let candidate = ServerCandidate {
            name: "github",
            transport: &transport,
        };
        assert!(validate_server(&candidate, &context(Scope::User, &[])).is_empty());
    }

    #[test]
    fn codes_round_trip_through_their_wire_names() {
        for code in FieldCode::ALL {
            assert_eq!(code.to_string(), code.wire_name());
            assert_eq!(code.wire_name().parse::<FieldCode>().ok(), Some(*code));
        }
    }
}
