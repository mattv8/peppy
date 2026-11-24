package gmlink

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func reactionRequestJSON(ownerGeneration int64) string {
	return fmt.Sprintf(`{"request_id":"reaction-request","google_conversation_id":"synthetic-conversation","google_message_id":"synthetic-message","action":"add","emoji":"👍","owner_generation":%d}`, ownerGeneration)
}

func TestPreparedReactionDispatchesOnceAndMapsUncertainOutcomes(t *testing.T) {
	for _, test := range []struct {
		name     string
		response *gmproto.SendReactionResponse
		err      error
		want     string
	}{
		{"accepted", &gmproto.SendReactionResponse{Success: true}, nil, "accepted"},
		{"transport error", nil, errors.New("synthetic"), "outcome_unknown"},
		{"false success", &gmproto.SendReactionResponse{Success: false}, nil, "outcome_unknown"},
	} {
		t.Run(test.name, func(t *testing.T) {
			link, _, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
			defer cleanup()
			request := reactionRequestJSON(link.OwnerGeneration())
			prepared, err := link.PrepareReaction(request)
			if err != nil || decodeSendCode(t, prepared) != "prepared" {
				t.Fatalf("prepare = %q, %v", prepared, err)
			}
			calls := 0
			link.sendReaction = func(_ context.Context, payload *gmproto.SendReactionRequest) (*gmproto.SendReactionResponse, error) {
				calls++
				if payload.GetMessageID() != "synthetic-message" || payload.GetAction() != gmproto.SendReactionRequest_ADD || payload.GetReactionData().GetUnicode() != "👍" {
					t.Fatalf("payload = %#v", payload)
				}
				return test.response, test.err
			}
			committed, err := link.CommitPreparedReaction(request)
			if err != nil || decodeSendCode(t, committed) != test.want {
				t.Fatalf("commit = %q, %v", committed, err)
			}
			if calls != 1 {
				t.Fatalf("calls = %d, want 1", calls)
			}
			replayed, err := link.CommitPreparedReaction(request)
			if err != nil || decodeSendCode(t, replayed) != "rejected" {
				t.Fatalf("replay = %q, %v", replayed, err)
			}
		})
	}
}

func TestPreparedReactionRefusesStaleOwnerAndInvalidEmoji(t *testing.T) {
	link, _, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	if result, err := link.PrepareReaction(reactionRequestJSON(link.OwnerGeneration() + 1)); err != nil || decodeSendCode(t, result) != "rejected" {
		t.Fatalf("stale owner = %q, %v", result, err)
	}
	request := fmt.Sprintf(`{"request_id":"reaction-request","google_conversation_id":"synthetic-conversation","google_message_id":"synthetic-message","action":"add","emoji":"plain text","owner_generation":%d}`, link.OwnerGeneration())
	if result, err := link.PrepareReaction(request); err != nil || decodeSendCode(t, result) != "invalid_emoji" {
		t.Fatalf("invalid emoji = %q, %v", result, err)
	}
}

