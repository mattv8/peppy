//! Developer-only, deliberately SIMULATED gateway command line.
use std::{
    env, fs,
    fs::OpenOptions,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use peppy_client_core::{
    Client, ClientConfig, DatabaseKey, IncomingSms, KeyProfile, PermitDecision, SendResult,
    VaultCheckHeader,
};
use peppy_domain::{DeviceId, VaultId};
use peppy_gateway_simulator::{
    BOOTSTRAP_FILE, CREDENTIAL_FILE, DESKTOP_IMPORT_FILE, DEVELOPER_KEY_FILE, SimulatorError,
    SimulatorState, bootstrap_snapshot, declare_compaction_capability, drain_media, effects_path,
    http_client, publish_capabilities, read_bounded_json, read_or_create_route, read_state,
    run_one_carrier_effect_for_route, sync_http, upload_pending, write_state,
};
use peppy_hosted_api::enrollment::AdmissionChallenge;
use peppy_hosted_client::newenrollment::{ClientKind, EnrollmentClient};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
struct Bootstrap {
    profile: KeyProfile,
    header: VaultCheckHeader,
    fingerprint: String,
}

const DATABASE_FILE: &str = "gateway.sqlcipher";
const INITIALIZING_FILE: &str = "SIMULATED-client-initializing.json";
const INITIALIZED_FILE: &str = "SIMULATED-client-initialized.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RuntimeMarker {
    version: u8,
    vault_id: String,
    device_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseBootstrap {
    Existing,
    FirstStart,
}

fn usage() {
    eprintln!(
        "SIMULATED gateway: claim-owner STATE_DIR | admit-gateway STATE_DIR | admit-device STATE_DIR | run STATE_DIR\nSecrets are read from protected files, stdin, or PEPPY_SIMULATOR_PASSPHRASE; never argv."
    );
}
fn protected(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| format!("refusing to replace {}", path.display()))?;
    file.write_all(bytes)
        .map_err(|_| "SIMULATED file write failed".to_string())?;
    file.sync_all()
        .map_err(|_| "SIMULATED file sync failed".to_string())
}
fn sync_directory(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "SIMULATED state directory sync failed".to_string())
}

fn runtime_marker(state: &SimulatorState) -> RuntimeMarker {
    RuntimeMarker {
        version: 1,
        vault_id: state.vault_id.clone(),
        device_id: state.device_id.clone(),
    }
}

fn read_runtime_marker(path: &Path, state: &SimulatorState) -> Result<(), String> {
    let marker: RuntimeMarker = serde_json::from_slice(
        &fs::read(path).map_err(|_| "SIMULATED initialization marker read failed")?,
    )
    .map_err(|_| "SIMULATED initialization marker invalid")?;
    if marker != runtime_marker(state) {
        return Err("SIMULATED state identity mismatch; fresh pairing required".into());
    }
    Ok(())
}

fn make_existing_database_private(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(
            "SIMULATED client database is missing for an initialized identity; fresh pairing required"
                .into(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "SIMULATED database permissions failed")?;
    }
    Ok(())
}

fn prepare_database_bootstrap(
    dir: &Path,
    state: &SimulatorState,
) -> Result<DatabaseBootstrap, String> {
    let database = dir.join(DATABASE_FILE);
    let initializing = dir.join(INITIALIZING_FILE);
    let initialized = dir.join(INITIALIZED_FILE);

    if initialized.exists() {
        read_runtime_marker(&initialized, state)?;
        make_existing_database_private(&database)?;
        return Ok(DatabaseBootstrap::Existing);
    }

    if initializing.exists() {
        read_runtime_marker(&initializing, state)?;
    } else if database.exists() {
        // Upgrade an existing pre-marker simulator database without discarding its durable ledger.
        make_existing_database_private(&database)?;
        protected(
            &initialized,
            &serde_json::to_vec_pretty(&runtime_marker(state))
                .map_err(|_| "SIMULATED initialization marker encoding failed")?,
        )?;
        sync_directory(dir)?;
        return Ok(DatabaseBootstrap::Existing);
    } else {
        protected(
            &initializing,
            &serde_json::to_vec_pretty(&runtime_marker(state))
                .map_err(|_| "SIMULATED initialization marker encoding failed")?,
        )?;
        sync_directory(dir)?;
    }

    if database.exists() {
        make_existing_database_private(&database)?;
    } else {
        protected(&database, b"")?;
        sync_directory(dir)?;
    }
    Ok(DatabaseBootstrap::FirstStart)
}

