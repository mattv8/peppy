use crate::common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};

fn draft_aad(vault_id: VaultId, draft_id: DraftId, revision_id: uuid::Uuid) -> Vec<u8> {
    [
        b"peppy.draft.v1\0".as_slice(),
        vault_id.0.as_bytes(),
        draft_id.0.as_bytes(),
        revision_id.as_bytes(),
    ]
    .concat()
}

fn sealed_head(vault: &Vault, draft_id: DraftId, record: DraftRecordV1) -> RemoteDraftChange {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Draft).unwrap();
    let plaintext = serde_json::to_vec(&record).unwrap();
    let sealed = encrypt(
        &key,
        &draft_aad(vault.id, draft_id, record.revision_id),
        &plaintext,
    )
    .unwrap();
    let mut sealed_record = 1_u32.to_be_bytes().to_vec();
    sealed_record.extend_from_slice(&sealed.nonce);
    sealed_record.extend_from_slice(&sealed.ciphertext);
    RemoteDraftChange::Head {
        draft_id,
        branch_id: uuid::Uuid::new_v4(),
        revision_id: record.revision_id,
        parent_revision_id: None,
        sealed_record,
        attachment_ids: record
            .attachments
            .iter()
            .map(|attachment| attachment.attachment_id)
            .collect(),
        expires_at_ms: 1_000,
    }
}

fn descriptor(config: &ClientConfig, attachment_id: AttachmentId) -> DraftRecordAttachmentV1 {
    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection.execute_batch("PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; PRAGMA cipher_compatibility = 4; PRAGMA cipher_memory_security = ON;").unwrap();
    connection.query_row(
        "SELECT media_type,display_name,plaintext_bytes,ciphertext_bytes,ciphertext_sha256,stream_version,file_key FROM attachments WHERE attachment_id=?",
        [attachment_id.to_string()],
        |row| Ok(DraftRecordAttachmentV1 {
            attachment_id,
            media_type: row.get(0)?,
            display_name: row.get(1)?,
            plaintext_bytes: u64::try_from(row.get::<_, i64>(2)?).unwrap(),
            ciphertext_bytes: u64::try_from(row.get::<_, i64>(3)?).unwrap(),
            ciphertext_sha256: row.get(4)?,
            stream_version: u32::try_from(row.get::<_, i64>(5)?).unwrap(),
            file_key: row.get(6)?,
        }),
    ).unwrap()
}

#[test]
fn authored_save_creates_a_stable_pending_push() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "draft".into(),
                recipients: vec!["+14155552671".into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let first = client.pending_draft_pushes().unwrap();
    let second = client.pending_draft_pushes().unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].draft_id, saved.draft_id);
    assert!(first[0].authored);
}

#[test]
fn attempted_initial_push_requires_reconciliation_but_never_attempted_draft_stays_local_sendable() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let attempted = save_draft(&client, "attempted", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);

    assert!(
        client
            .mark_draft_push_attempt(push.draft_id, push.revision_id)
            .unwrap()
    );
    let attempted = client
        .save_compose_draft(
            attempted.draft_id,
            attempted.revision,
            ComposeDraftUpdate {
                text: "attempted r2".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    assert!(
        client
            .draft_sync_status(attempted.draft_id)
            .unwrap()
            .send_requires_connection
    );
    assert_eq!(
        client.begin_draft_send(attempted.draft_id, attempted.revision, None),
        Err(Error::DraftNotSynced)
    );
    drop(client);

    let reopened = unlocked(&client_config, &vault);
    assert_eq!(
        reopened.begin_draft_send(attempted.draft_id, attempted.revision, None),
        Err(Error::DraftNotSynced)
    );

    let fresh = unlocked(&config(&dir, "fresh", &vault), &vault);
    let never_attempted = save_draft(&fresh, "offline", vec![ADDRESS.into()]);
    assert!(
        fresh
            .begin_draft_send(never_attempted.draft_id, never_attempted.revision, None)
            .unwrap()
            .remote
            .is_none()
    );
}

#[test]
fn local_queue_rechecks_a_push_attempt_after_the_initial_send_gate() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let draft = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "offline".into(),
            recipients: vec!["+14155552672".into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);

    assert!(
        client
            .begin_draft_send(draft.draft_id, draft.revision, None)
            .unwrap()
            .remote
            .is_none()
    );
    assert!(
        client
            .mark_draft_push_attempt(push.draft_id, push.revision_id)
            .unwrap()
    );

    assert_eq!(
        client.send_compose_draft(draft.draft_id, draft.revision),
        Err(Error::DraftNotSynced)
    );
}

#[test]
fn local_queue_retirement_makes_a_later_push_mark_stale() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let draft = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "offline".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);

    client
        .send_compose_draft(draft.draft_id, draft.revision)
        .unwrap();

    assert!(
        !client
            .mark_draft_push_attempt(push.draft_id, push.revision_id)
            .unwrap()
    );
}

#[test]
fn marking_a_replaced_pending_revision_is_stale() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let first = save_draft(&client, "first", vec![ADDRESS.into()]);
    let first_push = client.pending_draft_pushes().unwrap().remove(0);
    let second = client
        .save_compose_draft(
            first.draft_id,
            first.revision,
            ComposeDraftUpdate {
                text: "second".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let second_push = client.pending_draft_pushes().unwrap().remove(0);

    assert!(
        !client
            .mark_draft_push_attempt(first.draft_id, first_push.revision_id)
            .unwrap()
    );
    assert!(
        client
            .mark_draft_push_attempt(second.draft_id, second_push.revision_id)
            .unwrap()
    );
}

#[test]
fn schema_47_pending_revision_is_backfilled_as_an_ambiguous_attempt_on_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let draft = save_draft(&client, "legacy", vec![ADDRESS.into()]);
    drop(client);

    let connection = rusqlite::Connection::open(&client_config.database_path).unwrap();
    connection.execute_batch("PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; PRAGMA cipher_compatibility = 4; PRAGMA cipher_memory_security = ON; DROP TABLE draft_push_attempts; UPDATE schema_meta SET version=47;").unwrap();
    drop(connection);

    let reopened = unlocked(&client_config, &vault);
    assert_eq!(
        reopened.begin_draft_send(draft.draft_id, draft.revision, None),
        Err(Error::DraftNotSynced)
    );
}

const FEATURE_DRAFT_TABLES: [&str; 14] = [
    "draft_sync",
    "draft_sync_conflicts",
    "draft_sync_terminals",
    "draft_send_intents",
    "draft_sync_revisions",
    "draft_media_references",
    "draft_sync_resurrections",
    "draft_sync_resurrection_attachments",
    "draft_attachment_failures",
    "draft_sync_meta",
    "draft_sync_merge_bases",
    "draft_sync_merge_fallbacks",
    "draft_push_attempts",
    "draft_sync_quarantines",
];

fn encrypted_connection(config: &ClientConfig) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection.execute_batch("PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; PRAGMA cipher_compatibility = 4; PRAGMA cipher_memory_security = ON;").unwrap();
    connection
}

fn table_exists(connection: &rusqlite::Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
            [table],
            |row| row.get(0),
        )
        .unwrap()
}

