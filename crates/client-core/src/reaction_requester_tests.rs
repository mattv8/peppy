//! Requester-side reaction behaviour: support, routes, busy/replace/dismiss, projections, text.

use super::*;

use crate::reaction_tests::{
    Fixture, actor, grant_eligibility, outbox_count, reaction, seed_message, state_of,
};
use unicode_segmentation::UnicodeSegmentation;

const SIM: &str = "sim-1";

/// An RCS conversation whose gateway reports the capability for the active epoch.
fn native_conversation(fixture: &Fixture, gateway: DeviceId, supported: bool) -> ConversationId {
    let conversation = ConversationId::new();
    grant_eligibility(fixture, gateway, conversation);
    fixture.execute(&format!(
        "INSERT INTO reaction_capabilities(device_id,supported,key_epoch,producer_sequence) VALUES('{gateway}',{},1,1)",
        i64::from(supported)
    ));
    conversation
}

fn route(gateway: DeviceId) -> GatewayRoute {
    GatewayRoute::Carrier {
        gateway_device_id: gateway,
        subscription_id: SIM.to_owned(),
    }
}

fn request_row(fixture: &Fixture, message: MessageId) -> Option<ReactionRequestId> {
    fixture
        .query_text(&format!(
            "SELECT request_id FROM reaction_requests WHERE message_id='{message}' ORDER BY rowid DESC LIMIT 1"
        ))
        .map(|value| parse(&value).unwrap())
}

#[test]
fn reaction_support_is_native_only_for_a_capable_eligible_gateway() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let empty = ConversationId::new();
    assert_eq!(
        fixture.client.reaction_support(empty).unwrap().support,
        ReactionSupport::None
    );

    let texted = ConversationId::new();
    seed_message(&fixture, Transport::Sms, Direction::Incoming, "hi", texted);
    assert_eq!(
        fixture.client.reaction_support(texted).unwrap(),
        ReactionSupportView {
            support: ReactionSupport::Text,
            gateway_id: None
        }
    );

    let absent = ConversationId::new();
    grant_eligibility(&fixture, gateway, absent);
    assert_eq!(
        fixture.client.reaction_support(absent).unwrap(),
        ReactionSupportView {
            support: ReactionSupport::NeedsPhoneUpdate,
            gateway_id: Some(gateway)
        },
        "a gateway with no capability for the active epoch never advertised reactions"
    );

    let native = native_conversation(&fixture, gateway, true);
    assert_eq!(
        fixture.client.reaction_support(native).unwrap(),
        ReactionSupportView {
            support: ReactionSupport::Native,
            gateway_id: Some(gateway)
        }
    );

    let declined = native_conversation(&fixture, DeviceId::new(), false);
    assert_eq!(
        fixture.client.reaction_support(declined).unwrap().support,
        ReactionSupport::None
    );
}

#[test]
fn reaction_react_refuses_an_rcs_message_whose_phone_predates_reactions() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = ConversationId::new();
    grant_eligibility(&fixture, gateway, conversation);
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    let before = outbox_count(&fixture);

    let result = fixture
        .client
        .react_to_message(message, Some("👍".into()), true, None)
        .unwrap();

    assert_eq!(result, refusal(SubmitRefusal::NeedsPhoneUpdate));
    assert!(request_row(&fixture, message).is_none());
    assert_eq!(outbox_count(&fixture), before);
}

#[test]
fn reaction_react_native_needs_an_online_gateway_and_queues_one_request() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = native_conversation(&fixture, gateway, true);
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    let before = outbox_count(&fixture);

    let offline = fixture
        .client
        .react_to_message(message, Some("👍".into()), false, None)
        .unwrap();
    assert_eq!(offline, refusal(SubmitRefusal::Offline));
    assert!(request_row(&fixture, message).is_none());
    assert_eq!(outbox_count(&fixture), before);

    let accepted = fixture
        .client
        .react_to_message(message, Some("👍".into()), true, None)
        .unwrap();
    assert!(accepted.accepted);
    assert_eq!(accepted.route, Some(ReactionRoute::Native));
    let request = accepted.request_id.unwrap();
    assert_eq!(state_of(&fixture, request).as_deref(), Some("sending"));
    assert_eq!(
        fixture
            .query_text(&format!(
                "SELECT target_gateway_id FROM reaction_requests WHERE request_id='{request}'"
            ))
            .unwrap(),
        gateway.to_string()
    );
    assert_eq!(outbox_count(&fixture), before + 1);

    assert_eq!(
        fixture
            .client
            .react_to_message(message, Some("❤️".into()), true, None)
            .unwrap(),
        refusal(SubmitRefusal::Busy),
        "a sending request blocks another reaction to the message"
    );
}

