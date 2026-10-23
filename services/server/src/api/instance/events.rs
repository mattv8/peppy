use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Extension, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use futures_util::stream;
use peppy_hosted_api::enrollment::InstanceEvent;
use tokio::sync::{OwnedSemaphorePermit, watch};

use super::requests::request_summaries;
use super::{ApiError, ApiResult, ApiState, require_admin};

const MAX_SESSION_STREAMS: usize = 4;
const STREAM_LIFETIME: Duration = Duration::from_secs(30 * 60);

pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/v1/instance/events", get(events))
}

struct SessionStreamGuard {
    sessions: Arc<std::sync::Mutex<std::collections::HashMap<Vec<u8>, usize>>>,
    key: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for SessionStreamGuard {
    fn drop(&mut self) {
        let mut sessions = self
            .sessions
            .lock()
            .expect("event stream mutex not poisoned");
        if let Some(count) = sessions.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                sessions.remove(&self.key);
            }
        }
    }
}

async fn events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    shutdown: Option<Extension<watch::Receiver<bool>>>,
) -> ApiResult<Response> {
    let session = require_admin(&state, &headers, false).await?;
    let (token, _) = super::super::instance_session_token(&headers)
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "invalid_session"))?;
    let key = crate::instance::digest(token.as_bytes()).to_vec();
    let permit = state
        .event_stream_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::TOO_MANY_REQUESTS, "event_stream_limited"))?;
    {
        let mut sessions = state
            .event_stream_sessions
            .lock()
            .expect("event stream mutex not poisoned");
        let count = sessions.entry(key.clone()).or_default();
        if *count >= MAX_SESSION_STREAMS {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "event_stream_limited",
            ));
        }
        *count += 1;
    }
    let guard = SessionStreamGuard {
        sessions: state.event_stream_sessions.clone(),
        key,
        _permit: permit,
    };
    let events = state.instance_events.subscribe();
    let shutdown = shutdown.map(|Extension(receiver)| receiver);
    let initial = request_summaries(&state).await?;
    let stream_state = (
        state,
        session.vault_id,
        crate::instance::digest(token.as_bytes()).to_vec(),
        events,
        shutdown,
        tokio::time::Instant::now() + STREAM_LIFETIME,
        Some(initial),
        Some(guard),
    );
    let stream = stream::unfold(
        stream_state,
        |(state, vault_id, token_digest, mut events, mut shutdown, deadline, initial, guard)| async move {
            let snapshot = match initial {
                Some(requests) => Some(requests),
                None => {
                    if tokio::time::Instant::now() >= deadline {
                        return None;
                    }
                    let wake = async {
                        tokio::select! {
                            _ = events.recv() => {},
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                            _ = tokio::time::sleep_until(deadline) => {},
                            _ = async { match &mut shutdown { Some(receiver) => { let _ = receiver.changed().await; }, None => std::future::pending::<()>().await } } => {},
                        }
                    };
                    wake.await;
                    if tokio::time::Instant::now() >= deadline {
                        return None;
                    }
                    if shutdown.as_ref().is_some_and(|receiver| *receiver.borrow()) {
                        return None;
                    }
                    let still_admin: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM instance_sessions sessions JOIN vault_owners owners ON owners.vault_id=sessions.vault_id WHERE sessions.token_digest=$1 AND sessions.vault_id=$2 AND sessions.revoked_at IS NULL AND sessions.expires_at>now() AND owners.is_admin)").bind(&token_digest).bind(vault_id).fetch_one(&state.db).await.unwrap_or(false);
                    if !still_admin {
                        return None;
                    }
                    request_summaries(&state).await.ok()
                }
            }?;
            let data =
                serde_json::to_string(&InstanceEvent::Snapshot { requests: snapshot }).ok()?;
            Some((
                Ok::<Event, Infallible>(Event::default().event("snapshot").data(data)),
                (
                    state,
                    vault_id,
                    token_digest,
                    events,
                    shutdown,
                    deadline,
                    None,
                    guard,
                ),
            ))
        },
    );
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}
