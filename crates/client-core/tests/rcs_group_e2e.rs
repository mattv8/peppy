//! End-to-end group sends (M4 C4e): desktop send, command sync to the gateway, gateway auto-routing
//! (D5) through create, ingest, and the native RCS pipeline. Two real clients share one vault and
//! exchange commands through the mock server, exactly as sender and gateway devices do.
#[path = "common/mod.rs"]
mod common;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

const GOOGLE: &str = "g-weekend";
// Valid numbers: provider-row routing canonicalises with libphonenumber.
const SELF: &str = "+14155550100";
const ALEX: &str = "+14155552671";
const SAM: &str = "+14155552672";
const LEE: &str = "+14155552673";
const SUBSCRIPTION: &str = "android-subscription-7";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Go's member digest over the non-self numbers: SHA-256 of the sorted numbers joined by LF.
fn digest(members: &[&str]) -> String {
    let mut sorted = members.to_vec();
    sorted.sort_unstable();
    STANDARD.encode(Sha256::digest(sorted.join("\n").as_bytes()))
}

fn participant(number: &str, is_self: bool) -> Value {
    json!({
        "participant_id": format!("p-{number}"),
        "identifier_type": 1,
        "raw_number": number,
        "formatted_number": number,
        "participant_id_present": true,
        "number_present": true,
        "visible": true,
        "self": is_self,
        "canonical_address": number,
        "canonical_address_present": true,
        "display_name": "",
    })
}

/// Google's complete snapshot: one group with this gateway plus the visible non-self `members`.
fn snapshot(conversations: Vec<Value>) -> String {
    json!({
        "code": "conversation_candidate_snapshot",
        "snapshot_token": "1:2:1",
        "generation": 1,
        "stream_epoch": 2,
        "revision": 1,
        "run_incarnation": "run",
        "pairing_evidence_id": "pairing",
        "complete": true,
        "conversations": conversations,
    })
    .to_string()
}

fn group_at(google_id: &str, members: &[&str], last_message_ms: i64) -> Value {
    let mut participants = vec![participant(SELF, true)];
    participants.extend(members.iter().map(|number| participant(number, false)));
    json!({
        "google_conversation_id": google_id,
        "conversation_type": 2,
        "conversation_status": 1,
        "classification": "eligible",
        "group": true,
        "visible_non_self_count": members.len(),
        "last_message_ms": last_message_ms,
        "participants": participants,
        "name": "Weekend",
        "is_group_chat": true,
        "read_only": false,
        "invisible_participant_count": 0,
        "member_digest": digest(members),
    })
}

/// A fresh target observation of the group: self plus the visible non-self `members`, every
/// timestamp at `at`. The shape is the one the native pending tests use for 1:1 proofs.
fn observation(members: &[&str], at: i64) -> RcsMappingObservation {
    let participants = std::iter::once(RcsParticipantObservation {
        identity: SELF.into(),
        google_participant_id: "p-self".into(),
        identifier_type: 1,
        is_visible: true,
        is_me: true,
    })
    .chain(members.iter().map(|member| RcsParticipantObservation {
        identity: (*member).into(),
        google_participant_id: format!("p-{member}"),
        identifier_type: 1,
        is_visible: true,
        is_me: false,
    }))
    .collect();
    RcsMappingObservation {
        google_conversation_id: GOOGLE.into(),
        google_candidate_count: 1,
        participants,
        conversation_type: RcsConversationType::Rcs,
        status: "eligible".into(),
        send_mode: RcsSendMode::Automatic,
        read_only: false,
        default_outgoing: true,
        selected_sim_id: SUBSCRIPTION.into(),
        selected_sim_own_identity: SELF.into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: SUBSCRIPTION.into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: SELF.into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: at,
            route_context_revision: 1,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: NativeRcsOutgoing {
            outgoing_id: "google-outgoing".into(),
            sim_participant_id: "google-outgoing".into(),
            own_identity_raw: SELF.into(),
            sim_payload: NativeRcsSimPayload {
                two: 2,
                sim_number: 7,
            },
            rcs_chats_enabled: true,
        },
        native_evidence: NativeRcsEvidence {
            lifetime: NativeRcsLifetime {
                runtime_id: "runtime".into(),
                run_generation: 1,
                link_generation: 1,
                session_generation: 1,
                context_revision: 1,
            },
            lookup_id: "lookup".into(),
            lookup_complete: true,
            google_candidate_count: 1,
            settings_revision: 1,
            settings_provenance: NativeSettingsProvenance::CurrentEvent,
            settings_observed_at_ms: at,
            settings_stream_epoch: 1,
            settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
            current_settings_sims: vec![NativeRcsOutgoing {
                outgoing_id: "google-outgoing".into(),
                sim_participant_id: "google-outgoing".into(),
                own_identity_raw: SELF.into(),
                sim_payload: NativeRcsSimPayload {
                    two: 2,
                    sim_number: 7,
                },
                rcs_chats_enabled: true,
            }],
            settings_validated_at_ms: at,
            conversation_observed_at_ms: at,
        },
        identity_region: "US".into(),
        observed_at_ms: at,
        context_revision: 1,
        link_generation: 1,
        session_generation: 1,
        canonicalizer_revision: 1,
        mapping_revision: 1,
        link_state: RcsLinkState::Linked,
    }
}

