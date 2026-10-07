use super::*;
use peppy_client_core::{
    Cursor, EnvelopeId, IngestResult, RawSnapshotRecord, SnapshotProgress, SnapshotPurpose,
};

const MAX_BATCH: usize = 1_000;
const MAX_SNAPSHOT_PAGE: usize = 500;

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "receive_cursor"
            | "apply_pending"
            | "pending_outbox"
            | "ack_outbox"
            | "ingest"
            | "key_status"
            | "activate_epoch"
            | "seal_pending"
            | "set_server_compaction_support"
            | "compaction_status"
            | "sync_work_status"
            | "compaction_backfill_step"
            | "begin_snapshot"
            | "snapshot_progress"
            | "append_snapshot_page"
            | "finish_snapshot"
            | "_worker_local_attachment_ids"
    )
}

impl BrowserCore {
    pub(super) fn sync_command(&self, command: &str, args: Value) -> Result<Value, Failure> {
        match command {
            "receive_cursor" => self.receive_cursor(),
            "apply_pending" => self.apply_pending(args),
            "pending_outbox" => self.pending_outbox(args),
            "ack_outbox" => self.ack_outbox(args),
            "ingest" => self.ingest(args),
            "key_status" => self.key_status(),
            "activate_epoch" => self.activate_epoch(args),
            "seal_pending" => self.seal_pending(args),
            "set_server_compaction_support" => self.set_server_compaction_support(args),
            "compaction_status" => self.compaction_status(),
            "sync_work_status" => self.sync_work_status(),
            "compaction_backfill_step" => self.compaction_backfill_step(args),
            "begin_snapshot" => self.begin_snapshot(args),
            "snapshot_progress" => self.snapshot_progress(),
            "append_snapshot_page" => self.append_snapshot_page(args),
            "finish_snapshot" => self.finish_snapshot(args),
            "_worker_local_attachment_ids" => {
                let after = args
                    .get("after")
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(invalid)?
                            .parse::<AttachmentId>()
                            .map_err(|_| invalid())
                    })
                    .transpose()?;
                let ids = self
                    .client()?
                    .local_attachment_ids(after, batch_limit(&args)?)
                    .map_err(core)?;
                Ok(
                    json!({"attachmentIds": ids.iter().map(ToString::to_string).collect::<Vec<_>>() }),
                )
            }
            _ => Err(unknown_command()),
        }
    }

    fn receive_cursor(&self) -> Result<Value, Failure> {
        Ok(json!({"cursor": self.client()?.receive_cursor().map_err(core)?.0.to_string()}))
    }

    fn apply_pending(&self, args: Value) -> Result<Value, Failure> {
        let report = self
            .client()?
            .apply_pending(batch_limit(&args)?)
            .map_err(core)?;
        Ok(
            json!({"applied": report.applied, "quarantined": report.quarantined, "waitingForKeys": report.waiting_for_keys, "drained": report.drained, "snapshotRemaining": report.snapshot_remaining}),
        )
    }

    fn pending_outbox(&self, args: Value) -> Result<Value, Failure> {
        let envelopes = self
            .client()?
            .pending_outbox_batch(batch_limit(&args)?)
            .map_err(core)?;
        Ok(json!({"envelopes": envelopes}))
    }

    fn ack_outbox(&self, args: Value) -> Result<Value, Failure> {
        let envelope_id = required_string(&args, "envelopeId")?
            .parse::<EnvelopeId>()
            .map_err(|_| invalid())?;
        self.client()?.ack_outbox(envelope_id).map_err(core)?;
        Ok(json!({}))
    }

    fn ingest(&self, args: Value) -> Result<Value, Failure> {
        let cursor = parse_cursor(required_string(&args, "cursor")?)?;
        let envelope = args.get("envelope").ok_or_else(invalid)?;
        let envelope_json = serde_json::to_vec(envelope).map_err(|_| invalid())?;
        let result = self
            .client()?
            .ingest_raw(&envelope_json, cursor)
            .map_err(core)?;
        Ok(ingest_result_view(result))
    }

    fn key_status(&self) -> Result<Value, Failure> {
        let status = self.client()?.key_status().map_err(core)?;
        Ok(
            json!({"activeEpoch": status.active_epoch.map(|epoch| epoch.to_string()), "unlockedEpochs": status.unlocked_epochs.into_iter().map(|epoch| epoch.to_string()).collect::<Vec<_>>() }),
        )
    }

    fn activate_epoch(&self, args: Value) -> Result<Value, Failure> {
        let epoch = parse_u32(required_string(&args, "epoch")?)?;
        self.client()?.activate_epoch(epoch).map_err(core)?;
        Ok(json!({}))
    }

    fn seal_pending(&self, args: Value) -> Result<Value, Failure> {
        let sealed = self
            .client()?
            .seal_pending_batch(batch_limit(&args)?)
            .map_err(core)?;
        Ok(json!({"sealed": sealed}))
    }

    fn set_server_compaction_support(&self, args: Value) -> Result<Value, Failure> {
        let supported = args
            .get("supported")
            .and_then(Value::as_bool)
            .ok_or_else(invalid)?;
        let active = args.get("active").and_then(Value::as_bool).unwrap_or(false);
        json_result(
            self.client()?
                .set_server_compaction_state(supported, active)
                .map_err(core)?,
        )
    }

    fn compaction_backfill_step(&self, args: Value) -> Result<Value, Failure> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|limit| u32::try_from(limit).ok())
            .filter(|limit| *limit > 0)
            .ok_or_else(invalid)?;
        json_result(
            self.client()?
                .compaction_backfill_step_json(limit)
                .map_err(core)?,
        )
    }

    fn compaction_status(&self) -> Result<Value, Failure> {
        json_result(self.client()?.contact_sync_readiness_json().map_err(core)?)
    }

    fn sync_work_status(&self) -> Result<Value, Failure> {
        let status = self.client()?.sync_work_status().map_err(core)?;
        Ok(json!({
            "pendingSeal": status.pending_seal,
            "pendingApply": status.pending_apply,
            "pendingSnapshot": status.pending_snapshot,
        }))
    }

    fn begin_snapshot(&self, args: Value) -> Result<Value, Failure> {
        let high_water = parse_cursor(required_string(&args, "highWater")?)?;
        let record_count = parse_u64(required_string(&args, "recordCount")?)?;
        let purpose = match required_string(&args, "purpose")? {
            "bootstrap" => SnapshotPurpose::Resync,
            "restore" => SnapshotPurpose::Restore,
            _ => return Err(invalid()),
        };
        let generation = args
            .get("serverCompactionGeneration")
            .map(|value| parse_u64(value.as_str().ok_or_else(invalid)?))
            .transpose()?;
        let progress = self
            .client()?
            .begin_snapshot_with_compaction(high_water, record_count, purpose, generation)
            .map_err(core)?;
        Ok(snapshot_progress_view(progress))
    }

    fn snapshot_progress(&self) -> Result<Value, Failure> {
        Ok(
            json!({"progress": self.client()?.snapshot_progress().map_err(core)?.map(snapshot_progress_view)}),
        )
    }

    fn append_snapshot_page(&self, args: Value) -> Result<Value, Failure> {
        let generation = parse_u64(required_string(&args, "generation")?)?;
        let records = args
            .get("records")
            .and_then(Value::as_array)
            .filter(|records| !records.is_empty() && records.len() <= MAX_SNAPSHOT_PAGE)
            .ok_or_else(invalid)?;
        let records = records
            .iter()
            .map(|record| {
                let cursor = parse_cursor(required_string(record, "cursor")?)?;
                let envelope = record.get("envelope").ok_or_else(invalid)?;
                Ok(RawSnapshotRecord {
                    cursor,
                    envelope_json: serde_json::to_vec(envelope).map_err(|_| invalid())?,
                })
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        let progress = self
            .client()?
            .append_snapshot_raw_page(generation, &records)
            .map_err(core)?;
        Ok(snapshot_progress_view(progress))
    }

    fn finish_snapshot(&self, args: Value) -> Result<Value, Failure> {
        let generation = parse_u64(required_string(&args, "generation")?)?;
        let report = self.client()?.finish_snapshot(generation).map_err(core)?;
        Ok(
            json!({"journaled": report.journaled.to_string(), "duplicate": report.duplicate.to_string(), "quarantined": report.quarantined.to_string(), "receiveCursor": report.receive_cursor.to_string()}),
        )
    }
}

fn batch_limit(args: &Value) -> Result<usize, Failure> {
    let Some(limit) = args.get("limit") else {
        return Ok(MAX_BATCH);
    };
    let limit = limit.as_u64().ok_or_else(invalid)?;
    usize::try_from(limit)
        .ok()
        .filter(|limit| *limit <= MAX_BATCH)
        .ok_or_else(invalid)
}

fn required_string<'a>(args: &'a Value, name: &str) -> Result<&'a str, Failure> {
    args.get(name).and_then(Value::as_str).ok_or_else(invalid)
}

