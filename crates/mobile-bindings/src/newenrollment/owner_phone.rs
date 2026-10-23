use super::{NativeEnrollmentClient, NativeEnrollmentMethod};
use crate::MobileBindingsError;
use peppy_hosted_api::enrollment::{EnrollmentMethod, EnrollmentState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NativeOwnerPhoneStage {
    Waiting,
    Verify,
    Approved,
    Connected,
    Expired,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NativeOwnerPhoneStatus {
    pub invitation_id: String,
    pub stage: NativeOwnerPhoneStage,
    pub expires_in_seconds: u32,
    /// Locally derived six-digit SAS; present only while verification is required.
    pub sas: Option<String>,
    /// SHA-256 base64url-no-pad digest of the validated transcript and typed claim.
    pub claim_digest: Option<String>,
}

#[uniffi::export]
impl NativeEnrollmentClient {
    pub fn owner_phone_qr_payload(&self) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .get_qr_payload()
            .map_err(super::enrollment_error)
    }

    pub fn owner_passphrase_matches(
        &self,
        passphrase: String,
    ) -> Result<bool, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .owner_passphrase_matches(&passphrase)
            .map_err(super::enrollment_error)
    }

    pub fn owner_phone_invitation_id(&self) -> Result<Option<String>, MobileBindingsError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .owner_invitation_id()
            .map(|id| id.to_string()))
    }

    pub fn owner_phone_status(
        &self,
        status_json: String,
    ) -> Result<NativeOwnerPhoneStatus, MobileBindingsError> {
        let view = self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .owner_phone_view(status_json)
            .map_err(super::enrollment_error)?;
        Ok(NativeOwnerPhoneStatus {
            invitation_id: view.invitation_id.to_string(),
            stage: owner_phone_stage(view.state),
            expires_in_seconds: view.expires_in_seconds,
            sas: view.sas,
            claim_digest: view.claim_digest,
        })
    }

    pub fn owner_phone_method(
        &self,
    ) -> Result<Option<NativeEnrollmentMethod>, MobileBindingsError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .owner_phone_method()
            .map(native_enrollment_method))
    }
}

#[uniffi::export]
pub fn owner_phone_should_rotate(
    stage: NativeOwnerPhoneStage,
    expires_in_seconds: u32,
    active: NativeEnrollmentMethod,
    requested: NativeEnrollmentMethod,
) -> bool {
    stage == NativeOwnerPhoneStage::Expired
        || (stage == NativeOwnerPhoneStage::Waiting
            && (expires_in_seconds <= 15 || active != requested))
}

fn native_enrollment_method(method: EnrollmentMethod) -> NativeEnrollmentMethod {
    match method {
        EnrollmentMethod::Transfer => NativeEnrollmentMethod::Transfer,
        EnrollmentMethod::SharedSecret => NativeEnrollmentMethod::SharedSecret,
    }
}

