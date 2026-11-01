//! Native transport for the account-access notice. The notice decision is owned by
//! `peppy_hosted_client::account_access`; this file only performs the device-authenticated
//! GET and maps typed outcomes for the renderer.
use crate::{
    error::{BridgeError, BridgeResult},
    hosted::hosted_origin,
    session::Session,
};
use peppy_desktop_api::account_access::{AccountAccessResult, account_access_result_from_response};
use peppy_hosted_client::account_access::{self as codec, MAX_ACCOUNT_ACCESS_RESPONSE_JSON};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Origins that may host the notice's action URL: the device's API origin and the hosted
/// account origin used by the billing handoff. The account origin may be unavailable on
/// hosts without hosted configuration; the notice then simply has no action.
fn trusted_origins(session: &Session) -> Vec<String> {
    let mut origins = vec![session.api.origin().to_owned()];
    origins.extend(hosted_origin().ok());
    origins
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default()
}

pub async fn status(session: Arc<Session>) -> BridgeResult<AccountAccessResult> {
    let generation = session.generation.clone();
    let request = codec::build_device_account_access_request();
    let (status, body) = session
        .api
        .retention_request(
            request.method,
            &request.relative_path,
            None,
            None,
            MAX_ACCOUNT_ACCESS_RESPONSE_JSON + 1,
        )
        .await?;
    if status == 401 {
        return Err(BridgeError::new(
            "revoked",
            "The server rejected this device credential (revoked or invalid).",
        ));
    }
    if session.cancel.is_cancelled() || session.generation != generation {
        return Ok(AccountAccessResult::TemporarilyUnavailable);
    }
    let Ok(body) = String::from_utf8(body) else {
        return Ok(AccountAccessResult::TemporarilyUnavailable);
    };
    Ok(account_access_result_from_response(
        status,
        &body,
        now_unix(),
        &trusted_origins(&session),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contacts::tests::mock_server,
        tests::{fixture_at, open},
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_only_response_maps_to_a_notice_without_untrusted_action_url() {
        let (origin, seen) = mock_server(|method, path, _| {
            assert_eq!((method, path), ("GET", "/v1/account/access"));
            (
                200,
                r#"{"access":"read_only","state":"read_only","reason":"past_due","deadline":null,"manage_url":"https://evil.example/account"}"#.into(),
            )
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        let result = status(session).await.unwrap();

        assert_eq!(seen.lock().unwrap().len(), 1);
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["kind"], "notice");
        assert_eq!(value["notice"]["kind"], "read_only_past_due");
        assert_eq!(value["notice"]["severity"], "error");
        assert!(value["notice"]["manageUrl"].is_null());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ok_state_maps_to_none_and_legacy_routes_to_unsupported() {
        let (origin, _seen) = mock_server(|_, _, _| {
            (200, r#"{"access":"read_write","state":"ok","reason":null,"deadline":null,"manage_url":null}"#.into())
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        assert_eq!(status(session).await.unwrap(), AccountAccessResult::None);

        let (origin, _seen) = mock_server(|_, _, _| (404, "{}".into())).await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        assert_eq!(
            status(session).await.unwrap(),
            AccountAccessResult::Unsupported
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_failure_and_malformed_body_are_temporarily_unavailable() {
        let (origin, _seen) = mock_server(|_, _, _| (503, "{}".into())).await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        assert_eq!(
            status(session).await.unwrap(),
            AccountAccessResult::TemporarilyUnavailable
        );

        let (origin, _seen) = mock_server(|_, _, _| (200, "not json".into())).await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        assert_eq!(
            status(session).await.unwrap(),
            AccountAccessResult::TemporarilyUnavailable
        );
    }
}
