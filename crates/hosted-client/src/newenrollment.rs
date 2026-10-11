//! Opaque, transport-free enrollment request construction.

use crate::{HostedClientError, canonical_origin};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_crypto::enrollment::{
    EncryptedHandoff, EnrollmentContext, EnrollmentPurpose, EnrollmentRole as CryptoRole,
    HandoffRootPackage, PhoneClaimMacInput, TransferKeypair, VerifiedHandoff,
    derive_admission_keypair, open_handoff, phone_claim_authenticator, seal_handoff,
    sign_device_proof, verify_phone_claim_authenticator,
};
use peppy_crypto::{
    KeyProfile, RootKey, VaultCheckHeader, create_vault_check_header, derive_root_key,
    verify_vault_check_header,
};
pub use peppy_hosted_api::enrollment::ClientKind;
use peppy_hosted_api::enrollment::{
    AdmissionChallenge, AdmissionChallengeRequest, AdmissionComplete, ApproveInvitationRequest,
    ClaimInstanceRequest, ClaimInstanceResponse, ClaimInvitationRequest, CompleteAdmissionRequest,
    ConfirmInvitationRequest, CreateInvitationRequest, EnrollmentMethod, EnrollmentRole,
    EnrollmentState, EnrollmentTranscript, InvitationClaim, InvitationCreated, InvitationStatus,
    PhonePairingQr, ProposedDevice, VaultMaterial,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const CHECKPOINT_VERSION: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnrollmentMode {
    Fresh,
    ExistingOwner,
}

pub struct EnrollmentClient {
    origin: String,
    kind: ClientKind,
    device_name: String,
    device_id: Uuid,
    signing_seed: Zeroizing<[u8; 32]>,
    credential: Zeroizing<String>,
    owner: Option<OwnerMaterial>,
    phone: Option<PhoneFlow>,
    manual: Option<ManualAdmission>,
    mode: EnrollmentMode,
}

struct OwnerMaterial {
    operation_id: Uuid,
    profile: KeyProfile,
    header: VaultCheckHeader,
}

/// A verified root handoff that can only be activated by native code.  It has no public root,
/// credential, or serialization accessor.
pub struct NativeIdentityActivation(VerifiedHandoff);

impl NativeIdentityActivation {
    pub fn activate(self, client: &peppy_client_core::Client) -> Result<(), HostedClientError> {
        client
            .import_verified_handoff(self.0)
            .map_err(|_| HostedClientError::Crypto)
    }
}

/// Public, non-secret enrollment identity facts needed to configure a native client.
#[derive(Clone)]
pub struct NativeEnrollmentMetadata {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub role: EnrollmentRole,
    pub profile: KeyProfile,
    /// Authenticated encrypted vault-check material for protected native identity storage.
    pub header: VaultCheckHeader,
}

/// Opaque enrollment result for a native host. The device credential may only be copied into
/// the host's secure store; it is never serializable or suitable for a UI/worker response.
pub struct NativeEnrollmentIdentity {
    origin: String,
    metadata: NativeEnrollmentMetadata,
    credential: Zeroizing<String>,
    activation: Option<NativeIdentityActivation>,
}

impl NativeEnrollmentIdentity {
    pub fn metadata(&self) -> &NativeEnrollmentMetadata {
        &self.metadata
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn device_credential_for_native_secure_store(&self) -> String {
        self.credential.to_string()
    }

    pub fn into_activation(self) -> Option<NativeIdentityActivation> {
        self.activation
    }
}

struct ManualAdmission {
    context: EnrollmentContext,
    profile: KeyProfile,
}

enum PhoneFlow {
    OwnerDraft {
        method: EnrollmentMethod,
        transfer: TransferKeypair,
        invitation_secret: Zeroizing<[u8; 32]>,
    },
    OwnerInvitation {
        method: EnrollmentMethod,
        invitation_id: Uuid,
        transfer: TransferKeypair,
        invitation_secret: Zeroizing<[u8; 32]>,
    },
    RecipientDraft {
        qr: PhonePairingQr,
        transfer: TransferKeypair,
    },
    RecipientClaimed {
        qr: PhonePairingQr,
        invitation_id: Uuid,
        claim_secret: Zeroizing<String>,
        method: EnrollmentMethod,
        owner_transfer_public_key: [u8; 32],
        transfer: TransferKeypair,
    },
    PendingAdmission {
        context: EnrollmentContext,
        root: RootKey,
    },
    PendingTransferAdmission {
        context: EnrollmentContext,
        handoff: Box<VerifiedHandoff>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    origin: String,
    kind: ClientKind,
    device_name: String,
    device_id: Uuid,
    signing_seed: Vec<u8>,
    credential: String,
    owner: Option<OwnerCheckpoint>,
    existing_owner: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerCheckpoint {
    operation_id: Uuid,
    profile: KeyProfile,
    header: VaultCheckHeader,
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        self.signing_seed.zeroize();
        self.credential.zeroize();
    }
}

impl EnrollmentClient {
    pub fn new(
        origin: String,
        kind: ClientKind,
        device_name: String,
    ) -> Result<Self, HostedClientError> {
        let origin = canonical_origin(&origin)?;
        if device_name.is_empty()
            || device_name.len() > 128
            || device_name.chars().any(char::is_control)
        {
            return Err(HostedClientError::InvalidRequest);
        }
        libsodium_rs::ensure_init().map_err(|_| HostedClientError::Crypto)?;
        let mut signing_seed = [0_u8; 32];
        let mut credential_bytes = [0_u8; 48];
        libsodium_rs::random::fill_bytes(&mut signing_seed);
        libsodium_rs::random::fill_bytes(&mut credential_bytes);
        let credential = lowercase_hex(&credential_bytes);
        credential_bytes.zeroize();
        Ok(Self {
            origin,
            kind,
            device_name,
            device_id: Uuid::new_v4(),
            signing_seed: Zeroizing::new(signing_seed),
            credential: Zeroizing::new(credential),
            owner: None,
            phone: None,
            manual: None,
            mode: EnrollmentMode::Fresh,
        })
    }

    /// Prepares invitation-only owner state from protected, already-authenticated identity
    /// material. It does not import, replace, or expose the existing device credential.
    pub fn for_existing_owner(
        origin: String,
        kind: ClientKind,
        device_name: String,
        profile: KeyProfile,
        header: VaultCheckHeader,
    ) -> Result<Self, HostedClientError> {
        profile
            .validate()
            .map_err(|_| HostedClientError::InvalidProfile)?;
        profile
            .fingerprint()
            .map_err(|_| HostedClientError::InvalidProfile)?;
        if profile.vault_id.is_nil() || header.profile != profile {
            return Err(HostedClientError::InvalidProfile);
        }
        let mut client = Self::new(origin, kind, device_name)?;
        client.owner = Some(OwnerMaterial {
            operation_id: Uuid::new_v4(),
            profile,
            header,
        });
        client.mode = EnrollmentMode::ExistingOwner;
        Ok(client)
    }

    /// Confirms that protected pending first-owner state belongs to this canonical origin.
    pub fn is_pending_owner_creation_for_origin(&self, origin: &str) -> bool {
        self.mode == EnrollmentMode::Fresh && self.owner.is_some() && self.origin == origin
    }

    /// Checks a resumed first-owner passphrase against the checkpointed public vault header.
    pub fn owner_passphrase_matches(&self, passphrase: &str) -> Result<bool, HostedClientError> {
        let owner = self
            .owner
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?;
        let passphrase = Zeroizing::new(passphrase.to_owned());
        let Ok(root) = derive_root_key(&passphrase, &owner.profile) else {
            return Ok(false);
        };
        Ok(verify_vault_check_header(&root, &owner.profile, &owner.header).is_ok())
    }

    pub fn create_owner_request(
        &mut self,
        setup_token: String,
        admin_password: String,
        passphrase: String,
    ) -> Result<String, HostedClientError> {
        if self.mode != EnrollmentMode::Fresh
            || setup_token.is_empty()
            || admin_password.len() < 12
            || admin_password.len() > 1024
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let passphrase = Zeroizing::new(passphrase);
        if !peppy_crypto::passphrase::passphrase_acceptable(&passphrase) {
            return Err(HostedClientError::InvalidRequest);
        }
        if self.owner.is_none() {
            let profile =
                KeyProfile::new(Uuid::new_v4(), 1).map_err(|_| HostedClientError::Crypto)?;
            let root =
                derive_root_key(&passphrase, &profile).map_err(|_| HostedClientError::Crypto)?;
            let header = create_vault_check_header(&root, profile.clone())
                .map_err(|_| HostedClientError::Crypto)?;
            self.owner = Some(OwnerMaterial {
                operation_id: Uuid::new_v4(),
                profile,
                header,
            });
        }
        let owner = self.owner.as_ref().ok_or(HostedClientError::Crypto)?;
        let root =
            derive_root_key(&passphrase, &owner.profile).map_err(|_| HostedClientError::Crypto)?;
        verify_vault_check_header(&root, &owner.profile, &owner.header)
            .map_err(|_| HostedClientError::InvalidRequest)?;
        let admission = derive_admission_keypair(&root).map_err(|_| HostedClientError::Crypto)?;
        let material = vault_material(&owner.profile, &owner.header, admission.public_key())?;
        let request = ClaimInstanceRequest {
            operation_id: owner.operation_id,
            setup_token,
            admin_password,
            device: self.device()?,
            material,
            credential_digest: credential_digest(&self.credential),
        };
        serde_json::to_string(&request).map_err(|_| HostedClientError::InvalidRequest)
    }

    pub fn manual_challenge_request(
        &self,
        vault_id: Option<String>,
    ) -> Result<String, HostedClientError> {
        if self.mode != EnrollmentMode::Fresh {
            return Err(HostedClientError::InvalidRequest);
        }
        let vault_id = vault_id
            .map(|value| Uuid::parse_str(&value).map_err(|_| HostedClientError::InvalidRequest))
            .transpose()?;
        serde_json::to_string(&AdmissionChallengeRequest {
            device: self.device()?,
            vault_id,
            credential_digest: credential_digest(&self.credential),
        })
        .map_err(|_| HostedClientError::InvalidRequest)
    }

    pub fn complete_manual_challenge_request(
        &mut self,
        challenge_json: String,
        passphrase: String,
    ) -> Result<String, HostedClientError> {
        if self.mode != EnrollmentMode::Fresh {
            return Err(HostedClientError::InvalidRequest);
        }
        let challenge: AdmissionChallenge =
            serde_json::from_str(&challenge_json).map_err(|_| HostedClientError::InvalidRequest)?;
        if challenge.device != self.device()?
            || challenge.credential_digest != credential_digest(&self.credential)
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let profile: KeyProfile = serde_json::from_value(challenge.profile.clone())
            .map_err(|_| HostedClientError::InvalidProfile)?;
        if profile.vault_id != challenge.vault_id
            || profile.key_epoch != challenge.key_epoch
            || profile
                .fingerprint()
                .map_err(|_| HostedClientError::InvalidProfile)?
                != challenge.fingerprint
        {
            return Err(HostedClientError::InvalidProfile);
        }
        let root = derive_root_key(&passphrase, &profile).map_err(|_| HostedClientError::Crypto)?;
        let context = EnrollmentContext {
            purpose: EnrollmentPurpose::Manual,
            canonical_origin: self.origin.clone(),
            vault_id: challenge.vault_id,
            fingerprint: decode_hex32(&challenge.fingerprint)?,
            key_epoch: challenge.key_epoch,
            enrollment_id: challenge.enrollment_id,
            server_nonce: decode_b64_32(&challenge.nonce)?,
            owner_transfer_public_key: [0; 32],
            recipient_transfer_public_key: [0; 32],
            recipient_signing_public_key: self.signing_public_key()?,
            device_id: self.device_id,
            credential_digest: decode_hex32(&challenge.credential_digest)?,
            role: crypto_role(challenge.requested_role),
        };
        let admission_signature = URL_SAFE_NO_PAD.encode(
            derive_admission_keypair(&root)
                .map_err(|_| HostedClientError::Crypto)?
                .sign_context(&context)
                .map_err(|_| HostedClientError::Crypto)?,
        );
        let device_signature = URL_SAFE_NO_PAD.encode(
            sign_device_proof(&self.device_secret_key()?, &context)
                .map_err(|_| HostedClientError::Crypto)?,
        );
        let request = serde_json::to_string(&CompleteAdmissionRequest {
            admission_signature,
            device_signature,
            credential_digest: credential_digest(&self.credential),
        })
        .map_err(|_| HostedClientError::InvalidRequest)?;
        self.manual = Some(ManualAdmission { context, profile });
        Ok(request)
    }

    /// Finalizes the first owner only when the claimed ids still match this checkpointed owner.
    pub fn finish_owner_admission(
        &self,
        response_json: String,
        passphrase: String,
    ) -> Result<NativeEnrollmentIdentity, HostedClientError> {
        let response: ClaimInstanceResponse = parse(&response_json)?;
        if self.mode != EnrollmentMode::Fresh {
            return Err(HostedClientError::InvalidRequest);
        }
        let owner = self
            .owner
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?;
        if response.vault_id != owner.profile.vault_id || response.device_id != self.device_id {
            return Err(HostedClientError::InvalidRequest);
        }
        self.identity_from_profile(
            response.vault_id,
            EnrollmentRole::Owner,
            owner.profile.clone(),
            owner.header.clone(),
            passphrase,
        )
    }

    /// Finalizes a manual admission only after validating the original signed challenge context.
    pub fn finish_manual_admission(
        &mut self,
        response_json: String,
        passphrase: String,
    ) -> Result<NativeEnrollmentIdentity, HostedClientError> {
        let complete: AdmissionComplete = parse(&response_json)?;
        if self.mode != EnrollmentMode::Fresh {
            return Err(HostedClientError::InvalidRequest);
        }
        let manual = self
            .manual
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?;
        if complete.vault_id != manual.context.vault_id
            || complete.device_id != manual.context.device_id
            || complete.role != api_role(manual.context.role)
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let (profile, header) = material_profile_header(&complete.material)?;
        if profile != manual.profile {
            return Err(HostedClientError::InvalidProfile);
        }
        let identity = self.identity_from_profile(
            complete.vault_id,
            complete.role,
            profile,
            header,
            passphrase,
        )?;
        self.manual = None;
        Ok(identity)
    }

    /// Starts an owner invitation. The X25519 secret never leaves this opaque state.
    pub fn create_phone_invitation(
        &mut self,
        method: EnrollmentMethod,
    ) -> Result<String, HostedClientError> {
        if self.owner.is_none() || self.phone.is_some() {
            return Err(HostedClientError::InvalidRequest);
        }
        let transfer = TransferKeypair::generate().map_err(|_| HostedClientError::Crypto)?;
        let mut secret = [0; 32];
        libsodium_rs::random::fill_bytes(&mut secret);
        let request = CreateInvitationRequest {
            method,
            transfer_public_key: URL_SAFE_NO_PAD.encode(transfer.public_key()),
            invitation_token_digest: URL_SAFE_NO_PAD.encode(Sha256::digest(secret)),
        };
        self.phone = Some(PhoneFlow::OwnerDraft {
            method,
            transfer,
            invitation_secret: Zeroizing::new(secret),
        });
        serde_json::to_string(&request).map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Binds the server-created id and QR token to the key generated for this invitation.
    pub fn consume_created_invitation(
        &mut self,
        created_json: String,
    ) -> Result<(), HostedClientError> {
        let created: InvitationCreated = parse(&created_json)?;
        let flow = self.phone.take().ok_or(HostedClientError::InvalidRequest)?;
        let PhoneFlow::OwnerDraft {
            method,
            transfer,
            invitation_secret,
        } = flow
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        if created.invitation_id.is_nil() || !(1..=300).contains(&created.expires_in_seconds) {
            return Err(HostedClientError::InvalidRequest);
        }
        self.phone = Some(PhoneFlow::OwnerInvitation {
            method,
            invitation_id: created.invitation_id,
            transfer,
            invitation_secret,
        });
        Ok(())
    }

    pub fn get_qr_payload(&self) -> Result<String, HostedClientError> {
        let PhoneFlow::OwnerInvitation {
            transfer,
            invitation_secret,
            ..
        } = self
            .phone
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        serde_json::to_string(&PhonePairingQr {
            version: 1,
            purpose: "connect_phone".into(),
            origin: self.origin.clone(),
            invitation_token: URL_SAFE_NO_PAD.encode(**invitation_secret),
            transfer_public_key: URL_SAFE_NO_PAD.encode(transfer.public_key()),
        })
        .map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Parses a QR locally and constructs the phone claim with this device's independent keys.
    pub fn claim_phone_invitation(&mut self, qr_json: String) -> Result<String, HostedClientError> {
        if self.phone.is_some() || self.manual.is_some() {
            return Err(HostedClientError::InvalidRequest);
        }
        let qr: PhonePairingQr = parse(&qr_json)?;
        if qr.version != 1
            || qr.purpose != "connect_phone"
            || qr.origin != self.origin
            || decode_b64_32(&qr.invitation_token).is_err()
            || decode_b64_32(&qr.transfer_public_key).is_err()
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let transfer = TransferKeypair::generate().map_err(|_| HostedClientError::Crypto)?;
        let secret = decode_b64_32(&qr.invitation_token)?;
        let token_digest: [u8; 32] = Sha256::digest(secret).into();
        let owner_transfer_public_key = decode_b64_32(&qr.transfer_public_key)?;
        let recipient_transfer_public_key = transfer.public_key();
        let device = self.device()?;
        let digest = credential_digest(&self.credential);
        let credential_digest = decode_hex32(&digest)?;
        let authenticator = phone_claim_authenticator(
            &secret,
            &PhoneClaimMacInput {
                canonical_origin: &self.origin,
                invitation_token_digest: &token_digest,
                owner_transfer_public_key: &owner_transfer_public_key,
                recipient_transfer_public_key: &recipient_transfer_public_key,
                recipient_signing_public_key: &self.signing_public_key()?,
                device_id: self.device_id,
                credential_digest: &credential_digest,
                client_kind: client_kind_name(self.kind),
                device_name: &self.device_name,
            },
        )
        .map_err(|_| HostedClientError::Crypto)?;
        let request = ClaimInvitationRequest {
            invitation_token_digest: URL_SAFE_NO_PAD.encode(token_digest),
            claim_authenticator: URL_SAFE_NO_PAD.encode(authenticator),
            device,
            transfer_public_key: URL_SAFE_NO_PAD.encode(recipient_transfer_public_key),
            credential_digest: digest,
        };
        self.phone = Some(PhoneFlow::RecipientDraft { qr, transfer });
        serde_json::to_string(&request).map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Pins the server claim response to the scanned QR and the locally generated recipient key.
    pub fn consume_claim_response(&mut self, claim_json: String) -> Result<(), HostedClientError> {
        let claim: InvitationClaim = parse(&claim_json)?;
        let flow = self.phone.take().ok_or(HostedClientError::InvalidRequest)?;
        let PhoneFlow::RecipientDraft { qr, transfer } = flow else {
            return Err(HostedClientError::InvalidRequest);
        };
        if claim.origin != self.origin
            || claim.vault_id.is_nil()
            || claim.key_epoch == 0
            || decode_hex32(&claim.fingerprint).is_err()
            || decode_b64_32(&claim.claim_secret).is_err()
            || !(1..=300).contains(&claim.expires_in_seconds)
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let owner_transfer_public_key = decode_b64_32(&claim.owner_transfer_public_key)?;
        if owner_transfer_public_key != decode_b64_32(&qr.transfer_public_key)? {
            return Err(HostedClientError::InvalidRequest);
        }
        self.phone = Some(PhoneFlow::RecipientClaimed {
            qr,
            invitation_id: claim.invitation_id,
            claim_secret: Zeroizing::new(claim.claim_secret),
            method: claim.method,
            owner_transfer_public_key,
            transfer,
        });
        Ok(())
    }

    /// Native/worker transport descriptor for claimant-authenticated invitation status polling.
    /// This intentionally contains the claim secret and must never be returned to a UI surface.
    pub fn phone_status_request(&self) -> Result<String, HostedClientError> {
        let PhoneFlow::RecipientClaimed {
            invitation_id,
            claim_secret,
            ..
        } = self
            .phone
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        serde_json::to_string(&serde_json::json!({
            "method": "GET",
            "path": format!("/v1/enrollment/invitations/{invitation_id}"),
            "headers": { "Peppy-Claim-Secret": claim_secret.to_string() },
        }))
        .map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Claimants have no DELETE authority; this only abandons local protected flow state.
    pub fn cancel_phone_flow(&mut self) -> Result<(), HostedClientError> {
        if !matches!(
            self.phone.as_ref(),
            Some(PhoneFlow::RecipientDraft { .. } | PhoneFlow::RecipientClaimed { .. })
        ) {
            return Err(HostedClientError::InvalidRequest);
        }
        self.phone = None;
        Ok(())
    }

    /// Computes the locally verified six-digit SAS. It never accepts a server-provided code.
    pub fn phone_sas(&self, status_json: String) -> Result<String, HostedClientError> {
        self.phone_context(&parse(&status_json)?)?
            .sas()
            .map_err(|_| HostedClientError::Crypto)
    }

    /// Produces the owner approval. A transfer approval contains an authenticated encrypted root;
    /// shared-secret approval deliberately contains no root material.
    pub fn approve_phone_invitation(
        &self,
        status_json: String,
        passphrase: String,
    ) -> Result<String, HostedClientError> {
        let status: InvitationStatus = parse(&status_json)?;
        let context = self.phone_context(&status)?;
        let PhoneFlow::OwnerInvitation {
            method, transfer, ..
        } = self
            .phone
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        if status.state != EnrollmentState::Claimed {
            return Err(HostedClientError::InvalidRequest);
        }
        let transcript_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(
            context
                .canonical_bytes()
                .map_err(|_| HostedClientError::Crypto)?,
        ));
        let (encrypted_key_package, transfer_nonce) = match method {
            EnrollmentMethod::SharedSecret => (None, None),
            EnrollmentMethod::Transfer => {
                let owner = self
                    .owner
                    .as_ref()
                    .ok_or(HostedClientError::InvalidRequest)?;
                let root = derive_root_key(&passphrase, &owner.profile)
                    .map_err(|_| HostedClientError::Crypto)?;
                let package =
                    HandoffRootPackage::new(root, owner.profile.clone(), owner.header.clone())
                        .map_err(|_| HostedClientError::Crypto)?;
                let encrypted = seal_handoff(
                    transfer,
                    &context.recipient_transfer_public_key,
                    &context,
                    package,
                )
                .map_err(|_| HostedClientError::Crypto)?;
                (
                    Some(URL_SAFE_NO_PAD.encode(encrypted.ciphertext)),
                    Some(URL_SAFE_NO_PAD.encode(encrypted.nonce)),
                )
            }
        };
        serde_json::to_string(&ApproveInvitationRequest {
            transcript_hash,
            encrypted_key_package,
            transfer_nonce,
        })
        .map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Locally opens/derives the admission root and signs both mandatory enrollment proofs.
    pub fn confirm_phone_invitation(
        &mut self,
        status_json: String,
        shared_secret: Option<String>,
    ) -> Result<String, HostedClientError> {
        let status: InvitationStatus = parse(&status_json)?;
        if status.state != EnrollmentState::Approved {
            return Err(HostedClientError::InvalidRequest);
        }
        let context = self.phone_context(&status)?;
        let flow = self.phone.take().ok_or(HostedClientError::InvalidRequest)?;
        let PhoneFlow::RecipientClaimed {
            claim_secret,
            method,
            owner_transfer_public_key,
            transfer,
            ..
        } = flow
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        let (admission_signature, pending) = match method {
            EnrollmentMethod::Transfer => {
                let ciphertext = status
                    .encrypted_key_package
                    .as_deref()
                    .ok_or(HostedClientError::InvalidRequest)
                    .and_then(decode_b64)?;
                let nonce = status
                    .transfer_nonce
                    .as_deref()
                    .ok_or(HostedClientError::InvalidRequest)
                    .and_then(decode_b64_24)?;
                let handoff = open_handoff(
                    &transfer,
                    &owner_transfer_public_key,
                    &context,
                    &EncryptedHandoff { nonce, ciphertext },
                )
                .map_err(|_| HostedClientError::Crypto)?;
                let signature = handoff
                    .sign_admission_context(&context)
                    .map_err(|_| HostedClientError::Crypto)?;
                (
                    signature,
                    PhoneFlow::PendingTransferAdmission {
                        context: context.clone(),
                        handoff: Box::new(handoff),
                    },
                )
            }
            EnrollmentMethod::SharedSecret => {
                if status.encrypted_key_package.is_some() || status.transfer_nonce.is_some() {
                    return Err(HostedClientError::InvalidRequest);
                }
                let profile = status
                    .profile
                    .ok_or(HostedClientError::InvalidRequest)
                    .and_then(profile_from_value)?;
                validate_profile_for_context(&profile, &context)?;
                let root = derive_root_key(
                    &shared_secret.ok_or(HostedClientError::InvalidRequest)?,
                    &profile,
                )
                .map_err(|_| HostedClientError::Crypto)?;
                let signature = derive_admission_keypair(&root)
                    .map_err(|_| HostedClientError::Crypto)?
                    .sign_context(&context)
                    .map_err(|_| HostedClientError::Crypto)?;
                (
                    signature,
                    PhoneFlow::PendingAdmission {
                        context: context.clone(),
                        root,
                    },
                )
            }
        };
        let transcript_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(
            context
                .canonical_bytes()
                .map_err(|_| HostedClientError::Crypto)?,
        ));
        let admission_signature = URL_SAFE_NO_PAD.encode(admission_signature);
        let device_signature = URL_SAFE_NO_PAD.encode(
            sign_device_proof(&self.device_secret_key()?, &context)
                .map_err(|_| HostedClientError::Crypto)?,
        );
        let request = ConfirmInvitationRequest {
            claim_secret: claim_secret.to_string(),
            transcript_hash,
            device_signature,
            admission_signature,
        };
        self.phone = Some(pending);
        serde_json::to_string(&request).map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Accepts the server's enrolled metadata only when it matches the signed transcript, then
    /// returns an opaque activation object for native core initialization.
    pub fn finish_phone_admission(
        &mut self,
        complete_json: String,
    ) -> Result<NativeEnrollmentIdentity, HostedClientError> {
        let complete: AdmissionComplete = parse(&complete_json)?;
        let flow = self.phone.take().ok_or(HostedClientError::InvalidRequest)?;
        let (context, root, handoff) = match flow {
            PhoneFlow::PendingAdmission { context, root } => (context, Some(root), None),
            PhoneFlow::PendingTransferAdmission { context, handoff } => {
                (context, None, Some(*handoff))
            }
            _ => return Err(HostedClientError::InvalidRequest),
        };
        if complete.vault_id != context.vault_id
            || complete.device_id != context.device_id
            || complete.role != api_role(context.role)
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let (profile, header) = material_profile_header(&complete.material)?;
        validate_profile_for_context(&profile, &context)?;
        if let Some(root) = root {
            let _package = HandoffRootPackage::new(root, profile.clone(), header.clone())
                .map_err(|_| HostedClientError::Crypto)?;
        }
        Ok(self.native_identity(
            context.vault_id,
            context.device_id,
            api_role(context.role),
            profile,
            header,
            handoff.map(NativeIdentityActivation),
        ))
    }

    fn identity_from_profile(
        &self,
        vault_id: Uuid,
        role: EnrollmentRole,
        profile: KeyProfile,
        header: VaultCheckHeader,
        passphrase: String,
    ) -> Result<NativeEnrollmentIdentity, HostedClientError> {
        if vault_id != profile.vault_id || self.device_id.is_nil() {
            return Err(HostedClientError::InvalidRequest);
        }
        let root = derive_root_key(&passphrase, &profile).map_err(|_| HostedClientError::Crypto)?;
        let _package = HandoffRootPackage::new(root, profile.clone(), header.clone())
            .map_err(|_| HostedClientError::Crypto)?;
        Ok(self.native_identity(vault_id, self.device_id, role, profile, header, None))
    }

    fn native_identity(
        &self,
        vault_id: Uuid,
        device_id: Uuid,
        role: EnrollmentRole,
        profile: KeyProfile,
        header: VaultCheckHeader,
        activation: Option<NativeIdentityActivation>,
    ) -> NativeEnrollmentIdentity {
        NativeEnrollmentIdentity {
            origin: self.origin.clone(),
            metadata: NativeEnrollmentMetadata {
                vault_id,
                device_id,
                role,
                profile,
                header,
            },
            credential: Zeroizing::new(self.credential.to_string()),
            activation,
        }
    }

    pub fn checkpoint(&self) -> Result<Vec<u8>, HostedClientError> {
        // `TransferKeypair` intentionally has no secret export API. Refusing to serialize an
        // active phone flow is safer than silently replacing its key after a restart.
        if self.phone.is_some() {
            return Err(HostedClientError::InvalidRequest);
        }
        serde_json::to_vec(&Checkpoint {
            version: CHECKPOINT_VERSION,
            origin: self.origin.clone(),
            kind: self.kind,
            device_name: self.device_name.clone(),
            device_id: self.device_id,
            signing_seed: self.signing_seed.to_vec(),
            credential: self.credential.to_string(),
            owner: self.owner.as_ref().map(|owner| OwnerCheckpoint {
                operation_id: owner.operation_id,
                profile: owner.profile.clone(),
                header: owner.header.clone(),
            }),
            existing_owner: self.mode == EnrollmentMode::ExistingOwner,
        })
        .map_err(|_| HostedClientError::Crypto)
    }

    pub fn restore(checkpoint: Vec<u8>) -> Result<Self, HostedClientError> {
        let mut checkpoint: Checkpoint =
            serde_json::from_slice(&checkpoint).map_err(|_| HostedClientError::InvalidRequest)?;
        if checkpoint.version != CHECKPOINT_VERSION
            || canonical_origin(&checkpoint.origin)? != checkpoint.origin
            || checkpoint.device_name.is_empty()
            || checkpoint.device_name.len() > 128
            || checkpoint.credential.len() != 96
            || !checkpoint
                .credential
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let seed: [u8; 32] = checkpoint
            .signing_seed
            .as_slice()
            .try_into()
            .map_err(|_| HostedClientError::InvalidRequest)?;
        let owner = checkpoint
            .owner
            .take()
            .map(|owner| {
                if owner.profile.vault_id.is_nil() || owner.header.profile != owner.profile {
                    return Err(HostedClientError::InvalidRequest);
                }
                Ok(OwnerMaterial {
                    operation_id: owner.operation_id,
                    profile: owner.profile,
                    header: owner.header,
                })
            })
            .transpose()?;
        if checkpoint.existing_owner && owner.is_none() {
            return Err(HostedClientError::InvalidRequest);
        }
        Ok(Self {
            origin: std::mem::take(&mut checkpoint.origin),
            kind: checkpoint.kind,
            device_name: std::mem::take(&mut checkpoint.device_name),
            device_id: checkpoint.device_id,
            signing_seed: Zeroizing::new(seed),
            credential: Zeroizing::new(std::mem::take(&mut checkpoint.credential)),
            owner,
            phone: None,
            manual: None,
            mode: if checkpoint.existing_owner {
                EnrollmentMode::ExistingOwner
            } else {
                EnrollmentMode::Fresh
            },
        })
    }

    fn phone_context(
        &self,
        status: &InvitationStatus,
    ) -> Result<EnrollmentContext, HostedClientError> {
        let transcript = status
            .transcript
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?;
        transcript
            .validate_phone_fields()
            .map_err(|_| HostedClientError::InvalidRequest)?;
        if transcript.origin != self.origin {
            return Err(HostedClientError::InvalidRequest);
        }
        match self
            .phone
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        {
            PhoneFlow::OwnerInvitation {
                method,
                invitation_id,
                transfer,
                invitation_secret,
            } => {
                if transcript.enrollment_id != *invitation_id
                    || transcript.owner_transfer_public_key
                        != URL_SAFE_NO_PAD.encode(transfer.public_key())
                    || status.claim.is_none()
                {
                    return Err(HostedClientError::InvalidRequest);
                }
                if status.claim.as_ref().is_some_and(|claim| {
                    claim.device.device_id != transcript.device_id
                        || claim.device.signing_public_key != transcript.signing_public_key
                        || claim.transfer_public_key != transcript.recipient_transfer_public_key
                        || claim.credential_digest != transcript.credential_digest
                }) {
                    return Err(HostedClientError::InvalidRequest);
                }
                let claim = status
                    .claim
                    .as_ref()
                    .ok_or(HostedClientError::InvalidRequest)?;
                let verified = claim_authenticator_valid(
                    invitation_secret,
                    &self.origin,
                    &transcript.nonce,
                    &transfer.public_key(),
                    claim,
                )?;
                if !verified {
                    return Err(HostedClientError::InvalidRequest);
                }
                let _ = method;
            }
            PhoneFlow::RecipientClaimed {
                qr,
                invitation_id,
                owner_transfer_public_key,
                transfer,
                ..
            } => {
                if transcript.enrollment_id != *invitation_id
                    || transcript.device_id != self.device_id
                    || transcript.role != EnrollmentRole::Gateway
                    || transcript.signing_public_key
                        != URL_SAFE_NO_PAD.encode(self.signing_public_key()?)
                    || transcript.credential_digest != credential_digest(&self.credential)
                    || transcript.owner_transfer_public_key
                        != URL_SAFE_NO_PAD.encode(owner_transfer_public_key)
                    || transcript.recipient_transfer_public_key
                        != URL_SAFE_NO_PAD.encode(transfer.public_key())
                    || transcript.nonce != token_digest_base64url(&qr.invitation_token)?
                {
                    return Err(HostedClientError::InvalidRequest);
                }
            }
            _ => return Err(HostedClientError::InvalidRequest),
        }
        context_from_transcript(transcript)
    }

    fn device(&self) -> Result<ProposedDevice, HostedClientError> {
        Ok(ProposedDevice {
            device_id: self.device_id,
            kind: self.kind,
            name: self.device_name.clone(),
            signing_public_key: URL_SAFE_NO_PAD.encode(self.signing_public_key()?),
        })
    }
    fn signing_public_key(&self) -> Result<[u8; 32], HostedClientError> {
        Ok(
            *libsodium_rs::crypto_sign::KeyPair::from_seed(&self.signing_seed[..])
                .map_err(|_| HostedClientError::Crypto)?
                .public_key
                .as_bytes(),
        )
    }
    fn device_secret_key(&self) -> Result<[u8; 64], HostedClientError> {
        Ok(
            *libsodium_rs::crypto_sign::KeyPair::from_seed(&self.signing_seed[..])
                .map_err(|_| HostedClientError::Crypto)?
                .secret_key
                .as_bytes(),
        )
    }
}

fn parse<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, HostedClientError> {
    if json.is_empty() || json.len() > 16 * 1024 {
        return Err(HostedClientError::InvalidRequest);
    }
    serde_json::from_str(json).map_err(|_| HostedClientError::InvalidRequest)
}
fn context_from_transcript(
    value: &EnrollmentTranscript,
) -> Result<EnrollmentContext, HostedClientError> {
    Ok(EnrollmentContext {
        purpose: EnrollmentPurpose::Phone,
        canonical_origin: value.origin.clone(),
        vault_id: value.vault_id,
        fingerprint: decode_hex32(&value.fingerprint)?,
        key_epoch: value.key_epoch,
        enrollment_id: value.enrollment_id,
        server_nonce: decode_b64_32(&value.nonce)?,
        owner_transfer_public_key: decode_b64_32(&value.owner_transfer_public_key)?,
        recipient_transfer_public_key: decode_b64_32(&value.recipient_transfer_public_key)?,
        recipient_signing_public_key: decode_b64_32(&value.signing_public_key)?,
        device_id: value.device_id,
        credential_digest: decode_hex32(&value.credential_digest)?,
        role: crypto_role(value.role),
    })
}
fn token_digest_base64url(token: &str) -> Result<String, HostedClientError> {
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(decode_b64_32(token)?)))
}
fn decode_b64(value: &str) -> Result<Vec<u8>, HostedClientError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| HostedClientError::InvalidRequest)
}
fn decode_b64_24(value: &str) -> Result<[u8; 24], HostedClientError> {
    decode_b64(value)?
        .try_into()
        .map_err(|_| HostedClientError::InvalidRequest)
}
fn profile_from_value(value: serde_json::Value) -> Result<KeyProfile, HostedClientError> {
    serde_json::from_value(value).map_err(|_| HostedClientError::InvalidProfile)
}
fn validate_profile_for_context(
    profile: &KeyProfile,
    context: &EnrollmentContext,
) -> Result<(), HostedClientError> {
    if profile.vault_id != context.vault_id
        || profile.key_epoch != context.key_epoch
        || profile
            .fingerprint()
            .map_err(|_| HostedClientError::InvalidProfile)?
            != lowercase_hex(&context.fingerprint)
    {
        return Err(HostedClientError::InvalidProfile);
    }
    Ok(())
}
fn material_profile_header(
    material: &VaultMaterial,
) -> Result<(KeyProfile, VaultCheckHeader), HostedClientError> {
    let profile = profile_from_value(material.profile.clone())?;
    let header: VaultCheckHeader = serde_json::from_slice(&decode_b64(&material.check_header)?)
        .map_err(|_| HostedClientError::InvalidProfile)?;
    if header.profile != profile
        || material.key_epoch != profile.key_epoch
        || material.fingerprint
            != profile
                .fingerprint()
                .map_err(|_| HostedClientError::InvalidProfile)?
    {
        return Err(HostedClientError::InvalidProfile);
    }
    Ok((profile, header))
}
fn api_role(role: CryptoRole) -> EnrollmentRole {
    match role {
        CryptoRole::Owner => EnrollmentRole::Owner,
        CryptoRole::Device => EnrollmentRole::Device,
        CryptoRole::Gateway => EnrollmentRole::Gateway,
    }
}
fn client_kind_name(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::Browser => "browser",
        ClientKind::Desktop => "desktop",
        ClientKind::Android => "android",
        ClientKind::Ios => "ios",
    }
}
fn claim_authenticator_valid(
    secret: &[u8; 32],
    origin: &str,
    nonce: &str,
    owner: &[u8; 32],
    claim: &peppy_hosted_api::enrollment::InvitationPublicClaim,
) -> Result<bool, HostedClientError> {
    let token_digest = decode_b64_32(nonce)?;
    let recipient_transfer = decode_b64_32(&claim.transfer_public_key)?;
    let signing = decode_b64_32(&claim.device.signing_public_key)?;
    let digest = decode_hex32(&claim.credential_digest)?;
    let authenticator = decode_b64_32(&claim.claim_authenticator)?;
    verify_phone_claim_authenticator(
        secret,
        &PhoneClaimMacInput {
            canonical_origin: origin,
            invitation_token_digest: &token_digest,
            owner_transfer_public_key: owner,
            recipient_transfer_public_key: &recipient_transfer,
            recipient_signing_public_key: &signing,
            device_id: claim.device.device_id,
            credential_digest: &digest,
            client_kind: client_kind_name(claim.device.kind),
            device_name: &claim.device.name,
        },
        &authenticator,
    )
    .map_err(|_| HostedClientError::Crypto)
}

fn vault_material(
    profile: &KeyProfile,
    header: &VaultCheckHeader,
    admission_public_key: [u8; 32],
) -> Result<VaultMaterial, HostedClientError> {
    Ok(VaultMaterial {
        profile: serde_json::to_value(profile).map_err(|_| HostedClientError::Crypto)?,
        check_header: URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(header).map_err(|_| HostedClientError::Crypto)?),
        fingerprint: profile
            .fingerprint()
            .map_err(|_| HostedClientError::Crypto)?,
        key_epoch: profile.key_epoch,
        admission_public_key: URL_SAFE_NO_PAD.encode(admission_public_key),
    })
}
fn credential_digest(credential: &str) -> String {
    lowercase_hex(&Sha256::digest(credential.as_bytes()))
}
fn decode_hex32(value: &str) -> Result<[u8; 32], HostedClientError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(HostedClientError::InvalidRequest);
    }
    let mut bytes = [0_u8; 32];
    for (index, chunk) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        bytes[index] = (hex_nibble(chunk[0])? << 4) | hex_nibble(chunk[1])?;
    }
    Ok(bytes)
}
fn decode_b64_32(value: &str) -> Result<[u8; 32], HostedClientError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(HostedClientError::InvalidRequest)
}
fn crypto_role(role: EnrollmentRole) -> CryptoRole {
    match role {
        EnrollmentRole::Owner => CryptoRole::Owner,
        EnrollmentRole::Device => CryptoRole::Device,
        EnrollmentRole::Gateway => CryptoRole::Gateway,
    }
}
fn hex_nibble(byte: u8) -> Result<u8, HostedClientError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(HostedClientError::InvalidRequest),
    }
}
fn lowercase_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://enroll.example";
    const SECRET: &str = "alpha bravo charlie delta echo foxtrot";

    fn phone_claimed_roundtrip() -> (
        EnrollmentClient,
        EnrollmentClient,
        InvitationStatus,
        VaultMaterial,
    ) {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        let owner_request: ClaimInstanceRequest = parse(
            &owner
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .unwrap(),
        )
        .unwrap();
        let invitation: CreateInvitationRequest = parse(
            &owner
                .create_phone_invitation(EnrollmentMethod::Transfer)
                .unwrap(),
        )
        .unwrap();
        owner
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id: Uuid::from_u128(7),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let qr: PhonePairingQr = parse(&owner.get_qr_payload().unwrap()).unwrap();
        let mut recipient =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Android, "phone".into()).unwrap();
        let claim_request: ClaimInvitationRequest = parse(
            &recipient
                .claim_phone_invitation(serde_json::to_string(&qr).unwrap())
                .unwrap(),
        )
        .unwrap();
        // The claim response carries the vault id, obtained from the locally generated profile.
        let profile: KeyProfile =
            serde_json::from_value(owner_request.material.profile.clone()).unwrap();
        recipient
            .consume_claim_response(
                serde_json::to_string(&InvitationClaim {
                    invitation_id: Uuid::from_u128(7),
                    claim_secret: URL_SAFE_NO_PAD.encode([4; 32]),
                    origin: ORIGIN.into(),
                    vault_id: profile.vault_id,
                    fingerprint: owner_request.material.fingerprint.clone(),
                    key_epoch: 1,
                    method: EnrollmentMethod::Transfer,
                    owner_transfer_public_key: invitation.transfer_public_key.clone(),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let transcript = EnrollmentTranscript {
            origin: ORIGIN.into(),
            vault_id: profile.vault_id,
            fingerprint: owner_request.material.fingerprint.clone(),
            key_epoch: 1,
            enrollment_id: Uuid::from_u128(7),
            nonce: claim_request.invitation_token_digest.clone(),
            owner_transfer_public_key: invitation.transfer_public_key,
            recipient_transfer_public_key: claim_request.transfer_public_key,
            signing_public_key: claim_request.device.signing_public_key.clone(),
            device_id: claim_request.device.device_id,
            credential_digest: claim_request.credential_digest.clone(),
            role: EnrollmentRole::Gateway,
        };
        let claimed = InvitationStatus {
            state: EnrollmentState::Claimed,
            expires_in_seconds: 300,
            claim: Some(peppy_hosted_api::enrollment::InvitationPublicClaim {
                device: claim_request.device,
                transfer_public_key: transcript.recipient_transfer_public_key.clone(),
                credential_digest: transcript.credential_digest.clone(),
                claim_authenticator: claim_request.claim_authenticator,
            }),
            transcript: Some(transcript),
            encrypted_key_package: None,
            transfer_nonce: None,
            profile: None,
        };
        (owner, recipient, claimed, owner_request.material)
    }

    fn phone_roundtrip() -> (EnrollmentClient, InvitationStatus, VaultMaterial) {
        let (owner, recipient, claimed, material) = phone_claimed_roundtrip();
        let mut spoofed = claimed.clone();
        spoofed.claim.as_mut().unwrap().credential_digest = "0".repeat(64);
        assert!(
            owner
                .approve_phone_invitation(serde_json::to_string(&spoofed).unwrap(), SECRET.into())
                .is_err()
        );
        let approval: ApproveInvitationRequest = parse(
            &owner
                .approve_phone_invitation(serde_json::to_string(&claimed).unwrap(), SECRET.into())
                .unwrap(),
        )
        .unwrap();
        let approved = InvitationStatus {
            state: EnrollmentState::Approved,
            encrypted_key_package: approval.encrypted_key_package,
            transfer_nonce: approval.transfer_nonce,
            profile: Some(material.profile.clone()),
            ..claimed
        };
        (recipient, approved, material)
    }

    #[test]
    fn phone_transfer_roundtrip_rejects_tampering_and_finishes_only_matching_metadata() {
        let (mut recipient, approved, material) = phone_roundtrip();
        let sas = recipient
            .phone_sas(serde_json::to_string(&approved).unwrap())
            .unwrap();
        assert_eq!(sas.len(), 6);
        let mut tampered = serde_json::to_value(&approved).unwrap();
        tampered["transcript"]["role"] = serde_json::json!("device");
        assert!(
            recipient
                .confirm_phone_invitation(tampered.to_string(), None)
                .is_err()
        );
        let confirm = recipient
            .confirm_phone_invitation(serde_json::to_string(&approved).unwrap(), None)
            .unwrap();
        assert!(confirm.contains("admission_signature"));
        let transcript = approved.transcript.as_ref().unwrap();
        let wrong = AdmissionComplete {
            vault_id: transcript.vault_id,
            device_id: Uuid::new_v4(),
            role: EnrollmentRole::Gateway,
            material: material.clone(),
        };
        assert!(
            recipient
                .finish_phone_admission(serde_json::to_string(&wrong).unwrap())
                .is_err()
        );
    }

    #[test]
    fn owner_rejects_a_consistent_relay_claim_before_sas_or_handoff() {
        let (owner, _recipient, claimed, _material) = phone_claimed_roundtrip();
        let honest_status = serde_json::to_string(&claimed).unwrap();
        let mut relayed = claimed;
        let captured_authenticator = relayed.claim.as_ref().unwrap().claim_authenticator.clone();
        let relay_recipient_key =
            URL_SAFE_NO_PAD.encode(TransferKeypair::generate().unwrap().public_key());
        relayed.claim.as_mut().unwrap().transfer_public_key = relay_recipient_key.clone();
        relayed
            .transcript
            .as_mut()
            .unwrap()
            .recipient_transfer_public_key = relay_recipient_key;

        assert_eq!(
            relayed.claim.as_ref().unwrap().transfer_public_key,
            relayed
                .transcript
                .as_ref()
                .unwrap()
                .recipient_transfer_public_key
        );
        assert_eq!(
            relayed.claim.as_ref().unwrap().claim_authenticator,
            captured_authenticator
        );
        let relayed_status = serde_json::to_string(&relayed).unwrap();
        assert!(owner.phone_sas(relayed_status.clone()).is_err());
        assert!(
            owner
                .approve_phone_invitation(relayed_status, SECRET.into())
                .is_err()
        );
        assert_eq!(owner.phone_sas(honest_status.clone()).unwrap().len(), 6);
        assert!(
            owner
                .approve_phone_invitation(honest_status, SECRET.into())
                .is_ok()
        );
    }

    #[test]
    fn recipient_rejects_a_relay_owner_key_before_sas_or_admission() {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        let owner_request: ClaimInstanceRequest = parse(
            &owner
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .unwrap(),
        )
        .unwrap();
        let invitation: CreateInvitationRequest = parse(
            &owner
                .create_phone_invitation(EnrollmentMethod::Transfer)
                .unwrap(),
        )
        .unwrap();
        owner
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id: Uuid::from_u128(7),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let qr: PhonePairingQr = parse(&owner.get_qr_payload().unwrap()).unwrap();
        let mut recipient =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Android, "phone".into()).unwrap();
        let claim_request: ClaimInvitationRequest = parse(
            &recipient
                .claim_phone_invitation(serde_json::to_string(&qr).unwrap())
                .unwrap(),
        )
        .unwrap();
        let profile: KeyProfile =
            serde_json::from_value(owner_request.material.profile.clone()).unwrap();
        let relay_owner_key =
            URL_SAFE_NO_PAD.encode(TransferKeypair::generate().unwrap().public_key());
        let relay_claim = InvitationClaim {
            invitation_id: Uuid::from_u128(7),
            claim_secret: URL_SAFE_NO_PAD.encode([4; 32]),
            origin: ORIGIN.into(),
            vault_id: profile.vault_id,
            fingerprint: owner_request.material.fingerprint.clone(),
            key_epoch: 1,
            method: EnrollmentMethod::Transfer,
            owner_transfer_public_key: relay_owner_key.clone(),
            expires_in_seconds: 300,
        };
        let relay_status = InvitationStatus {
            state: EnrollmentState::Approved,
            expires_in_seconds: 300,
            claim: Some(peppy_hosted_api::enrollment::InvitationPublicClaim {
                device: claim_request.device.clone(),
                transfer_public_key: claim_request.transfer_public_key.clone(),
                credential_digest: claim_request.credential_digest.clone(),
                claim_authenticator: claim_request.claim_authenticator.clone(),
            }),
            transcript: Some(EnrollmentTranscript {
                origin: ORIGIN.into(),
                vault_id: profile.vault_id,
                fingerprint: owner_request.material.fingerprint.clone(),
                key_epoch: 1,
                enrollment_id: Uuid::from_u128(7),
                nonce: claim_request.invitation_token_digest,
                owner_transfer_public_key: relay_owner_key,
                recipient_transfer_public_key: claim_request.transfer_public_key,
                signing_public_key: claim_request.device.signing_public_key,
                device_id: claim_request.device.device_id,
                credential_digest: claim_request.credential_digest,
                role: EnrollmentRole::Gateway,
            }),
            encrypted_key_package: None,
            transfer_nonce: None,
            profile: None,
        };

        assert!(
            recipient
                .consume_claim_response(serde_json::to_string(&relay_claim).unwrap())
                .is_err()
        );
        let relay_status = serde_json::to_string(&relay_status).unwrap();
        assert!(recipient.phone_sas(relay_status.clone()).is_err());
        assert!(
            recipient
                .confirm_phone_invitation(relay_status, None)
                .is_err()
        );
        assert!(
            recipient
                .finish_phone_admission(
                    serde_json::to_string(&AdmissionComplete {
                        vault_id: profile.vault_id,
                        device_id: claim_request.device.device_id,
                        role: EnrollmentRole::Gateway,
                        material: owner_request.material,
                    })
                    .unwrap(),
                )
                .is_err()
        );
        assert_ne!(
            qr.transfer_public_key,
            relay_claim.owner_transfer_public_key
        );
        assert_eq!(invitation.transfer_public_key, qr.transfer_public_key);
    }

    #[test]
    fn phone_claim_server_payload_does_not_contain_the_qr_secret() {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        owner
            .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
            .unwrap();
        owner
            .create_phone_invitation(EnrollmentMethod::Transfer)
            .unwrap();
        owner
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id: Uuid::from_u128(7),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let qr: PhonePairingQr = parse(&owner.get_qr_payload().unwrap()).unwrap();
        let mut recipient =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Android, "phone".into()).unwrap();
        let server_payload = recipient
            .claim_phone_invitation(serde_json::to_string(&qr).unwrap())
            .unwrap();

        assert!(!server_payload.contains(&qr.invitation_token));
        assert!(
            serde_json::from_str::<serde_json::Value>(&server_payload)
                .unwrap()
                .get("invitation_token")
                .is_none()
        );
    }

    #[test]
    fn claimant_status_descriptor_is_transport_only_and_local_cancel_clears_it() {
        let (mut recipient, approved, _) = phone_roundtrip();
        let descriptor: serde_json::Value =
            serde_json::from_str(&recipient.phone_status_request().unwrap()).unwrap();
        let invitation_id = approved.transcript.as_ref().unwrap().enrollment_id;
        assert_eq!(descriptor["method"], "GET");
        assert_eq!(
            descriptor["path"],
            format!("/v1/enrollment/invitations/{invitation_id}")
        );
        assert!(
            descriptor["headers"]["Peppy-Claim-Secret"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(descriptor.get("invitation_token").is_none());
        recipient.cancel_phone_flow().unwrap();
        assert!(recipient.phone_status_request().is_err());
        assert!(recipient.cancel_phone_flow().is_err());
    }

    #[test]
    fn owner_finish_binds_checkpoint_ids_and_keeps_wrong_secret_unusable() {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        let request: ClaimInstanceRequest = parse(
            &owner
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .unwrap(),
        )
        .unwrap();
        let profile: KeyProfile = serde_json::from_value(request.material.profile.clone()).unwrap();
        let session = peppy_hosted_api::enrollment::InstanceSession {
            session_token: Some("session".into()),
            csrf_token: None,
            expires_in_seconds: 60,
        };
        let wrong = ClaimInstanceResponse {
            vault_id: request.device.device_id,
            device_id: request.device.device_id,
            replayed: false,
            session: session.clone(),
        };
        assert!(
            owner
                .finish_owner_admission(serde_json::to_string(&wrong).unwrap(), SECRET.into())
                .is_err()
        );
        let response = ClaimInstanceResponse {
            vault_id: profile.vault_id,
            device_id: request.device.device_id,
            replayed: false,
            session,
        };
        assert!(
            owner
                .finish_owner_admission(
                    serde_json::to_string(&response).unwrap(),
                    "wrong secret".into()
                )
                .is_err()
        );
        let browser_response = ClaimInstanceResponse {
            session: peppy_hosted_api::enrollment::InstanceSession {
                session_token: None,
                csrf_token: Some("csrf".into()),
                expires_in_seconds: 60,
            },
            ..response
        };
        let identity = owner
            .finish_owner_admission(
                serde_json::to_string(&browser_response).unwrap(),
                SECRET.into(),
            )
            .unwrap();
        assert_eq!(identity.metadata().vault_id, profile.vault_id);
        assert_eq!(identity.metadata().device_id, response.device_id);
        assert_eq!(identity.metadata().header.profile, profile);
        assert_eq!(
            identity.device_credential_for_native_secure_store().len(),
            96
        );
    }

    #[test]
    fn resumed_owner_rejects_a_different_passphrase_before_building_a_request() {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        let first = owner
            .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
            .unwrap();
        let checkpoint = owner.checkpoint().unwrap();
        let mut resumed = EnrollmentClient::restore(checkpoint).unwrap();

        assert!(
            resumed
                .create_owner_request(
                    "setup".into(),
                    "password long enough".into(),
                    "wrong secret with enough separate words".into(),
                )
                .is_err()
        );
        assert_eq!(
            resumed
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .unwrap(),
            first
        );
    }

    #[test]
    fn first_owner_rejects_weak_passphrases_before_checkpointing_material() {
        let mut client =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        for passphrase in ["", "weak", "alpha alpha charlie delta echo foxtrot"] {
            assert!(
                client
                    .create_owner_request(
                        "setup".into(),
                        "password long enough".into(),
                        passphrase.into()
                    )
                    .is_err()
            );
        }
        assert!(
            client
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .is_ok()
        );
    }

    #[test]
    fn existing_owner_can_create_an_invitation_without_entering_fresh_owner_flow() {
        let mut fresh =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        let request: ClaimInstanceRequest = parse(
            &fresh
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .unwrap(),
        )
        .unwrap();
        let profile: KeyProfile = serde_json::from_value(request.material.profile).unwrap();
        let header: VaultCheckHeader =
            serde_json::from_slice(&decode_b64(&request.material.check_header).unwrap()).unwrap();
        let mut owner = EnrollmentClient::for_existing_owner(
            ORIGIN.into(),
            ClientKind::Browser,
            "existing".into(),
            profile.clone(),
            header.clone(),
        )
        .unwrap();
        assert!(
            owner
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .is_err()
        );
        let invitation: CreateInvitationRequest = parse(
            &owner
                .create_phone_invitation(EnrollmentMethod::Transfer)
                .unwrap(),
        )
        .unwrap();
        owner
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id: Uuid::new_v4(),
                    expires_in_seconds: 60,
                })
                .unwrap(),
            )
            .unwrap();
        let qr: PhonePairingQr = parse(&owner.get_qr_payload().unwrap()).unwrap();
        assert_eq!(qr.transfer_public_key, invitation.transfer_public_key);
        assert_eq!(profile.vault_id, header.profile.vault_id);
    }

    #[test]
    fn manual_finish_pins_challenge_and_retries_after_wrong_secret() {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        let owner_request: ClaimInstanceRequest = parse(
            &owner
                .create_owner_request("setup".into(), "password long enough".into(), SECRET.into())
                .unwrap(),
        )
        .unwrap();
        let profile: KeyProfile =
            serde_json::from_value(owner_request.material.profile.clone()).unwrap();
        let mut joiner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Android, "phone".into()).unwrap();
        let request: AdmissionChallengeRequest = parse(
            &joiner
                .manual_challenge_request(Some(profile.vault_id.to_string()))
                .unwrap(),
        )
        .unwrap();
        let challenge = AdmissionChallenge {
            enrollment_id: Uuid::from_u128(8),
            origin: ORIGIN.into(),
            vault_id: profile.vault_id,
            profile: owner_request.material.profile.clone(),
            fingerprint: owner_request.material.fingerprint.clone(),
            key_epoch: profile.key_epoch,
            device: request.device,
            requested_role: EnrollmentRole::Gateway,
            nonce: URL_SAFE_NO_PAD.encode([2; 32]),
            credential_digest: request.credential_digest,
            expires_in_seconds: 60,
        };
        joiner
            .complete_manual_challenge_request(
                serde_json::to_string(&challenge).unwrap(),
                SECRET.into(),
            )
            .unwrap();
        let complete = AdmissionComplete {
            vault_id: profile.vault_id,
            device_id: challenge.device.device_id,
            role: EnrollmentRole::Gateway,
            material: owner_request.material,
        };
        assert!(
            joiner
                .finish_manual_admission(
                    serde_json::to_string(&complete).unwrap(),
                    "wrong secret".into()
                )
                .is_err()
        );
        let identity = joiner
            .finish_manual_admission(serde_json::to_string(&complete).unwrap(), SECRET.into())
            .unwrap();
        assert_eq!(identity.metadata().device_id, challenge.device.device_id);
    }
}
