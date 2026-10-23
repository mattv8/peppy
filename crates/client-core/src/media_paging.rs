//! Metadata-only, cursor-stable media paging around one visible attachment.

use crate::{
    AttachmentId, Client, ConversationId, Error, MediaCursor, MediaPage, MediaPageItem, MessageId,
    attachment_row, decode, media_metadata_for, parse,
};
use rusqlite::{Connection, params};

/// A bounded media window centered on one visible attachment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPageAround {
    pub target: MediaPageItem,
    pub newer: Vec<MediaPageItem>,
    pub older: Vec<MediaPageItem>,
    pub newer_cursor: Option<MediaCursor>,
    pub older_cursor: Option<MediaCursor>,
}

type MediaRow = (String, i64, i64, String, i64, String);

impl Client {
    /// Resolves an unambiguous attachment and returns its bounded neighboring media rows.
    pub fn media_page_around(
        &self,
        conversation_id: ConversationId,
        attachment_id: AttachmentId,
        limit: usize,
    ) -> Result<MediaPageAround, Error> {
        validate_limit(limit)?;
        let store = self.lock()?;
        let anchors = anchor_rows(&store.conn, conversation_id, attachment_id, None)?;
        let [anchor] = anchors.as_slice() else {
            return if anchors.is_empty() {
                Err(Error::NotFound)
            } else {
                Err(Error::InvalidRequest("media anchor ambiguous"))
            };
        };
        media_page_around_row(&store.conn, conversation_id, anchor.clone(), limit)
    }

    /// Returns an anchored window for an explicitly identified message attachment.
    pub fn media_page_around_at(
        &self,
        conversation_id: ConversationId,
        message_id: MessageId,
        attachment_id: AttachmentId,
        limit: usize,
    ) -> Result<MediaPageAround, Error> {
        validate_limit(limit)?;
        let store = self.lock()?;
        let anchors = anchor_rows(
            &store.conn,
            conversation_id,
            attachment_id,
            Some(message_id),
        )?;
        let [anchor] = anchors.as_slice() else {
            return if anchors.is_empty() {
                Err(Error::NotFound)
            } else {
                Err(Error::InvalidRequest("media anchor ambiguous"))
            };
        };
        media_page_around_row(&store.conn, conversation_id, anchor.clone(), limit)
    }

