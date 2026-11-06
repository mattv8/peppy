package gmlink

import (
	"sort"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	m4GroupParticipantLimit = 100
	m4GroupStringLimit      = 256
)

// m4GroupProjection contains Android-local facts only. It is not a gomobile or
// Host event shape and must not be logged.
type m4GroupProjection struct {
	ConversationID string
	Name           string
	Type           int32
	Status         int32
	ReadOnly       bool
	SendMode       int32
	ParticipantIDs []string
}

func projectM4Group(conversation *gmproto.Conversation) (m4GroupProjection, string) {
	if conversation == nil || !conversation.GetIsGroupChat() || !m4BoundedString(conversation.GetConversationID()) || !m4BoundedOptionalString(conversation.GetName()) ||
		!m4KnownConversationType(conversation.GetType()) || !m4KnownConversationStatus(conversation.GetStatus()) || !m4KnownSendMode(conversation.GetSendMode()) {
		return m4GroupProjection{}, "group_projection_unverified"
	}

	participants := make([]string, 0, len(conversation.GetParticipants()))
	seen := make(map[string]struct{})
	for _, participant := range conversation.GetParticipants() {
		if participant == nil || !participant.GetIsVisible() || participant.GetIsMe() {
			continue
		}
		participantID := participant.GetID().GetParticipantID()
		if !m4BoundedString(participantID) {
			return m4GroupProjection{}, "group_projection_unverified"
		}
		if _, duplicate := seen[participantID]; duplicate {
			return m4GroupProjection{}, "group_projection_unverified"
		}
		seen[participantID] = struct{}{}
		participants = append(participants, participantID)
		if len(participants) > m4GroupParticipantLimit {
			return m4GroupProjection{}, "group_projection_unverified"
		}
	}
	if len(participants) == 0 {
		return m4GroupProjection{}, "group_projection_unverified"
	}
	sort.Strings(participants)
	return m4GroupProjection{
		ConversationID: conversation.GetConversationID(),
		Name:           conversation.GetName(),
		Type:           int32(conversation.GetType()),
		Status:         int32(conversation.GetStatus()),
		ReadOnly:       conversation.GetReadOnly(),
		SendMode:       int32(conversation.GetSendMode()),
		ParticipantIDs: participants,
	}, "verified"
}

func m4BoundedString(value string) bool {
	return value != "" && utf8.ValidString(value) && len(value) <= m4GroupStringLimit
}

func m4BoundedOptionalString(value string) bool {
	return utf8.ValidString(value) && len(value) <= m4GroupStringLimit
}

func m4KnownConversationType(value gmproto.ConversationType) bool {
	_, known := gmproto.ConversationType_name[int32(value)]
	return known
}

func m4KnownConversationStatus(value gmproto.ConversationStatus) bool {
	_, known := gmproto.ConversationStatus_name[int32(value)]
	return known
}

func m4KnownSendMode(value gmproto.ConversationSendMode) bool {
	_, known := gmproto.ConversationSendMode_name[int32(value)]
	return known
}

// m4GroupRequestFixtures pin the two request shapes without calling libgm.
func m4GroupRequestFixtures() (resolve, create *gmproto.GetOrCreateConversationRequest) {
	resolve = &gmproto.GetOrCreateConversationRequest{Numbers: []*gmproto.ContactNumber{{Number: "opaque-member-a"}, {Number: "opaque-member-b"}}}
	name, createGroup := "group-label", true
	create = &gmproto.GetOrCreateConversationRequest{
		Numbers:        []*gmproto.ContactNumber{{Number: "opaque-member-a"}, {Number: "opaque-member-b"}},
		RCSGroupName:   &name,
		CreateRCSGroup: &createGroup,
	}
	return resolve, create
}

// m4ConversationEventFixture records that the existing drain event identifies
// only a conversation. A group change must trigger a targeted refetch.
func m4ConversationEventFixture() conversationEvent {
	return conversationEvent{Code: "conversation_updated", GoogleConversationID: "opaque-conversation"}
}
