//! One-shot, server-verified approvals for starting or testing an MCP launch.
//!
//! A discovered or typed MCP definition is not permission to execute it. The UI first asks
//! [`McpLaunchApprovals::prepare`] to describe the exact command line, shows it to the user and
//! echoes the issued nonce back; only [`consent_for`] turns that echo into a
//! [`LaunchConsent`]. Nonces are bound to a digest of the transport and expire after
//! [`APPROVAL_TTL`], so an approval cannot be replayed or reused for a different command.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use at_fs::sha256_hex;
use at_mcp::{LaunchConsent, StdioLaunch};
use at_model::{EnvValue, McpServerId, McpTransport};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How long a prepared launch approval stays valid.
pub const APPROVAL_TTL: Duration = Duration::from_secs(120);

/// Upper bound on pending approvals held in memory.
const MAX_PENDING: usize = 32;

/// A one-shot launch approval could not be verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchApprovalError {
    /// No approval with this identity was issued, or it was already used.
    #[error("the launch approval is missing or already used")]
    Missing,
    /// The approval expired before it was used.
    #[error("the launch approval expired; review the command again")]
    Expired,
    /// The transport presented differs from the one the user approved.
    #[error("the launch approval does not match this command")]
    Mismatched,
}

/// Exact launch described for the user's confirmation dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpLaunchApprovalView {
    /// One-shot identity to echo back with the test request.
    pub approval_id: String,
    /// Digest of the exact transport; echoed back and re-verified.
    pub request_hash: String,
    /// Transport kind (`stdio` or `http`).
    pub transport: String,
    /// Executable that would start, for a stdio server.
    pub command: Option<String>,
    /// Arguments passed verbatim, for a stdio server.
    pub args: Vec<String>,
    /// Working directory, when the definition pins one.
    pub cwd: Option<String>,
    /// Environment or header names; values are never shown.
    pub value_names: Vec<String>,
    /// Endpoint URL, for an HTTP server.
    pub url: Option<String>,
    /// Milliseconds until the approval expires.
    pub expires_in_ms: u64,
}

/// Echo of a prepared launch approval supplied by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpLaunchApproval {
    /// One-shot identity returned by `mcp_launch_prepare`.
    pub approval_id: String,
    /// Digest returned by `mcp_launch_prepare`.
    pub request_hash: String,
}

/// Pending approvals, keyed by their nonce.
#[derive(Default)]
pub struct McpLaunchApprovals {
    pending: Mutex<BTreeMap<String, PendingApproval>>,
}

struct PendingApproval {
    fingerprint: String,
    expires_at: Instant,
}

impl McpLaunchApprovals {
    /// An empty approval registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Describes `transport` and issues a one-shot, expiring approval for exactly it.
    pub fn prepare(&self, transport: &McpTransport, now: Instant) -> McpLaunchApprovalView {
        let fingerprint = fingerprint(transport);
        let approval_id = McpServerId::new().to_string();
        let expires_at = now + APPROVAL_TTL;
        let mut pending = self.lock();
        pending.retain(|_, entry| entry.expires_at > now);
        while pending.len() >= MAX_PENDING {
            let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            pending.remove(&oldest);
        }
        pending.insert(
            approval_id.clone(),
            PendingApproval {
                fingerprint: fingerprint.clone(),
                expires_at,
            },
        );
        describe(transport, approval_id, fingerprint)
    }

