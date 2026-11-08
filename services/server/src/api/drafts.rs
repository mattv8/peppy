use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_hosted_api::drafts::{
    AcceptDraftSendRequest, AcceptDraftSendResponse, DraftBranchCloseReason, DraftChange,
    DraftChangesResponse, DraftTerminalReason, MAX_DRAFT_CHANGES_PAGE, PutDraftRevisionRequest,
    PutDraftRevisionResponse,
};
use serde::Deserialize;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{
    ApiError, ApiResult, ApiState, Operation, auth_with_mode, authorize, database_unavailable,
    revalidate_principal, withdraw_legacy_retention_mutation,
};
use crate::scope;

async fn lock_vault(tx: &mut Transaction<'_, Postgres>, vault: Uuid) -> ApiResult<()> {
    sqlx::query("SELECT vault_id FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault)
        .fetch_one(&mut **tx)
        .await
        .map(|_| ())
        .map_err(|error| database_unavailable(&error, "draft_lock_vault"))
}

async fn next_seq(tx: &mut Transaction<'_, Postgres>, vault: Uuid) -> ApiResult<i64> {
    sqlx::query_scalar("UPDATE vaults SET draft_change_seq=draft_change_seq+1 WHERE vault_id=$1 RETURNING draft_change_seq")
        .bind(vault).fetch_one(&mut **tx).await.map_err(|error| database_unavailable(&error, "draft_next_seq"))
}

fn terminal_error(reason: &str) -> ApiError {
    ApiError(
        StatusCode::GONE,
        match reason {
            "sent" => "draft_sent",
            "discarded" => "draft_discarded",
            _ => "draft_expired",
        },
    )
}

#[derive(Deserialize)]
pub(super) struct ChangesQuery {
    after: Option<String>,
    limit: Option<u32>,
}

pub(super) async fn list(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ChangesQuery>,
) -> ApiResult<Json<DraftChangesResponse>> {
    let principal = auth_with_mode(&s, &headers).await?;
    authorize(&s, &principal, Operation::Sync).await?;
    let after = parse_change_cursor(query.after.as_deref())
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_cursor"))?;
    let limit = i64::from(
        query
            .limit
            .unwrap_or(MAX_DRAFT_CHANGES_PAGE)
            .clamp(1, MAX_DRAFT_CHANGES_PAGE),
    );
    let mut tx = scope::begin(&s.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "draft_list_begin"))?;
    let rows = sqlx::query("SELECT * FROM (SELECT b.change_seq,b.draft_id,b.branch_id,b.head_revision_id,r.parent_revision_id,r.producer_device_id,r.sealed_record,r.attachment_ids,to_char(b.expires_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') expires_at,to_char(r.accepted_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') accepted_at,b.state,b.close_reason,NULL::text terminal_reason,NULL::uuid command_id,NULL::text terminated_at FROM draft_branches b JOIN draft_revisions r ON r.vault_id=b.vault_id AND r.draft_id=b.draft_id AND r.revision_id=b.head_revision_id WHERE b.vault_id=$1 UNION ALL SELECT t.change_seq,t.draft_id,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,t.reason,t.command_id,to_char(t.terminated_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM draft_terminals t WHERE t.vault_id=$1) changes WHERE change_seq>$2 ORDER BY change_seq LIMIT $3")
        .bind(principal.vault).bind(after).bind(limit + 1).fetch_all(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_list"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "draft_list_commit"))?;
    let more = rows.len() as i64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let next_after = rows
        .last()
        .map(|row| row.get::<i64, _>("change_seq").to_string())
        .unwrap_or_else(|| after.to_string());
    let changes = rows
        .into_iter()
        .map(|row| {
            let seq = row.get::<i64, _>("change_seq").to_string();
            let draft_id = row.get("draft_id");
            if let Some(reason) = row.get::<Option<String>, _>("terminal_reason") {
                return DraftChange::Terminal {
                    seq,
                    draft_id,
                    reason: terminal_reason(&reason),
                    command_id: row.get("command_id"),
                    terminated_at: timestamp(&row, "terminated_at"),
                };
            }
            let branch_id = row.get("branch_id");
            if row.get::<String, _>("state") == "closed" {
                return DraftChange::BranchClosed {
                    seq,
                    draft_id,
                    branch_id,
                    reason: close_reason(
                        row.get::<Option<String>, _>("close_reason")
                            .as_deref()
                            .unwrap_or("expired"),
                    ),
                };
            }
            DraftChange::Head {
                seq,
                draft_id,
                branch_id,
                revision_id: row.get("head_revision_id"),
                parent_revision_id: row.get("parent_revision_id"),
                producer_device_id: row.get("producer_device_id"),
                sealed_record: URL_SAFE_NO_PAD.encode(row.get::<Vec<u8>, _>("sealed_record")),
                attachment_ids: row.get("attachment_ids"),
                expires_at: timestamp(&row, "expires_at"),
                accepted_at: timestamp(&row, "accepted_at"),
            }
        })
        .collect();
    Ok(Json(DraftChangesResponse {
        changes,
        next_after,
        more,
    }))
}

/// Canonical change cursor: `0`, or a nonzero unpadded decimal within `i64`.
fn parse_change_cursor(value: Option<&str>) -> Option<i64> {
    let raw = value.unwrap_or("0");
    let canonical = raw == "0"
        || (raw.starts_with(|c: char| matches!(c, '1'..='9'))
            && raw.bytes().all(|byte| byte.is_ascii_digit()));
    if canonical {
        raw.parse::<i64>().ok()
    } else {
        None
    }
}

fn timestamp(row: &sqlx::postgres::PgRow, column: &str) -> String {
    row.try_get(column).unwrap_or_default()
}
fn close_reason(value: &str) -> DraftBranchCloseReason {
    match value {
        "superseded" => DraftBranchCloseReason::Superseded,
        "resolved" => DraftBranchCloseReason::Resolved,
        _ => DraftBranchCloseReason::Expired,
    }
}
fn terminal_reason(value: &str) -> DraftTerminalReason {
    match value {
        "sent" => DraftTerminalReason::Sent,
        "discarded" => DraftTerminalReason::Discarded,
        _ => DraftTerminalReason::Expired,
    }
}

pub(super) async fn put(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path((draft_id, revision_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<PutDraftRevisionRequest>,
) -> ApiResult<Json<PutDraftRevisionResponse>> {
    let principal = auth_with_mode(&s, &headers).await?;
    authorize(&s, &principal, Operation::Upload).await?;
    request.validate().map_err(|error| {
        ApiError(
            if matches!(
                error,
                peppy_hosted_api::drafts::DraftErrorCode::DraftRecordTooLarge
            ) {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            },
            if matches!(
                error,
                peppy_hosted_api::drafts::DraftErrorCode::DraftRecordTooLarge
            ) {
                "draft_record_too_large"
            } else {
                "draft_invalid_request"
            },
        )
    })?;
    let sealed = URL_SAFE_NO_PAD
        .decode(&request.sealed_record)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "draft_invalid_request"))?;
    let mut tx = scope::begin(&s.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "draft_put_begin"))?;
    lock_vault(&mut tx, principal.vault).await?;
    revalidate_principal(&mut tx, &principal).await?;
    withdraw_legacy_retention_mutation(&mut tx, &headers, principal.vault, principal.device)
        .await?;
    if let Some(row) = sqlx::query("SELECT b.branch_id,r.conflict,to_char(r.expires_at_at_accept AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') expires_at_at_accept,b.state FROM draft_revisions r JOIN draft_branches b ON b.vault_id=r.vault_id AND b.branch_id=r.branch_id WHERE r.vault_id=$1 AND r.draft_id=$2 AND r.revision_id=$3").bind(principal.vault).bind(draft_id).bind(revision_id).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_duplicate"))? { let expires_at = timestamp(&row, "expires_at_at_accept"); return Ok(Json(PutDraftRevisionResponse { branch_id: row.get("branch_id"), revision_id, expires_at, conflict: row.get("conflict"), duplicate: true })); }
    if let Some(reason) = sqlx::query_scalar::<_, String>(
        "SELECT reason FROM draft_terminals WHERE vault_id=$1 AND draft_id=$2 FOR UPDATE",
    )
    .bind(principal.vault)
    .bind(draft_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "draft_put_terminal"))?
    {
        return Err(terminal_error(&reason));
    }
    let (unexpired, past_deadline): (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE expires_at>now()),count(*) FILTER (WHERE expires_at<=now()) FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live'")
        .bind(principal.vault)
        .bind(draft_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "draft_put_deadline"))?;
    if unexpired == 0 && past_deadline > 0 {
        return Err(ApiError(StatusCode::GONE, "draft_expired"));
    }
    let attachments: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM attachments WHERE vault_id=$1 AND attachment_id=ANY($2)",
    )
    .bind(principal.vault)
    .bind(&request.attachment_ids)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "draft_put_attachments"))?;
    if attachments != request.attachment_ids.len() as i64 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "draft_attachment_unavailable",
        ));
    }
    let base = request.base_revision_id;
    let target = sqlx::query("SELECT branch_id,to_char(expires_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') expires_at FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND head_revision_id=$3 AND state='live' AND expires_at>now() FOR UPDATE").bind(principal.vault).bind(draft_id).bind(base).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_head"))?;
    let (branch_id, conflict, expires_at, prior_head) = if let Some(row) = target {
        (
            row.get("branch_id"),
            false,
            if request.authored {
                sqlx::query_scalar("SELECT to_char((now()+interval '30 days') AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|error| database_unavailable(&error, "draft_put_expiry"))?
            } else {
                row.get("expires_at")
            },
            base,
        )
    } else {
        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live' AND expires_at>now()").bind(principal.vault).bind(draft_id).fetch_one(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_count"))?;
        if live >= 3 {
            return Err(ApiError(StatusCode::CONFLICT, "draft_branch_limit"));
        }
        sqlx::query("INSERT INTO draft_incarnations(vault_id,draft_id) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(principal.vault).bind(draft_id).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_incarnation"))?;
        (
            Uuid::new_v4(),
            live > 0,
            sqlx::query_scalar("SELECT to_char((now()+interval '30 days') AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| database_unavailable(&error, "draft_put_new_expiry"))?,
            None,
        )
    };
    let seq = next_seq(&mut tx, principal.vault).await?;
    sqlx::query("INSERT INTO draft_branches(vault_id,draft_id,branch_id,head_revision_id,state,expires_at,change_seq) VALUES($1,$2,$3,$4,'live',$5::timestamptz,$6) ON CONFLICT(vault_id,branch_id) DO UPDATE SET head_revision_id=EXCLUDED.head_revision_id,expires_at=EXCLUDED.expires_at,change_seq=EXCLUDED.change_seq").bind(principal.vault).bind(draft_id).bind(branch_id).bind(revision_id).bind(&expires_at).bind(seq).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_branch"))?;
    if let Some(prior) = prior_head {
        sqlx::query("UPDATE draft_revisions SET sealed_record=NULL WHERE vault_id=$1 AND draft_id=$2 AND revision_id=$3 AND sealed_record IS NOT NULL").bind(principal.vault).bind(draft_id).bind(prior).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_clear_head"))?;
    }
    sqlx::query("INSERT INTO draft_revisions(vault_id,draft_id,revision_id,branch_id,parent_revision_id,producer_device_id,sealed_record,attachment_ids,authored,conflict,expires_at_at_accept) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::timestamptz)").bind(principal.vault).bind(draft_id).bind(revision_id).bind(branch_id).bind(base).bind(principal.device).bind(sealed).bind(&request.attachment_ids).bind(request.authored).bind(conflict).bind(&expires_at).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_revision"))?;
    if !request.resolves_branch_ids.is_empty() {
        if request.resolves_branch_ids.contains(&branch_id) {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "draft_invalid_request",
            ));
        }
        let resolvable: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND branch_id=ANY($3) AND state='live'").bind(principal.vault).bind(draft_id).bind(&request.resolves_branch_ids).fetch_one(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_resolve_check"))?;
        if resolvable != request.resolves_branch_ids.len() as i64 {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "draft_invalid_request",
            ));
        }
        for resolved_branch in &request.resolves_branch_ids {
            let close_seq = next_seq(&mut tx, principal.vault).await?;
            sqlx::query("UPDATE draft_branches SET state='closed',close_reason='resolved',change_seq=$4 WHERE vault_id=$1 AND draft_id=$2 AND branch_id=$3").bind(principal.vault).bind(draft_id).bind(resolved_branch).bind(close_seq).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_resolve"))?;
        }
        sqlx::query("UPDATE draft_revisions SET sealed_record=NULL WHERE vault_id=$1 AND draft_id=$2 AND branch_id=ANY($3) AND sealed_record IS NOT NULL").bind(principal.vault).bind(draft_id).bind(&request.resolves_branch_ids).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_resolve_clear"))?;
    }
    for attachment_id in &request.attachment_ids {
        sqlx::query("INSERT INTO draft_attachment_holds(vault_id,draft_id,attachment_id,kind,expires_at) VALUES($1,$2,$3,'draft',$4::timestamptz) ON CONFLICT(vault_id,draft_id,attachment_id,kind) DO UPDATE SET expires_at=GREATEST(draft_attachment_holds.expires_at,EXCLUDED.expires_at)").bind(principal.vault).bind(draft_id).bind(attachment_id).bind(&expires_at).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_put_hold"))?;
    }
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "draft_put_commit"))?;
    let _ = s.drafts_changed.send(principal.vault);
    Ok(Json(PutDraftRevisionResponse {
        branch_id,
        revision_id,
        expires_at,
        conflict,
        duplicate: false,
    }))
}

