//! Shared two-client fixtures: real SQLCipher clients and a minimal in-memory server that
//! assigns contiguous vault cursors exactly like `/v1/events`.
#![allow(dead_code)] // individual suites use only a subset of the shared fixtures

pub use peppy_client_core::*;
use peppy_crypto::{create_vault_check_header, derive_root_key};
use tempfile::TempDir;

pub const PASSPHRASE: &str = "correct horse battery staple";
pub const DB_KEY: [u8; 32] = [7; 32];
pub const ADDRESS: &str = "+15555550100";

pub struct Vault {
    pub id: VaultId,
    pub profile: KeyProfile,
    pub header: VaultCheckHeader,
}
impl Vault {
    pub fn new() -> Self {
        let id = VaultId::new();
        Self::epoch(id, 1)
    }
    pub fn epoch(id: VaultId, epoch: u32) -> Self {
        let profile = KeyProfile::new(id.0, epoch).unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        Self {
            id,
            profile,
            header,
        }
    }
}

pub fn config(dir: &TempDir, name: &str, vault: &Vault) -> ClientConfig {
    ClientConfig {
        database_path: dir.path().join(format!("{name}.db")),
        vault_id: vault.id,
        device_id: DeviceId::new(),
    }
}
pub fn open(config: &ClientConfig) -> Client {
    Client::open(config.clone(), DatabaseKey::new(&DB_KEY).unwrap()).unwrap()
}
pub fn current_schema_version() -> i64 {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "schema-version", &vault);
    drop(open(&config));
    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    let key: String = DB_KEY.iter().map(|byte| format!("{byte:02x}")).collect();
    connection
        .execute_batch(&format!(
            "PRAGMA key = \"x'{key}'\"; PRAGMA cipher_compatibility = 4;"
        ))
        .unwrap();
    connection
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap()
}
pub fn decrypted_outbox(client: &Client, vault: &Vault) -> Vec<serde_json::Value> {
    use peppy_crypto::{EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key};

    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    client
        .pending_outbox()
        .unwrap()
        .iter()
        .map(|envelope| {
            let purpose = match envelope.purpose {
                EnvelopePurpose::Event => KeyPurpose::Event,
                EnvelopePurpose::Command => KeyPurpose::Command,
            };
            let key = derive_purpose_key(&root, &vault.profile, purpose).unwrap();
            let (nonce, ciphertext) = envelope.ciphertext.split_at(24);
            let encrypted = EncryptedEnvelope {
                nonce: nonce.try_into().unwrap(),
                ciphertext: ciphertext.to_vec(),
            };
            let plaintext = decrypt(&key, &envelope.aad_bytes().unwrap(), &encrypted)
                .expect("pending outbox envelope must authenticate with the fixture vault");
            serde_json::from_slice(&plaintext).unwrap()
        })
        .collect()
}
pub fn assert_rcs_private_payload(value: &serde_json::Value) {
    fn keys(value: &serde_json::Value, required: &[&str], optional: &[&str]) {
        let object = value.as_object().expect("expected a payload object");
        let mut expected = required.to_vec();
        expected.extend(
            optional
                .iter()
                .copied()
                .filter(|key| object.contains_key(*key)),
        );
        expected.sort_unstable();
        let mut actual: Vec<_> = object.keys().map(String::as_str).collect();
        actual.sort_unstable();
        assert_eq!(actual, expected, "unexpected payload fields: {value}");
    }
    if let Some(compaction) = value.get("compaction") {
        keys(compaction, &["key", "supersedes", "terminal"], &[]);
        for reference in compaction["supersedes"].as_array().unwrap() {
            keys(reference, &["producer_device_id", "producer_sequence"], &[]);
        }
    }
    match value["kind"].as_str().unwrap() {
        "message" => {
            keys(
                value,
                &[
                    "attachments",
                    "body",
                    "conversation_id",
                    "direction",
                    "imported",
                    "kind",
                    "message_id",
                    "mms_context",
                    "provider_message_id",
                    "recipients",
                    "sender_address",
                    "sent_at_ms",
                    "source_device_id",
                    "source_sequence",
                    "subject",
                    "transport",
                ],
                &["rcs_capture", "ordered_at_ms"],
            );
            assert!(
                value["provider_message_id"].is_null(),
                "provider identifier leaked: {value}"
            );
            assert!(value["mms_context"].is_null());
            assert!(value["attachments"].as_array().unwrap().is_empty());
        }
        "rcs_conversation_metadata" => keys(
            value,
            &[
                "compaction",
                "conversation_id",
                "kind",
                "logical_revision",
                "rcs_history_gap",
                "transport",
                "writer_device_id",
            ],
            &["rcs_encryption"],
        ),
        "rcs_message_status" => keys(
            value,
            &["compaction", "kind", "message_id", "rcs_reported", "status"],
            &[],
        ),
        "rcs_message_reported" => keys(
            value,
            &["compaction", "kind", "message_id", "rcs_reported"],
            &[],
        ),
        "rcs_link_status" => {
            keys(value, &["compaction", "kind", "status"], &[]);
            keys(
                &value["status"],
                &[
                    "availability",
                    "capability_revision",
                    "feature_bits",
                    "gateway_device_id",
                    "health",
                    "observed_at_ms",
                    "phone_responsive",
                    "state",
                ],
                &["linked_since_ms"],
            );
        }
        "rcs_media_state" => {
            keys(
                value,
                &[
                    "attachment_index",
                    "kind",
                    "message_id",
                    "metadata",
                    "state",
                ],
                &["compaction"],
            );
            keys(
                &value["metadata"],
                &["display_name", "media_type", "plaintext_bytes"],
                &[],
            );
        }
        kind => panic!("unexpected RCS payload kind: {kind}"),
    }
}
pub fn assert_invalid_request<T>(result: Result<T, Error>) {
    match result {
        Err(Error::InvalidRequest(_)) => {}
        Err(error) => panic!("expected InvalidRequest, got {error:?}"),
        Ok(_) => panic!("expected InvalidRequest, got success"),
    }
}
pub fn unlocked(config: &ClientConfig, vault: &Vault) -> Client {
    let client = open(config);
    client
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    connect(&client);
    client
}
/// The host handshake with the mock `Server`, which stores compaction headers unchanged and
/// whose whole roster fences snapshots (`compaction_supported` and `compaction_active`).
pub fn connect(client: &Client) {
    client.set_server_compaction_state(true, true).unwrap();
}

