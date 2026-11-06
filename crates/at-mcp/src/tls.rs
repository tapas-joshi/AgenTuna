//! Process-wide TLS crypto provider selection.
//!
//! The workspace builds `reqwest` with `rustls-no-provider` so no C toolchain
//! (aws-lc) is required. Building any `reqwest::Client` therefore panics unless
//! a provider has been installed first. Every code path that builds an HTTP
//! client calls [`ensure_crypto_provider`] before doing so.

use std::sync::Once;

static INSTALL: Once = Once::new();

/// Installs the `ring` rustls provider as the process default, once.
///
/// Idempotent and safe to call from any thread. If another component already
/// installed a provider, that provider is kept and the call is a no-op.
pub fn ensure_crypto_provider() {
    INSTALL.call_once(|| {
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        {
            tracing::debug!("a rustls crypto provider was already installed");
        }
    });
}
