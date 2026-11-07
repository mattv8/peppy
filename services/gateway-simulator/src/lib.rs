//! Deliberately simulated carrier gateway support.  It uses the public client-core and HTTP
//! contracts; the only simulated side effect is an fsynced command-id journal.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use futures_util::StreamExt;
use peppy_client_core::{
    Client, Cursor, DeviceId, PermitDecision, RawSnapshotRecord, SendResult, SnapshotPurpose,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod transport;
pub use transport::{HttpTransport, http_client, private_http_transport};

#[derive(Debug, Error)]
pub enum SimulatorError {
    #[error("SIMULATED gateway HTTP request failed")]
    Http,
    #[error("SIMULATED gateway {0} request was rejected by the server")]
    Rejected(RejectedEndpoint),
    #[error("SIMULATED gateway received invalid response")]
    Response,
    #[error("SIMULATED gateway file operation failed")]
    Io,
    #[error("SIMULATED development TLS certificate is unreadable or invalid")]
    DevelopmentCertificate,
    #[error("client-core operation failed")]
    Core,
    #[error("SIMULATED identity has prior producer history; fresh pairing required")]
    FreshPairingRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectedEndpoint {
    Outbox,
    Media,
}

impl std::fmt::Display for RejectedEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Outbox => "outbox",
            Self::Media => "media",
        })
    }
}

/// Persisted non-secret simulator routing state.  The database key is deliberately separate and
/// supplied through stdin or `PEPPY_SIMULATOR_DB_KEY_HEX` for developer tests.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimulatorState {
    pub version: u8,
    #[serde(rename = "origin")]
    pub origin: String,
    #[serde(rename = "vaultId")]
    pub vault_id: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceToken")]
    pub device_token: String,
}

pub const CREDENTIAL_FILE: &str = "credentials.json";
pub const DESKTOP_IMPORT_FILE: &str = "desktop-import.json";
pub const BOOTSTRAP_FILE: &str = "vault-bootstrap.json";
pub const DEVELOPER_KEY_FILE: &str = "SIMULATED-developer-db-key.hex";
pub const ROUTE_FILE: &str = "SIMULATED-route.json";
const MAX_JSON_RESPONSE_BYTES: usize = 8 * 1024 * 1024 + 64 * 1024;
const OUTBOX_BATCH: usize = 100;
const SNAPSHOT_APPLY_BATCH: usize = 1_000;

/// The result of one bounded durable replay request.
///
/// `replay_backlog` reflects the server page cursor, rather than the number of records that
/// happened to apply locally. Consumers can use it to schedule another bounded pass when a page
/// contains only duplicates, quarantined records, or records waiting for keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub applied: usize,
    pub replay_backlog: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimulatorRoute {
    pub subscription_id: String,
}

pub fn read_or_create_route(state_dir: &Path) -> Result<SimulatorRoute, SimulatorError> {
    let path = state_dir.join(ROUTE_FILE);
    if path.exists() {
        return serde_json::from_slice(&fs::read(path).map_err(|_| SimulatorError::Io)?)
            .map_err(|_| SimulatorError::Response);
    }
    let route = SimulatorRoute {
        subscription_id: "simulated-route-1".into(),
    };
    write_state_file(&path, &route)?;
    Ok(route)
}

fn create_private(path: &Path) -> Result<std::fs::File, SimulatorError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|_| SimulatorError::Io)
}

pub fn write_state_file<T: Serialize>(path: &Path, value: &T) -> Result<(), SimulatorError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| SimulatorError::Response)?;
    let mut file = create_private(path)?;
    file.write_all(&bytes).map_err(|_| SimulatorError::Io)?;
    file.sync_all().map_err(|_| SimulatorError::Io)
}

/// Returns the exact canonical origin accepted for authenticated simulator requests. HTTPS is
/// required except for explicit loopback development endpoints; paths, credentials, queries and
/// fragments are never accepted as part of an origin.
pub fn canonical_origin(value: &str) -> Result<String, SimulatorError> {
    let parsed = url::Url::parse(value).map_err(|_| SimulatorError::Response)?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(SimulatorError::Response);
    }
    let loopback = match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err(SimulatorError::Response);
    }
    let canonical = parsed.origin().ascii_serialization();
    if canonical != value {
        return Err(SimulatorError::Response);
    }
    Ok(canonical)
}

pub fn read_state(path: &Path) -> Result<SimulatorState, SimulatorError> {
    let state: SimulatorState =
        serde_json::from_slice(&fs::read(path).map_err(|_| SimulatorError::Io)?)
            .map_err(|_| SimulatorError::Response)?;
    if state.version != 1 || state.device_token.is_empty() {
        return Err(SimulatorError::Response);
    }
    canonical_origin(&state.origin)?;
    Ok(state)
}

pub fn write_state(path: &Path, state: &SimulatorState) -> Result<(), SimulatorError> {
    canonical_origin(&state.origin)?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|_| SimulatorError::Response)?;
    let mut file = create_private(path)?;
    file.write_all(&bytes).map_err(|_| SimulatorError::Io)?;
    file.sync_all().map_err(|_| SimulatorError::Io)?;
    Ok(())
}

fn decode_bounded_json(bytes: &[u8]) -> Result<serde_json::Value, SimulatorError> {
    if bytes.len() > MAX_JSON_RESPONSE_BYTES {
        return Err(SimulatorError::Response);
    }
    serde_json::from_slice(bytes).map_err(|_| SimulatorError::Response)
}

/// Reads one JSON response without allowing replay/snapshot metadata to exceed the server's 8 MiB
/// payload budget plus framing allowance.
pub async fn read_bounded_json(
    response: reqwest::Response,
) -> Result<serde_json::Value, SimulatorError> {
    let response = response
        .error_for_status()
        .map_err(|_| SimulatorError::Http)?;
    read_bounded_json_body(response).await
}

async fn read_bounded_json_body(
    response: reqwest::Response,
) -> Result<serde_json::Value, SimulatorError> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| SimulatorError::Http)?;
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(SimulatorError::Response)?;
        if next > MAX_JSON_RESPONSE_BYTES {
            return Err(SimulatorError::Response);
        }
        bytes.extend_from_slice(&chunk);
    }
    decode_bounded_json(&bytes)
}

