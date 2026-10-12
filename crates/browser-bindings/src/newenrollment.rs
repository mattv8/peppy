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
    passphrase: SecretString,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfirmInvitation {
    status_json: String,
    shared_secret: Option<SecretString>,
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
                let sas = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .phone_sas(input.status_json)
                    .map_err(|_| invalid())?;
                Ok(json!({"sas": sas}))
            }
            "_worker_enrollment_approve_phone_invitation" => {
                let mut input: ApproveInvitation =
                    serde_json::from_str(raw.get()).map_err(|_| invalid())?;
                let request = self
                    .enrollment
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .approve_phone_invitation(
                        input.status_json,
                        std::mem::take(&mut *input.passphrase.0),
                    )
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

    const ORIGIN: &str = "https://peppy.test/";

    fn dispatch(core: &mut BrowserCore, command: &str, args: &str) -> Value {
        serde_json::from_str(&core.dispatch(&format!(r#"{{"command":"{command}","args":{args}}}"#)))
            .unwrap()
    }

    fn value(response: &Value) -> Value {
        eprintln!("test response: {response}");
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
