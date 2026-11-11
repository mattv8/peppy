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
	conversationPageSize       = 25
	conversationTotalPageLimit = 60
	conversationItemLimit      = 1500
	conversationTimeout        = 30 * time.Second
	conversationPageTimeout    = 30 * time.Second
	conversationScanTimeout    = 5 * time.Minute
	conversationEventLimit     = 128
	// conversationRefreshAttempts bounds the scans one refresh spends on updates that arrive mid-scan.
	conversationRefreshAttempts   = 3
	conversationEventDrainTimeout = 5 * time.Second
	conversationEventDrainPoll    = 20 * time.Millisecond
	// conversationIndexMaxAge bounds how long a published index is reused without a rescan.
	conversationIndexMaxAge = 10 * time.Minute
	// sessionEvidenceRequestInterval bounds how often one run asks the phone to resend its settings.
	sessionEvidenceRequestInterval = 30 * time.Second
	sessionEvidenceRequestTimeout  = 10 * time.Second
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
	ParticipantID           string `json:"participant_id"`
	IdentifierType          int32  `json:"identifier_type"`
	RawNumber               string `json:"raw_number"`
	FormattedNumber         string `json:"formatted_number"`
	ParticipantIDPresent    bool   `json:"participant_id_present"`
	NumberPresent           bool   `json:"number_present"`
	Visible                 bool   `json:"visible"`
	Self                    bool   `json:"self"`
	CanonicalAddress        string `json:"canonical_address"`
	CanonicalAddressPresent bool   `json:"canonical_address_present"`
	DisplayName             string `json:"display_name"`
}

type conversationCandidateRecord struct {
	GoogleConversationID      string                             `json:"google_conversation_id"`
	ConversationType          int32                              `json:"conversation_type"`
	ConversationStatus        int32                              `json:"conversation_status"`
	Classification            string                             `json:"classification"`
	Group                     bool                               `json:"group"`
	VisibleNonSelfCount       int                                `json:"visible_non_self_count"`
	LastMessageMS             int64                              `json:"last_message_ms"`
	Participants              []conversationCandidateParticipant `json:"participants"`
	Name                      string                             `json:"name"`
	IsGroupChat               bool                               `json:"is_group_chat"`
	ReadOnly                  bool                               `json:"read_only"`
	InvisibleParticipantCount int                                `json:"invisible_participant_count"`
	MemberDigest              string                             `json:"member_digest"`
}

type conversationCandidateSnapshot struct {
	Code              string                        `json:"code"`
	SnapshotToken     string                        `json:"snapshot_token"`
	Generation        uint64                        `json:"generation"`
	StreamEpoch       uint64                        `json:"stream_epoch"`
	Revision          uint64                        `json:"revision"`
	RunIncarnation    string                        `json:"run_incarnation,omitempty"`
	PairingEvidenceID string                        `json:"pairing_evidence_id,omitempty"`
	Complete          bool                          `json:"complete"`
	Conversations     []conversationCandidateRecord `json:"conversations"`
}

type conversationLastActivityResult struct {
	Code          string           `json:"code"`
	Complete      bool             `json:"complete"`
	LastMessageMS map[string]int64 `json:"last_message_ms"`
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
	if !link.conversationIndexCurrentLocked() {
		return unavailableConversationCandidateSnapshotJSON()
	}
	return link.conversationCandidateSnapshotLocked()
}

// conversationIndexCurrentLocked reports whether the index is complete for the current owner generation and stream.
func (link *Link) conversationIndexCurrentLocked() bool {
	return link.hasIndexVerificationLocked() && link.conversationIndexComplete && link.conversationIndexGeneration == link.generation &&
		link.conversationIndexStreamEpoch == link.streamEpoch && link.runGeneration == link.generation && link.pendingConversationEvents == 0
}

// maxLastActivityIDs bounds one ConversationLastActivity request.
const maxLastActivityIDs = 500

