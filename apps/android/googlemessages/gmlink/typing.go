package gmlink

import (
	"context"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const typingRequestTimeout = 5 * time.Second

type typingState string

const (
	typingStateStarted typingState = "started"
	typingStateStopped typingState = "stopped"
)

type typingRecord struct {
	GoogleConversationID string `json:"google_conversation_id"`
	State                string `json:"state"`
}

type typingProjection struct {
	Code   string
	Record *typingRecord
}

type gatewayTypingIntent struct {
	ConversationID string
	State          typingState
	DeadlineMono   uint64
}

type typingSetRequest struct {
	ConversationID string
	SIMPayload     *gmproto.SIMPayload
}

// projectTypingData reduces a pinned-libgm typing event to a fixed-shape record.
func projectTypingData(data *gmproto.TypingData, old bool, conversations []*gmproto.Conversation) typingProjection {
	if old || data == nil || data.GetConversationID() == "" || data.GetUser().GetNumber() == "" {
		return typingProjection{Code: "typing_ignored"}
	}
	state, ok := typingStateFor(data.GetType())
	if !ok || !typingParticipantMatches(data, conversations) {
		return typingProjection{Code: "typing_ignored"}
	}
	return typingProjection{Code: "ok", Record: &typingRecord{
		GoogleConversationID: data.GetConversationID(),
		State:                string(state),
	}}
}

func typingStateFor(value gmproto.TypingTypes) (typingState, bool) {
	switch value {
	case gmproto.TypingTypes_STARTED_TYPING:
		return typingStateStarted, true
	case gmproto.TypingTypes_STOPPED_TYPING:
		return typingStateStopped, true
	default:
		return "", false
	}
}

func typingParticipantMatches(data *gmproto.TypingData, conversations []*gmproto.Conversation) bool {
	for _, conversation := range conversations {
		if conversation == nil || conversation.GetConversationID() != data.GetConversationID() {
			continue
		}
		participants := visibleOtherParticipants(conversation)
		return len(participants) == 1 && participants[0].GetID().GetNumber() == data.GetUser().GetNumber()
	}
	return false
}

// typingSetArguments admits only a current start intent for a known one-to-one conversation.
func typingSetArguments(intent gatewayTypingIntent, nowMono uint64, conversations []*gmproto.Conversation, simPayload *gmproto.SIMPayload) (*typingSetRequest, string) {
	if intent.DeadlineMono <= nowMono {
		return nil, "typing_expired"
	}
	conversation, code := typingConversationFor(intent.ConversationID, conversations)
	if code != "" {
		return nil, code
	}
	if intent.State == typingStateStopped {
		return nil, "typing_stopped"
	}
	if intent.State != typingStateStarted || simPayload == nil {
		return nil, "typing_refused"
	}
	return &typingSetRequest{ConversationID: conversation.GetConversationID(), SIMPayload: simPayload}, "ok"
}

func typingConversationFor(id string, conversations []*gmproto.Conversation) (*gmproto.Conversation, string) {
	if id == "" {
		return nil, "typing_unknown_conversation"
	}
	for _, conversation := range conversations {
		if conversation != nil && conversation.GetConversationID() == id {
			if len(visibleOtherParticipants(conversation)) != 1 {
				return nil, "typing_not_one_to_one"
			}
			return conversation, ""
		}
	}
	return nil, "typing_unknown_conversation"
}

func typingContext(parent context.Context) (context.Context, context.CancelFunc) {
	return context.WithTimeout(parent, typingRequestTimeout)
}
