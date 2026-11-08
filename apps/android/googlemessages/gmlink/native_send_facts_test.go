package gmlink

import (
	"encoding/json"
	"testing"

	"github.com/google/uuid"
)

type nativeFactsBinder interface{ BindNativeRcsFacts(string, string) string }

func factsBinder(t *testing.T, link *Link) nativeFactsBinder {
	t.Helper()
	binder, ok := any(link).(nativeFactsBinder)
	if !ok {
		t.Fatal("native facts binding is not implemented")
	}
	return binder
}

// Schema-derived stored facts for fake transport tests, not a Rust-produced fixture.
func nativeFactsFixture(t *testing.T, link *Link) map[string]any {
	t.Helper()
	link.mu.Lock()
	defer link.mu.Unlock()
	ticket := link.nativeTicket
	conversationID := uuid.NewString()
	digest := make([]int, 32)
	digest[0] = 7
	return map[string]any{
		"request": map[string]any{"command_id": ticket.request.CommandID, "peppy_conversation_id": conversationID, "canonical_recipient": "+12025550123"},
		"stored_mapping": map[string]any{
			"conversation_id": conversationID, "google_conversation_id": "synthetic-conversation", "class": "OneToOne",
			"proof_version": 2, "mapping_revision": 29, "verified_at_ms": ticket.routeObservation.ObservedAtMS,
			"proof": map[string]any{"canonical_participant": "+12025550123", "identity_region": "US", "own_identity": "+12025550101", "digest": digest},
		},
	}
}

func bindNativeFixtureTarget(t *testing.T, link *Link, prepared nativeRcsPrepareResult) {
	t.Helper()
	if got := stage2Code(t, link.BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))); got != "target_bound" {
		t.Fatal("target binding failed")
	}
}

func TestNativeSendBindFactsRequiresTargetAndBindsOnlyOnce(t *testing.T) {
	for _, targetBound := range []bool{false, true} {
		link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
		if targetBound {
			bindNativeFixtureTarget(t, link, prepared)
		}
		encoded, err := json.Marshal(nativeFactsFixture(t, link))
		if err != nil {
			t.Fatal(err)
		}
		got := stage2Code(t, factsBinder(t, link).BindNativeRcsFacts(prepared.TicketID, string(encoded)))
		if targetBound {
			if got != "facts_bound" {
				t.Fatal(got)
			}
			if stage2Code(t, factsBinder(t, link).BindNativeRcsFacts(prepared.TicketID, string(encoded))) != "rejected" {
				t.Fatal("facts bound twice")
			}
		} else if got != "rejected" {
			t.Fatal("facts bound before target")
		}
		if phone.countAction("SEND_MESSAGE") != 0 {
			t.Fatal("facts binding dispatched")
		}
		cleanup()
	}
}

func TestNativeSendBindFactsRejectsMismatchedStoredAuthority(t *testing.T) {
	for _, field := range []string{"command_id", "conversation_id", "google_conversation_id", "mapping_revision", "verified_at_ms", "identity_region", "canonical_participant", "class", "settings_drift"} {
		t.Run(field, func(t *testing.T) {
			link, _, cleanup, prepared, _ := nativeTargetFixture(t)
			defer cleanup()
			bindNativeFixtureTarget(t, link, prepared)
			facts := nativeFactsFixture(t, link)
			request := facts["request"].(map[string]any)
			mapping := facts["stored_mapping"].(map[string]any)
			proof := mapping["proof"].(map[string]any)
			switch field {
			case "command_id":
				request[field] = uuid.NewString()
			case "conversation_id":
				mapping[field] = uuid.NewString()
			case "google_conversation_id":
				mapping[field] = "other-google"
			case "mapping_revision":
				mapping[field] = 30
			case "verified_at_ms":
				mapping[field] = 1
			case "identity_region":
				proof[field] = "CA"
			case "canonical_participant":
				proof[field] = "+12025550124"
			case "class":
				mapping[field] = "SelfChat"
			case "settings_drift":
				link.mu.Lock()
				link.settingsVersion++
				link.mu.Unlock()
			}
			encoded, err := json.Marshal(facts)
			if err != nil {
				t.Fatal(err)
			}
			if stage2Code(t, factsBinder(t, link).BindNativeRcsFacts(prepared.TicketID, string(encoded))) != "rejected" {
				t.Fatal("mismatched facts accepted")
			}
			link.mu.Lock()
			remaining := link.nativeTicket
			link.mu.Unlock()
			if remaining != nil {
				t.Fatal("invalid facts left ticket usable")
			}
		})
	}
}
