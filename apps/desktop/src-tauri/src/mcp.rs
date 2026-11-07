//! Private native MCP management bridge. Pairing material never reaches Tauri IPC.
use crate::{
    AppState,
    error::{BridgeError, BridgeResult},
    net::MAX_JSON_BYTES,
};
use peppy_hosted_api::mcp::{
    McpAuthorizationDecision, McpAuthorizationView, McpDeviceStatus, McpEnrollmentPreparation,
    McpGrantView, McpScope, McpWorkerActivateResponse,
};
use peppy_hosted_api::{
    DeviceRosterResponse,
    computer_enrollment::{ComputerEnrollmentCreated, ComputerEnrollmentStatus},
    enrollment::AdmissionComplete,
};
use peppy_hosted_client::newenrollment::{ClientKind, EnrollmentClient};
use serde::Serialize;
use std::str::FromStr;
use tauri::{State, WebviewWindow};
use uuid::Uuid;

const COMPUTER_ENROLLMENT_PATH: &str = "/v1/enrollment/computers";

async fn session(state: &AppState) -> BridgeResult<std::sync::Arc<crate::session::Session>> {
    let operation = crate::admit_update_operation(state)?;
    state.require_session_admitted(&operation).await
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

#[tauri::command]
pub async fn mcp_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<McpDeviceStatus> {
    crate::require_main(window.label())?;
    session(&state)
        .await?
        .api
        .get_json("/v1/mcp/device", MAX_JSON_BYTES)
        .await
        .map_err(Into::into)
}

#[derive(Serialize)]
struct Activation<'a> {
    operation_id: Uuid,
    status: &'a ComputerEnrollmentStatus,
}

#[derive(Serialize)]
struct Completion<'a> {
    operation_id: Uuid,
    admission: &'a AdmissionComplete,
}

#[tauri::command]
pub async fn mcp_enable(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<McpDeviceStatus> {
    crate::require_main(window.label())?;
    let session = session(&state).await?;
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
    current(&state, &session).await?;
    let prepared: McpEnrollmentPreparation = session
        .api
        .post_json_with_account(
            "/v1/mcp/device/prepare",
            &(),
            account.as_deref().map(String::as_str),
        )
        .await
        .map_err(BridgeError::from)?;
    current(&state, &session).await?;
    let vault =
        crate::sync::fetch_vault(&session.api, &binding.vault_id, &binding.device_id).await?;
    current(&state, &session).await?;
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
    let created: ComputerEnrollmentCreated = session
        .api
        .post_json_with_account(
            COMPUTER_ENROLLMENT_PATH,
            &create,
            account.as_deref().map(String::as_str),
        )
        .await
        .map_err(BridgeError::from)?;
    current(&state, &session).await?;
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
    let status: ComputerEnrollmentStatus = session
        .api
        .post_json_with_account(
            &format!("/v1/enrollment/computers/{invitation_id}/approve"),
            &serde_json::from_str::<serde_json::Value>(&approval)
                .map_err(|_| BridgeError::host_state())?,
            account.as_deref().map(String::as_str),
        )
        .await
        .map_err(BridgeError::from)?;
    current(&state, &session).await?;
    let activation: McpWorkerActivateResponse = session
        .api
        .post_json_with_account(
            "/v1/mcp/device/activate",
            &Activation {
                operation_id: prepared.operation_id,
                status: &status,
            },
            account.as_deref().map(String::as_str),
        )
        .await
        .map_err(BridgeError::from)?;
    current(&state, &session).await?;
    let admission: AdmissionComplete = session
        .api
        .post_json_with_account(
            &format!("/v1/enrollment/computers/{invitation_id}/confirm"),
            &activation.confirm,
            account.as_deref().map(String::as_str),
        )
        .await
        .map_err(BridgeError::from)?;
    current(&state, &session).await?;
    let result = session
        .api
        .post_json_with_account(
            "/v1/mcp/device/complete",
            &Completion {
                operation_id: prepared.operation_id,
                admission: &admission,
            },
            account.as_deref().map(String::as_str),
        )
        .await
        .map_err(BridgeError::from)?;
    current(&state, &session).await?;
    Ok(result)
}

#[tauri::command]
pub async fn mcp_grants(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<Vec<McpGrantView>> {
    crate::require_main(window.label())?;
    session(&state)
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
    session(&state)
        .await?
        .api
        .post_discard(&format!("/v1/mcp/grants/{id}/revoke"), &())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn mcp_authorization(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<McpAuthorizationView> {
    crate::require_main(window.label())?;
    let id = authorization_id(&id)?;
    session(&state)
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
    session(&state)
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
    session(&state)
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

    #[test]
    fn computer_creation_uses_the_collection_route() {
        assert_eq!(COMPUTER_ENROLLMENT_PATH, "/v1/enrollment/computers");
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
