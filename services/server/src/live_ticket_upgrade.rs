use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use url::Origin;

const PREFIX: &str = "peppy-live-v2.";
const MAX_PROTOCOL_LENGTH: usize = 160;

#[derive(Debug, Eq, PartialEq)]
pub enum UpgradeMode {
    Bearer,
    Ticket {
        session_id: [u8; 16],
        secret: [u8; 32],
        protocol_token: String,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub enum UpgradeRejection {
    ProtocolInvalid,
    AuthModeMixed,
    OriginForbidden,
    Unsupported,
}

impl UpgradeRejection {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ProtocolInvalid => "live_ticket_protocol_invalid",
            Self::AuthModeMixed => "live_auth_mode_mixed",
            Self::OriginForbidden => "live_origin_forbidden",
            Self::Unsupported => "live_ticket_unsupported",
        }
    }
}

pub fn classify_live_upgrade(
    headers: &HeaderMap,
    trusted: Option<&Origin>,
) -> Result<UpgradeMode, UpgradeRejection> {
    let protocols: Vec<_> = headers.get_all("sec-websocket-protocol").iter().collect();
    let authorizations: Vec<_> = headers.get_all("authorization").iter().collect();
    if protocols.is_empty() {
        return if authorizations.len() == 1 {
            Ok(UpgradeMode::Bearer)
        } else {
            Err(UpgradeRejection::ProtocolInvalid)
        };
    }
    if protocols.len() != 1 || !authorizations.is_empty() {
        return Err(if authorizations.is_empty() {
            UpgradeRejection::ProtocolInvalid
        } else {
            UpgradeRejection::AuthModeMixed
        });
    }
    let token = protocols[0]
        .to_str()
        .map_err(|_| UpgradeRejection::ProtocolInvalid)?;
    let ticket = parse_ticket(token)?;
    let trusted = trusted.ok_or(UpgradeRejection::Unsupported)?;
    let origins: Vec<_> = headers.get_all("origin").iter().collect();
    if origins.len() != 1 {
        return Err(UpgradeRejection::OriginForbidden);
    }
    let origin = origins[0]
        .to_str()
        .map_err(|_| UpgradeRejection::OriginForbidden)?;
    let parsed: url::Url = origin
        .parse()
        .map_err(|_| UpgradeRejection::OriginForbidden)?;
    if parsed.origin() != *trusted || parsed.origin().ascii_serialization() != origin {
        return Err(UpgradeRejection::OriginForbidden);
    }
    Ok(ticket)
}

fn parse_ticket(token: &str) -> Result<UpgradeMode, UpgradeRejection> {
    if token.len() > MAX_PROTOCOL_LENGTH || token.contains(',') {
        return Err(UpgradeRejection::ProtocolInvalid);
    }
    let Some(body) = token.strip_prefix(PREFIX) else {
        return Err(UpgradeRejection::ProtocolInvalid);
    };
    let mut parts = body.split('.');
    let (Some(session), Some(secret), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(UpgradeRejection::ProtocolInvalid);
    };
    let session = decode::<16>(session)?;
    let secret = decode::<32>(secret)?;
    Ok(UpgradeMode::Ticket {
        session_id: session,
        secret,
        protocol_token: token.to_owned(),
    })
}

fn decode<const N: usize>(value: &str) -> Result<[u8; N], UpgradeRejection> {
    if value.is_empty() || value.contains('=') {
        return Err(UpgradeRejection::ProtocolInvalid);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| UpgradeRejection::ProtocolInvalid)?;
    if bytes.len() != N || URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(UpgradeRejection::ProtocolInvalid);
    }
    bytes
        .try_into()
        .map_err(|_| UpgradeRejection::ProtocolInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn ticket() -> String {
        format!(
            "{PREFIX}{}.{}",
            URL_SAFE_NO_PAD.encode([1_u8; 16]),
            URL_SAFE_NO_PAD.encode([2_u8; 32])
        )
    }
    fn trusted() -> Origin {
        "https://web.peppy.test"
            .parse::<url::Url>()
            .unwrap()
            .origin()
    }
    fn headers(protocol: Option<&str>, origin: Option<&str>, bearer: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(protocol) = protocol {
            headers.append(
                "sec-websocket-protocol",
                HeaderValue::from_str(protocol).unwrap(),
            );
        }
        if let Some(origin) = origin {
            headers.append("origin", HeaderValue::from_str(origin).unwrap());
        }
        if bearer {
            headers.append(
                "authorization",
                HeaderValue::from_static("Bearer synthetic"),
            );
        }
        headers
    }
    #[test]
    fn live_ticket_upgrade_table_driven_validation() {
        let token = ticket();
        let cases = [
            (headers(None, None, true), None, Ok(UpgradeMode::Bearer)),
            (
                headers(Some(&token), Some("https://web.peppy.test"), false),
                Some(trusted()),
                Ok(UpgradeMode::Ticket {
                    session_id: [1; 16],
                    secret: [2; 32],
                    protocol_token: token.clone(),
                }),
            ),
            (
                headers(Some(&token), Some("https://web.peppy.test"), true),
                Some(trusted()),
                Err(UpgradeRejection::AuthModeMixed),
            ),
            (
                headers(Some(&token), Some("https://wrong.peppy.test"), false),
                Some(trusted()),
                Err(UpgradeRejection::OriginForbidden),
            ),
            (
                headers(
                    Some("peppy-live-v2.bad"),
                    Some("https://web.peppy.test"),
                    false,
                ),
                Some(trusted()),
                Err(UpgradeRejection::ProtocolInvalid),
            ),
            (
                headers(Some(&token), Some("https://web.peppy.test"), false),
                None,
                Err(UpgradeRejection::Unsupported),
            ),
        ];
        for (headers, trusted, expected) in cases {
            assert_eq!(classify_live_upgrade(&headers, trusted.as_ref()), expected);
        }
    }
}
