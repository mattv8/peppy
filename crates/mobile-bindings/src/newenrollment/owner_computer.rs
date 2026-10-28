use super::NativeEnrollmentClient;
use crate::{MobileBindingsError, NativeClient};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use peppy_hosted_api::computer_enrollment::{ComputerEnrollmentCreated, ComputerPairingQr};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub(super) struct OwnerComputerPrompt {
    origin: String,
    invitation_id: String,
    digest: String,
    created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NativeOwnerComputerCreateRequest {
    /// Canonical hosted API origin validated against the scanned QR.
    pub origin: String,
    pub invitation_id: String,
    /// Digest of the exact scanned QR payload. Return it with asynchronous responses.
    pub prompt_digest: String,
    /// The canonical hosted API create request body; native hosts add their owner session.
    pub request_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NativeOwnerComputerView {
    pub origin: String,
    pub invitation_id: String,
    pub prompt_digest: String,
    pub created: bool,
    /// Locally derived after the server nonce has been pinned.
    pub sas: Option<String>,
}

#[uniffi::export]
impl NativeEnrollmentClient {
    /// Supplies the enrolled owner UUID from protected native identity storage. The shared client
    /// rejects scans until this has been configured; it never uses its local enrollment UUID.
    pub fn configure_owner_computer_device(
        &self,
        device_id: String,
    ) -> Result<(), MobileBindingsError> {
        let device_id =
            uuid::Uuid::parse_str(&device_id).map_err(|_| MobileBindingsError::InvalidRequest)?;
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .configure_computer_owner_device(device_id)
            .map_err(super::enrollment_error)
    }

    /// Validates a computer QR locally and creates the body for the authenticated owner request.
    pub fn owner_computer_create_request(
        &self,
        qr_json: String,
    ) -> Result<NativeOwnerComputerCreateRequest, MobileBindingsError> {
        let qr: ComputerPairingQr =
            serde_json::from_str(&qr_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
        let prompt = OwnerComputerPrompt {
            origin: qr.origin,
            invitation_id: qr.invitation_id.to_string(),
            digest: prompt_digest(&qr_json),
            created: false,
        };
        let request_json = self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .scan_computer_qr(qr_json)
            .map_err(super::enrollment_error)?;
        let mut active = self
            .owner_computer
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?;
        if active.is_some() {
            return Err(MobileBindingsError::InvalidRequest);
        }
        *active = Some(prompt);
        let prompt = active.as_ref().expect("prompt was set");
        Ok(NativeOwnerComputerCreateRequest {
            origin: prompt.origin.clone(),
            invitation_id: prompt.invitation_id.clone(),
            prompt_digest: prompt.digest.clone(),
            request_json,
        })
    }

    /// Pins one create response only when it belongs to the currently displayed QR prompt.
    pub fn consume_owner_computer_created(
        &self,
        invitation_id: String,
        prompt_digest: String,
        created_json: String,
    ) -> Result<(), MobileBindingsError> {
        let created: ComputerEnrollmentCreated =
            serde_json::from_str(&created_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
        let mut prompt = self
            .owner_computer
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?;
        let active = prompt.as_mut().ok_or(MobileBindingsError::InvalidRequest)?;
        if active.created
            || active.invitation_id != invitation_id
            || active.digest != prompt_digest
            || created.expires_in_seconds == 0
        {
            return Err(MobileBindingsError::InvalidRequest);
        }
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .accept_computer_created(created_json)
            .map_err(super::enrollment_error)?;
        active.created = true;
        Ok(())
    }

    /// Returns local prompt identity and the locally derived comparison code, never server SAS.
    pub fn owner_computer_view(&self) -> Result<NativeOwnerComputerView, MobileBindingsError> {
        let prompt = self
            .owner_computer
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?;
        let active = prompt.as_ref().ok_or(MobileBindingsError::InvalidRequest)?;
        let sas = if active.created {
            Some(
                self.inner
                    .lock()
                    .map_err(|_| MobileBindingsError::Crypto)?
                    .computer_sas()
                    .map_err(super::enrollment_error)?,
            )
        } else {
            None
        };
        Ok(NativeOwnerComputerView {
            origin: active.origin.clone(),
            invitation_id: active.invitation_id.clone(),
            prompt_digest: active.digest.clone(),
            created: active.created,
            sas,
        })
    }

    /// Clears only local computer-enrollment state; it does not cancel the server enrollment.
    pub fn cancel_owner_computer_flow(&self) -> Result<(), MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .cancel_computer_flow()
            .map_err(super::enrollment_error)?;
        self.owner_computer
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .take();
        Ok(())
    }

    /// Verifies the separately prompted vault key phrase, requires an unlocked native core, and
    /// emits the authenticated approval only after an explicit comparison-code confirmation.
    pub fn approve_owner_computer_request(
        &self,
        invitation_id: String,
        prompt_digest: String,
        codes_match: bool,
        vault_key_phrase: String,
        native_client: Arc<NativeClient>,
    ) -> Result<String, MobileBindingsError> {
        let prompt = self
            .owner_computer
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?;
        let active = prompt.as_ref().ok_or(MobileBindingsError::InvalidRequest)?;
        if !active.created
            || active.invitation_id != invitation_id
            || active.digest != prompt_digest
            || !codes_match
        {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let mut client = self.inner.lock().map_err(|_| MobileBindingsError::Crypto)?;
        if !client
            .owner_passphrase_matches(&vault_key_phrase)
            .map_err(super::enrollment_error)?
        {
            return Err(MobileBindingsError::InvalidRequest);
        }
        crate::with_client(&native_client.client, |core| {
            client
                .approve_computer_enrollment_with_client(codes_match, core)
                .map_err(|_| peppy_client_core::Error::Crypto)
        })
    }
}

fn prompt_digest(qr_json: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(qr_json.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::newenrollment::{
        new_native_enrollment_client, new_native_existing_owner_enrollment_client,
        NativeEnrollmentClientKind,
    };
    use crate::{create_vault_material, open_native_client, NativeOpenConfig};
    use peppy_hosted_api::computer_enrollment::ComputerEnrollmentCreated;
    use uuid::Uuid;

    const ORIGIN: &str = "https://enroll.example";
    const PHRASE: &str = "alpha bravo charlie delta echo foxtrot";

    fn owner() -> (
        Arc<NativeEnrollmentClient>,
        crate::NativeVaultMaterial,
        Uuid,
    ) {
        let material = create_vault_material(Uuid::new_v4().to_string(), PHRASE.into()).unwrap();
        let owner = new_native_existing_owner_enrollment_client(
            ORIGIN.into(),
            NativeEnrollmentClientKind::Android,
            "owner".into(),
            material.profile_json.clone(),
            material.header_json.clone(),
        )
        .unwrap();
        (owner, material, Uuid::from_u128(77))
    }

    fn claimant_qr() -> String {
        new_native_enrollment_client(
            ORIGIN.into(),
            NativeEnrollmentClientKind::Desktop,
            "computer".into(),
        )
        .unwrap()
        .inner
        .lock()
        .unwrap()
        .begin_computer_claimant()
        .unwrap()
    }

    fn created() -> String {
        serde_json::to_string(&ComputerEnrollmentCreated {
            server_nonce: URL_SAFE_NO_PAD.encode([7; 32]),
            expires_in_seconds: 60,
        })
        .unwrap()
    }

    #[test]
    fn owner_computer_scan_rejects_malformed_cross_origin_and_non_owner() {
        let (owner, _, owner_device_id) = owner();
        assert!(owner.owner_computer_create_request(claimant_qr()).is_err());
        owner
            .configure_owner_computer_device(owner_device_id.to_string())
            .unwrap();
        assert!(owner
            .owner_computer_create_request("not json".into())
            .is_err());
        let foreign = new_native_enrollment_client(
            "https://other.example".into(),
            NativeEnrollmentClientKind::Desktop,
            "computer".into(),
        )
        .unwrap()
        .inner
        .lock()
        .unwrap()
        .begin_computer_claimant()
        .unwrap();
        assert!(owner.owner_computer_create_request(foreign).is_err());
        let claimant = new_native_enrollment_client(
            ORIGIN.into(),
            NativeEnrollmentClientKind::Android,
            "phone".into(),
        )
        .unwrap();
        assert!(claimant
            .owner_computer_create_request(claimant_qr())
            .is_err());
    }

    #[test]
    fn owner_computer_pins_prompt_before_sas_and_rejects_stale_created_response() {
        let (owner, _, owner_device_id) = owner();
        owner
            .configure_owner_computer_device(owner_device_id.to_string())
            .unwrap();
        let request = owner.owner_computer_create_request(claimant_qr()).unwrap();
        assert_eq!(owner.owner_computer_view().unwrap().sas, None);
        assert!(owner
            .consume_owner_computer_created(
                Uuid::new_v4().to_string(),
                request.prompt_digest.clone(),
                created(),
            )
            .is_err());
        owner
            .consume_owner_computer_created(
                request.invitation_id.clone(),
                request.prompt_digest.clone(),
                created(),
            )
            .unwrap();
        let view = owner.owner_computer_view().unwrap();
        assert!(view.sas.is_some());
        assert!(owner
            .consume_owner_computer_created(request.invitation_id, request.prompt_digest, created())
            .is_err());
    }

    #[test]
    fn owner_computer_uses_actual_owner_id_and_caches_approval_after_unlocked_proof() {
        let (owner, material, owner_device_id) = owner();
        owner
            .configure_owner_computer_device(owner_device_id.to_string())
            .unwrap();
        let request = owner.owner_computer_create_request(claimant_qr()).unwrap();
        owner
            .consume_owner_computer_created(
                request.invitation_id.clone(),
                request.prompt_digest.clone(),
                created(),
            )
            .unwrap();
        let vault_id: peppy_crypto::KeyProfile =
            serde_json::from_str(&material.profile_json).unwrap();
        let path = std::env::temp_dir().join(format!("peppy-owner-computer-{}.db", Uuid::new_v4()));
        let native = open_native_client(NativeOpenConfig {
            database_path: path.to_string_lossy().into_owned(),
            vault_id: vault_id.vault_id.to_string(),
            device_id: Uuid::new_v4().to_string(),
            database_key: vec![9; 32],
        })
        .unwrap();
        assert!(owner
            .approve_owner_computer_request(
                request.invitation_id.clone(),
                request.prompt_digest.clone(),
                false,
                PHRASE.into(),
                native.clone(),
            )
            .is_err());
        assert!(owner
            .approve_owner_computer_request(
                request.invitation_id.clone(),
                request.prompt_digest.clone(),
                true,
                "wrong phrase".into(),
                native.clone(),
            )
            .is_err());
        assert!(owner
            .approve_owner_computer_request(
                request.invitation_id.clone(),
                request.prompt_digest.clone(),
                true,
                PHRASE.into(),
                native.clone(),
            )
            .is_err());
        native
            .unlock(material.profile_json, material.header_json, PHRASE.into())
            .unwrap();
        let first_approval = owner
            .approve_owner_computer_request(
                request.invitation_id.clone(),
                request.prompt_digest.clone(),
                true,
                PHRASE.into(),
                native.clone(),
            )
            .unwrap();
        let approval: peppy_hosted_api::computer_enrollment::ApproveComputerEnrollmentRequest =
            serde_json::from_str(&first_approval).unwrap();
        assert_eq!(approval.transcript.owner_device_id, owner_device_id);
        assert!(!approval.owner_proof.is_empty());
        let retry = owner
            .approve_owner_computer_request(
                request.invitation_id,
                request.prompt_digest,
                true,
                PHRASE.into(),
                native,
            )
            .unwrap();
        assert_eq!(retry, first_approval);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cancel_owner_computer_flow_clears_the_prompt_and_allows_a_new_scan() {
        let (owner, _, owner_device_id) = owner();
        owner
            .configure_owner_computer_device(owner_device_id.to_string())
            .unwrap();
        owner.owner_computer_create_request(claimant_qr()).unwrap();
        owner.cancel_owner_computer_flow().unwrap();
        assert!(owner.owner_computer_view().is_err());
        assert!(owner.owner_computer_create_request(claimant_qr()).is_ok());
    }
}
