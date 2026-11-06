//! Authenticated-plaintext receiver transitions for ephemeral typing v2.
//!
//! This module deliberately does not parse, decrypt, seal, or persist frames.
//! Integration supplies authenticated facts and commits a cloned candidate only
//! after its reply has been successfully sealed.

use crate::{ConversationId, DeviceId};
use std::collections::{HashMap, HashSet, VecDeque};

const CHALLENGE_TIMEOUT_MS: u64 = 10_000;
const CHALLENGE_SPACING_MS: u64 = 3_000;
const CHALLENGE_COOLDOWN_MS: u64 = 60_000;
const LEASE_MS: u64 = 15_000;
const RENEWAL_WINDOW_MS: u64 = 5_000;
const QUOTA_INTERVAL_MS: u64 = 3_000;
const QUOTA_WINDOW_MS: u64 = 60_000;
const MAX_EPISODE_CHALLENGES: u8 = 3;
const MAX_PEER_RECORDS: usize = 256;
const MAX_GLOBAL_RECORDS: usize = 4_096;
const MAX_PENDING: usize = 100;
const MAX_VALIDATED: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SessionId(pub(crate) [u8; 16]);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ChallengeNonce(pub(crate) [u8; 16]);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AuthenticatedSequence(pub(crate) u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct MonoTime(pub(crate) u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoutedTyping {
    Display,
    GatewayIntent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTypingState {
    Started,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedTyping {
    pub(crate) peer: DeviceId,
    pub(crate) session: SessionId,
    pub(crate) conversation: ConversationId,
    pub(crate) sequence: AuthenticatedSequence,
    pub(crate) state: SessionTypingState,
    pub(crate) routing: RoutedTyping,
    pub(crate) received_mono_ms: MonoTime,
    /// C3 derives this from already validated wire timing.
    pub(crate) wire_deadline_mono_ms: MonoTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedAck {
    pub(crate) peer: DeviceId,
    pub(crate) session: SessionId,
    pub(crate) challenger_session: SessionId,
    pub(crate) nonce: ChallengeNonce,
    pub(crate) sequence_floor: AuthenticatedSequence,
    pub(crate) received_mono_ms: MonoTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChallengeIntent {
    pub(crate) peer: DeviceId,
    pub(crate) issued_mono_ms: MonoTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PeerSessionUpdate {
    pub(crate) peer: DeviceId,
    pub(crate) replaced_session_token: Option<u64>,
    pub(crate) current_session_token: u64,
    pub(crate) validation_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionAction {
    pub(crate) routing: RoutedTyping,
    pub(crate) peer: DeviceId,
    pub(crate) session_token: u64,
    pub(crate) sequence: AuthenticatedSequence,
    pub(crate) conversation: ConversationId,
    pub(crate) state: SessionTypingState,
    pub(crate) deadline_mono_ms: MonoTime,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SessionTransition {
    pub(crate) session_update: Option<PeerSessionUpdate>,
    pub(crate) action: Option<SessionAction>,
    pub(crate) challenge: Option<ChallengeIntent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionError {
    Capacity,
    CounterExhausted,
}

#[derive(Clone, Debug)]
pub(crate) struct EphemeralV2SessionState {
    local_session: SessionId,
    pending: HashMap<DeviceId, PendingChallenge>,
    episodes: HashMap<DeviceId, Episode>,
    validated: HashMap<DeviceId, ValidatedSession>,
    records: HashMap<(DeviceId, ConversationId), ConversationRecord>,
    next_session_token: u64,
    next_validation_revision: u64,
}

#[derive(Clone, Copy, Debug)]
struct PendingChallenge {
    nonce: ChallengeNonce,
    issued_mono_ms: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Episode {
    attempts: u8,
    last_issue_mono_ms: Option<u64>,
    cooldown_until_mono_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct ValidatedSession {
    session: SessionId,
    floor: u64,
    lease_deadline_mono_ms: u64,
    token: u64,
    revision: u64,
}

#[derive(Clone, Debug, Default)]
struct ConversationRecord {
    session: Option<SessionId>,
    high_water: u64,
    last_emitted_start_mono_ms: Option<u64>,
    emitted_starts: VecDeque<u64>,
    active_deadline_mono_ms: Option<u64>,
}

impl EphemeralV2SessionState {
    pub(crate) fn new(local_session: SessionId) -> Self {
        Self {
            local_session,
            pending: HashMap::new(),
            episodes: HashMap::new(),
            validated: HashMap::new(),
            records: HashMap::new(),
            next_session_token: 0,
            next_validation_revision: 0,
        }
    }

    /// Plans an authenticated typing transition. A returned challenge must be recorded only
    /// after C3 seals it successfully with [`Self::record_challenge`].
    pub(crate) fn observe_typing(
        &mut self,
        input: AuthenticatedTyping,
    ) -> Result<SessionTransition, SessionError> {
        self.prune(input.received_mono_ms.0);
        let Some(validated) = self.validated.get(&input.peer).copied() else {
            return self.challenge_for(input.peer, input.received_mono_ms.0);
        };
        if validated.session != input.session
            || input.received_mono_ms.0 >= validated.lease_deadline_mono_ms
        {
            return self.challenge_for(input.peer, input.received_mono_ms.0);
        }

        let key = (input.peer, input.conversation);
        let is_new = !self.records.contains_key(&key);
        if is_new && !self.has_record_capacity(input.peer) {
            return Ok(SessionTransition::default());
        }
        let record = self.records.entry(key).or_default();
        if record.session != Some(input.session) {
            record.session = Some(input.session);
            record.high_water = 0;
            record.active_deadline_mono_ms = None;
        }
        if input.sequence.0 <= validated.floor.max(record.high_water) {
            return Ok(SessionTransition::default());
        }
        // Order is deliberately consumed before quota evaluation.
        record.high_water = input.sequence.0;
        let deadline_mono_ms = MonoTime(
            validated
                .lease_deadline_mono_ms
                .min(input.wire_deadline_mono_ms.0),
        );
        let action = if input.state == SessionTypingState::Started
            && record.admit_start(input.received_mono_ms.0)
        {
            record.active_deadline_mono_ms = Some(deadline_mono_ms.0);
            Some(SessionAction {
                routing: input.routing,
                peer: input.peer,
                session_token: validated.token,
                sequence: input.sequence,
                conversation: input.conversation,
                state: input.state,
                deadline_mono_ms,
            })
        } else if input.state == SessionTypingState::Stopped
            && record
                .active_deadline_mono_ms
                .is_some_and(|deadline| input.received_mono_ms.0 < deadline)
        {
            record.active_deadline_mono_ms = None;
            Some(SessionAction {
                routing: input.routing,
                peer: input.peer,
                session_token: validated.token,
                sequence: input.sequence,
                conversation: input.conversation,
                state: input.state,
                deadline_mono_ms,
            })
        } else {
            None
        };
        let challenge =
            if validated.lease_deadline_mono_ms - input.received_mono_ms.0 <= RENEWAL_WINDOW_MS {
                self.challenge_for(input.peer, input.received_mono_ms.0)?
                    .challenge
            } else {
                None
            };
        Ok(SessionTransition {
            session_update: None,
            action,
            challenge,
        })
    }

    /// Records a challenge only after C3 has reserved its nonce and successfully sealed it.
    pub(crate) fn record_challenge(
        &mut self,
        intent: ChallengeIntent,
        nonce: ChallengeNonce,
    ) -> Result<(), SessionError> {
        self.prune(intent.issued_mono_ms.0);
        if self.pending.contains_key(&intent.peer) || self.pending.len() >= MAX_PENDING {
            return Err(SessionError::Capacity);
        }
        let episode = self.episodes.entry(intent.peer).or_default();
        if episode
            .cooldown_until_mono_ms
            .is_some_and(|until| intent.issued_mono_ms.0 < until)
            || episode.attempts >= MAX_EPISODE_CHALLENGES
        {
            return Err(SessionError::Capacity);
        }
        episode.attempts += 1;
        episode.last_issue_mono_ms = Some(intent.issued_mono_ms.0);
        self.pending.insert(
            intent.peer,
            PendingChallenge {
                nonce,
                issued_mono_ms: intent.issued_mono_ms.0,
            },
        );
        Ok(())
    }

    pub(crate) fn accept_ack(
        &mut self,
        input: AuthenticatedAck,
    ) -> Result<SessionTransition, SessionError> {
        self.prune(input.received_mono_ms.0);
        if input.challenger_session != self.local_session {
            return Ok(SessionTransition::default());
        }
        let Some(pending) = self.pending.get(&input.peer).copied() else {
            return Ok(SessionTransition::default());
        };
        if input.nonce != pending.nonce
            || input
                .received_mono_ms
                .0
                .saturating_sub(pending.issued_mono_ms)
                >= CHALLENGE_TIMEOUT_MS
        {
            return Ok(SessionTransition::default());
        }
        let lease_deadline_mono_ms = pending
            .issued_mono_ms
            .checked_add(LEASE_MS)
            .ok_or(SessionError::CounterExhausted)?;
        let existing = self.validated.get(&input.peer).copied();
        let same_session = existing.is_some_and(|session| session.session == input.session);
        if existing.is_none() && self.validated.len() >= MAX_VALIDATED {
            return Err(SessionError::Capacity);
        }

        let transition = if same_session {
            let session = self.validated.get_mut(&input.peer).expect("checked above");
            session.floor = session.floor.max(input.sequence_floor.0);
            session.lease_deadline_mono_ms = lease_deadline_mono_ms;
            SessionTransition::default()
        } else {
            let token = self
                .next_session_token
                .checked_add(1)
                .ok_or(SessionError::CounterExhausted)?;
            let revision = self
                .next_validation_revision
                .checked_add(1)
                .ok_or(SessionError::CounterExhausted)?;
            self.next_session_token = token;
            self.next_validation_revision = revision;
            self.validated.insert(
                input.peer,
                ValidatedSession {
                    session: input.session,
                    floor: input.sequence_floor.0,
                    lease_deadline_mono_ms,
                    token,
                    revision,
                },
            );
            for ((peer, _), record) in &mut self.records {
                if *peer == input.peer {
                    record.session = None;
                    record.high_water = 0;
                    record.active_deadline_mono_ms = None;
                }
            }
            SessionTransition {
                session_update: Some(PeerSessionUpdate {
                    peer: input.peer,
                    replaced_session_token: existing.map(|session| session.token),
                    current_session_token: token,
                    validation_revision: revision,
                }),
                ..SessionTransition::default()
            }
        };
        self.pending.remove(&input.peer);
        self.episodes.remove(&input.peer);
        Ok(transition)
    }

    fn challenge_for(
        &mut self,
        peer: DeviceId,
        now_ms: u64,
    ) -> Result<SessionTransition, SessionError> {
        if let Some(pending) = self.pending.get(&peer) {
            if now_ms.saturating_sub(pending.issued_mono_ms) < CHALLENGE_TIMEOUT_MS {
                return Ok(SessionTransition::default());
            }
            self.pending.remove(&peer);
        }
        let episode = self.episodes.entry(peer).or_default();
        if episode
            .cooldown_until_mono_ms
            .is_some_and(|until| now_ms < until)
        {
            return Ok(SessionTransition::default());
        }
        if episode.cooldown_until_mono_ms.is_some() {
            *episode = Episode::default();
        }
        if episode.attempts >= MAX_EPISODE_CHALLENGES {
            episode.cooldown_until_mono_ms = Some(
                now_ms
                    .checked_add(CHALLENGE_COOLDOWN_MS)
                    .ok_or(SessionError::CounterExhausted)?,
            );
            return Ok(SessionTransition::default());
        }
        if episode
            .last_issue_mono_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < CHALLENGE_SPACING_MS)
            || self.pending.len() >= MAX_PENDING
        {
            return Ok(SessionTransition::default());
        }
        Ok(SessionTransition {
            challenge: Some(ChallengeIntent {
                peer,
                issued_mono_ms: MonoTime(now_ms),
            }),
            ..SessionTransition::default()
        })
    }

    fn has_record_capacity(&self, peer: DeviceId) -> bool {
        self.records.len() < MAX_GLOBAL_RECORDS
            && self
                .records
                .keys()
                .filter(|(record_peer, _)| *record_peer == peer)
                .count()
                < MAX_PEER_RECORDS
    }

    fn prune(&mut self, now_ms: u64) {
        self.pending.retain(|_, pending| {
            now_ms.saturating_sub(pending.issued_mono_ms) < CHALLENGE_TIMEOUT_MS
        });
        let pending_peers: HashSet<_> = self.pending.keys().copied().collect();
        self.validated.retain(|peer, session| {
            now_ms < session.lease_deadline_mono_ms || pending_peers.contains(peer)
        });
        let validated = &self.validated;
        self.records.retain(|(peer, _), record| {
            record
                .emitted_starts
                .retain(|start| now_ms.saturating_sub(*start) < QUOTA_WINDOW_MS);
            if record
                .active_deadline_mono_ms
                .is_some_and(|deadline| now_ms >= deadline)
            {
                record.active_deadline_mono_ms = None;
            }
            let order_is_live = record.session.is_some_and(|session| {
                validated
                    .get(peer)
                    .is_some_and(|validated| validated.session == session)
            });
            order_is_live
                || !record.emitted_starts.is_empty()
                || record.active_deadline_mono_ms.is_some()
        });
    }
}

impl ConversationRecord {
    fn admit_start(&mut self, now_ms: u64) -> bool {
        self.emitted_starts
            .retain(|start| now_ms.saturating_sub(*start) < QUOTA_WINDOW_MS);
        if self
            .last_emitted_start_mono_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < QUOTA_INTERVAL_MS)
            || self.emitted_starts.len() >= 20
        {
            return false;
        }
        self.last_emitted_start_mono_ms = Some(now_ms);
        self.emitted_starts.push_back(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn device(value: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(value))
    }
    fn conversation(value: u128) -> ConversationId {
        ConversationId(Uuid::from_u128(value))
    }
    fn session(value: u8) -> SessionId {
        SessionId([value; 16])
    }
    fn nonce(value: u8) -> ChallengeNonce {
        ChallengeNonce([value; 16])
    }
    fn typing(
        peer: DeviceId,
        session: SessionId,
        conversation: ConversationId,
        sequence: u64,
        state: SessionTypingState,
        now: u64,
    ) -> AuthenticatedTyping {
        AuthenticatedTyping {
            peer,
            session,
            conversation,
            sequence: AuthenticatedSequence(sequence),
            state,
            routing: RoutedTyping::Display,
            received_mono_ms: MonoTime(now),
            wire_deadline_mono_ms: MonoTime(now + LEASE_MS),
        }
    }
    fn validate(
        state: &mut EphemeralV2SessionState,
        peer: DeviceId,
        peer_session: SessionId,
        at: u64,
    ) -> SessionTransition {
        let intent = state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                1,
                SessionTypingState::Started,
                at,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(intent, nonce(1)).unwrap();
        state
            .accept_ack(AuthenticatedAck {
                peer,
                session: peer_session,
                challenger_session: session(9),
                nonce: nonce(1),
                sequence_floor: AuthenticatedSequence(100),
                received_mono_ms: MonoTime(at + 1),
            })
            .unwrap()
    }

    #[test]
    fn delayed_ack_keeps_high_water_and_rejects_replay() {
        let peer = device(1);
        let peer_session = session(2);
        let mut state = EphemeralV2SessionState::new(session(9));
        let initial = validate(&mut state, peer, peer_session, 0)
            .session_update
            .unwrap();
        assert_eq!(
            (
                initial.replaced_session_token,
                initial.current_session_token,
                initial.validation_revision
            ),
            (None, 1, 1)
        );
        let renewal = state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                102,
                SessionTypingState::Started,
                10_000,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(renewal, nonce(2)).unwrap();
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                103,
                SessionTypingState::Stopped,
                10_001
            ))
            .unwrap()
            .action
            .is_some());
        state
            .accept_ack(AuthenticatedAck {
                peer,
                session: peer_session,
                challenger_session: session(9),
                nonce: nonce(2),
                sequence_floor: AuthenticatedSequence(100),
                received_mono_ms: MonoTime(10_002),
            })
            .unwrap();
        assert_eq!(
            state
                .observe_typing(typing(
                    peer,
                    peer_session,
                    conversation(1),
                    102,
                    SessionTypingState::Started,
                    10_003
                ))
                .unwrap(),
            SessionTransition::default()
        );
    }

    #[test]
    fn pending_renewal_blocks_expired_lease_without_new_nonce() {
        let peer = device(1);
        let peer_session = session(2);
        let mut state = EphemeralV2SessionState::new(session(9));
        validate(&mut state, peer, peer_session, 0);
        let renewal = state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                101,
                SessionTypingState::Started,
                10_000,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(renewal, nonce(2)).unwrap();
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                102,
                SessionTypingState::Started,
                LEASE_MS
            ))
            .unwrap()
            .challenge
            .is_none());
    }

    #[test]
    fn flood_cannot_replenish_attempts_or_extend_cooldown() {
        let peer = device(1);
        let mut state = EphemeralV2SessionState::new(session(9));
        for at in [0, 10_000, 20_000] {
            let intent = state
                .observe_typing(typing(
                    peer,
                    session(2),
                    conversation(1),
                    1,
                    SessionTypingState::Started,
                    at,
                ))
                .unwrap()
                .challenge
                .unwrap();
            state
                .record_challenge(intent, nonce((at / 10_000 + 1) as u8))
                .unwrap();
        }
        assert!(state
            .observe_typing(typing(
                peer,
                session(2),
                conversation(1),
                1,
                SessionTypingState::Started,
                30_000
            ))
            .unwrap()
            .challenge
            .is_none());
        assert!(state
            .observe_typing(typing(
                peer,
                session(2),
                conversation(1),
                1,
                SessionTypingState::Started,
                89_999
            ))
            .unwrap()
            .challenge
            .is_none());
        assert!(state
            .observe_typing(typing(
                peer,
                session(2),
                conversation(1),
                1,
                SessionTypingState::Started,
                90_000
            ))
            .unwrap()
            .challenge
            .is_some());
    }

    #[test]
    fn installs_and_replaces_with_checked_peer_update() {
        let peer = device(1);
        let mut state = EphemeralV2SessionState::new(session(9));
        validate(&mut state, peer, session(2), 0);
        let intent = state
            .observe_typing(typing(
                peer,
                session(3),
                conversation(1),
                1,
                SessionTypingState::Started,
                1_000,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(intent, nonce(2)).unwrap();
        let update = state
            .accept_ack(AuthenticatedAck {
                peer,
                session: session(3),
                challenger_session: session(9),
                nonce: nonce(2),
                sequence_floor: AuthenticatedSequence(1),
                received_mono_ms: MonoTime(1_001),
            })
            .unwrap()
            .session_update
            .unwrap();
        assert_eq!(update.replaced_session_token, Some(1));
        assert_eq!(update.current_session_token, 2);
        assert_eq!(update.validation_revision, 2);
    }

    #[test]
    fn wrong_ack_tuple_has_no_authority() {
        let peer = device(1);
        let mut state = EphemeralV2SessionState::new(session(9));
        let intent = state
            .observe_typing(typing(
                peer,
                session(2),
                conversation(1),
                1,
                SessionTypingState::Started,
                0,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(intent, nonce(1)).unwrap();
        for ack in [
            AuthenticatedAck {
                peer: device(2),
                session: session(2),
                challenger_session: session(9),
                nonce: nonce(1),
                sequence_floor: AuthenticatedSequence(1),
                received_mono_ms: MonoTime(1),
            },
            AuthenticatedAck {
                peer,
                session: session(2),
                challenger_session: session(9),
                nonce: nonce(2),
                sequence_floor: AuthenticatedSequence(1),
                received_mono_ms: MonoTime(1),
            },
            AuthenticatedAck {
                peer,
                session: session(2),
                challenger_session: session(8),
                nonce: nonce(1),
                sequence_floor: AuthenticatedSequence(1),
                received_mono_ms: MonoTime(1),
            },
        ] {
            assert_eq!(state.accept_ack(ack).unwrap(), SessionTransition::default());
        }
        assert!(state
            .accept_ack(AuthenticatedAck {
                peer,
                session: session(3),
                challenger_session: session(9),
                nonce: nonce(1),
                sequence_floor: AuthenticatedSequence(1),
                received_mono_ms: MonoTime(1)
            })
            .unwrap()
            .session_update
            .is_some());
    }

    #[test]
    fn conversations_have_independent_three_second_quotas_and_suppression_consumes_order() {
        let peer = device(1);
        let peer_session = session(2);
        let mut state = EphemeralV2SessionState::new(session(9));
        validate(&mut state, peer, peer_session, 0);
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                101,
                SessionTypingState::Started,
                1
            ))
            .unwrap()
            .action
            .is_some());
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(2),
                101,
                SessionTypingState::Started,
                1
            ))
            .unwrap()
            .action
            .is_some());
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                102,
                SessionTypingState::Started,
                2
            ))
            .unwrap()
            .action
            .is_none());
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                102,
                SessionTypingState::Started,
                3_001
            ))
            .unwrap()
            .action
            .is_none());
    }

    #[test]
    fn stale_outcome_keeps_host_fence_metadata() {
        let peer = device(1);
        let peer_session = session(2);
        let mut state = EphemeralV2SessionState::new(session(9));
        validate(&mut state, peer, peer_session, 0);
        let action = state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                101,
                SessionTypingState::Started,
                1,
            ))
            .unwrap()
            .action
            .unwrap();
        assert_eq!(
            (action.peer, action.session_token, action.sequence),
            (peer, 1, AuthenticatedSequence(101))
        );
    }

    #[test]
    fn cap_refusal_preserves_existing_state() {
        let peer = device(1);
        let peer_session = session(2);
        let mut state = EphemeralV2SessionState::new(session(9));
        validate(&mut state, peer, peer_session, 0);
        for id in 0..MAX_PEER_RECORDS as u128 {
            state
                .observe_typing(typing(
                    peer,
                    peer_session,
                    conversation(id),
                    101,
                    SessionTypingState::Started,
                    1,
                ))
                .unwrap();
        }
        let records_before = state.records.len();
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(999),
                101,
                SessionTypingState::Started,
                1
            ))
            .unwrap()
            .action
            .is_none());
        assert_eq!(state.records.len(), records_before);
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(0),
                102,
                SessionTypingState::Started,
                3_001
            ))
            .unwrap()
            .action
            .is_some());
    }

    #[test]
    fn stops_require_a_live_emitted_start_but_consume_order() {
        let peer = device(1);
        let peer_session = session(2);
        let mut state = EphemeralV2SessionState::new(session(9));
        validate(&mut state, peer, peer_session, 0);
        for (sequence, state_kind, now, expected) in [
            (101, SessionTypingState::Stopped, 1, false),
            (102, SessionTypingState::Started, 2, true),
            (103, SessionTypingState::Stopped, 3, true),
            (104, SessionTypingState::Stopped, 4, false),
        ] {
            assert_eq!(
                state
                    .observe_typing(typing(
                        peer,
                        peer_session,
                        conversation(1),
                        sequence,
                        state_kind,
                        now
                    ))
                    .unwrap()
                    .action
                    .is_some(),
                expected
            );
        }
    }

    #[test]
    fn expired_records_restore_peer_and_global_capacity() {
        let mut state = EphemeralV2SessionState::new(session(9));
        for peer_id in 1..=16 {
            let peer = device(peer_id);
            let peer_session = session(peer_id as u8);
            validate(&mut state, peer, peer_session, 0);
            let count = if peer_id == 16 {
                MAX_PEER_RECORDS - 1
            } else {
                MAX_PEER_RECORDS
            };
            for conversation_id in 0..count as u128 {
                state
                    .observe_typing(typing(
                        peer,
                        peer_session,
                        conversation(conversation_id),
                        101,
                        SessionTypingState::Started,
                        1,
                    ))
                    .unwrap();
            }
        }
        let peer_sixteen = device(16);
        assert!(state
            .observe_typing(typing(
                peer_sixteen,
                session(16),
                conversation(999),
                101,
                SessionTypingState::Started,
                1
            ))
            .unwrap()
            .action
            .is_some());
        let records_before = state.records.len();
        let peer_seventeen = device(17);
        validate(&mut state, peer_seventeen, session(17), 0);
        assert!(state
            .observe_typing(typing(
                peer_seventeen,
                session(17),
                conversation(0),
                101,
                SessionTypingState::Started,
                1
            ))
            .unwrap()
            .action
            .is_none());
        assert_eq!(state.records.len(), records_before);
        let peer = device(1);
        let intent = state
            .observe_typing(typing(
                peer,
                session(1),
                conversation(999),
                101,
                SessionTypingState::Started,
                QUOTA_WINDOW_MS + 1,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(intent, nonce(2)).unwrap();
        state
            .accept_ack(AuthenticatedAck {
                peer,
                session: session(1),
                challenger_session: session(9),
                nonce: nonce(2),
                sequence_floor: AuthenticatedSequence(100),
                received_mono_ms: MonoTime(QUOTA_WINDOW_MS + 2),
            })
            .unwrap();
        assert!(state
            .observe_typing(typing(
                peer,
                session(1),
                conversation(999),
                101,
                SessionTypingState::Started,
                QUOTA_WINDOW_MS + 3
            ))
            .unwrap()
            .action
            .is_some());
    }

    #[test]
    fn expired_lease_with_pending_renewal_preserves_same_session_authority() {
        let peer = device(1);
        let peer_session = session(2);
        let mut state = EphemeralV2SessionState::new(session(9));
        let challenge = state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                1,
                SessionTypingState::Started,
                0,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(challenge, nonce(1)).unwrap();
        state
            .accept_ack(AuthenticatedAck {
                peer,
                session: peer_session,
                challenger_session: session(9),
                nonce: nonce(1),
                sequence_floor: AuthenticatedSequence(5),
                received_mono_ms: MonoTime(1),
            })
            .unwrap();
        let renewal = state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                99,
                SessionTypingState::Started,
                10_000,
            ))
            .unwrap()
            .challenge
            .unwrap();
        state.record_challenge(renewal, nonce(2)).unwrap();
        state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                102,
                SessionTypingState::Started,
                10_001,
            ))
            .unwrap();
        state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                103,
                SessionTypingState::Stopped,
                10_002,
            ))
            .unwrap();
        let renewal_ack = state
            .accept_ack(AuthenticatedAck {
                peer,
                session: peer_session,
                challenger_session: session(9),
                nonce: nonce(2),
                sequence_floor: AuthenticatedSequence(100),
                received_mono_ms: MonoTime(16_000),
            })
            .unwrap();
        assert_eq!(renewal_ack.session_update, None);
        let validated = state.validated[&peer];
        assert_eq!(
            (validated.token, validated.revision, validated.floor),
            (1, 1, 100)
        );
        assert_eq!(state.records[&(peer, conversation(1))].high_water, 103);
        assert!(state
            .observe_typing(typing(
                peer,
                peer_session,
                conversation(1),
                102,
                SessionTypingState::Started,
                16_001
            ))
            .unwrap()
            .action
            .is_none());
    }
}
