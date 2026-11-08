//! Private native MCP management bridge. Pairing material never reaches Tauri IPC.
use crate::{
    error::{BridgeError, BridgeResult},
    net::{NetError, MAX_JSON_BYTES},
    AppState,
};
use peppy_hosted_api::mcp::{
    McpAuthorizationDecision, McpAuthorizationView, McpDeviceStatus, McpEnrollmentPreparation,
    McpGrantView, McpHostActivationRequest, McpHostCompletionRequest, McpScope,
    McpWorkerActivateResponse, WebhookCreated, WebhookInput, WebhookList, WebhookSecret,
    WebhookTestResult, WebhookView,
};
use peppy_hosted_api::{
    computer_enrollment::{ComputerEnrollmentCreated, ComputerEnrollmentStatus},
    enrollment::AdmissionComplete,
    DeviceRosterResponse,
};
use peppy_hosted_client::newenrollment::{ClientKind, EnrollmentClient};
use serde::{de::DeserializeOwned, Serialize};
use std::str::FromStr;
use tauri::{State, WebviewWindow};
use uuid::Uuid;

const COMPUTER_ENROLLMENT_PATH: &str = "/v1/enrollment/computers";
// Webhook list responses can exceed the general JSON limit; the server bounds them at 2 MiB.
const MAX_WEBHOOK_LIST_BYTES: usize = 2 * 1024 * 1024;
// Server codes that the webhook settings UI can act on; anything else keeps the generic mapping.
const WEBHOOK_ERRORS: &[(&str, &str)] = &[
    ("webhook_url_invalid", "The webhook URL is invalid."),
    ("webhook_url_insecure", "Webhook URLs must use HTTPS."),
    (
        "webhook_url_blocked",
        "The webhook URL points to a blocked network address.",
    ),
    ("webhook_name_invalid", "The webhook name is invalid."),
    ("webhook_filter_invalid", "The webhook filter is invalid."),
    ("webhook_limit", "The webhook limit has been reached."),
    ("webhook_not_found", "This webhook no longer exists."),
    ("mcp_unavailable", "The MCP server client is unavailable."),
    (
        "mcp_device_required",
        "Enable the Peppy server client before managing webhooks.",
    ),
    (
        "mcp_worker_unavailable",
        "The Peppy server client is not responding.",
    ),
    (
        "mcp_worker_invalid",
        "The Peppy server client returned an invalid response.",
    ),
    ("owner_required", "Only an owner can manage webhooks."),
];

struct McpCommand<'a> {
    state: &'a AppState,
    _operation: crate::update_safety::OperationGuard,
}

impl<'a> McpCommand<'a> {
    fn admit(state: &'a AppState) -> BridgeResult<Self> {
        Ok(Self {
            state,
            _operation: crate::admit_update_operation(state)?,
        })
    }

    async fn session(&self) -> BridgeResult<std::sync::Arc<crate::session::Session>> {
        self.state.require_session_admitted(&self._operation).await
    }
}

fn current_account(
    hosted: &crate::hosted::HostedState,
    generation: Option<u64>,
) -> BridgeResult<()> {
    if let Some(generation) = generation {
        hosted.ensure_mcp_account_session_current(generation)?;
    }
    Ok(())
}

async fn current(
    state: &AppState,
    expected: &std::sync::Arc<crate::session::Session>,
) -> BridgeResult<()> {
    let current = state.require_session().await?;
    if current.binding != expected.binding || !std::sync::Arc::ptr_eq(&current, expected) {
        return Err(BridgeError::new(
            "session-changed",
            "The active session changed during MCP enablement.",
        ));
    }
    Ok(())
}

pub(crate) async fn mcp_account_post<T, B>(
    state: &AppState,
    session: &std::sync::Arc<crate::session::Session>,
    hosted: &crate::hosted::HostedState,
    account_revision: Option<u64>,
    path: &str,
    body: &B,
    account: Option<&str>,
) -> BridgeResult<T>
where
    T: DeserializeOwned,
    B: Serialize + ?Sized,
{
    current(state, session).await?;
    current_account(hosted, account_revision)?;
    let response = session
        .api
        .post_json_with_account(path, body, account)
        .await
        .map_err(BridgeError::from)?;
    current(state, session).await?;
    current_account(hosted, account_revision)?;
    Ok(response)
}

