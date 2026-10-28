//! Regression coverage for history-import ordering and upload prioritization.
mod common;

use common::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn mms_input(id: &str, imported: bool) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "gateway".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-1".into()),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: Vec::new(),
        subject: None,
        body: id.into(),
        imported,
        observed_at_ms: 1,
        transaction_id: None,
    }
}

fn attachment(client: &Client, directory: &TempDir, name: &str) -> AttachmentId {
    let path = directory.path().join(name);
    fs::write(&path, name).unwrap();
    client
        .prepare_attachment(&path, "image/png", name)
        .unwrap()
        .attachment_id
}

fn complete_mms_with_media(
    client: &Client,
    id: &str,
    imported: bool,
    media: AttachmentId,
    thumbnail: AttachmentId,
) {
    let acquisition = client
        .begin_mms_acquisition(mms_input(id, imported))
        .unwrap();
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "part", media)
        .unwrap();
    client
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "part",
            None,
            None,
            None,
            Some(thumbnail),
        )
        .unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
}

fn history_plan(plans: &[MmsBackfillScanPlan]) -> &MmsBackfillScanPlan {
    plans
        .iter()
        .find(|plan| plan.phase == MmsBackfillPhase::History)
        .unwrap()
}

fn imported_sms_plan_is_paused(client: &Client) -> bool {
    client
        .plan_sms_scans("gateway", "sim-1", "1000")
        .unwrap()
        .into_iter()
        .find(|plan| plan.imported)
        .unwrap()
        .paused
}

fn capture_events(client: &Client, count: usize) {
    for id in 0..count {
        client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: ADDRESS.into(),
                body: format!("event-{id}"),
                provider_message_id: Some(format!("event-{id}")),
                imported: true,
            })
            .unwrap();
    }
}

#[test]
fn pending_media_history_events_do_not_backpressure_sms_import() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "gateway", &vault), &vault);
    for id in 0..500 {
        let media = attachment(&client, &directory, &format!("held-media-{id}.png"));
        let thumbnail = attachment(&client, &directory, &format!("held-thumbnail-{id}.png"));
        complete_mms_with_media(&client, &id.to_string(), true, media, thumbnail);
    }

    client.open_history_import_gate("contacts_settled").unwrap();
    assert!(!imported_sms_plan_is_paused(&client));
}

#[test]
fn eligible_history_events_backpressure_sms_import_at_the_threshold() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "gateway", &vault), &vault);
    capture_events(&client, 500);

    client.open_history_import_gate("contacts_settled").unwrap();
    assert!(imported_sms_plan_is_paused(&client));
}

#[test]
fn version_36_reinstalls_history_backpressure_indexes_on_reopen() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "history-backpressure-index-reinstall", &vault);
    let client = unlocked(&config, &vault);
    drop(client);

    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    let version: i64 = connection
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 38);
    let outbox_index: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='index' AND name='outbox_pending_events_history'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        outbox_index.contains("(envelope_id,compaction,purpose,state)"),
        "{outbox_index}"
    );
    connection
        .execute_batch(
            "DROP INDEX outbox_pending_events_history; DROP INDEX contact_photo_registrations_unacknowledged;",
        )
        .unwrap();
    drop(connection);

    drop(open(&config));
    drop(open(&config));

    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    let index_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('outbox_pending_events_history','contact_photo_registrations_unacknowledged')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 2);
    let outbox_index: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='index' AND name='outbox_pending_events_history'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        outbox_index.contains("(envelope_id,compaction,purpose,state)"),
        "{outbox_index}"
    );
}

