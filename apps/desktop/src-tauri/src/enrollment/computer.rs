//! Native transport and lifecycle for a computer claimant. The renderer receives only the QR
//! payload that it renders and derived progress; bearer tokens and transfer keys stay here.
use super::*;
use peppy_hosted_api::enrollment::EnrollmentState;

const COMPUTER_STATUS_PATH: &str = "/v1/enrollment/computers";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComputerClaimantView {
    pub stage: &'static str,
    pub invitation_id: String,
    pub qr_payload: Option<String>,
    pub awaiting_owner: bool,
    pub state: &'static str,
    pub expires_in_seconds: u32,
    pub verification_code: Option<String>,
}

pub struct ClaimantState {
    expected_binding: Option<crate::credentials::Binding>,
    qr_payload: String,
    view: ComputerClaimantView,
    confirm_request: Option<Zeroizing<String>>,
    prepared_identity: Option<NativeEnrollmentIdentity>,
}

#[derive(Deserialize)]
struct StatusRequest {
    path: String,
    body: serde_json::Value,
}

fn state_name(state: &EnrollmentState) -> &'static str {
    match state {
        EnrollmentState::Waiting => "waiting",
        EnrollmentState::Claimed => "claimed",
        EnrollmentState::Approved => "approved",
        EnrollmentState::Confirmed => "confirmed",
        EnrollmentState::Consumed => "consumed",
        EnrollmentState::Denied => "denied",
        EnrollmentState::Expired => "expired",
    }
}

fn claimant_view(
    value: peppy_hosted_client::newenrollment::ComputerClaimantView,
    qr_payload: Option<String>,
) -> ComputerClaimantView {
    let stage = match value.state {
        EnrollmentState::Approved => "compare_codes",
        EnrollmentState::Confirmed | EnrollmentState::Consumed => "finishing",
        EnrollmentState::Denied => "denied",
        EnrollmentState::Expired => "expired",
        _ if value.awaiting_owner => "waiting_owner",
        _ => "waiting_owner",
    };
    ComputerClaimantView {
        stage,
        invitation_id: value.invitation_id.to_string(),
        qr_payload,
        awaiting_owner: value.awaiting_owner,
        state: state_name(&value.state),
        expires_in_seconds: value.expires_in_seconds,
        verification_code: value.sas,
    }
}

fn saved_identity_blocks_claimant(state: &AppState, origin: &str) -> BridgeResult<bool> {
    saved_identity_for_origin(state, origin)
}

#[cfg(test)]
fn config_has_saved_identity(config: &crate::credentials::HostConfig, origin: &str) -> bool {
    config
        .bindings
        .iter()
        .any(|known| known.binding.origin == origin)
}

fn claimant_cancellation_allowed(claimant: &ClaimantState) -> bool {
    claimant.confirm_request.is_none() && claimant.view.state != "approved"
}

fn claimant_must_finish(claimant: &ClaimantState) -> bool {
    claimant.view.state == "approved"
        || claimant.confirm_request.is_some()
        || claimant.prepared_identity.is_some()
}

async fn install_prepared_identity(
    app: &AppHandle,
    state: &AppState,
    expected_binding: Option<crate::credentials::Binding>,
) -> BridgeResult<()> {
    let mut identity = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant
        .as_mut()
        .ok_or_else(enrollment_error)?
        .prepared_identity
        .take()
        .ok_or_else(enrollment_error)?;
    if let Err(error) = install_identity(app, state, &mut identity, None, expected_binding, true).await {
        state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .computer_claimant
            .as_mut()
            .ok_or_else(enrollment_error)?
            .prepared_identity = Some(identity);
        return Err(error);
    }
    Ok(())
}

fn connected_view(invitation_id: String) -> ComputerClaimantView {
    ComputerClaimantView {
        stage: "connected",
        invitation_id,
        qr_payload: None,
        awaiting_owner: false,
        state: "consumed",
        expires_in_seconds: 0,
        verification_code: None,
    }
}

