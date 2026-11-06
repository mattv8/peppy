package gmlink

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
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
	ConversationID string      `json:"conversation_id"`
	State          typingState `json:"state"`
	DeadlineMono   uint64      `json:"deadline_mono"`
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

var typingMonotonicOrigin = time.Now()

func (link *Link) projectTypingEventFor(queued queuedEvent, data *gmproto.TypingData) {
	link.mu.Lock()
	if link.closed || queued.generation != link.generation || queued.streamEpoch != link.streamEpoch || !link.streamSourceMatchesLocked(queued.streamSource) || !link.hasOwnerVerificationLocked() {
		link.mu.Unlock()
		return
	}
	conversations := make([]*gmproto.Conversation, 0, len(link.conversationRecords))
	for _, conversation := range link.conversationRecords {
		conversations = append(conversations, cloneConversation(conversation))
	}
	revision := link.stateRevision
	link.mu.Unlock()
	projection := projectTypingData(data, false, conversations)
	if projection.Record == nil {
		return
	}
	encoded, _ := json.Marshal(projection.Record)
	link.enqueueOutput(hostOutput{
		typingRecord:      string(encoded),
		generation:        queued.generation,
		revision:          revision,
		streamEpoch:       queued.streamEpoch,
		requireGeneration: true,
		requireRevision:   true,
		requireStream:     true,
	})
}

// SetTyping validates one core intent and performs at most one bounded libgm effect.
func (link *Link) SetTyping(intentJSON string) string {
	var intent gatewayTypingIntent
	if err := json.Unmarshal([]byte(intentJSON), &intent); err != nil {
		return "typing_invalid"
	}

	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	client, runCtx, generation, streamEpoch, ok := link.activeRuntime()
	if !ok {
		return "typing_stale"
	}
	link.mu.Lock()
	if !link.hasOwnerVerificationLocked() {
		link.mu.Unlock()
		return "typing_stale"
	}
	conversations := make([]*gmproto.Conversation, 0, len(link.conversationRecords))
	for _, conversation := range link.conversationRecords {
		conversations = append(conversations, cloneConversation(conversation))
	}
	simPayload := typingSIMPayload(intent.ConversationID, link.conversationRecords, link.settings)
	now := link.typingNowMono
	effect := link.setTyping
	link.mu.Unlock()
	if now == nil {
		now = func() uint64 { return uint64(time.Since(typingMonotonicOrigin).Milliseconds()) }
	}
	arguments, code := typingSetArguments(intent, now(), conversations, simPayload)
	if code != "ok" {
		return code
	}
	if !link.typingOwned(generation, streamEpoch) {
		return "typing_stale"
	}
	if effect == nil {
		effect = func(ctx context.Context, conversationID string, payload *gmproto.SIMPayload) error {
			return client.SetTyping(ctx, conversationID, payload)
		}
	}
	ctx, cancel := typingContext(runCtx)
	err := effect(ctx, arguments.ConversationID, arguments.SIMPayload)
	cancel()
	if err != nil {
		switch {
		case errors.Is(err, context.DeadlineExceeded):
			return "typing_deadline"
		case errors.Is(err, context.Canceled):
			return "typing_cancelled"
		default:
			return "typing_failed"
		}
	}
	if !link.typingOwned(generation, streamEpoch) {
		return "typing_stale"
	}
	return "ok"
}

func typingSIMPayload(conversationID string, conversations map[string]*gmproto.Conversation, settings *gmproto.Settings) *gmproto.SIMPayload {
	conversation := conversations[conversationID]
	if conversation == nil || settings == nil || conversation.GetDefaultOutgoingID() == "" {
		return nil
	}
	var payload *gmproto.SIMPayload
	for _, card := range settings.GetSIMCards() {
		if card.GetSIMParticipant().GetID() != conversation.GetDefaultOutgoingID() || !card.GetRCSChats().GetEnabled() || card.GetSIMData().GetSIMPayload() == nil {
			continue
		}
		if payload != nil {
			return nil
		}
		payload, _ = proto.Clone(card.GetSIMData().GetSIMPayload()).(*gmproto.SIMPayload)
	}
	return payload
}

func (link *Link) typingOwned(generation, streamEpoch uint64) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	return !link.closed && link.generation == generation && link.runGeneration == generation && link.streamEpoch == streamEpoch && link.hasOwnerVerificationLocked()
}
