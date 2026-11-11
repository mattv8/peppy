package gmlink

import (
	"encoding/json"
	"time"
	"unicode/utf8"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

const (
	maxNativeRcsPrepareJSONBytes = 65536
	maxNativeCandidateBytes      = 8 * 1024 * 1024
	maxNativePrepareResultBytes  = 6*maxNativeCandidateBytes + 4096
)

// NativeRcsTargetClassifier synchronously publishes one core classification.
// Classify must not reenter Go or perform gateway acquisition, network, scheduling, or cleanup.
type NativeRcsTargetClassifier interface {
	Classify(candidateSnapshot string) bool
}

type nativeRcsPrepareResult struct {
	Code              string `json:"code"`
	TicketID          string `json:"ticket_id,omitempty"`
	CandidateSnapshot string `json:"candidate_snapshot,omitempty"`
	RunIncarnation    string `json:"run_incarnation,omitempty"`
	RunGeneration     uint64 `json:"run_generation,omitempty"`
	StreamEpoch       uint64 `json:"stream_epoch,omitempty"`
	PairingEvidenceID string `json:"pairing_evidence_id,omitempty"`
}

func nativeRcsJSON(result nativeRcsPrepareResult) string {
	encoded, _ := json.Marshal(result)
	return string(encoded)
}

// WithNativeRcsTargetClassification runs the core publication while the prepared ticket remains current.
func (link *Link) WithNativeRcsTargetClassification(ticketID string, classifier NativeRcsTargetClassifier) (accepted bool) {
	if classifier == nil {
		return false
	}

	link.mu.Lock()
	defer link.mu.Unlock()
	defer func() {
		if recover() != nil {
			accepted = false
		}
	}()

	ticket := link.nativeTicket
	if !link.nativeTicketCurrentLocked(ticket, ticketID) || ticket.routeObservation == nil || ticket.targetObservation != nil ||
		link.sendAdmissionInFlight || link.nativeInFlight != nil {
		return false
	}
	return classifier.Classify(string(ticket.candidate))
}

// PrepareNativeRcs freezes the local facts that later native-send phases must recheck.
// It sends no message. A refusal for missing settings may ask the phone to resend them.
func (link *Link) PrepareNativeRcs(requestJSON string) string {
	if len(requestJSON) > maxNativeRcsPrepareJSONBytes || !utf8.ValidString(requestJSON) {
		return link.nativePrepareResult("invalid_request", "invalid_request")
	}
	request, ok := decodeNativeRcsPrepare(requestJSON)
	if !ok || !validNativeRcsPrepare(request) {
		return link.nativePrepareResult("invalid_request", "invalid_request")
	}

	link.mu.Lock()
	now := link.nowForTicketLocked()
	if link.preparedTicket != nil && !now.Before(link.preparedTicket.expiresAt) {
		link.clearPreparedTicketLocked()
	}
	link.expireNativeTicketLocked(now)
	if link.nativeTicket != nil || link.nativeInFlight != nil || link.preparedTicket != nil || link.sendAdmissionInFlight {
		link.mu.Unlock()
		return link.nativePrepareResult("busy", "busy")
	}
	if reason := link.nativePrepareFailureReasonLocked(); reason != "prepared" {
		var requestSessionEvidence func()
		if reason == "settings_missing" || reason == "settings_stale" {
			requestSessionEvidence = link.sessionEvidenceRequestLocked()
		}
		link.mu.Unlock()
		if requestSessionEvidence != nil {
			go requestSessionEvidence()
		}
		return link.nativePrepareResult("rejected", reason)
	}
	selected := uniqueEnabledRcsCard(link.settings)
	if selected == nil {
		link.mu.Unlock()
		return link.nativePrepareResult("rejected", "sim_unavailable")
	}
	candidate := []byte(link.conversationCandidateSnapshotLocked())
	if len(candidate) > maxNativeCandidateBytes {
		link.mu.Unlock()
		return link.nativePrepareResult("rejected", "candidate_too_large")
	}
	ticket := &nativeRcsTicket{
		id:              uuid.NewString(),
		expiresAt:       now.Add(preparedRcsTicketTTL),
		generation:      link.generation,
		streamEpoch:     link.streamEpoch,
		indexRevision:   link.indexRevision,
		settingsVersion: link.settingsVersion,
		settingsEpoch:   link.settingsStreamEpoch,
		runIncarnation:  link.runIncarnationID,
		pairingEvidence: link.pairingEvidenceID,
		request:         request,
		candidate:       append([]byte(nil), candidate...),
		settings:        proto.Clone(link.settings).(*gmproto.Settings),
		selectedCard:    proto.Clone(selected).(*gmproto.SIMCard),
		settingsAt:      link.settingsAt,
	}
	result := nativeRcsPrepareResult{
		Code: "prepared", TicketID: ticket.id, CandidateSnapshot: string(ticket.candidate),
		RunIncarnation: ticket.runIncarnation, RunGeneration: ticket.generation,
		StreamEpoch: ticket.streamEpoch, PairingEvidenceID: ticket.pairingEvidence,
	}
	encoded, fits := nativePreparedResultJSON(result)
	if !fits {
		link.mu.Unlock()
		return link.nativePrepareResult("rejected", "result_too_large")
	}
	link.nativeTicket = ticket
	link.mu.Unlock()
	link.emit("native_prepare", "prepared", 0)
	return string(encoded)
}

func (link *Link) nativePrepareResult(code, reason string) string {
	link.emit("native_prepare", reason, 0)
	return nativeRcsJSON(nativeRcsPrepareResult{Code: code})
}

func nativePreparedResultJSON(result nativeRcsPrepareResult) (string, bool) {
	encoded, _ := json.Marshal(result)
	return string(encoded), len(encoded) <= maxNativePrepareResultBytes
}

func validNativeRcsPrepare(request nativeRcsPrepareRequest) bool {
	if !validNativeUUID(request.CommandID) || request.CanonicalizerRevision == 0 || !validNativeText(request.IdentityRegion) {
		return false
	}
	binding := request.CarrierBinding
	return validNativeText(binding.CarrierRouteID) && binding.AndroidSubscriptionID >= 0 &&
		validNativeText(binding.OwnNumberRaw) && validOwnNumberSource(binding.OwnNumberSource) && binding.RouteObservedAtMS > 0
}

func validNativeUUID(value string) bool {
	parsed, err := uuid.Parse(value)
	return err == nil && parsed.Variant() == uuid.RFC4122 && parsed.String() == value
}

func validNativeText(value string) bool {
	return value != "" && len(value) <= maxOpaqueIDBytes && utf8.ValidString(value)
}

func (link *Link) nativePrepareFailureReasonLocked() string {
	if link.closed || link.state != stateLinked || !link.running {
		return "link_unavailable"
	}
	if !link.hasSendVerificationLocked() {
		return "send_unverified"
	}
	if link.runGeneration != link.generation {
		return "run_stale"
	}
	if !link.conversationIndexComplete {
		return "index_incomplete"
	}
	if link.conversationIndexGeneration != link.generation {
		return "index_generation"
	}
	if link.conversationIndexStreamEpoch != link.streamEpoch {
		return "index_epoch"
	}
	if link.pendingConversationEvents != 0 {
		return "pending_events"
	}
	if link.settings == nil {
		return "settings_missing"
	}
	if link.settingsAt.IsZero() || link.settingsStreamEpoch != link.streamEpoch {
		return "settings_stale"
	}
	if !validNativeUUID(link.runIncarnationID) {
		return "run_evidence_invalid"
	}
	if !validNativeUUID(link.pairingEvidenceID) {
		return "pairing_evidence_invalid"
	}
	return "prepared"
}

func uniqueEnabledRcsCard(settings *gmproto.Settings) *gmproto.SIMCard {
	var selected *gmproto.SIMCard
	for _, card := range settings.GetSIMCards() {
		if !card.GetRCSChats().GetEnabled() {
			continue
		}
		if card.GetSIMParticipant().GetID() == "" || card.GetSIMData().GetSIMPayload() == nil || selected != nil {
			return nil
		}
		selected = card
	}
	return selected
}

// nativeTicketCurrentLocked checks the frozen facts without mutating ticket state.
// Callers must hold link.mu and match the ticket pointer and stable ID first.
func (link *Link) nativeTicketCurrentLocked(ticket *nativeRcsTicket, ticketID string) bool {
	return ticket != nil && link.nativeTicket == ticket && ticket.id == ticketID && link.nativeFrozenCurrentLocked(ticket)
}

// Frozen currency remains required after consumption transfers ownership to the in-flight record.
func (link *Link) nativeFrozenCurrentLocked(ticket *nativeRcsTicket) bool {
	currentCard := uniqueEnabledRcsCard(link.settings)
	return ticket != nil && link.nowForTicketLocked().Before(ticket.expiresAt) &&
		link.nativeBaseAllowedLocked() && link.hasSendVerificationLocked() && link.generation == ticket.generation &&
		link.streamEpoch == ticket.streamEpoch && link.indexRevision == ticket.indexRevision &&
		link.settingsVersion == ticket.settingsVersion && link.settingsStreamEpoch == ticket.settingsEpoch &&
		link.runIncarnationID == ticket.runIncarnation && link.pairingEvidenceID == ticket.pairingEvidence &&
		proto.Equal(currentCard, ticket.selectedCard)
}

func (link *Link) nativeBaseAllowedLocked() bool {
	return !link.closed && link.state == stateLinked && link.running && link.runGeneration == link.generation && link.conversationIndexComplete &&
		link.conversationIndexGeneration == link.generation && link.conversationIndexStreamEpoch == link.streamEpoch &&
		link.pendingConversationEvents == 0 && link.settings != nil && !link.settingsAt.IsZero() &&
		link.settingsStreamEpoch == link.streamEpoch && validNativeUUID(link.runIncarnationID) && validNativeUUID(link.pairingEvidenceID)
}

func (link *Link) expireNativeTicketLocked(now time.Time) {
	if link.nativeTicket != nil && !link.nativeTicket.expiresAt.IsZero() && !now.Before(link.nativeTicket.expiresAt) {
		link.nativeTicket = nil
	}
}

// RevokeNativeSend atomically revokes only the exact prepared or in-flight ticket.
// It never waits on transport locks or performs a network operation.
func (link *Link) RevokeNativeSend(ticketID string) string {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.nativeTicket != nil && link.nativeTicket.id == ticketID {
		link.nativeTicket = nil
		return nativeRcsJSON(nativeRcsPrepareResult{Code: "revoked"})
	}
	if link.nativeInFlight == nil || link.nativeInFlight.ticketID != ticketID {
		return nativeRcsJSON(nativeRcsPrepareResult{Code: "none"})
	}
	if link.nativeInFlight.admitted {
		return nativeRcsJSON(nativeRcsPrepareResult{Code: "late"})
	}
	link.nativeInFlight.revoked = true
	return nativeRcsJSON(nativeRcsPrepareResult{Code: "revoked"})
}