fn column_exists(connection: &rusqlite::Connection, table: &str, column: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
            rusqlite::params![table, column],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn fresh_schema_49_contains_the_production_and_draft_layouts() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "fresh-49", &vault);
    drop(unlocked(&client_config, &vault));

    let connection = encrypted_connection(&client_config);
    assert_eq!(
        connection
            .query_row("SELECT version FROM schema_meta", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        49
    );
    assert!(column_exists(
        &connection,
        "notification_preferences",
        "show_source"
    ));
    assert!(column_exists(&connection, "draft_sync", "branch_closed"));
    assert!(
        FEATURE_DRAFT_TABLES
            .iter()
            .all(|table| table_exists(&connection, table))
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_push_attempts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn production_schema_46_keeps_legacy_drafts_and_adds_the_feature_schema() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "production-46", &vault);
    let client = unlocked(&client_config, &vault);
    let image = dir.path().join("production-46-image.png");
    std::fs::write(&image, b"production-46-image").unwrap();
    let attachment = client
        .prepare_attachment(&image, "image/png", "production-46.png")
        .unwrap();
    let compose = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "existing image".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: None,
        })
        .unwrap();
    drop(client);

    let connection = encrypted_connection(&client_config);
    connection.execute_batch(
        "INSERT INTO drafts(conversation_id,content,revision) VALUES('legacy','keep',7);
         INSERT INTO notification_preferences(id,message_banners,mirrored_banners,preview,mute_with_heads,logical_revision,writer_device_id,producer_device_id,source_sequence,show_source)
         VALUES(1,1,1,'full',0,1,'writer','producer',1,1);",
    ).unwrap();
    for table in FEATURE_DRAFT_TABLES {
        connection
            .execute_batch(&format!("DROP TABLE {table}"))
            .unwrap();
    }
    connection
        .execute("UPDATE schema_meta SET version=46", [])
        .unwrap();
    assert!(table_exists(&connection, "drafts"));
    assert!(table_exists(&connection, "compose_drafts"));
    assert!(table_exists(&connection, "compose_draft_redirects"));
    assert!(
        FEATURE_DRAFT_TABLES
            .iter()
            .all(|table| !table_exists(&connection, table))
    );
    drop(connection);

    let reopened = unlocked(&client_config, &vault);

    let connection = encrypted_connection(&client_config);
    assert_eq!(
        connection
            .query_row("SELECT version FROM schema_meta", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        49
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT content FROM drafts WHERE conversation_id='legacy'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "keep"
    );
    assert!(
        connection
            .query_row(
                "SELECT show_source FROM notification_preferences WHERE id=1",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
    assert!(
        FEATURE_DRAFT_TABLES
            .iter()
            .all(|table| table_exists(&connection, table))
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_push_attempts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(connection);

    assert_eq!(
        reopened.attachment_info(attachment.attachment_id).unwrap(),
        attachment
    );
    assert_eq!(
        reopened
            .compose_draft(compose.draft_id)
            .unwrap()
            .unwrap()
            .attachment_ids,
        vec![attachment.attachment_id]
    );
    assert!(
        !reopened
            .discard_unreferenced_attachment(attachment.attachment_id)
            .unwrap()
    );
    let edited = reopened
        .save_compose_draft(
            compose.draft_id,
            compose.revision,
            ComposeDraftUpdate {
                text: "existing image edited".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    let connection = encrypted_connection(&client_config);
    assert!(connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=? AND pending_sealed_record IS NOT NULL)",
            [edited.draft_id.to_string()],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
}

#[test]
fn feature_schema_46_backfills_pending_drafts_and_repairs_notification_preferences() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "feature-46", &vault);
    let client = unlocked(&client_config, &vault);
    let draft = save_draft(&client, "legacy", vec![ADDRESS.into()]);
    drop(client);

    let connection = encrypted_connection(&client_config);
    connection
        .execute_batch(
            "ALTER TABLE notification_preferences DROP COLUMN show_source;
             ALTER TABLE draft_sync DROP COLUMN branch_closed;
             DROP TABLE draft_sync_merge_bases;
             DROP TABLE draft_sync_merge_fallbacks;
             DROP TABLE draft_push_attempts;
             DROP TABLE draft_sync_quarantines;
             UPDATE schema_meta SET version=46;",
        )
        .unwrap();
    drop(connection);

    let reopened = unlocked(&client_config, &vault);
    assert_eq!(
        reopened.begin_draft_send(draft.draft_id, draft.revision, None),
        Err(Error::DraftNotSynced)
    );
    drop(reopened);

    let connection = encrypted_connection(&client_config);
    assert_eq!(
        connection
            .query_row("SELECT version FROM schema_meta", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        49
    );
    assert!(column_exists(
        &connection,
        "notification_preferences",
        "show_source"
    ));
    assert!(column_exists(&connection, "draft_sync", "branch_closed"));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM draft_push_attempts WHERE draft_id=?",
                [draft.draft_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn feature_schema_48_preserves_attempt_evidence_without_backfilling_never_attempted_drafts() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "feature-48", &vault);
    let client = unlocked(&client_config, &vault);
    let attempted = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "attempted".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: None,
        })
        .unwrap();
    let never_attempted = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "offline".into(),
            recipients: vec!["+14155552672".into()],
            attachment_ids: vec![],
            route: None,
        })
        .unwrap();
    let attempted_push = client
        .pending_draft_pushes()
        .unwrap()
        .into_iter()
        .find(|push| push.draft_id == attempted.draft_id)
        .unwrap();
    assert!(
        client
            .mark_draft_push_attempt(attempted_push.draft_id, attempted_push.revision_id)
            .unwrap()
    );
    drop(client);

    let connection = encrypted_connection(&client_config);
    connection.execute_batch(
        "INSERT INTO notification_preferences(id,message_banners,mirrored_banners,preview,mute_with_heads,logical_revision,writer_device_id,producer_device_id,source_sequence,show_source)
         VALUES(1,1,1,'hidden',0,1,'writer','producer',1,1);
         ALTER TABLE notification_preferences DROP COLUMN show_source;
         UPDATE schema_meta SET version=48;",
    ).unwrap();
    drop(connection);

    let reopened = unlocked(&client_config, &vault);
    assert_eq!(
        reopened.begin_draft_send(attempted.draft_id, attempted.revision, None),
        Err(Error::DraftNotSynced)
    );
    assert!(
        reopened
            .begin_draft_send(never_attempted.draft_id, never_attempted.revision, None)
            .unwrap()
            .remote
            .is_none()
    );
    drop(reopened);

    let connection = encrypted_connection(&client_config);
    assert_eq!(
        connection
            .query_row("SELECT version FROM schema_meta", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        49
    );
    assert!(
        !connection
            .query_row(
                "SELECT show_source FROM notification_preferences WHERE id=1",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM draft_push_attempts WHERE draft_id=?",
                [attempted.draft_id.to_string()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

fn assert_empty_attempt_table_preserves_never_attempted_draft(version: i64) {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, &format!("empty-attempts-{version}"), &vault);
    let client = unlocked(&client_config, &vault);
    let draft = save_draft(&client, "offline", vec![ADDRESS.into()]);
    drop(client);

    let connection = encrypted_connection(&client_config);
    let before: (String, Vec<u8>) = connection
        .query_row(
            "SELECT pending_revision_id,pending_sealed_record FROM draft_sync WHERE draft_id=?",
            [draft.draft_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let attempt_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM draft_push_attempts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(attempt_count, 0);
    connection
        .execute("UPDATE schema_meta SET version=?", [version])
        .unwrap();
    drop(connection);

    let reopened = unlocked(&client_config, &vault);
    assert!(
        !reopened
            .draft_sync_status(draft.draft_id)
            .unwrap()
            .send_requires_connection
    );
    assert!(
        reopened
            .begin_draft_send(draft.draft_id, draft.revision, None)
            .unwrap()
            .remote
            .is_none()
    );
    drop(reopened);

    let connection = encrypted_connection(&client_config);
    let after: (String, Vec<u8>) = connection
        .query_row(
            "SELECT pending_revision_id,pending_sealed_record FROM draft_sync WHERE draft_id=?",
            [draft.draft_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let attempt_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM draft_push_attempts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(attempt_count, 0);
    assert_eq!(after, before);
}

#[test]
fn schema_45_with_empty_attempt_table_does_not_backfill_never_attempted_draft() {
    assert_empty_attempt_table_preserves_never_attempted_draft(45);
}

#[test]
fn schema_48_with_empty_attempt_table_does_not_backfill_never_attempted_draft() {
    assert_empty_attempt_table_preserves_never_attempted_draft(48);
}

fn save_draft(client: &Client, text: &str, recipients: Vec<String>) -> ComposeDraft {
    let draft = client.recipientless_compose_draft().unwrap();
    client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: text.into(),
                recipients,
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap()
}

fn remote_head(push: &DraftPush, branch_id: uuid::Uuid) -> RemoteDraftChange {
    RemoteDraftChange::Head {
        draft_id: push.draft_id,
        branch_id,
        revision_id: push.revision_id,
        parent_revision_id: push.base_revision_id,
        sealed_record: push.sealed_record.clone(),
        attachment_ids: push.attachment_ids.clone(),
        expires_at_ms: 1_000,
    }
}

fn accept_push(client: &Client, push: &DraftPush, branch_id: uuid::Uuid) {
    client
        .record_draft_push(
            push.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id,
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();
}

#[test]
fn undecryptable_head_is_quarantined_and_advances_the_page_cursor() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let saved = save_draft(&sender, "remote", vec![ADDRESS.into()]);
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    let mut change = remote_head(&push, uuid::Uuid::new_v4());
    let RemoteDraftChange::Head { sealed_record, .. } = &mut change else {
        unreachable!();
    };
    sealed_record[4] ^= 1;

    receiver.apply_draft_changes(vec![change], "1").unwrap();
    assert_eq!(receiver.draft_sync_cursor().unwrap(), "1");
    assert!(receiver.compose_draft(saved.draft_id).unwrap().is_none());
}

#[test]
fn malformed_head_is_quarantined_without_blocking_an_unrelated_valid_head() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let _malformed_draft = save_draft(&sender, "malformed", vec![ADDRESS.into()]);
    let malformed_push = sender.pending_draft_pushes().unwrap().remove(0);
    let mut malformed = remote_head(&malformed_push, uuid::Uuid::new_v4());
    let RemoteDraftChange::Head { sealed_record, .. } = &mut malformed else {
        unreachable!();
    };
    sealed_record[4] ^= 1;
    let valid_sender = unlocked(&config(&dir, "valid-sender", &vault), &vault);
    let valid_draft = save_draft(&valid_sender, "valid", vec!["+14155552672".into()]);
    let valid_push = valid_sender.pending_draft_pushes().unwrap().remove(0);

    receiver
        .apply_draft_changes(
            vec![malformed, remote_head(&valid_push, uuid::Uuid::new_v4())],
            "2",
        )
        .unwrap();

    assert_eq!(receiver.draft_sync_cursor().unwrap(), "2");
    assert!(
        receiver
            .compose_draft(valid_draft.draft_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn invalid_second_descriptor_rolls_back_before_quarantine_and_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver_config = config(&dir, "receiver", &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let source = dir.path().join("a.png");
    std::fs::write(&source, b"a").unwrap();
    let a = sender
        .prepare_attachment(&source, "image/png", "a.png")
        .unwrap();
    sender
        .mark_attachment_uploaded(a.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let draft_id = DraftId::new();
    let revision_id = uuid::Uuid::new_v4();
    let mut invalid_b = descriptor(&sender_config, a.attachment_id);
    invalid_b.attachment_id = AttachmentId::new();
    invalid_b.file_key = vec![0];
    let malformed = sealed_head(
        &vault,
        draft_id,
        DraftRecordV1 {
            version: 1,
            draft_id,
            revision_id,
            text: "bad".into(),
            recipients: vec![ADDRESS.into()],
            route: None,
            attachments: vec![
                descriptor(&sender_config, a.attachment_id),
                invalid_b.clone(),
            ],
            authored_at_ms: 0,
        },
    );
    let valid = save_draft(&sender, "valid", vec!["+15555552672".into()]);
    let valid_push = sender.pending_draft_pushes().unwrap().pop().unwrap();

    receiver
        .apply_draft_changes(
            vec![malformed, remote_head(&valid_push, uuid::Uuid::new_v4())],
            "2",
        )
        .unwrap();
    assert_eq!(receiver.draft_sync_cursor().unwrap(), "2");
    assert!(receiver.attachment_info(a.attachment_id).is_err());
    assert!(receiver.attachment_info(invalid_b.attachment_id).is_err());
    let connection = rusqlite::Connection::open(&receiver_config.database_path).unwrap();
    connection.execute_batch("PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; PRAGMA cipher_compatibility = 4; PRAGMA cipher_memory_security = ON;").unwrap();
    let mutations: i64 = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM attachments WHERE attachment_id IN (?,?)) + (SELECT COUNT(*) FROM draft_media_references WHERE attachment_id IN (?,?))",
        [a.attachment_id.to_string(), invalid_b.attachment_id.to_string(), a.attachment_id.to_string(), invalid_b.attachment_id.to_string()],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(mutations, 0);
    drop(connection);
    drop(receiver);
    let reopened = unlocked(&receiver_config, &vault);
    assert!(reopened.attachment_info(a.attachment_id).is_err());
    assert!(reopened.compose_draft(valid.draft_id).unwrap().is_some());
}

#[test]
fn out_of_range_descriptor_is_quarantined_without_blocking_a_valid_head() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let source = dir.path().join("a.png");
    std::fs::write(&source, b"a").unwrap();
    let a = sender
        .prepare_attachment(&source, "image/png", "a.png")
        .unwrap();
    let mut invalid = descriptor(&sender_config, a.attachment_id);
    invalid.plaintext_bytes = u64::MAX;
    invalid.ciphertext_bytes = u64::MAX;
    let draft_id = DraftId::new();
    let malformed = sealed_head(
        &vault,
        draft_id,
        DraftRecordV1 {
            version: 1,
            draft_id,
            revision_id: uuid::Uuid::new_v4(),
            text: "bad".into(),
            recipients: vec![ADDRESS.into()],
            route: None,
            attachments: vec![invalid],
            authored_at_ms: 0,
        },
    );
    let valid = save_draft(&sender, "valid", vec!["+15555552672".into()]);
    let valid_push = sender.pending_draft_pushes().unwrap().pop().unwrap();

    receiver
        .apply_draft_changes(
            vec![malformed, remote_head(&valid_push, uuid::Uuid::new_v4())],
            "2",
        )
        .unwrap();
    assert_eq!(receiver.draft_sync_cursor().unwrap(), "2");
    assert!(receiver.attachment_info(a.attachment_id).is_err());
    assert!(receiver.compose_draft(valid.draft_id).unwrap().is_some());
}

#[test]
fn retired_draft_rotates_before_missing_descriptor_and_ignores_a_late_old_head() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let sent = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "sent".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let old_head = client.pending_draft_pushes().unwrap().remove(0);
    client
        .send_compose_draft(sent.draft_id, sent.revision)
        .unwrap();
    let cleared = client.compose_draft(sent.draft_id).unwrap().unwrap();
    let missing = AttachmentId::new();
    let fresh = client
        .save_compose_draft(
            cleared.draft_id,
            cleared.revision,
            ComposeDraftUpdate {
                text: "needs media".into(),
                recipients: cleared.recipients.clone(),
                attachment_ids: vec![missing],
                route: cleared.route.clone(),
            },
        )
        .unwrap();
    assert_ne!(fresh.draft_id, sent.draft_id);

    let source = dir.path().join("complete.png");
    std::fs::write(&source, b"complete").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/png", "complete.png")
        .unwrap();
    let complete = client
        .save_compose_draft(
            fresh.draft_id,
            fresh.revision,
            ComposeDraftUpdate {
                text: fresh.text,
                recipients: fresh.recipients,
                attachment_ids: vec![attachment.attachment_id],
                route: fresh.route,
            },
        )
        .unwrap();
    drop(client);
    let reopened = unlocked(&client_config, &vault);
    reopened
        .apply_draft_changes(vec![remote_head(&old_head, uuid::Uuid::new_v4())], "1")
        .unwrap();
    assert_eq!(
        reopened
            .compose_draft(complete.draft_id)
            .unwrap()
            .unwrap()
            .attachment_ids,
        vec![attachment.attachment_id]
    );
    assert!(reopened.compose_draft(sent.draft_id).unwrap().is_none());
}

#[test]
fn accepted_echo_preserves_a_later_pending_save() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let saved = save_draft(&sender, "r1", vec![ADDRESS.into()]);
    let r1 = sender.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    receiver
        .apply_draft_changes(vec![remote_head(&r1, branch_id)], "1")
        .unwrap();
    let adopted = receiver.compose_draft(saved.draft_id).unwrap().unwrap();
    receiver
        .save_compose_draft(
            adopted.draft_id,
            adopted.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: adopted.recipients,
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let pending = receiver.pending_draft_pushes().unwrap().remove(0);

    receiver
        .apply_draft_changes(vec![remote_head(&r1, branch_id)], "2")
        .unwrap();

    assert_eq!(receiver.pending_draft_pushes().unwrap(), vec![pending]);
    assert_eq!(
        receiver
            .compose_draft(saved.draft_id)
            .unwrap()
            .unwrap()
            .text,
        "r2"
    );
}

#[test]
fn clean_device_fast_forwards_to_a_newer_head() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let saved = save_draft(&sender, "first", vec![ADDRESS.into()]);
    let branch_id = uuid::Uuid::new_v4();
    let first = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&first, branch_id)], "1")
        .unwrap();
    let saved = sender
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "second".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let second = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&second, branch_id)], "2")
        .unwrap();

    assert_eq!(
        receiver
            .compose_draft(saved.draft_id)
            .unwrap()
            .unwrap()
            .text,
        "second"
    );
}

#[test]
fn disjoint_caption_edits_are_merged_and_queued() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let initial = save_draft(&sender, "hello world", vec![ADDRESS.into()]);
    let branch_id = uuid::Uuid::new_v4();
    let first = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&first, branch_id)], "1")
        .unwrap();
    let local = receiver.compose_draft(initial.draft_id).unwrap().unwrap();
    receiver
        .save_compose_draft(
            local.draft_id,
            local.revision,
            ComposeDraftUpdate {
                text: "hi world".into(),
                recipients: local.recipients.clone(),
                attachment_ids: local.attachment_ids.clone(),
                route: local.route.clone(),
            },
        )
        .unwrap();
    sender
        .save_compose_draft(
            initial.draft_id,
            initial.revision,
            ComposeDraftUpdate {
                text: "hello earth".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let remote = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&remote, branch_id)], "2")
        .unwrap();

    assert_eq!(
        receiver
            .compose_draft(initial.draft_id)
            .unwrap()
            .unwrap()
            .text,
        "hi earth"
    );
    let push = receiver.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(push.base_revision_id, Some(remote.revision_id));
    assert!(push.resolves_branch_ids.is_empty());
    assert_eq!(
        receiver.draft_sync_status(initial.draft_id).unwrap().notice,
        Some(DraftNotice::UpdatedFromOtherDevice)
    );
}

