/// Every app command is declared in the app ACL manifest. Tauri then rejects any app command
/// that a window's capability does not explicitly allow (`capabilities/*.json`), so composer
/// windows can only reach the conversation/draft/read/attachment/close commands.
const COMMANDS: &[&str] = &[
    "load_state",
    "load_history",
    "configure_server",
    "unlock_sync",
    "get_app_lock_status",
    "unlock_app",
    "set_app_lock",
    "save_draft",
    "discard_draft",
    "send_draft",
    "mark_seen",
    "pick_attachments",
    "retry_attachment",
    "save_attachment",
    "request_attachment_download",
    "media_page",
    "media_page_around",
    "media_page_newer",
    "open_media",
    "load_media",
    "close_media",
    "publish_attachment",
    "open_composer",
    "dismiss_notification",
    "dismiss_all_notifications",
    "set_app_muted",
    "mark_notifications_seen",
    "set_notification_preferences",
    "set_notification_context",
    "request_notification_permission",
    "list_contact_books",
    "list_contacts",
    "forget_contact_book",
    "submit_contact_edit",
    "list_contact_edits",
    "pick_contact_photo",
    "search_contact_recipients",
    "request_contact_repair",
    "request_contact_sync",
    "get_contact_sync_request",
    "list_restorable_contacts",
    "restore_contact",
    "list_devices",
    "revoke_device",
    "hide_head",
    "close_composer",
    "close_head_panel",
    "set_start_at_login",
    "popout_conversation",
    "acknowledge_lifecycle",
    "hosted_account",
    "hosted_sign_in",
    "hosted_sign_out",
    "hosted_open_billing",
    "hosted_provision",
    "enrollment_discover",
    "enrollment_start_over",
    "enrollment_create_owner",
    "enrollment_sign_in",
    "enrollment_connect_shared_secret",
    "enrollment_begin_vault_request",
    "enrollment_vault_request_status",
    "enrollment_create_vault",
    "enrollment_add_phone",
    "enrollment_rotate_phone",
    "enrollment_cancel_phone",
    "reveal_vault_secret",
    "enrollment_approve",
    "enrollment_status",
    "enrollment_cancel",
    "enrollment_sign_out",
    "update_status",
    "check_for_updates",
    "restart_for_update",
    "defer_update",
    "set_automatic_updates",
    "report_update_activity",
];

fn main() {
    println!("cargo:rerun-if-env-changed=PEPPY_DEV_TLS_CERT");
    println!("cargo:rerun-if-env-changed=PEPPY_UPDATE_CHANNEL");
    println!("cargo:rerun-if-env-changed=PEPPY_UPDATE_PUBLIC_KEY");
    #[cfg(feature = "dev-local-tls")]
    embed_development_tls_ca();
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}

#[cfg(feature = "dev-local-tls")]
fn embed_development_tls_ca() {
    use std::{env, fs, path::PathBuf};

    let certificate = env::var_os("PEPPY_DEV_TLS_CERT").unwrap_or_else(|| {
        panic!("dev-local-tls requires PEPPY_DEV_TLS_CERT to name the local development CA")
    });
    let certificate = PathBuf::from(certificate);
    if !certificate.is_absolute() {
        panic!("PEPPY_DEV_TLS_CERT must be an absolute path to the local development CA");
    }
    println!("cargo:rerun-if-changed={}", certificate.display());
    let pem = fs::read(&certificate).unwrap_or_else(|_| {
        panic!(
            "could not read the local development CA at {}",
            certificate.display()
        )
    });
    let pem = std::str::from_utf8(&pem)
        .unwrap_or_else(|_| panic!("the local development CA must be UTF-8 PEM certificate data"));
    let pem = pem.trim();
    if !pem.starts_with("-----BEGIN CERTIFICATE-----")
        || !pem.ends_with("-----END CERTIFICATE-----")
        || pem.matches("-----BEGIN").count() != 1
        || pem.matches("-----END").count() != 1
    {
        panic!("the local development CA must contain exactly one certificate and no extra data");
    }
    use rustls_pki_types::{pem::PemObject, CertificateDer};
    let mut certificates = CertificateDer::pem_slice_iter(pem.as_bytes());
    let certificate = certificates
        .next()
        .unwrap_or_else(|| panic!("the local development CA must contain exactly one certificate"))
        .unwrap_or_else(|_| panic!("the local development CA is not valid PEM certificate data"));
    if certificates.next().is_some() {
        panic!("the local development CA must contain exactly one certificate");
    }
    // `dev_tls.py prepare` owns CA basic-constraints and expiry validation. This build input
    // check ensures that the resulting public certificate is a Rustls trust anchor.
    rustls::RootCertStore::empty()
        .add(certificate.clone())
        .unwrap_or_else(|_| panic!("the local development CA is not a valid trust anchor"));
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
            .join("dev-local-tls-ca.der"),
        certificate.as_ref(),
    )
    .expect("could not embed the local development CA");
}
