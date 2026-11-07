use super::*;

struct ReplicaServer {
    pool: PgPool,
    hub: crate::live::LiveHub,
    backend: Arc<crate::live_postgres::PgLiveBackend>,
    registry: TicketDbWorkRegistry,
    base_url: String,
    ws_url: String,
    task: JoinHandle<()>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    lost_controls: Option<Arc<AtomicUsize>>,
}

impl ReplicaServer {
    async fn start(base: &LiveTestServer, lose_controls: bool) -> Self {
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
        Self::start_with_database_url(base, lose_controls, &database_url).await
    }

    async fn start_with_database_url(
        base: &LiveTestServer,
        lose_controls: bool,
        database_url: &str,
    ) -> Self {
        let pool = replica_pool(database_url, &base.schema).await;
        let hub = crate::live::LiveHub::new();
        let backend = Arc::new(crate::live_postgres::PgLiveBackend::new(
            pool.clone(),
            hub.clone(),
        ));
        wait_for_backend(&backend).await;
        let lost_controls = lose_controls.then(|| Arc::new(AtomicUsize::new(0)));

        let registry = TicketDbWorkRegistry::new();
        let state = ApiState {
            db: pool.clone(),
            live_hub: hub.clone(),
            live_backend: backend.clone(),
            live_backend_override: lost_controls.as_ref().map(|controls| {
                Arc::new(LostControlBackend {
                    backend: backend.clone(),
                    controls: controls.clone(),
                }) as Arc<dyn crate::live::LiveBackend>
            }),
            live_readiness: backend.subscribe_readiness(),
            live_ticket_work: registry.clone(),
            live_rate_limiter: Arc::new(Mutex::new(crate::live::LiveRateLimiter::default())),
            live_upgrade_limiter: Arc::new(Mutex::new(crate::live::LiveRateLimiter::with_limit(
                crate::live::LIVE_UPGRADE_RATE_LIMIT,
            ))),
            live_ticket_http_limiter: Arc::new(Mutex::new(
                crate::live::LiveRateLimiter::with_limit(30),
            )),
            ..base.state.clone()
        };
        let router = axum::Router::new()
            .route("/v1/live", axum::routing::get(live_websocket))
            .route(
                "/v1/live/tickets",
                axum::routing::post(issue_live_ticket).delete(close_live_ticket_session),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            crate::serving::serve(listener, router, async move {
                let _ = shutdown_receiver.await;
            })
            .await
            .unwrap();
        });
        Self {
            pool,
            hub,
            backend,
            registry,
            base_url: format!("http://{address}"),
            ws_url: format!("ws://{address}/v1/live"),
            task,
            shutdown: Mutex::new(Some(shutdown_sender)),
            lost_controls,
        }
    }

    async fn connect_with_ticket(
        &self,
        session_id: &str,
        ticket: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let protocol = live_ticket_protocol(session_id, ticket);
        let mut request = self.ws_url.clone().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "https://web.peppy.test".parse().unwrap());
        request
            .headers_mut()
            .insert("sec-websocket-protocol", protocol.parse().unwrap());
        let (socket, response) = connect_async(request).await.unwrap();
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            &protocol
        );
        socket
    }

    async fn close_ticket(
        &self,
        token: &str,
        session_id: &str,
        lifetime_handle: &str,
    ) -> reqwest::Response {
        bounded(
            reqwest::Client::new()
                .delete(format!("{}/v1/live/tickets", self.base_url))
                .bearer_auth(token)
                .json(&lifetime_body(session_id, lifetime_handle))
                .send(),
        )
        .await
        .unwrap()
    }

    async fn issue_status(&self, token: &str, session_id: &str) -> reqwest::StatusCode {
        bounded(
            reqwest::Client::new()
                .post(format!("{}/v1/live/tickets", self.base_url))
                .bearer_auth(token)
                .json(&serde_json::json!({"session_id": session_id}))
                .send(),
        )
        .await
        .unwrap()
        .status()
    }

    async fn shutdown(self) {
        if let Some(sender) = self.shutdown.lock().unwrap().take() {
            let _ = sender.send(());
        }
        self.task.abort();
        let _ = self.task.await;
        self.pool.close().await;
    }

    fn begin_shutdown(&self) {
        if let Some(sender) = self.shutdown.lock().unwrap().take() {
            let _ = sender.send(());
        }
    }
}