#[test]
fn auto_merge_rebases_on_an_incoming_distinct_branch_and_closes_only_the_old_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let initial = save_draft(&sender, "hello world", vec![ADDRESS.into()]);
    let branch_a = uuid::Uuid::new_v4();
    let first = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&first, branch_a)], "1")
        .unwrap();
    let local = receiver.compose_draft(initial.draft_id).unwrap().unwrap();
    receiver
        .save_compose_draft(
            local.draft_id,
            local.revision,
            ComposeDraftUpdate {
                text: "hi world".into(),
                recipients: local.recipients.clone(),
                attachment_ids: local.attachment_ids.clone(),
                route: local.route.clone(),
            },
        )
        .unwrap();
    sender
        .save_compose_draft(
            initial.draft_id,
            initial.revision,
            ComposeDraftUpdate {
                text: "hello earth".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let remote = sender.pending_draft_pushes().unwrap().remove(0);
    let branch_b = uuid::Uuid::new_v4();
    receiver
        .apply_draft_changes(vec![remote_head(&remote, branch_b)], "2")
        .unwrap();

    let push = receiver.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(push.base_revision_id, Some(remote.revision_id));
    assert_eq!(push.resolves_branch_ids, vec![branch_a]);
    let sender_draft = sender.compose_draft(initial.draft_id).unwrap().unwrap();
    sender
        .save_compose_draft(
            sender_draft.draft_id,
            sender_draft.revision,
            ComposeDraftUpdate {
                text: "hello mars".into(),
                recipients: sender_draft.recipients.clone(),
                attachment_ids: sender_draft.attachment_ids.clone(),
                route: sender_draft.route.clone(),
            },
        )
        .unwrap();
    let second_remote = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&second_remote, branch_b)], "3")
        .unwrap();

    let push = receiver.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(push.base_revision_id, Some(second_remote.revision_id));
    assert_eq!(push.resolves_branch_ids, vec![branch_a]);
}