fn is_producer_identity_conflict(status: reqwest::StatusCode, body: &serde_json::Value) -> bool {
    status == reqwest::StatusCode::CONFLICT && body["code"] == "idempotency_conflict"
}

fn snapshot_page_has_producer(records: &[serde_json::Value], device_id: DeviceId) -> bool {
    let own_device_id = device_id.0.to_string();
    records
        .iter()
        .any(|row| row["envelope"]["producer_device_id"].as_str() == Some(own_device_id.as_str()))
}

fn raw_replay_record(item: &serde_json::Value) -> Result<(Cursor, Vec<u8>), SimulatorError> {
    let cursor = Cursor(
        item["cursor"]
            .as_str()
            .ok_or(SimulatorError::Response)?
            .parse()
            .map_err(|_| SimulatorError::Response)?,
    );
    let envelope = item.get("envelope").ok_or(SimulatorError::Response)?;
    let raw = serde_json::to_vec(envelope).map_err(|_| SimulatorError::Response)?;
    Ok((cursor, raw))
}

/// HTTP durable replay. WebSocket notifications are intentionally only a hint; callers invoke
/// this after a hint or on their bounded periodic sync. Returns the number of records actually
/// applied by client-core, including across every bounded snapshot drain after a resync.
pub async fn sync_http(
    client: &Client,
    api_origin: &str,
    token: &str,
) -> Result<usize, SimulatorError> {
    Ok(sync_http_report(client, api_origin, token).await?.applied)
}

/// Replays one bounded durable page and exposes whether the fetched server page has more records.
pub async fn sync_http_report(
    client: &Client,
    api_origin: &str,
    token: &str,
) -> Result<SyncReport, SimulatorError> {
    let transport = transport::transport_for_validated_origin(canonical_origin(api_origin)?)?;
    sync_http_with_transport_report(client, &transport, token).await
}

/// Syncs through a host-configured private transport whose origin and redirect policy were
/// validated when it was constructed.
pub async fn sync_http_with_transport(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
) -> Result<usize, SimulatorError> {
    Ok(sync_http_with_transport_report(client, transport, token)
        .await?
        .applied)
}

/// The private-transport counterpart to [`sync_http_report`].
pub async fn sync_http_with_transport_report(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
) -> Result<SyncReport, SimulatorError> {
    let api_origin = transport.origin();
    let http = transport.client();
    let after = client.receive_cursor().map_err(|_| SimulatorError::Core)?.0;
    let response = http
        .get(format!("{api_origin}/v1/events?after={after}&limit=100"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| SimulatorError::Http)?;
    if response.status() == reqwest::StatusCode::CONFLICT {
        return Ok(SyncReport {
            applied: resync_snapshot_with_transport(client, transport, token).await?,
            replay_backlog: false,
        });
    }
    let response = read_bounded_json(response).await?;
    let events = response["events"]
        .as_array()
        .ok_or(SimulatorError::Response)?;
    for item in events {
        let (cursor, envelope_json) = raw_replay_record(item)?;
        client
            .ingest_raw(&envelope_json, cursor)
            .map_err(|_| SimulatorError::Core)?;
    }
    let replay_backlog = replay_backlog(&response)?;
    let applied = client
        .apply_pending(100)
        .map_err(|_| SimulatorError::Core)
        .map(|x| x.applied)?;
    Ok(SyncReport {
        applied,
        replay_backlog,
    })
}

fn replay_backlog(page: &serde_json::Value) -> Result<bool, SimulatorError> {
    match page.get("next_after") {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::String(_)) => Ok(true),
        _ => Err(SimulatorError::Response),
    }
}

/// Declares that this simulator build fences every snapshot page. Older or unavailable servers
/// leave the declaration inactive so legacy sync can continue; rejected credentials still fail.
pub async fn declare_compaction_capability(
    api_origin: &str,
    token: &str,
) -> Result<bool, SimulatorError> {
    let api_origin = canonical_origin(api_origin)?;
    let transport = transport::transport_for_validated_origin(api_origin)?;
    declare_compaction_capability_with_transport(&transport, token).await
}

/// Declares compaction fencing through a host-configured private transport.
pub async fn declare_compaction_capability_with_transport(
    transport: &HttpTransport,
    token: &str,
) -> Result<bool, SimulatorError> {
    let response = match transport
        .client()
        .post(format!("{}/v1/compaction/capability", transport.origin()))
        .bearer_auth(token)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return Ok(false),
    };
    match response.status() {
        reqwest::StatusCode::NO_CONTENT => Ok(true),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            Err(SimulatorError::Http)
        }
        _ => Ok(false),
    }
}

pub async fn publish_capabilities(
    api_origin: &str,
    token: &str,
    subscription_id: &str,
) -> Result<(), SimulatorError> {
    let api_origin = canonical_origin(api_origin)?;
    http_client(&api_origin)?.post(format!("{api_origin}/v1/capabilities")).bearer_auth(token)
        .json(&serde_json::json!({"simulator":true,"capabilities":{"sims":[{"subscription_id":subscription_id,"label":"Simulated SIM","sms":"available","mms":"available","mms_content_version":3}]}}))
        .send().await.map_err(|_| SimulatorError::Http)?.error_for_status().map_err(|_| SimulatorError::Http)?;
    Ok(())
}

/// Imports immutable history in staged pages after a typed replay expiry/ahead response. Snapshot
/// commands remain historical inside client-core and therefore never reach carrier effects. The
/// return value is the accumulated `apply_pending` applied count, never the journaled count.
pub async fn resync_snapshot(
    client: &Client,
    api_origin: &str,
    token: &str,
) -> Result<usize, SimulatorError> {
    let transport = transport::transport_for_validated_origin(canonical_origin(api_origin)?)?;
    resync_snapshot_with_transport(client, &transport, token).await
}

async fn resync_snapshot_with_transport(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
) -> Result<usize, SimulatorError> {
    import_snapshot(client, transport, token, None).await
}

/// Bootstraps a newly created simulator database from immutable history before any controls,
/// uploads, or permits are enabled. Prior records produced by this same device identity prove that
/// its original database was lost, so the identity must be freshly paired instead of reused.
pub async fn bootstrap_snapshot(
    client: &Client,
    api_origin: &str,
    token: &str,
    own_device_id: DeviceId,
) -> Result<usize, SimulatorError> {
    let transport = transport::transport_for_validated_origin(canonical_origin(api_origin)?)?;
    import_snapshot(client, &transport, token, Some(own_device_id)).await
}

