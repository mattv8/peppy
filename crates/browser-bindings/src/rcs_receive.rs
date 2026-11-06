use crate::{Failure, core};
use peppy_client_core::{Client, ConversationId, Message};

pub(crate) fn message_view(
    client: &Client,
    message: &Message,
) -> Result<peppy_desktop_api::MessageView, Failure> {
    let attachments = message
        .payload
        .record
        .attachments
        .iter()
        .filter_map(|reference| {
            client
                .attachment_info(reference.attachment_id)
                .ok()
                .map(|info| {
                    peppy_desktop_api::snapshot_attachment_view(
                        &info,
                        None,
                        client.deferred_media_status(info.attachment_id),
                    )
                })
        })
        .collect();
    peppy_desktop_api::rcs_receive::message_view(client, message, attachments).map_err(core)
}
pub(crate) fn conversation_encryption(
    client: &Client,
    id: ConversationId,
) -> Result<Option<bool>, Failure> {
    client
        .rcs_conversation_metadata(id)
        .map_err(core)
        .map(|value| value.and_then(|item| item.rcs_encryption))
}
pub(crate) fn conversation_history_gap(
    client: &Client,
    id: ConversationId,
) -> Result<bool, Failure> {
    client
        .rcs_conversation_metadata(id)
        .map_err(core)
        .map(|value| value.is_some_and(|item| item.rcs_history_gap))
}
