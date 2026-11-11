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

pub fn message_view(
    client: &Client,
    message: &Message,
    mut attachments: Vec<AttachmentView>,
) -> Result<MessageView, Error> {
    let is_rcs = message.payload.transport == Transport::Rcs;
    let mut has_outgoing_rcs_media = false;
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
    if !is_rcs && !has_outgoing_rcs_media {
        return Ok(view);
    }
    view.transport = Some("rcs");
    if !is_rcs {
        return Ok(view);
    }
    view.rcs_reported = client
        .rcs_message_reported(message.payload.record.message_id)?
        .unwrap_or(false);
    if let Some(status) = client.rcs_message_status(message.payload.record.message_id)? {
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
        ClientConfig, ConversationId, DatabaseKey, DeviceId, Direction, MessagePayload,
        MessageRecord, SourceSequence,
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
}
