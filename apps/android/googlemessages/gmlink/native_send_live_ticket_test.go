//go:build rcs_native_contract

package gmlink

import (
	"bufio"
	"encoding/json"
	"net"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

const nativeLiveTicketFrameCap = 8 * 1024 * 1024

type nativeLiveTicketFrame struct {
	CommandID   string `json:"command_id,omitempty"`
	Route       string `json:"route,omitempty"`
	Target      string `json:"target,omitempty"`
	Snapshot    string `json:"snapshot,omitempty"`
	NowMS       int64  `json:"now_ms,omitempty"`
	Facts       string `json:"facts,omitempty"`
	Observation string `json:"observation,omitempty"`
	Commit      string `json:"commit,omitempty"`
	Summary     string `json:"summary,omitempty"`
}

func readNativeLiveTicketFrame(t *testing.T, scanner *bufio.Scanner, maxBytes int) nativeLiveTicketFrame {
	t.Helper()
	if !scanner.Scan() {
		t.Fatalf("read live ticket frame: %v", scanner.Err())
	}
	if len(scanner.Bytes()) > maxBytes {
		t.Fatal("live ticket frame exceeds cap")
	}
	var frame nativeLiveTicketFrame
	if err := json.Unmarshal(scanner.Bytes(), &frame); err != nil {
		t.Fatalf("decode live ticket frame: %v", err)
	}
	return frame
}

func writeNativeLiveTicketFrame(t *testing.T, encoder *json.Encoder, frame nativeLiveTicketFrame) {
	t.Helper()
	if err := encoder.Encode(frame); err != nil {
		t.Fatalf("write live ticket frame: %v", err)
	}
}

func TestNativeRcsLiveCoreRoundTrip(t *testing.T) {
	address := os.Getenv("PEPPY_RCS_NATIVE_LIVE_ADDR")
	if address == "" {
		t.Fatal("PEPPY_RCS_NATIVE_LIVE_ADDR is required for rcs_native_contract")
	}
	connection, err := net.DialTimeout("tcp", address, 15*time.Second)
	if err != nil {
		t.Fatalf("connect Rust live ticket server: %v", err)
	}
	defer connection.Close()
	if err := connection.SetDeadline(time.Now().Add(15 * time.Second)); err != nil {
		t.Fatal(err)
	}
	scanner := bufio.NewScanner(connection)
	scanner.Buffer(make([]byte, 0, 64*1024), nativeLiveTicketFrameCap)
	encoder := json.NewEncoder(connection)
	command := readNativeLiveTicketFrame(t, scanner, nativeLiveTicketFrameCap)
	if !validNativeUUID(command.CommandID) {
		t.Fatal("Rust did not provide a command id")
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
	if result := link.RefreshConversationIndex(); result != `{"code":"ok"}` {
		t.Fatal(result)
	}
	now := time.Now().UnixMilli()
	request, err := json.Marshal(map[string]any{
		"command_id": command.CommandID, "canonicalizer_revision": 1, "identity_region": "US",
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1,
			"own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now},
	})
	if err != nil {
		t.Fatal(err)
	}
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(request))), &prepared); err != nil || prepared.Code != "prepared" {
		t.Fatalf("prepare failed: %v", err)
	}
	var route, target nativeRcsObservationResult
	if err := json.Unmarshal([]byte(link.BindNativeRcsRoute(prepared.TicketID, 11, 17, 23, 29, now)), &route); err != nil || route.Code != "route_bound" {
		t.Fatalf("route bind failed: %v", err)
	}
	if err := json.Unmarshal([]byte(link.BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))), &target); err != nil || target.Code != "target_bound" {
		t.Fatalf("target bind failed: %v", err)
	}
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{
		Route: route.Observation, Target: target.Observation, Snapshot: prepared.CandidateSnapshot, NowMS: time.Now().UnixMilli(),
	})

	facts := readNativeLiveTicketFrame(t, scanner, maxNativeRcsFactsJSONBytes+1024)
	if facts.Facts == "" || stage2Code(t, link.BindNativeRcsFacts(prepared.TicketID, facts.Facts)) != "facts_bound" {
		t.Fatal("Rust facts were not bound to the live ticket")
	}
	link.mu.Lock()
	observation := link.nativeTicket.targetObservationJSON
	link.mu.Unlock()
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Observation: observation, NowMS: time.Now().UnixMilli()})

	commitFrame := readNativeLiveTicketFrame(t, scanner, maxNativeRcsCommitJSONBytes+1024)
	commit, valid := decodeNativeRcsCommit(commitFrame.Commit)
	if !valid {
		t.Fatal("Rust commit was rejected before dispatch")
	}
	if stage2Code(t, committer(t, link).CommitNativeRcs(prepared.TicketID, commitFrame.Commit)) != "accepted" {
		t.Fatal("live Rust commit was not accepted")
	}
	phone.mu.Lock()
	requests := append([]*gmproto.SendMessageRequest(nil), phone.sendRequests...)
	phone.mu.Unlock()
	if phone.countAction("GET_CONVERSATION") != 2 || phone.countAction("SEND_MESSAGE") != 1 || len(requests) != 1 ||
		requests[0].GetTmpID() != commit.TmpID || requests[0].GetMessagePayload().GetMessageInfo()[0].GetMessageContent().GetContent() != commit.MessageBody {
		t.Fatal("live ticket did not dispatch the exact Rust-owned body and tmp id once")
	}
	if stage2Code(t, committer(t, link).CommitNativeRcs(prepared.TicketID, commitFrame.Commit)) != "rejected" || phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("live ticket replay dispatched")
	}
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Summary: "accepted"})
}