async fn replica_pool(database_url: &str, schema: &str) -> PgPool {
    let mut isolated: url::Url = database_url.parse().unwrap();
    isolated
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    PgPoolOptions::new()
        .max_connections(2)
        .connect(isolated.as_str())
        .await
        .unwrap()
}

struct LostControlBackend {
    backend: Arc<crate::live_postgres::PgLiveBackend>,
    controls: Arc<AtomicUsize>,
}

impl crate::live::LiveBackend for LostControlBackend {
    fn publish(
        &self,
        vault_id: VaultId,
        frame: EphemeralSignalV2,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::live::LiveBackendError>> + Send + '_>> {
        self.backend.publish(vault_id, frame)
    }

    fn publish_control(
        &self,
        _vault_id: VaultId,
        _device_id: DeviceId,
        _ticket: crate::live::LiveTicketIdentity,
        _kind: crate::live::LiveControlKind,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::live::LiveBackendError>> + Send + '_>> {
        self.controls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

async fn wait_for_backend(backend: &crate::live_postgres::PgLiveBackend) {
    let mut readiness = backend.subscribe_readiness();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !*readiness.borrow_and_update() {
            readiness
                .changed()
                .await
                .expect("replica backend remains alive");
        }
    })
    .await
    .expect("replica listener registered");
}

async fn active_ticket(base: &LiveTestServer, session_id: &str) -> crate::live::LiveTicketIdentity {
    let hash = Sha256::digest(URL_SAFE_NO_PAD.decode(session_id).unwrap()).to_vec();
    let (incarnation, connection): (i64, i64) = sqlx::query_as(
        "SELECT session_incarnation, active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3",
    )
    .bind(base.vault)
    .bind(base.owner_device)
    .bind(hash)
    .fetch_one(&base.pool)
    .await
    .unwrap();
    crate::live::LiveTicketIdentity::new(
        URL_SAFE_NO_PAD
            .decode(session_id)
            .unwrap()
            .try_into()
            .unwrap(),
        incarnation,
        connection,
    )
}

async fn retained_message_counts(pool: &PgPool) -> (i64, i64, i64) {
    (
        sqlx::query_scalar("SELECT count(*) FROM event_log")
            .fetch_one(pool)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT count(*) FROM encrypted_records")
            .fetch_one(pool)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT count(*) FROM device_wake_jobs")
            .fetch_one(pool)
            .await
            .unwrap(),
    )
}

async fn next_termination(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
) -> Option<CloseFrame> {
    loop {
        match tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("socket did not terminate")
        {
            Some(Ok(tungstenite::Message::Close(frame))) => return frame,
            Some(Ok(tungstenite::Message::Ping(payload))) => socket
                .send(tungstenite::Message::Pong(payload))
                .await
                .unwrap(),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

async fn terminal_without_data(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
) {
    loop {
        match socket.next().await {
            Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => return,
            Some(Ok(tungstenite::Message::Ping(payload))) => socket
                .send(tungstenite::Message::Pong(payload))
                .await
                .unwrap(),
            Some(Ok(message)) => panic!("socket produced data after shutdown: {message:?}"),
        }
    }
}

async fn wait_for_a_listener_control_barrier(
    replica_a: &ReplicaServer,
    replica_b: &ReplicaServer,
    vault: Uuid,
) {
    let marker = crate::live::LiveTicketIdentity::new([251; 16], 1, 1);
    let device = DeviceId(Uuid::new_v4());
    let mut registration =
        replica_a
            .hub
            .register_ticket_with_identity(VaultId(vault), device, marker);
    crate::live::LiveBackend::publish_control(
        &*replica_b.backend,
        VaultId(vault),
        device,
        marker,
        crate::live::LiveControlKind::Close,
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), registration.receiver.recv())
            .await
            .expect("A listener did not process the control barrier"),
        None,
        "A listener control barrier must cancel its exact marker"
    );
}

fn spawn_replica_ticket_connect(
    replica: &ReplicaServer,
    session_id: String,
    ticket: String,
) -> JoinHandle<
    Result<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
        tungstenite::Error,
    >,
> {
    let ws_url = replica.ws_url.clone();
    tokio::spawn(async move {
        let protocol = live_ticket_protocol(&session_id, &ticket);
        let mut request = ws_url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "https://web.peppy.test".parse().unwrap());
        request
            .headers_mut()
            .insert("sec-websocket-protocol", protocol.parse().unwrap());
        connect_async(request).await.map(|(socket, _)| socket)
    })
}

