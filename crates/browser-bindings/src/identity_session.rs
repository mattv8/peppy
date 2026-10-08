//! Worker-private identity commands. Only the private transport bootstrap returns a bearer token;
//! public state and checkpoint responses never contain plaintext credentials or database keys.

use super::*;
use crate::local_identity::{self, IdentityMetadata, IdentitySecrets, WrappedIdentity};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_hosted_client::device_credentials::{
    CREDENTIAL_EXPORT_FILENAME, MAX_CREDENTIAL_BYTES, PortableDeviceCredential,
    parse_portable_credential, serialize_portable_credential,
};
use serde::Deserialize;

pub(super) struct IdentitySession {
    metadata: IdentityMetadata,
    secrets: IdentitySecrets,
    wrapped: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Initialize {
    origin: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Enroll {
    metadata: IdentityMetadata,
    device_token: SecretString,
    passphrase: SecretString,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Unlock {
    wrapped_identity: String,
    passphrase: SecretString,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rotate {
    profile: KeyProfile,
    header: VaultCheckHeader,
    passphrase: SecretString,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialFile {
    bytes: SecretBytes,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyCredentialFile {
    metadata: IdentityMetadata,
    device_token: SecretString,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableMetadataInput {
    bytes: SecretBytes,
    vault: AuthenticatedVault,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedVault {
    vault_id: String,
    device_id: String,
    role: local_identity::DeviceRole,
    public_key_profile: Value,
    encrypted_vault_check_header: String,
    profile_fingerprint: String,
    key_epoch: u32,
}

enum CredentialFileFormat {
    Portable,
    Legacy,
}

impl BrowserCore {
    pub(super) fn identity_command(
        &mut self,
        command: &str,
        raw: &RawValue,
    ) -> Result<Value, Failure> {
        match command {
            "_worker_initialize" => self.initialize_identity(raw),
            "_worker_enroll_identity" => self.enroll_identity(raw),
            "_worker_unlock_identity" => self.unlock_identity(raw),
            "_worker_checkpoint_identity" => self.checkpoint_identity(raw),
            "_worker_identity_metadata" => self.identity_metadata(raw),
            "_worker_transport_token" => self.transport_token(raw),
            "_worker_rotate_identity" => self.rotate_identity(raw),
            "_worker_parse_credential_file" => self.parse_credential_file(raw),
            "_worker_portable_identity_metadata" => self.portable_identity_metadata(raw),
            "_worker_export_credential" => self.export_credential(raw),
            _ => Err(unknown_command()),
        }
    }

    fn initialize_identity(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        let input: Initialize = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let origin = canonical_origin(&input.origin)?;
        if self
            .trusted_origin
            .as_deref()
            .is_some_and(|bound| bound != origin)
        {
            return Err(invalid());
        }
        self.trusted_origin = Some(origin);
        Ok(json!({}))
    }

    fn enroll_identity(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        let mut input: Enroll = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let origin = self.trusted_origin.as_deref().ok_or_else(invalid)?;
        if self.client.is_some()
            || self.identity_session.is_some()
            || self.root.join("client.db").exists()
        {
            return Err(Failure::new(
                "identity-exists",
                "A local identity already exists. Nothing was reset.",
            ));
        }
        if input.metadata.origin != origin {
            return Err(invalid());
        }
        let mut database_key = Zeroizing::new([0_u8; 32]);
        libsodium_rs::ensure_init()
            .map_err(|_| Failure::new("unavailable", "Secure random generation is unavailable."))?;
        libsodium_rs::random::fill_bytes(&mut *database_key);
        let secrets =
            IdentitySecrets::new(*database_key, std::mem::take(&mut *input.device_token.0))
                .map_err(identity_failure)?;
        let wrapped =
            local_identity::seal_identity(input.metadata.clone(), &secrets, &input.passphrase.0)
                .map_err(identity_failure)?;
        let serialized = serialize_wrapped(&wrapped)?;
        let client = open_client(&self.root, &input.metadata, &secrets)?;
        client
            .unlock(
                &input.metadata.profile,
                &input.metadata.header,
                &input.passphrase.0,
            )
            .map_err(core)?;
        self.device_id = Some(DeviceId(input.metadata.device_id));
        self.context.origin = Some(origin.to_owned());
        self.context.device_role = Some(role_name(input.metadata.role).to_owned());
        self.client = Some(client);
        self.identity_session = Some(IdentitySession {
            metadata: input.metadata,
            secrets,
            wrapped: serialized,
        });
        Ok(public_state(
            self.identity_session.as_ref().expect("identity was set"),
        ))
    }

    fn unlock_identity(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        let input: Unlock = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let origin = self.trusted_origin.as_deref().ok_or_else(invalid)?;
        if self.client.is_some()
            || self.identity_session.is_some()
            || !self.root.join("client.db").is_file()
        {
            return Err(Failure::new(
                "identity-unavailable",
                "The existing local identity cannot be opened.",
            ));
        }
        let unlocked = local_identity::open_identity(
            input.wrapped_identity.as_bytes(),
            origin,
            &input.passphrase.0,
        )
        .map_err(identity_failure)?;
        let metadata = unlocked.metadata().clone();
        let secrets = unlocked.into_secrets();
        let client = open_client(&self.root, &metadata, &secrets)?;
        if client
            .key_status()
            .map_err(core)?
            .active_epoch
            .is_some_and(|epoch| epoch != metadata.profile.key_epoch)
        {
            return Err(Failure::new(
                "epoch-mismatch",
                "The local identity epoch does not match the protected database.",
            ));
        }
        client
            .unlock(&metadata.profile, &metadata.header, &input.passphrase.0)
            .map_err(core)?;
        self.device_id = Some(DeviceId(metadata.device_id));
        self.context.origin = Some(origin.to_owned());
        self.context.device_role = Some(role_name(metadata.role).to_owned());
        self.client = Some(client);
        self.identity_session = Some(IdentitySession {
            metadata,
            secrets,
            wrapped: input.wrapped_identity.into_bytes(),
        });
        Ok(public_state(
            self.identity_session.as_ref().expect("identity was set"),
        ))
    }

    fn checkpoint_identity(&self, raw: &RawValue) -> Result<Value, Failure> {
        empty(raw)?;
        let session = self.identity_session.as_ref().ok_or_else(unavailable)?;
        let wrapped = std::str::from_utf8(&session.wrapped).map_err(|_| invalid())?;
        Ok(json!({"wrappedIdentity": wrapped}))
    }

    fn identity_metadata(&self, raw: &RawValue) -> Result<Value, Failure> {
        empty(raw)?;
        Ok(public_state(
            self.identity_session.as_ref().ok_or_else(unavailable)?,
        ))
    }

    fn transport_token(&self, raw: &RawValue) -> Result<Value, Failure> {
        empty(raw)?;
        let session = self.identity_session.as_ref().ok_or_else(unavailable)?;
        // The Worker moves this bootstrap token into its fetch closure; it is never a UI-port DTO.
        Ok(session
            .secrets
            .with_device_token(|token| json!({"deviceToken": token})))
    }

    fn rotate_identity(&mut self, raw: &RawValue) -> Result<Value, Failure> {
        let input: Rotate = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let session = self.identity_session.as_ref().ok_or_else(unavailable)?;
        if input.profile.vault_id != session.metadata.vault_id
            || input.header.profile != input.profile
        {
            return Err(invalid());
        }
        if input.profile.key_epoch <= session.metadata.profile.key_epoch {
            return Err(Failure::new(
                "invalid-rotation",
                "The new identity epoch must be newer.",
            ));
        }
        let mut metadata = session.metadata.clone();
        metadata.profile = input.profile;
        metadata.header = input.header;
        let wrapped =
            local_identity::seal_identity(metadata.clone(), &session.secrets, &input.passphrase.0)
                .map_err(identity_failure)?;
        let serialized = serialize_wrapped(&wrapped)?;
        self.client()?
            .unlock(&metadata.profile, &metadata.header, &input.passphrase.0)
            .map_err(core)?;
        self.client()?
            .activate_epoch(metadata.profile.key_epoch)
            .map_err(core)?;
        let session = self.identity_session.as_mut().ok_or_else(unavailable)?;
        session.metadata = metadata;
        session.wrapped = serialized;
        Ok(json!({"epoch": session.metadata.profile.key_epoch.to_string()}))
    }

    fn parse_credential_file(&self, raw: &RawValue) -> Result<Value, Failure> {
        let input: CredentialFile = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let origin = self.trusted_origin.as_deref().ok_or_else(invalid)?;
        if input.bytes.0.len() > MAX_LEGACY_CREDENTIAL_BYTES {
            return if legacy_marker(&input.bytes.0) {
                Err(invalid_identity())
            } else if portable_marker(&input.bytes.0) {
                Err(portable_failure(
                    peppy_hosted_client::device_credentials::PortableCredentialError::TooLarge,
                ))
            } else {
                Err(invalid_identity())
            };
        }
        match classify_credential_file(&input.bytes.0)? {
            CredentialFileFormat::Portable => {
                self.parse_portable_credential_file(&input.bytes.0, origin)
            }
            CredentialFileFormat::Legacy => {
                self.parse_legacy_credential_file(&input.bytes.0, origin)
            }
        }
    }

    fn portable_identity_metadata(&self, raw: &RawValue) -> Result<Value, Failure> {
        let input: PortableMetadataInput =
            serde_json::from_str(raw.get()).map_err(|_| invalid_vault())?;
        let origin = self.trusted_origin.as_deref().ok_or_else(invalid)?;
        if input.bytes.0.len() > MAX_CREDENTIAL_BYTES {
            return Err(portable_failure(
                peppy_hosted_client::device_credentials::PortableCredentialError::TooLarge,
            ));
        }
        let credential = parse_portable_credential(&input.bytes.0).map_err(portable_failure)?;
        if credential.origin() != origin {
            return Err(credential_origin_mismatch());
        }
        let vault_id = Uuid::parse_str(&input.vault.vault_id).map_err(|_| invalid_vault())?;
        let device_id = Uuid::parse_str(&input.vault.device_id).map_err(|_| invalid_vault())?;
        if vault_id.to_string() != credential.vault_id()
            || device_id.to_string() != credential.device_id()
        {
            return Err(credential_vault_mismatch());
        }
        let profile: KeyProfile =
            serde_json::from_value(input.vault.public_key_profile).map_err(|_| invalid_vault())?;
        let header_bytes = Zeroizing::new(
            STANDARD
                .decode(input.vault.encrypted_vault_check_header.as_bytes())
                .map_err(|_| invalid_vault())?,
        );
        let header: VaultCheckHeader =
            serde_json::from_slice(&header_bytes).map_err(|_| invalid_vault())?;
        if profile.vault_id != vault_id
            || header.profile != profile
            || input.vault.key_epoch != profile.key_epoch
            || profile.fingerprint().map_err(|_| invalid_vault())?
                != input.vault.profile_fingerprint
        {
            return Err(credential_vault_mismatch());
        }
        let metadata = IdentityMetadata {
            version: 1,
            origin: credential.origin().to_owned(),
            vault_id,
            device_id,
            role: input.vault.role,
            profile,
            header,
        };
        local_identity::validate_metadata(&metadata).map_err(|_| invalid_vault())?;
        serde_json::to_value(metadata).map_err(|_| invalid_vault())
    }

    fn parse_portable_credential_file(&self, bytes: &[u8], origin: &str) -> Result<Value, Failure> {
        match parse_portable_credential(bytes) {
            Ok(credential) => {
                if credential.origin() != origin {
                    return Err(Failure::new(
                        "credential-origin-mismatch",
                        "The credential belongs to a different server.",
                    ));
                }
                Ok(json!({
                    "format": "portable",
                    "origin": credential.origin(),
                    "vaultId": credential.vault_id(),
                    "deviceId": credential.device_id(),
                    "deviceToken": credential.device_token(),
                }))
            }
            Err(error) => Err(portable_failure(error)),
        }
    }

    fn parse_legacy_credential_file(&self, bytes: &[u8], origin: &str) -> Result<Value, Failure> {
        let legacy: LegacyCredentialFile =
            serde_json::from_slice(bytes).map_err(|_| invalid_identity())?;
        if legacy.metadata.origin != origin {
            return Err(credential_origin_mismatch());
        }
        local_identity::validate_metadata(&legacy.metadata).map_err(|_| invalid_identity())?;
        local_identity::validate_token(&legacy.device_token.0).map_err(|_| invalid_identity())?;
        Ok(json!({
            "format": "legacy",
            "metadata": legacy.metadata,
            "deviceToken": &*legacy.device_token.0,
        }))
    }

    fn export_credential(&self, raw: &RawValue) -> Result<Value, Failure> {
        empty(raw)?;
        let origin = self.trusted_origin.as_deref().ok_or_else(unavailable)?;
        let session = self.identity_session.as_ref().ok_or_else(unavailable)?;
        if session.metadata.origin != origin || self.context.origin.as_deref() != Some(origin) {
            return Err(unavailable());
        }
        let keys = self.client()?.key_status().map_err(core)?;
        if !keys
            .active_epoch
            .is_some_and(|epoch| keys.unlocked_epochs.contains(&epoch))
        {
            return Err(Failure::new(
                "locked",
                "Unlock sync before exporting credentials.",
            ));
        }
        session.secrets.with_device_token(|token| {
            let credential = PortableDeviceCredential::new(
                session.metadata.origin.clone(),
                session.metadata.vault_id.to_string(),
                session.metadata.device_id.to_string(),
                token.to_owned(),
            )
            .map_err(|_| invalid())?;
            let bytes = serialize_portable_credential(&credential).map_err(|_| invalid())?;
            Ok(json!({"filename": CREDENTIAL_EXPORT_FILENAME, "bytes": bytes.to_vec()}))
        })
    }
}

const MAX_LEGACY_CREDENTIAL_BYTES: usize = 1024 * 1024;

fn classify_credential_file(bytes: &[u8]) -> Result<CredentialFileFormat, Failure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid_identity())?;
    let object = value.as_object().ok_or_else(invalid_identity)?;
    if object.contains_key("metadata") && object.contains_key("deviceToken") {
        return Ok(CredentialFileFormat::Legacy);
    }
    if object.contains_key("version") {
        return Ok(CredentialFileFormat::Portable);
    }
    Err(invalid_identity())
}

fn portable_marker(bytes: &[u8]) -> bool {
    json_key_marker(bytes, b"\"version\"")
}

fn legacy_marker(bytes: &[u8]) -> bool {
    json_key_marker(bytes, b"\"metadata\"") && json_key_marker(bytes, b"\"deviceToken\"")
}

fn json_key_marker(bytes: &[u8], marker: &[u8]) -> bool {
    bytes.windows(marker.len()).any(|part| part == marker)
}

fn invalid_identity() -> Failure {
    Failure::new("invalid-identity", "The selected identity file is invalid.")
}

fn invalid_vault() -> Failure {
    Failure::new(
        "credential-invalid-vault",
        "The authenticated vault response is invalid.",
    )
}

fn credential_vault_mismatch() -> Failure {
    Failure::new(
        "credential-vault-mismatch",
        "The credential does not match the authenticated vault.",
    )
}

fn credential_origin_mismatch() -> Failure {
    Failure::new(
        "credential-origin-mismatch",
        "The credential belongs to a different server.",
    )
}

fn portable_failure(
    error: peppy_hosted_client::device_credentials::PortableCredentialError,
) -> Failure {
    use peppy_hosted_client::device_credentials::{OriginError, PortableCredentialError};

    match error {
        PortableCredentialError::TooLarge => Failure::new(
            "credential-file-too-large",
            "The credential file exceeds the 16 KiB safety limit.",
        ),
        PortableCredentialError::InvalidJson => Failure::new(
            "credential-invalid-json",
            "The credential file is not valid Peppy credential JSON.",
        ),
        PortableCredentialError::UnsupportedVersion => Failure::new(
            "credential-unsupported-version",
            "Only Peppy v1 device credentials are supported.",
        ),
        PortableCredentialError::InvalidToken => Failure::new(
            "credential-invalid-token",
            "The credential does not contain a valid device token.",
        ),
        PortableCredentialError::InvalidVaultId => Failure::new(
            "credential-invalid-vault-id",
            "The credential vault ID is invalid.",
        ),
        PortableCredentialError::InvalidDeviceId => Failure::new(
            "credential-invalid-device-id",
            "The credential device ID is invalid.",
        ),
        PortableCredentialError::InvalidOrigin(OriginError::Empty | OriginError::InvalidUrl) => {
            Failure::new(
                "credential-invalid-origin",
                "The server origin is not a valid URL.",
            )
        }
        PortableCredentialError::InvalidOrigin(OriginError::Credentials) => Failure::new(
            "credential-origin-credentials",
            "Server origin must not include credentials.",
        ),
        PortableCredentialError::InvalidOrigin(OriginError::PathQueryOrFragment) => Failure::new(
            "credential-origin-path",
            "Server origin must not include a path, query, or fragment.",
        ),
        PortableCredentialError::InvalidOrigin(OriginError::Insecure) => Failure::new(
            "credential-origin-insecure",
            "Use an HTTPS origin, or an explicit loopback HTTP origin for development.",
        ),
        PortableCredentialError::Serialization => invalid(),
    }
}

fn empty(raw: &RawValue) -> Result<(), Failure> {
    serde_json::from_str::<Empty>(raw.get())
        .map(|_| ())
        .map_err(|_| invalid())
}

fn open_client(
    root: &Path,
    metadata: &IdentityMetadata,
    secrets: &IdentitySecrets,
) -> Result<Client, Failure> {
    secrets.with_database_key(|key| {
        let key = DatabaseKey::new(key).map_err(core)?;
        Client::open(
            ClientConfig {
                database_path: root.join("client.db"),
                vault_id: VaultId(metadata.vault_id),
                device_id: DeviceId(metadata.device_id),
            },
            key,
        )
        .map_err(core)
    })
}

fn serialize_wrapped(wrapped: &WrappedIdentity) -> Result<Vec<u8>, Failure> {
    serde_json::to_vec(wrapped).map_err(|_| invalid())
}

fn identity_failure(error: local_identity::IdentityError) -> Failure {
    match error {
        local_identity::IdentityError::AuthFailed
        | local_identity::IdentityError::InvalidPassphrase => Failure::new(
            "unlock-failed",
            "The passphrase did not unlock this vault. No local data was changed.",
        ),
        local_identity::IdentityError::Unavailable => {
            Failure::new("unavailable", "The protected identity is unavailable.")
        }
        local_identity::IdentityError::Invalid => invalid(),
    }
}

fn role_name(role: local_identity::DeviceRole) -> &'static str {
    match role {
        local_identity::DeviceRole::Owner => "owner",
        local_identity::DeviceRole::Device => "device",
        local_identity::DeviceRole::Gateway => "gateway",
    }
}

fn public_state(session: &IdentitySession) -> Value {
    json!({"vaultId": session.metadata.vault_id, "deviceId": session.metadata.device_id, "role": role_name(session.metadata.role), "unlocked": true, "epoch": session.metadata.profile.key_epoch.to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use std::sync::OnceLock;

    const ORIGIN: &str = "https://peppy.test";
    const PHRASE: &str = "correct horse battery staple";
    const NEW_PHRASE: &str = "new correct horse battery staple";
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn profile_and_header() -> &'static (KeyProfile, VaultCheckHeader) {
        static FIXTURE: OnceLock<(KeyProfile, VaultCheckHeader)> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let profile = KeyProfile::new(Uuid::new_v4(), 1).unwrap();
            let root = derive_root_key(PHRASE, &profile).unwrap();
            let header = create_vault_check_header(&root, profile.clone()).unwrap();
            (profile, header)
        })
    }

    fn metadata(device_id: Uuid) -> IdentityMetadata {
        let (profile, header) = profile_and_header();
        IdentityMetadata {
            version: 1,
            origin: ORIGIN.into(),
            vault_id: profile.vault_id,
            device_id,
            role: local_identity::DeviceRole::Device,
            profile: profile.clone(),
            header: header.clone(),
        }
    }

    fn dispatch(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
            .unwrap()
    }

    fn value(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        let response = dispatch(core, command, args);
        assert_eq!(response["ok"], true, "{response}");
        response["value"].clone()
    }

    fn error_code(core: &mut BrowserCore, command: &str, args: Value) -> String {
        let response = dispatch(core, command, args);
        assert_eq!(response["ok"], false, "{response}");
        response["error"]["code"].as_str().unwrap().to_owned()
    }

    fn initialize(core: &mut BrowserCore) {
        value(
            core,
            "_worker_initialize",
            json!({"origin": format!("{ORIGIN}/")}),
        );
    }

    fn enroll(core: &mut BrowserCore, metadata: &IdentityMetadata) -> Value {
        value(
            core,
            "_worker_enroll_identity",
            json!({"metadata": metadata, "deviceToken": TOKEN, "passphrase": PHRASE}),
        )
    }

    fn checkpoint(core: &mut BrowserCore) -> String {
        value(core, "_worker_checkpoint_identity", json!({}))["wrappedIdentity"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn save_draft(core: &mut BrowserCore, device_id: Uuid, text: &str) -> Value {
        value(
            core,
            "save_draft",
            json!({
                "id": "new",
                "conversationId": "",
                "text": text,
                "recipientIds": ["+15555550100"],
                "attachmentIds": [],
                "gatewayId": device_id,
                "simId": "sim-1",
                "expectedRevision": "0"
            }),
        )
    }

    #[test]
    fn enroll_restore_uses_real_sqlcipher_and_keeps_public_responses_sanitized() {
        let root = tempfile::tempdir().unwrap();
        let device_id = Uuid::new_v4();
        let metadata = metadata(device_id);
        let mut core = BrowserCore::new(root.path().to_owned());

        initialize(&mut core);
        let enrolled = enroll(&mut core, &metadata);
        assert_eq!(enrolled["vaultId"], metadata.vault_id.to_string());
        assert_eq!(enrolled["deviceId"], device_id.to_string());
        assert_eq!(enrolled["role"], "device");
        assert_eq!(enrolled["unlocked"], true);
        let draft = save_draft(&mut core, device_id, "retained identity draft");
        let wrapped = checkpoint(&mut core);
        let token = value(&mut core, "_worker_transport_token", json!({}));
        assert_eq!(token["deviceToken"], TOKEN);

        let public_text = serde_json::to_string(&enrolled).unwrap();
        let checkpoint_text = wrapped.as_bytes();
        assert!(!public_text.contains(TOKEN));
        assert!(!public_text.contains(PHRASE));
        assert!(!wrapped.contains(TOKEN));
        assert!(!wrapped.contains(PHRASE));
        let unlocked = local_identity::open_identity(checkpoint_text, ORIGIN, PHRASE).unwrap();
        unlocked.secrets().with_database_key(|key| {
            assert!(
                !public_text
                    .as_bytes()
                    .windows(key.len())
                    .any(|candidate| candidate == key)
            );
            assert!(
                !checkpoint_text
                    .windows(key.len())
                    .any(|candidate| candidate == key)
            );
        });

        value(&mut core, "close", json!({}));
        assert_eq!(
            error_code(
                &mut core,
                "_worker_unlock_identity",
                json!({"wrappedIdentity": wrapped, "passphrase": "wrong phrase"}),
            ),
            "unlock-failed"
        );
        assert!(core.client.is_none());
        assert!(core.identity_session.is_none());
        assert!(root.path().join("client.db").is_file());

        let restored = value(
            &mut core,
            "_worker_unlock_identity",
            json!({"wrappedIdentity": wrapped, "passphrase": PHRASE}),
        );
        assert_eq!(restored["vaultId"], metadata.vault_id.to_string());
        assert!(!restored.to_string().contains(TOKEN));
        let snapshot = value(&mut core, "snapshot", json!({}));
        assert_eq!(snapshot["draft"]["id"], draft["id"]);
        assert_eq!(snapshot["draft"]["text"], "retained identity draft");
        assert_eq!(
            value(&mut core, "_worker_transport_token", json!({}))["deviceToken"],
            TOKEN
        );
    }

    #[test]
    fn trusted_origin_input_and_no_reset_guards_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());

        assert_eq!(
            error_code(
                &mut core,
                "_worker_enroll_identity",
                json!({"metadata": metadata, "deviceToken": TOKEN, "passphrase": PHRASE}),
            ),
            "invalid-request"
        );
        assert!(!root.path().join("client.db").exists());
        initialize(&mut core);
        assert_eq!(
            error_code(
                &mut core,
                "_worker_initialize",
                json!({"origin": "https://other.test"}),
            ),
            "invalid-request"
        );
        for args in [
            json!({"metadata": metadata, "deviceToken": 7, "passphrase": PHRASE}),
            json!({"metadata": metadata, "deviceToken": TOKEN, "passphrase": false}),
            json!({"metadata": metadata, "deviceToken": "short", "passphrase": PHRASE}),
        ] {
            assert_eq!(
                error_code(&mut core, "_worker_enroll_identity", args),
                "invalid-request"
            );
            assert!(!root.path().join("client.db").exists());
        }
        assert_eq!(
            error_code(
                &mut core,
                "_worker_enroll_identity",
                json!({"metadata": metadata, "deviceToken": TOKEN, "passphrase": "wrong phrase"}),
            ),
            "unlock-failed"
        );
        assert!(!root.path().join("client.db").exists());
        let mut wrong_header = metadata.clone();
        wrong_header.header.check.ciphertext[0] ^= 1;
        assert_eq!(
            error_code(
                &mut core,
                "_worker_enroll_identity",
                json!({"metadata": wrong_header, "deviceToken": TOKEN, "passphrase": PHRASE}),
            ),
            "unlock-failed"
        );
        assert!(!root.path().join("client.db").exists());

        let mut mismatched = metadata.clone();
        mismatched.origin = "https://other.test".into();
        assert_eq!(
            error_code(
                &mut core,
                "_worker_enroll_identity",
                json!({"metadata": mismatched, "deviceToken": TOKEN, "passphrase": PHRASE}),
            ),
            "invalid-request"
        );
        assert!(!root.path().join("client.db").exists());

        enroll(&mut core, &metadata);
        let wrapped = checkpoint(&mut core);
        value(&mut core, "close", json!({}));
        assert_eq!(
            error_code(
                &mut core,
                "_worker_initialize",
                json!({"origin": "https://other.test"}),
            ),
            "invalid-request"
        );
        assert_eq!(
            error_code(
                &mut core,
                "_worker_enroll_identity",
                json!({"metadata": metadata, "deviceToken": TOKEN, "passphrase": PHRASE}),
            ),
            "identity-exists"
        );
        assert!(root.path().join("client.db").is_file());

        std::fs::remove_file(root.path().join("client.db")).unwrap();
        let mut missing_database = BrowserCore::new(root.path().to_owned());
        initialize(&mut missing_database);
        assert_eq!(
            error_code(
                &mut missing_database,
                "_worker_unlock_identity",
                json!({"wrappedIdentity": wrapped, "passphrase": PHRASE}),
            ),
            "identity-unavailable"
        );
        assert!(!root.path().join("client.db").exists());
    }

    #[test]
    fn worker_initialize_allows_only_loopback_http_origins() {
        for origin in [
            "http://localhost:7100",
            "http://127.0.0.1:7100",
            "http://[::1]:7100",
            "https://example.test",
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut core = BrowserCore::new(root.path().to_owned());
            assert_eq!(
                dispatch(&mut core, "_worker_initialize", json!({"origin": origin}))["ok"],
                true,
                "{origin}"
            );
        }

        for origin in [
            "http://192.168.1.1",
            "http://example.test",
            "http://localhost.evil.test",
            "http://127.0.0.1.evil.test",
            "http://user:secret@localhost",
            "http://localhost/path",
            "http://localhost?query=value",
            "http://localhost#fragment",
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut core = BrowserCore::new(root.path().to_owned());
            assert_eq!(
                error_code(&mut core, "_worker_initialize", json!({"origin": origin})),
                "invalid-request",
                "{origin}"
            );
        }
    }

    #[test]
    fn rotation_failures_preserve_old_state_and_success_cuts_over_atomically() {
        let root = tempfile::tempdir().unwrap();
        let device_id = Uuid::new_v4();
        let metadata = metadata(device_id);
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        enroll(&mut core, &metadata);
        let draft = save_draft(&mut core, device_id, "survives key rotation");
        let old_wrapped = checkpoint(&mut core);

        let new_profile = KeyProfile::new(metadata.vault_id, 2).unwrap();
        let new_root = derive_root_key(NEW_PHRASE, &new_profile).unwrap();
        let new_header = create_vault_check_header(&new_root, new_profile.clone()).unwrap();
        assert_eq!(
            error_code(
                &mut core,
                "_worker_rotate_identity",
                json!({"profile": new_profile, "header": new_header, "passphrase": false}),
            ),
            "invalid-request"
        );
        assert_eq!(checkpoint(&mut core), old_wrapped);
        assert_eq!(
            error_code(
                &mut core,
                "_worker_rotate_identity",
                json!({"profile": new_profile, "header": new_header, "passphrase": "wrong new phrase"}),
            ),
            "unlock-failed"
        );
        assert_eq!(checkpoint(&mut core), old_wrapped);
        assert_eq!(
            value(&mut core, "_worker_transport_token", json!({}))["deviceToken"],
            TOKEN
        );
        assert_eq!(
            core.client().unwrap().key_status().unwrap().active_epoch,
            Some(1)
        );

        let rotated = value(
            &mut core,
            "_worker_rotate_identity",
            json!({"profile": new_profile, "header": new_header, "passphrase": NEW_PHRASE}),
        );
        assert_eq!(rotated["epoch"], "2");
        let new_wrapped = checkpoint(&mut core);
        assert_ne!(new_wrapped, old_wrapped);
        assert_eq!(
            core.client().unwrap().key_status().unwrap().active_epoch,
            Some(2)
        );

        value(&mut core, "close", json!({}));
        assert_eq!(
            error_code(
                &mut core,
                "_worker_unlock_identity",
                json!({"wrappedIdentity": old_wrapped, "passphrase": PHRASE}),
            ),
            "epoch-mismatch"
        );
        assert!(core.client.is_none());
        assert!(core.identity_session.is_none());
        let restored = value(
            &mut core,
            "_worker_unlock_identity",
            json!({"wrappedIdentity": new_wrapped, "passphrase": NEW_PHRASE}),
        );
        assert_eq!(restored["epoch"], "2");
        assert_eq!(restored["vaultId"], metadata.vault_id.to_string());
        assert_eq!(restored["deviceId"], device_id.to_string());
        let snapshot = value(&mut core, "snapshot", json!({}));
        assert_eq!(snapshot["draft"]["id"], draft["id"]);
        assert_eq!(snapshot["draft"]["text"], "survives key rotation");
        assert_eq!(
            value(&mut core, "_worker_transport_token", json!({}))["deviceToken"],
            TOKEN
        );
    }

    #[test]
    fn authenticated_metadata_transplants_are_rejected_without_state_change() {
        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        enroll(&mut core, &metadata);
        let wrapped = checkpoint(&mut core);
        value(&mut core, "close", json!({}));
        for args in [
            json!({"wrappedIdentity": 7, "passphrase": PHRASE}),
            json!({"wrappedIdentity": wrapped, "passphrase": false}),
        ] {
            assert_eq!(
                error_code(&mut core, "_worker_unlock_identity", args),
                "invalid-request"
            );
        }
        assert!(core.client.is_none());

        let mut origin_attack: Value = serde_json::from_str(&wrapped).unwrap();
        origin_attack["metadata"]["origin"] = json!("https://other.test");
        assert_eq!(
            error_code(
                &mut core,
                "_worker_unlock_identity",
                json!({"wrappedIdentity": origin_attack.to_string(), "passphrase": PHRASE}),
            ),
            "invalid-request"
        );

        let replacement_profile = KeyProfile::new(metadata.vault_id, 2).unwrap();
        let replacement_root = derive_root_key(PHRASE, &replacement_profile).unwrap();
        let replacement_header =
            create_vault_check_header(&replacement_root, replacement_profile.clone()).unwrap();
        let mut profile_attack: Value = serde_json::from_str(&wrapped).unwrap();
        profile_attack["metadata"]["profile"] = serde_json::to_value(replacement_profile).unwrap();
        profile_attack["metadata"]["header"] = serde_json::to_value(replacement_header).unwrap();
        assert_eq!(
            error_code(
                &mut core,
                "_worker_unlock_identity",
                json!({"wrappedIdentity": profile_attack.to_string(), "passphrase": PHRASE}),
            ),
            "unlock-failed"
        );
        assert!(core.client.is_none());
        assert!(core.identity_session.is_none());
        assert!(root.path().join("client.db").is_file());
    }

    #[test]
    fn worker_exports_only_active_unlocked_portable_credentials() {
        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        enroll(&mut core, &metadata);

        let exported = value(&mut core, "_worker_export_credential", json!({}));
        assert_eq!(exported["filename"], CREDENTIAL_EXPORT_FILENAME);
        let bytes: Vec<u8> = serde_json::from_value(exported["bytes"].clone()).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(TOKEN));
        for forbidden in [
            "databaseKey",
            "passphrase",
            "wrappedIdentity",
            "profile",
            "header",
        ] {
            assert!(!text.contains(forbidden), "serialized {forbidden}");
        }

        value(&mut core, "close", json!({}));
        assert_eq!(
            error_code(&mut core, "_worker_export_credential", json!({})),
            "credentials-required"
        );
    }

    #[test]
    fn worker_rejects_wrong_origin_before_returning_a_portable_token() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        let credential = serde_json::json!({
            "version": 1,
            "origin": "https://other.test",
            "vaultId": Uuid::new_v4(),
            "deviceId": Uuid::new_v4(),
            "deviceToken": TOKEN,
        });

        let response = dispatch(
            &mut core,
            "_worker_parse_credential_file",
            json!({"bytes": credential.to_string().as_bytes()}),
        );
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "credential-origin-mismatch");
        assert!(!response.to_string().contains(TOKEN));
    }

    #[test]
    fn worker_detects_legacy_identity_files_without_exposing_them_to_the_public_api() {
        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        let legacy = json!({"metadata": metadata, "deviceToken": TOKEN});

        let parsed = value(
            &mut core,
            "_worker_parse_credential_file",
            json!({"bytes": legacy.to_string().as_bytes()}),
        );
        assert_eq!(parsed["format"], "legacy");
        assert_eq!(parsed["deviceToken"], TOKEN);
        assert_eq!(
            error_code(&mut core, "parse_credential_file", json!({"bytes": []})),
            "unknown-command"
        );
    }

    #[test]
    fn worker_builds_metadata_only_from_matching_authenticated_portable_vaults() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        let credential = json!({
            "version": 1,
            "origin": ORIGIN,
            "vaultId": metadata.vault_id,
            "deviceId": metadata.device_id,
            "deviceToken": TOKEN,
        });
        let mut vault = json!({
            "vault_id": metadata.vault_id,
            "device_id": metadata.device_id,
            "role": "owner",
            "public_key_profile": metadata.profile,
            "encrypted_vault_check_header": STANDARD.encode(serde_json::to_vec(&metadata.header).unwrap()),
            "profile_fingerprint": metadata.profile.fingerprint().unwrap(),
            "key_epoch": metadata.profile.key_epoch,
        });

        for role in ["owner", "device", "gateway"] {
            vault["role"] = json!(role);
            let result = value(
                &mut core,
                "_worker_portable_identity_metadata",
                json!({"bytes": credential.to_string().as_bytes(), "vault": vault}),
            );
            assert_eq!(result["role"], role);
            assert_eq!(result["vaultId"], metadata.vault_id.to_string());
            assert!(result.get("deviceToken").is_none());
        }

        vault["vault_id"] = json!(Uuid::new_v4());
        assert_eq!(
            error_code(
                &mut core,
                "_worker_portable_identity_metadata",
                json!({"bytes": credential.to_string().as_bytes(), "vault": vault}),
            ),
            "credential-vault-mismatch"
        );

        for field in [
            "device_id",
            "public_key_profile",
            "encrypted_vault_check_header",
            "profile_fingerprint",
            "key_epoch",
        ] {
            let mut invalid = json!({
                "vault_id": metadata.vault_id,
                "device_id": metadata.device_id,
                "role": "device",
                "public_key_profile": metadata.profile,
                "encrypted_vault_check_header": STANDARD.encode(serde_json::to_vec(&metadata.header).unwrap()),
                "profile_fingerprint": metadata.profile.fingerprint().unwrap(),
                "key_epoch": metadata.profile.key_epoch,
            });
            invalid[field] = match field {
                "device_id" => json!(Uuid::new_v4()),
                "public_key_profile" => json!({}),
                "encrypted_vault_check_header" => json!("not-base64"),
                "profile_fingerprint" => json!("not-a-fingerprint"),
                "key_epoch" => json!(99),
                _ => unreachable!(),
            };
            assert!(
                dispatch(
                    &mut core,
                    "_worker_portable_identity_metadata",
                    json!({"bytes": credential.to_string().as_bytes(), "vault": invalid}),
                )["ok"]
                    == false,
                "{field}"
            );
        }
    }

