use std::{future::Future, io, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Extension, Router,
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::{Next, from_fn_with_state},
    response::Response,
};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tower::{Layer, ServiceExt};

const MAX_CONNECTIONS: usize = 1_024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const BUFFERED_BODY_RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
const GRACEFUL_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy)]
struct ServeOptions {
    connection_limit: usize,
    buffered_body_receipt_timeout: Duration,
    header_read_timeout: Duration,
    graceful_drain_timeout: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            connection_limit: MAX_CONNECTIONS,
            buffered_body_receipt_timeout: BUFFERED_BODY_RECEIPT_TIMEOUT,
            header_read_timeout: HEADER_READ_TIMEOUT,
            graceful_drain_timeout: GRACEFUL_DRAIN_TIMEOUT,
        }
    }
}

pub async fn serve<S>(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: S,
) -> io::Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    serve_with_options(listener, router, shutdown, ServeOptions::default()).await
}

async fn serve_with_options<S>(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: S,
    options: ServeOptions,
) -> io::Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    let (stream_shutdown, stream_shutdown_receiver) = watch::channel(false);
    let (draining, draining_receiver) = watch::channel(false);
    let connection_gate = ConnectionGate::new(options.connection_limit);
    let service = router
        .layer(from_fn_with_state(
            options.buffered_body_receipt_timeout,
            receive_buffered_body,
        ))
        .layer(Extension(stream_shutdown_receiver))
        .into_make_service_with_connect_info::<SocketAddr>();
    let mut connections = tokio::task::JoinSet::new();

    tokio::pin!(shutdown);
    loop {
        reap_completed_connections(&mut connections);
        let accepted = tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) if is_retryable_accept_error(&error) => {
                tracing::warn!(%error, "retryable listener accept failure");
                tokio::select! {
                    _ = &mut shutdown => break,
                    _ = tokio::time::sleep(ACCEPT_RETRY_DELAY) => continue,
                }
            }
            Err(error) => return Err(error),
        };
        let Some(permit) = connection_gate.try_acquire() else {
            tracing::warn!(%peer, "connection shed at global limit");
            drop(stream);
            continue;
        };
        let service = service.clone();
        let mut draining = draining_receiver.clone();
        connections.spawn(async move {
            let connection_lease = permit;
            let service = match service.oneshot(peer).await {
                Ok(service) => service,
                Err(never) => match never {},
            };
            let service = Extension(connection_lease.clone()).layer(service);
            let mut connection_builder = hyper::server::conn::http1::Builder::new();
            connection_builder
                .timer(TokioTimer::new())
                .header_read_timeout(options.header_read_timeout);
            let mut connection = std::pin::pin!(
                connection_builder
                    .serve_connection(TokioIo::new(stream), TowerToHyperService::new(service))
                    .with_upgrades()
            );
            tokio::select! {
                result = &mut connection => {
                    if let Err(error) = result {
                        tracing::debug!(%peer, %error, "connection ended with an HTTP error");
                    }
                }
                changed = draining.changed() => {
                    if changed.is_ok() && *draining.borrow() {
                        connection.as_mut().graceful_shutdown();
                        if tokio::time::timeout(options.graceful_drain_timeout, &mut connection).await.is_err() {
                            tracing::warn!(%peer, "connection exceeded graceful drain timeout");
                        }
                    }
                }
            }
            drop(connection_lease);
        });
    }

    let _ = stream_shutdown.send(true);
    let _ = draining.send(true);
    let _ = tokio::time::timeout(options.graceful_drain_timeout, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    Ok(())
}

fn reap_completed_connections(connections: &mut tokio::task::JoinSet<()>) {
    while let Some(result) = connections.try_join_next() {
        if let Err(error) = result {
            tracing::debug!(%error, "connection task ended unexpectedly");
        }
    }
}

fn is_retryable_accept_error(error: &io::Error) -> bool {
    !matches!(
        error.kind(),
        io::ErrorKind::AddrNotAvailable
            | io::ErrorKind::InvalidInput
            | io::ErrorKind::NotConnected
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::Unsupported
    )
}

