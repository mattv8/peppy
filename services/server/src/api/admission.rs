use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, SystemTime},
};

use sha2::{Digest, Sha256};
use sqlx::PgPool;

const WINDOW: Duration = Duration::from_secs(60);
const SOURCE_LIMIT: u8 = 10;
const COMPUTER_STATUS_SOURCE_PREFILTER_LIMIT: u8 = 60;
const LOCAL_BUCKET_CAP: usize = 4096;

#[derive(Clone, Copy)]
pub(crate) enum AdmissionOperation {
    Claim,
    ClaimReplay,
    Login,
    CreateVault,
    VaultRequest,
    PhoneClaim,
    ComputerStatus,
    ComputerConfirm,
    ManualChallenge,
    ManualProof,
}

impl AdmissionOperation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::ClaimReplay => "claim-replay",
            Self::Login => "login",
            Self::CreateVault => "create-vault",
            Self::VaultRequest => "vault-request",
            Self::PhoneClaim => "phone-claim",
            Self::ComputerStatus => "computer-status",
            Self::ComputerConfirm => "computer-confirm",
            Self::ManualChallenge => "manual-challenge",
            Self::ManualProof => "manual-proof",
        }
    }

    const fn uses_instance_global_limit(self) -> bool {
        matches!(
            self,
            Self::Claim | Self::ClaimReplay | Self::Login | Self::CreateVault | Self::VaultRequest
        )
    }

    pub(crate) const fn source_limit(self) -> u8 {
        match self {
            Self::ComputerStatus => 30,
            _ => SOURCE_LIMIT,
        }
    }

    pub(crate) const fn source_prefilter_limit(self) -> u8 {
        match self {
            Self::ComputerStatus => COMPUTER_STATUS_SOURCE_PREFILTER_LIMIT,
            _ => self.source_limit(),
        }
    }
}

#[derive(Clone, Copy)]
struct LocalBucket {
    window: SystemTime,
    count: u8,
}

/// A bounded, per-process prefilter that prevents unauthenticated traffic from
/// forcing every request through the durable admission transaction.
pub(crate) struct LocalAdmissions {
    sources: HashMap<[u8; 32], LocalBucket>,
}

impl LocalAdmissions {
    pub(crate) fn new() -> Self {
        Self {
            sources: HashMap::new(),
        }
    }

    pub(crate) fn admit(&mut self, source: [u8; 32]) -> bool {
        self.admit_with_limit(source, SOURCE_LIMIT)
    }

    pub(crate) fn admit_with_limit(&mut self, source: [u8; 32], limit: u8) -> bool {
        let now = SystemTime::now();
        self.sources
            .retain(|_, bucket| now.duration_since(bucket.window).unwrap_or_default() < WINDOW);
        if let Some(bucket) = self.sources.get_mut(&source) {
            if bucket.count >= limit {
                return false;
            }
            bucket.count += 1;
            return true;
        }
        if self.sources.len() >= LOCAL_BUCKET_CAP {
            return false;
        }
        self.sources.insert(
            source,
            LocalBucket {
                window: now,
                count: 1,
            },
        );
        true
    }
}

pub(crate) enum AdmissionError {
    Limited,
    Database(sqlx::Error),
}

pub(crate) fn source_digest(operation: &[u8], peer: Option<IpAddr>) -> [u8; 32] {
    targeted_source_digest(operation, peer, &[])
}

pub(crate) fn authority_source_digest(
    operation: &[u8],
    authority: &[u8],
    peer: Option<IpAddr>,
) -> [u8; 32] {
    targeted_source_digest(operation, peer, authority)
}

pub(crate) fn targeted_source_digest(
    operation: &[u8],
    peer: Option<IpAddr>,
    target: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"instance-account-v2:");
    hasher.update(operation);
    match peer.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V4(address)) => hasher.update(address.octets()),
        Some(IpAddr::V6(address)) => hasher.update(&address.octets()[..8]),
        None => hasher.update(b"global-fallback"),
    }
    hasher.update(target);
    hasher.finalize().into()
}

