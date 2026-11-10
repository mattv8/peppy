use crate::common::*;
use base64::Engine as _;
use peppy_client_core::{
    Error, VaultIdentityAdoption, VaultIdentityReadiness, VaultIdentitySnapshotState,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn unlocked_identity_client(config: &ClientConfig, vault: &Vault) -> Client {
    unlocked(config, vault)
}

fn established_status(establish_body: &str) -> String {
    let mut value: Value = serde_json::from_str(establish_body).unwrap();
    value["state"] = json!("established");
    serde_json::to_string(&value).unwrap()
}

fn establish(client: &Client) -> (String, Vec<u8>) {
    let pending = client.begin_vault_identity_establishment().unwrap();
    client
        .confirm_vault_identity_pending_durable(pending.operation_id.clone())
        .unwrap();
    let status = established_status(&pending.establish_body_json);
    let (operation_id, accepted) = match client.adopt_vault_identity(status).unwrap() {
        VaultIdentityAdoption::Adopted {
            operation_id,
            protected_accepted_bytes,
            pending_cleanup,
        } => {
            assert!(pending_cleanup);
            (operation_id, protected_accepted_bytes.to_vec())
        }
        _ => panic!("expected adoption"),
    };
    client
        .commit_vault_identity_acceptance(operation_id.clone())
        .unwrap();
    (operation_id, accepted)
}

#[test]
fn fresh_establishment_is_pending_before_it_can_become_ready() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "fresh", &vault);
    let client = unlocked_identity_client(&config, &vault);
    let pending = client.begin_vault_identity_establishment().unwrap();
    assert_eq!(
        client.vault_identity_snapshot().unwrap().state,
        VaultIdentitySnapshotState::Absent
    );
    client
        .confirm_vault_identity_pending_durable(pending.operation_id.clone())
        .unwrap();
    assert_eq!(
        client.vault_identity_snapshot().unwrap().state,
        VaultIdentitySnapshotState::Pending
    );
    assert!(matches!(
        client.vault_identity_readiness().unwrap(),
        VaultIdentityReadiness::Unavailable { .. }
    ));
    let adopted = client
        .adopt_vault_identity(established_status(&pending.establish_body_json))
        .unwrap();
    let (operation, bytes) = match adopted {
        VaultIdentityAdoption::Adopted {
            operation_id,
            protected_accepted_bytes,
            ..
        } => (operation_id, protected_accepted_bytes),
        _ => panic!("expected adoption"),
    };
    assert!(matches!(
        client.vault_identity_readiness().unwrap(),
        VaultIdentityReadiness::Unavailable { .. }
    ));
    client
        .commit_vault_identity_acceptance(operation.clone())
        .unwrap();
    assert_eq!(bytes.len(), 147);
    assert_eq!(
        client.vault_identity_readiness().unwrap(),
        VaultIdentityReadiness::Ready {
            key_epoch: 1,
            operation_id: operation
        }
    );
}

#[test]
fn begin_without_durable_confirmation_is_absent_after_reopen_and_retry_is_fresh() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "crash", &vault);
    let first = {
        let client = unlocked_identity_client(&config, &vault);
        client
            .begin_vault_identity_establishment()
            .unwrap()
            .operation_id
    };
    let client = unlocked_identity_client(&config, &vault);
    assert_eq!(
        client.vault_identity_snapshot().unwrap().state,
        VaultIdentitySnapshotState::Absent
    );
    assert_ne!(
        first,
        client
            .begin_vault_identity_establishment()
            .unwrap()
            .operation_id
    );
}

#[test]
fn durable_acceptance_restores_only_exact_protected_bytes_after_reopen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "restore", &vault);
    let accepted = {
        let client = unlocked_identity_client(&config, &vault);
        establish(&client).1
    };
    let client = open(&config);
    assert!(!client.vault_identity_snapshot().unwrap().ready);
    client
        .restore_vault_identity_from_protected(accepted.clone())
        .unwrap();
    assert!(client.vault_identity_snapshot().unwrap().ready);
    let mut malformed = accepted;
    malformed[90] ^= 1;
    assert_eq!(
        client.restore_vault_identity_from_protected(malformed),
        Err(Error::Crypto)
    );
    assert!(client.vault_identity_snapshot().unwrap().ready);
}

