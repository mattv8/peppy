//! Google Messages copies every chat message into the phone's message store. The copy and the
//! link's record of the same message carry one Google message id, so whichever reaches Peppy
//! second can be recognised and dropped.
//!
//! An id is trusted only inside one conversation and only across the two sources: a copy is
//! matched to a link message and a link message to a copy, never link to link or copy to copy.
//! Each source records every id it sees under its own identity for the message, whether or not
//! it stores that message, so an id either source reports for two different messages is found
//! out and then not trusted at all.

use crate::{ConversationId, Error, MessageId};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};

const MIRRORED_ROW_PREFIX: &str = "proto:";
const GOOGLE_MESSAGES_PACKAGE: &str = "com.google.android.apps.messaging.";
const MAX_MIRROR_ID_BYTES: usize = 128;
const LENGTH_DELIMITED: u64 = 2;

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rcs_mirror_ids(
           mirror_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           source TEXT NOT NULL CHECK(source IN ('link','capture')),
           message_id TEXT NOT NULL,
           conflicted INTEGER NOT NULL DEFAULT 0 CHECK(conflicted IN (0,1)),
           recorded_at_ms INTEGER NOT NULL,
           PRIMARY KEY(mirror_id,conversation_id,source)
         );",
    )?;
    Ok(())
}

/// The Google message id inside a mirrored row's transaction id, or `None` for any other row.
pub(crate) fn mirrored_message_id(transaction_id: &str) -> Option<String> {
    let encoded = transaction_id.strip_prefix(MIRRORED_ROW_PREFIX)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let envelope = field(&bytes, 1)?;
    if field(envelope, 1)? != GOOGLE_MESSAGES_PACKAGE.as_bytes() {
        return None;
    }
    let id = field(field(field(envelope, 2)?, 2)?, 5)?;
    valid_mirror_id(id).then(|| String::from_utf8_lossy(id).into_owned())
}

fn valid_mirror_id(id: &[u8]) -> bool {
    !id.is_empty() && id.len() <= MAX_MIRROR_ID_BYTES && id.iter().all(u8::is_ascii_graphic)
}

/// The first length-delimited protobuf field numbered `number`. Input that is malformed before
/// that field yields `None`; what follows the field is not read.
fn field(mut message: &[u8], number: u64) -> Option<&[u8]> {
    while !message.is_empty() {
        let key = varint(&mut message)?;
        if key & 7 != LENGTH_DELIMITED || key >> 3 == 0 {
            return None;
        }
        let length = usize::try_from(varint(&mut message)?).ok()?;
        if length > message.len() {
            return None;
        }
        let (value, rest) = message.split_at(length);
        if key >> 3 == number {
            return Some(value);
        }
        message = rest;
    }
    None
}

fn varint(message: &mut &[u8]) -> Option<u64> {
    const LAST_BYTE_SHIFT: u32 = 63;
    let mut value = 0u64;
    for shift in (0..=LAST_BYTE_SHIFT).step_by(7) {
        let (&byte, rest) = message.split_first()?;
        *message = rest;
        let overflows = shift == LAST_BYTE_SHIFT && byte > 1;
        if overflows {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// Whether the link saw a message under this id and stored nothing for it, because the capture
/// already held the text.
pub(crate) fn link_left_message_to_capture(
    conn: &Connection,
    google_message_id: &str,
    conversation_id: ConversationId,
) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_mirror_ids i WHERE i.mirror_id=? AND i.conversation_id=? AND i.source='link'
           AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.id=i.message_id))",
        params![google_message_id, conversation_id.to_string()],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Whether a stored message is text alone. Two copies of a message are matched only when both
/// are, since copies of a picture message can hold different media.
pub(crate) fn is_text_only(conn: &Connection, message_id: MessageId) -> Result<bool, Error> {
    let payload: String = conn.query_row(
        "SELECT payload FROM messages WHERE id=?",
        [message_id.to_string()],
        |row| row.get(0),
    )?;
    let payload: crate::MessagePayload = crate::decode(payload.as_bytes())?;
    let awaits_link_media: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_media_receive WHERE message_id=?)",
        [message_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(payload.record.attachments.is_empty() && !awaits_link_media)
}

/// Which side stored a message: the link, or the capture of the phone's message store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorSource {
    Link,
    Capture,
}

impl MirrorSource {
    fn code(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Capture => "capture",
        }
    }
}

