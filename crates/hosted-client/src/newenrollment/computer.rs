//! QR-first computer enrollment state. Network I/O remains with host adapters.

use super::*;
use peppy_crypto::computer_enrollment::{
    ComputerOwnerProofInput, ComputerRecipientBinding, computer_enrollment_claim_token,
    computer_owner_proof, computer_recipient_commitment, generate_computer_enrollment_qr_secret,
    verify_computer_owner_proof,
};
use peppy_hosted_api::computer_enrollment::{
    ApproveComputerEnrollmentRequest, COMPUTER_ENROLLMENT_PURPOSE, COMPUTER_ENROLLMENT_VERSION,
    ComputerEnrollmentConfirmRequest, ComputerEnrollmentCreated, ComputerEnrollmentRecipient,
    ComputerEnrollmentStatus, ComputerEnrollmentTranscript, ComputerPairingQr,
    CreateComputerEnrollmentRequest,
};

pub(super) enum ComputerFlow {
    ClaimantAwaiting {
        invitation_id: Uuid,
        secret: Zeroizing<[u8; 32]>,
        transfer: TransferKeypair,
        recipient: ComputerEnrollmentRecipient,
        binding: ComputerRecipientBinding,
    },
    PendingAdmission {
        context: EnrollmentContext,
        handoff: Box<VerifiedHandoff>,
        confirm: Zeroizing<String>,
    },
    OwnerScanned {
        qr: ComputerPairingQr,
        binding: ComputerRecipientBinding,
        transfer: TransferKeypair,
        server_nonce: Option<[u8; 32]>,
        owner_device_id: Uuid,
        approval: Option<Zeroizing<Vec<u8>>>,
    },
}

/// Presentation-neutral local status. `awaiting_owner` includes a server 404,
/// which adapters map here rather than treating as an authorization failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputerClaimantView {
    pub invitation_id: Uuid,
    pub awaiting_owner: bool,
    pub state: EnrollmentState,
    pub expires_in_seconds: u32,
    pub sas: Option<String>,
}