fn owner_phone_stage(state: EnrollmentState) -> NativeOwnerPhoneStage {
    match state {
        EnrollmentState::Waiting => NativeOwnerPhoneStage::Waiting,
        EnrollmentState::Claimed => NativeOwnerPhoneStage::Verify,
        EnrollmentState::Approved => NativeOwnerPhoneStage::Approved,
        EnrollmentState::Confirmed | EnrollmentState::Consumed => NativeOwnerPhoneStage::Connected,
        EnrollmentState::Expired => NativeOwnerPhoneStage::Expired,
        EnrollmentState::Denied => NativeOwnerPhoneStage::Denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_vault_material;
    use crate::newenrollment::{
        NativeEnrollmentClientKind, new_native_enrollment_client,
        new_native_existing_owner_enrollment_client,
    };
    use peppy_crypto::KeyProfile;
    use peppy_hosted_api::enrollment::{
        ClaimInvitationRequest, EnrollmentRole, EnrollmentTranscript, InvitationClaim,
        InvitationCreated, InvitationStatus, encode_base64url,
    };
    use std::sync::Arc;
    use uuid::Uuid;

    const ORIGIN: &str = "https://enroll.example";
    const SECRET: &str = "alpha bravo charlie delta echo foxtrot";
    const INVITATION_ID: Uuid = Uuid::from_u128(7);

    fn claimed_roundtrip() -> (
        Arc<NativeEnrollmentClient>,
        Arc<NativeEnrollmentClient>,
        InvitationStatus,
        serde_json::Value,
    ) {
        let material = create_vault_material(Uuid::new_v4().to_string(), SECRET.into()).unwrap();
        let profile: KeyProfile = serde_json::from_str(&material.profile_json).unwrap();
        let fingerprint = profile.fingerprint().unwrap();
        let owner = new_native_existing_owner_enrollment_client(
            ORIGIN.into(),
            NativeEnrollmentClientKind::Android,
            "owner".into(),
            material.profile_json,
            material.header_json,
        )
        .unwrap();
        let invitation = owner
            .create_phone_invitation_request(NativeEnrollmentMethod::Transfer)
            .unwrap();
        let invitation: peppy_hosted_api::enrollment::CreateInvitationRequest =
            serde_json::from_str(&invitation).unwrap();
        owner
            .consume_created_phone_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id: INVITATION_ID,
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let recipient = new_native_enrollment_client(
            ORIGIN.into(),
            NativeEnrollmentClientKind::Android,
            "phone".into(),
        )
        .unwrap();
        let claim_request: ClaimInvitationRequest = serde_json::from_str(
            &recipient
                .claim_phone_invitation_request(owner.owner_phone_qr_payload().unwrap())
                .unwrap(),
        )
        .unwrap();
        recipient
            .consume_phone_claim(
                serde_json::to_string(&InvitationClaim {
                    invitation_id: INVITATION_ID,
                    claim_secret: encode_base64url(&[4; 32]),
                    origin: ORIGIN.into(),
                    vault_id: profile.vault_id,
                    fingerprint: fingerprint.clone(),
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
            fingerprint,
            key_epoch: 1,
            enrollment_id: INVITATION_ID,
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
        (
            owner,
            recipient,
            claimed,
            serde_json::to_value(profile).unwrap(),
        )
    }

    #[test]
    fn owner_phone_qr_claim_status_and_digest_are_locally_verified() {
        let (owner, recipient, claimed, _) = claimed_roundtrip();
        assert_eq!(
            owner.owner_phone_invitation_id().unwrap(),
            Some(INVITATION_ID.to_string())
        );
        assert!(recipient.phone_status_request().is_ok());

        let status_json = serde_json::to_string(&claimed).unwrap();
        let view = owner.owner_phone_status(status_json.clone()).unwrap();
        assert_eq!(view.stage, NativeOwnerPhoneStage::Verify);
        assert_eq!(view.sas, Some(recipient.phone_sas(status_json).unwrap()));
        assert_eq!(
            view.claim_digest,
            owner
                .owner_phone_status(serde_json::to_string(&claimed).unwrap())
                .unwrap()
                .claim_digest
        );

        let mut relayed = claimed;
        let relayed_key = encode_base64url(&[9; 32]);
        relayed.claim.as_mut().unwrap().transfer_public_key = relayed_key.clone();
        relayed
            .transcript
            .as_mut()
            .unwrap()
            .recipient_transfer_public_key = relayed_key;
        assert!(
            owner
                .owner_phone_status(serde_json::to_string(&relayed).unwrap())
                .is_err()
        );
    }

    #[test]
    fn owner_phone_status_maps_terminal_states_and_rejects_foreign_or_malformed_statuses() {
        let (owner, _, claimed, _) = claimed_roundtrip();
        for (state, expected) in [
            (EnrollmentState::Approved, NativeOwnerPhoneStage::Approved),
            (EnrollmentState::Confirmed, NativeOwnerPhoneStage::Connected),
            (EnrollmentState::Consumed, NativeOwnerPhoneStage::Connected),
            (EnrollmentState::Expired, NativeOwnerPhoneStage::Expired),
            (EnrollmentState::Denied, NativeOwnerPhoneStage::Denied),
        ] {
            let mut status = claimed.clone();
            status.state = state;
            assert_eq!(
                owner
                    .owner_phone_status(serde_json::to_string(&status).unwrap())
                    .unwrap()
                    .stage,
                expected
            );
            let view = owner
                .owner_phone_status(serde_json::to_string(&status).unwrap())
                .unwrap();
            assert_eq!(view.sas, None);
            assert_eq!(
                view.claim_digest.is_some(),
                state == EnrollmentState::Approved
            );
        }
        let waiting = InvitationStatus {
            state: EnrollmentState::Waiting,
            expires_in_seconds: 16,
            claim: None,
            transcript: None,
            encrypted_key_package: None,
            transfer_nonce: None,
            profile: None,
        };
        assert_eq!(
            owner
                .owner_phone_status(serde_json::to_string(&waiting).unwrap())
                .unwrap()
                .stage,
            NativeOwnerPhoneStage::Waiting
        );
        let mut foreign = claimed;
        foreign.transcript.as_mut().unwrap().enrollment_id = Uuid::new_v4();
        for state in [
            EnrollmentState::Waiting,
            EnrollmentState::Expired,
            EnrollmentState::Denied,
        ] {
            let mut status = foreign.clone();
            status.state = state;
            assert!(
                owner
                    .owner_phone_status(serde_json::to_string(&status).unwrap())
                    .is_err()
            );
        }
        assert!(owner.owner_phone_status("not json".into()).is_err());
    }

    #[test]
    fn owner_phone_secret_and_approval_complete_the_recipient_handoff() {
        let (owner, recipient, claimed, material) = claimed_roundtrip();
        assert!(owner.owner_passphrase_matches(SECRET.into()).unwrap());
        assert!(
            !owner
                .owner_passphrase_matches("wrong secret".into())
                .unwrap()
        );
        let approval = owner
            .approve_phone_invitation_request(
                serde_json::to_string(&claimed).unwrap(),
                SECRET.into(),
            )
            .unwrap();
        let approval: peppy_hosted_api::enrollment::ApproveInvitationRequest =
            serde_json::from_str(&approval).unwrap();
        let approved = InvitationStatus {
            state: EnrollmentState::Approved,
            encrypted_key_package: approval.encrypted_key_package,
            transfer_nonce: approval.transfer_nonce,
            profile: Some(material),
            ..claimed
        };
        assert!(
            recipient
                .confirm_phone_invitation_request(serde_json::to_string(&approved).unwrap(), None)
                .is_ok()
        );
    }

    #[test]
    fn owner_phone_rotation_matches_desktop_policy() {
        assert!(!owner_phone_should_rotate(
            NativeOwnerPhoneStage::Waiting,
            16,
            NativeEnrollmentMethod::Transfer,
            NativeEnrollmentMethod::Transfer
        ));
        assert!(owner_phone_should_rotate(
            NativeOwnerPhoneStage::Waiting,
            15,
            NativeEnrollmentMethod::Transfer,
            NativeEnrollmentMethod::Transfer
        ));
        assert!(owner_phone_should_rotate(
            NativeOwnerPhoneStage::Expired,
            300,
            NativeEnrollmentMethod::Transfer,
            NativeEnrollmentMethod::Transfer
        ));
        assert!(owner_phone_should_rotate(
            NativeOwnerPhoneStage::Waiting,
            300,
            NativeEnrollmentMethod::Transfer,
            NativeEnrollmentMethod::SharedSecret
        ));
        assert!(!owner_phone_should_rotate(
            NativeOwnerPhoneStage::Verify,
            0,
            NativeEnrollmentMethod::Transfer,
            NativeEnrollmentMethod::SharedSecret
        ));
    }
}
