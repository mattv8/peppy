//go:build rcs_native_contract

package gmlink

import (
	"bufio"
	"bytes"
	"encoding/json"
	"net"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	nativeGroupLiveGoogleID = "synthetic-conversation"
	nativeGroupLiveSelf     = "+12025550101"
	nativeGroupLiveMemberA  = "+12025550102"
	nativeGroupLiveMemberB  = "+12025550103"
	nativeGroupLiveMemberC  = "+12025550104"
	// The Rust fixture writes the roster once (revision 1), and the link proof keeps that revision.
	// The route must carry the proved revision, not a placeholder (C4b Android follow-up).
	nativeGroupLiveProvedRevision = 1
)

// nativeGroupLiveConversation is a fake RCS group with one self and two visible non-self members.
// Participant types are phone numbers, as the group proof requires.
func nativeGroupLiveConversation() *gmproto.Conversation {
	participant := func(id, number string, isMe bool) *gmproto.Participant {
		return &gmproto.Participant{IsMe: isMe, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: id, Number: number, Type: 1}}
	}
	return &gmproto.Conversation{
		ConversationID: nativeGroupLiveGoogleID, IsGroupChat: true, Name: "Weekend",
		Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing",
		LastMessageTimestamp: time.Now().UnixMicro(),
		Participants: []*gmproto.Participant{
			participant("self", nativeGroupLiveSelf, true),
			participant("member-a", nativeGroupLiveMemberA, false),
			participant("member-b", nativeGroupLiveMemberB, false),
		},
	}
}

// nativeGroupLiveTicket runs Go's half of the group prepare: prepare with Rust's command id and
// the Go member digest, then bind route and target. The phone fixture is returned for roster edits.
func nativeGroupLiveTicket(t *testing.T, commandID string) (*Link, *phoneFixture, func(), nativeRcsPrepareResult, nativeRcsObservationResult, nativeRcsObservationResult) {
	t.Helper()
	link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	group := nativeGroupLiveConversation()
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
	link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = nativeGroupLiveSelf
	link.mu.Unlock()
	request, err := json.Marshal(map[string]any{
		"command_id": commandID, "canonicalizer_revision": 1, "identity_region": "US",
		"expected_member_digest": groupMemberDigest(group),
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1,
			"own_number_raw": nativeGroupLiveSelf, "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now},
	})
	if err != nil {
		cleanup()
		t.Fatal(err)
	}
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(request))), &prepared); err != nil || prepared.Code != "prepared" {
		cleanup()
		t.Fatalf("group prepare failed: %v", err)
	}
	var route, target nativeRcsObservationResult
	if err := json.Unmarshal([]byte(link.BindNativeRcsRoute(prepared.TicketID, 11, 17, 23, nativeGroupLiveProvedRevision, now)), &route); err != nil || route.Code != "route_bound" {
		cleanup()
		t.Fatalf("group route bind failed: %v", err)
	}
	if err := json.Unmarshal([]byte(link.BindNativeRcsTarget(prepared.TicketID, nativeGroupLiveGoogleID, preparedSnapshotToken(t, prepared))), &target); err != nil || target.Code != "target_bound" {
		cleanup()
		t.Fatalf("group target bind failed: %v", err)
	}
	return link, phone, cleanup, prepared, route, target
}

func dialNativeGroupLiveTicket(t *testing.T) (*bufio.Scanner, *json.Encoder, func()) {
	t.Helper()
	address := os.Getenv("PEPPY_RCS_NATIVE_LIVE_ADDR")
	if address == "" {
		t.Fatal("PEPPY_RCS_NATIVE_LIVE_ADDR is required for rcs_native_contract")
	}
	connection, err := net.DialTimeout("tcp", address, 15*time.Second)
	if err != nil {
		t.Fatalf("connect Rust live group ticket server: %v", err)
	}
	if err := connection.SetDeadline(time.Now().Add(15 * time.Second)); err != nil {
		connection.Close()
		t.Fatal(err)
	}
	scanner := bufio.NewScanner(connection)
	scanner.Buffer(make([]byte, 0, 64*1024), nativeLiveTicketFrameCap)
	return scanner, json.NewEncoder(connection), func() { connection.Close() }
}

