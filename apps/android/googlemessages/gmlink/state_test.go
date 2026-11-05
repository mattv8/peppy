package gmlink

import (
	"encoding/json"
	"testing"
)

func TestDecodeStateRejectsCorruption(t *testing.T) {
	if _, err := decodeState([]byte("not-json")); err == nil {
		t.Fatal("corrupt state was accepted")
	}
}

func TestDecodeStateReopensPairingAsUnlinked(t *testing.T) {
	state, err := decodeState([]byte(`{"version":1,"seq":4,"state":"pairing","stop_code":"","auth":{}}`))
	if err != nil || state.State != stateUnlinked || state.Seq != 4 {
		t.Fatalf("state=%+v err=%v", state, err)
	}
}

func TestPersistedEnvelopeHasOnlyPublicFieldsAndAuth(t *testing.T) {
	encoded, err := json.Marshal(persistedState{Version: stateVersion, State: stateUnlinked})
	if err != nil || string(encoded) != `{"version":1,"seq":0,"state":"unlinked","stop_code":""}` {
		t.Fatalf("encoded=%s err=%v", encoded, err)
	}
}
