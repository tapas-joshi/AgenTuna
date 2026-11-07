//! External links and webview confinement.
//!
//! Rendered Markdown is untrusted: a skill can contain any `[text](url)` pair. The preview never
//! injects HTML and never navigates the webview, so a link click has exactly one way out - the
//! [`open_external_link`] command, which validates the URL and hands it to the system browser.
//! The webview's own navigation handler ([`navigation_allowed`]) admits the app origin only and
//! refuses new windows, so a link that bypasses the preview still cannot leave the app.

use std::ffi::OsString;
use std::process::Command;
use std::time::{Duration, Instant};

use url::Url;

use super::commands::CommandError;

/// Longest link accepted from the webview.
pub const MAX_EXTERNAL_URL_BYTES: usize = 2048;

/// Port the Vite dev server listens on (`build.devUrl` in `tauri.conf.json`).
pub const DEV_SERVER_PORT: u16 = 1420;

/// How long the browser launcher may run before it is killed and reaped.
const BROWSER_GRACE: Duration = Duration::from_secs(10);

/// Validates a link from the webview and returns the parsed URL.
///
/// Only `https` URLs with a host and without userinfo are accepted. The length is bounded and
/// control characters are refused, so a crafted link cannot smuggle terminal escapes into a log
/// or a message. `javascript:`, `file:` and `data:` fail the scheme check.
///
/// # Errors
/// [`super::commands::CommandErrorCode::InvalidArgument`] for every rejected link; the message
/// names the `url` argument and never echoes the rejected value.
pub fn validate_external_url(raw: &str) -> Result<Url, CommandError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(CommandError::invalid_argument("url", "the link is empty"));
    }
    if raw.len() > MAX_EXTERNAL_URL_BYTES {
        return Err(CommandError::invalid_argument(
            "url",
            "the link is longer than the 2048-byte bound",
        ));
    }
    if raw.chars().any(char::is_control) {
        return Err(CommandError::invalid_argument(
            "url",
            "the link contains control characters",
        ));
    }
    let url = Url::parse(raw)
        .map_err(|_| CommandError::invalid_argument("url", "the link is not an absolute URL"))?;
    if url.scheme() != "https" {
        return Err(CommandError::invalid_argument(
            "url",
            "only https links open in the browser",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CommandError::invalid_argument(
            "url",
            "the link must not carry credentials",
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(CommandError::invalid_argument(
            "url",
            "the link has no host",
        ));
    }
    Ok(url)
}

/// Whether the webview may navigate to `url`.
///
/// The app origin is `tauri://localhost` (macOS, Linux) or `http(s)://tauri.localhost` (Windows
/// custom protocol). A debug build additionally accepts the Vite dev server on
/// [`DEV_SERVER_PORT`]. Everything else - including `about:`, `data:`, `file:` and any remote
/// origin - is denied.
#[must_use]
pub fn navigation_allowed(url: &Url) -> bool {
    navigation_allowed_with(url, cfg!(debug_assertions))
}

/// [`navigation_allowed`] with the dev-server exception chosen by the caller, so both the debug
/// and the release decision are testable in one build.
#[must_use]
pub fn navigation_allowed_with(url: &Url, allow_dev_server: bool) -> bool {
    if url.cannot_be_a_base() {
        return false;
    }
    let host = url.host_str().unwrap_or_default();
    let app_origin = (url.scheme() == "tauri" && host == "localhost")
        || (matches!(url.scheme(), "http" | "https")
            && host == "tauri.localhost"
            && url.port().is_none());
    app_origin
        || (allow_dev_server
            && url.scheme() == "http"
            && host == "localhost"
            && url.port_or_known_default() == Some(DEV_SERVER_PORT))
}

/// Opens a validated link in the user's default browser.
///
/// # Errors
/// [`super::commands::CommandErrorCode::InvalidArgument`] when [`validate_external_url`] rejects
/// the link, [`super::commands::CommandErrorCode::Unavailable`] when the platform launcher cannot
/// be started.
#[tauri::command]
pub async fn open_external_link(url: String) -> Result<(), CommandError> {
    let url = validate_external_url(&url)?;
    super::commands::run_blocking(move || open_in_browser(&url)).await
}

/// Spawns the platform launcher with the URL as one argv element, never through a shell.
fn open_in_browser(url: &Url) -> Result<(), CommandError> {
    let (program, mut args) = browser_launcher();
    args.push(OsString::from(url.as_str()));
    let child = Command::new(program)
        .args(&args)
        .spawn()
        .map_err(|_| CommandError::unavailable("the default browser could not be opened"))?;
    reap(child);
    Ok(())
}

/// The launcher program and the fixed argv in front of the URL.
fn browser_launcher() -> (&'static str, Vec<OsString>) {
    #[cfg(target_os = "macos")]
    {
        ("/usr/bin/open", Vec::new())
    }
    #[cfg(target_os = "windows")]
    {
        ("C:\\Windows\\explorer.exe", Vec::new())
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let program = if std::path::Path::new("/usr/bin/xdg-open").exists() {
            "/usr/bin/xdg-open"
        } else {
            "/usr/local/bin/xdg-open"
        };
        (program, Vec::new())
    }
}

/// Reaps the launcher without blocking the command for longer than [`BROWSER_GRACE`].
fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if start.elapsed() >= BROWSER_GRACE {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_EXTERNAL_URL_BYTES, navigation_allowed, navigation_allowed_with, validate_external_url,
    };
    use url::Url;

    fn allowed(raw: &str) -> bool {
        validate_external_url(raw).is_ok()
    }

    fn nav(raw: &str) -> bool {
        navigation_allowed(&Url::parse(raw).expect("test URL parses"))
    }

    fn nav_with_dev(raw: &str, dev: bool) -> bool {
        navigation_allowed_with(&Url::parse(raw).expect("test URL parses"), dev)
    }

    #[test]
    fn https_links_with_a_host_are_accepted() {
        for raw in [
            "https://example.com",
            "https://example.com/",
            "https://example.com/path/to/page?query=1&other=2#section",
            "https://sub.example.co.uk/a/b",
            "https://example.com:8443/secure",
            "https://例え.jp/パス",
            "  https://example.com/trimmed  ",
        ] {
            assert!(allowed(raw), "`{raw}` should open in the browser");
        }
    }

    #[test]
    fn non_https_and_non_web_schemes_are_rejected() {
        for raw in [
            "http://example.com",
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,<script>alert(1)</script>",
            "mailto:reviewer@example.com",
            "ftp://example.com/file",
            "tauri://localhost/index.html",
            "//example.com/no-scheme",
            "example.com/relative",
            "/relative/path",
            "",
        ] {
            assert!(!allowed(raw), "`{raw}` must not open in the browser");
        }
    }

    #[test]
    fn links_with_credentials_or_no_host_are_rejected() {
        for raw in [
            "https://user@example.com/",
            "https://user:password@example.com/",
            "https://:password@example.com/",
            "https:",
            "https://",
        ] {
            assert!(!allowed(raw), "`{raw}` must not open in the browser");
        }
    }

    #[test]
    fn links_beyond_the_length_bound_and_control_characters_are_rejected() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_EXTERNAL_URL_BYTES));
        assert!(!allowed(&long));
        assert!(!allowed("https://example.com/\u{7}bell"));
        assert!(!allowed("https://example.com/a\nb"));
    }

    #[test]
    fn the_error_names_the_url_argument_without_echoing_the_value() {
        let error = validate_external_url("javascript:alert(1)").expect_err("rejected");
        assert_eq!(
            error.code,
            super::super::commands::CommandErrorCode::InvalidArgument
        );
        assert!(error.message.contains("url"));
        assert!(!error.message.contains("alert"));
    }

    #[test]
    fn the_app_origin_navigates_and_everything_else_is_denied() {
        for raw in [
            "tauri://localhost",
            "tauri://localhost/",
            "tauri://localhost/index.html",
            "http://tauri.localhost",
            "http://tauri.localhost/index.html",
            "https://tauri.localhost/app.js",
        ] {
            assert!(nav(raw), "`{raw}` is the app origin");
        }
        for raw in [
            "https://example.com",
            "http://localhost:1420/",
            "tauri://evil.localhost/",
            "tauri://localhost.evil.com/",
            "http://evil.tauri.localhost/",
            "http://tauri.localhost:8080/",
            "https://tauri.localhost:8443/",
            "javascript:alert(1)",
            "data:text/html,<p>hi</p>",
            "file:///tmp/x",
            "about:blank",
            "tauri:settings",
        ] {
            assert!(
                !nav_with_dev(raw, false),
                "`{raw}` is outside the app origin"
            );
        }
    }

    #[test]
    fn the_dev_server_origin_is_limited_to_debug_builds_and_its_port() {
        assert!(nav_with_dev("http://localhost:1420/", true));
        assert!(nav_with_dev("http://localhost:1420/index.html", true));
        assert!(!nav_with_dev("http://localhost:1420/", false));
        for raw in [
            "http://localhost:3000/",
            "http://localhost/",
            "https://localhost:1420/",
            "http://127.0.0.1:1420/",
            "http://localhost:14200/",
        ] {
            assert!(!nav_with_dev(raw, true), "`{raw}` is not the dev origin");
        }
    }
}
