use base64::{engine::general_purpose::STANDARD, Engine as _};

const TEMPLATE: &str = include_str!("callback_page.html");
const LOGO: &[u8] = include_bytes!("../../../public/peppy-logo.svg");

pub(super) enum CallbackOutcome {
    Received,
    Cancelled,
}

pub(super) fn response(outcome: CallbackOutcome) -> String {
    let (outcome, heading) = match outcome {
        CallbackOutcome::Received => ("received", "Sign-in received"),
        CallbackOutcome::Cancelled => ("cancelled", "Sign-in cancelled"),
    };
    let logo = format!("data:image/svg+xml;base64,{}", STANDARD.encode(LOGO));
    let body = TEMPLATE
        .replace("{{OUTCOME}}", outcome)
        .replace("{{HEADING}}", heading)
        .replace("{{MESSAGE}}", "You can return to Peppy.")
        .replace("{{LOGO_SRC}}", &logo);

    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\ncache-control: no-store\r\nreferrer-policy: no-referrer\r\nx-content-type-options: nosniff\r\ncontent-security-policy: default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENS: &str = include_str!("../../../../../packages/desktop-ui/src/peppy-tokens.css");

    #[test]
    fn received_response_has_static_copy() {
        let response = response(CallbackOutcome::Received);
        let (_, body) = split_response(&response);

        assert!(body.contains("data-outcome=\"received\""));
        assert!(body.contains("<h1>Sign-in received</h1>"));
        assert!(body.contains("<p id=\"callback-message\">You can return to Peppy.</p>"));
        assert!(!body.contains("{{"));
    }

    #[test]
    fn cancelled_response_has_static_copy() {
        let response = response(CallbackOutcome::Cancelled);
        let (_, body) = split_response(&response);

        assert!(body.contains("data-outcome=\"cancelled\""));
        assert!(body.contains("<h1>Sign-in cancelled</h1>"));
        assert!(body.contains("<p id=\"callback-message\">You can return to Peppy.</p>"));
        assert!(!body.contains("{{"));
    }

    #[test]
    fn response_has_required_headers_and_length() {
        let response = response(CallbackOutcome::Received);
        let (headers, body) = split_response(&response);

        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
        for header in [
            "content-type: text/html; charset=utf-8",
            "connection: close",
            "cache-control: no-store",
            "referrer-policy: no-referrer",
            "x-content-type-options: nosniff",
            "content-security-policy: default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ] {
            assert!(headers.contains(header), "missing header: {header}");
        }
        assert_eq!(content_length(headers), body.len());
    }

    #[test]
    fn response_embeds_the_canonical_logo() {
        let response = response(CallbackOutcome::Received);
        let (_, body) = split_response(&response);
        let encoded_logo = body
            .split("src=\"data:image/svg+xml;base64,")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .expect("logo data URI");

        assert_eq!(STANDARD.decode(encoded_logo).unwrap(), LOGO);
    }

    #[test]
    fn template_tokens_match_desktop_tokens() {
        let declarations = template_token_declarations();
        assert!(declarations.len() >= 10, "expected dark and light tokens");
        for declaration in declarations {
            assert!(
                TOKENS.contains(&format!("{declaration};")),
                "token drifted from peppy-tokens.css: {declaration}"
            );
        }
    }

    fn split_response(response: &str) -> (&str, &str) {
        response.split_once("\r\n\r\n").expect("HTTP response")
    }

    fn content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .expect("content-length header")
            .parse()
            .expect("numeric content-length")
    }

    fn template_token_declarations() -> Vec<&'static str> {
        TEMPLATE
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("--peppy-") && line.ends_with(';'))
            .map(|line| line.trim_end_matches(';'))
            .filter(|line| !line.contains("Canvas"))
            .collect()
    }
}
