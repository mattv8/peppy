package gmlink

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

func nativePrepareFixture(t *testing.T) (*Link, func(), string) {
	t.Helper()
	link, cleanup, _, _ := preparedRcsFixture(t)
	link.mu.Lock()
	link.pairingEvidenceID = uuid.NewString()
	link.mu.Unlock()
	request := `{"command_id":"` + uuid.NewString() + `","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"android-subscription-1","android_subscription_id":1,"own_number_raw":"+12025550101","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":1}}`
	return link, cleanup, request
}

func TestNativeSendPrepareFreezesExactSnapshotAndRevokes(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	var result nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &result); err != nil || result.Code != "prepared" {
		t.Fatalf("prepare result=%+v err=%v", result, err)
	}
	link.mu.Lock()
	ticket := link.nativeTicket
	current := link.nativeTicketCurrentLocked(ticket, result.TicketID)
	link.mu.Unlock()
	if ticket == nil || string(ticket.candidate) != result.CandidateSnapshot || !current {
		t.Fatal("prepared ticket did not retain its exact frozen snapshot")
	}
	if got := stage2Code(t, link.RevokeNativeSend(result.TicketID)); got != "revoked" {
		t.Fatalf("revoke code=%q", got)
	}
	if got := stage2Code(t, link.RevokeNativeSend(result.TicketID)); got != "none" {
		t.Fatalf("second revoke code=%q", got)
	}
}

func TestNativeSendPrepareRejectsInvalidAndBusyOrExpiredSlots(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	if got := stage2Code(t, link.PrepareNativeRcs(`{"command_id":"bad"}`)); got != "invalid_request" {
		t.Fatalf("invalid code=%q", got)
	}
	link.mu.Lock()
	link.nativeInFlight = &nativeRcsInFlight{ticketID: "other"}
	link.mu.Unlock()
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "busy" {
		t.Fatalf("in-flight code=%q", got)
	}
	link.mu.Lock()
	link.nativeInFlight = nil
	link.nativeTicket = &nativeRcsTicket{id: "expired", expiresAt: link.nowForTicketLocked().Add(-time.Nanosecond)}
	link.mu.Unlock()
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "prepared" {
		t.Fatalf("expired slot code=%q", got)
	}
}

func TestNativeSendPrepareValidatesAndroidSubscriptionID(t *testing.T) {
	for _, test := range []struct {
		name string
		id   string
		want string
	}{
		{name: "zero", id: "0", want: "prepared"},
		{name: "negative", id: "-1", want: "invalid_request"},
	} {
		t.Run(test.name, func(t *testing.T) {
			link, cleanup, request := nativePrepareFixture(t)
			defer cleanup()
			request = strings.Replace(request, `"android_subscription_id":1`, `"android_subscription_id":`+test.id, 1)
			if got := stage2Code(t, link.PrepareNativeRcs(request)); got != test.want {
				t.Fatalf("subscription ID %s code=%q, want %q", test.id, got, test.want)
			}
		})
	}
}

func TestNativeSendRevokePreservesSuccessorAndMarksInflight(t *testing.T) {
	link, cleanup, _ := nativePrepareFixture(t)
	defer cleanup()
	link.mu.Lock()
	link.nativeTicket = &nativeRcsTicket{id: "successor"}
	link.nativeInFlight = &nativeRcsInFlight{ticketID: "inflight"}
	link.mu.Unlock()
	if got := stage2Code(t, link.RevokeNativeSend("unknown")); got != "none" {
		t.Fatalf("unknown code=%q", got)
	}
	if got := stage2Code(t, link.RevokeNativeSend("inflight")); got != "revoked" {
		t.Fatalf("inflight code=%q", got)
	}
	link.mu.Lock()
	revoked, successor := link.nativeInFlight.revoked, link.nativeTicket.id
	link.nativeInFlight.admitted = true
	link.mu.Unlock()
	if !revoked || successor != "successor" {
		t.Fatal("revoke mutated the wrong native operation")
	}
	if got := stage2Code(t, link.RevokeNativeSend("inflight")); got != "late" {
		t.Fatalf("admitted code=%q", got)
	}
}

func TestNativeSendPrepareDoesNotInvokeTransport(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	link.sendMessage = func(_ context.Context, _ *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
		t.Fatal("PrepareNativeRcs invoked SendMessage")
		return nil, nil
	}
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "prepared" {
		t.Fatalf("prepare code=%q", got)
	}
}

func TestNativeSendPrepareExpiresLegacyTicketBeforeBusyCheck(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	link.mu.Lock()
	link.preparedTicket = &preparedRcsTicket{id: "legacy", expiresAt: link.nowForTicketLocked().Add(-time.Nanosecond)}
	link.mu.Unlock()
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "prepared" {
		t.Fatalf("native prepare after expired legacy ticket = %q, want prepared", got)
	}
}