#[test]
fn conflict_contact_registration_and_unsupported_compaction_rows_do_not_backpressure_history() {
    for (name, exclusion) in [
        (
            "conflict",
            "INSERT INTO outbox_conflicts(envelope_id,cursor) SELECT envelope_id,seq FROM outbox",
        ),
        (
            "contact-registration",
            "INSERT INTO contact_photo_registrations(envelope_id,attachment_id,remote_object_id,producer_sequence,acknowledged) SELECT envelope_id,'attachment-' || seq,'remote-' || seq,seq,0 FROM outbox",
        ),
        (
            "unsupported-compaction",
            "UPDATE outbox SET compaction='{}'",
        ),
    ] {
        let directory = TempDir::new().unwrap();
        let vault = Vault::new();
        let config = config(&directory, name, &vault);
        let client = unlocked(&config, &vault);
        capture_events(&client, 500);
        drop(client);

        let connection = rusqlite::Connection::open(&config.database_path).unwrap();
        connection
            .execute_batch(
                "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
            )
            .unwrap();
        connection.execute_batch(exclusion).unwrap();
        drop(connection);

        let client = open(&config);
        client
            .unlock(&vault.profile, &vault.header, PASSPHRASE)
            .unwrap();
        if name == "unsupported-compaction" {
            client.set_server_compaction_supported(false).unwrap();
        }
        client.open_history_import_gate("contacts_settled").unwrap();
        assert!(client.pending_outbox().unwrap().is_empty(), "{name}");
        assert!(!imported_sms_plan_is_paused(&client), "{name}");
    }
}

#[test]
fn version_31_upgrade_preserves_partial_floors_completed_scopes_and_descending_progress() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v31-history", &vault);
    let client = open(&config);
    client.plan_sms_scans("partial", "sim", "100").unwrap();
    client.plan_sms_scans("complete", "sim", "100").unwrap();
    client
        .ack_sms_scan_exhausted("complete", "sim", true, "100")
        .unwrap();
    client
        .plan_mms_backfill_scans("partial", "sim", "100")
        .unwrap();
    client
        .plan_mms_backfill_scans("complete", "sim", "100")
        .unwrap();
    client
        .finish_mms_backfill_phase("complete", "sim", MmsBackfillPhase::History, "100", true)
        .unwrap();
    drop(client);

    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .execute_batch(
            "
            UPDATE sms_scan_checkpoints SET provider_message_id='20'
            WHERE source_generation='partial' AND subscription_id='sim' AND imported=1;
            UPDATE mms_scan_checkpoints SET provider_message_id='30'
            WHERE source_generation='partial' AND subscription_id='sim' AND imported=1;
            ALTER TABLE sms_scan_checkpoints DROP COLUMN descending_cursor;
            ALTER TABLE sms_scan_checkpoints DROP COLUMN history_floor;
            ALTER TABLE sms_scan_checkpoints DROP COLUMN descending;
            ALTER TABLE mms_backfill_scopes DROP COLUMN history_cursor;
            ALTER TABLE mms_backfill_scopes DROP COLUMN history_floor;
            ALTER TABLE mms_backfill_scopes DROP COLUMN history_descending;
            DROP TABLE history_import_gate;
            UPDATE schema_meta SET version=31;
            ",
        )
        .unwrap();
    drop(connection);

    let upgraded = open(&config);
    let sms = upgraded.plan_sms_scans("partial", "sim", "100").unwrap();
    let sms = sms.iter().find(|plan| plan.imported).unwrap();
    assert!(sms.descending);
    assert_eq!(sms.history_floor.as_deref(), Some("20"));
    assert_eq!(sms.checkpoint, None);
    upgraded
        .set_sms_scan_checkpoint("partial", "sim", true, "90")
        .unwrap();
    upgraded
        .set_sms_scan_checkpoint("partial", "sim", true, "95")
        .unwrap();

    let mms = upgraded
        .plan_mms_backfill_scans("partial", "sim", "100")
        .unwrap();
    let mms = history_plan(&mms);
    assert!(mms.descending);
    assert_eq!(mms.history_floor.as_deref(), Some("30"));
    assert_eq!(mms.cursor, None);
    upgraded
        .advance_mms_backfill_page("partial", "sim", MmsBackfillPhase::History, "90")
        .unwrap();
    upgraded
        .advance_mms_backfill_page("partial", "sim", MmsBackfillPhase::History, "95")
        .unwrap();

    let completed_sms = upgraded.plan_sms_scans("complete", "sim", "100").unwrap();
    let completed_sms = completed_sms.iter().find(|plan| plan.imported).unwrap();
    assert!(completed_sms.history_complete);
    assert!(!completed_sms.descending);
    let completed_mms = upgraded
        .plan_mms_backfill_scans("complete", "sim", "100")
        .unwrap();
    let completed_mms = history_plan(&completed_mms);
    assert!(completed_mms.history_complete);
    assert!(!completed_mms.descending);
    drop(upgraded);

    let reopened = open(&config);
    let sms = reopened.plan_sms_scans("partial", "sim", "100").unwrap();
    assert_eq!(
        sms.iter()
            .find(|plan| plan.imported)
            .unwrap()
            .checkpoint
            .as_deref(),
        Some("90")
    );
    let mms = reopened
        .plan_mms_backfill_scans("partial", "sim", "100")
        .unwrap();
    assert_eq!(history_plan(&mms).cursor.as_deref(), Some("90"));
}