pub(crate) async fn admit_durably(
    db: &PgPool,
    operation: AdmissionOperation,
    source: [u8; 32],
) -> Result<(), AdmissionError> {
    let mut tx = db.begin().await.map_err(AdmissionError::Database)?;
    sqlx::query(
        "INSERT INTO instance_admission_windows(window_started,count) VALUES(date_trunc('minute',transaction_timestamp()),0) ON CONFLICT DO NOTHING",
    )
    .execute(&mut *tx)
    .await
    .map_err(AdmissionError::Database)?;
    let source_allowed = sqlx::query_scalar::<_, i32>(
        "INSERT INTO instance_admission_sources(window_started,operation,source_digest,count) VALUES(date_trunc('minute',transaction_timestamp()),$1,$2,1) ON CONFLICT(window_started,operation,source_digest) DO UPDATE SET count=instance_admission_sources.count+1 WHERE instance_admission_sources.count < $3 RETURNING count",
    )
    .bind(operation.as_str())
    .bind(source.as_slice())
    .bind(i32::from(operation.source_limit()))
    .fetch_optional(&mut *tx)
    .await
    .map_err(AdmissionError::Database)?;
    if source_allowed.is_none() {
        return Err(AdmissionError::Limited);
    }
    if !operation.uses_instance_global_limit() {
        return tx.commit().await.map_err(AdmissionError::Database);
    }
    let global_allowed = sqlx::query_scalar::<_, i32>(
        "INSERT INTO instance_admission_windows(window_started,count) VALUES(date_trunc('minute',transaction_timestamp()),1) ON CONFLICT(window_started) DO UPDATE SET count=instance_admission_windows.count+1 WHERE instance_admission_windows.count < 100 RETURNING count",
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(AdmissionError::Database)?;
    if global_allowed.is_none() {
        return Err(AdmissionError::Limited);
    }
    tx.commit().await.map_err(AdmissionError::Database)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_digests_canonicalize_ipv4_mapped_ipv6_and_preserve_ipv6_prefixes() {
        let operation = b"claim";
        assert_eq!(
            source_digest(operation, Some("192.0.2.1".parse().unwrap())),
            source_digest(operation, Some("::ffff:192.0.2.1".parse().unwrap()))
        );
        assert_ne!(
            source_digest(operation, Some("192.0.2.1".parse().unwrap())),
            source_digest(operation, Some("192.0.2.2".parse().unwrap()))
        );
        assert_eq!(
            source_digest(operation, Some("2001:db8:1:2::1".parse().unwrap())),
            source_digest(operation, Some("2001:db8:1:2:ffff::1".parse().unwrap()))
        );
        assert_ne!(
            source_digest(operation, Some("2001:db8:1:2::1".parse().unwrap())),
            source_digest(operation, Some("2001:db8:1:3::1".parse().unwrap()))
        );
    }

    #[test]
    fn targeted_admission_keeps_enrollments_in_separate_buckets() {
        let source = Some("2001:db8:1:2::1".parse().unwrap());

        assert_ne!(
            targeted_source_digest(b"manual-proof", source, b"enrollment-a"),
            targeted_source_digest(b"manual-proof", source, b"enrollment-b")
        );
    }

    #[test]
    fn local_admission_never_evicts_a_live_bucket() {
        let mut admissions = LocalAdmissions::new();
        for value in 0..LOCAL_BUCKET_CAP {
            let mut source = [0_u8; 32];
            source[..8].copy_from_slice(&(value as u64).to_be_bytes());
            assert!(admissions.admit(source));
        }
        assert!(!admissions.admit([255; 32]));
    }

    #[test]
    fn source_prefilter_limits_random_targets_without_exhausting_other_sources() {
        let mut admissions = LocalAdmissions::new();
        let source = source_digest(b"computer-status", Some("192.0.2.1".parse().unwrap()));

        for _ in 0..30 {
            assert!(admissions.admit_with_limit(source, 30));
        }
        assert!(!admissions.admit_with_limit(source, 30));
        let legitimate_source =
            source_digest(b"computer-status", Some("192.0.2.2".parse().unwrap()));
        assert!(admissions.admit_with_limit(legitimate_source, 30));
        assert_eq!(admissions.sources.len(), 2);
    }

    #[test]
    fn hosted_phone_claims_from_one_peer_are_isolated_by_authoritative_vault() {
        let peer = Some("192.0.2.1".parse().unwrap());
        let first = authority_source_digest(b"phone-claim", b"vault-a", peer);
        let second = authority_source_digest(b"phone-claim", b"vault-b", peer);
        let mut admissions = LocalAdmissions::new();

        for _ in 0..SOURCE_LIMIT {
            assert!(admissions.admit(first));
            assert!(admissions.admit(second));
        }
        assert!(!admissions.admit(first));
        assert!(!admissions.admit(second));
    }

    #[test]
    fn hosted_phone_claims_from_one_peer_share_their_vault_source_limit() {
        let peer = Some("192.0.2.1".parse().unwrap());
        let source = authority_source_digest(b"phone-claim", b"vault-a", peer);
        let mut admissions = LocalAdmissions::new();

        for _ in 0..SOURCE_LIMIT {
            assert!(admissions.admit(source));
        }
        assert!(!admissions.admit(source));
    }

    #[test]
    fn computer_status_prefilter_allows_three_four_second_pollers_from_one_office() {
        let source = source_digest(b"computer-status", Some("192.0.2.1".parse().unwrap()));
        let mut admissions = LocalAdmissions::new();

        for _ in 0..45 {
            assert!(admissions.admit_with_limit(
                source,
                AdmissionOperation::ComputerStatus.source_prefilter_limit(),
            ));
        }
    }
}