    /// Pages strictly newer rows, closest to the exclusive cursor first.
    pub fn media_page_newer(
        &self,
        conversation_id: ConversationId,
        cursor: &MediaCursor,
        limit: usize,
    ) -> Result<MediaPage, Error> {
        validate_limit(limit)?;
        let store = self.lock()?;
        let rows = newer_rows(&store.conn, conversation_id, cursor, limit + 1)?;
        let has_more = rows.len() > limit;
        let items = rows
            .into_iter()
            .take(limit)
            .map(|row| media_page_item(&store.conn, row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MediaPage {
            next_cursor: has_more
                .then(|| items.last().map(|item| item.cursor.clone()))
                .flatten(),
            items,
        })
    }
}

fn validate_limit(limit: usize) -> Result<(), Error> {
    if !(1..=crate::MAX_HISTORY_PAGE).contains(&limit) {
        return Err(Error::InvalidRequest("media page size"));
    }
    Ok(())
}

fn media_page_around_row(
    conn: &Connection,
    conversation_id: ConversationId,
    anchor: MediaRow,
    limit: usize,
) -> Result<MediaPageAround, Error> {
    let target = media_page_item(conn, anchor)?;
    let capacity = limit - 1;
    let newer = newer_rows(conn, conversation_id, &target.cursor, limit + 1)?;
    let older = older_rows(conn, conversation_id, &target.cursor, limit + 1)?;
    let preferred_newer = capacity / 2;
    let newer_count = newer.len().min(preferred_newer);
    let older_count = older.len().min(capacity - newer_count);
    let newer_count = newer_count
        + newer
            .len()
            .saturating_sub(newer_count)
            .min(capacity - newer_count - older_count);
    let older_count = older_count
        + older
            .len()
            .saturating_sub(older_count)
            .min(capacity - newer_count - older_count);
    let has_more_newer = newer.len() > newer_count;
    let has_more_older = older.len() > older_count;
    let newer = newer
        .into_iter()
        .take(newer_count)
        .map(|row| media_page_item(conn, row))
        .collect::<Result<Vec<_>, _>>()?;
    let older = older
        .into_iter()
        .take(older_count)
        .map(|row| media_page_item(conn, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MediaPageAround {
        newer_cursor: has_more_newer.then(|| {
            newer
                .last()
                .map(|item| item.cursor.clone())
                .unwrap_or_else(|| target.cursor.clone())
        }),
        older_cursor: has_more_older.then(|| {
            older
                .last()
                .map(|item| item.cursor.clone())
                .unwrap_or_else(|| target.cursor.clone())
        }),
        target,
        newer,
        older,
    })
}

fn anchor_rows(
    conn: &Connection,
    conversation_id: ConversationId,
    attachment_id: AttachmentId,
    message_id: Option<MessageId>,
) -> Result<Vec<MediaRow>, Error> {
    let mut query = conn.prepare(
        "SELECT m.payload,m.sort_at_ms,m.local_order,m.id,ma.position,ma.attachment_id \
         FROM messages m JOIN message_attachments ma ON ma.message_id=m.id \
         WHERE m.conversation_id=?1 AND ma.attachment_id=?2 AND (?3 IS NULL OR m.id=?3) \
         ORDER BY m.sort_at_ms,m.local_order,m.id,ma.position LIMIT 2",
    )?;
    query
        .query_map(
            params![
                conversation_id.to_string(),
                attachment_id.to_string(),
                message_id.map(|id| id.to_string())
            ],
            media_row,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn newer_rows(
    conn: &Connection,
    conversation_id: ConversationId,
    cursor: &MediaCursor,
    limit: usize,
) -> Result<Vec<MediaRow>, Error> {
    let mut query = conn.prepare(
        "SELECT m.payload,m.sort_at_ms,m.local_order,m.id,ma.position,ma.attachment_id \
         FROM messages m JOIN message_attachments ma ON ma.message_id=m.id \
         WHERE m.conversation_id=?1 AND (m.sort_at_ms>?2 OR (m.sort_at_ms=?2 AND \
         (m.local_order>?3 OR (m.local_order=?3 AND (m.id>?4 OR (m.id=?4 AND ma.position>?5)))))) \
         ORDER BY m.sort_at_ms,m.local_order,m.id,ma.position LIMIT ?6",
    )?;
    query
        .query_map(
            params![
                conversation_id.to_string(),
                cursor.sort_at_ms,
                cursor.local_order,
                cursor.message_id.to_string(),
                i64::from(cursor.attachment_position),
                i64::try_from(limit).map_err(|_| Error::InvalidRequest("media page size"))?
            ],
            media_row,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn older_rows(
    conn: &Connection,
    conversation_id: ConversationId,
    cursor: &MediaCursor,
    limit: usize,
) -> Result<Vec<MediaRow>, Error> {
    let mut query = conn.prepare(
        "SELECT m.payload,m.sort_at_ms,m.local_order,m.id,ma.position,ma.attachment_id \
         FROM messages m JOIN message_attachments ma ON ma.message_id=m.id \
         WHERE m.conversation_id=?1 AND (m.sort_at_ms<?2 OR (m.sort_at_ms=?2 AND \
         (m.local_order<?3 OR (m.local_order=?3 AND (m.id<?4 OR (m.id=?4 AND ma.position<?5)))))) \
         ORDER BY m.sort_at_ms DESC,m.local_order DESC,m.id DESC,ma.position DESC LIMIT ?6",
    )?;
    query
        .query_map(
            params![
                conversation_id.to_string(),
                cursor.sort_at_ms,
                cursor.local_order,
                cursor.message_id.to_string(),
                i64::from(cursor.attachment_position),
                i64::try_from(limit).map_err(|_| Error::InvalidRequest("media page size"))?
            ],
            media_row,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn media_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn media_page_item(conn: &Connection, row: MediaRow) -> Result<MediaPageItem, Error> {
    let (payload, sort_at_ms, local_order, message_id, position, attachment_id) = row;
    let message_id = parse(&message_id)?;
    let attachment_id = parse(&attachment_id)?;
    Ok(MediaPageItem {
        message: decode(payload.as_bytes())?,
        attachment: attachment_row(conn, attachment_id)?
            .ok_or(Error::Database)?
            .info(attachment_id),
        media_meta: media_metadata_for(conn, message_id, attachment_id)?,
        cursor: MediaCursor {
            sort_at_ms,
            local_order,
            message_id,
            attachment_position: u32::try_from(position).map_err(|_| Error::Database)?,
        },
    })
}
