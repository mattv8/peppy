use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use peppy_client_core::MessagePayload;
use peppy_hosted_api::mcp::{
    WebhookCreated, WebhookDeliveryState, WebhookEvent, WebhookFilterMode, WebhookInput,
    WebhookList, WebhookSecret, WebhookTestResult, WebhookView,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::store::{self, MAX_QUEUE, MAX_WEBHOOKS, StoredWebhook, WebhookState};
use crate::{
    config::{WorkerConfig, canonical_public_api_url},
    custody::Custody,
    runtime::{Runtime, RuntimeState},
    webhooks::{
        egress,
        filter::{FilterEntryRef, FilterMode, matches},
        name::derive_name,
        payload::{
            AttachmentMeta, delivery_id, message_payload, random_delivery_id, rfc3339_seconds,
            test_payload,
        },
        signing::generate_secret,
    },
};

const RETRIES_MS: [i64; 5] = [5_000, 60_000, 600_000, 3_600_000, 21_600_000];
/// Upper bound for one delivery attempt or test send, DNS included.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for save-time DNS validation of a webhook URL.
const SAVE_RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Deliveries attempted per dispatch pass.
const MAX_DISPATCH_PER_PASS: usize = 20;
/// Inbound messages older than this are never delivered (no late replays).
const MAX_MESSAGE_AGE_MS: i64 = 86_400_000;
/// Messages sent this long before a webhook was created still count as new.
const BACKLOG_GRACE_MS: i64 = 300_000;
/// Receiver response bodies are ignored; at most this much is drained before the connection drops.
const MAX_RESPONSE_DRAIN_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct WebhookEngine {
    custody: Arc<Custody>,
    config: WorkerConfig,
    states: Arc<Mutex<HashMap<Uuid, Arc<Mutex<WebhookState>>>>>,
    #[cfg(test)]
    harness: TestHarness,
}

/// Test-only seams: an injectable clock and a loopback-permitting resolver.
#[cfg(test)]
#[derive(Clone, Default)]
struct TestHarness {
    now: Option<Arc<std::sync::atomic::AtomicI64>>,
    loopback: bool,
}

/// A vault state bound to the device that looked it up. Every lock re-checks the
/// binding, so an operation that resumes after revocation or rebinding cannot read
/// or write another binding's state.
#[derive(Clone)]
struct Handle {
    inner: Arc<Mutex<WebhookState>>,
    device: Uuid,
}

impl Handle {
    async fn lock(&self) -> Result<tokio::sync::MutexGuard<'_, WebhookState>, WebhookError> {
        let guard = self.inner.lock().await;
        if guard.revoked || guard.device_id != self.device {
            return Err(WebhookError::Missing);
        }
        Ok(guard)
    }
}

#[derive(Debug)]
pub enum WebhookError {
    Code(&'static str),
    Missing,
}

impl WebhookEngine {
    pub fn new(custody: Arc<Custody>, config: WorkerConfig) -> Self {
        Self {
            custody,
            config,
            states: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            harness: TestHarness::default(),
        }
    }
    #[cfg(test)]
    fn new_for_test(
        custody: Arc<Custody>,
        config: WorkerConfig,
        now: Arc<std::sync::atomic::AtomicI64>,
    ) -> Self {
        Self {
            harness: TestHarness {
                now: Some(now),
                loopback: true,
            },
            ..Self::new(custody, config)
        }
    }
    #[cfg(not(test))]
    fn now(&self) -> i64 {
        now_ms()
    }
    #[cfg(test)]
    fn now(&self) -> i64 {
        self.harness
            .now
            .as_ref()
            .map_or_else(now_ms, |now| now.load(std::sync::atomic::Ordering::SeqCst))
    }
    #[cfg(not(test))]
    fn permit_loopback(&self) -> bool {
        false
    }
    #[cfg(test)]
    fn permit_loopback(&self) -> bool {
        self.harness.loopback
    }
    /// Binds a cached vault state to the device that asked for it.
    ///
    /// Rules: a live handle serves only its own device; a request from any other
    /// device is stale (`Missing`) and never receives or persists through it. A
    /// tombstone left by `remove_vault` serves nobody, and only a different device
    /// may replace it with a fresh load.
    async fn state(&self, vault: Uuid, device: Uuid) -> Result<Handle, WebhookError> {
        let mut states = self.states.lock().await;
        if let Some(inner) = states.get(&vault) {
            let (cached_device, revoked) = {
                let locked = inner.lock().await;
                (locked.device_id, locked.revoked)
            };
            if cached_device == device {
                return if revoked {
                    Err(WebhookError::Missing)
                } else {
                    Ok(Handle {
                        inner: inner.clone(),
                        device,
                    })
                };
            }
            if !revoked {
                return Err(WebhookError::Missing);
            }
        }
        let state = store::load(&self.custody, vault, device)
            .map_err(|_| WebhookError::Code("mcp_unavailable"))?;
        let inner = Arc::new(Mutex::new(state));
        states.insert(vault, inner.clone());
        Ok(Handle { inner, device })
    }
    /// Single persistence choke point. A revoked handle never writes.
    fn save(&self, vault: Uuid, state: &WebhookState) -> Result<(), WebhookError> {
        if state.revoked {
            return Err(WebhookError::Missing);
        }
        store::save(&self.custody, vault, state).map_err(|_| WebhookError::Code("mcp_unavailable"))
    }
    /// Revokes the vault's webhook state: fences any cached handle under its mutex
    /// (waiting for in-flight operations to finish), clears it, and deletes the record.
    pub async fn remove_vault(&self, vault: Uuid, device: Uuid) -> Result<(), WebhookError> {
        let mut states = self.states.lock().await;
        let handle = states
            .entry(vault)
            .or_insert_with(|| Arc::new(Mutex::new(WebhookState::new(device))))
            .clone();
        let mut state = handle.lock().await;
        state.revoked = true;
        state.device_id = device;
        state.webhooks.clear();
        state.queue.clear();
        state.recent.clear();
        state.cursor = 0;
        state.cursor_initialized = false;
        store::remove(&self.custody, vault).map_err(|_| WebhookError::Code("mcp_unavailable"))
    }