/// A sender and a gateway sharing one vault. The gateway is linked and has a region when `live`.
struct Group {
    _directory: tempfile::TempDir,
    gateway_device: DeviceId,
    sender: Client,
    gateway: Client,
    server: Server,
}

impl Group {
    fn new(live: bool) -> Self {
        let directory = tempfile::TempDir::new().unwrap();
        let vault = Vault::new();
        let sender_config = config(&directory, "sender", &vault);
        let gateway_config = config(&directory, "gateway", &vault);
        let sender = unlocked(&sender_config, &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let now = now_ms();
        let book = json!({
            "schema_version": 1,
            "book": {
                "id": "group-region", "owner_device_id": gateway_config.device_id.to_string(),
                "generation": "1", "state": "active", "region": "US"
            },
            "contacts": []
        });
        gateway.capture_contact_book(&book.to_string()).unwrap();
        if live {
            gateway
                .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now, now)
                .unwrap();
            gateway
                .verify_rcs_mapping(&observation(&[ALEX, SAM], now), now)
                .unwrap();
        }
        Self {
            _directory: directory,
            gateway_device: gateway_config.device_id,
            sender,
            gateway,
            server: Server::default(),
        }
    }

    /// The desktop sends a group text. The draft carries a carrier route; the gateway decides.
    fn send(&mut self, members: &[&str]) -> (ConversationId, CommandId) {
        self.send_with(members, false, None)
    }

    /// As `send`, carrying the explicit duplicate confirmation a user gives after an unknown create
    /// and the member revision the composer saw (required once the group is Live).
    fn send_with(
        &mut self,
        members: &[&str],
        confirm_duplicate: bool,
        expected_member_revision: Option<u64>,
    ) -> (ConversationId, CommandId) {
        let draft = self
            .sender
            .save_new_compose_draft(ComposeDraftUpdate {
                group_name: None,
                text: "hello group".into(),
                recipients: members.iter().map(|m| (*m).to_owned()).collect(),
                attachment_ids: Vec::new(),
                route: Some(GatewayRoute::Carrier {
                    gateway_device_id: self.gateway_device,
                    subscription_id: SUBSCRIPTION.into(),
                }),
            })
            .unwrap();
        let queued = self
            .sender
            .send_compose_draft_for_group(
                draft.draft_id,
                draft.revision,
                GroupDraftSend {
                    group_name: Some("Weekend".into()),
                    expected_member_revision,
                    group_create_confirm_duplicate: confirm_duplicate,
                },
            )
            .unwrap();
        self.server.upload(&self.sender);
        self.server.sync(&self.gateway);
        (draft.conversation_id, queued.command_id)
    }

    fn ingest(&self, snapshot: &str, now: i64) -> GroupIngestReport {
        self.gateway.ingest_group_candidates(snapshot, now).unwrap()
    }

    fn pending(&self, command: CommandId) -> bool {
        self.gateway
            .pending_native_rcs_commands()
            .unwrap()
            .iter()
            .any(|candidate| candidate.command_id == command)
    }

    /// Carrier admission for the command, as the carrier host asks it.
    fn carrier(&self, command: CommandId) -> PermitDecision {
        self.gateway.begin_send_attempt(command).unwrap()
    }
}

/// After a native attempt exists, carrier must never permit the same command: it reports the
/// recorded attempt (or a block), so the message cannot also go out as MMS.
fn carrier_never_permits(decision: &PermitDecision) -> bool {
    !matches!(decision, PermitDecision::Permit(_))
}

