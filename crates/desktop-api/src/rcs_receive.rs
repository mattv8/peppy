use crate::{
    AttachmentStateDto, AttachmentView, MessageReplyView, MessageSender, MessageStatus,
    MessageTransport, MessageView, RcsAttachmentState, RcsSendStatus, SendCancellationStateDto,
    SendCancellationView, SentAgainView, message_view as base_message_view, send_recovery_view,
};
use peppy_client_core::{
    Client, Direction, Error, Message, MessageReplySummary, RcsMediaSyncState, RcsMessageStatus,
    SendCancellationState, Transport,
};
use std::collections::{HashMap, HashSet};

/// Reply targets of one page, keyed by target id. Build it once per page with
/// `page_reply_summaries`, so each distinct target is read once.
pub type PageReplies = HashMap<peppy_client_core::MessageId, MessageReplySummary>;

/// Loads the reply targets of `messages` in one batch.
pub fn page_reply_summaries(client: &Client, messages: &[&Message]) -> Result<PageReplies, Error> {
    let payloads: Vec<&peppy_client_core::MessagePayload> =
        messages.iter().map(|message| &message.payload).collect();
    client.message_reply_summaries(&payloads)
}

/// A message that is not a reply makes no lookup; a reply's target comes from the page batch.
fn reply_to_view(
    message: &Message,
    replies: &PageReplies,
) -> Result<Option<MessageReplyView>, Error> {
    let Some(target) = message.payload.reply_to else {
        return Ok(None);
    };
    let summary = replies.get(&target).ok_or(Error::Database)?;
    Ok(Some(MessageReplyView {
        message_id: summary.message_id.to_string(),
        sender: if summary.sender_self {
            MessageSender::SelfSent
        } else {
            MessageSender::Other
        },
        sender_address: summary.sender_address.clone(),
        excerpt: summary.excerpt.clone(),
        available: summary.available,
    }))
}

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
            SendCancellationState::None => SendCancellationStateDto::None,
            SendCancellationState::Requested => SendCancellationStateDto::Requested,
            SendCancellationState::Confirmed => SendCancellationStateDto::Confirmed,
            SendCancellationState::TooLate => SendCancellationStateDto::TooLate,
            SendCancellationState::Unknown => SendCancellationStateDto::Unknown,
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
    for receive in client.rcs_media_receive_views(message.payload.record.message_id)? {
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
            state: AttachmentStateDto::Pending,
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
        attachment.rcs_state = Some(attachment_rcs_state(receive.state)?);
        projected.push(attachment);
    }
    projected.extend(attachments);
    Ok(projected)
}

/// Receive states a pending or stored RCS attachment can be in. Send-only states are an
/// invariant violation here, as the receive table never stores them.
fn attachment_rcs_state(state: RcsMediaSyncState) -> Result<RcsAttachmentState, Error> {
    Ok(match state {
        RcsMediaSyncState::Downloading => RcsAttachmentState::Downloading,
        RcsMediaSyncState::Stored => RcsAttachmentState::Stored,
        RcsMediaSyncState::TooLarge => RcsAttachmentState::TooLarge,
        RcsMediaSyncState::QuotaFull => RcsAttachmentState::QuotaFull,
        RcsMediaSyncState::Failed => RcsAttachmentState::Failed,
        RcsMediaSyncState::Expired => RcsAttachmentState::Expired,
        RcsMediaSyncState::Sending
        | RcsMediaSyncState::Sent
        | RcsMediaSyncState::NotSent
        | RcsMediaSyncState::OutcomeUnknown
        | RcsMediaSyncState::MembersChanged
        | RcsMediaSyncState::SendFailed => return Err(Error::Database),
    })
}

/// Outgoing slot states. Receive-only states are an invariant violation here.
fn outgoing_rcs_send_status(state: RcsMediaSyncState) -> Result<RcsSendStatus, Error> {
    Ok(match state {
        RcsMediaSyncState::Sending => RcsSendStatus::Sending,
        RcsMediaSyncState::Sent => RcsSendStatus::Sent,
        RcsMediaSyncState::NotSent => RcsSendStatus::NotSent,
        RcsMediaSyncState::OutcomeUnknown => RcsSendStatus::OutcomeUnknown,
        RcsMediaSyncState::MembersChanged => RcsSendStatus::MembersChanged,
        RcsMediaSyncState::SendFailed => RcsSendStatus::SendFailed,
        RcsMediaSyncState::Downloading
        | RcsMediaSyncState::Stored
        | RcsMediaSyncState::TooLarge
        | RcsMediaSyncState::QuotaFull
        | RcsMediaSyncState::Failed
        | RcsMediaSyncState::Expired => return Err(Error::Database),
    })
}

