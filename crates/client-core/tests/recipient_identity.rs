//! Recipient identity uses the sole contact-book region and exact MMS participants.
use crate::common::*;
use serde_json::json;
use tempfile::TempDir;

fn mms_input(
    id: &str,
    direction: Direction,
    sender_address: Option<&str>,
    recipients: &[&str],
) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "telephony-v1".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: None,
        },
        direction,
        sender_address: sender_address.map(str::to_owned),
        recipients: recipients.iter().map(|address| (*address).into()).collect(),
        subject: None,
        body: "hello".into(),
        imported: false,
        observed_at_ms: 1,
        transaction_id: None,
    }
}

fn capture(client: &Client, input: MmsAcquisitionInput) -> ConversationId {
    let acquisition = client.begin_mms_acquisition(input).unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap()
        .conversation_id
}

fn draft_update(recipients: &[&str]) -> ComposeDraftUpdate {
    ComposeDraftUpdate {
        text: "reply".into(),
        recipients: recipients
            .iter()
            .map(|recipient| (*recipient).into())
            .collect(),
        attachment_ids: Vec::new(),
        route: None,
    }
}

#[test]
fn latest_incoming_group_reuses_exact_participants_with_known_region() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "phone", &vault);
    let client = unlocked(&cfg, &vault);
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {
                    "id": "uk",
                    "owner_device_id": cfg.device_id.to_string(),
                    "generation": "1",
                    "state": "active",
                    "region": "GB"
                },
                "contacts": []
            })
            .to_string(),
        )
        .unwrap();
    client
        .set_mms_own_address("sim-1", "+442071234567")
        .unwrap();

    let conversation = capture(
        &client,
        mms_input(
            "outgoing",
            Direction::Outgoing,
            None,
            &["+442079460018", "+442079460019"],
        ),
    );
    let first_incoming = capture(
        &client,
        mms_input(
            "incoming-1",
            Direction::Incoming,
            Some("020 7946 0018"),
            &["020 7123 4567", "020 7946 0019"],
        ),
    );
    let latest_incoming = capture(
        &client,
        mms_input(
            "incoming-2",
            Direction::Incoming,
            Some("+442079460018"),
            &["+442071234567", "+442079460019"],
        ),
    );

    assert_eq!(first_incoming, conversation);
    assert_eq!(latest_incoming, conversation);

    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            draft_update(&["020 7946 0018", "020 7946 0019"]),
        )
        .unwrap();
    assert_eq!(saved.conversation_id, conversation);
}