impl EnrollmentClient {
    /// Generates every claimant secret and identity value before any network request.
    /// Only desktop and browser clients may be computer recipients.
    pub fn begin_computer_claimant(&mut self) -> Result<String, HostedClientError> {
        if !matches!(self.kind, ClientKind::Desktop | ClientKind::Browser)
            || self.phone.is_some()
            || self.computer.is_some()
            || self.manual.is_some()
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let invitation_id = Uuid::new_v4();
        let secret =
            generate_computer_enrollment_qr_secret().map_err(|_| HostedClientError::Crypto)?;
        let transfer = TransferKeypair::generate().map_err(|_| HostedClientError::Crypto)?;
        let recipient = ComputerEnrollmentRecipient {
            device: self.device()?,
            credential_digest: credential_digest(&self.credential),
            requested_role: EnrollmentRole::Device,
        };
        let binding = computer_binding(&recipient, transfer.public_key())?;
        let commitment =
            computer_recipient_commitment(&binding).map_err(|_| HostedClientError::Crypto)?;
        let qr = ComputerPairingQr {
            version: COMPUTER_ENROLLMENT_VERSION,
            purpose: COMPUTER_ENROLLMENT_PURPOSE.into(),
            origin: self.origin.clone(),
            invitation_id,
            invitation_secret: URL_SAFE_NO_PAD.encode(*secret),
            recipient: recipient.clone(),
            recipient_transfer_public_key: URL_SAFE_NO_PAD.encode(transfer.public_key()),
            recipient_commitment: URL_SAFE_NO_PAD.encode(commitment),
        };
        self.computer = Some(ComputerFlow::ClaimantAwaiting {
            invitation_id,
            secret,
            transfer,
            recipient,
            binding,
        });
        serde_json::to_string(&qr).map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Private-worker/native transport descriptor for the claimant status route.
    pub fn computer_status_request(&self) -> Result<String, HostedClientError> {
        let ComputerFlow::ClaimantAwaiting {
            invitation_id,
            secret,
            ..
        } = self
            .computer
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        let claim_token =
            computer_enrollment_claim_token(secret).map_err(|_| HostedClientError::Crypto)?;
        serde_json::to_string(&serde_json::json!({
            "method": "POST",
            "path": format!("/v1/enrollment/computers/{invitation_id}/status"),
            "body": { "claim_token": URL_SAFE_NO_PAD.encode(claim_token) },
        }))
        .map_err(|_| HostedClientError::InvalidRequest)
    }

    /// Parses a claimant status. Hosts map an id-addressed 404 to
    /// `computer_awaiting_owner_view`; it is not a proof or an admission.
    pub fn computer_claimant_view(
        &self,
        status_json: String,
    ) -> Result<ComputerClaimantView, HostedClientError> {
        let status: ComputerEnrollmentStatus = parse(&status_json)?;
        let ComputerFlow::ClaimantAwaiting { invitation_id, .. } = self
            .computer
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        let (expires_in_seconds, sas) = match status.state {
            EnrollmentState::Expired | EnrollmentState::Denied => (0, None),
            EnrollmentState::Approved => {
                if status.expires_in_seconds == 0 {
                    return Err(HostedClientError::InvalidRequest);
                }
                let context = self.verified_computer_approval(&status)?;
                (
                    status.expires_in_seconds,
                    Some(context.sas().map_err(|_| HostedClientError::Crypto)?),
                )
            }
            _ if status.expires_in_seconds > 0 => (status.expires_in_seconds, None),
            _ => return Err(HostedClientError::InvalidRequest),
        };
        Ok(ComputerClaimantView {
            invitation_id: *invitation_id,
            awaiting_owner: false,
            state: status.state,
            expires_in_seconds,
            sas,
        })
    }

    pub fn computer_awaiting_owner_view(&self) -> Result<ComputerClaimantView, HostedClientError> {
        let ComputerFlow::ClaimantAwaiting { invitation_id, .. } = self
            .computer
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        Ok(ComputerClaimantView {
            invitation_id: *invitation_id,
            awaiting_owner: true,
            state: EnrollmentState::Waiting,
            expires_in_seconds: 0,
            sas: None,
        })
    }

    /// Verifies every owner-controlled field and MAC before trusting the owner key or opening the handoff.
    pub fn confirm_computer_enrollment(
        &mut self,
        status_json: String,
    ) -> Result<String, HostedClientError> {
        if let Some(ComputerFlow::PendingAdmission { confirm, .. }) = self.computer.as_ref() {
            return Ok(confirm.to_string());
        }
        let status: ComputerEnrollmentStatus = parse(&status_json)?;
        if status.state != EnrollmentState::Approved {
            return Err(HostedClientError::InvalidRequest);
        }
        if status.expires_in_seconds == 0 {
            return Err(HostedClientError::InvalidRequest);
        }
        let context = self.verified_computer_approval(&status)?;
        let ComputerFlow::ClaimantAwaiting {
            secret, transfer, ..
        } = self
            .computer
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
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
            transfer,
            &context.owner_transfer_public_key,
            &context,
            &EncryptedHandoff { nonce, ciphertext },
        )
        .map_err(|_| HostedClientError::Crypto)?;
        let claim_token =
            computer_enrollment_claim_token(secret).map_err(|_| HostedClientError::Crypto)?;
        let device_signature = sign_device_proof(&self.device_secret_key()?, &context)
            .map_err(|_| HostedClientError::Crypto)?;
        let admission_signature = handoff
            .sign_admission_context(&context)
            .map_err(|_| HostedClientError::Crypto)?;
        let confirm = serde_json::to_string(&ComputerEnrollmentConfirmRequest {
            claim_token: URL_SAFE_NO_PAD.encode(claim_token),
            device_signature: URL_SAFE_NO_PAD.encode(device_signature),
            admission_signature: URL_SAFE_NO_PAD.encode(admission_signature),
        })
        .map_err(|_| HostedClientError::InvalidRequest)?;
        let _ = self.computer.take();
        self.computer = Some(ComputerFlow::PendingAdmission {
            context,
            handoff: Box::new(handoff),
            confirm: Zeroizing::new(confirm.clone()),
        });
        Ok(confirm)
    }

