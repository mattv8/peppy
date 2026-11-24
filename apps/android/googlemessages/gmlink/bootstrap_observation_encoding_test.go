package gmlink

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestBootstrapObservationEncodingMatchesLegacyResult(t *testing.T) {
	for _, observation := range []nativeRcsObservation{
		{},
		{GoogleConversationID: "chat \"<&>\n雪", Participants: []nativeRcsParticipant{{Identity: "+123", IsVisible: true}}, NativeEvidence: nativeRcsEvidence{LookupID: "lookup", CurrentSettingsSIMs: []nativeRcsOutgoing{{OwnIdentityRaw: "+123"}}}},
		{GoogleConversationID: strings.Repeat("<", 8000)},
	} {
		outer, inner, ok := encodeNativeObservation("ok", observation)
		if !ok {
			t.Fatal("observation unexpectedly rejected")
		}
		var legacy nativeRcsObservationResult
		if err := json.Unmarshal([]byte(outer), &legacy); err != nil {
			t.Fatal(err)
		}
		want := bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "ok", Observation: legacy.Observation})
		got := bootstrapRcsJSON(nativeRcsBootstrapResult{Code: "ok", Observation: inner})
		if got != want {
			t.Fatalf("result JSON differs:\ngot %s\nwant %s", got, want)
		}
	}
}