func TestPreparedReactionRejectsInvalidationDuringLookupAndAdmission(t *testing.T) {
	t.Run("lookup", func(t *testing.T) {
		link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
		defer cleanup()
		defer phone.openGate()
		request := reactionRequestJSON(link.OwnerGeneration())
		if result, err := link.PrepareReaction(request); err != nil || decodeSendCode(t, result) != "prepared" {
			t.Fatalf("prepare = %q, %v", result, err)
		}
		phone.configure(func(knobs *fixtureKnobs) {
			knobs.gateSend = true
			knobs.gateAction = gmproto.ActionType_GET_CONVERSATION
		})
		calls := 0
		link.sendReaction = func(context.Context, *gmproto.SendReactionRequest) (*gmproto.SendReactionResponse, error) {
			calls++
			return &gmproto.SendReactionResponse{Success: true}, nil
		}
		done := make(chan string, 1)
		go func() {
			result, _ := link.CommitPreparedReaction(request)
			done <- decodeSendCode(t, result)
		}()
		awaitFixtureAction(t, phone, gmproto.ActionType_GET_CONVERSATION)
		link.mu.Lock()
		link.invalidateEvidenceLocked()
		link.mu.Unlock()
		phone.openGate()
		if result := <-done; result != "rejected" || calls != 0 {
			t.Fatalf("lookup invalidation = %q, calls=%d", result, calls)
		}
	})

	t.Run("admission", func(t *testing.T) {
		link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
		defer cleanup()
		phone.configure(func(knobs *fixtureKnobs) {
			knobs.failAction = gmproto.ActionType_SEND_REACTION
			knobs.failStatus = 500
		})
		request := reactionRequestJSON(link.OwnerGeneration())
		if result, err := link.PrepareReaction(request); err != nil || decodeSendCode(t, result) != "prepared" {
			t.Fatalf("prepare = %q, %v", result, err)
		}
		reached, release := make(chan struct{}), make(chan struct{})
		var releaseOnce sync.Once
		unblock := func() { releaseOnce.Do(func() { close(release) }) }
		defer unblock()
		send := link.client.SendReaction
		link.sendReaction = func(ctx context.Context, request *gmproto.SendReactionRequest) (*gmproto.SendReactionResponse, error) {
			close(reached)
			<-release
			return send(ctx, request)
		}
		done := make(chan string, 1)
		go func() {
			result, _ := link.CommitPreparedReaction(request)
			done <- decodeSendCode(t, result)
		}()
		select {
		case <-reached:
		case <-time.After(5 * time.Second):
			t.Fatal("reaction admission was not reached")
		}
		link.mu.Lock()
		link.invalidateEvidenceLocked()
		link.mu.Unlock()
		unblock()
		if result := recvWithin(t, done, 5*time.Second); result != "rejected" {
			t.Fatalf("admission invalidation = %q", result)
		}
		if calls := phone.countAction(gmproto.ActionType_SEND_REACTION.String()); calls != 0 {
			t.Fatalf("revoked reaction count = %d", calls)
		}
	})
}

func awaitFixtureAction(t *testing.T, phone *phoneFixture, want gmproto.ActionType) {
	t.Helper()
	deadline := time.NewTimer(5 * time.Second)
	defer deadline.Stop()
	for {
		select {
		case action := <-phone.actionReached:
			if action == want {
				return
			}
		case <-deadline.C:
			t.Fatalf("fixture did not receive %s", want)
		}
	}
}

func TestReactionEmojiFixture(t *testing.T) {
	fixturePath := filepath.Join("..", "..", "..", "..", "packages", "contracts", "fixtures", "reaction-emoji.json")
	var fixture struct {
		Accept []string `json:"accept"`
		Reject []string `json:"reject"`
	}
	encoded, err := os.ReadFile(fixturePath)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(encoded, &fixture); err != nil {
		t.Fatal(err)
	}
	for _, emoji := range fixture.Accept {
		if !validReactionEmoji(emoji) {
			t.Errorf("accepted emoji rejected: %q", emoji)
		}
	}
	for _, emoji := range fixture.Reject {
		if validReactionEmoji(emoji) {
			t.Errorf("rejected emoji accepted: %q", emoji)
		}
	}
}

func TestReactionEmojiBoundaryCases(t *testing.T) {
	for _, emoji := range []string{"👩🏽‍💻", "🏳️‍🌈"} {
		if !validReactionEmoji(emoji) {
			t.Errorf("accepted boundary emoji rejected: %q", emoji)
		}
	}
	for _, emoji := range []string{"🇨", "☺︎", "👍\u200d", "\u200d👍", "👨🏽‍👩🏽‍👧🏽‍👦🏽"} {
		if validReactionEmoji(emoji) {
			t.Errorf("rejected boundary emoji accepted: %q", emoji)
		}
	}
}
