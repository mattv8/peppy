package gmlink

import (
	"context"
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

const bootstrapRcsObservationDeadline = 5 * time.Second

type nativeRcsBootstrapRequest struct {
	GoogleConversationID  string                  `json:"google_conversation_id"`
	LinkGeneration        int64                   `json:"link_generation"`
	SessionGeneration     int64                   `json:"session_generation"`
	ContextRevision       int64                   `json:"context_revision"`
	MappingRevision       int64                   `json:"mapping_revision"`
	IdentityRegion        string                  `json:"identity_region"`
	CanonicalizerRevision int32                   `json:"canonicalizer_revision"`
	CarrierBinding        nativeRcsCarrierBinding `json:"carrier_binding"`
}

type nativeRcsBootstrapResult struct {
	Code        string `json:"code"`
	Observation string `json:"observation,omitempty"`
}

// BootstrapRcsObservation obtains one fresh, candidate-fenced mapping observation.
// Participant data is local-only and must never be logged, persisted, or synced.
func (link *Link) BootstrapRcsObservation(requestJSON string) string {
	var request nativeRcsBootstrapRequest
	if len(requestJSON) > maxNativeRcsPrepareJSONBytes || json.Unmarshal([]byte(requestJSON), &request) != nil ||
		request.GoogleConversationID == "" || request.LinkGeneration <= 0 || request.SessionGeneration <= 0 ||
		request.ContextRevision <= 0 || request.MappingRevision <= 0 ||
		request.CarrierBinding.CarrierRouteID == "" || request.CarrierBinding.OwnNumberRaw == "" {
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "invalid_request"})
	}

	link.mu.Lock()
	if !link.hasIndexVerificationLocked() || !link.conversationIndexComplete ||
		link.conversationIndexGeneration != link.generation || link.conversationIndexStreamEpoch != link.streamEpoch ||
		link.runGeneration != link.generation || link.pendingConversationEvents != 0 ||
		link.currentSupportedRcsConversationLocked(request.GoogleConversationID, link.generation, link.streamEpoch) == nil {
		link.mu.Unlock()
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}
	selected := uniqueEnabledRcsCard(link.settings)
	if selected == nil || link.client == nil || link.runCtx == nil || !link.running || link.state != stateLinked {
		link.mu.Unlock()
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}
	candidateCount := uint32(link.eligibleIndexedGoogleConversationCountLocked(request.GoogleConversationID, link.generation, link.streamEpoch))
	if candidateCount != 1 {
		link.mu.Unlock()
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}
	client, runCtx, generation, streamEpoch := link.client, link.runCtx, link.generation, link.streamEpoch
	settings := proto.Clone(link.settings).(*gmproto.Settings)
	selected = proto.Clone(selected).(*gmproto.SIMCard)
	settingsVersion, settingsEpoch, settingsAt := link.settingsVersion, link.settingsStreamEpoch, link.settingsAt
	runIncarnation, pairingEvidence := link.runIncarnationID, link.pairingEvidenceID
	indexRevision := link.indexRevision
	link.mu.Unlock()

	ctx, cancel := context.WithTimeout(runCtx, bootstrapRcsObservationDeadline)
	defer cancel()
	if !link.lockRPC(ctx) {
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}
	defer link.rpcMu.Unlock()

	conversation, code := link.getConversation(ctx, client, request.GoogleConversationID)
	if code != "" || conversation == nil || classifyConversation(conversation) != conversationClassificationEligible ||
		conversation.GetReadOnly() || conversation.GetSendMode() != gmproto.ConversationSendMode_SEND_MODE_AUTO ||
		conversation.GetDefaultOutgoingID() != selected.GetSIMParticipant().GetID() {
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}

	link.mu.Lock()
	current := !link.closed && link.running && link.state == stateLinked && link.generation == generation &&
		link.streamEpoch == streamEpoch && link.runGeneration == generation && link.settingsVersion == settingsVersion &&
		link.settingsStreamEpoch == settingsEpoch && link.runIncarnationID == runIncarnation && link.pairingEvidenceID == pairingEvidence &&
		link.indexRevision == indexRevision && link.currentSupportedRcsConversationLocked(request.GoogleConversationID, generation, streamEpoch) != nil &&
		uint32(link.eligibleIndexedGoogleConversationCountLocked(request.GoogleConversationID, generation, streamEpoch)) == candidateCount
	link.mu.Unlock()
	if !current {
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}

	outgoing, ok := nativeOutgoingFromCard(selected)
	if !ok {
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}
	sims := make([]nativeRcsOutgoing, 0, len(settings.GetSIMCards()))
	for _, card := range settings.GetSIMCards() {
		sim, valid := nativeOutgoingFromCard(card)
		if !valid {
			return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
		}
		sims = append(sims, sim)
	}
	mappingObservedAtMS := time.Now().UnixMilli()
	observation := nativeRcsObservation{
		GoogleConversationID: request.GoogleConversationID, GoogleCandidateCount: candidateCount, ConversationType: "Rcs",
		Status: "eligible", SendMode: "Automatic", DefaultOutgoing: true, SelectedSIMID: request.CarrierBinding.CarrierRouteID,
		SelectedSIMOwnIdentity: outgoing.OwnIdentityRaw, CarrierBinding: request.CarrierBinding, Outgoing: outgoing,
		NativeEvidence: nativeRcsEvidence{Lifetime: nativeRcsLifetime{RuntimeID: runIncarnation, RunGeneration: generation, LinkGeneration: uint64(request.LinkGeneration), SessionGeneration: uint64(request.SessionGeneration), ContextRevision: uint64(request.ContextRevision)}, LookupID: uuid.NewString(), LookupComplete: true, GoogleCandidateCount: candidateCount, SettingsRevision: settingsVersion, SettingsProvenance: "CurrentEvent", SettingsObservedAtMS: settingsAt.UnixMilli(), SettingsStreamEpoch: settingsEpoch, SettingsCurrentness: "ContinuousVerifiedFeed", CurrentSettingsSIMs: sims, SettingsValidatedAtMS: time.Now().UnixMilli(), ConversationObservedAtMS: time.Now().UnixMilli()},
		IdentityRegion: request.IdentityRegion, ObservedAtMS: mappingObservedAtMS, LinkGeneration: uint64(request.LinkGeneration), SessionGeneration: uint64(request.SessionGeneration), ContextRevision: uint64(request.ContextRevision), CanonicalizerRevision: uint32(request.CanonicalizerRevision), MappingRevision: uint64(request.MappingRevision), LinkState: "Linked",
	}
	for _, participant := range conversation.GetParticipants() {
		identifier := participant.GetID()
		observation.Participants = append(observation.Participants, nativeRcsParticipant{Identity: identifier.GetNumber(), GoogleParticipantID: identifier.GetParticipantID(), IdentifierType: int32(identifier.GetType()), IsVisible: participant.GetIsVisible(), IsMe: participant.GetIsMe()})
	}
	encoded, _, valid := encodeNativeObservation("ok", observation)
	if !valid {
		return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "rejected"})
	}
	return bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "ok", Observation: observationFromNativeJSON(encoded)})
}

func bootstrapRcsJSON(result nativeRcsBootstrapResult) string {
	encoded, _ := json.Marshal(result)
	return string(encoded)
}

func observationFromNativeJSON(encoded string) string {
	var result nativeRcsObservationResult
	if json.Unmarshal([]byte(encoded), &result) != nil {
		return ""
	}
	return result.Observation
}