#[test]
fn live_group_text_routes_to_rcs_once_and_projects_a_group_class() {
    let mut group = Group::new(true);
    let (conversation, command) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    let report = group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    assert_eq!(report.linked, 1);
    assert_eq!(
        group
            .gateway
            .proved_group_member_revision(conversation)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        group
            .gateway
            .verify_rcs_group_observation(&observation(&[ALEX, SAM], now), now)
            .unwrap(),
        GroupProofOutcome::Proven
    );

    // D5: the carrier-routed group text is a native candidate and prepares with the member digest.
    assert!(group.pending(command));
    let preparation = group.gateway.prepare_native_rcs_send(command, now).unwrap();
    assert_eq!(
        preparation.expected_member_digest(),
        Some(digest(&[ALEX, SAM]).as_str())
    );
    let committed = group
        .gateway
        .begin_native_rcs_send(preparation, &observation(&[ALEX, SAM], now), now)
        .unwrap();
    assert_eq!(
        committed.expected_member_digest.as_deref(),
        Some(digest(&[ALEX, SAM]).as_str())
    );
    assert_eq!(committed.target.class, NativeRcsTargetClass::Group);

    // Exactly once: the native attempt is recorded, so it is never listed or prepared again, and
    // the carrier path never sends it.
    assert!(!group.pending(command));
    assert!(group.gateway.prepare_native_rcs_send(command, now).is_err());
    assert!(carrier_never_permits(&group.carrier(command)));
}

#[test]
fn a_stale_composer_revision_is_members_changed_and_never_mms() {
    let mut group = Group::new(true);
    let (conversation, command) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    assert_eq!(
        group
            .gateway
            .proved_group_member_revision(conversation)
            .unwrap(),
        Some(1)
    );

    // Google adds a member: the group re-proves at revision 2. The command still carries 1.
    let changed = group.ingest(
        &snapshot(vec![group_at(GOOGLE, &[ALEX, SAM, LEE], now)]),
        now,
    );
    assert_eq!(changed.members_changed, 1);
    assert_eq!(
        group
            .gateway
            .proved_group_member_revision(conversation)
            .unwrap(),
        Some(2)
    );
    assert!(matches!(
        group.carrier(command),
        PermitDecision::Blocked(PermitBlock::GroupRefused(SendRefusalReason::MembersChanged))
    ));
    assert_eq!(
        group.gateway.group_send_reason(command).unwrap(),
        Some(SendRefusalReason::MembersChanged)
    );
    assert!(group.gateway.prepare_native_rcs_send(command, now).is_err());

    // The refusal reaches the sender's other devices through the gateway's send-status event.
    group.server.upload(&group.gateway);
    group.server.sync(&group.sender);
    let sent = group
        .sender
        .messages(conversation)
        .unwrap()
        .into_iter()
        .find(|message| message.payload.direction == peppy_client_core::Direction::Outgoing)
        .unwrap();
    assert_eq!(sent.send_refusal, Some(SendRefusalReason::MembersChanged));
    assert_eq!(
        sent.send_state,
        Some(peppy_client_core::SendState::FailedBeforeSubmission)
    );
}

#[test]
fn a_prepared_group_send_never_falls_back_to_carrier_when_the_roster_changes() {
    let mut group = Group::new(true);
    let (_, command) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    let preparation = group.gateway.prepare_native_rcs_send(command, now).unwrap();

    // The roster changes between prepare and begin: the native permit is refused and the group
    // pauses. Carrier admission then refuses too, so the message is never sent as MMS.
    group.ingest(
        &snapshot(vec![group_at(GOOGLE, &[ALEX, SAM, LEE], now)]),
        now,
    );
    assert!(matches!(
        group
            .gateway
            .begin_native_rcs_send(preparation, &observation(&[ALEX, SAM], now), now),
        Err(Error::GroupMembersChanged)
    ));
    let decision = group.carrier(command);
    assert!(
        matches!(
            decision,
            PermitDecision::Blocked(PermitBlock::GroupRefused(SendRefusalReason::MembersChanged))
        ),
        "a prepared group send must not reach carrier, got {decision:?}"
    );
}

