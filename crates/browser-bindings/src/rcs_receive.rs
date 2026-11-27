use crate::{Failure, core};
use peppy_client_core::{Client, Message};
use peppy_desktop_api::rcs_receive::PageReplies;
use std::collections::HashSet;

/// Loads the reply targets of one page of messages in a single batch.
pub(crate) fn page_replies(client: &Client, messages: &[Message]) -> Result<PageReplies, Failure> {
    let refs: Vec<&Message> = messages.iter().collect();
    peppy_desktop_api::rcs_receive::page_reply_summaries(client, &refs).map_err(core)
}

/// `revoked_devices` is the verified roster's revoked device ids (`context.rs`). `replies` comes
/// from `page_replies` for the page that holds `message`.
pub(crate) fn message_view(
    client: &Client,
    message: &Message,
    revoked_devices: &HashSet<String>,
    replies: &PageReplies,
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
    peppy_desktop_api::rcs_receive::message_view_in_page(
        client,
        message,
        attachments,
        revoked_devices,
        replies,
    )
    .map_err(core)
}
