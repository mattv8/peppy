//go:build rcs_native_contract

package gmlink

import (
	"encoding/json"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

func TestNativeRcsContractRustFixture(t *testing.T) {
	path := os.Getenv("PEPPY_RCS_NATIVE_FIXTURE_PATH")
	if path == "" {
		t.Fatal("PEPPY_RCS_NATIVE_FIXTURE_PATH is required for rcs_native_contract")
	}
	fixture, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read required Rust fixture %q: %v", path, err)
	}
	commit, ok := decodeNativeRcsCommit(string(fixture))
	if !ok {
		t.Fatalf("required Rust fixture %q was rejected", path)
	}
	if commit.CommandID == "" || commit.TmpID == "" || commit.MappingRevision == 0 || commit.ProofVersion != 2 ||
		commit.MessageBody == "" || commit.Target.GoogleConversationID == "" || commit.Target.ProofVersion != 2 {
		t.Fatalf("required Rust fixture %q decoded incomplete commit: %#v", path, commit)
	}
}

// Exports the receive projection emitted by Go for the Rust receive-contract consumer.
func TestNativeRcsExportReceiveRecord(t *testing.T) {
	path := os.Getenv("PEPPY_RCS_GO_RECEIVE_RECORD_PATH")
	if path == "" {
		t.Fatal("PEPPY_RCS_GO_RECEIVE_RECORD_PATH is required")
	}
	projection := projectRcsMessage(&libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "synthetic-message", ConversationID: "synthetic-conversation", TmpID: "synthetic-tmp",
		Timestamp: 1_700_000_000_000_000, Type: rcsMessageType,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_OUTGOING_DELIVERED},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "synthetic"}}}},
	}}, &gmproto.Conversation{ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS})
	if projection.Code != "ok" || projection.Record == nil {
		t.Fatalf("receive projection = %#v", projection)
	}
	fixture, err := json.Marshal(projection)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, fixture, 0600); err != nil {
		t.Fatal(err)
	}
}

// Exports actual Go-produced bytes for the Rust public-boundary contract test.
func TestNativeRcsExportObservationFixture(t *testing.T) {
	path := os.Getenv("PEPPY_RCS_GO_OBSERVATION_PATH")
	if path == "" {
		t.Fatal("PEPPY_RCS_GO_OBSERVATION_PATH is required")
	}
	link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	phone.mu.Lock()
	conversation := proto.Clone(phone.conversations[0]).(*gmproto.Conversation)
	conversation.Participants[0].ID.Number = "+12025550101"
	conversation.Participants[0].ID.Type = 1
	conversation.Participants[1].ID.Number = "+12025550123"
	conversation.Participants[1].ID.Type = 1
	conversation.Participants[1].ID.ParticipantID = "synthetic-peer"
	phone.conversations[0] = conversation
	phone.mu.Unlock()
	link.mu.Lock()
	link.pairingEvidenceID = uuid.NewString()
	link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = "+12025550101"
	link.mu.Unlock()
	if err := link.RefreshConversationIndex(); err != nil {
		t.Fatal(err)
	}
	now := time.Now().UnixMilli()
	request, _ := json.Marshal(map[string]any{
		"command_id": uuid.NewString(), "canonicalizer_revision": 1, "identity_region": "ZZ",
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1,
			"own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now},
	})
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(request))), &prepared); err != nil || prepared.Code != "prepared" {
		t.Fatal("prepare failed")
	}
	var route, target nativeRcsObservationResult
	if err := json.Unmarshal([]byte(link.BindNativeRcsRoute(prepared.TicketID, 11, 17, 23, 29, now)), &route); err != nil || route.Code != "route_bound" {
		t.Fatal("route bind failed")
	}
	if err := json.Unmarshal([]byte(link.BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))), &target); err != nil || target.Code != "target_bound" {
		t.Fatal("target bind failed")
	}
	fixture, err := json.Marshal(struct {
		Route    string `json:"route"`
		Target   string `json:"target"`
		Snapshot string `json:"snapshot"`
		NowMS    int64  `json:"now_ms"`
	}{route.Observation, target.Observation, prepared.CandidateSnapshot, time.Now().UnixMilli()})
	if err != nil {
		t.Fatal(err)
	}
	if phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("fixture generation sent a message")
	}
	if err := os.WriteFile(path, fixture, 0600); err != nil {
		t.Fatal(err)
	}
}