// TestNativeRcsLiveGroupCoreRoundTrip sends a Go-bound group ticket through Rust's permit and
// expects exactly one Google dispatch of the Rust-owned body and tmp id.
func TestNativeRcsLiveGroupCoreRoundTrip(t *testing.T) {
	scanner, encoder, closeConnection := dialNativeGroupLiveTicket(t)
	defer closeConnection()
	command := readNativeLiveTicketFrame(t, scanner, nativeLiveTicketFrameCap)
	if !validNativeUUID(command.CommandID) {
		t.Fatal("Rust did not provide a command id")
	}
	link, phone, cleanup, prepared, route, target := nativeGroupLiveTicket(t, command.CommandID)
	defer cleanup()
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{
		Route: route.Observation, Target: target.Observation, Snapshot: prepared.CandidateSnapshot, NowMS: time.Now().UnixMilli(),
	})

	facts := readNativeLiveTicketFrame(t, scanner, maxNativeRcsFactsJSONBytes+1024)
	if facts.Facts == "" || stage2Code(t, link.BindNativeRcsFacts(prepared.TicketID, facts.Facts)) != "facts_bound" {
		t.Fatal("Rust group facts were not bound to the live ticket")
	}
	link.mu.Lock()
	observation := link.nativeTicket.targetObservationJSON
	link.mu.Unlock()
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Observation: observation, NowMS: time.Now().UnixMilli()})

	commitFrame := readNativeLiveTicketFrame(t, scanner, maxNativeRcsCommitJSONBytes+1024)
	commit, valid := decodeNativeRcsCommit(commitFrame.Commit)
	if !valid || commit.Target.Class != "Group" {
		t.Fatal("Rust group commit was rejected before dispatch")
	}
	if stage2Code(t, link.CommitNativeRcs(prepared.TicketID, commitFrame.Commit)) != "accepted" {
		t.Fatal("live Rust group commit was not accepted")
	}
	phone.mu.Lock()
	requests := append([]*gmproto.SendMessageRequest(nil), phone.sendRequests...)
	phone.mu.Unlock()
	if len(requests) != 1 || requests[0].GetTmpID() != commit.TmpID ||
		requests[0].GetMessagePayload().GetMessageInfo()[0].GetMessageContent().GetContent() != commit.MessageBody {
		t.Fatal("live group ticket did not dispatch the exact Rust-owned body and tmp id once")
	}
	if stage2Code(t, link.CommitNativeRcs(prepared.TicketID, commitFrame.Commit)) != "rejected" || phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("live group ticket replay dispatched")
	}
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Summary: "accepted"})
}