async fn fetch_status(
    state: &AppState,
    hosted: &crate::hosted::HostedState,
) -> BridgeResult<Option<String>> {
    let (origin, request) = {
        let controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
        let origin = controller.origin()?.to_owned();
        let request = controller
            .client
            .as_ref()
            .ok_or_else(enrollment_error)?
            .computer_status_request()
            .map_err(|_| enrollment_error())?;
        (origin, request)
    };
    let request: StatusRequest = serde_json::from_str(&request).map_err(|_| enrollment_error())?;
    if !request.path.starts_with(COMPUTER_STATUS_PATH) {
        return Err(enrollment_error());
    }
    let account = hosted_account_header(state, hosted).await?;
    post_computer_status(&origin, request, account.as_deref().map(String::as_str)).await
}

async fn post_computer_status(
    origin: &str,
    request: StatusRequest,
    account_session: Option<&str>,
) -> BridgeResult<Option<String>> {
    let mut request_builder = http(&origin)?
        .post(format!("{origin}{}", request.path))
        .header("content-type", "application/json")
        .body(serde_json::to_string(&request.body).map_err(|_| enrollment_error())?);
    if let Some(account_session) = account_session {
        request_builder = request_builder.header("Peppy-Account-Session", account_session);
    }
    let response = request_builder
        .send()
        .await
        .map_err(|_| NetError::Offline)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(body(response).await?))
}

async fn post_computer_confirm(
    origin: &str,
    invitation_id: &str,
    request: Zeroizing<String>,
    account_session: Option<&str>,
) -> BridgeResult<String> {
    post(
        &http(origin)?,
        origin,
        &format!("{COMPUTER_STATUS_PATH}/{invitation_id}/confirm"),
        request.to_string(),
        None,
        account_session,
    )
    .await
}

fn update_view_from_status(state: &AppState, status: Option<String>) -> BridgeResult<ComputerClaimantView> {
    let mut controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
    let client = controller.client.as_ref().ok_or_else(enrollment_error)?;
    let local = match status {
        Some(status) => client.computer_claimant_view(status),
        None => client.computer_awaiting_owner_view(),
    }
    .map_err(|_| enrollment_error())?;
    let claimant = controller
        .computer_claimant
        .as_mut()
        .ok_or_else(enrollment_error)?;
    let view = claimant_view(local, Some(claimant.qr_payload.clone()));
    claimant.view = view.clone();
    Ok(view)
}

#[tauri::command]
pub async fn enrollment_begin_computer_claimant(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<ComputerClaimantView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let origin = {
        let mut controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
        let origin = controller.origin()?.to_owned();
        if let Some(claimant) = controller.computer_claimant.as_ref() {
            if claimant_must_finish(claimant) {
                return Err(BridgeError::new(
                    "enrollment-state",
                    "An approved computer connection must finish before another is started.",
                ));
            }
            // Waiting, claimed, denied, expired and remote terminal views have no opened
            // handoff, so they can be abandoned locally without rotating an approved flow.
            controller.computer_claimant = None;
            controller.client = None;
        }
        origin
    };
    if saved_identity_blocks_claimant(&state, &origin)? {
        return Err(BridgeError::new(
            "identity-preserved",
            "This device already has a saved identity. Unlock it instead of replacing it.",
        ));
    }
    let expected_binding = state.config()?.active_binding().cloned();
    let qr_payload = {
        let mut controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
        admission_client(&mut controller)?
            .begin_computer_claimant()
            .map_err(|_| enrollment_error())?
    };
    let invitation_id = serde_json::from_str::<serde_json::Value>(&qr_payload)
        .ok()
        .and_then(|value| value.get("invitation_id")?.as_str().map(ToOwned::to_owned))
        .ok_or_else(enrollment_error)?;
    let view = ComputerClaimantView {
        stage: "waiting_owner",
        invitation_id,
        qr_payload: Some(qr_payload.clone()),
        awaiting_owner: true,
        state: "waiting",
        expires_in_seconds: 0,
        verification_code: None,
    };
    state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant = Some(ClaimantState {
        expected_binding,
        qr_payload,
        view: view.clone(),
        confirm_request: None,
        prepared_identity: None,
    });
    Ok(view)
}

