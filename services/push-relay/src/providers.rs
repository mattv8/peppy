//! Fixed-destination, content-free APNs and FCM HTTP v1 delivery adapters.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{Provider, ProviderError, PushProvider};

const APNS_URL: &str = "https://api.push.apple.com/3/device/";
const APNS_SANDBOX_URL: &str = "https://api.sandbox.push.apple.com/3/device/";
const FCM_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const FCM_API_URL: &str = "https://fcm.googleapis.com";
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct ConfiguredProvider {
    client: Client,
    apns: Option<ApnsConfig>,
    fcm: Option<FcmConfig>,
    fcm_priority: FcmPriority,
    fcm_endpoint: &'static str,
    apns_token: Arc<Mutex<Option<CachedToken>>>,
    fcm_token: Arc<Mutex<Option<CachedToken>>>,
}
#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: SystemTime,
}

#[derive(Clone)]
struct ApnsConfig {
    team_id: String,
    key_id: String,
    bundle_id: String,
    key: Arc<String>,
    endpoint: &'static str,
    environment: &'static str,
}
#[derive(Clone)]
struct FcmConfig {
    project_id: String,
    client_email: String,
    private_key: Arc<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FcmPriority {
    Normal,
    High,
}

impl FcmPriority {
    fn from_environment() -> Result<Self, String> {
        Self::parse(std::env::var("PEPPY_RELAY_FCM_PRIORITY").ok().as_deref())
    }

    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            Some("normal") => Ok(Self::Normal),
            Some("high") | None => Ok(Self::High),
            _ => Err("PEPPY_RELAY_FCM_PRIORITY must be normal or high".into()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::High => "high",
        }
    }
}

#[derive(Deserialize)]
struct FcmSecret {
    project_id: String,
    client_email: String,
    private_key: String,
}

impl ConfiguredProvider {
    /// Empty provider selections are intentionally rejected: the safe default is unconfigured.
    pub fn from_environment(selection: &str) -> Result<Self, String> {
        let fcm_priority = FcmPriority::from_environment()?;
        let selected: Vec<_> = selection
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if selected.is_empty() || selected.iter().any(|p| *p != "apns" && *p != "fcm") {
            return Err("PEPPY_RELAY_PROVIDER must be unconfigured, apns, fcm, or apns,fcm".into());
        }
        let read = |name: &str| -> Result<String, String> {
            if let Ok(value) = std::env::var(name) {
                return Ok(value);
            }
            let path = std::env::var(format!("{name}_FILE"))
                .map_err(|_| format!("{name} or {name}_FILE is required"))?;
            std::fs::read_to_string(path).map_err(|_| format!("cannot read {name}_FILE"))
        };
        let apns: Option<ApnsConfig> = selected
            .contains(&"apns")
            .then(|| {
                let team_id = identifier(
                    read("PEPPY_RELAY_APNS_TEAM_ID")?,
                    "PEPPY_RELAY_APNS_TEAM_ID",
                )?;
                let key_id =
                    identifier(read("PEPPY_RELAY_APNS_KEY_ID")?, "PEPPY_RELAY_APNS_KEY_ID")?;
                let bundle_id = identifier(
                    read("PEPPY_RELAY_APNS_BUNDLE_ID")?,
                    "PEPPY_RELAY_APNS_BUNDLE_ID",
                )?;
                let key = read("PEPPY_RELAY_APNS_PRIVATE_KEY")?.trim().to_owned();
                EncodingKey::from_ec_pem(key.as_bytes()).map_err(|_| "invalid APNs private key")?;
                let (endpoint, environment) = match std::env::var("PEPPY_RELAY_APNS_ENV").as_deref()
                {
                    Ok("sandbox") => (APNS_SANDBOX_URL, "sandbox"),
                    Ok("production") | Err(_) => (APNS_URL, "production"),
                    _ => return Err("PEPPY_RELAY_APNS_ENV must be sandbox or production".into()),
                };
                Ok::<_, String>(ApnsConfig {
                    team_id,
                    key_id,
                    bundle_id,
                    key: Arc::new(key),
                    endpoint,
                    environment,
                })
            })
            .transpose()?;
        let fcm: Option<FcmConfig> = selected
            .contains(&"fcm")
            .then(|| {
                let secret: FcmSecret = serde_json::from_str(&read(
                    "PEPPY_RELAY_FCM_SERVICE_ACCOUNT",
                )?)
                .map_err(|_| "PEPPY_RELAY_FCM_SERVICE_ACCOUNT must be service-account JSON")?;
                let project_id = identifier(secret.project_id, "FCM project_id")?;
                let client_email = identifier(secret.client_email, "FCM client_email")?;
                let private_key = secret.private_key.trim().to_owned();
                EncodingKey::from_rsa_pem(private_key.as_bytes())
                    .map_err(|_| "invalid FCM private key")?;
                Ok::<_, String>(FcmConfig {
                    project_id,
                    client_email,
                    private_key: Arc::new(private_key),
                })
            })
            .transpose()?;
        let client = Client::builder()
            .timeout(PROVIDER_TIMEOUT)
            .build()
            .map_err(|_| "cannot initialize provider HTTP client")?;
        Ok(Self {
            client,
            apns,
            fcm,
            fcm_priority,
            fcm_endpoint: FCM_API_URL,
            apns_token: Arc::new(Mutex::new(None)),
            fcm_token: Arc::new(Mutex::new(None)),
        })
    }

