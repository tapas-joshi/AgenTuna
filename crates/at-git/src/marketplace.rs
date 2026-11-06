//! Explicit, policy-gated client for the skills.sh marketplace.

use std::path::Path;
use std::time::Duration;

use at_model::network::{
    NetworkAction, NetworkEvent, NetworkEventLog, NetworkPolicy, NetworkTrigger,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::Url;

/// Maximum search results accepted from the API.
pub const MARKETPLACE_RESULT_LIMIT: usize = 200;
/// Maximum response or cache document size in bytes.
pub const MARKETPLACE_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Marketplace cache freshness lifetime.
pub const MARKETPLACE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_ENDPOINT: &str = "https://skills.sh/api/v1/skills/search";

/// One validated marketplace listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceSkill {
    /// Stable skills.sh listing identity.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Short description when supplied by the service.
    pub description: String,
    /// GitHub owner inferred from the documented `owner/repo/skill` id.
    pub author: String,
    /// Install count reported by skills.sh.
    pub installs: u64,
    /// Git repository URL used by the existing Git import flow.
    pub source_url: String,
    /// Listing update time when supplied by the service.
    pub updated_at: Option<String>,
}

/// Cached marketplace rows and freshness status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketplacePage {
    /// Search results.
    pub skills: Vec<MarketplaceSkill>,
    /// True when returned from cache after a request failure or in offline mode.
    pub offline: bool,
    /// True when the offline cache is older than one hour.
    pub stale: bool,
}

/// Marketplace client failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MarketplaceError {
    /// Query or API endpoint is malformed.
    #[error("invalid marketplace query or endpoint")]
    InvalidInput,
    /// Policy refused the request before DNS or socket activity.
    #[error(transparent)]
    Denied(at_model::network::NetworkDenied),
    /// HTTP transport or server status failure.
    #[error("marketplace request failed")]
    Request,
    /// Response exceeded the byte bound.
    #[error("marketplace response exceeded the size limit")]
    TooLarge,
    /// Response did not match the validated API schema.
    #[error("marketplace response was invalid")]
    InvalidResponse,
    /// Cache could not be read or updated safely.
    #[error("marketplace cache operation failed: {0}")]
    Cache(String),
}

/// Client for explicit marketplace searches.
#[derive(Clone)]
pub struct MarketplaceClient {
    endpoint: Url,
    http: reqwest::Client,
}

impl MarketplaceClient {
    /// Creates a client for the official skills.sh API.
    ///
    /// # Errors
    /// Returns an error if a bounded HTTP client cannot be built.
    pub fn new() -> Result<Self, MarketplaceError> {
        Self::build(DEFAULT_ENDPOINT)
    }

    /// Creates a client for a compatible marketplace API endpoint.
    ///
    /// Requests still require explicit network-policy consent.
    ///
    /// # Errors
    /// Returns `InvalidInput` for a malformed or unsupported URL, or `Request` if HTTP setup fails.
    pub fn with_endpoint(endpoint: &str) -> Result<Self, MarketplaceError> {
        Self::build(endpoint)
    }