fn finish_database_bootstrap(dir: &Path, state: &SimulatorState) -> Result<(), String> {
    let initialized = dir.join(INITIALIZED_FILE);
    if !initialized.exists() {
        protected(
            &initialized,
            &serde_json::to_vec_pretty(&runtime_marker(state))
                .map_err(|_| "SIMULATED initialization marker encoding failed")?,
        )?;
        sync_directory(dir)?;
    }
    let initializing = dir.join(INITIALIZING_FILE);
    if initializing.exists() {
        fs::remove_file(initializing)
            .map_err(|_| "SIMULATED initialization marker cleanup failed")?;
        sync_directory(dir)?;
    }
    Ok(())
}

fn runtime_error(context: &str, error: SimulatorError) -> String {
    if matches!(error, SimulatorError::FreshPairingRequired) {
        "SIMULATED identity has prior producer history; fresh pairing required".into()
    } else {
        format!("SIMULATED {context}")
    }
}

fn phrase() -> Result<String, String> {
    if let Ok(value) = env::var("PEPPY_SIMULATOR_PASSPHRASE") {
        return Ok(value);
    }
    eprintln!("SIMULATED developer passphrase (stdin, one line):");
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|_| "SIMULATED stdin failed".to_string())?;
    Ok(value.trim_end_matches(['\n', '\r']).to_string())
}
fn key_file(dir: &Path) -> Result<[u8; 32], String> {
    let path = dir.join(DEVELOPER_KEY_FILE);
    if path.exists() {
        return hex::decode(
            fs::read_to_string(path).map_err(|_| "SIMULATED keystore read failed")?,
        )
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("invalid SIMULATED developer keystore".into());
    }
    let mut key = [0; 32];
    rand::RngCore::fill_bytes(&mut OsRng, &mut key);
    protected(&path, hex::encode(key).as_bytes())?;
    Ok(key)
}
fn load_bootstrap(dir: &Path) -> Result<Bootstrap, String> {
    serde_json::from_slice(
        &fs::read(dir.join(BOOTSTRAP_FILE)).map_err(|_| "missing SIMULATED vault bootstrap")?,
    )
    .map_err(|_| "invalid SIMULATED vault bootstrap".into())
}

fn enrollment_origin() -> Result<String, String> {
    env::var("PUBLIC_API_URL").map_err(|_| "SIMULATED PUBLIC_API_URL is required".into())
}

fn demo_setup_token() -> Result<String, String> {
    match env::var("PEPPY_DEMO_SETUP_TOKEN") {
        Ok(token) if !token.is_empty() => Ok(token),
        _ => Err("SIMULATED isolated demo setup token is required".into()),
    }
}

fn demo_admin_password() -> Result<String, String> {
    match env::var("PEPPY_DEMO_ADMIN_PASSWORD") {
        Ok(password) if !password.is_empty() => Ok(password),
        _ => Err("SIMULATED isolated demo administrator password is required".into()),
    }
}

