//! Bounded rolling-window accounting for ephemeral typing v2.
//!
//! Runtime integration owns candidate commit timing: control accounting is cloned
//! before sealing, while ingress accounting is intentionally retained pre-auth.

#![allow(dead_code)] // Removed when the B3 runtime begins using these private helpers.

use crate::DeviceId;
use std::collections::{HashMap, VecDeque};

const SECOND_MS: u64 = 1_000;
const MINUTE_MS: u64 = 60_000;
const MAX_PEER_RECORDS: usize = 100;

#[derive(Clone, Debug, Default)]
struct RollingWindow {
    accepted_at: VecDeque<u64>,
}

impl RollingWindow {
    fn prune(&mut self, now_ms: u64, window_ms: u64) {
        while self.accepted_at.front().is_some_and(|accepted_ms| {
            now_ms
                .checked_sub(*accepted_ms)
                .is_some_and(|elapsed_ms| elapsed_ms >= window_ms)
        }) {
            self.accepted_at.pop_front();
        }
    }

    fn has_capacity(&self, limit: usize) -> bool {
        self.accepted_at.len() < limit
    }

    fn accept(&mut self, now_ms: u64) {
        self.accepted_at.push_back(now_ms);
    }

    fn is_empty(&self) -> bool {
        self.accepted_at.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
struct Windows {
    minute: RollingWindow,
    second: RollingWindow,
}

impl Windows {
    fn prune(&mut self, now_ms: u64) {
        self.minute.prune(now_ms, MINUTE_MS);
        self.second.prune(now_ms, SECOND_MS);
    }

    fn admits(&self, minute_limit: usize, second_limit: usize) -> bool {
        self.minute.has_capacity(minute_limit) && self.second.has_capacity(second_limit)
    }

    fn accept(&mut self, now_ms: u64) {
        self.minute.accept(now_ms);
        self.second.accept(now_ms);
    }

    fn is_empty(&self) -> bool {
        self.minute.is_empty() && self.second.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ControlBudget {
    challenges: Windows,
    acks: Windows,
    ack_peers: HashMap<DeviceId, Windows>,
}

impl ControlBudget {
    pub(crate) fn admit_challenge(&mut self, mono_ms: u64) -> bool {
        self.challenges.prune(mono_ms);
        if !self.challenges.admits(20, 5) {
            return false;
        }
        self.challenges.accept(mono_ms);
        true
    }

    pub(crate) fn admit_ack(&mut self, peer: DeviceId, mono_ms: u64) -> bool {
        self.acks.prune(mono_ms);
        self.prune_ack_peers(mono_ms);
        if !self.acks.admits(40, 6) {
            return false;
        }
        let peer_budget = self.ack_peers.get(&peer);
        if peer_budget.is_some_and(|budget| !budget.admits(10, 2))
            || peer_budget.is_none() && self.ack_peers.len() == MAX_PEER_RECORDS
        {
            return false;
        }
        self.acks.accept(mono_ms);
        self.ack_peers.entry(peer).or_default().accept(mono_ms);
        true
    }

    fn prune_ack_peers(&mut self, mono_ms: u64) {
        self.ack_peers.retain(|_, budget| {
            budget.prune(mono_ms);
            !budget.is_empty()
        });
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct IngressBudget {
    ingress: Windows,
    peers: HashMap<DeviceId, Windows>,
}

impl IngressBudget {
    pub(crate) fn admit(&mut self, peer: DeviceId, mono_ms: u64) -> bool {
        self.ingress.prune(mono_ms);
        self.prune_peers(mono_ms);
        if !self.ingress.admits(6_000, 100) {
            return false;
        }
        let peer_budget = self.peers.get(&peer);
        if peer_budget.is_some_and(|budget| !budget.admits(120, 4))
            || peer_budget.is_none() && self.peers.len() == MAX_PEER_RECORDS
        {
            return false;
        }
        self.ingress.accept(mono_ms);
        self.peers.entry(peer).or_default().accept(mono_ms);
        true
    }

    fn prune_peers(&mut self, mono_ms: u64) {
        self.peers.retain(|_, budget| {
            budget.prune(mono_ms);
            !budget.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn peer(value: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(value))
    }

    #[test]
    fn challenges_enforce_half_open_second_and_minute_limits() {
        let mut budget = ControlBudget::default();
        for _ in 0..5 {
            assert!(budget.admit_challenge(0));
        }
        assert!(!budget.admit_challenge(999));
        assert!(budget.admit_challenge(1_000));
        for index in 0..14 {
            let mono_ms = ((index / 5) as u64 + 2) * SECOND_MS;
            assert!(budget.admit_challenge(mono_ms));
        }
        assert_eq!(budget.challenges.minute.accepted_at.len(), 20);
        assert!(!budget.admit_challenge(4_001));
        assert!(budget.admit_challenge(60_000));
    }

    #[test]
    fn acks_have_independent_peer_limits_and_a_shared_global_limit() {
        let mut budget = ControlBudget::default();
        let first = peer(1);
        let second = peer(2);
        for _ in 0..2 {
            assert!(budget.admit_ack(first, 0));
        }
        assert!(!budget.admit_ack(first, 0));
        for _ in 0..2 {
            assert!(budget.admit_ack(second, 0));
        }
        assert_eq!(budget.acks.second.accepted_at.len(), 4);
        assert!(budget.admit_ack(first, 1_000));
        for (index, id) in (3..=37).enumerate() {
            let second = ((index + 1) / 6 + 1) as u64;
            assert!(budget.admit_ack(peer(id), second * SECOND_MS));
        }
        assert_eq!(budget.acks.minute.accepted_at.len(), 40);
        assert!(!budget.admit_ack(peer(39), 7_000));
        assert!(budget.admit_ack(peer(39), 60_000));
    }

    #[test]
    fn ack_peer_minute_limit_expires_exactly_at_the_boundary() {
        let mut budget = ControlBudget::default();
        let challenger = peer(1);
        for second in 0..5 {
            for _ in 0..2 {
                assert!(budget.admit_ack(challenger, second * SECOND_MS));
            }
        }
        assert!(!budget.admit_ack(challenger, 4_001));
        assert!(budget.admit_ack(challenger, MINUTE_MS));
    }

    #[test]
    fn rejected_ack_does_not_consume_global_capacity() {
        let mut budget = ControlBudget::default();
        let limited_peer = peer(1);
        for _ in 0..2 {
            assert!(budget.admit_ack(limited_peer, 0));
        }
        assert!(!budget.admit_ack(limited_peer, 0));
        assert_eq!(budget.acks.second.accepted_at.len(), 2);
        for id in 2..=6 {
            assert!(budget.admit_ack(peer(id), 0));
        }
        assert_eq!(budget.acks.second.accepted_at.len(), 6);
        assert!(!budget.admit_ack(peer(7), 0));
    }

    #[test]
    fn ingress_global_limit_is_not_masked_by_peer_limits() {
        let mut budget = IngressBudget::default();
        for second in 0..60 {
            for id in 0..100 {
                assert!(budget.admit(peer(id), second * SECOND_MS));
            }
        }
        assert_eq!(budget.ingress.minute.accepted_at.len(), 6_000);
        assert!(!budget.admit(peer(1), 59_999));
    }

    #[test]
    fn ingress_peer_limits_expire_at_the_exact_boundaries() {
        let mut budget = IngressBudget::default();
        let sender = peer(1);
        for second in 0..30 {
            for _ in 0..4 {
                assert!(budget.admit(sender, second * SECOND_MS));
            }
        }
        assert_eq!(budget.peers[&sender].minute.accepted_at.len(), 120);
        assert!(!budget.admit(sender, 29_001));
        assert!(budget.admit(sender, MINUTE_MS));
    }

    #[test]
    fn ingress_refuses_new_peers_while_full_and_recovers_after_expiry() {
        let mut budget = IngressBudget::default();
        for id in 0..MAX_PEER_RECORDS as u128 {
            assert!(budget.admit(peer(id), 0));
        }
        assert!(!budget.admit(peer(101), 0));
        assert_eq!(budget.peers.len(), MAX_PEER_RECORDS);
        assert!(budget.admit(peer(101), MINUTE_MS));
        assert_eq!(budget.peers.len(), 1);
    }

    #[test]
    fn cloned_control_candidate_does_not_mutate_original_until_committed() {
        let mut original = ControlBudget::default();
        for _ in 0..5 {
            assert!(original.admit_challenge(0));
        }
        let mut candidate = original.clone();
        assert!(candidate.admit_challenge(SECOND_MS));
        assert_eq!(original.challenges.minute.accepted_at.len(), 5);
        assert_eq!(candidate.challenges.minute.accepted_at.len(), 6);
        original = candidate;
        assert_eq!(original.challenges.minute.accepted_at.len(), 6);
    }
}
