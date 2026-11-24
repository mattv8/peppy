use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::Transaction;

type ExistingNotification = (
    String,
    String,
    bool,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    bool,
    Option<String>,
    bool,
);

/// App name, icon, content version, dismissible and mark-read capability of a live notification.
type LiveNotification = (String, Option<String>, i64, bool, bool);

const MAX_NOTIFICATION_ICON_BYTES: usize = 8_192;
const MAX_NOTIFICATION_ICON_BASE64_BYTES: usize = MAX_NOTIFICATION_ICON_BYTES.div_ceil(3) * 4;
const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const PNG_IHDR: [u8; 8] = [0, 0, 0, 13, 73, 72, 68, 82];
const PNG_32PX_DIMENSIONS: [u8; 8] = [0, 0, 0, 32, 0, 0, 0, 32];

fn valid_icon(icon: Option<&str>) -> bool {
    let Some(icon) = icon else {
        return true;
    };
    if icon.len() > MAX_NOTIFICATION_ICON_BASE64_BYTES {
        return false;
    }
    let Ok(bytes) = STANDARD.decode(icon) else {
        return false;
    };
    bytes.len() <= MAX_NOTIFICATION_ICON_BYTES
        && bytes.len() >= 24
        && bytes[..8] == PNG_SIGNATURE
        && bytes[8..16] == PNG_IHDR
        && bytes[16..24] == PNG_32PX_DIMENSIONS
}

fn execute_cached<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<usize, Error> {
    Ok(conn.prepare_cached(sql)?.execute(params)?)
}

pub(super) fn valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NOTIFICATION_ID_BYTES
}
pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
pub(super) const BANNER_MAX_AGE_MS: i64 = 15 * 60 * 1000;

pub(super) fn banner_is_fresh(timestamp_ms: Option<i64>, now_ms: i64) -> bool {
    timestamp_ms.is_none_or(|timestamp_ms| {
        timestamp_ms <= 0 || now_ms.saturating_sub(timestamp_ms) <= BANNER_MAX_AGE_MS
    })
}

pub(super) fn valid_target(target: &NotificationTarget) -> bool {
    uuid::Uuid::parse_str(&target.source_device_id).is_ok()
        && valid(&target.notification_key)
        && uuid::Uuid::parse_str(&target.lifetime).is_ok()
}
fn unlocked(ctx: &Ctx<'_>) -> bool {
    ctx.active_epoch
        .is_some_and(|epoch| ctx.keys.contains_key(&epoch))
}
pub(super) fn valid_wire(n: &NotificationWire) -> bool {
    valid_target(&n.target)
        && valid(&n.instance)
        && valid(&n.package_name)
        && n.app_name.len() <= MAX_NOTIFICATION_ID_BYTES
        && valid_icon(n.app_icon_png_base64.as_deref())
        && n.title.len() + n.text.len() <= MAX_NOTIFICATION_TEXT_BYTES
        && (0..=253_402_300_799_999).contains(&n.posted_at)
        && n.category
            .as_ref()
            .is_none_or(|v| v.len() <= MAX_NOTIFICATION_ID_BYTES)
}

impl Client {
    pub fn notification_source_device_id(&self) -> Result<String, Error> {
        Ok(self.lock()?.config.device_id.to_string())
    }
    pub fn capture_notification(
        &self,
        input: NotificationCapture,
    ) -> Result<NotificationCaptureOutcome, Error> {
        if !valid(&input.notification_key)
            || !valid(&input.instance)
            || !valid(&input.package_name)
            || input.app_name.len() > MAX_NOTIFICATION_ID_BYTES
            || !valid_icon(input.app_icon_png_base64.as_deref())
            || input.title.len() + input.text.len() > MAX_NOTIFICATION_TEXT_BYTES
            || !(0..=253_402_300_799_999).contains(&input.posted_at)
            || input
                .category
                .as_ref()
                .is_some_and(|v| v.len() > MAX_NOTIFICATION_ID_BYTES)
        {
            return Err(Error::InvalidRequest("notification"));
        }
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        // Notifications deliberately have no plaintext locked queue.
        if ctx
            .active_epoch
            .is_none_or(|epoch| !ctx.keys.contains_key(&epoch))
        {
            return Ok(NotificationCaptureOutcome::DroppedLocked);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = ctx.device_id.to_string();
        let muted: bool = tx
            .query_row(
                "SELECT muted FROM app_filters WHERE source_device_id=? AND package_name=?",
                params![source, input.package_name],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if muted {
            return Ok(NotificationCaptureOutcome::FilteredOut);
        }
        let existing: Option<ExistingNotification> = tx.query_row("SELECT lifetime,instance,removed,package_name,app_name,title,text,category,posted_at,dismissible,app_icon_png_base64,mark_read_supported FROM notifications WHERE source_device_id=? AND notification_key=?", params![ctx.device_id.to_string(), input.notification_key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?))).optional()?;
        // Only an enabled phone advertises the capability; compare and publish this derived value.
        let mark_read_supported = input.mark_read_supported && mark_read_enabled(&tx)?;
        let retained_post: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM notifications n JOIN outbox o ON o.seq=n.source_sequence WHERE n.source_device_id=? AND n.notification_key=?)", params![source,input.notification_key], |r| r.get(0))?;
        if retained_post
            && existing.as_ref().is_some_and(
                |(
                    _,
                    _instance,
                    removed,
                    package,
                    app,
                    title,
                    text,
                    category,
                    _posted_at,
                    dismissible,
                    icon,
                    stored_mark_read,
                )| {
                    !removed
                        && package == &input.package_name
                        && app == &input.app_name
                        && title == &input.title
                        && text == &input.text
                        && category == &input.category
                        && *dismissible == input.dismissible
                        && icon == &input.app_icon_png_base64
                        && *stored_mark_read == mark_read_supported
                },
            )
        {
            // Re-notifying unchanged content updates the OS identity, but does not create
            // another encrypted history record. Removal/dismissal still track the current OS row.
            tx.execute("UPDATE notifications SET instance=? WHERE source_device_id=? AND notification_key=?", params![input.instance, source, input.notification_key])?;
            tx.execute("UPDATE notification_dismissals SET instance=? WHERE source_device_id=? AND notification_key=? AND lifetime=(SELECT lifetime FROM notifications WHERE source_device_id=? AND notification_key=?) AND completed=0", params![input.instance, source,input.notification_key,source,input.notification_key])?;
            tx.commit()?;
            return Ok(NotificationCaptureOutcome::Duplicate);
        }
        let lifetime = if existing
            .as_ref()
            .is_some_and(|(_, _, removed, ..)| !removed)
        {
            existing.unwrap().0
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        let wire = NotificationWire {
            target: NotificationTarget {
                source_device_id: ctx.device_id.to_string(),
                notification_key: input.notification_key.clone(),
                lifetime: lifetime.clone(),
            },
            instance: input.instance,
            package_name: input.package_name,
            app_name: input.app_name,
            app_icon_png_base64: input.app_icon_png_base64,
            title: input.title,
            text: input.text,
            category: input.category,
            posted_at: input.posted_at,
            dismissible: input.dismissible,
            mark_read_supported,
        };
        let (envelope_id, seq) = enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::NotificationPosted {
                notification: wire.clone(),
            },
        )?;
        tx.execute(
            "INSERT INTO outbox_notification_posts(envelope_id,source_device_id,package_name) VALUES(?,?,?)",
            params![envelope_id.to_string(), wire.target.source_device_id, wire.package_name],
        )?;
        // Local captures belong to the phone, not its desktop banner queue.
        upsert_notification(&tx, &wire, seq, true)?;
        tx.commit()?;
        Ok(NotificationCaptureOutcome::Captured)
    }

