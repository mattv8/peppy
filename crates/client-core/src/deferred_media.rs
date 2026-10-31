//! Deferred originals use additive encrypted events; the ordinary MMS event remains the legacy
//! fallback. Only the producing gateway may bind an original to a finalized remote object.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeferredMediaStatus {
    Held,
    UploadQueued,
    Available,
    Retryable { reason: &'static str },
}
const MEDIA_REQUEST_RETRY_MS: i64 = 5 * 60 * 1000;
#[derive(Clone, Debug, Default)]
pub struct DeferredMediaRequestReport {
    pub requested: Vec<AttachmentId>,
    pub already_requested: Vec<AttachmentId>,
}

/// No remote binding is claimed until the producer verifies finalization. The descriptor's remote
/// field is absent on the wire, rather than a fabricated URL or object identifier.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredOriginal {
    pub attachment_id: AttachmentId,
    media_type: String,
    display_name: String,
    plaintext_bytes: u64,
    ciphertext_bytes: u64,
    ciphertext_sha256: String,
    stream_version: u8,
    file_key: [u8; 32],
}
impl std::fmt::Debug for DeferredOriginal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeferredOriginal")
            .field("attachment_id", &self.attachment_id)
            .finish_non_exhaustive()
    }
}
impl Drop for DeferredOriginal {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.file_key.zeroize();
    }
}
impl DeferredOriginal {
    fn from_descriptor(d: &MediaDescriptor) -> Self {
        Self {
            attachment_id: d.attachment_id,
            media_type: d.media_type.clone(),
            display_name: d.display_name.clone(),
            plaintext_bytes: d.plaintext_bytes,
            ciphertext_bytes: d.ciphertext_bytes,
            ciphertext_sha256: d.ciphertext_sha256.clone(),
            stream_version: d.stream_version,
            file_key: d.file_key,
        }
    }
    fn descriptor(&self, remote: String) -> MediaDescriptor {
        MediaDescriptor {
            attachment_id: self.attachment_id,
            remote_object_id: remote,
            media_type: self.media_type.clone(),
            display_name: self.display_name.clone(),
            plaintext_bytes: self.plaintext_bytes,
            ciphertext_bytes: self.ciphertext_bytes,
            ciphertext_sha256: self.ciphertext_sha256.clone(),
            stream_version: self.stream_version,
            file_key: self.file_key,
        }
    }
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS mms_deferred_originals(attachment_id TEXT PRIMARY KEY,message_id TEXT NOT NULL,source_device_id TEXT NOT NULL,descriptor BLOB NOT NULL,requested INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS mms_deferred_message ON mms_deferred_originals(message_id);
        CREATE INDEX IF NOT EXISTS mms_deferred_capture_message ON mms_acquisitions(message_id) WHERE state='complete';
        CREATE INDEX IF NOT EXISTS attachments_pending_gateway_upload ON attachments(attachment_id) WHERE state='pending_upload';
        CREATE TABLE IF NOT EXISTS mms_deferred_announced(message_id TEXT PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS mms_deferred_candidates(message_id TEXT PRIMARY KEY,envelope_id TEXT NOT NULL UNIQUE);
        CREATE TABLE IF NOT EXISTS mms_deferred_bindings(attachment_id TEXT PRIMARY KEY,message_id TEXT NOT NULL,source_device_id TEXT NOT NULL,descriptor BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS mms_deferred_requests(attachment_id TEXT PRIMARY KEY,requested_at INTEGER NOT NULL DEFAULT 0,last_error TEXT,failure_sequence INTEGER NOT NULL DEFAULT 0);")?;
    Ok(())
}

impl Client {
    /// Enables durable deferred-MMS publication and records this Android gateway pass's network
    /// facts. This does not grant a carrier execution permit.
    pub fn enable_deferred_gateway_media(
        &self,
        wifi_connected: bool,
        media_wifi_only: bool,
    ) -> Result<(), Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (k, v) in [
            ("deferred_mms_enabled", true),
            ("gateway_media_wifi", wifi_connected),
            ("gateway_media_wifi_only", media_wifi_only),
        ] {
            tx.execute(
                "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v WHERE metadata.v != excluded.v",
                params![k, if v { "1" } else { "0" }],
            )?;
        }
        bootstrap_capture_candidates(&tx, &ctx)?;
        publish_ready_previews(&tx, &ctx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_gateway_media_uploads(&self) -> Result<Vec<CipherObject>, Error> {
        let restricted = {
            let store = self.lock()?;
            flag(&store.conn, "deferred_mms_enabled")?
                && flag(&store.conn, "gateway_media_wifi_only")?
                && !flag(&store.conn, "gateway_media_wifi")?
        };
        if !restricted {
            return self.pending_uploads();
        }
        self.cipher_objects("WITH eligible(id,priority) AS (
            SELECT d.attachment_id,0 FROM mms_deferred_originals d JOIN attachments a ON a.attachment_id=d.attachment_id WHERE d.requested=1 AND a.state='pending_upload' AND NOT EXISTS(SELECT 1 FROM attachment_remote_state rs WHERE rs.attachment_id=a.attachment_id AND rs.removed_by_storage_policy=1)
            UNION ALL SELECT mm.thumbnail_attachment_id,1 FROM message_media_meta mm JOIN messages m ON m.id=mm.message_id JOIN attachments a ON a.attachment_id=mm.thumbnail_attachment_id WHERE a.state='pending_upload' AND NOT EXISTS(SELECT 1 FROM attachment_remote_state rs WHERE rs.attachment_id=a.attachment_id AND rs.removed_by_storage_policy=1) AND COALESCE(json_extract(m.payload,'$.imported'),0)=0)
            SELECT e.id FROM eligible e JOIN attachments a ON a.attachment_id=e.id GROUP BY e.id ORDER BY MIN(e.priority),MIN(a.rowid)")
    }

    /// Wi-Fi restrictions still cover background downloads. An active carrier send or an explicit
    /// rehydration request may fetch its named originals without waiting for Wi-Fi.
    pub fn pending_gateway_media_downloads(&self) -> Result<Vec<CipherObject>, Error> {
        let mut downloads = self.pending_downloads()?;
        let store = self.lock()?;
        if !flag(&store.conn, "deferred_mms_enabled")?
            || !flag(&store.conn, "gateway_media_wifi_only")?
            || flag(&store.conn, "gateway_media_wifi")?
        {
            return Ok(downloads);
        }
        let mut query=store.conn.prepare("SELECT ma.attachment_id FROM command_ledger l JOIN commands c ON c.command_id=l.command_id JOIN message_attachments ma ON ma.message_id=c.message_id LEFT JOIN attempts a ON a.command_id=l.command_id WHERE c.gateway_device_id=? AND l.retired=0 AND l.historical=0 AND (a.command_id IS NULL OR a.state IN ('attempt_recorded','submitted_to_os','outcome_unknown')) UNION SELECT d.attachment_id FROM attachment_demand d JOIN gateway_mms_media_evictions e ON e.attachment_id=d.attachment_id")?;
        let allowed: HashSet<String> = query
            .query_map([store.config.device_id.to_string()], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<_, _>>()?;
        downloads.retain(|object| allowed.contains(&object.attachment_id.to_string()));
        Ok(downloads)
    }

    pub fn deferred_media_status(
        &self,
        id: AttachmentId,
    ) -> Result<Option<DeferredMediaStatus>, Error> {
        self.deferred_media_status_at(id, notifications::now_ms())
    }

    fn deferred_media_status_at(
        &self,
        id: AttachmentId,
        now: i64,
    ) -> Result<Option<DeferredMediaStatus>, Error> {
        let s = self.lock()?;
        let requested: Option<bool> = s
            .conn
            .query_row(
                "SELECT requested FROM mms_deferred_originals WHERE attachment_id=?",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(requested) = requested else {
            return Ok(None);
        };
        let row = attachment_row(&s.conn, id)?.ok_or(Error::Database)?;
        Ok(Some(if row.remote_object_id.is_some() {
            DeferredMediaStatus::Available
        } else if let Some(reason) = requested
            .then(|| retry_reason(&s.conn, id, now))
            .transpose()?
            .flatten()
        {
            DeferredMediaStatus::Retryable { reason }
        } else if requested {
            DeferredMediaStatus::UploadQueued
        } else {
            DeferredMediaStatus::Held
        }))
    }

    pub fn request_mms_media_sync(
        &self,
        ids: &[AttachmentId],
    ) -> Result<DeferredMediaRequestReport, Error> {
        self.request_mms_media_sync_at(ids, notifications::now_ms())
    }

    fn request_mms_media_sync_at(
        &self,
        ids: &[AttachmentId],
        now: i64,
    ) -> Result<DeferredMediaRequestReport, Error> {
        if ids.is_empty() || ids.len() > 32 {
            return Err(Error::InvalidRequest("media sync batch"));
        }
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut report = DeferredMediaRequestReport::default();
        let mut groups: std::collections::BTreeMap<(String, String), Vec<AttachmentId>> =
            std::collections::BTreeMap::new();
        for id in ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            attachment_retention::ensure_remote_available(&tx, id)?;
            let (message,source,requested):(String,String,bool)=tx.query_row("SELECT message_id,source_device_id,requested FROM mms_deferred_originals WHERE attachment_id=?",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(Error::InvalidRequest("unknown deferred media"))?;
            if (requested && retry_reason(&tx, id, now)?.is_none())
                || attachment_row(&tx, id)?.is_some_and(|r| r.remote_object_id.is_some())
            {
                report.already_requested.push(id);
                continue;
            }
            groups.entry((message, source)).or_default().push(id);
            report.requested.push(id);
        }
        for ((message, source), ids) in groups {
            enqueue(
                &tx,
                &ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::MmsMediaDemand {
                    message_id: parse(&message)?,
                    source_gateway_id: parse(&source)?,
                    attachment_ids: ids.clone(),
                },
            )?;
            for id in ids {
                tx.execute("INSERT INTO mms_deferred_requests(attachment_id,requested_at) VALUES(?,?) ON CONFLICT(attachment_id) DO UPDATE SET requested_at=excluded.requested_at,last_error=NULL",params![id.to_string(),now])?;
                tx.execute(
                    "UPDATE mms_deferred_originals SET requested=1 WHERE attachment_id=?",
                    [id.to_string()],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO attachment_demand(attachment_id,requested_at) VALUES(?,?)",
                    params![id.to_string(),now],
                )?;
            }
        }
        tx.commit()?;
        Ok(report)
    }

    /// Records only classified upload failures for this gateway's captured, still-unbound originals.
    pub fn record_gateway_media_failure(
        &self,
        id: AttachmentId,
        reason: &str,
    ) -> Result<bool, Error> {
        if failure_text(reason).is_none() {
            return Err(Error::InvalidRequest("media failure reason"));
        }
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let message:Option<String>=tx.query_row("SELECT d.message_id FROM mms_deferred_originals d JOIN attachments a ON a.attachment_id=d.attachment_id WHERE d.attachment_id=? AND d.source_device_id=? AND d.requested=1 AND a.remote_object_id IS NULL",params![id.to_string(),ctx.device_id.to_string()],|r|r.get(0)).optional()?;
        let Some(message) = message else {
            return Ok(false);
        };
        let previous: Option<String> = tx
            .query_row(
                "SELECT last_error FROM mms_deferred_requests WHERE attachment_id=?",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if previous.as_deref() == Some(reason) {
            return Ok(false);
        }
        let (_, sequence) = enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::MmsMediaFailure {
                message_id: parse(&message)?,
                source_device_id: ctx.device_id,
                attachment_id: id,
                reason: reason.to_owned(),
            },
        )?;
        store_failure(&tx, id, reason, sequence)?;
        tx.commit()?;
        Ok(true)
    }
}

fn failure_text(reason: &str) -> Option<&'static str> {
    match reason {
        "transient" => Some("The last upload failed temporarily. You can try again."),
        "quota" => Some("The last upload reached the vault storage limit."),
        "auth" => Some("The phone could not authorize the last upload."),
        "invalid_media" => Some("The original could not be read on the phone."),
        "permanent" => Some("The server rejected the last upload."),
        _ => None,
    }
}
fn retry_reason(
    conn: &Connection,
    id: AttachmentId,
    now: i64,
) -> Result<Option<&'static str>, Error> {
    let state: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT requested_at,last_error FROM mms_deferred_requests WHERE attachment_id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((_, Some(reason))) = &state {
        return Ok(failure_text(reason));
    }
    let requested_at = state.map_or(0, |s| s.0);
    Ok(
        (now < requested_at || now.saturating_sub(requested_at) >= MEDIA_REQUEST_RETRY_MS)
            .then_some("Still waiting for the phone. You can request sync again."),
    )
}
fn store_failure(
    conn: &Connection,
    id: AttachmentId,
    reason: &str,
    sequence: u64,
) -> Result<(), Error> {
    conn.execute("INSERT INTO mms_deferred_requests(attachment_id,last_error,failure_sequence) VALUES(?,?,?) ON CONFLICT(attachment_id) DO UPDATE SET last_error=excluded.last_error,failure_sequence=excluded.failure_sequence WHERE excluded.failure_sequence>mms_deferred_requests.failure_sequence",params![id.to_string(),reason,to_i64(sequence)?])?;
    Ok(())
}
pub(crate) fn apply_failure(
    conn: &Connection,
    producer: DeviceId,
    source: DeviceId,
    message: MessageId,
    id: AttachmentId,
    reason: &str,
    sequence: u64,
) -> Result<Option<QuarantineReason>, Error> {
    if producer != source || failure_text(reason).is_none() {
        return Ok(Some(QuarantineReason::InvalidPayload));
    }
    let prior: Option<(String, String)> = conn
        .query_row(
            "SELECT message_id,source_device_id FROM mms_deferred_originals WHERE attachment_id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((mid, owner)) = prior {
        if mid != message.to_string() || owner != source.to_string() {
            return Ok(Some(QuarantineReason::PayloadConflict));
        }
        store_failure(conn, id, reason, sequence)?;
    }
    Ok(None)
}

fn flag(conn: &Connection, key: &str) -> Result<bool, Error> {
    Ok(conn
        .query_row("SELECT v='1' FROM metadata WHERE k=?", [key], |r| r.get(0))
        .optional()?
        .unwrap_or(false))
}

pub(crate) fn register_capture(
    conn: &Connection,
    message: MessageId,
    envelope: EnvelopeId,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO mms_deferred_candidates(message_id,envelope_id) VALUES(?,?)",
        params![message.to_string(), envelope.to_string()],
    )?;
    Ok(())
}

/// Upgrade inventory is keyset-paged once. New captures register directly, so idle passes never
/// parse the retained legacy MMS outbox again after their early previews have been announced.
fn bootstrap_capture_candidates(conn: &Connection, ctx: &Ctx) -> Result<(), Error> {
    if flag(conn, "deferred_mms_bootstrap_done")? {
        return Ok(());
    }
    let priority = get_meta(conn, "deferred_mms_bootstrap_priority")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(-1);
    let sequence = get_meta(conn, "deferred_mms_bootstrap_sequence")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let rows = {
        let mut query=conn.prepare("SELECT priority,seq,envelope_id,plain FROM outbox WHERE state='unsealed' AND (priority,seq)>(?,?) ORDER BY priority,seq LIMIT 100")?;
        query
            .query_map(params![priority, sequence], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (_, _, envelope, bytes) in &rows {
        if let Ok(PrivatePayload::MmsMessage { message, .. }) = decode(bytes)
            && !message.imported
            && message.source_device_id == ctx.device_id
        {
            let owned:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM mms_acquisitions WHERE message_id=? AND state='complete') AND NOT EXISTS(SELECT 1 FROM mms_deferred_announced WHERE message_id=?)",params![message.record.message_id.to_string(),message.record.message_id.to_string()],|r|r.get(0))?;
            if owned {
                register_capture(conn, message.record.message_id, parse(envelope)?)?;
            }
        }
    }
    if let Some((p, s, _, _)) = rows.last() {
        set_meta(conn, "deferred_mms_bootstrap_priority", &p.to_string())?;
        set_meta(conn, "deferred_mms_bootstrap_sequence", &s.to_string())?;
    }
    if rows.len() < 100 {
        set_meta(conn, "deferred_mms_bootstrap_done", "1")?;
    }
    Ok(())
}

pub(crate) fn publish_ready_previews(conn: &Connection, ctx: &Ctx) -> Result<(), Error> {
    if !flag(conn, "deferred_mms_enabled")? {
        return Ok(());
    }
    let rows = {
        let mut q=conn.prepare("SELECT json_extract(CAST(o.plain AS TEXT),'$.message') FROM mms_deferred_candidates c JOIN outbox o ON o.envelope_id=c.envelope_id JOIN messages m ON m.id=c.message_id WHERE o.state='unsealed' AND m.source_device_id=? AND EXISTS(SELECT 1 FROM message_attachments ma JOIN attachments a ON a.attachment_id=ma.attachment_id WHERE ma.message_id=m.id AND a.remote_object_id IS NULL) AND NOT EXISTS(SELECT 1 FROM message_media_meta mm JOIN attachments t ON t.attachment_id=mm.thumbnail_attachment_id WHERE mm.message_id=m.id AND t.remote_object_id IS NULL) ORDER BY o.priority,o.seq LIMIT 32")?;
        q.query_map([ctx.device_id.to_string()], |r| {
            Ok(r.get::<_, String>(0)?.into_bytes())
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    for bytes in rows {
        let message: MessagePayload = decode(&bytes)?;
        if message.record.attachments.is_empty() {
            continue;
        }
        let mut originals = Vec::new();
        let mut held = false;
        for a in &message.record.attachments {
            let row = attachment_row(conn, a.attachment_id)?.ok_or(Error::Database)?;
            held |= row.remote_object_id.is_none();
            let d = MediaDescriptor {
                attachment_id: a.attachment_id,
                remote_object_id: String::new(),
                media_type: row.media_type.clone(),
                display_name: row.display_name.clone(),
                plaintext_bytes: row.plaintext_bytes,
                ciphertext_bytes: row.ciphertext_bytes,
                ciphertext_sha256: row.ciphertext_sha256.clone(),
                stream_version: STREAM_VERSION,
                file_key: *row.file_key,
            };
            originals.push(DeferredOriginal::from_descriptor(&d));
        }
        if !held {
            continue;
        }
        let mut metadata = Vec::new();
        let mut ready = true;
        let mut q=conn.prepare("SELECT attachment_id,width,height,duration_ms,thumbnail_attachment_id FROM message_media_meta WHERE message_id=? ORDER BY rowid")?;
        for row in q.query_map([message.record.message_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<u32>>(1)?,
                r.get::<_, Option<u32>>(2)?,
                r.get::<_, Option<u64>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })? {
            let (id, width, height, duration_ms, thumb) = row?;
            let thumbnail = if let Some(id) = thumb {
                let d = descriptor_for_attachment(conn, parse(&id)?)?;
                if d.is_none() {
                    ready = false;
                }
                d
            } else {
                None
            };
            metadata.push(MmsMediaMeta {
                attachment_id: parse(&id)?,
                width,
                height,
                duration_ms,
                thumbnail,
            });
        }
        if !ready {
            continue;
        }
        for o in &originals {
            remember_original(conn, &message, o)?;
        }
        enqueue(
            conn,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::MmsDeferredPreview {
                message: message.clone(),
                originals,
                media_meta: metadata,
            },
        )?;
        conn.execute(
            "INSERT INTO mms_deferred_announced(message_id) VALUES(?)",
            [message.record.message_id.to_string()],
        )?;
        conn.execute(
            "DELETE FROM mms_deferred_candidates WHERE message_id=?",
            [message.record.message_id.to_string()],
        )?;
        for reference in &message.record.attachments {
            if descriptor_for_attachment(conn, reference.attachment_id)?.is_some() {
                publish_binding(conn, ctx, reference.attachment_id)?;
            }
        }
    }
    Ok(())
}

fn remember_original(
    conn: &Connection,
    message: &MessagePayload,
    o: &DeferredOriginal,
) -> Result<(), Error> {
    conn.execute("INSERT OR IGNORE INTO mms_deferred_originals(attachment_id,message_id,source_device_id,descriptor) VALUES(?,?,?,?)",params![o.attachment_id.to_string(),message.record.message_id.to_string(),message.source_device_id.to_string(),json(o)?.into_bytes()])?;
    Ok(())
}

pub(crate) fn on_uploaded(conn: &Connection, ctx: &Ctx, id: AttachmentId) -> Result<(), Error> {
    publish_ready_previews(conn, ctx)?;
    publish_binding(conn, ctx, id)
}

fn publish_binding(conn: &Connection, ctx: &Ctx, id: AttachmentId) -> Result<(), Error> {
    let message:Option<String>=conn.query_row("SELECT message_id FROM mms_deferred_originals WHERE attachment_id=? AND source_device_id=?",params![id.to_string(),ctx.device_id.to_string()],|r|r.get(0)).optional()?;
    if let Some(message) = message {
        let emitted: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM mms_deferred_bindings WHERE attachment_id=?)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if !emitted {
            let d = descriptor_for_attachment(conn, id)?.ok_or(Error::Database)?;
            enqueue(
                conn,
                ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::MmsMediaBinding {
                    message_id: parse(&message)?,
                    source_device_id: ctx.device_id,
                    attachment: d.clone(),
                },
            )?;
            conn.execute("INSERT INTO mms_deferred_bindings(attachment_id,message_id,source_device_id,descriptor) VALUES(?,?,?,?)",params![id.to_string(),message,ctx.device_id.to_string(),json(&d)?.into_bytes()])?;
        }
    }
    Ok(())
}

pub(crate) fn apply_preview(
    conn: &Connection,
    producer: DeviceId,
    message: &MessagePayload,
    originals: &[DeferredOriginal],
    metadata: &[MmsMediaMeta],
    historical: bool,
) -> Result<Option<QuarantineReason>, Error> {
    use QuarantineReason as Q;
    let descriptors: Vec<_> = originals
        .iter()
        .map(|o| o.descriptor(o.attachment_id.to_string()))
        .collect();
    let valid = valid_message(message, producer)
        && message.transport == Transport::Mms
        && !message.imported
        && media_meta_matches(message, metadata)
        && media_matches(message, &descriptors);
    if !valid {
        return Ok(Some(Q::InvalidPayload));
    }
    for o in originals {
        if let Some(row) = attachment_row(conn, o.attachment_id)? {
            if row.ciphertext_sha256 != o.ciphertext_sha256
                || row.ciphertext_bytes != o.ciphertext_bytes
                || row.plaintext_bytes != o.plaintext_bytes
                || row.media_type != o.media_type
                || row.display_name != o.display_name
                || !same_bytes(row.file_key.as_slice(), &o.file_key)
            {
                return Ok(Some(Q::PayloadConflict));
            }
        } else {
            conn.execute("INSERT INTO attachments(attachment_id,state,media_type,display_name,plaintext_bytes,ciphertext_bytes,ciphertext_sha256,stream_version,file_key,remote_object_id) VALUES(?,'pending_download',?,?,?,?,?,?,?,NULL)",params![o.attachment_id.to_string(),o.media_type,o.display_name,to_i64(o.plaintext_bytes)?,to_i64(o.ciphertext_bytes)?,o.ciphertext_sha256,o.stream_version,o.file_key.as_slice()])?;
        }
        let old:Option<(String,String,Vec<u8>)>=conn.query_row("SELECT message_id,source_device_id,descriptor FROM mms_deferred_originals WHERE attachment_id=?",[o.attachment_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((mid, source, bytes)) = old
            && (mid != message.record.message_id.to_string()
                || source != producer.to_string()
                || decode::<DeferredOriginal>(&bytes)? != *o)
        {
            return Ok(Some(Q::PayloadConflict));
        }
        remember_original(conn, message, o)?;
        let binding:Option<(String,String,Vec<u8>)>=conn.query_row("SELECT message_id,source_device_id,descriptor FROM mms_deferred_bindings WHERE attachment_id=?",[o.attachment_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((mid, source, bytes)) = binding {
            if mid != message.record.message_id.to_string() || source != producer.to_string() {
                return Ok(Some(Q::PayloadConflict));
            }
            if let Some(q) = bind_known(conn, &decode(&bytes)?, o)? {
                return Ok(Some(q));
            }
        }
    }
    if !store_remote_media(conn, &thumbnail_descriptors(metadata))? {
        return Ok(Some(Q::PayloadConflict));
    }
    let inserted = insert_message(conn, message)?;
    if inserted == Inserted::Conflict
        || !store_message_media_meta(conn, message.record.message_id, metadata)?
    {
        return Ok(Some(Q::PayloadConflict));
    }
    if inserted == Inserted::New && !historical && message.direction == Direction::Incoming {
        queue_message_banner(conn, message)?;
    }
    Ok(None)
}

fn bind_known(
    conn: &Connection,
    d: &MediaDescriptor,
    o: &DeferredOriginal,
) -> Result<Option<QuarantineReason>, Error> {
    if DeferredOriginal::from_descriptor(d) != *o {
        return Ok(Some(QuarantineReason::PayloadConflict));
    }
    let row = attachment_row(conn, d.attachment_id)?.ok_or(Error::Database)?;
    if row
        .remote_object_id
        .as_ref()
        .is_some_and(|r| r != &d.remote_object_id)
    {
        return Ok(Some(QuarantineReason::PayloadConflict));
    }
    conn.execute("UPDATE attachments SET remote_object_id=? WHERE attachment_id=? AND remote_object_id IS NULL",params![d.remote_object_id,d.attachment_id.to_string()])?;
    Ok(None)
}

pub(crate) fn apply_binding(
    conn: &Connection,
    producer: DeviceId,
    message: MessageId,
    source: DeviceId,
    d: &MediaDescriptor,
) -> Result<Option<QuarantineReason>, Error> {
    use QuarantineReason as Q;
    if producer != source || !d.is_valid() {
        return Ok(Some(Q::InvalidPayload));
    }
    let old:Option<(String,String,Vec<u8>)>=conn.query_row("SELECT message_id,source_device_id,descriptor FROM mms_deferred_bindings WHERE attachment_id=?",[d.attachment_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((mid, src, bytes)) = old
        && (mid != message.to_string()
            || src != source.to_string()
            || decode::<MediaDescriptor>(&bytes)? != *d)
    {
        return Ok(Some(Q::PayloadConflict));
    }
    let original:Option<(String,String,Vec<u8>)>=conn.query_row("SELECT message_id,source_device_id,descriptor FROM mms_deferred_originals WHERE attachment_id=?",[d.attachment_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((mid, src, bytes)) = original {
        if mid != message.to_string() || src != source.to_string() {
            return Ok(Some(Q::PayloadConflict));
        }
        if let Some(q) = bind_known(conn, d, &decode(&bytes)?)? {
            return Ok(Some(q));
        }
    }
    conn.execute("INSERT OR IGNORE INTO mms_deferred_bindings(attachment_id,message_id,source_device_id,descriptor) VALUES(?,?,?,?)",params![d.attachment_id.to_string(),message.to_string(),source.to_string(),json(d)?.into_bytes()])?;
    Ok(None)
}

/// The legacy full message may beat individual binding events. Only its original producer and
/// message identity can release an unbound descriptor; other messages may not borrow that authority.
pub(crate) fn bind_from_message(
    conn: &Connection,
    message: &MessagePayload,
    media: &[MediaDescriptor],
) -> Result<bool, Error> {
    for d in media {
        if attachment_row(conn, d.attachment_id)?.is_some_and(|r| r.remote_object_id.is_none()) {
            let prior:Option<(String,String,Vec<u8>)>=conn.query_row("SELECT message_id,source_device_id,descriptor FROM mms_deferred_originals WHERE attachment_id=?",[d.attachment_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((mid, source, bytes)) = prior else {
                return Ok(false);
            };
            if mid != message.record.message_id.to_string()
                || source != message.source_device_id.to_string()
                || bind_known(conn, d, &decode(&bytes)?)?.is_some()
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub(crate) fn apply_demand(
    conn: &Connection,
    ctx: &Ctx,
    message: MessageId,
    source: DeviceId,
    ids: &[AttachmentId],
    historical: bool,
) -> Result<Option<QuarantineReason>, Error> {
    if ids.is_empty() || ids.len() > 32 {
        return Ok(Some(QuarantineReason::InvalidPayload));
    }
    if historical || ctx.device_id != source {
        return Ok(None);
    }
    for id in ids {
        let owned:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM mms_deferred_originals WHERE attachment_id=? AND message_id=? AND source_device_id=?)",params![id.to_string(),message.to_string(),source.to_string()],|r|r.get(0))?;
        if !owned {
            return Ok(Some(QuarantineReason::InvalidPayload));
        }
    }
    for id in ids {
        conn.execute("INSERT INTO mms_deferred_requests(attachment_id,requested_at) VALUES(?,?) ON CONFLICT(attachment_id) DO UPDATE SET requested_at=excluded.requested_at,last_error=NULL",params![id.to_string(),notifications::now_ms()])?;
        conn.execute(
            "UPDATE mms_deferred_originals SET requested=1 WHERE attachment_id=?",
            [id.to_string()],
        )?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unanswered_request_can_be_retried_without_claiming_phone_offline() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault = VaultId::new();
        let source = DeviceId::new();
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("retry.db"),
                vault_id: vault,
                device_id: source,
            },
            DatabaseKey::new(&[8; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        let file = dir.path().join("image.bin");
        std::fs::write(&file, b"original").unwrap();
        let id = client
            .prepare_attachment(&file, "image/png", "image.png")
            .unwrap()
            .attachment_id;
        {
            let s = client.lock().unwrap();
            let row = attachment_row(&s.conn, id).unwrap().unwrap();
            let o = DeferredOriginal {
                attachment_id: id,
                media_type: row.media_type,
                display_name: row.display_name,
                plaintext_bytes: row.plaintext_bytes,
                ciphertext_bytes: row.ciphertext_bytes,
                ciphertext_sha256: row.ciphertext_sha256,
                stream_version: 1,
                file_key: *row.file_key,
            };
            remember_original(&s.conn, &message(source, &o), &o).unwrap();
        }
        assert_eq!(
            client
                .request_mms_media_sync_at(&[id], 1000)
                .unwrap()
                .requested,
            vec![id]
        );
        assert!(matches!(
            client.deferred_media_status_at(id, 1001).unwrap(),
            Some(DeferredMediaStatus::UploadQueued)
        ));
        assert_eq!(
            client
                .request_mms_media_sync_at(&[id], 1001)
                .unwrap()
                .already_requested,
            vec![id]
        );
        assert!(matches!(
            client
                .deferred_media_status_at(id, 1000 + MEDIA_REQUEST_RETRY_MS)
                .unwrap(),
            Some(DeferredMediaStatus::Retryable { .. })
        ));
        assert_eq!(
            client
                .request_mms_media_sync_at(&[id], 1000 + MEDIA_REQUEST_RETRY_MS)
                .unwrap()
                .requested,
            vec![id]
        );
        assert_eq!(client.pending_outbox().unwrap().len(), 2);
    }

    #[test]
    fn upload_state_and_binding_outbox_commit_atomically() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault = VaultId::new();
        let source = DeviceId::new();
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("atomic.db"),
                vault_id: vault,
                device_id: source,
            },
            DatabaseKey::new(&[8; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        let file = dir.path().join("image.bin");
        std::fs::write(&file, b"original").unwrap();
        let id = client
            .prepare_attachment(&file, "image/png", "image.png")
            .unwrap()
            .attachment_id;
        {
            let s = client.lock().unwrap();
            let row = attachment_row(&s.conn, id).unwrap().unwrap();
            let o = DeferredOriginal {
                attachment_id: id,
                media_type: row.media_type,
                display_name: row.display_name,
                plaintext_bytes: row.plaintext_bytes,
                ciphertext_bytes: row.ciphertext_bytes,
                ciphertext_sha256: row.ciphertext_sha256,
                stream_version: 1,
                file_key: *row.file_key,
            };
            remember_original(&s.conn, &message(source, &o), &o).unwrap();
            s.conn.execute_batch("CREATE TRIGGER reject_binding BEFORE INSERT ON mms_deferred_bindings BEGIN SELECT RAISE(ABORT,'test binding write failure'); END;").unwrap();
        }
        assert!(
            client
                .mark_attachment_uploaded(id, &id.to_string())
                .is_err()
        );
        assert_eq!(
            client.attachment_info(id).unwrap().state,
            AttachmentState::PendingUpload
        );
        assert!(client.pending_outbox().unwrap().is_empty());
        client
            .lock()
            .unwrap()
            .conn
            .execute_batch("DROP TRIGGER reject_binding")
            .unwrap();
        client
            .mark_attachment_uploaded(id, &id.to_string())
            .unwrap();
        assert_eq!(
            client.attachment_info(id).unwrap().state,
            AttachmentState::Uploaded
        );
        assert_eq!(client.pending_outbox().unwrap().len(), 1);
    }

    fn original() -> DeferredOriginal {
        DeferredOriginal {
            attachment_id: AttachmentId::new(),
            media_type: "image/png".into(),
            display_name: "photo.png".into(),
            plaintext_bytes: 10,
            ciphertext_bytes: 40,
            ciphertext_sha256: "ab".repeat(32),
            stream_version: 1,
            file_key: [7; 32],
        }
    }
    fn message(source: DeviceId, original: &DeferredOriginal) -> MessagePayload {
        MessagePayload {
            record: MessageRecord {
                message_id: MessageId::new(),
                conversation_id: ConversationId::new(),
                source_sequence: SourceSequence(1),
                attachments: vec![AttachmentReference {
                    attachment_id: original.attachment_id,
                    pending: true,
                }],
            },
            source_device_id: source,
            provider_message_id: Some("test-provider".into()),
            sender_address: Some("+15555550100".into()),
            recipients: vec![],
            subject: None,
            body: "image".into(),
            transport: Transport::Mms,
            direction: Direction::Incoming,
            imported: false,
            sent_at_ms: Some(1),
            ordered_at_ms: None,
            mms_context: None,
        }
    }
    #[test]
    fn deferred_binding_requires_original_producer_and_immutable_descriptor() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("media.db"),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let source = DeviceId::new();
        let o = original();
        let m = message(source, &o);
        assert_eq!(
            apply_preview(
                conn,
                DeviceId::new(),
                &m,
                std::slice::from_ref(&o),
                &[],
                true
            )
            .unwrap(),
            Some(QuarantineReason::InvalidPayload)
        );
        assert_eq!(
            apply_preview(conn, source, &m, std::slice::from_ref(&o), &[], true).unwrap(),
            None
        );
        let d = o.descriptor(o.attachment_id.to_string());
        assert_eq!(
            apply_binding(conn, DeviceId::new(), m.record.message_id, source, &d).unwrap(),
            Some(QuarantineReason::InvalidPayload)
        );
        let mut changed = d.clone();
        changed.ciphertext_sha256 = "cd".repeat(32);
        assert_eq!(
            apply_binding(conn, source, m.record.message_id, source, &changed).unwrap(),
            Some(QuarantineReason::PayloadConflict)
        );
        assert!(
            attachment_row(conn, o.attachment_id)
                .unwrap()
                .unwrap()
                .remote_object_id
                .is_none()
        );
        assert_eq!(
            apply_binding(conn, source, m.record.message_id, source, &d).unwrap(),
            None
        );
        assert_eq!(
            apply_binding(conn, source, m.record.message_id, source, &d).unwrap(),
            None
        );
        assert_eq!(
            apply_binding(conn, source, MessageId::new(), source, &d).unwrap(),
            Some(QuarantineReason::PayloadConflict)
        );
    }
}
