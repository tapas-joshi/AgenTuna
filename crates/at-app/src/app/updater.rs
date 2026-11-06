//! Signed update transport, staging and downgrade policy.

use std::path::{Path, PathBuf};

use at_model::network::{NetworkAction, NetworkEvent, NetworkTrigger};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use super::network::{NetworkState, UpdateGateError};

/// Production Ed25519 public key, stored separately from the application source.
const UPDATE_PUBLIC_KEY_HEX: &str = include_str!("../../updater-pubkey.txt");

/// A fully parsed update manifest whose signature has been verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateManifest {
    /// Semantic release version.
    pub version: String,
    /// Minimum supported operating-system version.
    pub min_os: String,
    /// Release channel (`stable` or `beta`).
    pub channel: String,
    /// HTTPS URL of the update archive.
    pub url: String,
    /// Expected lowercase SHA-256 digest of the archive.
    pub sha256: String,
    /// Optional release notes URL.
    pub notes_url: Option<String>,
    /// Publication timestamp in RFC 3339 form.
    pub published_at: String,
}

#[derive(Deserialize)]
struct SignedManifest {
    version: String,
    min_os: String,
    channel: String,
    url: String,
    sha256: String,
    notes_url: Option<String>,
    published_at: String,
}

/// Failures while authenticating or validating an update manifest.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateManifestError {
    /// The input is not valid JSON or does not have the required manifest fields.
    #[error("update manifest is malformed")]
    Malformed,
    /// Signature encoding or Ed25519 verification failed.
    #[error("update signature is invalid")]
    SignatureInvalid,
    /// The signed document contains an unsupported field value.
    #[error("update manifest has invalid field values")]
    InvalidFields,
    /// The proposed version is older than the running version.
    #[error("update downgrade refused")]
    Downgrade,
}

/// Verifies the signature over canonical JSON before decoding or trusting any URL field.
///
/// Canonicalization uses recursively key-sorted JSON and compact UTF-8 encoding; the signature
/// member is removed before serialization. Signature bytes are lowercase or uppercase hex.
///
/// # Errors
/// Returns [`UpdateManifestError::SignatureInvalid`] for any invalid signature, and typed
/// malformed/field errors for invalid documents.
pub fn verify_manifest(
    input: &[u8],
    public_key: &[u8; 32],
) -> Result<UpdateManifest, UpdateManifestError> {
    let mut value: Value =
        serde_json::from_slice(input).map_err(|_| UpdateManifestError::Malformed)?;
    let object = value
        .as_object_mut()
        .ok_or(UpdateManifestError::Malformed)?;
    let signature_text = object
        .remove("signature")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(UpdateManifestError::Malformed)?;
    let signature_bytes =
        decode_hex(&signature_text).ok_or(UpdateManifestError::SignatureInvalid)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| UpdateManifestError::SignatureInvalid)?;
    let key =
        VerifyingKey::from_bytes(public_key).map_err(|_| UpdateManifestError::SignatureInvalid)?;
    let canonical =
        serde_json::to_vec(&sort_json(value)).map_err(|_| UpdateManifestError::Malformed)?;
    key.verify(&canonical, &signature)
        .map_err(|_| UpdateManifestError::SignatureInvalid)?;

    let mut signed_value: Value =
        serde_json::from_slice(input).map_err(|_| UpdateManifestError::Malformed)?;
    let signed_object = signed_value
        .as_object_mut()
        .ok_or(UpdateManifestError::Malformed)?;
    signed_object.insert("signature".to_owned(), Value::String(signature_text));
    let signed: SignedManifest =
        serde_json::from_value(signed_value).map_err(|_| UpdateManifestError::Malformed)?;
    let archive = Url::parse(&signed.url).map_err(|_| UpdateManifestError::InvalidFields)?;
    if !(archive.scheme() == "https" || is_loopback_http(&archive))
        || !matches!(signed.channel.as_str(), "stable" | "beta")
        || signed.sha256.len() != 64
        || !signed.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || signed.version.is_empty()
        || signed.min_os.is_empty()
        || signed.published_at.is_empty()
    {
        return Err(UpdateManifestError::InvalidFields);
    }
    if let Some(notes) = &signed.notes_url
        && Url::parse(notes).is_err()
    {
        return Err(UpdateManifestError::InvalidFields);
    }
    Ok(UpdateManifest {
        version: signed.version,
        min_os: signed.min_os,
        channel: signed.channel,
        url: signed.url,
        sha256: signed.sha256,
        notes_url: signed.notes_url,
        published_at: signed.published_at,
    })
}

