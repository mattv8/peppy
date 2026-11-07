use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use thiserror::Error;
use url::Url;

const MAX_URL_BYTES: usize = 2_048;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    #[error("invalid webhook URL")]
    Invalid,
    #[error("webhook URL must use HTTPS")]
    Insecure,
    #[error("webhook URL resolves to a blocked address")]
    Blocked,
}

impl UrlError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "webhook_url_invalid",
            Self::Insecure => "webhook_url_insecure",
            Self::Blocked => "webhook_url_blocked",
        }
    }
}

/// Validates an endpoint URL before it is stored.
pub fn validate_url(raw: &str) -> Result<Url, UrlError> {
    if raw.len() > MAX_URL_BYTES {
        return Err(UrlError::Invalid);
    }
    let mut url = Url::parse(raw).map_err(|_| UrlError::Invalid)?;
    if url.scheme() != "https" {
        return Err(UrlError::Insecure);
    }
    if url.host_str().is_none() {
        return Err(UrlError::Invalid);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::Invalid);
    }
    url.set_fragment(None);
    Ok(url)
}

/// Resolves and validates every address used for an endpoint.
pub async fn resolve_checked(
    url: &Url,
    deny_hosts: &[String],
) -> Result<Vec<SocketAddr>, UrlError> {
    resolve_checked_with(url, deny_hosts, false).await
}

/// Test harness only: permits loopback after all other address checks.
pub(crate) async fn resolve_checked_with(
    url: &Url,
    deny_hosts: &[String],
    permit_loopback: bool,
) -> Result<Vec<SocketAddr>, UrlError> {
    let host = url.host_str().ok_or(UrlError::Invalid)?;
    if blocked_host(host, deny_hosts) {
        return Err(UrlError::Blocked);
    }
    let port = url.port_or_known_default().ok_or(UrlError::Invalid)?;
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| UrlError::Blocked)?
        .collect();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| is_blocked_for_resolution(address.ip(), permit_loopback))
    {
        return Err(UrlError::Blocked);
    }
    Ok(addresses)
}

/// Returns whether an IP is disallowed for webhook egress.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    is_blocked_for_resolution(ip, false)
}

fn blocked_host(host: &str, deny_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || deny_hosts
            .iter()
            .any(|denied| denied.eq_ignore_ascii_case(&host))
}

fn is_blocked_for_resolution(ip: IpAddr, permit_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(ip) => blocked_v4(ip, permit_loopback),
        IpAddr::V6(ip) => blocked_v6(ip, permit_loopback),
    }
}

fn blocked_v4(ip: Ipv4Addr, permit_loopback: bool) -> bool {
    let [a, b, c, d] = ip.octets();
    let always_blocked = (!permit_loopback && a == 127)
        || (a == 0 && b == 0 && c == 0 && d == 0)
        || (224..=239).contains(&a)
        || (a == 169 && b == 254)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || (a == 255 && b == 255 && c == 255 && d == 255);
    always_blocked
        || a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (18..=19).contains(&b))
        || a == 0
        || a >= 240
}

fn blocked_v6(ip: Ipv6Addr, permit_loopback: bool) -> bool {
    // Native IPv6 classes are checked first so that `::1` and `::` are never
    // mistaken for IPv4-compatible `0.0.0.1` / `0.0.0.0`.
    let segments = ip.segments();
    let native_blocked = (!permit_loopback && ip.is_loopback())
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8);
    if native_blocked {
        return true;
    }
    if let Some(v4) = embedded_v4(ip) {
        return blocked_v4(v4, permit_loopback);
    }
    (segments[0] & 0xfe00) == 0xfc00
}

/// IPv4 addresses carried inside IPv6: IPv4-mapped `::ffff:0:0/96`,
/// IPv4-compatible `::/96` (excluding `::` and `::1`), and NAT64 `64:ff9b::/96`.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let octets = ip.octets();
    let is_v4_compatible = octets[..12].iter().all(|byte| *byte == 0)
        && octets[12..].iter().any(|byte| *byte != 0)
        && !ip.is_loopback();
    let is_v4_mapped =
        octets[..10].iter().all(|byte| *byte == 0) && octets[10] == 0xff && octets[11] == 0xff;
    let is_nat64 = octets[..12] == [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0];
    (is_v4_compatible || is_v4_mapped || is_nat64)
        .then(|| Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_normalizes_urls() {
        let url = validate_url("https://example.com/path#fragment").expect("valid URL");
        assert_eq!(url.as_str(), "https://example.com/path");
        assert_eq!(validate_url("http://example.com"), Err(UrlError::Insecure));
        assert_eq!(
            validate_url("https://user@example.com"),
            Err(UrlError::Invalid)
        );
    }

    #[test]
    fn blocks_special_and_private_addresses() {
        assert!(is_blocked_ip("127.0.0.1".parse().expect("ip")));
        assert!(is_blocked_ip("169.254.169.254".parse().expect("ip")));
        assert!(is_blocked_ip("10.0.0.1".parse().expect("ip")));
        assert!(is_blocked_ip("::ffff:127.0.0.1".parse().expect("ip")));
        assert!(is_blocked_ip("64:ff9b::a00:1".parse().expect("ip")));
        assert!(!is_blocked_ip("8.8.8.8".parse().expect("ip")));
    }

    #[test]
    fn native_ipv6_classes_are_not_read_as_embedded_ipv4() {
        // Regression: `::1` must not parse as IPv4-compatible `0.0.0.1`.
        // Native classes are checked before embedded IPv4.
        for (address, blocked) in [
            ("::1", true),
            ("::", true),
            ("::ffff:127.0.0.1", true),
            ("::ffff:10.0.0.1", true),
            ("64:ff9b::a9fe:a9fe", true),
            ("fe80::1", true),
            ("ff02::1", true),
        ] {
            let ip: IpAddr = address.parse().expect("ip");
            assert_eq!(is_blocked_ip(ip), blocked, "{address}");
        }
    }

    #[tokio::test]
    async fn resolution_rejects_denied_and_loopback_hosts() {
        let loopback = Url::parse("http://127.0.0.1:8080").expect("url");
        assert_eq!(
            resolve_checked(&loopback, &[]).await,
            Err(UrlError::Blocked)
        );
        assert_eq!(
            resolve_checked_with(&loopback, &[], true)
                .await
                .expect("permitted")[0]
                .ip(),
            "127.0.0.1".parse::<IpAddr>().expect("ip")
        );
        let public = Url::parse("https://8.8.8.8").expect("url");
        assert_eq!(
            resolve_checked(&public, &["8.8.8.8".to_owned()]).await,
            Err(UrlError::Blocked)
        );
    }
}