    /// Consumes the approval named by `approval`, refusing missing, expired, reused or mismatched ones.
    ///
    /// # Errors
    /// [`LaunchApprovalError::Missing`] when the nonce is unknown or already used,
    /// [`LaunchApprovalError::Expired`] after [`APPROVAL_TTL`], and
    /// [`LaunchApprovalError::Mismatched`] when either digest does not cover `transport`.
    pub fn consume(
        &self,
        approval: &McpLaunchApproval,
        transport: &McpTransport,
        now: Instant,
    ) -> Result<(), LaunchApprovalError> {
        let Some(pending) = self.lock().remove(&approval.approval_id) else {
            return Err(LaunchApprovalError::Missing);
        };
        if pending.expires_at <= now {
            return Err(LaunchApprovalError::Expired);
        }
        if pending.fingerprint != approval.request_hash
            || pending.fingerprint != fingerprint(transport)
        {
            return Err(LaunchApprovalError::Mismatched);
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, PendingApproval>> {
        match self.pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl std::fmt::Debug for McpLaunchApprovals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpLaunchApprovals")
            .field("pending", &self.lock().len())
            .finish()
    }
}

/// Verifies `approval` and mints consent for exactly `transport`.
///
/// Returns `None` for an HTTP transport: there is no child process to launch, so no consent
/// token applies.
///
/// # Errors
/// Propagates [`LaunchApprovalError`] when the approval is missing, expired, reused or
/// issued for a different transport.
pub fn consent_for(
    approvals: &McpLaunchApprovals,
    approval: &McpLaunchApproval,
    transport: &McpTransport,
    now: Instant,
) -> Result<Option<LaunchConsent>, LaunchApprovalError> {
    approvals.consume(approval, transport, now)?;
    Ok(StdioLaunch::from_transport(transport)
        .ok()
        .map(|launch| LaunchConsent::user_approved(&launch)))
}

fn describe(
    transport: &McpTransport,
    approval_id: String,
    request_hash: String,
) -> McpLaunchApprovalView {
    let (command, args, cwd, value_names, url) = match transport {
        McpTransport::Stdio {
            command,
            args,
            cwd,
            env,
        } => (
            Some(command.clone()),
            args.clone(),
            cwd.as_ref().map(|path| path.to_string_lossy().into_owned()),
            env.keys().cloned().collect(),
            None,
        ),
        McpTransport::Http { url, headers, .. } => (
            None,
            Vec::new(),
            None,
            headers.keys().cloned().collect(),
            Some(url.to_string()),
        ),
    };
    McpLaunchApprovalView {
        approval_id,
        request_hash,
        transport: transport.kind().to_owned(),
        command,
        args,
        cwd,
        value_names,
        url,
        expires_in_ms: u64::try_from(APPROVAL_TTL.as_millis()).unwrap_or(u64::MAX),
    }
}

/// Unambiguous digest of everything that would be launched.
fn fingerprint(transport: &McpTransport) -> String {
    let mut canonical = String::new();
    match transport {
        McpTransport::Stdio {
            command,
            args,
            cwd,
            env,
        } => {
            push_field(&mut canonical, "stdio");
            push_field(&mut canonical, command);
            for arg in args {
                push_field(&mut canonical, arg);
            }
            push_field(
                &mut canonical,
                &cwd.as_ref()
                    .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            );
            for (name, value) in env {
                push_value(&mut canonical, name, value);
            }
        }
        McpTransport::Http {
            url,
            headers,
            oauth,
        } => {
            push_field(&mut canonical, "http");
            push_field(&mut canonical, url.as_str());
            for (name, value) in headers {
                push_value(&mut canonical, name, value);
            }
            push_field(
                &mut canonical,
                if oauth.is_some() { "oauth" } else { "no-oauth" },
            );
        }
    }
    sha256_hex(canonical.as_bytes())
}

fn push_value(canonical: &mut String, name: &str, value: &EnvValue) {
    push_field(canonical, name);
    match value {
        EnvValue::Literal(literal) => {
            push_field(canonical, "literal");
            push_field(canonical, literal);
        }
        EnvValue::Secret(secret) => {
            push_field(canonical, "secret");
            push_field(canonical, &secret.env_name);
            push_field(canonical, &secret.keychain_account);
        }
    }
}

fn push_field(canonical: &mut String, value: &str) {
    canonical.push_str(&value.len().to_string());
    canonical.push(':');
    canonical.push_str(value);
    canonical.push('\n');
}

#[cfg(test)]
mod tests {
    use super::{
        APPROVAL_TTL, LaunchApprovalError, McpLaunchApproval, McpLaunchApprovals, consent_for,
    };
    use at_model::{EnvValue, McpTransport, SecretRef};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn stdio(command: &str) -> McpTransport {
        let mut env = BTreeMap::new();
        env.insert(
            "TOKEN".to_owned(),
            EnvValue::Secret(SecretRef {
                env_name: "TOKEN".to_owned(),
                keychain_account: "fixture-account".to_owned(),
            }),
        );
        env.insert("MODE".to_owned(), EnvValue::Literal("fixture".to_owned()));
        McpTransport::Stdio {
            command: command.to_owned(),
            args: vec!["--serve".to_owned()],
            cwd: Some(PathBuf::from("/sandbox")),
            env,
        }
    }

    fn echo(view: &super::McpLaunchApprovalView) -> McpLaunchApproval {
        McpLaunchApproval {
            approval_id: view.approval_id.clone(),
            request_hash: view.request_hash.clone(),
        }
    }

    #[test]
    fn view_shows_the_exact_command_without_values_and_starts_without_consent() {
        let approvals = McpLaunchApprovals::new();
        let now = Instant::now();
        let view = approvals.prepare(&stdio("/usr/bin/true"), now);
        assert_eq!(view.transport, "stdio");
        assert_eq!(view.command.as_deref(), Some("/usr/bin/true"));
        assert_eq!(view.args, vec!["--serve".to_owned()]);
        assert_eq!(view.cwd.as_deref(), Some("/sandbox"));
        assert_eq!(
            view.value_names,
            vec!["MODE".to_owned(), "TOKEN".to_owned()]
        );
        assert!(!view.request_hash.is_empty());
        assert_eq!(view.expires_in_ms, 120_000);
        assert!(approvals.lock().contains_key(&view.approval_id));
    }

    #[test]
    fn approval_is_one_shot() {
        let approvals = McpLaunchApprovals::new();
        let now = Instant::now();
        let transport = stdio("/usr/bin/true");
        let view = approvals.prepare(&transport, now);
        assert!(approvals.consume(&echo(&view), &transport, now).is_ok());
        assert_eq!(
            approvals.consume(&echo(&view), &transport, now),
            Err(LaunchApprovalError::Missing)
        );
    }

    #[test]
    fn missing_approval_never_mints_consent() {
        let approvals = McpLaunchApprovals::new();
        let transport = stdio("/usr/bin/true");
        let unknown = McpLaunchApproval {
            approval_id: "never-issued".to_owned(),
            request_hash: fingerprint(&transport),
        };
        let result = consent_for(&approvals, &unknown, &transport, Instant::now());
        assert!(matches!(result, Err(LaunchApprovalError::Missing)));
    }

    #[test]
    fn changed_command_or_environment_is_refused_and_burns_the_approval() {
        let approvals = McpLaunchApprovals::new();
        let now = Instant::now();
        let transport = stdio("/usr/bin/true");
        let view = approvals.prepare(&transport, now);
        let changed = stdio("/usr/bin/false");
        assert_eq!(
            approvals.consume(&echo(&view), &changed, now),
            Err(LaunchApprovalError::Mismatched)
        );
        assert_eq!(
            approvals.consume(&echo(&view), &transport, now),
            Err(LaunchApprovalError::Missing)
        );
    }

    #[test]
    fn approval_expires_after_two_minutes() {
        let approvals = McpLaunchApprovals::new();
        let now = Instant::now();
        let transport = stdio("/usr/bin/true");
        let view = approvals.prepare(&transport, now);
        let later = now + APPROVAL_TTL + Duration::from_millis(1);
        assert_eq!(
            approvals.consume(&echo(&view), &transport, later),
            Err(LaunchApprovalError::Expired)
        );
    }

    #[test]
    fn http_transport_verifies_without_minting_process_consent() {
        let approvals = McpLaunchApprovals::new();
        let now = Instant::now();
        let transport = McpTransport::Http {
            url: "https://example.invalid/mcp".parse().expect("fixture URL"),
            headers: BTreeMap::new(),
            oauth: None,
        };
        let view = approvals.prepare(&transport, now);
        assert_eq!(view.transport, "http");
        assert_eq!(view.command, None);
        assert_eq!(view.url.as_deref(), Some("https://example.invalid/mcp"));
        let result = consent_for(&approvals, &echo(&view), &transport, now);
        assert!(
            matches!(result, Ok(None)),
            "HTTP must not mint process consent"
        );
    }

    fn fingerprint(transport: &McpTransport) -> String {
        super::fingerprint(transport)
    }
}
