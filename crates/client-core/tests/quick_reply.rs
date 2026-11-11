mod common;

use common::*;

#[test]
fn quick_reply_queues_once_without_creating_a_compose_draft() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming(incoming("hello", "quick-reply-1"))
        .unwrap();
    assert_eq!(
        client
            .quick_reply_context(received.conversation_id)
            .unwrap(),
        QuickReplyContext {
            recipients: vec![ADDRESS.into()],
            transport: Transport::Sms,
            expected_member_revision: None,
        }
    );
    let command_id = CommandId::new();
    let input = QuickReplyInput {
        command_id,
        conversation_id: received.conversation_id,
        body: "reply".into(),
        route: route(&config),
        expected_transport: Transport::Sms,
        expected_member_revision: None,
    };

    let queued = client.queue_quick_reply(input.clone()).unwrap();
    assert_eq!(client.queue_quick_reply(input).unwrap(), queued);
    assert!(client.compose_drafts().unwrap().is_empty());
    assert_eq!(client.messages(received.conversation_id).unwrap().len(), 2);
}

#[test]
fn quick_reply_reconciles_after_sealing_and_acknowledgement() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-reconcile", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming(incoming("hello", "quick-reply-reconcile-1"))
        .unwrap();
    let input = QuickReplyInput {
        command_id: CommandId::new(),
        conversation_id: received.conversation_id,
        body: "reply".into(),
        route: route(&config),
        expected_transport: Transport::Sms,
        expected_member_revision: None,
    };

    let queued = client.queue_quick_reply(input.clone()).unwrap();
    // Quick replies seal synchronously; this ensures a later sealing pass cannot disturb
    // reconciliation after queue-time plaintext has been discarded.
    assert_eq!(client.seal_pending_batch(10).unwrap(), 0);
    assert_eq!(
        client.reconcile_quick_reply(&input).unwrap(),
        Some(queued.clone())
    );

    client.ack_outbox(queued.envelope_id).unwrap();
    assert_eq!(client.reconcile_quick_reply(&input).unwrap(), Some(queued));
}

#[test]
fn quick_reply_reconciles_a_confirmed_cancellation_without_requeueing() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "quick-reply-cancel-sender", &vault);
    let gateway_config = config(&directory, "quick-reply-cancel-gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let received = sender
        .capture_incoming(incoming("hello", "quick-reply-cancel-1"))
        .unwrap();
    let input = QuickReplyInput {
        command_id: CommandId::new(),
        conversation_id: received.conversation_id,
        body: "reply".into(),
        route: route(&gateway_config),
        expected_transport: Transport::Sms,
        expected_member_revision: None,
    };
    let queued = sender.queue_quick_reply(input.clone()).unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    sender.request_send_cancellation(queued.message_id).unwrap();
    server.upload(&sender);
    server.sync(&gateway);
    assert!(gateway.process_pending_send_cancellations().unwrap());
    server.upload(&gateway);
    server.sync(&sender);
    drop(sender);
    let sender = unlocked(&sender_config, &vault);

    assert_eq!(
        sender
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Confirmed
    );
    assert!(sender.message_by_id(queued.message_id).unwrap().is_none());
    let messages_before = sender.messages(received.conversation_id).unwrap();
    let outbox_before = sender.pending_outbox().unwrap();

    assert_eq!(
        sender.reconcile_quick_reply(&input).unwrap(),
        Some(queued.clone())
    );
    assert_eq!(sender.queue_quick_reply(input.clone()).unwrap(), queued);
    assert_eq!(
        sender.queue_quick_reply(QuickReplyInput {
            body: "changed".into(),
            ..input
        }),
        Err(Error::ExistingCommandConflict)
    );
    assert_eq!(
        sender.messages(received.conversation_id).unwrap(),
        messages_before
    );
    assert_eq!(sender.pending_outbox().unwrap(), outbox_before);
}

#[test]
fn quick_reply_preserves_a_populated_compose_draft() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-draft", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming(incoming("hello", "quick-reply-draft-1"))
        .unwrap();
    let draft = client
        .create_compose_draft(Some(received.conversation_id))
        .unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "unsent composer text".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: Vec::new(),
                route: Some(route(&config)),
                group_name: None,
            },
        )
        .unwrap();

    client
        .queue_quick_reply(QuickReplyInput {
            command_id: CommandId::new(),
            conversation_id: received.conversation_id,
            body: "notification reply".into(),
            route: route(&config),
            expected_transport: Transport::Sms,
            expected_member_revision: None,
        })
        .unwrap();

    assert_eq!(client.compose_draft(saved.draft_id).unwrap(), Some(saved));
}

