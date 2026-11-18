//! Phone-side reaction behaviour: jobs, one-use attempts, outcomes, publication, and resolution.

use super::*;

use peppy_crypto::{create_vault_check_header, derive_root_key};

use crate::reaction_tests::{
    Fixture, actor, grant_eligibility, mark_link_linked, outbox_count, outbox_count_of, reaction,
    reopen_unlocked, seed_message, seed_rcs_identity, state_of,
};

/// The next journal cursor for this database; journal cursors must stay contiguous.
fn next_cursor(fixture: &Fixture) -> u64 {
    fixture
        .query_text("SELECT COALESCE(MAX(cursor),0)+1 FROM journal")
        .unwrap()
        .parse()
        .unwrap()
}

fn now() -> i64 {
    notifications::now_ms()
}

/// Delivers a reaction request from another device to this fixture's device.
fn deliver_request(
    fixture: &Fixture,
    target: DeviceId,
    message: MessageId,
    emoji: Option<&str>,
    requested_at_ms: i64,
) -> ReactionRequestId {
    let request = ReactionRequestId::new();
    let conversation = ConversationId::new();
    let payload = PrivatePayload::ReactionRequest {
        request_id: request,
        message_id: message,
        conversation_id: conversation,
        target_gateway_id: target,
        emoji: emoji.map(str::to_owned),
        requested_at_ms,
    };
    let envelope = fixture.event(DeviceId::new(), 1, 1, payload);
    let cursor = next_cursor(fixture);
    assert_eq!(fixture.apply(&envelope, cursor).applied, 1);
    request
}

fn insert_request(
    fixture: &Fixture,
    request: ReactionRequestId,
    target: DeviceId,
    requested_at_ms: i64,
    key_epoch: u32,
    from_snapshot: bool,
) {
    fixture.execute(&format!(
        "INSERT INTO reaction_requests(request_id,message_id,conversation_id,target_gateway_id,emoji,requested_at_ms,requester_device_id,key_epoch,state,updated_at_ms,from_snapshot)
         VALUES('{request}','{}','{}','{target}','👍',{requested_at_ms},'{}',{key_epoch},'sending',0,{})",
        MessageId::new(),
        ConversationId::new(),
        DeviceId::new(),
        i64::from(from_snapshot),
    ));
}

/// A message with an own reaction already in the stored set.
fn seed_own_reaction(
    fixture: &Fixture,
    message: MessageId,
    conversation: ConversationId,
    emoji: Option<&str>,
) {
    let Some(emoji) = emoji else { return };
    let store = fixture.client.lock().unwrap();
    crate::reactions::store_message_reactions(
        &store.conn,
        message,
        conversation,
        DeviceId::new(),
        1,
        1,
        &[reaction(emoji, vec![actor(true, None)])],
    )
    .unwrap();
}

/// A linked RCS incoming message with its Google identity, ready for a phone attempt.
fn ready_rcs_message(fixture: &Fixture, google_message: &str) -> (MessageId, ConversationId) {
    let conversation = ConversationId::new();
    let message = seed_message(
        fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    seed_rcs_identity(fixture, message, conversation, google_message);
    mark_link_linked(fixture);
    (message, conversation)
}

#[test]
fn reaction_jobs_offer_a_live_request_for_this_gateway_until_it_is_attempted() {
    let fixture = Fixture::new();
    let own = fixture.device_id;
    let request = deliver_request(&fixture, own, MessageId::new(), Some("👍"), now());
    deliver_request(
        &fixture,
        DeviceId::new(),
        MessageId::new(),
        Some("👍"),
        now(),
    );

    let jobs = fixture.client.pending_reaction_jobs().unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "another gateway's request is not this phone's job"
    );
    assert_eq!(jobs[0].request_id, request);
    assert_eq!(jobs[0].emoji.as_deref(), Some("👍"));
    assert_eq!(
        fixture.client.pending_reaction_jobs().unwrap().len(),
        1,
        "listing is not an attempt"
    );
}