#[test]
fn auto_merge_does_not_close_a_tracked_branch_already_closed_remotely() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let saved = save_draft(&client, "hello world", vec![ADDRESS.into()]);
    let initial = client.pending_draft_pushes().unwrap().remove(0);
    let branch_a = uuid::Uuid::new_v4();
    accept_push(&client, &initial, branch_a);
    sender
        .apply_draft_changes(vec![remote_head(&initial, branch_a)], "1")
        .unwrap();
    let local = client.compose_draft(saved.draft_id).unwrap().unwrap();
    client
        .save_compose_draft(
            local.draft_id,
            local.revision,
            ComposeDraftUpdate {
                text: "hi world".into(),
                recipients: local.recipients.clone(),
                attachment_ids: local.attachment_ids.clone(),
                route: local.route.clone(),
            },
        )
        .unwrap();
    let remote = sender.compose_draft(saved.draft_id).unwrap().unwrap();
    sender
        .save_compose_draft(
            remote.draft_id,
            remote.revision,
            ComposeDraftUpdate {
                text: "hello earth".into(),
                recipients: remote.recipients.clone(),
                attachment_ids: remote.attachment_ids.clone(),
                route: remote.route.clone(),
            },
        )
        .unwrap();
    let remote_push = sender.pending_draft_pushes().unwrap().remove(0);
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::BranchClosed {
                draft_id: saved.draft_id,
                branch_id: branch_a,
            }],
            "2",
        )
        .unwrap();
    client
        .apply_draft_changes(vec![remote_head(&remote_push, uuid::Uuid::new_v4())], "3")
        .unwrap();

    assert_eq!(
        client.compose_draft(saved.draft_id).unwrap().unwrap().text,
        "hi earth"
    );
    let outgoing = client.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(outgoing.base_revision_id, Some(remote_push.revision_id));
    assert!(!outgoing.resolves_branch_ids.contains(&branch_a));
}

#[test]
fn auto_merge_keeps_an_unrelated_same_draft_conflict_reviewable() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let initial = save_draft(&sender, "hello world", vec![ADDRESS.into()]);
    let base_branch = uuid::Uuid::new_v4();
    let base = sender.pending_draft_pushes().unwrap().remove(0);
    client
        .apply_draft_changes(vec![remote_head(&base, base_branch)], "1")
        .unwrap();
    let local = client.compose_draft(initial.draft_id).unwrap().unwrap();
    client
        .save_compose_draft(
            local.draft_id,
            local.revision,
            ComposeDraftUpdate {
                text: "hi world".into(),
                recipients: local.recipients,
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let b = sender
        .save_compose_draft(
            initial.draft_id,
            initial.revision,
            ComposeDraftUpdate {
                text: "hey world".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let b_push = sender.pending_draft_pushes().unwrap().remove(0);
    let b_branch = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&b_push, b_branch)], "2")
        .unwrap();
    sender
        .save_compose_draft(
            b.draft_id,
            b.revision,
            ComposeDraftUpdate {
                text: "hello earth".into(),
                recipients: b.recipients,
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let c_push = sender.pending_draft_pushes().unwrap().remove(0);
    let c_branch = uuid::Uuid::new_v4();

    client
        .apply_draft_changes(vec![remote_head(&c_push, c_branch)], "3")
        .unwrap();

    let status = client.draft_sync_status(initial.draft_id).unwrap();
    assert!(
        status
            .conflicts
            .iter()
            .any(|conflict| conflict.branch_id == b_branch)
    );
    assert!(
        client
            .pending_draft_pushes()
            .unwrap()
            .iter()
            .all(|push| !push.resolves_branch_ids.contains(&b_branch))
    );
}

#[test]
fn closed_tracked_branch_still_requires_remote_send_arbitration() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "remote", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    accept_push(&client, &push, branch_id);
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::BranchClosed {
                draft_id: saved.draft_id,
                branch_id,
            }],
            "1",
        )
        .unwrap();

    assert!(
        client
            .begin_draft_send(saved.draft_id, saved.revision, None)
            .unwrap()
            .remote
            .is_some()
    );
}

