use super::*;
use peppy_protocol::{
    MEMBERSHIP_PROTOCOL_VERSION, MembershipBootstrapV1, MembershipChallengeV1, MembershipControlV1,
    MembershipResponseV1, MembershipRoleV1,
};
use tokio_tungstenite::tungstenite::Message;

const CAPABILITY: &str = "membership-version-v1";

type LiveSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_message(socket: &mut LiveSocket) -> Message {
    tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("socket response timed out")
        .expect("socket closed without a frame")
        .expect("socket read failed")
}

async fn next_text(socket: &mut LiveSocket) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return text.to_string(),
                Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
                Message::Pong(_) => {}
                frame => panic!("expected text frame, got {frame:?}"),
            }
        }
    })
    .await
    .expect("socket did not receive a text frame")
}

async fn connect_capable(server: &LiveTestServer) -> (LiveSocket, MembershipBootstrapV1) {
    let mut request = server.ws_url.clone().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", server.owner_token).parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("x-peppy-live-capabilities", CAPABILITY.parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    expect_strict_hello(&mut socket).await;
    let MembershipControlV1::Bootstrap(bootstrap) =
        MembershipControlV1::parse(next_text(&mut socket).await.as_bytes()).unwrap()
    else {
        panic!("expected membership bootstrap");
    };
    (socket, bootstrap)
}

async fn challenge(
    socket: &mut LiveSocket,
    connection_id: &str,
    challenge_byte: u8,
) -> MembershipResponseV1 {
    let challenge_id = URL_SAFE_NO_PAD.encode([challenge_byte; 16]);
    let challenge = MembershipControlV1::Challenge(MembershipChallengeV1 {
        r#type: "membership_challenge".into(),
        version: MEMBERSHIP_PROTOCOL_VERSION,
        connection_id: connection_id.into(),
        challenge_id: challenge_id.clone(),
    });
    socket
        .send(Message::Text(
            serde_json::to_string(&challenge).unwrap().into(),
        ))
        .await
        .unwrap();
    let MembershipControlV1::Response(response) =
        MembershipControlV1::parse(next_text(socket).await.as_bytes()).unwrap()
    else {
        panic!("expected membership response");
    };
    assert_eq!(response.connection_id, connection_id);
    assert_eq!(response.challenge_id, challenge_id);
    response
}

