//! Authenticated, origin-bound HTTP. Redirects are disabled, TLS is mandatory except for
//! explicit loopback development origins, every response body is read incrementally against a
//! hard cap (Content-Length is only an early rejection hint), and media is streamed to/from
//! files instead of being buffered.
use crate::{error::BridgeError, origin::is_loopback_http};
use futures_util::StreamExt;
use reqwest::{header, redirect, RequestBuilder, Response, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

pub const MAX_JSON_BYTES: usize = 1024 * 1024;
/// History/snapshot pages: same bound core applies to a snapshot page.
pub const MAX_PAGE_BYTES: usize = peppy_client_core::MAX_SNAPSHOT_PAGE_BYTES;
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;
const TRANSFER_CHUNK: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetError {
    Offline,
    Revoked,
    Status { status: u16, code: Option<String> },
    TooLarge,
    Invalid,
    Local,
}

impl NetError {
    pub fn is_status(&self, status: u16, code: &str) -> bool {
        matches!(self, NetError::Status { status: s, code: Some(c) } if *s == status && c == code)
    }
}

impl From<NetError> for BridgeError {
    fn from(error: NetError) -> Self {
        match error {
            NetError::Offline => {
                BridgeError::new("offline", "Could not reach the configured server.")
            }
            NetError::Revoked => BridgeError::new(
                "revoked",
                "The server rejected this device credential (revoked or invalid).",
            ),
            NetError::Status { status, code } => BridgeError::new(
                "server-rejected",
                match code {
                    Some(code) => {
                        format!("The server rejected the request (HTTP {status}, {code}).")
                    }
                    None => format!("The server rejected the request (HTTP {status})."),
                },
            ),
            NetError::TooLarge => BridgeError::new(
                "response-too-large",
                "A server response exceeded the native safety limit.",
            ),
            NetError::Invalid => BridgeError::new(
                "server-response",
                "The server returned an invalid response.",
            ),
            NetError::Local => {
                BridgeError::new("attachment-local", "A local transfer file is unavailable.")
            }
        }
    }
}

/// Only short `snake_case` server error codes are surfaced; anything else is dropped.
fn sanitize_code(value: &serde_json::Value) -> Option<String> {
    let code = value.get("code")?.as_str()?;
    (code.len() <= 64
        && !code.is_empty()
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    .then(|| code.to_owned())
}

/// Reads a response body incrementally, failing as soon as `cap` would be exceeded.
pub async fn read_bounded(mut response: Response, cap: usize) -> Result<Vec<u8>, NetError> {
    if response
        .content_length()
        .is_some_and(|length| length > cap as u64)
    {
        return Err(NetError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| NetError::Offline)? {
        if body.len() + chunk.len() > cap {
            return Err(NetError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub struct Api {
    http: reqwest::Client,
    transfer: reqwest::Client,
    origin: String,
    token: Zeroizing<String>,
}

fn builder(origin: &str) -> Result<reqwest::ClientBuilder, BridgeError> {
    crate::tls::http_builder(origin).map(|builder| {
        builder
            .redirect(redirect::Policy::none())
            .https_only(!is_loopback_http(origin))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("Peppy-Desktop")
    })
}

impl Api {
    /// `origin` must already be validated (see `origin::validate_origin`).
    pub fn new(origin: &str, token: &str) -> Result<Self, BridgeError> {
        let failed = |_| BridgeError::new("network", "Could not create the native network client.");
        Ok(Self {
            http: builder(origin)?
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(failed)?,
            transfer: builder(origin)?
                .read_timeout(Duration::from_secs(30))
                .build()
                .map_err(failed)?,
            origin: origin.to_owned(),
            token: Zeroizing::new(token.to_owned()),
        })
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    /// Native-only: used for the WebSocket upgrade header.
    pub fn bearer(&self) -> &str {
        &self.token
    }
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.origin, path)
    }

    async fn execute(&self, request: RequestBuilder) -> Result<Response, NetError> {
        let response = request
            .bearer_auth(self.token.as_str())
            .send()
            .await
            .map_err(|_| NetError::Offline)?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(NetError::Revoked);
        }
        let code = read_bounded(response, MAX_ERROR_BODY_BYTES)
            .await
            .ok()
            .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
            .and_then(|value| sanitize_code(&value));
        Err(NetError::Status {
            status: status.as_u16(),
            code,
        })
    }

    async fn json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        cap: usize,
    ) -> Result<T, NetError> {
        let body = read_bounded(self.execute(request).await?, cap).await?;
        serde_json::from_slice(&body).map_err(|_| NetError::Invalid)
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        cap: usize,
    ) -> Result<T, NetError> {
        self.json(self.http.get(self.url(path)), cap).await
    }

    pub async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, NetError> {
        self.json(self.http.post(self.url(path)).json(body), MAX_JSON_BYTES)
            .await
    }

    pub async fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, NetError> {
        self.json(self.http.post(self.url(path)), MAX_JSON_BYTES)
            .await
    }

    /// Posts a body and discards a bounded response body.
    pub async fn post_discard<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), NetError> {
        read_bounded(
            self.execute(self.http.post(self.url(path)).json(body))
                .await?,
            MAX_JSON_BYTES,
        )
        .await
        .map(|_| ())
    }

    /// `DELETE` with a JSON body; any 2xx is success and the bounded body is discarded.
    pub async fn delete_json<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), NetError> {
        read_bounded(
            self.execute(self.http.delete(self.url(path)).json(body))
                .await?,
            MAX_JSON_BYTES,
        )
        .await
        .map(|_| ())
    }

    /// Streams exactly `length` bytes of a local file as the request body.
    pub async fn put_file(&self, path: &str, file: &Path, length: u64) -> Result<(), NetError> {
        let source = tokio::fs::File::open(file)
            .await
            .map_err(|_| NetError::Local)?;
        if source.metadata().await.map_err(|_| NetError::Local)?.len() != length {
            return Err(NetError::Local);
        }
        let stream = tokio_util::io::ReaderStream::with_capacity(
            tokio::io::AsyncReadExt::take(source, length),
            TRANSFER_CHUNK,
        );
        let request = self
            .transfer
            .put(self.url(path))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, length)
            .body(reqwest::Body::wrap_stream(stream));
        read_bounded(self.execute(request).await?, MAX_ERROR_BODY_BYTES)
            .await
            .map(|_| ())
    }

    /// Streams a response to a new owner-only file, enforcing the exact expected length.
    /// The partial file is removed on any failure.
    pub async fn download_to(
        &self,
        path: &str,
        target: &Path,
        expected: u64,
    ) -> Result<(), NetError> {
        let response = self.execute(self.transfer.get(self.url(path))).await?;
        if response
            .content_length()
            .is_some_and(|length| length != expected)
        {
            return Err(NetError::Invalid);
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut output = options.open(target).await.map_err(|_| NetError::Local)?;
        let result = async {
            let mut stream = response.bytes_stream();
            let mut total = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| NetError::Offline)?;
                total = total
                    .checked_add(chunk.len() as u64)
                    .ok_or(NetError::TooLarge)?;
                if total > expected {
                    return Err(NetError::TooLarge);
                }
                output
                    .write_all(&chunk)
                    .await
                    .map_err(|_| NetError::Local)?;
            }
            // A short body is a dropped connection: transient, retried next round.
            if total != expected {
                return Err(NetError::Offline);
            }
            output.flush().await.map_err(|_| NetError::Local)?;
            output.sync_all().await.map_err(|_| NetError::Local)
        }
        .await;
        drop(output);
        if result.is_err() {
            let _ = tokio::fs::remove_file(target).await;
        }
        result
    }

    /// Uploads an already re-encoded public image (bounded by the caller).
    pub async fn post_public_copy<T: DeserializeOwned>(
        &self,
        path: &str,
        safe_name: &str,
        bytes: Vec<u8>,
    ) -> Result<T, NetError> {
        let request = self
            .transfer
            .post(self.url(path))
            .header("x-file-name", safe_name)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes);
        self.json(request, MAX_JSON_BYTES).await
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    #[cfg(feature = "dev-local-tls")]
    use std::time::Duration;
    use std::{path::Path, process::Command, sync::Arc};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsAcceptor;
    #[cfg(feature = "dev-local-tls")]
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    // A trust-policy mutation that accepts this private root for a non-loopback
    // origin, a mismatched hostname, or the default client must fail these tests.
    #[tokio::test]
    async fn development_ca_trust_is_loopback_scoped_for_https_and_wss() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let fixture = temporary_certificate();
        let certificate = certificate_der(fixture.path());
        let wrong_fixture = temporary_certificate();
        let wrong_certificate = certificate_der(wrong_fixture.path());

        let https = tls_server(fixture.path(), false).await;
        let client = crate::tls::http_builder_with(&https, Some(&certificate))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            client.get(&https).send().await.unwrap().status(),
            StatusCode::OK
        );

        let wss = tls_server(fixture.path(), true)
            .await
            .replace("https://", "wss://");
        let connector = crate::tls::websocket_connector_with(
            &wss.replacen("wss://", "https://", 1),
            Some(&certificate),
        )
        .unwrap()
        .expect("loopback fixture requires the injected CA");
        tokio_tungstenite::connect_async_tls_with_config(wss, None, true, Some(connector))
            .await
            .expect("the loopback development CA verifies the WSS server");

        let wrong_root = tls_server(fixture.path(), false).await;
        let wrong_client = crate::tls::http_builder_with(&wrong_root, Some(&wrong_certificate))
            .unwrap()
            .build()
            .unwrap();
        assert!(wrong_client.get(&wrong_root).send().await.is_err());

        let hostname_mismatch = tls_server(fixture.path(), false).await;
        let ip_origin = hostname_mismatch.replacen("localhost", "127.0.0.1", 1);
        let hostname_client = crate::tls::http_builder_with(&ip_origin, Some(&certificate))
            .unwrap()
            .build()
            .unwrap();
        assert!(hostname_client.get(ip_origin).send().await.is_err());

        assert!(!crate::tls::trusts_development_ca(
            "https://example.test",
            Some(&certificate)
        ));
        assert!(!crate::tls::trusts_development_ca(
            "https://localhost",
            None
        ));
        assert!(crate::tls::trusts_development_ca(
            "https://localhost",
            Some(&certificate)
        ));
        assert!(crate::tls::http_builder("https://localhost").is_ok());
        #[cfg(not(feature = "dev-local-tls"))]
        assert!(crate::tls::websocket_connector("https://localhost")
            .unwrap()
            .is_none());
        let default_client = reqwest::Client::new();
        let default_tls = tls_server(fixture.path(), false).await;
        assert!(default_client.get(default_tls).send().await.is_err());
    }

    /// Requires a locally running development TLS proxy with the CA embedded at build time.
    #[cfg(feature = "dev-local-tls")]
    #[tokio::test]
    #[ignore = "requires a running local development TLS stack"]
    async fn local_dev_server_uses_bundled_ca() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let origin = std::env::var("PEPPY_DEV_TLS_TEST_ORIGIN")
            .expect("PEPPY_DEV_TLS_TEST_ORIGIN must name the local HTTPS development origin");
        let origin = crate::origin::validate_origin(&origin)
            .expect("PEPPY_DEV_TLS_TEST_ORIGIN must be a valid origin");
        assert!(
            crate::origin::is_loopback_https(&origin),
            "PEPPY_DEV_TLS_TEST_ORIGIN must be an explicit loopback HTTPS origin"
        );

        let client = crate::tls::http_builder(&origin)
            .expect("the embedded local development CA must build an HTTP client")
            .timeout(Duration::from_secs(5))
            .build()
            .expect("the HTTP client must be valid");
        let health = tokio::time::timeout(
            Duration::from_secs(5),
            client.get(format!("{origin}/healthz")).send(),
        )
        .await
        .expect("the local HTTPS health request timed out")
        .expect("the bundled CA must verify the local HTTPS server");
        assert_eq!(health.status(), StatusCode::OK);

        let request = crate::origin::websocket_url(&origin)
            .expect("the local HTTPS origin must produce a WSS endpoint")
            .into_client_request()
            .expect("the WSS endpoint must produce a request");
        let connector = crate::tls::websocket_connector(&origin)
            .expect("the embedded local development CA must build a WSS connector")
            .expect("loopback HTTPS must use the local development WSS connector");
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async_tls_with_config(request, None, true, Some(connector)),
        )
        .await
        .expect("the local WSS request timed out");
        match result {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert!(matches!(response.status().as_u16(), 401 | 403));
            }
            Ok(_) => panic!("the unauthenticated local WSS endpoint unexpectedly upgraded"),
            Err(error) => panic!("the bundled CA did not verify the local WSS server: {error}"),
        }
    }

    fn temporary_certificate() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let ca = directory.path().join("ca.pem");
        let ca_key = directory.path().join("ca-key.pem");
        let certificate = directory.path().join("certificate.pem");
        let key = directory.path().join("key.pem");
        let request = directory.path().join("certificate.csr");
        let serial = directory.path().join("certificate.srl");
        assert!(Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout"])
            .arg(&ca_key)
            .arg("-out")
            .arg(&ca)
            .args([
                "-subj",
                "/CN=localhost",
                "-days",
                "1",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
            ])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
            .args(["req", "-newkey", "rsa:2048", "-nodes", "-keyout"])
            .arg(&key)
            .arg("-out")
            .arg(&request)
            .args(["-subj", "/CN=localhost"])
            .status()
            .unwrap()
            .success());
        std::fs::write(
            directory.path().join("extensions.cnf"),
            "[v3]\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost\n",
        )
        .unwrap();
        assert!(Command::new("openssl")
            .args(["x509", "-req", "-in"])
            .arg(&request)
            .arg("-CA")
            .arg(&ca)
            .arg("-CAkey")
            .arg(&ca_key)
            .args(["-CAserial"])
            .arg(&serial)
            .args(["-CAcreateserial", "-out"])
            .arg(&certificate)
            .args(["-days", "1", "-extfile"])
            .arg(directory.path().join("extensions.cnf"))
            .args(["-extensions", "v3"])
            .status()
            .unwrap()
            .success());
        directory
    }

    fn certificate_der(directory: &Path) -> Vec<u8> {
        rustls_pemfile::certs(&mut std::io::BufReader::new(
            std::fs::File::open(directory.join("ca.pem")).unwrap(),
        ))
        .next()
        .unwrap()
        .unwrap()
        .as_ref()
        .to_vec()
    }

    async fn tls_server(directory: &Path, websocket: bool) -> String {
        let certificates = rustls_pemfile::certs(&mut std::io::BufReader::new(
            std::fs::File::open(directory.join("certificate.pem")).unwrap(),
        ))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
            std::fs::File::open(directory.join("key.pem")).unwrap(),
        ))
        .unwrap()
        .unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = TlsAcceptor::from(Arc::new(config)).accept(stream).await else {
                return;
            };
            if websocket {
                let _ = tokio_tungstenite::accept_async(stream).await;
            } else {
                let mut request = [0; 1024];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
            }
        });
        format!("https://localhost:{}", address.port())
    }

    /// Minimal one-shot HTTP/1.1 server returning `head` then `body`.
    pub async fn serve_once(head: String, body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{address}")
    }

    fn chunked(body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in body.chunks(1000) {
            out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    #[tokio::test]
    async fn responses_are_bounded_without_trusting_content_length() {
        // Declared length above the cap: rejected before reading.
        let origin = serve_once(
            "HTTP/1.1 200 OK\r\ncontent-length: 5000\r\nconnection: close\r\n\r\n".into(),
            vec![b'a'; 5000],
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        assert_eq!(
            read_bounded(api.execute(api.http.get(api.url("/"))).await.unwrap(), 4096).await,
            Err(NetError::TooLarge)
        );
        // Chunked body without Content-Length that exceeds the cap mid-stream.
        let origin = serve_once(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".into(),
            chunked(&vec![b'b'; 9000]),
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        assert_eq!(
            read_bounded(api.execute(api.http.get(api.url("/"))).await.unwrap(), 4096).await,
            Err(NetError::TooLarge)
        );
        // Within the cap.
        let origin = serve_once(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".into(),
            chunked(b"{\"ok\":true}"),
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        let value: serde_json::Value = api.get_json("/", 4096).await.unwrap();
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_status_codes_are_typed() {
        let origin = serve_once("HTTP/1.1 302 Found\r\nlocation: https://elsewhere.test/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(), vec![]).await;
        let api = Api::new(&origin, "t").unwrap();
        assert!(matches!(
            api.get_json::<serde_json::Value>("/v1/vault", 4096).await,
            Err(NetError::Status { status: 302, .. })
        ));
        let origin = serve_once(
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
            vec![],
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        assert_eq!(
            api.get_json::<serde_json::Value>("/v1/vault", 4096)
                .await
                .unwrap_err(),
            NetError::Revoked
        );
        let body = b"{\"code\":\"resync_required\",\"reason\":\"cursor_expired\"}".to_vec();
        let origin = serve_once(
            format!(
                "HTTP/1.1 409 Conflict\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            ),
            body,
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        assert!(api
            .get_json::<serde_json::Value>("/v1/events", 4096)
            .await
            .unwrap_err()
            .is_status(409, "resync_required"));
        let body = b"{\"code\":\"<script>\"}".to_vec();
        let origin = serve_once(
            format!(
                "HTTP/1.1 400 Bad Request\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            ),
            body,
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        assert_eq!(
            api.get_json::<serde_json::Value>("/", 4096)
                .await
                .unwrap_err(),
            NetError::Status {
                status: 400,
                code: None
            }
        );
    }

    #[tokio::test]
    async fn downloads_enforce_exact_length_and_remove_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let origin = serve_once(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".into(),
            chunked(&vec![7u8; 3000]),
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        let target = dir.path().join("too-long");
        assert_eq!(
            api.download_to("/x", &target, 2000).await,
            Err(NetError::TooLarge)
        );
        assert!(!target.exists());
        let origin = serve_once(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".into(),
            chunked(&vec![7u8; 3000]),
        )
        .await;
        let api = Api::new(&origin, "t").unwrap();
        let target = dir.path().join("ok");
        api.download_to("/x", &target, 3000).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap().len(), 3000);
    }
}