async fn receive_buffered_body(
    State(body_receipt_timeout): State<Duration>,
    request: Request,
    next: Next,
) -> Response {
    if !requires_buffered_body_receipt(&request) {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let body = match tokio::time::timeout(
        body_receipt_timeout,
        Limited::new(body, DEFAULT_BODY_LIMIT).collect(),
    )
    .await
    {
        Ok(Ok(collected)) => Body::from(collected.to_bytes()),
        Ok(Err(error)) if error.is::<LengthLimitError>() => return body_too_large_response(),
        Ok(Err(_)) => return invalid_body_response(),
        Err(_) => return timeout_response(),
    };
    next.run(Request::from_parts(parts, body)).await
}

fn requires_buffered_body_receipt(request: &Request) -> bool {
    !matches!(
        request.method(),
        &Method::GET | &Method::HEAD | &Method::OPTIONS
    ) && !is_streaming_attachment_request(request.method(), request.uri().path())
}

fn is_streaming_attachment_request(method: &Method, path: &str) -> bool {
    let mut segments = path.trim_matches('/').rsplit('/');
    let (action, attachment_id, collection, version) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    );
    matches!(
        (method.as_str(), action, attachment_id, collection, version),
        (
            "PUT",
            Some("upload"),
            Some(_),
            Some("attachments"),
            Some("v1")
        ) | (
            "POST",
            Some("public-copies"),
            Some(_),
            Some("attachments"),
            Some("v1")
        )
    )
}

fn timeout_response() -> Response {
    coded_response(StatusCode::SERVICE_UNAVAILABLE, "request_timeout")
}

