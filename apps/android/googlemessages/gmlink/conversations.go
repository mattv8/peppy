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

func (link *Link) getConversation(ctx context.Context, client *libgm.Client, id string) (*gmproto.Conversation, string) {
	ctx, cancel := context.WithTimeout(ctx, conversationTimeout)
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

func (link *Link) indexConversationsFor(generation uint64, conversations []*gmproto.Conversation) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || link.runGeneration != generation {
		return false
	}
	link.indexConversationsLocked(conversations)
	return true
}

func (link *Link) indexConversations(conversations []*gmproto.Conversation) {
	link.mu.Lock()
	defer link.mu.Unlock()
	link.indexConversationsLocked(conversations)
}

func (link *Link) indexConversationsLocked(conversations []*gmproto.Conversation) {
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

func (link *Link) recordConversationEventFor(generation uint64, conversation *gmproto.Conversation) {
	if conversation == nil || conversation.GetConversationID() == "" {
		return
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation {
		return
	}
	link.indexConversationsLocked([]*gmproto.Conversation{conversation})
	if !link.hasOwnerVerificationLocked() {
		return
	}
	link.conversationEvents = append(link.conversationEvents, conversationEvent{Code: "conversation_updated", GoogleConversationID: conversation.GetConversationID()})
}

func (link *Link) recordConversationEvent(conversation *gmproto.Conversation) {
	if conversation == nil || conversation.GetConversationID() == "" {
		return
	}
	link.indexConversations([]*gmproto.Conversation{conversation})
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || !link.hasOwnerVerificationLocked() {
		return
	}
	link.conversationEvents = append(link.conversationEvents, conversationEvent{Code: "conversation_updated", GoogleConversationID: conversation.GetConversationID()})
}

func (link *Link) recordSettingsFor(generation, streamEpoch uint64, receivedAt time.Time, event *libgm.WrappedSettings) {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || streamEpoch != link.streamEpoch {
		return
	}
	if event == nil || event.IsOld || event.Settings == nil || event.Settings.GetSIMCards() == nil {
		link.invalidateSettingsLocked()
		return
	}
	link.settings, link.settingsAt = event.Settings, receivedAt
	link.settingsVersion++
}

func (link *Link) recordSettings(event *libgm.WrappedSettings) {
	link.mu.Lock()
	generation, streamEpoch := link.generation, link.streamEpoch
	link.mu.Unlock()
	link.recordSettingsFor(generation, streamEpoch, time.Now(), event)
}

func (link *Link) invalidateSettingsLocked() {
	link.settings, link.settingsAt = nil, time.Time{}
}
