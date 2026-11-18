//! Recovery state of a carrier SMS or MMS whose send may not have reached the carrier. Native and
//! browser hosts project it with the same function, from the message and the host's verified
//! revoked-device set. Only display facts are returned; the core decides what a copy may do.
use peppy_client_core::{Client, Direction, Error, Message, SendRouteKind, SendState, Transport};
use serde::Serialize;
use std::collections::HashSet;

/// The message-level recovery state the UI reads as `MessageView.sendRecovery`.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRecoveryView {
    /// `not-sent`, `unconfirmed-removed` or `unconfirmed-removed-cancel`.
    pub kind: &'static str,
    pub can_send_again: bool,
    /// The "Send again" copy of this message, when this device holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_again: Option<SentAgainView>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentAgainView {
    pub message_id: String,
}

/// Projects the recovery state of one message. `revoked_devices` is the host's verified roster
/// (an empty set until the roster has loaded). A device that is not in the set is not revoked.
pub fn send_recovery_view(
    client: &Client,
    message: &Message,
    revoked_devices: &HashSet<String>,
) -> Result<Option<SendRecoveryView>, Error> {
    if message.payload.direction != Direction::Outgoing
        || !matches!(message.payload.transport, Transport::Sms | Transport::Mms)
        || message.send_route == Some(SendRouteKind::Rcs)
    {
        return Ok(None);
    }
    let kind = match message.send_state {
        Some(SendState::FailedBeforeSubmission) if message.send_refusal.is_none() => "not-sent",
        Some(SendState::QueuedLocal | SendState::AcceptedServer | SendState::PersistedGateway)
            if target_revoked(message, revoked_devices) =>
        {
            if message.send_cancel_requested {
                "unconfirmed-removed-cancel"
            } else {
                "unconfirmed-removed"
            }
        }
        _ => return Ok(None),
    };
    let sent_again = message
        .sent_again_message_id
        .map(|message_id| SentAgainView {
            message_id: message_id.to_string(),
        });
    // The core refuses a copy for any group conversation; `group_view` is the closest public fact.
    // It also reports a one-member roster as a group, which only hides the button (never shows one
    // the core would refuse). A route the core cannot establish (`None`) is shown but not offered.
    let can_send_again = message.send_route == Some(SendRouteKind::Carrier)
        && kind != "unconfirmed-removed-cancel"
        && sent_again.is_none()
        && client
            .group_view(message.payload.record.conversation_id)?
            .is_none();
    Ok(Some(SendRecoveryView {
        kind,
        can_send_again,
        sent_again,
    }))
}

