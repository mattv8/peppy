package gmlink

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

const nativeCommitOutcomeWait = 2 * time.Second

func awaitNativeCommitOutcome(t *testing.T, signal <-chan struct{}) {
	t.Helper()
	select {
	case <-signal:
	case <-time.After(nativeCommitOutcomeWait):
		t.Fatal("commit did not reach admission barrier")
	}
}

func awaitNativeCommitOutcomeResult(t *testing.T, done <-chan string) string {
	t.Helper()
	select {
	case result := <-done:
		return result
	case <-time.After(nativeCommitOutcomeWait):
		t.Fatal("commit did not finish")
		return ""
	}
}

func TestNativeSendCommitAcceptsFetchedDisplayStateChange(t *testing.T) {
	link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
	defer cleanup()
	phone.mu.Lock()
	changed := proto.Clone(phone.conversations[0]).(*gmproto.Conversation)
	changed.LastMessageTimestamp = 99
	changed.Unread = !changed.Unread
	phone.conversations[0] = changed
	phone.mu.Unlock()

	if got := stage2Code(t, committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "accepted" {
		t.Fatalf("commit = %s", got)
	}
	if phone.countAction("GET_CONVERSATION") != 2 || phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("commit skipped the fresh read or did not send exactly once")
	}
}

func TestNativeSendCommitFreshGetRejectsChangedOrInvalidResponses(t *testing.T) {
	for _, test := range []struct {
		name   string
		change func(*phoneFixture)
	}{
		{"routing field outside candidate schema", func(phone *phoneFixture) {
			phone.mu.Lock()
			changed := proto.Clone(phone.conversations[0]).(*gmproto.Conversation)
			changed.DefaultOutgoingID = "synthetic-other-sim"
			phone.conversations[0] = changed
			phone.mu.Unlock()
		}},
		{"wrong response", func(phone *phoneFixture) {
			phone.mu.Lock()
			wrong := proto.Clone(phone.conversations[0]).(*gmproto.Conversation)
			wrong.ConversationID = "wrong-conversation"
			phone.mu.Unlock()
			phone.configure(func(knobs *fixtureKnobs) { knobs.getOverride, knobs.getOverrideSet = wrong, true })
		}},
		{"nil response", func(phone *phoneFixture) {
			phone.configure(func(knobs *fixtureKnobs) { knobs.getOverride, knobs.getOverrideSet = nil, true })
		}},
		{"get error", func(phone *phoneFixture) {
			phone.configure(func(knobs *fixtureKnobs) {
				knobs.failAction, knobs.failStatus = gmproto.ActionType_GET_CONVERSATION, 500
			})
		}},
	} {
		t.Run(test.name, func(t *testing.T) {
			link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
			defer cleanup()
			test.change(phone)
			if got := stage2Code(t, committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "rejected" {
				t.Fatalf("commit = %s", got)
			}
			if phone.countAction("GET_CONVERSATION") != 2 || phone.countAction("SEND_MESSAGE") != 0 {
				t.Fatal("invalid commit get reached send or skipped fresh read")
			}
		})
	}
}

func TestNativeSendCommitAdmissionFenceRejectsLiveDrift(t *testing.T) {
	for _, test := range []struct {
		name   string
		change func(*Link)
	}{
		{"settings version", func(link *Link) { link.settingsVersion++ }},
		{"settings epoch", func(link *Link) { link.settingsStreamEpoch++ }},
		{"settings payload", func(link *Link) { link.settings.SIMCards[0].SIMData.SIMPayload.SIMNumber++ }},
		{"own identity", func(link *Link) { link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = "+12025550999" }},
		{"pending events", func(link *Link) { link.pendingConversationEvents++ }},
		{"index revision", func(link *Link) { link.indexRevision++ }},
		{"owner retirement", func(link *Link) { link.ownerVerification = nil }},
		{"stream retirement", func(link *Link) { link.streamEpoch++ }},
		{"stop", func(link *Link) { link.Stop() }},
	} {
		t.Run(test.name, func(t *testing.T) {
			link, phone, cleanup, ticketID, commit := nativeDispatchFixture(t)
			defer cleanup()
			reached, release := make(chan struct{}), make(chan struct{})
			var releaseOnce sync.Once
			unblock := func() { releaseOnce.Do(func() { close(release) }) }
			defer unblock()
			link.mu.Lock()
			ticket := link.nativeTicket
			frozen := ticket.targetObservationJSON
			link.beforeSendAdmission = func() { close(reached); <-release }
			link.mu.Unlock()
			done := make(chan string, 1)
			go func() {
				done <- stage2Code(t, committer(t, link).CommitNativeRcs(ticketID, nativeCommitJSON(t, commit)))
			}()
			awaitNativeCommitOutcome(t, reached)
			if test.name == "stop" {
				test.change(link)
			} else {
				link.mu.Lock()
				test.change(link)
				link.mu.Unlock()
			}
			unblock()
			if got := awaitNativeCommitOutcomeResult(t, done); got != "rejected" {
				t.Fatalf("commit = %s", got)
			}
			if ticket.targetObservationJSON != frozen {
				t.Fatal("commit repaired or restamped frozen observation")
			}
			if phone.countAction("SEND_MESSAGE") != 0 {
				t.Fatal("drifted commit sent a message")
			}
		})
	}
}

func TestNativeSendCommitPostAdmissionUnknownResponses(t *testing.T) {
	for _, test := range []struct {
		name     string
		response func(*Link, context.Context, *gmproto.SendMessageRequest, *gmproto.SendMessageResponse, error) (*gmproto.SendMessageResponse, error)
	}{
		{"nil", func(_ *Link, _ context.Context, _ *gmproto.SendMessageRequest, _ *gmproto.SendMessageResponse, _ error) (*gmproto.SendMessageResponse, error) {
			return nil, nil
		}},
		{"error", func(_ *Link, _ context.Context, _ *gmproto.SendMessageRequest, _ *gmproto.SendMessageResponse, _ error) (*gmproto.SendMessageResponse, error) {
			return nil, errors.New("after admission")
		}},
		{"account switch", func(_ *Link, _ context.Context, _ *gmproto.SendMessageRequest, _ *gmproto.SendMessageResponse, _ error) (*gmproto.SendMessageResponse, error) {
			return &gmproto.SendMessageResponse{Status: gmproto.SendMessageResponse_SUCCESS, GoogleAccountSwitch: &gmproto.AccountChangeOrSomethingEvent{}}, nil
		}},
		{"non success", func(_ *Link, _ context.Context, _ *gmproto.SendMessageRequest, _ *gmproto.SendMessageResponse, _ error) (*gmproto.SendMessageResponse, error) {
			return &gmproto.SendMessageResponse{}, nil
		}},
		{"cancellation", func(link *Link, ctx context.Context, _ *gmproto.SendMessageRequest, _ *gmproto.SendMessageResponse, _ error) (*gmproto.SendMessageResponse, error) {
			link.Stop()
			return nil, ctx.Err()
		}},
	} {
		t.Run(test.name, func(t *testing.T) {
			link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
			defer cleanup()
			link.mu.Lock()
			client := link.client
			link.sendMessage = func(ctx context.Context, request *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
				response, err := client.SendMessage(ctx, request)
				return test.response(link, ctx, request, response, err)
			}
			link.mu.Unlock()
			if got := stage2Code(t, committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "outcome_unknown" {
				t.Fatalf("commit = %s", got)
			}
			if stage2Code(t, committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit))) != "rejected" || phone.countAction("SEND_MESSAGE") != 1 {
				t.Fatal("unknown outcome replayed a send")
			}
		})
	}
}

