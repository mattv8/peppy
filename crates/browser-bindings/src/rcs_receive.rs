use crate::{Failure, core};
use peppy_client_core::{Client, Message};

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
                    let mut view = peppy_desktop_api::snapshot_attachment_view(
                        &info,
                        None,
                        client.deferred_media_status(info.attachment_id),
                    );
                    view.media_meta = peppy_desktop_api::message_attachment_media_metadata_view(
                        client,
                        message.payload.record.message_id,
                        info.attachment_id,
                    )
                    .ok()
                    .flatten()
                    .map(Box::new);
                    view
                })
        })
        .collect();
    peppy_desktop_api::rcs_receive::message_view(client, message, attachments).map_err(core)
}
