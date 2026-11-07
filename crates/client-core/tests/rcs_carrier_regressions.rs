//! Public regression matrix for RCS carrier classification and admission (M2-B review finding 10).
//! Every case drives real `Client` APIs and asserts an exact decision. Refusals also assert that
//! no proof, mapping, diagnostic, or attempt was written. Each test names the fix it guards.

mod common;

use common::*;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

const SIM: &str = "android-subscription-7";
const OTHER_SIM: &str = "android-subscription-8";
const OWN: &str = "+12025550100";
const PEER: &str = "+12025550123";
const OTHER: &str = "+12025550124";
const NATIONAL_PEER: &str = "(202) 555-0123";
const NATIONAL_OWN: &str = "(202) 555-0100";
const TOKEN: &str = "1:1:1";
const SHORT_CODE_SMS: &str = "rcs_carrier_admission_short_code_sms";
const GROUP_MMS: &str = "rcs_carrier_admission_group_mms_pre_m4";

/// Linked native evidence for the selected SIM. `count` 1 yields a mapping; 0 yields only the
/// accepted route (no Google match), which is what classification needs.
fn observation(
    participant: &str,
    count: u32,
    revision: u64,
    observed_at_ms: i64,
) -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "conversation-local".into(),
        google_candidate_count: count,
        participants: vec![RcsParticipantObservation {
            identity: participant.into(),
            google_participant_id: "google-peer".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        }],
        conversation_type: RcsConversationType::Rcs,
        status: "eligible".into(),
        send_mode: RcsSendMode::Automatic,
        read_only: false,
        default_outgoing: true,
        selected_sim_id: SIM.into(),
        selected_sim_own_identity: OWN.into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: SIM.into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: OWN.into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: observed_at_ms,
            route_context_revision: revision,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: NativeRcsOutgoing {
            outgoing_id: "google-outgoing".into(),
            sim_participant_id: "google-outgoing".into(),
            own_identity_raw: OWN.into(),
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
                context_revision: revision,
            },
            lookup_id: "lookup".into(),
            lookup_complete: true,
            google_candidate_count: count,
            settings_revision: 1,
            settings_provenance: NativeSettingsProvenance::CurrentEvent,
            settings_observed_at_ms: observed_at_ms,
            settings_stream_epoch: 1,
            settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
            current_settings_sims: vec![NativeRcsOutgoing {
                outgoing_id: "google-outgoing".into(),
                sim_participant_id: "google-outgoing".into(),
                own_identity_raw: OWN.into(),
                sim_payload: NativeRcsSimPayload {
                    two: 2,
                    sim_number: 7,
                },
                rcs_chats_enabled: true,
            }],
            settings_validated_at_ms: observed_at_ms,
            conversation_observed_at_ms: observed_at_ms,
        },
        identity_region: "US".into(),
        observed_at_ms,
        link_generation: 1,
        session_generation: 1,
        context_revision: revision,
        canonicalizer_revision: 1,
        mapping_revision: revision,
        link_state: RcsLinkState::Linked,
    }
}

fn participant(number: &str, is_self: bool) -> Value {
    json!({
        "participant_id": format!("participant-{number}"),
        "identifier_type": 1,
        "raw_number": number,
        "formatted_number": "",
        "participant_id_present": true,
        "number_present": !number.is_empty(),
        "visible": true,
        "self": is_self,
    })
}

fn conversation(
    google_id: &str,
    conversation_type: i32,
    status: i32,
    classification: &str,
    participants: Vec<Value>,
) -> Value {
    let visible_non_self = participants
        .iter()
        .filter(|participant| participant["self"].as_bool() == Some(false))
        .count();
    json!({
        "google_conversation_id": google_id,
        "conversation_type": conversation_type,
        "conversation_status": status,
        "classification": classification,
        "group": visible_non_self > 1,
        "visible_non_self_count": visible_non_self,
        "participants": participants,
    })
}

fn eligible_peer(google_id: &str, number: &str) -> Value {
    conversation(
        google_id,
        2,
        1,
        "eligible",
        vec![participant(number, false)],
    )
}

fn snapshot(conversations: Vec<Value>, revision: u64) -> String {
    json!({
        "code": "conversation_candidate_snapshot",
        "snapshot_token": format!("1:1:{revision}"),
        "generation": 1,
        "stream_epoch": 1,
        "revision": revision,
        "run_incarnation": "runtime",
        "complete": true,
        "conversations": conversations,
    })
    .to_string()
}

fn proof(
    conversation_id: ConversationId,
    canonical: &str,
    identity_region: &str,
    canonicalizer_revision: u32,
    context_revision: u64,
) -> CarrierOnlyProof {
    CarrierOnlyProof {
        conversation_id,
        recipients: vec![canonical.into()],
        canonical_recipient: canonical.into(),
        identity_region: identity_region.into(),
        canonicalizer_revision,
        subscription_id: SIM.into(),
        observed_at_ms: 1_000,
        link_generation: 1,
        session_generation: 1,
        context_revision,
    }
}

