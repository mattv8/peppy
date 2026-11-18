//! End to end through the real core, with a small copy of the core's two-client server: a gateway
//! that swallows a send during setup retires it as not sent, and the sender's own phone, now ready,
//! is the route for the copy.
use crate::compose::{self, SendAgainInput};
use crate::{GatewayAvailability, GatewayView, send_recovery_view};
use peppy_client_core::{
    ApplyReport, Client, ClientConfig, CommandId, ConversationId, Cursor, DatabaseKey, DeviceId,
    Envelope, GatewayRoute, IngestResult, MessageId, OutgoingSms, SendState, SnapshotPurpose,
    SnapshotRecord, VaultId,
};
use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};
use std::collections::HashSet;
use std::str::FromStr;
use tempfile::TempDir;

const PASSPHRASE: &str = "send again flow passphrase";
const DB_KEY: [u8; 32] = [7; 32];
const SIM: &str = "sim-1";

struct Vault {
    id: VaultId,
    profile: KeyProfile,
    header: peppy_crypto::VaultCheckHeader,
}

impl Vault {
    fn new() -> Self {
        let id = VaultId::new();
        let profile = KeyProfile::new(id.0, 1).unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        Self {
            id,
            profile,
            header,
        }
    }
}

fn config(dir: &TempDir, name: &str, vault: &Vault) -> ClientConfig {
    ClientConfig {
        database_path: dir.path().join(format!("{name}.db")),
        vault_id: vault.id,
        device_id: DeviceId::new(),
    }
}

fn unlocked(config: &ClientConfig, vault: &Vault) -> Client {
    let client = Client::open(config.clone(), DatabaseKey::new(&DB_KEY).unwrap()).unwrap();
    client
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    client.set_server_compaction_state(true, true).unwrap();
    client
}

fn route(gateway: &ClientConfig) -> GatewayRoute {
    GatewayRoute::Carrier {
        gateway_device_id: gateway.device_id,
        subscription_id: SIM.into(),
    }
}

fn sms(conversation_id: ConversationId, body: &str) -> OutgoingSms {
    OutgoingSms {
        conversation_id,
        recipients: vec!["+15555550123".into()],
        body: body.into(),
    }
}

/// Vault-wide log with contiguous cursors from 1 and envelope-id idempotency.
#[derive(Default)]
struct Server {
    log: Vec<Envelope>,
}

impl Server {
    fn upload(&mut self, client: &Client) {
        let pending = client.pending_outbox().unwrap();
        for envelope in &pending {
            if !self
                .log
                .iter()
                .any(|known| known.envelope_id == envelope.envelope_id)
            {
                self.log.push(envelope.clone());
            }
            client.ack_outbox(envelope.envelope_id).unwrap();
        }
    }

    fn sync(&self, client: &Client) -> ApplyReport {
        let from = usize::try_from(client.receive_cursor().unwrap().0).unwrap();
        for (index, envelope) in self.log.iter().enumerate().skip(from) {
            let result = client.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
            assert!(matches!(
                result,
                IngestResult::Journaled | IngestResult::Duplicate
            ));
        }
        client.apply_pending(100).unwrap()
    }

    /// Imports the whole log as history, so every command is a historical ledger row.
    fn import_history(&self, gateway: &Client) {
        let records: Vec<SnapshotRecord> = self
            .log
            .iter()
            .enumerate()
            .map(|(index, envelope)| SnapshotRecord {
                cursor: Cursor(index as u64 + 1),
                envelope: envelope.clone(),
            })
            .collect();
        let session = gateway
            .begin_snapshot(
                Cursor(self.log.len() as u64),
                records.len() as u64,
                SnapshotPurpose::Resync,
            )
            .unwrap();
        for page in records.chunks(2) {
            gateway
                .append_snapshot_page(session.generation, page)
                .unwrap();
        }
        gateway.finish_snapshot(session.generation).unwrap();
        gateway.apply_pending(usize::MAX).unwrap();
    }
}

fn phone(id: DeviceId) -> GatewayView {
    GatewayView {
        id: id.to_string(),
        name: "Phone".into(),
        sim_id: SIM.into(),
        online: true,
        simulated: false,
        supports_sms: true,
        supports_mms: false,
        availability: GatewayAvailability::Connected,
        setting_up: false,
        last_seen_age_seconds: None,
        presence: None,
        wake_route: false,
        capability_note: None,
        mms_content_version: None,
        mms_max_bytes: None,
        mms_limit_source: None,
        mms_max_recipients: None,
    }
}

fn runnable(gateway: &Client) -> Vec<CommandId> {
    gateway
        .pending_commands()
        .unwrap()
        .into_iter()
        .map(|command| command.command_id)
        .collect()
}

#[test]
fn a_swallowed_send_is_copied_through_its_own_ready_phone_and_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let (computer_cfg, gateway_cfg) = (
        config(&dir, "computer", &vault),
        config(&dir, "gateway", &vault),
    );
    let computer = unlocked(&computer_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    let original = computer
        .queue_send(sms(conversation, "swallowed"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&computer);
    server.import_history(&gateway);
    assert!(runnable(&gateway).is_empty(), "history is never runnable");

    // The phone was setting up, so it retires the swallowed send as not sent, then becomes ready.
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 1);
    server.upload(&gateway);
    server.sync(&computer);
    computer.apply_pending(usize::MAX).unwrap();

    let no_revocations = HashSet::new();
    let before = computer
        .message_by_id(original.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(before.send_state, Some(SendState::FailedBeforeSubmission));
    let view = send_recovery_view(&computer, &before, &no_revocations)
        .unwrap()
        .unwrap();
    assert_eq!(view.kind, "not-sent");
    assert!(view.can_send_again);

    // The only phone is the one that retired the send: its own route is the copy's route.
    let copy = compose::send_again(
        &computer,
        &SendAgainInput {
            message_id: original.message_id.to_string(),
            gateway_id: gateway_cfg.device_id.to_string(),
            sim_id: SIM.into(),
        },
        Some(&[phone(gateway_cfg.device_id)]),
        &no_revocations,
    )
    .ok()
    .expect("the own phone is a valid route for a not-sent message");

    server.upload(&computer);
    server.sync(&gateway);
    gateway.apply_pending(usize::MAX).unwrap();
    let commands = runnable(&gateway);
    assert_eq!(commands.len(), 1, "the copy is the one runnable command");
    assert_ne!(commands[0], original.command_id);

    let after = computer
        .message_by_id(original.message_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.send_state, Some(SendState::FailedBeforeSubmission));
    let view = send_recovery_view(&computer, &after, &no_revocations)
        .unwrap()
        .unwrap();
    assert_eq!(view.kind, "not-sent");
    assert!(!view.can_send_again);
    assert_eq!(
        view.sent_again.map(|copy| copy.message_id),
        Some(copy.message_id.clone())
    );
    let copy_id = MessageId::from_str(&copy.message_id).unwrap();
    assert!(computer.message_by_id(copy_id).unwrap().is_some());
}
