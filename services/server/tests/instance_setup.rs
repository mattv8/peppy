use peppy_server::instance::{
    browser_csrf_token, digest, password_hash, password_matches, validate_password,
};

#[test]
fn administrator_password_has_bounded_utf8_length() {
    assert!(validate_password("short").is_err());
    assert!(validate_password(&"a".repeat(12)).is_ok());
    assert!(validate_password(&"a".repeat(1024)).is_ok());
    assert!(validate_password(&"a".repeat(1025)).is_err());
}

#[test]
fn argon2id_verifier_rejects_wrong_password() {
    let hash = password_hash("administrator password").unwrap();
    assert!(password_matches(&hash, "administrator password"));
    assert!(!password_matches(&hash, "administrator password!"));
}

#[test]
fn argon2id_verifier_accepts_a_hash_slice_without_a_null_terminated_backing_buffer() {
    let mut hash_with_suffix = password_hash("administrator password").unwrap();
    let hash_length = hash_with_suffix.len();
    hash_with_suffix.push('x');
    let hash = &hash_with_suffix[..hash_length];

    assert!(password_matches(hash, "administrator password"));
}

#[test]
fn setup_and_session_digests_are_fixed_length() {
    assert_eq!(digest(b"setup-token").len(), 32);
}

#[test]
fn browser_csrf_tokens_are_stable_and_domain_separated_from_session_tokens() {
    let session_token = "browser-session-token";

    let csrf_token = browser_csrf_token(session_token);

    assert_eq!(csrf_token, browser_csrf_token(session_token));
    assert_ne!(csrf_token, session_token);
    assert_ne!(
        digest(csrf_token.as_bytes()),
        digest(session_token.as_bytes())
    );
}