fn encrypted_connection(config: &ClientConfig) -> Connection {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

/// A sender and a gateway over one vault. The gateway holds one captured conversation with `peer`.
struct Lab {
    _directory: tempfile::TempDir,
    gateway_config: ClientConfig,
    sender: Client,
    gateway: Client,
    server: Server,
    conversation: ConversationId,
}

impl Lab {
    fn new(peer: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let vault = Vault::new();
        let sender_config = config(&directory, "sender", &vault);
        let gateway_config = config(&directory, "gateway", &vault);
        let sender = unlocked(&sender_config, &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let conversation = gateway
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: peer.into(),
                body: String::new(),
                provider_message_id: Some("provider-local".into()),
                imported: true,
            })
            .unwrap()
            .conversation_id;
        Self {
            _directory: directory,
            gateway_config,
            sender,
            gateway,
            server: Server::default(),
            conversation,
        }
    }

    fn book(&self, book_id: &str, region: &str) {
        self.book_generation(book_id, region, "1");
    }

    fn book_generation(&self, book_id: &str, region: &str, generation: &str) {
        self.gateway
            .capture_contact_book(
                &json!({
                    "schema_version": 1,
                    "book": {
                        "id": book_id,
                        "owner_device_id": self.gateway_config.device_id.to_string(),
                        "generation": generation,
                        "state": "active",
                        "region": region,
                    },
                    "contacts": [],
                })
                .to_string(),
            )
            .unwrap();
    }

    /// Records accepted linked route evidence at revision 1 without any Google mapping.
    fn link(&self) {
        assert_eq!(
            self.gateway
                .verify_rcs_mapping(&observation(PEER, 0, 1, 1_000), 1_000)
                .unwrap(),
            RcsMappingOutcome::Refused(RcsMappingRefusal::GoogleConversationAmbiguous)
        );
    }

    fn queue(&mut self, command: impl FnOnce(&Client) -> QueuedSend) -> CommandId {
        let queued = command(&self.sender);
        self.server.upload(&self.sender);
        self.server.sync(&self.gateway);
        queued.command_id
    }

    fn command(&mut self, recipient: &str, subscription: &str) -> CommandId {
        let conversation_id = self.conversation;
        let route = GatewayRoute::Carrier {
            gateway_device_id: self.gateway_config.device_id,
            subscription_id: subscription.into(),
        };
        self.queue(move |sender| {
            sender
                .queue_send(
                    OutgoingSms {
                        conversation_id,
                        recipients: vec![recipient.into()],
                        body: "carrier".into(),
                    },
                    route,
                )
                .unwrap()
        })
    }

    fn group(&mut self, recipients: &[&str]) -> CommandId {
        let conversation_id = self.conversation;
        let recipients: Vec<String> = recipients.iter().map(|value| (*value).into()).collect();
        let route = GatewayRoute::Carrier {
            gateway_device_id: self.gateway_config.device_id,
            subscription_id: SIM.into(),
        };
        self.queue(move |sender| {
            sender
                .queue_mms(
                    OutgoingMms {
                        conversation_id,
                        recipients,
                        body: "group".into(),
                        attachment_ids: Vec::new(),
                        subject: None,
                    },
                    route,
                )
                .unwrap()
        })
    }

    fn permit(&self, command: CommandId, now_ms: i64) -> PermitDecision {
        self.gateway.begin_send_attempt_at(command, now_ms).unwrap()
    }

    fn automatic(&self, command: CommandId, now_ms: i64) -> AutomaticPermitDecision {
        self.gateway
            .begin_automatic_send_attempt(command, None, now_ms)
            .unwrap()
    }

    fn classify(
        &self,
        address: &str,
        snapshot_json: &str,
        now_ms: i64,
    ) -> Result<RcsTargetDecision, Error> {
        self.gateway.classify_and_commit_rcs_target(
            self.conversation,
            address,
            snapshot_json,
            now_ms,
        )
    }

    fn metadata(&self, key: &str) -> Option<String> {
        encrypted_connection(&self.gateway_config)
            .query_row("SELECT v FROM metadata WHERE k=?", [key], |row| row.get(0))
            .optional()
            .unwrap()
    }

    fn proof_stored(&self) -> bool {
        self.metadata(&format!("rcs_carrier_only_proof_v1/{}", self.conversation))
            .is_some()
    }

    fn diagnostic(&self) -> Option<String> {
        self.metadata(&format!(
            "rcs_carrier_admission_diagnostic_v1/{}",
            self.conversation
        ))
    }

    fn attempts(&self, command: CommandId) -> i64 {
        encrypted_connection(&self.gateway_config)
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE command_id=?",
                [command.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn record_proof(&self, proof: &CarrierOnlyProof) {
        self.gateway
            .record_carrier_only_proof(proof, 1_000)
            .unwrap();
    }
}

fn blocked() -> PermitDecision {
    PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
}

// Guards: 78824404 (finding 1). Unknown facts never become a nonmatch, so no absence is recorded.
#[test]
fn unknown_or_unverifiable_facts_block_absence_without_a_proof() {
    let lab = Lab::new(PEER);
    lab.link();
    let mut unknown_identifier = participant(PEER, false);
    unknown_identifier["identifier_type"] = json!(99);
    let mut missing_participant_id = participant(PEER, false);
    missing_participant_id["participant_id"] = json!("");
    missing_participant_id["participant_id_present"] = json!(false);
    let mut inconsistent_number_flag = participant(PEER, false);
    inconsistent_number_flag["number_present"] = json!(false);
    let cases = [
        (
            "unknown conversation type",
            conversation(
                "google",
                99,
                1,
                "unsupported",
                vec![participant(PEER, false)],
            ),
        ),
        (
            "unknown conversation status",
            conversation(
                "google",
                2,
                99,
                "unsupported",
                vec![participant(PEER, false)],
            ),
        ),
        (
            "unknown participant identifier type",
            conversation("google", 1, 1, "unsupported", vec![unknown_identifier]),
        ),
        (
            "unknown classification",
            conversation("google", 2, 1, "unknown", vec![participant(PEER, false)]),
        ),
        (
            "unsupported missing participant identity",
            conversation("google", 1, 1, "unsupported", vec![missing_participant_id]),
        ),
        (
            "unsupported inconsistent number flag",
            conversation(
                "google",
                1,
                1,
                "unsupported",
                vec![inconsistent_number_flag],
            ),
        ),
        (
            "numberless peer",
            conversation("google", 1, 1, "unsupported", vec![participant("", false)]),
        ),
        (
            "numberless self",
            conversation(
                "google",
                2,
                1,
                "unsupported",
                vec![participant(PEER, false), participant("", true)],
            ),
        ),
        (
            "self identity forged against the route",
            conversation(
                "google",
                2,
                1,
                "eligible",
                vec![participant(OTHER, false), participant(PEER, true)],
            ),
        ),
    ];
    for (case, record) in cases {
        assert_eq!(
            lab.classify(PEER, &snapshot(vec![record], 1), 1_000)
                .unwrap(),
            RcsTargetDecision::Unknown {
                snapshot_token: Some(TOKEN.into())
            },
            "{case}"
        );
        assert!(!lab.proof_stored(), "{case} must not record a proof");
    }
}

#[test]
fn target_capable_unknown_record_mints_no_target_authority_or_absence_proof() {
    let lab = Lab::new(PEER);
    lab.link();
    let decision = lab
        .classify(
            PEER,
            &snapshot(
                vec![
                    eligible_peer("exact", PEER),
                    conversation("unknown", 99, 1, "eligible", vec![participant(PEER, false)]),
                ],
                1,
            ),
            1_000,
        )
        .unwrap();

    assert_eq!(
        decision,
        RcsTargetDecision::Unknown {
            snapshot_token: Some(TOKEN.into())
        }
    );
    assert!(!lab.proof_stored());
}

#[test]
fn public_exact_match_with_unknown_candidates_never_persists_carrier_absence() {
    let mut empty_number = participant("", false);
    empty_number["number_present"] = json!(false);
    let malformed_number = participant("not-a-phone", false);
    let mut contradictory_number = participant(PEER, false);
    contradictory_number["number_present"] = json!(false);
    let mut nontelephone = participant(PEER, false);
    nontelephone["identifier_type"] = json!(99);
    let forged_self = conversation(
        "unknown",
        2,
        1,
        "eligible",
        vec![participant(OTHER, false), participant(PEER, true)],
    );
    let mut inconsistent_group =
        conversation("unknown", 2, 1, "unknown", vec![participant(OTHER, false)]);
    inconsistent_group["group"] = json!(true);
    inconsistent_group["visible_non_self_count"] = json!(2);
    let uncertain_multipeer = conversation(
        "unknown",
        2,
        1,
        "unknown",
        vec![
            participant(OTHER, false),
            participant("+12025550125", false),
        ],
    );
    let cases = [
        (
            "national target equivalent",
            conversation(
                "unknown",
                2,
                1,
                "unknown",
                vec![participant("(202) 555-0123", false)],
            ),
        ),
        (
            "absent number",
            conversation("unknown", 2, 1, "unknown", vec![empty_number]),
        ),
        (
            "malformed number",
            conversation("unknown", 2, 1, "unknown", vec![malformed_number]),
        ),
        (
            "contradictory number presence",
            conversation("unknown", 2, 1, "unknown", vec![contradictory_number]),
        ),
        (
            "nontelephone identifier",
            conversation("unknown", 2, 1, "unknown", vec![nontelephone]),
        ),
        ("forged self", forged_self),
        ("inconsistent group count", inconsistent_group),
        ("uncertain multipeer", uncertain_multipeer),
    ];
    for (name, unknown) in cases {
        let lab = Lab::new(PEER);
        lab.book("home", "US");
        lab.link();
        assert_eq!(
            lab.classify(
                PEER,
                &snapshot(vec![eligible_peer("exact", PEER), unknown], 1),
                1_000
            )
            .unwrap(),
            RcsTargetDecision::Unknown {
                snapshot_token: Some(TOKEN.into())
            },
            "{name}"
        );
        assert!(!lab.proof_stored(), "{name}");
    }

    let lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    assert_eq!(
        lab.classify(
            PEER,
            &snapshot(
                vec![conversation(
                    "unknown",
                    99,
                    1,
                    "unsupported",
                    vec![participant(OTHER, false)]
                )],
                1
            ),
            1_000
        )
        .unwrap(),
        RcsTargetDecision::Unknown {
            snapshot_token: Some(TOKEN.into())
        }
    );
    assert!(!lab.proof_stored());
}

#[test]
fn unsupported_canonicalizer_revisions_establish_no_route_or_classifier_authority() {
    for revision in [0, 2] {
        let lab = Lab::new(PEER);
        let mut invalid = observation(PEER, 0, 1, 1_000);
        invalid.canonicalizer_revision = revision;
        assert_eq!(
            lab.gateway.verify_rcs_mapping(&invalid, 1_000).unwrap(),
            RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
        );
        assert!(
            lab.gateway
                .carrier_route_context(lab.conversation, SIM)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
            RcsTargetDecision::Unknown {
                snapshot_token: None
            }
        );
    }
}

#[test]
fn equal_tuple_canonicalizer_contradiction_revokes_retained_carrier_absence_authority() {
    for automatic in [false, true] {
        let mut lab = Lab::new(PEER);
        lab.book("home", "US");
        lab.link();
        assert!(matches!(
            lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
            RcsTargetDecision::NotFoundComplete { .. }
        ));
        assert!(lab.proof_stored());

        let mut unsupported = observation(PEER, 0, 1, 1_000);
        unsupported.canonicalizer_revision = 2;
        assert_eq!(
            lab.gateway.verify_rcs_mapping(&unsupported, 1_000).unwrap(),
            RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
        );

        let command = lab.command(PEER, SIM);
        if automatic {
            assert_eq!(
                lab.automatic(command, 1_000),
                AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
            );
        } else {
            assert_eq!(lab.permit(command, 1_000), blocked());
        }
        assert_eq!(lab.attempts(command), 0);
    }
}

#[test]
fn legacy_go_context_cannot_consume_a_retained_carrier_absence_proof() {
    for automatic in [false, true] {
        let mut lab = Lab::new(PEER);
        lab.book("home", "US");
        lab.link();
        assert!(matches!(
            lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
            RcsTargetDecision::NotFoundComplete { .. }
        ));
        assert!(lab.proof_stored());
        let mut context: Value =
            serde_json::from_str(&lab.metadata("rcs_mapping_context_v1").unwrap()).unwrap();
        for key in [
            "go_runtime_id",
            "go_run_generation",
            "go_stream_epoch",
            "go_pairing_evidence_id",
        ] {
            context.as_object_mut().unwrap().remove(key);
        }
        encrypted_connection(&lab.gateway_config)
            .execute(
                "UPDATE metadata SET v=? WHERE k='rcs_mapping_context_v1'",
                [context.to_string()],
            )
            .unwrap();
        let command = lab.command(PEER, SIM);

        if automatic {
            assert_eq!(
                lab.automatic(command, 1_000),
                AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
            );
        } else {
            assert_eq!(lab.permit(command, 1_000), blocked());
        }
        assert_eq!(lab.attempts(command), 0);
    }
}

// Guards: 4656c968 (finding 9). Unrelated unknown records never suppress exact matches.
#[test]
fn unrelated_unknown_records_do_not_hide_exact_found_or_ambiguous_matches() {
    let lab = Lab::new(PEER);
    lab.link();
    let unrelated_unknown = conversation(
        "unknown",
        99,
        1,
        "eligible",
        vec![participant(OTHER, false)],
    );
    assert_eq!(
        lab.classify(
            PEER,
            &snapshot(
                vec![eligible_peer("exact", PEER), unrelated_unknown.clone()],
                1
            ),
            1_000
        )
        .unwrap(),
        RcsTargetDecision::Found {
            google_conversation_id: "exact".into(),
            snapshot_token: TOKEN.into(),
        }
    );
    assert_eq!(
        lab.classify(
            PEER,
            &snapshot(
                vec![
                    eligible_peer("first", PEER),
                    eligible_peer("second", PEER),
                    unrelated_unknown,
                ],
                1
            ),
            1_000
        )
        .unwrap(),
        RcsTargetDecision::Ambiguous {
            snapshot_token: TOKEN.into()
        }
    );
    assert!(!lab.proof_stored());
}

// Guards: 78824404 (finding 1, positive control). Fully classified unrelated records never block
// a complete absence, and the proof is bound to the validated region and canonicalizer.
#[test]
fn fully_classified_unrelated_records_still_allow_complete_absence() {
    let lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    let sms = conversation("sms", 1, 1, "unsupported", vec![participant(OTHER, false)]);
    let group = conversation(
        "group",
        2,
        1,
        "unsupported",
        vec![
            participant(OTHER, false),
            participant("+12025550125", false),
        ],
    );
    let self_chat = conversation("self", 2, 1, "unsupported", vec![participant(OWN, true)]);
    let decision = lab
        .classify(PEER, &snapshot(vec![sms, group, self_chat], 1), 1_000)
        .unwrap();
    assert_eq!(
        decision,
        RcsTargetDecision::NotFoundComplete {
            carrier_only_proof: proof(lab.conversation, PEER, "US", 1, 1),
        }
    );
    assert!(lab.proof_stored());
}

// Guards: 103622b3 (finding 2). Refused native evidence mints no route capability.
#[test]
fn refused_native_route_evidence_mints_no_carrier_route_context() {
    let mut lab = Lab::new(PEER);
    let mut invalid = observation(PEER, 1, 1, 1_000);
    invalid.carrier_binding.is_current_default_sms = false;
    assert_eq!(
        lab.gateway.verify_rcs_mapping(&invalid, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
    );
    assert!(
        lab.gateway
            .carrier_route_context(lab.conversation, SIM)
            .unwrap()
            .is_none()
    );
    lab.book("home", "US");
    assert_eq!(
        lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
        RcsTargetDecision::Unknown {
            snapshot_token: None
        }
    );
    assert!(!lab.proof_stored());
    let command = lab.command(PEER, SIM);
    assert_eq!(lab.permit(command, 1_000), blocked());
    assert_eq!(lab.attempts(command), 0);

    // Positive control: the same route, once accepted, yields the capability.
    lab.link();
    assert!(
        lab.gateway
            .carrier_route_context(lab.conversation, SIM)
            .unwrap()
            .is_some()
    );
}

// Guards: 103622b3 (finding 2). A later rejected observation changes the context; older accepted
// authority cannot revive when the older observation is replayed.
#[test]
fn later_rejected_observation_withdraws_older_route_authority() {
    let lab = Lab::new(PEER);
    lab.link();
    assert!(
        lab.gateway
            .carrier_route_context(lab.conversation, SIM)
            .unwrap()
            .is_some()
    );
    let mut invalid = observation(PEER, 1, 2, 1_001);
    invalid.carrier_binding.is_current_default_sms = false;
    assert_eq!(
        lab.gateway.verify_rcs_mapping(&invalid, 1_001).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
    );
    assert!(
        lab.gateway
            .carrier_route_context(lab.conversation, SIM)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        lab.gateway
            .verify_rcs_mapping(&observation(PEER, 0, 1, 1_002), 1_002)
            .unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::RetiredContext)
    );
    assert!(
        lab.gateway
            .carrier_route_context(lab.conversation, SIM)
            .unwrap()
            .is_none()
    );
}

// Guards: 103622b3 (finding 2). Explicit unlink validation is transactional: a failed validation
// leaves the linked context, route, and proof gate unchanged.
#[test]
fn failed_explicit_unlink_validation_writes_no_context() {
    let mut lab = Lab::new(PEER);
    lab.link();
    let command = lab.command(PEER, SIM);
    let mut retired_unlink = observation(PEER, 0, 0, 1_000);
    retired_unlink.link_state = RcsLinkState::Unlinked;
    assert!(
        lab.gateway
            .update_rcs_context(&retired_unlink, 1_000)
            .is_err()
    );
    assert!(
        lab.gateway
            .carrier_route_context(lab.conversation, SIM)
            .unwrap()
            .is_some()
    );
    assert_eq!(lab.permit(command, 1_000), blocked());
    assert_eq!(lab.attempts(command), 0);
}

// Guards: c6708d78 (finding 3). Region and canonicalizer are compared at admission; a mismatch
// blocks without invalidating, and a later context change withdraws the proof's authority.
#[test]
fn proof_region_canonicalizer_and_context_revision_gate_admission() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    let blocked_command = lab.command(PEER, SIM);

    assert!(
        lab.gateway
            .record_carrier_only_proof(&proof(lab.conversation, PEER, "CA", 1, 1), 1_000)
            .is_err()
    );
    assert!(!lab.proof_stored());
    assert_eq!(lab.permit(blocked_command, 1_000), blocked());
    assert!(
        lab.gateway
            .record_carrier_only_proof(&proof(lab.conversation, PEER, "US", 2, 1), 1_000)
            .is_err()
    );
    assert!(!lab.proof_stored());
    assert_eq!(lab.permit(blocked_command, 1_000), blocked());
    assert_eq!(lab.attempts(blocked_command), 0);

    let current_command = lab.command(PEER, SIM);
    lab.record_proof(&proof(lab.conversation, PEER, "US", 1, 1));
    assert!(matches!(
        lab.permit(current_command, 1_000),
        PermitDecision::Permit(_)
    ));

    let stale_command = lab.command(PEER, SIM);
    lab.record_proof(&proof(lab.conversation, PEER, "US", 1, 1));
    lab.gateway
        .verify_rcs_mapping(&observation(PEER, 0, 2, 1_001), 1_001)
        .unwrap();
    assert_eq!(lab.permit(stale_command, 1_001), blocked());
    assert_eq!(lab.attempts(stale_command), 0);
}

// Guards: c6708d78 (finding 8). A plus-prefixed identity needs no singleton region: the absence
// commits with the region-independent representation and that proof admits the send.
#[test]
fn plus_identity_without_a_singleton_region_commits_and_admits() {
    let mut lab = Lab::new(PEER);
    lab.link();
    let command = lab.command(PEER, SIM);
    let decision = lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap();
    let RcsTargetDecision::NotFoundComplete { carrier_only_proof } = &decision else {
        panic!("plus identity must classify as complete absence: {decision:?}");
    };
    assert_eq!(carrier_only_proof.canonical_recipient, PEER);
    assert!(carrier_only_proof.identity_region.is_empty());
    assert!(lab.proof_stored());
    assert!(matches!(
        lab.permit(command, 1_000),
        PermitDecision::Permit(_)
    ));
}

// Guards: c6708d78 (finding 8). Zero or conflicting regions never make a national target
// classifiable, while a plus target stays region-independent.
#[test]
fn national_targets_need_one_region_and_plus_targets_do_not() {
    let lab = Lab::new(PEER);
    lab.link();
    assert_eq!(
        lab.classify(NATIONAL_PEER, &snapshot(Vec::new(), 1), 1_000)
            .unwrap(),
        RcsTargetDecision::Unknown {
            snapshot_token: Some(TOKEN.into())
        }
    );
    assert!(!lab.proof_stored());
    lab.book("home", "US");
    lab.book("work", "CA");
    assert_eq!(
        lab.classify(NATIONAL_PEER, &snapshot(Vec::new(), 1), 1_000)
            .unwrap(),
        RcsTargetDecision::Unknown {
            snapshot_token: Some(TOKEN.into())
        }
    );
    assert!(!lab.proof_stored());
    assert!(matches!(
        lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
        RcsTargetDecision::NotFoundComplete { .. }
    ));
}

// Guards: c6708d78 (finding 4). Two spellings of one national number under the singleton region
// classify and admit with the same canonical proof.
#[test]
fn national_spelling_admits_through_the_same_singleton_region_proof() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    let command = lab.command(NATIONAL_PEER, SIM);
    let decision = lab
        .classify(NATIONAL_PEER, &snapshot(Vec::new(), 1), 1_000)
        .unwrap();
    assert_eq!(
        decision,
        RcsTargetDecision::NotFoundComplete {
            carrier_only_proof: proof(lab.conversation, PEER, "US", 1, 1),
        }
    );
    assert!(matches!(
        lab.permit(command, 1_000),
        PermitDecision::Permit(_)
    ));
}

