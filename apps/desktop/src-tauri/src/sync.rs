//! Native network supervisor: bounded HTTP catch-up (outbox, media, replay, staged snapshot
//! resync) plus a persistent authenticated WebSocket (`ready` / `event` / `resync_required`)
//! with bounded, jittered reconnect and cooperative cancellation. Every client-core call runs
//! in `spawn_blocking`; the Tokio workers never block on SQLCipher, Argon2 or file crypto.
use crate::{
    error::{core_error, BridgeError, BridgeResult},
    gateways::{extract_device_role, gateway_views, CapabilitiesResponse, DevicesResponse},
    net::{NetError, MAX_JSON_BYTES, MAX_PAGE_BYTES, NON_API_ERROR_CODE},
    origin::websocket_url,
    session::{Session, VaultSummary},
};
use futures_util::{FutureExt, SinkExt, StreamExt};
use peppy_client_core::{
    Client, CompletionReplayNeed, Cursor, DraftPush, DraftPushOutcome, DraftTerminalReasonCore,
    DraftTransferDirection, DraftUpload, EnvelopePurpose, HistorySearchReplayProof,
    HistorySearchSyncFacts, KeyProfile, RawReplayRecord, RawSnapshotRecord, SnapshotManifestEntry,
    SnapshotPurpose, VaultCheckHeader, MAX_APPLY_BATCH, MAX_SEAL_BATCH, MAX_SNAPSHOT_MANIFEST_PAGE,
    MAX_SNAPSHOT_PAGE,
};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::Duration,
};
use tokio::time::{sleep, timeout, Instant};
use tokio_tungstenite::tungstenite::{
    self, client::IntoClientRequest, protocol::WebSocketConfig, Message,
};

const MAX_BACKOFF: Duration = Duration::from_secs(60);
const WORK_INTERVAL: Duration = Duration::from_secs(15);
const DRAFT_SAVE_WAKE_DELAY: Duration = Duration::from_secs(2);
#[cfg(test)]
const DRAFT_SEND_PUSH_LOCK_WAIT: Duration = Duration::from_millis(200);
#[cfg(not(test))]
const DRAFT_SEND_PUSH_LOCK_WAIT: Duration = Duration::from_secs(10);
const GATEWAY_REFRESH_EVERY: u32 = 4;
const WS_MAX_MESSAGE: usize = 2 * 1024 * 1024;
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WS_READY_TIMEOUT: Duration = Duration::from_secs(10);
const WS_PING_AFTER: Duration = Duration::from_secs(45);
const WS_IDLE_LIMIT: Duration = Duration::from_secs(120);
const WS_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_REPLAY_PAGES_PER_ROUND: usize = 20;
const WS_BATCH_FRAMES: usize = 100;
const WS_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_OUTBOX_BATCHES: usize = 8;
const OUTBOX_BATCH: usize = 32;
const MEDIA_PER_ROUND: usize = 4;
const DEMANDED_DOWNLOAD_SLOTS: usize = 3;
pub(crate) const MAX_DOWNLOAD_UNAVAILABLE_ATTEMPTS: u8 = 6;
const DOWNLOAD_UNAVAILABLE_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DOWNLOAD_UNAVAILABLE_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Apply steps per normal pass; snapshot drains continue until `snapshot_remaining == 0`.
const APPLY_STEPS: usize = 8;
const DECODER_RECOVERY_PAGE: u32 = 256;
const WS_CLOSE_REVOKED: u16 = 4401;
const WS_CLOSE_RESYNC: u16 = 4409;

struct DraftSaveWake {
    generation: u64,
    cancel: tokio_util::sync::CancellationToken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplayOutcome {
    CaughtUp,
    MorePages,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveBatchDecision {
    Append,
    Flush,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeferredFrameControl {
    Cancelled,
    Resync,
    Liveness,
    RefreshPresence,
    ProcessFrame,
}

enum LiveBatchItem<'a> {
    Event {
        cursor: &'a str,
        envelope_bytes: usize,
    },
    Other,
}

fn live_batch_decision(
    batch_len: usize,
    batch_bytes: usize,
    previous_cursor: Option<&str>,
    next: LiveBatchItem<'_>,
) -> LiveBatchDecision {
    let LiveBatchItem::Event {
        cursor,
        envelope_bytes,
    } = next
    else {
        return LiveBatchDecision::Flush;
    };
    let cursor_regressed = previous_cursor
        .and_then(|previous| previous.parse::<u64>().ok().zip(cursor.parse::<u64>().ok()))
        .is_some_and(|(previous, cursor)| cursor <= previous);
    if batch_len >= WS_BATCH_FRAMES
        || batch_bytes >= WS_BATCH_BYTES
        || batch_bytes + envelope_bytes >= WS_BATCH_BYTES
        || cursor_regressed
    {
        LiveBatchDecision::Flush
    } else {
        LiveBatchDecision::Append
    }
}

fn deferred_frame_control(
    cancel: &tokio_util::sync::CancellationToken,
    repair_wake: &tokio::sync::Notify,
    replay_wake: &tokio::sync::Notify,
    liveness: &mut tokio::time::Interval,
    presence_refresh_due: Option<Instant>,
) -> DeferredFrameControl {
    if cancel.is_cancelled() {
        DeferredFrameControl::Cancelled
    } else if repair_wake.notified().now_or_never().is_some()
        || replay_wake.notified().now_or_never().is_some()
    {
        DeferredFrameControl::Resync
    } else if liveness.tick().now_or_never().is_some() {
        DeferredFrameControl::Liveness
    } else if presence_refresh_due.is_some_and(|due| due <= Instant::now()) {
        DeferredFrameControl::RefreshPresence
    } else {
        DeferredFrameControl::ProcessFrame
    }
}

static DRAFT_SAVE_WAKE_GENERATION: AtomicU64 = AtomicU64::new(0);
static DRAFT_SAVE_WAKES: LazyLock<Mutex<HashMap<usize, DraftSaveWake>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct DownloadRetryState {
    attempts: u8,
    retry_at: Instant,
}

pub(crate) fn is_storage_policy_removal(error: &NetError) -> bool {
    error.is_status(410, "attachment_removed_by_storage_policy")
}

fn is_recoverable_download_unavailable(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Net(error) if error.is_status(409, "attachment_unavailable")
    )
}

fn schedule_download_retry(session: &Arc<Session>, id: peppy_client_core::AttachmentId) -> bool {
    let mut retries = session
        .download_retries
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    advance_download_retry(&mut retries, id)
}

fn advance_download_retry(
    retries: &mut HashMap<peppy_client_core::AttachmentId, DownloadRetryState>,
    id: peppy_client_core::AttachmentId,
) -> bool {
    let attempts = {
        let retry = retries.entry(id).or_insert(DownloadRetryState {
            attempts: 0,
            retry_at: Instant::now(),
        });
        retry.attempts = retry.attempts.saturating_add(1);
        retry.attempts
    };
    if attempts >= MAX_DOWNLOAD_UNAVAILABLE_ATTEMPTS {
        retries.remove(&id);
        return false;
    }
    let delay = DOWNLOAD_UNAVAILABLE_INITIAL_BACKOFF
        .saturating_mul(1u32 << (attempts - 1))
        .min(DOWNLOAD_UNAVAILABLE_MAX_BACKOFF);
    retries.get_mut(&id).expect("entry was inserted").retry_at = Instant::now() + delay;
    true
}

fn download_retry_ready(session: &Arc<Session>, id: &peppy_client_core::AttachmentId) -> bool {
    session
        .download_retries
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(id)
        .is_none_or(|retry| retry.retry_at <= Instant::now())
}

pub(crate) fn clear_download_retry(session: &Session, id: &peppy_client_core::AttachmentId) {
    session
        .download_retries
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(id);
}

/// Allocates the media batch after the host filters permanent and delayed failures.
fn select_downloads<T>(candidates: Vec<(T, bool)>) -> Vec<T> {
    let (mut demanded, mut eager): (Vec<_>, Vec<_>) =
        candidates.into_iter().partition(|(_, demanded)| *demanded);
    let mut selected = Vec::with_capacity(MEDIA_PER_ROUND);
    let demanded_slots = DEMANDED_DOWNLOAD_SLOTS.min(demanded.len());
    selected.extend(demanded.drain(..demanded_slots).map(|(object, _)| object));
    let eager_slots = (MEDIA_PER_ROUND - DEMANDED_DOWNLOAD_SLOTS).min(eager.len());
    selected.extend(eager.drain(..eager_slots).map(|(object, _)| object));
    selected.extend(
        demanded
            .into_iter()
            .chain(eager)
            .take(MEDIA_PER_ROUND - selected.len())
            .map(|(object, _)| object),
    );
    selected
}

/// Sync records gateway HTTP outages as reachability failures without changing other API clients.
fn sync_net_error(error: NetError) -> BridgeError {
    match error {
        NetError::Status {
            status: 502..=504, ..
        } => BridgeError::new(
            "server-unavailable",
            "The configured server is temporarily unavailable.",
        ),
        error => error.into(),
    }
}

/// Runs client-core work off the async workers.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> BridgeResult<T> + Send + 'static,
) -> BridgeResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| BridgeError::host_state())?
}

fn client(session: &Arc<Session>) -> Client {
    session.client.clone()
}

/// First wait before the same unresolved replay-head need is requested again. Each repeat doubles it.
const REPLAY_HEAD_RETRY: Duration = Duration::from_secs(1);
/// Longest wait between requests for one unresolved need.
const REPLAY_HEAD_RETRY_CAP: Duration = Duration::from_secs(60);

/// Session-wide memory of the replay-head requests owed deletion members caused. A need that is new
/// (a newer reported head, or a different gate) is requested at once. The same need is requested
/// again only after an exponential wait, so a head that never arrives costs a bounded number of
/// replays however many socket batches arrive. Satisfied or absent need resets the memory. Pure:
/// callers pass `now`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplayHeadAsks {
    asked: Option<CompletionReplayNeed>,
    attempts: u32,
    not_before: Option<Instant>,
}

impl ReplayHeadAsks {
    /// Whether a replay should be requested for `need` at `now`.
    pub(crate) fn admit(&mut self, need: Option<CompletionReplayNeed>, now: Instant) -> bool {
        let Some(need) = need else {
            *self = Self::default();
            return false;
        };
        if self.asked == Some(need) {
            if self.not_before.is_some_and(|at| now < at) {
                return false;
            }
            self.attempts = self.attempts.saturating_add(1);
        } else {
            self.attempts = 1;
        }
        self.asked = Some(need);
        self.not_before = Some(now + replay_head_retry(self.attempts));
        true
    }
}

/// Wait after the `attempts`-th request for one need: `REPLAY_HEAD_RETRY` doubled per repeat, capped.
fn replay_head_retry(attempts: u32) -> Duration {
    let doublings = attempts.saturating_sub(1).min(16);
    REPLAY_HEAD_RETRY
        .saturating_mul(1 << doublings)
        .min(REPLAY_HEAD_RETRY_CAP)
}

/// One deferred apply pass, then the replay request its records may now need. Records journaled while
/// locked, or with a compaction key not yet installed, become ready here with no socket frame, so the
/// worker runs this after an unlock and on its own passes.
async fn apply_pass_and_wake_owed_deletions(session: &Arc<Session>) -> BridgeResult<()> {
    drain_apply(session, APPLY_STEPS).await?;
    wake_replay_for_owed_deletions(session).await
}

/// Asks the live loop for one replay when owed deletion members need a newer replay head that this
/// request has not already asked for (see `ReplayHeadAsks`). Socket records carry no head, so the
/// live loop asks after each socket batch through this function as well.
async fn wake_replay_for_owed_deletions(session: &Arc<Session>) -> BridgeResult<()> {
    let c = client(session);
    let need = blocking(move || c.completion_replay_need().map_err(core_error)).await?;
    let admitted = session
        .replay_head_asks
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .admit(need, Instant::now());
    if admitted {
        session.replay_wake.notify_one();
    }
    Ok(())
}

/// Resets the save debounce for this session. Cancellation fences delayed work from retired
/// sessions, while the generation ensures a superseded save cannot wake early.
pub(crate) fn schedule_draft_push(session: Arc<Session>, delay: Duration) {
    let key = Arc::as_ptr(&session) as usize;
    let generation = DRAFT_SAVE_WAKE_GENERATION.fetch_add(1, Ordering::Relaxed);
    let cancel = tokio_util::sync::CancellationToken::new();
    let previous = DRAFT_SAVE_WAKES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(
            key,
            DraftSaveWake {
                generation,
                cancel: cancel.clone(),
            },
        );
    if let Some(previous) = previous {
        previous.cancel.cancel();
    }
    tokio::spawn(async move {
        tokio::select! {
            _ = session.cancel.cancelled() => {}
            _ = cancel.cancelled() => {}
            _ = sleep(delay) => {
                let current = {
                    let mut wakes = DRAFT_SAVE_WAKES
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    if wakes.get(&key).is_some_and(|wake| wake.generation == generation) {
                        wakes.remove(&key);
                        true
                    } else {
                        false
                    }
                };
                if current && !session.cancel.is_cancelled() {
                    session.request_work();
                }
                return;
            }
        }
        let mut wakes = DRAFT_SAVE_WAKES
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if wakes
            .get(&key)
            .is_some_and(|wake| wake.generation == generation)
        {
            wakes.remove(&key);
        }
    });
}

pub(crate) async fn schedule_draft_push_after_save(session: Arc<Session>, draft_id: String) {
    let client = client(&session);
    let pending = blocking(move || {
        client
            .pending_draft_pushes()
            .map(|pushes| {
                pushes
                    .iter()
                    .any(|push| push.draft_id.to_string() == draft_id)
            })
            .map_err(core_error)
    })
    .await
    .unwrap_or(false);
    if pending && !session.cancel.is_cancelled() {
        schedule_draft_push(session, DRAFT_SAVE_WAKE_DELAY);
    }
}

pub(crate) async fn wake_draft_sync_if_pending(session: Arc<Session>) {
    let client = client(&session);
    let pending = blocking(move || {
        client
            .pending_draft_pushes()
            .map(|pushes| !pushes.is_empty())
            .map_err(core_error)
    })
    .await
    .unwrap_or(false);
    if pending && !session.cancel.is_cancelled() {
        session.request_work();
    }
}

#[derive(Deserialize)]
pub struct VaultResponse {
    pub vault_id: String,
    pub device_id: String,
    pub public_key_profile: serde_json::Value,
    pub encrypted_vault_check_header: String,
    pub profile_fingerprint: String,
}

/// Authenticated `/v1/vault`, verifying that the server's principal is exactly this binding.
pub async fn fetch_vault(
    api: &crate::net::Api,
    vault_id: &str,
    device_id: &str,
) -> BridgeResult<VaultResponse> {
    let vault: VaultResponse = api
        .get_json("/v1/vault", MAX_JSON_BYTES)
        .await
        .map_err(sync_net_error)?;
    let same = |a: &str, b: &str| {
        uuid::Uuid::parse_str(a)
            .ok()
            .zip(uuid::Uuid::parse_str(b).ok())
            .is_some_and(|(a, b)| a == b)
    };
    if !same(&vault.vault_id, vault_id) || !same(&vault.device_id, device_id) {
        return Err(BridgeError::new(
            "credential-mismatch",
            "The credential does not match the authenticated server device.",
        ));
    }
    Ok(vault)
}

pub fn vault_header(vault: &VaultResponse) -> BridgeResult<(KeyProfile, VaultCheckHeader)> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let invalid = |m: &'static str| BridgeError::new("server-response", m);
    let profile: KeyProfile = serde_json::from_value(vault.public_key_profile.clone())
        .map_err(|_| invalid("The server key profile is invalid."))?;
    let bytes = STANDARD
        .decode(vault.encrypted_vault_check_header.as_bytes())
        .map_err(|_| invalid("The server vault header is invalid."))?;
    let header: VaultCheckHeader = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The server vault header has an unsupported encoding."))?;
    Ok((profile, header))
}

async fn refresh_vault(session: &Arc<Session>) -> BridgeResult<()> {
    let vault = fetch_vault(
        &session.api,
        &session.binding.vault_id,
        &session.binding.device_id,
    )
    .await?;
    let (profile, _) = vault_header(&vault)?;
    session.set_security_status(|status| {
        status.vault = Some(VaultSummary {
            epoch: profile.key_epoch,
            fingerprint: vault.profile_fingerprint.clone(),
        })
    });
    Ok(())
}

#[derive(Default)]
pub(crate) struct GatewayRefreshState {
    last_applied: u64,
    capabilities: Option<CapabilitiesResponse>,
}

pub async fn refresh_gateways(session: &Arc<Session>) -> BridgeResult<()> {
    let sequence = session
        .gateway_refresh_sequence
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    let devices: DevicesResponse = session
        .api
        .get_json("/v1/devices", MAX_JSON_BYTES)
        .await
        .map_err(sync_net_error)?;
    let capabilities: BridgeResult<CapabilitiesResponse> = session
        .api
        .get_json("/v1/capabilities", MAX_JSON_BYTES)
        .await
        .map_err(sync_net_error);
    let (accepted, result) = match capabilities {
        Ok(response) => (Some(response), Ok(())),
        Err(error) => (None, Err(error)),
    };
    let mut state = session
        .gateway_refresh_state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if sequence > state.last_applied {
        state.last_applied = sequence;
        if let Some(capabilities) = accepted {
            // The accepted snapshot must advance even if the actor coalesces this refresh.
            state.capabilities = Some(capabilities);
        }
        let views = if let Some(capabilities) = &state.capabilities {
            gateway_views(&devices, capabilities)
        } else {
            let qualifying_ids: std::collections::HashSet<_> = gateway_views(
                &devices,
                &CapabilitiesResponse {
                    capabilities: Vec::new(),
                },
            )
            .into_iter()
            .map(|view| view.id)
            .collect();
            let mut views = session
                .status
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .gateways
                .clone();
            views.retain(|view| qualifying_ids.contains(&view.id));
            views
        };
        submit_gateway_views(session, &devices, views);
    }
    result
}

#[cfg(test)]
pub(crate) fn apply_gateway_refresh(
    session: &Session,
    devices: &DevicesResponse,
    capabilities: &CapabilitiesResponse,
) {
    let views = gateway_views(devices, capabilities);
    submit_gateway_views(session, devices, views);
}