/// Makes a fixture's initial replay explicit before exercising imported history capture.
pub fn catch_up_initial_replay(client: &Client, high_water: u64) {
    client
        .observe_server_high_water(Cursor(high_water))
        .unwrap();
    assert!(client.initial_replay_caught_up().unwrap());
}
pub fn route(gateway: &ClientConfig) -> GatewayRoute {
    GatewayRoute::Carrier {
        gateway_device_id: gateway.device_id,
        subscription_id: "sim-1".into(),
    }
}
pub fn sms(conversation_id: ConversationId, body: &str) -> OutgoingSms {
    OutgoingSms {
        conversation_id,
        recipients: vec![ADDRESS.into()],
        body: body.into(),
    }
}
pub fn incoming(body: &str, provider_id: &str) -> IncomingSms {
    IncomingSms {
        conversation_id: None,
        sender_address: ADDRESS.into(),
        body: body.into(),
        provider_message_id: Some(provider_id.into()),
        imported: false,
    }
}

/// Vault-wide log with contiguous cursors starting at 1 and envelope-ID idempotency.
#[derive(Default)]
pub struct Server {
    pub log: Vec<Envelope>,
}
impl Server {
    pub fn upload(&mut self, client: &Client) -> usize {
        let pending = client.pending_outbox().unwrap();
        for envelope in &pending {
            if !self
                .log
                .iter()
                .any(|e| e.envelope_id == envelope.envelope_id)
            {
                self.log.push(envelope.clone());
            }
            client.ack_outbox(envelope.envelope_id).unwrap();
        }
        pending.len()
    }
    pub fn sync(&self, client: &Client) -> ApplyReport {
        let from = usize::try_from(client.receive_cursor().unwrap().0).unwrap();
        for (index, envelope) in self.log.iter().enumerate().skip(from) {
            let result = client.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
            assert!(matches!(
                result,
                IngestResult::Journaled | IngestResult::Duplicate
            ));
        }
        assert_eq!(client.receive_cursor().unwrap().0, self.log.len() as u64);
        client
            .observe_replay_head(Cursor(self.log.len() as u64))
            .unwrap();
        client.apply_pending(100).unwrap()
    }
    pub fn redeliver(&self, client: &Client) -> ApplyReport {
        let cursor = client.receive_cursor().unwrap().0;
        assert!(
            cursor > 0,
            "redelivery requires a previously delivered cursor"
        );
        for (index, envelope) in self.log.iter().enumerate().take(cursor as usize) {
            assert_eq!(
                client.ingest(envelope, Cursor(index as u64 + 1)).unwrap(),
                IngestResult::Duplicate
            );
        }
        client.apply_pending(100).unwrap()
    }
}

pub fn only_message(client: &Client, conversation: ConversationId) -> Message {
    let mut messages = client.messages(conversation).unwrap();
    assert_eq!(messages.len(), 1);
    messages.remove(0)
}

fn length_delimited(number: u8, value: &[u8]) -> Vec<u8> {
    let mut encoded = vec![number << 3 | 2, u8::try_from(value.len()).unwrap()];
    encoded.extend_from_slice(value);
    encoded
}

/// The transaction id Google Messages writes on the copy of a chat message in the phone's store.
pub fn mirrored_transaction_id(google_message_id: &str) -> String {
    use base64::Engine;
    let mut identity = length_delimited(2, &length_delimited(5, google_message_id.as_bytes()));
    identity.extend(length_delimited(5, &[7; 16]));
    identity.extend(length_delimited(6, &[9; 16]));
    let mut envelope = length_delimited(1, b"com.google.android.apps.messaging.");
    envelope.extend(length_delimited(2, &identity));
    format!(
        "proto:{}",
        base64::engine::general_purpose::STANDARD.encode(length_delimited(1, &envelope))
    )
}