#[test]
fn reaction_react_refuses_invalid_emoji_and_unreactable_messages() {
    let fixture = Fixture::new();
    let conversation = ConversationId::new();
    let incoming = seed_message(
        &fixture,
        Transport::Sms,
        Direction::Incoming,
        "hi",
        conversation,
    );
    let outgoing = seed_message(
        &fixture,
        Transport::Sms,
        Direction::Outgoing,
        "sent by me",
        conversation,
    );
    let silent = seed_message(
        &fixture,
        Transport::Sms,
        Direction::Incoming,
        "",
        conversation,
    );

    assert_eq!(
        fixture
            .client
            .react_to_message(
                incoming,
                Some("hello".into()),
                true,
                Some(route(DeviceId::new()))
            )
            .unwrap(),
        refusal(SubmitRefusal::InvalidEmoji)
    );
    assert_eq!(
        fixture
            .client
            .react_to_message(
                outgoing,
                Some("👍".into()),
                true,
                Some(route(DeviceId::new()))
            )
            .unwrap(),
        refusal(SubmitRefusal::NotReactable),
        "an outgoing message is reactable only once the gateway reports it sent"
    );
    assert_eq!(
        fixture
            .client
            .react_to_message(
                silent,
                Some("👍".into()),
                true,
                Some(route(DeviceId::new()))
            )
            .unwrap(),
        refusal(SubmitRefusal::NotReactable),
        "media or empty text is not reactable in a text chat"
    );
    assert!(matches!(
        fixture
            .client
            .react_to_message(MessageId::new(), Some("👍".into()), true, None),
        Err(Error::NotFound)
    ));
}

#[test]
fn reaction_react_text_uses_the_composer_route_and_never_creates_a_request() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = ConversationId::new();
    let message = seed_message(
        &fixture,
        Transport::Sms,
        Direction::Incoming,
        "Running late",
        conversation,
    );
    let before = outbox_count(&fixture);

    assert_eq!(
        fixture
            .client
            .react_to_message(message, None, true, Some(route(gateway)))
            .unwrap(),
        refusal(SubmitRefusal::Unsupported),
        "a text reaction cannot remove"
    );
    assert_eq!(
        fixture
            .client
            .react_to_message(message, Some("👍".into()), false, Some(route(gateway)))
            .unwrap(),
        refusal(SubmitRefusal::Offline)
    );
    assert_eq!(
        fixture
            .client
            .react_to_message(message, Some("👍".into()), true, None)
            .unwrap(),
        refusal(SubmitRefusal::Unsupported),
        "no composer route, no text reaction"
    );

    let accepted = fixture
        .client
        .react_to_message(message, Some("👍".into()), true, Some(route(gateway)))
        .unwrap();
    assert_eq!(
        accepted,
        ReactionSubmit {
            accepted: true,
            request_id: None,
            route: Some(ReactionRoute::Text),
            reason: None
        }
    );
    assert!(
        request_row(&fixture, message).is_none(),
        "text reactions are not tracked as requests"
    );
    assert_eq!(
        outbox_count(&fixture),
        before + 1,
        "one ordinary outgoing message is queued"
    );

    let window = fixture
        .client
        .messages_page(conversation, None, 10)
        .unwrap()
        .messages;
    let reply = window
        .iter()
        .find(|item| item.payload.direction == Direction::Outgoing)
        .expect("the reaction text is an outgoing message");
    assert_eq!(reply.payload.body, "Reacted 👍 to “Running late”");
    assert_eq!(reply.payload.recipients, vec!["+15550100".to_owned()]);
}

