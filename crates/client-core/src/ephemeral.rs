use crate::{ConversationId, DeviceId, Error, PurposeKey, VaultId};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_crypto::{EncryptedEnvelope, decrypt, encrypt};
use peppy_protocol::{EPHEMERAL_PROTOCOL_VERSION, EphemeralSignalV1, MAX_EPHEMERAL_TTL_MS};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

pub const TYPING_START_INTERVAL_MS: i64 = 3_000;
pub const TYPING_STARTS_PER_MINUTE: usize = 20;
pub const MAX_TYPING_ROSTER: usize = 100;
const MAX_TYPING_TRACKED_CONVERSATIONS: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypingOrigin {
    VaultDevice,
    GoogleContact,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypingState {
    Started,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypingView {
    pub conversation_id: ConversationId,
    pub state: TypingState,
    pub origin: TypingOrigin,
    pub expires_at_ms: i64,
}

#[derive(Default)]
pub(crate) struct EphemeralTypingState {
    outgoing: HashMap<ConversationId, OutgoingState>,
    incoming: HashMap<(DeviceId, ConversationId), IncomingState>,
}

struct OutgoingState {
    active: bool,
    next_sequence: u64,
    last_start_at_ms: Option<i64>,
    starts: VecDeque<i64>,
    expires_at_ms: i64,
}

struct IncomingState {
    last_sequence: u64,
    starts: VecDeque<i64>,
    expires_at_ms: i64,
}

pub(crate) struct SealContext<'a> {
    pub(crate) key: &'a PurposeKey,
    pub(crate) vault_id: VaultId,
    pub(crate) sender_device_id: DeviceId,
    pub(crate) key_epoch: u32,
}

pub(crate) struct SealRequest {
    pub(crate) conversation_id: ConversationId,
    pub(crate) origin: TypingOrigin,
    pub(crate) state: TypingState,
    pub(crate) now_ms: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TypingPlaintext {
    version: u8,
    origin: TypingOrigin,
    conversation_id: ConversationId,
    state: TypingState,
}

impl EphemeralTypingState {
    pub(crate) fn clear(&mut self) {
        self.outgoing.clear();
        self.incoming.clear();
    }

    pub(crate) fn seal(
        &mut self,
        context: SealContext<'_>,
        request: SealRequest,
    ) -> Result<Option<EphemeralSignalV1>, Error> {
        let now_ms = u64::try_from(request.now_ms)
            .map_err(|_| Error::InvalidRequest("invalid typing time"))?;
        self.prune(now_ms);
        if !self.outgoing.contains_key(&request.conversation_id)
            && self.outgoing.len() >= MAX_TYPING_TRACKED_CONVERSATIONS
        {
            return Ok(None);
        }
        let entry = self
            .outgoing
            .entry(request.conversation_id)
            .or_insert_with(|| OutgoingState {
                active: false,
                next_sequence: 0,
                last_start_at_ms: None,
                starts: VecDeque::new(),
                expires_at_ms: now_ms as i64,
            });
        entry
            .starts
            .retain(|start| now_ms.saturating_sub(*start as u64) < 60_000);
        if request.state == TypingState::Started
            && (entry.last_start_at_ms.is_some_and(|start| {
                now_ms.saturating_sub(start as u64) < TYPING_START_INTERVAL_MS as u64
            }) || entry.starts.len() >= TYPING_STARTS_PER_MINUTE)
        {
            return Ok(None);
        }
        if request.state == TypingState::Stopped && !entry.active {
            return Ok(None);
        }
        entry.next_sequence = entry.next_sequence.checked_add(1).ok_or(Error::Crypto)?;
        if request.state == TypingState::Started {
            entry.last_start_at_ms = Some(now_ms as i64);
            entry.starts.push_back(now_ms as i64);
            entry.active = true;
        } else {
            entry.active = false;
        }
        let expires_at_ms = now_ms
            .checked_add(MAX_EPHEMERAL_TTL_MS)
            .ok_or(Error::Crypto)?;
        entry.expires_at_ms = expires_at_ms as i64;
        let mut signal = EphemeralSignalV1 {
            protocol_version: EPHEMERAL_PROTOCOL_VERSION,
            kind: "typing".into(),
            sender_device_id: context.sender_device_id,
            key_epoch: context.key_epoch,
            sent_at_ms: now_ms.to_string(),
            expires_at_ms: expires_at_ms.to_string(),
            sequence: entry.next_sequence.to_string(),
            ciphertext_b64: STANDARD.encode([0]),
        };
        let plaintext = serde_json::to_vec(&TypingPlaintext {
            version: 1,
            origin: request.origin,
            conversation_id: request.conversation_id,
            state: request.state,
        })
        .map_err(|_| Error::Crypto)?;
        let encrypted = encrypt(
            context.key,
            &signal
                .aad_bytes(context.vault_id)
                .map_err(|_| Error::Crypto)?,
            &plaintext,
        )
        .map_err(|_| Error::Crypto)?;
        let mut frame = encrypted.nonce.to_vec();
        frame.extend_from_slice(&encrypted.ciphertext);
        signal.ciphertext_b64 = STANDARD.encode(frame);
        signal.validate().map_err(|_| Error::Crypto)?;
        Ok(Some(signal))
    }

    pub(crate) fn open(
        &mut self,
        keys: &std::collections::BTreeMap<u32, crate::EpochKeys>,
        vault_id: VaultId,
        signal: &EphemeralSignalV1,
        trusted_devices: &[DeviceId],
        received_at_ms: i64,
    ) -> Option<TypingView> {
        let received_at_ms = u64::try_from(received_at_ms).ok()?;
        self.prune(received_at_ms);
        if trusted_devices.len() > MAX_TYPING_ROSTER
            || !trusted_devices.contains(&signal.sender_device_id)
            || signal.validate().is_err()
        {
            return None;
        }
        let sent_at_ms = signal.sent_at_ms.parse::<u64>().ok()?;
        let expires_at_ms = signal.expires_at_ms.parse::<u64>().ok()?;
        let sequence = signal.sequence.parse::<u64>().ok()?;
        if sent_at_ms > received_at_ms || expires_at_ms <= received_at_ms {
            return None;
        }
        let key = &keys.get(&signal.key_epoch)?.event;
        let frame = STANDARD.decode(&signal.ciphertext_b64).ok()?;
        let nonce: [u8; 24] = frame.get(..24)?.try_into().ok()?;
        let plaintext = decrypt(
            key,
            &signal.aad_bytes(vault_id).ok()?,
            &EncryptedEnvelope {
                nonce,
                ciphertext: frame.get(24..)?.to_vec(),
            },
        )
        .ok()?;
        let plaintext: TypingPlaintext = serde_json::from_slice(&plaintext).ok()?;
        if plaintext.version != 1 {
            return None;
        }
        let key = (signal.sender_device_id, plaintext.conversation_id);
        if !self.incoming.contains_key(&key)
            && self.incoming.len() >= MAX_TYPING_TRACKED_CONVERSATIONS
        {
            return None;
        }
        let entry = self.incoming.entry(key).or_insert_with(|| IncomingState {
            last_sequence: 0,
            starts: VecDeque::new(),
            expires_at_ms: expires_at_ms as i64,
        });
        if sequence <= entry.last_sequence {
            return None;
        }
        entry
            .starts
            .retain(|start| received_at_ms.saturating_sub(*start as u64) < 60_000);
        if plaintext.state == TypingState::Started && entry.starts.len() >= TYPING_STARTS_PER_MINUTE
        {
            return None;
        }
        entry.last_sequence = sequence;
        if plaintext.state == TypingState::Started {
            entry.starts.push_back(received_at_ms as i64);
        }
        let expires_at_ms = expires_at_ms.min(received_at_ms.checked_add(MAX_EPHEMERAL_TTL_MS)?);
        entry.expires_at_ms = expires_at_ms as i64;
        Some(TypingView {
            conversation_id: plaintext.conversation_id,
            state: plaintext.state,
            origin: plaintext.origin,
            expires_at_ms: expires_at_ms as i64,
        })
    }

    fn prune(&mut self, now_ms: u64) {
        self.outgoing
            .retain(|_, state| state.expires_at_ms > now_ms as i64);
        self.incoming
            .retain(|_, state| state.expires_at_ms > now_ms as i64);
    }
}