#[test]
fn reaction_jobs_skip_snapshot_rows_and_answer_expired_and_retired_requests_not_sent() {
    let fixture = Fixture::new();
    let own = fixture.device_id;
    let (snapshot, expired, retired) = (
        ReactionRequestId::new(),
        ReactionRequestId::new(),
        ReactionRequestId::new(),
    );
    insert_request(&fixture, snapshot, own, now(), 1, true);
    insert_request(
        &fixture,
        expired,
        own,
        now() - REACTION_REQUEST_TTL_MS - 1_000,
        1,
        false,
    );
    insert_request(&fixture, retired, own, now(), 0, false);
    let before = outbox_count(&fixture);

    assert!(fixture.client.pending_reaction_jobs().unwrap().is_empty());

    assert_eq!(
        state_of(&fixture, snapshot).as_deref(),
        Some("sending"),
        "a snapshot never authorizes or answers"
    );
    assert_eq!(state_of(&fixture, expired).as_deref(), Some("not_sent"));
    assert_eq!(state_of(&fixture, retired).as_deref(), Some("not_sent"));
    assert_eq!(
        fixture
            .query_text(&format!(
                "SELECT refusal FROM reaction_requests WHERE request_id='{expired}'"
            ))
            .as_deref(),
        Some("expired")
    );
    assert_eq!(
        fixture
            .query_text(&format!(
                "SELECT refusal FROM reaction_requests WHERE request_id='{retired}'"
            ))
            .as_deref(),
        Some("expired")
    );
    assert_eq!(
        outbox_count(&fixture),
        before + 2,
        "each answer is a ReactionResult"
    );
}

#[test]
fn reaction_capability_publishes_only_when_the_value_changes() {
    let fixture = Fixture::new();
    let before = outbox_count(&fixture);
    fixture.client.publish_reaction_capability(true).unwrap();
    assert_eq!(outbox_count(&fixture), before + 1);
    fixture.client.publish_reaction_capability(true).unwrap();
    assert_eq!(
        outbox_count(&fixture),
        before + 1,
        "an unchanged capability is not republished"
    );
    fixture.client.publish_reaction_capability(false).unwrap();
    assert_eq!(outbox_count(&fixture), before + 2);
    assert_eq!(
        fixture
            .query_text(&format!(
                "SELECT supported FROM reaction_capabilities WHERE device_id='{}'",
                fixture.device_id
            ))
            .as_deref(),
        Some("0")
    );
}

#[test]
fn reaction_begin_permits_one_attempt_and_records_one_outcome() {
    let fixture = Fixture::new();
    let (message, _conversation) = ready_rcs_message(&fixture, "g-permit");
    let request = deliver_request(&fixture, fixture.device_id, message, Some("👍"), now());

    let decision = fixture.client.begin_reaction_attempt(request).unwrap();
    assert_eq!(
        decision,
        ReactionAttemptDecision::Permit {
            request_id: request,
            google_conversation_id: "google-conversation".to_owned(),
            google_message_id: "g-permit".to_owned(),
            action: ReactionAction::Add,
            emoji: "👍".to_owned(),
        }
    );
    let before = outbox_count(&fixture);
    fixture
        .client
        .record_reaction_outcome(request, ReactionResultState::Sent)
        .unwrap();
    assert_eq!(state_of(&fixture, request).as_deref(), Some("sent"));
    assert_eq!(outbox_count(&fixture), before + 1);

    fixture
        .client
        .record_reaction_outcome(request, ReactionResultState::Unknown)
        .unwrap();
    assert_eq!(
        state_of(&fixture, request).as_deref(),
        Some("sent"),
        "a settled attempt is not rewritten"
    );
    assert_eq!(
        outbox_count(&fixture),
        before + 1,
        "and not published twice"
    );
    assert_eq!(
        fixture.client.begin_reaction_attempt(request).unwrap(),
        ReactionAttemptDecision::Refused {
            reason: ReactionRefusal::Busy
        }
    );
}