    /// Converts a scanned QR into the owner-authenticated create request.
    pub fn scan_computer_qr(&mut self, qr_json: String) -> Result<String, HostedClientError> {
        if self.mode != EnrollmentMode::ExistingOwner
            || self.owner.is_none()
            || self.computer_owner_device_id.is_none()
            || self.phone.is_some()
            || self.computer.is_some()
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let qr: ComputerPairingQr = parse(&qr_json)?;
        if qr.version != COMPUTER_ENROLLMENT_VERSION
            || qr.purpose != COMPUTER_ENROLLMENT_PURPOSE
            || qr.origin != self.origin
            || qr.invitation_id.is_nil()
            || !matches!(
                qr.recipient.device.kind,
                ClientKind::Desktop | ClientKind::Browser
            )
            || qr.recipient.requested_role != EnrollmentRole::Device
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let secret = decode_b64_32(&qr.invitation_secret)?;
        let transfer_public_key = decode_b64_32(&qr.recipient_transfer_public_key)?;
        let binding = computer_binding(&qr.recipient, transfer_public_key)?;
        if URL_SAFE_NO_PAD
            .encode(computer_recipient_commitment(&binding).map_err(|_| HostedClientError::Crypto)?)
            != qr.recipient_commitment
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let claim_token =
            computer_enrollment_claim_token(&secret).map_err(|_| HostedClientError::Crypto)?;
        let request = CreateComputerEnrollmentRequest {
            invitation_id: qr.invitation_id,
            recipient: qr.recipient.clone(),
            recipient_transfer_public_key: qr.recipient_transfer_public_key.clone(),
            recipient_commitment: qr.recipient_commitment.clone(),
            claim_token_digest: URL_SAFE_NO_PAD.encode(Sha256::digest(claim_token)),
        };
        self.computer = Some(ComputerFlow::OwnerScanned {
            qr,
            binding,
            transfer: TransferKeypair::generate().map_err(|_| HostedClientError::Crypto)?,
            server_nonce: None,
            owner_device_id: self
                .computer_owner_device_id
                .ok_or(HostedClientError::InvalidRequest)?,
            approval: None,
        });
        serde_json::to_string(&request).map_err(|_| HostedClientError::InvalidRequest)
    }

    pub fn accept_computer_created(
        &mut self,
        created_json: String,
    ) -> Result<(), HostedClientError> {
        let created: ComputerEnrollmentCreated = parse(&created_json)?;
        if created.expires_in_seconds == 0 {
            return Err(HostedClientError::InvalidRequest);
        }
        let ComputerFlow::OwnerScanned { server_nonce, .. } = self
            .computer
            .as_mut()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        if server_nonce.is_some() {
            return Err(HostedClientError::InvalidRequest);
        }
        *server_nonce = Some(decode_b64_32(&created.server_nonce)?);
        Ok(())
    }

    /// Returns locally derived SAS and emits approval only after the owner confirms it matches.
    pub fn approve_computer_enrollment_with_client(
        &mut self,
        codes_match: bool,
        client: &peppy_client_core::Client,
    ) -> Result<String, HostedClientError> {
        if !codes_match {
            return Err(HostedClientError::InvalidRequest);
        }
        let owner = self
            .owner
            .clone()
            .ok_or(HostedClientError::InvalidRequest)?;
        let ComputerFlow::OwnerScanned {
            qr,
            binding,
            transfer,
            server_nonce: Some(server_nonce),
            owner_device_id,
            approval,
        } = self
            .computer
            .as_mut()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        if let Some(approval) = approval {
            return String::from_utf8(approval.to_vec()).map_err(|_| HostedClientError::Crypto);
        }
        let context = owner_computer_context(
            &self.origin,
            &owner,
            qr,
            *server_nonce,
            transfer.public_key(),
        )?;
        let encrypted = client
            .seal_phone_handoff(transfer, &context)
            .map_err(|_| HostedClientError::KeysUnavailable)?;
        let proof_input = ComputerOwnerProofInput {
            context: &context,
            recipient: binding,
            owner_device_id: *owner_device_id,
            granted_role: CryptoRole::Device,
            transfer_nonce: &encrypted.nonce,
            handoff_ciphertext_hash: &Sha256::digest(&encrypted.ciphertext).into(),
        };
        let secret = decode_b64_32(&qr.invitation_secret)?;
        let proof =
            computer_owner_proof(&secret, &proof_input).map_err(|_| HostedClientError::Crypto)?;
        let request = serde_json::to_string(&ApproveComputerEnrollmentRequest {
            transcript: computer_transcript(&context, qr.recipient.clone(), *owner_device_id)?,
            encrypted_key_package: URL_SAFE_NO_PAD.encode(encrypted.ciphertext),
            transfer_nonce: URL_SAFE_NO_PAD.encode(encrypted.nonce),
            owner_proof: URL_SAFE_NO_PAD.encode(proof),
        })
        .map_err(|_| HostedClientError::InvalidRequest)?;
        *approval = Some(Zeroizing::new(request.as_bytes().to_vec()));
        Ok(request)
    }

