//! Optional local-development TLS trust. The embedded CA is never used for a remote origin.
use crate::error::{BridgeError, BridgeResult};

fn configuration_error() -> BridgeError {
    BridgeError::new(
        "local-tls-config",
        "The bundled local development TLS CA is invalid. Re-run the desktop development helper with DEBUG=true.",
    )
}

#[cfg(feature = "dev-local-tls")]
const DEVELOPMENT_CA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dev-local-tls-ca.der"));

fn additional_ca<'a>(origin: &str, certificate: Option<&'a [u8]>) -> Option<&'a [u8]> {
    if crate::origin::is_loopback_https(origin) {
        return certificate;
    }
    None
}

pub(crate) fn http_builder_with(
    origin: &str,
    certificate: Option<&[u8]>,
) -> BridgeResult<reqwest::ClientBuilder> {
    let builder = reqwest::Client::builder();
    let Some(certificate) = additional_ca(origin, certificate) else {
        return Ok(builder);
    };
    let certificate =
        reqwest::Certificate::from_der(certificate).map_err(|_| configuration_error())?;
    Ok(builder.add_root_certificate(certificate))
}

/// Adds the opt-in development root only for HTTPS loopback endpoints.
pub fn http_builder(origin: &str) -> BridgeResult<reqwest::ClientBuilder> {
    #[cfg(feature = "dev-local-tls")]
    return http_builder_with(origin, Some(DEVELOPMENT_CA));
    #[cfg(not(feature = "dev-local-tls"))]
    http_builder_with(origin, None)
}

pub(crate) fn websocket_connector_with(
    origin: &str,
    certificate: Option<&[u8]>,
) -> BridgeResult<Option<tokio_tungstenite::Connector>> {
    let Some(certificate) = additional_ca(origin, certificate) else {
        return Ok(None);
    };
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(native.certs);
    roots
        .add(rustls::pki_types::CertificateDer::from(
            certificate.to_vec(),
        ))
        .map_err(|_| configuration_error())?;
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Some(tokio_tungstenite::Connector::Rustls(
        std::sync::Arc::new(config),
    )))
}

/// Supplies a Rustls connector only when the opt-in CA applies. Native roots remain installed.
pub fn websocket_connector(origin: &str) -> BridgeResult<Option<tokio_tungstenite::Connector>> {
    #[cfg(feature = "dev-local-tls")]
    return websocket_connector_with(origin, Some(DEVELOPMENT_CA));
    #[cfg(not(feature = "dev-local-tls"))]
    websocket_connector_with(origin, None)
}

#[cfg(test)]
pub(crate) fn trusts_development_ca(origin: &str, certificate: Option<&[u8]>) -> bool {
    additional_ca(origin, certificate).is_some()
}
