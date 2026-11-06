package gmlink

import (
	"context"
	"encoding/json"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	conversationPageSize   = 25
	conversationPageLimit  = 20
	conversationTimeout    = 30 * time.Second
	conversationEventLimit = 128
)

type conversationSummary struct {
	id       string
	eligible bool
}

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

func (link *Link) indexConversationsFor(generation, streamEpoch uint64, conversations []*gmproto.Conversation) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || link.runGeneration != generation || streamEpoch != link.streamEpoch {
		return false
	}
	link.applyConversationsLocked(conversations)
	return true
}

func (link *Link) indexConversations(conversations []*gmproto.Conversation) {
	link.mu.Lock()
	defer link.mu.Unlock()
	link.applyConversationsLocked(conversations)
}

func (link *Link) applyConversationsLocked(conversations []*gmproto.Conversation) {
	link.indexConversationsLocked(conversations)
	link.indexRevision++
}

func (link *Link) indexConversationsLocked(conversations []*gmproto.Conversation) {
	for _, conversation := range conversations {
		if conversation == nil || conversation.GetConversationID() == "" {
			continue
		}
		link.removeConversationLocked(conversation.GetConversationID())
		others := visibleOtherParticipants(conversation)
		for _, participant := range others {
			target := participant.GetID().GetNumber()
			if target == "" {
				continue
			}
			summary := conversationSummary{
				id:       conversation.GetConversationID(),
				eligible: len(others) == 1 && conversation.GetType() == gmproto.ConversationType_RCS && conversation.GetStatus() == gmproto.ConversationStatus_ACTIVE,
			}
			link.conversationCandidates[target] = append(link.conversationCandidates[target], summary)
			link.conversationTargets[summary.id] = append(link.conversationTargets[summary.id], target)
		}
	}
}

func (link *Link) removeConversationLocked(id string) {
	for _, target := range link.conversationTargets[id] {
		summaries := link.conversationCandidates[target]
		for index := len(summaries) - 1; index >= 0; index-- {
			if summaries[index].id == id {
				summaries = append(summaries[:index], summaries[index+1:]...)
			}
		}
		if len(summaries) == 0 {
			delete(link.conversationCandidates, target)
		} else {
			link.conversationCandidates[target] = summaries
		}
	}
	delete(link.conversationTargets, id)
}

func (link *Link) replaceConversationIndexFor(generation, streamEpoch, revision uint64, conversations []*gmproto.Conversation) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || link.runGeneration != generation || streamEpoch != link.streamEpoch || revision != link.indexRevision {
		return false
	}
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.applyConversationsLocked(conversations)
	return true
}

func visibleOtherParticipants(conversation *gmproto.Conversation) []*gmproto.Participant {
	var participants []*gmproto.Participant
	for _, participant := range conversation.GetParticipants() {
		if participant.GetIsVisible() && !participant.GetIsMe() {
			participants = append(participants, participant)
		}
	}
	return participants
}

func (link *Link) recordConversationEventLocked(id string) {
	for index := range link.conversationEvents {
		if link.conversationEvents[index].GoogleConversationID == id {
			link.conversationEvents[index] = conversationEvent{Code: "conversation_updated", GoogleConversationID: id}
			return
		}
	}
	if len(link.conversationEvents) >= conversationEventLimit {
		link.conversationEvents = []conversationEvent{{Code: "conversation_resync"}}
		return
	}
	link.conversationEvents = append(link.conversationEvents, conversationEvent{Code: "conversation_updated", GoogleConversationID: id})
}

func (link *Link) recordConversationEventFor(generation uint64, source libgm.StreamSource, streamEpoch uint64, conversation *gmproto.Conversation) {
	if conversation == nil || conversation.GetConversationID() == "" {
		return
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || streamEpoch != link.streamEpoch || !link.streamSourceMatchesLocked(source) {
		return
	}
	link.applyConversationsLocked([]*gmproto.Conversation{conversation})
	if !link.hasOwnerVerificationLocked() {
		return
	}
	link.recordConversationEventLocked(conversation.GetConversationID())
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
	link.recordConversationEventLocked(conversation.GetConversationID())
}

func (link *Link) recordSettingsFor(generation uint64, source libgm.StreamSource, streamEpoch uint64, receivedAt time.Time, event *libgm.WrappedSettings) {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || streamEpoch != link.streamEpoch || !link.streamSourceMatchesLocked(source) {
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
	link.recordSettingsFor(generation, libgm.StreamSource{}, streamEpoch, time.Now(), event)
}

func (link *Link) invalidateSettingsLocked() {
	link.settings, link.settingsAt = nil, time.Time{}
}
