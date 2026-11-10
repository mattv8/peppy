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
	ConversationID      string      `json:"conversation_id"`
	State               typingState `json:"state"`
	RemainingValidityMS uint64      `json:"remaining_validity_ms"`
	LinkGeneration      uint64      `json:"link_generation"`
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
func typingSetArguments(intent gatewayTypingIntent, conversations []*gmproto.Conversation, simPayload *gmproto.SIMPayload) (*typingSetRequest, string) {
	if intent.RemainingValidityMS == 0 || intent.RemainingValidityMS > uint64(typingRequestTimeout/time.Millisecond) {
		return nil, "typing_invalid"
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

func typingContext(parent context.Context, remaining time.Duration) (context.Context, context.CancelFunc) {
	if remaining > typingRequestTimeout {
		remaining = typingRequestTimeout
	}
	return context.WithTimeout(parent, remaining)
}

func (link *Link) projectTypingEventFor(queued queuedEvent, data *gmproto.TypingData) {
	link.mu.Lock()
	if link.closed || queued.generation != link.generation || queued.streamEpoch != link.streamEpoch || !link.streamSourceMatchesLocked(queued.streamSource) || !link.hasReceiveVerificationLocked() {
		link.mu.Unlock()
		return
	}
	conversation := cloneConversation(link.conversationRecords[data.GetConversationID()])
	revision := link.stateRevision
	link.mu.Unlock()
	projection := projectTypingData(data, false, []*gmproto.Conversation{conversation})
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
	link.mu.Lock()
	now := link.typingNow
	link.mu.Unlock()
	if now == nil {
		now = time.Now
	}
	receivedAt := now()

	var intent gatewayTypingIntent
	if err := json.Unmarshal([]byte(intentJSON), &intent); err != nil {
		return "typing_invalid"
	}
	if intent.RemainingValidityMS == 0 || intent.RemainingValidityMS > uint64(typingRequestTimeout/time.Millisecond) {
		return "typing_invalid"
	}

	link.mu.Lock()
	client, runCtx, generation, streamEpoch := link.client, link.runCtx, link.runGeneration, link.streamEpoch
	ok := !link.closed && link.state == stateLinked && link.running && client != nil && runCtx != nil && generation == link.generation
	link.mu.Unlock()
	if !ok || intent.LinkGeneration != generation {
		return "typing_stale"
	}
	validity := time.Duration(intent.RemainingValidityMS) * time.Millisecond
	remaining := validity - now().Sub(receivedAt)
	if remaining <= 0 || remaining > validity {
		return "typing_expired"
	}
	ctx, cancel := typingContext(runCtx, remaining)
	defer cancel()
	if !link.lockRPC(ctx) {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return "typing_deadline"
		}
		return "typing_cancelled"
	}
	defer link.rpcMu.Unlock()
	link.mu.Lock()
	ok = !link.closed && link.state == stateLinked && link.running && link.client == client && link.runGeneration == generation && link.generation == generation && link.streamEpoch == streamEpoch
	if !ok || !link.hasTypingOutboundVerificationLocked() {
		link.mu.Unlock()
		return "typing_stale"
	}
	conversation := link.currentSupportedRcsConversationLocked(intent.ConversationID, generation, streamEpoch)
	if conversation == nil {
		link.mu.Unlock()
		return "typing_refused"
	}
	simPayload := typingSIMPayload(conversation, link.settings)
	effect := link.setTyping
	link.mu.Unlock()
	arguments, code := typingSetArguments(intent, []*gmproto.Conversation{conversation}, simPayload)
	if code != "ok" {
		return code
	}
	if !link.typingAdmissionCurrent(generation, streamEpoch, intent.ConversationID) {
		return "typing_stale"
	}
	if effect == nil {
		effect = func(ctx context.Context, conversationID string, payload *gmproto.SIMPayload) error {
			return client.SetTyping(ctx, conversationID, payload)
		}
	}
	err := effect(ctx, arguments.ConversationID, arguments.SIMPayload)
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
	if !link.typingAdmissionCurrent(generation, streamEpoch, intent.ConversationID) {
		return "typing_stale"
	}
	return "ok"
}

func typingSIMPayload(conversation *gmproto.Conversation, settings *gmproto.Settings) *gmproto.SIMPayload {
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

func (link *Link) typingAdmissionCurrent(generation, streamEpoch uint64, conversationID string) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	return !link.closed && link.generation == generation && link.runGeneration == generation && link.streamEpoch == streamEpoch && link.hasTypingOutboundVerificationLocked() &&
		link.currentSupportedRcsConversationLocked(conversationID, generation, streamEpoch) != nil
}
