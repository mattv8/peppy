//! Bounded maintenance for server-enforced draft expiry.

use sqlx::{PgPool, Row};

const EXPIRABLE_DRAFT_ATTACHMENT: &str = "a.draft_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM draft_attachment_holds h WHERE h.vault_id=a.vault_id AND h.attachment_id=a.attachment_id AND h.expires_at>now()) AND NOT EXISTS(SELECT 1 FROM attachment_record_references r WHERE r.vault_id=a.vault_id AND r.attachment_id=a.attachment_id) AND NOT EXISTS(SELECT 1 FROM public_attachment_copies p WHERE p.vault_id=a.vault_id AND p.attachment_id=a.attachment_id AND p.ready_at IS NOT NULL AND p.revoked_at IS NULL AND p.retired_at IS NULL)";

/// Expires due branches one vault at a time. The caller uses the maintenance
/// pool, so this deliberately does not install a tenant scope.
pub async fn expire_due(db: &PgPool, limit: i64) -> Result<u64, sqlx::Error> {
    // Vaults with a due branch or a lapsed hold: lapsed holds are the trigger
    // for unreferenced draft-object cleanup even when no branch has expired.
    let vaults = sqlx::query_scalar::<_, uuid::Uuid>(
        &format!("SELECT vault_id FROM (SELECT vault_id FROM draft_branches WHERE state='live' AND expires_at<=now() UNION SELECT vault_id FROM draft_attachment_holds WHERE expires_at<=now() UNION SELECT a.vault_id FROM attachments a WHERE {EXPIRABLE_DRAFT_ATTACHMENT}) due ORDER BY vault_id LIMIT $1"),
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut expired = 0;
    for vault_id in vaults {
        let mut tx = db.begin().await?;
        // A vault locked by a concurrent request or worker is skipped this pass.
        let locked =
            sqlx::query("SELECT vault_id FROM vaults WHERE vault_id=$1 FOR UPDATE SKIP LOCKED")
                .bind(vault_id)
                .fetch_optional(&mut *tx)
                .await?;
        if locked.is_none() {
            tx.rollback().await?;
            continue;
        }
        let drafts = sqlx::query("SELECT draft_id FROM draft_branches WHERE vault_id=$1 AND state='live' AND expires_at<=now() FOR UPDATE")
            .bind(vault_id).fetch_all(&mut *tx).await?;
        let mut seen = std::collections::HashSet::new();
        for draft in drafts {
            let draft_id: uuid::Uuid = draft.get("draft_id");
            if !seen.insert(draft_id) {
                continue;
            }
            let closed: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT branch_id FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live' AND expires_at<=now() ORDER BY branch_id")
                .bind(vault_id).bind(draft_id).fetch_all(&mut *tx).await?;
            for branch_id in &closed {
                let seq: i64 = sqlx::query_scalar("UPDATE vaults SET draft_change_seq=draft_change_seq+1 WHERE vault_id=$1 RETURNING draft_change_seq").bind(vault_id).fetch_one(&mut *tx).await?;
                sqlx::query("UPDATE draft_branches SET state='closed',close_reason='expired',change_seq=$3 WHERE vault_id=$1 AND branch_id=$2")
                    .bind(vault_id).bind(branch_id).bind(seq).execute(&mut *tx).await?;
            }
            if !closed.is_empty() {
                sqlx::query("UPDATE draft_revisions SET sealed_record=NULL WHERE vault_id=$1 AND draft_id=$2 AND branch_id=ANY($3) AND sealed_record IS NOT NULL").bind(vault_id).bind(draft_id).bind(&closed).execute(&mut *tx).await?;
                let live: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live')").bind(vault_id).bind(draft_id).fetch_one(&mut *tx).await?;
                if !live {
                    let terminal_seq: i64 = sqlx::query_scalar("UPDATE vaults SET draft_change_seq=draft_change_seq+1 WHERE vault_id=$1 RETURNING draft_change_seq").bind(vault_id).fetch_one(&mut *tx).await?;
                    sqlx::query("INSERT INTO draft_terminals(vault_id,draft_id,reason,change_seq) VALUES($1,$2,'expired',$3) ON CONFLICT DO NOTHING").bind(vault_id).bind(draft_id).bind(terminal_seq).execute(&mut *tx).await?;
                    sqlx::query("UPDATE draft_attachment_holds SET expires_at=now() WHERE vault_id=$1 AND draft_id=$2 AND kind IN ('draft','staging')").bind(vault_id).bind(draft_id).execute(&mut *tx).await?;
                }
                expired += closed.len() as u64;
            }
        }
        sqlx::query("DELETE FROM draft_attachment_holds WHERE vault_id=$1 AND expires_at<=now()")
            .bind(vault_id)
            .execute(&mut *tx)
            .await?;
        let keys = sqlx::query(&format!("WITH candidates AS (SELECT a.vault_id,a.attachment_id,a.object_key FROM attachments a WHERE a.vault_id=$1 AND {EXPIRABLE_DRAFT_ATTACHMENT}) DELETE FROM attachments a USING candidates c WHERE a.vault_id=c.vault_id AND a.attachment_id=c.attachment_id RETURNING c.object_key"))
            .bind(vault_id).fetch_all(&mut *tx).await?;
        for key in keys {
            sqlx::query("INSERT INTO storage_deletions(object_key,vault_id,reason) VALUES($1,$2,'draft_expired') ON CONFLICT(object_key) DO UPDATE SET not_before=now()").bind(key.get::<String,_>("object_key")).bind(vault_id).execute(&mut *tx).await?;
        }
        let backlog: (i64, Option<f64>) = sqlx::query_as("SELECT count(*)::bigint, EXTRACT(EPOCH FROM now()-min(not_before))::double precision FROM storage_deletions WHERE not_before<=now()")
            .fetch_one(&mut *tx).await?;
        tracing::info!(vault_id=%vault_id, due_storage_deletions=backlog.0, oldest_overdue_seconds=?backlog.1, "draft retention storage deletion backlog");
        tx.commit().await?;
    }
    Ok(expired)
}