#[tauri::command]
pub async fn mcp_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<McpDeviceStatus> {
    crate::require_main(window.label())?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .get_json("/v1/mcp/device", MAX_JSON_BYTES)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn mcp_enable(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<McpDeviceStatus> {
    crate::require_main(window.label())?;
    let command = McpCommand::admit(&state)?;
    let session = command.session().await?;
    let binding = session.binding.clone();
    let roster: DeviceRosterResponse = session
        .api
        .get_json("/v1/devices", MAX_JSON_BYTES)
        .await
        .map_err(BridgeError::from)?;
    let owner = roster.devices.into_iter().any(|device| {
        device.device_id.to_string() == binding.device_id
            && device.role == "owner"
            && !device.revoked
    });
    if !owner {
        return Err(BridgeError::new(
            "owner_required",
            "Only an owner can enable MCP.",
        ));
    }
    let account = crate::enrollment::hosted_account_header(&state, &hosted).await?;
    let account_generation = match account.as_deref() {
        Some(bearer) => Some(hosted.capture_mcp_account_session(bearer).await?),
        None => None,
    };
    let prepared: McpEnrollmentPreparation = mcp_account_post(
        &state,
        &session,
        &hosted,
        account_generation,
        "/v1/mcp/device/prepare",
        &(),
        account.as_deref().map(String::as_str),
    )
    .await?;
    let vault =
        crate::sync::fetch_vault(&session.api, &binding.vault_id, &binding.device_id).await?;
    current(&state, &session).await?;
    current_account(&hosted, account_generation)?;
    let (profile, header) = crate::sync::vault_header(&vault)?;
    let mut owner_client = EnrollmentClient::for_existing_owner(
        session.api.origin().to_owned(),
        ClientKind::Desktop,
        "Peppy Desktop".into(),
        profile.clone(),
        header.clone(),
    )
    .map_err(|_| BridgeError::new("enrollment", "Could not prepare MCP enrollment."))?;
    let owner_device_id = Uuid::from_str(&binding.device_id).map_err(|_| {
        BridgeError::new(
            "credential-mismatch",
            "The active device identity is invalid.",
        )
    })?;
    owner_client
        .configure_computer_owner_device(owner_device_id)
        .map_err(|_| BridgeError::new("enrollment", "Could not prepare MCP enrollment."))?;
    let create: serde_json::Value = serde_json::from_str(
        &owner_client
            .scan_computer_qr(
                serde_json::to_string(&prepared.pairing).map_err(|_| BridgeError::host_state())?,
            )
            .map_err(|_| BridgeError::new("enrollment", "Could not prepare MCP enrollment."))?,
    )
    .map_err(|_| BridgeError::host_state())?;
    let invitation_id = prepared.pairing.invitation_id;
    let created: ComputerEnrollmentCreated = mcp_account_post(
        &state,
        &session,
        &hosted,
        account_generation,
        COMPUTER_ENROLLMENT_PATH,
        &create,
        account.as_deref().map(String::as_str),
    )
    .await?;
    owner_client
        .accept_computer_created(
            serde_json::to_string(&created).map_err(|_| BridgeError::host_state())?,
        )
        .map_err(|_| BridgeError::new("enrollment", "Could not prepare MCP enrollment."))?;
    let approval = match owner_client.approve_computer_enrollment_with_client(true, &session.client)
    {
        Ok(approval) => approval,
        Err(_) => {
            let secret = crate::enrollment::stored_vault_secret(
                &*state.store,
                &binding.vault_secret_account(),
            )?;
            let client = session.client.clone();
            crate::sync::blocking(move || {
                client
                    .unlock(&profile, &header, &secret)
                    .map_err(crate::error::core_error)
            })
            .await?;
            current(&state, &session).await?;
            current_account(&hosted, account_generation)?;
            owner_client
                .approve_computer_enrollment_with_client(true, &session.client)
                .map_err(|_| {
                    BridgeError::new(
                        "unlock-required",
                        "Unlock or re-approval is required before enabling MCP.",
                    )
                })?
        }
    };
    let approval = serde_json::from_str::<serde_json::Value>(&approval)
        .map_err(|_| BridgeError::host_state())?;
    let status: ComputerEnrollmentStatus = mcp_account_post(
        &state,
        &session,
        &hosted,
        account_generation,
        &format!("/v1/enrollment/computers/{invitation_id}/approve"),
        &approval,
        account.as_deref().map(String::as_str),
    )
    .await?;
    let activation: McpWorkerActivateResponse = mcp_account_post(
        &state,
        &session,
        &hosted,
        account_generation,
        "/v1/mcp/device/activate",
        &McpHostActivationRequest {
            operation_id: prepared.operation_id,
            status: status.clone(),
        },
        account.as_deref().map(String::as_str),
    )
    .await?;
    let admission: AdmissionComplete = mcp_account_post(
        &state,
        &session,
        &hosted,
        account_generation,
        &format!("/v1/enrollment/computers/{invitation_id}/confirm"),
        &activation.confirm,
        account.as_deref().map(String::as_str),
    )
    .await?;
    let result = mcp_account_post(
        &state,
        &session,
        &hosted,
        account_generation,
        "/v1/mcp/device/complete",
        &McpHostCompletionRequest {
            operation_id: prepared.operation_id,
            admission: admission.clone(),
        },
        account.as_deref().map(String::as_str),
    )
    .await?;
    Ok(result)
}

#[tauri::command]
pub async fn mcp_grants(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<Vec<McpGrantView>> {
    crate::require_main(window.label())?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .get_json("/v1/mcp/grants", MAX_JSON_BYTES)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn mcp_revoke_grant(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    let id = grant_id(&id)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .post_discard(&format!("/v1/mcp/grants/{id}/revoke"), &())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn webhooks_list(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<WebhookList> {
    crate::require_main(window.label())?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .get_json("/v1/mcp/webhooks", MAX_WEBHOOK_LIST_BYTES)
        .await
        .map_err(webhook_error)
}

#[tauri::command]
pub async fn webhook_create(
    window: WebviewWindow,
    state: State<'_, AppState>,
    input: WebhookInput,
) -> BridgeResult<WebhookCreated> {
    crate::require_main(window.label())?;
    validate_webhook_input(&input)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .post_json("/v1/mcp/webhooks", &input)
        .await
        .map_err(webhook_error)
}

#[tauri::command]
pub async fn webhook_update(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
    input: WebhookInput,
) -> BridgeResult<WebhookView> {
    crate::require_main(window.label())?;
    let id = webhook_id(&id)?;
    validate_webhook_input(&input)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .put_json(&format!("/v1/mcp/webhooks/{id}"), &input)
        .await
        .map_err(webhook_error)
}

#[tauri::command]
pub async fn webhook_delete(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    let id = webhook_id(&id)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .delete_discard(&format!("/v1/mcp/webhooks/{id}"))
        .await
        .map_err(webhook_error)
}

#[tauri::command]
pub async fn webhook_reveal_secret(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<WebhookSecret> {
    crate::require_main(window.label())?;
    let id = webhook_id(&id)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .post_empty(&format!("/v1/mcp/webhooks/{id}/secret"))
        .await
        .map_err(webhook_error)
}

#[tauri::command]
pub async fn webhook_rotate_secret(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<WebhookSecret> {
    crate::require_main(window.label())?;
    let id = webhook_id(&id)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .post_empty(&format!("/v1/mcp/webhooks/{id}/rotate-secret"))
        .await
        .map_err(webhook_error)
}

#[tauri::command]
pub async fn webhook_test(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<WebhookTestResult> {
    crate::require_main(window.label())?;
    let id = webhook_id(&id)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .post_empty(&format!("/v1/mcp/webhooks/{id}/test"))
        .await
        .map_err(webhook_error)
}

#[tauri::command]
pub async fn mcp_authorization(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<McpAuthorizationView> {
    crate::require_main(window.label())?;
    let id = authorization_id(&id)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .get_json(&format!("/v1/mcp/authorizations/{id}"), MAX_JSON_BYTES)
        .await
        .map_err(Into::into)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationScopes<'a> {
    scopes: &'a [McpScope],
}

#[tauri::command]
pub async fn mcp_approve_authorization(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
    scopes: Vec<McpScope>,
) -> BridgeResult<McpAuthorizationDecision> {
    crate::require_main(window.label())?;
    let id = authorization_id(&id)?;
    validate_scopes(&scopes)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .post_json(
            &format!("/v1/mcp/authorizations/{id}/approve"),
            &AuthorizationScopes { scopes: &scopes },
        )
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn mcp_deny_authorization(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<McpAuthorizationDecision> {
    crate::require_main(window.label())?;
    let id = authorization_id(&id)?;
    let command = McpCommand::admit(&state)?;
    command
        .session()
        .await?
        .api
        .post_empty(&format!("/v1/mcp/authorizations/{id}/deny"))
        .await
        .map_err(Into::into)
}

fn grant_id(value: &str) -> BridgeResult<Uuid> {
    Uuid::from_str(value)
        .ok()
        .filter(|id| id.to_string() == value)
        .ok_or_else(|| BridgeError::new("invalid-request", "The MCP identifier is invalid."))
}

fn webhook_id(value: &str) -> BridgeResult<Uuid> {
    grant_id(value)
}

fn webhook_error(error: NetError) -> BridgeError {
    match error {
        NetError::Status {
            status,
            code: Some(code),
        } => match WEBHOOK_ERRORS.iter().find(|(known, _)| *known == code) {
            Some(&(known, message)) => BridgeError::new(known, message),
            None => BridgeError::from(NetError::Status {
                status,
                code: Some(code),
            }),
        },
        error => error.into(),
    }
}

fn validate_webhook_input(input: &WebhookInput) -> BridgeResult<()> {
    if serde_json::to_vec(input)
        .map_err(|_| BridgeError::new("invalid-request", "The webhook input is invalid."))?
        .len()
        > 64 * 1024
    {
        return Err(BridgeError::new(
            "invalid-request",
            "The webhook input is too large.",
        ));
    }
    Ok(())
}

fn authorization_id(value: &str) -> BridgeResult<&str> {
    let bytes = value.as_bytes();
    if bytes.len() == 43
        && bytes[..42]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        // A 32-byte unpadded base64url value has 43 characters. Its final sextet has two
        // unused bits and four significant bits, giving sixteen canonical encodings.
        && matches!(
            bytes[42],
            b'A' | b'E'
                | b'I'
                | b'M'
                | b'Q'
                | b'U'
                | b'Y'
                | b'c'
                | b'g'
                | b'k'
                | b'o'
                | b's'
                | b'w'
                | b'0'
                | b'4'
                | b'8'
        )
    {
        return Ok(value);
    }
    Err(BridgeError::new(
        "invalid-request",
        "The MCP authorization identifier is invalid.",
    ))
}

fn validate_scopes(scopes: &[McpScope]) -> BridgeResult<()> {
    if scopes.is_empty()
        || scopes.len() > 3
        || scopes
            .iter()
            .enumerate()
            .any(|(index, scope)| scopes[..index].contains(scope))
    {
        return Err(BridgeError::new(
            "invalid-request",
            "MCP scopes are invalid.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_scope_validation_rejects_empty_and_duplicate_scopes() {
        assert_eq!(validate_scopes(&[]).unwrap_err().code, "invalid-request");
        assert_eq!(
            validate_scopes(&[McpScope::ConversationsRead, McpScope::ConversationsRead])
                .unwrap_err()
                .code,
            "invalid-request"
        );
        assert!(validate_scopes(&[McpScope::ConversationsRead, McpScope::MessagesSend]).is_ok());
    }

    #[test]
    fn authorization_scope_wire_rejects_unknown_values() {
        assert!(serde_json::from_str::<Vec<McpScope>>(r#"["unknown:scope"]"#).is_err());
    }

    #[test]
    fn grant_ids_require_canonical_uuid_values() {
        assert!(grant_id("not-a-uuid").is_err());
        assert!(grant_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(grant_id("550E8400-E29B-41D4-A716-446655440000").is_err());
    }

    #[test]
    fn authorization_ids_require_canonical_opaque_base64url_values() {
        for final_char in "AEIMQUYcgkosw048".chars() {
            let authorization = format!("{}{}", "A".repeat(42), final_char);
            assert_eq!(authorization_id(&authorization).unwrap(), authorization);
        }
        for invalid in [
            "short",
            &format!("{}.", "A".repeat(42)),
            &format!("{}A/", "A".repeat(41)),
            &format!("{}Q", "A".repeat(43)),
            &format!("{}B", "A".repeat(42)),
        ] {
            assert!(authorization_id(invalid).is_err(), "{invalid}");
        }
    }

    fn webhook_status(code: &str) -> NetError {
        NetError::Status {
            status: 422,
            code: Some(code.into()),
        }
    }

    #[test]
    fn webhook_errors_preserve_each_known_server_code() {
        for code in [
            "webhook_url_invalid",
            "webhook_url_insecure",
            "webhook_url_blocked",
            "webhook_name_invalid",
            "webhook_filter_invalid",
            "webhook_limit",
            "webhook_not_found",
            "mcp_unavailable",
            "mcp_device_required",
            "mcp_worker_unavailable",
            "mcp_worker_invalid",
            "owner_required",
        ] {
            let mapped = webhook_error(webhook_status(code));
            assert_eq!(mapped.code, code);
            assert!(!mapped.message.is_empty());
            assert!(!mapped.message.contains(code));
        }
    }

    #[test]
    fn webhook_errors_map_unknown_codes_to_the_generic_rejection() {
        let mapped = webhook_error(webhook_status("webhook_internal_detail"));
        assert_eq!(mapped.code, "server-rejected");
    }

    #[test]
    fn webhook_errors_keep_statusless_and_transport_errors_generic() {
        assert_eq!(
            webhook_error(NetError::Status {
                status: 500,
                code: None,
            })
            .code,
            "server-rejected"
        );
        assert_eq!(webhook_error(NetError::Revoked).code, "revoked");
        assert_eq!(webhook_error(NetError::Offline).code, "offline");
        assert_eq!(webhook_error(NetError::TooLarge).code, "response-too-large");
    }

    #[test]
    fn computer_creation_uses_the_collection_route() {
        assert_eq!(COMPUTER_ENROLLMENT_PATH, "/v1/enrollment/computers");
    }

    #[tokio::test]
    async fn admitted_command_blocks_restart_across_an_await() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            root.path().to_path_buf(),
            std::sync::Arc::new(crate::secure_store::MemoryStore::default()),
            std::sync::Arc::new(|| {}),
        );
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let resume = std::sync::Arc::new(tokio::sync::Notify::new());
        let command_started = started.clone();
        let command_resume = resume.clone();
        let command = McpCommand::admit(&state).unwrap();
        let request = tokio::spawn(async move {
            command_started.notify_one();
            command_resume.notified().await;
            Err::<(), _>(BridgeError::new(
                "fake-transport",
                "The fake request failed.",
            ))
        });

        started.notified().await;
        let restart_gate = state.update_safety.clone();
        let restart = tokio::spawn(async move { restart_gate.reserve_async().await });
        tokio::task::yield_now().await;
        assert!(!restart.is_finished());

        resume.notify_one();
        assert_eq!(request.await.unwrap().unwrap_err().code, "fake-transport");
        drop(command);
        drop(restart.await.unwrap().unwrap());
    }

    async fn delayed_account_effect(
        path: &'static str,
        replace_account: bool,
    ) -> (
        BridgeError,
        Vec<String>,
        std::sync::Arc<crate::session::Session>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let reached = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let server_reached = reached.clone();
        let server_release = release.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            server_reached.notify_one();
            server_release.notified().await;
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await.unwrap();
            String::from_utf8(buffer[..read].to_vec()).unwrap()
        });
        let fixture = crate::tests::fixture_at(&format!("http://{address}"));
        let session = std::sync::Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let root = tempfile::tempdir().unwrap();
        let state = std::sync::Arc::new(AppState::new(
            root.path().to_path_buf(),
            std::sync::Arc::new(crate::secure_store::MemoryStore::default()),
            std::sync::Arc::new(|| {}),
        ));
        state
            .update_config(|config| {
                config.origin = Some(fixture.binding.origin.clone());
                config.remember(&fixture.binding);
                config.active = Some(fixture.binding.clone());
            })
            .unwrap();
        *state.session.lock().await = Some(session.clone());
        let store = crate::secure_store::MemoryStore::default();
        let hosted = std::sync::Arc::new(crate::hosted::HostedState::default());
        let generation = hosted.begin_sign_in_attempt();
        crate::hosted::commit_sign_in_session(
            &store,
            &hosted,
            "https://peppy.test",
            generation,
            crate::hosted::HostedSessionRecord {
                account_id: "550e8400-e29b-41d4-a716-446655440000".into(),
                bearer: zeroize::Zeroizing::new("account-a".into()),
                expires_at_unix: u64::MAX,
            },
        )
        .await
        .unwrap();
        let revision = hosted
            .capture_mcp_account_session("account-a")
            .await
            .unwrap();
        let request_state = state.clone();
        let request_session = session.clone();
        let request_hosted = hosted.clone();
        let request = tokio::spawn(async move {
            mcp_account_post::<serde_json::Value, _>(
                &request_state,
                &request_session,
                &request_hosted,
                Some(revision),
                path,
                &(),
                Some("account-a"),
            )
            .await
            .unwrap_err()
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), reached.notified())
            .await
            .expect("the production MCP account request should reach the fake server");
        if replace_account {
            let generation = hosted.begin_sign_in_attempt();
            crate::hosted::commit_sign_in_session(
                &store,
                &hosted,
                "https://peppy.test",
                generation,
                crate::hosted::HostedSessionRecord {
                    account_id: "550e8400-e29b-41d4-a716-446655440001".into(),
                    bearer: zeroize::Zeroizing::new("account-b".into()),
                    expires_at_unix: u64::MAX,
                },
            )
            .await
            .unwrap();
        } else {
            crate::hosted::tombstone_session(&store, &hosted, "https://peppy.test")
                .await
                .unwrap();
        }
        release.notify_one();
        let request = request.await.unwrap();
        let received = server.await.unwrap();
        (request, vec![received], session)
    }

    #[tokio::test]
    async fn pending_sign_in_then_committed_account_replacement_aborts_mcp_effects() {
        let (error, requests, _) = delayed_account_effect("/v1/mcp/device/prepare", true).await;

        assert_eq!(error.code, "hosted-account-changed");
        assert!(requests[0].starts_with("POST /v1/mcp/device/prepare HTTP/1.1\r\n"));
        assert!(!requests[0].contains("/v1/enrollment/computers"));
    }

    #[tokio::test]
    async fn sign_out_during_final_complete_aborts_without_affecting_device_session() {
        let (error, requests, session) =
            delayed_account_effect("/v1/mcp/device/complete", false).await;

        assert_eq!(error.code, "hosted-account-changed");
        assert!(requests[0].starts_with("POST /v1/mcp/device/complete HTTP/1.1\r\n"));
        assert!(session.client.pending_outbox().is_ok());
    }

    #[tokio::test]
    async fn computer_creation_posts_to_the_collection_http_route() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await
                .unwrap();
            String::from_utf8(buffer[..read].to_vec()).unwrap()
        });
        let api = crate::net::Api::new(&format!("http://{address}"), "credential").unwrap();

        let _: serde_json::Value = api
            .post_json_with_account(
                COMPUTER_ENROLLMENT_PATH,
                &serde_json::json!({"claim": "private"}),
                Some("account"),
            )
            .await
            .unwrap();
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /v1/enrollment/computers HTTP/1.1\r\n"));
        assert!(!request.contains("/v1/enrollment/computers/"));
        assert!(
            request.contains("peppy-account-session: account")
                || request.contains("Peppy-Account-Session: account")
        );
    }
}
