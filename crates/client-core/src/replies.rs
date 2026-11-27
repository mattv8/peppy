//! Reply context: who a message replies to, and the quoted excerpt of that target.

use super::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageReplySummary {
    pub message_id: MessageId,
    pub sender_self: bool,
    pub sender_address: Option<String>,
    pub excerpt: Option<String>,
    pub available: bool,
}

impl Client {
    /// Summarizes the message `message` replies to. `None` when it is not a reply; a target that
    /// is not loaded is reported as unavailable.
    pub fn message_reply_summary(
        &self,
        message: &MessagePayload,
    ) -> Result<Option<MessageReplySummary>, Error> {
        let Some(target) = message.reply_to else {
            return Ok(None);
        };
        Ok(self
            .message_reply_summaries(std::slice::from_ref(&message))?
            .remove(&target))
    }

    /// Summaries of every target the `messages` reply to, keyed by target id. Each distinct target
    /// is read once, from its stored payload alone (no send evidence or reactions). A target that
    /// is not loaded is reported as unavailable. Messages that are not replies add no entry.
    pub fn message_reply_summaries(
        &self,
        messages: &[&MessagePayload],
    ) -> Result<HashMap<MessageId, MessageReplySummary>, Error> {
        let mut seen = HashSet::new();
        let targets: Vec<MessageId> = messages
            .iter()
            .filter_map(|message| message.reply_to)
            .filter(|target| seen.insert(*target))
            .collect();
        if targets.is_empty() {
            return Ok(HashMap::new());
        }
        let store = self.lock()?;
        let stored = stored_reply_targets(&store.conn, &targets)?;
        Ok(targets
            .into_iter()
            .map(|target| (target, summarize(target, stored.get(&target))))
            .collect())
    }
}

/// Loads every target in one statement. Confirmed-cancelled targets are unavailable, as they are
/// for the timeline.
fn stored_reply_targets(
    conn: &Connection,
    targets: &[MessageId],
) -> Result<HashMap<MessageId, MessagePayload>, Error> {
    let ids: Vec<String> = targets.iter().map(ToString::to_string).collect();
    let json = serde_json::to_string(&ids).map_err(|_| Error::Database)?;
    let mut statement = conn.prepare(
        "SELECT m.id,m.payload FROM messages m
         WHERE m.id IN (SELECT value FROM json_each(?1))
           AND NOT EXISTS(SELECT 1 FROM send_cancellations x WHERE x.message_id=m.id AND x.state='confirmed')",
    )?;
    let rows = statement
        .query_map([json], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, payload)| {
            let target: MessageId = crate::parse(&id)?;
            Ok((target, decode(payload.as_bytes())?))
        })
        .collect()
}

