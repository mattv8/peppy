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
    WebhookTestResult, WebhookView, MCP_COMPLETE_REQUEST_TIMEOUT_MS,
};
use peppy_hosted_api::{
    computer_enrollment::{ComputerEnrollmentCreated, ComputerEnrollmentStatus},
    enrollment::AdmissionComplete,
    DeviceRosterResponse,
};
use peppy_hosted_client::newenrollment::{ClientKind, EnrollmentClient};
use serde::{de::DeserializeOwned, Serialize};
use std::{str::FromStr, time::Duration};
use tauri::{State, WebviewWindow};
use uuid::Uuid;
use zeroize::Zeroizing;

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
        "Enable Peppy MCP before managing webhooks.",
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

fn account_session_rejected(error: &NetError) -> Option<(u16, &str)> {
    match error {
        NetError::Status {
            status,
            code: Some(code),
        } if matches!(*status, 401 | 403)
            && matches!(
                code.as_str(),
                "mcp_enrollment_unauthorized" | "enrollment_unauthorized"
            ) =>
        {
            Some((*status, code))
        }
        _ => None,
    }
}

fn hosted_session_required(path: &str, status: u16, code: &str) -> BridgeError {
    let path = path.split('?').next().unwrap_or(path);
    BridgeError::new(
        "hosted-session-required",
        format!(
            "Sign in again to Peppy Hosted before enrolling (POST {path}, HTTP {status}, {code})."
        ),
    )
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

async fn mcp_account_session(
    state: &AppState,
    hosted: &crate::hosted::HostedState,
    origin: &str,
) -> BridgeResult<Option<Zeroizing<String>>> {
    if origin != crate::hosted::hosted_origin()? {
        return Ok(None);
    }
    crate::hosted::enrollment_account_session(state, hosted, origin)
        .await
        .map(Some)
}

struct McpAccountAuthorization {
    account: Option<Zeroizing<String>>,
    revision: Option<u64>,
}

struct McpAccountPostOptions<'a> {
    account: Option<&'a str>,
    timeout: Option<Duration>,
}

enum McpAccountPostError {
    Bridge(BridgeError),
    Network(NetError),
}

impl McpAccountPostError {
    fn into_bridge(self) -> BridgeError {
        match self {
            Self::Bridge(error) => error,
            Self::Network(error) => error.into(),
        }
    }
}

async fn begin_mcp_account_post<T, B>(
    state: &AppState,
    session: &std::sync::Arc<crate::session::Session>,
    hosted: &crate::hosted::HostedState,
    path: &str,
    body: &B,
) -> BridgeResult<(T, McpAccountAuthorization)>
where
    T: DeserializeOwned,
    B: Serialize + ?Sized,
{
    let account = mcp_account_session(state, hosted, session.api.origin()).await?;
    let revision = match account.as_deref() {
        Some(bearer) => Some(hosted.capture_mcp_account_session(bearer).await?),
        None => None,
    };
    let response = mcp_account_post(
        state,
        session,
        hosted,
        revision,
        path,
        body,
        account.as_deref().map(String::as_str),
    )
    .await?;
    Ok((response, McpAccountAuthorization { account, revision }))
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
    mcp_account_post_with_timeout(
        state,
        session,
        hosted,
        account_revision,
        path,
        body,
        McpAccountPostOptions {
            account,
            timeout: None,
        },
    )
    .await
    .map_err(McpAccountPostError::into_bridge)
}