// Guards: c6708d78 (finding 4). The short-code exception needs exactly one recipient and one
// singleton US/CA region; anything else is refused without a diagnostic or attempt.
#[test]
fn short_code_exception_requires_a_singleton_us_or_ca_region() {
    type ShortCodeCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let cases: [ShortCodeCase<'_>; 9] = [
        ("US singleton", &[("home", "US")], "22345"),
        ("CA singleton", &[("home", "CA")], "22345"),
        ("absent region", &[], "22345"),
        (
            "US and CA regions",
            &[("home", "US"), ("work", "CA")],
            "22345",
        ),
        ("GB region", &[("home", "GB")], "22345"),
        ("plus form", &[("home", "US")], PEER),
        ("ten-digit regular number", &[("home", "US")], "2025550123"),
        ("four-digit code", &[("home", "US")], "1234"),
        ("leading digit one", &[("home", "US")], "123456"),
    ];
    for (case, books, recipient) in cases {
        let mut lab = Lab::new(PEER);
        for (book_id, region) in books {
            lab.book(book_id, region);
        }
        lab.link();
        let command = lab.command(recipient, SIM);
        let decision = lab.permit(command, 1_000);
        let admitted = case == "US singleton" || case == "CA singleton";
        if admitted {
            assert!(matches!(decision, PermitDecision::Permit(_)), "{case}");
            assert_eq!(lab.diagnostic().as_deref(), Some(SHORT_CODE_SMS), "{case}");
            assert_eq!(lab.attempts(command), 1, "{case}");
        } else {
            assert_eq!(decision, blocked(), "{case}");
            assert_eq!(lab.diagnostic(), None, "{case}");
            assert_eq!(lab.attempts(command), 0, "{case}");
        }
        assert!(!lab.proof_stored(), "{case}");
    }
}