fn target_revoked(message: &Message, revoked_devices: &HashSet<String>) -> bool {
    message
        .target_gateway_id
        .is_some_and(|target| revoked_devices.contains(&target.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{
        ClientConfig, ConversationId, DatabaseKey, DeviceId, MessageId, MessagePayload,
        MessageRecord, OutgoingSms, SourceSequence,
    };
    use std::str::FromStr;

    struct Fixture {
        _directory: tempfile::TempDir,
        path: std::path::PathBuf,
        client: Client,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("send-recovery.db");
        let client = Client::open(
            ClientConfig {
                database_path: path.clone(),
                vault_id: peppy_client_core::VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        Fixture {
            _directory: directory,
            path,
            client,
        }
    }

    fn message(
        transport: Transport,
        direction: Direction,
        conversation: ConversationId,
    ) -> Message {
        Message {
            payload: MessagePayload {
                record: MessageRecord {
                    message_id: MessageId::new(),
                    conversation_id: conversation,
                    source_sequence: SourceSequence(1),
                    attachments: vec![],
                },
                source_device_id: DeviceId::new(),
                provider_message_id: None,
                sender_address: None,
                recipients: vec!["+15555550123".into()],
                subject: None,
                body: "Hello".into(),
                transport,
                direction,
                rcs_capture: false,
                imported: false,
                sent_at_ms: Some(2),
                ordered_at_ms: None,
                mms_context: None,
            },
            seen: true,
            send_state: Some(SendState::QueuedLocal),
            status_at_ms: None,
            send_refusal: None,
            target_gateway_id: Some(DeviceId::new()),
            send_cancel_requested: false,
            sent_again_message_id: None,
            send_route: Some(SendRouteKind::Carrier),
            reactions: Vec::new(),
            own_reaction_request: None,
            reactable: None,
        }
    }

    fn revoked(message: &Message) -> HashSet<String> {
        HashSet::from([message.target_gateway_id.unwrap().to_string()])
    }

    fn project(
        client: &Client,
        message: &Message,
        revoked: &HashSet<String>,
    ) -> Option<SendRecoveryView> {
        send_recovery_view(client, message, revoked).unwrap()
    }

    #[test]
    fn a_failed_before_submission_send_is_not_sent_and_can_be_sent_again() {
        let fixture = fixture();
        let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        outgoing.send_state = Some(SendState::FailedBeforeSubmission);

        let view = project(&fixture.client, &outgoing, &HashSet::new()).unwrap();

        assert_eq!(view.kind, "not-sent");
        assert!(view.can_send_again);
        assert!(view.sent_again.is_none());
    }

    #[test]
    fn a_send_routed_over_rcs_has_no_recovery_state_even_when_stored_as_sms() {
        let fixture = fixture();
        let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        outgoing.send_state = Some(SendState::FailedBeforeSubmission);
        outgoing.send_route = Some(SendRouteKind::Rcs);

        assert!(project(&fixture.client, &outgoing, &HashSet::new()).is_none());
    }

    #[test]
    fn a_send_whose_route_is_unknown_shows_the_kind_but_no_button() {
        let fixture = fixture();
        let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        outgoing.send_state = Some(SendState::FailedBeforeSubmission);
        outgoing.send_route = None;

        let view = project(&fixture.client, &outgoing, &HashSet::new()).unwrap();

        assert_eq!(view.kind, "not-sent");
        assert!(!view.can_send_again);
        let mut removed = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        removed.send_route = None;
        let roster = revoked(&removed);
        let view = project(&fixture.client, &removed, &roster).unwrap();
        assert_eq!(view.kind, "unconfirmed-removed");
        assert!(!view.can_send_again);
    }

    #[test]
    fn a_failed_send_with_a_refusal_reason_has_no_recovery_state() {
        let fixture = fixture();
        let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        outgoing.send_state = Some(SendState::FailedBeforeSubmission);
        outgoing.send_refusal = Some(peppy_client_core::SendRefusalReason::CreateNotRcs);

        assert!(project(&fixture.client, &outgoing, &HashSet::new()).is_none());
    }

    #[test]
    fn a_waiting_send_to_a_revoked_phone_is_unconfirmed_removed() {
        let fixture = fixture();
        for state in [
            SendState::QueuedLocal,
            SendState::AcceptedServer,
            SendState::PersistedGateway,
        ] {
            let mut outgoing = message(Transport::Mms, Direction::Outgoing, ConversationId::new());
            outgoing.send_state = Some(state);
            let view = project(&fixture.client, &outgoing, &revoked(&outgoing)).unwrap();

            assert_eq!(view.kind, "unconfirmed-removed", "{state:?}");
            assert!(view.can_send_again);
        }
    }

    #[test]
    fn a_target_that_is_not_revoked_has_no_recovery_state() {
        let fixture = fixture();
        let outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        let other = HashSet::from([DeviceId::new().to_string()]);

        assert!(project(&fixture.client, &outgoing, &other).is_none());
        assert!(project(&fixture.client, &outgoing, &HashSet::new()).is_none());
    }

    #[test]
    fn a_message_without_a_target_has_no_recovery_state_even_when_roster_is_known() {
        let fixture = fixture();
        let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        outgoing.target_gateway_id = None;
        let all = HashSet::from([DeviceId::new().to_string()]);

        assert!(project(&fixture.client, &outgoing, &all).is_none());
    }

    #[test]
    fn a_revoked_phone_with_a_confirmed_or_unknown_outcome_has_no_recovery_state() {
        let fixture = fixture();
        for state in [
            SendState::SubmittedToOs,
            SendState::Sent,
            SendState::Delivered,
            SendState::OutcomeUnknown,
            SendState::FailedConfirmed,
        ] {
            let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
            outgoing.send_state = Some(state);
            assert!(
                project(&fixture.client, &outgoing, &revoked(&outgoing)).is_none(),
                "{state:?} must never offer a copy"
            );
        }
    }

    #[test]
    fn a_cancellation_request_shows_the_cancel_kind_without_a_button() {
        let fixture = fixture();
        let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        outgoing.send_cancel_requested = true;

        let view = project(&fixture.client, &outgoing, &revoked(&outgoing)).unwrap();

        assert_eq!(view.kind, "unconfirmed-removed-cancel");
        assert!(!view.can_send_again);
    }

    #[test]
    fn a_sent_again_copy_is_reported_and_closes_the_button() {
        let fixture = fixture();
        let copy = MessageId::new();
        let mut outgoing = message(Transport::Sms, Direction::Outgoing, ConversationId::new());
        outgoing.send_state = Some(SendState::FailedBeforeSubmission);
        outgoing.sent_again_message_id = Some(copy);

        let view = project(&fixture.client, &outgoing, &HashSet::new()).unwrap();

        assert_eq!(view.kind, "not-sent");
        assert!(!view.can_send_again);
        assert_eq!(
            view.sent_again,
            Some(SentAgainView {
                message_id: copy.to_string()
            })
        );
    }

    #[test]
    fn rcs_and_incoming_messages_never_get_a_recovery_state() {
        let fixture = fixture();
        let mut rcs = message(Transport::Rcs, Direction::Outgoing, ConversationId::new());
        rcs.send_state = Some(SendState::FailedBeforeSubmission);
        let mut incoming = message(Transport::Sms, Direction::Incoming, ConversationId::new());
        incoming.send_state = None;

        assert!(project(&fixture.client, &rcs, &HashSet::new()).is_none());
        assert!(project(&fixture.client, &incoming, &HashSet::new()).is_none());
    }

    #[test]
    fn a_group_conversation_shows_the_kind_but_no_button() {
        let fixture = fixture();
        let conversation = ConversationId::new();
        {
            let db = rusqlite::Connection::open(&fixture.path).unwrap();
            db.execute_batch(
                "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
            )
            .unwrap();
            db.execute(
                "INSERT INTO conversation_group_members(conversation_id, canonical_address) VALUES(?,?),(?,?)",
                rusqlite::params![
                    conversation.to_string(),
                    "+15551230001",
                    conversation.to_string(),
                    "+15551230002"
                ],
            )
            .unwrap();
        }
        let mut outgoing = message(Transport::Mms, Direction::Outgoing, conversation);
        outgoing.send_state = Some(SendState::FailedBeforeSubmission);

        let view = project(&fixture.client, &outgoing, &HashSet::new()).unwrap();

        assert_eq!(view.kind, "not-sent");
        assert!(!view.can_send_again);
    }

    #[test]
    fn the_projection_serializes_camel_case_and_omits_an_absent_copy() {
        let view = SendRecoveryView {
            kind: "not-sent",
            can_send_again: true,
            sent_again: None,
        };
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"kind": "not-sent", "canSendAgain": true})
        );

        let copy = SendRecoveryView {
            kind: "unconfirmed-removed",
            can_send_again: false,
            sent_again: Some(SentAgainView {
                message_id: "message-copy".into(),
            }),
        };
        assert_eq!(
            serde_json::to_value(&copy).unwrap(),
            serde_json::json!({
                "kind": "unconfirmed-removed",
                "canSendAgain": false,
                "sentAgain": {"messageId": "message-copy"}
            })
        );
    }

    #[test]
    fn a_queued_sms_to_a_phone_that_is_later_revoked_is_offered_a_copy() {
        let fixture = fixture();
        let gateway = DeviceId::new();
        let conversation = ConversationId::new();
        fixture
            .client
            .queue_send(
                OutgoingSms {
                    conversation_id: conversation,
                    recipients: vec!["+15555550123".into()],
                    body: "stranded".into(),
                },
                peppy_client_core::GatewayRoute::Carrier {
                    gateway_device_id: gateway,
                    subscription_id: "sim-1".into(),
                },
            )
            .unwrap();
        let stored = fixture
            .client
            .messages(conversation)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let roster = HashSet::from([gateway.to_string()]);

        let view = project(&fixture.client, &stored, &roster).unwrap();

        assert_eq!(view.kind, "unconfirmed-removed");
        assert!(view.can_send_again);
        assert!(project(&fixture.client, &stored, &HashSet::new()).is_none());
        assert_eq!(
            stored.target_gateway_id,
            Some(DeviceId::from_str(&gateway.to_string()).unwrap())
        );
    }
}