async fn mcp_account_post_with_timeout<T, B>(
    state: &AppState,
    session: &std::sync::Arc<crate::session::Session>,
    hosted: &crate::hosted::HostedState,
    account_revision: Option<u64>,
    path: &str,
    body: &B,
    options: McpAccountPostOptions<'_>,
) -> Result<T, McpAccountPostError>
where
    T: DeserializeOwned,
    B: Serialize + ?Sized,
{
    current(state, session)
        .await
        .map_err(McpAccountPostError::Bridge)?;
    current_account(hosted, account_revision).map_err(McpAccountPostError::Bridge)?;
    let response = match session
        .api
        .post_json_with_account_timeout(path, body, options.account, options.timeout)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            if options.account.is_none() {
                return Err(McpAccountPostError::Network(error));
            }
            let Some((status, code)) = account_session_rejected(&error) else {
                return Err(McpAccountPostError::Network(error));
            };
            crate::hosted::force_renew_mcp_account_session(state, hosted, session.api.origin())
                .await
                .map_err(|_| {
                    McpAccountPostError::Bridge(hosted_session_required(path, status, code))
                })?;
            current(state, session)
                .await
                .map_err(McpAccountPostError::Bridge)?;
            options
                .account
                .map(|bearer| hosted.capture_mcp_account_session(bearer))
                .expect("account rejections require an account session")
                .await
                .map_err(McpAccountPostError::Bridge)?;
            current_account(hosted, account_revision).map_err(McpAccountPostError::Bridge)?;
            match session
                .api
                .post_json_with_account_timeout(path, body, options.account, options.timeout)
                .await
            {
                Ok(response) => response,
                Err(error) => match account_session_rejected(&error) {
                    Some((status, code)) => {
                        return Err(McpAccountPostError::Bridge(hosted_session_required(
                            path, status, code,
                        )))
                    }
                    None => return Err(McpAccountPostError::Network(error)),
                },
            }
        }
    };
    current(state, session)
        .await
        .map_err(McpAccountPostError::Bridge)?;
    current_account(hosted, account_revision).map_err(McpAccountPostError::Bridge)?;
    Ok(response)
}

fn ambiguous_mcp_complete(error: &NetError) -> bool {
    matches!(
        error,
        NetError::Offline | NetError::Status { status: 504, .. }
    ) || error.is_status(503, "mcp_worker_unavailable")
}