// Guards: c6708d78 (findings 4, 6). Both carrier entry points admit the same short code with the
// same fixed diagnostic and then refuse replays through the recorded attempt.
#[test]
fn short_code_exception_is_identical_on_both_carrier_entry_points() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    let legacy = lab.command("22345", SIM);
    let automatic = lab.command("22346", SIM);

    assert!(matches!(
        lab.permit(legacy, 1_000),
        PermitDecision::Permit(_)
    ));
    assert_eq!(lab.diagnostic().as_deref(), Some(SHORT_CODE_SMS));
    assert_eq!(
        lab.permit(legacy, 1_000),
        PermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
    );

    let AutomaticPermitDecision::Carrier(received) = lab.automatic(automatic, 1_000) else {
        panic!("automatic admission must carry the short code");
    };
    assert_eq!(received.subscription_id, SIM);
    assert_eq!(lab.attempts(automatic), 1);
    assert_eq!(
        lab.automatic(automatic, 1_000),
        AutomaticPermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
    );
}

// Guards: c6708d78 (finding 5). The sender is excluded, so own plus one peer is not a group, and
// two spellings of one peer are one recipient.
#[test]
fn group_exception_counts_distinct_non_self_recipients_only() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    let own_and_peer = lab.group(&[OWN, PEER]);
    assert_eq!(lab.permit(own_and_peer, 1_000), blocked());
    let spelled_own = lab.group(&[NATIONAL_OWN, PEER]);
    assert_eq!(lab.permit(spelled_own, 1_000), blocked());
    let duplicate_peer = lab.group(&[PEER, NATIONAL_PEER, OWN]);
    assert_eq!(lab.permit(duplicate_peer, 1_000), blocked());
    assert_eq!(lab.diagnostic(), None);
    for command in [own_and_peer, spelled_own, duplicate_peer] {
        assert_eq!(lab.attempts(command), 0);
    }

    let two_peers = lab.group(&[PEER, OTHER]);
    assert!(matches!(
        lab.permit(two_peers, 1_000),
        PermitDecision::Permit(_)
    ));
    assert_eq!(lab.diagnostic().as_deref(), Some(GROUP_MMS));
    assert_eq!(lab.attempts(two_peers), 1);
}