/// Rejects an older candidate unless the user explicitly allowed downgrade.
///
/// # Errors
/// Returns [`UpdateManifestError::Downgrade`] when the candidate is older and permission is off.
pub fn check_downgrade(
    current: &str,
    candidate: &str,
    allow_downgrade: bool,
) -> Result<(), UpdateManifestError> {
    if !allow_downgrade && compare_versions(candidate, current).is_some_and(|order| order.is_lt()) {
        return Err(UpdateManifestError::Downgrade);
    }
    Ok(())
}

fn compare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    fn components(version: &str) -> Option<Vec<u64>> {
        let version = version.strip_prefix('v').unwrap_or(version);
        let version = version.split_once('-').map_or(version, |(base, _)| base);
        version.split('.').map(|part| part.parse().ok()).collect()
    }
    let mut left = components(left)?;
    let mut right = components(right)?;
    let width = left.len().max(right.len());
    left.resize(width, 0);
    right.resize(width, 0);
    Some(left.cmp(&right))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    pairs
        .iter()
        .map(|[high, low]| {
            let high = (*high as char).to_digit(16)? as u8;
            let low = (*low as char).to_digit(16)? as u8;
            Some((high << 4) | low)
        })
        .collect()
}

fn sort_json(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: Map<String, Value> = object
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect();
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        other => other,
    }
}

/// Result status from an explicit update check.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateOutcome {
    /// The archive was authenticated, checksum-verified, and staged.
    Staged {
        /// Version of the staged update.
        version: String,
        /// Path of the verified archive in the staging directory.
        path: PathBuf,
    },
    /// No newer version was offered.
    Current,
}

