use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_crypto::{EncryptedEnvelope, KeyProfile, VaultCheckHeader};
use peppy_hosted_api::{
    attachments::{AttachmentRetentionClass, ReservedAttachmentResponse},
    enrollment::{
        AdmissionChallenge, AdmissionComplete, ClaimInstanceResponse, ClientKind, DeploymentMode,
        EnrollmentMethod, EnrollmentRole, EnrollmentState, EnrollmentTranscript, InstanceInfo,
        InstanceSession, InvitationClaim, InvitationCreated, InvitationPublicClaim,
        InvitationStatus, ProposedDevice, VaultMaterial, VaultRequestCreated, VaultRequestState,
        VaultRequestStatus,
    },
};
use serde::Serialize;
use std::path::PathBuf;
use uuid::Uuid;

fn golden(name: &str, value: impl Serialize) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts/fixtures/server")
        .join(name);
    let actual = serde_json::to_string_pretty(&value).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{}: {error}; regenerate with UPDATE_GOLDENS=1 cargo test -p peppy-server --test native_response_goldens", path.display())
    });
    assert_eq!(
        actual,
        expected,
        "{} differs; regenerate with UPDATE_GOLDENS=1 cargo test -p peppy-server --test native_response_goldens",
        path.display()
    );
}

#[test]
fn native_enrollment_response_goldens() {
    let id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let profile = KeyProfile {
        crypto_suite: 1,
        salt: [0; 16],
        vault_id: id,
        key_epoch: 1,
    };
    golden(
        "instance.200.json",
        InstanceInfo {
            mode: DeploymentMode::Community,
            claimed: false,
            setup_required: true,
            password_auth: false,
            qr_pairing: false,
            shared_secret_pairing: false,
        },
    );
    let session = InstanceSession {
        session_token: Some("example-session-token".into()),
        csrf_token: None,
        expires_in_seconds: 43_200,
    };
    golden("instance-session.200.json", session.clone());
    golden(
        "instance-claim.200.json",
        ClaimInstanceResponse {
            vault_id: id,
            device_id: id,
            replayed: false,
            session,
        },
    );
    golden(
        "instance-vault-requests.200.json",
        VaultRequestCreated {
            request_id: id,
            expires_in_seconds: 600,
        },
    );
    golden(
        "instance-vault-request-status.200.json",
        VaultRequestStatus {
            request_id: id,
            state: VaultRequestState::Pending,
            expires_in_seconds: 600,
        },
    );
    golden(
        "enrollment-invitations.200.json",
        InvitationCreated {
            invitation_id: id,
            expires_in_seconds: 300,
        },
    );
    golden(
        "enrollment-invitation-status.200.json",
        InvitationStatus {
            state: EnrollmentState::Waiting,
            expires_in_seconds: 300,
            claim: None,
            transcript: None,
            encrypted_key_package: None,
            transfer_nonce: None,
            profile: Some(serde_json::to_value(&profile).unwrap()),
        },
    );
    let encoded = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned();
    let fingerprint = profile.fingerprint().unwrap();
    // Synthetic and low-entropy, so secret scanners do not mistake the sample for a real digest.
    let credential_digest = "00".repeat(32);
    let device = ProposedDevice {
        device_id: id,
        kind: ClientKind::Android,
        name: "Example phone".into(),
        signing_public_key: encoded.clone(),
    };
    let claim = InvitationPublicClaim {
        device: device.clone(),
        transfer_public_key: encoded.clone(),
        credential_digest: credential_digest.clone(),
        claim_authenticator: encoded.clone(),
    };
    let transcript = EnrollmentTranscript {
        origin: "https://example.test".into(),
        vault_id: id,
        fingerprint: fingerprint.clone(),
        key_epoch: 1,
        enrollment_id: id,
        nonce: encoded.clone(),
        owner_transfer_public_key: encoded.clone(),
        recipient_transfer_public_key: encoded.clone(),
        signing_public_key: encoded.clone(),
        device_id: id,
        credential_digest: credential_digest.clone(),
        role: EnrollmentRole::Gateway,
    };
    let status = InvitationStatus {
        state: EnrollmentState::Claimed,
        expires_in_seconds: 300,
        claim: Some(claim),
        transcript: Some(transcript),
        encrypted_key_package: None,
        transfer_nonce: None,
        profile: Some(serde_json::to_value(&profile).unwrap()),
    };
    golden(
        "enrollment-invitation-status-claimed.200.json",
        status.clone(),
    );
    golden(
        "enrollment-invitation-status-approved.200.json",
        InvitationStatus {
            state: EnrollmentState::Approved,
            encrypted_key_package: Some(URL_SAFE_NO_PAD.encode([0; 64])),
            transfer_nonce: Some(URL_SAFE_NO_PAD.encode([0; 24])),
            ..status
        },
    );
    let header = VaultCheckHeader {
        profile: profile.clone(),
        check: EncryptedEnvelope {
            nonce: [0; 24],
            ciphertext: vec![0; 32],
        },
    };
    golden(
        "enrollment-invitation-claim.200.json",
        InvitationClaim {
            invitation_id: id,
            claim_secret: encoded.clone(),
            origin: "https://example.test".into(),
            vault_id: id,
            fingerprint: fingerprint.clone(),
            key_epoch: 1,
            method: EnrollmentMethod::Transfer,
            owner_transfer_public_key: encoded.clone(),
            expires_in_seconds: 300,
        },
    );
    golden(
        "enrollment-challenges.200.json",
        AdmissionChallenge {
            enrollment_id: id,
            origin: "https://example.test".into(),
            vault_id: id,
            profile: serde_json::to_value(&profile).unwrap(),
            fingerprint: fingerprint.clone(),
            key_epoch: 1,
            device,
            requested_role: EnrollmentRole::Gateway,
            nonce: encoded.clone(),
            credential_digest: credential_digest.clone(),
            expires_in_seconds: 120,
        },
    );
    golden(
        "enrollment-challenge-complete.200.json",
        AdmissionComplete {
            vault_id: id,
            device_id: id,
            role: EnrollmentRole::Gateway,
            material: VaultMaterial {
                profile: serde_json::to_value(&profile).unwrap(),
                check_header: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
                fingerprint,
                key_epoch: 1,
                admission_public_key: encoded,
            },
        },
    );
    golden(
        "attachments-reserve.200.json",
        ReservedAttachmentResponse {
            attachment_id: id,
            expires_in_seconds: 3600,
            retention_class: AttachmentRetentionClass::Protected,
        },
    );
}
