//! Emits deterministic synthetic input for the compiled tester-crypto WASM smoke test.

use peppy_crypto::{CRYPTO_SUITE_1, KeyProfile, create_vault_check_header, derive_root_key};
use peppy_hosted_api::tester_admin_v2::{VaultBindingV2, VaultCheckHeaderV2};
use serde_json::json;
use uuid::Uuid;

fn id(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("fixture UUID is valid")
}

fn main() {
    let profile = KeyProfile {
        crypto_suite: CRYPTO_SUITE_1,
        salt: [42; 16],
        vault_id: id("11111111-1111-1111-1111-111111111111"),
        key_epoch: 7,
    };
    let passphrase = "tester smoke phrase";
    let root = derive_root_key(passphrase, &profile).expect("fixture root derives");
    let vault_check_header: VaultCheckHeaderV2 = create_vault_check_header(&root, profile.clone())
        .expect("fixture header encrypts")
        .into();
    let binding = VaultBindingV2 {
        account_id: id("22222222-2222-2222-2222-222222222222"),
        vault_id: profile.vault_id,
        profile_fingerprint: profile.fingerprint().expect("fixture profile fingerprints"),
        key_epoch: profile.key_epoch,
        profile: profile.into(),
    };
    println!(
        "{}",
        serde_json::to_string(&json!({
            "passphrase": passphrase,
            "binding": binding,
            "vaultCheckHeader": vault_check_header,
            "registryId": id("33333333-3333-3333-3333-333333333333"),
            "entryId": id("44444444-4444-4444-4444-444444444444"),
            "keyId": id("55555555-5555-5555-5555-555555555555"),
        }))
        .expect("fixture serializes")
    );
}
