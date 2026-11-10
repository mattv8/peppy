package gmlink

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
)

type nativeTargetClassifierFunc func(string) bool

func (classify nativeTargetClassifierFunc) Classify(candidateSnapshot string) bool {
	return classify(candidateSnapshot)
}

func TestNativeTargetClassificationRejectsInvalidTicketBeforeCallback(t *testing.T) {
	for _, change := range []struct {
		name  string
		apply func(*Link, nativeRcsPrepareResult)
	}{
		{name: "stop", apply: func(link *Link, _ nativeRcsPrepareResult) { link.Stop() }},
		{name: "revoke", apply: func(link *Link, prepared nativeRcsPrepareResult) { link.RevokeNativeSend(prepared.TicketID) }},
		{name: "index_drift", apply: func(link *Link, _ nativeRcsPrepareResult) {
			link.mu.Lock()
			link.indexRevision++
			link.mu.Unlock()
		}},
		{name: "settings_drift", apply: func(link *Link, _ nativeRcsPrepareResult) {
			link.mu.Lock()
			link.settingsVersion++
			link.mu.Unlock()
		}},
		{name: "expired", apply: func(link *Link, _ nativeRcsPrepareResult) {
			link.mu.Lock()
			link.nativeTicket.expiresAt = link.nowForTicketLocked().Add(-time.Nanosecond)
			link.mu.Unlock()
		}},
		{name: "replaced", apply: func(link *Link, _ nativeRcsPrepareResult) {
			link.mu.Lock()
			link.nativeTicket = &nativeRcsTicket{id: uuid.NewString()}
			link.mu.Unlock()
		}},
	} {
		t.Run(change.name, func(t *testing.T) {
			link, _, cleanup, prepared, _ := nativeTargetFixture(t)
			defer cleanup()
			change.apply(link, prepared)
			called := false
			if link.WithNativeRcsTargetClassification(prepared.TicketID, nativeTargetClassifierFunc(func(string) bool {
				called = true
				return true
			})) || called {
				t.Fatal("invalid ticket reached classifier")
			}
		})
	}
}

func TestNativeTargetClassificationRequiresRouteAndUnboundTarget(t *testing.T) {
	t.Run("nil_classifier", func(t *testing.T) {
		link, _, cleanup, prepared, _ := nativeTargetFixture(t)
		defer cleanup()
		if link.WithNativeRcsTargetClassification(prepared.TicketID, nil) {
			t.Fatal("nil classifier succeeded")
		}
	})

	t.Run("route_unbound", func(t *testing.T) {
		link, cleanup, request := nativePrepareFixture(t)
		defer cleanup()
		var prepared nativeRcsPrepareResult
		if err := json.Unmarshal([]byte(link.PrepareNativeRcs(request)), &prepared); err != nil || prepared.Code != "prepared" {
			t.Fatal("prepare failed")
		}
		called := false
		if link.WithNativeRcsTargetClassification(prepared.TicketID, nativeTargetClassifierFunc(func(string) bool {
			called = true
			return true
		})) || called {
			t.Fatal("unbound route reached classifier")
		}
	})

	for _, state := range []string{"target_bound", "admission", "inflight"} {
		t.Run(state, func(t *testing.T) {
			link, _, cleanup, prepared, _ := nativeTargetFixture(t)
			defer cleanup()
			link.mu.Lock()
			switch state {
			case "target_bound":
				link.nativeTicket.targetObservation = &nativeRcsObservation{}
			case "admission":
				link.sendAdmissionInFlight = true
			case "inflight":
				link.nativeInFlight = &nativeRcsInFlight{ticketID: uuid.NewString()}
			}
			link.mu.Unlock()
			called := false
			if link.WithNativeRcsTargetClassification(prepared.TicketID, nativeTargetClassifierFunc(func(string) bool {
				called = true
				return true
			})) || called {
				t.Fatalf("%s ticket reached classifier", state)
			}
		})
	}
}

func TestNativeTargetClassificationPassesFrozenSnapshotAndPreservesTicket(t *testing.T) {
	link, _, cleanup, prepared, _ := nativeTargetFixture(t)
	defer cleanup()
	var got string
	if !link.WithNativeRcsTargetClassification(prepared.TicketID, nativeTargetClassifierFunc(func(snapshot string) bool {
		got = snapshot
		return true
	})) {
		t.Fatal("classification rejected current route")
	}
	if got != prepared.CandidateSnapshot {
		t.Fatalf("classifier snapshot = %q, want exact frozen bytes %q", got, prepared.CandidateSnapshot)
	}
	if got := stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))); got != "target_bound" {
		t.Fatalf("Found continuation code = %q, want target_bound", got)
	}
}

func TestNativeTargetClassificationBlocksConcurrentMutation(t *testing.T) {
	link, _, cleanup, prepared, _ := nativeTargetFixture(t)
	defer cleanup()
	entered := make(chan struct{})
	release := make(chan struct{})
	classified := make(chan bool, 1)
	go func() {
		classified <- link.WithNativeRcsTargetClassification(prepared.TicketID, nativeTargetClassifierFunc(func(string) bool {
			close(entered)
			<-release
			return true
		}))
	}()
	select {
	case <-entered:
	case <-time.After(time.Second):
		t.Fatal("classifier did not start")
	}
	mutated := make(chan string, 1)
	go func() { mutated <- link.RevokeNativeSend(prepared.TicketID) }()
	select {
	case result := <-mutated:
		t.Fatalf("mutation completed during classification: %s", stage2Code(t, result))
	case <-time.After(100 * time.Millisecond):
	}
	close(release)
	select {
	case accepted := <-classified:
		if !accepted {
			t.Fatal("classification did not complete")
		}
	case <-time.After(time.Second):
		t.Fatal("classification did not finish")
	}
	select {
	case result := <-mutated:
		if got := stage2Code(t, result); got != "revoked" {
			t.Fatalf("delayed mutation code = %q, want revoked", got)
		}
	case <-time.After(time.Second):
		t.Fatal("mutation did not proceed after classification")
	}
}

func TestNativeTargetClassificationFailureAndPanicReleaseLock(t *testing.T) {
	for _, classifier := range []struct {
		name     string
		classify nativeTargetClassifierFunc
	}{
		{name: "false", classify: func(string) bool { return false }},
		{name: "panic", classify: func(string) bool { panic("classifier panic") }},
	} {
		t.Run(classifier.name, func(t *testing.T) {
			link, _, cleanup, prepared, _ := nativeTargetFixture(t)
			defer cleanup()
			if link.WithNativeRcsTargetClassification(prepared.TicketID, classifier.classify) {
				t.Fatal("failed classification succeeded")
			}
			if got := stage2Code(t, link.RevokeNativeSend(prepared.TicketID)); got != "revoked" {
				t.Fatalf("callback failure retained lock or consumed ticket: %q", got)
			}
		})
	}
}
