package gmlink

import (
	"encoding/json"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const nativeTargetRaceWait = 4 * time.Second

func awaitNativeTargetRaceAction(t *testing.T, phone *phoneFixture, want gmproto.ActionType) {
	t.Helper()
	timer := time.NewTimer(nativeTargetRaceWait)
	defer timer.Stop()
	for {
		select {
		case got := <-phone.actionReached:
			if got == want {
				return
			}
		case <-timer.C:
			t.Fatalf("did not reach %s", want)
		}
	}
}

func awaitNativeTargetRaceResult(t *testing.T, done <-chan string) string {
	t.Helper()
	select {
	case result := <-done:
		return result
	case <-time.After(nativeTargetRaceWait):
		t.Fatal("target bind did not finish")
		return ""
	}
}

func prepareNativeTargetRaceSuccessor(t *testing.T, link *Link) nativeRcsPrepareResult {
	t.Helper()
	now := time.Now().UnixMilli()
	request, err := json.Marshal(map[string]any{
		"command_id": uuid.NewString(), "canonicalizer_revision": 1, "identity_region": "US",
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1, "own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now},
	})
	if err != nil {
		t.Fatal(err)
	}
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(request))), &prepared); err != nil || prepared.Code != "prepared" {
		t.Fatalf("successor prepare = %q, %v", prepared.Code, err)
	}
	return prepared
}

func TestNativeSendTargetSharesDeadlineAcrossRPCLockAndFetch(t *testing.T) {
	t.Run("lock wait consumes the fetch deadline", func(t *testing.T) {
		link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
		defer cleanup()
		defer phone.openGate()
		phone.configure(func(knobs *fixtureKnobs) {
			knobs.gateSend = true
			knobs.gateAction = gmproto.ActionType_GET_CONVERSATION
		})
		link.rpcMu.Lock()
		locked := true
		defer func() {
			if locked {
				link.rpcMu.Unlock()
			}
		}()

		started := time.Now()
		done := make(chan string, 1)
		go func() {
			done <- stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared)))
		}()
		wait := time.NewTimer(2 * time.Second)
		defer wait.Stop()
		<-wait.C
		link.rpcMu.Unlock()
		locked = false
		awaitNativeTargetRaceAction(t, phone, gmproto.ActionType_GET_CONVERSATION)
		if got := awaitNativeTargetRaceResult(t, done); got != "rejected" {
			t.Fatalf("target bind = %s", got)
		}
		if elapsed := time.Since(started); elapsed > 6*time.Second+500*time.Millisecond {
			t.Fatalf("target bind exceeded shared deadline: %s", elapsed)
		}
		if phone.countAction("SEND_MESSAGE") != 0 {
			t.Fatal("deadline rejection sent a message")
		}
	})

	t.Run("ticket expiry while waiting refuses before fetch", func(t *testing.T) {
		link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
		defer cleanup()
		link.rpcMu.Lock()
		locked := true
		defer func() {
			if locked {
				link.rpcMu.Unlock()
			}
		}()
		done := make(chan string, 1)
		go func() {
			done <- stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared)))
		}()
		wait := time.NewTimer(3 * time.Second)
		defer wait.Stop()
		<-wait.C
		link.mu.Lock()
		deadline := link.nativeTicket.expiresAt
		link.ticketNow = func() time.Time { return deadline }
		link.mu.Unlock()
		link.rpcMu.Unlock()
		locked = false
		if got := awaitNativeTargetRaceResult(t, done); got != "rejected" {
			t.Fatalf("target bind = %s", got)
		}
		if phone.countAction("GET_CONVERSATION") != 0 || phone.countAction("SEND_MESSAGE") != 0 {
			t.Fatal("expired ticket reached transport")
		}
	})
}

func TestNativeSendTargetBlockedOldLookupPreservesSuccessor(t *testing.T) {
	link, phone, cleanup, prepared, observedAt := nativeTargetFixture(t)
	defer cleanup()
	defer phone.openGate()
	phone.configure(func(knobs *fixtureKnobs) {
		knobs.gateSend = true
		knobs.gateAction = gmproto.ActionType_GET_CONVERSATION
	})
	done := make(chan string, 1)
	go func() {
		done <- stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared)))
	}()
	awaitNativeTargetRaceAction(t, phone, gmproto.ActionType_GET_CONVERSATION)
	if got := stage2Code(t, link.RevokeNativeSend(prepared.TicketID)); got != "revoked" {
		t.Fatalf("old revoke = %s", got)
	}
	successor := prepareNativeTargetRaceSuccessor(t, link)
	if got := stage2Code(t, link.BindNativeRcsRoute(successor.TicketID, 31, 37, 41, 43, observedAt)); got != "route_bound" {
		t.Fatalf("successor route bind = %s", got)
	}
	phone.openGate()
	if got := awaitNativeTargetRaceResult(t, done); got != "rejected" {
		t.Fatalf("old target bind = %s", got)
	}
	link.mu.Lock()
	remaining := link.nativeTicket
	link.mu.Unlock()
	if remaining == nil || remaining.id != successor.TicketID || remaining.routeObservation == nil {
		t.Fatal("old lookup cleared or changed successor")
	}
	if phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("target lookup sent a message")
	}
}

func TestNativeSendTargetConcurrentBindConsumesOneTargetSlot(t *testing.T) {
	link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
	defer cleanup()
	start := make(chan struct{})
	results := make(chan string, 2)
	var group sync.WaitGroup
	for range 2 {
		group.Add(1)
		go func() {
			defer group.Done()
			<-start
			results <- stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared)))
		}()
	}
	close(start)
	group.Wait()
	close(results)
	bound, rejected := 0, 0
	for result := range results {
		switch result {
		case "target_bound":
			bound++
		case "rejected":
			rejected++
		default:
			t.Fatalf("target bind = %s", result)
		}
	}
	if bound != 1 || rejected != 1 {
		t.Fatalf("target_bound=%d rejected=%d", bound, rejected)
	}
	if phone.countAction("GET_CONVERSATION") != 1 || phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("concurrent target binds did not perform exactly one read-only fetch")
	}
	if got := stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))); got != "rejected" {
		t.Fatalf("duplicate target bind = %s", got)
	}
	if phone.countAction("GET_CONVERSATION") != 1 {
		t.Fatal("duplicate target bind revived the ticket")
	}
}
