/// Every app command is declared in the app ACL manifest. Tauri then rejects any app command
/// that a window's capability does not explicitly allow (`capabilities/*.json`), so composer
/// windows can only reach the conversation/draft/read/attachment/close commands.
const COMMANDS: &[&str] = &[
    "peek_notification_activation",
    "ack_notification_activation",
    "load_state",
    "load_history",
    "search_local",
    "advance_search_index",
    "get_history_search",
    "start_history_search",
    "cancel_history_search",
    "load_message_window",
    "open_external_url",
    "configure_server",
    "unlock_sync",
    "get_app_lock_status",
    "unlock_app",
    "set_app_lock",
    "save_draft",
    "discard_draft",
    "send_draft",
    "mark_seen",
    "mark_all_read",
    "pick_attachments",
    "retry_attachment",
    "save_attachment",
    "request_attachment_download",
    "media_sync_request",
    "media_page",
    "media_page_around",
    "media_page_newer",
    "open_media",
    "load_media",
    "hydrate_media_preview",
    "copy_attachment_image",
    "close_media",
    "open_media_viewer",
    "publish_attachment",
    "open_composer",
    "dismiss_notification",
    "dismiss_all_notifications",
    "set_app_muted",
    "set_conversation_pinned",
    "set_conversation_tone",
    "mark_notifications_seen",
    "set_notification_preferences",
    "set_notification_context",
    "request_notification_permission",
    "open_notification_settings",
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
    "set_maximize_button_region",
    "close_head_panel",
    "set_start_at_login",
    "get_notification_sound",
    "set_notification_sound",
    "popout_conversation",
    "acknowledge_lifecycle",
    "hosted_account",
    "hosted_session_renew",
    "hosted_origin_url",
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
    "enrollment_begin_computer_claimant",
    "enrollment_computer_claimant_status",
    "enrollment_confirm_computer_claimant",
    "enrollment_cancel_computer_claimant",
    "enrollment_discard_computer_claimant",
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
    let mut attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    if embed_windows_common_controls_manifest() {
        // The linker owns the sole application manifest on this target. tauri-winres would
        // otherwise emit a second RT_MANIFEST resource that collides with it.
        attributes = attributes.windows_attributes(
            tauri_build::WindowsAttributes::new_without_app_manifest(),
        );
    }
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}

/// Embeds the Common Controls v6 manifest through the MSVC linker for every package artifact,
/// including the lib unit-test harness that Cargo gives no link-argument scope. Returns `true`
/// when the linker owns the application manifest.
fn embed_windows_common_controls_manifest() -> bool {
    use std::{env, ffi::OsStr, path::PathBuf};

    let target_os = env::var_os("CARGO_CFG_TARGET_OS");
    let target_env = env::var_os("CARGO_CFG_TARGET_ENV");
    if target_os.as_deref() != Some(OsStr::new("windows"))
        || target_env.as_deref() != Some(OsStr::new("msvc"))
    {
        return false;
    }

    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"))
            .join("windows-common-controls-manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    true
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