/// A Google message id as one source reported it for a message of one conversation.
#[derive(Clone, Copy)]
pub(crate) struct MirrorKey<'a> {
    pub mirror_id: &'a str,
    pub conversation_id: ConversationId,
    pub source: MirrorSource,
}

/// Remembers the source's own identity for the message it saw under a Google message id. The
/// same source reporting the id for a second message makes the id untrusted from then on.
pub(crate) fn record(
    conn: &Connection,
    key: MirrorKey<'_>,
    message_id: MessageId,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO rcs_mirror_ids(mirror_id,conversation_id,source,message_id,recorded_at_ms) VALUES(?,?,?,?,?)
         ON CONFLICT(mirror_id,conversation_id,source) DO UPDATE SET conflicted=1 WHERE message_id<>excluded.message_id",
        params![
            key.mirror_id,
            key.conversation_id.to_string(),
            key.source.code(),
            message_id.to_string(),
            crate::notifications::now_ms(),
        ],
    )?;
    Ok(())
}

/// The message a source stored under a Google message id, while neither source has made the id
/// untrusted and the message is still in that conversation.
pub(crate) fn message(conn: &Connection, key: MirrorKey<'_>) -> Result<Option<MessageId>, Error> {
    conn.query_row(
        "SELECT i.message_id FROM rcs_mirror_ids i JOIN messages m ON m.id=i.message_id AND m.conversation_id=i.conversation_id
          WHERE i.mirror_id=? AND i.conversation_id=? AND i.source=?
            AND NOT EXISTS(SELECT 1 FROM rcs_mirror_ids c WHERE c.mirror_id=i.mirror_id AND c.conversation_id=i.conversation_id AND c.conflicted=1)",
        params![
            key.mirror_id,
            key.conversation_id.to_string(),
            key.source.code()
        ],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|message_id| crate::parse(&message_id).map_err(|_| Error::Database))
    .transpose()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn length_delimited(number: u8, value: &[u8]) -> Vec<u8> {
        let mut encoded = vec![number << 3 | 2, u8::try_from(value.len()).unwrap()];
        encoded.extend_from_slice(value);
        encoded
    }

    /// A transaction id shaped like the one Google Messages writes for a mirrored chat row.
    pub(crate) fn mirrored_transaction_id(package: &str, message_id: &str) -> String {
        let mut identity = length_delimited(2, &length_delimited(5, message_id.as_bytes()));
        identity.extend(length_delimited(5, &[7; 16]));
        identity.extend(length_delimited(6, &[9; 16]));
        let mut envelope = length_delimited(1, package.as_bytes());
        envelope.extend(length_delimited(2, &identity));
        format!(
            "proto:{}",
            base64::engine::general_purpose::STANDARD.encode(length_delimited(1, &envelope))
        )
    }

    #[test]
    fn a_mirrored_row_names_its_google_message_id() {
        let transaction_id =
            mirrored_transaction_id(GOOGLE_MESSAGES_PACKAGE, "MxAbCdEfGhIjKlMnOpQr=StU");

        assert_eq!(
            mirrored_message_id(&transaction_id).as_deref(),
            Some("MxAbCdEfGhIjKlMnOpQr=StU")
        );
    }

    #[test]
    fn rows_that_are_not_mirrored_chat_messages_have_no_google_message_id() {
        let truncated = mirrored_transaction_id(GOOGLE_MESSAGES_PACKAGE, "MxAbCdEf");
        for transaction_id in [
            "",
            "peppy-5e4d0b62-3c9a-4b07-9d7e-3d1c5f6a7b8c",
            "T18c2a9f3b1",
            "proto:",
            "proto:not base64",
            &mirrored_transaction_id("com.example.other.", "MxAbCdEfGhIjKlMnOpQr=StU"),
            &mirrored_transaction_id(GOOGLE_MESSAGES_PACKAGE, ""),
            &mirrored_transaction_id(GOOGLE_MESSAGES_PACKAGE, "has space"),
            &truncated[..truncated.len() - 8],
        ] {
            assert_eq!(
                mirrored_message_id(transaction_id),
                None,
                "{transaction_id}"
            );
        }
    }

    #[test]
    fn a_number_too_large_for_the_encoding_is_not_read_as_a_smaller_one() {
        let transaction_id =
            mirrored_transaction_id(GOOGLE_MESSAGES_PACKAGE, "MxAbCdEfGhIjKlMnOpQr=StU");
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(transaction_id.strip_prefix("proto:").unwrap())
            .unwrap();
        // The first key, 0x0a, written as ten bytes whose last one overflows 64 bits.
        let mut overflowing_key = vec![0x8a];
        overflowing_key.extend([0x80; 8]);
        overflowing_key.push(0x02);
        bytes.splice(0..1, overflowing_key);
        let overflowing = format!(
            "proto:{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        let field_zero = format!(
            "proto:{}",
            base64::engine::general_purpose::STANDARD.encode([0x02, 0x00])
        );

        assert_eq!(mirrored_message_id(&overflowing), None);
        assert_eq!(mirrored_message_id(&field_zero), None);
    }

    fn store_with_message(conversation_id: ConversationId, message_id: MessageId) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        conn.execute_batch("CREATE TABLE messages(id TEXT PRIMARY KEY, conversation_id TEXT);")
            .unwrap();
        add_message(&conn, conversation_id, message_id);
        conn
    }

    fn add_message(conn: &Connection, conversation_id: ConversationId, message_id: MessageId) {
        conn.execute(
            "INSERT INTO messages(id,conversation_id) VALUES(?,?)",
            params![message_id.to_string(), conversation_id.to_string()],
        )
        .unwrap();
    }

    fn key(conversation_id: ConversationId, source: MirrorSource) -> MirrorKey<'static> {
        MirrorKey {
            mirror_id: "MxShared",
            conversation_id,
            source,
        }
    }

    #[test]
    fn an_id_is_known_only_for_its_conversation_and_source() {
        let (conversation, message_id) = (ConversationId::new(), MessageId::new());
        let conn = store_with_message(conversation, message_id);

        record(&conn, key(conversation, MirrorSource::Link), message_id).unwrap();

        assert_eq!(
            message(&conn, key(conversation, MirrorSource::Link)).unwrap(),
            Some(message_id)
        );
        assert_eq!(
            message(&conn, key(conversation, MirrorSource::Capture)).unwrap(),
            None
        );
        assert_eq!(
            message(&conn, key(ConversationId::new(), MirrorSource::Link)).unwrap(),
            None
        );
    }

    #[test]
    fn an_id_one_source_reports_for_two_messages_is_no_longer_trusted() {
        let (conversation, first, second) =
            (ConversationId::new(), MessageId::new(), MessageId::new());
        let conn = store_with_message(conversation, first);
        add_message(&conn, conversation, second);
        let link = key(conversation, MirrorSource::Link);

        record(&conn, link, first).unwrap();
        record(&conn, link, first).unwrap();
        assert_eq!(message(&conn, link).unwrap(), Some(first));
        record(&conn, link, second).unwrap();

        assert_eq!(message(&conn, link).unwrap(), None);
    }

    #[test]
    fn an_id_untrusted_for_one_source_is_untrusted_for_the_other() {
        let (conversation, captured) = (ConversationId::new(), MessageId::new());
        let conn = store_with_message(conversation, captured);
        let (link, capture) = (
            key(conversation, MirrorSource::Link),
            key(conversation, MirrorSource::Capture),
        );
        record(&conn, capture, captured).unwrap();
        record(&conn, link, MessageId::new()).unwrap();
        assert_eq!(message(&conn, capture).unwrap(), Some(captured));

        record(&conn, link, MessageId::new()).unwrap();

        assert_eq!(message(&conn, capture).unwrap(), None);
    }

    #[test]
    fn the_link_leaves_a_message_to_the_capture_when_it_stored_nothing_for_it() {
        let (conversation, stored) = (ConversationId::new(), MessageId::new());
        let conn = store_with_message(conversation, stored);
        let link = key(conversation, MirrorSource::Link);
        assert!(!link_left_message_to_capture(&conn, "MxShared", conversation).unwrap());

        record(&conn, link, MessageId::new()).unwrap();
        assert!(link_left_message_to_capture(&conn, "MxShared", conversation).unwrap());

        let other = MirrorKey {
            mirror_id: "MxStored",
            ..link
        };
        record(&conn, other, stored).unwrap();
        assert!(!link_left_message_to_capture(&conn, "MxStored", conversation).unwrap());
    }

    #[test]
    fn an_id_whose_message_is_gone_is_not_known() {
        let (conversation, message_id) = (ConversationId::new(), MessageId::new());
        let conn = store_with_message(conversation, message_id);
        record(&conn, key(conversation, MirrorSource::Link), message_id).unwrap();

        conn.execute("DELETE FROM messages", []).unwrap();

        assert_eq!(
            message(&conn, key(conversation, MirrorSource::Link)).unwrap(),
            None
        );
    }
}