#[test]
fn reaction_second_begin_during_an_unfinished_attempt_is_refused_and_settles_it_unknown() {
    let fixture = Fixture::new();
    let (message, _) = ready_rcs_message(&fixture, "g-second");
    let request = deliver_request(&fixture, fixture.device_id, message, Some("👍"), now());
    assert!(matches!(
        fixture.client.begin_reaction_attempt(request).unwrap(),
        ReactionAttemptDecision::Permit { .. }
    ));
    let before = outbox_count(&fixture);

    assert_eq!(
        fixture.client.begin_reaction_attempt(request).unwrap(),
        ReactionAttemptDecision::Refused {
            reason: ReactionRefusal::Busy
        },
        "a second begin is never a second permit"
    );
    assert_eq!(
        state_of(&fixture, request).as_deref(),
        Some("unknown"),
        "the unfinished attempt is treated as unknown"
    );
    assert_eq!(
        outbox_count(&fixture),
        before + 1,
        "and its ReactionResult is published once"
    );
    fixture
        .client
        .record_reaction_outcome(request, ReactionResultState::Sent)
        .unwrap();
    assert_eq!(
        state_of(&fixture, request).as_deref(),
        Some("unknown"),
        "a late outcome cannot overturn unknown"
    );
}

#[test]
fn reaction_action_table_follows_the_current_and_desired_emoji() {
    let cases = [
        (None, None, ReactionAction::Noop),
        (None, Some("👍"), ReactionAction::Add),
        (Some("👍"), None, ReactionAction::Remove),
        (Some("👍"), Some("👍"), ReactionAction::Noop),
        (Some("👍"), Some("❤️"), ReactionAction::Switch),
    ];
    for (current, desired, expected) in cases {
        let fixture = Fixture::new();
        let (message, conversation) = ready_rcs_message(&fixture, "g-table");
        seed_own_reaction(&fixture, message, conversation, current);
        let request = deliver_request(&fixture, fixture.device_id, message, desired, now());
        match fixture.client.begin_reaction_attempt(request).unwrap() {
            ReactionAttemptDecision::Permit { action, .. } => {
                assert_eq!(action, expected, "{current:?} -> {desired:?}")
            }
            other => panic!("{current:?} -> {desired:?} refused: {other:?}"),
        }
    }
}

#[test]
fn reaction_begin_refuses_each_unreachable_message_and_records_not_sent() {
    let cases = [
        (ReactionRefusal::MessageUnavailable, "missing"),
        (ReactionRefusal::NotRcs, "sms"),
        (ReactionRefusal::MessageUnavailable, "no_identity"),
        (ReactionRefusal::MessageUnavailable, "conflicted"),
        (ReactionRefusal::LinkUnavailable, "unlinked"),
    ];
    for (expected, case) in cases {
        let fixture = Fixture::new();
        let conversation = ConversationId::new();
        let message = match case {
            "missing" => MessageId::new(),
            "sms" => seed_message(
                &fixture,
                Transport::Sms,
                Direction::Incoming,
                "hi",
                conversation,
            ),
            "no_identity" => seed_message(
                &fixture,
                Transport::Rcs,
                Direction::Incoming,
                "hi",
                conversation,
            ),
            "conflicted" => {
                let message = ready_rcs_message(&fixture, "g-conflict").0;
                fixture.execute(&format!(
                    "UPDATE rcs_mirror_ids SET conflicted=1 WHERE message_id='{message}'"
                ));
                message
            }
            _ => {
                let message = seed_message(
                    &fixture,
                    Transport::Rcs,
                    Direction::Incoming,
                    "hi",
                    conversation,
                );
                seed_rcs_identity(&fixture, message, conversation, "g-unlinked");
                message
            }
        };
        let request = deliver_request(&fixture, fixture.device_id, message, Some("👍"), now());
        let before = outbox_count(&fixture);
        assert_eq!(
            fixture.client.begin_reaction_attempt(request).unwrap(),
            ReactionAttemptDecision::Refused {
                reason: expected.clone()
            },
            "{case}"
        );
        assert_eq!(
            state_of(&fixture, request).as_deref(),
            Some("not_sent"),
            "{case}"
        );
        assert_eq!(
            outbox_count(&fixture),
            before + 1,
            "{case} publishes its refusal"
        );
    }
}