/// Definitive outgoing slot outcomes that outrank raw receipt history for the whole message.
#[derive(Default)]
struct OutgoingMediaOutcome {
    send_failed: bool,
    outcome_unknown: bool,
}

impl OutgoingMediaOutcome {
    fn record(&mut self, status: RcsSendStatus) {
        self.send_failed |= status == RcsSendStatus::SendFailed;
        self.outcome_unknown |= status == RcsSendStatus::OutcomeUnknown;
    }

    /// Unknown wins over failure, and both win over any receipt.
    fn definitive_status(&self) -> Option<MessageStatus> {
        if self.outcome_unknown {
            Some(MessageStatus::Unknown)
        } else if self.send_failed {
            Some(MessageStatus::FailedConfirmed)
        } else {
            None
        }
    }
}

/// `revoked_devices` is the host's verified roster of revoked device ids (see `send_recovery`).
/// The recovery state and the "Send again" link are decided last, on the final displayed transport:
/// a message that displays as RCS, even one stored as SMS, never carries `sendRecovery` or
/// `sentAgain`. The link is independent of the recovery state, so it stays after recovery closes.
pub fn message_view(
    client: &Client,
    message: &Message,
    attachments: Vec<AttachmentView>,
    revoked_devices: &HashSet<String>,
) -> Result<MessageView, Error> {
    let replies = page_reply_summaries(client, &[message])?;
    message_view_in_page(client, message, attachments, revoked_devices, &replies)
}

/// `message_view` for a message of a page whose reply targets were loaded by `page_reply_summaries`.
pub fn message_view_in_page(
    client: &Client,
    message: &Message,
    attachments: Vec<AttachmentView>,
    revoked_devices: &HashSet<String>,
    replies: &PageReplies,
) -> Result<MessageView, Error> {
    let mut view = conversation_message_view(client, message, attachments, replies)?;
    let displays_as_rcs = view.transport == Some(MessageTransport::Rcs);
    view.sent_again = if displays_as_rcs {
        None
    } else {
        message
            .sent_again_message_id
            .map(|message_id| SentAgainView {
                message_id: message_id.to_string(),
            })
    };
    view.send_recovery = if displays_as_rcs {
        None
    } else {
        send_recovery_view(client, message, revoked_devices)?
    };
    Ok(view)
}