async fn import_snapshot(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
    reject_producer: Option<DeviceId>,
) -> Result<usize, SimulatorError> {
    let api_origin = transport.origin();
    let http = transport.client();
    let snapshot = read_bounded_json(
        http.get(format!("{api_origin}/v1/snapshot"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| SimulatorError::Http)?,
    )
    .await?;
    let high_water = Cursor(
        snapshot["high_water_cursor"]
            .as_str()
            .ok_or(SimulatorError::Response)?
            .parse()
            .map_err(|_| SimulatorError::Response)?,
    );
    let count: u64 = snapshot["record_count"]
        .as_str()
        .ok_or(SimulatorError::Response)?
        .parse()
        .map_err(|_| SimulatorError::Response)?;
    let compaction_generation = match snapshot.get("compaction_supported") {
        None | Some(serde_json::Value::Bool(false)) => None,
        Some(serde_json::Value::Bool(true)) => {
            let value = snapshot["compaction_generation"]
                .as_str()
                .ok_or(SimulatorError::Response)?;
            let generation = value.parse::<u64>().map_err(|_| SimulatorError::Response)?;
            if generation.to_string() != value {
                return Err(SimulatorError::Response);
            }
            Some(generation)
        }
        _ => return Err(SimulatorError::Response),
    };
    // A 409 may arrive after a prior snapshot was published but before it fully drained. Core
    // deliberately rejects replacing that generation, so finish its bounded drain first.
    let mut applied = drain_published_snapshot(client).await?;
    let mut progress = client
        .begin_snapshot_with_compaction(
            high_water,
            count,
            SnapshotPurpose::Resync,
            compaction_generation,
        )
        .map_err(|_| SimulatorError::Core)?;
    let mut after = 0_u64;
    while progress.received_records < count {
        let page = read_bounded_json(
            http.get(format!(
                "{api_origin}/v1/snapshot/records?high_water={}&after={after}&limit=200{}",
                high_water.0,
                progress
                    .server_compaction_generation
                    .map(|generation| format!("&compaction_generation={generation}"))
                    .unwrap_or_default(),
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| SimulatorError::Http)?,
        )
        .await?;
        let records = page["records"].as_array().ok_or(SimulatorError::Response)?;
        if records.is_empty() {
            return Err(SimulatorError::Response);
        }
        if reject_producer.is_some_and(|device_id| snapshot_page_has_producer(records, device_id)) {
            return Err(SimulatorError::FreshPairingRequired);
        }
        // Keep canonical raw JSON for C5: malformed/oversize records must be staged and
        // quarantined during drain, never discarded by host-side envelope parsing.
        let parsed: Result<Vec<_>, _> = records
            .iter()
            .map(|row| {
                Ok(RawSnapshotRecord {
                    cursor: Cursor(
                        row["cursor"]
                            .as_str()
                            .ok_or(SimulatorError::Response)?
                            .parse()
                            .map_err(|_| SimulatorError::Response)?,
                    ),
                    envelope_json: serde_json::to_vec(&row["envelope"])
                        .map_err(|_| SimulatorError::Response)?,
                })
            })
            .collect();
        let parsed = parsed?;
        after = parsed.last().ok_or(SimulatorError::Response)?.cursor.0;
        progress = client
            .append_snapshot_raw_page(progress.generation, &parsed)
            .map_err(|_| SimulatorError::Core)?;
    }
    client
        .finish_snapshot(progress.generation)
        .map_err(|_| SimulatorError::Core)?;
    // Match normal sync_http: return records actually applied, not records merely journaled.
    // Published generations drain in bounded batches. A zero-applied batch is not terminal because
    // malformed/history rows may have drained while further staging remains.
    loop {
        let report = client
            .apply_pending(SNAPSHOT_APPLY_BATCH)
            .map_err(|_| SimulatorError::Core)?;
        applied = applied
            .checked_add(report.applied)
            .ok_or(SimulatorError::Response)?;
        if report.snapshot_remaining == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    Ok(applied)
}

async fn drain_published_snapshot(client: &Client) -> Result<usize, SimulatorError> {
    let mut applied = 0_usize;
    loop {
        let report = client
            .apply_pending(SNAPSHOT_APPLY_BATCH)
            .map_err(|_| SimulatorError::Core)?;
        applied = applied
            .checked_add(report.applied)
            .ok_or(SimulatorError::Response)?;
        if report.snapshot_remaining == 0 {
            return Ok(applied);
        }
        tokio::task::yield_now().await;
    }
}

pub async fn upload_pending(
    client: &Client,
    api_origin: &str,
    token: &str,
) -> Result<usize, SimulatorError> {
    let transport = transport::transport_for_validated_origin(canonical_origin(api_origin)?)?;
    upload_pending_with_transport(client, &transport, token).await
}

pub async fn upload_pending_with_transport(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
) -> Result<usize, SimulatorError> {
    upload_pending_with_transport_retention(client, transport, token, false).await
}

/// Publishes through a runtime that has completed retention negotiation.
pub async fn upload_pending_with_transport_retention(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
    retention_protocol: bool,
) -> Result<usize, SimulatorError> {
    let api_origin = transport.origin();
    let http = transport.client();
    let mut uploaded = 0_usize;
    loop {
        client
            .seal_pending_batch(OUTBOX_BATCH)
            .map_err(|_| SimulatorError::Core)?;
        let pending = client
            .pending_outbox_batch(OUTBOX_BATCH)
            .map_err(|_| SimulatorError::Core)?;
        if pending.is_empty() {
            return Ok(uploaded);
        }
        for envelope in &pending {
            let response = retention_request(
                http.post(format!("{api_origin}/v1/events")),
                retention_protocol,
            )
            .bearer_auth(token)
            .json(envelope)
            .send()
            .await
            .map_err(|_| SimulatorError::Http)?;
            if response.status() == reqwest::StatusCode::CONFLICT {
                let body = read_bounded_json_body(response).await?;
                if is_producer_identity_conflict(reqwest::StatusCode::CONFLICT, &body) {
                    return Err(SimulatorError::FreshPairingRequired);
                }
                if body
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                {
                    return Err(SimulatorError::Rejected(RejectedEndpoint::Outbox));
                }
                return Err(SimulatorError::Http);
            }
            upload_response(response, RejectedEndpoint::Outbox).await?;
            client
                .ack_outbox(envelope.envelope_id)
                .map_err(|_| SimulatorError::Core)?;
            uploaded = uploaded.checked_add(1).ok_or(SimulatorError::Response)?;
        }
    }
}

/// Bounded native-host attachment transfer. File keys never leave client-core: this function sees
/// only ciphertext paths, authenticated sizes/digests, and opaque server object IDs.
pub async fn drain_media(
    client: &Client,
    api_origin: &str,
    token: &str,
    state_dir: &Path,
) -> Result<(), SimulatorError> {
    let transport = transport::transport_for_validated_origin(canonical_origin(api_origin)?)?;
    drain_media_with_transport(client, &transport, token, state_dir).await
}

pub async fn drain_media_with_transport(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
    state_dir: &Path,
) -> Result<(), SimulatorError> {
    upload_media_with_transport(client, transport, token).await?;
    download_media_with_transport(client, transport, token, state_dir).await
}

/// Uploads pending attachment ciphertext without exposing media keys to the host.
pub async fn upload_media_with_transport(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
) -> Result<(), SimulatorError> {
    upload_media_with_transport_retention(client, transport, token, false).await
}

/// Uploads media with core-owned retention metadata after protocol negotiation.
pub async fn upload_media_with_transport_retention(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
    retention_protocol: bool,
) -> Result<(), SimulatorError> {
    let api_origin = transport.origin();
    let http = transport.client();
    for item in client.pending_uploads().map_err(|_| SimulatorError::Core)? {
        let retention = client
            .attachment_upload_retention(item.attachment_id)
            .map_err(|_| SimulatorError::Core)?;
        let retention_class = match retention.retention_class {
            peppy_client_core::AttachmentRetentionClass::Protected => "protected",
            peppy_client_core::AttachmentRetentionClass::MessageMedia => "message_media",
        };
        let reservation = read_upload_json(retention_request(http.post(format!("{api_origin}/v1/attachments/reserve")), retention_protocol)
            .bearer_auth(token).json(&serde_json::json!({"attachment_id":item.attachment_id.0,"declared_ciphertext_bytes":item.ciphertext_bytes,"declared_ciphertext_sha256":item.ciphertext_sha256,"retention_class":retention_class,"reference_tracking":retention.reference_tracking}))
            .send().await.map_err(|_| SimulatorError::Http)?).await?;
        let remote = reservation["attachment_id"]
            .as_str()
            .ok_or(SimulatorError::Response)?;
        let path = client
            .native_cipher_file(item.attachment_id)
            .map_err(|_| SimulatorError::Core)?;
        let source = tokio::fs::File::open(path)
            .await
            .map_err(|_| SimulatorError::Io)?;
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(source));
        upload_response(
            retention_request(
                http.put(format!("{api_origin}/v1/attachments/{remote}/upload")),
                retention_protocol,
            )
            .bearer_auth(token)
            .body(body)
            .send()
            .await
            .map_err(|_| SimulatorError::Http)?,
            RejectedEndpoint::Media,
        )
        .await?;
        upload_response(
            retention_request(
                http.post(format!("{api_origin}/v1/attachments/{remote}/finalize")),
                retention_protocol,
            )
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| SimulatorError::Http)?,
            RejectedEndpoint::Media,
        )
        .await?;
        client
            .mark_attachment_uploaded(item.attachment_id, remote)
            .map_err(|_| SimulatorError::Core)?;
    }
    Ok(())
}

fn retention_request(request: reqwest::RequestBuilder, enabled: bool) -> reqwest::RequestBuilder {
    if enabled {
        request.header(
            peppy_hosted_api::attachments::ATTACHMENT_RETENTION_PROTOCOL_HEADER,
            peppy_hosted_api::attachments::ATTACHMENT_RETENTION_PROTOCOL_VERSION,
        )
    } else {
        request
    }
}

/// Installs pending attachment ciphertext after removing stale simulator-only partial files.
pub async fn download_media_with_transport(
    client: &Client,
    transport: &HttpTransport,
    token: &str,
    state_dir: &Path,
) -> Result<(), SimulatorError> {
    let api_origin = transport.origin();
    let http = transport.client();
    let download_dir = state_dir.join("SIMULATED-downloads");
    ensure_download_dir(state_dir, &download_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&download_dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| SimulatorError::Io)?;
    }
    remove_stale_download_parts(&download_dir).await?;
    for item in client
        .pending_downloads()
        .map_err(|_| SimulatorError::Core)?
    {
        let remote = item
            .remote_object_id
            .as_deref()
            .ok_or(SimulatorError::Response)?;
        remove_file_if_present(&download_dir.join(format!("{}.ppss", item.attachment_id.0)))
            .await?;
        let target = download_dir.join(format!(
            "{}.{}.part",
            item.attachment_id.0,
            uuid::Uuid::new_v4()
        ));
        let download = async {
            let mut output_options = tokio::fs::OpenOptions::new();
            output_options.write(true).create_new(true);
            #[cfg(unix)]
            {
                output_options.mode(0o600);
            }
            let mut output = output_options
                .open(&target)
                .await
                .map_err(|_| SimulatorError::Io)?;
            let response = http
                .get(format!("{api_origin}/v1/attachments/{remote}"))
                .bearer_auth(token)
                .send()
                .await
                .map_err(|_| SimulatorError::Http)?;
            if response.status() == reqwest::StatusCode::GONE {
                let body = read_bounded_json_body(response).await?;
                if body["code"] == "attachment_removed_by_storage_policy" {
                    client
                        .report_attachment_removed_by_storage_policy(item.attachment_id)
                        .map_err(|_| SimulatorError::Core)?;
                    return Ok(());
                }
                return Err(SimulatorError::Http);
            }
            let mut stream = response
                .error_for_status()
                .map_err(|_| SimulatorError::Http)?
                .bytes_stream();
            let mut total = 0_u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| SimulatorError::Http)?;
                total = total
                    .checked_add(chunk.len() as u64)
                    .ok_or(SimulatorError::Response)?;
                if total > item.ciphertext_bytes {
                    return Err(SimulatorError::Response);
                }
                tokio::io::AsyncWriteExt::write_all(&mut output, &chunk)
                    .await
                    .map_err(|_| SimulatorError::Io)?;
            }
            tokio::io::AsyncWriteExt::flush(&mut output)
                .await
                .map_err(|_| SimulatorError::Io)?;
            client
                .install_downloaded_attachment(item.attachment_id, &target)
                .map_err(|_| SimulatorError::Core)
        }
        .await;
        let _ = tokio::fs::remove_file(&target).await;
        download?;
    }
    Ok(())
}

