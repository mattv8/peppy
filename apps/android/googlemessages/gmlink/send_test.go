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
	link, fixture, cleanup, token, tmpID := preparedRcsFixtureWithPhone(t)
	_ = fixture
	return link, cleanup, token, tmpID
}

func preparedRcsFixtureWithPhone(t *testing.T) (*Link, *phoneFixture, func(), string, string) {
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
	return link, fixture, cleanup, snapshot.SnapshotToken, "00000000-0000-4000-8000-000000000000"
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
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "outcome_unknown" {
		t.Fatalf("libgm rejection = %s", got)
	}
}

func TestPreparedRcsMapsEveryLibgmStatusConservatively(t *testing.T) {
	link, cleanup, token, tmpID := preparedRcsFixture(t)
	defer cleanup()
	for _, test := range []struct {
		status gmproto.SendMessageResponse_Status
		code   string
	}{
		{gmproto.SendMessageResponse_UNKNOWN, "outcome_unknown"},
		{gmproto.SendMessageResponse_SUCCESS, "accepted"},
		{gmproto.SendMessageResponse_FAILURE_2, "outcome_unknown"},
		{gmproto.SendMessageResponse_FAILURE_3, "outcome_unknown"},
		{gmproto.SendMessageResponse_FAILURE_4, "outcome_unknown"},
		{gmproto.SendMessageResponse_Status(99), "outcome_unknown"},
	} {
		ticket := prepareTicket(t, link, token, tmpID)
		link.mu.Lock()
		link.sendMessage = func(context.Context, *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
			return &gmproto.SendMessageResponse{Status: test.status}, nil
		}
		link.mu.Unlock()
		if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != test.code {
			t.Fatalf("status %d = %s", test.status, got)
		}
	}
}

func TestPrepareRcsStrictJSONAcceptsWorstCaseEscapedBody(t *testing.T) {
	body := strings.Repeat("\"", maxRcsBodyBytes)
	request := `{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","snapshot_token":"1:1:1","content":"` + strings.ReplaceAll(body, `"`, `\"`) + `"}`
	var decoded prepareRcsRequest
	if !decodeFixedJSON(request, &decoded) || decoded.Content != body || !validPrepareRcsRequest(decoded) {
		t.Fatal("worst-case escaped prepare body was refused")
	}
	overLimit := strings.Repeat("x", maxRcsBodyBytes+1)
	request = `{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","snapshot_token":"1:1:1","content":"` + overLimit + `"}`
	if !decodeFixedJSON(request, &decoded) || validPrepareRcsRequest(decoded) {
		t.Fatal("over-limit decoded body was accepted")
	}
}

func TestPrepareRcsStrictJSONRejectsAliasesDuplicatesAndTrailingData(t *testing.T) {
	valid := `{"tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","snapshot_token":"1:1:1","content":"body"}`
	for _, input := range []string{
		strings.Replace(valid, `"tmp_id"`, `"Tmp_ID"`, 1),
		strings.Replace(valid, `"content":"body"`, `"content":"body","content":"again"`, 1),
		strings.Replace(valid, `"content":"body"`, `"content":null`, 1),
		strings.Replace(valid, `"content":"body"`, `"unknown":true,"content":"body"`, 1),
		valid + ` {}`,
	} {
		var decoded prepareRcsRequest
		if decodeFixedJSON(input, &decoded) {
			t.Fatalf("accepted malformed envelope: %s", input)
		}
	}
}

func TestPreparedRcsAdmissionRejectsRevocationBeforeHTTPWrite(t *testing.T) {
	link, fixture, cleanup, token, tmpID := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	ticket := prepareTicket(t, link, token, tmpID)
	reached, release := make(chan struct{}), make(chan struct{})
	link.mu.Lock()
	link.beforeSendAdmission = func() {
		close(reached)
		<-release
	}
	link.mu.Unlock()
	result := make(chan string, 1)
	go func() { result <- decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))) }()
	select {
	case <-reached:
	case <-time.After(5 * time.Second):
		t.Fatal("transport admission was not reached")
	}
	link.mu.Lock()
	link.invalidateEvidenceLocked()
	link.mu.Unlock()
	close(release)
	if got := <-result; got != "rejected" {
		t.Fatalf("revoked admission = %s", got)
	}
	fixture.mu.Lock()
	sends := len(fixture.sendRequests)
	fixture.mu.Unlock()
	if sends != 0 {
		t.Fatalf("revoked send requests = %d", sends)
	}
}

func TestPreparedRcsRejectsChangedFetchedConversationWithoutIndexChange(t *testing.T) {
	link, fixture, cleanup, token, tmpID := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	ticket := prepareTicket(t, link, token, tmpID)
	fixture.mu.Lock()
	fixture.conversations[0] = cloneConversation(fixture.conversations[0])
	fixture.conversations[0].Status = gmproto.ConversationStatus_ARCHIVED
	fixture.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "rejected" {
		t.Fatalf("changed fetched conversation = %s", got)
	}
	fixture.mu.Lock()
	sends := len(fixture.sendRequests)
	fixture.mu.Unlock()
	if sends != 0 {
		t.Fatalf("changed fetched conversation sent %d requests", sends)
	}
}

func TestPreparedRcsExpiresWhileWaitingForRPCSlot(t *testing.T) {
	link, fixture, cleanup, token, tmpID := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	now := time.Now()
	link.mu.Lock()
	link.ticketNow = func() time.Time { return now }
	link.mu.Unlock()
	ticket := prepareTicket(t, link, token, tmpID)
	link.rpcMu.Lock()
	result := make(chan string, 1)
	go func() { result <- decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))) }()
	awaitSafety(t, "commit waiting for RPC slot", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.sendAdmissionInFlight
	})
	if got := decodeSendCode(t, link.PrepareRcs(`{"tmp_id":"11111111-1111-4111-8111-111111111111","google_conversation_id":"synthetic-conversation","snapshot_token":"`+token+`","content":"synthetic-content"}`)); got != "busy" {
		t.Fatalf("prepare while admission in flight = %s", got)
	}
	now = now.Add(preparedRcsTicketTTL)
	link.rpcMu.Unlock()
	select {
	case got := <-result:
		if got != "rejected" {
			t.Fatalf("expired RPC wait = %s", got)
		}
	case <-time.After(rcsSendDeadline + time.Second):
		t.Fatal("commit did not respect admission budget")
	}
	fixture.mu.Lock()
	sends := len(fixture.sendRequests)
	fixture.mu.Unlock()
	if sends != 0 {
		t.Fatalf("expired RPC wait sent %d requests", sends)
	}
}

func TestPreparedRcsSuccessWithAccountSwitchIsUnknown(t *testing.T) {
	link, cleanup, token, tmpID := preparedRcsFixture(t)
	defer cleanup()
	ticket := prepareTicket(t, link, token, tmpID)
	link.mu.Lock()
	link.sendMessage = func(context.Context, *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
		return &gmproto.SendMessageResponse{Status: gmproto.SendMessageResponse_SUCCESS, GoogleAccountSwitch: &gmproto.AccountChangeOrSomethingEvent{}}, nil
	}
	link.mu.Unlock()
	if got := decodeSendCode(t, link.CommitPreparedRcs(ticket, commitPermit(tmpID, token))); got != "outcome_unknown" {
		t.Fatalf("account switch success = %s", got)
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