fn conversation_message_view(
    client: &Client,
    message: &Message,
    mut attachments: Vec<AttachmentView>,
    replies: &PageReplies,
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
                let send_status = outgoing_rcs_send_status(status.state)?;
                outgoing_outcome.record(send_status);
                attachment.index = Some(index);
                attachment.rcs_send_status = Some(send_status);
                if let Some(reason) = status.reason {
                    attachment.error = Some(reason.code().to_owned());
                }
            } else {
                attachment.index = Some(index);
            }
        }
    }
    let mut view = base_message_view(message, attachments);
    view.reply_to = reply_to_view(message, replies)?;
    view.send_cancellation = send_cancellation_view(client, message)?;
    if !is_rcs && !has_outgoing_rcs_media && !has_rcs_evidence {
        return Ok(view);
    }
    view.transport = Some(MessageTransport::Rcs);
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
            RcsMessageStatus::Sending => MessageStatus::Submitted,
            RcsMessageStatus::Sent => MessageStatus::Sent,
            RcsMessageStatus::Delivered => MessageStatus::DeliveryConfirmed,
            RcsMessageStatus::Read => MessageStatus::Read,
            RcsMessageStatus::Failed => MessageStatus::FailedConfirmed,
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
    use crate::SendRecoveryKind;
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
            (SendCancellationState::None, SendCancellationStateDto::None),
            (
                SendCancellationState::Requested,
                SendCancellationStateDto::Requested,
            ),
            (
                SendCancellationState::Confirmed,
                SendCancellationStateDto::Confirmed,
            ),
            (
                SendCancellationState::TooLate,
                SendCancellationStateDto::TooLate,
            ),
            (
                SendCancellationState::Unknown,
                SendCancellationStateDto::Unknown,
            ),
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

        let view = message_view(&client, &message, vec![], &HashSet::new()).unwrap();

        assert!(view.send_cancellation.is_none());
    }

    #[test]
    fn reaction_facts_survive_the_rcs_projection() {
        let client = client();
        let mut message = rcs_message(None);
        message.reactions = vec![peppy_client_core::MessageReaction {
            emoji: "👍".into(),
            actors: vec![peppy_client_core::ReactionActor {
                is_self: true,
                address: None,
            }],
        }];
        message.reactable = Some(peppy_client_core::ReactionRoute::Native);

        let value =
            serde_json::to_value(message_view(&client, &message, vec![], &HashSet::new()).unwrap())
                .unwrap();

        assert_eq!(value["reactable"], "native");
        assert_eq!(value["reactions"][0]["emoji"], "👍");
        assert_eq!(value["reactions"][0]["actors"][0]["self"], true);
        assert!(value.get("ownReactionRequest").is_none());
    }

    fn rcs_message(send_state: Option<peppy_client_core::SendState>) -> Message {
        Message {
            payload: MessagePayload {
                reply_to: None,
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
            target_gateway_id: None,
            send_cancel_requested: false,
            sent_again_message_id: None,
            send_route: None,
            reactions: Vec::new(),
            own_reaction_request: None,
            reactable: None,
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
            (
                peppy_client_core::SendState::QueuedLocal,
                MessageStatus::QueuedLocal,
            ),
            (
                peppy_client_core::SendState::FailedBeforeSubmission,
                MessageStatus::FailedBeforeSubmit,
            ),
            (
                peppy_client_core::SendState::OutcomeUnknown,
                MessageStatus::Unknown,
            ),
        ] {
            let view =
                message_view(&client, &rcs_message(Some(state)), vec![], &HashSet::new()).unwrap();
            assert_eq!(view.transport, Some(MessageTransport::Rcs));
            assert_eq!(view.status, Some(expected));
            assert_eq!(view.status_at.as_deref(), Some("2"));
            assert!(!view.rcs_reported);
        }
    }

    #[test]
    fn a_stored_sms_that_later_displays_as_rcs_loses_its_send_recovery() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("rcs-recovery.db");
        let client = Client::open(
            ClientConfig {
                database_path: path.clone(),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        let mut message = rcs_message(Some(peppy_client_core::SendState::FailedBeforeSubmission));
        message.payload.transport = Transport::Sms;
        message.send_route = Some(peppy_client_core::SendRouteKind::Carrier);

        let before = message_view(&client, &message, vec![], &HashSet::new()).unwrap();
        assert_eq!(before.transport, Some(MessageTransport::Sms));
        assert_eq!(
            before.send_recovery.as_ref().map(|r| r.kind),
            Some(SendRecoveryKind::NotSent)
        );

        // RCS evidence for the same message makes the displayed transport RCS.
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(
                "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
            )
            .unwrap();
            db.execute(
                "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,1,0)",
                [message.payload.record.message_id.to_string()],
            )
            .unwrap();
        }
        let after = message_view(&client, &message, vec![], &HashSet::new()).unwrap();

        assert_eq!(after.transport, Some(MessageTransport::Rcs));
        assert!(after.send_recovery.is_none());
    }

    fn sent_again_sms(send_state: peppy_client_core::SendState) -> (Message, String) {
        let mut message = rcs_message(Some(send_state));
        message.payload.transport = Transport::Sms;
        message.send_route = Some(peppy_client_core::SendRouteKind::Carrier);
        let copy = peppy_client_core::MessageId::new();
        message.sent_again_message_id = Some(copy);
        (message, copy.to_string())
    }

    #[test]
    fn a_sent_again_link_is_top_level_while_the_original_is_in_recovery() {
        let client = client();
        let (message, copy) = sent_again_sms(peppy_client_core::SendState::FailedBeforeSubmission);

        let view = message_view(&client, &message, vec![], &HashSet::new()).unwrap();

        assert_eq!(
            view.send_recovery.as_ref().map(|r| r.kind),
            Some(SendRecoveryKind::NotSent)
        );
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["sentAgain"]["messageId"], copy);
        assert_eq!(value["sendRecovery"]["sentAgain"]["messageId"], copy);
    }

    #[test]
    fn a_sent_again_link_outlives_the_recovery_state() {
        let client = client();
        for state in [
            peppy_client_core::SendState::SubmittedToOs,
            peppy_client_core::SendState::Sent,
            peppy_client_core::SendState::Delivered,
        ] {
            let (message, copy) = sent_again_sms(state);

            let view = message_view(&client, &message, vec![], &HashSet::new()).unwrap();

            assert!(view.send_recovery.is_none(), "{state:?} has no recovery");
            let value = serde_json::to_value(&view).unwrap();
            assert!(value.get("sendRecovery").is_none());
            assert_eq!(value["sentAgain"]["messageId"], copy);
        }
    }

    #[test]
    fn a_message_displayed_as_rcs_has_no_sent_again_link() {
        let client = client();
        let mut message = rcs_message(Some(peppy_client_core::SendState::Delivered));
        message.sent_again_message_id = Some(peppy_client_core::MessageId::new());

        let view = message_view(&client, &message, vec![], &HashSet::new()).unwrap();

        assert_eq!(view.transport, Some(MessageTransport::Rcs));
        assert!(view.sent_again.is_none());
        assert!(
            serde_json::to_value(&view)
                .unwrap()
                .get("sentAgain")
                .is_none()
        );
    }

    #[test]
    fn outgoing_mms_without_rcs_media_status_stays_mms() {
        let view = message_view(&client(), &mms_message(), vec![], &HashSet::new()).unwrap();

        assert_eq!(view.transport, Some(MessageTransport::Mms));
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
            message_view(&self.client, &message, attachments, &HashSet::new()).unwrap()
        }

        fn base(&self) -> MessageView {
            base_message_view(&self.message, vec![])
        }
    }

    #[test]
    fn incoming_slots_project_in_index_order_and_propagate_malformed_rows() {
        let media = outgoing_media(0);
        let db = rusqlite::Connection::open(&media.database_path).unwrap();
        db.execute_batch(&format!("PRAGMA key = \"{SQLCIPHER_KEY}\";"))
            .unwrap();
        let id = media.message.payload.record.message_id.to_string();
        db.execute(
            "INSERT INTO rcs_media_receive(message_id,attachment_index,state,media_type,display_name,plaintext_bytes) VALUES(?,3,'downloading','image/png','third.png',30),(?,1,'too_large','video/mp4','first.mp4',10)",
            rusqlite::params![id, id],
        ).unwrap();
        let mut message = media.message.clone();
        message.payload.direction = Direction::Incoming;
        message.payload.transport = Transport::Rcs;
        let view = message_view(&media.client, &message, vec![], &HashSet::new()).unwrap();
        assert_eq!(view.attachments.len(), 2);
        for (attachment, index, name, media_type, bytes, state) in [
            (
                &view.attachments[0],
                1,
                "first.mp4",
                "video/mp4",
                10,
                RcsAttachmentState::TooLarge,
            ),
            (
                &view.attachments[1],
                3,
                "third.png",
                "image/png",
                30,
                RcsAttachmentState::Downloading,
            ),
        ] {
            assert_eq!(attachment.id, format!("{id}:{index}"));
            assert_eq!(attachment.index, Some(index));
            assert_eq!(attachment.name, name);
            assert_eq!(attachment.media_type, media_type);
            assert_eq!(attachment.byte_size, bytes);
            assert_eq!(attachment.rcs_state, Some(state));
            assert_eq!(attachment.state, AttachmentStateDto::Pending);
        }
        db.execute(
            "INSERT INTO rcs_media_receive(message_id,attachment_index,state,media_type,display_name,plaintext_bytes,attachment_id) VALUES(?,2,'downloading','image/png','bad.png',1,'invalid')",
            [id],
        ).unwrap();
        assert!(matches!(
            message_view(&media.client, &message, vec![], &HashSet::new()),
            Err(Error::Database)
        ));
    }

    #[test]
    fn explicit_rcs_failed_slot_stays_failed_after_a_read_receipt() {
        let media = outgoing_media(2);
        media.seed(&[(0, "send_failed"), (1, "sent")], Some(READ_RANK));
        let view = media.project(Transport::Rcs);
        assert_eq!(view.transport, Some(MessageTransport::Rcs));
        assert_eq!(view.status, Some(MessageStatus::FailedConfirmed));
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
        assert_eq!(view.status, Some(MessageStatus::Unknown));
        assert_eq!(view.status_at, media.base().status_at);
    }

    #[test]
    fn mms_shaped_failed_slot_is_not_reported_as_rcs_success() {
        let media = outgoing_media(2);
        media.seed(&[(0, "send_failed"), (1, "sent")], Some(READ_RANK));
        let view = media.project(Transport::Mms);
        assert_eq!(view.transport, Some(MessageTransport::Rcs));
        assert_eq!(view.status, Some(MessageStatus::FailedConfirmed));
        assert_eq!(view.status_at, media.base().status_at);
    }

    #[test]
    fn mms_shaped_unknown_slot_wins_over_failure() {
        let media = outgoing_media(2);
        media.seed(&[(0, "send_failed"), (1, "outcome_unknown")], None);
        let view = media.project(Transport::Mms);
        assert_eq!(view.transport, Some(MessageTransport::Rcs));
        assert_eq!(view.status, Some(MessageStatus::Unknown));
    }

    #[test]
    fn mms_shaped_success_keeps_the_ordinary_projection() {
        let media = outgoing_media(2);
        media.seed(&[(0, "sent"), (1, "sending")], Some(READ_RANK));
        let view = media.project(Transport::Mms);
        assert_eq!(view.transport, Some(MessageTransport::Rcs));
        assert_eq!(view.status, media.base().status);
        assert_eq!(view.status_at, media.base().status_at);
        assert!(!view.rcs_reported);
    }

    #[test]
    fn explicit_rcs_success_keeps_the_read_receipt() {
        let media = outgoing_media(2);
        media.seed(&[(0, "sent"), (1, "sent")], Some(READ_RANK));
        let view = media.project(Transport::Rcs);
        assert_eq!(view.status, Some(MessageStatus::Read));
        assert_eq!(view.status_at, None);
    }

    #[test]
    fn explicit_rcs_without_slot_rows_keeps_the_delivery_receipt() {
        let media = outgoing_media(1);
        media.seed(&[], Some(DELIVERED_RANK));
        let view = media.project(Transport::Rcs);
        assert_eq!(view.status, Some(MessageStatus::DeliveryConfirmed));
        assert_eq!(view.status_at, media.base().status_at);
    }

    #[test]
    fn outgoing_mms_and_sms_with_read_evidence_project_as_rcs() {
        for transport in [Transport::Mms, Transport::Sms] {
            let media = outgoing_media(1);
            media.seed(&[], Some(READ_RANK));

            let view = media.project(transport);

            assert_eq!(view.transport, Some(MessageTransport::Rcs));
            assert_eq!(view.status, Some(MessageStatus::Read));
            assert_eq!(view.status_at, None);
            assert!(view.rcs_reported);
        }
    }

    #[test]
    fn outgoing_mms_with_echo_only_evidence_projects_as_rcs() {
        let media = outgoing_media(1);
        media.seed_reported(&[], None, true);

        let view = media.project(Transport::Mms);

        assert_eq!(view.transport, Some(MessageTransport::Rcs));
        assert_eq!(view.status, media.base().status);
        assert!(view.rcs_reported);
    }

    #[test]
    fn outgoing_mms_and_sms_with_unreported_echo_only_evidence_keep_their_transport() {
        for (transport, expected) in [
            (Transport::Mms, MessageTransport::Mms),
            (Transport::Sms, MessageTransport::Sms),
        ] {
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

        assert_eq!(view.transport, Some(MessageTransport::Rcs));
        assert_eq!(view.status, Some(MessageStatus::FailedConfirmed));
        assert!(!view.rcs_reported);
    }
}
