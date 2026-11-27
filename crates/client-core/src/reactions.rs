//! Message reactions: requester API (route, projections, dismissal) and phone-side jobs
//! (capability, attempts, outcomes, and publication of the RCS reaction set).
//!
//! A request is one row per request id, written by the requester and answered by the owning
//! gateway's phone with a `ReactionResult`. The phone may only act on a request it received
//! live, for its active key epoch, inside the request window, and once per request id.

use super::*;
use crate::reaction_validation::{MAX_ACTORS_PER_REACTION, valid_emoji, valid_message_reactions};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// A requester's own stale `sending` request is shown and dismissed as unknown after this.
const DISMISS_AFTER_MS: i64 = REACTION_REQUEST_TTL_MS + REACTION_REQUEST_SKEW_MS;
/// Provider message timestamps can be ahead of the local request clock.
const FAILED_REACTION_SKEW_MS: i64 = 300_000;

/// How the requester can reach a message: a native RCS reaction or an ordinary text reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionRoute {
    Native,
    Text,
}

/// What a conversation can do for reactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionSupport {
    Native,
    Text,
    NeedsPhoneUpdate,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionSupportView {
    pub support: ReactionSupport,
    /// Gateway the route uses: the RCS gateway for `Native` and `NeedsPhoneUpdate`. `None` for
    /// text, whose carrier gateway and subscription come from the composer's route.
    pub gateway_id: Option<DeviceId>,
}

/// Routing facts for a conversation, read together from the local store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionRouting {
    pub spam: bool,
    pub support: ReactionSupportView,
    pub stored_route: Option<GatewayRoute>,
}