#[test]
fn protected_identity_cannot_restore_without_an_accepted_pin() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source_config = config(&dir, "source", &vault);
    let bytes = unlocked_identity_client(&source_config, &vault)
        .begin_vault_identity_establishment()
        .unwrap()
        .protected_pending_bytes
        .to_vec();
    let target_config = config(&dir, "target", &vault);
    let target = open(&target_config);
    assert_eq!(
        target.restore_vault_identity_from_protected(bytes),
        Err(Error::InvalidRequest("vault identity is not pinned"))
    );
}

#[test]
fn concurrent_establishment_loser_adopts_authenticated_server_winner() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let winner = unlocked_identity_client(&config(&dir, "winner", &vault), &vault);
    let loser = unlocked_identity_client(&config(&dir, "loser", &vault), &vault);
    let winner_pending = winner.begin_vault_identity_establishment().unwrap();
    let loser_pending = loser.begin_vault_identity_establishment().unwrap();
    loser
        .confirm_vault_identity_pending_durable(loser_pending.operation_id)
        .unwrap();
    let adoption = loser
        .adopt_vault_identity(established_status(&winner_pending.establish_body_json))
        .unwrap();
    let operation = match adoption {
        VaultIdentityAdoption::Adopted {
            operation_id,
            pending_cleanup,
            ..
        } => {
            assert!(pending_cleanup);
            operation_id
        }
        _ => panic!("loser must adopt winner"),
    };
    assert_eq!(operation, winner_pending.operation_id);
    loser.commit_vault_identity_acceptance(operation).unwrap();
    assert!(loser.vault_identity_snapshot().unwrap().ready);
}

#[test]
fn same_server_value_is_idempotent_but_a_different_anchor_persists_conflict() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "pin", &vault);
    let client = unlocked_identity_client(&client_config, &vault);
    let pending = client.begin_vault_identity_establishment().unwrap();
    client
        .confirm_vault_identity_pending_durable(pending.operation_id.clone())
        .unwrap();
    let status = established_status(&pending.establish_body_json);
    let operation = match client.adopt_vault_identity(status.clone()).unwrap() {
        VaultIdentityAdoption::Adopted { operation_id, .. } => operation_id,
        _ => unreachable!(),
    };
    client.commit_vault_identity_acceptance(operation).unwrap();
    assert!(matches!(
        client.adopt_vault_identity(status).unwrap(),
        VaultIdentityAdoption::Adopted { .. }
    ));

    let rival = unlocked_identity_client(&config(&dir, "rival", &vault), &vault);
    let rival_status = established_status(
        &rival
            .begin_vault_identity_establishment()
            .unwrap()
            .establish_body_json,
    );
    assert!(matches!(
        client.adopt_vault_identity(rival_status).unwrap(),
        VaultIdentityAdoption::Conflict
    ));
    assert_eq!(
        client
            .vault_identity_snapshot()
            .unwrap()
            .unavailable_code
            .as_deref(),
        Some("identity_conflict")
    );
}