async fn post_enrollment_request(
    origin: &str,
    path: &str,
    request: String,
) -> Result<String, String> {
    let response = http_client(origin)
        .map_err(|error| format!("SIMULATED enrollment HTTP client failed: {error}"))?
        .post(format!("{origin}{path}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(request)
        .send()
        .await
        .map_err(|_| "SIMULATED enrollment request failed")?;
    let response = read_bounded_json(response)
        .await
        .map_err(|_| "SIMULATED enrollment response invalid")?;
    serde_json::to_string(&response).map_err(|_| "SIMULATED enrollment response invalid".into())
}

fn completion_path_for_challenge(challenge: &str) -> Result<String, String> {
    let challenge: AdmissionChallenge =
        serde_json::from_str(challenge).map_err(|_| "SIMULATED enrollment challenge invalid")?;
    Ok(format!(
        "/v1/enrollment/challenges/{}/complete",
        challenge.enrollment_id
    ))
}

fn store_enrollment_identity(
    dir: &Path,
    identity: peppy_hosted_client::newenrollment::NativeEnrollmentIdentity,
    credential_file: &str,
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|_| "SIMULATED state directory failed")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "SIMULATED state permissions failed")?;
    }
    let metadata = identity.metadata().clone();
    let vault_id = metadata.vault_id;
    let device_id = metadata.device_id;
    let profile = metadata.profile;
    let header = metadata.header;
    let bootstrap = Bootstrap {
        fingerprint: profile
            .fingerprint()
            .map_err(|_| "SIMULATED fingerprint failed")?,
        profile,
        header,
    };
    protected(
        &dir.join(BOOTSTRAP_FILE),
        &serde_json::to_vec_pretty(&bootstrap)
            .map_err(|_| "SIMULATED bootstrap encoding failed")?,
    )?;
    let _ = key_file(dir)?;
    if credential_file == CREDENTIAL_FILE {
        let _ = read_or_create_route(dir).map_err(|_| "SIMULATED route setup failed")?;
    }
    let credentials = SimulatorState {
        version: 1,
        origin: identity.origin().to_owned(),
        vault_id: vault_id.to_string(),
        device_id: device_id.to_string(),
        device_token: identity.device_credential_for_native_secure_store(),
    };
    write_state(&dir.join(credential_file), &credentials)
        .map_err(|_| "SIMULATED credential write failed")?;
    Ok(())
}

async fn claim_owner(dir: PathBuf) -> Result<(), String> {
    let origin = enrollment_origin()?;
    let passphrase = phrase()?;
    let mut enrollment = EnrollmentClient::new(
        origin.clone(),
        ClientKind::Android,
        "Simulated owner".into(),
    )
    .map_err(|_| "SIMULATED owner enrollment setup failed")?;
    let request = enrollment
        .create_owner_request(
            demo_setup_token()?,
            "admin".into(),
            demo_admin_password()?,
            passphrase.clone(),
        )
        .map_err(|_| "SIMULATED owner enrollment request failed")?;
    let response = post_enrollment_request(&origin, "/v1/instance/claim", request).await?;
    let identity = enrollment
        .finish_owner_admission(response, passphrase)
        .map_err(|_| "SIMULATED owner enrollment completion failed")?;
    store_enrollment_identity(&dir, identity, CREDENTIAL_FILE)?;
    println!("SIMULATED owner enrollment complete; protected state written");
    Ok(())
}

async fn admit(
    dir: PathBuf,
    kind: ClientKind,
    name: &str,
    credential_file: &str,
) -> Result<(), String> {
    let origin = enrollment_origin()?;
    let passphrase = phrase()?;
    let mut enrollment = EnrollmentClient::new(origin.clone(), kind, name.into())
        .map_err(|_| "SIMULATED enrollment setup failed")?;
    let challenge_request = enrollment
        .manual_challenge_request(None, None)
        .map_err(|_| "SIMULATED enrollment challenge request failed")?;
    let challenge =
        post_enrollment_request(&origin, "/v1/enrollment/challenges", challenge_request).await?;
    let completion_path = completion_path_for_challenge(&challenge)?;
    let completion_request = enrollment
        .complete_manual_challenge_request(challenge, passphrase.clone())
        .map_err(|_| "SIMULATED enrollment proof failed")?;
    let response = post_enrollment_request(&origin, &completion_path, completion_request).await?;
    let identity = enrollment
        .finish_manual_admission(response, passphrase)
        .map_err(|_| "SIMULATED enrollment completion failed")?;
    store_enrollment_identity(&dir, identity, credential_file)?;
    println!("SIMULATED device enrollment complete; protected state written");
    Ok(())
}