func TestNativeSendPrepareBusyBothDirectionsAndAtDeadline(t *testing.T) {
	t.Run("legacy_then_native", func(t *testing.T) {
		link, cleanup, request := nativePrepareFixture(t)
		defer cleanup()
		now := time.Unix(1_000, 0)
		link.mu.Lock()
		link.ticketNow = func() time.Time { return now }
		link.mu.Unlock()
		snapshot := link.ConversationCandidateSnapshot()
		var candidate conversationCandidateSnapshot
		if err := json.Unmarshal([]byte(snapshot), &candidate); err != nil {
			t.Fatal(err)
		}
		legacy := `{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","snapshot_token":"` + candidate.SnapshotToken + `","content":"body"}`
		if got := stage2Code(t, link.PrepareRcs(legacy)); got != "prepared" {
			t.Fatalf("legacy prepare = %q", got)
		}
		if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "busy" {
			t.Fatalf("native prepare while legacy prepared = %q", got)
		}
		now = now.Add(preparedRcsTicketTTL)
		if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "prepared" {
			t.Fatalf("native prepare at legacy expiry deadline = %q", got)
		}
	})
	t.Run("native_then_legacy", func(t *testing.T) {
		link, cleanup, request := nativePrepareFixture(t)
		defer cleanup()
		now := time.Unix(2_000, 0)
		link.mu.Lock()
		link.ticketNow = func() time.Time { return now }
		link.mu.Unlock()
		if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "prepared" {
			t.Fatalf("native prepare = %q", got)
		}
		snapshot := link.ConversationCandidateSnapshot()
		var candidate conversationCandidateSnapshot
		if err := json.Unmarshal([]byte(snapshot), &candidate); err != nil {
			t.Fatal(err)
		}
		legacy := `{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","snapshot_token":"` + candidate.SnapshotToken + `","content":"body"}`
		if got := stage2Code(t, link.PrepareRcs(legacy)); got != "busy" {
			t.Fatalf("legacy prepare while native prepared = %q", got)
		}
		now = now.Add(preparedRcsTicketTTL)
		if got := stage2Code(t, link.PrepareRcs(legacy)); got != "prepared" {
			t.Fatalf("legacy prepare at native expiry deadline = %q", got)
		}
	})
}

func TestNativeSendPrepareRefusalMatrix(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*Link)
	}{
		{"missing_pairing_uuid", func(link *Link) { link.pairingEvidenceID = "" }},
		{"stopped", func(link *Link) { link.running = false }},
		{"unverified", func(link *Link) { link.ownerVerification = nil }},
		{"incomplete", func(link *Link) { link.conversationIndexComplete = false }},
		{"pending", func(link *Link) { link.pendingConversationEvents = 1 }},
		{"stale_settings_epoch", func(link *Link) { link.settingsStreamEpoch++ }},
		{"no_enabled_card", func(link *Link) { link.settings.SIMCards[0].RCSChats.Enabled = false }},
		{"missing_card_id", func(link *Link) { link.settings.SIMCards[0].SIMParticipant.ID = "" }},
		{"missing_card_payload", func(link *Link) { link.settings.SIMCards[0].SIMData.SIMPayload = nil }},
		{"dual_enabled_cards", func(link *Link) {
			link.settings.SIMCards = append(link.settings.SIMCards, proto.Clone(link.settings.SIMCards[0]).(*gmproto.SIMCard))
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			link, cleanup, request := nativePrepareFixture(t)
			defer cleanup()
			link.mu.Lock()
			tc.mutate(link)
			link.mu.Unlock()
			if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "rejected" {
				t.Fatalf("code = %q, want rejected", got)
			}
		})
	}
}

func TestNativeSendCurrentnessDetectsFrozenStateDrift(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	var result nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &result); err != nil {
		t.Fatal(err)
	}
	link.mu.Lock()
	ticket := link.nativeTicket
	frozenSettings := proto.Clone(ticket.settings).(*gmproto.Settings)
	frozenCard := proto.Clone(ticket.selectedCard).(*gmproto.SIMCard)
	link.settings.SIMCards[0].SIMData.SIMPayload.SIMNumber++
	if link.nativeTicketCurrentLocked(ticket, result.TicketID) {
		link.mu.Unlock()
		t.Fatal("mutated selected SIM card remained current")
	}
	link.settings.SIMCards[0].SIMData.SIMPayload.SIMNumber--
	if !proto.Equal(ticket.settings, frozenSettings) || !proto.Equal(ticket.selectedCard, frozenCard) {
		link.mu.Unlock()
		t.Fatal("current settings mutation changed frozen ticket protobufs")
	}
	link.indexRevision++
	current := link.nativeTicketCurrentLocked(ticket, result.TicketID)
	link.mu.Unlock()
	if current {
		t.Fatal("index revision drift remained current")
	}
}

