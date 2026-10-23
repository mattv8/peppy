use std::{env, fs, path::PathBuf, process::ExitCode};

use base64::{Engine, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};

struct Arguments {
    asset_version: String,
    expected_version: String,
    asset_dir: PathBuf,
}

fn main() -> ExitCode {
    match verify_release() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("release signature verification failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn verify_release() -> Result<(), String> {
    let arguments = parse_arguments()?;
    let public_key = load_public_key()?;
    for (_, asset) in release_targets(&arguments.asset_version) {
        verify_asset(&public_key, &arguments.asset_dir, asset, &arguments.expected_version)?;
    }
    Ok(())
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut arguments = env::args().skip(1);
    let asset_version = arguments.next().ok_or("usage: manifest-verifier <version> <asset-dir> [expected-version]")?;
    let asset_dir = arguments.next().ok_or("usage: manifest-verifier <version> <asset-dir>")?;
    let expected_version = arguments.next().unwrap_or_else(|| asset_version.clone());
    if arguments.next().is_some() { return Err("usage: manifest-verifier <version> <asset-dir> [expected-version]".into()); }
    Ok(Arguments { asset_version, expected_version, asset_dir: asset_dir.into() })
}

fn load_public_key() -> Result<PublicKey, String> {
    let encoded_key = env::var("PEPPY_UPDATE_PUBLIC_KEY")
        .map_err(|_| "PEPPY_UPDATE_PUBLIC_KEY is required")?;
    let decoded_key = STANDARD
        .decode(encoded_key)
        .map_err(|_| "PEPPY_UPDATE_PUBLIC_KEY is not base64")?;
    let key_text = String::from_utf8(decoded_key)
        .map_err(|_| "PEPPY_UPDATE_PUBLIC_KEY is not UTF-8")?;
    PublicKey::decode(&key_text).map_err(|error| format!("invalid updater public key: {error}"))
}

fn verify_asset(public_key: &PublicKey, asset_dir: &PathBuf, asset: String, version: &str) -> Result<(), String> {
    let asset_path = asset_dir.join(&asset);
    let signature_path = asset_dir.join(format!("{asset}.sig"));
    let asset_bytes = fs::read(&asset_path)
        .map_err(|error| format!("cannot read {}: {error}", asset_path.display()))?;
    let encoded_signature = fs::read_to_string(&signature_path)
        .map_err(|error| format!("cannot read {}: {error}", signature_path.display()))?;
    let decoded_signature = STANDARD
        .decode(encoded_signature.trim())
        .map_err(|_| format!("cannot decode {}", signature_path.display()))?;
    let signature_text = String::from_utf8(decoded_signature)
        .map_err(|_| format!("{} is not UTF-8", signature_path.display()))?;
    let signature = Signature::decode(&signature_text)
        .map_err(|error| format!("cannot read {}: {error}", signature_path.display()))?;
    public_key
        .verify(&asset_bytes, &signature, false)
        .map_err(|error| format!("{}: {error}", asset_path.display()))?;
    let signed_version = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"));
    if signed_version == Some(version) {
        Ok(())
    } else {
        Err(format!("{} is not signed for version {version}", asset_path.display()))
    }
}

fn release_targets(version: &str) -> [(String, String); 6] {
    [
        ("linux-x86_64-appimage".into(), format!("peppy-{version}-linux-x86_64.AppImage")),
        ("linux-x86_64-deb".into(), format!("peppy-{version}-linux-x86_64.deb")),
        ("linux-x86_64-rpm".into(), format!("peppy-{version}-linux-x86_64.rpm")),
        ("darwin-aarch64-app".into(), format!("peppy-{version}-darwin-aarch64.app.tar.gz")),
        ("windows-x86_64-nsis".into(), format!("peppy-{version}-windows-x86_64-setup.exe")),
        ("windows-x86_64-msi".into(), format!("peppy-{version}-windows-x86_64.msi")),
    ]
}