fn submit_gateway_views(
    session: &Session,
    devices: &DevicesResponse,
    views: Vec<peppy_desktop_api::GatewayView>,
) {
    let device_role = extract_device_role(devices, &session.binding.device_id);
    let rcs_roster_validated = device_role.is_some();
    let rcs_live_roster = if rcs_roster_validated {
        devices
            .devices
            .iter()
            .filter(|device| !device.revoked)
            .map(|device| device.device_id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let revoked_device_ids = devices
        .devices
        .iter()
        .filter(|device| device.revoked)
        .map(|device| device.device_id.clone())
        .collect();
    session.live_typing.submit_gateway_refresh(
        session,
        crate::live_typing::GatewayRefresh {
            gateways: views,
            device_role,
            rcs_roster_validated,
            rcs_live_roster,
            revoked_device_ids,
        },
    );
}

/// Drains published snapshot records and applies journaled records in bounded steps, yielding
/// between steps. `applied == 0` alone never means complete: the loop continues while history is
/// still being drained (`snapshot_remaining > 0`) unless `max_steps` is reached or cancelled.
const CONVERGE_STEP_BUDGET: usize = 8;

pub async fn drain_apply(session: &Arc<Session>, max_steps: usize) -> BridgeResult<bool> {
    let c = client(session);
    blocking(move || {
        if c.decoder_recovery_pending().map_err(core_error)?.is_some() {
            return Err(core_error(peppy_client_core::Error::RecoveryPending));
        }
        Ok(())
    })
    .await?;
    let mut progressed = false;
    for _ in 0..max_steps {
        if session.cancel.is_cancelled() {
            break;
        }
        let c = client(session);
        let merge_pending = blocking(move || {
            let pending = match c.converge_conversations(0) {
                Ok(pending) => pending,
                Err(error) => {
                    eprintln!(
                        "peppy: conversation convergence probe failed: {}",
                        core_error(error).code
                    );
                    false
                }
            };
            Ok(pending)
        })
        .await?;
        progressed |= merge_pending;
        let c = client(session);
        let report = apply_batch(session, async move {
            blocking(move || c.apply_pending(MAX_APPLY_BATCH).map_err(core_error)).await
        })
        .await?;
        let step = report.applied + report.drained + report.quarantined + report.superseded > 0;
        progressed |= step;
        // No progress at all ends the pass (history waiting for keys stays journaled); while
        // records are still being drained or applied the loop continues.
        if !step {
            break;
        }
        tokio::task::yield_now().await;
    }
    for _ in 0..max_steps {
        if session.cancel.is_cancelled() {
            break;
        }
        let c = client(session);
        let result = blocking(move || {
            let pending = c.converge_conversations(0).map_err(core_error)?;
            let remaining = c
                .converge_conversations(CONVERGE_STEP_BUDGET)
                .map_err(core_error)?;
            Ok((pending, remaining))
        })
        .await;
        match result {
            Ok((pending, remaining)) => {
                progressed |= pending || remaining;
                if !remaining {
                    break;
                }
            }
            Err(error) => {
                eprintln!("peppy: conversation convergence failed: {}", error.code);
                break;
            }
        }
        tokio::task::yield_now().await;
    }
    if progressed {
        session.notify();
    }
    Ok(progressed)
}

async fn apply_batch(
    session: &Session,
    operation: impl std::future::Future<Output = BridgeResult<peppy_client_core::ApplyReport>>,
) -> BridgeResult<peppy_client_core::ApplyReport> {
    let report = {
        let _guard = session.apply_batch_lock.lock().await;
        let report = operation.await?;
        session.set_apply_remaining(report.snapshot_remaining);
        report
    };
    session.notify_progress(false);
    Ok(report)
}

#[cfg(test)]
mod apply_batch_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn overlapping_batches_publish_in_core_order() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_session = session.clone();
        let first = tokio::spawn(async move {
            apply_batch(&first_session, async {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Ok(peppy_client_core::ApplyReport {
                    snapshot_remaining: 10,
                    ..Default::default()
                })
            })
            .await
            .unwrap()
        });
        started_rx.await.unwrap();
        let (second_tx, mut second_rx) = tokio::sync::oneshot::channel();
        let second_session = session.clone();
        let second = tokio::spawn(async move {
            apply_batch(&second_session, async {
                let _ = second_tx.send(());
                Ok(peppy_client_core::ApplyReport::default())
            })
            .await
            .unwrap()
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second_rx)
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        first.await.unwrap();
        second_rx.await.unwrap();
        second.await.unwrap();
        assert!(!session.snapshot_applying());
    }
}

#[derive(Deserialize)]
struct RawRecord {
    cursor: String,
    envelope: Box<RawValue>,
}
#[derive(Deserialize)]
struct EventsPage {
    events: Vec<RawRecord>,
    #[serde(default)]
    high_water_cursor: Option<String>,
    /// Presence is material: old servers omit this, while a current terminal page says null.
    #[serde(default, deserialize_with = "deserialize_next_after")]
    next_after: OptionalField<Option<String>>,
}

#[derive(Default)]
enum OptionalField<T> {
    #[default]
    Missing,
    Present(T),
}

fn deserialize_next_after<'de, D>(
    deserializer: D,
) -> Result<OptionalField<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(OptionalField::Present)
}
#[derive(Deserialize)]
struct SnapshotStart {
    high_water_cursor: String,
    record_count: String,
    #[serde(default)]
    max_page_size: Option<u64>,
    #[serde(default)]
    compaction_supported: bool,
    #[serde(default)]
    compaction_generation: Option<String>,
    #[serde(default)]
    compaction_active: bool,
    #[serde(default)]
    manifest_supported: bool,
}
#[derive(Deserialize)]
struct SnapshotPage {
    records: Vec<RawRecord>,
}
#[derive(Deserialize)]
struct SnapshotManifestPage {
    entries: Vec<SnapshotManifestRecord>,
}
#[derive(Deserialize)]
struct SnapshotManifestRecord {
    cursor: String,
    digest: String,
    flags: u8,
}

fn parse_cursor(value: &str) -> BridgeResult<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|c| *c >= 1 && *c <= i64::MAX as u64)
        .ok_or_else(|| NetError::Invalid.into())
}

fn parse_high_water(value: &str) -> BridgeResult<Cursor> {
    value
        .parse::<u64>()
        .ok()
        .filter(|cursor| *cursor <= i64::MAX as u64 && cursor.to_string() == value)
        .map(Cursor)
        .ok_or_else(|| NetError::Invalid.into())
}

fn parse_compaction_generation(start: &SnapshotStart) -> BridgeResult<Option<u64>> {
    if !start.compaction_supported {
        return Ok(None);
    }
    let value = start
        .compaction_generation
        .as_deref()
        .ok_or(NetError::Invalid)?;
    let generation = value.parse::<u64>().map_err(|_| NetError::Invalid)?;
    if generation.to_string() != value {
        return Err(NetError::Invalid.into());
    }
    Ok(Some(generation))
}

/// Journals raw envelope bytes; malformed records reach core quarantine instead of failing a
/// typed parse here.
async fn ingest_page(session: &Arc<Session>, records: Vec<RawRecord>) -> BridgeResult<()> {
    let parsed = records
        .into_iter()
        .map(|r| {
            Ok(RawReplayRecord {
                cursor: Cursor(parse_cursor(&r.cursor)?),
                envelope_json: r.envelope.get().as_bytes().to_vec(),
            })
        })
        .collect::<BridgeResult<Vec<_>>>()?;
    let c = client(session);
    blocking(move || c.ingest_raw_batch(&parsed).map(|_| ()).map_err(core_error)).await
}

/// Staged snapshot import after `resync_required`; resumes a persisted generation when possible.
/// Records are passed raw so invalid ones are quarantined by core. Snapshot history is merge-only
/// and its commands are historical (never executable); the restore guard is never touched.
pub(crate) async fn snapshot_resync(session: &Arc<Session>) -> BridgeResult<()> {
    let result = snapshot_resync_inner(session).await;
    session.set_staging_progress(None, true);
    result
}

async fn snapshot_resync_inner(session: &Arc<Session>) -> BridgeResult<()> {
    // Refused before any network or core work while the window is open, whichever path asked.
    if !snapshot_gate(session)
        .wait_before_attempt(Instant::now())
        .is_zero()
    {
        return Err(BridgeError::new(
            SNAPSHOT_TOO_LARGE_CODE,
            "This history is too large for this version.",
        ));
    }
    // A previously published generation must finish draining before a new one may begin.
    drain_apply(session, usize::MAX).await?;
    let mut fresh = false;
    let mut manifest_allowed = true;
    // Only `Impossible` outcomes count as attempts. `ManifestUnsupported` switches to the full
    // import once: `manifest_allowed` stays false for the rest of this call, so it cannot spin.
    let mut impossible = 0;
    while impossible < MAX_SNAPSHOT_ATTEMPTS {
        let progress = start_or_resume(session, fresh, manifest_allowed).await?;
        match import_cut(session, progress).await {
            Ok(()) => return Ok(()),
            Err(CutError::Cancelled) => return Ok(()),
            Err(CutError::Impossible) => {
                session.set_staging_progress(None, true);
                // The staged cut can never complete (server moved/rolled back, or ended early):
                // never resume it again; `begin_snapshot` obsoletes it on the next attempt.
                *session
                    .abandoned_snapshot
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(progress.generation);
                fresh = true;
                impossible += 1;
            }
            Err(CutError::ManifestUnsupported) => {
                session.set_staging_progress(None, true);
                // The server has no manifest route: finish this import with the full download.
                *session
                    .abandoned_snapshot
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(progress.generation);
                fresh = true;
                manifest_allowed = false;
            }
            Err(CutError::Other(error)) => return Err(error),
        }
    }
    Err(BridgeError::new(
        "resync-required",
        "The server snapshot kept changing; history import will be retried. Live state is unchanged.",
    ))
}

const MAX_SNAPSHOT_ATTEMPTS: usize = 3;
const SNAPSHOT_TOO_LARGE_RETRY: Duration = Duration::from_secs(6 * 60 * 60);

/// Session-wide memory of a refused snapshot begin. It is shared by every snapshot path (live loop,
/// wake-ups, contact repair), so none of them can begin an import before the window ends. Pure:
/// callers pass `now`, so the decision is testable without sleeping.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotTooLargeGate {
    blocked_at: Option<Instant>,
}

impl SnapshotTooLargeGate {
    /// Records a refused begin at `now`; the next begin waits `SNAPSHOT_TOO_LARGE_RETRY`.
    pub(crate) fn record_too_large(&mut self, now: Instant) {
        self.blocked_at = Some(now);
    }

    /// Remaining wait before a begin may run again at `now`; zero when no begin is blocked.
    pub(crate) fn wait_before_attempt(&self, now: Instant) -> Duration {
        self.blocked_at.map_or(Duration::ZERO, |at| {
            SNAPSHOT_TOO_LARGE_RETRY.saturating_sub(now.saturating_duration_since(at))
        })
    }

    /// True while a refusal is outstanding, whether or not its window has ended. Only a successful
    /// begin clears it, so the history stays reported as too large until an import really starts.
    pub(crate) fn is_marked(&self) -> bool {
        self.blocked_at.is_some()
    }

    pub(crate) fn clear(&mut self) {
        self.blocked_at = None;
    }
}

/// Snapshot refusal code shared with the desktop bridge.
const SNAPSHOT_TOO_LARGE_CODE: &str = "snapshot-too-large";

/// The snapshot append a test forces to be refused as too large (the core's byte cap, which a real
/// test cannot reach within a practical size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppendKind {
    Records,
    Manifest,
}

/// Test seam for append refusals. Production builds never refuse here.
#[cfg(test)]
fn forced_append_refusal(session: &Session, kind: AppendKind) -> Option<BridgeError> {
    let forced = *session
        .refuse_snapshot_append
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    (forced == Some(kind)).then(|| BridgeError::new(SNAPSHOT_TOO_LARGE_CODE, "forced refusal"))
}

#[cfg(not(test))]
fn forced_append_refusal(_session: &Session, _kind: AppendKind) -> Option<BridgeError> {
    None
}

/// Records a too-large refusal of any snapshot write (begin or append) in the session gate, then
/// returns the result unchanged. Every snapshot write passes through here, so the window exists
/// before any caller decides how to react (a deferred repair, a failed live round).
fn record_snapshot_refusal<T>(session: &Session, result: BridgeResult<T>) -> BridgeResult<T> {
    if matches!(&result, Err(error) if error.code == SNAPSHOT_TOO_LARGE_CODE) {
        snapshot_gate(session).record_too_large(Instant::now());
    }
    result
}

