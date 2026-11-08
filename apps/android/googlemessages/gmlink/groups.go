package gmlink

import (
	"slices"
	"sort"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// Conservative local bounds, not Google protocol limits.
const (
	groupParticipantLimit = 100
	groupStringLimit      = 256
)

type groupResultCode string

const (
	groupResolved    groupResultCode = "resolved"
	groupNotFound    groupResultCode = "not_found"
	groupAmbiguous   groupResultCode = "ambiguous"
	groupStale       groupResultCode = "stale"
	groupUnsupported groupResultCode = "unsupported"
	groupRejected    groupResultCode = "rejected"
)

type groupSnapshotBinding struct {
	generation       uint64
	streamEpoch      uint64
	snapshotRevision uint64
	token            string
}

// These local observations are sensitive, never send eligibility or membership
// proofs. Rust owns phone normalization, self identity proof and proof hashes.
type groupProjection struct {
	ConversationID string
	Group          bool
	Type           int32
	Status         int32
	ReadOnly       bool
	SendMode       int32
	ParticipantIDs []string
	Binding        groupSnapshotBinding
}

type groupSnapshot struct {
	accepted bool
	binding  groupSnapshotBinding
	records  map[string]*gmproto.Conversation
}

// captureGroupSnapshot derives acceptance from Stage 2, never caller assertions.
// Clones are private to this view. Nothing here scans, emits or persists facts.
func (link *Link) captureGroupSnapshot() groupSnapshot {
	link.mu.Lock()
	defer link.mu.Unlock()
	if !link.hasOwnerVerificationLocked() || !link.conversationIndexComplete || link.conversationIndexGeneration != link.generation || link.conversationIndexStreamEpoch != link.streamEpoch || link.runGeneration != link.generation || link.pendingConversationEvents != 0 {
		return groupSnapshot{}
	}
	view := groupSnapshot{
		accepted: true,
		binding: groupSnapshotBinding{
			generation: link.generation, streamEpoch: link.streamEpoch,
			snapshotRevision: link.indexRevision,
			token:            snapshotToken(link.generation, link.streamEpoch, link.indexRevision),
		},
		records: make(map[string]*gmproto.Conversation, len(link.conversationRecords)),
	}
	for id, conversation := range link.conversationRecords {
		view.records[id] = cloneConversation(conversation)
	}
	return view
}

// lookupGroup compares opaque ParticipantID bytes, not phone identities. It
// checks captured binding consistency, not atomic freshness against a live Link.
// Snapshot revisions include name-only changes; they are not membership revisions.
func lookupGroup(view groupSnapshot, expectedToken string, requestedIDs []string) (groupProjection, groupResultCode) {
	binding := view.binding
	if !view.accepted || binding.token != expectedToken || binding.token != snapshotToken(binding.generation, binding.streamEpoch, binding.snapshotRevision) {
		return groupProjection{}, groupStale
	}
	requested, ok := groupRequestSet(requestedIDs)
	if !ok {
		return groupProjection{}, groupRejected
	}
	var match groupProjection
	matches, uncertain := 0, false
	for id, conversation := range view.records {
		if conversation == nil || id != conversation.GetConversationID() {
			uncertain = true
			continue
		}
		if !conversation.GetIsGroupChat() && knownGroupType(conversation.GetType()) && knownGroupStatus(conversation.GetStatus()) && knownGroupSendMode(conversation.GetSendMode()) {
			continue
		}
		projection, code := projectGroup(conversation)
		if code != groupResolved {
			// A malformed/unknown record can conceal another matching group.
			uncertain = true
			continue
		}
		if slices.Equal(projection.ParticipantIDs, requested) {
			match = projection
			matches++
		}
	}
	if matches > 1 {
		return groupProjection{}, groupAmbiguous
	}
	if uncertain {
		return groupProjection{}, groupUnsupported
	}
	if matches == 0 {
		return groupProjection{}, groupNotFound
	}
	match.Binding = binding
	return match, groupResolved
}

func projectGroup(c *gmproto.Conversation) (groupProjection, groupResultCode) {
	if c == nil || !boundedGroupString(c.GetConversationID()) {
		return groupProjection{}, groupRejected
	}
	if !c.GetIsGroupChat() || !knownGroupType(c.GetType()) || !knownGroupStatus(c.GetStatus()) || !knownGroupSendMode(c.GetSendMode()) {
		return groupProjection{}, groupUnsupported
	}
	ids, seen, selfCount := make([]string, 0), map[string]struct{}{}, 0
	for _, participant := range c.GetParticipants() {
		if participant == nil || !participant.GetIsVisible() {
			continue
		}
		id := participant.GetID().GetParticipantID()
		if !boundedGroupString(id) {
			return groupProjection{}, groupRejected
		}
		if _, duplicate := seen[id]; duplicate {
			return groupProjection{}, groupRejected
		}
		seen[id] = struct{}{}
		if participant.GetIsMe() {
			selfCount++
			if selfCount > 1 {
				return groupProjection{}, groupRejected
			}
			continue
		}
		ids = append(ids, id)
		if len(ids) > groupParticipantLimit {
			return groupProjection{}, groupRejected
		}
	}
	if len(ids) == 0 || selfCount != 1 {
		return groupProjection{}, groupRejected
	}
	sort.Strings(ids)
	return groupProjection{
		ConversationID: c.GetConversationID(), Group: c.GetIsGroupChat(),
		Type: int32(c.GetType()), Status: int32(c.GetStatus()),
		ReadOnly: c.GetReadOnly(), SendMode: int32(c.GetSendMode()), ParticipantIDs: ids,
	}, groupResolved
}

func groupRequestSet(ids []string) ([]string, bool) {
	if len(ids) == 0 || len(ids) > groupParticipantLimit {
		return nil, false
	}
	result := append([]string(nil), ids...)
	sort.Strings(result)
	for index, id := range result {
		if !boundedGroupString(id) || index > 0 && result[index-1] == id {
			return nil, false
		}
	}
	return result, true
}

func boundedGroupString(value string) bool {
	return value != "" && utf8.ValidString(value) && len(value) <= groupStringLimit
}

func knownGroupType(value gmproto.ConversationType) bool {
	_, ok := gmproto.ConversationType_name[int32(value)]
	return ok && value != gmproto.ConversationType_UNKNOWN_CONVERSATION_TYPE
}

func knownGroupStatus(value gmproto.ConversationStatus) bool {
	_, ok := gmproto.ConversationStatus_name[int32(value)]
	return ok && value != gmproto.ConversationStatus_UNKNOWN_CONVERSATION_STATUS
}

func knownGroupSendMode(value gmproto.ConversationSendMode) bool {
	_, ok := gmproto.ConversationSendMode_name[int32(value)]
	return ok
}
