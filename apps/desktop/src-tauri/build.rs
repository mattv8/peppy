/// Every app command is declared in the app ACL manifest. Tauri then rejects any app command
/// that a window's capability does not explicitly allow (`capabilities/*.json`), so composer
/// windows can only reach the conversation/draft/read/attachment/close commands.
const COMMANDS: &[&str] = &[
    "peek_notification_activation",
    "ack_notification_activation",
    "load_state",
    "launch_progress",
    "account_access",
    "attachment_retention_status",
    "attachment_retention_state",
    "load_history",
    "search_local",
    "advance_search_index",
    "get_history_search",
    "start_history_search",
    "cancel_history_search",
    "load_message_window",
    "open_external_url",
    "open_mcp_oauth_redirect",
    "configure_server",
    "unlock_sync",
    "get_app_lock_status",
    "retry_app_lock_check",
    "unlock_app",
    "set_app_lock",
    "save_draft",
    "discard_draft",
    "resolve_draft_conflict",
    "acknowledge_draft_notice",
    "send_draft",
    "send_again",
    "cancel_send",
    "react_to_message",
    "dismiss_reaction_request",
    "open_emoji_picker",
    "send_typing",
    "set_typing_sharing",
    "mark_seen",
    "copy_one_time_code",
    "mark_all_read",
    "pick_attachments",
    "prepare_pasted_attachment",
    "retry_attachment",
    "retry_rcs_media_download",
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
    "request_sender_unblock",
    "set_conversation_folder",
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
    "mcp_status",
    "mcp_enable",
    "mcp_grants",
    "mcp_revoke_grant",
    "webhooks_list",
    "webhook_create",
    "webhook_update",
    "webhook_delete",
    "webhook_reveal_secret",
    "webhook_rotate_secret",
    "webhook_test",
    "mcp_authorization",
    "mcp_approve_authorization",
    "mcp_deny_authorization",
    "hide_head",
    "close_composer",
    "set_maximize_button_region",
    "close_head_panel",
    "set_start_at_login",
    "startup_status",
    "set_open_in_background",
    "open_login_items",
    "get_notification_sound",
    "set_notification_sound",
    "get_notification_persistence",
    "set_notification_persistence",
    "get_notification_quick_reply",
    "set_notification_quick_reply",
    "notification_reply_recoveries",
    "claim_notification_reply_restore",
    "release_notification_reply_restore",
    "ack_notification_reply_restore",
    "discard_notification_reply",
    "popout_conversation",
    "acknowledge_lifecycle",
    "hosted_account",
    "hosted_session_renew",
    "hosted_origin_url",
    "hosted_sign_in",
    "hosted_sign_out",
    "hosted_open_billing",
    "hosted_provision",
    "hosted_delete_account",
    "hosted_deletion_status",
    "hosted_cancel_account_deletion",
    "purchase_options",
    "store_products",
    "store_purchase",
    "store_restore",
    "open_store_subscriptions",
    "open_play_subscriptions",
    "billing_summary",
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
    link_storekit_bridge();
    #[cfg(feature = "dev-local-tls")]
    embed_development_tls_ca();
    let mut attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    if embed_windows_common_controls_manifest() {
        // The linker owns the sole application manifest on this target. tauri-winres would
        // otherwise emit a second RT_MANIFEST resource that collides with it.
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}

/// The StoreKit bridge is built only for macOS targets.
#[cfg(not(target_os = "macos"))]
fn link_storekit_bridge() {}

#[cfg(target_os = "macos")]
fn link_storekit_bridge() {
    use std::{
        env,
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    fn find_archive(directory: &Path) -> Option<PathBuf> {
        for entry in fs::read_dir(directory).ok()?.filter_map(Result::ok) {
            let path = entry.path();
            if path.file_name() == Some(OsStr::new("libStoreKitBridge.a")) {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(archive) = find_archive(&path) {
                    return Some(archive);
                }
            }
        }
        None
    }

    if env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(OsStr::new("macos")) {
        return;
    }

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        Ok(arch) => panic!("unsupported macOS StoreKit bridge architecture: {arch}"),
        Err(_) => panic!("Cargo did not set CARGO_CFG_TARGET_ARCH"),
    };
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    let deployment_target =
        env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "11.0".to_string());
    let target = format!("{arch}-apple-macosx{deployment_target}");
    let configuration = if env::var("PROFILE").as_deref() == Ok("release") {
        "release"
    } else {
        "debug"
    };
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let package_path = manifest_dir.join("native/storekit");
    let scratch_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("swift-build/StoreKitBridge");
    let sdk_path = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .expect("could not invoke xcrun for the macOS SDK");
    assert!(sdk_path.status.success(), "could not locate the macOS SDK");
    let sdk_path = String::from_utf8(sdk_path.stdout).expect("xcrun returned a non-UTF-8 SDK path");
    let status = Command::new("swift")
        .args(["build", "--sdk", sdk_path.trim(), "-c", configuration])
        .args(["--triple", &target, "--package-path"])
        .arg(&package_path)
        .args(["--scratch-path"])
        .arg(&scratch_path)
        .status()
        .expect("could not invoke swift build");
    assert!(status.success(), "could not compile StoreKitBridge");

    let archive = find_archive(&scratch_path)
        .unwrap_or_else(|| panic!("Swift build did not produce libStoreKitBridge.a"));
    let architectures = Command::new("lipo")
        .args(["-archs"])
        .arg(&archive)
        .output()
        .expect("could not inspect StoreKitBridge architecture");
    assert!(
        architectures.status.success()
            && String::from_utf8_lossy(&architectures.stdout).trim() == arch,
        "StoreKitBridge archive does not match the requested {arch} architecture"
    );
    let products = archive
        .parent()
        .expect("StoreKitBridge archive has no parent");
    println!("cargo:rustc-link-search=native={}", products.display());
    println!("cargo:rustc-link-lib=static=StoreKitBridge");

    let target_info = Command::new("swift")
        .args(["-target", &target, "-print-target-info"])
        .output()
        .expect("could not query Swift target information");
    assert!(
        target_info.status.success(),
        "could not query Swift target information"
    );
    let target_info: serde_json::Value = serde_json::from_slice(&target_info.stdout)
        .expect("Swift returned invalid target information");
    let paths = target_info["paths"]["runtimeLibraryPaths"]
        .as_array()
        .expect("Swift target information omitted runtimeLibraryPaths");
    for path in paths {
        let path = path
            .as_str()
            .expect("Swift target information includes a non-string runtime path");
        println!("cargo:rustc-link-search=native={path}");
    }
    let clang_search_dirs = Command::new("/usr/bin/clang")
        .arg("--print-search-dirs")
        .output()
        .expect("could not query clang search paths");
    assert!(
        clang_search_dirs.status.success(),
        "could not query clang search paths"
    );
    let clang_search_dirs = String::from_utf8_lossy(&clang_search_dirs.stdout);
    let clang_library_path = clang_search_dirs
        .lines()
        .find_map(|line| line.strip_prefix("libraries: ="))
        .expect("clang did not report library search paths");
    println!("cargo:rustc-link-lib=clang_rt.osx");
    println!("cargo:rustc-link-search={clang_library_path}/lib/darwin");
    println!("cargo:rerun-if-changed={}", package_path.display());
    // App Store builds call StoreKit only on macOS 12+; direct macOS 11 builds need this weak link.
    println!("cargo:rustc-link-arg=-Wl,-weak-lswift_Concurrency");
    // The Swift concurrency runtime ships in newer OS versions; executables must find it there.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
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