#[tauri::command]
pub async fn enrollment_computer_claimant_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<ComputerClaimantView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    update_view_from_status(&state, fetch_status(&state, &hosted).await?)
}

#[tauri::command]
pub async fn enrollment_confirm_computer_claimant(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<ComputerClaimantView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let prepared = {
        let controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
        let claimant = controller.computer_claimant.as_ref().ok_or_else(enrollment_error)?;
        (claimant.prepared_identity.is_some(), claimant.expected_binding.clone(), claimant.view.invitation_id.clone())
    };
    if prepared.0 {
        install_prepared_identity(&app, &state, prepared.1).await?;
        let has_gateway = refresh_gateway_presence(&state).await?;
        let mut controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
        controller.computer_claimant = None;
        controller.has_gateway = has_gateway;
        set_identity_ready(&mut controller, Some(EnrollmentRole::Device));
        return Ok(connected_view(prepared.2));
    }
    let saved_request = {
        let controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
        controller
            .computer_claimant
            .as_ref()
            .and_then(|claimant| claimant.confirm_request.as_ref())
            .cloned()
    };
    let request = match saved_request {
        Some(request) => request,
        None => {
            let status = fetch_status(&state, &hosted).await?.ok_or_else(enrollment_error)?;
            let request = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .client
                .as_mut()
                .ok_or_else(enrollment_error)?
                .confirm_computer_enrollment(status)
                .map_err(|_| enrollment_error())?;
            state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .computer_claimant
                .as_mut()
                .ok_or_else(enrollment_error)?
                .confirm_request = Some(Zeroizing::new(request));
            state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .computer_claimant
                .as_ref()
                .and_then(|claimant| claimant.confirm_request.as_ref())
                .cloned()
                .ok_or_else(enrollment_error)?
        }
    };
    let (origin, expected_binding, invitation_id) = {
        let controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
        let claimant = controller.computer_claimant.as_ref().ok_or_else(enrollment_error)?;
        (
            controller.origin()?.to_owned(),
            claimant.expected_binding.clone(),
            claimant.view.invitation_id.clone(),
        )
    };
    let account = hosted_account_header(&state, &hosted).await?;
    let complete = post_computer_confirm(
        &origin,
        &invitation_id,
        request,
        account.as_deref().map(String::as_str),
    )
    .await?;
    let identity = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .finish_computer_admission(complete)
        .map_err(|_| enrollment_error())?;
    state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant
        .as_mut()
        .ok_or_else(enrollment_error)?
        .prepared_identity = Some(identity);
    install_prepared_identity(&app, &state, expected_binding).await?;
    let has_gateway = refresh_gateway_presence(&state).await?;
    let mut controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
    controller.computer_claimant = None;
    controller.has_gateway = has_gateway;
    set_identity_ready(&mut controller, Some(EnrollmentRole::Device));
    Ok(connected_view(invitation_id))
}

