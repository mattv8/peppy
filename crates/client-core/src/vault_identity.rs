use crate::{Error, Store};
use peppy_crypto::vault_identity::{
    RcsIdentityKey, VaultIdentityAnchor, VaultIdentityRecoveryRecord,
};
use peppy_protocol::{
    EstablishVaultIdentityV1, VAULT_IDENTITY_PROTOCOL_VERSION, VaultIdentityStatusV1,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const IDENTITY_UNAVAILABLE: &str = "identity_unavailable";
pub const IDENTITY_CONFLICT: &str = "identity_conflict";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultIdentitySnapshotState {
    Absent,
    Pending,
    Accepted,
    Conflict,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultIdentitySnapshot {
    pub state: VaultIdentitySnapshotState,
    pub ready: bool,
    pub needs_fetch: bool,
    pub operation_id: Option<String>,
    pub key_epoch: Option<u32>,
    pub unavailable_code: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultIdentityReadiness {
    Ready {
        key_epoch: u32,
        operation_id: String,
    },
    Unavailable {
        code: String,
    },
}
pub struct VaultIdentityPendingMaterial {
    pub operation_id: String,
    pub protected_pending_bytes: Zeroizing<Vec<u8>>,
    pub establish_body_json: String,
}
pub enum VaultIdentityAdoption {
    AbsentEligible,
    AbsentNotEligible,
    Adopted {
        operation_id: String,
        protected_accepted_bytes: Zeroizing<Vec<u8>>,
        pending_cleanup: bool,
    },
    Conflict,
    Unavailable {
        code: String,
    },
}

pub(crate) struct StagedPending {
    pub operation: Uuid,
    pub epoch: u32,
    pub fingerprint: String,
    pub anchor: VaultIdentityAnchor,
}

pub(crate) struct StagedAnchor {
    pub operation: Uuid,
    pub epoch: u32,
    pub fingerprint: String,
    pub anchor: VaultIdentityAnchor,
}

pub(crate) fn snapshot(store: &Store) -> Result<VaultIdentitySnapshot, Error> {
    let row: Option<(String, String, u32)> = store
        .conn
        .query_row(
            "SELECT state,operation_id,key_epoch FROM vault_identity_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let state = match row.as_ref().map(|r| r.0.as_str()) {
        None => VaultIdentitySnapshotState::Absent,
        Some("pending") => VaultIdentitySnapshotState::Pending,
        Some("accepted") => VaultIdentitySnapshotState::Accepted,
        _ => VaultIdentitySnapshotState::Conflict,
    };
    let ready =
        rcs_identity_key(store).is_ok() && matches!(state, VaultIdentitySnapshotState::Accepted);
    Ok(VaultIdentitySnapshot {
        state: state.clone(),
        ready,
        needs_fetch: !ready,
        operation_id: row.as_ref().map(|r| r.1.clone()),
        key_epoch: row.map(|r| r.2),
        unavailable_code: (!ready).then(|| {
            if matches!(state, VaultIdentitySnapshotState::Conflict) {
                IDENTITY_CONFLICT.into()
            } else {
                IDENTITY_UNAVAILABLE.into()
            }
        }),
    })
}

pub(crate) fn begin(store: &mut Store) -> Result<VaultIdentityPendingMaterial, Error> {
    if !matches!(snapshot(store)?.state, VaultIdentitySnapshotState::Absent) {
        return Err(Error::InvalidRequest("vault identity already established"));
    }
    let epoch = store.active_epoch.ok_or(Error::KeysUnavailable)?;
    if !store.identity_bootstrap_epochs.contains(&epoch) {
        return Err(Error::KeysUnavailable);
    }
    let keys = store.keys.get(&epoch).ok_or(Error::KeysUnavailable)?;
    let recovery = store
        .identity_recovery_keys
        .get(&epoch)
        .ok_or(Error::KeysUnavailable)?;
    let operation = Uuid::new_v4();
    let anchor =
        VaultIdentityAnchor::generate(store.config.vault_id.0, operation, store.config.device_id.0)
            .map_err(|_| Error::Crypto)?;
    let record = VaultIdentityRecoveryRecord::wrap(&anchor, recovery, &keys.profile)
        .map_err(|_| Error::Crypto)?;
    let dto = EstablishVaultIdentityV1 {
        protocol_version: VAULT_IDENTITY_PROTOCOL_VERSION,
        operation_id: operation,
        key_epoch: epoch,
        profile_fingerprint: keys.fingerprint.clone(),
        recovery_record: peppy_protocol::VaultIdentityRecoveryRecordV1::from_bytes(record.encode())
            .map_err(|_| Error::Crypto)?,
    };
    let protected_pending_bytes =
        anchor.with_protected_bytes(|bytes| Zeroizing::new(bytes.to_vec()));
    store.staged_pending = Some(StagedPending {
        operation,
        epoch,
        fingerprint: keys.fingerprint.clone(),
        anchor,
    });
    Ok(VaultIdentityPendingMaterial {
        operation_id: operation.to_string(),
        protected_pending_bytes,
        establish_body_json: serde_json::to_string(&dto).map_err(|_| Error::Database)?,
    })
}

pub(crate) fn confirm(store: &mut Store, operation: &str) -> Result<(), Error> {
    let operation = Uuid::parse_str(operation)
        .map_err(|_| Error::InvalidRequest("invalid vault identity operation"))?;
    let staged = store
        .staged_pending
        .as_ref()
        .ok_or(Error::InvalidRequest("missing protected pending identity"))?;
    if staged.operation != operation {
        return Err(Error::InvalidRequest(
            "protected pending operation mismatch",
        ));
    }
    let pin = staged.anchor.pin().with_bytes(|value| value.to_vec());
    let existing: Option<(String, Vec<u8>)> = store
        .conn
        .query_row(
            "SELECT operation_id,pin FROM vault_identity_state WHERE id=1 AND state='pending'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if existing
        .as_ref()
        .is_some_and(|(existing_operation, existing_pin)| {
            existing_operation == &operation.to_string() && existing_pin == &pin
        })
    {
        return Ok(());
    }
    store.conn.execute("INSERT INTO vault_identity_state(id,state,operation_id,key_epoch,profile_fingerprint,pin,conflict_evidence,updated_at_ms) VALUES(1,'pending',?,?,?,?,NULL,0)", params![operation.to_string(),staged.epoch,staged.fingerprint,pin])?;
    Ok(())
}

pub(crate) fn adopt(store: &mut Store, input: &str) -> Result<VaultIdentityAdoption, Error> {
    let status: VaultIdentityStatusV1 = serde_json::from_str(input)
        .map_err(|_| Error::InvalidRequest("invalid vault identity response"))?;
    match status {
        VaultIdentityStatusV1::Absent { protocol_version } => {
            let local_state = snapshot(store)?.state;
            Ok(if protocol_version != VAULT_IDENTITY_PROTOCOL_VERSION {
                VaultIdentityAdoption::Unavailable {
                    code: IDENTITY_UNAVAILABLE.into(),
                }
            } else if matches!(local_state, VaultIdentitySnapshotState::Absent)
                && store.active_epoch.is_some_and(|epoch| {
                    store.identity_bootstrap_epochs.contains(&epoch)
                        && store.identity_recovery_keys.contains_key(&epoch)
                })
            {
                VaultIdentityAdoption::AbsentEligible
            } else {
                VaultIdentityAdoption::AbsentNotEligible
            })
        }
        VaultIdentityStatusV1::Established {
            protocol_version,
            operation_id,
            key_epoch,
            profile_fingerprint,
            recovery_record,
        } => {
            if protocol_version != VAULT_IDENTITY_PROTOCOL_VERSION {
                return Ok(VaultIdentityAdoption::Unavailable {
                    code: IDENTITY_UNAVAILABLE.into(),
                });
            }
            let profile = store
                .keys
                .get(&key_epoch)
                .filter(|keys| keys.fingerprint == profile_fingerprint)
                .map(|keys| keys.profile.clone());
            let key = store.identity_recovery_keys.get(&key_epoch);
            let (Some(profile), Some(key)) = (profile, key) else {
                return Ok(VaultIdentityAdoption::Unavailable {
                    code: IDENTITY_UNAVAILABLE.into(),
                });
            };
            let record = VaultIdentityRecoveryRecord::decode(recovery_record.as_bytes())
                .map_err(|_| Error::InvalidRequest("invalid vault identity response"))?;
            let anchor = record
                .open(key, &profile, store.config.vault_id.0)
                .map_err(|_| Error::Crypto)?;
            if anchor.operation_id().map_err(|_| Error::Crypto)? != operation_id {
                return Err(Error::InvalidRequest(
                    "vault identity operation does not match anchor",
                ));
            }
            let pin = anchor.pin().with_bytes(|value| value.to_vec());
            let existing: Option<(String, Vec<u8>)> = store
                .conn
                .query_row(
                    "SELECT state,pin FROM vault_identity_state WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if existing
                .as_ref()
                .is_some_and(|(state, _)| state == "conflict")
            {
                clear_identity_material(store);
                return Ok(VaultIdentityAdoption::Conflict);
            }
            if existing
                .as_ref()
                .is_some_and(|(state, value)| state != "pending" && value != &pin)
            {
                store.conn.execute(
                    "UPDATE vault_identity_state SET state='conflict', conflict_evidence=?, updated_at_ms=0 WHERE id=1",
                    [recovery_record.as_bytes()],
                )?;
                clear_identity_material(store);
                return Ok(VaultIdentityAdoption::Conflict);
            }
            let pending_cleanup = existing
                .as_ref()
                .is_some_and(|(state, _)| state == "pending");
            store.staged_identity = Some(StagedAnchor {
                operation: operation_id,
                epoch: key_epoch,
                fingerprint: profile_fingerprint,
                anchor,
            });
            let protected_accepted_bytes = store
                .staged_identity
                .as_ref()
                .ok_or(Error::Database)?
                .anchor
                .with_protected_bytes(|value| Zeroizing::new(value.to_vec()));
            Ok(VaultIdentityAdoption::Adopted {
                operation_id: operation_id.to_string(),
                protected_accepted_bytes,
                pending_cleanup,
            })
        }
    }
}

pub(crate) fn commit(store: &mut Store, operation: &str) -> Result<(), Error> {
    let operation = Uuid::parse_str(operation)
        .map_err(|_| Error::InvalidRequest("invalid vault identity operation"))?;
    let state: Option<String> = store
        .conn
        .query_row(
            "SELECT state FROM vault_identity_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if state.as_deref() == Some("conflict") {
        clear_identity_material(store);
        return Err(Error::Conflict);
    }
    let staged = store
        .staged_identity
        .take()
        .ok_or(Error::InvalidRequest("missing accepted identity"))?;
    if staged.operation != operation {
        return Err(Error::InvalidRequest("accepted operation mismatch"));
    }
    let pin = staged.anchor.pin().with_bytes(|value| value.to_vec());
    let rcs_identity_key = staged.anchor.derive_rcs_key().map_err(|_| Error::Crypto)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(String, Vec<u8>)> = tx
        .query_row(
            "SELECT state,pin FROM vault_identity_state WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if existing
        .as_ref()
        .is_some_and(|(state, _)| state == "conflict")
    {
        drop(tx);
        clear_identity_material(store);
        return Err(Error::Conflict);
    }
    if existing
        .as_ref()
        .is_some_and(|(state, existing_pin)| state == "accepted" && existing_pin != &pin)
    {
        tx.execute(
            "UPDATE vault_identity_state SET state='conflict', conflict_evidence=NULL, updated_at_ms=0 WHERE id=1",
            [],
        )?;
        tx.commit()?;
        clear_identity_material(store);
        return Err(Error::Conflict);
    }
    tx.execute("INSERT INTO vault_identity_state(id,state,operation_id,key_epoch,profile_fingerprint,pin,conflict_evidence,updated_at_ms) VALUES(1,'accepted',?,?,?,?,NULL,0) ON CONFLICT(id) DO UPDATE SET state='accepted',operation_id=excluded.operation_id,key_epoch=excluded.key_epoch,profile_fingerprint=excluded.profile_fingerprint,pin=excluded.pin,conflict_evidence=NULL", params![operation.to_string(), staged.epoch, staged.fingerprint, pin])?;
    tx.commit()?;
    store.rcs_identity_key = Some(rcs_identity_key);
    Ok(())
}

pub(crate) fn restore(store: &mut Store, bytes: &[u8]) -> Result<(), Error> {
    let anchor = VaultIdentityAnchor::from_protected_bytes(bytes, store.config.vault_id.0)
        .map_err(|error| match error {
            peppy_crypto::CryptoError::InvalidCiphertext
            | peppy_crypto::CryptoError::UnsupportedSuite => Error::InvalidKeyCache,
            peppy_crypto::CryptoError::InvalidProfile => Error::IdentityMismatch,
            peppy_crypto::CryptoError::AuthenticationFailed => Error::Crypto,
            _ => Error::Crypto,
        })?;
    let pin = anchor.pin().with_bytes(|v| v.to_vec());
    let existing: Option<Vec<u8>> = store
        .conn
        .query_row(
            "SELECT pin FROM vault_identity_state WHERE id=1 AND state='accepted'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if existing.is_none() {
        return Err(Error::InvalidRequest("vault identity is not pinned"));
    }
    if existing.as_deref() != Some(pin.as_slice()) {
        return Err(Error::Conflict);
    }
    store.rcs_identity_key = Some(anchor.derive_rcs_key().map_err(|_| Error::Crypto)?);
    Ok(())
}

pub(crate) fn rcs_identity_key(store: &Store) -> Result<&RcsIdentityKey, Error> {
    let state: Option<String> = store
        .conn
        .query_row(
            "SELECT state FROM vault_identity_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if state.as_deref() != Some("accepted") {
        return Err(Error::KeysUnavailable);
    }
    store
        .rcs_identity_key
        .as_ref()
        .ok_or(Error::KeysUnavailable)
}

fn clear_identity_material(store: &mut Store) {
    store.staged_pending = None;
    store.staged_identity = None;
    store.rcs_identity_key = None;
}

pub(crate) fn export_recovery_key(store: &Store, epoch: u32) -> Result<Vec<u8>, Error> {
    let profile = store.keys.get(&epoch).ok_or(Error::KeysUnavailable)?;
    let key = store
        .identity_recovery_keys
        .get(&epoch)
        .ok_or(Error::KeysUnavailable)?;
    let mut bytes = Vec::with_capacity(137);
    bytes.extend_from_slice(b"PIK1");
    bytes.push(1);
    bytes.extend_from_slice(store.config.vault_id.0.as_bytes());
    bytes.extend_from_slice(store.config.device_id.0.as_bytes());
    bytes.extend_from_slice(&epoch.to_be_bytes());
    bytes.extend_from_slice(profile.fingerprint.as_bytes());
    key.with_native_cache_bytes(|value| bytes.extend_from_slice(value));
    Ok(bytes)
}
pub(crate) fn import_recovery_key(store: &mut Store, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() != 137 || &bytes[..4] != b"PIK1" || bytes[4] != 1 {
        return Err(Error::InvalidKeyCache);
    }
    if bytes[5..21] != *store.config.vault_id.0.as_bytes()
        || bytes[21..37] != *store.config.device_id.0.as_bytes()
    {
        return Err(Error::IdentityMismatch);
    }
    let epoch = u32::from_be_bytes(
        bytes[37..41]
            .try_into()
            .map_err(|_| Error::InvalidKeyCache)?,
    );
    let profile = store.keys.get(&epoch).ok_or(Error::InvalidProfile)?;
    if bytes[41..105] != *profile.fingerprint.as_bytes() {
        return Err(Error::InvalidKeyCache);
    }
    let raw = Zeroizing::new(
        bytes[105..]
            .try_into()
            .map_err(|_| Error::InvalidKeyCache)?,
    );
    store.identity_recovery_keys.insert(
        epoch,
        peppy_crypto::PurposeKey::import_native_cache(
            *raw,
            &profile.profile,
            peppy_crypto::KeyPurpose::VaultIdentityRecovery,
        )
        .map_err(|_| Error::Crypto)?,
    );
    Ok(())
}
