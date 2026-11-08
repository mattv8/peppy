package gmlink

import (
	"encoding/json"
	"unicode/utf8"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const maxNativeObservationBytes = 65536

// This local-only wire record mirrors Rust's strict RcsMappingObservation.
type nativeRcsObservation struct {
	GoogleConversationID   string                  `json:"google_conversation_id"`
	GoogleCandidateCount   uint32                  `json:"google_candidate_count"`
	Participants           []nativeRcsParticipant  `json:"participants"`
	ConversationType       string                  `json:"conversation_type"`
	Status                 string                  `json:"status"`
	SendMode               string                  `json:"send_mode"`
	ReadOnly               bool                    `json:"read_only"`
	DefaultOutgoing        bool                    `json:"default_outgoing"`
	SelectedSIMID          string                  `json:"selected_sim_id"`
	SelectedSIMOwnIdentity string                  `json:"selected_sim_own_identity"`
	CarrierBinding         nativeRcsCarrierBinding `json:"carrier_binding"`
	Outgoing               nativeRcsOutgoing       `json:"outgoing"`
	NativeEvidence         nativeRcsEvidence       `json:"native_evidence"`
	IdentityRegion         string                  `json:"identity_region"`
	ObservedAtMS           int64                   `json:"observed_at_ms"`
	LinkGeneration         uint64                  `json:"link_generation"`
	SessionGeneration      uint64                  `json:"session_generation"`
	ContextRevision        uint64                  `json:"context_revision"`
	CanonicalizerRevision  uint32                  `json:"canonicalizer_revision"`
	MappingRevision        uint64                  `json:"mapping_revision"`
	LinkState              string                  `json:"link_state"`
}

type nativeRcsObservationResult struct {
	Code        string `json:"code"`
	Observation string `json:"observation"`
}

func encodeNativeObservation(code string, observation nativeRcsObservation) (string, string, bool) {
	inner, err := json.Marshal(observation)
	if err != nil || len(inner) > maxNativeObservationBytes {
		return "", "", false
	}
	outer, err := json.Marshal(nativeRcsObservationResult{Code: code, Observation: string(inner)})
	return string(outer), string(inner), err == nil && len(outer) <= 6*maxNativeObservationBytes+256
}

func nativeOutgoingFromCard(card *gmproto.SIMCard) (nativeRcsOutgoing, bool) {
	if card == nil || card.GetSIMParticipant().GetID() == "" || card.GetSIMData().GetSIMPayload() == nil {
		return nativeRcsOutgoing{}, false
	}
	own := card.GetSIMData().GetInternationalPhoneNumber()
	if !utf8.ValidString(own) || (card.GetRCSChats().GetEnabled() && own == "") {
		return nativeRcsOutgoing{}, false
	}
	payload := card.GetSIMData().GetSIMPayload()
	return nativeRcsOutgoing{
		OutgoingID: card.GetSIMParticipant().GetID(), SIMParticipantID: card.GetSIMParticipant().GetID(),
		OwnIdentityRaw: own, SIMPayload: nativeRcsSIMPayload{Two: payload.GetTwo(), SIMNumber: payload.GetSIMNumber()},
		RcsChatsEnabled: card.GetRCSChats().GetEnabled(),
	}, true
}

// BindNativeRcsRoute stamps core-allocated ordering onto frozen, genuinely observed route facts.
func (link *Link) BindNativeRcsRoute(ticketID string, linkGeneration, sessionGeneration, contextRevision, mappingRevision, observedAtMS int64) string {
	link.mu.Lock()
	defer link.mu.Unlock()
	ticket := link.nativeTicket
	refuse := func() string {
		if ticket != nil && ticket.id == ticketID && link.nativeTicket == ticket {
			link.nativeTicket = nil
		}
		return nativeRcsJSON(nativeRcsPrepareResult{Code: "rejected"})
	}
	if !link.nativeTicketCurrentLocked(ticket, ticketID) || ticket.routeObservation != nil {
		return refuse()
	}
	nowMS := link.nowForTicketLocked().UnixMilli()
	if linkGeneration <= 0 || sessionGeneration <= 0 || contextRevision <= 0 || mappingRevision <= 0 ||
		observedAtMS < 0 || nowMS < observedAtMS || nowMS-observedAtMS > preparedRcsTicketTTL.Milliseconds() {
		return refuse()
	}
	outgoing, ok := nativeOutgoingFromCard(ticket.selectedCard)
	if !ok {
		return refuse()
	}
	sims := make([]nativeRcsOutgoing, 0, len(ticket.settings.GetSIMCards()))
	for _, card := range ticket.settings.GetSIMCards() {
		sim, valid := nativeOutgoingFromCard(card)
		if !valid {
			return refuse()
		}
		sims = append(sims, sim)
	}
	binding := ticket.request.CarrierBinding
	observation := nativeRcsObservation{
		Participants: []nativeRcsParticipant{}, ConversationType: "Other", SendMode: "Other", ReadOnly: true,
		SelectedSIMID: binding.CarrierRouteID, SelectedSIMOwnIdentity: outgoing.OwnIdentityRaw,
		CarrierBinding: nativeRcsCarrierBinding{
			CarrierRouteID: binding.CarrierRouteID, AndroidSubscriptionID: binding.AndroidSubscriptionID,
			IsCurrentDefaultSMS: true, OwnNumberRaw: binding.OwnNumberRaw, OwnNumberSource: binding.OwnNumberSource,
			RouteObservedAtMS: binding.RouteObservedAtMS, RouteContextRevision: uint64(contextRevision), SameHandsetEvidenceID: ticket.pairingEvidence,
		},
		Outgoing: outgoing,
		NativeEvidence: nativeRcsEvidence{
			Lifetime: nativeRcsLifetime{RuntimeID: ticket.runIncarnation, RunGeneration: ticket.generation, LinkGeneration: uint64(linkGeneration), SessionGeneration: uint64(sessionGeneration), ContextRevision: uint64(contextRevision)},
			LookupID: uuid.NewString(), SettingsRevision: ticket.settingsVersion, SettingsProvenance: "CurrentEvent",
			SettingsObservedAtMS: ticket.settingsAt.UnixMilli(), SettingsStreamEpoch: ticket.settingsEpoch,
			SettingsCurrentness: "ContinuousVerifiedFeed", CurrentSettingsSIMs: sims,
			SettingsValidatedAtMS: nowMS, ConversationObservedAtMS: nowMS,
		},
		IdentityRegion: ticket.request.IdentityRegion, ObservedAtMS: observedAtMS,
		LinkGeneration: uint64(linkGeneration), SessionGeneration: uint64(sessionGeneration), ContextRevision: uint64(contextRevision),
		CanonicalizerRevision: ticket.request.CanonicalizerRevision, MappingRevision: uint64(mappingRevision), LinkState: "Linked",
	}
	encoded, _, valid := encodeNativeObservation("route_bound", observation)
	if !valid {
		return refuse()
	}
	ticket.routeObservation = &observation
	return encoded
}
