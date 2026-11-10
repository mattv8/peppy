package gmlink

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

type nativeCommitter interface{ CommitNativeRcs(string, string) string }

func committer(t *testing.T, link *Link) nativeCommitter {
	t.Helper()
	operation, ok := any(link).(nativeCommitter)
	if !ok {
		t.Fatal("native commit is not implemented")
	}
	return operation
}

func nativeGroupDispatchFixture(t *testing.T) (*Link, *phoneFixture, func(), string, map[string]any) {
	t.Helper()
	link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	group := &gmproto.Conversation{ConversationID: "synthetic-conversation", IsGroupChat: true, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing", Participants: []*gmproto.Participant{
			{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "self", Number: "+12025550101"}},
			{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-a", Number: "+12025550102"}},
			{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-b", Number: "+12025550103"}},
		}}
	phone.mu.Lock()
	phone.conversations = []*gmproto.Conversation{group}
	phone.mu.Unlock()
	if result := link.RefreshConversationIndex(); result != `{"code":"ok"}` {
		cleanup()
		t.Fatal(result)
	}
	now := time.Now().UnixMilli()
	link.mu.Lock()
	link.pairingEvidenceID = uuid.NewString()
	link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = "+12025550101"
	link.mu.Unlock()
	prepare, _ := json.Marshal(map[string]any{"command_id": uuid.NewString(), "canonicalizer_revision": 1, "identity_region": "US", "expected_member_digest": groupMemberDigest(group), "carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1, "own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now}})
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(prepare))), &prepared); err != nil || prepared.Code != "prepared" {
		cleanup()
		t.Fatal("prepare failed")
	}
	routeResult := stage2Code(t, link.BindNativeRcsRoute(prepared.TicketID, 11, 17, 23, 29, now))
	targetResult := ""
	if routeResult == "route_bound" {
		targetResult = stage2Code(t, link.BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared)))
	}
	if routeResult != "route_bound" || targetResult != "target_bound" {
		cleanup()
		t.Fatalf("group target bind failed route=%s target=%s", routeResult, targetResult)
	}
	facts := nativeFactsFixture(t, link)
	facts["stored_mapping"].(map[string]any)["class"] = "Group"
	encodedFacts, _ := json.Marshal(facts)
	if stage2Code(t, link.BindNativeRcsFacts(prepared.TicketID, string(encodedFacts))) != "facts_bound" {
		cleanup()
		t.Fatal("group facts bind failed")
	}
	link.mu.Lock()
	observationJSON := link.nativeTicket.targetObservationJSON
	commandID := link.nativeTicket.request.CommandID
	digest := link.nativeTicket.request.ExpectedMemberDigest
	link.mu.Unlock()
	var observation map[string]json.RawMessage
	if json.Unmarshal([]byte(observationJSON), &observation) != nil {
		cleanup()
		t.Fatal("observation decode failed")
	}
	mapping := facts["stored_mapping"].(map[string]any)
	proof := mapping["proof"].(map[string]any)
	target := map[string]any{"proof_version": 2, "peppy_conversation_id": mapping["conversation_id"], "class": "Group", "canonical_recipient": proof["canonical_participant"], "canonical_own_identity": proof["own_identity"], "evidence": observation["native_evidence"]}
	for _, field := range []string{"google_conversation_id", "conversation_type", "status", "send_mode", "read_only", "default_outgoing", "participants", "identity_region", "canonicalizer_revision", "selected_sim_id", "carrier_binding", "outgoing"} {
		target[field] = observation[field]
	}
	return link, phone, cleanup, prepared.TicketID, map[string]any{"command_id": commandID, "tmp_id": "opaque-group-tmp", "mapping_revision": 29, "proof_version": 2, "proof_digest": proof["digest"], "message_body": "group body", "expected_member_digest": digest, "target": target}
}

func nativeDispatchFixture(t *testing.T) (*Link, *phoneFixture, func(), string, map[string]any) {
	t.Helper()
	link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
	bindNativeFixtureTarget(t, link, prepared)
	facts := nativeFactsFixture(t, link)
	encoded, err := json.Marshal(facts)
	if err != nil {
		t.Fatal(err)
	}
	if stage2Code(t, link.BindNativeRcsFacts(prepared.TicketID, string(encoded))) != "facts_bound" {
		t.Fatal("facts bind failed")
	}
	link.mu.Lock()
	observationJSON := link.nativeTicket.targetObservationJSON
	commandID := link.nativeTicket.request.CommandID
	link.mu.Unlock()
	var observation map[string]json.RawMessage
	if err := json.Unmarshal([]byte(observationJSON), &observation); err != nil {
		t.Fatal(err)
	}
	mapping := facts["stored_mapping"].(map[string]any)
	proof := mapping["proof"].(map[string]any)
	target := map[string]any{
		"proof_version": 2, "peppy_conversation_id": mapping["conversation_id"], "class": "OneToOne",
		"canonical_recipient": proof["canonical_participant"], "canonical_own_identity": proof["own_identity"],
		"evidence": observation["native_evidence"],
	}
	for _, field := range []string{"google_conversation_id", "conversation_type", "status", "send_mode", "read_only", "default_outgoing", "participants", "identity_region", "canonicalizer_revision", "selected_sim_id", "carrier_binding", "outgoing"} {
		target[field] = observation[field]
	}
	// Schema-derived input; the separate tagged integration check uses real Rust bytes.
	commit := map[string]any{"command_id": commandID, "tmp_id": "opaque-test-tmp", "mapping_revision": 29, "proof_version": 2,
		"proof_digest": proof["digest"], "message_body": "core-issued body", "target": target}
	return link, phone, cleanup, prepared.TicketID, commit
}

