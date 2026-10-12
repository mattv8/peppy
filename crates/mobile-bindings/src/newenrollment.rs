//! UniFFI facade for the opaque, transport-free enrollment request owner.

use crate::{MobileBindingsError, NativeClient};
use peppy_hosted_api::enrollment::{ClientKind, EnrollmentMethod, EnrollmentRole};
use std::sync::{Arc, Mutex};

#[derive(uniffi::Object)]
pub struct NativeEnrollmentClient {
    inner: Mutex<peppy_hosted_client::newenrollment::EnrollmentClient>,
}

#[derive(uniffi::Object)]
pub struct NativeEnrollmentIdentity {
    inner: Mutex<Option<peppy_hosted_client::newenrollment::NativeEnrollmentIdentity>>,
    metadata: NativeEnrollmentIdentityMetadata,
}

#[derive(Clone, uniffi::Record)]
pub struct NativeEnrollmentIdentityMetadata {
    pub origin: String,
    pub vault_id: String,
    pub device_id: String,
    pub role: String,
    pub profile_json: String,
    pub header_json: String,
}

#[derive(Clone, uniffi::Record)]
pub struct NativeVaultInvite {
    pub origin: String,
    pub invite_token: String,
}

#[derive(Clone, uniffi::Record)]
pub struct NativePendingVaultRequest {
    pub origin: String,
    pub request_id: String,
    pub display_name: String,
}

fn enrollment_error(_: peppy_hosted_client::HostedClientError) -> MobileBindingsError {
    MobileBindingsError::InvalidRequest
}

#[uniffi::export]
pub fn new_native_enrollment_client(
    origin: String,
    kind: NativeEnrollmentClientKind,
    device_name: String,
) -> Result<Arc<NativeEnrollmentClient>, MobileBindingsError> {
    Ok(Arc::new(NativeEnrollmentClient {
        inner: Mutex::new(
            peppy_hosted_client::newenrollment::EnrollmentClient::new(
                origin,
                client_kind(kind),
                device_name,
            )
            .map_err(enrollment_error)?,
        ),
    }))
}

#[uniffi::export]
pub fn parse_vault_invite_link(link: String) -> Result<NativeVaultInvite, MobileBindingsError> {
    let invite =
        peppy_hosted_client::newenrollment::EnrollmentClient::parse_vault_invite_link(&link)
            .map_err(enrollment_error)?;
    Ok(NativeVaultInvite {
        origin: invite.origin,
        invite_token: invite.invite_token,
    })
}