#[test]
fn closed_tracked_branch_still_queues_a_remote_discard() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate::default(),
        )
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    accept_push(&client, &push, branch_id);
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::BranchClosed {
                draft_id: saved.draft_id,
                branch_id,
            }],
            "1",
        )
        .unwrap();

    client
        .discard_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    assert_eq!(
        client.pending_draft_discards().unwrap(),
        vec![saved.draft_id]
    );
}

#[test]
fn caption_merge_rejects_touching_changes_and_deduplicates_identical_insertions() {
    for (base, local_text, remote_text, expected) in [
        ("ab", "aX", "aYb", None),
        ("ab", "aYb", "aX", None),
        ("abcd", "aXcd", "abYd", None),
        ("a🦀b", "aXb", "a🦀Yb", None),
        ("ab", "aXb", "aXb", Some("aXb")),
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let vault = Vault::new();
        let sender = unlocked(&config(&dir, "sender", &vault), &vault);
        let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
        let initial = save_draft(&sender, base, vec![ADDRESS.into()]);
        let branch_id = uuid::Uuid::new_v4();
        let first = sender.pending_draft_pushes().unwrap().remove(0);
        receiver
            .apply_draft_changes(vec![remote_head(&first, branch_id)], "1")
            .unwrap();
        let local = receiver.compose_draft(initial.draft_id).unwrap().unwrap();
        receiver
            .save_compose_draft(
                local.draft_id,
                local.revision,
                ComposeDraftUpdate {
                    text: local_text.into(),
                    recipients: local.recipients.clone(),
                    attachment_ids: local.attachment_ids.clone(),
                    route: local.route.clone(),
                },
            )
            .unwrap();
        sender
            .save_compose_draft(
                initial.draft_id,
                initial.revision,
                ComposeDraftUpdate {
                    text: remote_text.into(),
                    recipients: vec![ADDRESS.into()],
                    attachment_ids: vec![],
                    route: None,
                },
            )
            .unwrap();
        let remote = sender.pending_draft_pushes().unwrap().remove(0);
        receiver
            .apply_draft_changes(vec![remote_head(&remote, branch_id)], "2")
            .unwrap();

        match expected {
            Some(text) => assert_eq!(
                receiver
                    .compose_draft(initial.draft_id)
                    .unwrap()
                    .unwrap()
                    .text,
                text
            ),
            None => assert_eq!(
                receiver
                    .draft_sync_status(initial.draft_id)
                    .unwrap()
                    .conflicts
                    .len(),
                1
            ),
        }
    }
}

#[test]
fn overlapping_caption_edits_remain_a_conflict() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let initial = save_draft(&sender, "hello", vec![ADDRESS.into()]);
    let branch_id = uuid::Uuid::new_v4();
    let first = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&first, branch_id)], "1")
        .unwrap();
    let local = receiver.compose_draft(initial.draft_id).unwrap().unwrap();
    receiver
        .save_compose_draft(
            local.draft_id,
            local.revision,
            ComposeDraftUpdate {
                text: "hullo".into(),
                recipients: local.recipients.clone(),
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    sender
        .save_compose_draft(
            initial.draft_id,
            initial.revision,
            ComposeDraftUpdate {
                text: "hallo".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let remote = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&remote, branch_id)], "2")
        .unwrap();

    assert_eq!(
        receiver
            .draft_sync_status(initial.draft_id)
            .unwrap()
            .conflicts
            .len(),
        1
    );
}

#[test]
fn clean_remote_head_adopts_into_matching_conversation() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let saved = save_draft(&sender, "remote", vec![ADDRESS.into()]);
    let push = sender.pending_draft_pushes().unwrap().remove(0);

    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();

    let adopted = receiver.compose_draft(saved.draft_id).unwrap().unwrap();
    assert_eq!(adopted.text, "remote");
    assert_eq!(adopted.recipients, vec![ADDRESS]);
}

#[test]
fn dirty_local_head_is_stored_as_a_conflict() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "local", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();

    client
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    let status = client.draft_sync_status(saved.draft_id).unwrap();
    assert_eq!(status.conflicts.len(), 1);
    assert_eq!(status.conflicts[0].branch_id, branch_id);
    assert_eq!(
        client.compose_draft(saved.draft_id).unwrap().unwrap().text,
        "local"
    );
}

#[test]
fn remote_identity_does_not_overwrite_a_nonblank_conversation_draft() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let local = save_draft(&receiver, "local", vec![ADDRESS.into()]);
    let remote = save_draft(&sender, "remote", vec![ADDRESS.into()]);
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();

    receiver
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    assert_eq!(
        receiver
            .compose_draft(local.draft_id)
            .unwrap()
            .unwrap()
            .text,
        "local"
    );
    assert!(receiver.compose_draft(remote.draft_id).unwrap().is_none());
    assert_eq!(
        receiver
            .draft_sync_status(local.draft_id)
            .unwrap()
            .conflicts[0]
            .branch_id,
        branch_id
    );
    receiver
        .apply_draft_changes(
            vec![RemoteDraftChange::BranchClosed {
                draft_id: remote.draft_id,
                branch_id,
            }],
            "2",
        )
        .unwrap();
    assert!(
        receiver
            .draft_sync_status(local.draft_id)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn resolving_conflict_keeps_local_and_queues_other_branches() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "local", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    let resolved = client.resolve_draft_conflict(saved.draft_id, None).unwrap();
    let pending = client.pending_draft_pushes().unwrap().remove(0);

    assert_eq!(resolved.text, "local");
    assert_eq!(pending.resolves_branch_ids, vec![branch_id]);
}

#[test]
fn resolving_conflict_with_remote_version_applies_it_and_closed_branch_removes_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "remote", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    let resolved = client
        .resolve_draft_conflict(saved.draft_id, Some(branch_id))
        .unwrap();
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::BranchClosed {
                draft_id: saved.draft_id,
                branch_id,
            }],
            "2",
        )
        .unwrap();

    assert_eq!(resolved.text, "remote");
    assert!(
        client
            .draft_sync_status(saved.draft_id)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn discard_of_synced_draft_is_queued_then_cleared() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate::default(),
        )
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .record_draft_push(
            saved.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();

    client
        .discard_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    assert_eq!(
        client.pending_draft_discards().unwrap(),
        vec![saved.draft_id]
    );
    client.record_draft_discard(saved.draft_id).unwrap();
    assert!(client.pending_draft_discards().unwrap().is_empty());
}

