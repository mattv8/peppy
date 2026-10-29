use axum::http::HeaderMap;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct OwnerCredential {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub token_digest: Vec<u8>,
}

pub enum OwnerAuthorizationError {
    InvalidBearer,
    OwnerRequired,
    Database(sqlx::Error),
}

pub async fn resolve_owner_credential(
    db: &PgPool,
    hosted: bool,
    headers: &HeaderMap,
) -> Result<OwnerCredential, OwnerAuthorizationError> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| value.len() == 96 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or(OwnerAuthorizationError::InvalidBearer)?;
    let query = if hosted {
        "SELECT vault_id,device_id,token_digest FROM peppy.resolve_device_credential($1) WHERE role='owner'"
    } else {
        "SELECT c.vault_id,c.device_id,c.token_digest FROM device_credentials c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.token_digest=$1 AND c.revoked_at IS NULL AND d.revoked_at IS NULL AND d.role='owner'"
    };
    let digest = Sha256::digest(token.as_bytes());
    let row = sqlx::query(query)
        .bind(digest.as_slice())
        .fetch_optional(db)
        .await
        .map_err(OwnerAuthorizationError::Database)?
        .ok_or(OwnerAuthorizationError::OwnerRequired)?;
    Ok(OwnerCredential {
        vault_id: row.get("vault_id"),
        device_id: row.get("device_id"),
        token_digest: row.get("token_digest"),
    })
}