/// This device's latest request for a message whose state is still open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnReactionState {
    Sending,
    NotSent,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnReactionRequest {
    pub request_id: ReactionRequestId,
    /// `None` asks for the user's reaction to be removed.
    pub emoji: Option<String>,
    /// The user's emoji in the current set, if any.
    pub previous_emoji: Option<String>,
    pub state: OwnReactionState,
    pub refusal: Option<ReactionRefusal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitRefusal {
    Offline,
    Unsupported,
    NeedsPhoneUpdate,
    Busy,
    NotReactable,
    InvalidEmoji,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionSubmit {
    pub accepted: bool,
    pub request_id: Option<ReactionRequestId>,
    pub route: Option<ReactionRoute>,
    pub reason: Option<SubmitRefusal>,
}

/// One reaction entry as the gateway read it from the RCS link, before core canonicalises it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsReactionEntry {
    pub emoji: String,
    pub is_self: bool,
    pub addresses: Vec<String>,
    pub unresolved_actors: u32,
}

fn refused(reason: SubmitRefusal) -> ReactionSubmit {
    ReactionSubmit {
        accepted: false,
        request_id: None,
        route: None,
        reason: Some(reason),
    }
}

impl Client {
    // ---- Phone side -------------------------------------------------------------------------

    pub fn publish_reaction_capability(&self, supported: bool) -> Result<(), Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<bool> = tx
            .query_row(
                "SELECT supported FROM reaction_capabilities WHERE device_id=? AND key_epoch=?",
                params![ctx.device_id.to_string(), epoch],
                |row| row.get(0),
            )
            .optional()?;
        if previous != Some(supported) {
            let (_, sequence) = enqueue(
                &tx,
                &ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::ReactionCapability { supported },
            )?;
            tx.execute(
                "INSERT INTO reaction_capabilities(device_id,supported,key_epoch,producer_sequence) \
                 VALUES(?,?,?,?) ON CONFLICT(device_id) DO UPDATE SET \
                 supported=excluded.supported,key_epoch=excluded.key_epoch, \
                 producer_sequence=excluded.producer_sequence",
                params![ctx.device_id.to_string(), supported, epoch, sequence as i64],
            )?;
        }
        Ok(tx.commit()?)
    }

    /// Requests this phone may act on now: live, for the active epoch, live (not from a snapshot),
    /// and not yet attempted. Requests that have expired or belong to a retired epoch are
    /// answered `NotSent { expired }` here and never attempted.
    pub fn pending_reaction_jobs(&self) -> Result<Vec<ReactionJob>, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
        let now = notifications::now_ms();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_unfinished_attempts(&tx, &ctx)?;
        let device = ctx.device_id.to_string();
        let expired: Vec<(String, String)> = tx
            .prepare(
                "SELECT request_id,message_id FROM reaction_requests
                 WHERE target_gateway_id=? AND state='sending' AND from_snapshot=0
                 AND (requested_at_ms<=? OR requested_at_ms>? OR key_epoch<?)",
            )?
            .query_map(
                params![
                    device,
                    now - REACTION_REQUEST_TTL_MS,
                    now + REACTION_REQUEST_SKEW_MS,
                    epoch
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
            .collect::<Result<_, _>>()?;
        for (request, message) in expired {
            record_reaction_result(
                &tx,
                &ctx,
                parse(&request)?,
                parse(&message)?,
                ReactionResultState::NotSent {
                    reason: ReactionRefusal::Expired,
                },
            )?;
        }
        let jobs = tx
            .prepare(
                "SELECT r.request_id,r.message_id,r.conversation_id,r.emoji FROM reaction_requests r
                 WHERE r.target_gateway_id=? AND r.state='sending' AND r.from_snapshot=0 AND r.key_epoch=?
                 AND r.requested_at_ms>? AND r.requested_at_ms<=?
                 AND NOT EXISTS(SELECT 1 FROM reaction_attempts a WHERE a.request_id=r.request_id)
                 ORDER BY r.requested_at_ms,r.rowid",
            )?
            .query_map(
                params![device, epoch, now - REACTION_REQUEST_TTL_MS, now + REACTION_REQUEST_SKEW_MS],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )?
            .map(|row| -> Result<ReactionJob, Error> {
                let (request, message, conversation, emoji) = row?;
                Ok(ReactionJob {
                    request_id: parse(&request)?,
                    message_id: parse(&message)?,
                    conversation_id: parse(&conversation)?,
                    emoji,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        tx.commit()?;
        Ok(jobs)
    }

    /// Takes the one-use permit for a request. A second begin, or a begin after an attempt
    /// began, is always `Refused`; an attempt left unfinished is recorded `Unknown` first.
    pub fn begin_reaction_attempt(
        &self,
        request_id: ReactionRequestId,
    ) -> Result<ReactionAttemptDecision, Error> {
        let mut store = self.lock()?;
        ensure_recovery_complete(&store)?;
        let (conn, ctx) = store.parts();
        let epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
        let now = notifications::now_ms();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_unfinished_attempts(&tx, &ctx)?;
        let request = request_id.to_string();
        let device = ctx.device_id.to_string();
        let row: Option<(String, Option<String>, i64, u32)> = tx
            .query_row(
                "SELECT message_id,emoji,requested_at_ms,key_epoch FROM reaction_requests
                 WHERE request_id=? AND target_gateway_id=? AND state='sending' AND from_snapshot=0",
                params![request, device],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((message, desired, requested_at_ms, key_epoch)) = row else {
            let attempted: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM reaction_attempts WHERE request_id=?)",
                [&request],
                |row| row.get(0),
            )?;
            tx.commit()?;
            let reason = if attempted {
                ReactionRefusal::Busy
            } else {
                ReactionRefusal::Expired
            };
            return Ok(ReactionAttemptDecision::Refused { reason });
        };
        let message_id: MessageId = parse(&message)?;
        if requested_at_ms <= now - REACTION_REQUEST_TTL_MS
            || requested_at_ms > now + REACTION_REQUEST_SKEW_MS
            || key_epoch != epoch
        {
            record_reaction_result(
                &tx,
                &ctx,
                request_id,
                message_id,
                ReactionResultState::NotSent {
                    reason: ReactionRefusal::Expired,
                },
            )?;
            tx.commit()?;
            return Ok(ReactionAttemptDecision::Refused {
                reason: ReactionRefusal::Expired,
            });
        }
        if let Some(reason) = refusal_before_attempt(&tx, message_id)? {
            record_reaction_result(
                &tx,
                &ctx,
                request_id,
                message_id,
                ReactionResultState::NotSent {
                    reason: reason.clone(),
                },
            )?;
            tx.commit()?;
            return Ok(ReactionAttemptDecision::Refused { reason });
        }
        let identity: (String, String) = tx.query_row(
            "SELECT google_conversation_id,google_message_id FROM rcs_message_identities WHERE message_id=? LIMIT 1",
            [&message],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.execute(
            "INSERT INTO reaction_attempts(request_id,begun_at_ms) VALUES(?,?)",
            params![request, now],
        )?;
        let current = own_reaction(&tx, &message)?;
        let action = match (&desired, current.as_deref()) {
            (None, None) => ReactionAction::Noop,
            (None, Some(_)) => ReactionAction::Remove,
            (Some(_), None) => ReactionAction::Add,
            (Some(next), Some(current)) if next == current => ReactionAction::Noop,
            (Some(_), Some(_)) => ReactionAction::Switch,
        };
        let emoji = desired.or(current).unwrap_or_default();
        tx.commit()?;
        Ok(ReactionAttemptDecision::Permit {
            request_id,
            google_conversation_id: identity.0,
            google_message_id: identity.1,
            action,
            emoji,
        })
    }

    /// Finishes the one attempt for a request and publishes the result. A second outcome for the
    /// same attempt is ignored.
    pub fn record_reaction_outcome(
        &self,
        request_id: ReactionRequestId,
        outcome: ReactionResultState,
    ) -> Result<(), Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let request = request_id.to_string();
        let row: (String, Option<i64>) = tx
            .query_row(
                "SELECT r.message_id,a.finished_at_ms FROM reaction_attempts a JOIN reaction_requests r ON r.request_id=a.request_id
                 WHERE a.request_id=? AND r.target_gateway_id=?",
                params![request, ctx.device_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        if row.1.is_some() {
            return Ok(tx.commit()?);
        }
        tx.execute(
            "UPDATE reaction_attempts SET finished_at_ms=?,outcome=? WHERE request_id=?",
            params![
                notifications::now_ms(),
                reaction_result_state_code(&outcome),
                request
            ],
        )?;
        record_reaction_result(&tx, &ctx, request_id, parse(&row.0)?, outcome)?;
        Ok(tx.commit()?)
    }

    /// Publishes this gateway's view of one RCS message's reactions when it differs from the
    /// last published set. Returns whether a `MessageReactions` event was published.
    pub fn ingest_rcs_message_reactions(
        &self,
        google_conversation_id: &str,
        google_message_id: &str,
        reactions: Vec<RcsReactionEntry>,
        observed_at_ms: i64,
    ) -> Result<bool, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let conversation: Option<String> = tx
            .query_row(
                "SELECT conversation_id FROM rcs_message_identities WHERE google_message_id=? AND google_conversation_id=?",
                params![google_message_id, google_conversation_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(conversation) = conversation else {
            return Ok(false);
        };
        let conversation_id: ConversationId = parse(&conversation)?;
        let Some(message_id) = crate::rcs_mirror::message(
            &tx,
            crate::rcs_mirror::MirrorKey {
                mirror_id: google_message_id,
                conversation_id,
                source: crate::rcs_mirror::MirrorSource::Link,
            },
        )?
        else {
            return Ok(false);
        };
        let region = conversation_match::identity_region(&tx)?;
        let set = build_reaction_set(&reactions, &region)?;
        let stored: Option<(i64, String)> = tx
            .query_row(
                "SELECT observed_at_ms,reactions_json FROM message_reactions WHERE message_id=?",
                [message_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored_observed, stored_json)) = stored {
            let stored_set: Vec<MessageReaction> =
                serde_json::from_str(&stored_json).map_err(|_| Error::Database)?;
            if observed_at_ms < stored_observed || stored_set == set {
                return Ok(false);
            }
        }
        let (_, sequence) = enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::MessageReactions {
                message_id,
                conversation_id,
                gateway_id: ctx.device_id,
                observed_at_ms,
                reactions: set.clone(),
            },
        )?;
        store_message_reactions(
            &tx,
            message_id,
            conversation_id,
            ctx.device_id,
            observed_at_ms,
            sequence,
            &set,
        )?;
        tx.commit()?;
        Ok(true)
    }

    // ---- Requester side ---------------------------------------------------------------------

    /// Route and support for reactions in a conversation. `Native` needs an eligible RCS
    /// conversation whose gateway reports the capability for the active epoch. Otherwise a
    /// conversation that has messages can take text reactions through the composer's route.
    pub fn reaction_support(
        &self,
        conversation_id: ConversationId,
    ) -> Result<ReactionSupportView, Error> {
        let store = self.lock()?;
        support_view(&store.conn, store.active_epoch, conversation_id)
    }

    /// Reads the folder, reaction support and composer's route under one store lock.
    pub fn reaction_routing(
        &self,
        conversation_id: ConversationId,
    ) -> Result<ReactionRouting, Error> {
        let store = self.lock()?;
        let id = conversation_id.to_string();
        let spam = store.conn.query_row(
            &format!("SELECT COALESCE((SELECT folder='spam' FROM conversation_folders WHERE conversation_id=({})),0)", crate::conversation_folders::winner_sql("?1")),
            [&id],
            |row| row.get(0),
        )?;
        let support = support_view(&store.conn, store.active_epoch, conversation_id)?;
        let stored_route = crate::load_compose_draft(&store.conn, "conversation_id", &id)?
            .and_then(|draft| draft.route);
        Ok(ReactionRouting {
            spam,
            support,
            stored_route,
        })
    }

    /// The newest failed own reaction, allowing for provider clock skew.
    pub fn latest_failed_reaction_request(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<ReactionRequestId>, Error> {
        let store = self.lock()?;
        let id = conversation_id.to_string();
        let now = notifications::now_ms();
        let mut query = store.conn.prepare(&format!(
            "SELECT r.request_id,r.state,r.requested_at_ms FROM reaction_requests r
             WHERE COALESCE((SELECT target_id FROM conversation_merges WHERE source_id=r.conversation_id),r.conversation_id)=?1
               AND r.requester_device_id=?2 AND r.state IN {OPEN_REQUEST_STATES}
               AND r.requested_at_ms + {FAILED_REACTION_SKEW_MS} > COALESCE((SELECT MAX(m.sort_at_ms) FROM messages m
                 WHERE m.effective_conversation_id=?1 AND NOT EXISTS(SELECT 1 FROM send_cancellations x WHERE x.message_id=m.id AND x.state='confirmed')),0)
             ORDER BY r.requested_at_ms DESC,r.rowid DESC"
        ))?;
        let mut rows = query.query(params![id, store.config.device_id.to_string()])?;
        while let Some(row) = rows.next()? {
            let state: String = row.get(1)?;
            let requested_at_ms: i64 = row.get(2)?;
            if displayed_state(&state, requested_at_ms, now) != OwnReactionState::Sending {
                return Ok(Some(parse(&row.get::<_, String>(0)?)?));
            }
        }
        Ok(None)
    }

    /// Reacts to a message. `gateway_online` is the presence of the route's gateway as the
    /// composer sees it (`ReactionSupportView::gateway_id` for native, the composer's route for
    /// text). `text_route` is the composer's route for the conversation; text reactions go
    /// through it as an ordinary outgoing message. A text reaction cannot remove.
    pub fn react_to_message(
        &self,
        message_id: MessageId,
        emoji: Option<String>,
        gateway_online: bool,
        text_route: Option<GatewayRoute>,
    ) -> Result<ReactionSubmit, Error> {
        if emoji.as_deref().is_some_and(|value| !valid_emoji(value)) {
            return Ok(refused(SubmitRefusal::InvalidEmoji));
        }
        let message = self.message_by_id(message_id)?.ok_or(Error::NotFound)?;
        let conversation_id = crate::conversation_merge::survivor(
            &self.lock()?.conn,
            message.payload.record.conversation_id,
        )?;
        let support = self.reaction_support(conversation_id)?;
        if message.payload.transport == Transport::Rcs
            && support.support == ReactionSupport::NeedsPhoneUpdate
        {
            return Ok(refused(SubmitRefusal::NeedsPhoneUpdate));
        }
        match message.reactable {
            None => Ok(refused(SubmitRefusal::NotReactable)),
            Some(ReactionRoute::Native) => {
                let gateway = support.gateway_id.ok_or(Error::Database)?;
                if !gateway_online {
                    return Ok(refused(SubmitRefusal::Offline));
                }
                self.queue_native_reaction(message_id, conversation_id, gateway, emoji)
            }
            Some(ReactionRoute::Text) => {
                let Some(emoji) = emoji else {
                    return Ok(refused(SubmitRefusal::Unsupported));
                };
                let Some(route) = text_route else {
                    return Ok(refused(SubmitRefusal::Unsupported));
                };
                if !gateway_online {
                    return Ok(refused(SubmitRefusal::Offline));
                }
                self.queue_text_reaction(
                    message_id,
                    conversation_id,
                    &message.payload.body,
                    &emoji,
                    &route,
                )
            }
        }
    }

    fn queue_native_reaction(
        &self,
        message_id: MessageId,
        conversation_id: ConversationId,
        gateway: DeviceId,
        emoji: Option<String>,
    ) -> Result<ReactionSubmit, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let message = message_id.to_string();
        let device = ctx.device_id.to_string();
        let busy: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM reaction_requests WHERE message_id=? AND requester_device_id=? AND state IN ('sending','unknown'))",
            params![message, device],
            |row| row.get(0),
        )?;
        if busy {
            tx.commit()?;
            return Ok(refused(SubmitRefusal::Busy));
        }
        let now = notifications::now_ms();
        tx.execute(
            "UPDATE reaction_requests SET state='dismissed',updated_at_ms=? WHERE message_id=? AND requester_device_id=? AND state='not_sent'",
            params![now, message, device],
        )?;
        let request_id = ReactionRequestId::new();
        tx.execute(
            "INSERT INTO reaction_requests(request_id,message_id,conversation_id,target_gateway_id,emoji,requested_at_ms,requester_device_id,key_epoch,state,updated_at_ms,from_snapshot)
             VALUES(?,?,?,?,?,?,?,?,'sending',?,0)",
            params![request_id.to_string(), message, conversation_id.to_string(), gateway.to_string(), emoji, now, device, epoch, now],
        )?;
        enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::ReactionRequest {
                request_id,
                message_id,
                conversation_id,
                target_gateway_id: gateway,
                emoji,
                requested_at_ms: now,
            },
        )?;
        tx.commit()?;
        Ok(ReactionSubmit {
            accepted: true,
            request_id: Some(request_id),
            route: Some(ReactionRoute::Native),
            reason: None,
        })
    }

    fn queue_text_reaction(
        &self,
        message_id: MessageId,
        conversation_id: ConversationId,
        quoted_body: &str,
        emoji: &str,
        route: &GatewayRoute,
    ) -> Result<ReactionSubmit, Error> {
        let recipients = self.reply_recipients(message_id)?;
        let text = compose_reaction_text(quoted_body, emoji);
        let transport = compose_transport(&recipients, &text, false);
        let valid = match transport {
            Transport::Sms => validate_outgoing(
                &OutgoingSms {
                    conversation_id,
                    recipients: recipients.clone(),
                    body: text.clone(),
                },
                route,
            ),
            _ => validate_outgoing_mms(&recipients, &text, &[], None, route),
        };
        if valid.is_err() {
            return Ok(refused(SubmitRefusal::Unsupported));
        }
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        queue_message(
            &tx,
            &ctx,
            OutgoingSms {
                conversation_id,
                recipients,
                body: text,
            },
            route,
            Vec::new(),
            transport,
            None,
            None,
            None,
            None,
        )?;
        tx.commit()?;
        Ok(ReactionSubmit {
            accepted: true,
            request_id: None,
            route: Some(ReactionRoute::Text),
            reason: None,
        })
    }

    /// Dismisses this device's own request that is not sent, unknown, or stale. Dismissal is
    /// local and never synced.
    pub fn dismiss_reaction_request(&self, request_id: ReactionRequestId) -> Result<(), Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = notifications::now_ms();
        let row: (String, i64) = tx
            .query_row(
                "SELECT state,requested_at_ms FROM reaction_requests WHERE request_id=? AND requester_device_id=?",
                params![request_id.to_string(), ctx.device_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let dismissable = match row.0.as_str() {
            "not_sent" | "unknown" => true,
            "sending" => row.1 <= now - DISMISS_AFTER_MS,
            _ => false,
        };
        if !dismissable {
            return Err(Error::InvalidRequest("reaction request is not dismissable"));
        }
        tx.execute(
            "UPDATE reaction_requests SET state='dismissed',updated_at_ms=? WHERE request_id=?",
            params![now, request_id.to_string()],
        )?;
        Ok(tx.commit()?)
    }
}

/// Reasons a phone refuses before it touches the carrier-facing link. `None` means the attempt
/// may proceed.
fn refusal_before_attempt(
    conn: &Connection,
    message_id: MessageId,
) -> Result<Option<ReactionRefusal>, Error> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload FROM messages WHERE id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(Some(ReactionRefusal::MessageUnavailable));
    };
    let payload: MessagePayload = decode(payload.as_bytes())?;
    if payload.transport != Transport::Rcs {
        return Ok(Some(ReactionRefusal::NotRcs));
    }
    let message = message_id.to_string();
    // The reaction sends the identity row's Google id, so an id the opaque-id rule rejects is refused
    // here, before the attempt is recorded. Every row is checked, not just the one the permit reads.
    let identities: Vec<String> = conn
        .prepare("SELECT google_message_id FROM rcs_message_identities WHERE message_id=?")?
        .query_map([&message], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if identities.is_empty()
        || identities
            .iter()
            .any(|id| !crate::rcs_receive::valid_google_message_id(id))
        || crate::rcs_send::has_conflicted_mirror(conn, &message)?
    {
        return Ok(Some(ReactionRefusal::MessageUnavailable));
    }
    if crate::rcs_mapping::current_link_state(conn)? != Some(true) {
        return Ok(Some(ReactionRefusal::LinkUnavailable));
    }
    Ok(None)
}

/// Requests whose attempt never finished (a restart or a crash mid-commit) are `Unknown`. The
/// result is published, and the request is never retried.
fn settle_unfinished_attempts(conn: &Connection, ctx: &Ctx) -> Result<(), Error> {
    let unfinished: Vec<(String, String)> = conn
        .prepare(
            "SELECT a.request_id,r.message_id FROM reaction_attempts a JOIN reaction_requests r ON r.request_id=a.request_id
             WHERE a.finished_at_ms IS NULL AND r.target_gateway_id=?",
        )?
        .query_map([ctx.device_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (request, message) in unfinished {
        conn.execute(
            "UPDATE reaction_attempts SET finished_at_ms=?,outcome='unknown' WHERE request_id=?",
            params![notifications::now_ms(), request],
        )?;
        record_reaction_result(
            conn,
            ctx,
            parse(&request)?,
            parse(&message)?,
            ReactionResultState::Unknown,
        )?;
    }
    Ok(())
}

/// Stores a published or received reaction set when it is newer than the stored one, and marks
/// any requester request it satisfies as sent. Returns whether the stored set changed.
pub(crate) fn store_message_reactions(
    conn: &Connection,
    message_id: MessageId,
    conversation_id: ConversationId,
    gateway_id: DeviceId,
    observed_at_ms: i64,
    producer_sequence: u64,
    reactions: &[MessageReaction],
) -> Result<bool, Error> {
    let json = serde_json::to_string(reactions).map_err(|_| Error::Database)?;
    let changed = conn.execute(
        "INSERT INTO message_reactions(message_id,conversation_id,gateway_id,observed_at_ms,producer_sequence,reactions_json)
         VALUES(?,?,?,?,?,?) ON CONFLICT(message_id) DO UPDATE SET conversation_id=excluded.conversation_id,gateway_id=excluded.gateway_id,
         observed_at_ms=excluded.observed_at_ms,producer_sequence=excluded.producer_sequence,reactions_json=excluded.reactions_json
         WHERE excluded.observed_at_ms>message_reactions.observed_at_ms OR
         (excluded.observed_at_ms=message_reactions.observed_at_ms AND excluded.producer_sequence>message_reactions.producer_sequence)",
        params![message_id.to_string(), conversation_id.to_string(), gateway_id.to_string(), observed_at_ms, producer_sequence as i64, json],
    )? != 0;
    prune_superseded_staged(conn, message_id)?;
    if changed {
        let self_emoji = reactions
            .iter()
            .find(|reaction| reaction.actors.iter().any(|actor| actor.is_self))
            .map(|reaction| reaction.emoji.clone())
            .unwrap_or_default();
        // A set satisfies only requests made before the set was observed; an earlier set that
        // happens to match is not evidence for a request still waiting for its phone.
        conn.execute(
            "UPDATE reaction_requests SET state='sent',refusal=NULL,updated_at_ms=?
             WHERE message_id=? AND state IN ('sending','unknown') AND requested_at_ms<=? AND COALESCE(emoji,'')=?",
            params![notifications::now_ms(), message_id.to_string(), observed_at_ms, self_emoji],
        )?;
    }
    Ok(changed)
}

/// The canonical set core publishes: one reaction per emoji, self first, then canonical
/// addresses, then unresolved actors. An entry with no actor is dropped.
fn build_reaction_set(
    entries: &[RcsReactionEntry],
    region: &str,
) -> Result<Vec<MessageReaction>, Error> {
    let mut reactions: Vec<MessageReaction> = Vec::new();
    for entry in entries {
        if !valid_emoji(&entry.emoji) || reactions.iter().any(|known| known.emoji == entry.emoji) {
            return Err(Error::InvalidRequest("reaction entry"));
        }
        let mut actors = Vec::new();
        if entry.is_self {
            actors.push(ReactionActor {
                is_self: true,
                address: None,
            });
        }
        let addresses: std::collections::BTreeSet<String> = entry
            .addresses
            .iter()
            .map(|address| crate::conversation_match::canonical_address(address, Some(region)))
            .filter(|address| !address.is_empty())
            .collect();
        actors.extend(addresses.into_iter().map(|address| ReactionActor {
            is_self: false,
            address: Some(address),
        }));
        actors.extend((0..entry.unresolved_actors).map(|_| ReactionActor {
            is_self: false,
            address: None,
        }));
        if actors.is_empty() {
            continue;
        }
        if actors.len() > MAX_ACTORS_PER_REACTION {
            return Err(Error::InvalidRequest("reaction actors"));
        }
        reactions.push(MessageReaction {
            emoji: entry.emoji.clone(),
            actors,
        });
    }
    reactions.sort_by(|left, right| left.emoji.cmp(&right.emoji));
    if !valid_message_reactions(&reactions) {
        return Err(Error::InvalidRequest("reaction set"));
    }
    Ok(reactions)
}

pub(crate) fn reaction_refusal_code(reason: &ReactionRefusal) -> &'static str {
    match reason {
        ReactionRefusal::Expired => "expired",
        ReactionRefusal::MessageUnavailable => "message_unavailable",
        ReactionRefusal::NotRcs => "not_rcs",
        ReactionRefusal::LinkUnavailable => "link_unavailable",
        ReactionRefusal::Rejected => "rejected",
        ReactionRefusal::Busy => "busy",
        ReactionRefusal::InvalidEmoji => "invalid_emoji",
    }
}

fn reaction_refusal_from_code(code: &str) -> Option<ReactionRefusal> {
    Some(match code {
        "expired" => ReactionRefusal::Expired,
        "message_unavailable" => ReactionRefusal::MessageUnavailable,
        "not_rcs" => ReactionRefusal::NotRcs,
        "link_unavailable" => ReactionRefusal::LinkUnavailable,
        "rejected" => ReactionRefusal::Rejected,
        "busy" => ReactionRefusal::Busy,
        "invalid_emoji" => ReactionRefusal::InvalidEmoji,
        _ => return None,
    })
}

pub(crate) fn reaction_result_state_code(state: &ReactionResultState) -> &'static str {
    match state {
        ReactionResultState::Sent => "sent",
        ReactionResultState::NotSent { .. } => "not_sent",
        ReactionResultState::Unknown => "unknown",
    }
}

/// The emoji this device's own actor holds in the stored set, if any.
pub(crate) fn own_reaction(conn: &Connection, message_id: &str) -> Result<Option<String>, Error> {
    let reactions: Option<String> = conn
        .query_row(
            "SELECT reactions_json FROM message_reactions WHERE message_id=?",
            [message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(reactions
        .and_then(|value| serde_json::from_str::<Vec<MessageReaction>>(&value).ok())
        .and_then(|reactions| {
            reactions
                .into_iter()
                .find(|reaction| reaction.actors.iter().any(|actor| actor.is_self))
                .map(|reaction| reaction.emoji)
        }))
}

/// Moves a request to its terminal state for this gateway and publishes the result. Only a
/// request still `sending` changes state, so an earlier result or a set cannot be rewritten.
pub(crate) fn record_reaction_result(
    conn: &Connection,
    ctx: &Ctx,
    request_id: ReactionRequestId,
    message_id: MessageId,
    state: ReactionResultState,
) -> Result<(), Error> {
    let (state_code, refusal) = match &state {
        ReactionResultState::NotSent { reason } => {
            ("not_sent", Some(reaction_refusal_code(reason)))
        }
        _ => (reaction_result_state_code(&state), None),
    };
    conn.execute(
        "UPDATE reaction_requests SET state=?,refusal=?,updated_at_ms=? WHERE request_id=? AND target_gateway_id=? AND state='sending'",
        params![state_code, refusal, notifications::now_ms(), request_id.to_string(), ctx.device_id.to_string()],
    )?;
    enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::ReactionResult {
            request_id,
            message_id,
            target_gateway_id: ctx.device_id,
            state,
        },
    )?;
    Ok(())
}

/// The route support for one conversation, from the eligible RCS gateway's capability for the
/// active epoch, or text when the conversation has messages.
pub(crate) fn support_view(
    conn: &Connection,
    epoch: Option<u32>,
    conversation_id: ConversationId,
) -> Result<ReactionSupportView, Error> {
    // A withdrawn group is read-only: the same fact the composer reads as `Withdrawn`.
    if crate::rcs_group::group_is_withdrawn(conn, conversation_id)? {
        return Ok(ReactionSupportView {
            support: ReactionSupport::None,
            gateway_id: None,
        });
    }
    let gateway: Option<String> = conn
        .query_row(
            "SELECT gateway_device_id FROM rcs_link_eligibility_projection
             WHERE conversation_id=? AND eligibility='eligible'
             ORDER BY observed_at_ms DESC,producer_sequence DESC LIMIT 1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(gateway) = gateway {
        // Without an active epoch the capability cannot be read, so nothing is known about it.
        let Some(epoch) = epoch else {
            return Ok(ReactionSupportView {
                support: ReactionSupport::None,
                gateway_id: None,
            });
        };
        let capability: Option<bool> = conn
            .query_row(
                "SELECT supported FROM reaction_capabilities WHERE device_id=? AND key_epoch=?",
                params![gateway, epoch],
                |row| row.get(0),
            )
            .optional()?;
        let support = match capability {
            Some(true) => ReactionSupport::Native,
            Some(false) => ReactionSupport::None,
            None => ReactionSupport::NeedsPhoneUpdate,
        };
        let gateway_id = match support {
            ReactionSupport::Native | ReactionSupport::NeedsPhoneUpdate => Some(parse(&gateway)?),
            _ => None,
        };
        return Ok(ReactionSupportView {
            support,
            gateway_id,
        });
    }
    let has_messages: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE effective_conversation_id=?)",
        [conversation_id.to_string()],
        |row| row.get(0),
    )?;
    // Text reactions go out as ordinary messages through the composer route. Groups are refused:
    // a group send needs a member revision that a reaction cannot supply.
    let text_capable =
        has_messages && !crate::rcs_group::is_group_conversation(conn, conversation_id)?;
    Ok(ReactionSupportView {
        support: if text_capable {
            ReactionSupport::Text
        } else {
            ReactionSupport::None
        },
        gateway_id: None,
    })
}

/// Fills a message window entry: its stored set, this device's open request, and the route it
/// can be reacted to with.
pub(crate) fn project_message(
    conn: &Connection,
    device: DeviceId,
    epoch: Option<u32>,
    message: &mut Message,
) -> Result<(), Error> {
    let id = message.payload.record.message_id;
    let conversation_id =
        crate::conversation_merge::survivor(conn, message.payload.record.conversation_id)?;
    let support = support_view(conn, epoch, conversation_id)?.support;
    let stored: Option<String> = conn
        .query_row(
            "SELECT reactions_json FROM message_reactions WHERE message_id=?",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    message.reactions = stored
        .map(|value| serde_json::from_str(&value).map_err(|_| Error::Database))
        .transpose()?
        .unwrap_or_default();
    let own = own_reaction(conn, &id.to_string())?;
    message.own_reaction_request = own_request(conn, device, id, own)?;
    let status_settled = match message.payload.direction {
        Direction::Incoming => true,
        Direction::Outgoing => matches!(
            message.send_state,
            Some(SendState::Sent | SendState::Delivered)
        ),
    };
    message.reactable = match (support, message.payload.transport) {
        (ReactionSupport::Native, Transport::Rcs) if status_settled => Some(ReactionRoute::Native),
        // Only a text-capable conversation (no RCS gateway, not a group) takes text reactions.
        (ReactionSupport::Text, Transport::Sms | Transport::Mms)
            if status_settled && !message.payload.body.is_empty() =>
        {
            Some(ReactionRoute::Text)
        }
        _ => None,
    };
    Ok(())
}

/// request id, emoji, state, refusal, and requested time of one open request.
type OwnRequestRow = (String, Option<String>, String, Option<String>, i64);
const OPEN_REQUEST_STATES: &str = "('sending','not_sent','unknown')";

fn displayed_state(state: &str, requested_at_ms: i64, now: i64) -> OwnReactionState {
    match state {
        "sending" if requested_at_ms <= now - DISMISS_AFTER_MS => OwnReactionState::Unknown,
        "sending" => OwnReactionState::Sending,
        "not_sent" => OwnReactionState::NotSent,
        _ => OwnReactionState::Unknown,
    }
}

fn own_request(
    conn: &Connection,
    device: DeviceId,
    message_id: MessageId,
    previous_emoji: Option<String>,
) -> Result<Option<OwnReactionRequest>, Error> {
    let row: Option<OwnRequestRow> = conn
        .query_row(
            &format!(
                "SELECT request_id,emoji,state,refusal,requested_at_ms FROM reaction_requests
             WHERE message_id=? AND requester_device_id=? AND state IN {OPEN_REQUEST_STATES}
             ORDER BY requested_at_ms DESC,rowid DESC LIMIT 1"
            ),
            params![message_id.to_string(), device.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((request, emoji, state, refusal, requested_at_ms)) = row else {
        return Ok(None);
    };
    let state = displayed_state(&state, requested_at_ms, notifications::now_ms());
    Ok(Some(OwnReactionRequest {
        request_id: parse(&request)?,
        emoji,
        previous_emoji,
        state,
        refusal: refusal.as_deref().and_then(reaction_refusal_from_code),
    }))
}

/// The quoted text of a reaction: NFC first, then whitespace runs collapsed to one space. A quote
/// over 80 grapheme clusters keeps its first 79 clusters, trims trailing whitespace, and ends with
/// an ellipsis. Clusters are UAX #29 extended grapheme clusters, so truncation never splits one.
pub fn compose_reaction_text(body: &str, emoji: &str) -> String {
    format!("Reacted {emoji} to “{}”", quote_excerpt(body))
}

/// The quoted excerpt of a message body, shared by reaction and reply quotes. See
/// `compose_reaction_text` for the rule.
pub(crate) fn quote_excerpt(body: &str) -> String {
    let normalized = body
        .nfc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.graphemes(true).count() > 80 {
        let kept: String = normalized.graphemes(true).take(79).collect();
        format!("{}…", kept.trim_end())
    } else {
        normalized
    }
}

/// The carrier text of a reply: the quoted target excerpt on its own line, then the reply.
pub fn compose_reply_text(target_body: &str, reply: &str) -> String {
    quoted_reply(&quote_excerpt(target_body), reply)
}

/// `compose_reply_text` for a caller that already holds the excerpt.
pub(crate) fn quoted_reply(excerpt: &str, reply: &str) -> String {
    format!("“{excerpt}”\n{reply}")
}

/// Whether the stored eligibility projection says `gateway_id` owns the conversation's RCS link
/// right now. Ownership is a fact at apply time only: it is never a reason to refuse a set.
pub(crate) fn link_owner(
    conn: &Connection,
    conversation_id: ConversationId,
    gateway_id: DeviceId,
) -> Result<bool, Error> {
    let eligibility: Option<String> = conn
        .query_row(
            "SELECT eligibility FROM rcs_link_eligibility_projection WHERE conversation_id=? AND gateway_device_id=?",
            params![conversation_id.to_string(), gateway_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(eligibility.as_deref() == Some("eligible"))
}

/// Keeps an authentic set whose producer is not the owner by the projection held now. Only the
/// newest staged set per (message, producer) is kept, with the same ordering rule as the
/// published set.
pub(crate) fn stage_message_reactions(
    conn: &Connection,
    message_id: MessageId,
    conversation_id: ConversationId,
    gateway_id: DeviceId,
    observed_at_ms: i64,
    producer_sequence: u64,
    reactions: &[MessageReaction],
) -> Result<(), Error> {
    let json = serde_json::to_string(reactions).map_err(|_| Error::Database)?;
    conn.execute(
        "INSERT INTO reaction_sets_staged(message_id,conversation_id,gateway_id,observed_at_ms,producer_sequence,reactions_json)
         VALUES(?,?,?,?,?,?) ON CONFLICT(message_id,gateway_id) DO UPDATE SET conversation_id=excluded.conversation_id,
         observed_at_ms=excluded.observed_at_ms,producer_sequence=excluded.producer_sequence,reactions_json=excluded.reactions_json
         WHERE excluded.observed_at_ms>reaction_sets_staged.observed_at_ms OR
         (excluded.observed_at_ms=reaction_sets_staged.observed_at_ms AND excluded.producer_sequence>reaction_sets_staged.producer_sequence)",
        params![message_id.to_string(), conversation_id.to_string(), gateway_id.to_string(), observed_at_ms, producer_sequence as i64, json],
    )?;
    Ok(())
}

/// Drops staged sets that a published set now supersedes: older than it, or the same producer's
/// earlier or equal sequence at the same observation time.
fn prune_superseded_staged(conn: &Connection, message_id: MessageId) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM reaction_sets_staged WHERE message_id=? AND EXISTS(
            SELECT 1 FROM message_reactions m WHERE m.message_id=reaction_sets_staged.message_id AND (
              reaction_sets_staged.observed_at_ms < m.observed_at_ms OR
              (reaction_sets_staged.observed_at_ms = m.observed_at_ms AND reaction_sets_staged.gateway_id = m.gateway_id
               AND reaction_sets_staged.producer_sequence <= m.producer_sequence)))",
        [message_id.to_string()],
    )?;
    Ok(())
}

/// Called whenever a gateway's eligibility projection for a conversation changes. Staged sets
/// from that gateway are published when it now owns the link; otherwise they stay staged, so a
/// later owner proof still applies them.
pub(crate) fn settle_staged_message_reactions(
    conn: &Connection,
    conversation_id: ConversationId,
    gateway_id: DeviceId,
) -> Result<(), Error> {
    if !link_owner(conn, conversation_id, gateway_id)? {
        return Ok(());
    }
    let staged: Vec<(String, i64, i64, String)> = conn
        .prepare(
            "SELECT message_id,observed_at_ms,producer_sequence,reactions_json FROM reaction_sets_staged
             WHERE conversation_id=? AND gateway_id=?",
        )?
        .query_map(params![conversation_id.to_string(), gateway_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    conn.execute(
        "DELETE FROM reaction_sets_staged WHERE conversation_id=? AND gateway_id=?",
        params![conversation_id.to_string(), gateway_id.to_string()],
    )?;
    for (message, observed_at_ms, producer_sequence, json) in staged {
        let reactions: Vec<MessageReaction> =
            serde_json::from_str(&json).map_err(|_| Error::Database)?;
        store_message_reactions(
            conn,
            parse(&message)?,
            conversation_id,
            gateway_id,
            observed_at_ms,
            producer_sequence as u64,
            &reactions,
        )?;
    }
    Ok(())
}

impl Client {
    /// The conversation of one of this device's own reaction requests. `None` for any request this
    /// device did not make, so a host can scope dismissal to the composer's conversation.
    pub fn reaction_request_conversation(
        &self,
        request_id: ReactionRequestId,
    ) -> Result<Option<ConversationId>, Error> {
        let store = self.lock()?;
        let conversation: Option<String> = store
            .conn
            .query_row(
                "SELECT conversation_id FROM reaction_requests WHERE request_id=? AND requester_device_id=?",
                params![request_id.to_string(), store.config.device_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        conversation.map(|value| parse(&value)).transpose()
    }
}