async fn complete_mcp_enable(
    state: &AppState,
    session: &std::sync::Arc<crate::session::Session>,
    hosted: &crate::hosted::HostedState,
    account_revision: Option<u64>,
    account: Option<&str>,
    request: &McpHostCompletionRequest,
    timeout: Duration,
) -> BridgeResult<McpDeviceStatus> {
    for attempt in 0..2 {
        match mcp_account_post_with_timeout(
            state,
            session,
            hosted,
            account_revision,
            "/v1/mcp/device/complete",
            request,
            McpAccountPostOptions {
                account,
                timeout: Some(timeout),
            },
        )
        .await
        {
            Ok(response) => return Ok(response),
            Err(McpAccountPostError::Network(error))
                if ambiguous_mcp_complete(&error) && attempt == 0 => {}
            Err(McpAccountPostError::Network(error)) if ambiguous_mcp_complete(&error) => {
                return Err(BridgeError::new(
                    "mcp-enable-unconfirmed",
                    "Peppy MCP setup is still finishing. Its status will update here.",
                ));
            }
            Err(error) => return Err(error.into_bridge()),
        }
    }
    unreachable!("MCP completion retries exactly once")
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
    let (prepared, authorization): (McpEnrollmentPreparation, _) =
        begin_mcp_account_post(&state, &session, &hosted, "/v1/mcp/device/prepare", &()).await?;
    let account = authorization.account;
    let account_generation = authorization.revision;
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
    let result = complete_mcp_enable(
        &state,
        &session,
        &hosted,
        account_generation,
        account.as_deref().map(String::as_str),
        &McpHostCompletionRequest {
            operation_id: prepared.operation_id,
            admission: admission.clone(),
        },
        Duration::from_millis(MCP_COMPLETE_REQUEST_TIMEOUT_MS),
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
    async fn mcp_account_session_uses_the_captured_hosted_origin() {
        use crate::secure_store::SecretStore;

        let hosted_origin = crate::hosted::hosted_origin().unwrap();
        let store = std::sync::Arc::new(crate::secure_store::MemoryStore::default());
        store
            .set(
                &format!("hosted-session:{hosted_origin}"),
                br#"{"account_id":"account","bearer":"account-session","expires_at_unix":18446744073709551615}"#,
            )
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(root.path().to_path_buf(), store, std::sync::Arc::new(|| {}));
        let hosted = crate::hosted::HostedState::default();

        let account = mcp_account_session(&state, &hosted, &hosted_origin)
            .await
            .unwrap();
        assert_eq!(
            account.as_deref().map(String::as_str),
            Some("account-session")
        );
        assert!(
            mcp_account_session(&state, &hosted, "https://community.example")
                .await
                .unwrap()
                .is_none()
        );

        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            root.path().to_path_buf(),
            std::sync::Arc::new(crate::secure_store::MemoryStore::default()),
            std::sync::Arc::new(|| {}),
        );
        assert_eq!(
            mcp_account_session(
                &state,
                &crate::hosted::HostedState::default(),
                &hosted_origin
            )
            .await
            .unwrap_err()
            .code,
            "hosted-session-required"
        );
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

    async fn mcp_account_fixture(
        origin: &str,
    ) -> (
        AppState,
        std::sync::Arc<crate::session::Session>,
        crate::hosted::HostedState,
        std::sync::Arc<crate::secure_store::MemoryStore>,
    ) {
        let fixture = crate::tests::fixture_at(origin);
        let session = std::sync::Arc::new(crate::tests::open(&fixture, &fixture.binding, &[]));
        let store = std::sync::Arc::new(crate::secure_store::MemoryStore::default());
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(root.keep(), store.clone(), std::sync::Arc::new(|| {}));
        state
            .update_config(|config| {
                config.origin = Some(fixture.binding.origin.clone());
                config.remember(&fixture.binding);
                config.active = Some(fixture.binding.clone());
            })
            .unwrap();
        *state.session.lock().await = Some(session.clone());
        let hosted = crate::hosted::HostedState::default();
        let generation = hosted.begin_sign_in_attempt();
        crate::hosted::commit_sign_in_session(
            &*store,
            &hosted,
            origin,
            generation,
            crate::hosted::HostedSessionRecord {
                account_id: "550e8400-e29b-41d4-a716-446655440000".into(),
                bearer: Zeroizing::new("account".into()),
                expires_at_unix: u64::MAX,
            },
        )
        .await
        .unwrap();
        (state, session, hosted, store)
    }

    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn serve_mcp_responses(
        responses: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 4096];
                let read = socket.read(&mut request).await.unwrap();
                requests.push(String::from_utf8(request[..read].to_vec()).unwrap());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (origin, server)
    }

    async fn serve_delayed_mcp_responses(
        responses: Vec<(Duration, String)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            for (delay, response) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.spawn(async move {
                    let mut request = vec![0; 4096];
                    let read = socket.read(&mut request).await.unwrap();
                    tokio::time::sleep(delay).await;
                    let _ = socket.write_all(response.as_bytes()).await;
                    String::from_utf8(request[..read].to_vec()).unwrap()
                });
            }
            let mut received = Vec::new();
            while let Some(request) = requests.join_next().await {
                received.push(request.unwrap());
            }
            received
        });
        (origin, server)
    }

    fn complete_request() -> McpHostCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "operation_id": "550e8400-e29b-41d4-a716-446655440000",
            "admission": {
                "vault_id": "550e8400-e29b-41d4-a716-446655440001",
                "device_id": "550e8400-e29b-41d4-a716-446655440002",
                "role": "device",
                "material": {
                    "profile": {},
                    "check_header": "header",
                    "fingerprint": "fingerprint",
                    "key_epoch": 1,
                    "admission_public_key": "key"
                }
            }
        }))
        .unwrap()
    }

    fn mcp_device_status() -> &'static str {
        r#"{"vaultId":"550e8400-e29b-41d4-a716-446655440001","available":true,"serverOrigin":"https://peppy.example","hosted":true,"device":null}"#
    }

    async fn complete_with_timeout(
        origin: &str,
        timeout: Duration,
    ) -> BridgeResult<McpDeviceStatus> {
        let (state, session, hosted, _) = mcp_account_fixture(origin).await;
        let revision = hosted.capture_mcp_account_session("account").await?;
        complete_mcp_enable(
            &state,
            &session,
            &hosted,
            Some(revision),
            Some("account"),
            &complete_request(),
            timeout,
        )
        .await
    }

    #[tokio::test]
    async fn complete_timeout_replays_the_identical_request() {
        let (origin, server) = serve_delayed_mcp_responses(vec![
            (
                Duration::from_millis(50),
                http_response("200 OK", mcp_device_status()),
            ),
            (Duration::ZERO, http_response("200 OK", mcp_device_status())),
        ])
        .await;

        assert!(complete_with_timeout(&origin, Duration::from_millis(10))
            .await
            .is_ok());
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].split_once("\r\n\r\n").unwrap().1,
            requests[1].split_once("\r\n\r\n").unwrap().1,
        );
    }

    #[tokio::test]
    async fn complete_worker_unavailable_replays_once() {
        let (origin, server) = serve_mcp_responses(vec![
            http_response(
                "503 Service Unavailable",
                r#"{"code":"mcp_worker_unavailable"}"#,
            ),
            http_response("200 OK", mcp_device_status()),
        ])
        .await;

        assert!(complete_with_timeout(&origin, Duration::from_secs(1))
            .await
            .is_ok());
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn complete_gateway_timeout_replays_the_identical_request() {
        let (origin, server) = serve_mcp_responses(vec![
            http_response("504 Gateway Timeout", ""),
            http_response("200 OK", mcp_device_status()),
        ])
        .await;

        assert!(complete_with_timeout(&origin, Duration::from_secs(1))
            .await
            .is_ok());
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].split_once("\r\n\r\n").unwrap().1,
            requests[1].split_once("\r\n\r\n").unwrap().1,
        );
    }

    #[tokio::test]
    async fn complete_two_timeouts_are_unconfirmed() {
        let (origin, server) = serve_delayed_mcp_responses(vec![
            (
                Duration::from_millis(50),
                http_response("200 OK", mcp_device_status()),
            ),
            (
                Duration::from_millis(50),
                http_response("200 OK", mcp_device_status()),
            ),
        ])
        .await;

        let error = complete_with_timeout(&origin, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error.code, "mcp-enable-unconfirmed");
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn complete_operation_invalid_is_not_replayed() {
        let (origin, server) = serve_mcp_responses(vec![http_response(
            "409 Conflict",
            r#"{"code":"mcp_operation_invalid"}"#,
        )])
        .await;

        let error = complete_with_timeout(&origin, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.code, "server-rejected");
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn complete_worker_rejected_is_not_replayed() {
        let (origin, server) = serve_mcp_responses(vec![http_response(
            "502 Bad Gateway",
            r#"{"code":"mcp_worker_rejected"}"#,
        )])
        .await;

        let error = complete_with_timeout(&origin, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.code, "server-rejected");
        assert_eq!(server.await.unwrap().len(), 1);
    }

    async fn rejected_mcp_post(
        origin: &str,
    ) -> (
        BridgeError,
        std::sync::Arc<crate::secure_store::MemoryStore>,
    ) {
        let (state, session, hosted, store) = mcp_account_fixture(origin).await;
        let revision = hosted.capture_mcp_account_session("account").await.unwrap();
        let error = mcp_account_post::<serde_json::Value, _>(
            &state,
            &session,
            &hosted,
            Some(revision),
            "/v1/mcp/device/prepare",
            &(),
            Some("account"),
        )
        .await
        .unwrap_err();
        (error, store)
    }

    #[tokio::test]
    async fn account_rejection_forces_one_renewal_and_retries_once() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let responses = [
                b"HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: 38\r\nconnection: close\r\n\r\n{\"code\":\"mcp_enrollment_unauthorized\"}".as_slice(),
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 82\r\nconnection: close\r\n\r\n{\"account_id\":\"550e8400-e29b-41d4-a716-446655440000\",\"expires_in_seconds\":2592000}".as_slice(),
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".as_slice(),
            ];
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 4096];
                let read = socket.read(&mut request).await.unwrap();
                requests.push(String::from_utf8(request[..read].to_vec()).unwrap());
                socket.write_all(response).await.unwrap();
            }
            requests
        });
        let (state, session, hosted, _) = mcp_account_fixture(&origin).await;
        let revision = hosted.capture_mcp_account_session("account").await.unwrap();

        let response: serde_json::Value = mcp_account_post(
            &state,
            &session,
            &hosted,
            Some(revision),
            "/v1/mcp/device/prepare",
            &(),
            Some("account"),
        )
        .await
        .unwrap();
        assert_eq!(response, serde_json::json!({}));
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("POST /v1/mcp/device/prepare "));
        assert!(requests[1].starts_with("POST /hosted/v1/auth/session/renew "));
        assert!(requests[2].starts_with("POST /v1/mcp/device/prepare "));
    }

    #[tokio::test]
    async fn second_account_rejection_stops_after_one_retry() {
        let rejected = r#"{"code":"mcp_enrollment_unauthorized"}"#;
        let renewed =
            r#"{"account_id":"550e8400-e29b-41d4-a716-446655440000","expires_in_seconds":2592000}"#;
        let (origin, server) = serve_mcp_responses(vec![
            http_response("401 Unauthorized", rejected),
            http_response("200 OK", renewed),
            http_response("403 Forbidden", r#"{"code":"enrollment_unauthorized"}"#),
        ])
        .await;

        let (error, _) = rejected_mcp_post(&origin).await;
        assert_eq!(error.code, "hosted-session-required");
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/mcp/device/prepare "))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /hosted/v1/auth/session/renew "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn renewal_401_tombstones_the_account_without_retrying_enrollment() {
        use crate::secure_store::SecretStore;

        let (origin, server) = serve_mcp_responses(vec![
            http_response(
                "401 Unauthorized",
                r#"{"code":"mcp_enrollment_unauthorized"}"#,
            ),
            http_response("401 Unauthorized", ""),
        ])
        .await;

        let (error, store) = rejected_mcp_post(&origin).await;
        assert_eq!(error.code, "hosted-session-required");
        assert!(store
            .get(&format!("hosted-session:{origin}"))
            .unwrap()
            .is_some_and(|session| session.is_empty()));
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /v1/mcp/device/prepare "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn forbidden_account_rejection_retries_but_invalid_bearer_remains_revoked() {
        let renewed =
            r#"{"account_id":"550e8400-e29b-41d4-a716-446655440000","expires_in_seconds":2592000}"#;
        let (origin, server) = serve_mcp_responses(vec![
            http_response("403 Forbidden", r#"{"code":"enrollment_unauthorized"}"#),
            http_response("200 OK", renewed),
            http_response("401 Unauthorized", r#"{"code":"invalid_bearer"}"#),
        ])
        .await;

        let (error, _) = rejected_mcp_post(&origin).await;
        assert_eq!(error.code, "revoked");
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 3);
    }

    #[tokio::test]
    async fn switched_mcp_post_selects_account_for_the_captured_destination() {
        use crate::secure_store::SecretStore;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let community_origin = format!("http://{}", listener.local_addr().unwrap());
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
        let community_fixture = crate::tests::fixture_at(&community_origin);
        let community_session = std::sync::Arc::new(crate::tests::open(
            &community_fixture,
            &community_fixture.binding,
            &[],
        ));
        let hosted_origin = crate::hosted::hosted_origin().unwrap();
        let hosted_fixture = crate::tests::fixture_at(&hosted_origin);
        let hosted_session = std::sync::Arc::new(crate::tests::open(
            &hosted_fixture,
            &hosted_fixture.binding,
            &[],
        ));
        let store = std::sync::Arc::new(crate::secure_store::MemoryStore::default());
        store
            .set(
                &format!("hosted-session:{hosted_origin}"),
                br#"{"account_id":"account","bearer":"account-session","expires_at_unix":18446744073709551615}"#,
            )
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(root.path().to_path_buf(), store, std::sync::Arc::new(|| {}));
        let hosted = crate::hosted::HostedState::default();

        state
            .update_config(|config| {
                config.origin = Some(community_fixture.binding.origin.clone());
                config.remember(&community_fixture.binding);
                config.active = Some(community_fixture.binding.clone());
            })
            .unwrap();
        *state.session.lock().await = Some(community_session.clone());
        let captured = state.require_session().await.unwrap();

        state
            .update_config(|config| {
                config.origin = Some(hosted_fixture.binding.origin.clone());
                config.remember(&hosted_fixture.binding);
                config.active = Some(hosted_fixture.binding.clone());
            })
            .unwrap();
        *state.session.lock().await = Some(hosted_session);

        let error = match begin_mcp_account_post::<serde_json::Value, _>(
            &state,
            &captured,
            &hosted,
            "/v1/mcp/device/prepare",
            &(),
        )
        .await
        {
            Ok(_) => panic!("the captured community session must reject a switch to Hosted"),
            Err(error) => error,
        };
        assert_eq!(error.code, "session-changed");
        assert_eq!(
            error.message,
            "The active session changed during MCP enablement."
        );

        state
            .update_config(|config| {
                config.origin = Some(community_fixture.binding.origin.clone());
                config.active = Some(community_fixture.binding.clone());
            })
            .unwrap();
        *state.session.lock().await = Some(captured.clone());
        let (_response, authorization): (serde_json::Value, _) =
            begin_mcp_account_post(&state, &captured, &hosted, "/v1/mcp/device/prepare", &())
                .await
                .unwrap();
        assert!(authorization.account.is_none());
        assert!(!request
            .await
            .unwrap()
            .to_ascii_lowercase()
            .contains("peppy-account-session:"));
    }
}