async fn run(dir: PathBuf) -> Result<(), String> {
    let state = read_state(&dir.join(CREDENTIAL_FILE))
        .map_err(|error| format!("missing SIMULATED gateway credentials: {error}"))?;
    let bootstrap = load_bootstrap(&dir)?;
    let route = read_or_create_route(&dir).map_err(|_| "SIMULATED route read failed")?;
    let vault_id = VaultId(state.vault_id.parse().map_err(|_| "invalid vault id")?);
    let device_id = DeviceId(state.device_id.parse().map_err(|_| "invalid device id")?);
    let bootstrap_state = prepare_database_bootstrap(&dir, &state)?;
    let config = ClientConfig {
        database_path: dir.join(DATABASE_FILE),
        vault_id,
        device_id,
    };
    let client = Client::open(
        config,
        DatabaseKey::new(&key_file(&dir)?).map_err(|_| "invalid SIMULATED developer keystore")?,
    )
    .map_err(|_| "SIMULATED client open failed")?;
    client
        .unlock(&bootstrap.profile, &bootstrap.header, &phrase()?)
        .map_err(|_| "SIMULATED unlock failed")?;
    let _ = declare_compaction_capability(&state.origin, &state.device_token)
        .await
        .map_err(|_| "SIMULATED compaction capability declaration failed")?;
    if bootstrap_state == DatabaseBootstrap::FirstStart {
        bootstrap_snapshot(&client, &state.origin, &state.device_token, device_id)
            .await
            .map_err(|error| runtime_error("bootstrap snapshot failed", error))?;
        finish_database_bootstrap(&dir, &state)?;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if tx.send(line.unwrap_or_default()).is_err() {
                break;
            }
        }
    });
    publish_capabilities(&state.origin, &state.device_token, &route.subscription_id)
        .await
        .map_err(|_| "SIMULATED capability publish failed")?;
    println!(
        "SIMULATED gateway running; JSON lines: incoming, pause, resume, outcome, crash-after-effect, quit"
    );
    let mut paused = false;
    loop {
        while let Ok(line) = rx.try_recv() {
            let value: Value =
                serde_json::from_str(&line).map_err(|_| "SIMULATED control must be JSON")?;
            match value["type"].as_str() {
                Some("incoming") => {
                    client
                        .capture_incoming(IncomingSms {
                            conversation_id: None,
                            sender_address: value["senderAddress"]
                                .as_str()
                                .ok_or("senderAddress required")?
                                .into(),
                            body: value["body"].as_str().ok_or("body required")?.into(),
                            provider_message_id: value["providerMessageId"]
                                .as_str()
                                .map(str::to_owned),
                            imported: value["imported"].as_bool().unwrap_or(false),
                        })
                        .map_err(|_| "SIMULATED capture failed")?;
                }
                Some("pause") => paused = true,
                Some("resume") => paused = false,
                Some("crash-after-effect") => {
                    let _ = run_one_carrier_effect_for_route(
                        &client,
                        &effects_path(&dir),
                        &route.subscription_id,
                        true,
                    )
                    .map_err(|_| "SIMULATED effect failed")?;
                    return Ok(());
                }
                Some("outcome") => {
                    if let Some(command) = client
                        .pending_commands()
                        .map_err(|_| "SIMULATED command lookup failed")?
                        .into_iter()
                        .next()
                        && matches!(
                            client
                                .begin_send_attempt(command.command_id)
                                .map_err(|_| "SIMULATED permit failed")?,
                            PermitDecision::Permit(_)
                        )
                    {
                        client
                            .record_send_result(
                                command.command_id,
                                if value["state"] == "failed_confirmed" {
                                    SendResult::FailedConfirmed
                                } else {
                                    SendResult::Sent
                                },
                            )
                            .map_err(|_| "SIMULATED outcome failed")?;
                    }
                }
                Some("quit") => return Ok(()),
                _ => return Err("unknown SIMULATED control".into()),
            }
        }
        if !paused {
            eprintln!("SIMULATED phase=sync");
            publish_capabilities(&state.origin, &state.device_token, &route.subscription_id)
                .await
                .map_err(|_| "SIMULATED capability publish failed")?;
            let _ = declare_compaction_capability(&state.origin, &state.device_token)
                .await
                .map_err(|_| "SIMULATED compaction capability declaration failed")?;
            sync_http(&client, &state.origin, &state.device_token)
                .await
                .map_err(|_| "SIMULATED sync failed")?;
            eprintln!("SIMULATED phase=media");
            drain_media(&client, &state.origin, &state.device_token, &dir)
                .await
                .map_err(|_| "SIMULATED media transfer failed")?;
            upload_pending(&client, &state.origin, &state.device_token)
                .await
                .map_err(|error| runtime_error("upload failed", error))?;
            let _ = run_one_carrier_effect_for_route(
                &client,
                &effects_path(&dir),
                &route.subscription_id,
                false,
            )
            .map_err(|_| "SIMULATED effect failed")?;
            upload_pending(&client, &state.origin, &state.device_token)
                .await
                .map_err(|error| runtime_error("upload failed", error))?;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::main]
async fn main() {
    let mut args = env::args().skip(1);
    let result: Result<(), String> = match args.next().as_deref() {
        Some("claim-owner") => match args.next() {
            Some(dir) => claim_owner(PathBuf::from(dir)).await,
            None => Err("STATE_DIR required".into()),
        },
        Some("admit-gateway") => match args.next() {
            Some(dir) => {
                admit(
                    PathBuf::from(dir),
                    ClientKind::Android,
                    "Simulated gateway",
                    CREDENTIAL_FILE,
                )
                .await
            }
            None => Err("STATE_DIR required".into()),
        },
        Some("admit-device") => match args.next() {
            Some(dir) => {
                admit(
                    PathBuf::from(dir),
                    ClientKind::Desktop,
                    "Simulated desktop",
                    DESKTOP_IMPORT_FILE,
                )
                .await
            }
            None => Err("STATE_DIR required".into()),
        },
        Some("run") => match args.next() {
            Some(d) => run(PathBuf::from(d)).await,
            None => Err("STATE_DIR required".into()),
        },
        _ => {
            usage();
            return;
        }
    };
    if let Err(error) = result {
        eprintln!("SIMULATED gateway failed: {error}");
        std::process::exit(2)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn developer_secret_file_is_private_at_creation() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("developer-key");
        protected(&path, b"secret").unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn lost_database_requires_fresh_pairing_without_touching_credentials_or_effects() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = SimulatorState {
            version: 1,
            origin: "http://127.0.0.1:12345".into(),
            vault_id: VaultId::new().0.to_string(),
            device_id: DeviceId::new().0.to_string(),
            device_token: "synthetic-token".into(),
        };
        let credentials = dir.path().join(CREDENTIAL_FILE);
        write_state(&credentials, &state).unwrap();
        let credential_bytes = fs::read(&credentials).unwrap();

        assert_eq!(
            prepare_database_bootstrap(dir.path(), &state).unwrap(),
            DatabaseBootstrap::FirstStart
        );
        let database = dir.path().join(DATABASE_FILE);
        assert!(database.exists());
        assert_eq!(
            fs::metadata(&database).unwrap().permissions().mode() & 0o777,
            0o600
        );
        finish_database_bootstrap(dir.path(), &state).unwrap();
        assert_eq!(
            prepare_database_bootstrap(dir.path(), &state).unwrap(),
            DatabaseBootstrap::Existing
        );

        let effects = effects_path(dir.path());
        protected(&effects, b"existing-effect\n").unwrap();
        let effect_bytes = fs::read(&effects).unwrap();
        fs::remove_file(&database).unwrap();

        let error = prepare_database_bootstrap(dir.path(), &state).unwrap_err();
        assert!(error.contains("fresh pairing required"));
        assert_eq!(fs::read(credentials).unwrap(), credential_bytes);
        assert_eq!(fs::read(effects).unwrap(), effect_bytes);
    }

    #[test]
    fn completion_path_uses_typed_enrollment_id_and_rejects_invalid_challenges() {
        let enrollment_id = "018f4c6f-78bd-7c8c-8000-000000000001";
        let challenge = format!(
            r#"{{"enrollment_id":"{enrollment_id}","origin":"https://localhost","vault_id":"018f4c6f-78bd-7c8c-8000-000000000002","profile":{{}},"fingerprint":"fingerprint","key_epoch":1,"device":{{"device_id":"018f4c6f-78bd-7c8c-8000-000000000003","kind":"android","name":"gateway","signing_public_key":"key"}},"requested_role":"gateway","nonce":"nonce","credential_digest":"digest","expires_in_seconds":300}}"#
        );

        assert_eq!(
            completion_path_for_challenge(&challenge).unwrap(),
            format!("/v1/enrollment/challenges/{enrollment_id}/complete")
        );
        assert!(completion_path_for_challenge("{}").is_err());
        assert!(completion_path_for_challenge("not JSON").is_err());
    }
}