#[test]
fn reaction_unfinished_attempt_found_on_the_next_begin_is_unknown_and_refused() {
    let fixture = Fixture::new();
    let (message, _) = ready_rcs_message(&fixture, "g-crash");
    let request = deliver_request(&fixture, fixture.device_id, message, Some("👍"), now());
    fixture.execute(&format!(
        "INSERT INTO reaction_attempts(request_id,begun_at_ms) VALUES('{request}',{})",
        now()
    ));
    assert_eq!(
        fixture.client.begin_reaction_attempt(request).unwrap(),
        ReactionAttemptDecision::Refused {
            reason: ReactionRefusal::Busy
        }
    );
    assert_eq!(state_of(&fixture, request).as_deref(), Some("unknown"));
}

#[test]
fn reaction_begin_refuses_an_expired_or_retired_request() {
    let fixture = Fixture::new();
    let (expired, retired) = (ReactionRequestId::new(), ReactionRequestId::new());
    insert_request(
        &fixture,
        expired,
        fixture.device_id,
        now() - REACTION_REQUEST_TTL_MS - 5_000,
        1,
        false,
    );
    insert_request(&fixture, retired, fixture.device_id, now(), 0, false);
    for request in [expired, retired] {
        assert_eq!(
            fixture.client.begin_reaction_attempt(request).unwrap(),
            ReactionAttemptDecision::Refused {
                reason: ReactionRefusal::Expired
            }
        );
        assert_eq!(state_of(&fixture, request).as_deref(), Some("not_sent"));
    }
}

