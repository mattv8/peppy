use crate::{AttachmentView, MessageView, SendCancellationView, message_view as base_message_view};
use peppy_client_core::{
    Client, Direction, Error, Message, RcsMessageStatus, SendCancellationState, Transport,
};

const MAX_RCS_MEDIA_ATTACHMENTS: u32 = 10;

fn send_cancellation_view(
    client: &Client,
    message: &Message,
) -> Result<Option<SendCancellationView>, Error> {
    if message.payload.direction != Direction::Outgoing {
        return Ok(None);
    }
    match client.message_send_cancellation(message.payload.record.message_id) {
        Ok(view) => Ok(Some(project_send_cancellation(view))),
        // History and provider-imported rows can have no cancellation source. They remain
        // ordinary message views rather than failing the whole projection.
        Err(Error::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

fn project_send_cancellation(
    view: peppy_client_core::SendCancellationView,
) -> SendCancellationView {
    SendCancellationView {
        state: match view.state {
            SendCancellationState::None => "none",
            SendCancellationState::Requested => "requested",
            SendCancellationState::Confirmed => "confirmed",
            SendCancellationState::TooLate => "too-late",
            SendCancellationState::Unknown => "unknown",
        },
        can_cancel: view.can_request,
        requires_gateway_update: view.requires_gateway_update,
    }
}

fn incoming_attachments(
    client: &Client,
    message: &Message,
    mut attachments: Vec<AttachmentView>,
) -> Result<Vec<AttachmentView>, Error> {
    let mut projected = Vec::new();
    for index in 0..=MAX_RCS_MEDIA_ATTACHMENTS {
        let Some(receive) =
            client.rcs_media_receive_view(message.payload.record.message_id, index)?
        else {
            continue;
        };
        let stored = receive.attachment_id.as_ref().and_then(|attachment_id| {
            attachments
                .iter()
                .position(|attachment| attachment.id == attachment_id.to_string())
                .map(|position| attachments.remove(position))
        });
        let mut attachment = stored.unwrap_or(AttachmentView {
            id: receive.placeholder_id.clone(),
            name: receive.display_name.clone(),
            media_type: receive.media_type.clone(),
            byte_size: receive.plaintext_bytes,
            state: "pending",
            error: None,
            preview_url: None,
            transfer: None,
            retryable: None,
            media_hold: None,
            media_meta: None,
            remote_state: None,
            local_available: None,
            draft_state: None,
            index: None,
            rcs_state: None,
            rcs_send_status: None,
        });
        attachment.name = receive.display_name;
        attachment.media_type = receive.media_type;
        attachment.byte_size = receive.plaintext_bytes;
        attachment.index = Some(receive.attachment_index);
        attachment.rcs_state = Some(receive.state.code());
        projected.push(attachment);
    }
    projected.extend(attachments);
    Ok(projected)
}

/// Definitive outgoing slot outcomes that outrank raw receipt history for the whole message.
#[derive(Default)]
struct OutgoingMediaOutcome {
    send_failed: bool,
    outcome_unknown: bool,
}

impl OutgoingMediaOutcome {
    fn record(&mut self, code: &str) {
        self.send_failed |= code == "send_failed";
        self.outcome_unknown |= code == "outcome_unknown";
    }

    /// Unknown wins over failure, and both win over any receipt.
    fn definitive_status(&self) -> Option<&'static str> {
        if self.outcome_unknown {
            Some("unknown")
        } else if self.send_failed {
            Some("failed-confirmed")
        } else {
            None
        }
    }
}

pub fn message_view(
    client: &Client,
    message: &Message,
    mut attachments: Vec<AttachmentView>,
) -> Result<MessageView, Error> {
    let is_rcs = message.payload.transport == Transport::Rcs;
    let message_id = message.payload.record.message_id;
    let (rcs_status, rcs_reported) = if is_rcs || message.payload.direction == Direction::Outgoing {
        (
            client.rcs_message_status(message_id)?,
            client.rcs_message_reported(message_id)?,
        )
    } else {
        (None, None)
    };
    let has_rcs_evidence = rcs_status.is_some() || rcs_reported == Some(true);
    let mut has_outgoing_rcs_media = false;
    let mut outgoing_outcome = OutgoingMediaOutcome::default();
    if is_rcs || message.payload.direction == Direction::Outgoing {
        if message.payload.direction == Direction::Incoming {
            attachments = incoming_attachments(client, message, attachments)?;
        }
        for attachment in &mut attachments {
            if message.payload.direction == Direction::Incoming && attachment.rcs_state.is_some() {
                continue;
            }
            let index = message
                .payload
                .record
                .attachments
                .iter()
                .position(|reference| reference.attachment_id.to_string() == attachment.id)
                .and_then(|position| u32::try_from(position).ok())
                .ok_or(Error::Database)?;
            if message.payload.direction == Direction::Outgoing {
                let Some(status) =
                    client.rcs_media_outgoing_status(message.payload.record.message_id, index)?
                else {
                    if is_rcs {
                        attachment.index = Some(index);
                    }
                    continue;
                };
                if status.attachment_id.to_string() != attachment.id {
                    return Err(Error::Database);
                }
                has_outgoing_rcs_media = true;
                outgoing_outcome.record(status.state.code());
                attachment.index = Some(index);
                attachment.rcs_send_status = Some(status.state.code());
                if let Some(reason) = status.reason {
                    attachment.error = Some(reason.code().to_owned());
                }
            } else {
                attachment.index = Some(index);
            }
        }
    }
    let mut view = base_message_view(message, attachments);
    view.send_cancellation = send_cancellation_view(client, message)?;
    if !is_rcs && !has_outgoing_rcs_media && !has_rcs_evidence {
        return Ok(view);
    }
    view.transport = Some("rcs");
    let definitive = outgoing_outcome.definitive_status();
    if let Some(status) = definitive {
        view.status = Some(status);
    }
    if !is_rcs && (has_outgoing_rcs_media || !has_rcs_evidence) {
        return Ok(view);
    }
    view.rcs_reported = rcs_reported.unwrap_or(false);
    if definitive.is_none()
        && let Some(status) = rcs_status
    {
        view.status = Some(match status.status {
            RcsMessageStatus::Sending => "submitted",
            RcsMessageStatus::Sent => "sent",
            RcsMessageStatus::Delivered => "delivery-confirmed",
            RcsMessageStatus::Read => "read",
            RcsMessageStatus::Failed => "failed-confirmed",
        });
        if status.status == RcsMessageStatus::Read {
            view.status_at = None;
        }
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{
        ClientConfig, ConversationId, DatabaseKey, DeviceId, Direction, GatewayRoute,
        MessagePayload, MessageRecord, OutgoingMms, SourceSequence, VaultId,
    };

    fn client() -> Client {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.keep().join("rcs-view.db");
        Client::open(
            ClientConfig {
                database_path: path,
                vault_id: peppy_client_core::VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn cancellation_projection_uses_frozen_wire_states() {
        for (state, expected) in [
            (SendCancellationState::None, "none"),
            (SendCancellationState::Requested, "requested"),
            (SendCancellationState::Confirmed, "confirmed"),
            (SendCancellationState::TooLate, "too-late"),
            (SendCancellationState::Unknown, "unknown"),
        ] {
            let view = project_send_cancellation(peppy_client_core::SendCancellationView {
                state,
                can_request: true,
                requires_gateway_update: false,
            });
            assert_eq!(view.state, expected);
            assert!(view.can_cancel);
            assert!(!view.requires_gateway_update);
        }
    }

    #[test]
    fn incoming_history_without_cancellation_remains_a_valid_view() {
        let client = client();
        let mut message = rcs_message(None);
        message.payload.direction = Direction::Incoming;

        let view = message_view(&client, &message, vec![]).unwrap();

        assert!(view.send_cancellation.is_none());
    }

    fn rcs_message(send_state: Option<peppy_client_core::SendState>) -> Message {
        Message {
            payload: MessagePayload {
                record: MessageRecord {
                    message_id: peppy_client_core::MessageId::new(),
                    conversation_id: ConversationId::new(),
                    source_sequence: SourceSequence(1),
                    attachments: vec![],
                },
                source_device_id: DeviceId::new(),
                provider_message_id: None,
                sender_address: None,
                recipients: vec![],
                subject: None,
                body: "RCS fallback".into(),
                transport: Transport::Rcs,
                direction: Direction::Outgoing,
                rcs_capture: false,
                imported: false,
                sent_at_ms: Some(1),
                ordered_at_ms: None,
                mms_context: None,
            },
            seen: true,
            send_state,
            status_at_ms: Some(2),
            send_refusal: None,
        }
    }

    fn mms_message() -> Message {
        let mut message = rcs_message(None);
        message.payload.transport = Transport::Mms;
        message
    }

    #[test]
    fn rcs_without_a_status_row_preserves_m2_send_state() {
        let client = client();
        for (state, expected) in [
            (peppy_client_core::SendState::QueuedLocal, "queued-local"),
            (
                peppy_client_core::SendState::FailedBeforeSubmission,
                "failed-before-submit",
            ),
            (peppy_client_core::SendState::OutcomeUnknown, "unknown"),
        ] {
            let view = message_view(&client, &rcs_message(Some(state)), vec![]).unwrap();
            assert_eq!(view.transport, Some("rcs"));
            assert_eq!(view.status, Some(expected));
            assert_eq!(view.status_at.as_deref(), Some("2"));
            assert!(!view.rcs_reported);
        }
    }

    #[test]
    fn outgoing_mms_without_rcs_media_status_stays_mms() {
        let view = message_view(&client(), &mms_message(), vec![]).unwrap();

        assert_eq!(view.transport, Some("mms"));
        assert!(!view.rcs_reported);
    }

    const DELIVERED_RANK: i64 = 4;
    const READ_RANK: i64 = 5;
    const PASSPHRASE: &str = "correct horse battery staple";
    const GATEWAY_ID: &str = "00000000-0000-4000-8000-000000000001";
    const SQLCIPHER_KEY: &str =
        "x'0707070707070707070707070707070707070707070707070707070707070707'";

    struct OutgoingMedia {
        _directory: tempfile::TempDir,
        client: Client,
        database_path: std::path::PathBuf,
        message: Message,
    }

    /// A real queued MMS-shaped media command. Slot and receipt rows are seeded with SQL because
    /// desktop-api cannot reach native RCS admission; the Core writers are covered in client-core.
    fn outgoing_media(slots: u8) -> OutgoingMedia {
        let directory = tempfile::TempDir::new().unwrap();
        let database_path = directory.path().join("rcs-media.db");
        let vault_id = VaultId::new();
        let client = Client::open(
            ClientConfig {
                database_path: database_path.clone(),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        let profile = peppy_crypto::KeyProfile::new(vault_id.0, 1).unwrap();
        let root = peppy_crypto::derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, PASSPHRASE).unwrap();
        let attachment_ids = (0..slots)
            .map(|index| {
                let source = directory.path().join(format!("media-{index}.jpg"));
                std::fs::write(&source, [index + 1; 16]).unwrap();
                client
                    .prepare_attachment(&source, "image/jpeg", &format!("media-{index}.jpg"))
                    .unwrap()
                    .attachment_id
            })
            .collect();
        let conversation_id = ConversationId::new();
        let queued = client
            .queue_mms(
                OutgoingMms {
                    conversation_id,
                    recipients: vec!["+14155552671".into()],
                    body: "caption".into(),
                    attachment_ids,
                    subject: None,
                },
                GatewayRoute::Carrier {
                    gateway_device_id: GATEWAY_ID.parse().unwrap(),
                    subscription_id: "sim".into(),
                },
            )
            .unwrap();
        let message = client
            .messages(conversation_id)
            .unwrap()
            .into_iter()
            .find(|message| message.payload.record.message_id == queued.message_id)
            .unwrap();
        OutgoingMedia {
            _directory: directory,
            client,
            database_path,
            message,
        }
    }

    impl OutgoingMedia {
        fn seed(&self, slots: &[(u32, &str)], receipt_rank: Option<i64>) {
            self.seed_reported(slots, receipt_rank, true);
        }

        fn seed_reported(&self, slots: &[(u32, &str)], receipt_rank: Option<i64>, reported: bool) {
            let db = rusqlite::Connection::open(&self.database_path).unwrap();
            db.execute_batch(&format!("PRAGMA key = \"{SQLCIPHER_KEY}\";"))
                .unwrap();
            let id = self.message.payload.record.message_id.to_string();
            for (index, state) in slots {
                db.execute(
                    "INSERT INTO rcs_media_send_status(message_id,attachment_index,state) VALUES(?,?,?)",
                    rusqlite::params![id, index, state],
                )
                .unwrap();
            }
            if let Some(rank) = receipt_rank {
                db.execute(
                    "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?)",
                    rusqlite::params![id, rank, reported],
                )
                .unwrap();
            } else if reported {
                db.execute(
                    "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,1)",
                    rusqlite::params![id, 0],
                )
                .unwrap();
            }
        }

        fn project(&self, transport: Transport) -> MessageView {
            let mut message = self.message.clone();
            message.payload.transport = transport;
            let attachments = message
                .payload
                .record
                .attachments
                .iter()
                .map(|reference| {
                    crate::attachment_view(
                        &self
                            .client
                            .attachment_info(reference.attachment_id)
                            .unwrap(),
                        None,
                        None,
                    )
                })
                .collect();
            message_view(&self.client, &message, attachments).unwrap()
        }

        fn base(&self) -> MessageView {
            base_message_view(&self.message, vec![])
        }
    }

    #[test]
    fn explicit_rcs_failed_slot_stays_failed_after_a_read_receipt() {
        let media = outgoing_media(2);
        media.seed(&[(0, "send_failed"), (1, "sent")], Some(READ_RANK));
        let view = media.project(Transport::Rcs);
        assert_eq!(view.transport, Some("rcs"));
        assert_eq!(view.status, Some("failed-confirmed"));
        assert_eq!(view.status_at, media.base().status_at);
    }

    #[test]
    fn explicit_rcs_unknown_slot_wins_over_failure_and_read() {
        let media = outgoing_media(2);
        media.seed(
            &[(0, "send_failed"), (1, "outcome_unknown")],
            Some(READ_RANK),
        );
        let view = media.project(Transport::Rcs);
        assert_eq!(view.status, Some("unknown"));
        assert_eq!(view.status_at, media.base().status_at);
    }

    #[test]
    fn mms_shaped_failed_slot_is_not_reported_as_rcs_success() {
        let media = outgoing_media(2);
        media.seed(&[(0, "send_failed"), (1, "sent")], Some(READ_RANK));
        let view = media.project(Transport::Mms);
        assert_eq!(view.transport, Some("rcs"));
        assert_eq!(view.status, Some("failed-confirmed"));
        assert_eq!(view.status_at, media.base().status_at);
    }

    #[test]
    fn mms_shaped_unknown_slot_wins_over_failure() {
        let media = outgoing_media(2);
        media.seed(&[(0, "send_failed"), (1, "outcome_unknown")], None);
        let view = media.project(Transport::Mms);
        assert_eq!(view.transport, Some("rcs"));
        assert_eq!(view.status, Some("unknown"));
    }

    #[test]
    fn mms_shaped_success_keeps_the_ordinary_projection() {
        let media = outgoing_media(2);
        media.seed(&[(0, "sent"), (1, "sending")], Some(READ_RANK));
        let view = media.project(Transport::Mms);
        assert_eq!(view.transport, Some("rcs"));
        assert_eq!(view.status, media.base().status);
        assert_eq!(view.status_at, media.base().status_at);
        assert!(!view.rcs_reported);
    }

    #[test]
    fn explicit_rcs_success_keeps_the_read_receipt() {
        let media = outgoing_media(2);
        media.seed(&[(0, "sent"), (1, "sent")], Some(READ_RANK));
        let view = media.project(Transport::Rcs);
        assert_eq!(view.status, Some("read"));
        assert_eq!(view.status_at, None);
    }

    #[test]
    fn explicit_rcs_without_slot_rows_keeps_the_delivery_receipt() {
        let media = outgoing_media(1);
        media.seed(&[], Some(DELIVERED_RANK));
        let view = media.project(Transport::Rcs);
        assert_eq!(view.status, Some("delivery-confirmed"));
        assert_eq!(view.status_at, media.base().status_at);
    }

    #[test]
    fn outgoing_mms_and_sms_with_read_evidence_project_as_rcs() {
        for transport in [Transport::Mms, Transport::Sms] {
            let media = outgoing_media(1);
            media.seed(&[], Some(READ_RANK));

            let view = media.project(transport);

            assert_eq!(view.transport, Some("rcs"));
            assert_eq!(view.status, Some("read"));
            assert_eq!(view.status_at, None);
            assert!(view.rcs_reported);
        }
    }

    #[test]
    fn outgoing_mms_with_echo_only_evidence_projects_as_rcs() {
        let media = outgoing_media(1);
        media.seed_reported(&[], None, true);

        let view = media.project(Transport::Mms);

        assert_eq!(view.transport, Some("rcs"));
        assert_eq!(view.status, media.base().status);
        assert!(view.rcs_reported);
    }

    #[test]
    fn outgoing_mms_and_sms_with_unreported_echo_only_evidence_keep_their_transport() {
        for (transport, expected) in [(Transport::Mms, "mms"), (Transport::Sms, "sms")] {
            let media = outgoing_media(1);
            media.seed_reported(&[], None, false);

            let view = media.project(transport);

            assert_eq!(view.transport, Some(expected));
            assert!(!view.rcs_reported);
        }
    }

    #[test]
    fn outgoing_mms_with_unreported_failed_evidence_stays_failed() {
        let media = outgoing_media(1);
        media.seed_reported(&[], Some(3), false);

        let view = media.project(Transport::Mms);

        assert_eq!(view.transport, Some("rcs"));
        assert_eq!(view.status, Some("failed-confirmed"));
        assert!(!view.rcs_reported);
    }
}