fn version_text(version: &peppy_protocol::MembershipVersionV1) -> String {
    serde_json::to_value(version)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

async fn membership_version(server: &LiveTestServer) -> i64 {
    sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
        .bind(server.vault)
        .fetch_one(&server.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn capability_bearer_receives_bootstrap_and_current_challenge_response() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let (mut socket, bootstrap) = connect_capable(&server).await;
    let response = challenge(&mut socket, &bootstrap.connection_id, 0).await;
    assert_eq!(response.membership_version, bootstrap.membership_version);
    server.shutdown().await;
}

#[tokio::test]
async fn stale_challenge_protocol_version_is_refused_without_a_response() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let (mut socket, bootstrap) = connect_capable(&server).await;
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "membership_challenge",
                "version": 0,
                "connection_id": bootstrap.connection_id,
                "challenge_id": URL_SAFE_NO_PAD.encode([1_u8; 16]),
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let Message::Close(close) = next_message(&mut socket).await else {
        panic!("stale challenge leaked a non-close response");
    };
    let close = close.expect("server close must carry its fixed refusal");
    assert_eq!(u16::from(close.code), 1008);
    assert_eq!(close.reason, "invalid_live_frame");
    server.shutdown().await;
}

#[tokio::test]
async fn protected_membership_writer_inventory_bumps_the_challenge_version() {
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks.recheck_interval = Some(Duration::from_secs(5));
    let server = LiveTestServer::start_with_options(options).await;
    let (mut socket, bootstrap) = connect_capable(&server).await;
    let mut previous = membership_version(&server).await;

    let writes = [
        (
            "device enrollment",
            "UPDATE devices SET public_key = jsonb_build_object('writer', 'device_enroll') WHERE vault_id=$1 AND device_id=$2",
        ),
        (
            "instance revoke",
            "UPDATE devices SET revoked_at = now() WHERE vault_id=$1 AND device_id=$2",
        ),
        (
            "device/self/owner revoke",
            "UPDATE devices SET revoked_at = NULL WHERE vault_id=$1 AND device_id=$2",
        ),
        (
            "phone admission binding",
            "UPDATE device_credentials SET token_digest = token_digest || decode('01', 'hex') WHERE vault_id=$1 AND device_id=$2",
        ),
        (
            "computer admission binding",
            "UPDATE device_credentials SET token_digest = token_digest || decode('02', 'hex') WHERE vault_id=$1 AND device_id=$2",
        ),
        (
            "manual admission binding",
            "UPDATE device_credentials SET token_digest = token_digest || decode('03', 'hex') WHERE vault_id=$1 AND device_id=$2",
        ),
        (
            "role change",
            "UPDATE devices SET role = 'gateway' WHERE vault_id=$1 AND device_id=$2",
        ),
        (
            "signing identity change",
            "UPDATE devices SET public_key = jsonb_build_object('writer', 'signing_identity') WHERE vault_id=$1 AND device_id=$2",
        ),
    ];

    for (index, (writer, sql)) in writes.into_iter().enumerate() {
        sqlx::query(sql)
            .bind(server.vault)
            .bind(server.peer_device)
            .execute(&server.pool)
            .await
            .unwrap_or_else(|error| panic!("{writer} write failed: {error}"));
        let current = membership_version(&server).await;
        assert!(
            current > previous,
            "{writer} did not bump membership version"
        );
        let response = challenge(&mut socket, &bootstrap.connection_id, index as u8 + 2).await;
        assert_eq!(
            version_text(&response.membership_version),
            current.to_string(),
            "{writer} challenge returned a stale version"
        );
        previous = current;
    }
    server.shutdown().await;
}

#[tokio::test]
async fn challenge_revalidates_the_exact_bearer_credential() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let (mut socket, bootstrap) = connect_capable(&server).await;
    sqlx::query(
        "UPDATE device_credentials SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .execute(&server.pool)
    .await
    .unwrap();
    let challenge = MembershipControlV1::Challenge(MembershipChallengeV1 {
        r#type: "membership_challenge".into(),
        version: MEMBERSHIP_PROTOCOL_VERSION,
        connection_id: bootstrap.connection_id,
        challenge_id: URL_SAFE_NO_PAD.encode([10_u8; 16]),
    });
    socket
        .send(Message::Text(
            serde_json::to_string(&challenge).unwrap().into(),
        ))
        .await
        .unwrap();
    let Message::Close(close) = next_message(&mut socket).await else {
        panic!("revoked bearer received a membership response");
    };
    assert_eq!(close.unwrap().reason, "live_unavailable");
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_writer_cannot_mix_a_new_roster_with_an_old_version() {
    let gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        membership_after_version: Some(gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks;
    let server = LiveTestServer::start_with_options(options).await;
    let (mut socket, bootstrap) = connect_capable(&server).await;
    let old_version = membership_version(&server).await;
    let connection_id = bootstrap.connection_id.clone();
    let response_task = tokio::spawn(async move {
        let response = challenge(&mut socket, &connection_id, 11).await;
        (socket, response)
    });
    gate.wait_for_entries(2).await;
    sqlx::query("UPDATE devices SET role='gateway' WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.peer_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let new_version = membership_version(&server).await;
    assert!(new_version > old_version);
    gate.release_one();
    let (_, response) = response_task.await.unwrap();
    assert_eq!(
        version_text(&response.membership_version),
        old_version.to_string()
    );
    assert_eq!(
        response
            .roster
            .iter()
            .find(|device| device.device_id.0 == server.peer_device)
            .unwrap()
            .role,
        MembershipRoleV1::Device
    );
    server.shutdown().await;
}

#[tokio::test]
async fn unnegotiated_bearer_keeps_the_legacy_hello_and_refuses_controls() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "membership_challenge", "version": 1,
                "connection_id": URL_SAFE_NO_PAD.encode([12_u8; 16]),
                "challenge_id": URL_SAFE_NO_PAD.encode([13_u8; 16]),
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let close = loop {
        match next_message(&mut socket).await {
            Message::Close(close) => break close,
            Message::Ping(_) | Message::Pong(_) => continue,
            frame => panic!("legacy socket accepted a membership control: {frame:?}"),
        }
    };
    assert_eq!(close.unwrap().reason, "invalid_live_frame");
    server.shutdown().await;
}

#[tokio::test]
async fn membership_exchange_logs_no_credentials_or_membership_contents() {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let secrets = [
        server.owner_token.clone(),
        server.vault.to_string(),
        server.owner_device.to_string(),
        server.peer_device.to_string(),
    ];
    let (mut socket, bootstrap) = connect_capable(&server).await;
    let _ = challenge(&mut socket, &bootstrap.connection_id, 14).await;
    server.shutdown().await;
    let logs = captured.text();
    for secret in secrets {
        assert!(
            !logs.contains(&secret),
            "membership exchange logged secret roster material"
        );
    }
    assert!(!logs.contains(&bootstrap.connection_id));
}

#[tokio::test]
async fn negotiated_socket_multiplexes_membership_and_typing_in_both_directions() {
    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let (mut capable, bootstrap) = connect_capable(&server).await;
    let mut peer = server.connect_peer().await;
    expect_strict_hello(&mut peer).await;
    let _ = challenge(&mut capable, &bootstrap.connection_id, 21).await;

    let owner_frame = signal(server.owner_device);
    capable
        .send(Message::Text(owner_frame.clone().into()))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&next_text(&mut peer).await).unwrap(),
        serde_json::from_str::<serde_json::Value>(&owner_frame).unwrap()
    );

    let peer_frame = signal(server.peer_device);
    peer.send(Message::Text(peer_frame.clone().into()))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&next_text(&mut capable).await).unwrap(),
        serde_json::from_str::<serde_json::Value>(&peer_frame).unwrap()
    );
    wait_for_count(&backend.completed, 2).await;
    server.shutdown().await;
}

