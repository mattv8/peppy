//! Worker-private identity commands. Only the private transport bootstrap returns a bearer token;
//! public state and checkpoint responses never contain plaintext credentials or database keys.

use super::*;
use crate::local_identity::{self, IdentityMetadata, IdentitySecrets, WrappedIdentity};
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
}