#[test]
fn quick_reply_seeds_a_carrier_mms_group_roster_on_its_first_reply() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-carrier-group", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming_mms(IncomingMms {
            conversation_id: None,
            sender_address: "+15555550101".into(),
            body: "carrier group hello".into(),
            provider_message_id: Some("quick-reply-carrier-group-1".into()),
            imported: false,
            attachment_ids: Vec::new(),
            recipients: vec![ADDRESS.into(), "+15555550102".into()],
            subject: None,
        })
        .unwrap();
    let context = client
        .quick_reply_context(received.conversation_id)
        .unwrap();
    assert_eq!(context.transport, Transport::Mms);
    assert_eq!(context.expected_member_revision, None);

    let input = QuickReplyInput {
        command_id: CommandId::new(),
        conversation_id: received.conversation_id,
        body: "carrier group reply".into(),
        route: route(&config),
        expected_transport: Transport::Mms,
        expected_member_revision: context.expected_member_revision,
    };
    let queued = client.queue_quick_reply(input.clone()).unwrap();
    assert_eq!(client.queue_quick_reply(input.clone()).unwrap(), queued);
    assert_eq!(client.seal_pending_batch(10).unwrap(), 0);
    client.ack_outbox(queued.envelope_id).unwrap();
    assert_eq!(client.queue_quick_reply(input.clone()).unwrap(), queued);
    assert_eq!(
        client.queue_quick_reply(QuickReplyInput {
            expected_member_revision: Some(1),
            ..input
        }),
        Err(Error::ExistingCommandConflict)
    );

    assert_eq!(
        client.group_roster(received.conversation_id).unwrap(),
        [
            ADDRESS.to_owned(),
            "+15555550101".to_owned(),
            "+15555550102".to_owned()
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        client
            .message_by_id(queued.message_id)
            .unwrap()
            .unwrap()
            .payload
            .transport,
        Transport::Mms
    );
}

#[test]
fn quick_reply_rcs_command_reaches_native_gateway_admission() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "quick-reply-rcs-sender", &vault);
    let gateway_config = config(&directory, "quick-reply-rcs-gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let received = gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "peer-local".into(),
            body: "hello".into(),
            provider_message_id: Some("quick-reply-rcs-gateway-1".into()),
            imported: true,
        })
        .unwrap();
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    let observation = rcs_observation();
    assert!(matches!(
        gateway.verify_rcs_mapping(&observation, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&sender);

    let queued = sender
        .queue_quick_reply(QuickReplyInput {
            command_id: CommandId::new(),
            conversation_id: received.conversation_id,
            body: "native RCS reply".into(),
            route: GatewayRoute::RcsLink {
                gateway_device_id: gateway_config.device_id,
            },
            expected_transport: Transport::Sms,
            expected_member_revision: None,
        })
        .unwrap();
    server.upload(&sender);
    server.sync(&gateway);

    assert!(
        gateway
            .pending_native_rcs_commands()
            .unwrap()
            .iter()
            .any(|command| command.command_id == queued.command_id)
    );
    let preparation = gateway
        .prepare_native_rcs_send(queued.command_id, 1_000)
        .unwrap();
    let committed = gateway
        .begin_native_rcs_send(preparation, &observation, 1_000)
        .unwrap();
    assert_eq!(committed.message_body, "native RCS reply");
}

fn rcs_observation() -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "conversation-local".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: "peer-local".into(),
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
        selected_sim_id: "android-subscription-7".into(),
        selected_sim_own_identity: "+12025550100".into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: "android-subscription-7".into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: "+12025550100".into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: 1_000,
            route_context_revision: 1,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: NativeRcsOutgoing {
            outgoing_id: "google-outgoing".into(),
            sim_participant_id: "google-outgoing".into(),
            own_identity_raw: "+12025550100".into(),
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
            settings_observed_at_ms: 1_000,
            settings_stream_epoch: 1,
            settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
            current_settings_sims: vec![NativeRcsOutgoing {
                outgoing_id: "google-outgoing".into(),
                sim_participant_id: "google-outgoing".into(),
                own_identity_raw: "+12025550100".into(),
                sim_payload: NativeRcsSimPayload {
                    two: 2,
                    sim_number: 7,
                },
                rcs_chats_enabled: true,
            }],
            settings_validated_at_ms: 1_000,
            conversation_observed_at_ms: 1_000,
        },
        identity_region: "US".into(),
        observed_at_ms: 1_000,
        context_revision: 1,
        link_generation: 1,
        session_generation: 1,
        canonicalizer_revision: 1,
        mapping_revision: 1,
        link_state: RcsLinkState::Linked,
    }
}

