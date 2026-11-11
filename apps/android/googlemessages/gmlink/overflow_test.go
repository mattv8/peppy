package gmlink

import (
	"strings"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
)

func TestIngressOverflowRetiresOwningRunWithoutHostCallback(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	awaitSafety(t, "Run to become healthy", func() bool {
		return strings.Contains(link.Status(), `"healthy":true`)
	})
	link.mu.Lock()
	generation, revision := link.generation, link.stateRevision
	link.mu.Unlock()

	reached, release := host.holdNextEvent("temporary_error")
	link.queueEventFor(generation, &events.ListenTemporaryError{})
	select {
	case <-reached:
	case <-time.After(5 * time.Second):
		t.Fatal("consumer did not reach blocked host output")
	}
	defer func() {
		if release != nil {
			close(release)
		}
	}()
	overflowIngressWhileProjectionPaused(t, link, generation)
	if got := host.countEvent("link", "event_overflow"); got != 0 {
		t.Fatalf("overflow ingress made %d host callbacks", got)
	}
	if !strings.Contains(link.Status(), `"stop_code":"event_overflow"`) {
		t.Fatalf("overflow did not publish stop code: %s", link.Status())
	}
	link.mu.Lock()
	retired := link.generation
	link.mu.Unlock()
	if retired != generation+1 {
		t.Fatalf("generation = %d, want immediate retirement from %d", retired, generation)
	}
	if err := awaitSafetyResult(t, done, "Run after overflow"); err == nil || err.Error() != "event_overflow" {
		t.Fatalf("Run error = %v, want event_overflow", err)
	}
	if link.AcceptsOutput(int64(generation), int64(revision)) {
		t.Fatal("retired output identity remained admitted after Run returned")
	}

	close(release)
	release = nil
	host.awaitEvent(t, "conn", "temporary_error")
	link.emit("conn", "recovered", 0)
	host.awaitEvent(t, "conn", "recovered")
	if got := host.countEvent("link", "event_overflow"); got != 0 {
		t.Fatalf("overflow emitted %d asynchronous host events", got)
	}
	if link.AcceptsOutput(int64(generation), int64(revision)) {
		t.Fatal("retired output identity was readmitted after host output drained")
	}
}

// TestStartupOverflowReturnsEventOverflow overflows the ingress while Connect is still
// inside its synchronous token refresh, so Run reaches its post-connect ownership check
// with a retired generation. Run must report event_overflow, not a clean nil.
func TestStartupOverflowReturnsEventOverflow(t *testing.T) {
	auth := linkedSafetyAuth()
	auth.TachyonExpiry = time.Now().Add(-time.Hour)
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gateRefresh = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	awaitSafety(t, "Connect to reach token refresh", func() bool {
		fixture.mu.Lock()
		defer fixture.mu.Unlock()
		return fixture.refreshes == 1
	})
	link.mu.Lock()
	generation := link.generation
	link.mu.Unlock()

	reached, release := host.holdNextEvent("temporary_error")
	link.queueEventFor(generation, &events.ListenTemporaryError{})
	select {
	case <-reached:
	case <-time.After(5 * time.Second):
		t.Fatal("consumer did not reach blocked host output")
	}
	defer func() {
		if release != nil {
			close(release)
		}
	}()
	overflowIngressWhileProjectionPaused(t, link, generation)
	link.mu.Lock()
	retired := link.generation
	link.mu.Unlock()
	if retired != generation+1 {
		t.Fatalf("generation = %d, want startup overflow to retire %d", retired, generation)
	}
	fixture.openGate()
	if err := awaitSafetyResult(t, done, "Run after startup overflow"); err == nil || err.Error() != "event_overflow" {
		t.Fatalf("Run error = %v, want event_overflow", err)
	}
}

func TestCloseAfterIngressOverflowDoesNotDeliverOverflowEvent(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	link.mu.Lock()
	link.state = stateLinked
	link.generation = 9
	link.mu.Unlock()

	reached, release := host.holdNextEvent("temporary_error")
	link.queueEventFor(9, &events.ListenTemporaryError{})
	select {
	case <-reached:
	case <-time.After(5 * time.Second):
		t.Fatal("consumer did not reach blocked host output")
	}
	overflowIngressWhileProjectionPaused(t, link, 9)
	closed := make(chan struct{})
	host.mu.Lock()
	host.onEvent = func(_, code string) {
		if code == "temporary_error" {
			link.Close()
			close(closed)
		}
	}
	host.mu.Unlock()
	close(release)
	select {
	case <-closed:
	case <-time.After(5 * time.Second):
		t.Fatal("Close did not return")
	}
	if got := host.countEvent("link", "event_overflow"); got != 0 {
		t.Fatalf("closed link delivered %d overflow notifications", got)
	}
}

func overflowIngressWhileProjectionPaused(t *testing.T, link *Link, generation uint64) {
	t.Helper()
	// Host output runs independently, so blocking a Host callback does not pause
	// the ingress consumer. Pause it at the output-queue boundary before filling.
	link.outputMu.Lock()
	defer link.outputMu.Unlock()

	ingressReturned := make(chan struct{})
	go func() {
		defer close(ingressReturned)
		for range cap(link.changes) + 2 {
			link.queueEventFor(generation, &events.ListenTemporaryError{})
		}
	}()
	select {
	case <-ingressReturned:
	case <-time.After(3 * time.Second):
		t.Fatal("overflow ingress blocked on host output")
	}
}