fn body_too_large_response() -> Response {
    coded_response(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
}

fn invalid_body_response() -> Response {
    coded_response(StatusCode::BAD_REQUEST, "invalid_request_body")
}

fn coded_response(status: StatusCode, code: &'static str) -> Response {
    let mut response = Response::new(Body::from(format!(r#"{{"code":"{code}"}}"#)));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

#[derive(Clone)]
pub(crate) struct ConnectionLease {
    _permit: Arc<OwnedSemaphorePermit>,
}

struct ConnectionGate {
    permits: Arc<Semaphore>,
}

impl ConnectionGate {
    fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
        }
    }

    fn try_acquire(&self) -> Option<ConnectionLease> {
        self.permits
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| ConnectionLease {
                _permit: Arc::new(permit),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        Extension, Router,
        extract::ws::{WebSocket, WebSocketUpgrade},
        response::IntoResponse,
        routing::get,
    };
    use tokio::{io::AsyncReadExt, net::TcpStream, sync::oneshot};
    use tokio_tungstenite::connect_async;

    use super::{ConnectionLease, ServeOptions, serve_with_options};

    const MAX_RESPONSE_HEAD_BYTES: usize = 8 * 1024;
    const RESPONSE_READ_TIMEOUT: Duration = Duration::from_secs(1);

    fn test_options(connection_limit: usize) -> ServeOptions {
        ServeOptions {
            connection_limit,
            buffered_body_receipt_timeout: Duration::from_secs(30),
            header_read_timeout: Duration::from_millis(50),
            graceful_drain_timeout: Duration::from_millis(100),
        }
    }

    fn request_test_options() -> ServeOptions {
        ServeOptions {
            connection_limit: 8,
            buffered_body_receipt_timeout: Duration::from_millis(50),
            header_read_timeout: Duration::from_millis(50),
            graceful_drain_timeout: Duration::from_millis(100),
        }
    }

    async fn read_response_head(client: &mut TcpStream) -> Vec<u8> {
        let mut head = Vec::new();
        tokio::time::timeout(RESPONSE_READ_TIMEOUT, async {
            let mut byte = [0; 1];
            while !head.ends_with(b"\r\n\r\n") {
                assert!(
                    head.len() < MAX_RESPONSE_HEAD_BYTES,
                    "response head exceeded the test bound"
                );
                client.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
            }
        })
        .await
        .expect("response head was not received");
        head
    }

    async fn retained_websocket(
        lease: Option<Extension<ConnectionLease>>,
        upgrade: WebSocketUpgrade,
    ) -> impl IntoResponse {
        let lease = lease.map(|Extension(lease)| lease);
        upgrade.on_upgrade(move |socket| hold_websocket(socket, lease))
    }

    async fn hold_websocket(mut socket: WebSocket, lease: Option<ConnectionLease>) {
        let _lease = lease;
        while socket.recv().await.is_some() {}
    }

    async fn collect_body(_: String) -> &'static str {
        "ok"
    }

    #[tokio::test]
    async fn slow_headers_are_closed_after_the_configured_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_with_options(
            listener,
            Router::new().route("/", get(|| async { "ok" })),
            std::future::pending(),
            test_options(1),
        ));
        let mut client = TcpStream::connect(address).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut client, b"GET / HTTP/1.1\r\n")
            .await
            .unwrap();

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut response))
            .await
            .expect("slow header connection was not closed")
            .unwrap();
        assert!(response.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn shutdown_drains_an_idle_keepalive_connection_without_waiting_for_the_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_signal, shutdown) = oneshot::channel();
        let server = tokio::spawn(serve_with_options(
            listener,
            Router::new().route("/", get(|| async { "ok" })),
            async move {
                let _ = shutdown.await;
            },
            test_options(1),
        ));
        let mut client = TcpStream::connect(address).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(
            &mut client,
            b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n",
        )
        .await
        .unwrap();
        let head = read_response_head(&mut client).await;
        assert!(head.starts_with(b"HTTP/1.1 200 OK\r\n"));

        shutdown_signal.send(()).unwrap();
        tokio::time::timeout(Duration::from_millis(500), server)
            .await
            .expect("idle keepalive connection did not drain")
            .unwrap()
            .unwrap();
        let mut body = Vec::new();
        client.read_to_end(&mut body).await.unwrap();
        assert!(body.windows(2).any(|window| window == b"ok"));
    }

    #[tokio::test]
    async fn excess_connections_are_shed_while_an_admitted_connection_is_open() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_with_options(
            listener,
            Router::new().route("/", get(|| async { "ok" })),
            std::future::pending(),
            test_options(1),
        ));
        let first = TcpStream::connect(address).await.unwrap();
        let mut excess = TcpStream::connect(address).await.unwrap();

        let mut response = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), excess.read(&mut response))
                .await
                .expect("excess connection was not shed")
                .unwrap(),
            0
        );

        drop(first);
        server.abort();
    }

    #[tokio::test]
    async fn websocket_upgrade_retains_the_connection_limit_until_the_socket_closes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_with_options(
            listener,
            Router::new()
                .route("/", get(|| async { "ok" }))
                .route("/ws", get(retained_websocket)),
            std::future::pending(),
            test_options(1),
        ));
        let (socket, _) = connect_async(format!("ws://{address}/ws")).await.unwrap();

        let mut excess = TcpStream::connect(address).await.unwrap();
        let mut byte = [0; 1];
        assert_eq!(
            tokio::time::timeout(RESPONSE_READ_TIMEOUT, excess.read(&mut byte))
                .await
                .expect("excess connection was not shed")
                .unwrap(),
            0
        );

        drop(socket);
        let deadline = tokio::time::Instant::now() + RESPONSE_READ_TIMEOUT;
        loop {
            let mut admitted = TcpStream::connect(address).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(
                &mut admitted,
                b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
            let mut response = Vec::new();
            tokio::time::timeout(
                Duration::from_millis(50),
                admitted.read_to_end(&mut response),
            )
            .await
            .ok();
            if response.starts_with(b"HTTP/1.1 200 OK\r\n") {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "connection limit was not released after websocket close"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        server.abort();
    }

    #[tokio::test]
    async fn stalled_body_times_out_without_blocking_another_client_post() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_with_options(
            listener,
            Router::new()
                .route("/", get(|| async { "ok" }).post(collect_body))
                .route("/healthz", get(|| async { "healthy" })),
            std::future::pending(),
            request_test_options(),
        ));

        let mut stalled = TcpStream::connect(address).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(
            &mut stalled,
            b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 10\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        let mut admitted = TcpStream::connect(address).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(
            &mut admitted,
            b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        )
        .await
        .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(RESPONSE_READ_TIMEOUT, admitted.read_to_end(&mut response))
            .await
            .expect("normal post was blocked by the stalled body")
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));

        let mut health = TcpStream::connect(address).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(
            &mut health,
            b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        let mut health_response = Vec::new();
        tokio::time::timeout(
            RESPONSE_READ_TIMEOUT,
            health.read_to_end(&mut health_response),
        )
        .await
        .expect("health check was blocked by the stalled body")
        .unwrap();
        assert!(health_response.starts_with(b"HTTP/1.1 200 OK\r\n"));

        let mut timeout_response = Vec::new();
        tokio::time::timeout(
            RESPONSE_READ_TIMEOUT,
            stalled.read_to_end(&mut timeout_response),
        )
        .await
        .expect("stalled request did not time out")
        .unwrap();
        assert!(timeout_response.starts_with(b"HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(
            std::str::from_utf8(&timeout_response)
                .unwrap()
                .contains(r#"{"code":"request_timeout"}"#)
        );
        server.abort();
    }
}
