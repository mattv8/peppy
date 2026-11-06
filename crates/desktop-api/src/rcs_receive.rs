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
    } else {
        view.status = None;
    }
    Ok(view)
}
