//! Private worker routing for the shared enrollment state owner.
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_hosted_api::enrollment::EnrollmentMethod;
use peppy_hosted_client::newenrollment::ClientKind;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewClient {
    origin: String,
    device_name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExistingOwnerClient {
    device_name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnerRequest {
    #[serde(default = "empty_secret")]
    setup_token: SecretString,
    username: String,
    admin_password: SecretString,
    passphrase: SecretString,
}

fn empty_secret() -> SecretString {
    SecretString(Zeroizing::new(String::new()))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualChallenge {
    vault_id: Option<String>,
    vault_owner: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VaultInviteLink {
    link: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginVaultRequest {
    origin: String,
    invite_token: String,
    display_name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VaultRequestCreatedInput {
    json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VaultRequestStatusInput {
    json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVaultRequestInput {
    username: String,
    admin_password: SecretString,
    passphrase: SecretString,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManualComplete {
    challenge_json: String,
    passphrase: SecretString,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinishAdmission {
    response_json: String,
    passphrase: SecretString,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateInvitation {
    method: EnrollmentMethod,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreatedInvitation {
    created_json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimInvitation {
    qr_json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimResponse {
    claim_json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PhoneStatus {
    status_json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApproveInvitation {
    status_json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfirmInvitation {
    status_json: String,
    shared_secret: Option<SecretString>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ComputerStatus {
    status_json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinishComputer {
    complete_json: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostedOwnerPrepare {
    account_id: String,
    passphrase: SecretString,
    device_name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostedGrant {
    grant_json: SecretString,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostedOwnerFinish {
    response_json: String,
    passphrase: SecretString,
}

impl BrowserCore {
    pub(super) fn new_enrollment_command(
        &mut self,
        command: &str,
        raw: &RawValue,
    ) -> Result<Value, Failure> {
        match command {
            "_worker_enrollment_gateway_presence" => {
                empty_new_enrollment(raw)?;
                let has_gateway = self
                    .gateways_known
                    .then(|| !self.context.gateways.as_deref().unwrap_or(&[]).is_empty());
                Ok(match has_gateway {
                    Some(has_gateway) => json!({"hasGateway": has_gateway}),
                    None => json!({}),
                })
            }
            "_worker_enrollment_generate_secret" => {
                empty_new_enrollment(raw)?;
                // This private worker response is consumed only by the bridge-owned save dialog.
                Ok(json!({"passphrase": peppy_crypto::passphrase::generate_passphrase()}))
            }
            "_worker_enrollment_new" => {
                let input: NewClient = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let origin = canonical_origin(&input.origin)?;
                if self.identity_session.is_some()
                    || self.client.is_some()
                    || self.prepared_enrollment_identity.is_some()
                    || self
                        .trusted_origin
                        .as_deref()
                        .is_some_and(|trusted| trusted != origin)
                {
                    return Err(invalid());
                }
                self.trusted_origin = Some(origin.clone());
                self.enrollment = Some(
                    peppy_hosted_client::newenrollment::EnrollmentClient::new(
                        origin,
                        ClientKind::Browser,
                        input.device_name,
                    )
                    .map_err(|_| invalid())?,
                );
                Ok(json!({}))
            }
            "_worker_enrollment_new_owner_from_identity" => {
                let input: ExistingOwnerClient =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let session = self.identity_session.as_ref().ok_or_else(unavailable)?;
                let (origin, profile, header) =
                    session.owner_enrollment_material(self.client()?)?;
                self.enrollment = Some(
                    peppy_hosted_client::newenrollment::EnrollmentClient::for_existing_owner(
                        origin,
                        ClientKind::Browser,
                        input.device_name,
                        profile,
                        header,
                    )
                    .map_err(|_| invalid())?,
                );
                Ok(json!({}))
            }
            "_worker_enrollment_prepare_hosted_owner" => {
                let mut input: HostedOwnerPrepare =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let origin = self.trusted_origin.as_deref().ok_or_else(invalid)?;
                if self.client.is_some()
                    || self.identity_session.is_some()
                    || self.root.join("client.db").exists()
                    || self.enrollment.is_some()
                {
                    return Err(Failure::new(
                        "identity-exists",
                        "A local identity already exists. Nothing was reset.",
                    ));
                }
                if let Some(provisioning) = self.hosted_provisioning.as_ref() {
                    let view = provisioning.view();
                    if view.origin != origin || view.account_id != input.account_id {
                        return Err(invalid());
                    }
                    return Ok(
                        json!({"request": provisioning.grant_request().map_err(|_| invalid())?}),
                    );
                }
                let provisioning = peppy_hosted_client::prepare(
                    origin,
                    &input.account_id,
                    std::mem::take(&mut *input.passphrase.0),
                    ClientKind::Browser,
                    input.device_name,
                )
                .map_err(|_| invalid())?;
                let request = provisioning.grant_request().map_err(|_| invalid())?;
                self.hosted_provisioning = Some(provisioning);
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_accept_hosted_grant" => {
                let mut input: HostedGrant =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let provisioning = self.hosted_provisioning.as_ref().ok_or_else(unavailable)?;
                provisioning
                    .accept_grant(std::mem::take(&mut *input.grant_json.0))
                    .map_err(|_| invalid())?;
                Ok(json!({"request": provisioning.complete_request().map_err(|_| invalid())?}))
            }
            "_worker_enrollment_finish_hosted_owner" => {
                let input: HostedOwnerFinish =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let provisioning = self.hosted_provisioning.as_ref().ok_or_else(unavailable)?;
                let view = provisioning.view();
                let credential_json = Zeroizing::new(
                    provisioning
                        .credential_json(input.response_json)
                        .map_err(|_| invalid())?,
                );
                if !provisioning
                    .passphrase_matches(&input.passphrase.0)
                    .map_err(|_| invalid())?
                {
                    return Err(invalid());
                }
                let credential =
                    peppy_hosted_client::device_credentials::parse_portable_credential(
                        credential_json.as_bytes(),
                    )
                    .map_err(|_| invalid())?;
                if credential.origin() != view.origin
                    || credential.vault_id() != view.vault_id
                    || credential.device_id() != view.device_id
                {
                    return Err(invalid());
                }
                let device_id = Uuid::parse_str(credential.device_id()).map_err(|_| invalid())?;
                let device_token = Zeroizing::new(credential.device_token().to_owned());
                let result = self.install_hosted_owner_identity(
                    provisioning.profile(),
                    provisioning.header(),
                    device_id,
                    device_token,
                    input.passphrase,
                );
                if result.is_ok() {
                    self.hosted_provisioning = None;
                }
                result
            }
            "_worker_enrollment_checkpoint" => {
                empty_new_enrollment(raw)?;
                Ok(
                    json!({"checkpoint": URL_SAFE_NO_PAD.encode(self.enrollment.as_ref().ok_or_else(unavailable)?.checkpoint().map_err(|_| invalid())?)}),
                )
            }
            "_worker_enrollment_create_owner" => {
                let mut input: OwnerRequest =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .create_owner_request(
                        std::mem::take(&mut *input.setup_token.0),
                        input.username,
                        std::mem::take(&mut *input.admin_password.0),
                        std::mem::take(&mut *input.passphrase.0),
                    )
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_manual_challenge" => {
                let input: ManualChallenge =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                Ok(
                    json!({"request": self.enrollment.as_ref().ok_or_else(unavailable)?.manual_challenge_request(input.vault_id, input.vault_owner).map_err(|_| invalid())?}),
                )
            }
            "_worker_enrollment_parse_vault_invite_link" => {
                let input: VaultInviteLink =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let invite =
                    peppy_hosted_client::newenrollment::EnrollmentClient::parse_vault_invite_link(
                        &input.link,
                    )
                    .map_err(|_| invalid())?;
                Ok(json!({"origin": invite.origin, "inviteToken": invite.invite_token}))
            }
            "_worker_enrollment_begin_vault_request" => {
                let input: BeginVaultRequest =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .begin_vault_request(input.origin, input.invite_token, input.display_name)
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_accept_vault_request_created" => {
                let input: VaultRequestCreatedInput =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                self.enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .accept_vault_request_created(input.json)
                    .map_err(|_| invalid())?;
                Ok(json!({}))
            }
            "_worker_enrollment_vault_request_header" => {
                empty_new_enrollment(raw)?;
                Ok(
                    json!({"header": self.enrollment.as_ref().ok_or_else(unavailable)?.vault_request_header().map_err(|_| invalid())?}),
                )
            }
            "_worker_enrollment_parse_vault_request_status" => {
                let input: VaultRequestStatusInput =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let state = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .parse_vault_request_status(input.json)
                    .map_err(|_| invalid())?;
                Ok(json!({"state": state}))
            }
            "_worker_enrollment_create_vault" => {
                let mut input: CreateVaultRequestInput =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .create_vault_request(
                        input.username,
                        std::mem::take(&mut *input.admin_password.0),
                        std::mem::take(&mut *input.passphrase.0),
                    )
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_clear_vault_request" => {
                empty_new_enrollment(raw)?;
                self.enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .clear_vault_request();
                Ok(json!({}))
            }
            "_worker_enrollment_pending_vault_request" => {
                empty_new_enrollment(raw)?;
                Ok(
                    match self
                        .enrollment
                        .as_ref()
                        .ok_or_else(unavailable)?
                        .pending_vault_request()
                    {
                        Some((origin, request_id, display_name)) => {
                            json!({"origin": origin, "requestId": request_id, "displayName": display_name})
                        }
                        None => Value::Null,
                    },
                )
            }
            "_worker_enrollment_complete_manual_challenge" => {
                let mut input: ManualComplete =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                Ok(
                    json!({"request": self.enrollment.as_mut().ok_or_else(unavailable)?.complete_manual_challenge_request(input.challenge_json, std::mem::take(&mut *input.passphrase.0)).map_err(|_| invalid())?}),
                )
            }
            "_worker_enrollment_finish_owner" => {
                let mut input: FinishAdmission =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let passphrase = std::mem::take(&mut *input.passphrase.0);
                let identity = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .finish_owner_admission(input.response_json, passphrase.clone())
                    .map_err(|_| invalid())?;
                let result = self.install_enrollment_identity(identity, passphrase);
                if result.is_ok() {
                    self.enrollment = None;
                }
                result
            }
            "_worker_enrollment_finish_manual" => {
                let mut input: FinishAdmission =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let passphrase = std::mem::take(&mut *input.passphrase.0);
                let identity = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .finish_manual_admission(input.response_json, passphrase.clone())
                    .map_err(|_| invalid())?;
                let result = self.install_enrollment_identity(identity, passphrase);
                if result.is_ok() {
                    self.enrollment = None;
                }
                result
            }
            "_worker_enrollment_create_phone_invitation" => {
                let input: CreateInvitation =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .create_phone_invitation(input.method)
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_consume_created_phone_invitation" => {
                let input: CreatedInvitation =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                self.enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .consume_created_invitation(input.created_json)
                    .map_err(|_| invalid())?;
                Ok(json!({}))
            }
            "_worker_enrollment_get_phone_qr" => {
                empty_new_enrollment(raw)?;
                // The QR carries only the locally generated invitation secret, never a server copy.
                let qr = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .get_qr_payload()
                    .map_err(|_| invalid())?;
                Ok(json!({"qr": qr}))
            }
            "_worker_enrollment_claim_phone_invitation" => {
                let input: ClaimInvitation =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .claim_phone_invitation(input.qr_json)
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_consume_phone_claim" => {
                let input: ClaimResponse =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                self.enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .consume_claim_response(input.claim_json)
                    .map_err(|_| invalid())?;
                Ok(json!({}))
            }
            "_worker_enrollment_phone_status" => {
                let input: PhoneStatus = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let view = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .owner_phone_view(input.status_json)
                    .map_err(|_| invalid())?;
                Ok(json!({
                    "invitationId": view.invitation_id.to_string(),
                    "sas": view.sas,
                    "claimDigest": view.claim_digest,
                }))
            }
            "_worker_enrollment_approve_phone_invitation" => {
                let input: ApproveInvitation =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .approve_phone_invitation_with_client(input.status_json, self.client()?)
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_confirm_phone_invitation" => {
                let mut input: ConfirmInvitation =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .confirm_phone_invitation(
                        input.status_json,
                        input
                            .shared_secret
                            .as_mut()
                            .map(|secret| std::mem::take(&mut *secret.0)),
                    )
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_begin_computer_claimant" => {
                empty_new_enrollment(raw)?;
                let qr = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .begin_computer_claimant()
                    .map_err(|_| invalid())?;
                Ok(json!({"qr": qr}))
            }
            "_worker_enrollment_computer_status_request" => {
                empty_new_enrollment(raw)?;
                let request = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .computer_status_request()
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_computer_claimant_view" => {
                let input: ComputerStatus =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let view = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .computer_claimant_view(input.status_json)
                    .map_err(|_| invalid())?;
                Ok(json!({
                    "invitationId": view.invitation_id,
                    "awaitingOwner": view.awaiting_owner,
                    "state": view.state,
                    "expiresInSeconds": view.expires_in_seconds,
                    "sas": view.sas,
                }))
            }
            "_worker_enrollment_computer_awaiting_owner_view" => {
                empty_new_enrollment(raw)?;
                let view = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .computer_awaiting_owner_view()
                    .map_err(|_| invalid())?;
                Ok(json!({
                    "invitationId": view.invitation_id,
                    "awaitingOwner": view.awaiting_owner,
                    "state": view.state,
                    "expiresInSeconds": view.expires_in_seconds,
                }))
            }
            "_worker_enrollment_confirm_computer" => {
                let input: ComputerStatus =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_mut()
                    .ok_or_else(unavailable)?
                    .confirm_computer_enrollment(input.status_json)
                    .map_err(|_| invalid())?;
                Ok(json!({"request": request}))
            }
            "_worker_enrollment_finish_computer" => {
                let input: FinishComputer =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                if self.prepared_enrollment_identity.is_none() {
                    let identity = self
                        .enrollment
                        .as_mut()
                        .ok_or_else(unavailable)?
                        .finish_computer_admission(input.complete_json)
                        .map_err(|_| invalid())?;
                    self.prepared_enrollment_identity = Some(identity);
                }
                let mut identity = self
                    .prepared_enrollment_identity
                    .take()
                    .ok_or_else(unavailable)?;
                let result = self.install_prepared_verified_enrollment_identity(&mut identity);
                if result.is_ok() {
                    // The worker must now commit its encrypted checkpoint. A checkpoint failure
                    // poisons that worker; it must not re-enter this consumed in-memory session.
                    self.enrollment = None;
                } else {
                    self.prepared_enrollment_identity = Some(identity);
                }
                result
            }
            "_worker_enrollment_cancel_computer_claimant" => {
                empty_new_enrollment(raw)?;
                if self.client.is_some() || self.identity_session.is_some() {
                    return Err(invalid());
                }
                if self.prepared_enrollment_identity.take().is_none() {
                    self.enrollment
                        .as_mut()
                        .ok_or_else(unavailable)?
                        .cancel_computer_flow()
                        .map_err(|_| invalid())?;
                }
                Ok(json!({}))
            }
            _ => Err(unknown_command()),
        }
    }
}

fn empty_new_enrollment(raw: &RawValue) -> Result<(), Failure> {
    match serde_json::from_str::<serde_json::Map<String, Value>>(raw.get()) {
        Ok(value) if value.is_empty() => Ok(()),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use peppy_hosted_api::computer_enrollment::{
        ApproveComputerEnrollmentRequest, ComputerEnrollmentCreated, ComputerEnrollmentStatus,
    };
    use peppy_hosted_api::enrollment::{
        AdmissionComplete, ClaimInvitationRequest, EnrollmentRole, EnrollmentState,
        EnrollmentTranscript, InvitationClaim, InvitationPublicClaim, InvitationStatus,
        PhonePairingQr, VaultMaterial,
    };

    const ORIGIN: &str = "https://peppy.test/";

    fn dispatch(core: &mut BrowserCore, command: &str, args: &str) -> Value {
        serde_json::from_str(&core.dispatch(&format!(r#"{{"command":"{command}","args":{args}}}"#)))
            .unwrap()
    }

    fn value(response: &Value) -> Value {
        assert_eq!(response["ok"], true, "{response}");
        response["value"].clone()
    }

    fn owner_core() -> BrowserCore {
        let mut core = BrowserCore::new(std::path::PathBuf::new());
        let vault = VaultId::new();
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("correct horse battery staple", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        core.enrollment = Some(
            peppy_hosted_client::newenrollment::EnrollmentClient::for_existing_owner(
                ORIGIN.into(),
                ClientKind::Browser,
                "Owner browser".into(),
                profile,
                header,
            )
            .unwrap(),
        );
        core
    }

    fn browser_computer_completion_fixture() -> (BrowserCore, String, PathBuf) {
        let vault_id = Uuid::new_v4();
        let owner_device_id = Uuid::new_v4();
        let profile = KeyProfile::new(vault_id, 3).unwrap();
        let root_key = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root_key, profile.clone()).unwrap();

        let mut claimant = peppy_hosted_client::newenrollment::EnrollmentClient::new(
            ORIGIN.into(),
            ClientKind::Browser,
            "Browser computer".into(),
        )
        .unwrap();
        let qr = claimant.begin_computer_claimant().unwrap();
        let mut owner = peppy_hosted_client::newenrollment::EnrollmentClient::for_existing_owner(
            ORIGIN.into(),
            ClientKind::Desktop,
            "Owner computer".into(),
            profile.clone(),
            header.clone(),
        )
        .unwrap();
        owner
            .configure_computer_owner_device(owner_device_id)
            .unwrap();
        owner.scan_computer_qr(qr).unwrap();
        owner
            .accept_computer_created(
                serde_json::to_string(&ComputerEnrollmentCreated {
                    server_nonce: URL_SAFE_NO_PAD.encode([7; 32]),
                    expires_in_seconds: 60,
                })
                .unwrap(),
            )
            .unwrap();

        let owner_directory =
            std::env::temp_dir().join(format!("peppy-browser-owner-computer-{}", Uuid::new_v4()));
        std::fs::create_dir(&owner_directory).unwrap();
        let owner_client = Client::open(
            ClientConfig {
                database_path: owner_directory.join("owner.db"),
                vault_id: VaultId(vault_id),
                device_id: DeviceId(owner_device_id),
            },
            DatabaseKey::new(&[31; 32]).unwrap(),
        )
        .unwrap();
        owner_client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        let approval: ApproveComputerEnrollmentRequest = serde_json::from_str(
            &owner
                .approve_computer_enrollment_with_client(true, &owner_client)
                .unwrap(),
        )
        .unwrap();
        let status = ComputerEnrollmentStatus {
            state: EnrollmentState::Approved,
            expires_in_seconds: 60,
            transcript: Some(approval.transcript),
            encrypted_key_package: Some(approval.encrypted_key_package),
            transfer_nonce: Some(approval.transfer_nonce),
            owner_proof: Some(approval.owner_proof),
        };
        let status_json = serde_json::to_string(&status).unwrap();
        claimant.confirm_computer_enrollment(status_json).unwrap();
        let metadata = claimant.computer_awaiting_owner_view().err();
        assert!(metadata.is_some(), "confirmed flow is pending admission");

        let claimant_device_id = status
            .transcript
            .as_ref()
            .unwrap()
            .recipient
            .device
            .device_id;
        let complete = AdmissionComplete {
            vault_id,
            device_id: claimant_device_id,
            role: EnrollmentRole::Device,
            material: VaultMaterial {
                profile: serde_json::to_value(&profile).unwrap(),
                check_header: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
                fingerprint: profile.fingerprint().unwrap(),
                key_epoch: profile.key_epoch,
                admission_public_key: URL_SAFE_NO_PAD.encode([0; 32]),
            },
        };

        let browser_root =
            std::env::temp_dir().join(format!("peppy-browser-computer-retry-{}", Uuid::new_v4()));
        std::fs::create_dir(&browser_root).unwrap();
        let mut core = BrowserCore::new(browser_root.clone());
        value(&dispatch(
            &mut core,
            "_worker_initialize",
            &json!({"origin": ORIGIN}).to_string(),
        ));
        core.enrollment = Some(claimant);
        (
            core,
            serde_json::to_string(&complete).unwrap(),
            browser_root,
        )
    }

    #[test]
    fn computer_finish_retries_same_completion_after_real_open_failure() {
        let (mut core, complete_json, root) = browser_computer_completion_fixture();
        let existing_directory = root.join("client.db");
        std::fs::create_dir(&existing_directory).unwrap();
        let marker = existing_directory.join("user-data");
        std::fs::write(&marker, b"preserve me").unwrap();
        let args = json!({"completeJson": complete_json}).to_string();

        let first = dispatch(&mut core, "_worker_enrollment_finish_computer", &args);
        assert_eq!(first["ok"], false);
        assert_eq!(first["error"]["code"], "identity-exists");
        assert!(core.prepared_enrollment_identity.is_some());
        assert_eq!(std::fs::read(&marker).unwrap(), b"preserve me");

        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(existing_directory).unwrap();
        let second = dispatch(&mut core, "_worker_enrollment_finish_computer", &args);
        assert_eq!(second["ok"], true, "{second}");
        assert!(core.prepared_enrollment_identity.is_none());
        assert!(core.identity_session.is_some());
        assert_eq!(
            dispatch(&mut core, "_worker_checkpoint_identity", "{}")["ok"],
            true
        );
    }

    #[cfg(unix)]
    #[test]
    fn computer_finish_never_removes_existing_database_symlink_or_target() {
        use std::os::unix::fs::symlink;

        let (mut core, complete_json, root) = browser_computer_completion_fixture();
        let external = root.join("external-user.db");
        std::fs::write(&external, b"existing user database").unwrap();
        let database_link = root.join("client.db");
        symlink(&external, &database_link).unwrap();
        let args = json!({"completeJson": complete_json}).to_string();

        let first = dispatch(&mut core, "_worker_enrollment_finish_computer", &args);
        assert_eq!(first["ok"], false);
        assert!(
            std::fs::symlink_metadata(&database_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&external).unwrap(), b"existing user database");

        std::fs::remove_file(database_link).unwrap();
        let second = dispatch(&mut core, "_worker_enrollment_finish_computer", &args);
        assert_eq!(second["ok"], true, "{second}");
        assert_eq!(std::fs::read(&external).unwrap(), b"existing user database");
    }

    #[test]
    fn computer_finish_cleans_only_database_files_created_by_partial_open() {
        let (mut core, complete_json, root) = browser_computer_completion_fixture();
        let media_blocker = root.join("client.db.media");
        std::fs::write(&media_blocker, b"external blocker").unwrap();
        let args = json!({"completeJson": complete_json}).to_string();

        let first = dispatch(&mut core, "_worker_enrollment_finish_computer", &args);
        assert_eq!(first["ok"], false);
        assert!(core.prepared_enrollment_identity.is_some());
        assert!(
            !root.join("client.db").exists(),
            "a database created by the failed open must be removed for retry"
        );
        assert_eq!(std::fs::read(&media_blocker).unwrap(), b"external blocker");

        std::fs::remove_file(media_blocker).unwrap();
        let second = dispatch(&mut core, "_worker_enrollment_finish_computer", &args);
        assert_eq!(second["ok"], true, "{second}");
    }

    #[test]
    fn computer_finish_accepts_no_remote_passphrase() {
        assert!(serde_json::from_str::<FinishComputer>(r#"{"completeJson":"{}"}"#).is_ok());
        assert!(
            serde_json::from_str::<FinishComputer>(
                r#"{"completeJson":"{}","passphrase":"remote secret"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn phone_qr_requires_an_active_owner_invitation_and_empty_args() {
        let mut core = BrowserCore::new(std::path::PathBuf::new());
        let response = dispatch(&mut core, "_worker_enrollment_get_phone_qr", "{}");
        assert_eq!(response["ok"], false);

        let mut core = owner_core();
        let response = dispatch(&mut core, "_worker_enrollment_get_phone_qr", "{}");
        assert_eq!(response["ok"], false);

        value(&dispatch(
            &mut core,
            "_worker_enrollment_create_phone_invitation",
            r#"{"method":"transfer"}"#,
        ));
        let created = json!({
            "invitation_id": "00000000-0000-4000-8000-000000000001",
            "expires_in_seconds": 300
        })
        .to_string();
        value(&dispatch(
            &mut core,
            "_worker_enrollment_consume_created_phone_invitation",
            &json!({"createdJson": created}).to_string(),
        ));
        let rejected = dispatch(
            &mut core,
            "_worker_enrollment_get_phone_qr",
            r#"{"unexpected":true}"#,
        );
        assert_eq!(rejected["ok"], false);
    }

    #[test]
    fn phone_qr_command_is_private_worker_only() {
        let mut core = owner_core();
        let response: Value =
            serde_json::from_str(&core.dispatch(r#"{"command":"get_phone_qr","args":{}}"#))
                .unwrap();
        assert_eq!(response["error"]["code"], "unknown-command");
    }

    #[test]
    fn phone_approval_accepts_only_status_json_on_the_private_worker_boundary() {
        let mut core = owner_core();
        let status = json!({"state": "claimed"}).to_string();

        let status_only = dispatch(
            &mut core,
            "_worker_enrollment_approve_phone_invitation",
            &json!({"statusJson": status}).to_string(),
        );
        assert_eq!(status_only["ok"], false);

        let with_passphrase = dispatch(
            &mut core,
            "_worker_enrollment_approve_phone_invitation",
            &json!({"statusJson": "{}", "passphrase": "must-not-cross-worker-rpc"}).to_string(),
        );
        assert_eq!(with_passphrase["error"]["code"], "invalid-request");

        let public: Value = serde_json::from_str(&core.dispatch(
            r#"{"command":"enrollment_approve_phone_invitation","args":{"statusJson":"{}"}}"#,
        ))
        .unwrap();
        assert_eq!(public["error"]["code"], "unknown-command");
    }

    #[test]
    fn status_only_phone_approval_seals_a_handoff_the_recipient_can_confirm() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        let origin = canonical_origin(ORIGIN).unwrap();
        let vault = VaultId::new();
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("correct horse battery staple", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        let metadata = crate::local_identity::IdentityMetadata {
            version: 1,
            origin: origin.clone(),
            vault_id: vault.0,
            device_id: Uuid::new_v4(),
            role: crate::local_identity::DeviceRole::Owner,
            profile: profile.clone(),
            header,
        };
        value(&dispatch(
            &mut core,
            "_worker_initialize",
            &json!({"origin": metadata.origin}).to_string(),
        ));
        value(&dispatch(
            &mut core,
            "_worker_enroll_identity",
            &json!({
                "metadata": metadata,
                "deviceToken": "a".repeat(96),
                "passphrase": "correct horse battery staple",
            })
            .to_string(),
        ));
        value(&dispatch(
            &mut core,
            "_worker_enrollment_new_owner_from_identity",
            r#"{"deviceName":"Owner browser"}"#,
        ));

        value(&dispatch(
            &mut core,
            "_worker_enrollment_create_phone_invitation",
            r#"{"method":"transfer"}"#,
        ));
        value(&dispatch(
            &mut core,
            "_worker_enrollment_consume_created_phone_invitation",
            &json!({"createdJson": json!({
                "invitation_id": "00000000-0000-0000-0000-000000000007",
                "expires_in_seconds": 300,
            }).to_string()})
            .to_string(),
        ));
        let qr: PhonePairingQr = serde_json::from_str(
            value(&dispatch(
                &mut core,
                "_worker_enrollment_get_phone_qr",
                "{}",
            ))["qr"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let mut recipient = peppy_hosted_client::newenrollment::EnrollmentClient::new(
            origin.clone(),
            ClientKind::Android,
            "Recipient phone".into(),
        )
        .unwrap();
        let claim: ClaimInvitationRequest = serde_json::from_str(
            &recipient
                .claim_phone_invitation(serde_json::to_string(&qr).unwrap())
                .unwrap(),
        )
        .unwrap();
        recipient
            .consume_claim_response(
                serde_json::to_string(&InvitationClaim {
                    invitation_id: Uuid::from_u128(7),
                    claim_secret: URL_SAFE_NO_PAD.encode([4; 32]),
                    origin: origin.clone(),
                    vault_id: profile.vault_id,
                    fingerprint: profile.fingerprint().unwrap(),
                    key_epoch: profile.key_epoch,
                    method: EnrollmentMethod::Transfer,
                    owner_transfer_public_key: qr.transfer_public_key.clone(),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let claimed = InvitationStatus {
            state: EnrollmentState::Claimed,
            expires_in_seconds: 300,
            claim: Some(InvitationPublicClaim {
                device: claim.device.clone(),
                transfer_public_key: claim.transfer_public_key.clone(),
                credential_digest: claim.credential_digest.clone(),
                claim_authenticator: claim.claim_authenticator,
            }),
            transcript: Some(EnrollmentTranscript {
                origin,
                vault_id: profile.vault_id,
                fingerprint: profile.fingerprint().unwrap(),
                key_epoch: profile.key_epoch,
                enrollment_id: Uuid::from_u128(7),
                nonce: claim.invitation_token_digest,
                owner_transfer_public_key: qr.transfer_public_key,
                recipient_transfer_public_key: claim.transfer_public_key,
                signing_public_key: claim.device.signing_public_key,
                device_id: claim.device.device_id,
                credential_digest: claim.credential_digest,
                role: EnrollmentRole::Gateway,
            }),
            encrypted_key_package: None,
            transfer_nonce: None,
            profile: None,
        };
        let owner_view = value(&dispatch(
            &mut core,
            "_worker_enrollment_phone_status",
            &json!({"statusJson": serde_json::to_string(&claimed).unwrap()}).to_string(),
        ));
        assert_eq!(
            owner_view["invitationId"],
            "00000000-0000-0000-0000-000000000007"
        );
        assert!(owner_view["sas"].as_str().is_some());
        assert!(owner_view["claimDigest"].as_str().is_some());
        let approval = value(&dispatch(
            &mut core,
            "_worker_enrollment_approve_phone_invitation",
            &json!({"statusJson": serde_json::to_string(&claimed).unwrap()}).to_string(),
        ));
        let request = serde_json::from_str::<Value>(approval["request"].as_str().unwrap()).unwrap();
        let approved = InvitationStatus {
            state: EnrollmentState::Approved,
            encrypted_key_package: request["encrypted_key_package"].as_str().map(str::to_owned),
            transfer_nonce: request["transfer_nonce"].as_str().map(str::to_owned),
            profile: Some(serde_json::to_value(&profile).unwrap()),
            ..claimed
        };

        assert!(
            recipient
                .confirm_phone_invitation(serde_json::to_string(&approved).unwrap(), None)
                .unwrap()
                .contains("admission_signature")
        );
    }

    #[test]
    fn vault_invite_parser_is_allowlisted_only_for_the_private_worker() {
        let mut core = BrowserCore::new(std::path::PathBuf::new());
        let token = URL_SAFE_NO_PAD.encode([8; 32]);
        let value = value(&dispatch(
            &mut core,
            "_worker_enrollment_parse_vault_invite_link",
            &json!({"link": format!("https://enroll.example/#invite={token}")}).to_string(),
        ));
        assert_eq!(value["origin"], "https://enroll.example");
        assert_eq!(value["inviteToken"], token);
        let public: Value = serde_json::from_str(&core.dispatch(&format!(
            r#"{{"command":"enrollment_parse_vault_invite_link","args":{{"link":"https://enroll.example/#invite={token}"}}}}"#
        )))
        .unwrap();
        assert_eq!(public["error"]["code"], "unknown-command");
    }

    #[test]
    fn created_invitation_exposes_only_the_five_field_local_qr() {
        let mut core = owner_core();
        value(&dispatch(
            &mut core,
            "_worker_enrollment_create_phone_invitation",
            r#"{"method":"transfer"}"#,
        ));
        let created = json!({
            "invitation_id": "00000000-0000-4000-8000-000000000001",
            "expires_in_seconds": 300
        })
        .to_string();
        value(&dispatch(
            &mut core,
            "_worker_enrollment_consume_created_phone_invitation",
            &json!({"createdJson": created}).to_string(),
        ));

        let response = value(&dispatch(
            &mut core,
            "_worker_enrollment_get_phone_qr",
            "{}",
        ));
        let fields: Vec<&str> = response
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(fields, ["qr"]);

        let qr_text = response["qr"].as_str().unwrap();
        assert!(!qr_text.contains("credential"));
        let qr: Value = serde_json::from_str(qr_text).unwrap();
        let mut keys: Vec<&str> = qr.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "invitation_token",
                "origin",
                "purpose",
                "transfer_public_key",
                "version"
            ]
        );
        assert_eq!(qr["version"], 1);
        assert_eq!(qr["purpose"], "connect_phone");
        assert_eq!(qr["origin"], canonical_origin(ORIGIN).unwrap());
        for field in ["invitation_token", "transfer_public_key"] {
            let bytes = URL_SAFE_NO_PAD.decode(qr[field].as_str().unwrap()).unwrap();
            assert_eq!(bytes.len(), 32, "{field}");
        }
    }

    #[test]
    fn hosted_owner_install_supports_fresh_ephemeral_invitation_clients() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        let account = Uuid::new_v4().to_string();
        value(&dispatch(
            &mut core,
            "_worker_initialize",
            &json!({"origin": ORIGIN}).to_string(),
        ));

        let prepared = value(&dispatch(
            &mut core,
            "_worker_enrollment_prepare_hosted_owner",
            &json!({
                "accountId": account,
                "passphrase": "alpha bravo charlie delta echo foxtrot",
                "deviceName": "Browser owner"
            })
            .to_string(),
        ));
        let repeated = value(&dispatch(
            &mut core,
            "_worker_enrollment_prepare_hosted_owner",
            &json!({
                "accountId": account,
                "passphrase": "wrong phrase that must not replace the operation",
                "deviceName": "Other name"
            })
            .to_string(),
        ));
        assert_eq!(prepared, repeated);
        assert_eq!(
            dispatch(
                &mut core,
                "_worker_enrollment_prepare_hosted_owner",
                &json!({
                    "accountId": Uuid::new_v4(),
                    "passphrase": "alpha bravo charlie delta echo foxtrot",
                    "deviceName": "Browser owner"
                })
                .to_string(),
            )["ok"],
            false
        );

        let view = core.hosted_provisioning.as_ref().unwrap().view();
        value(&dispatch(
            &mut core,
            "_worker_enrollment_accept_hosted_grant",
            &json!({"grantJson": json!({
                "grant": format!("pgr_{}", "a".repeat(64)),
                "expires_in_seconds": 60
            }).to_string()})
            .to_string(),
        ));
        let complete = value(&dispatch(
            &mut core,
            "_worker_enrollment_accept_hosted_grant",
            &json!({"grantJson": json!({
                "grant": format!("pgr_{}", "a".repeat(64)),
                "expires_in_seconds": 60
            }).to_string()})
            .to_string(),
        ));
        assert!(complete["request"].is_string());

        let response = json!({
            "operation_id": view.operation_id,
            "vault_id": view.vault_id,
            "device_id": view.device_id,
            "already_provisioned": false,
        });
        let wrong_vault = json!({
            "operation_id": view.operation_id,
            "vault_id": Uuid::new_v4(),
            "device_id": view.device_id,
            "already_provisioned": false,
        });
        for (response, passphrase) in [
            (wrong_vault, "alpha bravo charlie delta echo foxtrot"),
            (
                response.clone(),
                "wrong passphrase with enough words here now",
            ),
        ] {
            assert_eq!(
                dispatch(
                    &mut core,
                    "_worker_enrollment_finish_hosted_owner",
                    &json!({"responseJson": response.to_string(), "passphrase": passphrase})
                        .to_string(),
                )["ok"],
                false
            );
            assert!(core.client.is_none());
            assert!(core.identity_session.is_none());
            assert!(core.hosted_provisioning.is_some());
        }

        let installed = value(&dispatch(
            &mut core,
            "_worker_enrollment_finish_hosted_owner",
            &json!({
                "responseJson": response.to_string(),
                "passphrase": "alpha bravo charlie delta echo foxtrot"
            })
            .to_string(),
        ));
        assert_eq!(installed["vaultId"], view.vault_id);
        assert_eq!(installed["deviceId"], view.device_id);
        assert_eq!(installed["role"], "owner");
        assert!(installed.get("deviceToken").is_none());
        assert!(!installed.to_string().contains("pgr_"));
        assert!(root.path().join("client.db").is_file());
        assert!(core.hosted_provisioning.is_none());

        for method in ["transfer", "shared_secret"] {
            value(&dispatch(
                &mut core,
                "_worker_enrollment_new_owner_from_identity",
                r#"{"deviceName":"Browser owner"}"#,
            ));
            value(&dispatch(
                &mut core,
                "_worker_enrollment_create_phone_invitation",
                &json!({"method": method}).to_string(),
            ));
        }

        assert_eq!(
            dispatch(
                &mut core,
                "_worker_enrollment_prepare_hosted_owner",
                &json!({
                    "accountId": account,
                    "passphrase": "alpha bravo charlie delta echo foxtrot",
                    "deviceName": "Browser owner"
                })
                .to_string(),
            )["ok"],
            false
        );
        for command in [
            "enrollment_prepare_hosted_owner",
            "enrollment_accept_hosted_grant",
            "enrollment_finish_hosted_owner",
        ] {
            let public: Value = serde_json::from_str(
                &core.dispatch(&json!({"command": command, "args": {}}).to_string()),
            )
            .unwrap();
            assert_eq!(public["error"]["code"], "unknown-command");
        }
    }
}