#[test]
fn pending_uploads_prioritize_live_media_then_contact_photos_then_imported_media() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "gateway", &vault);
    let client = unlocked(&config, &vault);

    let live_media = attachment(&client, &directory, "live-media.png");
    let live_thumbnail = attachment(&client, &directory, "live-thumbnail.png");
    complete_mms_with_media(&client, "live", false, live_media, live_thumbnail);

    let imported_media = attachment(&client, &directory, "imported-media.png");
    let imported_thumbnail = attachment(&client, &directory, "imported-thumbnail.png");
    complete_mms_with_media(
        &client,
        "imported-1",
        true,
        imported_media,
        imported_thumbnail,
    );

    let photo_path = directory.path().join("contact.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]))
        .save(&photo_path)
        .unwrap();
    let contact_photo = client
        .prepare_contact_photo(&photo_path)
        .unwrap()
        .attachment_id;
    client
        .capture_platform_contacts_json(
            &json!({
                "schema_version": 1,
                "book": {
                    "id": "book",
                    "owner_device_id": config.device_id.to_string(),
                    "generation": "1",
                    "state": "active"
                },
                "contacts": [{
                    "source_key": "photo",
                    "fields": {
                        "display_name": "Photo",
                        "name": {"given": "Photo"},
                        "phones": [], "emails": [], "addresses": [],
                        "photo": {"attachment_id": contact_photo.to_string()}
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();

    let uploads = client.pending_uploads().unwrap();
    let ids: Vec<_> = uploads
        .into_iter()
        .map(|upload| upload.attachment_id)
        .collect();
    assert_eq!(ids.len(), 5);
    for attachment in [
        live_media,
        live_thumbnail,
        contact_photo,
        imported_media,
        imported_thumbnail,
    ] {
        assert_eq!(ids.iter().filter(|id| **id == attachment).count(), 1);
    }
    assert!(ids[..2].contains(&live_media));
    assert!(ids[..2].contains(&live_thumbnail));
    assert_eq!(ids[2], contact_photo);
    assert!(ids[3..].contains(&imported_media));
    assert!(ids[3..].contains(&imported_thumbnail));
}

#[test]
fn reverse_inserted_history_keeps_the_newest_message_as_the_conversation_match_anchor() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "gateway", &vault), &vault);

    let capture = |id: &str, body: &str, sent_at_ms: i64| {
        let acquisition = client
            .begin_sms_acquisition(SmsAcquisitionInput {
                source: SmsSource {
                    source_generation: "gateway".into(),
                    subscription_id: "sim-1".into(),
                    provider_message_id: id.into(),
                    provider_thread_id: Some("thread-1".into()),
                    broadcast_id: None,
                },
                direction: Direction::Incoming,
                sender_address: Some(ADDRESS.into()),
                recipients: Vec::new(),
                body: body.into(),
                imported: true,
                sent_at_ms: Some(sent_at_ms),
                date_sent_ms: Some(sent_at_ms),
                creator: None,
                source_row_fingerprint: id.into(),
            })
            .unwrap();
        client
            .complete_sms_acquisition(&acquisition.acquisition_id)
            .unwrap()
    };

    let newest = capture("30", "newest", 300);
    capture("20", "middle", 200);
    capture("10", "oldest", 100);

    let live = client
        .begin_sms_acquisition(SmsAcquisitionInput {
            source: SmsSource {
                source_generation: "gateway".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: "31".into(),
                provider_thread_id: None,
                broadcast_id: None,
            },
            direction: Direction::Incoming,
            sender_address: Some(ADDRESS.into()),
            recipients: Vec::new(),
            body: "live".into(),
            imported: false,
            sent_at_ms: Some(400),
            date_sent_ms: Some(400),
            creator: None,
            source_row_fingerprint: "31".into(),
        })
        .unwrap();

    assert_eq!(live.conversation_id, newest.conversation_id);
}
