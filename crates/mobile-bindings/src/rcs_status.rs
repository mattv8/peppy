//! Read-only, sanitized RCS capability projections for native display surfaces.
//!
//! These records communicate capability state only; they never authorize a send
//! or other carrier effect.

use peppy_client_core::{
    RCS_CAPABILITY_STALE_MS, RcsCapabilityState, RcsEligibility, RcsEligibilityProjection,
    RcsLinkStatusProjection,
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
    /// First instant (ms) at which core reports this evidence as not current;
    /// `current` is true strictly before it. Native code must treat `now >=
    /// expires_at_ms` as not current, without rereading `current`, and schedule
    /// at most one timer for it. `None` means no valid deadline and `current` is
    /// `false`. Time expiry is separate from link state and health fields.
    pub expires_at_ms: Option<i64>,
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
    let expires_at_ms = status_expiry(value.status.observed_at_ms);
    NativeRcsLinkStatus {
        state: link_state_view(value.status.state),
        // Fail closed: evidence without a valid deadline is never display-current.
        current: value.current && expires_at_ms.is_some(),
        availability: value.status.availability,
        health: value.status.health,
        phone_responsive: value.status.phone_responsive,
        observed_at_ms: value.status.observed_at_ms,
        capability_revision: value.status.capability_revision,
        feature_bits: value.status.feature_bits,
        expires_at_ms,
    }
}

/// First instant at which core reports this evidence as not current. Core
/// treats `now - observed <= RCS_CAPABILITY_STALE_MS` as fresh, so the first
/// non-current millisecond is `observed + RCS_CAPABILITY_STALE_MS + 1`.
/// Mirrors the core observed-at validity range; overflow yields `None`.
fn status_expiry(observed_at_ms: i64) -> Option<i64> {
    if !(0..i64::MAX).contains(&observed_at_ms) {
        return None;
    }
    observed_at_ms
        .checked_add(RCS_CAPABILITY_STALE_MS)?
        .checked_add(1)
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

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{DeviceId, RcsLinkStatusEvent};

    fn projection(
        state: RcsCapabilityState,
        observed_at_ms: i64,
        current: bool,
    ) -> RcsLinkStatusProjection {
        RcsLinkStatusProjection {
            status: RcsLinkStatusEvent {
                gateway_device_id: DeviceId::new(),
                state,
                availability: false,
                health: false,
                phone_responsive: false,
                observed_at_ms,
                capability_revision: 1,
                feature_bits: 7,
                linked_since_ms: Some(observed_at_ms),
            },
            current,
        }
    }

    #[test]
    fn expiry_uses_canonical_stale_window_for_normal_timestamps() {
        let view = link_status_view(projection(RcsCapabilityState::Linked, 1_000, true));
        assert_eq!(
            view.expires_at_ms,
            Some(1_000 + RCS_CAPABILITY_STALE_MS + 1)
        );
        assert!(view.current);
    }

    #[test]
    fn expiry_is_first_non_current_instant_and_stays_representable_at_limit() {
        let observed = i64::MAX - RCS_CAPABILITY_STALE_MS - 1;
        let view = link_status_view(projection(RcsCapabilityState::Linked, observed, true));
        assert_eq!(view.expires_at_ms, Some(i64::MAX));
    }

    #[test]
    fn expiry_is_none_and_not_current_when_deadline_overflows() {
        for observed in [i64::MAX - RCS_CAPABILITY_STALE_MS, i64::MAX - 1] {
            let view = link_status_view(projection(RcsCapabilityState::Linked, observed, true));
            assert_eq!(view.expires_at_ms, None, "observed {observed}");
            assert!(!view.current, "observed {observed}");
        }
    }

    #[test]
    fn expiry_is_none_and_not_current_for_malformed_observed_timestamps() {
        for observed in [-1, i64::MAX] {
            let view = link_status_view(projection(RcsCapabilityState::Linked, observed, true));
            assert_eq!(view.expires_at_ms, None, "observed {observed}");
            assert!(!view.current, "observed {observed}");
        }
    }

    #[test]
    fn stale_status_retains_deadline_and_is_not_current() {
        let view = link_status_view(projection(RcsCapabilityState::Linked, 1_000, false));
        assert_eq!(
            view.expires_at_ms,
            Some(1_000 + RCS_CAPABILITY_STALE_MS + 1)
        );
        assert!(!view.current);
    }

    #[test]
    fn fresh_terminal_state_is_current_with_time_expiry_separate_from_capability() {
        let view = link_status_view(projection(RcsCapabilityState::Unlinked, 1_000, true));
        assert_eq!(view.state, NativeRcsLinkState::Unlinked);
        assert_eq!(
            view.expires_at_ms,
            Some(1_000 + RCS_CAPABILITY_STALE_MS + 1)
        );
        assert!(view.current);
        assert!(!view.availability);
        assert!(!view.health);
        assert!(!view.phone_responsive);
    }
}