    pub async fn list(&self, vault: Uuid, device: Uuid) -> Result<WebhookList, WebhookError> {
        let state = self.state(vault, device).await?;
        let state = state.lock().await?;
        Ok(WebhookList {
            available: true,
            webhooks: state
                .webhooks
                .iter()
                .map(|hook| view(hook, &state))
                .collect(),
        })
    }
    pub async fn create(
        &self,
        runtime: &Runtime,
        input: WebhookInput,
    ) -> Result<WebhookCreated, WebhookError> {
        validate(&input, &self.config).await?;
        let vault = runtime.vault_id().0;
        let state = self.state(vault, runtime.device_id().0).await?;
        let mut state = state.lock().await?;
        if state.webhooks.len() >= MAX_WEBHOOKS {
            return Err(WebhookError::Code("webhook_limit"));
        }
        if !state.cursor_initialized {
            state.cursor = runtime
                .client()
                .latest_message_local_order()
                .map_err(|_| WebhookError::Code("mcp_unavailable"))?;
            state.cursor_initialized = true;
        }
        let secret = generate_secret();
        let hook = StoredWebhook {
            id: Uuid::new_v4(),
            name: normalized_name(input.name),
            url: input.url,
            events: input.events,
            filter_mode: input.filter_mode,
            filter: input.filter,
            paused: input.paused,
            secret: secret.clone(),
            created_at_ms: self.now(),
            last_delivery_state: WebhookDeliveryState::Never,
            last_delivery_at_ms: None,
            last_status_code: None,
            last_error: None,
        };
        let result = WebhookCreated {
            webhook: view(&hook, &state),
            secret,
        };
        state.webhooks.push(hook);
        self.save(vault, &state)?;
        Ok(result)
    }
    pub async fn update(
        &self,
        runtime: &Runtime,
        id: Uuid,
        input: WebhookInput,
    ) -> Result<WebhookView, WebhookError> {
        validate(&input, &self.config).await?;
        let vault = runtime.vault_id().0;
        let state = self.state(vault, runtime.device_id().0).await?;
        let mut state = state.lock().await?;
        let hook = state
            .webhooks
            .iter_mut()
            .find(|hook| hook.id == id)
            .ok_or(WebhookError::Missing)?;
        hook.name = normalized_name(input.name);
        hook.url = input.url;
        hook.events = input.events;
        hook.filter_mode = input.filter_mode;
        hook.filter = input.filter;
        hook.paused = input.paused;
        let snapshot = StoredWebhook {
            id: hook.id,
            name: hook.name.clone(),
            url: hook.url.clone(),
            events: hook.events.clone(),
            filter_mode: hook.filter_mode.clone(),
            filter: hook.filter.clone(),
            paused: hook.paused,
            secret: String::new(),
            created_at_ms: hook.created_at_ms,
            last_delivery_state: hook.last_delivery_state.clone(),
            last_delivery_at_ms: hook.last_delivery_at_ms,
            last_status_code: hook.last_status_code,
            last_error: hook.last_error.clone(),
        };
        let result = view(&snapshot, &state);
        self.save(vault, &state)?;
        Ok(result)
    }
    pub async fn delete(&self, runtime: &Runtime, id: Uuid) -> Result<(), WebhookError> {
        let vault = runtime.vault_id().0;
        let state = self.state(vault, runtime.device_id().0).await?;
        let mut state = state.lock().await?;
        let before = state.webhooks.len();
        state.webhooks.retain(|hook| hook.id != id);
        if before == state.webhooks.len() {
            return Err(WebhookError::Missing);
        }
        state.queue.retain(|delivery| delivery.webhook_id != id);
        self.save(vault, &state)
    }
    pub async fn reveal(&self, runtime: &Runtime, id: Uuid) -> Result<WebhookSecret, WebhookError> {
        let state = self
            .state(runtime.vault_id().0, runtime.device_id().0)
            .await?;
        let state = state.lock().await?;
        let hook = state
            .webhooks
            .iter()
            .find(|hook| hook.id == id)
            .ok_or(WebhookError::Missing)?;
        Ok(WebhookSecret {
            id,
            secret: hook.secret.clone(),
        })
    }
    pub async fn rotate(&self, runtime: &Runtime, id: Uuid) -> Result<WebhookSecret, WebhookError> {
        let vault = runtime.vault_id().0;
        let state = self.state(vault, runtime.device_id().0).await?;
        let mut state = state.lock().await?;
        let hook = state
            .webhooks
            .iter_mut()
            .find(|hook| hook.id == id)
            .ok_or(WebhookError::Missing)?;
        hook.secret = generate_secret();
        let result = WebhookSecret {
            id,
            secret: hook.secret.clone(),
        };
        self.save(vault, &state)?;
        Ok(result)
    }
    pub async fn test(
        &self,
        runtime: &Runtime,
        id: Uuid,
    ) -> Result<WebhookTestResult, WebhookError> {
        // Snapshot under the mutex, then release it before any DNS or HTTP.
        let (url, secret, source_origin) = {
            let state = self
                .state(runtime.vault_id().0, runtime.device_id().0)
                .await?;
            let locked = state.lock().await?;
            let hook = locked
                .webhooks
                .iter()
                .find(|hook| hook.id == id)
                .ok_or(WebhookError::Missing)?;
            (
                hook.url.clone(),
                hook.secret.clone(),
                self.source_origin(&locked),
            )
        };
        let body = test_payload(id, &source_origin, self.now()).to_string();
        Ok(self
            .send(&url, &secret, random_delivery_id(), body, &source_origin)
            .await)
    }
    pub async fn after_sync(&self, runtime: &Runtime) -> Result<(), WebhookError> {
        if matches!(
            runtime.state().await,
            RuntimeState::Revoked | RuntimeState::NeedsApproval
        ) {
            return Ok(());
        }
        let vault = runtime.vault_id().0;
        let state = self.state(vault, runtime.device_id().0).await?;
        let mut locked = state.lock().await?;
        if !locked.cursor_initialized {
            locked.cursor = runtime
                .client()
                .latest_message_local_order()
                .map_err(|_| WebhookError::Code("mcp_unavailable"))?;
            locked.cursor_initialized = true;
            self.save(vault, &locked)?;
            return Ok(());
        }
        drop(locked);
        let mut contacts = HashMap::new();
        for _ in 0..5 {
            let cursor = state.lock().await?.cursor;
            let messages = runtime
                .client()
                .incoming_messages_after(cursor, 200)
                .map_err(|_| WebhookError::Code("mcp_unavailable"))?;
            if messages.is_empty() {
                break;
            }
            let (hooks, source_origin) = {
                let locked = state.lock().await?;
                (locked.webhooks.clone(), self.source_origin(&locked))
            };
            let now = self.now();
            let mut additions = Vec::new();
            for (order, message) in &messages {
                let sender = message.sender_address.as_deref().unwrap_or_default();
                let contact = contacts.entry(sender.to_owned()).or_insert_with(|| {
                    resolve_contact(runtime, sender, message.source_device_id.0)
                });
                let selected = hooks
                    .iter()
                    .filter(|hook| admits(hook, message, sender, contact.as_ref(), now))
                    .collect::<Vec<_>>();
                if selected.is_empty() {
                    continue;
                }
                let attachments = attachment_metadata(runtime, message);
                for hook in selected {
                    let message_id = message.record.message_id.to_string();
                    additions.push((
                        *order,
                        hook.id,
                        message_id.clone(),
                        super::store::Delivery {
                            delivery_id: delivery_id(hook.id, &message_id),
                            webhook_id: hook.id,
                            body: message_payload(
                                hook.id,
                                &source_origin,
                                message,
                                contact
                                    .as_ref()
                                    .map(|contact| contact.display_name.as_str()),
                                &attachments,
                                now,
                            )
                            .to_string(),
                            attempts: 0,
                            next_attempt_at_ms: now,
                            created_at_ms: now,
                        },
                    ));
                }
            }
            let mut locked = state.lock().await?;
            for (order, webhook_id, message_id, delivery) in additions {
                if locked.recent.contains(&(webhook_id, message_id.clone())) {
                    continue;
                }
                if locked.queue.len() >= MAX_QUEUE
                    && let Some(dropped) = locked.queue.pop_front()
                    && let Some(dropped_hook) = locked
                        .webhooks
                        .iter_mut()
                        .find(|item| item.id == dropped.webhook_id)
                {
                    dropped_hook.last_delivery_state = WebhookDeliveryState::Failed;
                    dropped_hook.last_error = Some("queue_overflow".into());
                }
                locked.queue.push_back(delivery);
                locked.remember(webhook_id, message_id);
                locked.cursor = locked.cursor.max(order);
            }
            if let Some((last, _)) = messages.last() {
                locked.cursor = *last;
            }
            self.save(vault, &locked)?;
            if messages.len() < 200 {
                break;
            }
        }
        Ok(())
    }
    /// Sends due deliveries. The queue item stays persisted until its attempt is
    /// recorded, so a cancelled or crashed attempt is retried rather than lost.
    pub async fn dispatch_due(&self, runtime: &Runtime) -> Result<(), WebhookError> {
        if matches!(
            runtime.state().await,
            RuntimeState::Revoked | RuntimeState::NeedsApproval
        ) {
            return Ok(());
        }
        let vault = runtime.vault_id().0;
        let state = self.state(vault, runtime.device_id().0).await?;
        for _ in 0..MAX_DISPATCH_PER_PASS {
            let Some(due) = self.next_due(&state).await else {
                break;
            };
            let result = self
                .send(
                    &due.url,
                    &due.secret,
                    due.delivery_id.clone(),
                    due.body,
                    &due.source_origin,
                )
                .await;
            self.record_attempt(vault, &state, &due.delivery_id, result)
                .await?;
        }
        Ok(())
    }