// Guards: c6708d78 (finding 5, both entry points). Own plus two peers is still a group exception
// on the automatic path, with the same diagnostic.
#[test]
fn group_exception_is_identical_on_the_automatic_entry_point() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    let group = lab.group(&[OWN, PEER, OTHER]);
    assert!(matches!(
        lab.automatic(group, 1_000),
        AutomaticPermitDecision::Carrier(_)
    ));
    assert_eq!(lab.diagnostic().as_deref(), Some(GROUP_MMS));
    assert_eq!(lab.attempts(group), 1);
}

// Guards: c6708d78 (finding 5, fail-closed). Without a region, national spellings cannot be
// proven distinct, so no group exception is granted.
#[test]
fn group_exception_refuses_unprovable_national_duplicates_without_a_region() {
    let mut lab = Lab::new(PEER);
    lab.link();
    let command = lab.group(&[NATIONAL_PEER, PEER]);
    assert_eq!(lab.permit(command, 1_000), blocked());
    assert_eq!(lab.diagnostic(), None);
    assert_eq!(lab.attempts(command), 0);
}

// Guards: c6708d78 (findings 4, 5). A live mapping for the conversation blocks before any
// exception; the mapping and its gate are not changed by the refusal.
#[test]
fn live_mapping_blocks_short_code_and_group_exceptions() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    assert!(matches!(
        lab.gateway
            .verify_rcs_mapping(&observation(PEER, 1, 1, 1_000), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let short_code = lab.command("22345", SIM);
    let group = lab.group(&[PEER, OTHER]);
    assert_eq!(lab.permit(short_code, 1_000), blocked());
    assert_eq!(
        lab.automatic(group, 1_000),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(lab.diagnostic(), None);
    assert_eq!(lab.attempts(short_code) + lab.attempts(group), 0);
    assert!(lab.gateway.rcs_mapping(lab.conversation).unwrap().is_some());
}

// Guards: pre-existing M2-B invariant (SIM fence). A proof for one SIM never admits a send on
// another SIM, on either carrier entry point.
#[test]
fn proof_for_one_sim_never_admits_a_send_on_another_sim() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    let command = lab.command(PEER, OTHER_SIM);
    lab.record_proof(&proof(lab.conversation, PEER, "US", 1, 1));
    assert_eq!(lab.permit(command, 1_000), blocked());
    assert_eq!(
        lab.automatic(command, 1_000),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(lab.attempts(command), 0);
}

// Guards: pre-existing M2-B invariant (expiry and clock rollback). Admission is bounded to the
// proof window and rejects a proof from the future without any stale permit.
#[test]
fn proof_expiry_and_clock_rollback_never_admit_a_stale_proof() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    lab.record_proof(&proof(lab.conversation, PEER, "US", 1, 1));

    let boundary = lab.command(PEER, SIM);
    assert!(matches!(
        lab.permit(boundary, 31_000),
        PermitDecision::Permit(_)
    ));
    let expired = lab.command(PEER, SIM);
    assert_eq!(lab.permit(expired, 31_001), blocked());
    assert_eq!(lab.attempts(expired), 0);
    assert!(lab.proof_stored());

    let rollback = lab.command(PEER, SIM);
    assert_eq!(lab.permit(rollback, 999), blocked());
    assert_eq!(lab.attempts(rollback), 0);
    assert!(!lab.proof_stored());
}

// Guards: 78824404 (route freshness at classification). Accepted native route evidence is current
// only inside the freshness window, so an old route cannot classify a later snapshot.
#[test]
fn accepted_route_evidence_must_be_current_when_classifying() {
    let lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    assert_eq!(
        lab.classify(PEER, &snapshot(Vec::new(), 1), 31_001)
            .unwrap(),
        RcsTargetDecision::Unknown {
            snapshot_token: None
        }
    );
    assert!(!lab.proof_stored());
    assert!(
        lab.gateway
            .carrier_route_context(lab.conversation, SIM)
            .unwrap()
            .is_some(),
        "the accessor reports presence; classification enforces freshness"
    );
    assert!(matches!(
        lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
        RcsTargetDecision::NotFoundComplete { .. }
    ));
}

// Guards: 4c64ef51 / c6708d78 (absence withdraws mapping atomically). A complete exact absence
// withdraws the live mapping and then admits the same target through the proof.
#[test]
fn complete_absence_withdraws_a_live_mapping_and_admits_sms() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    assert!(matches!(
        lab.gateway
            .verify_rcs_mapping(&observation(PEER, 1, 1, 1_000), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let command = lab.command(PEER, SIM);
    assert!(matches!(
        lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
        RcsTargetDecision::NotFoundComplete { .. }
    ));
    assert!(lab.gateway.rcs_mapping(lab.conversation).unwrap().is_none());
    assert!(matches!(
        lab.permit(command, 1_000),
        PermitDecision::Permit(_)
    ));
}

// Guards: c6708d78 (finding 8). The plus-target withdrawal matches the mapping by canonical
// participant alone, so a region-independent absence still withdraws the mapping.
#[test]
fn plus_absence_withdraws_a_live_mapping_without_a_singleton_region() {
    let lab = Lab::new(PEER);
    lab.link();
    assert!(matches!(
        lab.gateway
            .verify_rcs_mapping(&observation(PEER, 1, 1, 1_000), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert!(matches!(
        lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
        RcsTargetDecision::NotFoundComplete { .. }
    ));
    assert!(lab.gateway.rcs_mapping(lab.conversation).unwrap().is_none());
}

// Guards: 4c64ef51 (atomic absence). A remaining live mapping for the conversation fails the
// absence commit and leaves the mapping untouched.
#[test]
fn remaining_live_mapping_fails_absence_commit_atomically() {
    let lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    assert!(matches!(
        lab.gateway
            .verify_rcs_mapping(&observation(PEER, 1, 1, 1_000), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    lab.gateway
        .capture_incoming(IncomingSms {
            conversation_id: Some(lab.conversation),
            sender_address: OTHER.into(),
            body: String::new(),
            provider_message_id: Some("provider-other".into()),
            imported: true,
        })
        .unwrap();
    assert!(
        lab.classify(OTHER, &snapshot(Vec::new(), 1), 1_000)
            .is_err()
    );
    assert!(lab.gateway.rcs_mapping(lab.conversation).unwrap().is_some());
    assert!(!lab.proof_stored());
}

// Guards: 78824404 (Go revision is not core context revision). Go's indexRevision is validated
// inside its own token, and core's context revision is validated independently.
#[test]
fn go_index_revision_and_core_context_revision_are_validated_independently() {
    let lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    assert!(matches!(
        lab.classify(PEER, &snapshot(Vec::new(), 9), 1_000).unwrap(),
        RcsTargetDecision::NotFoundComplete { .. }
    ));

    let mut stale_token: Value = serde_json::from_str(&snapshot(Vec::new(), 8)).unwrap();
    stale_token["snapshot_token"] = json!("1:1:9");
    let mismatched_token = stale_token.to_string();
    let other_lab = Lab::new(PEER);
    other_lab.link();
    assert_eq!(
        other_lab.classify(PEER, &mismatched_token, 1_000).unwrap(),
        RcsTargetDecision::Unknown {
            snapshot_token: Some("1:1:9".into())
        }
    );
    assert!(!other_lab.proof_stored());
}

// Guards c6708d78 / F3: changes after proof creation, not just inconsistent proof inputs.
#[test]
fn region_and_canonicalizer_changes_after_proof_block_both_entry_points() {
    for change_region in [true, false] {
        let mut lab = Lab::new(PEER);
        lab.book("home", "US");
        lab.link();
        let command = lab.command(NATIONAL_PEER, SIM);
        lab.record_proof(&proof(lab.conversation, PEER, "US", 1, 1));
        if change_region {
            lab.book_generation("home", "CA", "2");
        } else {
            let mut changed = observation(PEER, 0, 2, 1_001);
            changed.canonicalizer_revision = 2;
            assert_eq!(
                lab.gateway.verify_rcs_mapping(&changed, 1_001).unwrap(),
                RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
            );
        }
        let stored = lab.metadata(&format!("rcs_carrier_only_proof_v1/{}", lab.conversation));
        assert_eq!(lab.permit(command, 1_001), blocked());
        assert_eq!(
            lab.automatic(command, 1_001),
            AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
        assert_eq!(lab.attempts(command), 0);
        assert_eq!(lab.diagnostic(), None);
        assert_eq!(
            lab.metadata(&format!("rcs_carrier_only_proof_v1/{}", lab.conversation)),
            stored
        );
    }
}

// Guards 103622b3 + c6708d78 / F2, F4–6: exceptions do not bypass native SIM/time fences.
#[test]
fn exceptions_require_current_native_route_on_the_original_sim() {
    for (group, wrong_sim, now_ms) in [
        (false, true, 1_000),
        (true, true, 1_000),
        (false, false, 31_001),
        (true, false, 31_001),
        (false, false, 999),
        (true, false, 999),
    ] {
        let mut lab = Lab::new(PEER);
        lab.book("home", "US");
        lab.link();
        let command = if group {
            lab.group(&[PEER, OTHER])
        } else {
            lab.command("22345", SIM)
        };
        if wrong_sim {
            let mut changed = observation(PEER, 0, 2, 1_000);
            changed.selected_sim_id = OTHER_SIM.into();
            changed.carrier_binding.carrier_route_id = OTHER_SIM.into();
            changed.carrier_binding.android_subscription_id = 8;
            assert_eq!(
                lab.gateway.verify_rcs_mapping(&changed, 1_000).unwrap(),
                RcsMappingOutcome::Refused(RcsMappingRefusal::GoogleConversationAmbiguous)
            );
        }
        assert_eq!(
            lab.permit(command, now_ms),
            blocked(),
            "group={group}, wrong_sim={wrong_sim}, time={now_ms}"
        );
        assert_eq!(
            lab.automatic(command, now_ms),
            AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
        assert_eq!(lab.attempts(command), 0);
        assert_eq!(lab.diagnostic(), None);
        assert!(!lab.proof_stored());
    }
}

// Guards c6708d78 / F4: one regular number is not a short code; multiple SMS recipients are
// refused by the public queue API before any command can reach admission.
#[test]
fn regular_number_overlap_and_multiple_sms_recipients_never_get_short_code_exception() {
    let mut lab = Lab::new(PEER);
    lab.book("home", "US");
    lab.link();
    assert_eq!(canonical_address("2025550123", Some("US")), PEER);
    let command = lab.command("2025550123", SIM);
    assert_eq!(lab.permit(command, 1_000), blocked());
    let before = lab.sender.pending_outbox().unwrap();
    assert_eq!(
        lab.sender.queue_send(
            OutgoingSms {
                conversation_id: lab.conversation,
                recipients: vec!["22345".into(), "22346".into()],
                body: "carrier".into()
            },
            GatewayRoute::Carrier {
                gateway_device_id: lab.gateway_config.device_id,
                subscription_id: SIM.into()
            }
        ),
        Err(Error::InvalidRequest("SMS requires exactly one recipient"))
    );
    assert_eq!(lab.sender.pending_outbox().unwrap(), before);
    assert_eq!(lab.attempts(command), 0);
    assert_eq!(lab.diagnostic(), None);
}

// Guards 103622b3 / F2: attestation metadata alone is not accepted native settings evidence.
#[test]
fn refused_native_settings_cannot_mint_carrier_context() {
    for missing_settings in [true, false] {
        let lab = Lab::new(PEER);
        let mut invalid = observation(PEER, 1, 1, 1_000);
        if missing_settings {
            invalid.native_evidence.current_settings_sims.clear();
        } else {
            invalid.outgoing.own_identity_raw = OTHER.into();
        }
        assert_eq!(
            lab.gateway.verify_rcs_mapping(&invalid, 1_000).unwrap(),
            RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
        );
        assert!(
            lab.gateway
                .carrier_route_context(lab.conversation, SIM)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap(),
            RcsTargetDecision::Unknown {
                snapshot_token: None
            }
        );
        assert!(!lab.proof_stored());
    }
}

// Guards c6708d78 / F6: failed attempt persistence rolls the fixed exception diagnostic back too.
#[test]
fn exception_diagnostics_and_attempts_commit_atomically_on_both_entry_points() {
    for automatic in [false, true] {
        let mut lab = Lab::new(PEER);
        lab.book("home", "US");
        lab.link();
        let command = lab.command("22345", SIM);
        encrypted_connection(&lab.gateway_config).execute_batch(
            "CREATE TRIGGER refuse_carrier_attempt BEFORE INSERT ON attempts BEGIN SELECT RAISE(ABORT, 'fixture refusal'); END;"
        ).unwrap();
        let result = if automatic {
            lab.gateway
                .begin_automatic_send_attempt(command, None, 1_000)
                .map(|_| ())
        } else {
            lab.gateway
                .begin_send_attempt_at(command, 1_000)
                .map(|_| ())
        };
        assert_eq!(result, Err(Error::Database));
        assert_eq!(lab.diagnostic(), None);
        assert_eq!(lab.attempts(command), 0);
        assert!(!lab.proof_stored());
        encrypted_connection(&lab.gateway_config)
            .execute_batch("DROP TRIGGER refuse_carrier_attempt;")
            .unwrap();
        assert!(matches!(
            lab.permit(command, 1_000),
            PermitDecision::Permit(_)
        ));
        assert_eq!(lab.diagnostic().as_deref(), Some(SHORT_CODE_SMS));
        assert_eq!(lab.attempts(command), 1);
    }
}

// Final review F1 / original F3: losing singleton evidence revokes a region-bound proof, on
// both public admission paths. The refusal must leave the proof and all effect state unchanged.
#[test]
fn region_bound_proof_refuses_ambiguous_or_absent_current_region() {
    for ambiguous in [true, false] {
        let mut lab = Lab::new(PEER);
        lab.book("home", "US");
        lab.link();
        let command = lab.command(NATIONAL_PEER, SIM);
        assert_eq!(
            lab.classify(NATIONAL_PEER, &snapshot(Vec::new(), 1), 1_000)
                .unwrap(),
            RcsTargetDecision::NotFoundComplete {
                carrier_only_proof: proof(lab.conversation, PEER, "US", 1, 1)
            }
        );
        let proof_key = format!("rcs_carrier_only_proof_v1/{}", lab.conversation);
        let stored = lab.metadata(&proof_key);
        if ambiguous {
            lab.book("work", "CA");
        } else {
            lab.gateway
                .forget_contact_book(&json!({"book_id":"home"}).to_string())
                .unwrap();
        }
        assert_eq!(lab.permit(command, 1_001), blocked());
        assert_eq!(
            lab.automatic(command, 1_001),
            AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
        assert_eq!(lab.attempts(command), 0);
        assert_eq!(lab.diagnostic(), None);
        assert_eq!(lab.metadata(&proof_key), stored);
    }
}

// Final review F1 positive control: explicit region-independent plus proofs do not depend on
// singleton evidence and continue to authorize both entry points when that evidence is lost.
#[test]
fn region_independent_plus_proof_survives_ambiguous_or_absent_region() {
    for ambiguous in [true, false] {
        let mut lab = Lab::new(PEER);
        lab.link();
        let decision = lab.classify(PEER, &snapshot(Vec::new(), 1), 1_000).unwrap();
        assert_eq!(
            decision,
            RcsTargetDecision::NotFoundComplete {
                carrier_only_proof: proof(lab.conversation, PEER, "", 1, 1)
            }
        );
        if ambiguous {
            lab.book("home", "US");
            lab.book("work", "CA");
        }
        let legacy = lab.command(PEER, SIM);
        let automatic = lab.command(PEER, SIM);
        assert!(matches!(
            lab.permit(legacy, 1_001),
            PermitDecision::Permit(_)
        ));
        assert!(matches!(
            lab.automatic(automatic, 1_001),
            AutomaticPermitDecision::Carrier(_)
        ));
        assert_eq!(lab.attempts(legacy), 1);
        assert_eq!(lab.attempts(automatic), 1);
        assert_eq!(lab.diagnostic(), None);
    }
}

// Final review F2 / original F1: a confirmed own-identity target is unsupported, not a complete
// absence. Unrelated confirmed self records still do not suppress absence for a different peer.
#[test]
fn exact_confirmed_self_target_never_authorizes_carrier_absence() {
    let self_snapshot = snapshot(
        vec![conversation(
            "self",
            2,
            1,
            "unsupported",
            vec![participant(OWN, true)],
        )],
        1,
    );
    let mut lab = Lab::new(OWN);
    lab.book("home", "US");
    lab.link();
    let command = lab.command(OWN, SIM);
    assert_eq!(
        lab.classify(OWN, &self_snapshot, 1_000).unwrap(),
        RcsTargetDecision::Unsupported {
            snapshot_token: TOKEN.into()
        }
    );
    assert!(!lab.proof_stored());
    assert_eq!(lab.permit(command, 1_000), blocked());
    assert_eq!(
        lab.automatic(command, 1_000),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(lab.attempts(command), 0);
    assert_eq!(lab.diagnostic(), None);

    let mut peer = Lab::new(PEER);
    peer.book("home", "US");
    peer.link();
    assert_eq!(
        peer.classify(PEER, &self_snapshot, 1_000).unwrap(),
        RcsTargetDecision::NotFoundComplete {
            carrier_only_proof: proof(peer.conversation, PEER, "US", 1, 1)
        }
    );
    let legacy = peer.command(PEER, SIM);
    let automatic = peer.command(PEER, SIM);
    assert!(matches!(
        peer.permit(legacy, 1_000),
        PermitDecision::Permit(_)
    ));
    assert!(matches!(
        peer.automatic(automatic, 1_000),
        AutomaticPermitDecision::Carrier(_)
    ));
}
