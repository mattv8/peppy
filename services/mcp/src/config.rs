use std::path::PathBuf;

use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    /// Fixed private API transport endpoint. Never selected by a caller.
    pub api_url: String,
    /// Canonical public origin used in the authenticated enrollment context.
    pub public_api_url: String,
    pub state_dir: PathBuf,
    pub key_file: PathBuf,
    pub worker_token_file: PathBuf,
    pub bind_addr: std::net::SocketAddr,
    pub webhooks_enabled: bool,
    pub webhooks_allow_private: bool,
    pub app_origin: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("PUBLIC_API_URL must be a canonical HTTPS origin, or loopback HTTP for development")]
    InvalidPublicApiUrl,
    #[error("PEPPY_MCP_API_URL must be a canonical private HTTP(S) origin")]
    InvalidPrivateApiUrl,
    #[error("PEPPY_MCP_BIND_ADDR must be a socket address")]
    InvalidBindAddress,
    #[error("PEPPY_MCP_STATE_DIR is required")]
    MissingStateDir,
}

impl WorkerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_getter(|name| std::env::var(name).ok())
    }

    fn from_getter(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let public_api_url = get("PUBLIC_API_URL").ok_or(ConfigError::InvalidPublicApiUrl)?;
        let public_api_url = canonical_public_api_url(&public_api_url)?;
        let state_dir = get("PEPPY_MCP_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/peppy-mcp"));
        let api_url = get("PEPPY_MCP_API_URL").ok_or(ConfigError::InvalidPrivateApiUrl)?;
        let api_url = canonical_private_api_url(&api_url)?;
        let key_file = get("PEPPY_MCP_KEY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_dir.join("storage-key"));
        let worker_token_file = get("PEPPY_MCP_WORKER_TOKEN_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/run/peppy-mcp/control-token"));
        let bind_addr = get("PEPPY_MCP_BIND_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8091".into())
            .parse()
            .map_err(|_| ConfigError::InvalidBindAddress)?;
        let app_origin = app_origin(get("PEPPY_WEB_CLIENT_HOST"), &public_api_url);
        Ok(Self {
            api_url,
            public_api_url,
            state_dir,
            key_file,
            worker_token_file,
            bind_addr,
            webhooks_enabled: enabled(get("PEPPY_WEBHOOKS_ENABLED")),
            webhooks_allow_private: enabled(get("PEPPY_WEBHOOKS_ALLOW_PRIVATE_NETWORKS")),
            app_origin,
        })
    }
}

fn enabled(value: Option<String>) -> bool {
    value.is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn app_origin(host: Option<String>, fallback: &str) -> String {
    host.and_then(|host| {
        let parsed = url::Url::parse(&format!("https://{host}")).ok()?;
        (parsed.host_str().is_some() && parsed.path() == "/" && parsed.username().is_empty())
            .then(|| parsed.origin().ascii_serialization())
    })
    .unwrap_or_else(|| fallback.to_owned())
}

fn canonical_public_api_url(value: &str) -> Result<String, ConfigError> {
    let parsed = url::Url::parse(value).map_err(|_| ConfigError::InvalidPublicApiUrl)?;
    let loopback = match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if (!matches!(parsed.scheme(), "https") && !(parsed.scheme() == "http" && loopback))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ConfigError::InvalidPublicApiUrl);
    }
    Ok(parsed.origin().ascii_serialization())
}

fn canonical_private_api_url(value: &str) -> Result<String, ConfigError> {
    let parsed = url::Url::parse(value).map_err(|_| ConfigError::InvalidPrivateApiUrl)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ConfigError::InvalidPrivateApiUrl);
    }
    Ok(parsed.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_api_url_matches_enrollment_origin_policy() {
        assert_eq!(
            canonical_public_api_url("https://api.example.test/").unwrap(),
            "https://api.example.test"
        );
        assert_eq!(
            canonical_public_api_url("http://localhost:8080").unwrap(),
            "http://localhost:8080"
        );
        for value in [
            "http://api.example.test",
            "https://user@api.example.test",
            "https://api.example.test/path",
            "https://api.example.test?query=1",
        ] {
            assert!(canonical_public_api_url(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn private_api_url_allows_compose_http_but_not_request_components() {
        assert_eq!(
            canonical_private_api_url("http://api:8080/").unwrap(),
            "http://api:8080"
        );
        assert_eq!(
            canonical_private_api_url("https://api.example.test").unwrap(),
            "https://api.example.test"
        );
        for value in [
            "ftp://api:8080",
            "http://user@api:8080",
            "http://api:8080/path",
            "http://api:8080?query=1",
        ] {
            assert!(
                canonical_private_api_url(value).is_err(),
                "accepted {value}"
            );
        }
    }

    #[test]
    fn webhook_environment_is_parsed_without_process_environment() {
        let config = WorkerConfig::from_getter(|name| match name {
            "PUBLIC_API_URL" => Some("https://api.example.test".into()),
            "PEPPY_MCP_API_URL" => Some("http://api:8080".into()),
            "PEPPY_WEBHOOKS_ENABLED" => Some("true".into()),
            "PEPPY_WEBHOOKS_ALLOW_PRIVATE_NETWORKS" => Some("1".into()),
            "PEPPY_WEB_CLIENT_HOST" => Some("app.example.test".into()),
            _ => None,
        })
        .unwrap();
        assert!(config.webhooks_enabled && config.webhooks_allow_private);
        assert_eq!(config.app_origin, "https://app.example.test");
    }
}
