use super::*;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct McpCommand {
    pub grant_id: Uuid,
    pub tool: String,
    pub idempotency_key: String,
    pub conversation_id: ConversationId,
    pub recipients: Vec<String>,
    pub body: String,
    pub attachment_ids: Vec<AttachmentId>,
    pub route: GatewayRoute,
    pub expected_transport: Transport,
    /// SHA-256 of the canonical caller request, before generated IDs, route lookup, or preflight.
    pub request_hash: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpQueuedCommand {
    pub message_id: MessageId,
    pub command_id: CommandId,
    pub envelope_id: EnvelopeId,
    pub conversation_id: ConversationId,
    pub attachment_ids: Vec<AttachmentId>,
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS mcp_command_receipts(grant_id TEXT NOT NULL,tool TEXT NOT NULL,idempotency_key TEXT NOT NULL,input_hash BLOB NOT NULL,response TEXT NOT NULL,PRIMARY KEY(grant_id,tool,idempotency_key));")?;
    Ok(())
}
impl Client {
    pub fn lookup_mcp_command(
        &self,
        grant_id: Uuid,
        tool: &str,
        idempotency_key: &str,
        request_hash: &[u8],
    ) -> Result<Option<McpQueuedCommand>, Error> {
        let store = self.lock()?;
        let prior: Option<(Vec<u8>, String)> = store.conn.query_row("SELECT input_hash,response FROM mcp_command_receipts WHERE grant_id=? AND tool=? AND idempotency_key=?", params![grant_id.to_string(), tool, idempotency_key], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        match prior {
            None => Ok(None),
            Some((stored, response)) if stored == request_hash => serde_json::from_str(&response)
                .map(Some)
                .map_err(|_| Error::Database),
            Some(_) => Err(Error::Conflict),
        }
    }
    pub fn queue_mcp_command(&self, command: McpCommand) -> Result<McpQueuedCommand, Error> {
        if command.tool.is_empty()
            || command.tool.len() > 64
            || command.idempotency_key.is_empty()
            || command.idempotency_key.len() > 256
        {
            return Err(Error::InvalidRequest("idempotency key"));
        }
        if command.request_hash.len() != 32 {
            return Err(Error::InvalidRequest("idempotency hash"));
        }
        let hash = command.request_hash.clone();
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<(Vec<u8>, String)> = tx.query_row("SELECT input_hash,response FROM mcp_command_receipts WHERE grant_id=? AND tool=? AND idempotency_key=?", params![command.grant_id.to_string(), command.tool, command.idempotency_key], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        if let Some((old_hash, response)) = prior {
            if old_hash != hash {
                return Err(Error::Conflict);
            }
            return serde_json::from_str(&response).map_err(|_| Error::Database);
        }
        let transport = classify_outgoing_transport(
            &tx,
            command.conversation_id,
            &command.recipients,
            &command.attachment_ids,
        )?;
        if transport != command.expected_transport {
            return Err(Error::InvalidRequest("transport changed; retry"));
        }
        if transport == Transport::Sms {
            validate_outgoing(
                &OutgoingSms {
                    conversation_id: command.conversation_id,
                    recipients: command.recipients.clone(),
                    body: command.body.clone(),
                },
                &command.route,
            )?;
        } else {
            validate_outgoing_mms(
                &command.recipients,
                &command.body,
                &command.attachment_ids,
                None,
                &command.route,
            )?;
        }
        let result = queue_message(
            &tx,
            &ctx,
            OutgoingSms {
                conversation_id: command.conversation_id,
                recipients: command.recipients,
                body: command.body,
            },
            &command.route,
            local_references(&tx, &command.attachment_ids)?,
            transport,
            None,
            None,
        )?;
        let response = McpQueuedCommand {
            message_id: result.message_id,
            command_id: result.command_id,
            envelope_id: result.envelope_id,
            conversation_id: command.conversation_id,
            attachment_ids: command.attachment_ids.clone(),
        };
        tx.execute("INSERT INTO mcp_command_receipts(grant_id,tool,idempotency_key,input_hash,response) VALUES(?,?,?,?,?)", params![command.grant_id.to_string(), command.tool, command.idempotency_key, hash, serde_json::to_string(&response).map_err(|_| Error::Database)?])?;
        tx.commit()?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::create_vault_check_header;

    fn opened(path: std::path::PathBuf, vault: VaultId, device: DeviceId) -> Client {
        let client = Client::open(
            ClientConfig {
                database_path: path,
                vault_id: vault,
                device_id: device,
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let root = derive_root_key("mcp commands test phrase", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        client
            .unlock(&profile, &header, "mcp commands test phrase")
            .unwrap();
        client
    }
    fn command(
        grant: Uuid,
        key: &str,
        body: &str,
        route: GatewayRoute,
        conversation_id: ConversationId,
    ) -> McpCommand {
        McpCommand {
            grant_id: grant,
            tool: "send_message".into(),
            idempotency_key: key.into(),
            conversation_id,
            recipients: vec!["15551234567".into()],
            body: body.into(),
            attachment_ids: Vec::new(),
            route,
            expected_transport: Transport::Sms,
            request_hash: Sha256::digest([key.as_bytes(), body.as_bytes()].concat()).to_vec(),
        }
    }
    fn route() -> GatewayRoute {
        GatewayRoute::Carrier {
            gateway_device_id: DeviceId::new(),
            subscription_id: "sim-1".into(),
        }
    }

    fn pending_remote_attachment(
        client: &Client,
        directory: &std::path::Path,
    ) -> (AttachmentId, std::path::PathBuf, std::path::PathBuf) {
        let source = directory.join("image.bin");
        std::fs::write(&source, b"prepared image").unwrap();
        let attachment = client
            .prepare_attachment(&source, "image/png", "image.png")
            .unwrap();
        let (cipher_path, downloaded) = {
            let store = client.lock().unwrap();
            let cipher_path = crate::media::cipher_path(&store.media_dir, attachment.attachment_id);
            let downloaded = directory.join("downloaded.ciphertext");
            std::fs::copy(&cipher_path, &downloaded).unwrap();
            (cipher_path, downloaded)
        };
        client
            .mark_attachment_uploaded(attachment.attachment_id, &Uuid::new_v4().to_string())
            .unwrap();
        client
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE attachments SET state=? WHERE attachment_id=?",
                params![
                    AttachmentState::PendingDownload.code(),
                    attachment.attachment_id.to_string()
                ],
            )
            .unwrap();
        (attachment.attachment_id, cipher_path, downloaded)
    }

    #[test]
    fn retry_replays_one_command_and_conflicting_input_is_rejected_without_mutation() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = opened(dir.path().join("mcp.db"), VaultId::new(), DeviceId::new());
        let grant = Uuid::new_v4();
        let route = route();
        let conversation = ConversationId::new();
        let first = client
            .queue_mcp_command(command(grant, "key", "first", route.clone(), conversation))
            .unwrap();
        let replay = client
            .queue_mcp_command(command(grant, "key", "first", route.clone(), conversation))
            .unwrap();
        assert_eq!(first, replay);
        assert_eq!(client.pending_outbox_count().unwrap(), 1);
        assert_eq!(
            client
                .queue_mcp_command(command(grant, "key", "different", route, conversation))
                .unwrap_err(),
            Error::Conflict
        );
        assert_eq!(client.pending_outbox_count().unwrap(), 1);
    }

    #[test]
    fn same_key_is_isolated_by_grant_and_survives_cache_only_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mcp.db");
        let vault = VaultId::new();
        let device = DeviceId::new();
        let client = opened(path.clone(), vault, device);
        let grant = Uuid::new_v4();
        let route = route();
        let conversation = ConversationId::new();
        let first = client
            .queue_mcp_command(command(grant, "key", "body", route.clone(), conversation))
            .unwrap();
        let second = client
            .queue_mcp_command(command(
                Uuid::new_v4(),
                "key",
                "body",
                route.clone(),
                conversation,
            ))
            .unwrap();
        assert_ne!(first.command_id, second.command_id);
        let cache = client.export_native_key_cache(1).unwrap();
        drop(client);
        let reopened = Client::open(
            ClientConfig {
                database_path: path,
                vault_id: vault,
                device_id: device,
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        reopened.import_native_key_cache(&cache).unwrap();
        assert_eq!(
            reopened
                .queue_mcp_command(command(grant, "key", "body", route, conversation))
                .unwrap(),
            first
        );
        assert_eq!(reopened.pending_outbox_count().unwrap(), 2);
    }

    #[test]
    fn group_text_queues_once_as_mms_and_replays() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = opened(dir.path().join("mcp.db"), VaultId::new(), DeviceId::new());
        let grant = Uuid::new_v4();
        let mut command = command(grant, "group", "hello", route(), ConversationId::new());
        command.recipients.push("15557654321".into());
        command.expected_transport = Transport::Mms;

        assert_eq!(
            client
                .mcp_send_transport(
                    command.conversation_id,
                    &command.recipients,
                    &command.attachment_ids,
                )
                .unwrap(),
            Transport::Mms
        );
        let first = client.queue_mcp_command(command.clone()).unwrap();
        assert_eq!(client.queue_mcp_command(command).unwrap(), first);
        assert_eq!(client.pending_outbox_count().unwrap(), 1);
    }

    #[test]
    fn discard_keeps_the_attachment_recoverable_when_ciphertext_unlink_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = opened(dir.path().join("mcp.db"), VaultId::new(), DeviceId::new());
        let source = dir.path().join("image.bin");
        std::fs::write(&source, b"prepared image").unwrap();
        let attachment = client
            .prepare_attachment(&source, "image/png", "image.png")
            .unwrap();
        let cipher_path = {
            let store = client.lock().unwrap();
            crate::media::cipher_path(&store.media_dir, attachment.attachment_id)
        };
        std::fs::remove_file(&cipher_path).unwrap();
        std::fs::create_dir(&cipher_path).unwrap();

        assert_eq!(
            client.discard_unreferenced_attachment(attachment.attachment_id),
            Err(Error::Storage)
        );
        assert!(client.attachment_info(attachment.attachment_id).is_ok());

        std::fs::remove_dir(&cipher_path).unwrap();
        assert!(
            client
                .discard_unreferenced_attachment(attachment.attachment_id)
                .unwrap()
        );
    }

    #[test]
    fn discard_delete_failure_leaves_a_non_reusable_cleanup_intent_after_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mcp.db");
        let vault = VaultId::new();
        let device = DeviceId::new();
        let client = opened(path.clone(), vault, device);
        let source = dir.path().join("image.bin");
        std::fs::write(&source, b"prepared image").unwrap();
        let attachment = client
            .prepare_attachment(&source, "image/png", "image.png")
            .unwrap();
        let downloaded = dir.path().join("downloaded.ciphertext");
        let cipher_path = {
            let store = client.lock().unwrap();
            let path = crate::media::cipher_path(&store.media_dir, attachment.attachment_id);
            std::fs::copy(&path, &downloaded).unwrap();
            path
        };
        client
            .mark_attachment_uploaded(attachment.attachment_id, &Uuid::new_v4().to_string())
            .unwrap();
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute_batch(&format!(
                    "CREATE TRIGGER fail_discard_delete BEFORE DELETE ON attachments WHEN OLD.attachment_id='{}' BEGIN SELECT RAISE(ABORT, 'forced delete failure'); END;",
                    attachment.attachment_id
                ))
                .unwrap();
        }
        assert_eq!(
            client.discard_unreferenced_attachment(attachment.attachment_id),
            Err(Error::Database)
        );
        let cache = client.export_native_key_cache(1).unwrap();
        drop(client);

        let client = Client::open(
            ClientConfig {
                database_path: path,
                vault_id: vault,
                device_id: device,
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        client.import_native_key_cache(&cache).unwrap();
        assert_eq!(
            client
                .attachment_info(attachment.attachment_id)
                .unwrap()
                .state,
            AttachmentState::PendingDownload
        );
        assert!(
            client
                .open_native_plaintext(attachment.attachment_id)
                .is_err()
        );
        {
            let store = client.lock().unwrap();
            let inventory: (i64, i64) = store
                .conn
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM attachments WHERE attachment_id=?), (SELECT COUNT(*) FROM attachment_cleanup_intents WHERE attachment_id=?)",
                    params![attachment.attachment_id.to_string(), attachment.attachment_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(inventory, (1, 1));
        }
        assert!(client.pending_downloads().unwrap().is_empty());
        assert!(matches!(
            client.request_attachment_download(attachment.attachment_id),
            Err(Error::InvalidMedia)
        ));
        assert!(matches!(
            client.install_downloaded_attachment(attachment.attachment_id, &downloaded),
            Err(Error::InvalidMedia)
        ));
        assert!(!cipher_path.exists());
        let mut reuse = command(
            Uuid::new_v4(),
            "discarded-attachment",
            "retry",
            route(),
            ConversationId::new(),
        );
        reuse.attachment_ids = vec![attachment.attachment_id];
        reuse.expected_transport = Transport::Mms;
        reuse.recipients.push("15557654321".into());
        assert!(matches!(
            client.queue_mcp_command(reuse),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(client.pending_outbox_count().unwrap(), 0);
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute_batch("DROP TRIGGER fail_discard_delete")
                .unwrap();
        }
        assert!(
            client
                .discard_unreferenced_attachment(attachment.attachment_id)
                .unwrap()
        );
        let store = client.lock().unwrap();
        let inventory: (i64, i64) = store
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM attachments WHERE attachment_id=?), (SELECT COUNT(*) FROM attachment_cleanup_intents WHERE attachment_id=?)",
                params![attachment.attachment_id.to_string(), attachment.attachment_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(inventory, (0, 0));
    }

    #[test]
    fn version_45_reopen_adds_cleanup_intents_without_losing_attachment_or_message() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mcp.db");
        let vault = VaultId::new();
        let device = DeviceId::new();
        let client = opened(path.clone(), vault, device);
        let source = dir.path().join("image.bin");
        std::fs::write(&source, b"prepared image").unwrap();
        let attachment = client
            .prepare_attachment(&source, "image/png", "image.png")
            .unwrap();
        client
            .queue_mcp_command(command(
                Uuid::new_v4(),
                "schema-45",
                "preserve this message",
                route(),
                ConversationId::new(),
            ))
            .unwrap();
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute_batch(
                    "DROP TABLE attachment_cleanup_intents;
                     UPDATE schema_meta SET version=45;",
                )
                .unwrap();
        }
        drop(client);

        let reopened = Client::open(
            ClientConfig {
                database_path: path,
                vault_id: vault,
                device_id: device,
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        let store = reopened.lock().unwrap();
        let state: String = store
            .conn
            .query_row(
                "SELECT state FROM attachments WHERE attachment_id=?",
                params![attachment.attachment_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let inventory: (i64, i64, i64) = store
            .conn
            .query_row(
                "SELECT (SELECT version FROM schema_meta), (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM attachment_cleanup_intents)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, AttachmentState::PendingUpload.code());
        assert_eq!(inventory, (SCHEMA_VERSION, 1, 0));
    }

    #[test]
    fn paused_install_cannot_promote_after_discard_delete_aborts() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = opened(
            directory.path().join("mcp.db"),
            VaultId::new(),
            DeviceId::new(),
        );
        let (attachment_id, cipher_path, downloaded) =
            pending_remote_attachment(&client, directory.path());
        let pause = crate::media::pause_install_before_promotion(attachment_id);
        let installer = client.clone();
        let installing = std::thread::spawn(move || {
            installer.install_downloaded_attachment(attachment_id, &downloaded)
        });
        pause.wait_until_entered();
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute_batch(&format!(
                    "CREATE TRIGGER fail_discard_delete BEFORE DELETE ON attachments WHEN OLD.attachment_id='{}' BEGIN SELECT RAISE(ABORT, 'forced delete failure'); END;",
                    attachment_id
                ))
                .unwrap();
        }
        assert_eq!(
            client.discard_unreferenced_attachment(attachment_id),
            Err(Error::Database)
        );
        pause.resume();
        assert_eq!(installing.join().unwrap(), Err(Error::InvalidMedia));
        assert!(!cipher_path.exists());
    }

    #[test]
    fn paused_install_cannot_publish_an_orphan_after_successful_discard() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = opened(
            directory.path().join("mcp.db"),
            VaultId::new(),
            DeviceId::new(),
        );
        let (attachment_id, cipher_path, downloaded) =
            pending_remote_attachment(&client, directory.path());
        let pause = crate::media::pause_install_before_promotion(attachment_id);
        let installer = client.clone();
        let installing = std::thread::spawn(move || {
            installer.install_downloaded_attachment(attachment_id, &downloaded)
        });
        pause.wait_until_entered();
        assert!(
            client
                .discard_unreferenced_attachment(attachment_id)
                .unwrap()
        );
        pause.resume();
        assert_eq!(installing.join().unwrap(), Err(Error::InvalidMedia));
        assert!(!cipher_path.exists());
        assert_eq!(client.attachment_info(attachment_id), Err(Error::NotFound));
    }

    #[test]
    fn paused_installer_adopts_available_media_without_removing_ciphertext() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = opened(
            directory.path().join("mcp.db"),
            VaultId::new(),
            DeviceId::new(),
        );
        let (attachment_id, cipher_path, downloaded) =
            pending_remote_attachment(&client, directory.path());
        let pause = crate::media::pause_install_before_promotion(attachment_id);
        let installer = client.clone();
        let installing = std::thread::spawn(move || {
            installer.install_downloaded_attachment(attachment_id, &downloaded)
        });
        pause.wait_until_entered();
        assert!(
            client
                .install_downloaded_attachment(
                    attachment_id,
                    &directory.path().join("downloaded.ciphertext")
                )
                .is_ok()
        );
        pause.resume();
        assert_eq!(installing.join().unwrap(), Ok(()));
        assert!(cipher_path.exists());
        assert!(client.open_native_plaintext(attachment_id).is_ok());
    }

    #[test]
    fn one_recipient_text_in_an_mms_thread_is_mms() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = opened(dir.path().join("mcp.db"), VaultId::new(), DeviceId::new());
        let conversation_id = ConversationId::new();
        client
            .capture_incoming_mms(IncomingMms {
                conversation_id: Some(conversation_id),
                sender_address: "15551234567".into(),
                body: "picture message".into(),
                provider_message_id: Some("mms-1".into()),
                imported: false,
                attachment_ids: Vec::new(),
                recipients: vec!["15551234567".into()],
                subject: None,
            })
            .unwrap();

        assert_eq!(
            client
                .mcp_send_transport(conversation_id, &["15551234567".into()], &[],)
                .unwrap(),
            Transport::Mms
        );
    }

    #[test]
    fn reply_recipients_follow_message_direction_and_mms_participants() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = opened(dir.path().join("mcp.db"), VaultId::new(), DeviceId::new());
        let sms = client
            .capture_incoming(IncomingSms {
                conversation_id: Some(ConversationId::new()),
                sender_address: "15551234567".into(),
                body: "SMS".into(),
                provider_message_id: Some("sms-1".into()),
                imported: false,
            })
            .unwrap();
        let sms_recipients = client.reply_recipients(sms.message_id).unwrap();
        assert_eq!(sms_recipients, vec!["15551234567"]);
        assert_eq!(
            client
                .mcp_send_transport(sms.conversation_id, &sms_recipients, &[])
                .unwrap(),
            Transport::Sms
        );
        let mut reaction = command(
            Uuid::new_v4(),
            "sms-reaction",
            "✓",
            route(),
            sms.conversation_id,
        );
        reaction.recipients = sms_recipients;
        let pending_before = client.pending_outbox_count().unwrap();
        let first = client.queue_mcp_command(reaction.clone()).unwrap();
        assert_eq!(client.queue_mcp_command(reaction).unwrap(), first);
        assert_eq!(client.pending_outbox_count().unwrap(), pending_before + 1);

        let acquisition = client
            .begin_mms_acquisition(MmsAcquisitionInput {
                source: MmsSource {
                    source_generation: "test".into(),
                    subscription_id: "sim-1".into(),
                    provider_message_id: "mms-1".into(),
                    provider_thread_id: Some("thread-1".into()),
                },
                direction: Direction::Incoming,
                sender_address: Some("15551234567".into()),
                recipients: vec!["15550000000".into(), "15557654321".into()],
                subject: None,
                body: "MMS".into(),
                imported: false,
                observed_at_ms: 1,
                transaction_id: None,
            })
            .unwrap();
        let mms = client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap();
        client.set_mms_own_address("sim-1", "15550000000").unwrap();
        assert_eq!(
            client.reply_recipients(mms.message_id).unwrap(),
            vec!["15551234567", "15557654321"]
        );
        assert_eq!(
            client
                .mcp_send_transport(
                    mms.conversation_id,
                    &["15551234567".into(), "15557654321".into()],
                    &[],
                )
                .unwrap(),
            Transport::Mms
        );

        let outgoing = client
            .queue_mcp_command(command(
                Uuid::new_v4(),
                "outgoing",
                "outgoing",
                route(),
                ConversationId::new(),
            ))
            .unwrap();
        assert_eq!(
            client.reply_recipients(outgoing.message_id).unwrap(),
            vec!["15551234567"]
        );

        let conversation_id = ConversationId::new();
        let mut first_mms = command(
            Uuid::new_v4(),
            "first-mms",
            "first",
            route(),
            conversation_id,
        );
        first_mms.recipients.push("15557654321".into());
        first_mms.expected_transport = Transport::Mms;
        let first_mms = client.queue_mcp_command(first_mms).unwrap();
        client
            .capture_incoming_mms(IncomingMms {
                conversation_id: Some(conversation_id),
                sender_address: "15551234567".into(),
                body: "later MMS".into(),
                provider_message_id: Some("later-mms".into()),
                imported: false,
                attachment_ids: Vec::new(),
                recipients: vec!["15559876543".into()],
                subject: None,
            })
            .unwrap();
        let first_mms_recipients = client.reply_recipients(first_mms.message_id).unwrap();
        assert_eq!(first_mms_recipients, vec!["15551234567", "15557654321"]);
        assert_eq!(
            client
                .mcp_send_transport(conversation_id, &first_mms_recipients, &[])
                .unwrap(),
            Transport::Mms
        );
    }

    #[test]
    fn replay_returns_receipt_before_rechecking_changed_transport() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = opened(dir.path().join("mcp.db"), VaultId::new(), DeviceId::new());
        let command = command(
            Uuid::new_v4(),
            "key",
            "body",
            route(),
            ConversationId::new(),
        );
        let first = client.queue_mcp_command(command.clone()).unwrap();
        client
            .capture_incoming_mms(IncomingMms {
                conversation_id: Some(command.conversation_id),
                sender_address: "15551234567".into(),
                body: "picture message".into(),
                provider_message_id: Some("mms-1".into()),
                imported: false,
                attachment_ids: Vec::new(),
                recipients: vec!["15551234567".into()],
                subject: None,
            })
            .unwrap();

        assert_eq!(client.queue_mcp_command(command).unwrap(), first);
    }
}
