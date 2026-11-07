package gmlink

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/google/uuid"
)

func TestNativeSendFoundationPersistsPairingEvidenceAndMintsRunIncarnation(t *testing.T) {
	evidence := "c2b8ac48-a6e6-4f9b-9467-37f48f83db82"
	state, err := decodeState([]byte(`{"version":1,"seq":4,"state":"linked","stop_code":"","pairing_evidence_id":"` + evidence + `","auth":{}}`))
	if err != nil || state.PairingEvidenceID != evidence {
		t.Fatalf("state=%+v err=%v", state, err)
	}
	if _, err := decodeState([]byte(`{"version":1,"state":"linked","stop_code":"","auth":{}}`)); err == nil {
		t.Fatal("legacy linked state without pairing evidence was accepted")
	}
	link, _ := openSafetyLink(t, nil)
	if _, err := uuid.Parse(link.runIncarnationID); err != nil {
		t.Fatalf("run incarnation is not a UUID: %q", link.runIncarnationID)
	}
}

func TestNativeSendFoundationSnapshotCarriesRunIncarnationWithoutChangingToken(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link)
	var snapshot conversationCandidateSnapshot
	if err := json.Unmarshal([]byte(link.ConversationCandidateSnapshot()), &snapshot); err != nil {
		t.Fatal(err)
	}
	if snapshot.RunIncarnation != link.runIncarnationID {
		t.Fatalf("snapshot incarnation = %q, want %q", snapshot.RunIncarnation, link.runIncarnationID)
	}
	if parts := strings.Split(snapshot.SnapshotToken, ":"); len(parts) != 3 || parts[0] != "7" || parts[1] != "3" {
		t.Fatalf("snapshot token = %q, want unchanged three-part token", snapshot.SnapshotToken)
	}
}

func TestNativeSendFoundationLegacyPrepareRefusesNativeOperation(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	link.nativeTicket = &nativeRcsTicket{id: "native-ticket"}
	link.mu.Unlock()
	if code := stage2Code(t, link.PrepareRcs(`{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"conversation","snapshot_token":"7:3:1","content":"body"}`)); code != "busy" {
		t.Fatalf("PrepareRcs code = %q, want busy", code)
	}
}

func TestNativeRcsFactsDecodeRejectsMissingOrUnexpectedAuthority(t *testing.T) {
	valid := `{"request":{"command_id":"command","peppy_conversation_id":"peppy","canonical_recipient":"+12025550100"},"stored_mapping":{"conversation_id":"peppy","google_conversation_id":"google","class":"OneToOne","proof_version":2,"proof":{"canonical_participant":"+12025550100","identity_region":"US","own_identity":"+12025550101","digest":[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31]},"mapping_revision":7,"verified_at_ms":1000}}`
	facts, ok := decodeNativeRcsFacts(valid)
	if !ok || facts.StoredMapping.MappingRevision != 7 || facts.StoredMapping.Proof.Digest[31] != 31 {
		t.Fatalf("facts=%#v ok=%t", facts, ok)
	}
	for _, input := range []string{
		`{"request":{"command_id":"command","peppy_conversation_id":"peppy"},"stored_mapping":{}}`,
		`{"request":{"command_id":"command","peppy_conversation_id":"peppy","canonical_recipient":"+12025550100"},"stored_mapping":{"conversation_id":"peppy","google_conversation_id":"google","class":"OneToOne","proof_version":2,"proof":{"canonical_participant":"+12025550100","identity_region":"US","own_identity":"+12025550101","digest":[]},"mapping_revision":7,"verified_at_ms":1000}}`,
	} {
		if _, ok := decodeNativeRcsFacts(input); ok {
			t.Fatal("accepted incomplete native facts")
		}
	}
}
