use url::Url;

const MAX_NAME_BYTES: usize = 80;

/// Derives a compact display name from an endpoint URL.
pub fn derive_name(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    let name = host.strip_prefix("www.").unwrap_or(host);
    if name.is_empty() {
        return "Webhook".to_owned();
    }
    name.chars().take(MAX_NAME_BYTES).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_name_from_host() {
        assert_eq!(
            derive_name(&Url::parse("https://www.example.com/hook").expect("url")),
            "example.com"
        );
    }
}
