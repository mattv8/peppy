//! Phone-first join: this desktop shows a join-request QR and claims an owner-approved intent.
//!
//! This module deliberately owns the claimant's short-lived material.  Nothing in `JoinSession`
//! is serializable or persisted; the webview gets only `JoinView`.

use crate::{
    error::{BridgeError, BridgeResult},
    net::{read_bounded, NetError},
    AppState,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use peppy_hosted_client::{
    claim::{pairing_proof_bytes, EnrollmentKey},
    join::{
        encode_join_request_qr, generate_join_key, intent_digest_hex, open_intent_token,
        JoinRequestQr,
    },
};
use peppy_protocol::JoinRequestState;
use reqwest::{header::HeaderValue, redirect, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::{AppHandle, State, WebviewWindow};
use tokio::sync::Mutex;
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_JOIN_JSON_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinView {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    qr_payload: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'static str>,
}

impl JoinView {
    fn idle() -> Self {
        Self {
            state: "idle",
            qr_payload: None,
            expires_in_seconds: None,
            sas: None,
            origin: None,
            error_code: None,
        }
    }
}

#[derive(Default)]
pub struct JoinState {
    inner: Mutex<JoinInner>,
}

#[derive(Default)]
struct JoinInner {
    generation: u64,
    session: Option<JoinSession>,
    view: Option<JoinView>,
}

struct JoinSession {
    origin: String,
    request_id: Uuid,
    poll_secret: Zeroizing<String>,
    join_secret: peppy_hosted_client::JoinKeySecret,
    join_public: [u8; 32],
    phase: Phase,
}

enum Phase {
    Waiting,
    Claimed(Claim),
    Confirm(Confirm),
}

struct Claim {
    intent: Zeroizing<String>,
    key: EnrollmentKey,
    device_id: String,
    key_digest: String,
    claim_secret: Zeroizing<String>,
    sas: String,
}

struct Confirm {
    claim: Claim,
    challenge_token: Zeroizing<String>,
    vault_id: String,
    key_epoch: u32,
    profile_fingerprint: String,
}

#[derive(Deserialize)]
struct Created {
    join_request_id: Uuid,
    poll_secret: String,
    expires_in_seconds: i64,
}
#[derive(Deserialize)]
struct Poll {
    state: JoinRequestState,
    sealed_intent_token: Option<String>,
    intent_digest: Option<String>,
    expires_in_seconds: i64,
}
#[derive(Deserialize)]
struct Claimed {
    key_digest: String,
    sas: String,
    claim_secret: String,
    expires_in_seconds: i64,
}
#[derive(Deserialize)]
struct Challenge {
    challenge_token: String,
    vault_id: String,
    key_epoch: u32,
    profile_fingerprint: String,
    requested_role: String,
    expires_in_seconds: i64,
}
#[derive(Deserialize)]
struct Consumed {
    vault_id: String,
    device_id: String,
    device_token: String,
}

fn failed(code: &'static str) -> JoinView {
    JoinView {
        state: "failed",
        qr_payload: None,
        expires_in_seconds: None,
        sas: None,
        origin: None,
        error_code: Some(code),
    }
}
/// Rate limiting, server errors and connectivity blips are transient: the join keeps its
/// secrets and the next poll retries. Only definite answers end it.
fn retryable(error: &NetError) -> bool {
    matches!(error, NetError::Offline)
        || matches!(error, NetError::Status { status, .. } if *status == 429 || *status >= 500)
}

fn retrying(session: &JoinSession) -> JoinView {
    JoinView {
        error_code: Some("join-retrying"),
        ..view(session, None)
    }
}

fn view(session: &JoinSession, expires: Option<i64>) -> JoinView {
    match &session.phase {
        Phase::Waiting => JoinView {
            state: "waiting",
            qr_payload: None,
            expires_in_seconds: expires,
            sas: None,
            origin: Some(session.origin.clone()),
            error_code: None,
        },
        Phase::Claimed(claim) => JoinView {
            state: "claimed",
            qr_payload: None,
            expires_in_seconds: expires,
            sas: Some(claim.sas.clone()),
            origin: Some(session.origin.clone()),
            error_code: None,
        },
        Phase::Confirm(confirm) => JoinView {
            state: "confirm",
            qr_payload: None,
            expires_in_seconds: expires,
            sas: Some(confirm.claim.sas.clone()),
            origin: Some(session.origin.clone()),
            error_code: None,
        },
    }
}

fn client(origin: &str) -> BridgeResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .https_only(!crate::origin::is_loopback_http(origin))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .user_agent("Peppy-Desktop")
        .build()
        .map_err(|_| BridgeError::new("network", "Could not create the native network client."))
}

async fn json<T: for<'a> Deserialize<'a>>(request: reqwest::RequestBuilder) -> Result<T, NetError> {
    let response = request.send().await.map_err(|_| NetError::Offline)?;
    if !response.status().is_success() {
        let status = response.status();
        let code = read_bounded(response, 4096)
            .await
            .ok()
            .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
            .and_then(|v| {
                v.get("code")?
                    .as_str()
                    .filter(|code| {
                        code.len() <= 64
                            && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    })
                    .map(str::to_owned)
            });
        return Err(NetError::Status {
            status: status.as_u16(),
            code,
        });
    }
    serde_json::from_slice(&read_bounded(response, MAX_JOIN_JSON_BYTES).await?)
        .map_err(|_| NetError::Invalid)
}

fn put(
    inner: &mut JoinInner,
    generation: u64,
    session: Option<JoinSession>,
    next: JoinView,
) -> JoinView {
    if inner.generation == generation {
        inner.session = session;
        inner.view = Some(next.clone());
        next
    } else {
        inner.view.clone().unwrap_or_else(JoinView::idle)
    }
}

impl JoinState {
    pub(crate) async fn start(&self, origin: Option<String>) -> BridgeResult<JoinView> {
        let state = self;
        let origin =
            crate::origin::validate_origin(origin.as_deref().unwrap_or("https://peppy.pro"))?;
        let generation = {
            let mut inner = state.inner.lock().await;
            inner.generation = inner.generation.wrapping_add(1);
            inner.session = None;
            inner.view = Some(JoinView::idle());
            inner.generation
        };
        let created: Created =
            json(client(&origin)?.post(format!("{origin}/v1/pairing/join-requests")))
                .await
                .map_err(BridgeError::from)?;
        let (join_secret, join_public) = generate_join_key()
            .map_err(|_| BridgeError::new("join-key", "Could not create a join key."))?;
        let qr_payload = encode_join_request_qr(&JoinRequestQr {
            https_origin: origin.clone(),
            join_request_id: created.join_request_id,
            join_key: URL_SAFE_NO_PAD.encode(join_public),
        });
        let next = JoinView {
            state: "waiting",
            qr_payload: Some(qr_payload),
            expires_in_seconds: Some(created.expires_in_seconds.max(0)),
            sas: None,
            origin: Some(origin.clone()),
            error_code: None,
        };
        let session = JoinSession {
            origin,
            request_id: created.join_request_id,
            poll_secret: Zeroizing::new(created.poll_secret),
            join_secret,
            join_public,
            phase: Phase::Waiting,
        };
        let mut inner = state.inner.lock().await;
        Ok(put(&mut inner, generation, Some(session), next))
    }

    pub(crate) async fn status(&self) -> BridgeResult<JoinView> {
        let state = self;
        let (generation, mut session) = {
            let mut inner = state.inner.lock().await;
            let generation = inner.generation;
            let Some(session) = inner.session.take() else {
                return Ok(inner.view.clone().unwrap_or_else(JoinView::idle));
            };
            (generation, session)
        };
        let http = match client(&session.origin) {
            Ok(http) => http,
            Err(error) => {
                let mut inner = state.inner.lock().await;
                let _ = put(&mut inner, generation, None, failed("join-network"));
                return Err(error);
            }
        };
        let result: BridgeResult<(Option<JoinSession>, JoinView)> = async move { match &mut session.phase {
        Phase::Waiting => {
            let path = format!(
                "{}/v1/pairing/join-requests/{}",
                session.origin, session.request_id
            );
            let Ok(secret) = HeaderValue::from_str(session.poll_secret.as_str()) else {
                return Ok((None, failed("join-invalid")));
            };
            match json::<Poll>(http.get(path).header("Peppy-Join-Secret", secret)).await {
                Ok(Poll {
                    state: JoinRequestState::Waiting,
                    expires_in_seconds,
                    ..
                }) => {
                    let next = view(&session, Some(expires_in_seconds.max(0)));
                    Ok((Some(session), next))
                }
                Ok(Poll {
                    state: JoinRequestState::Expired,
                    ..
                }) => Ok((
                    None,
                    JoinView {
                        state: "expired",
                        qr_payload: None,
                        expires_in_seconds: Some(0),
                        sas: None,
                        origin: Some(session.origin.clone()),
                        error_code: None,
                    },
                )),
                Ok(Poll {
                    state: JoinRequestState::Offered,
                    sealed_intent_token: Some(sealed),
                    intent_digest: Some(digest),
                    ..
                }) => {
                    let Ok(sealed) = URL_SAFE_NO_PAD.decode(sealed) else {
                        return Ok((None, failed("join-offer-invalid")));
                    };
                    let Ok(intent) =
                        open_intent_token(&session.join_secret, &session.join_public, &sealed)
                    else {
                        return Ok((None, failed("join-offer-invalid")));
                    };
                    if intent.len() != 43 || intent_digest_hex(&intent) != digest {
                        Ok((None, failed("join-offer-invalid")))
                    } else {
                        let key = EnrollmentKey::generate().map_err(|_| {
                            BridgeError::new("join-key", "Could not create an enrollment key.")
                        })?;
                        let device_id = Uuid::new_v4().to_string();
                        let public_key = key.public_key_base64url().map_err(|_| {
                            BridgeError::new("join-key", "Could not create an enrollment key.")
                        })?;
                        let body = serde_json::json!({"device_id": device_id, "public_key": {"ed25519_public_key": public_key}, "requested_role": "device"});
                        let claim: Claimed = match json(
                            http.post(format!(
                                "{}/v1/pairing/intents/{}/claim",
                                session.origin,
                                intent.as_str()
                            ))
                            .json(&body),
                        )
                        .await
                        {
                            Ok(claim) => claim,
                            // The offer stays on the server, so a transient failure retries the
                            // claim on the next poll with a fresh device key.
                            Err(error) if retryable(&error) => {
                                let next = retrying(&session);
                                return Ok((Some(session), next));
                            }
                            Err(error) => return Err(BridgeError::from(error)),
                        };
                        let sas = key
                            .pairing_sas(&intent, &device_id, &claim.key_digest)
                            .map_err(|_| {
                                BridgeError::new("join-claim-invalid", "The join claim is invalid.")
                            })?;
                        if sas != claim.sas {
                            Ok((None, failed("join-sas-mismatch")))
                        } else {
                            session.phase = Phase::Claimed(Claim {
                                intent,
                                key,
                                device_id,
                                key_digest: claim.key_digest,
                                claim_secret: Zeroizing::new(claim.claim_secret),
                                sas,
                            });
                            let next = view(&session, Some(claim.expires_in_seconds.max(0)));
                            Ok((Some(session), next))
                        }
                    }
                }
                Ok(_) => Ok((None, failed("join-offer-invalid"))),
                Err(error) if retryable(&error) => {
                    let next = retrying(&session);
                    Ok((Some(session), next))
                }
                Err(error) => Err(BridgeError::from(error)),
            }
        }
        Phase::Claimed(claim) => {
            let body = serde_json::json!({"device_id": claim.device_id, "key_digest": claim.key_digest, "claim_secret": claim.claim_secret.as_str()});
            match json::<Challenge>(
                http.post(format!(
                    "{}/v1/pairing/intents/{}/challenge",
                    session.origin,
                    claim.intent.as_str()
                ))
                .json(&body),
            )
            .await
            {
                Err(error)
                    if error.is_status(
                        StatusCode::UNAUTHORIZED.as_u16(),
                        "pairing_challenge_unavailable",
                    ) =>
                {
                    let next = view(&session, None);
                    Ok((Some(session), next))
                }
                Err(error) if retryable(&error) => {
                    let next = retrying(&session);
                    Ok((Some(session), next))
                }
                Err(error) => Err(BridgeError::from(error)),
                Ok(challenge) if challenge.requested_role != "device" => {
                    Ok((None, failed("join-role-invalid")))
                }
                Ok(challenge) => {
                    let claim = match std::mem::replace(&mut session.phase, Phase::Waiting) {
                        Phase::Claimed(claim) => claim,
                        _ => unreachable!(),
                    };
                    let expires = challenge.expires_in_seconds.max(0);
                    session.phase = Phase::Confirm(Confirm {
                        claim,
                        challenge_token: Zeroizing::new(challenge.challenge_token),
                        vault_id: challenge.vault_id,
                        key_epoch: challenge.key_epoch,
                        profile_fingerprint: challenge.profile_fingerprint,
                    });
                    let next = view(&session, Some(expires));
                    Ok((Some(session), next))
                }
            }
        }
        Phase::Confirm(_) => {
            let next = view(&session, None);
            Ok((Some(session), next))
        }
    } }.await;
        match result {
            Ok((session, next)) => {
                let mut inner = state.inner.lock().await;
                Ok(put(&mut inner, generation, session, next))
            }
            Err(error) => {
                let next = failed("join-network");
                let mut inner = state.inner.lock().await;
                let _ = put(&mut inner, generation, None, next);
                Err(error)
            }
        }
    }

    pub(crate) async fn cancel(&self) {
        let mut inner = self.inner.lock().await;
        inner.generation = inner.generation.wrapping_add(1);
        inner.session = None;
        inner.view = Some(JoinView::idle());
    }

    /// Consumes the approved challenge only after the person confirmed the matching code; the
    /// resulting credential is handed to `activate` and never reaches the webview.
    pub(crate) async fn confirm<F, Fut>(&self, activate: F) -> BridgeResult<JoinView>
    where
        F: FnOnce(Zeroizing<Vec<u8>>) -> Fut,
        Fut: std::future::Future<Output = BridgeResult<()>>,
    {
        let state = self;
        let (generation, session) = {
            let mut inner = state.inner.lock().await;
            let generation = inner.generation;
            if !matches!(
                inner.session.as_ref().map(|session| &session.phase),
                Some(Phase::Confirm(_))
            ) {
                return Err(BridgeError::new(
                    "join-state",
                    "The join request is not awaiting confirmation.",
                ));
            }
            let session = inner.session.take().expect("confirm phase checked above");
            (generation, session)
        };
        let result: BridgeResult<()> = async move {
    let JoinSession {
        origin,
        phase: Phase::Confirm(confirm),
        ..
    } = session
    else {
        return Err(BridgeError::new(
            "join-state",
            "The join request is not awaiting confirmation.",
        ));
    };
    let proof = pairing_proof_bytes(
        &confirm.challenge_token,
        &confirm.vault_id,
        &confirm.claim.device_id,
        &confirm.profile_fingerprint,
        confirm.key_epoch,
        "device",
    )
    .map_err(|_| BridgeError::new("join-proof", "Could not create the pairing proof."))?;
    let signature = confirm
        .claim
        .key
        .sign_pairing_proof(&proof)
        .map_err(|_| BridgeError::new("join-proof", "Could not sign the pairing proof."))?;
    let public_key = confirm
        .claim
        .key
        .public_key_base64url()
        .map_err(|_| BridgeError::new("join-key", "Could not read the enrollment key."))?;
    let body = serde_json::json!({"challenge_token": confirm.challenge_token.as_str(), "device_id": confirm.claim.device_id, "public_key": {"ed25519_public_key": public_key}, "profile_fingerprint": confirm.profile_fingerprint, "key_epoch": confirm.key_epoch, "signature": signature});
    let consumed: Consumed = json(
        client(&origin)?
            .post(format!("{origin}/v1/pairing/consume"))
            .json(&body),
    )
    .await
    .map_err(BridgeError::from)?;
    if consumed.device_id != confirm.claim.device_id || consumed.vault_id != confirm.vault_id {
        return Err(BridgeError::new(
            "join-consume-invalid",
            "The join response did not match this device.",
        ));
    }
    let credential = Zeroizing::new(serde_json::to_vec(&serde_json::json!({"version": 1, "origin": origin, "vaultId": consumed.vault_id, "deviceId": consumed.device_id, "deviceToken": consumed.device_token})).map_err(|_| BridgeError::new("join-credential", "Could not create the device credential."))?);
    activate(credential).await?;
        Ok(())
    }
    .await;
        let mut inner = state.inner.lock().await;
        match result {
            Ok(()) => Ok(put(
                &mut inner,
                generation,
                None,
                JoinView {
                    state: "approved",
                    qr_payload: None,
                    expires_in_seconds: None,
                    sas: None,
                    origin: None,
                    error_code: None,
                },
            )),
            Err(error) => {
                let _ = put(&mut inner, generation, None, failed("join-confirm"));
                Err(error)
            }
        }
    }
}

#[tauri::command]
pub async fn join_start(
    window: WebviewWindow,
    state: State<'_, JoinState>,
    origin: Option<String>,
) -> BridgeResult<JoinView> {
    crate::require_main(window.label())?;
    state.start(origin).await
}

#[tauri::command]
pub async fn join_status(
    window: WebviewWindow,
    state: State<'_, JoinState>,
) -> BridgeResult<JoinView> {
    crate::require_main(window.label())?;
    state.status().await
}

#[tauri::command]
pub async fn join_cancel(window: WebviewWindow, state: State<'_, JoinState>) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    state.cancel().await;
    Ok(())
}

#[tauri::command]
pub async fn join_confirm(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, JoinState>,
    app_state: State<'_, AppState>,
) -> BridgeResult<JoinView> {
    crate::require_main(window.label())?;
    state
        .confirm(|credential| async move {
            crate::activate_native_credential(&app, &app_state, credential).await
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, State as AxumState},
        http::{HeaderMap, StatusCode as AxumStatus},
        response::{IntoResponse, Response},
        routing::{get, post},
        Json, Router,
    };
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use peppy_hosted_client::{pairing_intent_sas, parse_join_request_qr, seal_intent_token};
    use peppy_protocol::pairing_key_digest;
    use serde_json::{json, Value};
    use std::{collections::HashMap, sync::Arc};

    const POLL_SECRET: &str = "cG9sbC1zZWNyZXQtcG9sbC1zZWNyZXQtcG9sbC1zZWM";
    const INTENT: &str = "aW50ZW50LXRva2VuLWludGVudC10b2tlbi1pbnRlbnQ";
    const CLAIM_SECRET: &str = "Y2xhaW0tc2VjcmV0LWNsYWltLXNlY3JldC1jbGFpbS0";
    const CHALLENGE: &str = "Y2hhbGxlbmdlLWNoYWxsZW5nZS1jaGFsbGVuZ2UtY2g";
    const VAULT: &str = "11111111-2222-4333-8444-555555555555";
    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct Script {
        offer: Option<(String, String)>,
        expired: bool,
        approved: bool,
        role: Option<&'static str>,
        wrong_sas: bool,
        poll_delay_ms: u64,
        poll_status: Option<u16>,
        claim_status: Option<u16>,
        consume_vault: Option<&'static str>,
        claimed: Option<(String, Vec<u8>)>,
        consume_body: Option<Value>,
        counts: HashMap<&'static str, u32>,
    }
    type Shared = Arc<std::sync::Mutex<Script>>;

    fn bump(script: &Shared, route: &'static str) {
        *script.lock().unwrap().counts.entry(route).or_default() += 1;
    }
    fn count(script: &Shared, route: &str) -> u32 {
        script
            .lock()
            .unwrap()
            .counts
            .get(route)
            .copied()
            .unwrap_or(0)
    }

    async fn create(AxumState(script): AxumState<Shared>) -> Json<Value> {
        bump(&script, "create");
        Json(
            json!({"join_request_id": Uuid::new_v4(), "poll_secret": POLL_SECRET, "expires_in_seconds": 300}),
        )
    }

    async fn poll(
        AxumState(script): AxumState<Shared>,
        Path(_id): Path<String>,
        headers: HeaderMap,
    ) -> Response {
        bump(&script, "poll");
        let delay = script.lock().unwrap().poll_delay_ms;
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if headers
            .get("Peppy-Join-Secret")
            .and_then(|h| h.to_str().ok())
            != Some(POLL_SECRET)
        {
            return (
                AxumStatus::NOT_FOUND,
                Json(json!({"code": "join_request_not_found"})),
            )
                .into_response();
        }
        let script = script.lock().unwrap();
        if let Some(status) = script.poll_status {
            return AxumStatus::from_u16(status).unwrap().into_response();
        }
        if script.expired {
            return Json(json!({"state": "expired", "expires_in_seconds": 0})).into_response();
        }
        match &script.offer {
            None => Json(json!({"state": "waiting", "expires_in_seconds": 250})).into_response(),
            Some((sealed, digest)) => Json(json!({"state": "offered", "sealed_intent_token": sealed, "intent_digest": digest, "expires_in_seconds": 200})).into_response(),
        }
    }

    async fn claim(
        AxumState(script): AxumState<Shared>,
        Path(intent): Path<String>,
        Json(body): Json<Value>,
    ) -> Response {
        bump(&script, "claim");
        if let Some(status) = script.lock().unwrap().claim_status.take() {
            return AxumStatus::from_u16(status).unwrap().into_response();
        }
        assert_eq!(intent, INTENT);
        assert_eq!(body["requested_role"], "device");
        let device = body["device_id"].as_str().unwrap().to_owned();
        let public = URL_SAFE_NO_PAD
            .decode(body["public_key"]["ed25519_public_key"].as_str().unwrap())
            .unwrap();
        let digest = pairing_key_digest(&public.clone().try_into().unwrap());
        let mut sas = pairing_intent_sas(intent, digest.clone(), device.clone()).unwrap();
        let mut script = script.lock().unwrap();
        if script.wrong_sas {
            sas = if sas == "000000" {
                "111111".into()
            } else {
                "000000".into()
            };
        }
        script.claimed = Some((device, public));
        Json(json!({"key_digest": digest, "sas": sas, "claim_secret": CLAIM_SECRET, "expires_in_seconds": 280})).into_response()
    }

    async fn challenge(AxumState(script): AxumState<Shared>, Json(body): Json<Value>) -> Response {
        bump(&script, "challenge");
        assert_eq!(body["claim_secret"], CLAIM_SECRET);
        let script = script.lock().unwrap();
        if !script.approved {
            return (
                AxumStatus::UNAUTHORIZED,
                Json(json!({"code": "pairing_challenge_unavailable"})),
            )
                .into_response();
        }
        Json(json!({"challenge_token": CHALLENGE, "vault_id": VAULT, "key_epoch": 1, "profile_fingerprint": FINGERPRINT, "requested_role": script.role.unwrap_or("device"), "expires_in_seconds": 120})).into_response()
    }

    async fn consume(AxumState(script): AxumState<Shared>, Json(body): Json<Value>) -> Response {
        bump(&script, "consume");
        let mut script = script.lock().unwrap();
        let (device, public) = script.claimed.clone().expect("claimed before consume");
        let proof =
            pairing_proof_bytes(CHALLENGE, VAULT, &device, FINGERPRINT, 1, "device").unwrap();
        let key = VerifyingKey::from_bytes(&public.try_into().unwrap()).unwrap();
        let signature = URL_SAFE_NO_PAD
            .decode(body["signature"].as_str().unwrap())
            .unwrap();
        key.verify(&proof, &Signature::from_slice(&signature).unwrap())
            .expect("consume proof is signed by the claimed key");
        script.consume_body = Some(body);
        let vault = script.consume_vault.unwrap_or(VAULT);
        Json(json!({"vault_id": vault, "device_id": device, "device_token": "1".repeat(96)}))
            .into_response()
    }

    async fn server(script: Shared) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let router = Router::new()
            .route("/v1/pairing/join-requests", post(create))
            .route("/v1/pairing/join-requests/{id}", get(poll))
            .route("/v1/pairing/intents/{intent}/claim", post(claim))
            .route("/v1/pairing/intents/{intent}/challenge", post(challenge))
            .route("/v1/pairing/consume", post(consume))
            .with_state(script);
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://127.0.0.1:{port}")
    }

    /// Plays the owner phone: reads the desktop QR and seals `intent` to its one-time key.
    fn phone_offer(script: &Shared, qr_payload: &str, intent: &str, digest_of: &str) {
        let qr = parse_join_request_qr(qr_payload, true).unwrap();
        let key: [u8; 32] = URL_SAFE_NO_PAD
            .decode(qr.join_key)
            .unwrap()
            .try_into()
            .unwrap();
        let sealed = URL_SAFE_NO_PAD.encode(seal_intent_token(&key, intent).unwrap());
        script.lock().unwrap().offer = Some((sealed, intent_digest_hex(digest_of)));
    }

    async fn started(script: &Shared) -> (JoinState, JoinView) {
        let origin = server(script.clone()).await;
        let state = JoinState::default();
        let view = state.start(Some(origin)).await.unwrap();
        assert_eq!(view.state, "waiting");
        (state, view)
    }

    async fn claimed(script: &Shared) -> JoinState {
        let (state, view) = started(script).await;
        phone_offer(script, view.qr_payload.as_deref().unwrap(), INTENT, INTENT);
        let view = state.status().await.unwrap();
        assert_eq!(view.state, "claimed");
        assert_eq!(view.sas.as_deref().map(str::len), Some(6));
        state
    }

    fn assert_no_secrets(view: &JoinView) {
        let json = serde_json::to_string(view).unwrap();
        for secret in [POLL_SECRET, INTENT, CLAIM_SECRET, CHALLENGE] {
            assert!(!json.contains(secret), "view leaked a secret: {json}");
        }
    }

    #[tokio::test]
    async fn waiting_then_expired() {
        let script = Shared::default();
        let (state, view) = started(&script).await;
        assert_no_secrets(&view);
        assert_eq!(state.status().await.unwrap().state, "waiting");
        script.lock().unwrap().expired = true;
        assert_eq!(state.status().await.unwrap().state, "expired");
        assert!(state.inner.lock().await.session.is_none());
    }

    #[tokio::test]
    async fn tampered_offer_fails_without_claiming() {
        let script = Shared::default();
        let (state, _) = started(&script).await;
        script.lock().unwrap().offer =
            Some((URL_SAFE_NO_PAD.encode([7u8; 96]), intent_digest_hex(INTENT)));
        let view = state.status().await.unwrap();
        assert_eq!(
            (view.state, view.error_code),
            ("failed", Some("join-offer-invalid"))
        );
        assert_eq!(count(&script, "claim"), 0);
    }

    #[tokio::test]
    async fn digest_mismatch_fails_without_claiming() {
        let script = Shared::default();
        let (state, view) = started(&script).await;
        phone_offer(
            &script,
            view.qr_payload.as_deref().unwrap(),
            INTENT,
            "another-intent",
        );
        let view = state.status().await.unwrap();
        assert_eq!(
            (view.state, view.error_code),
            ("failed", Some("join-offer-invalid"))
        );
        assert_eq!(count(&script, "claim"), 0);
    }

    #[tokio::test]
    async fn server_sas_mismatch_fails() {
        let script = Shared::default();
        script.lock().unwrap().wrong_sas = true;
        let (state, view) = started(&script).await;
        phone_offer(&script, view.qr_payload.as_deref().unwrap(), INTENT, INTENT);
        let view = state.status().await.unwrap();
        assert_eq!(
            (view.state, view.error_code),
            ("failed", Some("join-sas-mismatch"))
        );
    }

    #[tokio::test]
    async fn approval_requires_explicit_confirmation_before_consume() {
        let script = Shared::default();
        let state = claimed(&script).await;
        let view = state.status().await.unwrap();
        assert_eq!(
            view.state, "claimed",
            "unapproved challenge keeps the claim"
        );
        assert_no_secrets(&view);
        script.lock().unwrap().approved = true;
        let view = state.status().await.unwrap();
        assert_eq!(view.state, "confirm");
        assert_no_secrets(&view);
        assert_eq!(state.status().await.unwrap().state, "confirm");
        assert_eq!(count(&script, "consume"), 0);

        let activated = Arc::new(std::sync::Mutex::new(None::<Value>));
        let sink = activated.clone();
        let view = state
            .confirm(|credential| async move {
                *sink.lock().unwrap() = Some(serde_json::from_slice(&credential).unwrap());
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(view.state, "approved");
        assert_eq!(count(&script, "consume"), 1);
        let credential = activated.lock().unwrap().clone().unwrap();
        let device = script.lock().unwrap().claimed.clone().unwrap().0;
        assert_eq!(credential["vaultId"], VAULT);
        assert_eq!(credential["deviceId"], device.as_str());
        assert_eq!(credential["deviceToken"], "1".repeat(96));
        assert!(credential["origin"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:"));
        assert!(state.inner.lock().await.session.is_none());
    }

    #[tokio::test]
    async fn confirm_is_refused_before_approval_and_keeps_the_claim() {
        let script = Shared::default();
        let state = claimed(&script).await;
        let error = state.confirm(|_| async { Ok(()) }).await.unwrap_err();
        assert_eq!(error.code, "join-state");
        assert_eq!(state.status().await.unwrap().state, "claimed");
        assert_eq!(count(&script, "consume"), 0);
    }

    #[tokio::test]
    async fn activation_failure_surfaces_and_clears_the_join() {
        let script = Shared::default();
        let state = claimed(&script).await;
        script.lock().unwrap().approved = true;
        assert_eq!(state.status().await.unwrap().state, "confirm");
        let error = state
            .confirm(|_| async { Err(BridgeError::new("origin-binding", "no")) })
            .await
            .unwrap_err();
        assert_eq!(error.code, "origin-binding");
        let view = state.status().await.unwrap();
        assert_eq!(
            (view.state, view.error_code),
            ("failed", Some("join-confirm"))
        );
    }

    #[tokio::test]
    async fn gateway_role_challenge_is_rejected() {
        let script = Shared::default();
        let state = claimed(&script).await;
        {
            let mut script = script.lock().unwrap();
            script.approved = true;
            script.role = Some("gateway");
        }
        let view = state.status().await.unwrap();
        assert_eq!(
            (view.state, view.error_code),
            ("failed", Some("join-role-invalid"))
        );
    }

    #[tokio::test]
    async fn cancel_while_claimed_stops_all_requests() {
        let script = Shared::default();
        let state = claimed(&script).await;
        state.cancel().await;
        let before: u32 = script.lock().unwrap().counts.values().sum();
        assert_eq!(state.status().await.unwrap().state, "idle");
        let after: u32 = script.lock().unwrap().counts.values().sum();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn stale_status_does_not_overwrite_a_cancel() {
        let script = Shared::default();
        let (state, _) = started(&script).await;
        script.lock().unwrap().poll_delay_ms = 300;
        let state = Arc::new(state);
        let polling = {
            let state = state.clone();
            tokio::spawn(async move { state.status().await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        state.cancel().await;
        let stale = polling.await.unwrap().unwrap();
        assert_eq!(
            stale.state, "idle",
            "the superseded poll reports the newer state"
        );
        assert_eq!(state.status().await.unwrap().state, "idle");
    }

    #[tokio::test]
    async fn rejected_claim_does_not_leave_a_stuck_view() {
        let script = Shared::default();
        script.lock().unwrap().claim_status = Some(409);
        let (state, view) = started(&script).await;
        phone_offer(&script, view.qr_payload.as_deref().unwrap(), INTENT, INTENT);
        assert!(state.status().await.is_err());
        let view = state.status().await.unwrap();
        assert_eq!(
            (view.state, view.error_code),
            ("failed", Some("join-network"))
        );
    }

    #[tokio::test]
    async fn rate_limited_poll_keeps_the_join_alive() {
        let script = Shared::default();
        let (state, view) = started(&script).await;
        script.lock().unwrap().poll_status = Some(429);
        let retry = state.status().await.unwrap();
        assert_eq!(
            (retry.state, retry.error_code),
            ("waiting", Some("join-retrying"))
        );
        script.lock().unwrap().poll_status = Some(503);
        assert_eq!(
            state.status().await.unwrap().error_code,
            Some("join-retrying")
        );
        script.lock().unwrap().poll_status = None;
        phone_offer(&script, view.qr_payload.as_deref().unwrap(), INTENT, INTENT);
        assert_eq!(state.status().await.unwrap().state, "claimed");
    }

    #[tokio::test]
    async fn transient_claim_failure_retries_on_the_next_poll() {
        let script = Shared::default();
        script.lock().unwrap().claim_status = Some(503);
        let (state, view) = started(&script).await;
        phone_offer(&script, view.qr_payload.as_deref().unwrap(), INTENT, INTENT);
        let retry = state.status().await.unwrap();
        assert_eq!(
            (retry.state, retry.error_code),
            ("waiting", Some("join-retrying"))
        );
        assert_eq!(state.status().await.unwrap().state, "claimed");
        assert_eq!(count(&script, "claim"), 2);
    }

    #[tokio::test]
    async fn consume_for_another_vault_is_rejected_before_activation() {
        let script = Shared::default();
        script.lock().unwrap().consume_vault = Some("99999999-2222-4333-8444-555555555555");
        let state = claimed(&script).await;
        script.lock().unwrap().approved = true;
        assert_eq!(state.status().await.unwrap().state, "confirm");
        let error = state
            .confirm(|_| async { panic!("a mismatched vault must never be activated") })
            .await
            .unwrap_err();
        assert_eq!(error.code, "join-consume-invalid");
    }
}