#[test]
fn reaction_requests_replace_dismiss_and_block_by_state() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = native_conversation(&fixture, gateway, true);
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );

    let first = fixture
        .client
        .react_to_message(message, Some("👍".into()), true, None)
        .unwrap()
        .request_id
        .unwrap();
    assert!(
        fixture.client.dismiss_reaction_request(first).is_err(),
        "a fresh sending request is not dismissable"
    );

    fixture.execute(&format!("UPDATE reaction_requests SET state='not_sent',refusal='rejected' WHERE request_id='{first}'"));
    let second = fixture
        .client
        .react_to_message(message, Some("❤️".into()), true, None)
        .unwrap()
        .request_id
        .unwrap();
    assert_eq!(
        state_of(&fixture, first).as_deref(),
        Some("dismissed"),
        "a not_sent request is replaced"
    );

    fixture.execute(&format!(
        "UPDATE reaction_requests SET state='unknown' WHERE request_id='{second}'"
    ));
    assert_eq!(
        fixture
            .client
            .react_to_message(message, Some("😂".into()), true, None)
            .unwrap(),
        refusal(SubmitRefusal::Busy),
        "an unknown request blocks until dismissed"
    );
    fixture.client.dismiss_reaction_request(second).unwrap();
    assert_eq!(state_of(&fixture, second).as_deref(), Some("dismissed"));
    let third = fixture
        .client
        .react_to_message(message, Some("😂".into()), true, None)
        .unwrap()
        .request_id
        .unwrap();

    fixture.execute(&format!(
        "UPDATE reaction_requests SET requested_at_ms={} WHERE request_id='{third}'",
        notifications::now_ms() - REACTION_REQUEST_TTL_MS - REACTION_REQUEST_SKEW_MS - 1_000
    ));
    fixture.client.dismiss_reaction_request(third).unwrap();
    assert_eq!(
        state_of(&fixture, third).as_deref(),
        Some("dismissed"),
        "a stale sending request is dismissable"
    );
}

#[test]
fn reaction_window_projects_sets_own_requests_and_reactable_routes() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = native_conversation(&fixture, gateway, true);
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    {
        let store = fixture.client.lock().unwrap();
        crate::reactions::store_message_reactions(
            &store.conn,
            message,
            conversation,
            gateway,
            5,
            1,
            &[
                reaction("👍", vec![actor(true, None)]),
                reaction("❤️", vec![actor(false, Some("+15550100"))]),
            ],
        )
        .unwrap();
    }
    let request = fixture
        .client
        .react_to_message(message, Some("❤️".into()), true, None)
        .unwrap()
        .request_id
        .unwrap();

    let view = fixture
        .client
        .messages_page(conversation, None, 10)
        .unwrap()
        .messages
        .remove(0);
    assert_eq!(view.reactable, Some(ReactionRoute::Native));
    assert_eq!(view.reactions.len(), 2);
    let own = view
        .own_reaction_request
        .clone()
        .expect("the open request is projected");
    assert_eq!(own.request_id, request);
    assert_eq!(own.emoji.as_deref(), Some("❤️"));
    assert_eq!(own.previous_emoji.as_deref(), Some("👍"));
    assert_eq!(own.state, OwnReactionState::Sending);

    fixture.execute(&format!("UPDATE reaction_requests SET state='not_sent',refusal='link_unavailable' WHERE request_id='{request}'"));
    let view = fixture.client.message_by_id(message).unwrap().unwrap();
    let own = view.own_reaction_request.unwrap();
    assert_eq!(own.state, OwnReactionState::NotSent);
    assert_eq!(own.refusal, Some(ReactionRefusal::LinkUnavailable));

    fixture.execute(&format!("UPDATE reaction_requests SET state='sending',requested_at_ms={} WHERE request_id='{request}'", notifications::now_ms() - REACTION_REQUEST_TTL_MS - REACTION_REQUEST_SKEW_MS - 1_000));
    let own = fixture
        .client
        .message_by_id(message)
        .unwrap()
        .unwrap()
        .own_reaction_request
        .unwrap();
    assert_eq!(
        own.state,
        OwnReactionState::Unknown,
        "a stale sending request reads as unknown"
    );

    let texted = ConversationId::new();
    let sms = seed_message(
        &fixture,
        Transport::Sms,
        Direction::Incoming,
        "plain",
        texted,
    );
    let sms_view = fixture.client.message_by_id(sms).unwrap().unwrap();
    assert_eq!(sms_view.reactable, Some(ReactionRoute::Text));
    assert!(sms_view.own_reaction_request.is_none());
    let listed = fixture.client.list_conversations().unwrap();
    let text_row = listed
        .iter()
        .find(|row| row.conversation_id == texted)
        .unwrap();
    assert_eq!(text_row.reaction_support, ReactionSupport::Text);
    assert_eq!(text_row.reaction_gateway_id, None);
    let native_row = listed
        .iter()
        .find(|row| row.conversation_id == conversation)
        .unwrap();
    assert_eq!(native_row.reaction_gateway_id, Some(gateway));
}

