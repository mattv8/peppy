use peppy_client_core::{DeviceId, KeyProfile, VaultCheckHeader, VaultId};
use peppy_crypto::{create_vault_check_header, derive_root_key};
use peppy_server::api::create_owner;
use sqlx::PgPool;
use std::{future::Future, time::Duration};

pub(crate) const PHRASE: &str = "correct horse battery staple";
pub(crate) const ADDRESS: &str = "+15555550100";
pub(crate) const NETWORK_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct VaultFixture {
    pub(crate) vault: VaultId,
    pub(crate) profile: KeyProfile,
    pub(crate) header: VaultCheckHeader,
    pub(crate) fingerprint: String,
    // Read only by sms_end_to_end; recovery_end_to_end compiles this module
    // without reading it, so allow dead_code on this field alone.
    #[allow(dead_code)]
    pub(crate) owner_token: String,
}

pub(crate) struct Paired {
    pub(crate) id: DeviceId,
    pub(crate) token: String,
}

pub(crate) async fn network<T>(phase: &'static str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(NETWORK_TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("phase={phase} status=timeout"))
}

pub(crate) async fn create_vault(pool: &PgPool) -> VaultFixture {
    let vault = VaultId::new();
    let profile = KeyProfile::new(vault.0, 1).unwrap();
    let fingerprint = profile.fingerprint().unwrap();
    let root = derive_root_key(PHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let owner = create_owner(
        pool,
        serde_json::to_value(&profile).unwrap(),
        serde_json::to_vec(&header).unwrap(),
        fingerprint.clone(),
        1,
    )
    .await
    .unwrap();
    VaultFixture {
        vault,
        profile,
        header,
        fingerprint,
        owner_token: owner.device_token,
    }
}