    fn build(endpoint: &str) -> Result<Self, MarketplaceError> {
        let endpoint = Url::parse(endpoint).map_err(|_| MarketplaceError::InvalidInput)?;
        if !matches!(endpoint.scheme(), "https" | "http")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(MarketplaceError::InvalidInput);
        }
        at_mcp::ensure_crypto_provider();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| MarketplaceError::Request)?;
        Ok(Self { endpoint, http })
    }

    /// Searches skills.sh after the caller explicitly asks, using the one-hour cache.
    ///
    /// `cache_file` is inside the AgenTuna cache directory, whose parent must already exist. A
    /// denial returns the denial as an error; offline mode returns cached rows without a request.
    /// Transport/DNS failures return cached rows with `offline=true`, or `Request` if no cache
    /// exists. Cache writes use the safe `at-fs` pipeline.
    ///
    /// # Errors
    /// Returns typed errors for denied policy, invalid input, malformed/unbounded responses, and
    /// cache failures.
    pub async fn search(
        &self,
        query: &str,
        cache_file: &Path,
        policy: &NetworkPolicy,
        log: &mut NetworkEventLog,
    ) -> Result<MarketplacePage, MarketplaceError> {
        self.search_impl(query, cache_file, policy, log, false)
            .await
    }

    /// Explicitly refreshes a query, using ETag validation when a cached page exists.
    ///
    /// # Errors
    /// See [`Self::search`].
    pub async fn refresh(
        &self,
        query: &str,
        cache_file: &Path,
        policy: &NetworkPolicy,
        log: &mut NetworkEventLog,
    ) -> Result<MarketplacePage, MarketplaceError> {
        self.search_impl(query, cache_file, policy, log, true).await
    }

    async fn search_impl(
        &self,
        query: &str,
        cache_file: &Path,
        policy: &NetworkPolicy,
        log: &mut NetworkEventLog,
        force_refresh: bool,
    ) -> Result<MarketplacePage, MarketplaceError> {
        let normalized = normalize_query(query)?;
        let cached = load_cache(cache_file, &normalized)?;
        if policy.offline() {
            return cached
                .map(|cached| page_from_cache(cached, true))
                .ok_or(MarketplaceError::Request);
        }
        let host = self
            .endpoint
            .host_str()
            .ok_or(MarketplaceError::InvalidInput)?;
        policy
            .authorize(NetworkAction::MarketplaceBrowse, host, NetworkTrigger::User)
            .map_err(MarketplaceError::Denied)?;
        if !force_refresh
            && let Some(entry) = &cached
            && !is_stale(entry.checked_at)
        {
            return Ok(page_from_cache(entry.clone(), false));
        }
        let mut url = self.endpoint.clone();
        url.query_pairs_mut()
            .append_pair("q", &normalized)
            .append_pair("limit", &MARKETPLACE_RESULT_LIMIT.to_string());
        let mut request = self.http.get(url.clone());
        if let Some(etag) = cached.as_ref().and_then(|entry| entry.etag.as_deref()) {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let mut response = match request.send().await {
            Ok(response) => response,
            Err(_) => {
                log.record(NetworkEvent::new(
                    OffsetDateTime::now_utc(),
                    NetworkAction::MarketplaceBrowse,
                    at_model::network::NetworkHost::parse(host).ok(),
                    0,
                ));
                return cached
                    .map(|entry| page_from_cache(entry, true))
                    .ok_or(MarketplaceError::Request);
            }
        };
        let status = response.status();
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 512)
            .map(str::to_owned);
        if response
            .content_length()
            .is_some_and(|length| length > MARKETPLACE_MAX_BYTES as u64)
        {
            return Err(MarketplaceError::TooLarge);
        }
        if status == reqwest::StatusCode::NOT_MODIFIED {
            log.record(NetworkEvent::new(
                OffsetDateTime::now_utc(),
                NetworkAction::MarketplaceBrowse,
                at_model::network::NetworkHost::parse(host).ok(),
                0,
            ));
            let mut entry = cached.ok_or(MarketplaceError::InvalidResponse)?;
            entry.checked_at = OffsetDateTime::now_utc().unix_timestamp();
            if etag.is_some() {
                entry.etag = etag;
            }
            save_cache(cache_file, &normalized, &entry)?;
            return Ok(page_from_cache(entry, false));
        }
        if !status.is_success() {
            log.record(NetworkEvent::new(
                OffsetDateTime::now_utc(),
                NetworkAction::MarketplaceBrowse,
                at_model::network::NetworkHost::parse(host).ok(),
                0,
            ));
            return cached
                .map(|entry| page_from_cache(entry, true))
                .ok_or(MarketplaceError::Request);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| MarketplaceError::Request)?
        {
            if bytes.len().saturating_add(chunk.len()) > MARKETPLACE_MAX_BYTES {
                return Err(MarketplaceError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        log.record(NetworkEvent::new(
            OffsetDateTime::now_utc(),
            NetworkAction::MarketplaceBrowse,
            at_model::network::NetworkHost::parse(host).ok(),
            bytes.len() as u64,
        ));
        let raw: ApiResponse =
            serde_json::from_slice(&bytes).map_err(|_| MarketplaceError::InvalidResponse)?;
        if raw.data.len() > MARKETPLACE_RESULT_LIMIT {
            return Err(MarketplaceError::InvalidResponse);
        }
        let skills = raw
            .data
            .into_iter()
            .map(validate_skill)
            .collect::<Result<Vec<_>, _>>()?;
        let entry = CacheEntry {
            query: normalized.clone(),
            checked_at: OffsetDateTime::now_utc().unix_timestamp(),
            etag,
            skills,
        };
        save_cache(cache_file, &normalized, &entry)?;
        Ok(page_from_cache(entry, false))
    }

    /// Validates a listing's source and returns the Git URL for TS-020 to import.
    ///
    /// # Errors
    /// Returns `InvalidInput` for a URL that is not a valid HTTPS GitHub repository URL.
    pub fn install_source(skill: &MarketplaceSkill) -> Result<&str, MarketplaceError> {
        let url = Url::parse(&skill.source_url).map_err(|_| MarketplaceError::InvalidInput)?;
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(MarketplaceError::InvalidInput);
        }
        let mut id_parts = skill.id.split('/');
        let (Some(owner), Some(repository), Some(_)) =
            (id_parts.next(), id_parts.next(), id_parts.next())
        else {
            return Err(MarketplaceError::InvalidInput);
        };
        if id_parts.next().is_some()
            || url.path_segments().is_none_or(|mut segments| {
                segments.next() != Some(owner)
                    || segments.next() != Some(repository)
                    || segments.next().is_some_and(|part| !part.is_empty())
            })
        {
            return Err(MarketplaceError::InvalidInput);
        }
        Ok(&skill.source_url)
    }
    /// Authorizes an explicit install before handing the listing's repository to TS-020.
    ///
    /// # Errors
    /// Returns `InvalidInput` for an unsafe source URL or `Denied` before the install begins.
    pub fn authorize_install<'a>(
        skill: &'a MarketplaceSkill,
        policy: &NetworkPolicy,
    ) -> Result<&'a str, MarketplaceError> {
        let source = Self::install_source(skill)?;
        let url = Url::parse(source).map_err(|_| MarketplaceError::InvalidInput)?;
        policy
            .authorize(
                NetworkAction::MarketplaceInstall,
                url.host_str().ok_or(MarketplaceError::InvalidInput)?,
                NetworkTrigger::User,
            )
            .map_err(MarketplaceError::Denied)?;
        Ok(source)
    }
}

