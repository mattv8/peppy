package gmlink

import (
	"encoding/json"
	"strings"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestConversationFacadeRequiresVerification(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	if err := link.RefreshConversationIndex(); err == nil || err.Error() != "m2_verification_required" {
		t.Fatalf("RefreshConversationIndex error = %v", err)
	}
	for _, result := range []string{link.GetConversation("synthetic-conversation"), link.ResolveExistingConversation("synthetic-target")} {
		if !strings.Contains(result, `"code":"m2_verification_required"`) {
			t.Fatalf("result = %s", result)
		}
	}
}

func TestDrainConversationEventsExcludesParticipantIdentity(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	link.m2VerificationPassed = true
	link.mu.Unlock()
	link.projectEvent(&gmproto.Conversation{
		ConversationID: "synthetic-conversation",
		Participants:   []*gmproto.Participant{{ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	})
	result := link.DrainConversationEvents()
	if strings.Contains(result, "synthetic-target") || !strings.Contains(result, "synthetic-conversation") {
		t.Fatalf("event result = %s", result)
	}
}

func TestResolveExistingConversationNeverReturnsParticipantIdentity(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	result := link.ResolveExistingConversation("synthetic-target")
	var decoded map[string]any
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	if _, exists := decoded["participant"]; exists {
		t.Fatalf("participant identity leaked in %s", result)
	}
}