#[test]
fn malformed_unknown_and_absent_statuses_are_not_conflated() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked_identity_client(&config(&dir, "dto", &vault), &vault);
    assert!(matches!(
        client
            .adopt_vault_identity(r#"{"state":"absent","protocol_version":1}"#.into())
            .unwrap(),
        VaultIdentityAdoption::AbsentEligible
    ));
    assert!(matches!(
        client
            .adopt_vault_identity(r#"{"state":"absent","protocol_version":1,"extra":true}"#.into()),
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        client.adopt_vault_identity("not-json".into()),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn pik1_export_import_is_fixed_framed_and_rejects_length_version_and_device() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "pik", &vault);
    let client = unlocked_identity_client(&client_config, &vault);
    let bytes = client
        .export_vault_identity_recovery_key_for_native_storage(1)
        .unwrap();
    assert_eq!((&bytes[..5], bytes.len()), (&b"PIK1\x01"[..], 137));
    client
        .import_vault_identity_recovery_key_from_native_storage(bytes.clone())
        .unwrap();
    for mut bad in [bytes[..136].to_vec(), {
        let mut b = bytes.clone();
        b[4] = 2;
        b
    }] {
        assert_eq!(
            client.import_vault_identity_recovery_key_from_native_storage(std::mem::take(&mut bad)),
            Err(Error::InvalidKeyCache)
        );
    }
    let mut wrong_vault = bytes.clone();
    wrong_vault[5] ^= 1;
    assert_eq!(
        client.import_vault_identity_recovery_key_from_native_storage(wrong_vault),
        Err(Error::IdentityMismatch)
    );
    let mut wrong_epoch = bytes.clone();
    wrong_epoch[37..41].copy_from_slice(&2_u32.to_be_bytes());
    assert_eq!(
        client.import_vault_identity_recovery_key_from_native_storage(wrong_epoch),
        Err(Error::InvalidProfile)
    );
    let mut wrong_fingerprint = bytes.clone();
    wrong_fingerprint[41] = if wrong_fingerprint[41] == b'a' {
        b'b'
    } else {
        b'a'
    };
    assert_eq!(
        client.import_vault_identity_recovery_key_from_native_storage(wrong_fingerprint),
        Err(Error::InvalidKeyCache)
    );
    let other = unlocked_identity_client(&config(&dir, "other-device", &vault), &vault);
    assert_eq!(
        other.import_vault_identity_recovery_key_from_native_storage(bytes),
        Err(Error::IdentityMismatch)
    );
}

#[test]
fn operation_id_must_match_the_anchor_staged_by_begin() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked_identity_client(&config(&dir, "operation", &vault), &vault);
    client.begin_vault_identity_establishment().unwrap();
    assert_eq!(
        client.confirm_vault_identity_pending_durable(uuid::Uuid::new_v4().to_string()),
        Err(Error::InvalidRequest(
            "protected pending operation mismatch"
        ))
    );
    assert_eq!(
        client.vault_identity_snapshot().unwrap().state,
        VaultIdentitySnapshotState::Absent
    );
}

#[test]
fn identical_pending_confirmation_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked_identity_client(&config(&dir, "retry", &vault), &vault);
    let pending = client.begin_vault_identity_establishment().unwrap();
    client
        .confirm_vault_identity_pending_durable(pending.operation_id.clone())
        .unwrap();
    client
        .confirm_vault_identity_pending_durable(pending.operation_id.clone())
        .unwrap();
    assert_eq!(
        client
            .vault_identity_snapshot()
            .unwrap()
            .operation_id
            .as_deref(),
        Some(pending.operation_id.as_str())
    );
}

#[test]
fn sqlcipher_identity_row_contains_only_non_secret_state() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "at-rest", &vault);
    let (protected, serialized_state, debug_state) = {
        let client = unlocked_identity_client(&client_config, &vault);
        let pending = client.begin_vault_identity_establishment().unwrap();
        let protected = pending.protected_pending_bytes.to_vec();
        client
            .confirm_vault_identity_pending_durable(pending.operation_id)
            .unwrap();
        let snapshot = client.vault_identity_snapshot().unwrap();
        (
            protected,
            pending.establish_body_json,
            format!("{snapshot:?}"),
        )
    };
    let encoded_secret = base64::engine::general_purpose::STANDARD.encode(&protected);
    assert!(!serialized_state.contains(&encoded_secret));
    assert!(!debug_state.contains(&encoded_secret));
    let connection = rusqlite::Connection::open(&client_config.database_path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA key = \"x'{}'\"; PRAGMA cipher_compatibility = 4;",
            DB_KEY
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))
        .unwrap();
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(vault_identity_state)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "id",
            "state",
            "operation_id",
            "key_epoch",
            "profile_fingerprint",
            "pin",
            "conflict_evidence",
            "updated_at_ms"
        ]
    );
    let (pin_length, evidence): (usize, Option<Vec<u8>>) = connection
        .query_row(
            "SELECT length(pin),conflict_evidence FROM vault_identity_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(pin_length, 32);
    assert!(evidence.is_none());
    let database_bytes = std::fs::read(&client_config.database_path).unwrap();
    assert!(
        !database_bytes
            .windows(protected.len())
            .any(|window| window == protected)
    );
}