#[test]
fn attachment_failure_states_map_upload_download_and_unavailable() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let uploader = unlocked(&config(&dir, "uploader", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let source = dir.path().join("draft-image.png");
    std::fs::write(&source, b"draft image").unwrap();
    let attachment = uploader
        .prepare_attachment(&source, "image/png", "draft-image.png")
        .unwrap();
    let draft = uploader.recipientless_compose_draft().unwrap();
    let saved = uploader
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "image".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    uploader
        .record_draft_attachment_transfer_failure(
            attachment.attachment_id,
            DraftTransferDirection::Upload,
        )
        .unwrap();
    assert_eq!(
        uploader.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::UploadFailed
    );

    uploader
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let push = uploader.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();
    receiver
        .record_draft_attachment_transfer_failure(
            attachment.attachment_id,
            DraftTransferDirection::Download,
        )
        .unwrap();
    assert_eq!(
        receiver.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::DownloadFailed
    );

    let cipher = uploader
        .native_cipher_file(attachment.attachment_id)
        .unwrap();
    std::fs::remove_file(cipher).unwrap();
    assert_eq!(
        uploader.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::Unavailable
    );
}

#[test]
fn downloaded_synced_draft_queues_one_mms_after_verified_install() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let receiver_config = config(&dir, "receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let source = dir.path().join("shared-image.png");
    std::fs::write(&source, b"shared draft image").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/png", "shared-image.png")
        .unwrap();
    let cipher =
        std::fs::read(sender.native_cipher_file(attachment.attachment_id).unwrap()).unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let saved = sender
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "caption".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: Some(route(&sender_config)),
        })
        .unwrap();
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();
    let received = receiver.compose_draft(saved.draft_id).unwrap().unwrap();
    assert_eq!(received.route, Some(route(&sender_config)));
    assert!(
        receiver
            .begin_draft_send(saved.draft_id, received.revision, None)
            .is_err()
    );
    assert!(receiver.pending_outbox().unwrap().is_empty());

    let download = dir.path().join("shared-image.cipher");
    std::fs::write(&download, cipher).unwrap();
    receiver
        .install_downloaded_attachment(attachment.attachment_id, &download)
        .unwrap();
    assert_eq!(
        receiver
            .attachment_info(attachment.attachment_id)
            .unwrap()
            .state,
        AttachmentState::Available
    );
    assert_eq!(
        receiver.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::Uploaded
    );

    let routed = receiver
        .save_compose_draft(
            saved.draft_id,
            received.revision,
            ComposeDraftUpdate {
                text: received.text,
                recipients: received.recipients,
                attachment_ids: received.attachment_ids,
                route: Some(route(&receiver_config)),
            },
        )
        .unwrap();
    let route_push = receiver.pending_draft_pushes().unwrap().remove(0);
    accept_push(&receiver, &route_push, uuid::Uuid::new_v4());
    let send = receiver
        .begin_draft_send(saved.draft_id, routed.revision, None)
        .unwrap()
        .remote
        .unwrap();
    receiver
        .send_compose_draft_accepted(saved.draft_id, routed.revision, send.command_id)
        .unwrap();
    let queued = receiver.pending_outbox().unwrap();
    assert_eq!(queued.len(), 1);
    let message = only_message(&receiver, received.conversation_id);
    assert_eq!(message.payload.body, "caption");
    assert_eq!(message.payload.transport, Transport::Mms);
    assert_eq!(
        message.payload.record.attachments,
        vec![AttachmentReference {
            attachment_id: attachment.attachment_id,
            pending: false,
        }]
    );
    receiver
        .send_compose_draft_accepted(saved.draft_id, routed.revision, send.command_id)
        .unwrap();
    assert_eq!(receiver.pending_outbox().unwrap(), queued);
}

#[test]
fn resolving_same_draft_conflicts_uses_the_selected_branch_as_the_next_base() {
    for keep_remote in [false, true] {
        let dir = tempfile::TempDir::new().unwrap();
        let vault = Vault::new();
        let client = unlocked(&config(&dir, "client", &vault), &vault);
        let saved = save_draft(&client, "A", vec![ADDRESS.into()]);
        let push_a = client.pending_draft_pushes().unwrap().remove(0);
        let branch_a = uuid::Uuid::new_v4();
        accept_push(&client, &push_a, branch_a);
        let edited = client
            .save_compose_draft(
                saved.draft_id,
                saved.revision,
                ComposeDraftUpdate {
                    text: "B".into(),
                    recipients: vec![ADDRESS.into()],
                    attachment_ids: vec![],
                    route: None,
                },
            )
            .unwrap();
        let push_b = client.pending_draft_pushes().unwrap().remove(0);
        let branch_b = uuid::Uuid::new_v4();
        client
            .apply_draft_changes(vec![remote_head(&push_a, branch_a)], "1")
            .unwrap();
        client
            .apply_draft_changes(vec![remote_head(&push_b, branch_b)], "2")
            .unwrap();

        let resolved = client
            .resolve_draft_conflict(saved.draft_id, keep_remote.then_some(branch_b))
            .unwrap();
        let pending = client.pending_draft_pushes().unwrap().remove(0);
        assert_eq!(resolved.text, "B");
        assert_eq!(
            pending.base_revision_id,
            Some(if keep_remote {
                push_b.revision_id
            } else {
                push_a.revision_id
            })
        );
        assert!(pending.resolves_branch_ids.contains(&if keep_remote {
            branch_a
        } else {
            branch_b
        }));
        assert!(!pending.resolves_branch_ids.contains(&if keep_remote {
            branch_b
        } else {
            branch_a
        }));
        assert_eq!(resolved.attachment_ids, edited.attachment_ids);
        assert_eq!(resolved.route, edited.route);
    }
}

#[test]
fn attachment_inventory_excludes_remote_downloads_and_unavailable_placeholders() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let source = dir.path().join("inventory.png");
    std::fs::write(&source, b"inventory").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/png", "inventory.png")
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(
        sender
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&attachment.attachment_id)
    );
    let saved = sender
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "remote image".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: None,
        })
        .unwrap();
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();
    assert!(
        !receiver
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&attachment.attachment_id)
    );

    let local = sender.recipientless_compose_draft().unwrap();
    let local = sender
        .save_compose_draft(
            local.draft_id,
            local.revision,
            ComposeDraftUpdate {
                text: "offline".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    std::fs::remove_file(sender.native_cipher_file(attachment.attachment_id).unwrap()).unwrap();
    expire(&sender, local.draft_id, "1");
    let placeholder = sender
        .compose_drafts()
        .unwrap()
        .into_iter()
        .find(|draft| draft.draft_id != saved.draft_id)
        .unwrap()
        .attachment_ids[0];
    assert!(
        !sender
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&placeholder)
    );
}

#[test]
fn accepted_r1_after_r2_save_keeps_r2_pending_on_r1_base() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let r1 = client.pending_draft_pushes().unwrap().remove(0);
    let r2 = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let branch_id = uuid::Uuid::new_v4();
    accept_push(&client, &r1, branch_id);
    let pending = client.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(pending.base_revision_id, Some(r1.revision_id));
    assert_eq!(client.compose_draft(saved.draft_id).unwrap().unwrap(), r2);
    client
        .apply_draft_changes(vec![remote_head(&r1, branch_id)], "1")
        .unwrap();
    assert_eq!(client.pending_draft_pushes().unwrap(), vec![pending]);
    assert!(
        client
            .draft_sync_status(saved.draft_id)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn late_r1_acceptance_without_an_echo_cannot_merge_r2_on_an_old_base() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let initial = save_draft(&sender, "a b c", vec![ADDRESS.into()]);
    let branch_id = uuid::Uuid::new_v4();
    let r0 = sender.pending_draft_pushes().unwrap().remove(0);
    client
        .apply_draft_changes(vec![remote_head(&r0, branch_id)], "1")
        .unwrap();
    let adopted = client.compose_draft(initial.draft_id).unwrap().unwrap();
    let r1 = client
        .save_compose_draft(
            adopted.draft_id,
            adopted.revision,
            ComposeDraftUpdate {
                text: "x b c".into(),
                recipients: adopted.recipients.clone(),
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let r1_push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .save_compose_draft(
            r1.draft_id,
            r1.revision,
            ComposeDraftUpdate {
                text: "x b c!".into(),
                recipients: r1.recipients,
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    client
        .record_draft_push(
            initial.draft_id,
            r1_push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id,
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();
    sender
        .save_compose_draft(
            initial.draft_id,
            initial.revision,
            ComposeDraftUpdate {
                text: "a b y".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let r3 = sender.pending_draft_pushes().unwrap().remove(0);

    client
        .apply_draft_changes(vec![remote_head(&r3, branch_id)], "2")
        .unwrap();

    assert_eq!(
        client
            .draft_sync_status(initial.draft_id)
            .unwrap()
            .conflicts
            .len(),
        1
    );
}

#[test]
fn selecting_a_conflict_branch_closes_the_original_tracked_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let tracked = uuid::Uuid::new_v4();
    accept_push(&client, &push, tracked);
    let chosen = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, chosen)], "1")
        .unwrap();

    client
        .resolve_draft_conflict(saved.draft_id, Some(chosen))
        .unwrap();
    let outgoing = client.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(outgoing.base_revision_id, Some(push.revision_id));
    assert_eq!(outgoing.resolves_branch_ids, vec![tracked]);
}

#[test]
fn local_send_retires_the_old_remote_identity_before_a_delayed_acceptance() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let source = dir.path().join("sent-offline.png");
    std::fs::write(&source, b"sent offline").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/png", "sent-offline.png")
        .unwrap();
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "sent offline".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: Some(route(&client_config)),
        })
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let old_push = client
        .pending_draft_pushes()
        .unwrap()
        .into_iter()
        .find(|push| push.draft_id == saved.draft_id)
        .unwrap();

    client.set_draft_sync_supported(false).unwrap();
    client
        .send_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    client.set_draft_sync_supported(true).unwrap();
    assert!(client.pending_draft_pushes().unwrap().is_empty());
    let cleared = client.compose_draft(saved.draft_id).unwrap().unwrap();
    assert_eq!(cleared.draft_id, saved.draft_id);
    assert_eq!(cleared.revision, saved.revision + 1);

    accept_push(&client, &old_push, uuid::Uuid::new_v4());
    assert_eq!(
        client.pending_draft_discards().unwrap(),
        vec![saved.draft_id]
    );
    assert!(client.pending_draft_pushes().unwrap().is_empty());

    let next = client
        .save_compose_draft(
            cleared.draft_id,
            cleared.revision,
            ComposeDraftUpdate {
                text: "next".into(),
                recipients: cleared.recipients,
                attachment_ids: vec![],
                route: cleared.route,
            },
        )
        .unwrap();
    assert_ne!(next.draft_id, saved.draft_id);
    assert!(matches!(
        client.send_compose_draft(saved.draft_id, cleared.revision),
        Err(Error::StaleDraft { .. })
    ));
    assert_eq!(
        client.pending_draft_pushes().unwrap()[0].draft_id,
        next.draft_id
    );
}