    pub fn computer_sas(&self) -> Result<String, HostedClientError> {
        match self.computer.as_ref() {
            Some(ComputerFlow::OwnerScanned {
                qr,
                transfer,
                server_nonce: Some(nonce),
                ..
            }) => owner_computer_context(
                &self.origin,
                self.owner
                    .as_ref()
                    .ok_or(HostedClientError::InvalidRequest)?,
                qr,
                *nonce,
                transfer.public_key(),
            )?
            .sas()
            .map_err(|_| HostedClientError::Crypto),
            Some(ComputerFlow::ClaimantAwaiting { .. }) => Err(HostedClientError::InvalidRequest),
            _ => Err(HostedClientError::InvalidRequest),
        }
    }

    pub fn finish_computer_admission(
        &mut self,
        complete_json: String,
    ) -> Result<NativeEnrollmentIdentity, HostedClientError> {
        let complete: AdmissionComplete = parse(&complete_json)?;
        let ComputerFlow::PendingAdmission {
            context, handoff, ..
        } = self
            .computer
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        if complete.vault_id != context.vault_id
            || complete.device_id != context.device_id
            || complete.role != EnrollmentRole::Device
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let (profile, header) = material_profile_header(&complete.material)?;
        validate_verified_handoff_material(handoff, &profile, &header, context)?;
        let flow = self
            .computer
            .take()
            .ok_or(HostedClientError::InvalidRequest)?;
        let ComputerFlow::PendingAdmission {
            context, handoff, ..
        } = flow
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        Ok(self.native_identity(
            context.vault_id,
            context.device_id,
            EnrollmentRole::Device,
            profile,
            header,
            Some(NativeIdentityActivation(*handoff)),
        ))
    }

    fn computer_context(
        &self,
        status: &ComputerEnrollmentStatus,
    ) -> Result<EnrollmentContext, HostedClientError> {
        let transcript = status
            .transcript
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?;
        transcript
            .validate_fields()
            .map_err(|_| HostedClientError::InvalidRequest)?;
        let ComputerFlow::ClaimantAwaiting {
            invitation_id,
            transfer,
            recipient,
            ..
        } = self
            .computer
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
        if transcript.origin != self.origin
            || transcript.invitation_id != *invitation_id
            || transcript.recipient != *recipient
            || transcript.recipient_transfer_public_key
                != URL_SAFE_NO_PAD.encode(transfer.public_key())
            || transcript.granted_role != EnrollmentRole::Device
        {
            return Err(HostedClientError::InvalidRequest);
        }
        computer_context_from_transcript(transcript)
    }