func TestNativeSendCommitLateRealSuccessRemainsAccepted(t *testing.T) {
	link, phone, cleanup, ticketID, commit := nativeDispatchFixture(t)
	defer cleanup()
	reached, release := make(chan struct{}), make(chan struct{})
	var releaseOnce sync.Once
	unblock := func() { releaseOnce.Do(func() { close(release) }) }
	defer unblock()
	link.mu.Lock()
	client := link.client
	ticket := link.nativeTicket
	link.sendMessage = func(ctx context.Context, request *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error) {
		response, err := client.SendMessage(ctx, request)
		close(reached)
		<-release
		return response, err
	}
	link.mu.Unlock()
	done := make(chan string, 1)
	go func() {
		done <- stage2Code(t, committer(t, link).CommitNativeRcs(ticketID, nativeCommitJSON(t, commit)))
	}()
	awaitNativeCommitOutcome(t, reached)
	link.mu.Lock()
	link.ticketNow = func() time.Time { return ticket.expiresAt }
	link.mu.Unlock()
	if got := stage2Code(t, link.RevokeNativeSend(ticketID)); got != "late" {
		t.Fatalf("revoke = %s", got)
	}
	link.Stop()
	unblock()
	if got := awaitNativeCommitOutcomeResult(t, done); got != "accepted" {
		t.Fatalf("late success = %s", got)
	}
	if phone.countAction("SEND_MESSAGE") != 1 {
		t.Fatal("late success did not make exactly one real send")
	}
}
