use std::{fs, path::Path};

#[test]
fn test_harnesses_activate_common_controls_v6() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = crate_root.join("windows-common-controls-manifest.xml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", manifest_path.display()));
    let build_script = fs::read_to_string(crate_root.join("build.rs"))
        .expect("failed to read the desktop build script");

    assert!(manifest.contains("Microsoft.Windows.Common-Controls"));
    assert!(manifest.contains("version=\"6.0.0.0\""));
    assert!(build_script.contains("WindowsAttributes::new_without_app_manifest()"));
    assert!(build_script.contains("if embed_windows_common_controls_manifest()"));
    assert!(build_script.contains("cargo:rustc-link-arg=/MANIFEST:EMBED"));
    assert!(build_script.contains("cargo:rustc-link-arg=/MANIFESTINPUT:"));
}