#[tokio::test]
async fn replica_delete_closes_the_exact_remote_ticket_socket() {
    let base = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let replica_a = ReplicaServer::start(&base, false).await;
    let replica_b = ReplicaServer::start(&base, false).await;
    let session_id = lifecycle_session(201);
    let (ticket, handle) = issue_ticket_with_handle(&base, session_id.clone()).await;
    let mut socket = replica_a.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    let held_one = replica_a.pool.acquire().await.unwrap();
    let held_two = replica_a.pool.acquire().await.unwrap();

    assert_eq!(
        replica_b
            .close_ticket(&base.owner_token, &session_id, &handle)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let closed = tokio::time::timeout(Duration::from_millis(500), next_termination(&mut socket))
        .await
        .expect("real control must cancel before the blocked durable fallback can query");
    assert!(
        closed.is_none_or(|frame| {
            matches!(
                frame.code,
                tungstenite::protocol::frame::coding::CloseCode::Normal
                    | tungstenite::protocol::frame::coding::CloseCode::Policy
            )
        }),
        "control termination used an unexpected close code"
    );
    drop((held_one, held_two));

    replica_b.shutdown().await;
    replica_a.shutdown().await;
    base.shutdown().await;
}

#[tokio::test]
async fn replica_lost_control_with_healthy_listener_uses_durable_ticket_fallback() {
    let base = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let replica_a = ReplicaServer::start(&base, false).await;
    let replica_b = ReplicaServer::start(&base, true).await;
    let session_id = lifecycle_session(202);
    let (ticket, handle) = issue_ticket_with_handle(&base, session_id.clone()).await;
    let mut socket = replica_a.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    assert!(
        replica_a.backend.is_ready(),
        "listener remains healthy before close"
    );

    assert_eq!(
        replica_b
            .close_ticket(&base.owner_token, &session_id, &handle)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert_eq!(
        replica_b
            .lost_controls
            .as_ref()
            .unwrap()
            .load(Ordering::SeqCst),
        1,
        "the test backend suppressed exactly the close notification"
    );
    // cfg(test) rechecks every 50ms; one second allows the check and bounded close
    // without exercising the production 20s tick / 5s validation ceiling.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), next_close(&mut socket))
            .await
            .expect("durable fallback should close within the test recheck budget")
            .code,
        tungstenite::protocol::frame::coding::CloseCode::Policy
    );
    assert!(
        replica_a.backend.is_ready(),
        "fallback, not listener loss, closed the socket"
    );
    assert_eq!(
        retained_message_counts(&replica_a.pool).await,
        (0, 0, 0),
        "control notifications must not persist messages or wakes"
    );

    replica_b.shutdown().await;
    replica_a.shutdown().await;
    base.shutdown().await;
}

#[tokio::test]
async fn replica_stale_control_cannot_close_a_replacement_socket() {
    let base = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let replica_a = ReplicaServer::start(&base, false).await;
    let replica_b = ReplicaServer::start(&base, false).await;
    let old_session = lifecycle_session(203);
    let (old_ticket, old_handle) = issue_ticket_with_handle(&base, old_session.clone()).await;
    let mut old_socket = replica_a
        .connect_with_ticket(&old_session, &old_ticket)
        .await;
    expect_strict_hello(&mut old_socket).await;
    let old_identity = active_ticket(&base, &old_session).await;

    assert_eq!(
        replica_b
            .close_ticket(&base.owner_token, &old_session, &old_handle)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let _ = next_termination(&mut old_socket).await;
    sqlx::query(
        "DELETE FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3",
    )
    .bind(base.vault)
    .bind(base.owner_device)
    .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&old_session).unwrap()).to_vec())
    .execute(&base.pool)
    .await
    .unwrap();

    let replacement_session = old_session.clone();
    let (replacement_ticket, _) =
        issue_ticket_with_handle(&base, replacement_session.clone()).await;
    let mut replacement = replica_a
        .connect_with_ticket(&replacement_session, &replacement_ticket)
        .await;
    expect_strict_hello(&mut replacement).await;

    crate::live::LiveBackend::publish_control(
        &*replica_b.backend,
        VaultId(base.vault),
        DeviceId(base.owner_device),
        old_identity,
        crate::live::LiveControlKind::Close,
    )
    .await
    .unwrap();
    wait_for_a_listener_control_barrier(&replica_a, &replica_b, base.vault).await;
    assert!(
        replica_a
            .hub
            .ticket_generation(
                VaultId(base.vault),
                DeviceId(base.owner_device),
                URL_SAFE_NO_PAD
                    .decode(&replacement_session)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            )
            .is_some(),
        "stale control removed the replacement registration"
    );

    replica_b.shutdown().await;
    replica_a.shutdown().await;
    base.shutdown().await;
}

