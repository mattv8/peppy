package gmlink

import (
	"context"
	"encoding/json"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

const (
	conversationPageSize   = 25
	conversationPageLimit  = 20
	conversationItemLimit  = conversationPageSize * conversationPageLimit * 3
	conversationTimeout    = 30 * time.Second
	conversationEventLimit = 128
)

type conversationSummary struct {
	id           string
	eligible     bool
	conversation *gmproto.Conversation
}

type conversationEvent struct {
	Code                 string `json:"code"`
	GoogleConversationID string `json:"google_conversation_id"`
}

type conversationResult struct {
	Code                 string `json:"code"`
	GoogleConversationID string `json:"google_conversation_id,omitempty"`
}

type conversationPageLoader func(context.Context, *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error)

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

func scanConversationPages(ctx context.Context, load conversationPageLoader) ([]*gmproto.Conversation, string) {
	byID := make(map[string]*gmproto.Conversation)
	var order []string
	folders := []gmproto.ListConversationsRequest_Folder{
		gmproto.ListConversationsRequest_INBOX,
		gmproto.ListConversationsRequest_ARCHIVE,
		gmproto.ListConversationsRequest_SPAM_BLOCKED,
	}
	for _, folder := range folders {
		seenCursors := make(map[string]struct{})
		var cursor *gmproto.Cursor
		for page := 0; page < conversationPageLimit; page++ {
			if ctx.Err() != nil {
				return nil, "conversation_lookup_failed"
			}
			response, err := load(ctx, &gmproto.ListConversationsRequest{Count: conversationPageSize, Folder: folder, Cursor: cursor})
			if err != nil || response == nil {
				return nil, "conversation_lookup_failed"
			}
			for _, conversation := range response.GetConversations() {
				cloned := cloneConversation(conversation)
				if cloned == nil || cloned.GetConversationID() == "" {
					continue
				}
				id := cloned.GetConversationID()
				if _, exists := byID[id]; !exists {
					if len(byID) >= conversationItemLimit {
						return nil, "conversation_lookup_incomplete"
					}
					order = append(order, id)
				}
				byID[id] = cloned
			}
			next := response.GetCursor()
			if next == nil {
				break
			}
			encoded, err := proto.MarshalOptions{Deterministic: true}.Marshal(next)
			if err != nil || len(encoded) == 0 {
				return nil, "conversation_lookup_incomplete"
			}
			key := string(encoded)
			if _, repeated := seenCursors[key]; repeated {
				return nil, "conversation_lookup_incomplete"
			}
			seenCursors[key] = struct{}{}
			cursor, _ = proto.Clone(next).(*gmproto.Cursor)
			if page == conversationPageLimit-1 {
				return nil, "conversation_lookup_incomplete"
			}
		}
	}
	conversations := make([]*gmproto.Conversation, 0, len(order))
	for _, id := range order {
		conversations = append(conversations, byID[id])
	}
	return conversations, ""
}

func cloneConversation(conversation *gmproto.Conversation) *gmproto.Conversation {
	if conversation == nil {
		return nil
	}
	cloned, _ := proto.Clone(conversation).(*gmproto.Conversation)
	return cloned
}

func (link *Link) beginConversationIndexRefreshFor(generation, streamEpoch uint64) (uint64, bool) {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || link.runGeneration != generation || streamEpoch != link.streamEpoch || !link.hasOwnerVerificationLocked() {
		return 0, false
	}
	link.invalidateConversationCompletenessLocked()
	link.indexRevision++
	return link.indexRevision, true
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
	for _, source := range conversations {
		conversation := cloneConversation(source)
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
				id:           conversation.GetConversationID(),
				eligible:     len(others) == 1 && conversation.GetType() == gmproto.ConversationType_RCS && conversation.GetStatus() == gmproto.ConversationStatus_ACTIVE,
				conversation: conversation,
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
	if link.closed || generation != link.generation || link.runGeneration != generation || streamEpoch != link.streamEpoch || revision != link.indexRevision || link.pendingConversationEvents != 0 || !link.hasOwnerVerificationLocked() {
		return false
	}
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.indexConversationsLocked(conversations)
	link.indexRevision++
	link.conversationIndexComplete = true
	link.conversationIndexGeneration = generation
	link.conversationIndexStreamEpoch = streamEpoch
	link.conversationEvents = nil
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
	if len(link.conversationEvents) == 1 && link.conversationEvents[0].Code == "conversation_resync" {
		return
	}
	for index := range link.conversationEvents {
		if link.conversationEvents[index].GoogleConversationID == id {
			link.conversationEvents[index] = conversationEvent{Code: "conversation_updated", GoogleConversationID: id}
			return
		}
	}
	if len(link.conversationEvents) >= conversationEventLimit {
		link.invalidateConversationCompletenessLocked()
		link.indexRevision++
		link.conversationEvents = []conversationEvent{{Code: "conversation_resync"}}
		return
	}
	link.conversationEvents = append(link.conversationEvents, conversationEvent{Code: "conversation_updated", GoogleConversationID: id})
}

func (link *Link) recordConversationEventFor(generation uint64, source libgm.StreamSource, streamEpoch uint64, conversation *gmproto.Conversation) {
	link.recordQueuedConversationEventFor(generation, source, streamEpoch, conversation, false)
}

func (link *Link) recordQueuedConversationEventFor(generation uint64, source libgm.StreamSource, streamEpoch uint64, conversation *gmproto.Conversation, pending bool) {
	if conversation == nil || conversation.GetConversationID() == "" {
		return
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation || streamEpoch != link.streamEpoch || !link.streamSourceMatchesLocked(source) {
		return
	}
	if pending && link.pendingConversationEvents > 0 {
		link.pendingConversationEvents--
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

func (link *Link) invalidateConversationCompletenessLocked() {
	link.conversationIndexComplete = false
	link.conversationIndexGeneration = 0
	link.conversationIndexStreamEpoch = 0
}

func (link *Link) invalidateConversationIndexLocked() {
	link.invalidateConversationCompletenessLocked()
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.conversationEvents = []conversationEvent{{Code: "conversation_resync"}}
	link.pendingConversationEvents = 0
	link.indexRevision++
}

func (link *Link) clearConversationIndexLocked() {
	link.invalidateConversationCompletenessLocked()
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.conversationEvents = nil
	link.pendingConversationEvents = 0
	link.indexRevision++
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
