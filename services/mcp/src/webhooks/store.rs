use std::collections::VecDeque;

use peppy_hosted_api::mcp::{
    WebhookDeliveryState, WebhookEvent, WebhookFilterEntry, WebhookFilterMode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    custody::{Custody, CustodyError},
    runtime::RuntimeError,
};

pub const MAX_WEBHOOKS: usize = 10;
pub const MAX_QUEUE: usize = 500;
pub const MAX_RECENT: usize = 2_000;
const PURPOSE: &[u8] = b"peppy.mcp.webhooks.v1";

#[derive(Serialize, Deserialize)]
pub(crate) struct WebhookState {
    pub version: u8,
    pub device_id: Uuid,
    pub cursor: i64,
    pub cursor_initialized: bool,
    #[serde(default)]
    pub source_origin: String,
    pub webhooks: Vec<StoredWebhook>,
    pub queue: VecDeque<Delivery>,
    pub recent: VecDeque<(Uuid, String)>,
    /// In-memory fence set by vault removal. Never persisted: once set, every save
    /// through this handle fails, so a cached handle cannot re-create the record.
    #[serde(skip)]
    pub revoked: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredWebhook {
    pub id: Uuid,
    pub name: Option<String>,
    pub url: String,
    pub events: Vec<WebhookEvent>,
    pub filter_mode: WebhookFilterMode,
    pub filter: Vec<WebhookFilterEntry>,
    pub paused: bool,
    pub secret: String,
    pub created_at_ms: i64,
    pub last_delivery_state: WebhookDeliveryState,
    pub last_delivery_at_ms: Option<i64>,
    pub last_status_code: Option<u16>,
    pub last_error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Delivery {
    pub delivery_id: String,
    pub webhook_id: Uuid,
    pub body: String,
    pub attempts: u8,
    pub next_attempt_at_ms: i64,
    pub created_at_ms: i64,
}

impl WebhookState {
    pub fn new(device_id: Uuid) -> Self {
        Self {
            version: 1,
            device_id,
            cursor: 0,
            cursor_initialized: false,
            source_origin: String::new(),
            webhooks: Vec::new(),
            queue: VecDeque::new(),
            recent: VecDeque::new(),
            revoked: false,
        }
    }

    pub fn remember(&mut self, webhook_id: Uuid, message_id: String) {
        self.recent.push_back((webhook_id, message_id));
        while self.recent.len() > MAX_RECENT {
            self.recent.pop_front();
        }
    }
}

pub(crate) fn record_name(vault_id: Uuid) -> String {
    format!("webhooks-{vault_id}")
}

pub(crate) fn load(
    custody: &Custody,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<WebhookState, RuntimeError> {
    let Some(bytes) = custody.load(&record_name(vault_id), &aad(vault_id, device_id))? else {
        return Ok(WebhookState::new(device_id));
    };
    let state = serde_json::from_slice(&bytes).map_err(|_| RuntimeError::Core)?;
    Ok(state)
}

pub(crate) fn save(
    custody: &Custody,
    vault_id: Uuid,
    state: &WebhookState,
) -> Result<(), RuntimeError> {
    let bytes = serde_json::to_vec(state).map_err(|_| RuntimeError::Core)?;
    custody.store(
        &record_name(vault_id),
        &aad(vault_id, state.device_id),
        &bytes,
    )?;
    Ok(())
}

pub(crate) fn remove(custody: &Custody, vault_id: Uuid) -> Result<(), CustodyError> {
    custody.remove(&record_name(vault_id))
}

fn aad(vault_id: Uuid, device_id: Uuid) -> Vec<u8> {
    [PURPOSE, vault_id.as_bytes(), device_id.as_bytes()].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_queue_bounds_are_enforced() {
        let mut state = WebhookState::new(Uuid::nil());
        for index in 0..MAX_RECENT + 1 {
            state.remember(Uuid::nil(), index.to_string());
        }
        assert_eq!(state.recent.len(), MAX_RECENT);
        assert_eq!(state.recent.front().map(|item| item.1.as_str()), Some("1"));
    }
}