    /// Snapshots the first due delivery (any position in the bounded queue) and its
    /// endpoint under the mutex. Items whose webhook no longer exists are dropped.
    async fn next_due(&self, state: &Handle) -> Option<DueDelivery> {
        let mut locked = state.lock().await.ok()?;
        let now = self.now();
        while let Some(index) = due_index(&locked.queue, now) {
            let item = &locked.queue[index];
            let endpoint = locked
                .webhooks
                .iter()
                .find(|hook| hook.id == item.webhook_id)
                .map(|hook| (hook.url.clone(), hook.secret.clone()));
            match endpoint {
                Some((url, secret)) => {
                    let item = &locked.queue[index];
                    return Some(DueDelivery {
                        delivery_id: item.delivery_id.clone(),
                        url,
                        secret,
                        body: item.body.clone(),
                        source_origin: self.source_origin(&locked),
                    });
                }
                None => {
                    locked.queue.remove(index);
                }
            }
        }
        None
    }

    /// Applies one attempt's outcome, locating the item by delivery id. If the item
    /// vanished while the request was in flight (webhook deleted or vault revoked),
    /// the result is discarded.
    async fn record_attempt(
        &self,
        vault: Uuid,
        state: &Handle,
        delivery_id: &str,
        result: WebhookTestResult,
    ) -> Result<(), WebhookError> {
        let mut locked = state.lock().await?;
        let now = self.now();
        let WebhookState {
            webhooks, queue, ..
        } = &mut *locked;
        let Some(position) = queue
            .iter()
            .position(|item| item.delivery_id == delivery_id)
        else {
            return Ok(());
        };
        let mut item = queue.remove(position).ok_or(WebhookError::Missing)?;
        if let Some(hook) = webhooks.iter_mut().find(|hook| hook.id == item.webhook_id) {
            hook.last_delivery_at_ms = Some(now);
            hook.last_status_code = result.status_code;
            hook.last_error = result.error.clone();
            if result.ok {
                hook.last_delivery_state = WebhookDeliveryState::Delivered;
            } else if retryable(result.status_code, result.error.as_deref()) && item.attempts < 5 {
                item.attempts += 1;
                item.next_attempt_at_ms = now + RETRIES_MS[usize::from(item.attempts - 1)];
                hook.last_delivery_state = WebhookDeliveryState::Retrying;
                queue.insert(position, item);
            } else {
                hook.last_delivery_state = WebhookDeliveryState::Failed;
            }
        }
        self.save(vault, &locked)
    }
    pub async fn update_source_origin(
        &self,
        vault: Uuid,
        device: Uuid,
        source_origin: &str,
    ) -> Result<(), WebhookError> {
        let Ok(source_origin) = canonical_public_api_url(source_origin) else {
            return Ok(());
        };
        let state = self.state(vault, device).await?;
        let mut state = state.lock().await?;
        state.source_origin = source_origin;
        self.save(vault, &state)
    }
    fn source_origin(&self, state: &WebhookState) -> String {
        if state.source_origin.is_empty() {
            self.config.public_api_url.clone()
        } else {
            state.source_origin.clone()
        }
    }
    async fn send(
        &self,
        url: &str,
        secret: &str,
        id: String,
        body: String,
        source_origin: &str,
    ) -> WebhookTestResult {
        self.send_within(DELIVERY_TIMEOUT, url, secret, id, body, source_origin)
            .await
    }
    /// Bounds one whole attempt (validation, DNS, HTTP, drain) by `limit`.
    async fn send_within(
        &self,
        limit: Duration,
        url: &str,
        secret: &str,
        id: String,
        body: String,
        source_origin: &str,
    ) -> WebhookTestResult {
        let started = std::time::Instant::now();
        match tokio::time::timeout(
            limit,
            self.send_unbounded(url, secret, id, body, source_origin),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => result(false, None, started, "timeout"),
        }
    }
    async fn send_unbounded(
        &self,
        url: &str,
        secret: &str,
        id: String,
        body: String,
        source_origin: &str,
    ) -> WebhookTestResult {
        let started = std::time::Instant::now();
        let parsed = match self.validate_delivery_url(url) {
            Ok(value) => value,
            Err(error) => return result(false, None, started, error.code()),
        };
        let deny = api_host(&self.config.api_url);
        let addresses = match egress::resolve_checked_with(
            &parsed,
            &deny.into_iter().collect::<Vec<_>>(),
            self.permit_loopback(),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => return result(false, None, started, error.code()),
        };
        let timestamp = self.now() / 1000;
        let signature = match super::signing::sign(secret, &id, timestamp, body.as_bytes()) {
            Ok(value) => value,
            Err(_) => return result(false, None, started, "mcp_unavailable"),
        };
        let Some(host) = parsed.host_str() else {
            return result(false, None, started, "webhook_url_invalid");
        };
        let client = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .user_agent(format!("Peppy-Webhooks/1 (+{source_origin})"))
            .resolve_to_addrs(host, &addresses)
            .build()
        {
            Ok(client) => client,
            Err(_) => return result(false, None, started, "mcp_unavailable"),
        };
        match client
            .post(parsed)
            .header("content-type", "application/json")
            .header("webhook-id", id)
            .header("webhook-timestamp", timestamp)
            .header("webhook-signature", signature)
            .body(body)
            .send()
            .await
        {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let mut drained = 0;
                while drained < MAX_RESPONSE_DRAIN_BYTES {
                    match response.chunk().await {
                        Ok(Some(chunk)) => drained += chunk.len(),
                        _ => break,
                    }
                }
                if (200..300).contains(&status) {
                    result(true, Some(status), started, "")
                } else if (300..400).contains(&status) {
                    result(false, Some(status), started, "redirect_refused")
                } else {
                    result(false, Some(status), started, &format!("http_{status}"))
                }
            }
            Err(_) => result(false, None, started, "timeout"),
        }
    }
    fn validate_delivery_url(&self, raw: &str) -> Result<url::Url, egress::UrlError> {
        #[cfg(test)]
        if self.permit_loopback() {
            return url::Url::parse(raw).map_err(|_| egress::UrlError::Invalid);
        }
        egress::validate_url(raw)
    }
}
fn result(
    ok: bool,
    status: Option<u16>,
    started: std::time::Instant,
    error: &str,
) -> WebhookTestResult {
    WebhookTestResult {
        ok,
        status_code: status,
        duration_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
        error: (!ok).then(|| error.to_owned()),
    }
}
fn api_host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}
/// One queued delivery snapshotted for an attempt, with its endpoint.
struct DueDelivery {
    delivery_id: String,
    url: String,
    secret: String,
    body: String,
    source_origin: String,
}

