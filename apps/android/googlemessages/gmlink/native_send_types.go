package gmlink

import (
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

type nativeRcsTicket struct {
	id                    string
	expiresAt             time.Time
	generation            uint64
	streamEpoch           uint64
	indexRevision         uint64
	settingsVersion       uint64
	settingsEpoch         uint64
	runIncarnation        string
	pairingEvidence       string
	request               nativeRcsPrepareRequest
	candidate             []byte
	settings              *gmproto.Settings
	selectedCard          *gmproto.SIMCard
	settingsAt            time.Time
	routeObservation      *nativeRcsObservation
	targetObservation     *nativeRcsObservation
	targetObservationJSON string
	conversation          *gmproto.Conversation
	facts                 *nativeRcsFacts
	group                 bool
}

type nativeRcsInFlight struct {
	ticketID       string
	admitted       bool
	revoked        bool
	membersChanged bool
}

type nativeRcsPrepareRequest struct {
	CommandID             string
	CanonicalizerRevision uint32
	IdentityRegion        string
	CarrierBinding        nativeRcsPrepareCarrierBinding
	ExpectedMemberDigest  string
}

type nativeRcsPrepareCarrierBinding struct {
	CarrierRouteID        string
	AndroidSubscriptionID int32
	OwnNumberRaw          string
	OwnNumberSource       string
	RouteObservedAtMS     int64
}

type nativeRcsCommitProjection struct {
	CommandID            string
	TmpID                string
	MappingRevision      uint64
	ProofVersion         uint32
	ProofDigest          [32]byte
	MessageBody          string
	Target               nativeRcsTargetSnapshot
	ExpectedMemberDigest string
}

type nativeRcsTargetSnapshot struct {
	ProofVersion          uint32
	PeppyConversationID   string
	GoogleConversationID  string
	ConversationType      string
	Status                string
	SendMode              string
	ReadOnly              bool
	DefaultOutgoing       bool
	Participants          []nativeRcsParticipant
	Class                 string
	CanonicalRecipient    string
	CanonicalOwnIdentity  string
	IdentityRegion        string
	CanonicalizerRevision uint32
	SelectedSIMID         string
	CarrierBinding        nativeRcsCarrierBinding
	Outgoing              nativeRcsOutgoing
	Evidence              nativeRcsEvidence
}

type nativeRcsParticipant struct {
	Identity            string `json:"identity"`
	GoogleParticipantID string `json:"google_participant_id"`
	IdentifierType      int32  `json:"identifier_type"`
	IsVisible           bool   `json:"is_visible"`
	IsMe                bool   `json:"is_me"`
}

type nativeRcsCarrierBinding struct {
	CarrierRouteID        string `json:"carrier_route_id"`
	AndroidSubscriptionID int32  `json:"android_subscription_id"`
	IsCurrentDefaultSMS   bool   `json:"is_current_default_sms"`
	OwnNumberRaw          string `json:"own_number_raw"`
	OwnNumberSource       string `json:"own_number_source"`
	RouteObservedAtMS     int64  `json:"route_observed_at_ms"`
	RouteContextRevision  uint64 `json:"route_context_revision"`
	SameHandsetEvidenceID string `json:"same_handset_evidence_id"`
}

type nativeRcsSIMPayload struct {
	Two       int32 `json:"two"`
	SIMNumber int32 `json:"sim_number"`
}

type nativeRcsOutgoing struct {
	OutgoingID       string              `json:"outgoing_id"`
	SIMParticipantID string              `json:"sim_participant_id"`
	OwnIdentityRaw   string              `json:"own_identity_raw"`
	SIMPayload       nativeRcsSIMPayload `json:"sim_payload"`
	RcsChatsEnabled  bool                `json:"rcs_chats_enabled"`
}

type nativeRcsLifetime struct {
	RuntimeID         string `json:"runtime_id"`
	RunGeneration     uint64 `json:"run_generation"`
	LinkGeneration    uint64 `json:"link_generation"`
	SessionGeneration uint64 `json:"session_generation"`
	ContextRevision   uint64 `json:"context_revision"`
}

type nativeRcsEvidence struct {
	Lifetime                 nativeRcsLifetime   `json:"lifetime"`
	LookupID                 string              `json:"lookup_id"`
	LookupComplete           bool                `json:"lookup_complete"`
	GoogleCandidateCount     uint32              `json:"google_candidate_count"`
	SettingsRevision         uint64              `json:"settings_revision"`
	SettingsProvenance       string              `json:"settings_provenance"`
	SettingsObservedAtMS     int64               `json:"settings_observed_at_ms"`
	SettingsStreamEpoch      uint64              `json:"settings_stream_epoch"`
	SettingsCurrentness      string              `json:"settings_currentness"`
	CurrentSettingsSIMs      []nativeRcsOutgoing `json:"current_settings_sims"`
	SettingsValidatedAtMS    int64               `json:"settings_validated_at_ms"`
	ConversationObservedAtMS int64               `json:"conversation_observed_at_ms"`
}

type nativeRcsFacts struct {
	Request       nativeRcsFactsRequest
	StoredMapping nativeRcsStoredMapping
}

type nativeRcsFactsRequest struct {
	CommandID           string
	PeppyConversationID string
	CanonicalRecipient  string
}

type nativeRcsStoredMapping struct {
	ConversationID       string
	GoogleConversationID string
	Class                string
	ProofVersion         uint32
	Proof                nativeRcsStoredProof
	MappingRevision      uint64
	VerifiedAtMS         int64
}

type nativeRcsStoredProof struct {
	CanonicalParticipant string
	IdentityRegion       string
	OwnIdentity          string
	Digest               [32]byte
}