/// Failure while checking, authorizing, downloading, or staging an update.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// Central network policy refused a request.
    #[error(transparent)]
    Network(#[from] UpdateGateError),
    /// Network transport failed.
    #[error("update request failed")]
    Http(#[from] reqwest::Error),
    /// Manifest verification or downgrade policy failed.
    #[error(transparent)]
    Manifest(#[from] UpdateManifestError),
    /// The downloaded bytes did not match the signed checksum.
    #[error("update checksum mismatch")]
    ChecksumMismatch,
    /// Staging via at-fs failed.
    #[error("update staging failed")]
    Stage(#[from] at_fs::FsError),
    /// A response exceeded the bounded update size.
    #[error("update payload exceeds maximum size")]
    TooLarge,
}

/// Performs a user-triggered check and stages a verified archive.
///
/// Both requests pass the central network policy before opening a socket. Loopback HTTP is
/// accepted for local fixture servers; every non-loopback endpoint must use HTTPS. Files are
/// created atomically through at-fs. This function never installs or launches an update.
///
/// # Errors
/// Returns [`UpdateError`] on policy refusal, bad signature, downgrade, transport/checksum error,
/// or staging failure. A rejected manifest causes no archive request.
pub async fn check_and_stage(
    network: &NetworkState,
    client: &reqwest::Client,
    manifest_url: &str,
    current_version: &str,
    allow_downgrade: bool,
    staging_dir: &Path,
) -> Result<UpdateOutcome, UpdateError> {
    let key_bytes =
        decode_hex(UPDATE_PUBLIC_KEY_HEX.trim()).ok_or(UpdateManifestError::SignatureInvalid)?;
    let public_key: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| UpdateManifestError::SignatureInvalid)?;
    check_and_stage_with_key(
        network,
        client,
        manifest_url,
        current_version,
        allow_downgrade,
        staging_dir,
        &public_key,
    )
    .await
}

#[doc(hidden)]
pub async fn check_and_stage_with_key(
    network: &NetworkState,
    client: &reqwest::Client,
    manifest_url: &str,
    current_version: &str,
    allow_downgrade: bool,
    staging_dir: &Path,
    public_key: &[u8; 32],
) -> Result<UpdateOutcome, UpdateError> {
    let manifest_endpoint =
        Url::parse(manifest_url).map_err(|_| UpdateManifestError::InvalidFields)?;
    if !(manifest_endpoint.scheme() == "https" || is_loopback_http(&manifest_endpoint)) {
        return Err(UpdateManifestError::InvalidFields.into());
    }
    let manifest_host = manifest_endpoint
        .host_str()
        .ok_or(UpdateManifestError::InvalidFields)?;
    let host = network
        .authorize(
            NetworkAction::UpdateCheck,
            manifest_host,
            NetworkTrigger::User,
        )
        .map_err(UpdateGateError::Denied)?;
    let response = client
        .get(manifest_endpoint)
        .send()
        .await?
        .error_for_status()?;
    let manifest_bytes = response.bytes().await?;
    network.record(NetworkEvent::new(
        time::OffsetDateTime::now_utc(),
        NetworkAction::UpdateCheck,
        Some(host),
        manifest_bytes.len() as u64,
    ));
    let manifest = verify_manifest(&manifest_bytes, public_key)?;
    check_downgrade(current_version, &manifest.version, allow_downgrade)?;
    if !compare_versions(&manifest.version, current_version).is_some_and(|order| order.is_gt()) {
        return Ok(UpdateOutcome::Current);
    }
    let archive_url = Url::parse(&manifest.url).map_err(|_| UpdateManifestError::InvalidFields)?;
    let archive_host = archive_url
        .host_str()
        .ok_or(UpdateManifestError::InvalidFields)?;
    let authorized_host = network
        .authorize(
            NetworkAction::UpdateDownload,
            archive_host,
            NetworkTrigger::User,
        )
        .map_err(UpdateGateError::Denied)?;
    if !(archive_url.scheme() == "https" || is_loopback_http(&archive_url)) {
        return Err(UpdateManifestError::InvalidFields.into());
    }
    let response = client.get(archive_url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > 1_000_000_000)
    {
        return Err(UpdateError::TooLarge);
    }
    let bytes = response.bytes().await?;
    if bytes.len() > 1_000_000_000 {
        return Err(UpdateError::TooLarge);
    }
    network.record(NetworkEvent::new(
        time::OffsetDateTime::now_utc(),
        NetworkAction::UpdateDownload,
        Some(authorized_host),
        bytes.len() as u64,
    ));
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !digest.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(UpdateError::ChecksumMismatch);
    }
    let path = staging_dir.join(format!("{}.zip", manifest.version));
    at_fs::apply(&at_fs::WriteRequest::create(&path, bytes.to_vec()))?;
    Ok(UpdateOutcome::Staged {
        version: manifest.version,
        path,
    })
}

fn is_loopback_http(url: &Url) -> bool {
    url.scheme() == "http"
        && url.host().is_some_and(|host| match host {
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
            url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_fixture(version: &str) -> (Vec<u8>, [u8; 32]) {
        let mut secret = [0_u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut secret))
            .expect("test-only OS randomness");
        let signing = SigningKey::from_bytes(&secret);
        let mut value = serde_json::json!({
            "version": version,
            "min_os": "14.0",
            "channel": "stable",
            "url": "https://updates.example.test/app.zip",
            "sha256": "a".repeat(64),
            "notes_url": null,
            "published_at": "2026-11-06T00:00:00Z"
        });
        let canonical = serde_json::to_vec(&sort_json(value.clone())).expect("canonical fixture");
        let signature = signing.sign(&canonical);
        value.as_object_mut().expect("object fixture").insert(
            "signature".to_owned(),
            Value::String(
                signature
                    .to_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            ),
        );
        (
            serde_json::to_vec(&value).expect("fixture JSON"),
            signing.verifying_key().to_bytes(),
        )
    }

    #[test]
    fn verifies_signed_manifest_and_rejects_tampering() {
        let (fixture, public_key) = signed_fixture("2.0.0");
        assert_eq!(
            verify_manifest(&fixture, &public_key)
                .expect("valid fixture")
                .version,
            "2.0.0"
        );
        let mut tampered: Value = serde_json::from_slice(&fixture).expect("JSON fixture");
        tampered["url"] = Value::String("https://evil.example.test/payload.zip".to_owned());
        let tampered = serde_json::to_vec(&tampered).expect("tampered JSON");
        assert_eq!(
            verify_manifest(&tampered, &public_key),
            Err(UpdateManifestError::SignatureInvalid)
        );
    }

    #[test]
    fn refuses_downgrade_without_explicit_override() {
        assert_eq!(
            check_downgrade("2.0.0", "1.9.9", false),
            Err(UpdateManifestError::Downgrade)
        );
        assert_eq!(check_downgrade("2.0.0", "1.9.9", true), Ok(()));
        assert_eq!(check_downgrade("1.9.9", "2.0.0", false), Ok(()));
    }
}
