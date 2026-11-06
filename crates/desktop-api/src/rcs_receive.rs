use crate::{AttachmentView, MessageView, message_view as base_message_view};
use peppy_client_core::{Client, Error, Message, RcsMessageStatus, Transport};

pub fn message_view(
    client: &Client,
    message: &Message,
    attachments: Vec<AttachmentView>,
) -> Result<MessageView, Error> {
    let mut view = base_message_view(message, attachments);
    if message.payload.transport != Transport::Rcs {
        return Ok(view);
    }
    view.transport = Some("rcs");
    if let Some(status) = client.rcs_message_status(message.payload.record.message_id)? {
        view.status = Some(match status.status {
            RcsMessageStatus::Sending => "submitted",
            RcsMessageStatus::Sent => "sent",
            RcsMessageStatus::Delivered => "delivery-confirmed",
            RcsMessageStatus::Read => "read",
            RcsMessageStatus::Failed => "failed-confirmed",
        });
        view.rcs_reported = status.rcs_reported;
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{
        ClientConfig, ConversationId, DatabaseKey, DeviceId, Direction, MessagePayload, MessageRecord,
        SourceSequence,
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
        }
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
}
