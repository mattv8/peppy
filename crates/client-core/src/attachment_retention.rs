//! Durable hosted-attachment retention state. Transport stays in the hosts.

use crate::{AttachmentId, DeviceId, Error};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentUseKind {
    MessageHistory,
    PendingSend,
    ContactPhoto,
    RestoreCritical,
}

impl AttachmentUseKind {
    fn code(self) -> &'static str {
        match self {
            Self::MessageHistory => "message_history",
            Self::PendingSend => "pending_send",
            Self::ContactPhoto => "contact_photo",
            Self::RestoreCritical => "restore_critical",
        }
    }

    fn from_code(value: &str) -> Result<Self, Error> {
        match value {
            "message_history" => Ok(Self::MessageHistory),
            "pending_send" => Ok(Self::PendingSend),
            "contact_photo" => Ok(Self::ContactPhoto),
            "restore_critical" => Ok(Self::RestoreCritical),
            _ => Err(Error::Database),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentUseOperationKind {
    Register,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAttachmentUseOperation {
    pub operation: AttachmentUseOperationKind,
    pub use_id: Uuid,
    pub attachment_id: AttachmentId,
    pub device_id: DeviceId,
    pub kind: AttachmentUseKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentUseState {
    Registered,
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentUseResponse {
    pub use_id: Uuid,
    pub attachment_id: AttachmentId,
    pub device_id: DeviceId,
    pub kind: AttachmentUseKind,
    pub state: AttachmentUseState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAttachmentState {
    Available,
    RemovedByStoragePolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentRetentionState {
    pub remote: RemoteAttachmentState,
    pub local_available: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentRetentionClass {
    Protected,
    MessageMedia,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentUploadRetention {
    pub retention_class: AttachmentRetentionClass,
    pub reference_tracking: bool,
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS attachment_retention_uses(
            use_id TEXT PRIMARY KEY,
            attachment_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            registered INTEGER NOT NULL DEFAULT 0,
            release_requested INTEGER NOT NULL DEFAULT 0,
            released INTEGER NOT NULL DEFAULT 0,
            suppressed INTEGER NOT NULL DEFAULT 0,
            reconciliation_required INTEGER NOT NULL DEFAULT 0,
            reconciliation_queued INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS attachment_retention_pending
            ON attachment_retention_uses(registered, released, attachment_id);
        CREATE INDEX IF NOT EXISTS attachment_retention_kind_attachment
            ON attachment_retention_uses(kind,attachment_id,registered,released,suppressed);
        CREATE TABLE IF NOT EXISTS attachment_retention_lifecycle_candidates(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            use_id TEXT NOT NULL UNIQUE,
            due_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS attachment_remote_state(
            attachment_id TEXT PRIMARY KEY,
            removed_by_storage_policy INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS attachment_retention_effects(
            use_id TEXT NOT NULL,
            envelope_id TEXT NOT NULL,
            command_id TEXT,
            attachment_id TEXT NOT NULL DEFAULT '',
            kind TEXT NOT NULL DEFAULT '',
            PRIMARY KEY(use_id, envelope_id)
        );
        CREATE INDEX IF NOT EXISTS attachment_retention_effects_envelope
            ON attachment_retention_effects(envelope_id);
        CREATE TABLE IF NOT EXISTS attachment_retention_requirements(
            envelope_id TEXT NOT NULL,
            attachment_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            command_id TEXT,
            PRIMARY KEY(envelope_id,attachment_id,kind)
        );
        CREATE TABLE IF NOT EXISTS attachment_retention_opted_attachments(
            attachment_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            PRIMARY KEY(attachment_id,kind)
        );
        CREATE TABLE IF NOT EXISTS attachment_upload_retention(
            attachment_id TEXT PRIMARY KEY,
            retention_class TEXT NOT NULL,
            reference_tracking INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS attachment_retention_restores(
            use_id TEXT PRIMARY KEY,
            generation INTEGER NOT NULL,
            state TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS attachment_retention_restore_records(
            generation INTEGER NOT NULL,
            cursor INTEGER NOT NULL,
            PRIMARY KEY(generation,cursor)
        );
        CREATE TABLE IF NOT EXISTS attachment_retention_restore_projections(
            generation INTEGER PRIMARY KEY,
            state TEXT NOT NULL CHECK(state IN ('pending','promoted','failed','superseded','not_required'))
        );
        CREATE TRIGGER IF NOT EXISTS attachment_retention_projection_insert
        AFTER INSERT ON projection_generations
        BEGIN
            INSERT INTO attachment_retention_restore_projections(generation,state)
            VALUES(NEW.generation,CASE WHEN NEW.state IN ('draining','staging') THEN 'pending' ELSE NEW.state END)
            ON CONFLICT(generation) DO UPDATE SET state=excluded.state;
        END;
        CREATE TRIGGER IF NOT EXISTS attachment_retention_projection_update
        AFTER UPDATE OF state ON projection_generations
        BEGIN
            INSERT INTO attachment_retention_restore_projections(generation,state)
            VALUES(NEW.generation,CASE WHEN NEW.state IN ('draining','staging') THEN 'pending' ELSE NEW.state END)
            ON CONFLICT(generation) DO UPDATE SET state=excluded.state;
        END;
        CREATE TABLE IF NOT EXISTS attachment_retention_readiness(
            id INTEGER PRIMARY KEY CHECK(id=1),
            reconciliation_required INTEGER NOT NULL DEFAULT 1
        );
        INSERT OR IGNORE INTO attachment_retention_readiness(id) VALUES(1);
        INSERT OR IGNORE INTO metadata(k,v) VALUES('attachment_retention_protocol_enabled','0');

        CREATE INDEX IF NOT EXISTS attachment_retention_effects_command
            ON attachment_retention_effects(command_id,use_id);
        "#,
    )?;
    let candidate_columns = conn
        .prepare("PRAGMA table_info(attachment_retention_lifecycle_candidates)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !candidate_columns.iter().any(|name| name == "due_at") {
        conn.execute_batch(
            "ALTER TABLE attachment_retention_lifecycle_candidates
             ADD COLUMN due_at INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    let candidates_seeded = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata WHERE k='attachment_retention_candidates_seeded')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !candidates_seeded {
        conn.execute(
            "INSERT INTO attachment_retention_lifecycle_candidates(use_id)
             SELECT use_id FROM attachment_retention_uses
             WHERE registered=1 AND released=0 AND suppressed=0
             ON CONFLICT(use_id) DO NOTHING",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO metadata(k,v) VALUES('attachment_retention_candidates_seeded','1')",
            [],
        )?;
    }
    let has_release_requested = conn
        .prepare("PRAGMA table_info(attachment_retention_uses)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "release_requested");
    if !has_release_requested {
        conn.execute_batch(
            "ALTER TABLE attachment_retention_uses ADD COLUMN release_requested INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    let has_suppressed = conn
        .prepare("PRAGMA table_info(attachment_retention_uses)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "suppressed");
    if !has_suppressed {
        conn.execute_batch(
            "ALTER TABLE attachment_retention_uses ADD COLUMN suppressed INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    let use_columns = conn
        .prepare("PRAGMA table_info(attachment_retention_uses)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !use_columns
        .iter()
        .any(|name| name == "reconciliation_required")
    {
        conn.execute_batch(
            "ALTER TABLE attachment_retention_uses ADD COLUMN reconciliation_required INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    if !use_columns
        .iter()
        .any(|name| name == "reconciliation_queued")
    {
        conn.execute_batch(
            "ALTER TABLE attachment_retention_uses ADD COLUMN reconciliation_queued INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    let effect_columns = conn
        .prepare("PRAGMA table_info(attachment_retention_effects)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !effect_columns.iter().any(|name| name == "attachment_id") {
        conn.execute_batch(
            "ALTER TABLE attachment_retention_effects ADD COLUMN attachment_id TEXT NOT NULL DEFAULT '';",
        )?;
    }
    if !effect_columns.iter().any(|name| name == "kind") {
        conn.execute_batch(
            "ALTER TABLE attachment_retention_effects ADD COLUMN kind TEXT NOT NULL DEFAULT '';",
        )?;
    }
    install_lifecycle_candidate_triggers(conn)?;
    install_work_indexes(conn)?;
    Ok(())
}

const PENDING_REGISTER_INDEX: &str = "attachment_retention_pending_register_work";
const PENDING_RECONCILE_INDEX: &str = "attachment_retention_pending_reconcile_work";
const PENDING_RELEASE_INDEX: &str = "attachment_retention_pending_release_work";
const UNQUEUED_RECONCILIATION_INDEX: &str = "attachment_retention_unqueued_reconciliation_work";
const RECONCILIATION_CANDIDATE_SQL: &str = "SELECT rowid FROM attachment_retention_uses
     WHERE registered=1 AND released=0 AND suppressed=0 AND release_requested=0
       AND reconciliation_required=1 AND reconciliation_queued=0
     ORDER BY use_id LIMIT 1";

fn install_lifecycle_candidate_triggers(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS attachment_retention_effect_candidate;
        DROP TRIGGER IF EXISTS attachment_retention_send_status_candidate_insert;
        DROP TRIGGER IF EXISTS attachment_retention_send_status_candidate_update;
        DROP TRIGGER IF EXISTS attachment_retention_message_attachment_candidate_delete;
        DROP TRIGGER IF EXISTS attachment_retention_message_thumbnail_candidate_update;
        DROP TRIGGER IF EXISTS attachment_retention_message_thumbnail_candidate_delete;
        DROP TRIGGER IF EXISTS attachment_retention_contact_reference_candidate_delete;
        DROP TRIGGER IF EXISTS attachment_retention_contact_reference_candidate_update;
        DROP TRIGGER IF EXISTS attachment_retention_contact_registration_candidate_update;
        DROP TRIGGER IF EXISTS attachment_retention_contact_registration_candidate_delete;
        DROP TRIGGER IF EXISTS attachment_retention_contact_outbox_candidate_update;
        DROP TRIGGER IF EXISTS attachment_retention_restore_candidate;

        CREATE TRIGGER attachment_retention_effect_candidate
        AFTER INSERT ON attachment_retention_effects
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT NEW.use_id FROM attachment_retention_uses
            WHERE use_id=NEW.use_id AND registered=1 AND released=0 AND suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_send_status_candidate_insert
        AFTER INSERT ON send_status
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT u.use_id FROM attachment_retention_effects e
            JOIN attachment_retention_uses u ON u.use_id=e.use_id
            WHERE e.command_id=NEW.command_id AND u.kind='pending_send'
              AND u.registered=1 AND u.released=0 AND u.suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_send_status_candidate_update
        AFTER UPDATE OF state ON send_status
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT u.use_id FROM attachment_retention_effects e
            JOIN attachment_retention_uses u ON u.use_id=e.use_id
            WHERE e.command_id=NEW.command_id AND u.kind='pending_send'
              AND u.registered=1 AND u.released=0 AND u.suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_message_attachment_candidate_delete
        AFTER DELETE ON message_attachments
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT use_id FROM attachment_retention_uses
            WHERE attachment_id=OLD.attachment_id AND kind='message_history'
              AND registered=1 AND released=0 AND suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_message_thumbnail_candidate_update
        AFTER UPDATE OF thumbnail_attachment_id ON message_media_meta
        WHEN OLD.thumbnail_attachment_id IS NOT NULL
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT use_id FROM attachment_retention_uses
            WHERE attachment_id=OLD.thumbnail_attachment_id AND kind='message_history'
              AND registered=1 AND released=0 AND suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_message_thumbnail_candidate_delete
        AFTER DELETE ON message_media_meta
        WHEN OLD.thumbnail_attachment_id IS NOT NULL
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT use_id FROM attachment_retention_uses
            WHERE attachment_id=OLD.thumbnail_attachment_id AND kind='message_history'
              AND registered=1 AND released=0 AND suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_contact_reference_candidate_delete
        AFTER DELETE ON contact_photo_references
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT u.use_id FROM attachment_retention_uses u
            WHERE u.kind='contact_photo' AND u.registered=1 AND u.released=0 AND u.suppressed=0
              AND (u.attachment_id=OLD.attachment_id OR EXISTS(
                SELECT 1 FROM contact_photo_aliases a
                WHERE (a.old_id=u.attachment_id AND a.new_id=OLD.attachment_id)
                   OR (a.new_id=u.attachment_id AND a.old_id=OLD.attachment_id)))
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_contact_reference_candidate_update
        AFTER UPDATE OF attachment_id,retain_until ON contact_photo_references
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT u.use_id FROM attachment_retention_uses u
            WHERE u.kind='contact_photo' AND u.registered=1 AND u.released=0 AND u.suppressed=0
              AND (u.attachment_id=OLD.attachment_id OR EXISTS(
                SELECT 1 FROM contact_photo_aliases a
                WHERE (a.old_id=u.attachment_id AND a.new_id=OLD.attachment_id)
                   OR (a.new_id=u.attachment_id AND a.old_id=OLD.attachment_id)))
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_contact_registration_candidate_update
        AFTER UPDATE OF acknowledged ON contact_photo_registrations
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT u.use_id FROM attachment_retention_uses u
            WHERE u.kind='contact_photo' AND u.registered=1 AND u.released=0 AND u.suppressed=0
              AND (u.attachment_id=NEW.attachment_id OR EXISTS(
                SELECT 1 FROM contact_photo_aliases a
                WHERE (a.old_id=u.attachment_id AND a.new_id=NEW.attachment_id)
                   OR (a.new_id=u.attachment_id AND a.old_id=NEW.attachment_id)))
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_contact_registration_candidate_delete
        AFTER DELETE ON contact_photo_registrations
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT u.use_id FROM attachment_retention_uses u
            WHERE u.kind='contact_photo' AND u.registered=1 AND u.released=0 AND u.suppressed=0
              AND (u.attachment_id=OLD.attachment_id OR EXISTS(
                SELECT 1 FROM contact_photo_aliases a
                WHERE (a.old_id=u.attachment_id AND a.new_id=OLD.attachment_id)
                   OR (a.new_id=u.attachment_id AND a.old_id=OLD.attachment_id)))
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_contact_outbox_candidate_update
        AFTER UPDATE OF state ON outbox
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT u.use_id FROM outbox_contact_photos p
            JOIN attachment_retention_uses u
              ON u.attachment_id=p.attachment_id OR EXISTS(
                SELECT 1 FROM contact_photo_aliases a
                WHERE (a.old_id=u.attachment_id AND a.new_id=p.attachment_id)
                   OR (a.new_id=u.attachment_id AND a.old_id=p.attachment_id))
            WHERE p.outbox_sequence=NEW.seq AND u.kind='contact_photo'
              AND u.registered=1 AND u.released=0 AND u.suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        CREATE TRIGGER attachment_retention_restore_candidate
        AFTER UPDATE OF state ON attachment_retention_restores
        WHEN NEW.state='completed'
        BEGIN
            INSERT INTO attachment_retention_lifecycle_candidates(use_id)
            SELECT NEW.use_id FROM attachment_retention_uses
            WHERE use_id=NEW.use_id AND registered=1 AND released=0 AND suppressed=0
            ON CONFLICT(use_id) DO UPDATE SET due_at=0;
        END;
        "#,
    )?;
    Ok(())
}

fn install_work_indexes(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS {PENDING_REGISTER_INDEX}
             ON attachment_retention_uses(use_id)
             WHERE suppressed=0 AND registered=0;
         CREATE INDEX IF NOT EXISTS {PENDING_RECONCILE_INDEX}
             ON attachment_retention_uses(use_id)
             WHERE suppressed=0 AND registered=1 AND reconciliation_required=1 AND reconciliation_queued=1;
         CREATE INDEX IF NOT EXISTS {PENDING_RELEASE_INDEX}
             ON attachment_retention_uses(use_id)
             WHERE suppressed=0 AND release_requested=1 AND released=0;
         CREATE INDEX IF NOT EXISTS {UNQUEUED_RECONCILIATION_INDEX}
             ON attachment_retention_uses(use_id)
             WHERE registered=1 AND released=0 AND suppressed=0 AND release_requested=0
               AND reconciliation_required=1 AND reconciliation_queued=0;
         CREATE INDEX IF NOT EXISTS attachment_retention_due_lifecycle_candidates
             ON attachment_retention_lifecycle_candidates(due_at,id);"
    ))?;
    Ok(())
}

pub(crate) fn configure_protocol(conn: &Connection, enabled: bool) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES('attachment_retention_protocol_enabled',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        [if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

pub(crate) fn protocol_enabled(conn: &Connection) -> Result<bool, Error> {
    Ok(conn
        .query_row(
            "SELECT v='1' FROM metadata WHERE k='attachment_retention_protocol_enabled'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

fn insert_use(
    conn: &Connection,
    attachment_id: AttachmentId,
    device_id: DeviceId,
    kind: AttachmentUseKind,
) -> Result<Uuid, Error> {
    let use_id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO attachment_retention_uses(use_id,attachment_id,device_id,kind) VALUES(?,?,?,?)",
        params![use_id.to_string(), attachment_id.to_string(), device_id.to_string(), kind.code()],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO attachment_retention_opted_attachments(attachment_id,kind) VALUES(?,?)",
        params![attachment_id.to_string(), kind.code()],
    )?;
    Ok(use_id)
}

pub(crate) fn record_upload_retention(
    conn: &Connection,
    attachment_id: AttachmentId,
    retention_class: AttachmentRetentionClass,
    reference_tracking: bool,
) -> Result<(), Error> {
    if !protocol_enabled(conn)? {
        return Ok(());
    }
    let class = match retention_class {
        AttachmentRetentionClass::Protected => "protected",
        AttachmentRetentionClass::MessageMedia => "message_media",
    };
    conn.execute(
        "INSERT OR IGNORE INTO attachment_upload_retention(attachment_id,retention_class,reference_tracking) VALUES(?,?,?)",
        params![attachment_id.to_string(), class, reference_tracking],
    )?;
    Ok(())
}

fn automatically_tracked(
    conn: &Connection,
    attachment_id: AttachmentId,
    kind: AttachmentUseKind,
) -> Result<bool, Error> {
    let condition = match kind {
        AttachmentUseKind::MessageHistory | AttachmentUseKind::PendingSend => {
            "retention_class='message_media'"
        }
        AttachmentUseKind::ContactPhoto => "reference_tracking=1",
        AttachmentUseKind::RestoreCritical => return Ok(false),
    };
    Ok(conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM attachment_upload_retention WHERE attachment_id=? AND {condition})"),
        [attachment_id.to_string()],
        |row| row.get(0),
    )?)
}

pub(crate) fn ensure_producer_uses(
    conn: &Connection,
    attachment_ids: &[AttachmentId],
    device_id: DeviceId,
    kinds: &[AttachmentUseKind],
) -> Result<(), Error> {
    if !protocol_enabled(conn)? {
        return Ok(());
    }
    for attachment_id in attachment_ids {
        ensure_remote_available(conn, *attachment_id)?;
        for kind in kinds {
            if !automatically_tracked(conn, *attachment_id, *kind)? {
                continue;
            }
            let unassigned: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM attachment_retention_uses u WHERE u.attachment_id=?1 AND u.kind=?2 AND NOT EXISTS(SELECT 1 FROM attachment_retention_effects e WHERE e.use_id=u.use_id) AND NOT EXISTS(SELECT 1 FROM attachment_retention_restores r WHERE r.use_id=u.use_id))",
                params![attachment_id.to_string(), kind.code()],
                |row| row.get(0),
            )?;
            if !unassigned {
                insert_use(conn, *attachment_id, device_id, *kind)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn upload_retention(
    conn: &Connection,
    attachment_id: AttachmentId,
) -> Result<AttachmentUploadRetention, Error> {
    let stored: Option<(String, bool)> = conn
        .query_row(
            "SELECT retention_class,reference_tracking FROM attachment_upload_retention WHERE attachment_id=?",
            [attachment_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((retention_class, reference_tracking)) = stored else {
        let established_contact_tracking: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM contact_photo_attachments WHERE attachment_id=?1)
                 OR EXISTS(SELECT 1 FROM contact_photo_received WHERE attachment_id=?1)",
            [attachment_id.to_string()],
            |row| row.get(0),
        )?;
        return Ok(AttachmentUploadRetention {
            retention_class: AttachmentRetentionClass::Protected,
            reference_tracking: established_contact_tracking,
        });
    };
    Ok(AttachmentUploadRetention {
        retention_class: match retention_class.as_str() {
            "protected" => AttachmentRetentionClass::Protected,
            "message_media" => AttachmentRetentionClass::MessageMedia,
            _ => return Err(Error::Database),
        },
        reference_tracking,
    })
}

/// Persists the required use kind for a producer and binds every matching, unassigned explicit
/// lifecycle. A previously opted-in attachment with no fresh lifecycle remains unsatisfied.
pub(crate) fn associate_effects(
    conn: &Connection,
    attachment_ids: &[AttachmentId],
    kind: AttachmentUseKind,
    envelope_id: &str,
    command_id: Option<&str>,
) -> Result<(), Error> {
    for attachment_id in attachment_ids {
        let id = attachment_id.to_string();
        let opted: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM attachment_retention_opted_attachments m WHERE m.kind=?1 AND (m.attachment_id=?2 OR EXISTS(SELECT 1 FROM contact_photo_aliases a WHERE (a.old_id=m.attachment_id AND a.new_id=?2) OR (a.new_id=m.attachment_id AND a.old_id=?2))))",
            params![kind.code(), id],
            |row| row.get(0),
        )?;
        if !opted {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO attachment_retention_requirements(envelope_id,attachment_id,kind,command_id) VALUES(?,?,?,?)",
            params![envelope_id, id, kind.code(), command_id],
        )?;
        bind_unassigned_uses(conn, envelope_id, &id, kind, command_id)?;
    }
    Ok(())
}

fn bind_unassigned_uses(
    conn: &Connection,
    envelope_id: &str,
    attachment_id: &str,
    kind: AttachmentUseKind,
    command_id: Option<&str>,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO attachment_retention_effects(use_id,envelope_id,command_id,attachment_id,kind)
         SELECT u.use_id,?1,?2,?4,?3 FROM attachment_retention_uses u
         WHERE u.kind=?3
           AND (u.attachment_id=?4 OR EXISTS(SELECT 1 FROM contact_photo_aliases a WHERE (a.old_id=u.attachment_id AND a.new_id=?4) OR (a.new_id=u.attachment_id AND a.old_id=?4)))
           AND NOT EXISTS(SELECT 1 FROM attachment_retention_effects existing WHERE existing.use_id=u.use_id)
           AND NOT EXISTS(SELECT 1 FROM attachment_retention_restores restore WHERE restore.use_id=u.use_id)",
        params![envelope_id, command_id, kind.code(), attachment_id],
    )?;
    Ok(())
}

fn bind_use_to_existing_requirements(
    conn: &Connection,
    use_id: Uuid,
    attachment_id: AttachmentId,
    kind: AttachmentUseKind,
) -> Result<(), Error> {
    let mut requirements = conn.prepare(
        "SELECT envelope_id,attachment_id,command_id FROM attachment_retention_requirements r
         WHERE r.kind=?1
           AND (r.attachment_id=?2 OR EXISTS(SELECT 1 FROM contact_photo_aliases a WHERE (a.old_id=r.attachment_id AND a.new_id=?2) OR (a.new_id=r.attachment_id AND a.old_id=?2)))
           AND NOT EXISTS(SELECT 1 FROM attachment_retention_effects e WHERE e.envelope_id=r.envelope_id)
         ORDER BY r.rowid",
    )?;
    let rows = requirements
        .query_map(params![kind.code(), attachment_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (envelope_id, command_id) in rows {
        conn.execute(
            "INSERT OR IGNORE INTO attachment_retention_effects(use_id,envelope_id,command_id,attachment_id,kind) VALUES(?,?,?,?,?)",
            params![use_id.to_string(), envelope_id, command_id, attachment_id.to_string(), kind.code()],
        )?;
    }
    Ok(())
}

pub(crate) const OUTBOX_BLOCKED_SQL: &str = "EXISTS(SELECT 1 FROM attachment_retention_requirements r WHERE r.envelope_id=outbox.envelope_id AND (NOT EXISTS(SELECT 1 FROM attachment_retention_effects e WHERE e.envelope_id=r.envelope_id AND e.attachment_id=r.attachment_id AND e.kind=r.kind) OR EXISTS(SELECT 1 FROM attachment_retention_effects e JOIN attachment_retention_uses u ON u.use_id=e.use_id WHERE e.envelope_id=r.envelope_id AND e.attachment_id=r.attachment_id AND e.kind=r.kind AND (u.registered=0 OR u.released=1 OR u.suppressed=1 OR u.reconciliation_required=1))))";

pub(crate) fn clone_effects(
    conn: &Connection,
    source_envelope_id: &str,
    envelope_id: &str,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO attachment_retention_requirements(envelope_id,attachment_id,kind,command_id)
         SELECT ?2,attachment_id,kind,command_id FROM attachment_retention_requirements WHERE envelope_id=?1",
        params![source_envelope_id, envelope_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO attachment_retention_effects(use_id,envelope_id,command_id,attachment_id,kind)
         SELECT use_id,?2,command_id,attachment_id,kind FROM attachment_retention_effects WHERE envelope_id=?1",
        params![source_envelope_id, envelope_id],
    )?;
    Ok(())
}

fn bind_gateway_commands(
    conn: &Connection,
    use_id: Uuid,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    let mut commands = conn.prepare(
        "SELECT DISTINCT c.command_id FROM commands c
         JOIN command_ledger l ON l.command_id=c.command_id
         JOIN message_attachments ma ON ma.message_id=c.message_id
         LEFT JOIN attempts a ON a.command_id=c.command_id
         WHERE ma.attachment_id=? AND l.retired=0 AND l.historical=0 AND a.command_id IS NULL",
    )?;
    let command_ids = commands
        .query_map([attachment_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for command_id in command_ids {
        let identity = format!("gateway:{command_id}");
        conn.execute(
            "INSERT OR IGNORE INTO attachment_retention_requirements(envelope_id,attachment_id,kind,command_id) VALUES(?,?,'pending_send',?)",
            params![identity, attachment_id.to_string(), command_id],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO attachment_retention_effects(use_id,envelope_id,command_id,attachment_id,kind) VALUES(?,?,?,?,'pending_send')",
            params![use_id.to_string(), identity, command_id, attachment_id.to_string()],
        )?;
    }
    Ok(())
}

pub(crate) fn protect_gateway_command(
    conn: &Connection,
    device_id: DeviceId,
    command_id: &str,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    let identity = format!("gateway:{command_id}");
    let satisfied: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachment_retention_requirements r JOIN attachment_retention_effects e ON e.envelope_id=r.envelope_id AND e.attachment_id=r.attachment_id AND e.kind=r.kind WHERE r.envelope_id=? AND r.attachment_id=? AND r.kind='pending_send')",
        params![identity, attachment_id.to_string()],
        |row| row.get(0),
    )?;
    if satisfied {
        return Ok(());
    }
    if is_removed(conn, attachment_id)? {
        conn.execute(
            "INSERT OR IGNORE INTO attachment_retention_requirements(envelope_id,attachment_id,kind,command_id) VALUES(?,?,'pending_send',?)",
            params![identity, attachment_id.to_string(), command_id],
        )?;
        return Ok(());
    }
    ensure_producer_uses(
        conn,
        &[attachment_id],
        device_id,
        &[AttachmentUseKind::PendingSend],
    )?;
    associate_effects(
        conn,
        &[attachment_id],
        AttachmentUseKind::PendingSend,
        &identity,
        Some(command_id),
    )
}

pub(crate) fn protect_pending_gateway_commands(
    conn: &Connection,
    device_id: DeviceId,
) -> Result<(), Error> {
    let mut query = conn.prepare(
        "SELECT DISTINCT c.command_id,ma.attachment_id FROM commands c
         JOIN command_ledger l ON l.command_id=c.command_id
         JOIN message_attachments ma ON ma.message_id=c.message_id
         LEFT JOIN attempts a ON a.command_id=c.command_id
         WHERE l.retired=0 AND l.historical=0 AND a.command_id IS NULL
         ORDER BY c.command_id,ma.attachment_id",
    )?;
    let rows = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (command_id, attachment_id) in rows {
        protect_gateway_command(
            conn,
            device_id,
            &command_id,
            attachment_id.parse().map_err(|_| Error::Database)?,
        )?;
    }
    Ok(())
}

pub(crate) fn begin_use(
    conn: &mut Connection,
    attachment_id: AttachmentId,
    device_id: DeviceId,
    kind: AttachmentUseKind,
) -> Result<PendingAttachmentUseOperation, Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let removed: bool = tx.query_row(
        "SELECT COALESCE((SELECT removed_by_storage_policy FROM attachment_remote_state WHERE attachment_id=?),0)",
        [attachment_id.to_string()],
        |row| row.get(0),
    )?;
    if removed {
        return Err(Error::AttachmentRemovedByStoragePolicy);
    }
    let use_id = insert_use(&tx, attachment_id, device_id, kind)?;
    bind_use_to_existing_requirements(&tx, use_id, attachment_id, kind)?;
    if kind == AttachmentUseKind::PendingSend {
        bind_gateway_commands(&tx, use_id, attachment_id)?;
    }
    tx.commit()?;
    Ok(PendingAttachmentUseOperation {
        operation: AttachmentUseOperationKind::Register,
        use_id,
        attachment_id,
        device_id,
        kind,
    })
}

pub(crate) const MAX_PREPARED_OPERATIONS: usize = 256;

fn pending_sql(limited: bool) -> String {
    format!(
        "WITH work(row_id) AS (
           SELECT rowid FROM attachment_retention_uses
           WHERE suppressed=0 AND registered=0
           UNION
           SELECT rowid FROM attachment_retention_uses
           WHERE suppressed=0 AND registered=1 AND reconciliation_required=1 AND reconciliation_queued=1
           UNION
           SELECT rowid FROM attachment_retention_uses
           WHERE suppressed=0 AND release_requested=1 AND released=0
         )
         SELECT u.use_id,u.attachment_id,u.device_id,u.kind,u.registered,u.release_requested
         FROM attachment_retention_uses u
         WHERE u.rowid IN (SELECT row_id FROM work)
         ORDER BY u.rowid{}",
        if limited { " LIMIT ?" } else { "" }
    )
}

fn pending_with_limit(
    conn: &Connection,
    limit: Option<usize>,
) -> Result<Vec<PendingAttachmentUseOperation>, Error> {
    let sql = pending_sql(limit.is_some());
    let mut statement = conn.prepare(&sql)?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, bool>(4)?,
            row.get::<_, bool>(5)?,
        ))
    };
    let rows = if let Some(limit) = limit {
        statement
            .query_map(
                [i64::try_from(limit).map_err(|_| Error::Database)?],
                map_row,
            )?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        statement
            .query_map([], map_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    rows.into_iter()
        .map(
            |(use_id, attachment_id, device_id, kind, registered, release_requested)| {
                Ok(PendingAttachmentUseOperation {
                    operation: if registered && release_requested {
                        AttachmentUseOperationKind::Release
                    } else {
                        AttachmentUseOperationKind::Register
                    },
                    use_id: Uuid::parse_str(&use_id).map_err(|_| Error::Database)?,
                    attachment_id: attachment_id.parse().map_err(|_| Error::Database)?,
                    device_id: device_id.parse().map_err(|_| Error::Database)?,
                    kind: AttachmentUseKind::from_code(&kind)?,
                })
            },
        )
        .collect()
}

pub(crate) fn pending(conn: &Connection) -> Result<Vec<PendingAttachmentUseOperation>, Error> {
    pending_with_limit(conn, None)
}

pub(crate) fn preparation_pending(conn: &Connection) -> Result<bool, Error> {
    conn.query_row(
        "SELECT
           EXISTS(SELECT 1 FROM attachment_retention_uses WHERE suppressed=0 AND registered=0)
           OR EXISTS(SELECT 1 FROM attachment_retention_uses WHERE suppressed=0 AND registered=1 AND reconciliation_required=1 AND reconciliation_queued=1)
           OR EXISTS(SELECT 1 FROM attachment_retention_uses WHERE suppressed=0 AND release_requested=1 AND released=0)
           OR EXISTS(SELECT 1 FROM attachment_retention_uses WHERE registered=1 AND released=0 AND suppressed=0 AND release_requested=0 AND reconciliation_required=1 AND reconciliation_queued=0)
           OR EXISTS(
             SELECT 1 FROM attachment_retention_lifecycle_candidates c
             JOIN attachment_retention_uses u ON u.use_id=c.use_id
             WHERE u.registered=1 AND u.release_requested=0 AND u.released=0 AND u.suppressed=0
               AND c.due_at<=CAST(strftime('%s','now') AS INTEGER)
           )",
        [],
        |row| row.get(0),
    )
    .map_err(Error::from)
}

fn terminal_release_candidates(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(Uuid, AttachmentUseKind)>, Error> {
    let mut statement = conn.prepare(
        "SELECT u.use_id,u.kind
         FROM attachment_retention_lifecycle_candidates c
         JOIN attachment_retention_uses u ON u.use_id=c.use_id
         WHERE u.registered=1 AND u.release_requested=0 AND u.released=0 AND u.suppressed=0
           AND c.due_at<=CAST(strftime('%s','now') AS INTEGER)
         ORDER BY c.id
         LIMIT ?",
    )?;
    statement
        .query_map(
            [i64::try_from(limit).map_err(|_| Error::Database)?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .map(|row| {
            let (use_id, kind) = row?;
            Ok((
                Uuid::parse_str(&use_id).map_err(|_| Error::Database)?,
                AttachmentUseKind::from_code(&kind)?,
            ))
        })
        .collect()
}

fn queue_terminal_releases(conn: &Connection, limit: usize) -> Result<(), Error> {
    for (use_id, kind) in terminal_release_candidates(conn, limit)? {
        conn.execute(
            "DELETE FROM attachment_retention_lifecycle_candidates WHERE use_id=?",
            [use_id.to_string()],
        )?;
        if terminal_evidence(conn, use_id, kind)? {
            conn.execute(
                "UPDATE attachment_retention_uses
                 SET release_requested=1,reconciliation_required=0,reconciliation_queued=0
                 WHERE use_id=? AND registered=1 AND released=0 AND suppressed=0",
                [use_id.to_string()],
            )?;
        } else if kind == AttachmentUseKind::ContactPhoto {
            conn.execute(
                "INSERT INTO attachment_retention_lifecycle_candidates(use_id,due_at)
                 SELECT u.use_id,MIN(r.retain_until) FROM attachment_retention_uses u
                 JOIN contact_photo_references r
                   ON r.attachment_id=u.attachment_id OR EXISTS(
                     SELECT 1 FROM contact_photo_aliases a
                     WHERE (a.old_id=u.attachment_id AND a.new_id=r.attachment_id)
                        OR (a.new_id=u.attachment_id AND a.old_id=r.attachment_id))
                 WHERE u.use_id=? AND r.retain_until>CAST(strftime('%s','now') AS INTEGER)
                 GROUP BY u.use_id
                 ON CONFLICT(use_id) DO UPDATE SET due_at=excluded.due_at",
                [use_id.to_string()],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn prepare(
    conn: &mut Connection,
    limit: usize,
) -> Result<Vec<PendingAttachmentUseOperation>, Error> {
    if limit == 0 || limit > MAX_PREPARED_OPERATIONS {
        return Err(Error::InvalidRequest(
            "attachment retention operation limit",
        ));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let already_pending = pending_with_limit(&tx, Some(limit))?;
    let mut remaining = limit.saturating_sub(already_pending.len());
    if remaining > 0 {
        queue_terminal_releases(&tx, remaining)?;
        remaining = limit.saturating_sub(pending_with_limit(&tx, Some(limit))?.len());
    }
    if remaining > 0 {
        tx.execute(
            &format!(
                "UPDATE attachment_retention_uses SET reconciliation_queued=1
                 WHERE rowid IN ({})",
                RECONCILIATION_CANDIDATE_SQL.replace("LIMIT 1", "LIMIT ?")
            ),
            [i64::try_from(remaining).map_err(|_| Error::Database)?],
        )?;
    }
    let operations = pending_with_limit(&tx, Some(limit))?;
    tx.commit()?;
    Ok(operations)
}

pub(crate) fn apply_response(
    conn: &mut Connection,
    response: &AttachmentUseResponse,
) -> Result<(), Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: Option<(String, String, String, bool)> = tx
        .query_row(
            "SELECT attachment_id,device_id,kind,released FROM attachment_retention_uses WHERE use_id=?",
            [response.use_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((attachment_id, device_id, kind, already_released)) = current else {
        return Err(Error::NotFound);
    };
    if attachment_id != response.attachment_id.to_string()
        || device_id != response.device_id.to_string()
        || kind != response.kind.code()
    {
        return Err(Error::Conflict);
    }
    match response.state {
        AttachmentUseState::Registered if !already_released => {
            tx.execute(
                "UPDATE attachment_retention_uses SET registered=1,reconciliation_required=0,reconciliation_queued=0 WHERE use_id=?",
                [response.use_id.to_string()],
            )?;
            tx.execute(
                "INSERT INTO attachment_retention_lifecycle_candidates(use_id) VALUES(?)
                 ON CONFLICT(use_id) DO UPDATE SET due_at=0",
                [response.use_id.to_string()],
            )?;
        }
        AttachmentUseState::Registered => {}
        AttachmentUseState::Released => {
            tx.execute(
                "UPDATE attachment_retention_uses SET registered=1,release_requested=1,released=1,reconciliation_required=0,reconciliation_queued=0 WHERE use_id=?",
                [response.use_id.to_string()],
            )?;
            tx.execute(
                "DELETE FROM attachment_retention_lifecycle_candidates WHERE use_id=?",
                [response.use_id.to_string()],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn terminal_send_state(code: &str) -> bool {
    matches!(
        code,
        "sent" | "delivered" | "failed_before_submission" | "failed_confirmed"
    )
}

fn terminal_evidence(
    conn: &Connection,
    use_id: Uuid,
    kind: AttachmentUseKind,
) -> Result<bool, Error> {
    match kind {
        AttachmentUseKind::PendingSend => {
            let states = conn
                .prepare(
                    "SELECT s.state FROM attachment_retention_effects e LEFT JOIN commands c ON c.command_id=e.command_id LEFT JOIN send_status s ON s.rowid=(SELECT latest.rowid FROM send_status latest WHERE latest.command_id=e.command_id AND latest.producer_device_id=c.gateway_device_id ORDER BY latest.producer_sequence DESC LIMIT 1) WHERE e.use_id=? AND e.command_id IS NOT NULL",
                )?
                .query_map([use_id.to_string()], |row| row.get::<_, Option<String>>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(!states.is_empty()
                && states
                    .iter()
                    .all(|state| state.as_deref().is_some_and(terminal_send_state)))
        }
        AttachmentUseKind::ContactPhoto => conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM attachment_retention_effects WHERE use_id=?1)
                 AND NOT EXISTS(
                    SELECT 1 FROM attachment_retention_uses u JOIN contact_photo_references r
                    ON r.attachment_id=u.attachment_id OR EXISTS(SELECT 1 FROM contact_photo_aliases a WHERE (a.old_id=u.attachment_id AND a.new_id=r.attachment_id) OR (a.new_id=u.attachment_id AND a.old_id=r.attachment_id))
                    WHERE u.use_id=?1 AND (r.retain_until IS NULL OR r.retain_until>CAST(strftime('%s','now') AS INTEGER))
                 )
                 AND NOT EXISTS(
                    SELECT 1 FROM attachment_retention_uses u JOIN contact_photo_registrations r
                    ON r.attachment_id=u.attachment_id OR EXISTS(SELECT 1 FROM contact_photo_aliases a WHERE (a.old_id=u.attachment_id AND a.new_id=r.attachment_id) OR (a.new_id=u.attachment_id AND a.old_id=r.attachment_id))
                    WHERE u.use_id=?1 AND r.acknowledged=0
                 )
                 AND NOT EXISTS(
                    SELECT 1 FROM attachment_retention_uses u JOIN outbox_contact_photos p
                    ON p.attachment_id=u.attachment_id OR EXISTS(SELECT 1 FROM contact_photo_aliases a WHERE (a.old_id=u.attachment_id AND a.new_id=p.attachment_id) OR (a.new_id=u.attachment_id AND a.old_id=p.attachment_id))
                    JOIN outbox o ON o.seq=p.outbox_sequence
                    WHERE u.use_id=?1 AND o.state!='acknowledged'
                 )",
                [use_id.to_string()],
                |row| row.get(0),
            )
            .map_err(Error::from),
        AttachmentUseKind::MessageHistory => conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM attachment_retention_effects WHERE use_id=?1)
                 AND NOT EXISTS(
                    SELECT 1 FROM attachment_retention_uses u
                    WHERE u.use_id=?1 AND (
                        EXISTS(SELECT 1 FROM message_attachments ma WHERE ma.attachment_id=u.attachment_id)
                        OR EXISTS(SELECT 1 FROM message_media_meta mm WHERE mm.thumbnail_attachment_id=u.attachment_id)
                    )
                 )",
                [use_id.to_string()],
                |row| row.get(0),
            )
            .map_err(Error::from),
        AttachmentUseKind::RestoreCritical => conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM attachment_retention_restores WHERE use_id=? AND state='completed')",
                [use_id.to_string()],
                |row| row.get(0),
            )
            .map_err(Error::from),
    }
}

pub(crate) fn request_release(
    conn: &mut Connection,
    use_id: Uuid,
) -> Result<PendingAttachmentUseOperation, Error> {
    let row: (String, String, String, bool, bool, bool) = conn.query_row(
        "SELECT attachment_id,device_id,kind,registered,released,suppressed FROM attachment_retention_uses WHERE use_id=?",
        [use_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    let kind = AttachmentUseKind::from_code(&row.2)?;
    if !row.3 || row.4 || row.5 || !terminal_evidence(conn, use_id, kind)? {
        return Err(Error::Conflict);
    }
    conn.execute(
        "UPDATE attachment_retention_uses SET release_requested=1,reconciliation_required=0,reconciliation_queued=0 WHERE use_id=?",
        [use_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM attachment_retention_lifecycle_candidates WHERE use_id=?",
        [use_id.to_string()],
    )?;
    Ok(PendingAttachmentUseOperation {
        operation: AttachmentUseOperationKind::Release,
        use_id,
        attachment_id: row.0.parse().map_err(|_| Error::Database)?,
        device_id: row.1.parse().map_err(|_| Error::Database)?,
        kind,
    })
}

pub(crate) fn bind_restore_generation(conn: &Connection, generation: u64) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO attachment_retention_restores(use_id,generation,state)
         SELECT u.use_id,?,'staging' FROM attachment_retention_uses u
         WHERE u.kind='restore_critical'
           AND NOT EXISTS(SELECT 1 FROM attachment_retention_effects e WHERE e.use_id=u.use_id)
           AND NOT EXISTS(SELECT 1 FROM attachment_retention_restores r WHERE r.use_id=u.use_id)",
        [i64::try_from(generation).map_err(|_| Error::Database)?],
    )?;
    Ok(())
}

pub(crate) fn record_restore_cursor(
    conn: &Connection,
    generation: u64,
    cursor: i64,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO attachment_retention_restore_records(generation,cursor)
         SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM attachment_retention_restores WHERE generation=?1)",
        params![
            i64::try_from(generation).map_err(|_| Error::Database)?,
            cursor
        ],
    )?;
    Ok(())
}

pub(crate) fn record_nonprojected_restore_generation(
    conn: &Connection,
    generation: u64,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO attachment_retention_restore_projections(generation,state)
         VALUES(?,'not_required')
         ON CONFLICT(generation) DO UPDATE SET state='not_required'",
        [i64::try_from(generation).map_err(|_| Error::Database)?],
    )?;
    Ok(())
}

pub(crate) fn complete_processed_restores(conn: &Connection) -> Result<(), Error> {
    conn.execute(
        "UPDATE attachment_retention_restores AS r SET state='completed'
         WHERE r.state='staging'
           AND EXISTS(SELECT 1 FROM attachment_retention_restore_records x WHERE x.generation=r.generation)
           AND NOT EXISTS(SELECT 1 FROM snapshot_generations s WHERE s.generation=r.generation)
           AND NOT EXISTS(
             SELECT 1 FROM attachment_retention_restore_records x
             LEFT JOIN journal j ON j.cursor=x.cursor
             WHERE x.generation=r.generation AND (j.cursor IS NULL OR j.status NOT IN ('applied','duplicate'))
           )
           AND EXISTS(
             SELECT 1 FROM attachment_retention_restore_projections p
             WHERE p.generation=r.generation AND p.state IN ('promoted','not_required')
           )",
        [],
    )?;
    Ok(())
}

pub(crate) fn mark_restore_generation(
    conn: &Connection,
    generation: u64,
    state: &'static str,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE attachment_retention_restores SET state=? WHERE generation=?",
        params![
            state,
            i64::try_from(generation).map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

pub(crate) fn is_removed(conn: &Connection, attachment_id: AttachmentId) -> Result<bool, Error> {
    conn.query_row(
        "SELECT COALESCE((SELECT removed_by_storage_policy FROM attachment_remote_state WHERE attachment_id=?),0)",
        [attachment_id.to_string()],
        |row| row.get(0),
    )
    .map_err(Error::from)
}

pub(crate) fn ensure_remote_available(
    conn: &Connection,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    if is_removed(conn, attachment_id)? {
        Err(Error::AttachmentRemovedByStoragePolicy)
    } else {
        Ok(())
    }
}

pub(crate) fn report_removed(conn: &Connection, attachment_id: AttachmentId) -> Result<(), Error> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("INSERT INTO attachment_remote_state(attachment_id,removed_by_storage_policy) VALUES(?,1) ON CONFLICT(attachment_id) DO UPDATE SET removed_by_storage_policy=1", [attachment_id.to_string()])?;
    tx.execute(
        "DELETE FROM attachment_retention_lifecycle_candidates
         WHERE use_id IN (SELECT use_id FROM attachment_retention_uses WHERE attachment_id=?)",
        [attachment_id.to_string()],
    )?;
    tx.execute(
        "UPDATE attachment_retention_uses SET suppressed=1,reconciliation_required=0,reconciliation_queued=0 WHERE attachment_id=? AND released=0",
        [attachment_id.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn state(
    conn: &Connection,
    attachment_id: AttachmentId,
    local_available: bool,
) -> Result<AttachmentRetentionState, Error> {
    let removed: bool = conn.query_row("SELECT COALESCE((SELECT removed_by_storage_policy FROM attachment_remote_state WHERE attachment_id=?),0)", [attachment_id.to_string()], |row| row.get(0))?;
    Ok(AttachmentRetentionState {
        remote: if removed {
            RemoteAttachmentState::RemovedByStoragePolicy
        } else {
            RemoteAttachmentState::Available
        },
        local_available,
    })
}

pub(crate) fn mark_reconciled(conn: &Connection) -> Result<(), Error> {
    let missing: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachment_retention_uses WHERE reconciliation_required=1 AND released=0 AND suppressed=0)",
        [],
        |row| row.get(0),
    )?;
    if missing {
        return Err(Error::Conflict);
    }
    conn.execute(
        "UPDATE attachment_retention_readiness SET reconciliation_required=0 WHERE id=1",
        [],
    )?;
    Ok(())
}

pub(crate) fn require_reconciliation(conn: &Connection) -> Result<(), Error> {
    conn.execute(
        "UPDATE attachment_retention_readiness SET reconciliation_required=1 WHERE id=1",
        [],
    )?;
    conn.execute(
        "UPDATE attachment_retention_uses AS u SET reconciliation_required=1
         WHERE u.registered=1 AND u.released=0 AND u.suppressed=0
           AND EXISTS(
             SELECT 1 FROM attachment_retention_effects e
             WHERE e.use_id=u.use_id AND (
               EXISTS(SELECT 1 FROM outbox o WHERE o.envelope_id=e.envelope_id AND o.state!='acknowledged')
               OR EXISTS(
                 SELECT 1 FROM attachment_retention_requirements r
                 JOIN command_ledger l ON l.command_id=r.command_id
                 WHERE r.envelope_id=e.envelope_id AND r.command_id IS NOT NULL
                   AND l.retired=0 AND l.historical=0
               )
             )
           )",
        [],
    )?;
    Ok(())
}

pub(crate) fn ready(conn: &Connection) -> Result<bool, Error> {
    let reconciliation_required: bool = conn.query_row(
        "SELECT reconciliation_required FROM attachment_retention_readiness WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachment_retention_uses WHERE suppressed=0 AND (registered=0 OR reconciliation_required=1 OR (release_requested=1 AND released=0)))",
        [],
        |row| row.get(0),
    )?;
    Ok(protocol_enabled(conn)? && !reconciliation_required && !pending)
}

#[cfg(test)]
fn explain_plan(conn: &Connection, sql: &str) -> Result<Vec<String>, Error> {
    conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
        .query_map([], |row| row.get(3))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_release_scan_is_driven_only_by_durable_lifecycle_candidates() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE attachment_retention_uses(
                use_id TEXT PRIMARY KEY, attachment_id TEXT NOT NULL, device_id TEXT NOT NULL,
                kind TEXT NOT NULL, registered INTEGER NOT NULL, release_requested INTEGER NOT NULL,
                released INTEGER NOT NULL, suppressed INTEGER NOT NULL
             );
             CREATE TABLE attachment_retention_effects(
                use_id TEXT NOT NULL, envelope_id TEXT NOT NULL, command_id TEXT,
                attachment_id TEXT NOT NULL, kind TEXT NOT NULL
             );
             CREATE TABLE commands(command_id TEXT PRIMARY KEY, gateway_device_id TEXT);
             CREATE TABLE send_status(command_id TEXT, producer_device_id TEXT, producer_sequence INTEGER, state TEXT);
             CREATE TABLE message_attachments(attachment_id TEXT);
             CREATE TABLE message_media_meta(thumbnail_attachment_id TEXT);
             CREATE TABLE contact_photo_references(attachment_id TEXT, retain_until INTEGER);
             CREATE TABLE contact_photo_aliases(old_id TEXT, new_id TEXT);
             CREATE TABLE contact_photo_registrations(attachment_id TEXT, acknowledged INTEGER);
             CREATE TABLE outbox_contact_photos(outbox_sequence INTEGER, attachment_id TEXT);
             CREATE TABLE outbox(seq INTEGER, state TEXT);
             CREATE TABLE attachment_retention_restores(use_id TEXT, state TEXT);
             CREATE TABLE attachment_retention_lifecycle_candidates(
                id INTEGER PRIMARY KEY AUTOINCREMENT, use_id TEXT NOT NULL UNIQUE,
                due_at INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        let first = Uuid::new_v4();
        let hinted = Uuid::new_v4();
        for (use_id, attachment_id) in [(first, "first"), (hinted, "hinted")] {
            conn.execute(
                "INSERT INTO attachment_retention_uses
                 (use_id,attachment_id,device_id,kind,registered,release_requested,released,suppressed)
                 VALUES(?,?,?,'message_history',1,0,0,0)",
                params![use_id.to_string(), attachment_id, Uuid::new_v4().to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO attachment_retention_effects
                 (use_id,envelope_id,command_id,attachment_id,kind)
                 VALUES(?,?,NULL,?,'message_history')",
                params![
                    use_id.to_string(),
                    format!("envelope-{attachment_id}"),
                    attachment_id
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO attachment_retention_lifecycle_candidates(use_id) VALUES(?) ON CONFLICT(use_id) DO NOTHING",
            [hinted.to_string()],
        )
        .unwrap();

        assert_eq!(
            terminal_release_candidates(&conn, 1).unwrap(),
            vec![(hinted, AttachmentUseKind::MessageHistory)]
        );
    }

    #[test]
    fn future_contact_expiry_is_not_due_preparation_work() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE attachment_retention_uses(
                use_id TEXT PRIMARY KEY, attachment_id TEXT NOT NULL, device_id TEXT NOT NULL,
                kind TEXT NOT NULL, registered INTEGER NOT NULL, release_requested INTEGER NOT NULL,
                released INTEGER NOT NULL, suppressed INTEGER NOT NULL,
                reconciliation_required INTEGER NOT NULL, reconciliation_queued INTEGER NOT NULL
             );
             CREATE TABLE attachment_retention_lifecycle_candidates(
                id INTEGER PRIMARY KEY AUTOINCREMENT, use_id TEXT NOT NULL UNIQUE,
                due_at INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE attachment_retention_effects(use_id TEXT);
             CREATE TABLE contact_photo_references(attachment_id TEXT, retain_until INTEGER);
             CREATE TABLE contact_photo_aliases(old_id TEXT, new_id TEXT);
             CREATE TABLE contact_photo_registrations(attachment_id TEXT, acknowledged INTEGER);
             CREATE TABLE outbox_contact_photos(outbox_sequence INTEGER, attachment_id TEXT);
             CREATE TABLE outbox(seq INTEGER, state TEXT);",
        )
        .unwrap();
        let use_id = Uuid::new_v4().to_string();
        let attachment_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO attachment_retention_uses VALUES(?,?,?,'contact_photo',1,0,0,0,0,0)",
            params![use_id, attachment_id, Uuid::new_v4().to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachment_retention_effects(use_id) VALUES(?)",
            [use_id.clone()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contact_photo_references(attachment_id,retain_until)
             VALUES(?,CAST(strftime('%s','now') AS INTEGER)+3600)",
            [attachment_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachment_retention_lifecycle_candidates(use_id) VALUES(?)",
            [use_id.clone()],
        )
        .unwrap();
        queue_terminal_releases(&conn, 1).unwrap();
        let due_at: i64 = conn
            .query_row(
                "SELECT due_at FROM attachment_retention_lifecycle_candidates WHERE use_id=?",
                [use_id.clone()],
                |row| row.get(0),
            )
            .unwrap();
        let now: i64 = conn
            .query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(due_at > now, "finite holder must reschedule at its expiry");
        assert!(!preparation_pending(&conn).unwrap());
        conn.execute(
            "UPDATE attachment_retention_lifecycle_candidates
             SET due_at=CAST(strftime('%s','now') AS INTEGER) WHERE use_id=?",
            [use_id],
        )
        .unwrap();
        assert!(preparation_pending(&conn).unwrap());
    }

    #[test]
    fn inert_history_work_queries_use_partial_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE attachment_retention_uses(
                use_id TEXT PRIMARY KEY, attachment_id TEXT NOT NULL, device_id TEXT NOT NULL,
                kind TEXT NOT NULL, registered INTEGER NOT NULL, release_requested INTEGER NOT NULL,
                released INTEGER NOT NULL, suppressed INTEGER NOT NULL,
                reconciliation_required INTEGER NOT NULL, reconciliation_queued INTEGER NOT NULL
             );
             CREATE TABLE attachment_retention_lifecycle_candidates(
                id INTEGER PRIMARY KEY AUTOINCREMENT, use_id TEXT NOT NULL UNIQUE,
                due_at INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        install_work_indexes(&conn).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        for index in 0..10_000 {
            tx.execute(
                "INSERT INTO attachment_retention_uses VALUES(?,?,?,'message_history',1,0,0,0,0,0)",
                params![
                    Uuid::new_v4().to_string(),
                    format!("attachment-{index}"),
                    Uuid::new_v4().to_string()
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let pending_plan = explain_plan(&conn, &pending_sql(false)).unwrap();
        assert!(
            pending_plan
                .iter()
                .any(|line| line.contains(PENDING_REGISTER_INDEX))
        );
        assert!(
            pending_plan
                .iter()
                .any(|line| line.contains(PENDING_RECONCILE_INDEX))
        );
        assert!(
            pending_plan
                .iter()
                .any(|line| line.contains(PENDING_RELEASE_INDEX))
        );
        assert!(
            pending_plan.iter().all(|line| {
                !line.contains("SCAN attachment_retention_uses")
                    || line.contains("USING INDEX")
                    || line.contains("USING COVERING INDEX")
            }),
            "{pending_plan:?}"
        );

        let reconcile_plan = explain_plan(&conn, RECONCILIATION_CANDIDATE_SQL).unwrap();
        assert!(
            reconcile_plan
                .iter()
                .any(|line| line.contains(UNQUEUED_RECONCILIATION_INDEX)),
            "{reconcile_plan:?}"
        );
    }

    #[test]
    fn lifecycle_trigger_wakes_deferred_contact_candidate_and_preserves_uuid() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE attachment_retention_uses(
                use_id TEXT PRIMARY KEY, attachment_id TEXT NOT NULL, device_id TEXT NOT NULL,
                kind TEXT NOT NULL, registered INTEGER NOT NULL, release_requested INTEGER NOT NULL,
                released INTEGER NOT NULL, suppressed INTEGER NOT NULL,
                reconciliation_required INTEGER NOT NULL, reconciliation_queued INTEGER NOT NULL
             );
             CREATE TABLE attachment_retention_lifecycle_candidates(
                id INTEGER PRIMARY KEY AUTOINCREMENT, use_id TEXT NOT NULL UNIQUE,
                due_at INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE attachment_retention_effects(
                use_id TEXT, envelope_id TEXT, command_id TEXT, attachment_id TEXT, kind TEXT
             );
             CREATE TABLE send_status(command_id TEXT, state TEXT);
             CREATE TABLE message_attachments(attachment_id TEXT);
             CREATE TABLE message_media_meta(thumbnail_attachment_id TEXT);
             CREATE TABLE contact_photo_references(attachment_id TEXT, retain_until INTEGER);
             CREATE TABLE contact_photo_aliases(old_id TEXT, new_id TEXT);
             CREATE TABLE contact_photo_registrations(attachment_id TEXT, acknowledged INTEGER);
             CREATE TABLE outbox_contact_photos(outbox_sequence INTEGER, attachment_id TEXT);
             CREATE TABLE outbox(seq INTEGER, state TEXT);
             CREATE TABLE attachment_retention_restores(use_id TEXT, state TEXT);
             CREATE TRIGGER attachment_retention_contact_reference_candidate_delete
             AFTER DELETE ON contact_photo_references BEGIN SELECT 1; END;",
        )
        .unwrap();
        install_lifecycle_candidate_triggers(&conn).unwrap();
        install_work_indexes(&conn).unwrap();

        let use_id = Uuid::new_v4();
        let attachment_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO attachment_retention_uses VALUES(?,?,?,'contact_photo',1,0,0,0,0,0)",
            params![
                use_id.to_string(),
                attachment_id,
                Uuid::new_v4().to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachment_retention_effects
             (use_id,envelope_id,command_id,attachment_id,kind)
             VALUES(?,'envelope',NULL,?,'contact_photo')",
            params![use_id.to_string(), attachment_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contact_photo_references(attachment_id,retain_until)
             VALUES(?,CAST(strftime('%s','now') AS INTEGER)+3600)",
            [attachment_id.clone()],
        )
        .unwrap();
        conn.execute(
            "UPDATE attachment_retention_lifecycle_candidates
             SET due_at=CAST(strftime('%s','now') AS INTEGER)+3600 WHERE use_id=?",
            [use_id.to_string()],
        )
        .unwrap();
        assert!(!preparation_pending(&conn).unwrap());

        conn.execute(
            "DELETE FROM contact_photo_references WHERE attachment_id=?",
            [attachment_id],
        )
        .unwrap();
        assert!(
            preparation_pending(&conn).unwrap(),
            "a real holder deletion must wake the deferred candidate immediately"
        );
        let operations = prepare(&mut conn, 1).unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].use_id, use_id);
        assert_eq!(operations[0].operation, AttachmentUseOperationKind::Release);
    }
}