/// Index of the first queued delivery whose attempt is due, in queue order.
fn due_index(queue: &VecDeque<store::Delivery>, now: i64) -> Option<usize> {
    queue.iter().position(|item| item.next_attempt_at_ms <= now)
}

#[derive(Clone)]
struct ResolvedContact {
    book_id: String,
    contact_id: String,
    display_name: String,
}
impl ResolvedContact {
    fn identity(&self) -> Option<(&str, &str)> {
        Some((&self.book_id, &self.contact_id))
    }
}

fn resolve_contact(
    runtime: &Runtime,
    sender: &str,
    source_device_id: Uuid,
) -> Option<ResolvedContact> {
    if sender.is_empty() {
        return None;
    }
    let input =
        json!({"addresses":[sender],"source_device_id":source_device_id.to_string()}).to_string();
    let value: Value = serde_json::from_str(
        &runtime
            .client()
            .resolve_contact_addresses_json(&input)
            .ok()?,
    )
    .ok()?;
    if value
        .get("more_indexing")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("ambiguous")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(sender)))
    {
        return None;
    }
    let item = value
        .get("matches")?
        .as_array()?
        .iter()
        .find(|item| item.get("address").and_then(Value::as_str) == Some(sender))?;
    Some(ResolvedContact {
        book_id: item.get("book_id")?.as_str()?.to_owned(),
        contact_id: item.get("contact_id")?.as_str()?.to_owned(),
        display_name: item
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

fn attachment_metadata(
    runtime: &Runtime,
    message: &peppy_client_core::MessagePayload,
) -> Vec<AttachmentMeta> {
    message
        .record
        .attachments
        .iter()
        .take(20)
        .map(|reference| {
            let info = runtime
                .client()
                .attachment_info(reference.attachment_id)
                .ok();
            AttachmentMeta {
                id: reference.attachment_id.to_string(),
                content_type: info.as_ref().map(|info| info.media_type.clone()),
                size: info.as_ref().map(|info| info.plaintext_bytes),
            }
        })
        .collect()
}
/// Pure admission rule: whether one stored webhook should receive one inbound message.
fn admits(
    hook: &StoredWebhook,
    message: &MessagePayload,
    sender: &str,
    contact: Option<&ResolvedContact>,
    now: i64,
) -> bool {
    let time = message
        .sent_at_ms
        .or(message.ordered_at_ms)
        .unwrap_or_default();
    if hook.paused
        || message.imported
        || time < now - MAX_MESSAGE_AGE_MS
        || time < hook.created_at_ms - BACKLOG_GRACE_MS
    {
        return false;
    }
    let entries = hook
        .filter
        .iter()
        .map(|entry| FilterEntryRef {
            address: &entry.address,
            contact: entry.book_id.as_deref().zip(entry.contact_id.as_deref()),
        })
        .collect::<Vec<_>>();
    let mode = match hook.filter_mode {
        WebhookFilterMode::All => FilterMode::All,
        WebhookFilterMode::Only => FilterMode::Only,
        WebhookFilterMode::Except => FilterMode::Except,
    };
    matches(
        mode,
        &entries,
        sender,
        contact.and_then(|contact| contact.identity()),
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}
fn normalized_name(name: Option<String>) -> Option<String> {
    name.filter(|name| !name.trim().is_empty())
}
fn retryable(status: Option<u16>, error: Option<&str>) -> bool {
    if error == Some("webhook_url_blocked") {
        return false;
    }
    match status {
        None => true,
        Some(408 | 425 | 429) => true,
        Some(400..=499) => false,
        Some(300..=399) => false,
        _ => true,
    }
}
async fn validate(input: &WebhookInput, config: &WorkerConfig) -> Result<(), WebhookError> {
    if input
        .name
        .as_ref()
        .is_some_and(|name| name.chars().count() > 80 || name.chars().any(char::is_control))
    {
        return Err(WebhookError::Code("webhook_name_invalid"));
    }
    let url = egress::validate_url(&input.url).map_err(|error| WebhookError::Code(error.code()))?;
    if input.events != vec![WebhookEvent::MessageReceived]
        || input.filter.len() > 50
        || matches!(input.filter_mode, WebhookFilterMode::All) && !input.filter.is_empty()
        || !matches!(input.filter_mode, WebhookFilterMode::All) && input.filter.is_empty()
        || input.filter.iter().any(|entry| {
            entry.address.is_empty()
                || entry.address.len() > 64
                || entry.label.as_ref().is_some_and(|value| value.len() > 128)
                || entry
                    .contact_id
                    .as_ref()
                    .is_some_and(|value| value.len() > 128)
                || entry
                    .book_id
                    .as_ref()
                    .is_some_and(|value| value.len() > 128)
        })
    {
        return Err(WebhookError::Code("webhook_filter_invalid"));
    }
    let deny = api_host(&config.api_url).into_iter().collect::<Vec<_>>();
    bounded_resolution(SAVE_RESOLVE_TIMEOUT, egress::resolve_checked(&url, &deny))
        .await
        .map(|_| ())
}

/// Applies a save-time time limit to DNS validation. A stalled lookup rejects the URL.
async fn bounded_resolution<T>(
    limit: Duration,
    resolution: impl std::future::Future<Output = Result<T, egress::UrlError>>,
) -> Result<T, WebhookError> {
    match tokio::time::timeout(limit, resolution).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(egress::UrlError::Blocked)) => Err(WebhookError::Code("webhook_url_blocked")),
        Ok(Err(_)) | Err(_) => Err(WebhookError::Code("webhook_url_invalid")),
    }
}
fn view(hook: &StoredWebhook, state: &WebhookState) -> WebhookView {
    let parsed = url::Url::parse(&hook.url).ok();
    let derived = hook.name.is_none();
    WebhookView {
        id: hook.id,
        name: hook
            .name
            .clone()
            .unwrap_or_else(|| parsed.as_ref().map(derive_name).unwrap_or_default()),
        name_is_derived: derived,
        url: hook.url.clone(),
        events: hook.events.clone(),
        filter_mode: hook.filter_mode.clone(),
        filter: hook.filter.clone(),
        paused: hook.paused,
        created_at: rfc3339_seconds(hook.created_at_ms),
        last_delivery_state: hook.last_delivery_state.clone(),
        last_delivery_at: hook.last_delivery_at_ms.map(rfc3339_seconds),
        last_status_code: hook.last_status_code,
        last_error: hook.last_error.clone(),
        pending_deliveries: state
            .queue
            .iter()
            .filter(|delivery| delivery.webhook_id == hook.id)
            .count() as u32,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicI64, Ordering},
        },
    };

    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{HeaderMap, HeaderValue, StatusCode, header},
        response::{IntoResponse, Response},
        routing::post,
    };
    use peppy_client_core::IncomingMms;
    use peppy_hosted_api::mcp::WebhookFilterEntry;

    use super::*;
    use crate::{
        custody::Custody,
        webhooks::{payload::delivery_id, signing::sign},
    };

    #[derive(Clone)]
    struct Reply {
        status: u16,
        location: Option<&'static str>,
        /// Never answers: the request stays open until the sender gives up.
        hang: bool,
    }

    fn reply(status: u16) -> Reply {
        Reply {
            status,
            location: None,
            hang: false,
        }
    }

    fn hanging() -> Reply {
        Reply {
            hang: true,
            ..reply(200)
        }
    }

    /// One recorded request: headers and the exact body bytes.
    type Recorded = (HeaderMap, Vec<u8>);

    /// Local receiver that records every request and answers with a scripted
    /// status sequence (200 once the script is exhausted).
    #[derive(Clone, Default)]
    struct Receiver {
        replies: Arc<StdMutex<VecDeque<Reply>>>,
        requests: Arc<StdMutex<Vec<Recorded>>>,
    }

    async fn receive(
        State(receiver): State<Receiver>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        receiver
            .requests
            .lock()
            .unwrap()
            .push((headers, body.to_vec()));
        let scripted = receiver
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| reply(200));
        if scripted.hang {
            std::future::pending::<()>().await;
        }
        let mut response = StatusCode::from_u16(scripted.status)
            .unwrap()
            .into_response();
        if let Some(location) = scripted.location {
            response
                .headers_mut()
                .insert(header::LOCATION, HeaderValue::from_static(location));
        }
        response
    }

    struct TestEnv {
        _dir: tempfile::TempDir,
        engine: WebhookEngine,
        runtime: Runtime,
        now: Arc<AtomicI64>,
        receiver: Receiver,
        url: String,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl TestEnv {
        async fn new(replies: Vec<Reply>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
            let config = WorkerConfig {
                api_url: "http://api.test:8080".into(),
                public_api_url: "https://api.test".into(),
                state_dir: dir.path().to_path_buf(),
                key_file: dir.path().join("key"),
                worker_token_file: dir.path().join("token"),
                bind_addr: "127.0.0.1:0".parse().unwrap(),
            };
            let custody = Arc::new(
                Custody::bootstrap(dir.path().join("custody"), dir.path().join("key")).unwrap(),
            );
            let now = Arc::new(AtomicI64::new(now_ms()));
            let engine = WebhookEngine::new_for_test(custody, config, now.clone());
            let runtime = Runtime::test_runtime_for_registry(
                dir.path().join("runtime"),
                "http://127.0.0.1:1".into(),
            );
            let receiver = Receiver {
                replies: Arc::new(StdMutex::new(replies.into())),
                ..Receiver::default()
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let router = Router::new()
                .route("/hook", post(receive))
                .with_state(receiver.clone());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            Self {
                _dir: dir,
                engine,
                runtime,
                now,
                receiver,
                url: format!("http://{address}/hook"),
                server,
            }
        }

        fn vault(&self) -> Uuid {
            self.runtime.vault_id().0
        }

        fn device(&self) -> Uuid {
            self.runtime.device_id().0
        }

        fn now(&self) -> i64 {
            self.now.load(Ordering::SeqCst)
        }

        fn set_now(&self, ms: i64) {
            self.now.store(ms, Ordering::SeqCst);
        }

        async fn with_state<R>(&self, read: impl FnOnce(&mut WebhookState) -> R) -> R {
            let state = self
                .engine
                .state(self.vault(), self.device())
                .await
                .unwrap();
            let mut state = state.lock().await.unwrap();
            read(&mut state)
        }

        async fn add_hook(&self, hook: StoredWebhook) -> StoredWebhook {
            let stored = hook.clone();
            self.with_state(|state| state.webhooks.push(hook)).await;
            stored
        }

        async fn hook(&self, id: Uuid) -> StoredWebhook {
            self.with_state(|state| {
                state
                    .webhooks
                    .iter()
                    .find(|hook| hook.id == id)
                    .unwrap()
                    .clone()
            })
            .await
        }

        async fn queue(&self) -> Vec<store::Delivery> {
            self.with_state(|state| state.queue.iter().cloned().collect())
                .await
        }

        async fn cursor(&self) -> (i64, bool) {
            self.with_state(|state| (state.cursor, state.cursor_initialized))
                .await
        }

        fn capture(&self, sender: &str, provider_id: &str, imported: bool) {
            self.runtime
                .client()
                .capture_incoming_mms(IncomingMms {
                    conversation_id: None,
                    sender_address: sender.into(),
                    body: "hello".into(),
                    recipients: Vec::new(),
                    subject: None,
                    provider_message_id: Some(provider_id.into()),
                    imported,
                    attachment_ids: Vec::new(),
                })
                .unwrap();
        }

        fn latest_message(&self) -> MessagePayload {
            self.runtime
                .client()
                .incoming_messages_after(0, 500)
                .unwrap()
                .pop()
                .unwrap()
                .1
        }

        fn requests(&self) -> Vec<Recorded> {
            self.receiver.requests.lock().unwrap().clone()
        }

        async fn dispatch(&self) {
            self.engine.dispatch_due(&self.runtime).await.unwrap();
        }
    }

    fn hook(url: &str, created_at_ms: i64) -> StoredWebhook {
        StoredWebhook {
            id: Uuid::new_v4(),
            name: None,
            url: url.into(),
            events: vec![WebhookEvent::MessageReceived],
            filter_mode: WebhookFilterMode::All,
            filter: Vec::new(),
            paused: false,
            secret: generate_secret(),
            created_at_ms,
            last_delivery_state: WebhookDeliveryState::Never,
            last_delivery_at_ms: None,
            last_status_code: None,
            last_error: None,
        }
    }

    fn entry(address: &str) -> WebhookFilterEntry {
        WebhookFilterEntry {
            address: address.into(),
            label: None,
            contact_id: None,
            book_id: None,
        }
    }

    fn header_value(headers: &HeaderMap, name: &str) -> String {
        headers.get(name).unwrap().to_str().unwrap().to_owned()
    }

    /// Environment with one armed webhook and one queued delivery for message `m-1`.
    async fn armed(replies: Vec<Reply>) -> (TestEnv, StoredWebhook) {
        let env = TestEnv::new(replies).await;
        env.engine.after_sync(&env.runtime).await.unwrap();
        let stored = env.add_hook(hook(&env.url, env.now())).await;
        env.capture("+15550100001", "m-1", false);
        env.engine.after_sync(&env.runtime).await.unwrap();
        assert_eq!(env.queue().await.len(), 1);
        (env, stored)
    }

    #[test]
    fn retry_classification_is_safe() {
        assert!(retryable(Some(503), None));
        assert!(!retryable(Some(400), None));
        assert!(!retryable(Some(302), None));
    }

    #[tokio::test]
    async fn successful_delivery_is_signed_and_dequeued() {
        let (env, stored) = armed(Vec::new()).await;
        let queued = env.queue().await.remove(0);
        env.dispatch().await;

        let requests = env.requests();
        assert_eq!(requests.len(), 1);
        let (headers, body) = &requests[0];
        assert_eq!(header_value(headers, "content-type"), "application/json");
        assert!(header_value(headers, "user-agent").starts_with("Peppy-Webhooks/1"));
        assert_eq!(header_value(headers, "webhook-id"), queued.delivery_id);
        let timestamp = header_value(headers, "webhook-timestamp");
        assert_eq!(timestamp, (env.now() / 1000).to_string());
        assert_eq!(
            header_value(headers, "webhook-signature"),
            sign(
                &stored.secret,
                &queued.delivery_id,
                timestamp.parse().unwrap(),
                body
            )
            .unwrap()
        );
        assert_eq!(body.as_slice(), queued.body.as_bytes());

        assert!(env.queue().await.is_empty());
        let hook = env.hook(stored.id).await;
        assert_eq!(hook.last_delivery_state, WebhookDeliveryState::Delivered);
        assert_eq!(hook.last_status_code, Some(200));
        assert_eq!(hook.last_error, None);
    }

    #[tokio::test]
    async fn control_origin_is_persisted_and_invalid_values_keep_the_previous_origin() {
        let env = TestEnv::new(Vec::new()).await;

        env.engine
            .update_source_origin(env.vault(), env.device(), "https://app.example.test")
            .await
            .unwrap();
        env.engine
            .update_source_origin(env.vault(), env.device(), "http://example.test")
            .await
            .unwrap();

        assert_eq!(
            env.with_state(|state| state.source_origin.clone()).await,
            "https://app.example.test"
        );
    }

    #[tokio::test]
    async fn server_error_is_retried_with_backoff() {
        let (env, stored) = armed(vec![reply(503), reply(503)]).await;
        let start = env.now();

        env.dispatch().await;
        let queue = env.queue().await;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].attempts, 1);
        assert_eq!(queue[0].next_attempt_at_ms, start + 5_000);
        let hook = env.hook(stored.id).await;
        assert_eq!(hook.last_delivery_state, WebhookDeliveryState::Retrying);
        assert_eq!(hook.last_status_code, Some(503));
        assert_eq!(hook.last_error.as_deref(), Some("http_503"));

        env.dispatch().await;
        assert_eq!(env.requests().len(), 1, "not due yet");

        env.set_now(start + 5_000);
        env.dispatch().await;
        let queue = env.queue().await;
        assert_eq!(env.requests().len(), 2);
        assert_eq!(queue[0].attempts, 2);
        assert_eq!(queue[0].next_attempt_at_ms, start + 5_000 + 60_000);
    }

    #[tokio::test]
    async fn rate_limit_is_retried() {
        let (env, stored) = armed(vec![reply(429)]).await;
        env.dispatch().await;
        assert_eq!(env.queue().await.len(), 1);
        assert_eq!(
            env.hook(stored.id).await.last_delivery_state,
            WebhookDeliveryState::Retrying
        );
    }

    #[tokio::test]
    async fn client_error_is_permanent() {
        let (env, stored) = armed(vec![reply(400)]).await;
        env.dispatch().await;
        assert!(env.queue().await.is_empty());
        let hook = env.hook(stored.id).await;
        assert_eq!(hook.last_delivery_state, WebhookDeliveryState::Failed);
        assert_eq!(hook.last_error.as_deref(), Some("http_400"));

        env.set_now(env.now() + 30 * 86_400_000);
        env.dispatch().await;
        assert_eq!(env.requests().len(), 1);
    }

    #[tokio::test]
    async fn redirects_are_refused_not_followed() {
        let redirect = Reply {
            status: 302,
            location: Some("http://127.0.0.1:9/elsewhere"),
            ..reply(302)
        };
        let (env, stored) = armed(vec![redirect]).await;
        env.dispatch().await;
        assert_eq!(
            env.requests().len(),
            1,
            "redirect target must not be requested"
        );
        assert!(env.queue().await.is_empty());
        let hook = env.hook(stored.id).await;
        assert_eq!(hook.last_delivery_state, WebhookDeliveryState::Failed);
        assert_eq!(hook.last_status_code, Some(302));
        assert_eq!(hook.last_error.as_deref(), Some("redirect_refused"));
    }

    #[tokio::test]
    async fn exhausted_attempts_drop_the_delivery_and_fail_the_webhook() {
        let (env, stored) = armed(vec![reply(503); 6]).await;
        for _ in 0..6 {
            env.dispatch().await;
            if let Some(next) = env
                .queue()
                .await
                .first()
                .map(|item| item.next_attempt_at_ms)
            {
                env.set_now(next);
            }
        }
        assert_eq!(env.requests().len(), 6);
        assert!(env.queue().await.is_empty());
        let hook = env.hook(stored.id).await;
        assert_eq!(hook.last_delivery_state, WebhookDeliveryState::Failed);
        assert_eq!(hook.last_error.as_deref(), Some("http_503"));
    }

    #[tokio::test]
    async fn test_delivery_reports_outcome_without_touching_history() {
        let env = TestEnv::new(vec![reply(503)]).await;
        let stored = env.add_hook(hook(&env.url, env.now())).await;

        let failed = env.engine.test(&env.runtime, stored.id).await.unwrap();
        assert!(!failed.ok);
        assert_eq!(failed.status_code, Some(503));
        assert_eq!(failed.error.as_deref(), Some("http_503"));

        let ok = env.engine.test(&env.runtime, stored.id).await.unwrap();
        assert!(ok.ok);
        assert_eq!(ok.status_code, Some(200));
        assert_eq!(ok.error, None);

        let requests = env.requests();
        assert_eq!(requests.len(), 2);
        assert!(String::from_utf8_lossy(&requests[1].1).contains("webhook.test"));
        let untouched = env.hook(stored.id).await;
        assert_eq!(untouched.last_delivery_state, WebhookDeliveryState::Never);
        assert_eq!(untouched.last_delivery_at_ms, None);
        assert_eq!(untouched.last_status_code, None);
        assert!(env.queue().await.is_empty());
    }

    #[tokio::test]
    async fn duplicate_pass_enqueues_once_and_retries_keep_webhook_id() {
        let (env, stored) = armed(vec![reply(503)]).await;
        let message = env.latest_message();
        env.dispatch().await;

        env.with_state(|state| state.cursor = 0).await;
        env.engine.after_sync(&env.runtime).await.unwrap();
        assert_eq!(
            env.queue().await.len(),
            1,
            "same (webhook, message) is deduped"
        );

        env.set_now(env.queue().await[0].next_attempt_at_ms);
        env.dispatch().await;
        let requests = env.requests();
        assert_eq!(requests.len(), 2);
        let expected = delivery_id(stored.id, &message.record.message_id.to_string());
        assert!(expected.starts_with("msg_"));
        assert_eq!(header_value(&requests[0].0, "webhook-id"), expected);
        assert_eq!(header_value(&requests[1].0, "webhook-id"), expected);
        assert!(env.queue().await.is_empty());
    }

    #[tokio::test]
    async fn first_pass_initializes_cursor_without_backfill() {
        let env = TestEnv::new(Vec::new()).await;
        env.capture("+15550100002", "old-1", false);
        env.add_hook(hook(&env.url, env.now())).await;

        env.engine.after_sync(&env.runtime).await.unwrap();

        let (cursor, initialized) = env.cursor().await;
        assert!(initialized);
        assert_eq!(
            cursor,
            env.runtime.client().latest_message_local_order().unwrap()
        );
        assert!(env.queue().await.is_empty());
        env.dispatch().await;
        assert!(env.requests().is_empty());
    }

    #[tokio::test]
    async fn imported_history_is_never_selected() {
        let env = TestEnv::new(Vec::new()).await;
        let hook = hook(&env.url, env.now());
        env.capture("+15550100003", "imported-1", true);
        let message = env.latest_message();
        assert!(!admits(&hook, &message, "+15550100003", None, env.now()));
        env.capture("+15550100003", "live-1", false);
        let live = env.latest_message();
        assert!(admits(&hook, &live, "+15550100003", None, env.now()));
    }

    #[tokio::test]
    async fn messages_older_than_a_day_are_skipped() {
        let env = TestEnv::new(Vec::new()).await;
        env.capture("+15550100004", "aged-1", false);
        let message = env.latest_message();
        let stale = hook(&env.url, env.now() - 48 * 3_600_000);
        let now = env.now() + 25 * 3_600_000;
        assert!(!admits(&stale, &message, "+15550100004", None, now));
        assert!(admits(&stale, &message, "+15550100004", None, env.now()));
    }

    #[tokio::test]
    async fn messages_before_webhook_creation_minus_grace_are_skipped() {
        let env = TestEnv::new(Vec::new()).await;
        env.capture("+15550100005", "backlog-1", false);
        let message = env.latest_message();
        let late = hook(&env.url, env.now() + 10 * 60_000);
        assert!(!admits(&late, &message, "+15550100005", None, env.now()));
        let grace = hook(&env.url, env.now() + 4 * 60_000);
        assert!(admits(&grace, &message, "+15550100005", None, env.now()));
    }

    #[tokio::test]
    async fn paused_webhooks_receive_nothing() {
        let env = TestEnv::new(Vec::new()).await;
        env.capture("+15550100006", "paused-1", false);
        let message = env.latest_message();
        let mut paused = hook(&env.url, env.now());
        paused.paused = true;
        assert!(!admits(&paused, &message, "+15550100006", None, env.now()));
        paused.paused = false;
        assert!(admits(&paused, &message, "+15550100006", None, env.now()));
    }

    #[tokio::test]
    async fn only_and_except_match_stored_address_without_resolved_contact() {
        let env = TestEnv::new(Vec::new()).await;
        env.capture("+15550100007", "filter-1", false);
        let message = env.latest_message();
        let mut only = hook(&env.url, env.now());
        only.filter_mode = WebhookFilterMode::Only;
        only.filter = vec![entry("5550100007")];
        assert!(admits(&only, &message, "+15550100007", None, env.now()));
        only.filter = vec![entry("+15550100099")];
        assert!(!admits(&only, &message, "+15550100007", None, env.now()));

        let mut except = hook(&env.url, env.now());
        except.filter_mode = WebhookFilterMode::Except;
        except.filter = vec![entry("+15550100007")];
        assert!(!admits(&except, &message, "+15550100007", None, env.now()));
        assert!(admits(&except, &message, "+15550100008", None, env.now()));
    }

    #[tokio::test]
    async fn contact_identity_matches_a_different_number() {
        let env = TestEnv::new(Vec::new()).await;
        env.capture("+15550100555", "contact-1", false);
        let message = env.latest_message();
        let mut hook = hook(&env.url, env.now());
        hook.filter_mode = WebhookFilterMode::Only;
        hook.filter = vec![WebhookFilterEntry {
            address: "+15550100001".into(),
            label: None,
            contact_id: Some("contact".into()),
            book_id: Some("book".into()),
        }];
        let same_contact = ResolvedContact {
            book_id: "book".into(),
            contact_id: "contact".into(),
            display_name: "Jordan Lee".into(),
        };
        let other_contact = ResolvedContact {
            book_id: "book".into(),
            contact_id: "other".into(),
            display_name: "Sam".into(),
        };
        assert!(admits(
            &hook,
            &message,
            "+15550100555",
            Some(&same_contact),
            env.now()
        ));
        assert!(!admits(
            &hook,
            &message,
            "+15550100555",
            Some(&other_contact),
            env.now()
        ));
        assert!(!admits(&hook, &message, "+15550100555", None, env.now()));
    }

    fn pending_item(
        delivery_id: &str,
        webhook_id: Uuid,
        next_attempt_at_ms: i64,
    ) -> store::Delivery {
        store::Delivery {
            delivery_id: delivery_id.into(),
            webhook_id,
            body: "{}".into(),
            attempts: 0,
            next_attempt_at_ms,
            created_at_ms: 0,
        }
    }

    #[tokio::test]
    async fn cancelled_attempt_keeps_the_item_queued() {
        let (env, _stored) = armed(vec![hanging()]).await;

        let outcome = tokio::time::timeout(Duration::from_millis(300), env.dispatch()).await;

        assert!(outcome.is_err(), "the attempt is still in flight");
        assert_eq!(env.requests().len(), 1, "the request reached the receiver");
        let queue = env.queue().await;
        assert_eq!(queue.len(), 1, "the item must survive cancellation");
        assert_eq!(queue[0].attempts, 0);
    }

    #[tokio::test]
    async fn due_item_behind_a_future_retry_is_still_delivered() {
        let env = TestEnv::new(Vec::new()).await;
        let now = env.now();
        let first = env.add_hook(hook(&env.url, now)).await;
        let second = env.add_hook(hook(&env.url, now)).await;
        env.with_state(|state| {
            state
                .queue
                .push_back(pending_item("msg_a", first.id, now + 3_600_000));
            state.queue.push_back(pending_item("msg_b", second.id, now));
        })
        .await;

        env.dispatch().await;

        let requests = env.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(header_value(&requests[0].0, "webhook-id"), "msg_b");
        let queue = env.queue().await;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].delivery_id, "msg_a");
    }

    #[tokio::test]
    async fn outcome_for_a_deleted_item_is_discarded() {
        let env = TestEnv::new(Vec::new()).await;
        let stored = env.add_hook(hook(&env.url, env.now())).await;
        env.with_state(|state| state.queue.push_back(pending_item("msg_a", stored.id, 0)))
            .await;
        let state = env.engine.state(env.vault(), env.device()).await.unwrap();
        let failure = WebhookTestResult {
            ok: false,
            status_code: Some(503),
            duration_ms: 0,
            error: Some("http_503".into()),
        };

        env.engine
            .record_attempt(env.vault(), &state, "msg_gone", failure)
            .await
            .unwrap();

        assert_eq!(env.queue().await.len(), 1);
        assert_eq!(
            env.hook(stored.id).await.last_delivery_state,
            WebhookDeliveryState::Never
        );
    }

    #[tokio::test]
    async fn revoked_handle_cannot_re_persist_after_removal() {
        let env = TestEnv::new(Vec::new()).await;
        let (vault, device) = (env.vault(), env.device());
        let handle = env.engine.state(vault, device).await.unwrap();
        // An operation that already holds the handle when revocation starts.
        let mut in_flight = handle.lock().await.unwrap();
        let remover = {
            let engine = env.engine.clone();
            tokio::spawn(async move { engine.remove_vault(vault, device).await })
        };
        in_flight.webhooks.push(hook(&env.url, env.now()));
        env.engine.save(vault, &in_flight).unwrap();
        drop(in_flight);
        remover.await.unwrap().unwrap();
        let persisted = store::load(&env.engine.custody, vault, device).unwrap();
        assert!(persisted.webhooks.is_empty(), "removal deletes the record");

        // The same cached handle, used after removal, cannot lock, so it cannot save.
        assert!(handle.lock().await.is_err());
        let mut raw = handle.inner.lock().await;
        raw.webhooks.push(hook(&env.url, env.now()));
        assert!(env.engine.save(vault, &raw).is_err());
        drop(raw);
        let persisted = store::load(&env.engine.custody, vault, device).unwrap();
        assert!(persisted.webhooks.is_empty(), "the record stays absent");
        assert!(env.engine.list(vault, device).await.is_err());
    }

    #[tokio::test]
    async fn stale_request_cannot_write_into_a_replacement_binding() {
        let env = TestEnv::new(Vec::new()).await;
        let (vault, old_device) = (env.vault(), env.device());
        let replacement_device = Uuid::new_v4();
        // The old request resolved its handle before revocation and is still pending.
        let stale = env.engine.state(vault, old_device).await.unwrap();

        env.engine.remove_vault(vault, old_device).await.unwrap();
        // The replacement binding loads its own state for the same vault.
        let fresh = env.engine.state(vault, replacement_device).await.unwrap();
        let kept = hook(&env.url, env.now());
        {
            let mut state = fresh.lock().await.unwrap();
            state.webhooks.push(kept.clone());
            env.engine.save(vault, &state).unwrap();
        }

        // The old request resumes: neither a fresh lookup nor its cached handle
        // yields the replacement's state, and nothing it holds can be saved.
        assert!(env.engine.state(vault, old_device).await.is_err());
        assert!(env.engine.list(vault, old_device).await.is_err());
        assert!(stale.lock().await.is_err());
        let mut raw = stale.inner.lock().await;
        raw.webhooks.push(hook(&env.url, env.now()));
        assert!(env.engine.save(vault, &raw).is_err());
        drop(raw);

        let persisted = store::load(&env.engine.custody, vault, replacement_device).unwrap();
        assert_eq!(
            persisted.webhooks.len(),
            1,
            "replacement record is unchanged"
        );
        assert_eq!(persisted.webhooks[0].id, kept.id);
    }

    #[tokio::test]
    async fn list_completes_while_a_test_send_is_in_flight() {
        let env = TestEnv::new(vec![hanging()]).await;
        let stored = env.add_hook(hook(&env.url, env.now())).await;
        let test_op = env.engine.test(&env.runtime, stored.id);
        tokio::pin!(test_op);

        let listed = tokio::select! {
            _ = &mut test_op => panic!("test send finished before the list call"),
            listed = tokio::time::timeout(
                Duration::from_secs(2),
                env.engine.list(env.vault(), env.device()),
            ) => listed,
        };

        let listed = listed
            .expect("list must not wait for the outbound request")
            .unwrap();
        assert!(listed.available);
        assert_eq!(listed.webhooks.len(), 1);
    }

    #[tokio::test]
    async fn stalled_attempt_is_bounded_by_the_total_timeout() {
        let env = TestEnv::new(vec![hanging()]).await;
        let result = env
            .engine
            .send_within(
                Duration::from_millis(200),
                &env.url,
                &generate_secret(),
                "msg_stalled".into(),
                "{}".into(),
                "https://app.test",
            )
            .await;
        assert!(!result.ok);
        assert_eq!(result.status_code, None);
        assert_eq!(result.error.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn stalled_save_time_resolution_rejects_the_url() {
        let stalled = bounded_resolution(
            Duration::from_millis(20),
            std::future::pending::<Result<Vec<u8>, egress::UrlError>>(),
        )
        .await;
        assert!(matches!(
            stalled,
            Err(WebhookError::Code("webhook_url_invalid"))
        ));
        let blocked = bounded_resolution(
            Duration::from_secs(1),
            std::future::ready(Err::<Vec<u8>, _>(egress::UrlError::Blocked)),
        )
        .await;
        assert!(matches!(
            blocked,
            Err(WebhookError::Code("webhook_url_blocked"))
        ));
    }
}