#[test]
fn reaction_composition_counts_grapheme_clusters_and_trims_over_eighty() {
    let count = |text: &str| text.graphemes(true).count();
    assert_eq!(
        count("👨\u{200D}👩\u{200D}👧x"),
        2,
        "a ZWJ family is one cluster"
    );
    assert_eq!(count("e\u{301}x"), 2, "a combining accent joins its base");
    assert_eq!(count("🇬🇧🇫🇷"), 2, "regional indicators pair up");
    assert_eq!(count("👍🏽"), 1, "a skin tone joins its base");

    let flags = "🇬🇧".repeat(81);
    let text = compose_reaction_text(&flags, "👍");
    assert_eq!(text, format!("Reacted 👍 to “{}…”", "🇬🇧".repeat(79)));

    let exact = "x".repeat(80);
    assert_eq!(
        compose_reaction_text(&exact, "👍"),
        format!("Reacted 👍 to “{exact}”")
    );
}

fn refusal(reason: SubmitRefusal) -> ReactionSubmit {
    ReactionSubmit {
        accepted: false,
        request_id: None,
        route: None,
        reason: Some(reason),
    }
}

#[test]
fn reaction_request_conversation_is_known_only_for_this_devices_requests() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = native_conversation(&fixture, gateway, true);
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    let own = fixture
        .client
        .react_to_message(message, Some("👍".into()), true, None)
        .unwrap()
        .request_id
        .unwrap();
    assert_eq!(
        fixture.client.reaction_request_conversation(own).unwrap(),
        Some(conversation)
    );

    let other = ReactionRequestId::new();
    fixture.execute(&format!(
        "INSERT INTO reaction_requests(request_id,message_id,conversation_id,target_gateway_id,emoji,requested_at_ms,requester_device_id,key_epoch,state,updated_at_ms)
         VALUES('{other}','{}','{}','{gateway}',NULL,0,'{}',1,'sending',0)",
        MessageId::new(),
        ConversationId::new(),
        DeviceId::new(),
    ));
    assert_eq!(
        fixture.client.reaction_request_conversation(other).unwrap(),
        None,
        "another device's request is not scoped here"
    );
    assert_eq!(
        fixture
            .client
            .reaction_request_conversation(ReactionRequestId::new())
            .unwrap(),
        None
    );
}

/// Adds a second other participant to a conversation's roster, making it a group.
fn make_group(fixture: &Fixture, conversation: ConversationId) {
    fixture.execute(&format!(
        "INSERT INTO conversation_group_members(conversation_id,canonical_address) VALUES('{conversation}','+15550101'),('{conversation}','+15550102')"
    ));
}

