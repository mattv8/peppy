use crate::ConversationId;
use std::collections::{HashMap, VecDeque};

const MAX_WIRE_TTL_MS: i64 = 15_000;
const CLOCK_MONO_HEADROOM_MS: u64 = 60_000;
const WINDOW_MS: u64 = 60_000;
const RESERVATION_MS: u64 = 15_000;
const START_INTERVAL_MS: u64 = 3_000;
const BURST_WINDOW_MS: u64 = 1_000;
const MAX_OCCUPANCY: usize = 50;
const MAX_CONVERSATIONS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveClock {
    pub wall_ms: i64,
    pub mono_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PolicyError {
    Clock,
    InvalidContext,
    Capacity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClockState {
    last_mono_ms: u64,
    wall_effective_ms: i64,
}

impl ClockState {
    pub(crate) fn new(clock: LiveClock) -> Result<Self, PolicyError> {
        if !(0..=i64::MAX - MAX_WIRE_TTL_MS).contains(&clock.wall_ms)
            || clock.mono_ms > u64::MAX - CLOCK_MONO_HEADROOM_MS
        {
            return Err(PolicyError::InvalidContext);
        }
        Ok(Self {
            last_mono_ms: clock.mono_ms,
            wall_effective_ms: clock.wall_ms,
        })
    }

    pub(crate) fn advance(&mut self, clock: LiveClock) -> Result<i64, PolicyError> {
        if clock.wall_ms < 0 || clock.mono_ms < self.last_mono_ms {
            return Err(PolicyError::Clock);
        }
        let elapsed_ms = clock.mono_ms - self.last_mono_ms;
        let elapsed_ms = i64::try_from(elapsed_ms).map_err(|_| PolicyError::Clock)?;
        let advanced_wall_ms = self
            .wall_effective_ms
            .checked_add(elapsed_ms)
            .ok_or(PolicyError::Clock)?;
        let effective_wall_ms = advanced_wall_ms.max(clock.wall_ms);

        self.last_mono_ms = clock.mono_ms;
        self.wall_effective_ms = effective_wall_ms;
        Ok(effective_wall_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConversationBudget {
    active: bool,
    last_start_ms: Option<u64>,
    starts: VecDeque<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SenderBudget {
    sent: VecDeque<u64>,
    burst_starts: VecDeque<u64>,
    reservations: HashMap<ConversationId, u64>,
    conversations: HashMap<ConversationId, ConversationBudget>,
}

impl SenderBudget {
    pub(crate) fn new() -> Self {
        Self {
            sent: VecDeque::new(),
            burst_starts: VecDeque::new(),
            reservations: HashMap::new(),
            conversations: HashMap::new(),
        }
    }

    pub(crate) fn admit_start(
        &mut self,
        conversation: ConversationId,
        mono_ms: u64,
    ) -> Result<bool, PolicyError> {
        let mut candidate = self.clone();
        let admitted = candidate.admit_start_inner(conversation, mono_ms)?;
        *self = candidate;
        Ok(admitted)
    }

    pub(crate) fn admit_stop(
        &mut self,
        conversation: ConversationId,
        mono_ms: u64,
    ) -> Result<bool, PolicyError> {
        let mut candidate = self.clone();
        let admitted = candidate.admit_stop_inner(conversation, mono_ms)?;
        *self = candidate;
        Ok(admitted)
    }

    fn admit_start_inner(
        &mut self,
        conversation: ConversationId,
        mono_ms: u64,
    ) -> Result<bool, PolicyError> {
        let reservation_until = mono_ms
            .checked_add(RESERVATION_MS)
            .ok_or(PolicyError::InvalidContext)?;
        self.prune(mono_ms);

        if self
            .conversations
            .get(&conversation)
            .is_some_and(|entry| entry.active)
        {
            if self.occupancy() > MAX_OCCUPANCY - 1 || self.burst_starts.len() >= 5 {
                return Ok(false);
            }
            self.sent.push_back(mono_ms);
            self.burst_starts.push_back(mono_ms);
            self.reservations.insert(conversation, reservation_until);
            return Ok(true);
        }

        if !self.conversations.contains_key(&conversation)
            && self.conversations.len() >= MAX_CONVERSATIONS
        {
            return Err(PolicyError::Capacity);
        }
        if self.occupancy() > MAX_OCCUPANCY - 2 || self.burst_starts.len() >= 5 {
            return Ok(false);
        }
        if let Some(entry) = self.conversations.get(&conversation)
            && (entry.last_start_ms.is_some_and(|last| mono_ms - last < START_INTERVAL_MS)
                || entry.starts.len() >= 20)
        {
            return Ok(false);
        }

        let entry = self
            .conversations
            .entry(conversation)
            .or_insert_with(|| ConversationBudget {
                active: false,
                last_start_ms: None,
                starts: VecDeque::new(),
            });
        entry.active = true;
        entry.last_start_ms = Some(mono_ms);
        entry.starts.push_back(mono_ms);
        self.sent.push_back(mono_ms);
        self.burst_starts.push_back(mono_ms);
        self.reservations.insert(conversation, reservation_until);
        Ok(true)
    }

    fn admit_stop_inner(
        &mut self,
        conversation: ConversationId,
        mono_ms: u64,
    ) -> Result<bool, PolicyError> {
        self.prune(mono_ms);
        let Some(entry) = self.conversations.get_mut(&conversation) else {
            return Ok(false);
        };
        if !entry.active || self.reservations.remove(&conversation).is_none() {
            return Ok(false);
        }
        entry.active = false;
        self.sent.push_back(mono_ms);
        Ok(true)
    }

    fn prune(&mut self, mono_ms: u64) {
        Self::prune_window(&mut self.sent, mono_ms, WINDOW_MS);
        Self::prune_window(&mut self.burst_starts, mono_ms, BURST_WINDOW_MS);
        self.reservations.retain(|_, expires_at| *expires_at > mono_ms);
        for entry in self.conversations.values_mut() {
            Self::prune_window(&mut entry.starts, mono_ms, WINDOW_MS);
        }
        self.conversations
            .retain(|_, entry| entry.active || !entry.starts.is_empty());
    }

    fn prune_window(window: &mut VecDeque<u64>, mono_ms: u64, duration_ms: u64) {
        while window
            .front()
            .is_some_and(|timestamp| mono_ms.saturating_sub(*timestamp) >= duration_ms)
        {
            window.pop_front();
        }
    }

    fn occupancy(&self) -> usize {
        self.sent.len() + self.reservations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn conversation(number: u128) -> ConversationId {
        ConversationId::from_str(&format!("00000000-0000-0000-0000-{number:012x}")).unwrap()
    }

    #[test]
    fn clock_bootstrap_and_advance_boundaries_are_checked() {
        assert_eq!(
            ClockState::new(LiveClock { wall_ms: -1, mono_ms: 0 }),
            Err(PolicyError::InvalidContext)
        );
        assert_eq!(
            ClockState::new(LiveClock {
                wall_ms: i64::MAX - MAX_WIRE_TTL_MS + 1,
                mono_ms: 0,
            }),
            Err(PolicyError::InvalidContext)
        );
        assert!(ClockState::new(LiveClock {
            wall_ms: i64::MAX - MAX_WIRE_TTL_MS,
            mono_ms: u64::MAX - CLOCK_MONO_HEADROOM_MS,
        })
        .is_ok());

        let mut clock = ClockState::new(LiveClock {
            wall_ms: 10,
            mono_ms: 20,
        })
        .unwrap();
        assert_eq!(clock.advance(LiveClock { wall_ms: 5, mono_ms: 25 }), Ok(15));
        assert_eq!(clock.advance(LiveClock { wall_ms: 100, mono_ms: 26 }), Ok(100));
    }

    #[test]
    fn invalid_clock_has_zero_mutation_including_overflow() {
        let mut clock = ClockState::new(LiveClock {
            wall_ms: i64::MAX - MAX_WIRE_TTL_MS,
            mono_ms: 0,
        })
        .unwrap();
        let before = clock.clone();
        assert_eq!(
            clock.advance(LiveClock {
                wall_ms: 0,
                mono_ms: MAX_WIRE_TTL_MS as u64 + 1,
            }),
            Err(PolicyError::Clock)
        );
        assert_eq!(clock, before);
        assert_eq!(clock.advance(LiveClock { wall_ms: -1, mono_ms: 1 }), Err(PolicyError::Clock));
        assert_eq!(clock, before);
        assert_eq!(clock.advance(LiveClock { wall_ms: 0, mono_ms: u64::MAX }), Err(PolicyError::Clock));
        assert_eq!(clock, before);
    }

    #[test]
    fn starts_saturate_occupancy_without_blocking_active_stops() {
        let mut budget = SenderBudget::new();
        for number in 0..25 {
            assert_eq!(budget.admit_start(conversation(number), number as u64 * 200), Ok(true));
        }
        assert_eq!(budget.occupancy(), 50);
        assert_eq!(budget.admit_start(conversation(25), 5_000), Ok(false));
        for number in 0..25 {
            assert_eq!(budget.admit_stop(conversation(number), 5_000), Ok(true));
        }
        assert_eq!(budget.occupancy(), 50);
    }

    #[test]
    fn start_quota_releases_at_the_exact_sixty_second_boundary() {
        let mut budget = SenderBudget::new();
        let conversation = conversation(1);
        for number in 0..20 {
            let now = number * 3_000;
            assert_eq!(budget.admit_start(conversation, now), Ok(true));
            assert_eq!(budget.admit_stop(conversation, now), Ok(true));
        }
        assert_eq!(budget.admit_start(conversation, 59_000), Ok(false));
        assert_eq!(budget.admit_start(conversation, 60_000), Ok(true));
    }

    #[test]
    fn expired_reservations_release_and_late_stops_coalesce() {
        let mut budget = SenderBudget::new();
        let conversation = conversation(1);
        assert_eq!(budget.admit_start(conversation, 0), Ok(true));
        assert_eq!(budget.occupancy(), 2);
        assert_eq!(budget.admit_stop(conversation, RESERVATION_MS), Ok(false));
        assert_eq!(budget.occupancy(), 1);
    }

    #[test]
    fn conversation_cap_refuses_growth_without_live_eviction() {
        let mut budget = SenderBudget::new();
        for number in 0..MAX_CONVERSATIONS {
            budget.conversations.insert(
                conversation(number as u128),
                ConversationBudget {
                    active: true,
                    last_start_ms: None,
                    starts: VecDeque::new(),
                },
            );
        }
        let before = budget.clone();
        assert_eq!(budget.admit_start(conversation(MAX_CONVERSATIONS as u128), 0), Err(PolicyError::Capacity));
        assert_eq!(budget, before);
    }

    #[test]
    fn candidate_copy_rolls_back_failed_admission() {
        let mut budget = SenderBudget::new();
        assert_eq!(budget.admit_start(conversation(1), 0), Ok(true));
        let before = budget.clone();
        let mut candidate = budget.clone();
        assert_eq!(candidate.admit_start(conversation(2), u64::MAX), Err(PolicyError::InvalidContext));
        assert_eq!(candidate, before);
        assert_eq!(budget, before);
    }
}