    async fn send(
        &self,
        provider: Provider,
        token: &str,
        kind: &str,
        value: &str,
    ) -> Result<(), ProviderError> {
        match provider {
            Provider::Apns => match &self.apns {
                Some(config) => self.apns_send(config, token, kind, value).await,
                None => Err(ProviderError::Unavailable),
            },
            Provider::Fcm => match &self.fcm {
                Some(config) => self.fcm_send(config, token, kind, value).await,
                None => Err(ProviderError::Unavailable),
            },
        }
    }
    async fn apns_send(
        &self,
        c: &ApnsConfig,
        token: &str,
        kind: &str,
        value: &str,
    ) -> Result<(), ProviderError> {
        if !token.bytes().all(|b| b.is_ascii_hexdigit()) || !(64..=256).contains(&token.len()) {
            return Err(ProviderError::InvalidToken);
        }
        let jwt = self.apns_jwt(c).await?;
        let response = self.client.post(format!("{}{}", c.endpoint, token)).header("authorization", format!("bearer {jwt}")).header("apns-topic", &c.bundle_id).header("apns-push-type", "background").header("apns-priority", "5").json(&serde_json::json!({"aps":{"content-available":1},"peppy":{"kind":kind,"value":value}})).send().await.map_err(|_| ProviderError::Retryable)?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let result = classify_apns(status, &body);
        if matches!(result, Err(ProviderError::Retryable))
            && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
        {
            *self.apns_token.lock().await = None;
        }
        result
    }
    async fn apns_jwt(&self, c: &ApnsConfig) -> Result<String, ProviderError> {
        let mut cache = self.apns_token.lock().await;
        if let Some(token) = cache
            .as_ref()
            .filter(|token| token.expires_at > SystemTime::now())
        {
            return Ok(token.value.clone());
        }
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            iat: u64,
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(c.key_id.clone());
        let jwt = encode(
            &header,
            &Claims {
                iss: &c.team_id,
                iat: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
            &EncodingKey::from_ec_pem(c.key.as_bytes()).map_err(|_| ProviderError::Unavailable)?,
        )
        .map_err(|_| ProviderError::Unavailable)?;
        *cache = Some(CachedToken {
            value: jwt.clone(),
            expires_at: SystemTime::now() + Duration::from_secs(45 * 60),
        });
        Ok(jwt)
    }
    async fn fcm_send(
        &self,
        c: &FcmConfig,
        token: &str,
        kind: &str,
        value: &str,
    ) -> Result<(), ProviderError> {
        // Bind the clone in its own statement so the cache lock is released before the network
        // call; fcm_message re-locks the cache when FCM rejects the token.
        let cached = self
            .fcm_token
            .lock()
            .await
            .as_ref()
            .filter(|token| token.expires_at > SystemTime::now())
            .cloned();
        if let Some(cached) = cached {
            return self
                .fcm_message(c, token.to_owned(), cached, kind, value)
                .await;
        }
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            scope: &'a str,
            aud: &'a str,
            iat: u64,
            exp: u64,
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let assertion = encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iss: &c.client_email,
                scope: FCM_SCOPE,
                aud: FCM_TOKEN_URL,
                iat: now,
                exp: now + 300,
            },
            &EncodingKey::from_rsa_pem(c.private_key.as_bytes())
                .map_err(|_| ProviderError::Unavailable)?,
        )
        .map_err(|_| ProviderError::Unavailable)?;
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let access: Token = self
            .client
            .post(FCM_TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &assertion),
            ])
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?
            .json()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        let cached = CachedToken {
            value: access.access_token,
            expires_at: SystemTime::now()
                + Duration::from_secs(access.expires_in.saturating_sub(60)),
        };
        *self.fcm_token.lock().await = Some(cached.clone());
        self.fcm_message(c, token.to_owned(), cached, kind, value)
            .await
    }
    async fn fcm_message(
        &self,
        c: &FcmConfig,
        token: String,
        access: CachedToken,
        kind: &str,
        value: &str,
    ) -> Result<(), ProviderError> {
        let response = self
            .client
            .post(format!(
                "{}/v1/projects/{}/messages:send",
                self.fcm_endpoint, c.project_id
            ))
            .bearer_auth(access.value)
            .json(&fcm_payload(token, self.fcm_priority, kind, value))
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let result = classify_fcm(status, &body);
        if matches!(result, Err(ProviderError::Retryable))
            && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
        {
            *self.fcm_token.lock().await = None;
        }
        result
    }
}