#[test]
fn reaction_ingest_publishes_only_a_changed_set_and_canonicalises_addresses() {
    let fixture = Fixture::new();
    let (message, conversation) = ready_rcs_message(&fixture, "g-ingest");
    grant_eligibility(&fixture, fixture.device_id, conversation);
    let before = outbox_count(&fixture);
    let first = vec![
        RcsReactionEntry {
            emoji: "👍".into(),
            is_self: true,
            addresses: vec![],
            unresolved_actors: 0,
        },
        RcsReactionEntry {
            emoji: "❤️".into(),
            is_self: false,
            addresses: vec!["+1 415 555 2671".into(), "+1 415 555 2671".into()],
            unresolved_actors: 1,
        },
    ];
    let other = vec![RcsReactionEntry {
        emoji: "😂".into(),
        is_self: false,
        addresses: vec![],
        unresolved_actors: 1,
    }];

    assert!(
        fixture
            .client
            .ingest_rcs_message_reactions("google-conversation", "g-ingest", first.clone(), 100)
            .unwrap()
    );
    assert_eq!(outbox_count(&fixture), before + 1);
    assert!(
        !fixture
            .client
            .ingest_rcs_message_reactions("google-conversation", "g-ingest", first.clone(), 200)
            .unwrap(),
        "unchanged set"
    );
    assert!(
        !fixture
            .client
            .ingest_rcs_message_reactions("google-conversation", "g-ingest", other, 50)
            .unwrap(),
        "an older observation never publishes"
    );
    assert_eq!(outbox_count(&fixture), before + 1);

    let stored = fixture
        .query_text(&format!(
            "SELECT reactions_json FROM message_reactions WHERE message_id='{message}'"
        ))
        .unwrap();
    assert!(stored.contains("+14155552671"), "{stored}");
    assert_eq!(
        stored.matches("+14155552671").count(),
        1,
        "one canonical actor per address"
    );

    let invalid = vec![RcsReactionEntry {
        emoji: "hello".into(),
        is_self: false,
        addresses: vec![],
        unresolved_actors: 1,
    }];
    assert!(matches!(
        fixture.client.ingest_rcs_message_reactions(
            "google-conversation",
            "g-ingest",
            invalid,
            300
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert!(
        !fixture
            .client
            .ingest_rcs_message_reactions("google-conversation", "unknown-google", first, 300)
            .unwrap()
    );
}

#[test]
fn reaction_sent_resolves_only_for_a_set_observed_after_the_request() {
    let fixture = Fixture::new();
    let (message, conversation) = ready_rcs_message(&fixture, "g-resolve");
    grant_eligibility(&fixture, fixture.device_id, conversation);
    let requested_at = now();
    let request = deliver_request(
        &fixture,
        fixture.device_id,
        message,
        Some("❤️"),
        requested_at,
    );
    let self_set = |emoji: &str| {
        vec![RcsReactionEntry {
            emoji: emoji.into(),
            is_self: true,
            addresses: vec![],
            unresolved_actors: 0,
        }]
    };

    fixture
        .client
        .ingest_rcs_message_reactions(
            "google-conversation",
            "g-resolve",
            self_set("❤️"),
            requested_at - 1_000,
        )
        .unwrap();
    assert_eq!(
        state_of(&fixture, request).as_deref(),
        Some("sending"),
        "a set from before the request is not evidence"
    );

    fixture
        .client
        .ingest_rcs_message_reactions(
            "google-conversation",
            "g-resolve",
            self_set("👍"),
            requested_at + 1_000,
        )
        .unwrap();
    assert_eq!(
        state_of(&fixture, request).as_deref(),
        Some("sending"),
        "a different emoji does not satisfy the request"
    );

    fixture
        .client
        .ingest_rcs_message_reactions(
            "google-conversation",
            "g-resolve",
            self_set("❤️"),
            requested_at + 2_000,
        )
        .unwrap();
    assert_eq!(state_of(&fixture, request).as_deref(), Some("sent"));
}

#[test]
fn reaction_unknown_request_resolves_when_a_later_set_matches() {
    let fixture = Fixture::new();
    let (message, conversation) = ready_rcs_message(&fixture, "g-unknown");
    grant_eligibility(&fixture, fixture.device_id, conversation);
    let requested_at = now();
    let request = deliver_request(&fixture, fixture.device_id, message, None, requested_at);
    fixture.execute(&format!(
        "UPDATE reaction_requests SET state='unknown' WHERE request_id='{request}'"
    ));
    fixture
        .client
        .ingest_rcs_message_reactions(
            "google-conversation",
            "g-unknown",
            vec![RcsReactionEntry {
                emoji: "👍".into(),
                is_self: false,
                addresses: vec!["+15550100".into()],
                unresolved_actors: 0,
            }],
            requested_at + 1_000,
        )
        .unwrap();
    assert_eq!(
        state_of(&fixture, request).as_deref(),
        Some("sent"),
        "a removal is satisfied when the user no longer reacts"
    );
}

#[test]
fn reaction_request_from_a_retired_epoch_is_answered_not_sent_after_rotation() {
    let fixture = Fixture::new();
    let request = deliver_request(
        &fixture,
        fixture.device_id,
        MessageId::new(),
        Some("👍"),
        now(),
    );
    let passphrase = "epoch two has its own passphrase";
    let profile = KeyProfile::new(fixture.vault_id.0, 2).unwrap();
    let root = derive_root_key(passphrase, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    fixture
        .client
        .unlock(&profile, &header, passphrase)
        .unwrap();
    fixture.client.activate_epoch(2).unwrap();

    assert!(fixture.client.pending_reaction_jobs().unwrap().is_empty());
    assert_eq!(state_of(&fixture, request).as_deref(), Some("not_sent"));
    assert_eq!(
        fixture
            .query_text(&format!(
                "SELECT refusal FROM reaction_requests WHERE request_id='{request}'"
            ))
            .as_deref(),
        Some("expired")
    );
}

#[test]
fn reaction_restart_settles_an_unfinished_attempt_through_the_phone_api() {
    let fixture = Fixture::new();
    let (message, _) = ready_rcs_message(&fixture, "g-restart");
    let request = deliver_request(&fixture, fixture.device_id, message, Some("👍"), now());
    assert!(matches!(
        fixture.client.begin_reaction_attempt(request).unwrap(),
        ReactionAttemptDecision::Permit { .. }
    ));
    let before = outbox_count(&fixture);
    let (_dir, reopened, _profile) = reopen_unlocked(fixture);
    assert_eq!(
        reopened.begin_reaction_attempt(request).unwrap(),
        ReactionAttemptDecision::Refused {
            reason: ReactionRefusal::Busy
        }
    );
    assert_eq!(
        outbox_count_of(&reopened),
        before + 1,
        "the unknown outcome is published once"
    );
    assert!(reopened.pending_reaction_jobs().unwrap().is_empty());
}