#[test]
fn a_withdrawn_group_refuses_its_queued_text() {
    let mut group = Group::new(true);
    let (_, command) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    // Google's complete snapshot no longer has the group: it is withdrawn.
    group.ingest(&snapshot(Vec::new()), now);
    assert!(matches!(
        group.carrier(command),
        PermitDecision::Blocked(PermitBlock::GroupRefused(SendRefusalReason::Withdrawn))
    ));
    assert_eq!(
        group.gateway.group_send_reason(command).unwrap(),
        Some(SendRefusalReason::Withdrawn)
    );
    assert!(group.gateway.prepare_native_rcs_send(command, now).is_err());
}

#[test]
fn an_unlinked_group_text_goes_carrier_mms_as_before() {
    let mut group = Group::new(false);
    let (_, command) = group.send(&[ALEX, SAM]);
    assert!(
        !group.pending(command),
        "an unlinked group is not a native candidate"
    );
    match group.carrier(command) {
        PermitDecision::Permit(received) => {
            assert_eq!(received.message.transport, Transport::Mms);
            assert!(received.message.record.attachments.is_empty());
        }
        other => panic!("an unlinked group text must go carrier MMS, got {other:?}"),
    }
    assert_eq!(group.gateway.group_send_reason(command).unwrap(), None);
}

#[test]
fn a_create_pending_group_text_sends_over_rcs_once_the_created_group_goes_live() {
    let mut group = Group::new(true);
    let (conversation, command) = group.send(&[ALEX, SAM]);
    let now = now_ms();

    // No mapping yet: the message waits for the create. It is neither sent nor refused.
    assert!(matches!(
        group.carrier(command),
        PermitDecision::Blocked(PermitBlock::GroupCreatePending)
    ));
    assert!(!group.pending(command));

    let permit = match group
        .gateway
        .begin_rcs_group_create(conversation, command, now)
        .unwrap()
    {
        GroupCreateBegin::Permit(permit) => permit,
        other => panic!("expected a create permit, got {other:?}"),
    };
    assert_eq!(permit.numbers, vec![ALEX.to_owned(), SAM.to_owned()]);
    assert_eq!(
        group
            .gateway
            .commit_rcs_group_create(
                command,
                &json!({"code": "created_rcs_group", "google_conversation_id": GOOGLE}).to_string()
            )
            .unwrap(),
        GroupCreateOutcome::Pending
    );
    assert!(matches!(
        group.carrier(command),
        PermitDecision::Blocked(PermitBlock::GroupCreatePending)
    ));

    // The next complete snapshot proves the created group: the mapping goes Live, and the same
    // queued message is now a native candidate, sent over RCS exactly once.
    let report = group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    assert_eq!(report.linked, 1);
    assert_eq!(
        group
            .gateway
            .proved_group_member_revision(conversation)
            .unwrap(),
        Some(1)
    );
    assert!(group.pending(command));
    assert_eq!(
        group
            .gateway
            .verify_rcs_group_observation(&observation(&[ALEX, SAM], now), now)
            .unwrap(),
        GroupProofOutcome::Proven
    );
    let preparation = group.gateway.prepare_native_rcs_send(command, now).unwrap();
    let committed = group
        .gateway
        .begin_native_rcs_send(preparation, &observation(&[ALEX, SAM], now), now)
        .unwrap();
    assert_eq!(committed.target.class, NativeRcsTargetClass::Group);
    assert!(!group.pending(command));
    assert!(carrier_never_permits(&group.carrier(command)));
}

