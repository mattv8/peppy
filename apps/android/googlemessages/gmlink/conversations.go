package gmlink

import (
	"context"
	"encoding/json"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	conversationPageSize  = 25
	conversationPageLimit = 20
	conversationTimeout   = 30 * time.Second
	settingsMaxAge        = 5 * time.Minute
)

type conversationSummary struct{ id string }

type conversationEvent struct {
	Code                 string `json:"code"`
	GoogleConversationID string `json:"google_conversation_id"`
}

type conversationResult struct {
	Code                 string `json:"code"`
	GoogleConversationID string `json:"google_conversation_id,omitempty"`
}

func conversationJSON(code string) string {
	encoded, _ := json.Marshal(conversationResult{Code: code})
	return string(encoded)
}

func resolvedConversationJSON(id string) string {
	encoded, _ := json.Marshal(conversationResult{Code: "conversation_found", GoogleConversationID: id})
	return string(encoded)
}

func (link *Link) getConversation(client *libgm.Client, id string) (*gmproto.Conversation, string) {
	ctx, cancel := context.WithTimeout(context.Background(), conversationTimeout)
	conversation, err := client.GetConversation(ctx, id)
	cancel()
	if err != nil {
		return nil, "conversation_lookup_failed"
	}
	if conversation == nil || conversation.GetConversationID() == "" {
		return nil, "conversation_missing"
	}
	if conversation.GetConversationID() != id {
		return nil, "conversation_id_mismatch"
	}
	return conversation, ""
}

func (link *Link) indexConversations(conversations []*gmproto.Conversation) {
	link.mu.Lock()
	defer link.mu.Unlock()
	for _, conversation := range conversations {
		if conversation == nil || conversation.GetConversationID() == "" {
			continue
		}
		for _, participant := range visibleOtherParticipants(conversation) {
			target := participant.GetID().GetNumber()
			if target != "" {
				link.conversationIndex[target] = conversationSummary{id: conversation.GetConversationID()}
			}
		}
	}
}

func visibleOtherParticipants(conversation *gmproto.Conversation) []*gmproto.Participant {
	var participants []*gmproto.Participant
	for _, participant := range conversation.GetParticipants() {
		if participant.GetIsVisible() && !participant.GetIsMe() && participant.GetID().GetNumber() != "" {
			participants = append(participants, participant)
		}
	}
	return participants
}

func (link *Link) recordConversationEvent(conversation *gmproto.Conversation) {
	if conversation == nil || conversation.GetConversationID() == "" {
		return
	}
	link.indexConversations([]*gmproto.Conversation{conversation})
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || !link.m2VerificationPassed {
		return
	}
	link.conversationEvents = append(link.conversationEvents, conversationEvent{Code: "conversation_updated", GoogleConversationID: conversation.GetConversationID()})
}

func (link *Link) recordSettings(settings *gmproto.Settings) {
	if settings == nil || settings.GetSIMCards() == nil {
		return
	}
	link.mu.Lock()
	link.settings, link.settingsAt = settings, time.Now()
	link.mu.Unlock()
}
