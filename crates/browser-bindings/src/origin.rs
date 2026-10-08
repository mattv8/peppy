use url::Url;

pub(crate) fn canonical_origin(input: &str) -> Result<String, ()> {
    let url = Url::parse(input).map_err(|_| ())?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(());
    }
    Ok(url.origin().ascii_serialization())
}
