use crate::{
    Client, CommandId, Connection, ConversationId, DeviceId, Error, GatewayRoute, MessageId,
    MessagePayload, QueuedSend, Transport, compose_transport, decode, notifications, parse,
    queue_message, rcs_group, require_subscription, validate_outgoing,
    validate_outgoing_mms_content,
};
use rusqlite::{OptionalExtension, TransactionBehavior};

/// A notification-authored text command pinned to a conversation, route, and displayed group
/// revision. Recipients are always resolved from the current conversation in core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickReplyInput {
    pub command_id: CommandId,
    pub conversation_id: ConversationId,
    pub body: String,
    pub route: GatewayRoute,
    pub expected_transport: Transport,
    pub expected_member_revision: Option<u64>,
}

/// Read-only conversation data captured when a notification reply becomes actionable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickReplyContext {
    pub recipients: Vec<String>,
    pub transport: Transport,
    pub expected_member_revision: Option<u64>,
}

impl Client {
    /// Resolves the authoritative conversation state without creating or changing a draft.
    pub fn quick_reply_context(
        &self,
        conversation_id: ConversationId,
    ) -> Result<QuickReplyContext, Error> {
        let guard = self.lock()?;
        let recipients = current_recipients(&guard.conn, conversation_id)?;
        Ok(QuickReplyContext {
            transport: reply_transport(&recipients, ""),
            recipients,
            expected_member_revision: crate::group_metadata::member_revision(
                &guard.conn,
                conversation_id,
            )?,
        })
    }

    /// Returns a previous queue result for the exact command binding, before any mutable policy
    /// preflight. A mismatched binding is never safe to retry.
    pub fn reconcile_quick_reply(
        &self,
        input: &QuickReplyInput,
    ) -> Result<Option<QueuedSend>, Error> {
        let guard = self.lock()?;
        existing_queued_send(&guard.conn, input)
    }

    /// Queues a draft-independent reply. Reusing the same command ID returns the original queued
    /// command only when its durable message binding exactly matches this input.
    pub fn queue_quick_reply(&self, input: QuickReplyInput) -> Result<QueuedSend, Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(queued) = existing_queued_send(&tx, &input)? {
            tx.commit()?;
            return Ok(queued);
        }
        if crate::redirect_target(&tx, input.conversation_id.to_string(), false)?.is_some() {
            return Err(Error::InvalidRequest("conversation moved"));
        }

        let recipients = current_recipients(&tx, input.conversation_id)?;
        let transport = reply_transport(&recipients, &input.body);
        if transport != input.expected_transport {
            return Err(Error::InvalidRequest("quick reply transport changed"));
        }

        let message = crate::OutgoingSms {
            conversation_id: input.conversation_id,
            recipients,
            body: input.body,
        };
        if message.body.trim().is_empty() {
            return Err(Error::InvalidRequest("quick reply body"));
        }
        if transport == Transport::Sms {
            validate_outgoing(&message, &input.route)?;
        } else {
            validate_outgoing_mms_content(&message.recipients, &message.body, &[], None)?;
            match &input.route {
                GatewayRoute::Carrier { .. } => require_subscription(&input.route)?,
                GatewayRoute::RcsLink { .. } => {}
            }
        }

        let group = if message.recipients.len() > 1 {
            if rcs_group::group_is_withdrawn(&tx, input.conversation_id)? {
                return Err(Error::GroupMembersChanged);
            }
            rcs_group::prepare_quick_reply_group_send(
                &tx,
                &ctx,
                input.conversation_id,
                &message.recipients,
                input.expected_member_revision,
                notifications::now_ms(),
            )?
        } else {
            None
        };
        rcs_group::record_quick_reply_expected_revision(
            &tx,
            input.command_id,
            input.expected_member_revision,
        )?;
        if let Some(fields) = &group {
            rcs_group::record_send_fields(&tx, input.command_id, fields)?;
        }
        let queued = queue_message(
            &tx,
            &ctx,
            message,
            &input.route,
            Vec::new(),
            transport,
            None,
            Some(input.command_id),
            group,
        )?;
        tx.commit()?;
        Ok(queued)
    }
}

/// A quick reply is text alone, so it is queued by the composer's rule for a text.
fn reply_transport(recipients: &[String], body: &str) -> Transport {
    const NO_ATTACHMENTS: bool = false;
    compose_transport(recipients, body, NO_ATTACHMENTS)
}

fn current_recipients(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Vec<String>, Error> {
    let roster = rcs_group::roster(conn, conversation_id)?;
    if !roster.is_empty() {
        return Ok(roster.into_iter().collect());
    }
    let payload: String = conn
        .query_row(
            "SELECT payload FROM messages WHERE conversation_id=? ORDER BY sort_at_ms DESC,local_order DESC LIMIT 1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let payload: MessagePayload = decode(payload.as_bytes())?;
    let region = crate::conversation_match::identity_region(conn)?;
    let mut own_addresses = crate::conversation_match::OwnAddressCache::new();
    let recipients = crate::conversation_match::message_recipient_set(
        conn,
        &payload,
        Some(&region),
        &mut own_addresses,
    )?;
    match recipients {
        Some(recipients) => Ok(recipients.into_iter().collect()),
        None if payload.transport == Transport::Mms => {
            crate::mms_identity::mms_reply_context_for(conn, conversation_id)
                .map(|context| context.recipients)
        }
        None => Err(Error::InvalidRequest("conversation recipients unavailable")),
    }
}

fn existing_queued_send(
    conn: &Connection,
    input: &QuickReplyInput,
) -> Result<Option<QueuedSend>, Error> {
    let command_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM commands WHERE command_id=?)",
        [input.command_id.to_string()],
        |row| row.get(0),
    )?;
    if !command_exists {
        return Ok(None);
    }
    let existing: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT c.message_id,c.gateway_device_id,o.envelope_id,o.route FROM commands c JOIN outbox o ON o.command_id=c.command_id WHERE c.command_id=?",
            [input.command_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (message_id, gateway_device_id, envelope_id, route) =
        existing.ok_or(Error::ExistingCommandConflict)?;
    let message_id: MessageId = parse(&message_id)?;
    let payload: String = conn
        .query_row(
            "SELECT payload FROM messages WHERE id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::Database)?;
    let payload: MessagePayload = crate::decode(payload.as_bytes())?;
    let expected_gateway = match &input.route {
        GatewayRoute::Carrier {
            gateway_device_id, ..
        }
        | GatewayRoute::RcsLink { gateway_device_id } => gateway_device_id,
    };
    let stored_gateway: DeviceId = parse(&gateway_device_id)?;
    let stored_route: GatewayRoute = serde_json::from_str(&route).map_err(|_| Error::Database)?;
    let stored_group = rcs_group::send_fields(conn, input.command_id)?;
    let original_expected_member_revision =
        rcs_group::quick_reply_expected_revision(conn, input.command_id)?
            .unwrap_or(stored_group.expected_member_revision);
    if payload.record.conversation_id != input.conversation_id
        || payload.body != input.body
        || payload.transport != input.expected_transport
        || stored_gateway != *expected_gateway
        || stored_route != input.route
        || original_expected_member_revision != input.expected_member_revision
    {
        return Err(Error::ExistingCommandConflict);
    }
    Ok(Some(QueuedSend {
        message_id,
        command_id: input.command_id,
        envelope_id: parse(&envelope_id)?,
    }))
}