#[tokio::test]
async fn membership_challenge_spam_is_limited_before_extra_database_work() {
    let gate = LiveTicketTestGate::new(0);
    let hooks = LiveTicketTestHooks {
        membership_after_version: Some(gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks;
    options.ticket_hooks.recheck_interval = Some(Duration::from_secs(5));
    let server = LiveTestServer::start_with_options(options).await;
    *server
        .state
        .live_rate_limiter
        .lock()
        .expect("live limiter lock") = crate::live::LiveRateLimiter::with_limit(1);
    let (mut socket, bootstrap) = connect_capable(&server).await;
    let _ = challenge(&mut socket, &bootstrap.connection_id, 22).await;
    let second = MembershipControlV1::Challenge(MembershipChallengeV1 {
        r#type: "membership_challenge".into(),
        version: MEMBERSHIP_PROTOCOL_VERSION,
        connection_id: bootstrap.connection_id,
        challenge_id: URL_SAFE_NO_PAD.encode([23_u8; 16]),
    });
    socket
        .send(Message::Text(
            serde_json::to_string(&second).unwrap().into(),
        ))
        .await
        .unwrap();
    let close = loop {
        match next_message(&mut socket).await {
            Message::Close(close) => break close,
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            Message::Pong(_) => {}
            frame => panic!("rate-limited challenge produced {frame:?}"),
        }
    };
    assert_eq!(close.unwrap().reason, "live_rate_limited");
    assert_eq!(
        gate.entries(),
        2,
        "bootstrap plus one admitted challenge only"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn membership_challenge_database_stall_is_bounded_by_one_operation_deadline() {
    let gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_millis(500)),
        membership_after_version: Some(gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks;
    let server = LiveTestServer::start_with_options(options).await;
    let (mut socket, bootstrap) = connect_capable(&server).await;
    let pending = MembershipControlV1::Challenge(MembershipChallengeV1 {
        r#type: "membership_challenge".into(),
        version: MEMBERSHIP_PROTOCOL_VERSION,
        connection_id: bootstrap.connection_id,
        challenge_id: URL_SAFE_NO_PAD.encode([24_u8; 16]),
    });
    socket
        .send(Message::Text(
            serde_json::to_string(&pending).unwrap().into(),
        ))
        .await
        .unwrap();
    gate.wait_for_entries(2).await;
    let Message::Close(close) = next_message(&mut socket).await else {
        panic!("stalled membership challenge was not closed");
    };
    assert_eq!(close.unwrap().reason, "live_unavailable");
    server.shutdown().await;
}

#[tokio::test]
async fn membership_failure_logs_one_sanitized_operation_and_kind() {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let secrets = [
        server.owner_token.clone(),
        server.vault.to_string(),
        server.owner_device.to_string(),
        server.peer_device.to_string(),
    ];
    let (mut socket, bootstrap) = connect_capable(&server).await;
    sqlx::query(
        "UPDATE device_credentials SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .execute(&server.pool)
    .await
    .unwrap();
    let request = MembershipControlV1::Challenge(MembershipChallengeV1 {
        r#type: "membership_challenge".into(),
        version: MEMBERSHIP_PROTOCOL_VERSION,
        connection_id: bootstrap.connection_id.clone(),
        challenge_id: URL_SAFE_NO_PAD.encode([31_u8; 16]),
    });
    socket
        .send(Message::Text(
            serde_json::to_string(&request).unwrap().into(),
        ))
        .await
        .unwrap();
    let _ = next_message(&mut socket).await;
    server.shutdown().await;

    let logs = captured.text();
    let events: Vec<_> = logs
        .lines()
        .filter(|line| line.contains("live membership snapshot failed"))
        .collect();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("operation=\"live_membership_snapshot\""));
    assert!(events[0].contains("kind=\"inactive_credential\""));
    for secret in secrets {
        assert!(!events[0].contains(&secret));
    }
    assert!(!events[0].contains(&bootstrap.connection_id));
    for forbidden_field in [
        "vault_id",
        "device_id",
        "connection_id",
        "challenge_id",
        "roster",
        "token",
        "error=",
    ] {
        assert!(!events[0].contains(forbidden_field));
    }
}
