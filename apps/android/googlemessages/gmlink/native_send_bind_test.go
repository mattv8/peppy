package gmlink

import (
	"bytes"
	"encoding/json"
	"reflect"
	"strings"
	"testing"
	"time"
)

type nativeRouteBinder interface {
	BindNativeRcsRoute(string, int64, int64, int64, int64, int64) string
}

func routeBinder(t *testing.T, link *Link) nativeRouteBinder {
	t.Helper()
	binder, ok := any(link).(nativeRouteBinder)
	if !ok {
		t.Fatal("native route binding is not implemented")
	}
	return binder
}

func nativeRouteFixture(t *testing.T) (*Link, func(), string) {
	t.Helper()
	link, cleanup, request := nativePrepareFixture(t)
	link.mu.Lock()
	link.ticketNow = func() time.Time { return time.UnixMilli(1_000) }
	link.settingsAt = time.UnixMilli(500)
	link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = "+12025550101"
	link.mu.Unlock()
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &prepared); err != nil || prepared.Code != "prepared" {
		t.Fatalf("prepare failed: %v", err)
	}
	return link, cleanup, prepared.TicketID
}

func decodeNativeRouteObservation(t *testing.T, response string) map[string]any {
	t.Helper()
	var outer map[string]json.RawMessage
	if err := json.Unmarshal([]byte(response), &outer); err != nil || len(outer) != 2 || string(outer["code"]) != `"route_bound"` {
		t.Fatal("unexpected route response shape")
	}
	var observation string
	if err := json.Unmarshal(outer["observation"], &observation); err != nil {
		t.Fatal(err)
	}
	decoder := json.NewDecoder(strings.NewReader(observation))
	decoder.UseNumber()
	var result map[string]any
	if err := decoder.Decode(&result); err != nil {
		t.Fatal(err)
	}
	return result
}

func TestNativeSendBindRoutePreservesProvenanceAndIndependentCounters(t *testing.T) {
	link, cleanup, ticketID := nativeRouteFixture(t)
	defer cleanup()
	link.mu.Lock()
	deadline := link.nativeTicket.expiresAt
	runtimeID, pairingID := link.runIncarnationID, link.pairingEvidenceID
	goGeneration, streamEpoch, settingsVersion := link.generation, link.streamEpoch, link.settingsVersion
	link.mu.Unlock()
	const mappingRevision = int64(1<<53 + 1)
	result := decodeNativeRouteObservation(t, routeBinder(t, link).BindNativeRcsRoute(ticketID, 11, 17, 23, mappingRevision, 999))
	outgoing := map[string]any{
		"outgoing_id": "synthetic-outgoing", "sim_participant_id": "synthetic-outgoing", "own_identity_raw": "+12025550101",
		"sim_payload": map[string]any{"two": 0, "sim_number": 1}, "rcs_chats_enabled": true,
	}
	expected := map[string]any{
		"google_conversation_id": "", "google_candidate_count": 0, "participants": []any{},
		"conversation_type": "Other", "status": "", "send_mode": "Other", "read_only": true, "default_outgoing": false,
		"selected_sim_id": "android-subscription-1", "selected_sim_own_identity": "+12025550101",
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1,
			"is_current_default_sms": true, "own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber",
			"route_observed_at_ms": 1, "route_context_revision": 23, "same_handset_evidence_id": pairingID},
		"outgoing": outgoing,
		"native_evidence": map[string]any{
			"lifetime":  map[string]any{"runtime_id": runtimeID, "run_generation": goGeneration, "link_generation": 11, "session_generation": 17, "context_revision": 23},
			"lookup_id": "lookup-placeholder", "lookup_complete": false, "google_candidate_count": 0,
			"settings_revision": settingsVersion, "settings_provenance": "CurrentEvent", "settings_observed_at_ms": 500,
			"settings_stream_epoch": streamEpoch, "settings_currentness": "ContinuousVerifiedFeed",
			"current_settings_sims": []any{outgoing}, "settings_validated_at_ms": 1000, "conversation_observed_at_ms": 1000,
		},
		"identity_region": "US", "observed_at_ms": 999, "link_generation": 11, "session_generation": 17,
		"context_revision": 23, "canonicalizer_revision": 1, "mapping_revision": mappingRevision, "link_state": "Linked",
	}
	evidence := result["native_evidence"].(map[string]any)
	if lookup, ok := evidence["lookup_id"].(string); !ok || !validNativeUUID(lookup) {
		t.Fatal("lookup identity is not a fresh UUID")
	}
	evidence["lookup_id"] = "lookup-placeholder"
	encoded, err := json.Marshal(expected)
	if err != nil {
		t.Fatal(err)
	}
	decoder := json.NewDecoder(bytes.NewReader(encoded))
	decoder.UseNumber()
	var normalized map[string]any
	if err := decoder.Decode(&normalized); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(result, normalized) {
		t.Fatal("route observation differs from the independent complete schema expectation")
	}
	link.mu.Lock()
	unchangedDeadline := link.nativeTicket.expiresAt.Equal(deadline)
	link.mu.Unlock()
	if !unchangedDeadline {
		t.Fatal("route bind extended the ticket deadline")
	}
	if got := stage2Code(t, routeBinder(t, link).BindNativeRcsRoute(ticketID, 11, 17, 23, mappingRevision, 999)); got != "rejected" {
		t.Fatal("route ticket bound twice")
	}
}

func TestNativeSendBindRouteRefusesInvalidOrChangedFacts(t *testing.T) {
	for _, name := range []string{"zero_tuple", "negative_revision", "future_time", "expired", "settings_changed", "no_google_own_number", "oversized_observation"} {
		t.Run(name, func(t *testing.T) {
			link, cleanup, ticketID := nativeRouteFixture(t)
			defer cleanup()
			generation, revision, observed := int64(11), int64(29), int64(999)
			link.mu.Lock()
			switch name {
			case "zero_tuple":
				generation = 0
			case "negative_revision":
				revision = -1
			case "future_time":
				observed = 1001
			case "expired":
				link.ticketNow = func() time.Time { return time.UnixMilli(31_000) }
			case "settings_changed":
				link.settingsVersion++
			case "no_google_own_number":
				link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = ""
				link.nativeTicket.selectedCard.SIMData.InternationalPhoneNumber = ""
				link.nativeTicket.settings.SIMCards[0].SIMData.InternationalPhoneNumber = ""
			case "oversized_observation":
				link.nativeTicket.request.CarrierBinding.OwnNumberRaw = strings.Repeat("x", 65_537)
			}
			link.mu.Unlock()
			if got := stage2Code(t, routeBinder(t, link).BindNativeRcsRoute(ticketID, generation, 17, 23, revision, observed)); got != "rejected" {
				t.Fatalf("code = %q", got)
			}
			link.mu.Lock()
			remaining := link.nativeTicket
			link.mu.Unlock()
			if remaining != nil {
				t.Fatal("failed route bind retained a usable ticket")
			}
		})
	}
}

func TestNativeSendBindRouteWrongTicketDoesNotCancelSuccessor(t *testing.T) {
	link, cleanup, ticketID := nativeRouteFixture(t)
	defer cleanup()
	if got := stage2Code(t, routeBinder(t, link).BindNativeRcsRoute("old-ticket", 11, 17, 23, 29, 999)); got != "rejected" {
		t.Fatal(got)
	}
	decodeNativeRouteObservation(t, routeBinder(t, link).BindNativeRcsRoute(ticketID, 11, 17, 23, 29, 999))
}
