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
	if groupPath := os.Getenv("PEPPY_RCS_GO_GROUP_RECEIVE_RECORDS_PATH"); groupPath != "" {
		exportGroupReceiveRecords(t, groupPath)
	}
}

// exportGroupReceiveRecords writes a JSON array of real group receive projections, one per sender shape:
// incoming with a visible sender_address, incoming with sender_unknown, and outgoing self_sent.
func exportGroupReceiveRecords(t *testing.T, path string) {
	t.Helper()
	group := &gmproto.Conversation{
		ConversationID: "synthetic-group", Name: "Weekend", IsGroupChat: true,
		Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		Participants: []*gmproto.Participant{
			{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "self", Number: "+12025550101"}},
			{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-a", Number: "+12025550102"}},
			{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-b", Number: "+12025550103"}},
		},
	}
	newMessage := func(id string, status gmproto.MessageStatusType) *gmproto.Message {
		return &gmproto.Message{
			MessageID: id, ConversationID: "synthetic-group", TmpID: "synthetic-tmp-" + id,
			Timestamp: 1_700_000_000_000_000, Type: rcsMessageType,
			MessageStatus: &gmproto.MessageStatus{Status: status},
			MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "synthetic group"}}}},
		}
	}
	fromMember := newMessage("synthetic-group-incoming", gmproto.MessageStatusType_INCOMING_COMPLETE)
	fromMember.SenderParticipant = &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-a", Number: "+12025550102"}}
	fromStranger := newMessage("synthetic-group-unknown", gmproto.MessageStatusType_INCOMING_COMPLETE)
	fromStranger.ParticipantID = "synthetic-stranger"
	selfSent := newMessage("synthetic-group-self", gmproto.MessageStatusType_OUTGOING_DELIVERED)
	projections := make([]rcsProjection, 0, 3)
	for _, message := range []*gmproto.Message{fromMember, fromStranger, selfSent} {
		projection := projectRcsMessage(&libgm.WrappedMessage{Message: message}, group)
		if projection.Code != "ok" || projection.Record == nil {
			t.Fatalf("group receive projection = %#v", projection)
		}
		projections = append(projections, projection)
	}
	if projections[0].Record.SenderAddress != "+12025550102" || !projections[1].Record.SenderUnknown || !projections[2].Record.SelfSent {
		t.Fatalf("group receive attribution = %#v", projections)
	}
	fixture, err := json.Marshal(projections)
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
	if result := link.RefreshConversationIndex(); result != `{"code":"ok"}` {
		t.Fatal(result)
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
	if phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("fixture generation sent a message")
	}
	cleanup()
	groupLink, _, groupCleanup, groupTicket, groupCommit := nativeGroupDispatchFixture(t)
	defer groupCleanup()
	groupLink.mu.Lock()
	groupPrepare := map[string]any{
		"command_id":             groupLink.nativeTicket.request.CommandID,
		"canonicalizer_revision": groupLink.nativeTicket.request.CanonicalizerRevision,
		"identity_region":        groupLink.nativeTicket.request.IdentityRegion,
		"expected_member_digest": groupLink.nativeTicket.request.ExpectedMemberDigest,
		"carrier_binding": map[string]any{
			"carrier_route_id":        groupLink.nativeTicket.request.CarrierBinding.CarrierRouteID,
			"android_subscription_id": groupLink.nativeTicket.request.CarrierBinding.AndroidSubscriptionID,
			"own_number_raw":          groupLink.nativeTicket.request.CarrierBinding.OwnNumberRaw,
			"own_number_source":       groupLink.nativeTicket.request.CarrierBinding.OwnNumberSource,
			"route_observed_at_ms":    groupLink.nativeTicket.request.CarrierBinding.RouteObservedAtMS,
		},
	}
	groupLink.mu.Unlock()
	if _, ok := decodeNativeRcsPrepare(mustJSON(t, groupPrepare)); !ok {
		t.Fatal("group prepare export does not decode")
	}
	if _, ok := decodeNativeRcsCommit(nativeCommitJSON(t, groupCommit)); !ok {
		t.Fatal("group commit export does not decode")
	}
	fixture, err := json.Marshal(struct {
		Route        string         `json:"route"`
		Target       string         `json:"target"`
		Snapshot     string         `json:"snapshot"`
		NowMS        int64          `json:"now_ms"`
		GroupTicket  string         `json:"group_ticket"`
		GroupPrepare map[string]any `json:"group_prepare"`
		GroupCommit  map[string]any `json:"group_commit"`
	}{route.Observation, target.Observation, prepared.CandidateSnapshot, time.Now().UnixMilli(), groupTicket, groupPrepare, groupCommit})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, fixture, 0600); err != nil {
		t.Fatal(err)
	}
}

func mustJSON(t *testing.T, value any) string {
	t.Helper()
	encoded, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return string(encoded)
}