func TestNativeRcsLiveGroupMediaCoreRoundTrip(t *testing.T) {
	scanner, encoder, closeConnection := dialNativeGroupLiveTicket(t)
	defer closeConnection()
	command := readNativeLiveTicketFrame(t, scanner, nativeLiveTicketFrameCap)
	if !validNativeUUID(command.CommandID) {
		t.Fatal("Rust did not provide a command id")
	}
	link, phone, cleanup, prepared, route, target := nativeGroupLiveTicket(t, command.CommandID)
	defer cleanup()
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Route: route.Observation, Target: target.Observation, Snapshot: prepared.CandidateSnapshot, NowMS: time.Now().UnixMilli()})

	facts := readNativeLiveTicketFrame(t, scanner, maxNativeRcsFactsJSONBytes+1024)
	if facts.Facts == "" || stage2Code(t, link.BindNativeRcsFacts(prepared.TicketID, facts.Facts)) != "facts_bound" {
		t.Fatal("Rust group facts were not bound to the live ticket")
	}
	link.mu.Lock()
	observation := link.nativeTicket.targetObservationJSON
	link.mu.Unlock()
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Observation: observation, NowMS: time.Now().UnixMilli()})

	commitFrame := readNativeLiveTicketFrame(t, scanner, maxNativeRcsCommitJSONBytes+1024)
	commit, valid := decodeNativeRcsCommit(commitFrame.Commit)
	if !valid || commit.Target.Class != "Group" || commit.Media == nil || commit.MessageBody == "" {
		t.Fatal("Rust group media commit was rejected before dispatch")
	}
	if stage2Code(t, link.CommitNativeRcs(prepared.TicketID, commitFrame.Commit)) != "accepted" {
		t.Fatal("live Rust group media commit was not accepted")
	}
	phone.mu.Lock()
	requests := append([]*gmproto.SendMessageRequest(nil), phone.sendRequests...)
	phone.mu.Unlock()
	if len(requests) != 1 || !requests[0].GetForceRCS() {
		t.Fatalf("group media commit dispatched %#v", requests)
	}
	infos := requests[0].GetMessagePayload().GetMessageInfo()
	if len(infos) != 2 || infos[0].GetMessageContent().GetContent() != commit.MessageBody {
		t.Fatalf("group media message infos=%#v", infos)
	}
	media := infos[1].GetMediaContent()
	if media == nil || media.GetMediaID() != commit.Media.MediaID || !bytes.Equal(media.GetDecryptionKey(), commit.Media.Key) || media.GetMimeType() != commit.Media.MIME || media.GetSize() != commit.Media.Size || media.GetMediaName() != commit.Media.Name {
		t.Fatalf("group media request did not preserve Rust media: %#v", requests[0])
	}
	if stage2Code(t, link.CommitNativeRcs(prepared.TicketID, commitFrame.Commit)) != "rejected" || phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("live group media ticket replay dispatched")
	}
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Summary: "accepted"})
}

// TestNativeRcsLiveGroupRosterChangedRoundTrip changes the Google roster after Rust has begun the
// send. Go's commit must refuse with members_changed before any Google dispatch.
func TestNativeRcsLiveGroupRosterChangedRoundTrip(t *testing.T) {
	scanner, encoder, closeConnection := dialNativeGroupLiveTicket(t)
	defer closeConnection()
	command := readNativeLiveTicketFrame(t, scanner, nativeLiveTicketFrameCap)
	if !validNativeUUID(command.CommandID) {
		t.Fatal("Rust did not provide a command id")
	}
	link, phone, cleanup, prepared, route, target := nativeGroupLiveTicket(t, command.CommandID)
	defer cleanup()
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{
		Route: route.Observation, Target: target.Observation, Snapshot: prepared.CandidateSnapshot, NowMS: time.Now().UnixMilli(),
	})

	facts := readNativeLiveTicketFrame(t, scanner, maxNativeRcsFactsJSONBytes+1024)
	if facts.Facts == "" || stage2Code(t, link.BindNativeRcsFacts(prepared.TicketID, facts.Facts)) != "facts_bound" {
		t.Fatal("Rust group facts were not bound to the live ticket")
	}
	link.mu.Lock()
	observation := link.nativeTicket.targetObservationJSON
	link.mu.Unlock()
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Observation: observation, NowMS: time.Now().UnixMilli()})

	// The roster changes after the Rust send is prepared and begun, before Go commits it.
	phone.mu.Lock()
	phone.conversations[0].Participants = append(phone.conversations[0].Participants, &gmproto.Participant{
		IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-c", Number: nativeGroupLiveMemberC, Type: 1},
	})
	phone.mu.Unlock()

	commitFrame := readNativeLiveTicketFrame(t, scanner, maxNativeRcsCommitJSONBytes+1024)
	if _, valid := decodeNativeRcsCommit(commitFrame.Commit); !valid {
		t.Fatal("Rust group commit was rejected before dispatch")
	}
	result := stage2Code(t, link.CommitNativeRcs(prepared.TicketID, commitFrame.Commit))
	if result != "members_changed" || phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatalf("changed group roster result=%s sends=%d", result, phone.countAction("SEND_MESSAGE"))
	}
	writeNativeLiveTicketFrame(t, encoder, nativeLiveTicketFrame{Summary: result})
}
