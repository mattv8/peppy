package gmlink

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
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

const (
	conversationClassificationEligible    = "eligible"
	conversationClassificationUnsupported = "unsupported"
	conversationClassificationUnknown     = "unknown"
)

type conversationSummary struct {
	id           string
	eligible     bool
	self         bool
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

type conversationCandidateParticipant struct {
	ParticipantID        string `json:"participant_id"`
	IdentifierType       int32  `json:"identifier_type"`
	RawNumber            string `json:"raw_number"`
	FormattedNumber      string `json:"formatted_number"`
	ParticipantIDPresent bool   `json:"participant_id_present"`
	NumberPresent        bool   `json:"number_present"`
	Visible              bool   `json:"visible"`
	Self                 bool   `json:"self"`
}

type conversationCandidateRecord struct {
	GoogleConversationID string                             `json:"google_conversation_id"`
	ConversationType     int32                              `json:"conversation_type"`
	ConversationStatus   int32                              `json:"conversation_status"`
	Classification       string                             `json:"classification"`
	Group                bool                               `json:"group"`
	VisibleNonSelfCount  int                                `json:"visible_non_self_count"`
	Participants         []conversationCandidateParticipant `json:"participants"`
}

type conversationCandidateSnapshot struct {
	Code           string                        `json:"code"`
	SnapshotToken  string                        `json:"snapshot_token"`
	Generation     uint64                        `json:"generation"`
	StreamEpoch    uint64                        `json:"stream_epoch"`
	Revision       uint64                        `json:"revision"`
	RunIncarnation string                        `json:"run_incarnation,omitempty"`
	Complete       bool                          `json:"complete"`
	Conversations  []conversationCandidateRecord `json:"conversations"`
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

func unavailableConversationCandidateSnapshotJSON() string {
	encoded, _ := json.Marshal(conversationCandidateSnapshot{
		Code:          "conversation_candidate_snapshot_unavailable",
		Conversations: make([]conversationCandidateRecord, 0),
	})
	return string(encoded)
}

// ConversationCandidateSnapshot is a sensitive, local-only bridge to core. Its
// participant facts must never be logged, persisted, included in diagnostics, or synced.
func (link *Link) ConversationCandidateSnapshot() string {
	link.mu.Lock()
	defer link.mu.Unlock()
	if !link.hasOwnerVerificationLocked() || !link.conversationIndexComplete || link.conversationIndexGeneration != link.generation || link.conversationIndexStreamEpoch != link.streamEpoch || link.runGeneration != link.generation || link.pendingConversationEvents != 0 {
		return unavailableConversationCandidateSnapshotJSON()
	}
	return link.conversationCandidateSnapshotLocked()
}

func (link *Link) conversationCandidateSnapshotLocked() string {
	ids := make([]string, 0, len(link.conversationRecords))
	for id := range link.conversationRecords {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	records := make([]conversationCandidateRecord, 0, len(ids))
	for _, id := range ids {
		conversation := link.conversationRecords[id]
		participants := make([]conversationCandidateParticipant, 0, len(conversation.GetParticipants()))
		for _, participant := range conversation.GetParticipants() {
			identifier := participant.GetID()
			participantID := identifier.GetParticipantID()
			rawNumber := identifier.GetNumber()
			participants = append(participants, conversationCandidateParticipant{
				ParticipantID:        participantID,
				IdentifierType:       int32(identifier.GetType()),
				RawNumber:            rawNumber,
				FormattedNumber:      participant.GetFormattedNumber(),
				ParticipantIDPresent: participantID != "",
				NumberPresent:        rawNumber != "",
				Visible:              participant.GetIsVisible(),
				Self:                 participant.GetIsMe(),
			})
		}
		visibleNonSelfCount := len(visibleOtherParticipants(conversation))
		records = append(records, conversationCandidateRecord{
			GoogleConversationID: id,
			ConversationType:     int32(conversation.GetType()),
			ConversationStatus:   int32(conversation.GetStatus()),
			Classification:       classifyConversation(conversation),
			Group:                visibleNonSelfCount > 1,
			VisibleNonSelfCount:  visibleNonSelfCount,
			Participants:         participants,
		})
	}
	snapshot := conversationCandidateSnapshot{
		Code:           "conversation_candidate_snapshot",
		SnapshotToken:  fmt.Sprintf("%d:%d:%d", link.generation, link.streamEpoch, link.indexRevision),
		Generation:     link.generation,
		StreamEpoch:    link.streamEpoch,
		Revision:       link.indexRevision,
		RunIncarnation: link.runIncarnationID,
		Complete:       true,
		Conversations:  records,
	}
	encoded, _ := json.Marshal(snapshot)
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
			if err != nil || response == nil || ctx.Err() != nil {
				return nil, "conversation_lookup_failed"
			}
			for _, conversation := range response.GetConversations() {
				cloned := cloneConversation(conversation)
				if cloned == nil || cloned.GetConversationID() == "" {
					return nil, "conversation_lookup_incomplete"
				}
				id := cloned.GetConversationID()
				if existing, exists := byID[id]; exists {
					if !proto.Equal(existing, cloned) {
						return nil, "conversation_lookup_incomplete"
					}
					continue
				}
				if len(byID) >= conversationItemLimit {
					return nil, "conversation_lookup_incomplete"
				}
				order = append(order, id)
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
	link.clearPreparedTicketLocked()
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
		link.conversationRecords[conversation.GetConversationID()] = conversation
		others := visibleOtherParticipants(conversation)
		for _, participant := range conversation.GetParticipants() {
			if !participant.GetIsVisible() {
				continue
			}
			target := participant.GetID().GetNumber()
			if target == "" {
				continue
			}
			self := participant.GetIsMe()
			summary := conversationSummary{
				id:           conversation.GetConversationID(),
				eligible:     !self && len(others) == 1 && conversation.GetType() == gmproto.ConversationType_RCS && conversation.GetStatus() == gmproto.ConversationStatus_ACTIVE,
				self:         self,
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
	delete(link.conversationRecords, id)
}

func (link *Link) replaceConversationIndexFor(generation, streamEpoch, revision uint64, conversations []*gmproto.Conversation) bool {
	return link.replaceConversationIndexForContext(context.Background(), generation, streamEpoch, revision, conversations)
}

func (link *Link) replaceConversationIndexForContext(ctx context.Context, generation, streamEpoch, revision uint64, conversations []*gmproto.Conversation) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	if ctx.Err() != nil || link.closed || generation != link.generation || link.runGeneration != generation || streamEpoch != link.streamEpoch || revision != link.indexRevision || link.pendingConversationEvents != 0 || !link.hasOwnerVerificationLocked() {
		return false
	}
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.conversationRecords = make(map[string]*gmproto.Conversation)
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

func conversationBlocksCompleteAbsence(conversation *gmproto.Conversation) bool {
	classification := classifyConversation(conversation)
	if classification == conversationClassificationUnknown {
		return true
	}
	if classification != conversationClassificationUnsupported {
		return false
	}
	for _, participant := range conversation.GetParticipants() {
		if participant.GetIsVisible() && participant.GetID().GetNumber() == "" {
			return true
		}
	}
	return false
}

func (link *Link) currentSupportedRcsConversationLocked(id string, generation, streamEpoch uint64) *gmproto.Conversation {
	if id == "" || !link.conversationIndexComplete || link.conversationIndexGeneration != generation || link.conversationIndexStreamEpoch != streamEpoch || link.pendingConversationEvents != 0 {
		return nil
	}
	conversation := link.conversationRecords[id]
	if classifyConversation(conversation) != conversationClassificationEligible {
		return nil
	}
	return cloneConversation(conversation)
}

func classifyConversation(conversation *gmproto.Conversation) string {
	visibleOthers := visibleOtherParticipants(conversation)
	for _, participant := range visibleOthers {
		if participant.GetID().GetNumber() == "" {
			return conversationClassificationUnknown
		}
	}
	if len(visibleOthers) != 1 {
		for _, participant := range conversation.GetParticipants() {
			if participant.GetIsVisible() && participant.GetIsMe() {
				return conversationClassificationUnsupported
			}
		}
		if len(visibleOthers) == 0 {
			return conversationClassificationUnknown
		}
		return conversationClassificationUnsupported
	}
	if conversation.GetType() != gmproto.ConversationType_RCS || conversation.GetStatus() != gmproto.ConversationStatus_ACTIVE {
		return conversationClassificationUnsupported
	}
	return conversationClassificationEligible
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
	link.clearPreparedTicketLocked()
	link.conversationIndexComplete = false
	link.conversationIndexGeneration = 0
	link.conversationIndexStreamEpoch = 0
}

func (link *Link) invalidateConversationIndexLocked() {
	link.invalidateConversationCompletenessLocked()
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.conversationRecords = make(map[string]*gmproto.Conversation)
	link.conversationEvents = []conversationEvent{{Code: "conversation_resync"}}
	link.pendingConversationEvents = 0
	link.indexRevision++
}

func (link *Link) clearConversationIndexLocked() {
	link.invalidateConversationCompletenessLocked()
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.conversationRecords = make(map[string]*gmproto.Conversation)
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
	link.settings, link.settingsAt, link.settingsStreamEpoch = event.Settings, receivedAt, streamEpoch
	link.settingsVersion++
}

func (link *Link) recordSettings(event *libgm.WrappedSettings) {
	link.mu.Lock()
	generation, streamEpoch := link.generation, link.streamEpoch
	link.mu.Unlock()
	link.recordSettingsFor(generation, libgm.StreamSource{}, streamEpoch, time.Now(), event)
}

func (link *Link) invalidateSettingsLocked() {
	link.settings, link.settingsAt, link.settingsStreamEpoch = nil, time.Time{}, 0
}