#[test]
fn a_create_that_is_not_rcs_informs_that_message_and_later_sends_are_not_informed() {
    let mut group = Group::new(true);
    let (conversation, first) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    group
        .gateway
        .begin_rcs_group_create(conversation, first, now)
        .unwrap();
    assert_eq!(
        group
            .gateway
            .commit_rcs_group_create(first, r#"{"code":"not_rcs"}"#)
            .unwrap(),
        GroupCreateOutcome::NotRcs
    );
    assert_eq!(
        group.gateway.group_send_reason(first).unwrap(),
        Some(SendRefusalReason::CreateNotRcs)
    );
    assert!(
        !group.pending(first),
        "a not-RCS group never takes the RCS path"
    );

    // A later message to the same group is not a create trigger and carries no reason.
    let (_, later) = group.send(&[ALEX, SAM]);
    assert_eq!(group.gateway.group_send_reason(later).unwrap(), None);
    assert!(!group.pending(later));
    assert!(!matches!(
        group.carrier(later),
        PermitDecision::Blocked(PermitBlock::GroupCreatePending)
    ));
}

#[test]
fn an_unknown_create_refuses_the_message_as_create_unconfirmed_and_never_creates_again() {
    let mut group = Group::new(true);
    let (conversation, first) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    group
        .gateway
        .begin_rcs_group_create(conversation, first, now)
        .unwrap();
    assert_eq!(
        group
            .gateway
            .commit_rcs_group_create(first, r#"{"code":"unknown"}"#)
            .unwrap(),
        GroupCreateOutcome::Unknown
    );
    assert!(matches!(
        group.carrier(first),
        PermitDecision::Blocked(PermitBlock::GroupRefused(
            SendRefusalReason::CreateUnconfirmed
        ))
    ));
    assert_eq!(
        group.gateway.group_send_reason(first).unwrap(),
        Some(SendRefusalReason::CreateUnconfirmed)
    );

    // The next message does not create a second group while the outcome is unknown.
    let (_, second) = group.send(&[ALEX, SAM]);
    assert!(matches!(
        group.carrier(second),
        PermitDecision::Blocked(PermitBlock::GroupRefused(
            SendRefusalReason::CreateUnconfirmed
        ))
    ));
    assert!(
        group
            .gateway
            .begin_rcs_group_create(conversation, second, now)
            .is_ok_and(|begin| matches!(
                begin,
                GroupCreateBegin::Refused(SendRefusalReason::CreateUnconfirmed)
            ))
    );
}

/// Begins the message's create permit and commits Google's `answer` for it.
fn create_with(
    group: &Group,
    conversation: ConversationId,
    command: CommandId,
    answer: &str,
) -> GroupCreateOutcome {
    match group
        .gateway
        .begin_rcs_group_create(conversation, command, now_ms())
        .unwrap()
    {
        GroupCreateBegin::Permit(_) => {}
        other => panic!("expected a create permit, got {other:?}"),
    }
    group
        .gateway
        .commit_rcs_group_create(command, answer)
        .unwrap()
}

/// A message whose own create went unknown is terminal: no candidate, no native prepare, and carrier
/// refuses it as create-unconfirmed, whatever the group later links to.
fn assert_terminal(group: &Group, command: CommandId) {
    assert!(!group.pending(command));
    assert!(
        group
            .gateway
            .prepare_native_rcs_send(command, now_ms())
            .is_err()
    );
    assert!(matches!(
        group.carrier(command),
        PermitDecision::Blocked(PermitBlock::GroupRefused(
            SendRefusalReason::CreateUnconfirmed
        ))
    ));
}

#[test]
fn an_unknown_create_message_stays_unsent_when_a_confirmed_duplicate_links_the_group() {
    let mut group = Group::new(true);
    let (conversation, first) = group.send(&[ALEX, SAM]);
    assert_eq!(
        create_with(&group, conversation, first, r#"{"code":"unknown"}"#),
        GroupCreateOutcome::Unknown
    );
    // The user chooses to create again: a new command carrying the same text.
    let (_, second) = group.send_with(&[ALEX, SAM], true, Some(1));
    let created =
        json!({"code": "created_rcs_group", "google_conversation_id": GOOGLE}).to_string();
    assert_eq!(
        create_with(&group, conversation, second, &created),
        GroupCreateOutcome::Pending
    );
    let now = now_ms();
    let report = group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    assert_eq!(report.linked, 1);

    assert!(group.pending(second));
    assert_terminal(&group, first);
    let preparation = group.gateway.prepare_native_rcs_send(second, now).unwrap();
    group
        .gateway
        .begin_native_rcs_send(preparation, &observation(&[ALEX, SAM], now), now)
        .unwrap();
    assert!(!group.pending(second));
    assert_terminal(&group, first);
}

#[test]
fn an_unknown_create_message_never_reenters_carrier_when_the_duplicate_is_not_rcs() {
    let mut group = Group::new(true);
    let (conversation, first) = group.send(&[ALEX, SAM]);
    assert_eq!(
        create_with(&group, conversation, first, r#"{"code":"unknown"}"#),
        GroupCreateOutcome::Unknown
    );
    let (_, second) = group.send_with(&[ALEX, SAM], true, Some(1));
    assert_eq!(
        create_with(&group, conversation, second, r#"{"code":"not_rcs"}"#),
        GroupCreateOutcome::NotRcs
    );
    assert!(!group.pending(second), "a not-RCS group takes no RCS path");
    assert_terminal(&group, first);
}

#[test]
fn an_unknown_create_message_stays_unsent_after_a_later_positive_reconciliation() {
    let mut group = Group::new(true);
    let (conversation, first) = group.send(&[ALEX, SAM]);
    assert_eq!(
        create_with(&group, conversation, first, r#"{"code":"unknown"}"#),
        GroupCreateOutcome::Unknown
    );
    let now = now_ms();
    let report = group.ingest(
        &snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now + 60_000)]),
        now,
    );
    assert_eq!(report.linked, 1);
    assert_eq!(
        group.gateway.group_create_state(conversation).unwrap(),
        GroupCreateState::Linked
    );
    assert_terminal(&group, first);
}

/// Uploads the gateway's pending synced events and delivers them to the desktop-side device.
fn deliver_to_desktop(group: &mut Group) {
    group.server.upload(&group.gateway);
    group.server.sync(&group.sender);
}

#[test]
fn a_remote_device_shows_the_gateways_link_state_from_synced_authority() {
    let mut group = Group::new(true);
    let (conversation, _command) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    deliver_to_desktop(&mut group);
    let view = group.sender.group_view(conversation).unwrap().unwrap();
    assert_eq!(view.state, GroupLinkState::Live);
    assert!(!view.can_create);
    assert!(!view.create_needs_confirmation);

    // A non-gateway device that runs its own reconciliation must not overwrite the gateway's state.
    group
        .sender
        .ingest_group_candidates(&snapshot(vec![]), now)
        .unwrap();
    let after_local_sync = group.sender.group_view(conversation).unwrap().unwrap();
    assert_eq!(after_local_sync.state, GroupLinkState::Live);

    // One visible non-self member is no longer provable: the gateway pauses the group.
    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX], now)]), now);
    deliver_to_desktop(&mut group);
    let paused = group.sender.group_view(conversation).unwrap().unwrap();
    assert_eq!(paused.state, GroupLinkState::Paused);

    // The group vanishes from a complete snapshot: withdrawn, and never shown as unlinked.
    group.ingest(&snapshot(vec![]), now);
    deliver_to_desktop(&mut group);
    let withdrawn = group.sender.group_view(conversation).unwrap().unwrap();
    assert_eq!(withdrawn.state, GroupLinkState::Withdrawn);
}

