package gmlink

import (
	"context"
	"encoding/json"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestSendRcsRequiresVerificationBeforeSubmission(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	result := link.SendRcs(`{"command_id":"synthetic-command","tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","mapping_revision":1,"proof_digest":"synthetic-proof","content":"synthetic-content"}`)
	var decoded map[string]any
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	if got := decoded["code"]; got != "pending_prepare" {
		t.Fatalf("code = %v", got)
	}
	if _, exists := decoded["ack_at_ms"]; exists {
		t.Fatalf("verification failure has acknowledgement: %s", result)
	}
}

func preparedRcsFixture(t *testing.T) (*Link, func(), string, string) {
	t.Helper()
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	fixture.mu.Lock()
	fixture.conversations = []*gmproto.Conversation{{
		ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing",
		Participants: []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-outgoing"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	}}
	fixture.mu.Unlock()
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	awaitStreamOpen(t, link)
	setTestOwnerVerification(link)
	link.projectEvent(&libgm.WrappedSettings{Settings: &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-outgoing"}, SIMData: &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}}, RCSChats: &gmproto.RCSChats{Enabled: true}}}}})
	if err := link.RefreshConversationIndex(); err != nil {
		t.Fatal(err)
	}
	var snapshot struct {
		SnapshotToken string `json:"snapshot_token"`
	}
	if err := json.Unmarshal([]byte(link.ConversationCandidateSnapshot()), &snapshot); err != nil {
		t.Fatal(err)
	}
	cleanup := func() {
		link.Stop()
		select {
		case <-done:
		case <-time.After(5 * time.Second):
			t.Fatal("Run did not stop")
		}
		link.Close()
	}
	return link, cleanup, snapshot.SnapshotToken, "00000000-0000-4000-8000-000000000000"
}

func prepareTicket(t *testing.T, link *Link, snapshotToken, tmpID string) string {
	t.Helper()
	result := link.PrepareRcs(`{"tmp_id":"` + tmpID + `","google_conversation_id":"synthetic-conversation","snapshot_token":"` + snapshotToken + `","content":"synthetic-content"}`)
	var prepared struct {
		Code     string `json:"code"`
		TicketID string `json:"ticket_id"`
	}
	if err := json.Unmarshal([]byte(result), &prepared); err != nil || prepared.Code != "prepared" || prepared.TicketID == "" {
		t.Fatalf("prepare = %s err=%v", result, err)
	}
	return prepared.TicketID
}

func commitPermit(tmpID, snapshotToken string) string {
	return `{"command_id":"synthetic-command","tmp_id":"` + tmpID + `","google_conversation_id":"synthetic-conversation","snapshot_token":"` + snapshotToken + `","mapping_revision":1,"proof_digest":"` + strings.Repeat("A", 43) + "=" + `","content":"synthetic-content"}`
}

func TestPreparedRcsTicketIsSingleUseAndCanBeCancelled(t *testing.T) {
	link, cleanup, token, tmpID := preparedRcsFixture(t)
	defer cleanup()
	ticket := prepareTicket(t, link, token, tmpID)
	if got := decodeSendCode(t, link.CancelPreparedRcs(ticket)); got != "cancelled" {
		t.Fatalf("cancel = %s", got)
	}
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("cancelled commit = %s", got)
	}
	ticket = prepareTicket(t, link, token, tmpID)
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "accepted" {
		t.Fatalf("commit = %s", got)
	}
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("replay = %s", got)
	}
}

func TestPreparedRcsTicketRejectsExpiryAndIndexDrift(t *testing.T) {
	link, cleanup, token, tmpID := preparedRcsFixture(t)
	defer cleanup()
	now := time.Now()
	link.mu.Lock()
	link.ticketNow = func() time.Time { return now }
	link.mu.Unlock()
	ticket := prepareTicket(t, link, token, tmpID)
	now = now.Add(preparedRcsTicketTTL)
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("expired = %s", got)
	}
	now = time.Now()
	link.mu.Lock()
	link.ticketNow = func() time.Time { return now }
	link.mu.Unlock()
	ticket = prepareTicket(t, link, token, tmpID)
	link.mu.Lock()
	link.indexRevision++
	link.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("drifted = %s", got)
	}
}

func TestPreparedRcsTicketRejectsStreamGenerationAndTargetDrift(t *testing.T) {
	link, cleanup, token, tmpID := preparedRcsFixture(t)
	ticket := prepareTicket(t, link, token, tmpID)
	link.mu.Lock()
	link.streamEpoch++
	link.clearPreparedTicketLocked()
	link.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("stale stream = %s", got)
	}
	cleanup()

	link, cleanup, token, tmpID = preparedRcsFixture(t)
	ticket = prepareTicket(t, link, token, tmpID)
	link.mu.Lock()
	link.generation++
	link.clearPreparedTicketLocked()
	link.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("stale generation = %s", got)
	}
	cleanup()

	link, cleanup, token, tmpID = preparedRcsFixture(t)
	ticket = prepareTicket(t, link, token, tmpID)
	link.mu.Lock()
	link.applyConversationsLocked([]*gmproto.Conversation{{
		ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing",
		Participants: []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-outgoing"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "changed-target"}}},
	}})
	link.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("target drift = %s", got)
	}
	cleanup()
}

