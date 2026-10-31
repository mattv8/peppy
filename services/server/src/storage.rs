use std::{
    io::{Error as IoError, SeekFrom},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use aws_credential_types::Credentials;
use aws_sigv4::{
    http_request::{
        PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningParams,
        SigningSettings, UriPathNormalizationMode, sign,
    },
    sign::v4,
};
use futures_util::{Stream, StreamExt};
use quick_xml::{Reader, escape::resolve_predefined_entity, events::Event};
use reqwest::{
    Client, Method, RequestBuilder, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_RANGE, HeaderName, HeaderValue},
    redirect::Policy,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use url::Url;

use crate::config::S3Config;

pub type DataStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, IoError>> + Send + Sync + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObjectRange {
    pub start: u64,
    pub end: u64,
    pub total: u64,
}

/// TCP connect bound for every S3 request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum silence while reading any S3 response, including streamed bodies.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole-request bound for small control requests (HEAD, DELETE, probes).
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request bound for an object upload (attachments are at most 64 MiB).
const PUT_TIMEOUT: Duration = Duration::from_secs(300);
const STARTUP_PROBE_TIMEOUT: Duration = Duration::from_secs(360);
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_VALUE: &str = "AES256";
const PROBE_BYTES: usize = 20 * 1024 * 1024;
const PROBE_RANGE_START: usize = 8 * 1024 * 1024 + 13;
const PROBE_RANGE_BYTES: usize = 64;
const VERSION_LIST_BODY_LIMIT: usize = 1024 * 1024;
const VERSION_LIST_PAGE_LIMIT: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectVersion(String);

impl ObjectVersion {
    pub fn parse(value: &str) -> Option<Self> {
        (value != "null" && Self::is_valid(value)).then(|| Self(value.to_owned()))
    }

    pub(crate) fn listed(value: &str) -> Option<Self> {
        Self::is_valid(value).then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(value: &str) -> bool {
        (1..=1024).contains(&value.len())
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'_' | b'-' | b'/' | b'+' | b'=')
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionEntry {
    pub version: ObjectVersion,
    pub delete_marker: bool,
}

/// Storage failures callers must distinguish: a missing object is a durable
/// fact (409/404/410 at the API), anything else is transient (503).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum StorageError {
    #[error("storage object not found")]
    NotFound,
    #[error("storage unavailable: {0}")]
    Unavailable(&'static str),
    #[error("invalid storage request: {0}")]
    Invalid(&'static str),
}

impl StorageError {
    fn from_status(status: StatusCode, operation: &'static str) -> Self {
        if status == StatusCode::NOT_FOUND {
            Self::NotFound
        } else {
            Self::Unavailable(operation)
        }
    }
}

#[derive(Clone)]
pub struct Storage {
    endpoint: Url,
    bucket: String,
    credentials: Credentials,
    signing_region: String,
    client: Client,
    // Toggling backend versioning requires a restart so this probe-derived value can refresh.
    versioned: Arc<AtomicBool>,
}

impl Storage {
    /// Provisions when needed and verifies the backend encryption contract before
    /// publishing a usable storage instance.
    pub async fn initialize(config: &S3Config) -> Result<Self, StorageError> {
        let storage = Self::new(config);
        if !config.precreated_bucket {
            storage.ensure_bucket().await?;
        }
        storage.verify_encryption_contract().await?;
        Ok(storage)
    }

    /// Constructs an adapter without performing the startup encryption probe.
    /// Production startup must use [`Self::initialize`].
    pub fn new(config: &S3Config) -> Self {
        let credentials = Credentials::new(
            config.access_key.clone(),
            config.secret_key.clone(),
            None,
            None,
            "peppy-static-s3-config",
        );
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .expect("static S3 HTTP client configuration is valid");
        Self {
            endpoint: config.endpoint.clone(),
            bucket: config.bucket.clone(),
            credentials,
            signing_region: config.signing_region.clone(),
            client,
            versioned: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_versioned(&self) -> bool {
        self.versioned.load(Ordering::Relaxed)
    }

    /// Streams a spooled file to `key` with a signed content hash and length.
    pub async fn put_file(&self, key: &str, path: PathBuf) -> Result<(), StorageError> {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|_| StorageError::Invalid("unable to open S3 upload file"))?;
        let length = file
            .metadata()
            .await
            .map_err(|_| StorageError::Invalid("unable to inspect S3 upload file"))?
            .len();
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .await
                .map_err(|_| StorageError::Invalid("unable to hash S3 upload file"))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        file.seek(SeekFrom::Start(0))
            .await
            .map_err(|_| StorageError::Invalid("unable to rewind S3 upload file"))?;

        let url = self.object_url(key)?;
        let length_header = length.to_string();
        let body_hash = hex::encode(digest.finalize());
        let request = self.signed_request(
            Method::PUT,
            &url,
            &[
                ("content-length", length_header.as_str()),
                (SSE_HEADER, SSE_VALUE),
            ],
            SignableBody::Precomputed(body_hash),
        )?;
        // `content-length` is already on the request as a signed header; a second copy is
        // malformed framing that strict stores (Backblaze B2) reject with 400.
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
        let response = request
            .body(body)
            .timeout(PUT_TIMEOUT)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 PUT request failed"))?;
        if !response.status().is_success() {
            return Err(StorageError::Unavailable("S3 PUT rejected"));
        }
        require_sse_acknowledgement(
            response.headers(),
            "S3 PUT encryption acknowledgement missing",
        )?;
        Ok(())
    }

    /// Uploads an in-memory object used by the startup contract check.
    pub async fn put_bytes(&self, key: &str, bytes: Vec<u8>) -> Result<(), StorageError> {
        self.put_bytes_recording(key, bytes, None).await
    }

    async fn put_bytes_recording(
        &self,
        key: &str,
        bytes: Vec<u8>,
        version: Option<&Mutex<Option<ObjectVersion>>>,
    ) -> Result<(), StorageError> {
        let url = self.object_url(key)?;
        let request = self.signed_request(
            Method::PUT,
            &url,
            &[(SSE_HEADER, SSE_VALUE)],
            SignableBody::Bytes(&bytes),
        )?;
        let response = request
            .body(bytes)
            .timeout(PUT_TIMEOUT)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 PUT request failed"))?;
        if !response.status().is_success() {
            return Err(StorageError::Unavailable("S3 PUT rejected"));
        }
        if let Some(version) = version {
            *version
                .lock()
                .expect("disposable object version mutex poisoned") = response
                .headers()
                .get("x-amz-version-id")
                .and_then(|value| value.to_str().ok())
                .and_then(ObjectVersion::parse);
        }
        require_sse_acknowledgement(
            response.headers(),
            "S3 PUT encryption acknowledgement missing",
        )?;
        Ok(())
    }

    pub async fn list_versions(&self, key: &str) -> Result<Vec<VersionEntry>, StorageError> {
        let mut entries = Vec::new();
        let mut markers: Option<(String, String)> = None;
        for _ in 0..VERSION_LIST_PAGE_LIMIT {
            let mut url = self.bucket_url()?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("versions", "");
                query.append_pair("prefix", key);
                query.append_pair("max-keys", "1000");
                if let Some((key_marker, version_marker)) = &markers {
                    query.append_pair("key-marker", key_marker);
                    query.append_pair("version-id-marker", version_marker);
                }
            }
            let response = self
                .signed_request(Method::GET, &url, &[], SignableBody::empty())?
                .timeout(CONTROL_TIMEOUT)
                .send()
                .await
                .map_err(|_| StorageError::Unavailable("S3 version listing request failed"))?;
            if response.status() == StatusCode::FORBIDDEN {
                return Err(StorageError::Unavailable("S3 version listing forbidden"));
            }
            if !response.status().is_success() {
                return Err(StorageError::Unavailable("S3 version listing rejected"));
            }
            if response
                .content_length()
                .is_some_and(|length| length > VERSION_LIST_BODY_LIMIT as u64)
            {
                return Err(StorageError::Unavailable("S3 version listing too large"));
            }
            let page = parse_version_page(&read_limited_body(response).await?, key)?;
            entries.extend(page.entries);
            if !page.truncated {
                return Ok(entries);
            }
            let next_markers = page
                .next_markers
                .ok_or(StorageError::Unavailable("S3 version listing malformed"))?;
            if markers.as_ref() == Some(&next_markers) {
                return Err(StorageError::Unavailable("S3 version listing malformed"));
            }
            markers = Some(next_markers);
        }
        Err(StorageError::Unavailable("S3 version listing malformed"))
    }

    pub async fn delete_version(
        &self,
        key: &str,
        version: &ObjectVersion,
    ) -> Result<(), StorageError> {
        let mut url = self.object_url(key)?;
        url.query_pairs_mut()
            .append_pair("versionId", version.as_str());
        let response = self
            .signed_request(Method::DELETE, &url, &[], SignableBody::empty())?
            .timeout(CONTROL_TIMEOUT)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 version delete request failed"))?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else if response.status() == StatusCode::FORBIDDEN {
            Err(StorageError::Unavailable("S3 version delete forbidden"))
        } else {
            Err(StorageError::Unavailable("S3 version delete rejected"))
        }
    }

    /// Callers must purge only write-once keys: every stored version is destroyed irreversibly.
    /// Unversioned buckets fall back to a key-only delete.
    pub async fn purge(&self, key: &str) -> Result<(), StorageError> {
        if !self.is_versioned() {
            return self.delete(key).await;
        }
        let entries = self.list_versions(key).await?;
        if entries.is_empty() {
            return Ok(());
        }
        for delete_marker in [false, true] {
            for entry in entries
                .iter()
                .filter(|entry| entry.delete_marker == delete_marker)
            {
                self.delete_version(key, &entry.version).await?;
            }
        }
        if self.list_versions(key).await?.is_empty() {
            Ok(())
        } else {
            Err(StorageError::Unavailable("S3 purge incomplete"))
        }
    }

    pub async fn head_bytes(&self, key: &str) -> Result<i64, StorageError> {
        let url = self.object_url(key)?;
        let response = self
            .signed_request(Method::HEAD, &url, &[], SignableBody::empty())?
            .timeout(CONTROL_TIMEOUT)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 HEAD request failed"))?;
        if !response.status().is_success() {
            return Err(StorageError::from_status(
                response.status(),
                "S3 HEAD rejected",
            ));
        }
        require_sse_acknowledgement(
            response.headers(),
            "S3 HEAD encryption acknowledgement missing",
        )?;
        response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|length| *length >= 0)
            .ok_or(StorageError::Unavailable(
                "S3 object has invalid content length",
            ))
    }

    /// Idempotent delete: an already absent object is success.
    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let url = self.object_url(key)?;
        let response = self
            .signed_request(Method::DELETE, &url, &[], SignableBody::empty())?
            .timeout(CONTROL_TIMEOUT)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 DELETE request failed"))?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(StorageError::Unavailable("S3 DELETE rejected"))
        }
    }

    /// Streams an object. Body stalls are bounded by the client read timeout.
    pub async fn get(&self, key: &str) -> Result<DataStream, StorageError> {
        let url = self.object_url(key)?;
        let response = self
            .signed_request(Method::GET, &url, &[], SignableBody::empty())?
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 GET request failed"))?;
        if !response.status().is_success() {
            return Err(StorageError::from_status(
                response.status(),
                "S3 GET rejected",
            ));
        }
        require_sse_acknowledgement(
            response.headers(),
            "S3 GET encryption acknowledgement missing",
        )?;
        let stream = response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| IoError::other("S3 response stream failed"))
        });
        Ok(Box::pin(stream))
    }

    /// Streams an inclusive byte range after verifying that S3 returned exactly
    /// the bytes requested. The caller's known object size prevents a malformed
    /// storage response from being relayed with a false Content-Range.
    pub(crate) async fn get_range(
        &self,
        key: &str,
        range: ObjectRange,
    ) -> Result<DataStream, StorageError> {
        let url = self.object_url(key)?;
        let requested = format!("bytes={}-{}", range.start, range.end);
        let response = self
            .signed_request(
                Method::GET,
                &url,
                &[("range", requested.as_str())],
                SignableBody::empty(),
            )?
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 ranged GET request failed"))?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(StorageError::from_status(
                response.status(),
                "S3 ranged GET rejected",
            ));
        }
        let expected_length = range
            .end
            .checked_sub(range.start)
            .and_then(|length| length.checked_add(1))
            .ok_or(StorageError::Invalid("invalid requested S3 range"))?;
        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok());
        require_sse_acknowledgement(
            response.headers(),
            "S3 GET encryption acknowledgement missing",
        )?;
        if !range_response_is_valid(content_length, content_range, range, expected_length) {
            return Err(StorageError::Unavailable(
                "S3 ranged GET response is invalid",
            ));
        }
        let stream = response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| IoError::other("S3 response stream failed"))
        });
        Ok(Box::pin(stream))
    }

    /// Signed service-level request used by readiness.
    pub async fn probe(&self) -> Result<(), StorageError> {
        let mut url = self.endpoint.clone();
        url.set_path("/");
        let response = self
            .signed_request(Method::GET, &url, &[], SignableBody::empty())?
            .timeout(CONTROL_TIMEOUT)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 probe request failed"))?;
        if !response.status().is_success() {
            return Err(StorageError::Unavailable("S3 probe rejected"));
        }
        Ok(())
    }

    async fn verify_encryption_contract(&self) -> Result<(), StorageError> {
        let object = DisposableObject::new(
            self.clone(),
            format!("peppy-storage-probe-{}", uuid::Uuid::new_v4()),
        );
        let result = match tokio::time::timeout(
            STARTUP_PROBE_TIMEOUT,
            self.run_encryption_contract(&object),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(StorageError::Unavailable("S3 encryption probe timed out")),
        };
        self.versioned
            .store(object.version().is_some(), Ordering::Relaxed);
        let cleanup = object.cleanup().await;
        match (result, cleanup) {
            (Err(error), Err(cleanup_error)) => {
                tracing::warn!(%error, %cleanup_error, "storage encryption probe cleanup failed");
                Err(error)
            }
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    async fn run_encryption_contract(&self, object: &DisposableObject) -> Result<(), StorageError> {
        let key = object.key();
        let payload = probe_bytes();
        object.put_bytes(payload.clone()).await?;
        if self.head_bytes(key).await? != payload.len() as i64 {
            return Err(StorageError::Unavailable(
                "S3 encryption probe has invalid content length",
            ));
        }
        if self.read_exact(key, payload.len()).await? != payload {
            return Err(StorageError::Unavailable(
                "S3 encryption probe full read mismatch",
            ));
        }
        let range = ObjectRange {
            start: PROBE_RANGE_START as u64,
            end: (PROBE_RANGE_START + PROBE_RANGE_BYTES - 1) as u64,
            total: payload.len() as u64,
        };
        if self.read_range_exact(key, range).await?
            != payload[PROBE_RANGE_START..=range.end as usize]
        {
            return Err(StorageError::Unavailable(
                "S3 encryption probe range read mismatch",
            ));
        }
        Ok(())
    }

    async fn read_exact(&self, key: &str, expected_length: usize) -> Result<Vec<u8>, StorageError> {
        collect_exact(self.get(key).await?, expected_length).await
    }

    async fn read_range_exact(
        &self,
        key: &str,
        range: ObjectRange,
    ) -> Result<Vec<u8>, StorageError> {
        let expected_length = (range.end - range.start + 1) as usize;
        collect_exact(self.get_range(key, range).await?, expected_length).await
    }

    /// Creates the private bucket if needed; an existing bucket is success.
    pub async fn ensure_bucket(&self) -> Result<(), StorageError> {
        let url = self.bucket_url()?;
        let response = self
            .signed_request(Method::PUT, &url, &[], SignableBody::empty())?
            .body(Vec::new())
            .timeout(CONTROL_TIMEOUT)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable("S3 bucket request failed"))?;
        if response.status().is_success() || response.status() == StatusCode::CONFLICT {
            Ok(())
        } else {
            Err(StorageError::Unavailable("S3 bucket provisioning rejected"))
        }
    }

    /// Status of an unsigned GET, used to prove the bucket is not public.
    pub async fn unsigned_get_status(&self, key: &str) -> Result<StatusCode, StorageError> {
        let url = self.object_url(key)?;
        self.client
            .get(url)
            .timeout(CONTROL_TIMEOUT)
            .send()
            .await
            .map(|response| response.status())
            .map_err(|_| StorageError::Unavailable("unsigned S3 GET failed"))
    }

    fn bucket_url(&self) -> Result<Url, StorageError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| StorageError::Invalid("invalid S3 endpoint"))?
            .pop_if_empty()
            .push(&self.bucket);
        Ok(url)
    }

    fn object_url(&self, key: &str) -> Result<Url, StorageError> {
        if key.is_empty()
            || key
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            return Err(StorageError::Invalid("invalid S3 object key"));
        }
        let mut url = self.endpoint.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| StorageError::Invalid("invalid S3 endpoint"))?;
        segments.pop_if_empty().push(&self.bucket);
        for segment in key.split('/') {
            segments.push(segment);
        }
        drop(segments);
        Ok(url)
    }

    fn signed_request(
        &self,
        method: Method,
        url: &Url,
        headers: &[(&str, &str)],
        body: SignableBody<'_>,
    ) -> Result<RequestBuilder, StorageError> {
        let identity = self.credentials.clone().into();
        let mut settings = SigningSettings::default();
        // S3 canonicalizes the already encoded object path without path normalization.
        settings.percent_encoding_mode = PercentEncodingMode::Single;
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.signing_region)
            .name("s3")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|_| StorageError::Invalid("unable to construct S3 signing parameters"))?;
        let params: SigningParams<'_> = params.into();
        let signable =
            SignableRequest::new(method.as_str(), url.as_str(), headers.iter().copied(), body)
                .map_err(|_| StorageError::Invalid("unable to construct S3 signing request"))?;
        let (instructions, _) = sign(signable, &params)
            .map_err(|_| StorageError::Invalid("unable to sign S3 request"))?
            .into_parts();

        let mut request = self.client.request(method, url.clone());
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| StorageError::Invalid("invalid S3 request header"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| StorageError::Invalid("invalid S3 request header"))?;
            request = request.header(name, value);
        }
        for (name, value) in instructions.headers() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| StorageError::Invalid("invalid signed S3 header"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| StorageError::Invalid("invalid signed S3 header"))?;
            request = request.header(name, value);
        }
        Ok(request)
    }
}