async fn upload_response(
    response: reqwest::Response,
    endpoint: RejectedEndpoint,
) -> Result<reqwest::Response, SimulatorError> {
    if response.status().is_success() {
        return Ok(response);
    }
    if response.status().is_client_error() {
        return match read_bounded_json_body(response).await {
            Ok(body)
                if body
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .is_some() =>
            {
                Err(SimulatorError::Rejected(endpoint))
            }
            _ => Err(SimulatorError::Http),
        };
    }
    Err(SimulatorError::Http)
}

async fn read_upload_json(
    response: reqwest::Response,
) -> Result<serde_json::Value, SimulatorError> {
    read_bounded_json_body(upload_response(response, RejectedEndpoint::Media).await?).await
}

async fn remove_stale_download_parts(download_dir: &Path) -> Result<(), SimulatorError> {
    let mut entries = tokio::fs::read_dir(download_dir)
        .await
        .map_err(|_| SimulatorError::Io)?;
    while let Some(entry) = entries.next_entry().await.map_err(|_| SimulatorError::Io)? {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "part")
        {
            remove_file_if_present(&entry.path()).await?;
        }
    }
    Ok(())
}

fn ensure_download_dir(state_dir: &Path, download_dir: &Path) -> Result<(), SimulatorError> {
    fs::create_dir_all(state_dir).map_err(|_| SimulatorError::Io)?;
    match fs::symlink_metadata(download_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() => {
            Err(SimulatorError::Io)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(download_dir).map_err(|_| SimulatorError::Io)
        }
        Err(_) => Err(SimulatorError::Io),
    }
}