#[uniffi::export]
pub fn new_native_existing_owner_enrollment_client(
    origin: String,
    kind: NativeEnrollmentClientKind,
    device_name: String,
    profile_json: String,
    header_json: String,
) -> Result<Arc<NativeEnrollmentClient>, MobileBindingsError> {
    let profile =
        serde_json::from_str(&profile_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
    let header =
        serde_json::from_str(&header_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
    Ok(Arc::new(NativeEnrollmentClient {
        inner: Mutex::new(
            peppy_hosted_client::newenrollment::EnrollmentClient::for_existing_owner(
                origin,
                client_kind(kind),
                device_name,
                profile,
                header,
            )
            .map_err(enrollment_error)?,
        ),
    }))
}

#[derive(Clone, Copy, uniffi::Enum)]
pub enum NativeEnrollmentClientKind {
    Browser,
    Desktop,
    Android,
    Ios,
}

#[derive(Clone, Copy, uniffi::Enum)]
pub enum NativeEnrollmentMethod {
    Transfer,
    SharedSecret,
}

fn client_kind(kind: NativeEnrollmentClientKind) -> ClientKind {
    match kind {
        NativeEnrollmentClientKind::Browser => ClientKind::Browser,
        NativeEnrollmentClientKind::Desktop => ClientKind::Desktop,
        NativeEnrollmentClientKind::Android => ClientKind::Android,
        NativeEnrollmentClientKind::Ios => ClientKind::Ios,
    }
}

fn enrollment_method(method: NativeEnrollmentMethod) -> EnrollmentMethod {
    match method {
        NativeEnrollmentMethod::Transfer => EnrollmentMethod::Transfer,
        NativeEnrollmentMethod::SharedSecret => EnrollmentMethod::SharedSecret,
    }
}

#[uniffi::export]
impl NativeEnrollmentClient {
    pub fn checkpoint(&self) -> Result<Vec<u8>, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .checkpoint()
            .map_err(enrollment_error)
    }

    pub fn create_owner_request(
        &self,
        setup_token: String,
        username: String,
        admin_password: String,
        passphrase: String,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .create_owner_request(setup_token, username, admin_password, passphrase)
            .map_err(enrollment_error)
    }

    pub fn finish_owner_admission(
        &self,
        response_json: String,
        passphrase: String,
    ) -> Result<Arc<NativeEnrollmentIdentity>, MobileBindingsError> {
        finish_identity(&self.inner, |client| {
            client.finish_owner_admission(response_json, passphrase)
        })
    }

    pub fn manual_challenge_request(
        &self,
        vault_id: Option<String>,
        vault_owner: Option<String>,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .manual_challenge_request(vault_id, vault_owner)
            .map_err(enrollment_error)
    }

    pub fn begin_vault_request(
        &self,
        origin: String,
        invite_token: String,
        display_name: String,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .begin_vault_request(origin, invite_token, display_name)
            .map_err(enrollment_error)
    }

    pub fn accept_vault_request_created(&self, json: String) -> Result<(), MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .accept_vault_request_created(json)
            .map_err(enrollment_error)
    }

    pub fn vault_request_header(&self) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .vault_request_header()
            .map_err(enrollment_error)
    }

    pub fn vault_request_accepted(&self) -> Result<bool, MobileBindingsError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .vault_request_accepted())
    }

    pub fn has_prepared_owner_material(&self) -> Result<bool, MobileBindingsError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .has_prepared_owner_material())
    }

    pub fn parse_vault_request_status(&self, json: String) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .parse_vault_request_status(json)
            .map_err(enrollment_error)
    }

    pub fn create_vault_request(
        &self,
        username: String,
        admin_password: String,
        passphrase: String,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .create_vault_request(username, admin_password, passphrase)
            .map_err(enrollment_error)
    }

    pub fn clear_vault_request(&self) -> Result<(), MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .clear_vault_request();
        Ok(())
    }

    pub fn pending_vault_request(
        &self,
    ) -> Result<Option<NativePendingVaultRequest>, MobileBindingsError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .pending_vault_request()
            .map(
                |(origin, request_id, display_name)| NativePendingVaultRequest {
                    origin,
                    request_id,
                    display_name,
                },
            ))
    }

    pub fn complete_manual_challenge_request(
        &self,
        challenge_json: String,
        passphrase: String,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .complete_manual_challenge_request(challenge_json, passphrase)
            .map_err(enrollment_error)
    }

    pub fn finish_manual_admission(
        &self,
        response_json: String,
        passphrase: String,
    ) -> Result<Arc<NativeEnrollmentIdentity>, MobileBindingsError> {
        finish_identity(&self.inner, |client| {
            client.finish_manual_admission(response_json, passphrase)
        })
    }

    pub fn create_phone_invitation_request(
        &self,
        method: NativeEnrollmentMethod,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .create_phone_invitation(enrollment_method(method))
            .map_err(enrollment_error)
    }

    pub fn consume_created_phone_invitation(
        &self,
        created_json: String,
    ) -> Result<(), MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .consume_created_invitation(created_json)
            .map_err(enrollment_error)
    }

    pub fn claim_phone_invitation_request(
        &self,
        qr_json: String,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .claim_phone_invitation(qr_json)
            .map_err(enrollment_error)
    }

    pub fn consume_phone_claim(&self, claim_json: String) -> Result<(), MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .consume_claim_response(claim_json)
            .map_err(enrollment_error)
    }

    /// Opaque claimant-authenticated polling descriptor for native transport only.
    pub fn phone_status_request(&self) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .phone_status_request()
            .map_err(enrollment_error)
    }

    /// Abandons a local claimant flow; it does not perform a server-side cancellation.
    pub fn cancel_phone_flow(&self) -> Result<(), MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .cancel_phone_flow()
            .map_err(enrollment_error)
    }

    pub fn phone_sas(&self, status_json: String) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .phone_sas(status_json)
            .map_err(enrollment_error)
    }

    pub fn approve_phone_invitation_request(
        &self,
        status_json: String,
        passphrase: String,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .approve_phone_invitation(status_json, passphrase)
            .map_err(enrollment_error)
    }

    pub fn confirm_phone_invitation_request(
        &self,
        status_json: String,
        shared_secret: Option<String>,
    ) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .confirm_phone_invitation(status_json, shared_secret)
            .map_err(enrollment_error)
    }

    pub fn finish_phone_admission(
        &self,
        complete_json: String,
    ) -> Result<Arc<NativeEnrollmentIdentity>, MobileBindingsError> {
        finish_identity(&self.inner, |client| {
            client.finish_phone_admission(complete_json)
        })
    }
}