#[test]
fn a_remote_device_shows_an_unknown_create_as_needing_confirmation() {
    let mut group = Group::new(true);
    let (conversation, first) = group.send(&[ALEX, SAM]);
    let now = now_ms();
    group
        .gateway
        .begin_rcs_group_create(conversation, first, now)
        .unwrap();
    group
        .gateway
        .commit_rcs_group_create(first, r#"{"code":"unknown"}"#)
        .unwrap();
    deliver_to_desktop(&mut group);
    let view = group.sender.group_view(conversation).unwrap().unwrap();
    assert!(view.create_needs_confirmation);
    assert!(!view.can_create);
    assert_eq!(view.state, GroupLinkState::Unlinked);
}

fn group_eligibility(group: &Group, conversation: ConversationId) -> Option<RcsEligibility> {
    group
        .gateway
        .rcs_eligibility_projection(conversation, group.gateway_device, now_ms())
        .unwrap()
        .map(|projection| projection.eligibility)
}

#[test]
fn group_eligibility_follows_the_live_lifecycle_without_injected_projections() {
    let mut group = Group::new(true);
    let now = now_ms();
    group
        .gateway
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: group.gateway_device,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: now,
                capability_revision: 10,
                feature_bits: 1,
                linked_since_ms: Some(now),
            },
            &[],
            now,
        )
        .unwrap();
    let (conversation, _command) = group.send(&[ALEX, SAM]);

    // Ingest publishes eligibility itself once the group is Live.
    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    assert_eq!(
        group_eligibility(&group, conversation),
        Some(RcsEligibility::Eligible)
    );

    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX], now)]), now);
    assert_eq!(
        group_eligibility(&group, conversation),
        Some(RcsEligibility::NotEligible)
    );

    group.ingest(&snapshot(vec![group_at(GOOGLE, &[ALEX, SAM], now)]), now);
    assert_eq!(
        group_eligibility(&group, conversation),
        Some(RcsEligibility::Eligible)
    );

    group.ingest(&snapshot(vec![]), now);
    assert_eq!(
        group_eligibility(&group, conversation),
        Some(RcsEligibility::NotEligible)
    );
}