#[test]
fn quick_reply_rejects_conflicting_command_input() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-conflict", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming(incoming("hello", "quick-reply-2"))
        .unwrap();
    let command_id = CommandId::new();
    let input = QuickReplyInput {
        command_id,
        conversation_id: received.conversation_id,
        body: "first".into(),
        route: route(&config),
        expected_transport: Transport::Sms,
        expected_member_revision: None,
    };

    client.queue_quick_reply(input.clone()).unwrap();
    assert_eq!(
        client.queue_quick_reply(QuickReplyInput {
            body: "second".into(),
            ..input
        }),
        Err(Error::ExistingCommandConflict)
    );
}

#[test]
fn quick_reply_rejects_a_reused_command_id_for_a_different_sim() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-route-conflict", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming(incoming("hello", "quick-reply-3"))
        .unwrap();
    let command_id = CommandId::new();
    let input = QuickReplyInput {
        command_id,
        conversation_id: received.conversation_id,
        body: "reply".into(),
        route: route(&config),
        expected_transport: Transport::Sms,
        expected_member_revision: None,
    };

    client.queue_quick_reply(input.clone()).unwrap();
    let GatewayRoute::Carrier {
        gateway_device_id, ..
    } = input.route
    else {
        unreachable!();
    };
    assert_eq!(
        client.queue_quick_reply(QuickReplyInput {
            route: GatewayRoute::Carrier {
                gateway_device_id,
                subscription_id: "sim-2".into(),
            },
            ..input
        }),
        Err(Error::ExistingCommandConflict)
    );
}

#[test]
fn quick_reply_preserves_sms_command_encoding_for_an_rcs_route() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-rcs", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming(incoming("hello", "quick-reply-rcs-1"))
        .unwrap();

    let queued = client.queue_quick_reply(QuickReplyInput {
        command_id: CommandId::new(),
        conversation_id: received.conversation_id,
        body: "rcs reply".into(),
        route: GatewayRoute::RcsLink {
            gateway_device_id: config.device_id,
        },
        expected_transport: Transport::Sms,
        expected_member_revision: None,
    });
    let queued = queued.unwrap();
    let message = client.message_by_id(queued.message_id).unwrap().unwrap();
    assert_eq!(message.payload.transport, Transport::Sms);
}

#[test]
fn existing_mms_queue_rejects_an_rcs_link_route() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-mms-route-compat", &vault);
    let client = unlocked(&config, &vault);

    assert_eq!(
        client.queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "carrier MMS only".into(),
                attachment_ids: Vec::new(),
                subject: None,
            },
            GatewayRoute::RcsLink {
                gateway_device_id: config.device_id,
            },
        ),
        Err(Error::InvalidRequest("carrier route required"))
    );
}

#[test]
fn quick_reply_permits_rcs_group_text_with_roster_authority() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-rcs-group-text", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming_mms(IncomingMms {
            conversation_id: None,
            sender_address: "+15555550101".into(),
            body: "group hello".into(),
            provider_message_id: Some("quick-reply-rcs-group-text-1".into()),
            imported: false,
            attachment_ids: Vec::new(),
            recipients: vec![ADDRESS.into(), "+15555550102".into()],
            subject: None,
        })
        .unwrap();
    client
        .queue_quick_reply(QuickReplyInput {
            command_id: CommandId::new(),
            conversation_id: received.conversation_id,
            body: "seed carrier roster".into(),
            route: route(&config),
            expected_transport: Transport::Mms,
            expected_member_revision: None,
        })
        .unwrap();
    let context = client
        .quick_reply_context(received.conversation_id)
        .unwrap();
    assert_eq!(context.expected_member_revision, Some(1));

    let queued = client
        .queue_quick_reply(QuickReplyInput {
            command_id: CommandId::new(),
            conversation_id: received.conversation_id,
            body: "RCS group text".into(),
            route: GatewayRoute::RcsLink {
                gateway_device_id: config.device_id,
            },
            expected_transport: Transport::Mms,
            expected_member_revision: context.expected_member_revision,
        })
        .unwrap();
    assert_eq!(
        client
            .message_by_id(queued.message_id)
            .unwrap()
            .unwrap()
            .payload
            .transport,
        Transport::Mms
    );
}

#[test]
fn quick_reply_rejects_blank_body_before_queueing() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "quick-reply-blank", &vault);
    let client = unlocked(&config, &vault);
    let received = client
        .capture_incoming(incoming("hello", "quick-reply-blank-1"))
        .unwrap();

    assert!(matches!(
        client.queue_quick_reply(QuickReplyInput {
            command_id: CommandId::new(),
            conversation_id: received.conversation_id,
            body: " \n ".into(),
            route: route(&config),
            expected_transport: Transport::Sms,
            expected_member_revision: None,
        }),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(client.messages(received.conversation_id).unwrap().len(), 1);
}