fn fcm_payload(token: String, priority: FcmPriority, kind: &str, value: &str) -> serde_json::Value {
    serde_json::json!({
        "message": {
            "token": token,
            "android": { "priority": priority.as_str() },
            "data": { "kind": kind, "value": value }
        }
    })
}

fn identifier(value: String, name: &str) -> Result<String, String> {
    let value = value.trim().to_owned();
    if value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
    {
        Err(format!("invalid {name}"))
    } else {
        Ok(value)
    }
}
fn classify_apns(status: StatusCode, body: &str) -> Result<(), ProviderError> {
    if status.is_success() {
        Ok(())
    } else if body.contains("BadDeviceToken") || body.contains("Unregistered") {
        Err(ProviderError::InvalidToken)
    } else if status == StatusCode::UNAUTHORIZED
        || body.contains("ExpiredProviderToken")
        || status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
    {
        Err(ProviderError::Retryable)
    } else {
        Err(ProviderError::Permanent)
    }
}
fn classify_fcm(status: StatusCode, body: &str) -> Result<(), ProviderError> {
    if status.is_success() {
        Ok(())
    } else if body.contains("UNREGISTERED") {
        Err(ProviderError::InvalidToken)
    } else if status == StatusCode::UNAUTHORIZED
        || status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
    {
        Err(ProviderError::Retryable)
    } else {
        Err(ProviderError::Permanent)
    }
}
type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ProviderError>> + Send + 'a>>;
impl PushProvider for ConfiguredProvider {
    fn configured(&self) -> bool {
        self.apns.is_some() || self.fcm.is_some()
    }
    fn supports(&self, provider: Provider) -> bool {
        match provider {
            Provider::Apns => self.apns.is_some(),
            Provider::Fcm => self.fcm.is_some(),
        }
    }
    fn provider_name(&self) -> &'static str {
        match (self.apns.is_some(), self.fcm.is_some()) {
            (true, true) => "apns,fcm",
            (true, false) => "apns",
            (false, true) => "fcm",
            _ => "unconfigured",
        }
    }
    fn apns_environment(&self) -> Option<&'static str> {
        self.apns.as_ref().map(|config| config.environment)
    }
    fn send_challenge<'a>(&'a self, p: Provider, t: &'a str, c: &'a str) -> ProviderFuture<'a> {
        Box::pin(async move { self.send(p, t, "challenge", c).await })
    }
    fn send_wake<'a>(&'a self, p: Provider, t: &'a str, n: &'a str) -> ProviderFuture<'a> {
        Box::pin(async move { self.send(p, t, "wake", n).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn production_response_classification_is_precise() {
        assert_eq!(classify_apns(StatusCode::OK, ""), Ok(()));
        assert_eq!(
            classify_apns(StatusCode::BAD_REQUEST, r#"{"reason":"BadDeviceToken"}"#),
            Err(ProviderError::InvalidToken)
        );
        assert_eq!(
            classify_apns(
                StatusCode::BAD_REQUEST,
                r#"{"reason":"DeviceTokenNotForTopic"}"#
            ),
            Err(ProviderError::Permanent)
        );
        assert_eq!(
            classify_fcm(StatusCode::TOO_MANY_REQUESTS, ""),
            Err(ProviderError::Retryable)
        );
        assert_eq!(
            classify_fcm(
                StatusCode::NOT_FOUND,
                r#"{"error":{"status":"UNREGISTERED"}}"#
            ),
            Err(ProviderError::InvalidToken)
        );
    }

    #[test]
    fn fcm_priority_preserves_high_legacy_default_and_accepts_normal() {
        assert_eq!(FcmPriority::parse(None), Ok(FcmPriority::High));
        assert_eq!(FcmPriority::parse(Some("normal")), Ok(FcmPriority::Normal));
        assert!(FcmPriority::parse(Some("urgent")).is_err());
    }

    /// An FCM provider whose key cannot sign and whose Google hosts resolve to a closed loopback
    /// port, so the test never reaches the network and every signing attempt is observable.
    fn unsignable_fcm_provider(cached: Option<CachedToken>) -> (ConfiguredProvider, FcmConfig) {
        let closed = std::net::SocketAddr::from(([127, 0, 0, 1], 1));
        let config = FcmConfig {
            project_id: "fixture-project".into(),
            client_email: "relay@fixture-project.iam.example".into(),
            private_key: Arc::new("not a PEM private key".into()),
        };
        let provider = ConfiguredProvider {
            client: Client::builder()
                .resolve("fcm.googleapis.com", closed)
                .resolve("oauth2.googleapis.com", closed)
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            apns: None,
            fcm: Some(config.clone()),
            fcm_priority: FcmPriority::High,
            fcm_endpoint: FCM_API_URL,
            apns_token: Arc::new(Mutex::new(None)),
            fcm_token: Arc::new(Mutex::new(cached)),
        };
        (provider, config)
    }

    /// Serves canned FCM responses on loopback HTTP, one per connection, and counts requests.
    async fn fake_fcm(status: u16, requests: Arc<AtomicUsize>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let reason = StatusCode::from_u16(status)
            .unwrap()
            .canonical_reason()
            .unwrap_or("");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                requests.fetch_add(1, Ordering::SeqCst);
                read_http_request(&mut stream).await;
                let body = r#"{"error":{"status":"FIXTURE"}}"#;
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.ok();
            }
        });
        format!("http://{address}")
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) {
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                return;
            }
            received.extend_from_slice(&chunk[..read]);
            if let Some(head_end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&received[..head_end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if received.len() >= head_end + 4 + length {
                    return;
                }
            }
        }
    }

    #[tokio::test]
    async fn cached_fcm_access_token_skips_assertion_signing() {
        let (provider, config) = unsignable_fcm_provider(Some(CachedToken {
            value: "cached-access-token".into(),
            expires_at: SystemTime::now() + Duration::from_secs(600),
        }));
        // A cache hit goes straight to delivery; the closed port makes that retryable. Signing
        // first would fail on the invalid key as Unavailable.
        assert_eq!(
            provider
                .fcm_send(&config, "device-token", "wake", "nonce")
                .await,
            Err(ProviderError::Retryable)
        );

        let (expired, config) = unsignable_fcm_provider(Some(CachedToken {
            value: "expired-access-token".into(),
            expires_at: SystemTime::now() - Duration::from_secs(1),
        }));
        assert_eq!(
            expired
                .fcm_send(&config, "device-token", "wake", "nonce")
                .await,
            Err(ProviderError::Unavailable)
        );
    }

    /// A provider whose cached access token the fake FCM answers with `status`.
    async fn cached_fcm_provider(status: u16) -> (ConfiguredProvider, FcmConfig, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let base = fake_fcm(status, Arc::clone(&requests)).await;
        let (mut provider, config) = unsignable_fcm_provider(Some(CachedToken {
            value: "cached-access-token".into(),
            expires_at: SystemTime::now() + Duration::from_secs(600),
        }));
        provider.fcm_endpoint = Box::leak(base.into_boxed_str());
        (provider, config, requests)
    }

    #[tokio::test]
    async fn fcm_401_on_cached_token_clears_cache_without_deadlock() {
        let (provider, config, requests) = cached_fcm_provider(401).await;

        let first = tokio::time::timeout(
            Duration::from_secs(5),
            provider.fcm_send(&config, "device-token", "wake", "nonce"),
        )
        .await
        .expect("cached-token rejection must not deadlock");
        assert_eq!(first, Err(ProviderError::Retryable));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(provider.fcm_token.lock().await.is_none());

        // The cache is cleared, so the next send re-enters the signing path and fails fast on
        // the synthetic key instead of waiting on the lock held by the previous send.
        let second = tokio::time::timeout(
            Duration::from_secs(5),
            provider.fcm_send(&config, "device-token", "wake", "nonce"),
        )
        .await
        .expect("second send must not deadlock");
        assert_eq!(second, Err(ProviderError::Unavailable));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fcm_403_on_cached_token_is_permanent_and_does_not_deadlock() {
        let (provider, config, requests) = cached_fcm_provider(403).await;

        // 403 stays Permanent and keeps the cache, as before; the point is that the send returns.
        for attempt in 1..=2 {
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                provider.fcm_send(&config, "device-token", "wake", "nonce"),
            )
            .await
            .expect("cached-token 403 must not deadlock");
            assert_eq!(result, Err(ProviderError::Permanent));
            assert_eq!(requests.load(Ordering::SeqCst), attempt);
        }
        assert!(provider.fcm_token.lock().await.is_some());
    }

    #[test]
    fn fcm_payload_uses_configured_priority() {
        let payload = fcm_payload("token".into(), FcmPriority::Normal, "wake", "nonce");
        assert_eq!(payload["message"]["android"]["priority"], "normal");
    }
}