#[test]
fn independently_built_schema_version_50_fixture_upgrades_to_53() {
    let dir = TempDir::new().unwrap();
    let database_path = dir.path().join("upgrade.db");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/client_schema_v50.sqlcipher-fixture"
        ),
        &database_path,
    )
    .unwrap();
    let client_config = ClientConfig {
        database_path: database_path.clone(),
        vault_id: VaultId(uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap()),
        device_id: DeviceId(uuid::Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap()),
    };
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA key = \"x'{}'\"; PRAGMA cipher_compatibility = 4;",
            DB_KEY
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))
        .unwrap();
    let source_version: i64 = connection
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    let source_has_identity_table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='vault_identity_state')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((source_version, source_has_identity_table), (50, false));
    drop(connection);

    drop(open(&client_config));
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA key = \"x'{}'\"; PRAGMA cipher_compatibility = 4;",
            DB_KEY
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))
        .unwrap();
    let version: i64 = connection
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    let table: String = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='vault_identity_state'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((version, table.as_str()), (53, "vault_identity_state"));
}

#[test]
fn conflict_is_terminal_and_clears_staged_readiness_material() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked_identity_client(&config(&dir, "terminal-conflict", &vault), &vault);
    let pending = client.begin_vault_identity_establishment().unwrap();
    client
        .confirm_vault_identity_pending_durable(pending.operation_id.clone())
        .unwrap();
    let accepted_status = established_status(&pending.establish_body_json);
    let operation = match client
        .adopt_vault_identity(accepted_status.clone())
        .unwrap()
    {
        VaultIdentityAdoption::Adopted { operation_id, .. } => operation_id,
        _ => panic!("expected accepted anchor"),
    };
    client
        .commit_vault_identity_acceptance(operation.clone())
        .unwrap();
    assert!(matches!(
        client
            .adopt_vault_identity(accepted_status.clone())
            .unwrap(),
        VaultIdentityAdoption::Adopted { .. }
    ));

    let rival = unlocked_identity_client(&config(&dir, "terminal-rival", &vault), &vault);
    let conflicting_status = established_status(
        &rival
            .begin_vault_identity_establishment()
            .unwrap()
            .establish_body_json,
    );
    assert!(matches!(
        client.adopt_vault_identity(conflicting_status).unwrap(),
        VaultIdentityAdoption::Conflict
    ));
    assert_eq!(
        client.commit_vault_identity_acceptance(operation),
        Err(Error::Conflict)
    );
    assert!(matches!(
        client.adopt_vault_identity(accepted_status).unwrap(),
        VaultIdentityAdoption::Conflict
    ));
    assert_eq!(
        client.vault_identity_readiness().unwrap(),
        VaultIdentityReadiness::Unavailable {
            code: "identity_conflict".into()
        }
    );
}

#[test]
fn cache_only_recovery_authenticates_existing_anchor_but_cannot_bootstrap() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "cache-only", &vault);
    let (native_cache, identity_key, established) = {
        let client = unlocked_identity_client(&client_config, &vault);
        let pending = client.begin_vault_identity_establishment().unwrap();
        (
            client
                .export_native_key_cache(1)
                .unwrap()
                .native_storage_bytes()
                .to_vec(),
            client
                .export_vault_identity_recovery_key_for_native_storage(1)
                .unwrap(),
            established_status(&pending.establish_body_json),
        )
    };
    let client = open(&client_config);
    client
        .import_native_key_cache(&NativeKeyCache::from_native_storage(native_cache))
        .unwrap();
    client
        .import_vault_identity_recovery_key_from_native_storage(identity_key)
        .unwrap();
    assert!(matches!(
        client.begin_vault_identity_establishment(),
        Err(Error::KeysUnavailable)
    ));
    assert!(matches!(
        client.adopt_vault_identity(established).unwrap(),
        VaultIdentityAdoption::Adopted { .. }
    ));
}

