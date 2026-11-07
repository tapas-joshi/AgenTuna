//! GitHub REST calls used by the remote-backup setup flow (TC-017).
//!
//! Two operations exist and both keep the token out of every observable surface:
//! [`GitHubClient::validate_repo_scope`] reads the `x-oauth-scopes` header and
//! refuses a token without `repo` before anything is created, and
//! [`GitHubClient::create_private_repo`] creates a private repository only after
//! that check passes. The token travels in the `Authorization` header, never in a
//! URL, a log line or an error message: every error this module produces is built
//! from static text.

use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use url::Url;

/// Production endpoint for the GitHub REST API.
pub const DEFAULT_ENDPOINT: &str = "https://api.github.com";

/// Request timeout; a hung endpoint reports [`GitHubError::Offline`] instead of blocking.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Why a GitHub setup call failed. No variant carries the token or server text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GitHubError {
    /// The endpoint is not a usable `http(s)` URL.
    #[error("the GitHub endpoint is not a valid http or https URL")]
    InvalidEndpoint,
    /// The request never reached GitHub.
    #[error("GitHub could not be reached; the request was not sent or the response was lost")]
    Offline,
    /// GitHub rejected the credential (HTTP 401 or 403).
    #[error("GitHub rejected the token: it is invalid, expired or revoked")]
    AuthRejected,
    /// The token reports scopes and `repo` is not one of them.
    #[error("the token lacks the repo scope required to create a private repository")]
    MissingRepoScope,
    /// GitHub did not report scopes, so the repo check cannot pass.
    #[error("GitHub did not report token scopes; use a classically scoped token with repo access")]
    RepoScopeUnreported,
    /// The requested repository name is empty or contains unsupported characters.
    #[error("use a repository name with letters, digits, dots, hyphens or underscores")]
    InvalidRepoName,
    /// GitHub answered but refused to create the repository.
    #[error("GitHub refused to create the private repository")]
    CreateRefused,
    /// GitHub answered with a body this client does not understand.
    #[error("GitHub returned an unexpected response")]
    UnexpectedResponse,
}

/// A repository created by [`GitHubClient::create_private_repo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedRepo {
    /// Clone URL safe to store as the remote-backup repository.
    pub url: String,
}

/// Minimal GitHub REST client for the remote-backup setup actions.
pub struct GitHubClient {
    endpoint: Url,
    http: reqwest::Client,
}

impl GitHubClient {
    /// Client for the production GitHub API.
    ///
    /// # Errors
    /// [`GitHubError::InvalidEndpoint`] when the embedded endpoint cannot build a client.
    pub fn new() -> Result<Self, GitHubError> {
        Self::with_endpoint(DEFAULT_ENDPOINT)
    }

    /// Client for an explicit API base, used by tests against a loopback fixture server.
    ///
    /// # Errors
    /// [`GitHubError::InvalidEndpoint`] when the URL is not `http(s)` or the client
    /// cannot be built.
    pub fn with_endpoint(endpoint: &str) -> Result<Self, GitHubError> {
        let endpoint = Url::parse(endpoint).map_err(|_| GitHubError::InvalidEndpoint)?;
        if !matches!(endpoint.scheme(), "https" | "http") {
            return Err(GitHubError::InvalidEndpoint);
        }
        at_mcp::tls::ensure_crypto_provider();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| GitHubError::InvalidEndpoint)?;
        Ok(Self { endpoint, http })
    }

    /// Confirms the token carries the `repo` scope required for a private repository.
    ///
    /// Reads `GET /user` and inspects `x-oauth-scopes`. A token whose scopes are not
    /// reported is refused rather than assumed to be privileged.
    ///
    /// # Errors
    /// [`GitHubError::AuthRejected`] for a rejected credential,
    /// [`GitHubError::MissingRepoScope`] for a token without `repo`,
    /// [`GitHubError::RepoScopeUnreported`] when the header is absent,
    /// [`GitHubError::Offline`] for transport failures, and
    /// [`GitHubError::UnexpectedResponse`] for other statuses.
    pub async fn validate_repo_scope(&self, token: &str) -> Result<(), GitHubError> {
        let url = self.url("/user");
        let response = self.request(reqwest::Method::GET, url, token, None).await?;
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(GitHubError::AuthRejected);
        }
        if !response.status().is_success() {
            return Err(GitHubError::UnexpectedResponse);
        }
        let scopes = response
            .headers()
            .get("x-oauth-scopes")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        match scopes {
            None => Err(GitHubError::RepoScopeUnreported),
            Some(scopes) if scopes.split(',').any(|scope| scope.trim() == "repo") => Ok(()),
            Some(_) => Err(GitHubError::MissingRepoScope),
        }
    }

    /// Creates a private repository after the repo-scope check passes.
    ///
    /// The scope check runs first, so a token without `repo` never reaches the
    /// create endpoint.
    ///
    /// # Errors
    /// Every [`validate_repo_scope`](Self::validate_repo_scope) failure, plus
    /// [`GitHubError::InvalidRepoName`] for a malformed name,
    /// [`GitHubError::CreateRefused`] when GitHub refuses (for example a taken name),
    /// and [`GitHubError::UnexpectedResponse`] for a malformed success body.
    pub async fn create_private_repo(
        &self,
        token: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<CreatedRepo, GitHubError> {
        validate_repo_name(name)?;
        self.validate_repo_scope(token).await?;
        let mut body = serde_json::json!({ "name": name, "private": true });
        if let Some(description) = description.filter(|text| !text.trim().is_empty()) {
            body["description"] = serde_json::Value::String(description.to_owned());
        }
        let url = self.url("/user/repos");
        let response = self
            .request(reqwest::Method::POST, url, token, Some(body))
            .await?;
        match response.status() {
            StatusCode::CREATED | StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(GitHubError::AuthRejected);
            }
            StatusCode::UNPROCESSABLE_ENTITY => return Err(GitHubError::CreateRefused),
            _ => return Err(GitHubError::UnexpectedResponse),
        }
        #[derive(Deserialize)]
        struct Created {
            clone_url: Option<String>,
        }
        let created: Created = response
            .json()
            .await
            .map_err(|_| GitHubError::UnexpectedResponse)?;
        let url = created.clone_url.ok_or(GitHubError::UnexpectedResponse)?;
        if !valid_clone_url(&url) {
            return Err(GitHubError::UnexpectedResponse);
        }
        Ok(CreatedRepo { url })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint.as_str().trim_end_matches('/'))
    }

    async fn request(
        &self,
        method: reqwest::Method,
        url: String,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response, GitHubError> {
        let mut request = self
            .http
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header(reqwest::header::USER_AGENT, "AgenTuna");
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.map_err(|_| GitHubError::Offline)
    }
}

/// Validates a repository name before starting an explicit GitHub request.
///
/// # Errors
///
/// [`GitHubError::InvalidRepoName`] when the value is not a supported GitHub repository name.
pub fn validate_repo_name(name: &str) -> Result<(), GitHubError> {
    if valid_repo_name(name) {
        Ok(())
    } else {
        Err(GitHubError::InvalidRepoName)
    }
}

fn valid_clone_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path().ends_with(".git")
        && url
            .path()
            .split('/')
            .filter(|part| !part.is_empty())
            .count()
            == 2
}

/// Repository names GitHub accepts for a personal repository.
fn valid_repo_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}