#[derive(Deserialize)]
struct ApiResponse {
    data: Vec<ApiSkill>,
}
#[derive(Deserialize)]
struct ApiSkill {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    installs: u64,
    #[serde(rename = "installUrl")]
    install_url: String,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    query: String,
    checked_at: i64,
    etag: Option<String>,
    skills: Vec<MarketplaceSkill>,
}
#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    entries: Vec<CacheEntry>,
}

fn normalize_query(query: &str) -> Result<String, MarketplaceError> {
    if query.len() > 512 {
        return Err(MarketplaceError::InvalidInput);
    }
    let normalized = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.chars().count() < 2
        || normalized.chars().count() > 128
        || normalized.chars().any(char::is_control)
    {
        return Err(MarketplaceError::InvalidInput);
    }
    Ok(normalized)
}
fn validate_skill(api: ApiSkill) -> Result<MarketplaceSkill, MarketplaceError> {
    if api.id.len() > 256
        || api.name.is_empty()
        || api.name.len() > 256
        || api.name.chars().any(char::is_control)
        || api.description.len() > 4096
        || api.description.chars().any(char::is_control)
        || api.installs > i64::MAX as u64
    {
        return Err(MarketplaceError::InvalidResponse);
    }
    let mut parts = api.id.split('/');
    let author = parts
        .next()
        .filter(|value| valid_component(value))
        .ok_or(MarketplaceError::InvalidResponse)?
        .to_owned();
    if parts.next().is_none_or(|value| !valid_component(value))
        || parts.next().is_none_or(|value| !valid_component(value))
        || parts.next().is_some()
    {
        return Err(MarketplaceError::InvalidResponse);
    }
    let updated_at = api.updated_at.filter(|value| {
        value.len() <= 128
            && !value.chars().any(char::is_control)
            && OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).is_ok()
    });
    let skill = MarketplaceSkill {
        id: api.id,
        name: api.name,
        description: api.description,
        author,
        installs: api.installs,
        source_url: api.install_url,
        updated_at,
    };
    MarketplaceClient::install_source(&skill).map_err(|_| MarketplaceError::InvalidResponse)?;
    Ok(skill)
}
fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
fn is_stale(timestamp: i64) -> bool {
    OffsetDateTime::now_utc()
        .unix_timestamp()
        .saturating_sub(timestamp)
        >= MARKETPLACE_CACHE_TTL.as_secs() as i64
}
fn page_from_cache(entry: CacheEntry, offline: bool) -> MarketplacePage {
    MarketplacePage {
        skills: entry.skills,
        offline,
        stale: is_stale(entry.checked_at),
    }
}
fn load_cache(path: &Path, query: &str) -> Result<Option<CacheEntry>, MarketplaceError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MarketplaceError::Cache(error.to_string())),
    };
    if bytes.len() > MARKETPLACE_MAX_BYTES {
        return Err(MarketplaceError::TooLarge);
    }
    let cache: CacheFile =
        serde_json::from_slice(&bytes).map_err(|_| MarketplaceError::InvalidResponse)?;
    if cache.entries.len() > 64 {
        return Err(MarketplaceError::InvalidResponse);
    }
    for cached in &cache.entries {
        if normalize_query(&cached.query)? != cached.query
            || cached.skills.len() > MARKETPLACE_RESULT_LIMIT
        {
            return Err(MarketplaceError::InvalidResponse);
        }
        for skill in &cached.skills {
            let validated = validate_skill(ApiSkill {
                id: skill.id.clone(),
                name: skill.name.clone(),
                description: skill.description.clone(),
                installs: skill.installs,
                install_url: skill.source_url.clone(),
                updated_at: skill.updated_at.clone(),
            })?;
            if validated != *skill {
                return Err(MarketplaceError::InvalidResponse);
            }
        }
    }
    Ok(cache.entries.into_iter().find(|entry| entry.query == query))
}
fn save_cache(path: &Path, query: &str, entry: &CacheEntry) -> Result<(), MarketplaceError> {
    let previous = match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= MARKETPLACE_MAX_BYTES => Some(bytes),
        Ok(_) => return Err(MarketplaceError::TooLarge),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(MarketplaceError::Cache(error.to_string())),
    };
    let mut cache = match previous.as_deref() {
        Some(bytes) => serde_json::from_slice::<CacheFile>(bytes)
            .map_err(|_| MarketplaceError::InvalidResponse)?,
        None => CacheFile::default(),
    };
    cache.entries.retain(|cached| cached.query != query);
    cache.entries.push(CacheEntry {
        query: query.to_owned(),
        ..entry.clone()
    });
    if cache.entries.len() > 64 {
        cache.entries.remove(0);
    }
    let bytes = serde_json::to_vec(&cache).map_err(|_| MarketplaceError::InvalidResponse)?;
    if bytes.len() > MARKETPLACE_MAX_BYTES {
        return Err(MarketplaceError::TooLarge);
    }
    match previous {
        Some(previous) => {
            let expected = at_fs::ExpectedState::Hash(at_fs::sha256_hex(&previous));
            at_fs::apply(&at_fs::WriteRequest::replace(path, bytes, expected))
                .map_err(|error| MarketplaceError::Cache(error.to_string()))?;
        }
        None => {
            at_fs::apply(&at_fs::WriteRequest::create(path, bytes))
                .map_err(|error| MarketplaceError::Cache(error.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn rejects_marketplace_sources_outside_github() {
        let skill = MarketplaceSkill {
            id: "octo/tools/plan".to_owned(),
            name: "Plan".to_owned(),
            description: String::new(),
            author: "octo".to_owned(),
            installs: 1,
            source_url: "https://evil.example/repo".to_owned(),
            updated_at: None,
        };
        assert!(MarketplaceClient::install_source(&skill).is_err());
    }

    #[test]
    fn serves_two_hundred_fresh_cached_rows_within_the_latency_budget() {
        let directory = tempfile::tempdir().expect("temp cache");
        let cache = directory.path().join("marketplace.json");
        let skills = (0..MARKETPLACE_RESULT_LIMIT)
            .map(|index| MarketplaceSkill {
                id: format!("octo/tools/skill-{index}"),
                name: format!("Skill {index}"),
                description: "fixture listing".to_owned(),
                author: "octo".to_owned(),
                installs: index as u64,
                source_url: "https://github.com/octo/tools".to_owned(),
                updated_at: None,
            })
            .collect();
        let entry = CacheEntry {
            query: "skills".to_owned(),
            checked_at: OffsetDateTime::now_utc().unix_timestamp(),
            etag: None,
            skills,
        };
        save_cache(&cache, "skills", &entry).expect("save fixture cache");
        let client = MarketplaceClient::with_endpoint("http://127.0.0.1:1/search").expect("client");
        let policy = NetworkPolicy::default().with_marketplace_enabled(true);
        let mut log = NetworkEventLog::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let started = std::time::Instant::now();
        let page = runtime
            .block_on(client.search("skills", &cache, &policy, &mut log))
            .expect("cached marketplace page");
        assert!(started.elapsed() < Duration::from_millis(1000));
        assert_eq!(page.skills.len(), MARKETPLACE_RESULT_LIMIT);
        assert_eq!(log.entries().len(), 0);
    }
}