#[tokio::test]
async fn replica_same_incarnation_stale_connection_control_preserves_successor() {
    let base = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let replica_a = ReplicaServer::start(&base, false).await;
    let replica_b = ReplicaServer::start(&base, false).await;
    let session = lifecycle_session(211);
    let (old_ticket, handle) = issue_ticket_with_handle(&base, session.clone()).await;
    let mut old_socket = replica_a.connect_with_ticket(&session, &old_ticket).await;
    expect_strict_hello(&mut old_socket).await;
    let old_identity = active_ticket(&base, &session).await;

    let replacement_ticket = reconnect_ticket(&base, &session, &handle).await;
    let mut successor = replica_b
        .connect_with_ticket(&session, &replacement_ticket)
        .await;
    expect_strict_hello(&mut successor).await;
    let successor_identity = active_ticket(&base, &session).await;
    assert_eq!(
        successor_identity.session_incarnation(),
        old_identity.session_incarnation(),
        "replacement remains in the same durable incarnation"
    );
    assert_ne!(
        successor_identity.connection_id(),
        old_identity.connection_id()
    );
    let _ = next_termination(&mut old_socket).await;

    crate::live::LiveBackend::publish_control(
        &*replica_a.backend,
        VaultId(base.vault),
        DeviceId(base.owner_device),
        old_identity,
        crate::live::LiveControlKind::Close,
    )
    .await
    .unwrap();
    wait_for_a_listener_control_barrier(&replica_b, &replica_a, base.vault).await;
    assert!(
        replica_b
            .hub
            .ticket_generation(
                VaultId(base.vault),
                DeviceId(base.owner_device),
                URL_SAFE_NO_PAD
                    .decode(&session)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            )
            .is_some(),
        "stale C control closed the same-incarnation successor"
    );

    replica_b.shutdown().await;
    replica_a.shutdown().await;
    base.shutdown().await;
}

