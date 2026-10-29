//! Enrollment persistence helpers shared by community and hosted bootstrap paths.

pub mod computers;
pub mod invitations;
pub mod manual;
pub mod owner_authorization;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_hosted_api::enrollment::ClientKind;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Client-held owner metadata registered in the same transaction as
/// [`crate::api::bootstrap_owner`]. It intentionally contains public keys only.
pub struct OwnerEnrollmentMetadata {
    pub admission_public_key: [u8; 32],
    pub signing_public_key: [u8; 32],
    pub client_kind: ClientKind,
    pub device_name: String,
}

/// Records the admission verifier and first device metadata after owner
/// bootstrap, without changing the long-lived [`crate::api::OwnerBootstrap`]
/// primitive used by existing callers.
pub async fn register_owner_enrollment_metadata(
    transaction: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
    key_epoch: u32,
    metadata: OwnerEnrollmentMetadata,
) -> Result<(), String> {
    validate_device_name(&metadata.device_name)?;
    let client_kind = client_kind_name(metadata.client_kind);
    let updated = sqlx::query(
        "UPDATE devices
         SET name = $3, kind = $4, public_key = jsonb_build_object('ed25519_public_key', $5)
         WHERE vault_id = $1 AND device_id = $2 AND role = 'owner'",
    )
    .bind(vault_id)
    .bind(device_id)
    .bind(&metadata.device_name)
    .bind(client_kind)
    .bind(URL_SAFE_NO_PAD.encode(metadata.signing_public_key))
    .execute(&mut **transaction)
    .await
    .map_err(|error| error.to_string())?;
    if updated.rows_affected() != 1 {
        return Err("owner bootstrap device is missing".into());
    }
    let inserted = sqlx::query(
        "INSERT INTO vault_admission_keys(vault_id, key_epoch, profile_fingerprint, admission_public_key)
         SELECT vault_id, key_epoch, profile_fingerprint, $3
         FROM vaults
         WHERE vault_id = $1 AND key_epoch = $2
         ON CONFLICT (vault_id, key_epoch) DO NOTHING",
    )
    .bind(vault_id)
    .bind(key_epoch as i32)
    .bind(metadata.admission_public_key.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(|error| error.to_string())?;
    if inserted.rows_affected() != 1 {
        return Err("owner admission key already exists or profile epoch is invalid".into());
    }
    Ok(())
}

pub fn validate_device_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("device name must be 1 to 128 UTF-8 bytes without controls".into());
    }
    Ok(())
}

pub fn client_kind_name(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::Browser => "browser",
        ClientKind::Desktop => "desktop",
        ClientKind::Android => "android",
        ClientKind::Ios => "ios",
    }
}

#[cfg(test)]
mod tests {
    use super::validate_device_name;

    #[test]
    fn device_name_rejects_controls_and_overlong_values() {
        assert!(validate_device_name("phone").is_ok());
        assert!(validate_device_name("").is_err());
        assert!(validate_device_name("phone\n").is_err());
        assert!(validate_device_name(&"a".repeat(129)).is_err());
    }
}