#[test]
fn reaction_text_is_refused_in_sms_and_mms_groups_but_native_rcs_groups_are_unaffected() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let texted = ConversationId::new();
    let sms = seed_message(
        &fixture,
        Transport::Sms,
        Direction::Incoming,
        "group hi",
        texted,
    );
    make_group(&fixture, texted);
    let mms_group = ConversationId::new();
    let mms = seed_message(
        &fixture,
        Transport::Mms,
        Direction::Incoming,
        "group pic",
        mms_group,
    );
    make_group(&fixture, mms_group);
    for conversation in [texted, mms_group] {
        assert_eq!(
            fixture.client.reaction_support(conversation).unwrap(),
            ReactionSupportView {
                support: ReactionSupport::None,
                gateway_id: None
            }
        );
    }
    let before = outbox_count(&fixture);
    for message in [sms, mms] {
        assert_eq!(
            fixture
                .client
                .message_by_id(message)
                .unwrap()
                .unwrap()
                .reactable,
            None
        );
        assert_eq!(
            fixture
                .client
                .react_to_message(message, Some("👍".into()), true, Some(route(gateway)))
                .unwrap(),
            refusal(SubmitRefusal::NotReactable)
        );
    }
    assert_eq!(
        outbox_count(&fixture),
        before,
        "no MMS group send is queued"
    );

    let native = native_conversation(&fixture, gateway, true);
    make_group(&fixture, native);
    let rcs = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "rcs group",
        native,
    );
    assert_eq!(
        fixture.client.reaction_support(native).unwrap().support,
        ReactionSupport::Native
    );
    assert!(
        fixture
            .client
            .react_to_message(rcs, Some("👍".into()), true, None)
            .unwrap()
            .accepted
    );
}

#[test]
fn reaction_text_is_refused_for_a_withdrawn_read_only_group() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = ConversationId::new();
    let message = seed_message(
        &fixture,
        Transport::Sms,
        Direction::Incoming,
        "read only",
        conversation,
    );
    fixture.execute(&format!(
        "INSERT INTO rcs_group_mapping(conversation_id,google_conversation_id,gateway_device_id,state,proved_member_revision,member_set_digest,proof_observed_at_ms,snapshot_token,created_at_ms,updated_at_ms)
         VALUES('{conversation}','g-withdrawn','{gateway}','withdrawn',0,'digest',0,'token',0,0)"
    ));
    assert_eq!(
        fixture
            .client
            .reaction_support(conversation)
            .unwrap()
            .support,
        ReactionSupport::None
    );
    assert_eq!(
        fixture
            .client
            .react_to_message(message, Some("👍".into()), true, Some(route(gateway)))
            .unwrap(),
        refusal(SubmitRefusal::NotReactable)
    );
}

#[test]
fn reaction_projection_is_the_same_on_every_message_path() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = native_conversation(&fixture, gateway, true);
    let earlier = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "earlier",
        conversation,
    );
    let anchor = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "anchor",
        conversation,
    );
    let newer = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "newer",
        conversation,
    );
    let request = fixture
        .client
        .react_to_message(anchor, Some("👍".into()), true, None)
        .unwrap()
        .request_id
        .unwrap();

    let check = |label: &str, message: &Message| {
        if message.payload.record.message_id == anchor {
            let own = message
                .own_reaction_request
                .as_ref()
                .unwrap_or_else(|| panic!("{label}: own request missing"));
            assert_eq!(own.request_id, request, "{label}");
        }
        assert_eq!(message.reactable, Some(ReactionRoute::Native), "{label}");
    };
    for message in fixture.client.messages(conversation).unwrap() {
        check("messages", &message);
    }
    check(
        "latest_message",
        &fixture
            .client
            .latest_message(conversation)
            .unwrap()
            .unwrap(),
    );
    for message in fixture.client.messages_since(conversation, anchor).unwrap() {
        check("messages_since", &message);
    }
    for message in fixture
        .client
        .messages_page(conversation, None, 10)
        .unwrap()
        .messages
    {
        check("messages_page", &message);
    }
    check(
        "message_by_id",
        &fixture.client.message_by_id(anchor).unwrap().unwrap(),
    );
    let since: Vec<MessageId> = fixture
        .client
        .messages_since(conversation, anchor)
        .unwrap()
        .iter()
        .map(|m| m.payload.record.message_id)
        .collect();
    assert_eq!(since, vec![anchor, newer]);
    assert!(
        fixture
            .client
            .message_by_id(earlier)
            .unwrap()
            .unwrap()
            .reactable
            .is_some()
    );
}