#[tokio::test]
async fn replica_pool_outage_still_observes_shutdown_without_a_fresh_hello() {
    let base = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let replica_a = ReplicaServer::start(&base, false).await;
    let replica_b = ReplicaServer::start(&base, true).await;
    let session_id = lifecycle_session(205);
    let (ticket, handle) = issue_ticket_with_handle(&base, session_id.clone()).await;
    let mut socket = replica_a.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    let checkout_waiters = replica_a.registry.checkout_waiter_count();
    let held_one = replica_a.pool.acquire().await.unwrap();
    let held_two = replica_a.pool.acquire().await.unwrap();
    assert_eq!(
        replica_b
            .close_ticket(&base.owner_token, &session_id, &handle)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    bounded(
        replica_a
            .registry
            .wait_for_checkout_waiters(checkout_waiters + 1),
    )
    .await;
    replica_a.begin_shutdown();
    tokio::time::timeout(Duration::from_secs(1), terminal_without_data(&mut socket))
        .await
        .expect("shutdown must interrupt a fallback blocked on the replica pool");
    drop((held_one, held_two));

    replica_b.shutdown().await;
    replica_a.shutdown().await;
    base.shutdown().await;
}

#[tokio::test]
async fn replica_commit_ack_loss_never_installs_a_late_socket() {
    let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
    let proxy = DelayedCommitProxy::start_holding_acknowledgement(&database_url).await;
    let options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    let base = LiveTestServer::start_with_options(options).await;
    let replica_a = ReplicaServer::start(&base, false).await;
    let replica_b = ReplicaServer::start_with_database_url(&base, true, &proxy.proxy_url).await;
    sqlx::query("SELECT 1")
        .execute(&replica_b.pool)
        .await
        .expect("proxy-backed replica pool must be warm before the bounded request");
    let old_session = lifecycle_session(206);
    let (old_ticket, old_handle) = issue_ticket_with_handle(&base, old_session.clone()).await;
    let mut old_socket = replica_a
        .connect_with_ticket(&old_session, &old_ticket)
        .await;
    expect_strict_hello(&mut old_socket).await;
    let old_identity = active_ticket(&base, &old_session).await;

    let replacement_session = old_session.clone();
    let replacement_ticket = reconnect_ticket(&base, &replacement_session, &old_handle).await;
    proxy.arm();
    let connect =
        spawn_replica_ticket_connect(&replica_b, replacement_session.clone(), replacement_ticket);
    bounded(proxy.wait_for_acknowledgement()).await;
    let replacement_identity = active_ticket(&base, &replacement_session).await;
    assert_ne!(
        replacement_identity.connection_id(),
        old_identity.connection_id(),
        "PostgreSQL committed replacement C before its acknowledgement was released"
    );
    connect.abort();
    assert!(connect.await.unwrap_err().is_cancelled());
    bounded(replica_b.registry.wait_for_cancellations(1)).await;
    assert_eq!(
        replica_b.hub.ticket_generation(
            VaultId(base.vault),
            DeviceId(base.owner_device),
            URL_SAFE_NO_PAD
                .decode(&replacement_session)
                .unwrap()
                .try_into()
                .unwrap(),
        ),
        None,
        "late COMMIT must not register a socket after its HTTP caller settled"
    );
    assert_eq!(
        replica_b
            .lost_controls
            .as_ref()
            .unwrap()
            .load(Ordering::SeqCst),
        0,
        "indeterminate consume must not publish replacement control"
    );
    proxy.release_commit();
    bounded(replica_b.registry.wait_until_idle()).await;
    assert_eq!(
        replica_b.hub.ticket_generation(
            VaultId(base.vault),
            DeviceId(base.owner_device),
            URL_SAFE_NO_PAD
                .decode(&replacement_session)
                .unwrap()
                .try_into()
                .unwrap(),
        ),
        None,
        "released acknowledgement must not revive a settled caller"
    );
    assert_eq!(
        replica_b
            .lost_controls
            .as_ref()
            .unwrap()
            .load(Ordering::SeqCst),
        0,
        "released acknowledgement must not publish late replacement control"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), next_close(&mut old_socket))
            .await
            .expect("old tuple must fail durable fallback after replacement")
            .code,
        tungstenite::protocol::frame::coding::CloseCode::Policy
    );

    replica_b.shutdown().await;
    replica_a.shutdown().await;
    base.shutdown().await;
    proxy.shutdown();
}

#[tokio::test]
async fn replica_ticket_create_budget_is_shared_across_instances_after_reaping() {
    let base = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let replica_a = ReplicaServer::start(&base, false).await;
    let replica_b = ReplicaServer::start(&base, false).await;
    sqlx::query("INSERT INTO live_ticket_admission_events(event_id,vault_id,device_id,event_kind,created_at) SELECT gen_random_uuid(),$1,$2,'session_create',clock_timestamp() FROM generate_series(1,5)")
        .bind(base.vault)
        .bind(base.owner_device)
        .execute(&base.pool)
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first = lifecycle_session(208);
    let second = lifecycle_session(209);
    let left = async {
        barrier.wait().await;
        replica_a.issue_status(&base.owner_token, &first).await
    };
    let right = async {
        barrier.wait().await;
        replica_b.issue_status(&base.owner_token, &second).await
    };
    let (left, right) = tokio::join!(left, right);
    assert!(matches!(
        left,
        reqwest::StatusCode::CREATED | reqwest::StatusCode::TOO_MANY_REQUESTS
    ));
    assert!(matches!(
        right,
        reqwest::StatusCode::CREATED | reqwest::StatusCode::TOO_MANY_REQUESTS
    ));
    assert_eq!(
        usize::from(left == reqwest::StatusCode::CREATED)
            + usize::from(right == reqwest::StatusCode::CREATED),
        1
    );
    sqlx::query("DELETE FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2")
        .bind(base.vault)
        .bind(base.owner_device)
        .execute(&base.pool)
        .await
        .unwrap();
    assert_eq!(
        replica_a
            .issue_status(&base.owner_token, &lifecycle_session(210))
            .await,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "retained admission ledger, not retained ticket rows, enforces six creates"
    );

    replica_b.shutdown().await;
    replica_a.shutdown().await;
    base.shutdown().await;
}
