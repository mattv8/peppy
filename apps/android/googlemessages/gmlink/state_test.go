package gmlink

import (
	"encoding/json"
	"testing"
)

func TestDecodeStateReopensPairingAsPrevious(t *testing.T) {
	state, err := decodeState([]byte(`{"version":1,"seq":4,"state":"pairing","pairing_from":"needs_relink","stop_code":"","auth":{"cookie":"synthetic"}}`))
	if err != nil || state.State != stateNeedsRelink || len(state.Auth) == 0 || state.Seq != 4 {
		t.Fatalf("state=%+v err=%v", state, err)
	}
}

func TestPersistedEnvelopeHasOnlyPublicFieldsAndAuth(t *testing.T) {
	encoded, err := json.Marshal(persistedState{Version: stateVersion, State: stateUnlinked})
	if err != nil || string(encoded) != `{"version":1,"seq":0,"state":"unlinked","stop_code":""}` {
		t.Fatalf("encoded=%s err=%v", encoded, err)
	}
}