pub(super) async fn send(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(draft_id): Path<Uuid>,
    Json(request): Json<AcceptDraftSendRequest>,
) -> ApiResult<Json<AcceptDraftSendResponse>> {
    let principal = auth_with_mode(&s, &headers).await?;
    authorize(&s, &principal, Operation::Send).await?;
    let mut tx = scope::begin(&s.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "draft_send_begin"))?;
    lock_vault(&mut tx, principal.vault).await?;
    revalidate_principal(&mut tx, &principal).await?;
    if let Some((reason, command_id)) = sqlx::query_as::<_, (String, Option<Uuid>)>("SELECT reason,command_id FROM draft_terminals WHERE vault_id=$1 AND draft_id=$2 FOR UPDATE").bind(principal.vault).bind(draft_id).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_terminal"))? { if reason == "sent" { if command_id == Some(request.command_id) { return Ok(Json(AcceptDraftSendResponse { command_id: request.command_id, duplicate: true })); } return Err(ApiError(StatusCode::CONFLICT, "draft_already_sent")); } return Err(terminal_error(&reason)); }
    let attachments: Option<Vec<Uuid>> = sqlx::query_scalar("SELECT attachment_ids FROM draft_revisions r JOIN draft_branches b ON b.vault_id=r.vault_id AND b.branch_id=r.branch_id AND b.head_revision_id=r.revision_id WHERE r.vault_id=$1 AND r.draft_id=$2 AND r.revision_id=$3 AND b.state='live' AND b.expires_at>now() FOR UPDATE OF b").bind(principal.vault).bind(draft_id).bind(request.revision_id).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_head"))?;
    let Some(attachments) = attachments else {
        return Err(ApiError(StatusCode::CONFLICT, "draft_revision_not_head"));
    };
    let seq = next_seq(&mut tx, principal.vault).await?;
    sqlx::query("INSERT INTO draft_terminals(vault_id,draft_id,reason,command_id,change_seq) VALUES($1,$2,'sent',$3,$4)").bind(principal.vault).bind(draft_id).bind(request.command_id).bind(seq).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_terminal_insert"))?;
    let branches: Vec<Uuid> = sqlx::query_scalar("SELECT branch_id FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live' ORDER BY branch_id")
        .bind(principal.vault).bind(draft_id).fetch_all(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_branches"))?;
    for branch in branches {
        let close_seq = next_seq(&mut tx, principal.vault).await?;
        sqlx::query("UPDATE draft_branches SET state='closed',close_reason='superseded',change_seq=$4 WHERE vault_id=$1 AND draft_id=$2 AND branch_id=$3").bind(principal.vault).bind(draft_id).bind(branch).bind(close_seq).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_close"))?;
    }
    sqlx::query("UPDATE draft_revisions SET sealed_record=NULL WHERE vault_id=$1 AND draft_id=$2 AND sealed_record IS NOT NULL").bind(principal.vault).bind(draft_id).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_clear"))?;
    sqlx::query("UPDATE draft_attachment_holds SET expires_at=now() WHERE vault_id=$1 AND draft_id=$2 AND kind IN ('draft','staging')").bind(principal.vault).bind(draft_id).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_lapse"))?;
    for attachment_id in attachments {
        sqlx::query("INSERT INTO draft_attachment_holds(vault_id,draft_id,attachment_id,kind,expires_at) VALUES($1,$2,$3,'send',now()+interval '30 days') ON CONFLICT(vault_id,draft_id,attachment_id,kind) DO UPDATE SET expires_at=EXCLUDED.expires_at").bind(principal.vault).bind(draft_id).bind(attachment_id).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_send_hold"))?;
    }
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "draft_send_commit"))?;
    let _ = s.drafts_changed.send(principal.vault);
    Ok(Json(AcceptDraftSendResponse {
        command_id: request.command_id,
        duplicate: false,
    }))
}