async fn remove_file_if_present(path: &Path) -> Result<(), SimulatorError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SimulatorError::Io),
    }
}

fn open_private_effect_log(path: &Path) -> Result<std::fs::File, SimulatorError> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| SimulatorError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| SimulatorError::Io)?;
    }
    Ok(file)
}

/// Execute one simulated carrier attempt. The effect journal is fsynced *after* client-core's
/// durable permit. `crash_after_effect` models process death before the encrypted result exists.
pub fn run_one_carrier_effect(
    client: &Client,
    effects: &Path,
    crash_after_effect: bool,
) -> Result<bool, SimulatorError> {
    let Some(command) = client
        .pending_commands()
        .map_err(|_| SimulatorError::Core)?
        .into_iter()
        .next()
    else {
        return Ok(false);
    };
    let permit = client
        .begin_send_attempt(command.command_id)
        .map_err(|_| SimulatorError::Core)?;
    let PermitDecision::Permit(_) = permit else {
        return Ok(false);
    };
    let mut effect = open_private_effect_log(effects)?;
    writeln!(effect, "{}", command.command_id.0).map_err(|_| SimulatorError::Io)?;
    effect.sync_all().map_err(|_| SimulatorError::Io)?;
    if crash_after_effect {
        return Ok(true);
    }
    client
        .record_send_result(command.command_id, SendResult::Sent)
        .map_err(|_| SimulatorError::Core)?;
    Ok(true)
}

/// Runtime route fence: commands for another advertised SIM remain durable reconciliation work,
/// not simulated carrier effects.
pub fn run_one_carrier_effect_for_route(
    client: &Client,
    effects: &Path,
    subscription_id: &str,
    crash_after_effect: bool,
) -> Result<bool, SimulatorError> {
    let command = client
        .pending_commands()
        .map_err(|_| SimulatorError::Core)?
        .into_iter()
        .find(|command| command.subscription_id == subscription_id);
    let Some(command) = command else {
        return Ok(false);
    };
    let permit = client
        .begin_send_attempt(command.command_id)
        .map_err(|_| SimulatorError::Core)?;
    let PermitDecision::Permit(_) = permit else {
        return Ok(false);
    };
    let mut effect = open_private_effect_log(effects)?;
    writeln!(effect, "{}", command.command_id.0).map_err(|_| SimulatorError::Io)?;
    effect.sync_all().map_err(|_| SimulatorError::Io)?;
    if crash_after_effect {
        return Ok(true);
    }
    client
        .record_send_result(command.command_id, SendResult::Sent)
        .map_err(|_| SimulatorError::Core)?;
    Ok(true)
}