func TestPreparedRcsTicketRetiresAndConcurrentCommitDispatchesOnce(t *testing.T) {
	link, cleanup, token, tmpID := preparedRcsFixture(t)
	ticket := prepareTicket(t, link, token, tmpID)
	link.Stop()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("retired = %s", got)
	}
	cleanup()

	link, cleanup, token, tmpID = preparedRcsFixture(t)
	defer cleanup()
	ticket = prepareTicket(t, link, token, tmpID)
	var sends atomic.Int32
	link.mu.Lock()
	link.sendMessage = func(context.Context, *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
		sends.Add(1)
		return &gmproto.SendMessageResponse{Status: gmproto.SendMessageResponse_SUCCESS}, nil
	}
	link.mu.Unlock()
	var group sync.WaitGroup
	results := make(chan string, 2)
	for range 2 {
		group.Add(1)
		go func() {
			defer group.Done()
			results <- decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token)))
		}()
	}
	group.Wait()
	close(results)
	accepted := 0
	for result := range results {
		if result == "accepted" {
			accepted++
		} else if result != "rejected" {
			t.Fatalf("concurrent result = %s", result)
		}
	}
	if accepted != 1 || sends.Load() != 1 {
		t.Fatalf("accepted=%d sends=%d", accepted, sends.Load())
	}
}

func TestPreparedRcsMapsDeadlineAndLibgmRejection(t *testing.T) {
	link, cleanup, token, tmpID := preparedRcsFixture(t)
	defer cleanup()
	ticket := prepareTicket(t, link, token, tmpID)
	link.mu.Lock()
	link.sendMessage = func(context.Context, *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
		return nil, context.DeadlineExceeded
	}
	link.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "outcome_unknown" {
		t.Fatalf("deadline = %s", got)
	}
	ticket = prepareTicket(t, link, token, tmpID)
	link.mu.Lock()
	link.sendMessage = func(context.Context, *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
		return &gmproto.SendMessageResponse{Status: gmproto.SendMessageResponse_FAILURE_4}, nil
	}
	link.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("libgm rejection = %s", got)
	}
}