    #[test]
    fn worker_classifies_legacy_before_portable_and_keeps_legacy_size_errors() {
        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        let legacy =
            json!({"version": 1, "metadata": metadata, "deviceToken": TOKEN, "legacyExtra": true});
        assert_eq!(
            value(
                &mut core,
                "_worker_parse_credential_file",
                json!({"bytes": legacy.to_string().as_bytes()}),
            )["format"],
            "legacy"
        );
        let portable = format!(
            r#"{{"version":1,"origin":"{ORIGIN}","vaultId":"{}","deviceId":"{}","deviceToken":"{}"{}}}"#,
            Uuid::new_v4(),
            Uuid::new_v4(),
            TOKEN,
            " ".repeat(MAX_CREDENTIAL_BYTES)
        );
        assert_eq!(
            error_code(
                &mut core,
                "_worker_parse_credential_file",
                json!({"bytes": portable.as_bytes()})
            ),
            "credential-file-too-large"
        );
        assert_eq!(
            error_code(
                &mut core,
                "_worker_parse_credential_file",
                json!({"bytes": vec![b' '; MAX_LEGACY_CREDENTIAL_BYTES + 1]}),
            ),
            "invalid-identity"
        );
    }

    #[test]
    fn worker_export_requires_active_identity_session_and_rejects_closed_sessions() {
        let root = tempfile::tempdir().unwrap();
        let device_id = Uuid::new_v4();
        let metadata = metadata(device_id);
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        enroll(&mut core, &metadata);
        let _wrapped = checkpoint(&mut core);

        // Verify export works when freshly enrolled with active unlocked identity
        let exported = value(&mut core, "_worker_export_credential", json!({}));
        assert_eq!(exported["filename"], CREDENTIAL_EXPORT_FILENAME);
        let bytes: Vec<u8> = serde_json::from_value(exported["bytes"].clone()).unwrap();
        assert!(!bytes.is_empty());

        // Close session removes both client and identity_session
        value(&mut core, "close", json!({}));
        assert!(core.client.is_none());
        assert!(core.identity_session.is_none());

        // Export now fails because there is no active session (credentials-required)
        assert_eq!(
            error_code(&mut core, "_worker_export_credential", json!({})),
            "credentials-required"
        );
    }