pub(super) async fn discard(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(draft_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let principal = auth_with_mode(&s, &headers).await?;
    authorize(&s, &principal, Operation::AttachmentDelete).await?;
    let mut tx = scope::begin(&s.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "draft_discard_begin"))?;
    lock_vault(&mut tx, principal.vault).await?;
    revalidate_principal(&mut tx, &principal).await?;
    if let Some(reason) = sqlx::query_scalar::<_, String>(
        "SELECT reason FROM draft_terminals WHERE vault_id=$1 AND draft_id=$2 FOR UPDATE",
    )
    .bind(principal.vault)
    .bind(draft_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "draft_discard_terminal"))?
    {
        if reason == "discarded" {
            return Ok(StatusCode::NO_CONTENT);
        }
        return Err(terminal_error(&reason));
    }
    let seq = next_seq(&mut tx, principal.vault).await?;
    sqlx::query("INSERT INTO draft_terminals(vault_id,draft_id,reason,change_seq) VALUES($1,$2,'discarded',$3)").bind(principal.vault).bind(draft_id).bind(seq).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_discard_terminal_insert"))?;
    sqlx::query("UPDATE draft_attachment_holds SET expires_at=now() WHERE vault_id=$1 AND draft_id=$2 AND kind IN ('draft','staging')").bind(principal.vault).bind(draft_id).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "draft_discard_lapse"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "draft_discard_commit"))?;
    let _ = s.drafts_changed.send(principal.vault);
    Ok(StatusCode::NO_CONTENT)
}
