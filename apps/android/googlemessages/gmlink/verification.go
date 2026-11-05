package gmlink

import "encoding/json"

const (
	ownerVerificationProcedureRevision = "g0-libgm-verification-v1"
	pinnedLibgmRevision                = "v0.2610.1-0.20261103213626-d9140a5e807e"
)

// ownerVerification is runtime-only evidence. It is intentionally not a
// boolean and is never restored from persisted link state.
type ownerVerification struct {
	ProcedureRevision string   `json:"procedure_revision"`
	LibgmRevision     string   `json:"libgm_revision"`
	Capabilities      []string `json:"capabilities"`
	EvidenceReference string   `json:"evidence_reference"`
	Lifetime          string   `json:"lifetime"`
	generation        uint64
}

// SetOwnerVerification reserves the native handoff API, but no production
// producer exists in Stage 1. Rejecting all input prevents a caller-supplied
// JSON record from becoming send authority.
func (link *Link) SetOwnerVerification(evidenceJSON string) error {
	var evidence ownerVerification
	if err := json.Unmarshal([]byte(evidenceJSON), &evidence); err != nil ||
		evidence.ProcedureRevision != ownerVerificationProcedureRevision ||
		evidence.LibgmRevision != pinnedLibgmRevision || evidence.EvidenceReference == "" || evidence.Lifetime == "" || len(evidence.Capabilities) == 0 {
		return errOwnerVerificationInvalid
	}
	return errOwnerVerificationUnavailable
}

func (link *Link) hasOwnerVerificationLocked() bool {
	return !link.closed && link.ownerVerification != nil && link.ownerVerification.generation == link.generation
}