func TestNativeSendRevokeDoesNotWaitForRPCOrHitSuccessor(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	var first nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &first); err != nil {
		t.Fatal(err)
	}
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	done := make(chan string, 1)
	go func() { done <- link.RevokeNativeSend(first.TicketID) }()
	select {
	case result := <-done:
		if got := stage2Code(t, result); got != "revoked" {
			t.Fatalf("revoke while rpcMu held = %q", got)
		}
	case <-time.After(time.Second):
		t.Fatal("revoke waited for rpcMu")
	}
	var successor nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &successor); err != nil || successor.Code != "prepared" {
		t.Fatal(err)
	}
	if got := stage2Code(t, link.RevokeNativeSend(first.TicketID)); got != "none" {
		t.Fatalf("stale revoker against successor = %q", got)
	}
	link.mu.Lock()
	survives := link.nativeTicket != nil && link.nativeTicket.id == successor.TicketID
	link.mu.Unlock()
	if !survives {
		t.Fatal("stale revoker cleared successor ticket")
	}
}

func TestNativeSendPrepareResponsePreservesEscapedSnapshotAndLargeCounters(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	const large = uint64(1<<53 + 1)
	link.mu.Lock()
	link.generation, link.runGeneration, link.streamEpoch, link.indexRevision = large, large, large+1, large+2
	link.conversationIndexGeneration, link.conversationIndexStreamEpoch = large, large+1
	link.settingsStreamEpoch = large + 1
	link.ownerVerification.generation = large
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = "quote\" slash\\ snow 雪"
	link.mu.Unlock()
	var raw map[string]json.RawMessage
	resultJSON := link.PrepareNativeRcs(request)
	if err := json.Unmarshal([]byte(resultJSON), &raw); err != nil {
		t.Fatal(err)
	}
	want := map[string]bool{"code": true, "ticket_id": true, "candidate_snapshot": true, "run_incarnation": true, "run_generation": true, "stream_epoch": true, "pairing_evidence_id": true}
	if len(raw) != len(want) {
		t.Fatalf("response keys=%v", raw)
	}
	for key := range want {
		if _, ok := raw[key]; !ok {
			t.Fatalf("missing response key %q", key)
		}
	}
	var result nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(resultJSON), &result); err != nil || result.RunGeneration != large || result.StreamEpoch != large+1 {
		t.Fatalf("result=%+v err=%v", result, err)
	}
	link.mu.Lock()
	retained := string(link.nativeTicket.candidate)
	link.mu.Unlock()
	if retained != result.CandidateSnapshot || !strings.Contains(retained, `雪`) || !strings.Contains(retained, `\\`) {
		t.Fatal("snapshot bytes were not preserved through outer JSON encoding")
	}
}

func TestNativeSendPrepareSnapshotInnerLimit(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	link.mu.Lock()
	number := link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = "x"
	base := len(link.conversationCandidateSnapshotLocked()) - 1
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = strings.Repeat("x", maxNativeCandidateBytes-base)
	link.mu.Unlock()
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "prepared" {
		t.Fatalf("exact inner cap code=%q", got)
	}
	link.mu.Lock()
	link.nativeTicket = nil
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = strings.Repeat("x", maxNativeCandidateBytes-base+1)
	link.mu.Unlock()
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "rejected" {
		t.Fatalf("over-cap code=%q", got)
	}
	link.mu.Lock()
	installed := link.nativeTicket
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = number
	link.mu.Unlock()
	if installed != nil {
		t.Fatal("over-cap prepare installed a ticket")
	}
}

func TestNativeSendPrepareRejectsOldSettingsEvent(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	link.recordSettings(&libgm.WrappedSettings{IsOld: true})
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "rejected" {
		t.Fatalf("old settings event code=%q", got)
	}
}