fn summarize(target: MessageId, stored: Option<&MessagePayload>) -> MessageReplySummary {
    let Some(payload) = stored else {
        return MessageReplySummary {
            message_id: target,
            sender_self: false,
            sender_address: None,
            excerpt: None,
            available: false,
        };
    };
    MessageReplySummary {
        message_id: target,
        sender_self: payload.direction == Direction::Outgoing,
        sender_address: payload.sender_address.clone(),
        excerpt: Some(quote_excerpt(&payload.body)).filter(|excerpt| !excerpt.is_empty()),
        available: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaction_tests::{Fixture, seed_message};

    fn reply_to(fixture: &Fixture, target: MessageId) -> MessagePayload {
        let mut payload = fixture
            .client
            .message_by_id(target)
            .unwrap()
            .unwrap()
            .payload;
        payload.reply_to = Some(target);
        payload
    }

    #[test]
    fn reply_summary_is_none_without_a_reply_target() {
        let fixture = Fixture::new();
        let target = seed_message(
            &fixture,
            Transport::Sms,
            Direction::Incoming,
            "hi",
            ConversationId::new(),
        );
        let mut payload = reply_to(&fixture, target);
        payload.reply_to = None;
        assert_eq!(
            fixture.client.message_reply_summary(&payload).unwrap(),
            None
        );
    }

    #[test]
    fn reply_summary_describes_an_incoming_target() {
        let fixture = Fixture::new();
        let target = seed_message(
            &fixture,
            Transport::Sms,
            Direction::Incoming,
            "Running late,  start without me",
            ConversationId::new(),
        );
        let payload = reply_to(&fixture, target);
        assert_eq!(
            fixture.client.message_reply_summary(&payload).unwrap(),
            Some(MessageReplySummary {
                message_id: target,
                sender_self: false,
                sender_address: Some("+15550100".to_owned()),
                excerpt: Some("Running late, start without me".to_owned()),
                available: true,
            })
        );
    }

    #[test]
    fn reply_summary_describes_an_outgoing_target() {
        let fixture = Fixture::new();
        let target = seed_message(
            &fixture,
            Transport::Sms,
            Direction::Outgoing,
            "On my way",
            ConversationId::new(),
        );
        let payload = reply_to(&fixture, target);
        let summary = fixture
            .client
            .message_reply_summary(&payload)
            .unwrap()
            .unwrap();
        assert!(summary.sender_self);
        assert_eq!(summary.sender_address, None);
        assert_eq!(summary.excerpt.as_deref(), Some("On my way"));
        assert!(summary.available);
    }

    #[test]
    fn reply_summary_has_no_excerpt_for_an_empty_target_body() {
        let fixture = Fixture::new();
        let target = seed_message(
            &fixture,
            Transport::Sms,
            Direction::Incoming,
            "",
            ConversationId::new(),
        );
        let payload = reply_to(&fixture, target);
        let summary = fixture
            .client
            .message_reply_summary(&payload)
            .unwrap()
            .unwrap();
        assert_eq!(summary.excerpt, None);
        assert!(summary.available);
    }

    thread_local! {
        static STATEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn count_statement(_sql: &str) {
        STATEMENTS.with(|count| count.set(count.get() + 1));
    }

    #[test]
    fn a_page_reads_each_distinct_target_in_one_statement() {
        let fixture = Fixture::new();
        let first = seed_message(
            &fixture,
            Transport::Sms,
            Direction::Incoming,
            "Running late",
            ConversationId::new(),
        );
        let second = seed_message(
            &fixture,
            Transport::Sms,
            Direction::Outgoing,
            "On my way",
            ConversationId::new(),
        );
        let replies: Vec<MessagePayload> = (0..60)
            .map(|index| reply_to(&fixture, if index % 3 == 0 { second } else { first }))
            .collect();
        let page: Vec<&MessagePayload> = replies.iter().collect();

        fixture
            .client
            .lock()
            .unwrap()
            .conn
            .trace(Some(count_statement));
        STATEMENTS.with(|count| count.set(0));
        let summaries = fixture.client.message_reply_summaries(&page).unwrap();
        let statements = STATEMENTS.with(std::cell::Cell::get);
        fixture.client.lock().unwrap().conn.trace(None);

        assert_eq!(statements, 1, "one statement loads all distinct targets");
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[&first].excerpt.as_deref(), Some("Running late"));
        assert!(summaries[&second].sender_self);
        assert!(summaries.values().all(|summary| summary.available));
    }

    #[test]
    fn a_page_without_replies_adds_no_summaries() {
        let fixture = Fixture::new();
        let plain = fixture
            .client
            .message_by_id(seed_message(
                &fixture,
                Transport::Sms,
                Direction::Incoming,
                "hi",
                ConversationId::new(),
            ))
            .unwrap()
            .unwrap()
            .payload;
        assert!(
            fixture
                .client
                .message_reply_summaries(&[&plain])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reply_summary_reports_a_missing_target_as_unavailable() {
        let fixture = Fixture::new();
        let mut payload = reply_to(
            &fixture,
            seed_message(
                &fixture,
                Transport::Sms,
                Direction::Incoming,
                "hi",
                ConversationId::new(),
            ),
        );
        let missing = MessageId::new();
        payload.reply_to = Some(missing);
        assert_eq!(
            fixture.client.message_reply_summary(&payload).unwrap(),
            Some(MessageReplySummary {
                message_id: missing,
                sender_self: false,
                sender_address: None,
                excerpt: None,
                available: false,
            })
        );
    }
}