#[tauri::command]
pub fn enrollment_cancel_computer_claimant(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    let mut controller = state.enrollment.lock().map_err(|_| BridgeError::host_state())?;
    let claimant = controller.computer_claimant.as_ref().ok_or_else(enrollment_error)?;
    if !claimant_cancellation_allowed(claimant) || claimant.prepared_identity.is_some() {
        return Err(BridgeError::new(
            "enrollment-state",
            "An approved computer connection cannot be cancelled or replaced.",
        ));
    }
    controller.computer_claimant = None;
    controller.client = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn claimant_view_never_exposes_a_status_transport_secret() {
        let view = ComputerClaimantView {
            stage: "waiting_owner",
            invitation_id: "id".into(),
            qr_payload: Some("qr-only-payload".into()),
            awaiting_owner: true,
            state: "waiting",
            expires_in_seconds: 0,
            verification_code: None,
        };
        let value = serde_json::to_value(view).unwrap();
        assert!(value.get("claimToken").is_none());
        assert!(value.get("transferPrivateKey").is_none());
    }

    #[test]
    fn saved_locked_identity_blocks_new_claimant_without_touching_secure_storage() {
        let binding = crate::credentials::Binding {
            origin: "https://enroll.example".into(),
            vault_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
        };
        let mut config = crate::credentials::HostConfig::default();
        config.remember(&binding);
        assert!(config_has_saved_identity(&config, "https://enroll.example"));
        assert!(!config_has_saved_identity(&config, "https://other.example"));
    }

    #[test]
    fn approval_or_a_retained_confirm_request_cannot_be_cancelled() {
        let waiting = ClaimantState {
            expected_binding: None,
            qr_payload: "qr".into(),
            view: ComputerClaimantView {
                stage: "waiting_owner",
                invitation_id: "id".into(),
                qr_payload: Some("qr".into()),
                awaiting_owner: true,
                state: "waiting",
                expires_in_seconds: 0,
                verification_code: None,
            },
            confirm_request: None,
            prepared_identity: None,
        };
        assert!(claimant_cancellation_allowed(&waiting));
        let approved = ClaimantState {
            view: ComputerClaimantView { state: "approved", ..waiting.view.clone() },
            ..waiting
        };
        assert!(!claimant_cancellation_allowed(&approved));
    }

    #[test]
    fn expired_claimant_can_be_restarted_but_approved_claimant_cannot() {
        let claimant = ClaimantState {
            expected_binding: None,
            qr_payload: "qr".into(),
            view: ComputerClaimantView {
                stage: "expired",
                invitation_id: "id".into(),
                qr_payload: Some("qr".into()),
                awaiting_owner: false,
                state: "expired",
                expires_in_seconds: 0,
                verification_code: None,
            },
            confirm_request: None,
            prepared_identity: None,
        };
        assert!(!claimant_must_finish(&claimant));
        assert!(claimant_must_finish(&ClaimantState {
            view: ComputerClaimantView { state: "approved", ..claimant.view.clone() },
            ..claimant
        }));
    }

    #[tokio::test]
    async fn hosted_account_header_is_sent_for_status_and_confirm_retries_keep_the_body() {
        let headers = Arc::new(Mutex::new(Vec::new()));
        let confirm_bodies = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/v1/enrollment/computers/test/status",
                post({
                    let headers = headers.clone();
                    move |request_headers: HeaderMap| {
                        let headers = headers.clone();
                        async move {
                            headers.lock().unwrap().push(
                                request_headers
                                    .get("Peppy-Account-Session")
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or_default()
                                    .to_owned(),
                            );
                            (StatusCode::OK, "{}")
                        }
                    }
                }),
            )
            .route(
                "/v1/enrollment/computers/test/confirm",
                post({
                    let headers = headers.clone();
                    let confirm_bodies = confirm_bodies.clone();
                    move |request_headers: HeaderMap, body: String| {
                        let headers = headers.clone();
                        let confirm_bodies = confirm_bodies.clone();
                        async move {
                            headers.lock().unwrap().push(
                                request_headers
                                    .get("Peppy-Account-Session")
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or_default()
                                    .to_owned(),
                            );
                            confirm_bodies.lock().unwrap().push(body);
                            (StatusCode::OK, "{}")
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let status = post_computer_status(
            &origin,
            StatusRequest {
                path: "/v1/enrollment/computers/test/status".into(),
                body: serde_json::json!({"claim_token":"private"}),
            },
            Some("hosted-session"),
        )
        .await;
        assert_eq!(status.unwrap().as_deref(), Some("{}"));
        let request = Zeroizing::<String>::new("{\"claim_token\":\"private\"}".into());
        post_computer_confirm(&origin, "test", request.clone(), Some("hosted-session"))
            .await
            .unwrap();
        post_computer_confirm(&origin, "test", request, Some("hosted-session"))
            .await
            .unwrap();
        server.abort();

        assert_eq!(headers.lock().unwrap().as_slice(), ["hosted-session"; 3]);
        assert_eq!(
            confirm_bodies.lock().unwrap().as_slice(),
            ["{\"claim_token\":\"private\"}"; 2]
        );
    }
}