fn parse_cursor(value: &str) -> Result<Cursor, Failure> {
    Ok(Cursor(parse_u64(value)?))
}

fn parse_u32(value: &str) -> Result<u32, Failure> {
    canonical_decimal(value)?.parse().map_err(|_| invalid())
}

fn parse_u64(value: &str) -> Result<u64, Failure> {
    canonical_decimal(value)?.parse().map_err(|_| invalid())
}

fn canonical_decimal(value: &str) -> Result<&str, Failure> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    Ok(value)
}

fn json_result(value: String) -> Result<Value, Failure> {
    serde_json::from_str(&value).map_err(|_| core(peppy_client_core::Error::Database))
}

fn ingest_result_view(result: IngestResult) -> Value {
    match result {
        IngestResult::Journaled => json!({"state": "journaled"}),
        IngestResult::Duplicate => json!({"state": "duplicate"}),
        IngestResult::Quarantined(reason) => {
            json!({"state": "quarantined", "quarantineReason": format!("{reason:?}")})
        }
    }
}

fn snapshot_progress_view(progress: SnapshotProgress) -> Value {
    json!({
        "generation": progress.generation.to_string(),
        "highWater": progress.high_water.0.to_string(),
        "expectedRecords": progress.expected_records.to_string(),
        "receivedRecords": progress.received_records.to_string(),
        "lastCursor": progress.last_cursor.0.to_string(),
        "serverCompactionGeneration": progress.server_compaction_generation.map(|value| value.to_string(),),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    fn open_core() -> BrowserCore {
        let root = tempfile::tempdir().unwrap().keep();
        let vault = peppy_client_core::VaultId::new();
        let device = peppy_client_core::DeviceId::new();
        let client = Client::open(
            ClientConfig {
                database_path: root.join("client.db"),
                vault_id: vault,
                device_id: device,
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        let mut core = BrowserCore::new(root);
        core.client = Some(client);
        core
    }

    fn response(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
            .unwrap()
    }

    #[test]
    fn rejects_lossy_sync_numbers_and_oversized_batches() {
        let mut core = open_core();
        for cursor in [json!(1), json!("01"), json!("18446744073709551616")] {
            let result = response(
                &mut core,
                "ingest",
                json!({"cursor": cursor, "envelope": {}}),
            );
            assert_eq!(result["error"]["code"], "invalid-request");
        }
        let result = response(&mut core, "pending_outbox", json!({"limit": 1001}));
        assert_eq!(result["error"]["code"], "invalid-request");
    }

    #[test]
    fn raw_malformed_records_quarantine_without_blocking_contiguous_cursor() {
        let mut core = open_core();
        assert_eq!(
            response(&mut core, "ingest", json!({"cursor": "2", "envelope": {}}))["value"]["state"],
            "quarantined"
        );
        assert_eq!(
            response(&mut core, "receive_cursor", json!({}))["value"]["cursor"],
            "0"
        );
        assert_eq!(
            response(&mut core, "ingest", json!({"cursor": "1", "envelope": {}}))["value"]["state"],
            "quarantined"
        );
        assert_eq!(
            response(&mut core, "receive_cursor", json!({}))["value"]["cursor"],
            "2"
        );
    }

    #[test]
    fn stages_raw_snapshot_records_and_preserves_restore_guard_on_rejection() {
        let mut core = open_core();
        let started = response(
            &mut core,
            "begin_snapshot",
            json!({"highWater": "1", "recordCount": "1", "purpose": "bootstrap"}),
        );
        assert_eq!(started["value"]["generation"], "1");
        let appended = response(
            &mut core,
            "append_snapshot_page",
            json!({"generation": "1", "records": [{"cursor": "1", "envelope": {}}]}),
        );
        assert_eq!(appended["value"]["receivedRecords"], "1");
        assert_eq!(
            response(&mut core, "finish_snapshot", json!({"generation": "1"}))["ok"],
            true
        );
        assert_eq!(
            response(
                &mut core,
                "append_snapshot_page",
                json!({"generation": "1", "records": [{"cursor": "1", "envelope": {}}]})
            )["error"]["code"],
            "core"
        );

        let rejected = response(
            &mut core,
            "begin_snapshot",
            json!({"highWater": "0", "recordCount": "1", "purpose": "restore"}),
        );
        assert_eq!(rejected["error"]["code"], "core");
        assert!(core.client().unwrap().restore_guarded().unwrap());
    }

    #[test]
    fn sealed_outbox_retries_are_stable_until_acknowledged() {
        let root = tempfile::tempdir().unwrap().keep();
        let vault = peppy_client_core::VaultId::new();
        let device = peppy_client_core::DeviceId::new();
        let mut core = BrowserCore::new(root);
        assert_eq!(
            response(
                &mut core,
                "open",
                json!({"vaultId": vault, "deviceId": device, "databaseKey": vec![7; 32], "origin": "https://peppy.test/", "deviceRole": "device"})
            )["ok"],
            true
        );
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        assert_eq!(
            response(
                &mut core,
                "unlock",
                json!({"profile": profile, "header": header, "passphrase": "passphrase"})
            )["ok"],
            true
        );
        assert_eq!(
            response(
                &mut core,
                "set_host_context",
                json!({"connection": "connected", "origin": "https://peppy.test/", "deviceRole": "device", "gateways": [{"id": device, "name": "Gateway", "simId": "sim-1", "online": true, "simulated": true, "supportsSms": true, "supportsMms": true}]})
            )["ok"],
            true
        );
        let draft = response(
            &mut core,
            "save_draft",
            json!({"id": "new", "conversationId": "", "text": "hello", "recipientIds": ["+15555550100"], "attachmentIds": [], "gatewayId": device, "simId": "sim-1", "expectedRevision": "0"}),
        );
        assert_eq!(
            response(
                &mut core,
                "send_draft",
                json!({"id": draft["value"]["id"], "conversationId": draft["value"]["conversationId"], "text": draft["value"]["text"], "recipientIds": draft["value"]["recipientIds"], "attachmentIds": draft["value"]["attachmentIds"], "gatewayId": device, "simId": "sim-1", "expectedRevision": draft["value"]["revision"]})
            )["ok"],
            true
        );
        let first = response(&mut core, "pending_outbox", json!({"limit": 1}));
        let retry = response(&mut core, "pending_outbox", json!({"limit": 1}));
        assert_eq!(first["value"]["envelopes"], retry["value"]["envelopes"]);
        let envelope_id = first["value"]["envelopes"][0]["envelope_id"].clone();
        assert_eq!(
            response(&mut core, "ack_outbox", json!({"envelopeId": envelope_id}))["ok"],
            true
        );
        assert!(
            response(&mut core, "pending_outbox", json!({"limit": 1}))["value"]["envelopes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn compaction_adapter_records_server_state_and_exposes_bounded_backfill() {
        let mut core = open_core();
        let state = response(
            &mut core,
            "set_server_compaction_support",
            json!({"supported": true, "active": true}),
        );
        assert_eq!(state["ok"], true);
        assert!(response(&mut core, "compaction_status", json!({}))["value"].is_object());
        assert_eq!(
            response(&mut core, "sync_work_status", json!({}))["value"]["pendingApply"],
            false
        );
        let step = response(&mut core, "compaction_backfill_step", json!({"limit": 1}));
        assert_eq!(step["ok"], true);
        assert!(step["value"].get("backfill_complete").is_some());
        assert_eq!(
            response(&mut core, "compaction_backfill_step", json!({"limit": 0}))["error"]["code"],
            "invalid-request"
        );
    }
}