    #[test]
    fn worker_metadata_helper_distinguishes_vault_mismatch_from_invalid_vault() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);
        let credential = json!({
            "version": 1,
            "origin": ORIGIN,
            "vaultId": metadata.vault_id,
            "deviceId": metadata.device_id,
            "deviceToken": TOKEN,
        });

        let valid_vault = json!({
            "vault_id": metadata.vault_id,
            "device_id": metadata.device_id,
            "role": "owner",
            "public_key_profile": metadata.profile,
            "encrypted_vault_check_header": STANDARD.encode(serde_json::to_vec(&metadata.header).unwrap()),
            "profile_fingerprint": metadata.profile.fingerprint().unwrap(),
            "key_epoch": metadata.profile.key_epoch,
        });

        // Success case: matching vault
        let result = value(
            &mut core,
            "_worker_portable_identity_metadata",
            json!({"bytes": credential.to_string().as_bytes(), "vault": valid_vault}),
        );
        assert_eq!(result["role"], "owner");
        assert!(result.get("deviceToken").is_none());

        // credential-vault-mismatch: vault/device IDs differ (authenticated mismatch)
        let mut mismatched_vault = valid_vault.clone();
        mismatched_vault["vault_id"] = json!(Uuid::new_v4());
        assert_eq!(
            error_code(
                &mut core,
                "_worker_portable_identity_metadata",
                json!({"bytes": credential.to_string().as_bytes(), "vault": mismatched_vault}),
            ),
            "credential-vault-mismatch"
        );

        // credential-vault-mismatch: device_id also checked for consistency
        let mut device_mismatch = valid_vault.clone();
        device_mismatch["device_id"] = json!(Uuid::new_v4());
        assert_eq!(
            error_code(
                &mut core,
                "_worker_portable_identity_metadata",
                json!({"bytes": credential.to_string().as_bytes(), "vault": device_mismatch}),
            ),
            "credential-vault-mismatch"
        );

        // credential-invalid-vault: malformed response fields that fail parsing/validation
        // Note: fingerprint mismatch and key_epoch mismatch are caught as vault-mismatch (consistency checks),
        // while truly malformed/unparseable fields are invalid-vault
        for (field, bad_value) in [
            ("public_key_profile", json!({})),
            ("encrypted_vault_check_header", json!("not-base64")),
        ] {
            let mut invalid_vault = json!({
                "vault_id": metadata.vault_id,
                "device_id": metadata.device_id,
                "role": "device",
                "public_key_profile": metadata.profile,
                "encrypted_vault_check_header": STANDARD.encode(serde_json::to_vec(&metadata.header).unwrap()),
                "profile_fingerprint": metadata.profile.fingerprint().unwrap(),
                "key_epoch": metadata.profile.key_epoch,
            });
            invalid_vault[field] = bad_value;
            assert_eq!(
                error_code(
                    &mut core,
                    "_worker_portable_identity_metadata",
                    json!({"bytes": credential.to_string().as_bytes(), "vault": invalid_vault}),
                ),
                "credential-invalid-vault",
                "{field} should be credential-invalid-vault"
            );
        }

        // credential-vault-mismatch: fingerprint and epoch mismatches are consistency failures
        let mut bad_fingerprint = valid_vault.clone();
        bad_fingerprint["profile_fingerprint"] =
            json!("0000000000000000000000000000000000000000000000");
        assert_eq!(
            error_code(
                &mut core,
                "_worker_portable_identity_metadata",
                json!({"bytes": credential.to_string().as_bytes(), "vault": bad_fingerprint}),
            ),
            "credential-vault-mismatch"
        );

        let mut bad_epoch = valid_vault.clone();
        bad_epoch["key_epoch"] = json!(99);
        assert_eq!(
            error_code(
                &mut core,
                "_worker_portable_identity_metadata",
                json!({"bytes": credential.to_string().as_bytes(), "vault": bad_epoch}),
            ),
            "credential-vault-mismatch"
        );
    }

    #[test]
    fn worker_metadata_helper_rejects_wrong_origin_before_returning_metadata() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let root = tempfile::tempdir().unwrap();
        let metadata = metadata(Uuid::new_v4());
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);

        // Credential from different origin
        let credential = json!({
            "version": 1,
            "origin": "https://other.test",
            "vaultId": metadata.vault_id,
            "deviceId": metadata.device_id,
            "deviceToken": TOKEN,
        });

        let vault = json!({
            "vault_id": metadata.vault_id,
            "device_id": metadata.device_id,
            "role": "owner",
            "public_key_profile": metadata.profile,
            "encrypted_vault_check_header": STANDARD.encode(serde_json::to_vec(&metadata.header).unwrap()),
            "profile_fingerprint": metadata.profile.fingerprint().unwrap(),
            "key_epoch": metadata.profile.key_epoch,
        });

        // Should reject with credential-origin-mismatch before returning metadata
        let response = dispatch(
            &mut core,
            "_worker_portable_identity_metadata",
            json!({"bytes": credential.to_string().as_bytes(), "vault": vault}),
        );
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "credential-origin-mismatch");
        assert!(!response.to_string().contains(TOKEN));
    }

    #[test]
    fn worker_parses_legacy_origin_mismatch_before_returning_token() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        initialize(&mut core);

        let other_metadata = metadata(Uuid::new_v4());
        let mut wrong_origin_metadata = other_metadata.clone();
        wrong_origin_metadata.origin = "https://other.test".into();

        let legacy = json!({"metadata": wrong_origin_metadata, "deviceToken": TOKEN});

        // Should reject with credential-origin-mismatch before exposing token
        let response = dispatch(
            &mut core,
            "_worker_parse_credential_file",
            json!({"bytes": legacy.to_string().as_bytes()}),
        );
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "credential-origin-mismatch");
        assert!(!response.to_string().contains(TOKEN));
    }
}