    fn verified_computer_approval(
        &self,
        status: &ComputerEnrollmentStatus,
    ) -> Result<EnrollmentContext, HostedClientError> {
        let context = self.computer_context(status)?;
        let ComputerFlow::ClaimantAwaiting {
            invitation_id,
            secret,
            binding,
            ..
        } = self
            .computer
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
        else {
            return Err(HostedClientError::InvalidRequest);
        };
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
        let proof = status
            .owner_proof
            .as_deref()
            .ok_or(HostedClientError::InvalidRequest)
            .and_then(decode_b64_32)?;
        let owner_device_id = status
            .transcript
            .as_ref()
            .ok_or(HostedClientError::InvalidRequest)?
            .owner_device_id;
        let ciphertext_hash: [u8; 32] = Sha256::digest(&ciphertext).into();
        let input = ComputerOwnerProofInput {
            context: &context,
            recipient: binding,
            owner_device_id,
            granted_role: CryptoRole::Device,
            transfer_nonce: &nonce,
            handoff_ciphertext_hash: &ciphertext_hash,
        };
        if !verify_computer_owner_proof(
            secret,
            &self.origin,
            *invitation_id,
            binding,
            &input,
            &proof,
        )
        .map_err(|_| HostedClientError::Crypto)?
        {
            return Err(HostedClientError::InvalidRequest);
        }
        Ok(context)
    }

    /// Abandons every locally retained computer enrollment secret and pending handoff.
    pub fn cancel_computer_flow(&mut self) -> Result<(), HostedClientError> {
        let mut flow = self
            .computer
            .take()
            .ok_or(HostedClientError::InvalidRequest)?;
        if let ComputerFlow::OwnerScanned { qr, approval, .. } = &mut flow {
            qr.invitation_secret.zeroize();
            if let Some(bytes) = approval {
                bytes.zeroize();
            }
        }
        Ok(())
    }
}