fn expire(client: &Client, draft_id: DraftId, cursor: &str) {
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id,
                reason: DraftTerminalReasonCore::Expired,
                command_id: None,
            }],
            cursor,
        )
        .unwrap();
}

#[test]
fn expired_clean_draft_is_removed() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "clean", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .record_draft_push(
            saved.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();

    expire(&client, saved.draft_id, "1");

    assert!(client.compose_draft(saved.draft_id).unwrap().is_none());
}

#[test]
fn expired_dirty_draft_reprepares_images_under_fresh_ids() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let source = dir.path().join("draft-image.png");
    std::fs::write(&source, b"draft image bytes").unwrap();
    let old_attachment = client
        .prepare_attachment(&source, "image/png", "draft-image.png")
        .unwrap();
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "offline edit".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![old_attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    let old_ciphertext = std::fs::read(
        client
            .native_cipher_file(old_attachment.attachment_id)
            .unwrap(),
    )
    .unwrap();
    let old_plaintext = std::fs::read(
        client
            .open_native_plaintext(old_attachment.attachment_id)
            .unwrap()
            .path(),
    )
    .unwrap();

    expire(&client, saved.draft_id, "1");

    assert!(client.compose_draft(saved.draft_id).unwrap().is_none());
    let restored = client.compose_drafts().unwrap().into_iter().next().unwrap();
    let new_attachment = restored.attachment_ids[0];
    assert_ne!(restored.draft_id, saved.draft_id);
    assert_ne!(new_attachment, old_attachment.attachment_id);
    assert_eq!(restored.text, "offline edit");
    assert_eq!(restored.recipients, vec![ADDRESS]);
    assert_ne!(
        std::fs::read(client.native_cipher_file(new_attachment).unwrap()).unwrap(),
        old_ciphertext
    );
    assert_eq!(
        std::fs::read(client.open_native_plaintext(new_attachment).unwrap().path()).unwrap(),
        old_plaintext
    );
    assert_eq!(
        client.pending_draft_uploads().unwrap()[0].draft_id,
        restored.draft_id
    );
    assert_eq!(
        client.pending_draft_uploads().unwrap()[0]
            .object
            .attachment_id,
        new_attachment
    );
}

#[test]
fn expired_draft_with_missing_ciphertext_keeps_unavailable_placeholder() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let source = dir.path().join("missing-image.png");
    std::fs::write(&source, b"missing image").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/png", "missing-image.png")
        .unwrap();
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "offline edit".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    std::fs::remove_file(client.native_cipher_file(attachment.attachment_id).unwrap()).unwrap();

    expire(&client, saved.draft_id, "1");

    let restored = client.compose_drafts().unwrap().into_iter().next().unwrap();
    let placeholder = restored.attachment_ids[0];
    assert_ne!(placeholder, attachment.attachment_id);
    assert_eq!(
        client.attachment_info(placeholder).unwrap().display_name,
        "missing-image.png"
    );
    assert_eq!(
        client.draft_attachment_states(restored.draft_id).unwrap()[0].1,
        DraftAttachmentState::Unavailable
    );
    assert!(client.pending_draft_uploads().unwrap().is_empty());
}

#[test]
fn repeated_expired_terminal_and_push_termination_restore_once() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "offline", vec![ADDRESS.into()]);

    expire(&client, saved.draft_id, "1");
    let restored = client.compose_drafts().unwrap().into_iter().next().unwrap();
    expire(&client, saved.draft_id, "2");
    client
        .record_draft_push(
            saved.draft_id,
            uuid::Uuid::new_v4(),
            DraftPushOutcome::Terminated(DraftTerminalReasonCore::Expired),
        )
        .unwrap();

    assert_eq!(client.compose_drafts().unwrap(), vec![restored]);
}

#[test]
fn bootstrap_remains_required_until_successful_finish_across_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    assert!(client.draft_bootstrap_required().unwrap());
    client.apply_draft_changes(vec![], "1").unwrap();
    assert!(client.draft_bootstrap_required().unwrap());
    drop(client);

    let reopened = unlocked(&client_config, &vault);
    assert!(reopened.draft_bootstrap_required().unwrap());
    reopened.finish_draft_bootstrap(&[]).unwrap();
    assert!(!reopened.draft_bootstrap_required().unwrap());
    reopened.set_draft_sync_supported(false).unwrap();
    assert!(!reopened.draft_bootstrap_required().unwrap());
}

#[test]
fn abort_draft_send_intent_requires_the_matching_unqueued_command() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "ready", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .record_draft_push(
            saved.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();
    let command = client
        .begin_draft_send(saved.draft_id, saved.revision, None)
        .unwrap()
        .remote
        .unwrap()
        .command_id;

    client
        .abort_draft_send_intent(saved.draft_id, CommandId::new())
        .unwrap();
    assert_eq!(
        client
            .begin_draft_send(saved.draft_id, saved.revision, None)
            .unwrap()
            .remote
            .unwrap()
            .command_id,
        command
    );
    client
        .abort_draft_send_intent(saved.draft_id, command)
        .unwrap();
    assert_ne!(
        client
            .begin_draft_send(saved.draft_id, saved.revision, None)
            .unwrap()
            .remote
            .unwrap()
            .command_id,
        command
    );
}

#[test]
fn distinct_live_branch_is_retained_after_clean_tracked_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let tracked_branch = uuid::Uuid::new_v4();
    accept_push(&client, &push, tracked_branch);

    let conflicting_branch = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, conflicting_branch)], "1")
        .unwrap();

    let status = client.draft_sync_status(saved.draft_id).unwrap();
    assert_eq!(status.conflicts.len(), 1);
    assert_eq!(status.conflicts[0].branch_id, conflicting_branch);
    assert_eq!(
        client.compose_draft(saved.draft_id).unwrap().unwrap().text,
        "r1"
    );
}