// ConversationLastActivity reports last_message_ms for the requested Google conversation IDs
// from the current complete index. It makes no provider call. Unindexed IDs are omitted, and an
// absent provider timestamp reports 0. Conversation IDs are local-only and must not be logged.
func (link *Link) ConversationLastActivity(googleIdsJson string) string {
	var ids []string
	if json.Unmarshal([]byte(googleIdsJson), &ids) != nil || len(ids) > maxLastActivityIDs {
		return lastActivityJSON("invalid_request", false, nil)
	}
	for _, id := range ids {
		if id == "" {
			return lastActivityJSON("invalid_request", false, nil)
		}
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if !link.conversationIndexCurrentLocked() {
		return lastActivityJSON("conversation_last_activity_unavailable", false, nil)
	}
	values := make(map[string]int64, len(ids))
	for _, id := range ids {
		if conversation := link.conversationRecords[id]; conversation != nil {
			values[id] = lastMessageMillis(conversation)
		}
	}
	return lastActivityJSON("ok", true, values)
}

func lastActivityJSON(code string, complete bool, values map[string]int64) string {
	if values == nil {
		values = map[string]int64{}
	}
	encoded, _ := json.Marshal(conversationLastActivityResult{Code: code, Complete: complete, LastMessageMS: values})
	return string(encoded)
}

// lastMessageMillis converts Google's microsecond LastMessageTimestamp; an absent timestamp is 0.
func lastMessageMillis(conversation *gmproto.Conversation) int64 {
	return time.UnixMicro(conversation.GetLastMessageTimestamp()).UnixMilli()
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
		invisibleParticipantCount := 0
		for _, participant := range conversation.GetParticipants() {
			if participant == nil {
				continue
			}
			identifier := participant.GetID()
			participantID := identifier.GetParticipantID()
			rawNumber := identifier.GetNumber()
			canonicalAddress, canonicalAddressPresent := "", false
			if participant.GetIsVisible() {
				canonicalAddress, canonicalAddressPresent = canonicalGoogleNumber(rawNumber, participant.GetFormattedNumber())
			}
			if !participant.GetIsVisible() {
				invisibleParticipantCount++
			}
			participants = append(participants, conversationCandidateParticipant{
				ParticipantID:           participantID,
				IdentifierType:          int32(identifier.GetType()),
				RawNumber:               rawNumber,
				FormattedNumber:         participant.GetFormattedNumber(),
				ParticipantIDPresent:    participantID != "",
				NumberPresent:           rawNumber != "",
				Visible:                 participant.GetIsVisible(),
				Self:                    participant.GetIsMe(),
				CanonicalAddress:        canonicalAddress,
				CanonicalAddressPresent: canonicalAddressPresent,
				DisplayName:             participantDisplayName(participant),
			})
		}
		visibleNonSelfCount := len(visibleOtherParticipants(conversation))
		records = append(records, conversationCandidateRecord{
			GoogleConversationID:      id,
			ConversationType:          int32(conversation.GetType()),
			ConversationStatus:        int32(conversation.GetStatus()),
			Classification:            classifyConversation(conversation),
			Group:                     visibleNonSelfCount > 1,
			VisibleNonSelfCount:       visibleNonSelfCount,
			LastMessageMS:             lastMessageMillis(conversation),
			Participants:              participants,
			Name:                      conversation.GetName(),
			IsGroupChat:               conversation.GetIsGroupChat(),
			ReadOnly:                  conversation.GetReadOnly(),
			InvisibleParticipantCount: invisibleParticipantCount,
			MemberDigest:              groupMemberDigest(conversation),
		})
	}
	snapshot := conversationCandidateSnapshot{
		Code:              "conversation_candidate_snapshot",
		SnapshotToken:     fmt.Sprintf("%d:%d:%d", link.generation, link.streamEpoch, link.indexRevision),
		Generation:        link.generation,
		StreamEpoch:       link.streamEpoch,
		Revision:          link.indexRevision,
		RunIncarnation:    link.runIncarnationID,
		PairingEvidenceID: link.pairingEvidenceID,
		Complete:          true,
		Conversations:     records,
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
	conversations, code, _ := scanConversationPagesWithDiagnostic(ctx, load)
	return conversations, code
}

func scanConversationPagesWithDiagnostic(ctx context.Context, load conversationPageLoader) ([]*gmproto.Conversation, string, string) {
	byID := make(map[string]*gmproto.Conversation)
	var order []string
	pages := 0
	folders := []gmproto.ListConversationsRequest_Folder{
		gmproto.ListConversationsRequest_INBOX,
		gmproto.ListConversationsRequest_ARCHIVE,
		gmproto.ListConversationsRequest_SPAM_BLOCKED,
	}
	for _, folder := range folders {
		seenCursors := make(map[string]struct{})
		var cursor *gmproto.Cursor
		for {
			if pages == conversationTotalPageLimit {
				return nil, "conversation_lookup_incomplete", "page_limit"
			}
			if ctx.Err() != nil {
				return nil, "conversation_lookup_failed", "cancelled"
			}
			pages++
			pageCtx, cancel := context.WithTimeout(ctx, conversationPageTimeout)
			response, err := load(pageCtx, &gmproto.ListConversationsRequest{Count: conversationPageSize, Folder: folder, Cursor: cursor})
			pageErr := pageCtx.Err()
			cancel()
			if ctx.Err() != nil {
				return nil, "conversation_lookup_failed", "cancelled"
			}
			if pageErr != nil {
				return nil, "conversation_lookup_failed", conversationRefreshRPCDiagnostic(folder)
			}
			if err != nil || response == nil {
				return nil, "conversation_lookup_failed", conversationRefreshRPCDiagnostic(folder)
			}
			for _, conversation := range response.GetConversations() {
				cloned := cloneConversation(conversation)
				if cloned == nil || cloned.GetConversationID() == "" {
					return nil, "conversation_lookup_incomplete", "missing_id"
				}
				id := cloned.GetConversationID()
				if existing, exists := byID[id]; exists {
					if !proto.Equal(existing, cloned) {
						return nil, "conversation_lookup_incomplete", "conflicting_duplicate"
					}
					continue
				}
				if len(byID) >= conversationItemLimit {
					return nil, "conversation_lookup_incomplete", "item_limit"
				}
				order = append(order, id)
				byID[id] = cloned
			}
			next := response.GetCursor()
			if next == nil {
				if len(response.GetCursorBytes()) != 0 {
					return nil, "conversation_lookup_incomplete", "cursor_invalid"
				}
				break
			}
			encoded, err := proto.MarshalOptions{Deterministic: true}.Marshal(next)
			if err != nil || len(encoded) == 0 {
				return nil, "conversation_lookup_incomplete", "cursor_invalid"
			}
			key := string(encoded)
			if _, repeated := seenCursors[key]; repeated {
				return nil, "conversation_lookup_incomplete", "cursor_repeated"
			}
			seenCursors[key] = struct{}{}
			cursor, _ = proto.Clone(next).(*gmproto.Cursor)
			if pages == conversationTotalPageLimit {
				return nil, "conversation_lookup_incomplete", "page_limit"
			}
		}
	}
	conversations := make([]*gmproto.Conversation, 0, len(order))
	for _, id := range order {
		conversations = append(conversations, byID[id])
	}
	return conversations, "", ""
}

func conversationRefreshRPCDiagnostic(folder gmproto.ListConversationsRequest_Folder) string {
	switch folder {
	case gmproto.ListConversationsRequest_ARCHIVE:
		return "rpc_archive"
	case gmproto.ListConversationsRequest_SPAM_BLOCKED:
		return "rpc_spam"
	default:
		return "rpc_inbox"
	}
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
	if link.closed || generation != link.generation || link.runGeneration != generation || streamEpoch != link.streamEpoch || !link.hasIndexVerificationLocked() {
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
	if ctx.Err() != nil || link.closed || generation != link.generation || link.runGeneration != generation || streamEpoch != link.streamEpoch || revision != link.indexRevision || link.pendingConversationEvents != 0 || !link.hasIndexVerificationLocked() {
		return false
	}
	link.conversationCandidates = make(map[string][]conversationSummary)
	link.conversationTargets = make(map[string][]string)
	link.conversationRecords = make(map[string]*gmproto.Conversation)
	link.indexConversationsLocked(conversations)
	link.indexRevision++
	link.conversationIndexComplete = true
	link.conversationIndexScannedAt = link.nowForTicketLocked()
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
	if conversation != nil && conversation.GetIsGroupChat() && conversation.GetType() == gmproto.ConversationType_RCS && conversation.GetStatus() == gmproto.ConversationStatus_ACTIVE && link.hasGroupsVerificationLocked() {
		return cloneConversation(conversation)
	}
	if classifyConversation(conversation) != conversationClassificationEligible {
		return nil
	}
	return cloneConversation(conversation)
}

func (link *Link) eligibleIndexedGoogleConversationCountLocked(id string, generation, streamEpoch uint64) int {
	if link.currentSupportedRcsConversationLocked(id, generation, streamEpoch) == nil {
		return 0
	}
	for _, target := range link.conversationTargets[id] {
		candidates := link.conversationCandidates[target]
		isTarget := false
		for _, candidate := range candidates {
			if candidate.id == id && candidate.eligible {
				isTarget = true
				break
			}
		}
		if !isTarget {
			continue
		}
		count := 0
		for _, candidate := range candidates {
			if candidate.eligible {
				count++
			}
		}
		return count
	}
	return 0
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

// recordConversationEventLocked reports true when the correlation list overflowed and the index
// must be rescanned. The caller signals the host after releasing the lock.
func (link *Link) recordConversationEventLocked(id string) (resyncRequired bool) {
	if len(link.conversationEvents) == 1 && link.conversationEvents[0].Code == "conversation_resync" {
		return false
	}
	for index := range link.conversationEvents {
		if link.conversationEvents[index].GoogleConversationID == id {
			link.conversationEvents[index] = conversationEvent{Code: "conversation_updated", GoogleConversationID: id}
			return false
		}
	}
	if len(link.conversationEvents) >= conversationEventLimit {
		link.invalidateConversationCompletenessLocked()
		link.indexRevision++
		link.conversationEvents = []conversationEvent{{Code: "conversation_resync"}}
		return true
	}
	link.conversationEvents = append(link.conversationEvents, conversationEvent{Code: "conversation_updated", GoogleConversationID: id})
	return false
}

func (link *Link) recordConversationEventFor(generation uint64, source libgm.StreamSource, streamEpoch uint64, conversation *gmproto.Conversation) {
	link.recordQueuedConversationEventFor(generation, source, streamEpoch, conversation, false)
}

func (link *Link) recordQueuedConversationEventFor(generation uint64, source libgm.StreamSource, streamEpoch uint64, conversation *gmproto.Conversation, pending bool) {
	if conversation == nil || conversation.GetConversationID() == "" {
		return
	}
	link.mu.Lock()
	if link.closed || generation != link.generation || streamEpoch != link.streamEpoch || !link.streamSourceMatchesLocked(source) {
		link.mu.Unlock()
		return
	}
	if pending && link.pendingConversationEvents > 0 {
		link.pendingConversationEvents--
	}
	link.applyConversationsLocked([]*gmproto.Conversation{conversation})
	resyncRequired := link.hasIndexVerificationLocked() && link.recordConversationEventLocked(conversation.GetConversationID())
	link.mu.Unlock()
	if resyncRequired {
		link.emitFor(generation, "index", "stale", 0)
	}
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

// recordSettingsForTest bypasses stream provenance only for package tests.
// Bare *gmproto.Settings events are invalidation-only and never establish send readiness.
func (link *Link) recordSettingsForTest(event *libgm.WrappedSettings) {
	link.mu.Lock()
	generation, streamEpoch := link.generation, link.streamEpoch
	link.mu.Unlock()
	link.recordSettingsFor(generation, libgm.StreamSource{}, streamEpoch, time.Now(), event)
}

func (link *Link) invalidateSettingsLocked() {
	link.settings, link.settingsAt, link.settingsStreamEpoch = nil, time.Time{}, 0
}