pub fn effects_path(state_dir: &Path) -> PathBuf {
    state_dir.join("SIMULATED-carrier-effects.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_origins_are_canonical_https_or_loopback() {
        assert_eq!(
            canonical_origin("https://push.example.test").unwrap(),
            "https://push.example.test"
        );
        assert_eq!(
            canonical_origin("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            canonical_origin("http://[::1]:8080").unwrap(),
            "http://[::1]:8080"
        );
        for rejected in [
            "http://push.example.test",
            "https://push.example.test/",
            "https://user@push.example.test",
            "https://push.example.test/path",
            "https://push.example.test?query=1",
            "file:///tmp/socket",
        ] {
            assert!(
                matches!(canonical_origin(rejected), Err(SimulatorError::Response)),
                "accepted {rejected}"
            );
        }
    }

    #[test]
    fn json_and_raw_replay_boundaries_fail_closed_without_typed_envelope_parsing() {
        assert!(decode_bounded_json(br#"{"ok":true}"#).is_ok());
        assert!(matches!(
            decode_bounded_json(&vec![b' '; MAX_JSON_RESPONSE_BYTES + 1]),
            Err(SimulatorError::Response)
        ));
        let malformed = serde_json::json!({"cursor":"7","envelope":{"not":"an envelope"}});
        let (cursor, raw) = raw_replay_record(&malformed).unwrap();
        assert_eq!(cursor, Cursor(7));
        assert!(serde_json::from_slice::<peppy_client_core::Envelope>(&raw).is_err());
    }

    #[test]
    fn replay_backlog_uses_the_fetched_page_cursor_not_local_apply_count() {
        assert!(replay_backlog(&serde_json::json!({"next_after":"100"})).unwrap());
        assert!(!replay_backlog(&serde_json::json!({"next_after":null})).unwrap());
        assert!(matches!(
            replay_backlog(&serde_json::json!({"next_after":100})),
            Err(SimulatorError::Response)
        ));
    }

    #[tokio::test]
    async fn published_snapshot_drains_before_a_replacement_snapshot_begins() {
        let temp = tempfile::TempDir::new().unwrap();
        let vault = fake_http_tests::vault();
        let client = fake_http_tests::client(temp.path(), "client", &vault, DeviceId::new());
        let published = client
            .begin_snapshot(Cursor(1), 1, SnapshotPurpose::Resync)
            .unwrap();
        client
            .append_snapshot_raw_page(
                published.generation,
                &[RawSnapshotRecord {
                    cursor: Cursor(1),
                    envelope_json: br#"{"not":"an envelope"}"#.to_vec(),
                }],
            )
            .unwrap();
        client.finish_snapshot(published.generation).unwrap();

        assert_eq!(drain_published_snapshot(&client).await.unwrap(), 0);
        assert!(
            client
                .begin_snapshot(Cursor(1), 0, SnapshotPurpose::Resync)
                .is_ok()
        );
    }

    #[test]
    fn producer_idempotency_conflict_requires_fresh_pairing() {
        assert!(is_producer_identity_conflict(
            reqwest::StatusCode::CONFLICT,
            &serde_json::json!({"code":"idempotency_conflict"})
        ));
        assert!(!is_producer_identity_conflict(
            reqwest::StatusCode::CONFLICT,
            &serde_json::json!({"code":"retired_key_epoch"})
        ));
    }

    #[test]
    fn snapshot_identity_comparison_detects_only_own_producer_history() {
        let own = DeviceId::new();
        let other = DeviceId::new();
        let records = vec![serde_json::json!({
            "envelope":{"producer_device_id":own.0}
        })];
        assert!(snapshot_page_has_producer(&records, own));
        assert!(!snapshot_page_has_producer(&records, other));
    }

    #[cfg(unix)]
    #[test]
    fn credential_state_is_private_at_creation() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("credentials.json");
        write_state(
            &path,
            &SimulatorState {
                version: 1,
                origin: "https://push.example.test".into(),
                vault_id: "vault".into(),
                device_id: "device".into(),
                device_token: "not-logged".into(),
            },
        )
        .unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(test)]
mod fake_http_tests {
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post, put},
    };
    use peppy_client_core::{
        AttachmentState, ClientConfig, ConversationId, DatabaseKey, GatewayRoute, IncomingMms,
        KeyProfile, OutgoingSms, VaultCheckHeader, VaultId,
    };
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use tempfile::TempDir;

    use super::*;

    const PHRASE: &str = "correct horse battery staple";
    const REMOTE: &str = "6f1d2c3b-4a59-4e7f-8a1b-2c3d4e5f6a7b";

    /// Download behaviour served for each successive GET of the stored ciphertext.
    #[derive(Clone, Copy)]
    enum Download {
        ServerError,
        Truncated,
        Complete,
        StoragePolicyRemoved,
        GenericGone,
    }

    #[derive(Default)]
    struct FakeState {
        ciphertext: Vec<u8>,
        events: Vec<serde_json::Value>,
        downloads: Vec<Download>,
        reject_events: bool,
        event_status: Option<StatusCode>,
        reject_media: bool,
    }

    type Shared = Arc<Mutex<FakeState>>;

    async fn reserve(State(state): State<Shared>) -> Response {
        if state.lock().unwrap().reject_media {
            return policy_rejection();
        }
        Json(serde_json::json!({ "attachment_id": REMOTE })).into_response()
    }

    async fn upload(State(state): State<Shared>, body: Bytes) -> StatusCode {
        state.lock().unwrap().ciphertext = body.to_vec();
        StatusCode::NO_CONTENT
    }

    async fn finalize() -> StatusCode {
        StatusCode::OK
    }

    async fn event(
        State(state): State<Shared>,
        Json(envelope): Json<serde_json::Value>,
    ) -> Response {
        let mut state = state.lock().unwrap();
        if let Some(status) = state.event_status {
            return status.into_response();
        }
        if state.reject_events {
            return policy_rejection();
        }
        state.events.push(envelope);
        Json(serde_json::json!({ "cursor": state.events.len().to_string(), "duplicate": false }))
            .into_response()
    }

    fn policy_rejection() -> Response {
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"code":"operation_not_permitted"})),
        )
            .into_response()
    }

    async fn download(State(state): State<Shared>) -> Response {
        let mut state = state.lock().unwrap();
        let mode = if state.downloads.is_empty() {
            Download::Complete
        } else {
            state.downloads.remove(0)
        };
        match mode {
            Download::ServerError => {
                (StatusCode::INTERNAL_SERVER_ERROR, Vec::new()).into_response()
            }
            Download::Truncated => (
                StatusCode::OK,
                state.ciphertext[..state.ciphertext.len() - 1].to_vec(),
            )
                .into_response(),
            Download::Complete => (StatusCode::OK, state.ciphertext.clone()).into_response(),
            Download::StoragePolicyRemoved => (
                StatusCode::GONE,
                Json(serde_json::json!({"code":"attachment_removed_by_storage_policy"})),
            )
                .into_response(),
            Download::GenericGone => (
                StatusCode::GONE,
                Json(serde_json::json!({"code":"not_found"})),
            )
                .into_response(),
        }
    }

    async fn start_fake(state: Shared) -> String {
        let router = Router::new()
            .route("/v1/attachments/reserve", post(reserve))
            .route("/v1/attachments/{id}/upload", put(upload))
            .route("/v1/attachments/{id}/finalize", post(finalize))
            .route("/v1/attachments/{id}", get(download))
            .route("/v1/events", post(event))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    pub(super) struct Vault {
        id: VaultId,
        profile: KeyProfile,
        header: VaultCheckHeader,
    }

    pub(super) fn vault() -> Vault {
        let id = VaultId::new();
        let profile = KeyProfile::new(id.0, 1).unwrap();
        let root = derive_root_key(PHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        Vault {
            id,
            profile,
            header,
        }
    }

    pub(super) fn client(dir: &Path, name: &str, vault: &Vault, device_id: DeviceId) -> Client {
        let client = Client::open(
            ClientConfig {
                database_path: dir.join(format!("{name}.db")),
                vault_id: vault.id,
                device_id,
            },
            DatabaseKey::new(&[name.as_bytes()[0]; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&vault.profile, &vault.header, PHRASE)
            .unwrap();
        client
    }

    /// Delivers every captured event to `client` in server cursor order.
    fn deliver(client: &Client, state: &Shared, first_cursor: usize) {
        let events = state.lock().unwrap().events.clone();
        for (index, envelope) in events.iter().enumerate().skip(first_cursor - 1) {
            client
                .ingest_raw(
                    &serde_json::to_vec(envelope).unwrap(),
                    Cursor(index as u64 + 1),
                )
                .unwrap();
        }
        client.apply_pending(100).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interrupted_downloads_leave_no_partial_file_and_retry_installs() {
        let state: Shared = Arc::default();
        let origin = start_fake(state.clone()).await;
        let temp = TempDir::new().unwrap();
        let vault = vault();
        let gateway = client(temp.path(), "gateway", &vault, DeviceId::new());
        let desktop = client(temp.path(), "desktop", &vault, DeviceId::new());
        let png = fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/media/valid-1x1.png"),
        )
        .unwrap();
        let source = temp.path().join("incoming.png");
        fs::write(&source, &png).unwrap();
        let attachment = gateway
            .prepare_attachment(&source, "image/png", "incoming.png")
            .unwrap();
        gateway
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "SIMULATED image".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("fake-mms-1".into()),
                imported: false,
                attachment_ids: vec![attachment.attachment_id],
            })
            .unwrap();
        let gateway_dir = temp.path().join("gateway-state");
        drain_media(&gateway, &origin, "token", &gateway_dir)
            .await
            .unwrap();
        assert_eq!(upload_pending(&gateway, &origin, "token").await.unwrap(), 1);
        deliver(&desktop, &state, 1);
        let pending = desktop.pending_downloads().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].remote_object_id.as_deref(), Some(REMOTE));

        let desktop_dir = temp.path().join("desktop-state");
        let download_dir = desktop_dir.join("SIMULATED-downloads");
        fs::create_dir_all(&download_dir).unwrap();
        let stale_partial = download_dir.join("interrupted.part");
        let legacy_partial = download_dir.join(format!("{}.ppss", attachment.attachment_id.0));
        fs::write(&stale_partial, b"partial").unwrap();
        fs::write(&legacy_partial, b"legacy").unwrap();
        state.lock().unwrap().downloads = vec![Download::ServerError, Download::Truncated];
        assert!(matches!(
            drain_media(&desktop, &origin, "token", &desktop_dir).await,
            Err(SimulatorError::Http)
        ));
        assert!(!stale_partial.exists(), "stale partial was not removed");
        assert!(!legacy_partial.exists(), "legacy partial was not removed");
        assert!(matches!(
            drain_media(&desktop, &origin, "token", &desktop_dir).await,
            Err(SimulatorError::Core)
        ));
        assert!(
            fs::read_dir(&download_dir).unwrap().all(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|extension| extension != "part")),
            "install failure left a partial download"
        );
        assert_eq!(
            desktop
                .attachment_info(attachment.attachment_id)
                .unwrap()
                .state,
            AttachmentState::PendingDownload
        );

        drain_media(&desktop, &origin, "token", &desktop_dir)
            .await
            .unwrap();
        assert!(fs::read_dir(&download_dir).unwrap().all(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|extension| extension != "part")
        }));
        assert!(desktop.pending_downloads().unwrap().is_empty());
        let plaintext = desktop
            .open_native_plaintext(attachment.attachment_id)
            .unwrap();
        assert_eq!(fs::read(plaintext.path()).unwrap(), png);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typed_storage_policy_removal_is_recorded_and_generic_gone_still_fails() {
        let state: Shared = Arc::default();
        let origin = start_fake(state.clone()).await;
        let temp = TempDir::new().unwrap();
        let vault = vault();
        let gateway = client(temp.path(), "gateway", &vault, DeviceId::new());
        let desktop = client(temp.path(), "desktop", &vault, DeviceId::new());
        let source = temp.path().join("incoming.png");
        fs::write(&source, b"ciphertext input").unwrap();
        let attachment = gateway
            .prepare_attachment(&source, "image/png", "incoming.png")
            .unwrap();
        gateway
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "SIMULATED image".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("storage-policy-removal".into()),
                imported: false,
                attachment_ids: vec![attachment.attachment_id],
            })
            .unwrap();
        drain_media(
            &gateway,
            &origin,
            "token",
            &temp.path().join("gateway-state"),
        )
        .await
        .unwrap();
        upload_pending(&gateway, &origin, "token").await.unwrap();
        deliver(&desktop, &state, 1);

        state.lock().unwrap().downloads = vec![Download::GenericGone];
        assert!(matches!(
            download_media_with_transport(
                &desktop,
                &private_http_transport(&origin).unwrap(),
                "token",
                &temp.path().join("desktop-state"),
            )
            .await,
            Err(SimulatorError::Http)
        ));
        assert_eq!(desktop.pending_downloads().unwrap().len(), 1);

        state.lock().unwrap().downloads = vec![Download::StoragePolicyRemoved];
        download_media_with_transport(
            &desktop,
            &private_http_transport(&origin).unwrap(),
            "token",
            &temp.path().join("desktop-state"),
        )
        .await
        .unwrap();
        assert!(desktop.pending_downloads().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn redirect_outbox_response_is_not_acknowledged() {
        let state: Shared = Arc::default();
        state.lock().unwrap().event_status = Some(StatusCode::FOUND);
        let origin = start_fake(state).await;
        let temp = TempDir::new().unwrap();
        let vault = vault();
        let client = client(temp.path(), "client", &vault, DeviceId::new());
        client
            .queue_send(
                OutgoingSms {
                    conversation_id: ConversationId::new(),
                    recipients: vec!["+15555550100".into()],
                    body: "SIMULATED redirect".into(),
                },
                GatewayRoute {
                    gateway_device_id: DeviceId::new(),
                    subscription_id: "simulated-route-1".into(),
                },
            )
            .unwrap();

        assert!(matches!(
            upload_pending(&client, &origin, "token").await,
            Err(SimulatorError::Http)
        ));
        assert_eq!(client.pending_outbox_batch(OUTBOX_BATCH).unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn policy_rejected_outbox_waits_until_after_downloads() {
        let state: Shared = Arc::default();
        let origin = start_fake(state.clone()).await;
        let temp = TempDir::new().unwrap();
        let vault = vault();
        let gateway = client(temp.path(), "gateway", &vault, DeviceId::new());
        let desktop = client(temp.path(), "desktop", &vault, DeviceId::new());
        let png = fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/media/valid-1x1.png"),
        )
        .unwrap();
        let source = temp.path().join("incoming.png");
        fs::write(&source, &png).unwrap();
        let attachment = gateway
            .prepare_attachment(&source, "image/png", "incoming.png")
            .unwrap();
        gateway
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "SIMULATED image".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("fake-mms-1".into()),
                imported: false,
                attachment_ids: vec![attachment.attachment_id],
            })
            .unwrap();
        upload_media_with_transport(&gateway, &private_http_transport(&origin).unwrap(), "token")
            .await
            .unwrap();
        assert_eq!(upload_pending(&gateway, &origin, "token").await.unwrap(), 1);
        deliver(&desktop, &state, 1);
        assert_eq!(desktop.pending_downloads().unwrap().len(), 1);

        desktop
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550101".into(),
                body: "local pending outbox".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("fake-mms-2".into()),
                imported: false,
                attachment_ids: Vec::new(),
            })
            .unwrap();
        assert!(!desktop.pending_outbox_batch(100).unwrap().is_empty());
        state.lock().unwrap().reject_events = true;

        download_media_with_transport(
            &desktop,
            &private_http_transport(&origin).unwrap(),
            "token",
            &temp.path().join("desktop-state"),
        )
        .await
        .unwrap();
        assert!(desktop.pending_downloads().unwrap().is_empty());
        assert_eq!(
            fs::read(
                desktop
                    .open_native_plaintext(attachment.attachment_id)
                    .unwrap()
                    .path()
            )
            .unwrap(),
            png
        );
        assert!(matches!(
            upload_pending(&desktop, &origin, "token").await,
            Err(SimulatorError::Rejected(RejectedEndpoint::Outbox))
        ));
        assert!(!desktop.pending_outbox_batch(100).unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn policy_rejected_attachment_upload_is_classified_separately() {
        let state: Shared = Arc::default();
        state.lock().unwrap().reject_media = true;
        let origin = start_fake(state).await;
        let temp = TempDir::new().unwrap();
        let vault = vault();
        let client = client(temp.path(), "gateway", &vault, DeviceId::new());
        let source = temp.path().join("outgoing.png");
        fs::write(&source, b"ciphertext input").unwrap();
        let attachment = client
            .prepare_attachment(&source, "image/png", "outgoing.png")
            .unwrap();
        client
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "attachment pending upload".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("fake-mms-3".into()),
                imported: false,
                attachment_ids: vec![attachment.attachment_id],
            })
            .unwrap();

        assert!(matches!(
            upload_media_with_transport(
                &client,
                &private_http_transport(&origin).unwrap(),
                "token",
            )
            .await,
            Err(SimulatorError::Rejected(RejectedEndpoint::Media))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_download_directory_is_rejected_without_touching_its_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let external_part = external.path().join("x.part");
        fs::write(&external_part, b"preserve").unwrap();
        fs::set_permissions(external.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let original_permissions = fs::metadata(external.path()).unwrap().permissions().mode();
        let state_dir = temp.path().join("state");
        fs::create_dir(&state_dir).unwrap();
        symlink(external.path(), state_dir.join("SIMULATED-downloads")).unwrap();
        let vault = vault();
        let client = client(temp.path(), "client", &vault, DeviceId::new());

        assert!(matches!(
            download_media_with_transport(
                &client,
                &private_http_transport("http://127.0.0.1:1").unwrap(),
                "token",
                &state_dir,
            )
            .await,
            Err(SimulatorError::Io)
        ));
        assert_eq!(fs::read(&external_part).unwrap(), b"preserve");
        assert_eq!(
            fs::metadata(external.path()).unwrap().permissions().mode(),
            original_permissions
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn route_fenced_effect_leaves_a_foreign_sim_command_pending() {
        let state: Shared = Arc::default();
        let origin = start_fake(state.clone()).await;
        let temp = TempDir::new().unwrap();
        let vault = vault();
        let gateway_id = DeviceId::new();
        let gateway = client(temp.path(), "gateway", &vault, gateway_id);
        let desktop = client(temp.path(), "desktop", &vault, DeviceId::new());
        let queued = desktop
            .queue_send(
                OutgoingSms {
                    conversation_id: ConversationId::new(),
                    recipients: vec!["+15555550100".into()],
                    body: "SIMULATED foreign route".into(),
                },
                GatewayRoute {
                    gateway_device_id: gateway_id,
                    subscription_id: "sim-foreign".into(),
                },
            )
            .unwrap();
        assert_eq!(upload_pending(&desktop, &origin, "token").await.unwrap(), 1);
        deliver(&gateway, &state, 1);
        let effects = effects_path(temp.path());

        assert!(
            !run_one_carrier_effect_for_route(&gateway, &effects, "sim-active", false).unwrap()
        );
        assert!(
            !effects.exists(),
            "a foreign route produced a carrier effect"
        );
        let pending = gateway.pending_commands().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].command_id, queued.command_id);

        assert!(
            run_one_carrier_effect_for_route(&gateway, &effects, "sim-foreign", false).unwrap()
        );
        assert_eq!(
            fs::read_to_string(&effects).unwrap().trim(),
            queued.command_id.0.to_string()
        );
        assert!(gateway.pending_commands().unwrap().is_empty());
    }
}
