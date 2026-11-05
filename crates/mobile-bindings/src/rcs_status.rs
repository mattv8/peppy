//! Read-only, sanitized RCS capability projections for native display surfaces.
//!
//! These records communicate capability state only; they never authorize a send
//! or other carrier effect.

use peppy_client_core::{
    RcsCapabilityState, RcsEligibility, RcsEligibilityProjection, RcsLinkStatusProjection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsLinkState {
    Linked,
    Unlinked,
    NeedsRelink,
    SignedOut,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsLinkStatus {
    pub state: NativeRcsLinkState,
    /// Whether the content-free capability evidence is fresh. This is display
    /// state only and never authorizes a transport effect.
    pub current: bool,
    pub availability: bool,
    pub health: bool,
    pub phone_responsive: bool,
    pub observed_at_ms: i64,
    pub capability_revision: u64,
    pub feature_bits: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsEligibility {
    Eligible,
    NotEligible,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsEligibilityProjection {
    pub conversation_id: String,
    pub gateway_device_id: String,
    /// Stale, future, unavailable, or unlinked evidence is represented as
    /// `Unknown`; it is not send authority.
    pub eligibility: NativeRcsEligibility,
    pub feature_bits: u32,
    pub mapping_revision: u64,
    pub observed_at_ms: i64,
}

pub(crate) fn link_status_view(value: RcsLinkStatusProjection) -> NativeRcsLinkStatus {
    NativeRcsLinkStatus {
        state: link_state_view(value.status.state),
        current: value.current,
        availability: value.status.availability,
        health: value.status.health,
        phone_responsive: value.status.phone_responsive,
        observed_at_ms: value.status.observed_at_ms,
        capability_revision: value.status.capability_revision,
        feature_bits: value.status.feature_bits,
    }
}

pub(crate) fn eligibility_view(value: RcsEligibilityProjection) -> NativeRcsEligibilityProjection {
    NativeRcsEligibilityProjection {
        conversation_id: value.conversation_id.to_string(),
        gateway_device_id: value.gateway_device_id.to_string(),
        eligibility: match value.eligibility {
            RcsEligibility::Eligible => NativeRcsEligibility::Eligible,
            RcsEligibility::NotEligible => NativeRcsEligibility::NotEligible,
            RcsEligibility::Unknown => NativeRcsEligibility::Unknown,
        },
        feature_bits: value.feature_bits,
        mapping_revision: value.mapping_revision,
        observed_at_ms: value.observed_at_ms,
    }
}

fn link_state_view(value: RcsCapabilityState) -> NativeRcsLinkState {
    match value {
        RcsCapabilityState::Linked => NativeRcsLinkState::Linked,
        RcsCapabilityState::Unlinked => NativeRcsLinkState::Unlinked,
        RcsCapabilityState::NeedsRelink => NativeRcsLinkState::NeedsRelink,
        RcsCapabilityState::SignedOut => NativeRcsLinkState::SignedOut,
        RcsCapabilityState::Unavailable => NativeRcsLinkState::Unavailable,
    }
}
