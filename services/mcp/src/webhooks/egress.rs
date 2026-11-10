use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use thiserror::Error;
use url::{Host, Url};

const MAX_URL_BYTES: usize = 2_048;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    #[error("invalid webhook URL")]
    Invalid,
    #[error("webhook URL must use HTTPS")]
    Insecure,
    #[error("webhook URL resolves to a blocked address")]
    Blocked,
    #[error("webhook URL host could not be resolved")]
    Unresolved,
}

impl UrlError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "webhook_url_invalid",
            Self::Insecure => "webhook_url_insecure",
            Self::Blocked => "webhook_url_blocked",
            Self::Unresolved => "webhook_url_invalid",
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
    let port = url.port_or_known_default().ok_or(UrlError::Invalid)?;
    let addresses = match url.host().ok_or(UrlError::Invalid)? {
        Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Host::Domain(host) => {
            let host = normalize_domain(host);
            if blocked_host(&host, deny_hosts) {
                return Err(UrlError::Blocked);
            }
            lookup(&host, port).await?
        }
    };
    if addresses
        .iter()
        .any(|address| is_blocked_for_resolution(address.ip(), permit_loopback))
        || matches_denied_ip(&addresses, deny_hosts).await
    {
        return Err(UrlError::Blocked);
    }
    Ok(addresses)
}

/// Returns whether an IP is disallowed for webhook egress.
#[cfg(test)]
fn is_blocked_ip(ip: IpAddr) -> bool {
    is_blocked_for_resolution(ip, false)
}

fn blocked_host(host: &str, deny_hosts: &[String]) -> bool {
    let host = normalize_domain(host);
    host == "localhost"
        || host.ends_with(".localhost")
        || deny_hosts
            .iter()
            .any(|denied| ip_literal(denied).is_none() && normalize_domain(denied) == host)
}

fn normalize_domain(host: &str) -> String {
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

/// Parses an IP literal, accepting bracketed IPv6 as `Url::host_str` renders it.
fn ip_literal(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

async fn lookup(host: &str, port: u16) -> Result<Vec<SocketAddr>, UrlError> {
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| UrlError::Unresolved)?
        .collect();
    if addresses.is_empty() {
        return Err(UrlError::Unresolved);
    }
    Ok(addresses)
}

async fn matches_denied_ip(addresses: &[SocketAddr], deny_hosts: &[String]) -> bool {
    for denied in deny_hosts {
        let denied_ips = match ip_literal(denied) {
            Some(ip) => vec![ip],
            // An unresolvable deny host cannot widen egress access; ignore it.
            None => {
                let Ok(addresses) = lookup(&normalize_domain(denied), 0).await else {
                    continue;
                };
                addresses.into_iter().map(|address| address.ip()).collect()
            }
        };
        if addresses
            .iter()
            .any(|address| denied_ips.contains(&address.ip()))
        {
            return true;
        }
    }
    false
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
        || (a == 192 && b == 0 && c == 0)
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
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x2001 && segments[1] == 0)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 1);
    if native_blocked {
        return true;
    }
    if let Some(v4) = embedded_v4(ip) {
        return blocked_v4(v4, permit_loopback);
    }
    if segments[0] == 0x2002 {
        let octets = ip.octets();
        return blocked_v4(
            Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]),
            permit_loopback,
        );
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
        for (address, blocked) in [
            ("127.0.0.1", true),
            ("169.254.169.254", true),
            ("10.0.0.1", true),
            ("192.0.0.1", true),
            ("172.15.255.255", false),
            ("172.16.0.0", true),
            ("172.31.255.255", true),
            ("172.32.0.0", false),
            ("192.168.0.1", true),
            ("100.63.255.255", false),
            ("100.64.0.0", true),
            ("100.127.255.255", true),
            ("100.128.0.0", false),
            ("198.17.255.255", false),
            ("198.18.0.0", true),
            ("198.19.255.255", true),
            ("198.20.0.0", false),
            ("224.0.0.1", true),
            ("240.0.0.1", true),
            ("0.0.0.0", true),
            ("0.1.2.3", true),
            ("255.255.255.255", true),
            ("192.0.2.1", true),
            ("198.51.100.1", true),
            ("203.0.113.1", true),
            ("2001:db8::1", true),
            ("fbff::", false),
            ("fc00::", true),
            ("fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
            ("fe00::", false),
            ("::ffff:127.0.0.1", true),
            ("64:ff9b::a00:1", true),
            ("64:ff9b:1::1", true),
            ("fec0::1", true),
            ("2001::1", true),
            ("2002:0a00:0001::1", true),
            ("2002:0808:0808::1", false),
            ("8.8.8.8", false),
        ] {
            assert_eq!(
                is_blocked_ip(address.parse().expect("ip")),
                blocked,
                "{address}"
            );
        }
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
        let loopback_url = Url::parse("https://127.0.0.1").expect("url");
        assert_eq!(
            resolve_checked_with(&loopback_url, &["LOCALHOST.".to_owned()], true).await,
            Err(UrlError::Blocked)
        );
        let localhost = Url::parse("https://LOCALHOST.").expect("url");
        assert_eq!(
            resolve_checked(&localhost, &["localHOST".to_owned()]).await,
            Err(UrlError::Blocked)
        );
    }

    #[tokio::test]
    async fn literal_addresses_do_not_require_dns() {
        let public = Url::parse("https://[2606:4700:4700::1111]/").expect("url");
        assert_eq!(
            resolve_checked(&public, &[]).await.expect("public literal")[0].ip(),
            "2606:4700:4700::1111".parse::<IpAddr>().expect("ip")
        );
        let loopback = Url::parse("https://[::1]/").expect("url");
        assert_eq!(
            resolve_checked(&loopback, &[]).await,
            Err(UrlError::Blocked)
        );
    }

    #[tokio::test]
    async fn bracketed_ipv6_deny_entries_block_literal_and_resolved_addresses() {
        let denied = ["[2606:4700:4700::1111]".to_owned()];
        let url = Url::parse("https://[2606:4700:4700::1111]/x").expect("url");
        assert_eq!(resolve_checked(&url, &denied).await, Err(UrlError::Blocked));
        let address = SocketAddr::new("2606:4700:4700::1111".parse().expect("ip"), 443);
        assert!(matches_denied_ip(&[address], &denied).await);
        let other = SocketAddr::new("2606:4700:4700::1112".parse().expect("ip"), 443);
        assert!(!matches_denied_ip(&[other], &denied).await);
    }

    #[tokio::test]
    async fn unresolved_hosts_are_invalid_urls() {
        let url = Url::parse("https://does-not-exist.invalid").expect("url");
        assert_eq!(resolve_checked(&url, &[]).await, Err(UrlError::Unresolved));
    }
}