fn finish_identity(
    client: &Mutex<peppy_hosted_client::newenrollment::EnrollmentClient>,
    finish: impl FnOnce(
        &mut peppy_hosted_client::newenrollment::EnrollmentClient,
    ) -> Result<
        peppy_hosted_client::newenrollment::NativeEnrollmentIdentity,
        peppy_hosted_client::HostedClientError,
    >,
) -> Result<Arc<NativeEnrollmentIdentity>, MobileBindingsError> {
    let mut client = client.lock().map_err(|_| MobileBindingsError::Crypto)?;
    native_identity(finish(&mut client).map_err(enrollment_error)?)
}

#[uniffi::export]
impl NativeEnrollmentIdentity {
    pub fn metadata(&self) -> NativeEnrollmentIdentityMetadata {
        self.metadata.clone()
    }

    /// Returns the bearer only to a generated native binding for immediate secure-store install.
    pub fn credential_for_native_secure_store(&self) -> Result<String, MobileBindingsError> {
        self.inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .as_ref()
            .ok_or(MobileBindingsError::InvalidRequest)
            .map(peppy_hosted_client::newenrollment::NativeEnrollmentIdentity::device_credential_for_native_secure_store)
    }

    /// Consumes the verified root package directly into the supplied native core.
    pub fn activate_into(
        &self,
        native_client: Arc<NativeClient>,
    ) -> Result<(), MobileBindingsError> {
        let identity = self
            .inner
            .lock()
            .map_err(|_| MobileBindingsError::Crypto)?
            .take()
            .ok_or(MobileBindingsError::InvalidRequest)?;
        native_client.activate_enrollment_identity(
            identity
                .into_activation()
                .ok_or(MobileBindingsError::InvalidRequest)?,
        )
    }
}

fn native_identity(
    identity: peppy_hosted_client::newenrollment::NativeEnrollmentIdentity,
) -> Result<Arc<NativeEnrollmentIdentity>, MobileBindingsError> {
    let metadata = identity.metadata();
    let role = match metadata.role {
        EnrollmentRole::Owner => "owner",
        EnrollmentRole::Device => "device",
        EnrollmentRole::Gateway => "gateway",
    };
    Ok(Arc::new(NativeEnrollmentIdentity {
        metadata: NativeEnrollmentIdentityMetadata {
            origin: identity.origin().to_owned(),
            vault_id: metadata.vault_id.to_string(),
            device_id: metadata.device_id.to_string(),
            role: role.into(),
            profile_json: serde_json::to_string(&metadata.profile)
                .map_err(|_| MobileBindingsError::InvalidRequest)?,
            header_json: serde_json::to_string(&metadata.header)
                .map_err(|_| MobileBindingsError::InvalidRequest)?,
        },
        inner: Mutex::new(Some(identity)),
    }))
}

#[uniffi::export]
pub fn restore_native_enrollment_client(
    checkpoint: Vec<u8>,
) -> Result<Arc<NativeEnrollmentClient>, MobileBindingsError> {
    Ok(Arc::new(NativeEnrollmentClient {
        inner: Mutex::new(
            peppy_hosted_client::newenrollment::EnrollmentClient::restore(checkpoint)
                .map_err(enrollment_error)?,
        ),
    }))
}