pub(crate) struct DisposableObject {
    storage: Storage,
    key: String,
    version: Mutex<Option<ObjectVersion>>,
}

impl DisposableObject {
    pub(crate) fn new(storage: Storage, key: String) -> Self {
        Self {
            storage,
            key,
            version: Mutex::new(None),
        }
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    pub(crate) fn version(&self) -> Option<ObjectVersion> {
        self.version
            .lock()
            .expect("disposable object version mutex poisoned")
            .clone()
    }

    pub(crate) async fn put_bytes(&self, bytes: Vec<u8>) -> Result<(), StorageError> {
        self.storage
            .put_bytes_recording(&self.key, bytes, Some(&self.version))
            .await
    }

    pub(crate) async fn cleanup(&self) -> Result<(), StorageError> {
        match (self.storage.is_versioned(), self.version()) {
            (true, Some(version)) => {
                self.storage.delete_version(&self.key, &version).await?;
                self.storage.purge(&self.key).await
            }
            (true, None) => self.storage.purge(&self.key).await,
            (false, _) => self.storage.delete(&self.key).await,
        }
    }
}

struct VersionPage {
    entries: Vec<VersionEntry>,
    truncated: bool,
    next_markers: Option<(String, String)>,
}

async fn read_limited_body(response: reqwest::Response) -> Result<Vec<u8>, StorageError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StorageError::Unavailable("S3 version listing rejected"))?;
        if body.len().saturating_add(chunk.len()) > VERSION_LIST_BODY_LIMIT {
            return Err(StorageError::Unavailable("S3 version listing too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_version_page(body: &[u8], requested_key: &str) -> Result<VersionPage, StorageError> {
    let malformed = || StorageError::Unavailable("S3 version listing malformed");
    let mut reader = Reader::from_reader(body);
    let mut buffer = Vec::new();
    let mut field: Option<Vec<u8>> = None;
    let mut field_value = String::new();
    let mut entry = None;
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut next_key_marker = None;
    let mut next_version_marker = None;
    let mut saw_first_start = false;

    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|_| malformed())?
        {
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_vec();
                if !saw_first_start {
                    if name.as_slice() != b"ListVersionsResult" {
                        return Err(malformed());
                    }
                    saw_first_start = true;
                }
                match name.as_slice() {
                    b"Version" => entry = Some((false, None, None)),
                    b"DeleteMarker" => entry = Some((true, None, None)),
                    b"Key"
                    | b"VersionId"
                    | b"IsTruncated"
                    | b"NextKeyMarker"
                    | b"NextVersionIdMarker" => {
                        field = Some(name);
                        field_value.clear();
                    }
                    _ => {}
                }
            }
            Event::Text(text) => {
                if field.is_some() {
                    field_value.push_str(&text.decode().map_err(|_| malformed())?);
                }
            }
            Event::GeneralRef(reference) => {
                if field.is_some() {
                    if let Some(character) =
                        reference.resolve_char_ref().map_err(|_| malformed())?
                    {
                        field_value.push(character);
                    } else {
                        let name = reference.decode().map_err(|_| malformed())?;
                        field_value
                            .push_str(resolve_predefined_entity(&name).ok_or_else(malformed)?);
                    }
                }
            }
            Event::CData(text) => {
                if field.is_some() {
                    field_value.push_str(&text.decode().map_err(|_| malformed())?);
                }
            }
            Event::End(end) => {
                let name = end.local_name().as_ref().to_vec();
                if field.as_deref() == Some(name.as_slice()) {
                    let value = std::mem::take(&mut field_value);
                    field = None;
                    match name.as_slice() {
                        b"Key" => {
                            if let Some((_, key, _)) = &mut entry {
                                *key = Some(value);
                            }
                        }
                        b"VersionId" => {
                            if let Some((_, _, version)) = &mut entry {
                                *version = Some(value);
                            }
                        }
                        b"IsTruncated" => match value.trim() {
                            "true" => truncated = true,
                            "false" => truncated = false,
                            _ => return Err(malformed()),
                        },
                        b"NextKeyMarker" => next_key_marker = Some(value),
                        b"NextVersionIdMarker" => next_version_marker = Some(value),
                        _ => {}
                    }
                }
                match name.as_slice() {
                    b"Version" | b"DeleteMarker" => {
                        let (delete_marker, key, version) = entry.take().ok_or_else(malformed)?;
                        let key = key.ok_or_else(malformed)?;
                        let version = ObjectVersion::listed(&version.ok_or_else(malformed)?)
                            .ok_or_else(malformed)?;
                        if key == requested_key {
                            entries.push(VersionEntry {
                                version,
                                delete_marker,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !saw_first_start || entry.is_some() || field.is_some() {
        return Err(malformed());
    }
    let next_markers = next_key_marker.zip(next_version_marker);
    if truncated && next_markers.is_none() {
        return Err(malformed());
    }
    Ok(VersionPage {
        entries,
        truncated,
        next_markers,
    })
}

fn require_sse_acknowledgement(
    headers: &reqwest::header::HeaderMap,
    missing_message: &'static str,
) -> Result<(), StorageError> {
    if headers
        .get(SSE_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some(SSE_VALUE)
    {
        Ok(())
    } else {
        Err(StorageError::Unavailable(missing_message))
    }
}

async fn collect_exact(
    mut stream: DataStream,
    expected_length: usize,
) -> Result<Vec<u8>, StorageError> {
    let mut bytes = Vec::with_capacity(expected_length);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StorageError::Unavailable("S3 probe read failed"))?;
        if bytes.len().saturating_add(chunk.len()) > expected_length {
            return Err(StorageError::Unavailable(
                "S3 encryption probe returned too many bytes",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != expected_length {
        return Err(StorageError::Unavailable(
            "S3 encryption probe returned too few bytes",
        ));
    }
    Ok(bytes)
}

fn probe_bytes() -> Vec<u8> {
    (0..PROBE_BYTES)
        .map(|position| (position % 251) as u8)
        .collect()
}

fn content_range_matches(value: Option<&str>, expected: ObjectRange) -> bool {
    value.is_some_and(|value| {
        value
            == format!(
                "bytes {}-{}/{}",
                expected.start, expected.end, expected.total
            )
    })
}

fn range_response_is_valid(
    content_length: Option<u64>,
    content_range: Option<&str>,
    expected: ObjectRange,
    expected_length: u64,
) -> bool {
    content_length == Some(expected_length) && content_range_matches(content_range, expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::TryStreamExt;
    use std::io::Write;

    #[test]
    fn only_not_found_is_a_durable_absence() {
        assert_eq!(
            StorageError::from_status(StatusCode::NOT_FOUND, "x"),
            StorageError::NotFound
        );
        for status in [
            StatusCode::FORBIDDEN,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(
                StorageError::from_status(status, "x"),
                StorageError::Unavailable("x")
            );
        }
    }

    #[test]
    fn content_range_requires_exact_storage_response() {
        let range = ObjectRange {
            start: 2,
            end: 5,
            total: 10,
        };
        assert!(content_range_matches(Some("bytes 2-5/10"), range));
        assert!(!content_range_matches(Some("bytes 2-6/10"), range));
        assert!(!content_range_matches(Some("bytes 2-5/*"), range));
        assert!(!content_range_matches(None, range));
        assert!(range_response_is_valid(
            Some(4),
            Some("bytes 2-5/10"),
            range,
            4
        ));
        assert!(!range_response_is_valid(
            Some(3),
            Some("bytes 2-5/10"),
            range,
            4
        ));
    }

    #[test]
    fn object_version_accepts_s3_identifiers_only() {
        let b2 =
            "4_z27c88f1d182b150646ff0b16_f1004ba650fe24e6b_d20180809_m012156_c100_v0009990_t0003";
        let aws = "abc/def+ghi=jkl";
        assert_eq!(ObjectVersion::parse(b2).unwrap().as_str(), b2);
        assert_eq!(ObjectVersion::parse(aws).unwrap().as_str(), aws);
        for invalid in [
            "null".to_owned(),
            String::new(),
            "has space".to_owned(),
            "has&and".to_owned(),
            "a".repeat(1025),
        ] {
            assert_eq!(ObjectVersion::parse(&invalid), None);
        }
        assert_eq!(ObjectVersion::listed("null").unwrap().as_str(), "null");
    }

    #[test]
    fn parses_version_pages_with_exact_key_filtering_and_markers() {
        let page = parse_version_page(
            br#"<ListVersionsResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><IsTruncated>true</IsTruncated><NextKeyMarker>private/v/o</NextKeyMarker><NextVersionIdMarker>next/+=</NextVersionIdMarker><Version><Key>private/v/o</Key><VersionId>first</VersionId></Version><Version><Key>private/v/other</Key><VersionId>second</VersionId></Version><DeleteMarker><Key>private/v/o</Key><VersionId>third</VersionId></DeleteMarker></ListVersionsResult>"#,
            "private/v/o",
        )
        .expect("valid version page");
        assert!(page.truncated);
        assert_eq!(
            page.next_markers,
            Some(("private/v/o".into(), "next/+=".into()))
        );
        assert_eq!(
            page.entries,
            vec![
                VersionEntry {
                    version: ObjectVersion::parse("first").unwrap(),
                    delete_marker: false
                },
                VersionEntry {
                    version: ObjectVersion::parse("third").unwrap(),
                    delete_marker: true
                },
            ]
        );
    }

    #[test]
    fn rejects_malformed_version_pages() {
        for page in [
            &br#"<ListVersionsResult><IsTruncated>true</IsTruncated></ListVersionsResult>"#[..],
            &br#"<ListVersionsResult><Version><Key>private/v/o</Key><VersionId>invalid id</VersionId></Version></ListVersionsResult>"#[..],
            &br#"<ListVersionsResult><Version>"#[..],
        ] {
            assert!(matches!(
                parse_version_page(page, "private/v/o"),
                Err(StorageError::Unavailable("S3 version listing malformed"))
            ));
        }
    }

    #[test]
    fn parses_split_and_untrimmed_version_fields_exactly() {
        let cases = [
            (
                br#"<ListVersionsResult><IsTruncated>false</IsTruncated><Version><Key>private/v/o&amp;</Key><VersionId>one</VersionId></Version></ListVersionsResult>"#
                    .as_slice(),
                "private/v/o",
                0,
            ),
            (
                br#"<ListVersionsResult><IsTruncated>false</IsTruncated><Version><Key>a&amp;b</Key><VersionId>two</VersionId></Version></ListVersionsResult>"#
                    .as_slice(),
                "a&b",
                1,
            ),
            (
                br#"<ListVersionsResult><IsTruncated>false</IsTruncated><Version><Key><![CDATA[private/v/o]]></Key><VersionId>three</VersionId></Version></ListVersionsResult>"#
                    .as_slice(),
                "private/v/o",
                1,
            ),
            (
                br#"<ListVersionsResult><IsTruncated>false</IsTruncated><Version><Key> private/v/o </Key><VersionId>four</VersionId></Version></ListVersionsResult>"#
                    .as_slice(),
                "private/v/o",
                0,
            ),
        ];
        for (body, key, expected_entries) in cases {
            assert_eq!(
                parse_version_page(body, key)
                    .expect("valid version page")
                    .entries
                    .len(),
                expected_entries
            );
        }
    }

    #[test]
    fn version_pages_accept_null_version_ids() {
        let page = parse_version_page(
            br#"<ListVersionsResult><IsTruncated>false</IsTruncated><Version><Key>private/v/o</Key><VersionId>null</VersionId></Version></ListVersionsResult>"#,
            "private/v/o",
        )
        .expect("listing version id null is valid");
        assert_eq!(page.entries[0].version.as_str(), "null");
    }

    #[tokio::test]
    async fn disposable_object_retains_its_version_after_cancellation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let port = listener.local_addr().expect("local address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let expected = |request: &[u8]| -> Option<usize> {
                let head_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")?;
                let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))?
                    .trim()
                    .parse::<usize>()
                    .ok()?;
                Some(head_end + 4 + length)
            };
            while expected(&request).is_none_or(|total| request.len() < total) {
                let read = socket.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            tokio::io::AsyncWriteExt::write_all(
                &mut socket,
                b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nx-amz-server-side-encryption: AES256\r\nx-amz-version-id: V1\r\nconnection: close\r\n\r\n",
            )
            .await
            .expect("write response");
        });
        let storage = Storage::new(&S3Config {
            endpoint: Url::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint"),
            bucket: "bucket".into(),
            access_key: "access".into(),
            secret_key: "secret".into(),
            signing_region: "us-west-000".into(),
            precreated_bucket: true,
            readiness_timeout: Duration::from_secs(5),
        });
        let object = DisposableObject::new(storage, "private/v/o".into());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), async {
                object.put_bytes(b"object".to_vec()).await.unwrap();
                std::future::pending::<()>().await;
            })
            .await
            .is_err()
        );
        assert_eq!(object.version(), ObjectVersion::parse("V1"));
        server.await.expect("stub finished");
    }

    #[tokio::test]
    async fn delete_version_signs_the_form_encoded_target() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let port = listener.local_addr().expect("local address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            tokio::io::AsyncWriteExt::write_all(
                &mut socket,
                b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .expect("write response");
            String::from_utf8_lossy(&request).into_owned()
        });
        let storage = Storage::new(&S3Config {
            endpoint: Url::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint"),
            bucket: "bucket".into(),
            access_key: "access".into(),
            secret_key: "secret".into(),
            signing_region: "us-west-000".into(),
            precreated_bucket: true,
            readiness_timeout: Duration::from_secs(5),
        });
        let version = ObjectVersion::parse("abc/def+ghi=jkl").unwrap();
        storage
            .delete_version("private/v/o", &version)
            .await
            .expect("local stub accepts delete");
        let request = server.await.expect("captured request");
        let head = request.split("\r\n\r\n").next().unwrap_or_default();
        assert!(
            head.starts_with("DELETE /bucket/private/v/o?versionId=abc%2Fdef%2Bghi%3Djkl HTTP/1.1")
        );
        assert!(
            head.lines().any(
                |line| line.starts_with("authorization:") || line.starts_with("Authorization:")
            )
        );
    }

    /// Backblaze B2 rejects a PUT carrying two `content-length` headers as malformed
    /// framing; permissive stores (the integration fixtures) accept it silently.
    #[tokio::test]
    async fn put_file_sends_exactly_one_content_length_header() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let port = listener.local_addr().expect("local address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            // Drain the whole sized body before answering, so closing the socket never
            // resets a client that is still writing.
            let expected = |request: &[u8]| -> Option<usize> {
                let head_end = request.windows(4).position(|w| w == b"\r\n\r\n")?;
                let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())?;
                Some(head_end + 4 + length)
            };
            while expected(&request).is_none_or(|total| request.len() < total) {
                let read = socket.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            tokio::io::AsyncWriteExt::write_all(
                &mut socket,
                b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nx-amz-server-side-encryption: AES256\r\nconnection: close\r\n\r\n",
            )
            .await
            .expect("write response");
            String::from_utf8_lossy(&request).into_owned()
        });
        let storage = Storage::new(&S3Config {
            endpoint: Url::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint"),
            bucket: "bucket".into(),
            access_key: "access".into(),
            secret_key: "secret".into(),
            signing_region: "us-west-000".into(),
            precreated_bucket: true,
            readiness_timeout: Duration::from_secs(5),
        });
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        file.write_all(&[7_u8; 1024]).expect("write body");
        storage
            .put_file("private/v/o", file.path().to_path_buf())
            .await
            .expect("local stub accepts the upload");
        let request = tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("stub answered in time")
            .expect("captured request");
        let head = request.split("\r\n\r\n").next().unwrap_or_default();
        let content_length_headers = head
            .lines()
            .filter(|line| line.to_ascii_lowercase().starts_with("content-length:"))
            .collect::<Vec<_>>();
        assert_eq!(
            content_length_headers,
            vec!["content-length: 1024"],
            "request head:\n{head}"
        );
        assert!(
            !head.to_ascii_lowercase().contains("transfer-encoding"),
            "upload must be sized, not chunked:\n{head}"
        );
    }

    #[tokio::test]
    #[ignore = "requires disposable S3 configuration; run explicitly"]
    async fn disposable_storage_contract_accepts_a_64_mib_put_and_exact_ranges() {
        assert_eq!(
            std::env::var("PEPPY_DISPOSABLE_STORAGE_CONTRACT").as_deref(),
            Ok("1"),
            "set PEPPY_DISPOSABLE_STORAGE_CONTRACT=1 for this disposable-only test"
        );
        assert_ne!(
            std::env::var("PEPPY_ENV").ok().as_deref(),
            Some("production"),
            "refusing to contact production storage"
        );
        let config = crate::config::Config::from_env()
            .expect("complete disposable S3 test configuration is required");
        let storage = Storage::new(config.s3.as_ref().expect("S3 configuration is required"));
        let mut file = tempfile::NamedTempFile::new().expect("temporary file");
        const OBJECT_BYTES: usize = 64 * 1024 * 1024;
        const RANGE_START: usize = 1024;
        const RANGE_BYTES: usize = 1024;
        for offset in (0..OBJECT_BYTES).step_by(64 * 1024) {
            let bytes = patterned_bytes(offset, (OBJECT_BYTES - offset).min(64 * 1024));
            file.write_all(&bytes).expect("write patterned object");
        }
        let key = format!("storage-contract/{}", uuid::Uuid::new_v4());
        let result: Result<(), String> = async {
            storage
                .put_file(&key, file.path().to_path_buf())
                .await
                .map_err(|error| error.to_string())?;
            if storage
                .head_bytes(&key)
                .await
                .map_err(|error| error.to_string())?
                != OBJECT_BYTES as i64
            {
                return Err("stored object has an unexpected size".into());
            }
            let range = ObjectRange {
                start: RANGE_START as u64,
                end: (RANGE_START + RANGE_BYTES - 1) as u64,
                total: OBJECT_BYTES as u64,
            };
            let actual = storage
                .get(&key)
                .await
                .map_err(|error| error.to_string())?
                .try_collect::<Vec<Vec<u8>>>()
                .await
                .map_err(|error| error.to_string())?
                .concat();
            if actual != patterned_bytes(0, OBJECT_BYTES) {
                return Err("full object bytes do not match the uploaded pattern".into());
            }
            let actual = storage
                .get_range(&key, range)
                .await
                .map_err(|error| error.to_string())?
                .try_collect::<Vec<Vec<u8>>>()
                .await
                .map_err(|error| error.to_string())?
                .concat();
            if actual != patterned_bytes(RANGE_START, RANGE_BYTES) {
                return Err("ranged bytes do not match the uploaded pattern".into());
            }
            Ok(())
        }
        .await;
        let cleanup = storage.delete(&key).await;
        result.expect("disposable storage contract");
        cleanup.expect("delete contract object");
    }

    fn patterned_bytes(offset: usize, length: usize) -> Vec<u8> {
        (offset..offset + length)
            .map(|position| (position % 251) as u8)
            .collect()
    }
}