func TestPrepareRcsBindsOneTicketToTheCurrentStage2Target(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	fixture.mu.Lock()
	fixture.conversations = []*gmproto.Conversation{{
		ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing",
		Participants: []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-outgoing"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	}}
	fixture.mu.Unlock()
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	awaitStreamOpen(t, link)
	setTestOwnerVerification(link)
	link.projectEvent(&libgm.WrappedSettings{Settings: &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-outgoing"}, SIMData: &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}}, RCSChats: &gmproto.RCSChats{Enabled: true}}}}})
	if err := link.RefreshConversationIndex(); err != nil {
		t.Fatal(err)
	}
	snapshot := link.ConversationCandidateSnapshot()
	var envelope struct {
		SnapshotToken string `json:"snapshot_token"`
	}
	if err := json.Unmarshal([]byte(snapshot), &envelope); err != nil {
		t.Fatal(err)
	}
	result := link.PrepareRcs(`{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","snapshot_token":"` + envelope.SnapshotToken + `","content":"synthetic-content"}`)
	var prepared struct {
		Code     string `json:"code"`
		TicketID string `json:"ticket_id"`
	}
	if err := json.Unmarshal([]byte(result), &prepared); err != nil {
		t.Fatal(err)
	}
	if prepared.Code != "prepared" || prepared.TicketID == "" {
		t.Fatalf("unexpected preparation: %s", result)
	}
	if got := decodeSendCode(t, link.PrepareRcs(`{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","snapshot_token":"`+envelope.SnapshotToken+`","content":"synthetic-content"}`)); got != "busy" {
		t.Fatalf("second prepare = %s", got)
	}
	link.Stop()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not stop")
	}
}

func TestLegacySendRcsDoesNotSubmitWithoutPrepareCommit(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	fixture.mu.Lock()
	fixture.conversations = []*gmproto.Conversation{{
		ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing",
		Participants: []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-outgoing"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	}}
	fixture.mu.Unlock()
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	awaitStreamOpen(t, link)
	setTestOwnerVerification(link)
	link.projectEvent(&libgm.WrappedSettings{Settings: &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-outgoing"}, SIMData: &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}}, RCSChats: &gmproto.RCSChats{Enabled: true}}}}})
	if err := link.RefreshConversationIndex(); err != nil {
		t.Fatal(err)
	}
	result := link.SendRcs(`{"command_id":"synthetic-command","tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","mapping_revision":1,"proof_digest":"synthetic-proof","content":"synthetic-content"}`)
	if got := decodeSendCode(t, result); got != "pending_prepare" {
		t.Fatalf("code = %s result=%s", got, result)
	}
	fixture.mu.Lock()
	requests := append([]*gmproto.SendMessageRequest(nil), fixture.sendRequests...)
	fixture.mu.Unlock()
	if len(requests) != 0 {
		t.Fatalf("request = %+v", requests)
	}
	link.Stop()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not stop")
	}
}

func TestOldSettingsCannotReplaceCurrentSendAuthority(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	fixture.mu.Lock()
	fixture.conversations = []*gmproto.Conversation{{
		ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "current-outgoing",
		Participants: []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "current-outgoing"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	}}
	fixture.mu.Unlock()
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	awaitStreamOpen(t, link)
	setTestOwnerVerification(link)
	current := &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
		SIMParticipant: &gmproto.SIMParticipant{ID: "current-outgoing"},
		SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}},
		RCSChats:       &gmproto.RCSChats{Enabled: true},
	}}}
	old := &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
		SIMParticipant: &gmproto.SIMParticipant{ID: "old-outgoing"},
		SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 2}},
		RCSChats:       &gmproto.RCSChats{Enabled: true},
	}}}
	fixture.queueSettings(current)
	awaitSafety(t, "current settings", func() bool {
		_, code := link.currentSettings()
		return code == ""
	})
	fixture.queueOldEvent()
	fixture.queueSettings(old)
	awaitSafety(t, "old settings to revoke authority", func() bool {
		_, code := link.currentSettings()
		return code == "settings_unavailable"
	})

	result := link.SendRcs(`{"command_id":"synthetic-command","tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","mapping_revision":1,"proof_digest":"synthetic-proof","content":"synthetic-content"}`)
	if got := decodeSendCode(t, result); got != "pending_prepare" {
		t.Fatalf("code = %s result=%s", got, result)
	}
	if got := fixture.countAction(gmproto.ActionType_SEND_MESSAGE.String()); got != 0 {
		t.Fatalf("old settings allowed %d sends", got)
	}
	link.Stop()
	if err := awaitSafetyResult(t, done, "Run"); err != nil {
		t.Fatalf("Run error = %v", err)
	}
}

func TestBareSettingsCannotEnableSendAuthority(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	fixture.mu.Lock()
	fixture.conversations = []*gmproto.Conversation{{
		ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "current-outgoing",
		Participants: []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "current-outgoing"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	}}
	fixture.mu.Unlock()
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	awaitStreamOpen(t, link)
	setTestOwnerVerification(link)
	link.queueEvent(&libgm.WrappedSettings{Settings: &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
		SIMParticipant: &gmproto.SIMParticipant{ID: "current-outgoing"},
		SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}},
		RCSChats:       &gmproto.RCSChats{Enabled: true},
	}}}})
	awaitSafety(t, "queued current settings", func() bool {
		_, code := link.currentSettings()
		return code == ""
	})
	link.queueEvent(&gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: "unknown-outgoing"}}}})
	awaitSafety(t, "queued bare settings to invalidate authority", func() bool {
		_, code := link.currentSettings()
		return code == "settings_unavailable"
	})

	result := link.SendRcs(`{"command_id":"synthetic-command","tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","mapping_revision":1,"proof_digest":"synthetic-proof","content":"synthetic-content"}`)
	if got := decodeSendCode(t, result); got != "pending_prepare" {
		t.Fatalf("code = %s result=%s", got, result)
	}
	if got := fixture.countAction(gmproto.ActionType_SEND_MESSAGE.String()); got != 0 {
		t.Fatalf("bare settings allowed %d sends", got)
	}
	link.Stop()
	if err := awaitSafetyResult(t, done, "Run"); err != nil {
		t.Fatalf("Run error = %v", err)
	}
}

func awaitStreamOpen(t *testing.T, link *Link) {
	t.Helper()
	awaitSafety(t, "stream to open", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.streamOpen
	})
}

func decodeSendCode(t *testing.T, result string) string {
	t.Helper()
	var decoded struct {
		Code string `json:"code"`
	}
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	return decoded.Code
}