    pub fn remove_notification(&self, notification_key: &str, instance: &str) -> Result<(), Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        if !valid(notification_key) || !valid(instance) {
            return Err(Error::InvalidRequest("notification"));
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::remove_notification_tail(&tx, &ctx, notification_key, instance, None)?;
        tx.commit()?;
        Ok(())
    }

    /// Removes this lifetime only if the core's current OS instance still matches `instance`.
    pub fn remove_notification_if_current_instance(
        &self,
        notification_key: &str,
        instance: &str,
    ) -> Result<(), Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        if !valid(notification_key) || !valid(instance) {
            return Err(Error::InvalidRequest("notification"));
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::remove_notification_tail(&tx, &ctx, notification_key, instance, Some(instance))?;
        tx.commit()?;
        Ok(())
    }

    fn remove_notification_tail(
        tx: &Transaction<'_>,
        ctx: &Ctx<'_>,
        notification_key: &str,
        instance: &str,
        required_current_instance: Option<&str>,
    ) -> Result<(), Error> {
        let row: Option<(String, String)> = tx.query_row("SELECT lifetime,instance FROM notifications WHERE source_device_id=? AND notification_key=? AND removed=0", params![ctx.device_id.to_string(),notification_key], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((lifetime, current_instance)) = row else {
            return Ok(());
        };
        if required_current_instance.is_some_and(|expected| expected != current_instance) {
            return Ok(());
        }
        // Unconditional callers no longer mirror this key; they may name an update coalesced
        // (or dropped while locked) before capture. Conditional callers preserve newer instances.
        let target = NotificationTarget {
            source_device_id: ctx.device_id.to_string(),
            notification_key: notification_key.into(),
            lifetime,
        };
        let (_, seq) = enqueue(
            tx,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::NotificationRemoved {
                target: target.clone(),
                instance: instance.into(),
            },
        )?;
        remove_applied(tx, &target, seq)?;
        Ok(())
    }

    pub fn notification_snapshot(&self) -> Result<NotificationSnapshot, Error> {
        let s = self.lock()?;
        prune_banners(&s.conn)?;
        let mut q = s.conn.prepare("SELECT source_device_id,notification_key,lifetime,package_name,app_name,app_icon_png_base64,title,text,category,posted_at,dismissible,seen, EXISTS(SELECT 1 FROM notification_dismissals d WHERE d.source_device_id=notifications.source_device_id AND d.notification_key=notifications.notification_key AND d.lifetime=notifications.lifetime) FROM notifications WHERE removed=0 AND NOT EXISTS(SELECT 1 FROM app_filters f WHERE f.source_device_id=notifications.source_device_id AND f.package_name=notifications.package_name AND f.muted=1) ORDER BY posted_at DESC,source_device_id,notification_key,lifetime LIMIT 1000")?;
        let notifications = q
            .query_map([], |r| {
                Ok(MirroredNotification {
                    target: NotificationTarget {
                        source_device_id: r.get(0)?,
                        notification_key: r.get(1)?,
                        lifetime: r.get(2)?,
                    },
                    package_name: r.get(3)?,
                    app_name: r.get(4)?,
                    app_icon_png_base64: r.get(5)?,
                    title: r.get(6)?,
                    text: r.get(7)?,
                    category: r.get(8)?,
                    posted_at: r.get(9)?,
                    dismissible: r.get(10)?,
                    seen: r.get(11)?,
                    dismissal_pending: r.get(12)?,
                    one_time_code: one_time_code::detect_notification(
                        &r.get::<_, String>(7)?,
                        &r.get::<_, String>(6)?,
                    )
                    .map(|code| code.code),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut q = s.conn.prepare("SELECT source_device_id,package_name,app_name,muted FROM app_filters ORDER BY source_device_id,package_name")?;
        let app_filters = q
            .query_map([], |r| {
                Ok(AppFilter {
                    source_device_id: r.get(0)?,
                    package_name: r.get(1)?,
                    app_name: r.get(2)?,
                    muted: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let dismissal_blocks = notification_dismissal_status::snapshot_blocks(&s.conn)?;
        Ok(NotificationSnapshot {
            notifications,
            app_filters,
            dismissal_blocks,
        })
    }

    pub fn set_app_muted(
        &self,
        source_device_id: &str,
        package_name: &str,
        app_name: &str,
        muted: bool,
    ) -> Result<(), Error> {
        if uuid::Uuid::parse_str(source_device_id).is_err()
            || !valid(package_name)
            || app_name.len() > MAX_NOTIFICATION_ID_BYTES
        {
            return Err(Error::InvalidRequest("notification filter"));
        }
        let mut g = self.lock()?;
        let (conn, ctx) = g.parts();
        if !unlocked(&ctx) {
            return Err(Error::KeysUnavailable);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let filter = AppFilter {
            source_device_id: source_device_id.into(),
            package_name: package_name.into(),
            app_name: app_name.into(),
            muted,
        };
        let revision = get_meta(&tx, "notification_filter_clock")?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Error::Database)?;
        set_meta(&tx, "notification_filter_clock", &revision.to_string())?;
        let (_, sequence) = enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::AppFilter {
                filter: filter.clone(),
                logical_revision: revision,
                writer_device_id: ctx.device_id.to_string(),
            },
        )?;
        apply_filter(
            &tx,
            &filter,
            revision,
            &ctx.device_id.to_string(),
            &ctx.device_id.to_string(),
            sequence,
        )?;
        if muted {
            tx.execute("DELETE FROM banner_candidates WHERE acknowledged=0 AND kind='notification' AND source_device_id=? AND notification_key IN (SELECT notification_key FROM notifications WHERE source_device_id=? AND package_name=?)",params![source_device_id,source_device_id,package_name])?;
        }
        Ok(tx.commit()?)
    }
    pub fn dismiss_notification(&self, target: NotificationTarget) -> Result<(), Error> {
        if !valid_target(&target) {
            return Err(Error::InvalidRequest("notification target"));
        }
        let mut g = self.lock()?;
        let (conn, ctx) = g.parts();
        if !unlocked(&ctx) {
            return Err(Error::KeysUnavailable);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        dismiss_in_tx(&tx, &ctx, &target)?;
        Ok(tx.commit()?)
    }
    /// Records up to `MAX_NOTIFICATION_DISMISS_TARGETS` dismissals in one transaction and reports
    /// each target's outcome in input order. Nothing is recorded when any target is malformed.
    pub fn dismiss_notifications(
        &self,
        targets: Vec<NotificationTarget>,
    ) -> Result<Vec<NotificationDismissResult>, Error> {
        if targets.len() > MAX_NOTIFICATION_DISMISS_TARGETS {
            return Err(Error::InvalidRequest("too many notification targets"));
        }
        if targets.iter().any(|target| !valid_target(target)) {
            return Err(Error::InvalidRequest("notification target"));
        }
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let mut g = self.lock()?;
        let (conn, ctx) = g.parts();
        if !unlocked(&ctx) {
            return Err(Error::KeysUnavailable);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut results = Vec::with_capacity(targets.len());
        for target in targets {
            let outcome = record_dismissal(&tx, &ctx, &target, true)?;
            results.push(NotificationDismissResult { target, outcome });
        }
        seal_all_pending(&tx, &ctx)?;
        tx.commit()?;
        Ok(results)
    }
}

/// Classifies one dismissal in precedence order and records it when it is new. The caller seals.
fn record_dismissal(
    c: &Connection,
    ctx: &Ctx<'_>,
    target: &NotificationTarget,
    check_mute: bool,
) -> Result<NotificationDismissOutcome, Error> {
    let outcome = dismissal_outcome(c, target, check_mute)?;
    if outcome == NotificationDismissOutcome::Recorded {
        enqueue_without_sealing(
            c,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::NotificationDismiss {
                target: target.clone(),
            },
        )?;
        apply_dismiss(c, target, false, ctx.active_epoch.unwrap())?;
    }
    Ok(outcome)
}

fn dismissal_outcome(
    c: &Connection,
    target: &NotificationTarget,
    check_mute: bool,
) -> Result<NotificationDismissOutcome, Error> {
    // A stale UI action cannot target a repost, so a missing live row is `Gone`, even when muted.
    let state: Option<(bool, bool)> = c
        .query_row(
            "SELECT n.dismissible,EXISTS(SELECT 1 FROM app_filters f WHERE f.source_device_id=n.source_device_id AND f.package_name=n.package_name AND f.muted=1) FROM notifications n WHERE n.source_device_id=? AND n.notification_key=? AND n.lifetime=? AND n.removed=0",
            params![target.source_device_id, target.notification_key, target.lifetime],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((dismissible, muted)) = state else {
        return Ok(NotificationDismissOutcome::Gone);
    };
    if check_mute && muted {
        Ok(NotificationDismissOutcome::Muted)
    } else if !dismissible {
        Ok(NotificationDismissOutcome::NotDismissible)
    } else if has_dismissal(c, target)? {
        Ok(NotificationDismissOutcome::AlreadyRecorded)
    } else {
        Ok(NotificationDismissOutcome::Recorded)
    }
}

/// Single-target dismissal: refuses a non-dismissible row and treats stale or repeated targets as
/// success. It does not consult app mute state.
fn dismiss_in_tx(c: &Connection, ctx: &Ctx<'_>, target: &NotificationTarget) -> Result<(), Error> {
    match record_dismissal(c, ctx, target, false)? {
        NotificationDismissOutcome::NotDismissible => {
            Err(Error::InvalidRequest("notification cannot be dismissed"))
        }
        NotificationDismissOutcome::Recorded => {
            seal_pending(c, ctx, MAX_SEAL_BATCH)?;
            Ok(())
        }
        NotificationDismissOutcome::AlreadyRecorded
        | NotificationDismissOutcome::Gone
        | NotificationDismissOutcome::Muted => Ok(()),
    }
}

/// Seals in `MAX_SEAL_BATCH` steps until a step seals fewer rows, so this call's dismissals are
/// sealed even when older unsealed rows are ordered ahead of them.
fn seal_all_pending(c: &Connection, ctx: &Ctx<'_>) -> Result<(), Error> {
    while seal_pending(c, ctx, MAX_SEAL_BATCH)? == MAX_SEAL_BATCH {}
    Ok(())
}

impl Client {
    pub fn mark_notifications_seen(&self, targets: Vec<NotificationTarget>) -> Result<(), Error> {
        if targets.len() > 1000 || targets.iter().any(|target| !valid_target(target)) {
            return Err(Error::InvalidRequest("notification targets"));
        }
        let mut s = self.lock()?;
        let tx = s
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut mark_seen = tx.prepare(
            "UPDATE notifications SET seen=1 WHERE source_device_id=? AND notification_key=? AND lifetime=?",
        )?;
        for t in targets {
            mark_seen.execute(params![t.source_device_id, t.notification_key, t.lifetime])?;
        }
        drop(mark_seen);
        prune_banners(&tx)?;
        tx.commit()?;
        Ok(())
    }
    pub fn pending_notification_dismissals(
        &self,
        limit: usize,
    ) -> Result<Vec<NotificationDismissal>, Error> {
        let s = self.lock()?;
        let Some(epoch) = s.active_epoch.filter(|epoch| s.keys.contains_key(epoch)) else {
            return Ok(Vec::new());
        };
        let mut q=s.conn.prepare("SELECT d.id,d.source_device_id,d.notification_key,d.lifetime,d.instance FROM notification_dismissals d JOIN notifications n ON n.source_device_id=d.source_device_id AND n.notification_key=d.notification_key AND n.lifetime=d.lifetime AND n.instance=d.instance WHERE d.completed=0 AND d.historical=0 AND d.source_device_id=? AND d.key_epoch=? AND n.removed=0 AND n.dismissible=1 ORDER BY d.rowid LIMIT ?")?;
        Ok(q.query_map(
            params![s.config.device_id.to_string(), epoch, limit.min(100) as i64],
            |r| {
                Ok(NotificationDismissal {
                    id: r.get(0)?,
                    target: NotificationTarget {
                        source_device_id: r.get(1)?,
                        notification_key: r.get(2)?,
                        lifetime: r.get(3)?,
                    },
                    instance: r.get(4)?,
                })
            },
        )?
        .collect::<Result<_, _>>()?)
    }
    pub fn complete_notification_dismissal(&self, id: &str) -> Result<(), Error> {
        self.lock()?.conn.execute(
            "UPDATE notification_dismissals SET completed=1 WHERE id=?",
            params![id],
        )?;
        Ok(())
    }

    /// Asks the source phone to mark this content version read through the app's own action, and
    /// shows it read here. `Ok(false)` when the notification changed, left, or lost the capability.
    pub fn request_notification_mark_read(
        &self,
        target: NotificationTarget,
        content_sequence: SourceSequence,
    ) -> Result<bool, Error> {
        let Ok(sequence) = i64::try_from(content_sequence.0) else {
            return Ok(false);
        };
        if !valid_target(&target) {
            return Err(Error::InvalidRequest("notification target"));
        }
        let mut g = self.lock()?;
        let (conn, ctx) = g.parts();
        if !unlocked(&ctx) {
            return Err(Error::KeysUnavailable);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !current_version(&tx, &target, sequence, "mark_read_supported=1")? {
            return Ok(false);
        }
        let requested_at_ms = now_ms();
        enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::NotificationMarkRead {
                target: target.clone(),
                content_sequence,
                requested_at_ms,
            },
        )?;
        apply_mark_read(
            &tx,
            &target,
            sequence,
            requested_at_ms,
            false,
            ctx.active_epoch.unwrap(),
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Dismisses the notification on its phone only if the banner's content version is still current.
    pub fn dismiss_notification_if_unchanged(
        &self,
        target: NotificationTarget,
        content_sequence: SourceSequence,
    ) -> Result<bool, Error> {
        let Ok(sequence) = i64::try_from(content_sequence.0) else {
            return Ok(false);
        };
        if !valid_target(&target) {
            return Err(Error::InvalidRequest("notification target"));
        }
        let mut g = self.lock()?;
        let (conn, ctx) = g.parts();
        if !unlocked(&ctx) {
            return Err(Error::KeysUnavailable);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !current_version(&tx, &target, sequence, "dismissible=1")? {
            return Ok(false);
        }
        dismiss_in_tx(&tx, &ctx, &target)?;
        tx.commit()?;
        Ok(true)
    }

    /// The source phone's executable requests: live, unexpired, current-epoch, never attempted,
    /// for an unchanged notification that still reports the capability. Instances are current.
    pub fn pending_notification_mark_reads(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<NotificationMarkReadRequest>, Error> {
        let mut store = self.lock()?;
        if restore_guarded(&store.conn)? {
            return Ok(Vec::new());
        }
        let Some(epoch) = store
            .active_epoch
            .filter(|epoch| store.keys.contains_key(epoch))
        else {
            return Ok(Vec::new());
        };
        let device = store.config.device_id.to_string();
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (earliest, latest) = mark_read_window(now_ms);
        tx.execute(
            "UPDATE notification_mark_reads SET attempted=1 WHERE attempted=0 AND (requested_at_ms<? OR requested_at_ms>?)",
            params![earliest, latest],
        )?;
        // A consumed version stays as a tombstone while it can still match the live notification,
        // so a later request for the same version can never run again.
        tx.execute(
            "DELETE FROM notification_mark_reads WHERE attempted=1 AND requested_at_ms<? AND NOT EXISTS(SELECT 1 FROM notifications n WHERE n.source_device_id=notification_mark_reads.source_device_id AND n.notification_key=notification_mark_reads.notification_key AND n.lifetime=notification_mark_reads.lifetime AND n.source_sequence=notification_mark_reads.content_sequence AND n.removed=0)",
            params![now_ms.saturating_sub(MARK_READ_RETENTION_MS)],
        )?;
        let requests = if mark_read_enabled(&tx)? {
            let mut q = tx.prepare(&format!("SELECT r.id,r.source_device_id,r.notification_key,r.lifetime,n.instance FROM notification_mark_reads r JOIN notifications n ON n.source_device_id=r.source_device_id AND n.notification_key=r.notification_key AND n.lifetime=r.lifetime AND n.source_sequence=r.content_sequence WHERE {MARK_READ_ELIGIBLE_SQL} ORDER BY r.requested_at_ms LIMIT ?5"))?;
            q.query_map(
                params![device, epoch, earliest, latest, limit.min(100) as i64],
                |r| {
                    Ok(NotificationMarkReadRequest {
                        id: r.get(0)?,
                        target: NotificationTarget {
                            source_device_id: r.get(1)?,
                            notification_key: r.get(2)?,
                            lifetime: r.get(3)?,
                        },
                        instance: r.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        tx.commit()?;
        Ok(requests)
    }

    /// Consumes the one-use permit immediately before the OS effect. True only for the call that
    /// consumed it while the request was still eligible; the host never retries after that.
    pub fn begin_notification_mark_read(&self, id: &str, now_ms: i64) -> Result<bool, Error> {
        let mut store = self.lock()?;
        if restore_guarded(&store.conn)? {
            return Ok(false);
        }
        let Some(epoch) = store
            .active_epoch
            .filter(|epoch| store.keys.contains_key(epoch))
        else {
            return Ok(false);
        };
        let device = store.config.device_id.to_string();
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !mark_read_enabled(&tx)? {
            return Ok(false);
        }
        let (earliest, latest) = mark_read_window(now_ms);
        let consumed = tx.execute(
            &format!("UPDATE notification_mark_reads SET attempted=1 WHERE id=?5 AND EXISTS(SELECT 1 FROM notification_mark_reads r JOIN notifications n ON n.source_device_id=r.source_device_id AND n.notification_key=r.notification_key AND n.lifetime=r.lifetime AND n.source_sequence=r.content_sequence WHERE r.id=?5 AND {MARK_READ_ELIGIBLE_SQL})"),
            params![device, epoch, earliest, latest, id],
        )?;
        tx.commit()?;
        Ok(consumed == 1)
    }
    pub fn pending_banner_candidates(&self, limit: usize) -> Result<Vec<BannerCandidate>, Error> {
        let mut s = self.lock()?;
        prune_banners(&s.conn)?;
        if s.active_epoch
            .is_none_or(|epoch| !s.keys.contains_key(&epoch))
        {
            return Ok(Vec::new());
        }
        type CandidateRow = (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
            i64,
            Option<i64>,
        );
        let rows: Vec<CandidateRow> = s.conn.prepare(&format!("SELECT id,kind,COALESCE((SELECT target_id FROM conversation_merges WHERE source_id=b.conversation_id),b.conversation_id),source_device_id,notification_key,lifetime,title,body,created_at,content_sequence FROM banner_candidates b WHERE acknowledged=0 AND NOT EXISTS(SELECT 1 FROM conversation_folders f WHERE f.conversation_id=({}) AND f.folder='spam') AND ((kind='message' AND EXISTS(SELECT 1 FROM messages m WHERE m.id=b.id AND m.seen=0)) OR (kind='notification' AND EXISTS(SELECT 1 FROM notifications n WHERE n.source_device_id=b.source_device_id AND n.notification_key=b.notification_key AND n.lifetime=b.lifetime AND n.removed=0 AND NOT n.seen AND NOT EXISTS(SELECT 1 FROM notification_dismissals d WHERE d.source_device_id=n.source_device_id AND d.notification_key=n.notification_key AND d.lifetime=n.lifetime) AND NOT EXISTS(SELECT 1 FROM app_filters f WHERE f.source_device_id=n.source_device_id AND f.package_name=n.package_name AND f.muted=1)))) ORDER BY rowid LIMIT ?", crate::conversation_folders::winner_sql("COALESCE((SELECT target_id FROM conversation_merges WHERE source_id=b.conversation_id),b.conversation_id)")))?.query_map(params![limit.min(1000) as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)))?.collect::<Result<_, _>>()?;
        let rows = rows
            .into_iter()
            .map(|row| {
                let (id, kind, _, source_device_id, notification_key, lifetime, _, _, _, _) = &row;
                let message = (kind == "message")
                    .then(|| s.conn.query_row("SELECT payload FROM messages WHERE id=?", [id], |row| row.get::<_, String>(0)).optional())
                    .transpose()?
                    .flatten()
                    .and_then(|payload| decode::<MessagePayload>(payload.as_bytes()).ok());
                let app = (kind == "notification")
                    .then(|| match (source_device_id.as_deref(), notification_key.as_deref(), lifetime.as_deref()) {
                        (Some(source_device_id), Some(notification_key), Some(lifetime)) => s.conn.query_row("SELECT app_name,app_icon_png_base64,source_sequence,dismissible,mark_read_supported FROM notifications WHERE source_device_id=? AND notification_key=? AND lifetime=?", params![source_device_id, notification_key, lifetime], |row| Ok::<LiveNotification, rusqlite::Error>((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional(),
                        _ => Ok(None),
                    })
                    .transpose()?
                    .flatten();
                Ok((row, message, app))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let has_message_sender = rows.iter().any(|(_, message, _)| {
            message
                .as_ref()
                .is_some_and(|payload| payload.sender_address.is_some())
        });
        let resolver = if has_message_sender {
            contact_resolution::current_display_resolver(&mut s.conn)?
        } else {
            None
        };
        rows.into_iter()
            .map(
                |(
                    (
                        id,
                        kind,
                        conversation_id,
                        source_device_id,
                        notification_key,
                        lifetime,
                        title,
                        body,
                        created_at,
                        shown_sequence,
                    ),
                    message,
                    app,
                )| {
                    let notification_target = match (
                        source_device_id.as_deref(),
                        notification_key.as_deref(),
                        lifetime.as_deref(),
                    ) {
                        (Some(source_device_id), Some(notification_key), Some(lifetime)) => {
                            Some(NotificationTarget {
                                source_device_id: source_device_id.to_owned(),
                                notification_key: notification_key.to_owned(),
                                lifetime: lifetime.to_owned(),
                            })
                        }
                        _ => None,
                    };
                    let one_time_code = if kind == "message" {
                        message
                            .as_ref()
                            .filter(|payload| payload.direction == Direction::Incoming)
                            .and_then(|payload| one_time_code::detect(&payload.body))
                            .map(|code| code.code)
                    } else {
                        one_time_code::detect_notification(&body, &title).map(|code| code.code)
                    };
                    // Actions authorize only the version this banner shows; once the live row has
                    // moved on, the banner keeps its text but offers no phone effect.
                    let (content_sequence, dismissible, mark_read_supported) = app
                        .as_ref()
                        .filter(|(_, _, live, ..)| shown_sequence == Some(*live))
                        .and_then(|(_, _, live, dismissible, mark_read)| {
                            Some((
                                Some(SourceSequence(u64::try_from(*live).ok()?)),
                                *dismissible,
                                *mark_read,
                            ))
                        })
                        .unwrap_or((None, false, false));
                    let (source_name, app_icon_png_base64) = if kind == "message" {
                        let source_name = message.and_then(|payload| {
                            let sender = payload.sender_address?;
                            resolver
                                .as_ref()
                                .and_then(|resolver| {
                                    resolver
                                        .resolve(
                                            &sender,
                                            Some(&payload.source_device_id.to_string()),
                                        )
                                        .ok()
                                        .and_then(|resolution| resolution.chosen_display_name)
                                })
                                .or(Some(sender))
                        });
                        (source_name, None)
                    } else {
                        match app {
                            Some((app_name, icon, ..)) => (
                                if app_name.is_empty() {
                                    None
                                } else {
                                    Some(app_name)
                                },
                                icon.filter(|icon| valid_icon(Some(icon))),
                            ),
                            None => (None, None),
                        }
                    };
                    Ok(BannerCandidate {
                        id,
                        kind,
                        conversation_id,
                        notification_target,
                        title,
                        body,
                        created_at,
                        source_name,
                        app_icon_png_base64,
                        one_time_code,
                        content_sequence,
                        dismissible,
                        mark_read_supported,
                    })
                },
            )
            .collect()
    }
    pub fn ack_banner_candidates(&self, ids: Vec<String>) -> Result<(), Error> {
        let s = self.lock()?;
        let mut delete = s.conn.prepare("DELETE FROM banner_candidates WHERE id=?")?;
        for id in ids {
            delete.execute(params![id])?;
        }
        Ok(())
    }
}

fn upsert_notification(
    c: &Connection,
    n: &NotificationWire,
    seq: u64,
    historical: bool,
) -> Result<(), Error> {
    let tombstone: Option<i64> = c.query_row("SELECT source_sequence FROM notification_tombstones WHERE source_device_id=? AND notification_key=? AND lifetime=?", params![n.target.source_device_id,n.target.notification_key,n.target.lifetime], |r| r.get(0)).optional()?;
    // Removal ends a lifetime permanently. A fresh post must carry a fresh lifetime.
    if tombstone.is_some() {
        return Ok(());
    }
    let old:Option<(String,i64)>=c.query_row("SELECT lifetime,source_sequence FROM notifications WHERE source_device_id=? AND notification_key=?",params![n.target.source_device_id,n.target.notification_key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let first_post = old
        .as_ref()
        .is_none_or(|(lifetime, _)| lifetime != &n.target.lifetime);
    if old.as_ref().is_some_and(|(_, s)| *s >= seq as i64) {
        return Ok(());
    }
    execute_cached(
        c,
        "INSERT INTO notifications(source_device_id,notification_key,lifetime,instance,package_name,app_name,app_icon_png_base64,title,text,category,posted_at,dismissible,seen,removed,source_sequence,mark_read_supported) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,0,0,?,?) ON CONFLICT(source_device_id,notification_key) DO UPDATE SET seen=CASE WHEN notifications.lifetime=excluded.lifetime THEN notifications.seen ELSE 0 END,lifetime=excluded.lifetime,instance=excluded.instance,package_name=excluded.package_name,app_name=excluded.app_name,app_icon_png_base64=excluded.app_icon_png_base64,title=excluded.title,text=excluded.text,category=excluded.category,posted_at=excluded.posted_at,dismissible=excluded.dismissible,removed=0,source_sequence=excluded.source_sequence,mark_read_supported=excluded.mark_read_supported WHERE excluded.source_sequence>notifications.source_sequence",
        params![
            n.target.source_device_id,
            n.target.notification_key,
            n.target.lifetime,
            n.instance,
            n.package_name,
            n.app_name,
            n.app_icon_png_base64,
            n.title,
            n.text,
            n.category,
            n.posted_at,
            n.dismissible,
            seq as i64,
            n.mark_read_supported
        ],
    )?;
    // A dismissal received before the post remains presentation state; bind its eventual
    // OS effect to the first matching instance only, never to a subsequent update.
    execute_cached(
        c,
        "UPDATE notification_dismissals SET instance=? WHERE source_device_id=? AND notification_key=? AND lifetime=? AND completed=0",
        params![
            n.instance,
            n.target.source_device_id,
            n.target.notification_key,
            n.target.lifetime
        ],
    )?;
    execute_cached(
        c,
        "INSERT OR IGNORE INTO app_filters(source_device_id,package_name,app_name,muted) VALUES(?,?,?,0)",
        params![n.target.source_device_id, n.package_name, n.app_name],
    )?;
    if !historical && first_post && banner_is_fresh(Some(n.posted_at), now_ms()) {
        let id = json(&n.target)?;
        c.execute("INSERT OR IGNORE INTO banner_candidates(id,kind,conversation_id,source_device_id,notification_key,lifetime,title,body,created_at,content_sequence) VALUES(?, 'notification',NULL,?,?,?,?,?,?,?)",params![id,n.target.source_device_id,n.target.notification_key,n.target.lifetime,if n.title.is_empty() { &n.app_name } else { &n.title },n.text,now_ms(),seq as i64])?;
    }
    prune_banners(c)?;
    Ok(())
}
pub(super) fn remove_applied(
    c: &Connection,
    t: &NotificationTarget,
    seq: u64,
) -> Result<(), Error> {
    execute_cached(
        c,
        "INSERT INTO notification_tombstones(source_device_id,notification_key,lifetime,source_sequence) VALUES(?,?,?,?) ON CONFLICT(source_device_id,notification_key,lifetime) DO UPDATE SET source_sequence=MAX(source_sequence,excluded.source_sequence)",
        params![
            t.source_device_id,
            t.notification_key,
            t.lifetime,
            seq as i64
        ],
    )?;
    execute_cached(
        c,
        "UPDATE notifications SET removed=1,title='',text='',app_icon_png_base64=NULL,source_sequence=? WHERE source_device_id=? AND notification_key=? AND lifetime=? AND source_sequence<=?",
        params![
            seq as i64,
            t.source_device_id,
            t.notification_key,
            t.lifetime,
            seq as i64
        ],
    )?;
    execute_cached(
        c,
        "DELETE FROM banner_candidates WHERE source_device_id=? AND notification_key=? AND lifetime=?",
        params![t.source_device_id, t.notification_key, t.lifetime],
    )?;
    execute_cached(
        c,
        "UPDATE notification_dismissals SET completed=1 WHERE source_device_id=? AND notification_key=? AND lifetime=?",
        params![t.source_device_id, t.notification_key, t.lifetime],
    )?;
    Ok(())
}
pub(super) fn apply_filter(
    c: &Connection,
    f: &AppFilter,
    revision: u64,
    writer: &str,
    producer: &str,
    sequence: u64,
) -> Result<(), Error> {
    let revision = i64::try_from(revision).map_err(|_| Error::InvalidRequest("filter revision"))?;
    let clock = get_meta(c, "notification_filter_clock")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    set_meta(
        c,
        "notification_filter_clock",
        &clock.max(revision).to_string(),
    )?;
    let changed = c.execute("INSERT INTO app_filters(source_device_id,package_name,app_name,muted,source_sequence,producer_device_id,logical_revision,writer_device_id) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(source_device_id,package_name) DO UPDATE SET app_name=excluded.app_name,muted=excluded.muted,source_sequence=excluded.source_sequence,producer_device_id=excluded.producer_device_id,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id WHERE excluded.logical_revision>app_filters.logical_revision OR (excluded.logical_revision=app_filters.logical_revision AND excluded.writer_device_id>app_filters.writer_device_id)",params![f.source_device_id,f.package_name,f.app_name,f.muted,sequence as i64,producer,revision,writer])?;
    if changed > 0 && f.muted {
        // Sealed bytes cannot be edited. Remove entire unsent post envelopes using metadata,
        // without interpreting ciphertext or disturbing unrelated SMS/command outbox rows.
        c.execute("DELETE FROM outbox_conflicts WHERE envelope_id IN (SELECT p.envelope_id FROM outbox_notification_posts p JOIN outbox o ON o.envelope_id=p.envelope_id WHERE p.source_device_id=? AND p.package_name=? AND o.state!='acknowledged')", params![f.source_device_id,f.package_name])?;
        // Purged posts will never upload: drop them from the compaction frontier so no later
        // record references a sequence the server can never resolve.
        c.execute("DELETE FROM compaction_frontier WHERE producer_device_id=(SELECT v FROM metadata WHERE k='device_id') AND source_sequence IN (SELECT o.seq FROM outbox o JOIN outbox_notification_posts p ON p.envelope_id=o.envelope_id WHERE o.state!='acknowledged' AND p.source_device_id=? AND p.package_name=?)", params![f.source_device_id,f.package_name])?;
        c.execute("DELETE FROM outbox WHERE state!='acknowledged' AND envelope_id IN (SELECT envelope_id FROM outbox_notification_posts WHERE source_device_id=? AND package_name=?)", params![f.source_device_id,f.package_name])?;
        c.execute("DELETE FROM banner_candidates WHERE kind='notification' AND source_device_id=? AND notification_key IN (SELECT notification_key FROM notifications WHERE source_device_id=? AND package_name=?)",params![f.source_device_id,f.source_device_id,f.package_name])?;
    }
    Ok(())
}

pub(super) fn prune_banners(c: &Connection) -> Result<(), Error> {
    c.execute("DELETE FROM banner_candidates WHERE created_at<? OR (kind='message' AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.id=banner_candidates.id AND m.seen=0)) OR (kind='notification' AND NOT EXISTS(SELECT 1 FROM notifications n WHERE n.source_device_id=banner_candidates.source_device_id AND n.notification_key=banner_candidates.notification_key AND n.lifetime=banner_candidates.lifetime AND n.removed=0 AND n.seen=0 AND NOT EXISTS(SELECT 1 FROM notification_dismissals d WHERE d.source_device_id=n.source_device_id AND d.notification_key=n.notification_key AND d.lifetime=n.lifetime)))", params![now_ms().saturating_sub(86_400_000)])?;
    Ok(())
}
fn has_dismissal(c: &Connection, t: &NotificationTarget) -> Result<bool, Error> {
    Ok(c.query_row("SELECT 1 FROM notification_dismissals WHERE source_device_id=? AND notification_key=? AND lifetime=? LIMIT 1",params![t.source_device_id,t.notification_key,t.lifetime], |_| Ok(())).optional()?.is_some())
}
fn apply_dismiss(
    c: &Connection,
    t: &NotificationTarget,
    historical: bool,
    epoch: u32,
) -> Result<(), Error> {
    if has_dismissal(c, t)? {
        return Ok(());
    }
    let instance:Option<String>=c.query_row("SELECT instance FROM notifications WHERE source_device_id=? AND notification_key=? AND lifetime=? AND removed=0 AND dismissible=1",params![t.source_device_id,t.notification_key,t.lifetime],|r|r.get(0)).optional()?;
    c.execute("INSERT OR IGNORE INTO notification_dismissals(id,source_device_id,notification_key,lifetime,instance,historical,key_epoch) VALUES(?,?,?,?,?,?,?)",params![json(t)?,t.source_device_id,t.notification_key,t.lifetime,instance.unwrap_or_default(),historical,epoch])?;
    Ok(())
}
fn mark_read_enabled(c: &Connection) -> Result<bool, Error> {
    Ok(c.query_row(
        "SELECT notification_mark_read_enabled AND mirroring_enabled FROM gateway_settings WHERE id=1",
        [],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or(false))
}

/// Every device shows the requested content version as read; only the source phone records the
/// one-use effect. Snapshot- or restore-learned requests are stored historical and never run, and
/// a request arriving while the setting or mirroring is off is stored already retired.
fn apply_mark_read(
    c: &Connection,
    t: &NotificationTarget,
    content_sequence: i64,
    requested_at_ms: i64,
    historical: bool,
    epoch: u32,
) -> Result<(), Error> {
    execute_cached(
        c,
        "UPDATE notifications SET seen=1 WHERE source_device_id=? AND notification_key=? AND lifetime=? AND source_sequence=? AND removed=0",
        params![
            t.source_device_id,
            t.notification_key,
            t.lifetime,
            content_sequence
        ],
    )?;
    execute_cached(
        c,
        "INSERT OR IGNORE INTO notification_mark_reads(id,source_device_id,notification_key,lifetime,content_sequence,requested_at_ms,historical,key_epoch,attempted) SELECT ?,?,?,?,?,?,?,?,NOT COALESCE((SELECT notification_mark_read_enabled AND mirroring_enabled FROM gateway_settings WHERE id=1),0) WHERE ?=(SELECT v FROM metadata WHERE k='device_id')",
        params![
            uuid::Uuid::new_v4().to_string(),
            t.source_device_id,
            t.notification_key,
            t.lifetime,
            content_sequence,
            requested_at_ms,
            historical,
            epoch,
            t.source_device_id
        ],
    )?;
    prune_banners(c)
}

/// Retires every unattempted request, so turning the setting or mirroring off never leaves one to
/// run after it is turned back on.
pub(super) fn retire_pending_mark_reads(c: &Connection) -> Result<(), Error> {
    c.execute(
        "UPDATE notification_mark_reads SET attempted=1 WHERE attempted=0",
        [],
    )?;
    Ok(())
}

/// Consumed rows for versions that can no longer execute are kept a day, then removed.
const MARK_READ_RETENTION_MS: i64 = 86_400_000;

/// Inclusive `requested_at_ms` bounds for a request that may still run at `now_ms`.
fn mark_read_window(now_ms: i64) -> (i64, i64) {
    (
        now_ms.saturating_sub(NOTIFICATION_MARK_READ_TTL_MS + sender_blocklist::REQUEST_SKEW_MS),
        now_ms.saturating_add(sender_blocklist::REQUEST_SKEW_MS),
    )
}

/// The notification is live at exactly the content version a banner showed, and `condition` holds.
fn current_version(
    c: &Connection,
    t: &NotificationTarget,
    content_sequence: i64,
    condition: &str,
) -> Result<bool, Error> {
    Ok(c.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM notifications WHERE source_device_id=? AND notification_key=? AND lifetime=? AND source_sequence=? AND removed=0 AND {condition})"),
        params![t.source_device_id, t.notification_key, t.lifetime, content_sequence],
        |r| r.get(0),
    )?)
}

const MARK_READ_ELIGIBLE_SQL: &str = "r.attempted=0 AND r.historical=0 AND r.source_device_id=?1 AND r.key_epoch=?2 AND r.requested_at_ms>=?3 AND r.requested_at_ms<=?4 AND n.removed=0 AND n.mark_read_supported=1";

/// Shape and authorship rules for a received `AppFilter` event.
pub(super) fn valid_filter_event(
    filter: &AppFilter,
    logical_revision: u64,
    writer_device_id: &str,
    producer: &str,
) -> bool {
    valid(&filter.source_device_id)
        && valid(&filter.package_name)
        && uuid::Uuid::parse_str(&filter.source_device_id).is_ok()
        && uuid::Uuid::parse_str(writer_device_id).is_ok()
        && writer_device_id == producer
        && filter.app_name.len() <= MAX_NOTIFICATION_ID_BYTES
        && logical_revision != 0
        && logical_revision < i64::MAX as u64
}
pub(super) fn apply_notification_payload(
    c: &Connection,
    p: PrivatePayload,
    producer: DeviceId,
    seq: u64,
    historical: bool,
    epoch: u32,
) -> Result<bool, Error> {
    if i64::try_from(seq).is_err() {
        return Ok(false);
    }
    match p {
        PrivatePayload::NotificationPosted { notification } => {
            if !valid_wire(&notification)
                || notification.target.source_device_id != producer.to_string()
            {
                return Ok(false);
            };
            upsert_notification(c, &notification, seq, historical)?;
            Ok(true)
        }
        PrivatePayload::NotificationRemoved { target, instance } => {
            if !valid_target(&target)
                || !valid(&instance)
                || target.source_device_id != producer.to_string()
            {
                return Ok(false);
            };
            remove_applied(c, &target, seq)?;
            Ok(true)
        }
        PrivatePayload::NotificationDismiss { target } => {
            if !valid_target(&target) {
                return Ok(false);
            }
            apply_dismiss(c, &target, historical, epoch)?;
            Ok(true)
        }
        PrivatePayload::NotificationMarkRead {
            target,
            content_sequence,
            requested_at_ms,
        } => {
            if !valid_target(&target)
                || i64::try_from(content_sequence.0).is_err()
                || requested_at_ms < 0
            {
                return Ok(false);
            }
            apply_mark_read(
                c,
                &target,
                content_sequence.0 as i64,
                requested_at_ms,
                historical,
                epoch,
            )?;
            Ok(true)
        }
        PrivatePayload::AppFilter {
            filter,
            logical_revision,
            writer_device_id,
        } => {
            if !valid_filter_event(
                &filter,
                logical_revision,
                &writer_device_id,
                &producer.to_string(),
            ) {
                return Ok(false);
            };
            apply_filter(
                c,
                &filter,
                logical_revision,
                &writer_device_id,
                &producer.to_string(),
                seq,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use serde_json::json;

    #[test]
    fn banner_freshness_allows_unknown_future_and_the_cutoff_boundary() {
        let now = now_ms();
        assert!(banner_is_fresh(None, now));
        assert!(banner_is_fresh(Some(0), now));
        assert!(banner_is_fresh(Some(now + 1), now));
        assert!(banner_is_fresh(Some(now - BANNER_MAX_AGE_MS), now));
        assert!(!banner_is_fresh(Some(now - BANNER_MAX_AGE_MS - 1), now));
    }

    fn unlocked_client() -> (Client, tempfile::TempDir) {
        let directory = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let config = ClientConfig {
            database_path: directory.path().join("notifications.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let client = Client::open(config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        (client, directory)
    }

    fn address_index_count(client: &Client) -> i64 {
        client
            .lock()
            .unwrap()
            .conn
            .query_row("SELECT COUNT(*) FROM contact_address_index", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn only_message_banners_refresh_the_contact_address_index() {
        let (client, _directory) = unlocked_client();
        let sender = "+12025550101";
        let owner = client.lock().unwrap().config.device_id;
        client
            .capture_contact_book(
                &json!({
                    "schema_version": 1,
                    "book": {
                        "id": "contacts",
                        "owner_device_id": owner.to_string(),
                        "generation": "1",
                        "state": "active",
                        "region": "US",
                    },
                    "contacts": [{
                        "id": "contact",
                        "book_id": "contacts",
                        "display_name": "Fresh Contact",
                        "name": {},
                        "phones": [{"id": "phone", "value": sender}],
                        "emails": [],
                        "addresses": [],
                    }],
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(
            address_index_count(&client),
            0,
            "the contact index starts unbuilt"
        );

        assert!(client.pending_banner_candidates(10).unwrap().is_empty());
        assert_eq!(
            address_index_count(&client),
            0,
            "an empty batch must not refresh"
        );

        let notification = NotificationWire {
            target: NotificationTarget {
                source_device_id: DeviceId::new().to_string(),
                notification_key: "mirrored".into(),
                lifetime: uuid::Uuid::new_v4().to_string(),
            },
            instance: "one".into(),
            package_name: "com.example.mail".into(),
            app_name: "Mail".into(),
            app_icon_png_base64: None,
            title: "Subject".into(),
            text: "Body".into(),
            category: None,
            posted_at: now_ms(),
            dismissible: true,
            mark_read_supported: false,
        };
        upsert_notification(&client.lock().unwrap().conn, &notification, 1, false).unwrap();
        assert_eq!(client.pending_banner_candidates(10).unwrap().len(), 1);
        assert_eq!(
            address_index_count(&client),
            0,
            "mirrored-notification batches must not refresh"
        );

        let captured = client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: sender.into(),
                body: "Message body".into(),
                provider_message_id: Some("message".into()),
                imported: false,
            })
            .unwrap();
        client.lock().unwrap().conn.execute(
            "INSERT INTO banner_candidates(id,kind,conversation_id,title,body,created_at) VALUES(?, 'message', ?, ?, ?, ?)",
            params![
                captured.message_id.to_string(),
                captured.conversation_id.to_string(),
                sender,
                "Message body",
                now_ms()
            ],
        ).unwrap();
        let candidates = client.pending_banner_candidates(10).unwrap();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.source_name.as_deref() == Some("Fresh Contact"))
        );
        assert!(
            address_index_count(&client) > 0,
            "message batches refresh the contact address index"
        );
    }

    #[test]
    fn historical_mark_read_request_never_executes() {
        let (client, _directory) = unlocked_client();
        client
            .set_gateway_settings(GatewaySettings {
                mirroring_enabled: true,
                notification_mark_read_enabled: true,
                ..GatewaySettings::default()
            })
            .unwrap();
        client
            .capture_notification(NotificationCapture {
                notification_key: "k1".into(),
                instance: "i1".into(),
                package_name: "com.example.mail".into(),
                app_name: "Mail".into(),
                app_icon_png_base64: None,
                title: "Subject".into(),
                text: "Body".into(),
                category: None,
                posted_at: now_ms(),
                dismissible: true,
                mark_read_supported: true,
            })
            .unwrap();
        let (target, sequence): (NotificationTarget, i64) = {
            let store = client.lock().unwrap();
            store
                .conn
                .query_row(
                    "SELECT source_device_id,notification_key,lifetime,source_sequence FROM notifications",
                    [],
                    |r| {
                        Ok((
                            NotificationTarget {
                                source_device_id: r.get(0)?,
                                notification_key: r.get(1)?,
                                lifetime: r.get(2)?,
                            },
                            r.get(3)?,
                        ))
                    },
                )
                .unwrap()
        };
        let apply = |historical| {
            let store = client.lock().unwrap();
            let epoch = store.active_epoch.unwrap();
            assert!(
                apply_notification_payload(
                    &store.conn,
                    PrivatePayload::NotificationMarkRead {
                        target: target.clone(),
                        content_sequence: SourceSequence(sequence as u64),
                        requested_at_ms: now_ms(),
                    },
                    DeviceId::new(),
                    7,
                    historical,
                    epoch,
                )
                .unwrap()
            );
        };
        apply(true);
        assert!(
            client
                .pending_notification_mark_reads(now_ms(), 10)
                .unwrap()
                .is_empty(),
            "a snapshot- or restore-learned request never authorizes the effect"
        );
        apply(false);
        assert!(
            client
                .pending_notification_mark_reads(now_ms(), 10)
                .unwrap()
                .is_empty(),
            "a later live copy of the same version does not replace the historical row"
        );
    }
}