#[test]
fn duplicate_remote_descriptor_is_reused_but_a_mismatched_id_is_quarantined() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let source = dir.path().join("image.png");
    std::fs::write(&source, b"image").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/png", "image.png")
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let saved = sender
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "image".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: None,
        })
        .unwrap();
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    receiver
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();
    receiver
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "2")
        .unwrap();

    let mut mismatched = remote_head(&push, uuid::Uuid::new_v4());
    let RemoteDraftChange::Head { attachment_ids, .. } = &mut mismatched else {
        unreachable!()
    };
    attachment_ids[0] = AttachmentId::new();
    receiver.apply_draft_changes(vec![mismatched], "3").unwrap();
    assert_eq!(receiver.draft_sync_cursor().unwrap(), "3");
    assert_eq!(
        receiver
            .compose_draft(saved.draft_id)
            .unwrap()
            .unwrap()
            .attachment_ids,
        vec![attachment.attachment_id]
    );
}

#[test]
fn save_new_and_recipient_move_keep_draft_sync_state() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "new".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: None,
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "destination".into(),
            recipients: vec!["+15555550101".into()],
            attachment_ids: vec![],
            route: None,
        })
        .unwrap();
    let moved = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "moved".into(),
                recipients: vec!["+15555550101".into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();

    assert_ne!(moved.draft_id, saved.draft_id);
    assert_eq!(
        client.pending_draft_discards().unwrap(),
        vec![saved.draft_id]
    );
    assert_eq!(
        client.pending_draft_pushes().unwrap()[0].draft_id,
        moved.draft_id
    );
}

#[test]
fn unsupported_never_accepted_draft_stays_local_sendable() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "offline", vec![ADDRESS.into()]);

    client.set_draft_sync_supported(false).unwrap();

    assert!(
        !client
            .draft_sync_status(saved.draft_id)
            .unwrap()
            .send_requires_connection
    );
    assert!(
        client
            .begin_draft_send(saved.draft_id, saved.revision, None)
            .unwrap()
            .remote
            .is_none()
    );
}

#[test]
fn unsupported_sync_cannot_revive_a_retired_draft_identity() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "offline".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();

    client
        .send_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    client.set_draft_sync_supported(false).unwrap();
    let cleared = client.compose_draft(saved.draft_id).unwrap().unwrap();
    let replacement = client
        .save_compose_draft(
            cleared.draft_id,
            cleared.revision,
            ComposeDraftUpdate {
                text: "next".into(),
                recipients: cleared.recipients,
                attachment_ids: vec![],
                route: cleared.route,
            },
        )
        .unwrap();

    assert_ne!(replacement.draft_id, saved.draft_id);
}

#[test]
fn matching_sent_terminal_recovers_one_snapshot_command_after_empty_apply() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "send snapshot".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    let remote = client
        .begin_draft_send(saved.draft_id, saved.revision, None)
        .unwrap()
        .remote
        .unwrap();
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: saved.draft_id,
                reason: DraftTerminalReasonCore::Sent,
                command_id: Some(remote.command_id),
            }],
            "1",
        )
        .unwrap();
    let first = client.pending_outbox().unwrap();
    client.apply_draft_changes(vec![], "2").unwrap();
    assert_eq!(client.pending_outbox().unwrap(), first);
    assert_eq!(first.len(), 1);
}

#[test]
fn provisional_sent_push_never_queues_a_local_command() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "send snapshot".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    let _command = client
        .begin_draft_send(saved.draft_id, saved.revision, None)
        .unwrap()
        .remote
        .unwrap()
        .command_id;

    client
        .record_draft_push(
            saved.draft_id,
            push.revision_id,
            DraftPushOutcome::Terminated(DraftTerminalReasonCore::Sent),
        )
        .unwrap();

    assert!(client.pending_outbox().unwrap().is_empty());
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: saved.draft_id,
                reason: DraftTerminalReasonCore::Sent,
                command_id: Some(CommandId::new()),
            }],
            "1",
        )
        .unwrap();
    assert!(client.pending_outbox().unwrap().is_empty());
}

#[test]
fn accepted_snapshot_replay_does_not_clear_a_moved_destination() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "before move".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    let command = client
        .begin_draft_send(saved.draft_id, saved.revision, None)
        .unwrap()
        .remote
        .unwrap()
        .command_id;
    let moved = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "moved destination".into(),
                recipients: vec!["+15555550101".into()],
                attachment_ids: vec![],
                route: Some(route(&client_config)),
            },
        )
        .unwrap();

    client
        .send_compose_draft_accepted(saved.draft_id, saved.revision, command)
        .unwrap();

    assert_eq!(
        only_message(&client, saved.conversation_id).payload.body,
        "before move"
    );
    assert_eq!(
        client.compose_draft(moved.draft_id).unwrap().unwrap().text,
        "moved destination"
    );
}

#[test]
fn send_intent_keeps_r1_snapshot_after_r2_edit_and_rejects_stale_cas() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "r1".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    assert!(matches!(
        client.begin_draft_send(saved.draft_id, saved.revision + 1, None),
        Err(Error::StaleDraft { .. })
    ));
    let remote = client
        .begin_draft_send(saved.draft_id, saved.revision, None)
        .unwrap()
        .remote
        .unwrap();
    let edited = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: Some(route(&client_config)),
            },
        )
        .unwrap();
    assert_eq!(
        client
            .begin_draft_send(saved.draft_id, edited.revision, None)
            .unwrap()
            .remote
            .unwrap(),
        remote
    );
    client
        .send_compose_draft_accepted(saved.draft_id, saved.revision, remote.command_id)
        .unwrap();
    assert_eq!(
        only_message(&client, saved.conversation_id).payload.body,
        "r1"
    );
    assert_eq!(
        client.compose_draft(saved.draft_id).unwrap().unwrap().text,
        "r2"
    );
}

#[test]
fn nonmatching_sent_terminal_keeps_dirty_draft_without_queueing() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: saved.draft_id,
                reason: DraftTerminalReasonCore::Sent,
                command_id: Some(CommandId::new()),
            }],
            "1",
        )
        .unwrap();
    let kept = client.compose_drafts().unwrap();
    assert_eq!(kept.len(), 1);
    assert_ne!(kept[0].draft_id, saved.draft_id);
    assert_eq!(kept[0].conversation_id, saved.conversation_id);
    assert_eq!(kept[0].text, "r2");
    assert!(client.pending_outbox().unwrap().is_empty());
}

#[test]
fn unsupported_sync_uses_legacy_only_for_never_synced_drafts() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    client.set_draft_sync_supported(false).unwrap();
    let local = save_draft(&client, "local", vec![ADDRESS.into()]);
    assert!(
        client
            .begin_draft_send(local.draft_id, local.revision, None)
            .unwrap()
            .remote
            .is_none()
    );
    client.set_draft_sync_supported(true).unwrap();
    let synced = save_draft(&client, "synced", vec!["+15555550101".into()]);
    let push = client
        .pending_draft_pushes()
        .unwrap()
        .into_iter()
        .find(|push| push.draft_id == synced.draft_id)
        .unwrap();
    accept_push(&client, &push, uuid::Uuid::new_v4());
    client.set_draft_sync_supported(false).unwrap();
    assert!(
        client
            .begin_draft_send(synced.draft_id, synced.revision, None)
            .unwrap()
            .remote
            .is_some()
    );
}

#[test]
fn intent_reference_prevents_attachment_cleanup_until_abort_and_discard() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let source = dir.path().join("held.png");
    std::fs::write(&source, b"held").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/png", "held.png")
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "held".into(),
            recipients: vec![],
            attachment_ids: vec![attachment.attachment_id],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    let command = client
        .begin_draft_send(saved.draft_id, saved.revision, None)
        .unwrap()
        .remote
        .unwrap()
        .command_id;
    assert!(
        !client
            .discard_unreferenced_attachment(attachment.attachment_id)
            .unwrap()
    );
    client
        .abort_draft_send_intent(saved.draft_id, command)
        .unwrap();
    let emptied = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate::default(),
        )
        .unwrap();
    client
        .discard_compose_draft(saved.draft_id, emptied.revision)
        .unwrap();
    assert!(
        client
            .discard_unreferenced_attachment(attachment.attachment_id)
            .unwrap()
    );
}