fn computer_binding(
    recipient: &ComputerEnrollmentRecipient,
    transfer_public_key: [u8; 32],
) -> Result<ComputerRecipientBinding, HostedClientError> {
    Ok(ComputerRecipientBinding {
        transfer_public_key,
        signing_public_key: decode_b64_32(&recipient.device.signing_public_key)?,
        device_id: recipient.device.device_id,
        credential_digest: decode_hex32(&recipient.credential_digest)?,
        requested_role: crypto_role(recipient.requested_role),
        client_kind: client_kind_name(recipient.device.kind).into(),
        device_name: recipient.device.name.clone(),
    })
}
fn computer_context_from_transcript(
    value: &ComputerEnrollmentTranscript,
) -> Result<EnrollmentContext, HostedClientError> {
    Ok(EnrollmentContext {
        purpose: EnrollmentPurpose::Computer,
        canonical_origin: value.origin.clone(),
        vault_id: value.vault_id,
        fingerprint: decode_hex32(&value.fingerprint)?,
        key_epoch: value.key_epoch,
        enrollment_id: value.invitation_id,
        server_nonce: decode_b64_32(&value.server_nonce)?,
        owner_transfer_public_key: decode_b64_32(&value.owner_transfer_public_key)?,
        recipient_transfer_public_key: decode_b64_32(&value.recipient_transfer_public_key)?,
        recipient_signing_public_key: decode_b64_32(&value.recipient.device.signing_public_key)?,
        device_id: value.recipient.device.device_id,
        credential_digest: decode_hex32(&value.recipient.credential_digest)?,
        role: crypto_role(value.granted_role),
    })
}
fn owner_computer_context(
    origin: &str,
    owner: &OwnerMaterial,
    qr: &ComputerPairingQr,
    server_nonce: [u8; 32],
    owner_transfer_public_key: [u8; 32],
) -> Result<EnrollmentContext, HostedClientError> {
    Ok(EnrollmentContext {
        purpose: EnrollmentPurpose::Computer,
        canonical_origin: origin.into(),
        vault_id: owner.profile.vault_id,
        fingerprint: decode_hex32(
            &owner
                .profile
                .fingerprint()
                .map_err(|_| HostedClientError::InvalidProfile)?,
        )?,
        key_epoch: owner.profile.key_epoch,
        enrollment_id: qr.invitation_id,
        server_nonce,
        owner_transfer_public_key,
        recipient_transfer_public_key: decode_b64_32(&qr.recipient_transfer_public_key)?,
        recipient_signing_public_key: decode_b64_32(&qr.recipient.device.signing_public_key)?,
        device_id: qr.recipient.device.device_id,
        credential_digest: decode_hex32(&qr.recipient.credential_digest)?,
        role: CryptoRole::Device,
    })
}
fn computer_transcript(
    context: &EnrollmentContext,
    recipient: ComputerEnrollmentRecipient,
    owner_device_id: Uuid,
) -> Result<ComputerEnrollmentTranscript, HostedClientError> {
    Ok(ComputerEnrollmentTranscript {
        origin: context.canonical_origin.clone(),
        vault_id: context.vault_id,
        fingerprint: lowercase_hex(&context.fingerprint),
        key_epoch: context.key_epoch,
        invitation_id: context.enrollment_id,
        server_nonce: URL_SAFE_NO_PAD.encode(context.server_nonce),
        owner_transfer_public_key: URL_SAFE_NO_PAD.encode(context.owner_transfer_public_key),
        owner_device_id,
        recipient,
        recipient_transfer_public_key: URL_SAFE_NO_PAD
            .encode(context.recipient_transfer_public_key),
        granted_role: EnrollmentRole::Device,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://enroll.example";
    const SECRET: &str = "alpha bravo charlie delta echo foxtrot";

    fn owner() -> EnrollmentClient {
        let mut fresh =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "owner".into()).unwrap();
        let request: ClaimInstanceRequest = parse(
            &fresh
                .create_owner_request(
                    "setup".into(),
                    "admin".into(),
                    "password long enough".into(),
                    SECRET.into(),
                )
                .unwrap(),
        )
        .unwrap();
        let profile = serde_json::from_value(request.material.profile).unwrap();
        let header =
            serde_json::from_slice(&decode_b64(&request.material.check_header).unwrap()).unwrap();
        let mut owner = EnrollmentClient::for_existing_owner(
            ORIGIN.into(),
            ClientKind::Desktop,
            "owner".into(),
            profile,
            header,
        )
        .unwrap();
        owner
            .configure_computer_owner_device(Uuid::from_u128(77))
            .unwrap();
        owner
    }

    #[test]
    fn qr_first_roundtrip_is_local_and_owner_creation_is_bound_to_it() {
        let mut claimant =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "computer".into()).unwrap();
        let qr_json = claimant.begin_computer_claimant().unwrap();
        let qr: ComputerPairingQr = parse(&qr_json).unwrap();
        assert_eq!(qr.origin, ORIGIN);
        assert_eq!(qr.recipient.requested_role, EnrollmentRole::Device);
        assert!(
            claimant
                .computer_status_request()
                .unwrap()
                .contains(&qr.invitation_id.to_string())
        );

        let mut owner = owner();
        let create: CreateComputerEnrollmentRequest =
            parse(&owner.scan_computer_qr(qr_json).unwrap()).unwrap();
        assert_eq!(create.invitation_id, qr.invitation_id);
        assert_eq!(create.recipient, qr.recipient);
        assert_eq!(create.recipient_commitment, qr.recipient_commitment);
    }

    #[test]
    fn claimant_rejects_malicious_or_expired_responses_before_admission() {
        let mut claimant =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Browser, "computer".into()).unwrap();
        claimant.begin_computer_claimant().unwrap();
        let status = ComputerEnrollmentStatus {
            state: EnrollmentState::Approved,
            expires_in_seconds: 60,
            transcript: None,
            encrypted_key_package: None,
            transfer_nonce: None,
            owner_proof: None,
        };
        assert!(
            claimant
                .confirm_computer_enrollment(serde_json::to_string(&status).unwrap())
                .is_err()
        );
        assert!(claimant.finish_computer_admission("{}".into()).is_err());
        assert!(
            claimant.computer_status_request().is_ok(),
            "a rejected response cannot consume local state"
        );
    }

    #[test]
    fn owner_scan_requires_owner_role_and_created_response_cannot_be_replayed() {
        let mut claimant =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "computer".into()).unwrap();
        let qr = claimant.begin_computer_claimant().unwrap();
        let mut not_owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "other".into()).unwrap();
        assert!(not_owner.scan_computer_qr(qr.clone()).is_err());

        let mut owner = owner();
        owner.scan_computer_qr(qr).unwrap();
        let created = serde_json::to_string(&ComputerEnrollmentCreated {
            server_nonce: URL_SAFE_NO_PAD.encode([7; 32]),
            expires_in_seconds: 60,
        })
        .unwrap();
        owner.accept_computer_created(created.clone()).unwrap();
        assert!(
            owner.accept_computer_created(created).is_err(),
            "a race retry must not replace the pinned nonce"
        );
        assert!(owner.computer_sas().is_ok());
    }

    #[test]
    fn computer_finish_rejects_same_profile_wrong_root_header_without_consuming_handoff() {
        let mut claimant =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Browser, "computer".into()).unwrap();
        let vault_id = Uuid::new_v4();
        let profile = KeyProfile::new(vault_id, 3).unwrap();
        let root = derive_root_key(SECRET, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let sender = TransferKeypair::generate().unwrap();
        let recipient = TransferKeypair::generate().unwrap();
        let context = EnrollmentContext {
            purpose: EnrollmentPurpose::Computer,
            canonical_origin: ORIGIN.into(),
            vault_id,
            fingerprint: decode_hex32(&profile.fingerprint().unwrap()).unwrap(),
            key_epoch: profile.key_epoch,
            enrollment_id: Uuid::new_v4(),
            server_nonce: [2; 32],
            owner_transfer_public_key: sender.public_key(),
            recipient_transfer_public_key: recipient.public_key(),
            recipient_signing_public_key: [3; 32],
            device_id: claimant.device_id,
            credential_digest: [4; 32],
            role: CryptoRole::Device,
        };
        let package = HandoffRootPackage::new(root, profile.clone(), header.clone()).unwrap();
        let encrypted = seal_handoff(&sender, &recipient.public_key(), &context, package).unwrap();
        let handoff = open_handoff(&recipient, &sender.public_key(), &context, &encrypted).unwrap();
        claimant.computer = Some(ComputerFlow::PendingAdmission {
            context: context.clone(),
            handoff: Box::new(handoff),
            confirm: Zeroizing::new("confirm".into()),
        });

        let attacker_root = derive_root_key("attacker phrase", &profile).unwrap();
        let attacker_header = create_vault_check_header(&attacker_root, profile.clone()).unwrap();
        let wrong = AdmissionComplete {
            vault_id,
            device_id: context.device_id,
            role: EnrollmentRole::Device,
            material: vault_material(&profile, &attacker_header, [0; 32]).unwrap(),
        };
        assert!(
            claimant
                .finish_computer_admission(serde_json::to_string(&wrong).unwrap())
                .is_err()
        );

        let correct = AdmissionComplete {
            vault_id,
            device_id: context.device_id,
            role: EnrollmentRole::Device,
            material: vault_material(&profile, &header, [0; 32]).unwrap(),
        };
        let mut identity = claimant
            .finish_computer_admission(serde_json::to_string(&correct).unwrap())
            .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "peppy-hosted-client-activation-retry-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let wrong_client = peppy_client_core::Client::open(
            peppy_client_core::ClientConfig {
                database_path: directory.join("wrong.db"),
                vault_id: peppy_client_core::VaultId(vault_id),
                device_id: peppy_client_core::DeviceId(Uuid::new_v4()),
            },
            peppy_client_core::DatabaseKey::new(&[41; 32]).unwrap(),
        )
        .unwrap();
        assert!(identity.activate_verified_handoff(&wrong_client).is_err());

        let correct_client = peppy_client_core::Client::open(
            peppy_client_core::ClientConfig {
                database_path: directory.join("correct.db"),
                vault_id: peppy_client_core::VaultId(vault_id),
                device_id: peppy_client_core::DeviceId(context.device_id),
            },
            peppy_client_core::DatabaseKey::new(&[42; 32]).unwrap(),
        )
        .unwrap();
        assert!(identity.activate_verified_handoff(&correct_client).unwrap());
    }

    #[test]
    fn phones_cannot_be_computer_recipients() {
        let mut phone =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Android, "phone".into()).unwrap();
        assert!(phone.begin_computer_claimant().is_err());
    }
}