/// Lock the session-wide snapshot gate. A poisoned lock still holds valid gate state.
fn snapshot_gate(session: &Session) -> std::sync::MutexGuard<'_, SnapshotTooLargeGate> {
    session
        .snapshot_too_large
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// A contact repair that was refused as too large must be allowed to run again once the window ends.
fn settle_repair_attempt(session: &Session, result: &BridgeResult<()>) {
    if matches!(result, Err(error) if error.code == SNAPSHOT_TOO_LARGE_CODE || error.code == "update-in-progress")
    {
        session
            .repair_attempted
            .store(false, std::sync::atomic::Ordering::SeqCst);
    } else {
        session
            .repair_finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

enum CutError {
    Impossible,
    /// The manifest route is absent (404/405): the next attempt downloads the full history.
    ManifestUnsupported,
    /// The session was cancelled mid-cut; the cut stays staged and nothing is published.
    Cancelled,
    Other(BridgeError),
}

impl From<BridgeError> for CutError {
    fn from(error: BridgeError) -> Self {
        // Core reports an inconsistent/stale generation as `SnapshotMismatch`.
        if error.code == "resync-required" {
            CutError::Impossible
        } else {
            CutError::Other(error)
        }
    }
}

/// Begins or resumes the Resync cut and returns its manifest page limit. A manifest cut is begun
/// only when the server advertises the manifest and an earlier attempt has not met a 404/405.
async fn start_or_resume(
    session: &Arc<Session>,
    fresh: bool,
    manifest_allowed: bool,
) -> BridgeResult<peppy_client_core::SnapshotProgress> {
    if !fresh {
        let c = client(session);
        let abandoned = *session
            .abandoned_snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let resume = blocking(move || c.snapshot_progress().map_err(core_error))
            .await?
            .filter(|p| p.received_records < p.expected_records && Some(p.generation) != abandoned);
        if let Some(progress) = resume {
            session.set_staging_progress(
                Some((progress.received_records, progress.expected_records)),
                false,
            );
            return Ok(progress);
        }
    }
    let start: SnapshotStart = session
        .api
        .get_json("/v1/snapshot", MAX_JSON_BYTES)
        .await
        .map_err(sync_net_error)?;
    let invalid = || BridgeError::from(NetError::Invalid);
    let high_water = start
        .high_water_cursor
        .parse::<u64>()
        .map_err(|_| invalid())?;
    let count = start.record_count.parse::<u64>().map_err(|_| invalid())?;
    let compaction_generation = parse_compaction_generation(&start).ok().flatten();
    let _ = start.max_page_size;
    let manifest = manifest_allowed && start.manifest_supported;
    let c = client(session);
    let progress = blocking(move || {
        if manifest {
            c.begin_snapshot_with_manifest(Cursor(high_water), count, compaction_generation)
                .map_err(core_error)
        } else {
            c.begin_snapshot_with_compaction(
                Cursor(high_water),
                count,
                SnapshotPurpose::Resync,
                compaction_generation,
            )
            .map_err(core_error)
        }
    })
    .await;
    let progress = record_snapshot_refusal(session, progress)?;
    session.set_staging_progress(
        Some((progress.received_records, progress.expected_records)),
        false,
    );
    snapshot_gate(session).clear();
    session.set_status(|status| {
        if status.error_code == Some(SNAPSHOT_TOO_LARGE_CODE) {
            status.error_code = None;
        }
    });
    Ok(progress)
}

/// Metadata-only probe; a failed or legacy response deliberately disables compactable output.
async fn probe_snapshot_capability(session: &Arc<Session>) -> BridgeResult<()> {
    match session
        .api
        .post_discard("/v1/compaction/capability", &())
        .await
    {
        Ok(()) => {}
        Err(NetError::Revoked) => return Err(NetError::Revoked.into()),
        // A 404 is an older server. Any other unavailable declaration also leaves compaction
        // disabled, while normal legacy sync continues.
        Err(_) => {
            let c = client(session);
            return blocking(move || {
                c.set_server_compaction_state(false, false)
                    .map(|_| ())
                    .map_err(core_error)
            })
            .await;
        }
    }
    let start: SnapshotStart = match session.api.get_json("/v1/snapshot", MAX_JSON_BYTES).await {
        Ok(start) => start,
        Err(NetError::Revoked) => return Err(NetError::Revoked.into()),
        Err(_) => {
            let c = client(session);
            return blocking(move || {
                c.set_server_compaction_state(false, false)
                    .map(|_| ())
                    .map_err(core_error)
            })
            .await;
        }
    };
    // Capability metadata is optional. A malformed optional generation must not block the
    // ordinary replay path; treat it like an unavailable capability instead.
    let supported = parse_compaction_generation(&start).ok().flatten().is_some();
    let active = supported && start.compaction_active;
    let c = client(session);
    // Store both trusted snapshot fields and do one bounded backfill on every connect.
    blocking(move || {
        c.set_server_compaction_state(supported, active)
            .map(|_| ())
            .map_err(core_error)
    })
    .await
}

/// Pages one fixed cut into core staging and publishes it. Returns `Impossible` when this cut
/// cannot be completed (typed resync/invalid cursor from the server, an early end, or a core
/// mismatch), so the caller starts a new cut instead of retrying the same one forever.
async fn import_cut(
    session: &Arc<Session>,
    mut progress: peppy_client_core::SnapshotProgress,
) -> Result<(), CutError> {
    if progress.manifest {
        import_manifest_cut(session, &mut progress).await?;
    } else {
        import_records_cut(session, &mut progress).await?;
    }
    let (c, generation) = (client(session), progress.generation);
    let _retention = session.attachment_retention_maintenance.lock().await;
    session.begin_attachment_retention_restore();
    blocking(move || c.finish_snapshot(generation).map_err(core_error)).await?;
    session.set_staging_progress(None, true);
    drain_apply(session, usize::MAX).await?;
    Ok(())
}

async fn import_records_cut(
    session: &Arc<Session>,
    progress: &mut peppy_client_core::SnapshotProgress,
) -> Result<(), CutError> {
    let mut limit = MAX_SNAPSHOT_PAGE.min(200);
    while progress.received_records < progress.expected_records {
        if session.cancel.is_cancelled() {
            return Err(CutError::Cancelled);
        }
        let path = format!(
            "/v1/snapshot/records?high_water={}&after={}&limit={limit}{}",
            progress.high_water.0,
            progress.last_cursor.0,
            progress
                .server_compaction_generation
                .map(|v| format!("&compaction_generation={v}"))
                .unwrap_or_default(),
        );
        let page: SnapshotPage = match session.api.get_json(&path, MAX_PAGE_BYTES).await {
            Ok(page) => page,
            Err(NetError::TooLarge) if limit > 1 => {
                limit /= 2;
                continue;
            }
            Err(NetError::Status {
                status: 409 | 400, ..
            }) => return Err(CutError::Impossible),
            Err(error) => return Err(CutError::Other(sync_net_error(error))),
        };
        if page.records.is_empty() {
            return Err(CutError::Impossible);
        }
        match append_snapshot_records(session, progress, page.records).await {
            Ok(next) => {
                *progress = next;
                session.set_staging_progress(
                    Some((progress.received_records, progress.expected_records)),
                    false,
                );
            }
            // Page byte cap: retry the same position with a smaller page.
            Err(error) if error.code == "invalid-request" && limit > 1 => limit /= 2,
            Err(error) => return Err(error.into()),
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn import_manifest_cut(
    session: &Arc<Session>,
    progress: &mut peppy_client_core::SnapshotProgress,
) -> Result<(), CutError> {
    let mut record_limit = MAX_SNAPSHOT_PAGE.min(200);
    while progress.received_records < progress.expected_records {
        if session.cancel.is_cancelled() {
            return Err(CutError::Cancelled);
        }
        // The server clamps `limit` to its own maximum, which equals MAX_SNAPSHOT_MANIFEST_PAGE.
        let path = format!(
            "/v1/snapshot/manifest?high_water={}&after={}&limit={MAX_SNAPSHOT_MANIFEST_PAGE}{}",
            progress.high_water.0,
            progress.last_cursor.0,
            progress
                .server_compaction_generation
                .map(|v| format!("&compaction_generation={v}"))
                .unwrap_or_default(),
        );
        let page: SnapshotManifestPage = match session.api.get_json(&path, MAX_PAGE_BYTES).await {
            Ok(page) => page,
            Err(NetError::Status {
                status: 404 | 405, ..
            }) => return Err(CutError::ManifestUnsupported),
            Err(NetError::Status {
                status: 400 | 409, ..
            })
            | Err(NetError::Invalid) => return Err(CutError::Impossible),
            Err(error) => return Err(CutError::Other(sync_net_error(error))),
        };
        if page.entries.is_empty() {
            return Err(CutError::Impossible);
        }
        let entries = page
            .entries
            .into_iter()
            .map(parse_manifest_entry)
            .collect::<BridgeResult<Vec<_>>>()
            .map_err(|_| CutError::Impossible)?;
        let (c, generation) = (client(session), progress.generation);
        let forced = forced_append_refusal(session, AppendKind::Manifest);
        let manifest = match record_snapshot_refusal(
            session,
            match forced {
                Some(error) => Err(error),
                None => {
                    blocking(move || {
                        c.append_snapshot_manifest(generation, &entries)
                            .map_err(core_error)
                    })
                    .await
                }
            },
        ) {
            Ok(manifest) => manifest,
            // Too many entries for one page, or another refused page: restart the cut.
            Err(error) if error.code == "invalid-request" => return Err(CutError::Impossible),
            Err(error) => return Err(error.into()),
        };
        *progress = manifest.progress;
        session.set_staging_progress(
            Some((progress.received_records, progress.expected_records)),
            false,
        );
        let mut remaining = manifest.missing as usize;
        while remaining > 0 {
            if session.cancel.is_cancelled() {
                return Err(CutError::Cancelled);
            }
            let limit = remaining.min(record_limit);
            let path = format!(
                "/v1/snapshot/records?high_water={}&after={}&limit={limit}{}",
                progress.high_water.0,
                progress.last_cursor.0,
                progress
                    .server_compaction_generation
                    .map(|v| format!("&compaction_generation={v}"))
                    .unwrap_or_default(),
            );
            let page: SnapshotPage = match session.api.get_json(&path, MAX_PAGE_BYTES).await {
                Ok(page) => page,
                Err(NetError::TooLarge) if record_limit > 1 => {
                    record_limit /= 2;
                    continue;
                }
                Err(NetError::Status {
                    status: 400 | 409, ..
                }) => return Err(CutError::Impossible),
                Err(error) => return Err(CutError::Other(sync_net_error(error))),
            };
            if page.records.is_empty() {
                return Err(CutError::Impossible);
            }
            let count = page.records.len();
            match append_snapshot_records(session, progress, page.records).await {
                Ok(next) => {
                    // Extra records from a server that ignores `limit` are records of this cut too.
                    *progress = next;
                    session.set_staging_progress(
                        Some((progress.received_records, progress.expected_records)),
                        false,
                    );
                    remaining = remaining.saturating_sub(count);
                }
                Err(error) if error.code == "invalid-request" && record_limit > 1 => {
                    record_limit /= 2
                }
                Err(error) => return Err(error.into()),
            }
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}

/// Decodes one manifest entry. Anything outside the wire contract is rejected: a cursor must be
/// decimal digits, the digest exactly 64 lowercase hex characters and flags 0..=3.
fn parse_manifest_entry(record: SnapshotManifestRecord) -> BridgeResult<SnapshotManifestEntry> {
    let invalid = || BridgeError::from(NetError::Invalid);
    if record.cursor.bytes().any(|byte| !byte.is_ascii_digit()) || record.flags > 0b11 {
        return Err(invalid());
    }
    Ok(SnapshotManifestEntry {
        cursor: Cursor(parse_cursor(&record.cursor)?),
        digest: lower_hex_digest(&record.digest).ok_or_else(invalid)?,
        has_compaction: record.flags & 1 != 0,
        has_origin_tag: record.flags & 2 != 0,
    })
}

/// Exactly 64 lowercase hex characters (a SHA-256 digest); anything else is `None`.
fn lower_hex_digest(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let nibble = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let mut digest = [0; 32];
    for (index, out) in digest.iter_mut().enumerate() {
        *out = (nibble(bytes[2 * index])? << 4) | nibble(bytes[2 * index + 1])?;
    }
    Some(digest)
}

async fn append_snapshot_records(
    session: &Arc<Session>,
    progress: &peppy_client_core::SnapshotProgress,
    records: Vec<RawRecord>,
) -> BridgeResult<peppy_client_core::SnapshotProgress> {
    let records = records
        .into_iter()
        .map(|r| {
            Ok(RawSnapshotRecord {
                cursor: Cursor(parse_cursor(&r.cursor)?),
                envelope_json: r.envelope.get().as_bytes().to_vec(),
            })
        })
        .collect::<BridgeResult<Vec<_>>>()?;
    let (c, generation) = (client(session), progress.generation);
    let forced = forced_append_refusal(session, AppendKind::Records);
    record_snapshot_refusal(
        session,
        match forced {
            Some(error) => Err(error),
            None => {
                blocking(move || {
                    c.append_snapshot_raw_page(generation, &records)
                        .map_err(core_error)
                })
                .await
            }
        },
    )
}

/// HTTP replay after the durable receive cursor (bounded pages, adaptive size).
async fn replay(session: &Arc<Session>) -> BridgeResult<ReplayOutcome> {
    let mut limit = 100usize;
    let mut pages = 0;
    let mut caught_up = false;
    while pages < MAX_REPLAY_PAGES_PER_ROUND && !session.cancel.is_cancelled() {
        let c = client(session);
        let after = blocking(move || c.receive_cursor().map_err(core_error))
            .await?
            .0;
        let contexts = history_sync_contexts(session).await.unwrap_or_default();
        let page: EventsPage = match session
            .api
            .get_json(
                &format!("/v1/events?after={after}&limit={limit}"),
                MAX_PAGE_BYTES,
            )
            .await
        {
            Ok(page) => page,
            Err(error) if error.is_status(409, "resync_required") => {
                snapshot_resync(session).await?;
                pages += 1;
                continue;
            }
            Err(NetError::TooLarge) if limit > 1 => {
                limit /= 2;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let count = page.events.len();
        let replay_target = page
            .high_water_cursor
            .as_deref()
            .map(parse_high_water)
            .transpose()?;
        let proof =
            page.high_water_cursor
                .as_ref()
                .map(|high_water_cursor| HistorySearchReplayProof {
                    context: peppy_client_core::HistorySearchSyncContext {
                        started_at_ms: 0,
                        snapshot_generation: 0,
                    },
                    high_water_cursor: high_water_cursor.clone(),
                    next_after_present: matches!(page.next_after, OptionalField::Present(_)),
                    terminal_next_after: matches!(page.next_after, OptionalField::Present(None)),
                });
        if count == 0 {
            let _ = observe_history_replay(session, contexts, proof).await;
            // An empty page proves this pull reached the server head, so the cut is safe to expose
            // even when nothing new arrived (e.g. after a restart). Without a head there is no proof.
            if let Some(target) = replay_target {
                let c = client(session);
                blocking(move || c.observe_replay_head(target).map_err(core_error)).await?;
                session.set_replay_target_high_water(Some(target));
                session.notify();
            }
            caught_up = true;
            break;
        }
        let _page_in_flight = session.replay_page_in_flight();
        match ingest_page(session, page.events).await {
            Ok(()) => {}
            Err(error) if error.code == "invalid-request" && limit > 1 => {
                limit /= 2;
                continue;
            }
            Err(error) => return Err(error),
        }
        // Both the receive frontier and this target are now safe to expose: receipt committed in
        // one SQLite transaction. Do not notify when the HTTP page is merely fetched.
        if let Some(target) = replay_target {
            let c = client(session);
            blocking(move || c.observe_replay_head(target).map_err(core_error)).await?;
        }
        session.set_replay_target_high_water(replay_target);
        session.notify();
        drain_apply(session, APPLY_STEPS).await?;
        let _ = observe_history_replay(session, contexts, proof).await;
        pages += 1;
        if count < limit {
            caught_up = true;
            break;
        }
    }
    if caught_up && !session.cancel.is_cancelled() {
        session
            .attachment_retention_replay_observed
            .store(true, std::sync::atomic::Ordering::Release);
    }
    Ok(if caught_up || session.cancel.is_cancelled() {
        ReplayOutcome::CaughtUp
    } else {
        ReplayOutcome::MorePages
    })
}

async fn replay_until_caught_up(session: &Arc<Session>) -> BridgeResult<()> {
    let mut replay_outcome = ReplayOutcome::MorePages;
    while replay_outcome == ReplayOutcome::MorePages && !session.cancel.is_cancelled() {
        let before_client = client(session);
        let before = blocking(move || before_client.receive_cursor().map_err(core_error))
            .await?
            .0;
        replay_outcome = replay(session).await?;
        let after_client = client(session);
        let advanced = blocking(move || after_client.receive_cursor().map_err(core_error))
            .await?
            .0
            > before;
        if !advanced {
            break;
        }
    }
    Ok(())
}

async fn history_sync_contexts(
    session: &Arc<Session>,
) -> BridgeResult<Vec<(uuid::Uuid, peppy_client_core::HistorySearchSyncContext)>> {
    let attempts = session.active_history_attempts();
    if attempts.is_empty() {
        return Ok(Vec::new());
    }
    let client = client(session);
    match blocking(move || client.history_search_sync_context().map_err(core_error)).await {
        Ok(context) => Ok(attempts
            .into_iter()
            .map(|attempt| (attempt, context.clone()))
            .collect()),
        Err(error) => {
            eprintln!("peppy: history-search context failed: {}", error.code);
            Ok(Vec::new())
        }
    }
}

fn handle_history_observation_error(session: &Session, attempt: uuid::Uuid, error: &BridgeError) {
    eprintln!("peppy: history-search observation failed: {}", error.code);
    if error.code == "not-found" {
        session.finish_history_attempt(attempt);
    }
}

async fn observe_history_replay(
    session: &Arc<Session>,
    contexts: Vec<(uuid::Uuid, peppy_client_core::HistorySearchSyncContext)>,
    proof: Option<HistorySearchReplayProof>,
) -> BridgeResult<()> {
    for (attempt, context) in contexts {
        if session.cancel.is_cancelled() {
            break;
        }
        let facts = HistorySearchSyncFacts {
            proof: proof.clone().map(|mut proof| {
                proof.context = context;
                proof
            }),
        };
        let client = client(session);
        match blocking(move || {
            client
                .observe_history_search_sync(attempt, facts)
                .map_err(core_error)
        })
        .await
        {
            Ok(_) => session.notify(),
            Err(error) => handle_history_observation_error(session, attempt, &error),
        }
    }
    Ok(())
}

async fn observe_history_progress(session: &Arc<Session>) -> BridgeResult<()> {
    for attempt in session.active_history_attempts() {
        let client = client(session);
        match blocking(move || {
            client
                .observe_history_search_sync(attempt, HistorySearchSyncFacts::default())
                .map_err(core_error)
        })
        .await
        {
            Ok(_) => session.notify(),
            Err(error) => handle_history_observation_error(session, attempt, &error),
        }
    }
    Ok(())
}

fn record_work_error(session: &Arc<Session>, code: &'static str) {
    session.set_status(|status| status.work_error = Some(code));
}

fn transport_fatal(error: &NetError) -> bool {
    matches!(error, NetError::Offline | NetError::Revoked)
}

/// How a media transfer failure is handled.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MediaOutcome {
    /// Stop this round (offline or revoked).
    Fatal,
    /// Transient server condition (timeouts, 429, 5xx): retried next round, never cached.
    Retry,
    /// Deterministic rejection or verification failure: skipped until restart and shown.
    Permanent,
}

pub(crate) enum Failure {
    Net(NetError),
    Permanent(BridgeError),
}
impl From<NetError> for Failure {
    fn from(error: NetError) -> Self {
        Failure::Net(error)
    }
}

pub(crate) fn classify(failure: &Failure) -> MediaOutcome {
    match failure {
        Failure::Net(error) if transport_fatal(error) => MediaOutcome::Fatal,
        Failure::Net(NetError::Status {
            status: 408 | 425 | 429 | 500..=599,
            ..
        }) => MediaOutcome::Retry,
        _ => MediaOutcome::Permanent,
    }
}

fn draft_transfer_failure_direction(
    is_draft_attachment: bool,
    direction: DraftTransferDirection,
    failure: &Failure,
) -> Option<DraftTransferDirection> {
    (is_draft_attachment && classify(failure) == MediaOutcome::Permanent).then_some(direction)
}

/// Applies the media failure policy; returns `Err` only for a round-ending transport failure.
fn handle_media_failure(
    session: &Arc<Session>,
    id: peppy_client_core::AttachmentId,
    failure: Failure,
) -> BridgeResult<()> {
    match classify(&failure) {
        MediaOutcome::Fatal => match failure {
            Failure::Net(error) => Err(error.into()),
            Failure::Permanent(error) => Err(error),
        },
        MediaOutcome::Retry => {
            record_work_error(session, "media-retry");
            Ok(())
        }
        MediaOutcome::Permanent => {
            session
                .transfer_errors
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(id, "media-transfer-failed".into());
            Ok(())
        }
    }
}

async fn record_draft_transfer_failure(
    session: &Arc<Session>,
    id: peppy_client_core::AttachmentId,
    direction: DraftTransferDirection,
) -> BridgeResult<()> {
    let c = client(session);
    blocking(move || {
        c.record_draft_attachment_transfer_failure(id, direction)
            .map_err(core_error)
    })
    .await
}

async fn clear_draft_transfer_failure(
    session: &Arc<Session>,
    id: peppy_client_core::AttachmentId,
) -> BridgeResult<()> {
    let c = client(session);
    blocking(move || {
        c.clear_draft_attachment_transfer_failure(id)
            .map_err(core_error)
    })
    .await
}

fn handle_download_failure(
    session: &Arc<Session>,
    id: peppy_client_core::AttachmentId,
    failure: Failure,
) -> BridgeResult<()> {
    if is_recoverable_download_unavailable(&failure) && schedule_download_retry(session, id) {
        record_work_error(session, "media-retry");
        return Ok(());
    }
    handle_media_failure(session, id, failure)
}

#[derive(Deserialize)]
struct Reserved {
    attachment_id: String,
}

#[derive(Deserialize)]
struct Finalized {
    attachment_id: String,
    duplicate: bool,
}

pub(crate) async fn record_storage_policy_removal(
    session: &Arc<Session>,
    attachment_id: peppy_client_core::AttachmentId,
) -> Result<(), Failure> {
    let client = client(session);
    blocking(move || {
        client
            .report_attachment_removed_by_storage_policy(attachment_id)
            .map_err(core_error)
    })
    .await
    .map_err(Failure::Permanent)?;
    clear_download_retry(session, &attachment_id);
    session
        .transfer_errors
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(&attachment_id);
    session.notify();
    Ok(())
}

#[cfg(test)]
pub(crate) async fn upload_one(
    session: &Arc<Session>,
    object: peppy_client_core::CipherObject,
) -> Result<(), Failure> {
    upload_object(session, object).await
}

/// Reserve/upload/finalize one prepared object using core-owned classification and tracking.
async fn upload_object(
    session: &Arc<Session>,
    object: peppy_client_core::CipherObject,
) -> Result<(), Failure> {
    upload_object_for_draft(session, object, None).await
}

async fn upload_object_for_draft(
    session: &Arc<Session>,
    object: peppy_client_core::CipherObject,
    draft_id: Option<peppy_client_core::DraftId>,
) -> Result<(), Failure> {
    if session.cancel.is_cancelled() {
        return Ok(());
    }
    let id = object.attachment_id;
    let core = client(session);
    let retention = blocking(move || core.attachment_upload_retention(id).map_err(core_error))
        .await
        .map_err(Failure::Permanent)?;
    let retention_class = match retention.retention_class {
        peppy_client_core::AttachmentRetentionClass::Protected => "protected",
        peppy_client_core::AttachmentRetentionClass::MessageMedia => "message_media",
    };
    let body = serde_json::json!({
        "attachment_id": id.to_string(),
        "declared_ciphertext_bytes": object.ciphertext_bytes,
        "declared_ciphertext_sha256": object.ciphertext_sha256,
        "reference_tracking": retention.reference_tracking,
        "retention_class": retention_class,
        "draft_id": draft_id.map(|id| id.to_string()),
    });
    let recovering_finalized = match session
        .api
        .post_json::<_, Reserved>("/v1/attachments/reserve", &body)
        .await
    {
        Ok(reserved) => {
            let remote =
                uuid::Uuid::parse_str(&reserved.attachment_id).map_err(|_| NetError::Invalid)?;
            if remote.to_string() != id.to_string() {
                return Err(NetError::Invalid.into());
            }
            false
        }
        // A finalized reservation eventually expires, so an idempotent reserve can conflict.
        // Probe finalize with the same authenticated origin/token; only its typed duplicate proof
        // is enough to recover without uploading bytes again.
        Err(NetError::Status { status: 409, .. }) => true,
        Err(error) if is_storage_policy_removal(&error) => {
            record_storage_policy_removal(session, id).await?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let remote = id.to_string();
    if !recovering_finalized {
        let c = client(session);
        let path = blocking(move || c.native_cipher_file(id).map_err(core_error))
            .await
            .map_err(Failure::Permanent)?;
        match session
            .api
            .put_file(
                &format!("/v1/attachments/{remote}/upload"),
                &path,
                object.ciphertext_bytes,
                &session.cancel,
            )
            .await
        {
            Ok(()) => {}
            // Possibly uploaded and finalized before a crash: finalize decides.
            Err(NetError::Status {
                status: 404 | 409, ..
            }) => {}
            Err(error) if is_storage_policy_removal(&error) => {
                record_storage_policy_removal(session, id).await?;
                return Ok(());
            }
            Err(NetError::Cancelled) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    let finalized: Finalized = match session
        .api
        .post_empty(&format!("/v1/attachments/{remote}/finalize"))
        .await
    {
        Ok(finalized) => finalized,
        Err(error) if is_storage_policy_removal(&error) => {
            record_storage_policy_removal(session, id).await?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let finalized_id =
        uuid::Uuid::parse_str(&finalized.attachment_id).map_err(|_| NetError::Invalid)?;
    if finalized_id.to_string() != id.to_string() || (recovering_finalized && !finalized.duplicate)
    {
        return Err(NetError::Invalid.into());
    }
    let c = client(session);
    blocking(move || c.mark_attachment_uploaded(id, &remote).map_err(core_error))
        .await
        .map_err(Failure::Permanent)
}

async fn download_one(
    session: &Arc<Session>,
    object: peppy_client_core::CipherObject,
) -> Result<(), Failure> {
    if session.cancel.is_cancelled() {
        return Ok(());
    }
    let remote = object.remote_object_id.clone().ok_or(NetError::Invalid)?;
    let remote = uuid::Uuid::parse_str(&remote)
        .map_err(|_| NetError::Invalid)?
        .to_string();
    let dir = session.data_dir.join("downloads");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_| NetError::Local)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| NetError::Local)?;
    }
    let target = dir.join(format!("{}.part", object.attachment_id));
    let _ = tokio::fs::remove_file(&target).await;
    match session
        .api
        .download_to(
            &format!("/v1/attachments/{remote}"),
            &target,
            object.ciphertext_bytes,
            &session.cancel,
        )
        .await
    {
        Ok(()) => {}
        Err(error) if is_storage_policy_removal(&error) => {
            record_storage_policy_removal(session, object.attachment_id).await?;
            return Ok(());
        }
        Err(NetError::Cancelled) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let (c, id, file) = (client(session), object.attachment_id, target.clone());
    let installed = blocking(move || {
        c.install_downloaded_attachment(id, &file)
            .map_err(core_error)
    })
    .await;
    let _ = tokio::fs::remove_file(&target).await;
    installed.map_err(Failure::Permanent)
}

fn draft_timestamp_ms(value: &str) -> Result<i64, NetError> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| NetError::Invalid)
        .map(|value| value.unix_timestamp_nanos().div_euclid(1_000_000) as i64)
}

fn remote_draft_change(
    change: peppy_hosted_api::drafts::DraftChange,
) -> Result<peppy_client_core::RemoteDraftChange, NetError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use peppy_client_core::{DraftId, RemoteDraftChange};
    use peppy_hosted_api::drafts::{DraftChange, DraftTerminalReason};
    Ok(match change {
        DraftChange::Head {
            draft_id,
            branch_id,
            revision_id,
            parent_revision_id,
            sealed_record,
            attachment_ids,
            expires_at,
            ..
        } => RemoteDraftChange::Head {
            draft_id: DraftId(draft_id),
            branch_id,
            revision_id,
            parent_revision_id,
            sealed_record: URL_SAFE_NO_PAD
                .decode(sealed_record)
                .map_err(|_| NetError::Invalid)?,
            attachment_ids: attachment_ids
                .into_iter()
                .map(peppy_client_core::AttachmentId)
                .collect(),
            expires_at_ms: draft_timestamp_ms(&expires_at)?,
        },
        DraftChange::BranchClosed {
            draft_id,
            branch_id,
            ..
        } => RemoteDraftChange::BranchClosed {
            draft_id: DraftId(draft_id),
            branch_id,
        },
        DraftChange::Terminal {
            draft_id,
            reason,
            command_id,
            ..
        } => RemoteDraftChange::Terminal {
            draft_id: DraftId(draft_id),
            reason: match reason {
                DraftTerminalReason::Sent => DraftTerminalReasonCore::Sent,
                DraftTerminalReason::Discarded => DraftTerminalReasonCore::Discarded,
                DraftTerminalReason::Expired => DraftTerminalReasonCore::Expired,
            },
            command_id: command_id.map(peppy_client_core::CommandId),
        },
    })
}

async fn upload_draft_attachment(session: &Arc<Session>, upload: DraftUpload) -> BridgeResult<()> {
    let id = upload.object.attachment_id;
    match upload_object_for_draft(session, upload.object, Some(upload.draft_id)).await {
        Ok(()) => clear_draft_transfer_failure(session, id).await,
        Err(failure) => {
            let direction =
                draft_transfer_failure_direction(true, DraftTransferDirection::Upload, &failure);
            handle_media_failure(session, id, failure)?;
            if let Some(direction) = direction {
                record_draft_transfer_failure(session, id, direction).await?;
            }
            Ok(())
        }
    }
}

/// Marks, PUTs and records one pending revision. Returns the recorded outcome, or `None` when the
/// push was no longer eligible or failed transiently.
async fn push_draft_revision(
    session: &Arc<Session>,
    push: DraftPush,
) -> BridgeResult<Option<DraftPushOutcome>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use peppy_hosted_api::drafts::{PutDraftRevisionRequest, PutDraftRevisionResponse};

    let c = client(session);
    let attempted = blocking(move || {
        c.mark_draft_push_attempt(push.draft_id, push.revision_id)
            .map_err(core_error)
    })
    .await?;
    if !attempted {
        return Ok(None);
    }
    let request = PutDraftRevisionRequest {
        base_revision_id: push.base_revision_id,
        sealed_record: URL_SAFE_NO_PAD.encode(push.sealed_record),
        attachment_ids: push.attachment_ids.iter().map(|id| id.0).collect(),
        authored: push.authored,
        resolves_branch_ids: push.resolves_branch_ids,
    };
    let outcome = match session
        .api
        .put_json::<_, PutDraftRevisionResponse>(
            &format!(
                "/v1/drafts/{}/revisions/{}",
                push.draft_id, push.revision_id
            ),
            &request,
        )
        .await
    {
        Ok(response) => DraftPushOutcome::Accepted {
            branch_id: response.branch_id,
            expires_at_ms: draft_timestamp_ms(&response.expires_at).map_err(BridgeError::from)?,
            conflict: response.conflict,
        },
        Err(NetError::Status {
            status: 410,
            code: Some(code),
        }) => match code.as_str() {
            "draft_sent" => DraftPushOutcome::Terminated(DraftTerminalReasonCore::Sent),
            "draft_discarded" => DraftPushOutcome::Terminated(DraftTerminalReasonCore::Discarded),
            "draft_expired" => DraftPushOutcome::Terminated(DraftTerminalReasonCore::Expired),
            _ => return Ok(None),
        },
        Err(NetError::Status {
            status: 409,
            code: Some(code),
        }) if code == "draft_branch_limit" => DraftPushOutcome::BranchLimit,
        Err(NetError::Status {
            status: 422,
            code: Some(code),
        }) if code == "draft_attachment_unavailable" => DraftPushOutcome::AttachmentUnavailable,
        Err(NetError::Status {
            status: 413,
            code: Some(code),
        }) if code == "draft_record_too_large" => DraftPushOutcome::Rejected,
        Err(NetError::Status {
            status: 422,
            code: Some(code),
        }) if code == "draft_invalid_request" => DraftPushOutcome::Rejected,
        Err(error) if transport_fatal(&error) => return Err(error.into()),
        Err(_) => return Ok(None),
    };
    let recorded = outcome.clone();
    let c = client(session);
    blocking(move || {
        c.record_draft_push(push.draft_id, push.revision_id, recorded)
            .map_err(core_error)
    })
    .await?;
    Ok(Some(outcome))
}

pub(crate) enum DraftPushNow {
    Accepted { conflict: bool },
    Terminated(DraftTerminalReasonCore),
    NotPushed,
}

/// Uploads one draft's pending attachments and pushes its pending revision for an immediate send.
/// Waits for an in-flight worker push rather than racing it; a bounded wait yields `NotPushed`.
pub(crate) async fn push_draft_now(
    session: &Arc<Session>,
    draft_id: peppy_client_core::DraftId,
) -> BridgeResult<DraftPushNow> {
    if retention_maintenance_ready(session).await? {
        let failed = session
            .transfer_errors
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let c = client(session);
        let uploads = blocking(move || c.pending_draft_uploads().map_err(core_error)).await?;
        for upload in uploads.into_iter().filter(|upload| {
            upload.draft_id == draft_id && !failed.contains(&upload.object.attachment_id)
        }) {
            upload_draft_attachment(session, upload).await?;
        }
    }
    let Ok(_lock) = timeout(DRAFT_SEND_PUSH_LOCK_WAIT, session.draft_push_lock.lock()).await else {
        return Ok(DraftPushNow::NotPushed);
    };
    let c = client(session);
    let push = blocking(move || {
        c.pending_draft_pushes()
            .map_err(core_error)
            .map(|pushes| pushes.into_iter().find(|push| push.draft_id == draft_id))
    })
    .await?;
    let Some(push) = push else {
        return Ok(DraftPushNow::NotPushed);
    };
    Ok(match push_draft_revision(session, push).await? {
        Some(DraftPushOutcome::Accepted { conflict, .. }) => DraftPushNow::Accepted { conflict },
        Some(DraftPushOutcome::Terminated(reason)) => DraftPushNow::Terminated(reason),
        _ => DraftPushNow::NotPushed,
    })
}

/// Synchronizes encrypted draft metadata and draft-only attachment objects. Draft contents are
/// deliberately never logged; malformed server records fail the round without advancing the cursor.
async fn sync_drafts(
    session: &Arc<Session>,
    retention_maintenance_ready: bool,
    failed: &[peppy_client_core::AttachmentId],
) -> BridgeResult<bool> {
    use peppy_hosted_api::drafts::DraftChangesResponse;
    // Two MAX_DRAFT_RECORD_BYTES records can expand to roughly 700 KiB as JSON/base64, below browser's 1 MiB page cap.
    const DRAFT_PAGE_LIMIT: usize = 2;

    let c = client(session);
    let bootstrap = blocking(move || c.draft_bootstrap_required().map_err(core_error)).await?;
    let after = if bootstrap {
        "0".to_owned()
    } else {
        let c = client(session);
        blocking(move || c.draft_sync_cursor().map_err(core_error)).await?
    };
    let first: DraftChangesResponse = match session
        .api
        .get_json(
            &format!("/v1/drafts?after={after}&limit={DRAFT_PAGE_LIMIT}"),
            MAX_PAGE_BYTES,
        )
        .await
    {
        Ok(page) => page,
        Err(NetError::Status {
            status: 404 | 405,
            code: Some(code),
        }) if code != NON_API_ERROR_CODE => {
            let c = client(session);
            blocking(move || c.set_draft_sync_supported(false).map_err(core_error)).await?;
            return Ok(false);
        }
        Err(NetError::Status {
            status: 404,
            code: None,
        }) => match session
            .api
            .get_json_200::<serde_json::Value>("/v1/vault", MAX_JSON_BYTES)
            .await
        {
            Ok(_) => {
                let c = client(session);
                blocking(move || c.set_draft_sync_supported(false).map_err(core_error)).await?;
                return Ok(false);
            }
            Err(error) if transport_fatal(&error) => return Err(error.into()),
            Err(_) => return Ok(false),
        },
        Err(error) if transport_fatal(&error) => return Err(error.into()),
        Err(_) => return Ok(false),
    };
    let c = client(session);
    blocking(move || c.set_draft_sync_supported(true).map_err(core_error)).await?;
    let mut seen_draft_ids = Vec::new();
    let mut fully_applied = true;
    let mut page = first;
    loop {
        if bootstrap {
            seen_draft_ids.extend(page.changes.iter().map(|change| match change {
                peppy_hosted_api::drafts::DraftChange::Head { draft_id, .. }
                | peppy_hosted_api::drafts::DraftChange::BranchClosed { draft_id, .. }
                | peppy_hosted_api::drafts::DraftChange::Terminal { draft_id, .. } => {
                    peppy_client_core::DraftId(*draft_id)
                }
            }));
        }
        let changes = page
            .changes
            .into_iter()
            .map(remote_draft_change)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BridgeError::from)?;
        let next_after = page.next_after.clone();
        let c = client(session);
        let applied_cursor = next_after.clone();
        let applied =
            tokio::task::spawn_blocking(move || c.apply_draft_changes(changes, &applied_cursor))
                .await
                .map_err(|_| BridgeError::host_state())?;
        if let Err(error) = applied {
            match error {
                peppy_client_core::Error::KeysUnavailable
                | peppy_client_core::Error::Crypto
                | peppy_client_core::Error::InvalidMedia
                | peppy_client_core::Error::InvalidRequest(_) => {
                    record_work_error(session, "draft-apply-deferred");
                    fully_applied = false;
                    break;
                }
                error => return Err(core_error(error)),
            }
        }
        if session.cancel.is_cancelled() {
            return Ok(false);
        }
        if !page.more {
            break;
        }
        page = session
            .api
            .get_json(
                &format!("/v1/drafts?after={next_after}&limit={DRAFT_PAGE_LIMIT}"),
                MAX_PAGE_BYTES,
            )
            .await
            .map_err(BridgeError::from)?;
    }
    if bootstrap && fully_applied {
        let c = client(session);
        blocking(move || {
            c.finish_draft_bootstrap(&seen_draft_ids)
                .map_err(core_error)
        })
        .await?;
    }
    if retention_maintenance_ready {
        let c = client(session);
        let uploads = blocking(move || c.pending_draft_uploads().map_err(core_error)).await?;
        for upload in uploads
            .into_iter()
            .filter(|upload| !failed.contains(&upload.object.attachment_id))
            .take(MEDIA_PER_ROUND)
        {
            if session.cancel.is_cancelled() {
                return Ok(true);
            }
            upload_draft_attachment(session, upload).await?;
        }
    }
    let _lock = session.draft_push_lock.lock().await;
    let c = client(session);
    let pushes = blocking(move || c.pending_draft_pushes().map_err(core_error)).await?;
    for push in pushes {
        push_draft_revision(session, push).await?;
    }
    let c = client(session);
    let discards = blocking(move || c.pending_draft_discards().map_err(core_error)).await?;
    for draft_id in discards {
        match session
            .api
            .post_discard(
                &format!("/v1/drafts/{draft_id}/discard"),
                &serde_json::json!({}),
            )
            .await
        {
            Ok(()) => {
                let c = client(session);
                blocking(move || c.record_draft_discard(draft_id).map_err(core_error)).await?;
            }
            Err(error) if transport_fatal(&error) => return Err(error.into()),
            Err(_) => {}
        }
    }
    Ok(true)
}

async fn retention_maintenance_ready(session: &Arc<Session>) -> BridgeResult<bool> {
    if !session
        .attachment_retention_negotiated
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Ok(false);
    }
    if !session
        .attachment_retention_supported
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Ok(true);
    }
    match crate::attachment_retention::drain_due_operations(session, &session.generation).await {
        Ok(()) => Ok(true),
        Err(error) if error.code != "revoked" => {
            record_work_error(session, "attachment-retention-deferred");
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod draft_push_outcome_tests {
    use super::*;
    use crate::{
        contacts::tests::mock_server,
        tests::{fixture_at, open},
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_documented_client_rejections_are_permanent() {
        let cases = [
            (413, "draft_record_too_large", true),
            (422, "draft_invalid_request", true),
            (413, "draft_invalid_request", false),
            (422, "draft_record_too_large", false),
            (500, "draft_invalid_request", false),
        ];
        for (status, code, rejected) in cases {
            let (origin, seen) = mock_server(move |_, _, _| {
                (status, serde_json::json!({ "code": code }).to_string())
            })
            .await;
            let fixture = fixture_at(&origin);
            let session = Arc::new(open(&fixture, &fixture.binding, &[]));
            session
                .client
                .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
                .unwrap();
            session.client.set_draft_sync_supported(true).unwrap();
            let draft = session
                .save_draft(&crate::tests::input(
                    "",
                    "",
                    "draft",
                    &["+15555550100"],
                    "0",
                ))
                .unwrap();
            let push = session.client.pending_draft_pushes().unwrap().remove(0);
            let draft_id = push.draft_id;
            let outcome = push_draft_revision(&session, push).await.unwrap();
            assert!(
                if rejected {
                    matches!(outcome, Some(DraftPushOutcome::Rejected))
                } else {
                    outcome.is_none()
                },
                "{status} {code}"
            );
            assert_eq!(
                seen.lock()
                    .unwrap()
                    .iter()
                    .filter(|request| request.method == "PUT")
                    .count(),
                1
            );
            let state = session.client.draft_sync_status(draft_id).unwrap().state;
            assert_eq!(
                matches!(state, peppy_client_core::DraftSyncState::Failed),
                rejected,
                "{status} {code}"
            );
            assert_eq!(draft.id, draft_id.to_string());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_draft_route_uses_the_authenticated_vault_probe() {
        let (origin, seen) = mock_server(|method, path, _| match (method, path) {
            // A router 404 is JSON but has no API error code. A plaintext 404 is classified as
            // a proxy/non-API response and must not disable draft sync.
            ("GET", "/v1/drafts?after=0&limit=2") => (404, "{}".into()),
            ("GET", "/v1/vault") => (200, "{}".into()),
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        assert!(!sync_drafts(&session, false, &[]).await.unwrap());
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|request| request.path == "/v1/vault"));
        assert!(seen.iter().all(|request| request.path != "/v1/gateways"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_draft_route_probe_propagates_revocation() {
        let (origin, _) = mock_server(|method, path, _| match (method, path) {
            ("GET", "/v1/drafts?after=0&limit=2") => (404, "{}".into()),
            ("GET", "/v1/vault") => (401, "{}".into()),
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        assert_eq!(
            sync_drafts(&session, false, &[]).await.unwrap_err().code,
            "revoked"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deferred_bootstrap_page_does_not_complete_absence_reconciliation() {
        let response = serde_json::json!({
            "changes": [{
                "kind": "head",
                "seq": "1",
                "draft_id": "00000000-0000-0000-0000-000000000001",
                "branch_id": "00000000-0000-0000-0000-000000000002",
                "revision_id": "00000000-0000-0000-0000-000000000003",
                "parent_revision_id": null,
                "sealed_record": "AA",
                "attachment_ids": [],
                "expires_at": "2026-12-01T00:00:00Z"
            }],
            "next_after": "1",
            "more": true
        })
        .to_string();
        let (origin, _) = mock_server(move |method, path, _| match (method, path) {
            ("GET", "/v1/drafts?after=0&limit=2") => (200, response.clone()),
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        sync_drafts(&session, false, &[]).await.unwrap();
        assert!(session.client.draft_bootstrap_required().unwrap());
    }

    #[test]
    fn production_classifier_retries_transient_server_failures() {
        assert_eq!(
            classify(&Failure::Net(NetError::Status {
                status: 503,
                code: None,
            })),
            MediaOutcome::Retry
        );
    }

    #[test]
    fn production_draft_failure_policy_records_permanent_downloads_only_for_drafts() {
        assert_eq!(
            draft_transfer_failure_direction(
                true,
                DraftTransferDirection::Download,
                &Failure::Net(NetError::Invalid),
            ),
            Some(DraftTransferDirection::Download)
        );
        assert_eq!(
            draft_transfer_failure_direction(
                false,
                DraftTransferDirection::Download,
                &Failure::Net(NetError::Invalid),
            ),
            None
        );
        assert_eq!(
            draft_transfer_failure_direction(
                true,
                DraftTransferDirection::Download,
                &Failure::Net(NetError::Status {
                    status: 503,
                    code: None,
                }),
            ),
            None
        );
    }
}

/// Bounded outbound and media work: uploads (before held MMS envelopes can seal), sealing,
/// outbox upload with acknowledgement only after HTTP acceptance, then verified downloads.
pub async fn work_round(session: &Arc<Session>) -> BridgeResult<()> {
    // Each round reports only its own problems.
    session.set_status(|status| status.work_error = None);
    // A transient retention transport failure defers quota-bearing producers, but does not turn
    // the negotiated protocol off or prevent safe replay/outbox work from making progress.
    let retention_maintenance_ready = retention_maintenance_ready(session).await?;
    let mut changed = false;
    let failed: Vec<_> = session
        .transfer_errors
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .keys()
        .copied()
        .collect();
    changed |= sync_drafts(session, retention_maintenance_ready, &failed).await?;
    let tracked = contact_photo_transfers(session).await?;
    if !tracked.unavailable.is_empty() {
        record_work_error(session, "contact-photo-unavailable");
    }
    if retention_maintenance_ready {
        let c = client(session);
        let uploads = blocking(move || c.pending_uploads().map_err(core_error)).await?;
        for object in uploads
            .into_iter()
            .filter(|o| !failed.contains(&o.attachment_id))
            .take(MEDIA_PER_ROUND)
        {
            if session.cancel.is_cancelled() {
                return Ok(());
            }
            let id = object.attachment_id;
            match upload_object(session, object).await {
                Ok(()) => changed = true,
                Err(failure) => {
                    handle_media_failure(session, id, failure)?;
                    changed = true;
                }
            }
        }
    }
    for _ in 0..MAX_OUTBOX_BATCHES {
        if session.cancel.is_cancelled() {
            return Ok(());
        }
        let c = client(session);
        let sealed = blocking(move || match c.seal_pending_batch(MAX_SEAL_BATCH) {
            Ok(count) => Ok(count),
            Err(peppy_client_core::Error::KeysUnavailable) => Ok(0),
            Err(error) => Err(core_error(error)),
        })
        .await?;
        if sealed == 0 {
            break;
        }
    }
    // Held contact-photo envelopes become uploadable only after every reference is registered.
    if retention_maintenance_ready {
        changed |= register_contact_photo_references(session).await?;
    }
    'outbox: for _ in 0..MAX_OUTBOX_BATCHES {
        if session.cancel.is_cancelled() {
            return Ok(());
        }
        let c = client(session);
        let batch =
            blocking(move || c.pending_outbox_batch(OUTBOX_BATCH).map_err(core_error)).await?;
        if batch.is_empty() {
            break;
        }
        for envelope in batch {
            if session.cancel.is_cancelled() {
                return Ok(());
            }
            let path = match envelope.purpose {
                EnvelopePurpose::Command => "/v1/commands",
                EnvelopePurpose::Event => "/v1/events",
            };
            match session.api.post_discard(path, &envelope).await {
                Ok(()) => {
                    let (c, id) = (client(session), envelope.envelope_id);
                    blocking(move || c.ack_outbox(id).map_err(core_error)).await?;
                    changed = true;
                }
                Err(error) if transport_fatal(&error) => return Err(error.into()),
                Err(_) => {
                    // Typed rejection (for example a retired key epoch). The row stays queued and
                    // is not acknowledged; inbound sync continues.
                    record_work_error(session, "outbox-rejected");
                    break 'outbox;
                }
            }
        }
    }
    if !tracked.reclaims.is_empty() {
        reclaim_contact_photos(session, &tracked.reclaims).await?;
    }
    let c = client(session);
    let downloads = blocking(move || c.pending_download_candidates().map_err(core_error)).await?;
    let downloads = select_downloads(
        downloads
            .into_iter()
            .filter(|candidate| {
                !failed.contains(&candidate.object.attachment_id)
                    && download_retry_ready(session, &candidate.object.attachment_id)
            })
            .map(|candidate| (candidate.object, candidate.demanded))
            .collect(),
    );
    let c = client(session);
    let draft_attachment_ids = blocking(move || {
        c.compose_drafts().map_err(core_error).map(|drafts| {
            drafts
                .into_iter()
                .flat_map(|draft| draft.attachment_ids)
                .collect::<Vec<_>>()
        })
    })
    .await?;
    for object in downloads {
        if session.cancel.is_cancelled() {
            return Ok(());
        }
        let id = object.attachment_id;
        let is_draft_attachment = draft_attachment_ids.contains(&id);
        match download_one(session, object).await {
            Ok(()) => {
                clear_download_retry(session, &id);
                if is_draft_attachment {
                    clear_draft_transfer_failure(session, id).await?;
                }
                session.invalidate_cached_attachment(&id);
                changed = true;
            }
            Err(failure) => {
                let direction = draft_transfer_failure_direction(
                    is_draft_attachment,
                    DraftTransferDirection::Download,
                    &failure,
                );
                handle_download_failure(session, id, failure)?;
                if let Some(direction) = direction {
                    record_draft_transfer_failure(session, id, direction).await?;
                }
                changed = true;
            }
        }
    }
    // Records journaled while keys were locked (for example history that arrived before an
    // unlock or key-cache import) are applied here; the worker is woken after every unlock.
    apply_pass_and_wake_owed_deletions(session).await?;
    if session
        .attachment_retention_supported
        .load(std::sync::atomic::Ordering::Acquire)
        && session
            .attachment_retention_replay_observed
            .load(std::sync::atomic::Ordering::Acquire)
        && !session
            .attachment_retention_ready_advertised
            .load(std::sync::atomic::Ordering::Acquire)
    {
        match crate::attachment_retention::reconcile_after_replay(session, &session.generation)
            .await
        {
            Ok(()) => {}
            Err(error) if error.code == "revoked" => return Err(error),
            Err(_) => record_work_error(session, "attachment-retention-deferred"),
        }
    }
    if changed {
        session.notify();
    }
    Ok(())
}

#[derive(Deserialize, Default)]
struct ContactPhotoTransfers {
    #[serde(default)]
    registrations: Vec<PhotoRegistration>,
    #[serde(default)]
    reclaims: Vec<PhotoReclaim>,
    #[serde(default)]
    unavailable: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct PhotoRegistration {
    envelope_id: String,
    /// Local attachment ID used for durable acknowledgement and removal bookkeeping.
    local_attachment_id: String,
    /// Remote object ID.
    attachment_id: String,
    producer_device_id: String,
    producer_sequence: String,
}
#[derive(Deserialize)]
struct PhotoReclaim {
    /// Local attachment ID (acknowledgement key).
    attachment_id: String,
    remote_object_id: String,
    #[serde(default)]
    release_after_cursor: Option<String>,
}

async fn contact_photo_transfers(session: &Arc<Session>) -> BridgeResult<ContactPhotoTransfers> {
    let c = client(session);
    let raw = blocking(move || c.contact_photo_transfer_state_json().map_err(core_error)).await?;
    serde_json::from_str(&raw).map_err(|_| BridgeError::host_state())
}

fn canonical_uuid(value: &str) -> Option<String> {
    uuid::Uuid::parse_str(value)
        .ok()
        .map(|u| u.to_string())
        .filter(|u| u == value)
}

/// `POST /v1/attachments/{remote}/references` for each core-queued registration, acknowledging
/// only a successful response. Returns whether anything was released.
async fn register_contact_photo_references(session: &Arc<Session>) -> BridgeResult<bool> {
    let transfers = contact_photo_transfers(session).await?;
    let mut released = false;
    for registration in transfers.registrations {
        if session.cancel.is_cancelled() {
            return Ok(released);
        }
        let Some(remote) = canonical_uuid(&registration.attachment_id) else {
            record_work_error(session, "contact-photo-reference");
            continue;
        };
        let body = serde_json::json!({"references": [{
            "producer_device_id": registration.producer_device_id,
            "producer_sequence": registration.producer_sequence,
        }]});
        match session
            .api
            .post_discard(&format!("/v1/attachments/{remote}/references"), &body)
            .await
        {
            Ok(()) => {
                let (c, input) = (
                    client(session),
                    serde_json::json!({
                        "schema_version": 1,
                        "envelope_id": registration.envelope_id,
                        "attachment_id": remote,
                    })
                    .to_string(),
                );
                blocking(move || {
                    c.acknowledge_contact_photo_reference_json(&input)
                        .map_err(core_error)
                })
                .await?;
                released = true;
            }
            Err(error) if is_storage_policy_removal(&error) => {
                let attachment_id = registration
                    .local_attachment_id
                    .parse()
                    .map_err(|_| BridgeError::host_state())?;
                record_storage_policy_removal(session, attachment_id)
                    .await
                    .map_err(|failure| match failure {
                        Failure::Net(error) => BridgeError::from(error),
                        Failure::Permanent(error) => error,
                    })?;
            }
            Err(error) if transport_fatal(&error) => return Err(error.into()),
            // Not acknowledged: the envelope stays held and is retried next round.
            Err(_) => record_work_error(session, "contact-photo-reference"),
        }
    }
    Ok(released)
}

#[derive(Deserialize)]
struct ReplayMarks {
    replay_floor_cursor: String,
}

/// Fresh server proof inputs for reclaim: the current compaction generation and replay floor.
/// `None` when the server cannot prove a release now (no compaction, or this device must resync).
async fn release_proof_inputs(session: &Arc<Session>) -> BridgeResult<Option<(u64, u64)>> {
    let start: SnapshotStart = match session.api.get_json("/v1/snapshot", MAX_JSON_BYTES).await {
        Ok(start) => start,
        Err(error) if transport_fatal(&error) => return Err(error.into()),
        Err(_) => return Ok(None),
    };
    let Some(generation) = parse_compaction_generation(&start).ok().flatten() else {
        return Ok(None);
    };
    let c = client(session);
    let after = blocking(move || c.receive_cursor().map_err(core_error))
        .await?
        .0;
    let marks: ReplayMarks = match session
        .api
        .get_json(&format!("/v1/events?after={after}&limit=1"), MAX_PAGE_BYTES)
        .await
    {
        Ok(marks) => marks,
        Err(error) if transport_fatal(&error) => return Err(error.into()),
        Err(_) => return Ok(None),
    };
    let floor = marks
        .replay_floor_cursor
        .parse::<u64>()
        .map_err(|_| BridgeError::from(NetError::Invalid))?;
    Ok(Some((generation, floor)))
}

/// Owner-side reclaim handshake for released contact photos: `DELETE` with the fresh
/// generation and replay floor once the floor covers every reference. 204/404 complete and 409
/// backs off (core schedules the retry); other failures leave the candidate untouched.
async fn reclaim_contact_photos(
    session: &Arc<Session>,
    reclaims: &[PhotoReclaim],
) -> BridgeResult<()> {
    let Some((generation, floor)) = release_proof_inputs(session).await? else {
        return Ok(());
    };
    for reclaim in reclaims {
        if session.cancel.is_cancelled() {
            return Ok(());
        }
        let Some(after) = reclaim
            .release_after_cursor
            .as_deref()
            .and_then(|c| c.parse::<u64>().ok())
        else {
            continue;
        };
        let Some(remote) = canonical_uuid(&reclaim.remote_object_id) else {
            continue;
        };
        if floor < after || floor == 0 {
            continue;
        }
        let body = serde_json::json!({
            "compaction_generation": generation.to_string(),
            "release_before_cursor": floor.to_string(),
        });
        let status = match session
            .api
            .delete_json(&format!("/v1/attachments/{remote}"), &body)
            .await
        {
            Ok(()) => 204,
            Err(NetError::Status {
                status: status @ (404 | 409),
                ..
            }) => status,
            Err(error) if is_storage_policy_removal(&error) => {
                let attachment_id = reclaim
                    .attachment_id
                    .parse()
                    .map_err(|_| BridgeError::host_state())?;
                record_storage_policy_removal(session, attachment_id)
                    .await
                    .map_err(|failure| match failure {
                        Failure::Net(error) => BridgeError::from(error),
                        Failure::Permanent(error) => error,
                    })?;
                let (c, input) = (
                    client(session),
                    serde_json::json!({
                        "schema_version": 1,
                        "attachment_id": reclaim.attachment_id,
                        // A storage-policy tombstone is terminal like an already absent object.
                        "http_status": 404,
                    })
                    .to_string(),
                );
                if blocking(move || {
                    c.acknowledge_contact_photo_reclaim_json(&input)
                        .map_err(core_error)
                })
                .await
                .is_err()
                {
                    record_work_error(session, "contact-photo-reclaim");
                }
                continue;
            }
            Err(error) if transport_fatal(&error) => return Err(error.into()),
            Err(_) => {
                record_work_error(session, "contact-photo-reclaim");
                continue;
            }
        };
        let (c, input) = (
            client(session),
            serde_json::json!({
                "schema_version": 1,
                "attachment_id": reclaim.attachment_id,
                "http_status": status,
            })
            .to_string(),
        );
        // An unknown or already-settled candidate is not a round failure.
        if blocking(move || {
            c.acknowledge_contact_photo_reclaim_json(&input)
                .map_err(core_error)
        })
        .await
        .is_err()
        {
            record_work_error(session, "contact-photo-reclaim");
        }
    }
    Ok(())
}

/// One fenced compaction snapshot while core latches a contact projection repair. Runs once per
/// session (or per manual request) so an unsupported server cannot cause a snapshot loop.
/// Runs the optional contact repair. A refusal by the window only defers it: the caller's round keeps
/// going (replay, drafts, outbox), and the repair is attempted again once the window allows.
pub(crate) async fn repair_if_required(session: &Arc<Session>) -> BridgeResult<()> {
    let c = client(session);
    let required = blocking(move || c.contact_repair_required().map_err(core_error)).await?;
    if !required {
        return Ok(());
    }
    let window_open = !snapshot_gate(session)
        .wait_before_attempt(Instant::now())
        .is_zero();
    if window_open
        || session
            .repair_attempted
            .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Ok(());
    }
    session
        .repair_finished
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if session
        .repair_attempted
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return Ok(());
    }
    let result = snapshot_resync(session).await;
    settle_repair_attempt(session, &result);
    session.notify();
    match result {
        Err(error) if error.code == SNAPSHOT_TOO_LARGE_CODE => Ok(()),
        other => other,
    }
}

fn revoke(session: &Arc<Session>) {
    session.set_status(|status| {
        status.revoked = true;
        status.live = false;
        status.error_code = Some("revoked");
    });
    session.cancel.cancel();
    session.notify();
}

pub(crate) fn jitter(base: Duration) -> Duration {
    let mut bytes = [0u8; 2];
    let _ = getrandom::fill(&mut bytes);
    base + base.mul_f64(f64::from(u16::from_le_bytes(bytes)) / f64::from(u16::MAX) / 4.0)
}

/// Worker wait: `delay`, an explicit work request, or cancellation (false when cancelled).
/// Uses its own `Notify`, so a reconnect wait can never consume a work wake-up.
pub(crate) async fn pause_worker(session: &Arc<Session>, delay: Duration) -> bool {
    tokio::select! {
        _ = session.cancel.cancelled() => false,
        _ = sleep(delay) => true,
        _ = session.work_wake.notified() => true,
    }
}

/// Reconnect wait for the live task (woken by unlock, never by ordinary work requests).
pub(crate) async fn pause_live(session: &Arc<Session>, delay: Duration) -> bool {
    tokio::select! {
        _ = session.cancel.cancelled() => false,
        _ = sleep(delay) => true,
        _ = session.reconnect_wake.notified() => true,
        _ = session.replay_wake.notified() => true,
    }
}

/// A connection must stay up this long before reconnect backoff resets, so a server that
/// accepts and immediately drops sockets cannot drive a tight reconnect loop.
pub const STABLE_CONNECTION: Duration = Duration::from_secs(60);

pub fn next_backoff(current: Duration, stable: bool) -> Duration {
    if stable {
        Duration::from_secs(1)
    } else {
        (current * 2).min(MAX_BACKOFF)
    }
}

/// Outbound/media worker: runs immediately, on wake hints, and every 15 s.
async fn worker(session: Arc<Session>) {
    session.first_snapshot_or_timeout().await;
    if session.cancel.is_cancelled() {
        return;
    }
    let mut ticks = 0u32;
    let mut backoff = Duration::from_secs(1);
    loop {
        if session.cancel.is_cancelled() {
            return;
        }
        if ticks.is_multiple_of(GATEWAY_REFRESH_EVERY) {
            match refresh_gateways(&session).await {
                Err(error) if error.code == "revoked" => return revoke(&session),
                _ => {}
            }
        }
        ticks = ticks.wrapping_add(1);
        let delay = match work_round(&session).await {
            Ok(()) => {
                backoff = Duration::from_secs(1);
                WORK_INTERVAL
            }
            Err(error) if error.code == "revoked" => return revoke(&session),
            Err(error) if error.code == "update-in-progress" => WORK_INTERVAL,
            Err(_) => {
                backoff = (backoff * 2).min(MAX_BACKOFF);
                jitter(backoff)
            }
        };
        if !pause_worker(&session, delay).await {
            return;
        }
    }
}

enum WsEnd {
    Cancelled,
    Revoked,
    Resync,
    Ended { stable: bool, code: &'static str },
}

const PRESENCE_REFRESH_DELAY: Duration = Duration::from_millis(100);

fn schedule_presence_refresh(due: &mut Option<Instant>, now: Instant) {
    if due.is_none() {
        *due = Some(now + PRESENCE_REFRESH_DELAY);
    }
}

fn handle_live_hint(
    session: &Session,
    kind: &str,
    presence_refresh_due: &mut Option<Instant>,
) -> bool {
    match kind {
        "presence_changed" => schedule_presence_refresh(presence_refresh_due, Instant::now()),
        "drafts_changed" => {
            session.request_work();
            return true;
        }
        _ => {}
    }
    false
}

fn close_code(frame: Option<&tungstenite::protocol::CloseFrame>) -> Option<u16> {
    frame.map(|frame| u16::from(frame.code))
}

#[derive(Deserialize)]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    envelope: Option<Box<RawValue>>,
}

async fn ws_session(session: &Arc<Session>) -> WsEnd {
    let ended = |code| WsEnd::Ended {
        stable: false,
        code,
    };
    let Ok(url) = websocket_url(session.api.origin()) else {
        return ended("invalid-origin");
    };
    let Ok(mut request) = url.into_client_request() else {
        return ended("invalid-origin");
    };
    let Ok(mut bearer) =
        tungstenite::http::HeaderValue::from_str(&format!("Bearer {}", session.api.bearer()))
    else {
        return ended("credential-invalid");
    };
    bearer.set_sensitive(true);
    request
        .headers_mut()
        .insert(tungstenite::http::header::AUTHORIZATION, bearer);
    let config = WebSocketConfig::default()
        .max_message_size(Some(WS_MAX_MESSAGE))
        .max_frame_size(Some(WS_MAX_MESSAGE));
    let connector = match crate::tls::websocket_connector(session.api.origin()) {
        Ok(connector) => connector,
        Err(_) => return ended("local-tls-config"),
    };
    // Plain `ws://` is only produced for validated loopback origins; redirects are never followed.
    let connected = tokio::select! {
        _ = session.cancel.cancelled() => return WsEnd::Cancelled,
        result = timeout(WS_CONNECT_TIMEOUT, tokio_tungstenite::connect_async_tls_with_config(request, Some(config), true, connector)) => result,
    };
    let mut socket = match connected {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(tungstenite::Error::Http(response))) if response.status() == 401 => {
            return WsEnd::Revoked;
        }
        _ => return ended("live-unavailable"),
    };
    let c = client(session);
    let Ok(cursor) = blocking(move || c.receive_cursor().map_err(core_error)).await else {
        return ended("core");
    };
    let hello = serde_json::json!({"protocol_version": 1, "resume_cursor": cursor.0.to_string()})
        .to_string();
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return ended("live-unavailable");
    }
    // Negotiation: the first data frame must be `ready`.
    let deadline = Instant::now() + WS_READY_TIMEOUT;
    loop {
        let next = tokio::select! {
            _ = session.cancel.cancelled() => { let _ = timeout(WS_CLOSE_TIMEOUT, socket.close(None)).await; return WsEnd::Cancelled }
            next = tokio::time::timeout_at(deadline, socket.next()) => next,
        };
        match next {
            Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<Frame>(text.as_str())
            {
                Ok(frame) if frame.kind == "ready" => break,
                Ok(frame) if frame.kind == "resync_required" => return WsEnd::Resync,
                _ => return ended("live-protocol"),
            },
            Ok(Some(Ok(Message::Close(frame)))) => {
                return match close_code(frame.as_ref()) {
                    Some(WS_CLOSE_REVOKED) => WsEnd::Revoked,
                    Some(WS_CLOSE_RESYNC) => WsEnd::Resync,
                    _ => ended("live-closed"),
                };
            }
            Ok(Some(Ok(_))) => continue,
            _ => return ended("live-unavailable"),
        }
    }
    session.set_status(|status| {
        status.live = true;
        status.error_code = None;
    });
    session.notify();
    let ready_at = Instant::now();
    let ended = |code| WsEnd::Ended {
        stable: ready_at.elapsed() >= STABLE_CONNECTION,
        code,
    };
    let mut last_frame = Instant::now();
    let mut presence_refresh_due = None;
    let mut pinged = false;
    let mut liveness = tokio::time::interval(Duration::from_secs(15));
    let mut pending = None;
    'frames: loop {
        let next = if pending.is_some() {
            match deferred_frame_control(
                &session.cancel,
                &session.repair_wake,
                &session.replay_wake,
                &mut liveness,
                presence_refresh_due,
            ) {
                DeferredFrameControl::Cancelled => {
                    let _ = timeout(WS_CLOSE_TIMEOUT, socket.close(None)).await;
                    return WsEnd::Cancelled;
                }
                DeferredFrameControl::Resync => {
                    let _ = timeout(WS_CLOSE_TIMEOUT, socket.close(None)).await;
                    return WsEnd::Resync;
                }
                DeferredFrameControl::Liveness => {
                    let idle = last_frame.elapsed();
                    if idle > WS_IDLE_LIMIT {
                        return ended("live-timeout");
                    }
                    if idle > WS_PING_AFTER && !pinged {
                        pinged = true;
                        if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                            return ended("live-unavailable");
                        }
                    } else if socket.flush().await.is_err() {
                        return ended("live-unavailable");
                    }
                    continue;
                }
                DeferredFrameControl::RefreshPresence => {
                    presence_refresh_due = None;
                    let _ = refresh_gateways(session).await;
                    continue;
                }
                DeferredFrameControl::ProcessFrame => pending.take().unwrap(),
            }
        } else {
            let next = tokio::select! {
                _ = session.cancel.cancelled() => { let _ = timeout(WS_CLOSE_TIMEOUT, socket.close(None)).await; return WsEnd::Cancelled }
                // A manual contact repair runs in the next HTTP round.
                _ = session.repair_wake.notified() => { let _ = timeout(WS_CLOSE_TIMEOUT, socket.close(None)).await; return WsEnd::Resync }
                _ = session.replay_wake.notified() => { let _ = timeout(WS_CLOSE_TIMEOUT, socket.close(None)).await; return WsEnd::Resync }
                _ = liveness.tick() => {
                    let idle = last_frame.elapsed();
                    if idle > WS_IDLE_LIMIT {
                        return ended("live-timeout");
                    }
                    if idle > WS_PING_AFTER && !pinged {
                        pinged = true;
                        if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                            return ended("live-unavailable");
                        }
                    } else if socket.flush().await.is_err() {
                        return ended("live-unavailable");
                    }
                    continue;
                }
                _ = async {
                    match presence_refresh_due {
                        Some(due) => tokio::time::sleep_until(due).await,
                        None => std::future::pending().await,
                    }
                } => {
                    presence_refresh_due = None;
                    let _ = refresh_gateways(session).await;
                    continue;
                }
                next = socket.next() => next,
            };
            last_frame = Instant::now();
            pinged = false;
            next
        };
        match next {
            Some(Ok(Message::Text(text))) => {
                let Ok(frame) = serde_json::from_str::<Frame>(text.as_str()) else {
                    return ended("live-protocol");
                };
                match frame.kind.as_str() {
                    "event" => {
                        let (Some(cursor), Some(envelope)) = (frame.cursor, frame.envelope) else {
                            return ended("live-protocol");
                        };
                        let mut records = vec![RawRecord { cursor, envelope }];
                        let mut batch_bytes = records[0].envelope.get().len();
                        loop {
                            if records.len() >= WS_BATCH_FRAMES || batch_bytes >= WS_BATCH_BYTES {
                                break;
                            }
                            let Some(next) = socket.next().now_or_never() else {
                                break;
                            };
                            last_frame = Instant::now();
                            pinged = false;
                            let Some(Ok(Message::Text(next_text))) = next else {
                                pending = Some(next);
                                break;
                            };
                            let Ok(next_frame) = serde_json::from_str::<Frame>(next_text.as_str())
                            else {
                                pending = Some(Some(Ok(Message::Text(next_text))));
                                break;
                            };
                            if next_frame.kind != "event"
                                || next_frame.cursor.is_none()
                                || next_frame.envelope.is_none()
                            {
                                debug_assert_eq!(
                                    live_batch_decision(
                                        records.len(),
                                        batch_bytes,
                                        records.last().map(|record| record.cursor.as_str()),
                                        LiveBatchItem::Other,
                                    ),
                                    LiveBatchDecision::Flush
                                );
                                pending = Some(Some(Ok(Message::Text(next_text))));
                                break;
                            }
                            let next_cursor = next_frame.cursor.as_deref().unwrap();
                            let next_envelope = next_frame.envelope.as_ref().unwrap();
                            if live_batch_decision(
                                records.len(),
                                batch_bytes,
                                records.last().map(|record| record.cursor.as_str()),
                                LiveBatchItem::Event {
                                    cursor: next_cursor,
                                    envelope_bytes: next_envelope.get().len(),
                                },
                            ) == LiveBatchDecision::Flush
                            {
                                pending = Some(Some(Ok(Message::Text(next_text))));
                                break;
                            }
                            batch_bytes += next_envelope.get().len();
                            records.push(RawRecord {
                                cursor: next_frame.cursor.unwrap(),
                                envelope: next_frame.envelope.unwrap(),
                            });
                        }
                        if let Err(error) = ingest_page(session, records).await {
                            if error.code == "update-in-progress" {
                                return WsEnd::Resync;
                            }
                            session.set_status(|status| status.error_code = Some(error.code));
                            return ended(error.code);
                        }
                        // The live record is durable before observers receive a new snapshot.
                        session.notify();
                        if let Err(error) = drain_apply(session, APPLY_STEPS).await {
                            if error.code == "update-in-progress" {
                                return WsEnd::Resync;
                            }
                            return ended("core");
                        }
                        // Socket records carry no replay head. Owed deletion members that wait only
                        // for one get it from the live loop's next HTTP round. The notify coalesces,
                        // and nothing asks again until another record arrives.
                        if let Err(error) = wake_replay_for_owed_deletions(session).await {
                            return ended(error.code);
                        }
                        let _ = observe_history_progress(session).await;
                        continue 'frames;
                    }
                    "resync_required" => return WsEnd::Resync,
                    "presence_changed" | "drafts_changed" => {
                        handle_live_hint(session, &frame.kind, &mut presence_refresh_due);
                    }
                    _ => {}
                }
            }
            Some(Ok(Message::Close(frame))) => {
                return match close_code(frame.as_ref()) {
                    Some(WS_CLOSE_REVOKED) => WsEnd::Revoked,
                    Some(WS_CLOSE_RESYNC) => WsEnd::Resync,
                    _ => ended("live-closed"),
                };
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return ended("live-unavailable"),
        }
    }
}

/// Live loop: verify credential, HTTP catch-up (including staged snapshot), then WebSocket;
/// reconnect with bounded jittered backoff. Revocation stops all networking for the session.
async fn live(session: Arc<Session>) {
    session.first_snapshot_or_timeout().await;
    if session.cancel.is_cancelled() {
        return;
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        if session.cancel.is_cancelled() {
            return;
        }
        if let Err(error) = recover_decoder(&session, DECODER_RECOVERY_PAGE).await {
            eprintln!("peppy: decoder recovery failed: {}", error.code);
            if !pause_live(&session, jitter(backoff)).await {
                return;
            }
            backoff = next_backoff(backoff, false);
            continue;
        }
        if session.cancel.is_cancelled() {
            return;
        }
        let round = async {
            refresh_vault(&session).await?;
            probe_snapshot_capability(&session).await?;
            let c = client(&session);
            let bootstrap = !session
                .attachment_retention_replay_observed
                .load(std::sync::atomic::Ordering::Acquire)
                && blocking(move || c.receive_cursor().map_err(core_error))
                    .await?
                    .0
                    == 0;
            if bootstrap {
                session.set_bootstrap(true, false);
            }
            let replay_result = replay_until_caught_up(&session).await;
            if bootstrap {
                session.set_bootstrap(false, true);
            }
            replay_result?;
            if session
                .attachment_retention_supported
                .load(std::sync::atomic::Ordering::Acquire)
            {
                match crate::attachment_retention::reconcile_after_replay(
                    &session,
                    &session.generation,
                )
                .await
                {
                    Ok(()) => {}
                    Err(error) if error.code == "revoked" => return Err(error),
                    Err(_) => record_work_error(&session, "attachment-retention-deferred"),
                }
            }
            repair_if_required(&session).await?;
            drain_apply(&session, APPLY_STEPS).await.map(|_| ())
        }
        .await;
        if let Err(error) = round {
            if error.code == "revoked" {
                return revoke(&session);
            }
            if error.code == "update-in-progress" {
                continue;
            }
            let snapshot_too_large = error.code == "snapshot-too-large";
            session.set_status(|status| {
                if snapshot_too_large {
                    // The server is reachable; this is a product compatibility state, not a
                    // broken connection.
                    status.live = true;
                } else {
                    status.live = false;
                }
                status.error_code = Some(error.code);
            });
            session.notify();
            if snapshot_too_large {
                // Wait out the remaining window. A refusal without a recorded window (for example an
                // append refusal) starts one here, so the loop never spins on it.
                let wait = {
                    let mut gate = snapshot_gate(&session);
                    let now = Instant::now();
                    if gate.wait_before_attempt(now).is_zero() {
                        gate.record_too_large(now);
                    }
                    gate.wait_before_attempt(now)
                };
                if !pause_live(&session, wait).await {
                    return;
                }
                continue;
            }
            backoff = next_backoff(backoff, false);
            if !pause_live(&session, jitter(backoff)).await {
                return;
            }
            continue;
        }
        match ws_session(&session).await {
            WsEnd::Cancelled => return,
            WsEnd::Revoked => return revoke(&session),
            // The next HTTP round imports a snapshot. A short pause bounds any resync ping-pong.
            WsEnd::Resync => {
                if !pause_live(&session, jitter(Duration::from_secs(1))).await {
                    return;
                }
            }
            WsEnd::Ended { stable, code } => {
                session.set_status(|status| {
                    status.live = false;
                    status.error_code = Some(code);
                });
                session.notify();
                backoff = next_backoff(backoff, stable);
                if !pause_live(&session, jitter(backoff)).await {
                    return;
                }
            }
        }
    }
}

async fn recover_decoder(session: &Arc<Session>, page: u32) -> BridgeResult<()> {
    recover_decoder_with_page_callback(session, page, || {}).await
}

async fn recover_decoder_with_page_callback(
    session: &Arc<Session>,
    page: u32,
    mut after_page: impl FnMut() + Send,
) -> BridgeResult<()> {
    let c = client(session);
    if blocking(move || c.decoder_recovery_pending().map_err(core_error))
        .await?
        .is_none()
    {
        return Ok(());
    }
    session.notify();
    loop {
        if session.cancel.is_cancelled() {
            return Ok(());
        }
        let c = client(session);
        let progress = blocking(move || c.recover_decoder_page(page).map_err(core_error)).await?;
        after_page();
        if progress.is_none() {
            session.notify();
            session.request_work();
            return Ok(());
        }
        session.notify_progress(false);
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod first_snapshot_tests {
    use super::*;

    #[tokio::test]
    async fn recovery_finishes_before_apply() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open_deferred(&fixture, &fixture.binding, &[]));
        crate::tests::unlock(&session, &fixture);
        assert!(matches!(
            session.client.apply_pending(1),
            Err(peppy_client_core::Error::RecoveryPending)
        ));
        recover_decoder(&session, 1).await.unwrap();
        assert!(session.client.decoder_recovery_pending().unwrap().is_none());
        assert!(session.client.apply_pending(1).is_ok());
    }

    #[tokio::test]
    async fn live_loop_recovers_before_network_replay() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open_deferred(&fixture, &fixture.binding, &[]));
        crate::tests::unlock(&session, &fixture);
        session.mark_first_snapshot_served();
        let task = tokio::spawn(live(session.clone()));
        tokio::time::timeout(Duration::from_secs(5), async {
            while session.client.decoder_recovery_pending().unwrap().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        session.cancel.cancel();
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sealed_outbox_upload_continues_during_recovery() {
        let (origin, seen) =
            crate::contacts::tests::mock_server(|method, path, _| match (method, path) {
                ("POST", "/v1/commands") => (200, "{}".into()),
                _ => (404, r#"{"code":"unsupported"}"#.into()),
            })
            .await;
        let fixture = crate::tests::fixture_at(&origin);
        let session = Arc::new(crate::tests::open_deferred(&fixture, &fixture.binding, &[]));
        crate::tests::unlock(&session, &fixture);
        while session.client.recover_decoder_page(1).unwrap().is_some() {}
        session
            .client
            .queue_send(
                peppy_client_core::OutgoingSms {
                    conversation_id: peppy_client_core::ConversationId::new(),
                    recipients: vec!["+15555550101".into()],
                    body: "pending".into(),
                },
                peppy_client_core::GatewayRoute::Carrier {
                    gateway_device_id: peppy_client_core::DeviceId::new(),
                    subscription_id: "sim-1".into(),
                },
            )
            .unwrap();
        let second = peppy_client_core::KeyProfile::new(fixture.profile.vault_id, 2).unwrap();
        let root = peppy_crypto::derive_root_key(crate::tests::PHRASE, &second).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root, second.clone()).unwrap();
        session
            .client
            .unlock(&second, &header, crate::tests::PHRASE)
            .unwrap();
        assert!(session.client.decoder_recovery_pending().unwrap().is_some());
        assert!(!session.client.pending_outbox().unwrap().is_empty());
        crate::attachment_retention::negotiate(&session)
            .await
            .unwrap();
        let result = work_round(&session).await;
        assert!(matches!(result, Err(error) if error.code == "update-in-progress"));
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.path == "/v1/commands"));
        assert!(session.client.pending_outbox().unwrap().is_empty());
        assert!(session.client.decoder_recovery_pending().unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coded_outbox_rejection_leaves_envelope_queued_after_one_round() {
        let (origin, seen) =
            crate::contacts::tests::mock_server(|method, path, _| match (method, path) {
                ("POST", "/v1/commands") => (400, r#"{"code":"invalid_command"}"#.into()),
                _ => (404, r#"{"code":"unsupported"}"#.into()),
            })
            .await;
        let fixture = crate::tests::fixture_at(&origin);
        let session = Arc::new(crate::tests::open_deferred(&fixture, &fixture.binding, &[]));
        crate::tests::unlock(&session, &fixture);
        while session.client.recover_decoder_page(1).unwrap().is_some() {}
        session
            .client
            .queue_send(
                peppy_client_core::OutgoingSms {
                    conversation_id: peppy_client_core::ConversationId::new(),
                    recipients: vec!["+15555550101".into()],
                    body: "pending".into(),
                },
                peppy_client_core::GatewayRoute::Carrier {
                    gateway_device_id: peppy_client_core::DeviceId::new(),
                    subscription_id: "sim-1".into(),
                },
            )
            .unwrap();
        let before = session.client.pending_outbox().unwrap();
        assert_eq!(before.len(), 1);
        crate::attachment_retention::negotiate(&session)
            .await
            .unwrap();

        work_round(&session).await.unwrap();

        // Pins current behaviour of review finding D-29; flip this assertion when the fix is authorized.
        let after = session.client.pending_outbox().unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].envelope_id, before[0].envelope_id);
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|request| request.method == "POST" && request.path == "/v1/commands")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_recovery_resumes_in_reopened_session() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let second = peppy_client_core::KeyProfile::new(fixture.profile.vault_id, 2).unwrap();
        let root = peppy_crypto::derive_root_key(crate::tests::PHRASE, &second).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root, second.clone()).unwrap();
        {
            let session = Arc::new(crate::tests::open_deferred(&fixture, &fixture.binding, &[]));
            crate::tests::unlock(&session, &fixture);
            session
                .client
                .unlock(&second, &header, crate::tests::PHRASE)
                .unwrap();
            let mut calls = 0;
            recover_decoder_with_page_callback(&session, 1, || {
                calls += 1;
                session.cancel.cancel();
            })
            .await
            .unwrap();
            assert_eq!(calls, 1);
            assert!(session.cancel.is_cancelled());
            assert!(session.client.decoder_recovery_pending().unwrap().is_some());
        }
        let session = Arc::new(crate::tests::open_deferred(&fixture, &fixture.binding, &[]));
        crate::tests::unlock(&session, &fixture);
        session
            .client
            .unlock(&second, &header, crate::tests::PHRASE)
            .unwrap();
        recover_decoder(&session, 1).await.unwrap();
        assert!(session.client.decoder_recovery_pending().unwrap().is_none());
    }

    fn session() -> Arc<Session> {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]))
    }

    /// Sealed records of an owner's book (state and contact `c1`), then the deleter's fence over that
    /// book and its member for `c1`. The fixture's event key seals them, so they apply once unlocked.
    fn owed_deletion_envelopes(
        fixture: &crate::tests::Fixture,
    ) -> Vec<peppy_client_core::Envelope> {
        use peppy_client_core::{
            DeviceId, Envelope, EnvelopeId, EnvelopePurpose, PrivatePayload, SourceSequence,
            VaultId,
        };
        use peppy_crypto::{derive_purpose_key, derive_root_key, encrypt, KeyPurpose};
        use serde_json::json;

        let root = derive_root_key(crate::tests::PHRASE, &fixture.profile).unwrap();
        let key = derive_purpose_key(&root, &fixture.profile, KeyPurpose::Event).unwrap();
        let owner = DeviceId::new();
        let deleter = DeviceId::new();
        let book = json!({"id": "book", "owner_device_id": owner.to_string(),
            "generation": "1", "state": "active", "region": "US"});
        let contact = json!({"id": "c1", "book_id": "book", "revision": "1",
            "display_name": "c1", "name": {"given": "c1"}, "phones": [],
            "emails": [], "addresses": []});
        let payloads = [
            (
                owner,
                1,
                PrivatePayload::ContactBookState { book: book.clone() },
            ),
            (owner, 2, PrivatePayload::ContactUpserted { book, contact }),
            (
                deleter,
                1,
                PrivatePayload::ContactBookDeleted {
                    book_id: "book".into(),
                    owner_device_id: owner.to_string(),
                    generation: "1".into(),
                    deleted_at_ms: 1000,
                },
            ),
        ];
        payloads
            .into_iter()
            .map(|(producer, sequence, payload)| {
                let mut envelope = Envelope {
                    protocol_version: 1,
                    envelope_id: EnvelopeId::new(),
                    command_id: None,
                    vault_id: VaultId(uuid::Uuid::parse_str(&fixture.binding.vault_id).unwrap()),
                    producer_device_id: producer,
                    producer_sequence: SourceSequence(sequence),
                    key_epoch: 1,
                    crypto_suite: fixture.profile.crypto_suite,
                    profile_fingerprint: fixture.profile.fingerprint().unwrap(),
                    purpose: EnvelopePurpose::Event,
                    route: None,
                    compaction: None,
                    origin_tag: None,
                    ciphertext: Vec::new(),
                };
                let encrypted = encrypt(
                    &key,
                    &envelope.aad_bytes().unwrap(),
                    &serde_json::to_vec(&payload).unwrap(),
                )
                .unwrap();
                envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
                envelope
            })
            .collect()
    }

    #[tokio::test]
    async fn owed_deletion_wakes_replay_until_head_is_observed() {
        use peppy_client_core::Cursor;

        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let client = &session.client;
        client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        for (index, envelope) in owed_deletion_envelopes(&fixture).iter().enumerate() {
            client.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
            client.apply_pending(100).unwrap();
            if index == 1 {
                client.observe_replay_head(Cursor(2)).unwrap();
            }
        }
        assert!(client.completion_needs_replay_head().unwrap());
        wake_replay_for_owed_deletions(&session).await.unwrap();
        assert!(session.replay_wake.notified().now_or_never().is_some());
        assert!(session.replay_wake.notified().now_or_never().is_none());
        client.observe_replay_head(Cursor(3)).unwrap();
        client.apply_pending(100).unwrap();
        assert!(!client.completion_needs_replay_head().unwrap());
        wake_replay_for_owed_deletions(&session).await.unwrap();
        assert!(session.replay_wake.notified().now_or_never().is_none());
    }

    #[tokio::test]
    async fn owed_deletion_wakes_replay_after_unlock_without_a_socket_frame() {
        use peppy_client_core::Cursor;

        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let client = &session.client;
        // Locked: the records are journaled, the fence cannot apply, and nothing is owed or asked.
        for (index, envelope) in owed_deletion_envelopes(&fixture).iter().enumerate() {
            client.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
        }
        drain_apply(&session, APPLY_STEPS).await.unwrap();
        wake_replay_for_owed_deletions(&session).await.unwrap();
        assert!(session.replay_wake.notified().now_or_never().is_none());

        // Unlock, then the worker's deferred pass applies the fence. No socket frame follows.
        client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        apply_pass_and_wake_owed_deletions(&session).await.unwrap();
        assert!(session.replay_wake.notified().now_or_never().is_some());
        assert!(session.replay_wake.notified().now_or_never().is_none());

        // The replay that the wake starts reports the head, and the owed member is then published.
        let queued = client.pending_outbox_count().unwrap();
        client
            .observe_replay_head(client.receive_cursor().unwrap())
            .unwrap();
        drain_apply(&session, APPLY_STEPS).await.unwrap();
        assert_eq!(client.pending_outbox_count().unwrap(), queued + 1);
        assert!(!client.completion_needs_replay_head().unwrap());
    }

    #[tokio::test]
    async fn owed_deletion_asks_once_across_repeated_socket_batches_without_a_head() {
        use peppy_client_core::Cursor;

        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let client = &session.client;
        client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        for (index, envelope) in owed_deletion_envelopes(&fixture).iter().enumerate() {
            client.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
        }
        client.apply_pending(100).unwrap();

        // Each socket batch repeats the same unresolved need while no head arrives.
        for _ in 0..5 {
            wake_replay_for_owed_deletions(&session).await.unwrap();
        }

        assert!(session.replay_wake.notified().now_or_never().is_some());
        assert!(session.replay_wake.notified().now_or_never().is_none());
    }

    #[test]
    fn replay_head_asks_back_off_for_an_unchanged_need() {
        let need = CompletionReplayNeed {
            replay_head: Some(4),
            gate: 9,
        };
        let start = Instant::now();
        let mut asks = ReplayHeadAsks::default();
        assert!(asks.admit(Some(need), start));
        assert!(!asks.admit(Some(need), start + Duration::from_millis(999)));
        assert!(asks.admit(Some(need), start + Duration::from_secs(1)));
        assert!(!asks.admit(Some(need), start + Duration::from_millis(2999)));
        assert!(asks.admit(Some(need), start + Duration::from_secs(3)));

        // Socket batches once a second for ten minutes ask only a handful of times.
        let mut asks = ReplayHeadAsks::default();
        let admitted = (0..600)
            .filter(|second| asks.admit(Some(need), start + Duration::from_secs(*second)))
            .count();
        assert!((2..=16).contains(&admitted), "admitted {admitted} requests");
    }

    #[test]
    fn replay_head_asks_follow_a_changed_or_satisfied_need() {
        let start = Instant::now();
        let low = CompletionReplayNeed {
            replay_head: Some(4),
            gate: 9,
        };
        let newer_head = CompletionReplayNeed {
            replay_head: Some(7),
            gate: 9,
        };
        let higher_gate = CompletionReplayNeed {
            replay_head: Some(7),
            gate: 12,
        };
        let mut asks = ReplayHeadAsks::default();
        assert!(asks.admit(Some(low), start));
        assert!(!asks.admit(Some(low), start));
        assert!(
            asks.admit(Some(newer_head), start),
            "a newer head is a new need"
        );
        assert!(!asks.admit(Some(newer_head), start));
        assert!(
            asks.admit(Some(higher_gate), start),
            "a new gate is a new need"
        );
        assert!(
            !asks.admit(None, start),
            "a satisfied need asks for nothing"
        );
        assert!(
            asks.admit(Some(low), start),
            "satisfaction resets the backoff"
        );
    }

    #[tokio::test]
    async fn drain_apply_finishes_pending_conversation_merge() {
        use peppy_client_core::{ConversationId, IncomingSms};
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        session
            .client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        let first = ConversationId::new();
        let second = ConversationId::new();
        for index in 0..40 {
            session
                .client
                .capture_incoming(IncomingSms {
                    conversation_id: Some(if index % 2 == 0 { first } else { second }),
                    sender_address: "+14155552671".into(),
                    body: format!("message {index}"),
                    provider_message_id: Some(format!("merge-{index}")),
                    imported: false,
                })
                .unwrap();
        }
        assert!(drain_apply(&session, 100).await.unwrap());
        let conversations = session.client.list_conversations().unwrap();
        assert_eq!(conversations.len(), 1);
        let survivor = conversations[0].conversation_id;
        assert_eq!(
            session
                .client
                .messages_page(survivor, None, 100)
                .unwrap()
                .messages
                .len(),
            40
        );
        let source = if survivor == first { second } else { first };
        assert_eq!(
            session.client.conversation_redirect(source).unwrap(),
            Some(survivor)
        );
    }

    #[tokio::test]
    async fn first_snapshot_served_returns_immediately() {
        let session = session();
        session.mark_first_snapshot_served();
        tokio::time::timeout(
            Duration::from_millis(100),
            session.first_snapshot_or_timeout(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn first_snapshot_timeout_returns_without_a_window() {
        let session = session();
        let start = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(12), session.first_snapshot_or_timeout())
            .await
            .unwrap();
        assert!(start.elapsed() >= Duration::from_secs(10));
    }

    #[tokio::test]
    async fn first_snapshot_cancel_allows_workers_to_drain() {
        let session = session();
        session.track_worker(tokio::spawn(worker(session.clone())));
        session.track_worker(tokio::spawn(live(session.clone())));
        tokio::task::yield_now().await;
        assert!(drain(&session, Duration::from_secs(1)).await);
    }
}

#[cfg(test)]
mod transport_status_tests {
    use super::*;
    use crate::{
        contacts::tests::mock_server,
        credentials::{parse_credential, tests::credential_json, tests::TOKEN},
        secure_store::MemoryStore,
        tests::noop,
        tests::{fixture_at, open},
        AppState,
    };
    use serde_json::json;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    #[test]
    fn live_batch_decision_preserves_cursor_order_and_boundaries() {
        assert_eq!(
            live_batch_decision(
                1,
                8,
                Some("4"),
                LiveBatchItem::Event {
                    cursor: "5",
                    envelope_bytes: 8,
                },
            ),
            LiveBatchDecision::Append
        );
        assert_eq!(
            live_batch_decision(
                1,
                8,
                Some("5"),
                LiveBatchItem::Event {
                    cursor: "5",
                    envelope_bytes: 8,
                },
            ),
            LiveBatchDecision::Flush
        );
        assert_eq!(
            live_batch_decision(
                WS_BATCH_FRAMES,
                8,
                Some("5"),
                LiveBatchItem::Event {
                    cursor: "6",
                    envelope_bytes: 8,
                },
            ),
            LiveBatchDecision::Flush
        );
        assert_eq!(
            live_batch_decision(
                1,
                WS_BATCH_BYTES - 1,
                Some("5"),
                LiveBatchItem::Event {
                    cursor: "6",
                    envelope_bytes: 1,
                },
            ),
            LiveBatchDecision::Flush
        );
        assert_eq!(
            live_batch_decision(1, 8, Some("5"), LiveBatchItem::Other),
            LiveBatchDecision::Flush
        );
    }

    #[tokio::test]
    async fn deferred_frames_prioritize_cancellation_and_wakes() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let repair_wake = tokio::sync::Notify::new();
        let replay_wake = tokio::sync::Notify::new();
        let mut liveness = tokio::time::interval(Duration::from_secs(3600));
        liveness.tick().await;

        cancel.cancel();
        assert_eq!(
            deferred_frame_control(&cancel, &repair_wake, &replay_wake, &mut liveness, None,),
            DeferredFrameControl::Cancelled
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        repair_wake.notify_one();
        assert_eq!(
            deferred_frame_control(&cancel, &repair_wake, &replay_wake, &mut liveness, None,),
            DeferredFrameControl::Resync
        );

        replay_wake.notify_one();
        assert_eq!(
            deferred_frame_control(&cancel, &repair_wake, &replay_wake, &mut liveness, None,),
            DeferredFrameControl::Resync
        );
    }

    #[tokio::test]
    async fn replay_catch_up_fetches_past_one_round_until_a_short_page() {
        let requests = Arc::new(AtomicUsize::new(0));
        let route_requests = requests.clone();
        let (origin, _) = mock_server(move |method, path, _| match (method, path) {
            ("GET", path) if path.starts_with("/v1/events?") => {
                let page = route_requests.fetch_add(1, Ordering::SeqCst);
                let count = if page < MAX_REPLAY_PAGES_PER_ROUND {
                    100
                } else {
                    1
                };
                let first = page * 100 + 1;
                let events = (first..first + count)
                    .map(|cursor| json!({"cursor": cursor.to_string(), "envelope": "event"}))
                    .collect::<Vec<_>>();
                (200, json!({"events": events}).to_string())
            }
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        replay_until_caught_up(&session).await.unwrap();

        assert_eq!(
            requests.load(Ordering::SeqCst),
            MAX_REPLAY_PAGES_PER_ROUND + 1
        );
        assert_eq!(
            session.client.receive_cursor().unwrap(),
            Cursor((MAX_REPLAY_PAGES_PER_ROUND * 100 + 1) as u64)
        );
    }

    #[tokio::test]
    async fn replay_catch_up_stops_when_a_full_round_does_not_advance_the_cursor() {
        let requests = Arc::new(AtomicUsize::new(0));
        let route_requests = requests.clone();
        let events = (1..=100)
            .map(|cursor| json!({"cursor": cursor.to_string(), "envelope": "event"}))
            .collect::<Vec<_>>();
        let response = json!({"events": events}).to_string();
        let (origin, _) = mock_server(move |method, path, _| match (method, path) {
            ("GET", path) if path.starts_with("/v1/events?") => {
                route_requests.fetch_add(1, Ordering::SeqCst);
                (200, response.clone())
            }
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        for cursor in 1..=100 {
            session
                .client
                .ingest_raw(br#""event""#, Cursor(cursor))
                .unwrap();
        }

        replay_until_caught_up(&session).await.unwrap();

        assert_eq!(
            requests.load(Ordering::SeqCst),
            MAX_REPLAY_PAGES_PER_ROUND,
            "a non-advancing round must not start another replay round"
        );
        assert_eq!(session.client.receive_cursor().unwrap(), Cursor(100));
    }

    #[tokio::test]
    async fn empty_first_page_after_restart_records_the_server_head_for_sync_currency() {
        let (origin, _) = mock_server(move |method, path, _| match (method, path) {
            ("GET", path) if path.starts_with("/v1/events?") => (
                200,
                json!({"events": [], "high_water_cursor": "0", "next_after": null}).to_string(),
            ),
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        // Restart: a fresh session over the same store has no in-memory replay cut.
        drop(open(&fixture, &fixture.binding, &[]));
        let restarted = Arc::new(open(&fixture, &fixture.binding, &[]));
        restarted.set_status(|status| status.live = true);
        assert!(!restarted.sync_current().unwrap());

        replay_until_caught_up(&restarted).await.unwrap();

        assert!(restarted.sync_current().unwrap());
    }

    #[tokio::test]
    async fn replay_page_in_flight_keeps_sync_not_current() {
        let (origin, _) = mock_server(move |method, path, _| match (method, path) {
            ("GET", path) if path.starts_with("/v1/events?") => (
                200,
                json!({"events": [], "high_water_cursor": "0", "next_after": null}).to_string(),
            ),
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        session.set_status(|status| status.live = true);
        replay_until_caught_up(&session).await.unwrap();
        assert!(session.sync_current().unwrap());

        let in_flight = session.replay_page_in_flight();
        assert!(!session.sync_current().unwrap());
        drop(in_flight);

        assert!(session.sync_current().unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn draft_save_wake_debounces_many_saves() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let wake = session.work_wake.notified();

        for _ in 0..3 {
            schedule_draft_push(session.clone(), Duration::from_millis(10));
        }

        tokio::time::timeout(Duration::from_millis(100), wake)
            .await
            .expect("the latest save wakes draft sync");
        assert!(
            tokio::time::timeout(Duration::from_millis(25), session.work_wake.notified())
                .await
                .is_err(),
            "cancelled debounce timers must not add a second wake"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replaced_session_cancels_its_draft_save_wake() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        schedule_draft_push(session.clone(), Duration::from_millis(10));
        session.cancel.cancel();

        assert!(
            tokio::time::timeout(Duration::from_millis(50), session.work_wake.notified())
                .await
                .is_err(),
            "a cancelled session must never receive its delayed wake"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drafts_changed_frame_wakes_draft_sync_but_unknown_frame_does_not() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let mut presence_refresh_due = None;

        assert!(handle_live_hint(
            &session,
            "drafts_changed",
            &mut presence_refresh_due
        ));
        tokio::time::timeout(Duration::from_millis(25), session.work_wake.notified())
            .await
            .expect("drafts_changed wakes draft sync");

        assert!(!handle_live_hint(
            &session,
            "unknown_frame",
            &mut presence_refresh_due
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), session.work_wake.notified())
                .await
                .is_err(),
            "unknown frames remain ignored"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anchored_import_refuses_a_stale_server_profile_before_unlock() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            root.path().to_path_buf(),
            Arc::new(MemoryStore::default()),
            noop(),
        );
        let vault = uuid::Uuid::new_v4();
        let device = uuid::Uuid::new_v4();
        let profile = peppy_client_core::KeyProfile::new(vault, 1).unwrap();
        let key_root = peppy_crypto::derive_root_key(crate::tests::PHRASE, &profile).unwrap();
        let header = peppy_crypto::create_vault_check_header(&key_root, profile.clone()).unwrap();
        let origin = "http://127.0.0.1:9";
        let anchored = serde_json::json!({
            "version": 2,
            "origin": origin,
            "vaultId": vault.to_string(),
            "deviceId": device.to_string(),
            "deviceToken": TOKEN,
            "epochFloor": 2,
            "profileFingerprint": "a".repeat(64),
        })
        .to_string();
        let (credential, anchor) =
            crate::credentials::parse_anchored_import(anchored.as_bytes()).unwrap();
        crate::activate_anchored_import(&state, Arc::new(credential), anchor)
            .await
            .unwrap();
        let session = state.session().await.unwrap().unwrap();
        let error = crate::unlock_with(
            &state,
            &session,
            profile.clone(),
            header.clone(),
            zeroize::Zeroizing::new(crate::tests::PHRASE.to_owned()),
            "a".repeat(64),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "profile-mismatch");

        // Control: the same unlock succeeds when the import carries no anchor.
        let control_root = tempfile::tempdir().unwrap();
        let control = AppState::new(
            control_root.path().to_path_buf(),
            Arc::new(MemoryStore::default()),
            noop(),
        );
        let plain = Arc::new(
            parse_credential(&credential_json(
                origin,
                &vault.to_string(),
                &device.to_string(),
                TOKEN,
            ))
            .unwrap(),
        );
        crate::activate_import(&control, plain).await.unwrap();
        let control_session = control.session().await.unwrap().unwrap();
        crate::unlock_with(
            &control,
            &control_session,
            profile,
            header,
            zeroize::Zeroizing::new(crate::tests::PHRASE.to_owned()),
            "a".repeat(64),
        )
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn transport_revocation_does_not_reopen_the_same_binding() {
        let root = tempfile::tempdir().unwrap();
        let mut state = AppState::new(
            root.path().to_path_buf(),
            Arc::new(MemoryStore::default()),
            noop(),
        );
        let origin = "http://127.0.0.1:9";
        let credential = Arc::new(
            parse_credential(&credential_json(
                origin,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                TOKEN,
            ))
            .unwrap(),
        );
        crate::activate_import(&state, credential).await.unwrap();
        let session = state.session().await.unwrap().unwrap();
        let _ = session
            .snapshot(
                None,
                None,
                crate::head(),
                Some(origin.to_owned()),
                crate::notifications::NotificationPreferences::default(),
            )
            .unwrap();

        revoke(&session);
        assert!(session.is_revoked());
        assert_eq!(
            state.config().unwrap().active_binding(),
            Some(&session.binding),
        );
        assert!(state
            .session
            .lock()
            .await
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &session)));
        state.app_locked.store(false, Ordering::Release);

        assert_eq!(
            crate::finalize_snapshot(&state, &session).await.unwrap(),
            crate::SnapshotFinalization::Revoked,
        );
        let snapshot = crate::revoked_snapshot(Some(origin.to_owned()));
        assert_eq!(snapshot.connection.state, "error");
        assert_eq!(snapshot.connection.error_code, Some("revoked"));
        assert_eq!(snapshot.encryption.state, "locked");
        assert!(snapshot.gateways.is_empty());
        assert!(snapshot.conversations.is_empty());
        assert!(snapshot.notifications.is_empty());
        assert!(snapshot.draft.is_none());
        assert!(snapshot.contact_resolution.is_none());
        assert!(snapshot.contact_books.is_none());
        state.app_locked.store(true, Ordering::Release);
        assert_eq!(
            crate::finalize_snapshot(&state, &session).await.unwrap(),
            crate::SnapshotFinalization::Locked,
        );
        state.app_locked.store(false, Ordering::Release);

        let error = match state.session().await {
            Err(error) => error,
            Ok(_) => panic!("a transport-revoked binding must not reopen"),
        };
        assert_eq!(error.code, "revoked");
        state.config_path = root.path().to_path_buf();
        assert_eq!(
            crate::finalize_snapshot(&state, &session)
                .await
                .unwrap_err()
                .code,
            "host-config",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn same_binding_replacement_retries_after_a_revoked_session_drain_timeout() {
        let root = tempfile::tempdir().unwrap();
        let mut state = AppState::new(
            root.path().to_path_buf(),
            Arc::new(MemoryStore::default()),
            noop(),
        );
        let shutdown_timeout = state.session_drain_timeout;
        state.session_drain_timeout = Duration::from_millis(10);
        let origin = "http://127.0.0.1:9";
        let credential = Arc::new(
            parse_credential(&credential_json(
                origin,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                TOKEN,
            ))
            .unwrap(),
        );
        crate::activate_import(&state, credential.clone())
            .await
            .unwrap();
        let old = state.session().await.unwrap().unwrap();
        revoke(&old);
        assert!(drain(&old, shutdown_timeout).await);
        let (release, held) = tokio::sync::oneshot::channel();
        old.retain_workers(vec![tokio::spawn(async move {
            let _ = held.await;
        })]);

        let error = crate::activate_import(&state, credential.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code, "session-draining");
        let retained = state.session.lock().await.clone().unwrap();
        assert!(Arc::ptr_eq(&retained, &old));
        assert!(retained.is_revoked());
        state.app_locked.store(false, Ordering::Release);
        assert_eq!(
            crate::finalize_snapshot(&state, &old).await.unwrap(),
            crate::SnapshotFinalization::Revoked,
        );
        let snapshot = crate::revoked_snapshot(Some(origin.to_owned()));
        assert!(snapshot.conversations.is_empty());
        assert!(snapshot.draft.is_none());

        release.send(()).unwrap();
        assert!(drain(&old, shutdown_timeout).await);
        crate::activate_import(&state, credential).await.unwrap();
        let replacement = state.session().await.unwrap().unwrap();
        assert!(!Arc::ptr_eq(&replacement, &old));
        assert!(!replacement.is_revoked());
        assert_eq!(
            crate::finalize_snapshot(&state, &old).await.unwrap(),
            crate::SnapshotFinalization::Empty,
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finalization_retains_the_installed_slot_while_config_authority_is_held() {
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new(
            root.path().to_path_buf(),
            Arc::new(MemoryStore::default()),
            noop(),
        ));
        let origin = "http://127.0.0.1:9";
        let credential = Arc::new(
            parse_credential(&credential_json(
                origin,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                TOKEN,
            ))
            .unwrap(),
        );
        crate::activate_import(&state, credential.clone())
            .await
            .unwrap();
        let session = state.session().await.unwrap().unwrap();
        session.set_status(|status| status.revoked = true);
        state.app_locked.store(false, Ordering::Release);

        let (acquired_tx, acquired_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let blocking_state = state.clone();
        let blocker = std::thread::spawn(move || {
            let _guard = blocking_state.config_lock.lock().unwrap();
            acquired_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        tokio::task::spawn_blocking(move || acquired_rx.recv().unwrap())
            .await
            .unwrap();

        let finalizing = tokio::spawn({
            let state = state.clone();
            let session = session.clone();
            async move { crate::finalize_snapshot(&state, &session).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state.session.try_lock().is_err() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("finalization did not retain the installed session slot");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), state.session.lock())
                .await
                .is_err()
        );

        release_tx.send(()).unwrap();
        tokio::task::spawn_blocking(move || blocker.join().unwrap())
            .await
            .unwrap();
        assert_eq!(
            finalizing.await.unwrap().unwrap(),
            crate::SnapshotFinalization::Revoked,
        );

        crate::activate_import(&state, credential).await.unwrap();
        assert!(!Arc::ptr_eq(
            &state.session().await.unwrap().unwrap(),
            &session,
        ));
    }

    #[test]
    fn presence_hints_coalesce_without_dropping_the_first_refresh() {
        let now = Instant::now();
        let mut due = None;
        schedule_presence_refresh(&mut due, now);
        let first = due.expect("the first presence hint schedules a refresh");
        schedule_presence_refresh(&mut due, now + Duration::from_millis(10));
        assert_eq!(due, Some(first));
        assert_eq!(first.duration_since(now), PRESENCE_REFRESH_DELAY);
    }

    #[test]
    fn gateway_statuses_map_to_server_unavailable_only_in_sync() {
        for status in [502, 503, 504] {
            assert_eq!(
                sync_net_error(NetError::Status { status, code: None }).code,
                "server-unavailable"
            );
        }
        assert_eq!(
            sync_net_error(NetError::Status {
                status: 500,
                code: None,
            })
            .code,
            "server-rejected"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn replay_resync_during_held_capability_negotiation_completes_the_current_open() {
        let (entered_sender, entered) = std::sync::mpsc::channel();
        let (release_sender, release) = std::sync::mpsc::channel();
        let release = Arc::new(std::sync::Mutex::new(Some(release)));
        let events = Arc::new(AtomicUsize::new(0));
        let capabilities = Arc::new(AtomicUsize::new(0));
        let capability_bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let route_release = release.clone();
        let route_events = events.clone();
        let route_capabilities = capabilities.clone();
        let route_capability_bodies = capability_bodies.clone();
        let (origin, _) = mock_server(move |method, path, body| match (method, path) {
            ("PUT", "/v1/devices/self/retention-capability") => {
                route_capability_bodies.lock().unwrap().push(body.to_owned());
                if route_capabilities.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered_sender.send(()).unwrap();
                    route_release.lock().unwrap().take().unwrap().recv().unwrap();
                }
                (204, String::new())
            }
            ("GET", "/v1/snapshot") => (200, serde_json::json!({"high_water_cursor":"0","record_count":"0","compaction_supported":false,"compaction_active":false}).to_string()),
            ("GET", path) if path.starts_with("/v1/events") => {
                if route_events.fetch_add(1, Ordering::SeqCst) == 0 {
                    (409, serde_json::json!({"code":"resync_required"}).to_string())
                } else {
                    (200, serde_json::json!({"events":[]}).to_string())
                }
            }
            _ => (404, "{}".into()),
        }).await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let negotiating = tokio::spawn({
            let session = session.clone();
            async move { crate::attachment_retention::negotiate(&session).await }
        });
        // Deadlock guards only: the waits cover HTTP round trips and blocking
        // storage work, which can take seconds on a loaded CI runner.
        const DEADLOCK_GUARD: Duration = Duration::from_secs(10);
        tokio::time::timeout(DEADLOCK_GUARD, async {
            while entered.try_recv().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capability negotiation never reached the held request");

        let replaying = tokio::spawn({
            let session = session.clone();
            async move { replay(&session).await }
        });
        tokio::time::timeout(DEADLOCK_GUARD, async {
            while session
                .attachment_retention_epoch
                .load(std::sync::atomic::Ordering::Acquire)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replay never advanced the attachment retention epoch");
        release_sender.send(()).unwrap();
        replaying.await.unwrap().unwrap();
        negotiating.await.unwrap().unwrap();
        crate::attachment_retention::reconcile_after_replay(&session, &session.generation)
            .await
            .unwrap();

        assert!(session
            .attachment_retention_negotiated
            .load(std::sync::atomic::Ordering::Acquire));
        assert!(session
            .client
            .attachment_retention_protocol_enabled()
            .unwrap());
        assert!(session
            .attachment_retention_replay_observed
            .load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            session
                .attachment_retention_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );
        assert!(session
            .attachment_retention_ready_advertised
            .load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(capabilities.load(Ordering::Acquire), 2);
        assert!(capability_bodies.lock().unwrap()[0].contains("cleanup_ready\":false"));
        assert!(capability_bodies.lock().unwrap()[1].contains("cleanup_ready\":true"));
    }

    #[tokio::test]
    async fn missing_history_attempt_does_not_abort_a_healthy_observation() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let healthy = session
            .client
            .start_history_search(uuid::Uuid::new_v4())
            .unwrap()
            .attempt_id
            .unwrap();
        let missing = uuid::Uuid::new_v4();
        session.attach_history_interest(missing, "missing");
        session.attach_history_interest(healthy, "healthy");

        observe_history_progress(&session).await.unwrap();

        assert!(session.history_attempt_token(missing).is_none());
        assert!(session.history_attempt_token(healthy).is_some());
        assert_eq!(
            session
                .client
                .history_search_status(Some(healthy))
                .unwrap()
                .attempt_id,
            Some(healthy)
        );
    }

    #[test]
    fn replay_page_preserves_next_after_presence() {
        let missing: EventsPage = serde_json::from_str(r#"{"events":[]}"#).unwrap();
        let terminal: EventsPage =
            serde_json::from_str(r#"{"events":[],"next_after":null}"#).unwrap();
        let more: EventsPage = serde_json::from_str(r#"{"events":[],"next_after":"42"}"#).unwrap();

        assert!(matches!(missing.next_after, OptionalField::Missing));
        assert!(matches!(terminal.next_after, OptionalField::Present(None)));
        assert!(
            matches!(more.next_after, OptionalField::Present(Some(ref cursor)) if cursor == "42")
        );
    }

    #[tokio::test]
    async fn replay_page_conflict_rolls_back_its_prefix_before_apply() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        session.client.ingest_raw(b"old", Cursor(2)).unwrap();
        let records = vec![
            RawRecord {
                cursor: "1".into(),
                envelope: serde_json::from_str(r#""new""#).unwrap(),
            },
            RawRecord {
                cursor: "2".into(),
                envelope: serde_json::from_str(r#""different""#).unwrap(),
            },
        ];

        let error = ingest_page(&session, records).await.unwrap_err();

        assert_eq!(error.code, "sync-conflict");
        assert_eq!(session.client.receive_cursor().unwrap(), Cursor(0));
        assert_eq!(session.client.quarantined().unwrap().len(), 1);
        assert_eq!(
            session
                .client
                .apply_pending(MAX_APPLY_BATCH)
                .unwrap()
                .applied,
            0
        );
    }
}

/// Starts the worker and live tasks on Tauri's async runtime; both stop when the session's
/// cancellation token fires (session replaced, origin changed, credential revoked, app exit).
// Keep photo-transport regressions beside that path; shared MIME helpers follow below.
#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod contact_photo_tests {
    use super::*;
    use crate::contacts::tests::mock_server;
    use crate::tests::{fixture_at, open};
    use serde_json::{json, Value};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reclaim_fetches_proof_inputs_before_deleting() {
        let (origin, seen) = mock_server(|method, path, _| match (method, path) {
            ("GET", "/v1/snapshot") => (200, json!({"high_water_cursor": "80", "record_count": "0", "compaction_supported": true, "compaction_generation": "3"}).to_string()),
            ("GET", "/v1/events?after=0&limit=1") => (200, json!({"high_water_cursor": "80", "replay_floor_cursor": "50", "next_after": null, "events": []}).to_string()),
            ("DELETE", _) => (409, json!({"code": "attachment_release_not_proven"}).to_string()),
            _ => (404, "{}".into()),
        }).await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let remote = uuid::Uuid::new_v4().to_string();
        let reclaims = vec![PhotoReclaim {
            attachment_id: uuid::Uuid::new_v4().to_string(),
            remote_object_id: remote.clone(),
            release_after_cursor: Some("40".into()),
        }];

        reclaim_contact_photos(&session, &reclaims).await.unwrap();

        // Pins current behaviour of review finding D-31; flip this assertion when the fix is authorized.
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .map(|request| (request.method.clone(), request.path.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("GET".into(), "/v1/snapshot".into()),
                ("GET".into(), "/v1/events?after=0&limit=1".into()),
                ("DELETE".into(), format!("/v1/attachments/{remote}")),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reclaims_use_fresh_generation_and_floor_only_once_the_floor_covers_references() {
        let (origin, seen) = mock_server(|method, path, _| match (method, path) {
            ("GET", "/v1/snapshot") => (200, json!({"high_water_cursor": "80", "record_count": "0", "compaction_supported": true, "compaction_generation": "3"}).to_string()),
            ("GET", p) if p.starts_with("/v1/events") => (200, json!({"high_water_cursor": "80", "replay_floor_cursor": "50", "next_after": null, "events": []}).to_string()),
            ("DELETE", _) => (409, json!({"code": "attachment_release_not_proven"}).to_string()),
            _ => (404, "{}".into()),
        })
        .await;
        let f = fixture_at(&origin);
        let session = Arc::new(open(&f, &f.binding, &[]));
        let covered = uuid::Uuid::new_v4().to_string();
        let uncovered = uuid::Uuid::new_v4().to_string();
        let reclaims = vec![
            PhotoReclaim {
                attachment_id: uuid::Uuid::new_v4().to_string(),
                remote_object_id: covered.clone(),
                release_after_cursor: Some("40".into()),
            },
            PhotoReclaim {
                attachment_id: uuid::Uuid::new_v4().to_string(),
                remote_object_id: uncovered.clone(),
                release_after_cursor: Some("60".into()),
            },
            PhotoReclaim {
                attachment_id: uuid::Uuid::new_v4().to_string(),
                remote_object_id: uuid::Uuid::new_v4().to_string(),
                release_after_cursor: None,
            },
        ];
        reclaim_contact_photos(&session, &reclaims).await.unwrap();
        let seen = seen.lock().unwrap().clone();
        let deletes = seen
            .iter()
            .filter(|s| s.method == "DELETE")
            .collect::<Vec<_>>();
        assert_eq!(
            deletes.len(),
            1,
            "only the candidate whose references the floor covers"
        );
        assert_eq!(deletes[0].path, format!("/v1/attachments/{covered}"));
        let body: Value = serde_json::from_str(&deletes[0].body).unwrap();
        assert_eq!(
            body,
            json!({"compaction_generation": "3", "release_before_cursor": "50"})
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reclaims_wait_when_the_server_cannot_prove_release() {
        let (origin, seen) = mock_server(|method, path, _| match (method, path) {
            ("GET", "/v1/snapshot") => (200, json!({"high_water_cursor": "80", "record_count": "0", "compaction_supported": false}).to_string()),
            _ => (404, "{}".into()),
        })
        .await;
        let f = fixture_at(&origin);
        let session = Arc::new(open(&f, &f.binding, &[]));
        let reclaims = vec![PhotoReclaim {
            attachment_id: uuid::Uuid::new_v4().to_string(),
            remote_object_id: uuid::Uuid::new_v4().to_string(),
            release_after_cursor: Some("1".into()),
        }];
        reclaim_contact_photos(&session, &reclaims).await.unwrap();
        assert!(seen.lock().unwrap().iter().all(|s| s.method != "DELETE"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn storage_policy_reclaim_tombstones_the_local_id_and_completes_without_retry() {
        let remote = uuid::Uuid::new_v4().to_string();
        let route_remote = remote.clone();
        let (origin, seen) = mock_server(move |method, path, _| match (method, path) {
            ("GET", "/v1/snapshot") => (
                200,
                json!({
                    "high_water_cursor": "80",
                    "record_count": "0",
                    "compaction_supported": true,
                    "compaction_generation": "3",
                })
                .to_string(),
            ),
            ("GET", path) if path.starts_with("/v1/events") => (
                200,
                json!({"replay_floor_cursor": "50", "events": []}).to_string(),
            ),
            ("DELETE", path) => {
                assert_eq!(path, format!("/v1/attachments/{route_remote}"));
                (
                    410,
                    json!({"code": "attachment_removed_by_storage_policy"}).to_string(),
                )
            }
            _ => (404, "{}".into()),
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let source = fixture.dir.path().join("reclaimed-local.txt");
        std::fs::write(&source, b"verified local bytes").unwrap();
        let local = session
            .client
            .prepare_attachment(&source, "text/plain", "reclaimed-local.txt")
            .unwrap()
            .attachment_id;
        let reclaims = vec![PhotoReclaim {
            attachment_id: local.to_string(),
            remote_object_id: remote,
            release_after_cursor: Some("40".into()),
        }];

        reclaim_contact_photos(&session, &reclaims).await.unwrap();

        let state = session.client.attachment_retention_state(local).unwrap();
        assert_eq!(
            state.remote,
            peppy_client_core::RemoteAttachmentState::RemovedByStoragePolicy
        );
        assert!(state.local_available);
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|request| request.method == "DELETE")
                .count(),
            1,
            "a typed storage-policy response is terminal rather than retried"
        );
    }
}

#[cfg(test)]
mod download_scheduling_tests {
    use super::*;

    #[test]
    fn only_exact_typed_gone_is_storage_policy_removal() {
        let removed = NetError::Status {
            status: 410,
            code: Some("attachment_removed_by_storage_policy".into()),
        };
        let generic_gone = NetError::Status {
            status: 410,
            code: None,
        };
        let missing = NetError::Status {
            status: 404,
            code: Some("attachment_removed_by_storage_policy".into()),
        };

        assert!(is_storage_policy_removal(&removed));
        assert!(!is_storage_policy_removal(&generic_gone));
        assert!(!is_storage_policy_removal(&missing));
    }

    #[test]
    fn only_structured_attachment_unavailable_is_recoverable() {
        let unavailable = Failure::Net(NetError::Status {
            status: 409,
            code: Some("attachment_unavailable".into()),
        });
        let missing = Failure::Net(NetError::Status {
            status: 404,
            code: Some("attachment_not_found".into()),
        });

        assert!(is_recoverable_download_unavailable(&unavailable));
        assert!(!is_recoverable_download_unavailable(&missing));
        assert_eq!(classify(&missing), MediaOutcome::Permanent);
    }

    #[tokio::test]
    async fn recording_policy_removal_preserves_a_verified_local_copy() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let source = fixture.dir.path().join("retained-local.png");
        std::fs::write(&source, b"verified local bytes").unwrap();
        let prepared = session.prepare_attachment(&source).unwrap();
        let attachment_id = prepared.id.parse().unwrap();

        assert!(record_storage_policy_removal(&session, attachment_id)
            .await
            .is_ok());

        let state = session
            .client
            .attachment_retention_state(attachment_id)
            .unwrap();
        assert_eq!(
            state.remote,
            peppy_client_core::RemoteAttachmentState::RemovedByStoragePolicy
        );
        assert!(state.local_available);
        assert!(source.exists());
    }

    #[test]
    fn unavailable_backoff_is_bounded_and_success_clears_session_state() {
        let id: peppy_client_core::AttachmentId = uuid::Uuid::new_v4().to_string().parse().unwrap();
        let mut retries = HashMap::new();

        for _ in 1..MAX_DOWNLOAD_UNAVAILABLE_ATTEMPTS {
            assert!(advance_download_retry(&mut retries, id));
        }
        assert!(
            retries.get(&id).unwrap().retry_at > Instant::now(),
            "a recoverable unavailable response delays the next work round"
        );
        assert!(
            !advance_download_retry(&mut retries, id),
            "the maximum unavailable failure falls through to permanent handling"
        );
        assert!(retries.is_empty());

        assert!(advance_download_retry(&mut retries, id));
        retries.remove(&id);
        assert!(
            retries.is_empty(),
            "a successful download clears its retry state"
        );
    }

    #[test]
    fn download_selection_reserves_eager_capacity_and_reuses_empty_slots() {
        let selected = select_downloads(vec![
            ("demand-1", true),
            ("demand-2", true),
            ("demand-3", true),
            ("demand-4", true),
            ("eager-1", false),
            ("eager-2", false),
        ]);
        assert_eq!(
            selected,
            vec!["demand-1", "demand-2", "demand-3", "eager-1"]
        );
        assert_eq!(
            select_downloads(vec![
                ("demand-1", true),
                ("eager-1", false),
                ("eager-2", false),
                ("eager-3", false),
                ("eager-4", false),
            ]),
            vec!["demand-1", "eager-1", "eager-2", "eager-3"],
            "unused demand capacity is available to eager work"
        );
    }

    #[test]
    fn filtered_failures_and_backoff_do_not_consume_download_slots() {
        let retrying: peppy_client_core::AttachmentId =
            uuid::Uuid::new_v4().to_string().parse().unwrap();
        let mut retries = HashMap::new();
        assert!(advance_download_retry(&mut retries, retrying));
        assert!(retries.get(&retrying).unwrap().retry_at > Instant::now());

        let selected = select_downloads(
            vec![
                ("permanent", true, false),
                ("backing-off", true, false),
                ("last-row-demand", true, true),
                ("eager-1", false, true),
                ("eager-2", false, true),
                ("eager-3", false, true),
            ]
            .into_iter()
            .filter(|(_, _, eligible)| *eligible)
            .map(|(id, demanded, _)| (id, demanded))
            .collect(),
        );

        assert_eq!(
            selected,
            vec!["last-row-demand", "eager-1", "eager-2", "eager-3"]
        );
    }
}

pub fn start(session: &Arc<Session>) {
    Session::register_started(session);
    let opening_session = session.clone();
    session.track_worker(tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            match crate::attachment_retention::negotiate(&opening_session).await {
                Ok(()) => return,
                Err(error) if error.code == "revoked" => return revoke(&opening_session),
                Err(_) => {}
            }
            let retry = jitter(backoff);
            backoff = (backoff * 2).min(MAX_BACKOFF);
            tokio::select! {
                _ = opening_session.cancel.cancelled() => return,
                _ = sleep(retry) => {}
            }
        }
    }));
    session.track_worker(tokio::spawn(worker(session.clone())));
    session.track_worker(tokio::spawn(live(session.clone())));
    crate::live_typing::start(session.clone());
}

async fn join_workers(workers: &mut [tokio::task::JoinHandle<()>], duration: Duration) -> bool {
    timeout(duration, async {
        for worker in workers {
            let _ = worker.await;
        }
    })
    .await
    .is_ok()
}

/// Cancels the session supervisor and waits for all tracked tasks to finish. Timed-out handles
/// remain retained for a later drain; they are never aborted because their blocking work may
/// still own durable state.
pub async fn drain(session: &Arc<Session>, duration: Duration) -> bool {
    session.cancel.cancel();
    let mut workers = session.take_workers();
    let drained = join_workers(&mut workers, duration).await;
    if !drained {
        session.retain_workers(
            workers
                .into_iter()
                .filter(|worker| !worker.is_finished())
                .collect(),
        );
    }
    drained
}

/// Cancels and drains every started session, including retired sessions retained by their
/// supervisor tasks. The deadline covers the entire process scope without holding a session lock.
pub async fn drain_all(duration: Duration) -> bool {
    let sessions = Session::active_and_retired();
    for session in &sessions {
        session.cancel.cancel();
    }
    let mut workers = sessions
        .iter()
        .map(|session| (session, session.take_workers()))
        .collect::<Vec<_>>();
    let drained = timeout(duration, async {
        for (_, handles) in &mut workers {
            for handle in handles {
                let _ = handle.await;
            }
        }
    })
    .await
    .is_ok();
    if !drained {
        for (session, handles) in workers {
            session.retain_workers(
                handles
                    .into_iter()
                    .filter(|handle| !handle.is_finished())
                    .collect(),
            );
        }
    }
    drained
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    use crate::tests::{fixture_at, open};

    #[tokio::test]
    async fn timeout_does_not_abort_a_worker() {
        let (release, released) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            released.await.unwrap();
        });
        let mut workers = vec![worker];
        assert!(!join_workers(&mut workers, Duration::from_millis(1)).await);
        assert!(!workers[0].is_finished());
        release.send(()).unwrap();
        assert!(join_workers(&mut workers, Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn cancelling_a_session_waits_for_admitted_blocking_core_work_to_drain() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::sync_channel(1);
        session.track_worker(tokio::spawn(async move {
            let _ = blocking(move || {
                entered.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok::<(), BridgeError>(())
            })
            .await;
        }));

        entered_rx.await.unwrap();
        assert!(!drain(&session, Duration::from_millis(1)).await);
        release.send(()).unwrap();
        assert!(drain(&session, Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn drain_all_retains_timed_out_retired_session_workers() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        Session::register_started(&session);
        let worker_session = session.clone();
        let (release, released) = tokio::sync::oneshot::channel();
        session.track_worker(tokio::spawn(async move {
            let _session = worker_session;
            released.await.unwrap();
        }));
        drop(session);

        assert!(!drain_all(Duration::from_millis(1)).await);
        release.send(()).unwrap();
        assert!(drain_all(Duration::from_secs(1)).await);
    }

    #[test]
    fn gateway_refresh_keeps_only_authenticated_non_revoked_roster_members() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = open(&fixture, &fixture.binding, &[]);
        let active_peer = uuid::Uuid::new_v4().to_string();
        let revoked_peer = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({"devices": [
            {"device_id": fixture.binding.device_id, "role": "device", "revoked": false},
            {"device_id": active_peer, "role": "gateway", "revoked": false},
            {"device_id": revoked_peer, "role": "device", "revoked": true}
        ]}))
        .unwrap();
        let capabilities: CapabilitiesResponse =
            serde_json::from_value(serde_json::json!({"capabilities": []})).unwrap();

        apply_gateway_refresh(&session, &devices, &capabilities);

        let status = session.status.lock().unwrap();
        assert!(status.rcs_roster_validated);
        assert_eq!(
            status.rcs_live_roster,
            vec![fixture.binding.device_id, active_peer]
        );
    }
}

#[cfg(test)]
mod snapshot_too_large_gate_tests {
    use super::*;
    use crate::tests::{fixture_at, open};

    const SIX_HOURS: Duration = Duration::from_secs(6 * 60 * 60);

    #[test]
    fn first_attempt_at_start_is_not_blocked() {
        let gate = SnapshotTooLargeGate::default();
        assert_eq!(gate.wait_before_attempt(Instant::now()), Duration::ZERO);
    }

    #[test]
    fn too_large_begin_waits_six_hours_without_an_immediate_retry() {
        let start = Instant::now();
        let mut gate = SnapshotTooLargeGate::default();
        gate.record_too_large(start);
        assert_eq!(gate.wait_before_attempt(start), SIX_HOURS);
        assert_eq!(
            gate.wait_before_attempt(start + Duration::from_secs(60 * 60)),
            SIX_HOURS - Duration::from_secs(60 * 60)
        );
        assert_eq!(
            gate.wait_before_attempt(start + SIX_HOURS - Duration::from_millis(1)),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn retry_is_allowed_once_six_hours_have_passed() {
        let start = Instant::now();
        let mut gate = SnapshotTooLargeGate::default();
        gate.record_too_large(start);
        assert_eq!(gate.wait_before_attempt(start + SIX_HOURS), Duration::ZERO);
        assert_eq!(
            gate.wait_before_attempt(start + SIX_HOURS + Duration::from_secs(1)),
            Duration::ZERO
        );
    }

    #[test]
    fn completed_round_clears_the_block() {
        let start = Instant::now();
        let mut gate = SnapshotTooLargeGate::default();
        gate.record_too_large(start);
        gate.clear();
        assert_eq!(gate, SnapshotTooLargeGate::default());
        assert_eq!(gate.wait_before_attempt(start), Duration::ZERO);
    }

    #[test]
    fn a_new_too_large_begin_after_clear_starts_a_fresh_window() {
        let start = Instant::now();
        let mut gate = SnapshotTooLargeGate::default();
        gate.record_too_large(start);
        gate.clear();
        let later = start + Duration::from_secs(60 * 60);
        gate.record_too_large(later);
        assert_eq!(gate.wait_before_attempt(later), SIX_HOURS);
        // The fresh window runs from `later`, so the original six hours have no effect.
        assert_eq!(
            gate.wait_before_attempt(start + SIX_HOURS),
            Duration::from_secs(60 * 60)
        );
        assert_eq!(gate.wait_before_attempt(later + SIX_HOURS), Duration::ZERO);
    }

    #[test]
    fn a_refused_retry_restarts_the_window_from_the_refusal() {
        let start = Instant::now();
        let mut gate = SnapshotTooLargeGate::default();
        gate.record_too_large(start);
        let retry = start + SIX_HOURS;
        assert_eq!(gate.wait_before_attempt(retry), Duration::ZERO);
        gate.record_too_large(retry);
        assert_eq!(gate.wait_before_attempt(retry), SIX_HOURS);
        assert!(gate.is_marked());
    }

    #[test]
    fn refusal_stays_reported_until_a_begin_succeeds() {
        let start = Instant::now();
        let mut gate = SnapshotTooLargeGate::default();
        gate.record_too_large(start);
        assert!(gate.is_marked());
        assert!(gate.wait_before_attempt(start + SIX_HOURS).is_zero());
        assert!(
            gate.is_marked(),
            "an expired window alone must not clear the report"
        );
    }

    fn too_large_session() -> Arc<Session> {
        let f = fixture_at("http://127.0.0.1:1");
        Arc::new(open(&f, &f.binding, &[]))
    }

    /// An unlocked session, for calls that need the core database (contact-repair latch). The fixture
    /// owns the database directory, so the caller keeps it alive for the whole test.
    fn unlocked_too_large_session() -> (crate::tests::Fixture, Arc<Session>) {
        let f = fixture_at("http://127.0.0.1:1");
        let session = Arc::new(open(&f, &f.binding, &[]));
        session
            .client
            .unlock(&f.profile, &f.header, crate::tests::PHRASE)
            .unwrap();
        (f, session)
    }

    #[test]
    fn connection_view_reports_a_refused_import_as_connected_with_its_code() {
        let session = too_large_session();
        session.set_status(|status| status.live = true);
        let before = session
            .snapshot(None, None, crate::head(), None, Default::default())
            .unwrap()
            .0;
        assert_eq!(before.connection.state, "connected");
        assert_eq!(before.connection.error_code, None);

        snapshot_gate(&session).record_too_large(Instant::now());
        let during = session
            .snapshot(None, None, crate::head(), None, Default::default())
            .unwrap()
            .0;
        assert_eq!(during.connection.state, "connected");
        assert_eq!(during.connection.error_code, Some("snapshot-too-large"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refused_window_defers_the_optional_repair_without_failing_the_live_round() {
        let (_fixture, session) = unlocked_too_large_session();
        session.client.request_contact_repair().unwrap();
        snapshot_gate(&session).record_too_large(Instant::now());
        // The repair is only deferred: the round that calls it must not fail, so replay, drafts and
        // outbox keep running, and the live task does not sleep out the window.
        repair_if_required(&session).await.unwrap();
        assert!(!session
            .repair_attempted
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refused_window_stops_snapshot_resync_before_any_network_or_core_work() {
        let session = too_large_session();
        snapshot_gate(&session).record_too_large(Instant::now());
        let error = snapshot_resync(&session).await.unwrap_err();
        assert_eq!(error.code, SNAPSHOT_TOO_LARGE_CODE);
    }

    #[test]
    fn refused_contact_repair_is_released_so_it_can_run_after_the_window() {
        let session = too_large_session();
        session
            .repair_attempted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        settle_repair_attempt(
            &session,
            &Err(BridgeError::new(SNAPSHOT_TOO_LARGE_CODE, "too large")),
        );
        assert!(!session
            .repair_attempted
            .load(std::sync::atomic::Ordering::SeqCst));

        session
            .repair_attempted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        settle_repair_attempt(&session, &Ok(()));
        assert!(session
            .repair_attempted
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn failed_repair_attempt_is_spent_until_rearmed() {
        let session = too_large_session();
        session
            .repair_attempted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        settle_repair_attempt(&session, &Err(BridgeError::new("offline", "offline")));
        assert!(session.repair_exhausted());
        session
            .repair_attempted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!session.repair_exhausted());
    }

    #[test]
    fn refused_repair_remains_retryable() {
        let session = too_large_session();
        session
            .repair_attempted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        settle_repair_attempt(
            &session,
            &Err(BridgeError::new(SNAPSHOT_TOO_LARGE_CODE, "too large")),
        );
        assert!(!session.repair_exhausted());
        assert!(!session
            .repair_attempted
            .load(std::sync::atomic::Ordering::SeqCst));
        session
            .repair_attempted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        settle_repair_attempt(
            &session,
            &Err(core_error(peppy_client_core::Error::RecoveryPending)),
        );
        assert!(!session.repair_exhausted());
        assert!(!session
            .repair_attempted
            .load(std::sync::atomic::Ordering::SeqCst));
    }
}
