use crate::{
    session::rcs_conversation_flags,
    tests::{fixture_at, open, Fixture, PHRASE},
};
use peppy_client_core::{
    ConversationId, Error, IncomingSms, RcsConversationMetadata, RcsConversationTransport,
};
use std::str::FromStr;

fn unlock(session: &crate::session::Session, fixture: &Fixture) {
    session
        .client
        .unlock(&fixture.profile, &fixture.header, PHRASE)
        .unwrap();
}

fn metadata(rcs_encryption: Option<bool>, rcs_history_gap: bool) -> RcsConversationMetadata {
    RcsConversationMetadata {
        conversation_id: ConversationId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap(),
        transport: RcsConversationTransport::Rcs,
        rcs_encryption,
        rcs_history_gap,
        logical_revision: 1,
        writer_device_id: "device".into(),
    }
}

#[test]
fn present_rcs_metadata_projects_both_conversation_facts() {
    assert_eq!(
        rcs_conversation_flags(Ok(Some(metadata(Some(true), true)))),
        (Some(true), true)
    );
    assert_eq!(
        rcs_conversation_flags(Ok(Some(metadata(Some(false), false)))),
        (Some(false), false)
    );
}

#[test]
fn absent_rcs_metadata_makes_no_claim() {
    assert_eq!(rcs_conversation_flags(Ok(None)), (None, false));
}

#[test]
fn failed_rcs_metadata_read_omits_the_claim_instead_of_failing_the_snapshot() {
    assert_eq!(rcs_conversation_flags(Err(Error::Database)), (None, false));
}

#[test]
fn message_window_keeps_sms_rows_on_the_rcs_aware_projection() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = open(&fixture, &fixture.binding, &[]);
    unlock(&session, &fixture);
    let captured = session
        .client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+15555550199".into(),
            body: "window".into(),
            provider_message_id: Some("rcs-window-sms".into()),
            imported: false,
        })
        .unwrap();
    let window = session
        .message_window(peppy_desktop_api::MessageWindowRequestView::Around {
            conversation_id: captured.conversation_id.to_string(),
            message_id: captured.message_id.to_string(),
        })
        .unwrap();
    assert_eq!(window.messages.len(), 1);
    let message = &window.messages[0];
    assert_eq!(message.id, captured.message_id.to_string());
    assert_eq!(message.transport.as_deref(), Some("sms"));
    assert!(message.status.is_none());
}
