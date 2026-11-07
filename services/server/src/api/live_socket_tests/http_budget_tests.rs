use super::*;

#[tokio::test]
async fn http_policy_and_pool_checkout_share_the_original_entry_budget() {
    let policy_gate = LiveTicketTestGate::new(1);
    let policy = Arc::new(GatedPolicy::new(policy_gate.clone()));
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.policy = policy.clone();
    options.max_connections = 2;
    options.ticket_hooks.http_operation_budget = Some(Duration::from_millis(250));
    let server = LiveTestServer::start_with_options(options).await;

    policy.enable();
    let _ = server.snapshot().await;
    let before = live_ticket_device_snapshot(&server).await;
    let base_url = server.base_url.clone();
    let token = server.owner_token.clone();
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{base_url}/v1/live/tickets"))
            .bearer_auth(token)
            .json(&serde_json::json!({"session_id": URL_SAFE_NO_PAD.encode([115_u8; 16])}))
            .send()
            .await
            .unwrap()
    });
    bounded(policy_gate.wait_for_entries(1)).await;
    let first = server.pool.acquire().await.unwrap();
    let second = server.pool.acquire().await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    policy_gate.release_one();
    bounded(server.state.live_ticket_work.wait_for_checkout_waiters(1)).await;

    let response = tokio::time::timeout(Duration::from_millis(180), request)
        .await
        .expect("pool checkout received a fresh HTTP budget")
        .unwrap();
    drop(first);
    drop(second);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        live_ticket_device_snapshot(&server).await,
        before,
        "expired HTTP work wrote durable ticket authority"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn http_issue_samples_all_expiries_after_a_delayed_device_lock() {
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks.http_operation_budget = Some(Duration::from_secs(2));
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([116_u8; 16]);
    let mut blocker = server.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();

    let base_url = server.base_url.clone();
    let token = server.owner_token.clone();
    let request_session_id = session_id.clone();
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{base_url}/v1/live/tickets"))
            .bearer_auth(token)
            .json(&serde_json::json!({"session_id": request_session_id}))
            .send()
            .await
            .unwrap()
    });
    bounded(async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM pg_stat_activity
                    WHERE $1 = ANY(pg_blocking_pids(pid))
                )",
            )
            .bind(blocker_pid)
            .fetch_one(&server.admin)
            .await
            .unwrap();
            if blocked {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let while_locked: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
        .fetch_one(&server.admin)
        .await
        .unwrap();
    blocker.rollback().await.unwrap();

    let issued = bounded(request).await.unwrap();
    assert_eq!(issued.status(), StatusCode::CREATED);
    let issued_body = bounded(issued.json::<serde_json::Value>()).await.unwrap();
    let handle = issued_body["lifetime_handle"].as_str().unwrap();
    let initial_expiry_matches_single_post_lock_sample: bool = sqlx::query_scalar(
        "SELECT s.created_at=t.issued_at
             AND t.expires_at=t.issued_at+interval '30 seconds'
             AND s.lease_until=t.issued_at+interval '90 seconds'
             AND t.issued_at>$3::timestamptz
         FROM live_ticket_sessions s
         JOIN live_tickets t
           ON t.vault_id=s.vault_id
          AND t.device_id=s.device_id
          AND t.session_id_hash=s.session_id_hash
          AND t.session_incarnation=s.session_incarnation
         WHERE s.vault_id=$1 AND s.device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .bind(while_locked)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(
        initial_expiry_matches_single_post_lock_sample,
        "issue used a pre-lock or separately sampled expiry clock"
    );
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::POST,
            lifetime_body(&session_id, handle),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    let reconnect_expiry_matches_its_single_sample: bool = sqlx::query_scalar(
        "SELECT t.expires_at=t.issued_at+interval '30 seconds'
             AND s.lease_until=t.issued_at+interval '90 seconds'
         FROM live_ticket_sessions s
         JOIN live_tickets t
           ON t.vault_id=s.vault_id
          AND t.device_id=s.device_id
          AND t.session_id_hash=s.session_id_hash
          AND t.session_incarnation=s.session_incarnation
         WHERE s.vault_id=$1 AND s.device_id=$2
         ORDER BY t.issued_at DESC
         LIMIT 1",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(
        reconnect_expiry_matches_its_single_sample,
        "reconnect recomputed a ticket expiry or lease from a separate clock sample"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn abandoned_http_work_after_begin_executes_no_next_authority_statement() {
    let gate = LiveTicketTestGate::new(1);
    let next_statement_ran = Arc::new(AtomicBool::new(false));
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let pool = server.pool.clone();
    let work_registry = server.state.live_ticket_work.clone();
    let next_statement = next_statement_ran.clone();
    let workflow_gate = gate.clone();
    let (next_stage_sender, next_stage_receiver) = oneshot::channel();
    let workflow: TicketWorkflow<ApiResult<()>> = Box::new(move |scope| {
        Box::pin(async move {
            scope.stage().await?.execute("BEGIN").await?;
            workflow_gate.enter().await;
            let next_stage = scope.stage().await;
            let can_run_next_statement = next_stage.is_ok();
            let _ = next_stage_sender.send(can_run_next_statement);
            let connection = next_stage?;
            connection.execute("SELECT 1").await?;
            next_statement.store(true, Ordering::SeqCst);
            commit_ticket_http(scope).await?;
            Ok(Ok(()))
        })
    });
    let caller = tokio::spawn(async move {
        run_ticket_http_work(
            &work_registry,
            &pool,
            OperationDeadline::after(Duration::from_secs(2)),
            workflow,
        )
        .await
    });

    bounded(gate.wait_for_entries(1)).await;
    caller.abort();
    assert!(
        caller.await.is_err(),
        "caller abort must finish before gate release"
    );
    gate.release_one();
    assert!(
        !bounded(next_stage_receiver).await.unwrap_or(false),
        "cancelled caller passed the next authority-stage fence"
    );
    assert!(
        !next_statement_ran.load(Ordering::SeqCst),
        "cancelled caller allowed an authority statement after BEGIN"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn committed_work_held_past_deadline_cannot_return_a_successful_http_outcome() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let gate = LiveTicketTestGate::new(1);
    let committed = Arc::new(AtomicBool::new(false));
    let registry = server.state.live_ticket_work.clone();
    let pool = server.pool.clone();
    let workflow_gate = gate.clone();
    let workflow_committed = committed.clone();
    let caller = tokio::spawn(async move {
        let workflow: TicketWorkflow<ApiResult<&'static str>> = Box::new(move |scope| {
            Box::pin(async move {
                scope.stage().await?.execute("BEGIN").await?;
                scope.stage().await?.execute("SELECT 1").await?;
                commit_ticket_http(scope).await?;
                workflow_committed.store(true, Ordering::SeqCst);
                workflow_gate.enter().await;
                Ok(Ok("committed"))
            })
        });
        run_ticket_http_work(
            &registry,
            &pool,
            OperationDeadline::after(Duration::from_millis(100)),
            workflow,
        )
        .await
    });

    bounded(gate.wait_for_entries(1)).await;
    let outcome = bounded(caller).await.unwrap();
    assert!(committed.load(Ordering::SeqCst));
    assert_eq!(outcome.unwrap_err().0, StatusCode::SERVICE_UNAVAILABLE);
    gate.release_one();
    server.shutdown().await;
}

#[tokio::test]
async fn unused_ticket_remains_consumable_when_issuer_credential_is_revoked_but_another_is_active()
{
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([117_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let replacement_token = "replacement-live-ticket-credential";
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(Sha256::digest(replacement_token.as_bytes()).as_slice())
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE device_credentials SET revoked_at=clock_timestamp() WHERE token_digest=$1")
        .bind(Sha256::digest(server.owner_token.as_bytes()).as_slice())
        .execute(&server.pool)
        .await
        .unwrap();

    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    bounded(expect_strict_hello(&mut socket)).await;
    socket.close(None).await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn trusted_forwarded_peers_are_limited_independently_from_untrusted_spoofs() {
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.trusted_proxy_cidrs = vec!["127.0.0.1/32".parse().unwrap()];
    let server = LiveTestServer::start_with_options(options).await;
    let body = lifetime_body(
        &lifecycle_session(119),
        &encode_live_ticket_lifetime_handle(99),
    );
    let client = reqwest::Client::new();
    for _ in 0..30 {
        let response = client
            .delete(format!("{}/v1/live/tickets", server.base_url))
            .bearer_auth(&server.owner_token)
            .header("x-forwarded-for", "192.0.2.119")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    let shared_peer = client
        .delete(format!("{}/v1/live/tickets", server.base_url))
        .bearer_auth(&server.peer_token)
        .header("x-forwarded-for", "192.0.2.119")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(shared_peer.status(), StatusCode::TOO_MANY_REQUESTS);
    let distinct_peer = client
        .delete(format!("{}/v1/live/tickets", server.base_url))
        .bearer_auth(&server.peer_token)
        .header("x-forwarded-for", "192.0.2.120")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(distinct_peer.status(), StatusCode::CONFLICT);
    server.shutdown().await;
}