func nativeCommitJSON(t *testing.T, commit map[string]any) string {
	t.Helper()
	encoded, err := json.Marshal(commit)
	if err != nil {
		t.Fatal(err)
	}
	return string(encoded)
}

func TestNativeSendCommitFetchesAgainAndDispatchesExactlyOnce(t *testing.T) {
	link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
	defer cleanup()
	operation := committer(t, link)
	if got := stage2Code(t, operation.CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "accepted" {
		t.Fatalf("commit = %s", got)
	}
	if phone.countAction("GET_CONVERSATION") != 2 || phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("commit did not separately fetch and send exactly once")
	}
	if stage2Code(t, operation.CommitNativeRcs(ticket, nativeCommitJSON(t, commit))) != "rejected" {
		t.Fatal("ticket replay accepted")
	}
	if phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("ticket replay dispatched")
	}
}

func TestNativeGroupSendChecksLiveMembersBeforeGoogleCall(t *testing.T) {
	t.Run("happy", func(t *testing.T) {
		link, phone, cleanup, ticket, commit := nativeGroupDispatchFixture(t)
		defer cleanup()
		if got := stage2Code(t, link.CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "accepted" || phone.countAction("SEND_MESSAGE") != 1 {
			t.Fatalf("result=%s sends=%d", got, phone.countAction("SEND_MESSAGE"))
		}
	})
	t.Run("mismatch", func(t *testing.T) {
		link, phone, cleanup, ticket, commit := nativeGroupDispatchFixture(t)
		defer cleanup()
		phone.mu.Lock()
		phone.conversations[0].Participants = append(phone.conversations[0].Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-c", Number: "+12025550104"}})
		phone.mu.Unlock()
		if got := stage2Code(t, link.CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "members_changed" || phone.countAction("SEND_MESSAGE") != 0 {
			t.Fatalf("result=%s sends=%d", got, phone.countAction("SEND_MESSAGE"))
		}
	})
}

func TestNativeSendCommitRejectsChangedProjectionBeforeTransport(t *testing.T) {
	for _, field := range []string{"command_id", "mapping_revision", "proof_digest", "google_conversation_id", "canonical_recipient", "carrier_binding", "evidence"} {
		t.Run(field, func(t *testing.T) {
			link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
			defer cleanup()
			target := commit["target"].(map[string]any)
			switch field {
			case "command_id":
				commit[field] = "different-command"
			case "mapping_revision":
				commit[field] = 30
			case "proof_digest":
				digest := make([]int, 32)
				digest[0] = 8
				commit[field] = digest
			case "google_conversation_id", "canonical_recipient":
				target[field] = "different-target"
			case "carrier_binding", "evidence":
				var nested map[string]any
				if err := json.Unmarshal(target[field].(json.RawMessage), &nested); err != nil {
					t.Fatal(err)
				}
				if field == "carrier_binding" {
					nested["own_number_raw"] = "+12025550199"
				} else {
					nested["settings_revision"] = 99
				}
				target[field] = nested
			}
			if stage2Code(t, committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit))) != "rejected" {
				t.Fatal("changed projection accepted")
			}
			if phone.countAction("GET_CONVERSATION") != 1 || phone.countAction("SEND_MESSAGE") != 0 {
				t.Fatal("invalid projection reached transport")
			}
		})
	}
}

func TestNativeSendCommitUncertainTransportDoesNotRetry(t *testing.T) {
	link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
	defer cleanup()
	phone.configure(func(knobs *fixtureKnobs) { knobs.failAction = gmproto.ActionType_SEND_MESSAGE; knobs.failStatus = 500 })
	operation := committer(t, link)
	if stage2Code(t, operation.CommitNativeRcs(ticket, nativeCommitJSON(t, commit))) != "outcome_unknown" {
		t.Fatal("uncertain dispatch was not unknown")
	}
	if stage2Code(t, operation.CommitNativeRcs(ticket, nativeCommitJSON(t, commit))) != "rejected" {
		t.Fatal("uncertain ticket replayed")
	}
	if phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("uncertain dispatch retried")
	}
}