func TestNativeSendPrepareInvalidAuthorityLeavesNoTicket(t *testing.T) {
	invalid := []string{
		`{"command_id":"00000000-0000-4000-8000-000000000000","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"","android_subscription_id":1,"own_number_raw":"+12025550101","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":1}}`,
		`{"command_id":"00000000-0000-4000-8000-000000000000","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"route","android_subscription_id":1,"own_number_raw":"","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":1}}`,
		`{"command_id":"00000000-0000-4000-8000-000000000000","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"route","android_subscription_id":-1,"own_number_raw":"+12025550101","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":1}}`,
		`{"command_id":"00000000-0000-4000-8000-000000000000","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"route","android_subscription_id":1,"own_number_raw":"+12025550101","own_number_source":"Other","route_observed_at_ms":1}}`,
		`{"command_id":"00000000-0000-4000-8000-000000000000","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"route","android_subscription_id":1,"own_number_raw":"+12025550101","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":0}}`,
	}
	for _, request := range invalid {
		link, cleanup, _ := nativePrepareFixture(t)
		if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "invalid_request" {
			t.Fatalf("invalid authority code=%q", got)
		}
		link.mu.Lock()
		installed := link.nativeTicket
		link.mu.Unlock()
		cleanup()
		if installed != nil {
			t.Fatal("invalid authority installed native ticket")
		}
	}
	for _, field := range []string{"run", "pairing"} {
		t.Run(field, func(t *testing.T) {
			link, cleanup, request := nativePrepareFixture(t)
			defer cleanup()
			link.mu.Lock()
			if field == "run" {
				link.runIncarnationID = "not-a-uuid"
			} else {
				link.pairingEvidenceID = "not-a-uuid"
			}
			link.mu.Unlock()
			if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "rejected" {
				t.Fatalf("malformed %s identity code=%q", field, got)
			}
			link.mu.Lock()
			installed := link.nativeTicket
			link.mu.Unlock()
			if installed != nil {
				t.Fatal("malformed runtime identity installed native ticket")
			}
		})
	}
}

func TestNativeSendPrepareEscapingHeavySnapshotUsesOuterBudget(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	const targetInner = 7 * 1024 * 1024
	link.mu.Lock()
	number := link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = "\x00"
	base := len(link.conversationCandidateSnapshotLocked()) - len(`\u0000`)
	nuls := (targetInner - base) / len(`\u0000`)
	remainder := targetInner - base - nuls*len(`\u0000`)
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = strings.Repeat("\x00", nuls) + strings.Repeat("x", remainder)
	inner := link.conversationCandidateSnapshotLocked()
	link.mu.Unlock()
	if len(inner) >= maxNativeCandidateBytes || len(inner) <= 6*1024*1024 {
		t.Fatalf("inner escaping fixture bytes=%d", len(inner))
	}
	resultJSON := link.PrepareNativeRcs(request)
	if len(resultJSON) <= maxNativeCandidateBytes || len(resultJSON) > maxNativePrepareResultBytes {
		t.Fatalf("outer response bytes=%d", len(resultJSON))
	}
	var result nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(resultJSON), &result); err != nil || result.Code != "prepared" || result.CandidateSnapshot != inner {
		t.Fatalf("escaping result=%+v err=%v", result, err)
	}
	link.mu.Lock()
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = number
	link.mu.Unlock()
}

func TestNativeSendCurrentnessRejectsRealSettingsIndexAndLifecycleEvents(t *testing.T) {
	tests := []struct {
		name   string
		mutate func(*Link)
	}{
		{"settings_event", func(link *Link) {
			settings := proto.Clone(link.settings).(*gmproto.Settings)
			link.recordSettings(&libgm.WrappedSettings{Settings: settings})
		}},
		{"index_event", func(link *Link) {
			link.mu.Lock()
			link.applyConversationsLocked([]*gmproto.Conversation{cloneConversation(link.conversationRecords["synthetic-conversation"])})
			link.mu.Unlock()
		}},
		{"stop", func(link *Link) { link.Stop() }},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			link, cleanup, request := nativePrepareFixture(t)
			defer cleanup()
			var result nativeRcsPrepareResult
			if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &result); err != nil {
				t.Fatal(err)
			}
			tc.mutate(link)
			link.mu.Lock()
			current := link.nativeTicketCurrentLocked(link.nativeTicket, result.TicketID)
			link.mu.Unlock()
			if current {
				t.Fatalf("ticket remained current after %s", tc.name)
			}
		})
	}
}

func TestNativeSendCurrentnessRejectsContinuityLoss(t *testing.T) {
	link, cleanup, request := nativePrepareFixture(t)
	defer cleanup()
	var result nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &result); err != nil {
		t.Fatal(err)
	}
	link.mu.Lock()
	generation, source := link.generation, link.streamSource
	link.mu.Unlock()
	link.queueEventFor(generation, &libgm.ContinuityLost{Source: source})
	link.mu.Lock()
	current := link.nativeTicketCurrentLocked(link.nativeTicket, result.TicketID)
	link.mu.Unlock()
	if current {
		t.Fatal("ticket remained current after continuity loss")
	}
	if got := stage2Code(t, link.RevokeNativeSend(result.TicketID)); got != "revoked" {
		t.Fatalf("revoke invalidated ticket=%q", got)
	}
	if got := stage2Code(t, link.PrepareNativeRcs(request)); got != "rejected" {
		t.Fatalf("prepare after continuity loss=%q", got)
	}
}