#[test]
fn authenticated_anchor_rejects_substituted_server_operation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked_identity_client(&config(&dir, "operation-auth", &vault), &vault);
    let pending = client.begin_vault_identity_establishment().unwrap();
    let mut status: Value =
        serde_json::from_str(&established_status(&pending.establish_body_json)).unwrap();
    status["operation_id"] = json!(uuid::Uuid::new_v4());
    assert!(matches!(
        client.adopt_vault_identity(status.to_string()),
        Err(Error::InvalidRequest(
            "vault identity operation does not match anchor"
        ))
    ));
    status["operation_id"] = json!(uuid::Uuid::nil());
    assert!(matches!(
        client.adopt_vault_identity(status.to_string()),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn protected_restore_has_fixed_shape_vault_authentication_and_conflict_outcomes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "restore-errors", &vault);
    let accepted = {
        let client = unlocked_identity_client(&client_config, &vault);
        establish(&client).1
    };
    let client = open(&client_config);
    assert_eq!(
        client.restore_vault_identity_from_protected(accepted[..146].to_vec()),
        Err(Error::InvalidKeyCache)
    );
    let mut unauthenticated = accepted.clone();
    unauthenticated[90] ^= 1;
    assert_eq!(
        client.restore_vault_identity_from_protected(unauthenticated),
        Err(Error::Crypto)
    );
    let foreign_vault = Vault::new();
    let foreign = unlocked_identity_client(
        &config(&dir, "foreign-vault", &foreign_vault),
        &foreign_vault,
    )
    .begin_vault_identity_establishment()
    .unwrap()
    .protected_pending_bytes
    .to_vec();
    assert_eq!(
        client.restore_vault_identity_from_protected(foreign),
        Err(Error::IdentityMismatch)
    );
    let rival = unlocked_identity_client(&config(&dir, "restore-conflict", &vault), &vault)
        .begin_vault_identity_establishment()
        .unwrap()
        .protected_pending_bytes
        .to_vec();
    assert_eq!(
        client.restore_vault_identity_from_protected(rival),
        Err(Error::Conflict)
    );
}

#[test]
fn verified_handoff_supplies_recovery_key_for_reopen_and_established_adoption() {
    use peppy_crypto::derive_root_key;
    use peppy_crypto::enrollment::{
        EnrollmentContext, EnrollmentPurpose, EnrollmentRole, HandoffRootPackage, TransferKeypair,
        open_handoff, seal_handoff,
    };

    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "handoff-identity", &vault);
    let sender = TransferKeypair::generate().unwrap();
    let recipient = TransferKeypair::generate().unwrap();
    let context = EnrollmentContext {
        purpose: EnrollmentPurpose::Phone,
        canonical_origin: "https://peppy.test".into(),
        vault_id: vault.id.0,
        fingerprint: peppy_crypto::profile_fingerprint_bytes(&vault.profile).unwrap(),
        key_epoch: 1,
        enrollment_id: uuid::Uuid::new_v4(),
        server_nonce: [2; 32],
        owner_transfer_public_key: sender.public_key(),
        recipient_transfer_public_key: recipient.public_key(),
        recipient_signing_public_key: [3; 32],
        device_id: client_config.device_id.0,
        credential_digest: [4; 32],
        role: EnrollmentRole::Gateway,
    };
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let package =
        HandoffRootPackage::new(root, vault.profile.clone(), vault.header.clone()).unwrap();
    let encrypted = seal_handoff(&sender, &recipient.public_key(), &context, package).unwrap();
    let handoff = open_handoff(&recipient, &sender.public_key(), &context, &encrypted).unwrap();
    let (native_cache, identity_key, status) = {
        let client = open(&client_config);
        client.import_verified_handoff(handoff).unwrap();
        let pending = client.begin_vault_identity_establishment().unwrap();
        (
            client
                .export_native_key_cache(1)
                .unwrap()
                .native_storage_bytes()
                .to_vec(),
            client
                .export_vault_identity_recovery_key_for_native_storage(1)
                .unwrap(),
            established_status(&pending.establish_body_json),
        )
    };
    let client = open(&client_config);
    client
        .import_native_key_cache(&NativeKeyCache::from_native_storage(native_cache))
        .unwrap();
    client
        .import_vault_identity_recovery_key_from_native_storage(identity_key)
        .unwrap();
    assert!(matches!(
        client.adopt_vault_identity(status).unwrap(),
        VaultIdentityAdoption::Adopted { .. }
    ));
}
