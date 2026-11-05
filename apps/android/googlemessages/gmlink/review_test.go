package gmlink

import (
	"encoding/json"
	"fmt"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
)

// awaitProcessed waits until the link has handled every event queued before this
// call. The queue is ordered, so a sentinel event marks the position.
func awaitProcessed(t *testing.T, link *Link, host *safetyHost) {
	t.Helper()
	before := host.countEvent("conn", "no_data")
	link.queueEvent(&events.NoDataReceived{})
	awaitSafety(t, "queued events to be processed", func() bool { return host.countEvent("conn", "no_data") > before })
}

// persistedCookies returns the cookies in the last persisted envelope.
func persistedCookies(t *testing.T, host *safetyHost) map[string]string {
	t.Helper()
	var auth struct {
		Cookies map[string]string `json:"cookies"`
	}
	if err := json.Unmarshal(host.lastEnvelope(t).Auth, &auth); err != nil {
		t.Fatal(err)
	}
	return auth.Cookies
}

func TestRunEndsAfterNeedsRelink(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)

	link.queueEvent(&events.GaiaLoggedOut{})

	if err := awaitSafetyResult(t, done, "Run"); err != nil {
		t.Fatalf("Run error = %v, want nil after the link needs relink", err)
	}
	expectStatus(t, link, `"state":"needs_relink"`, `"running":false`, `"healthy":false`)
	if !host.hasEvent("link", "relink_needed") {
		t.Fatal("relink_needed was not reported to the host")
	}
}

func TestEventsCannotChangeStateOutsideLinked(t *testing.T) {
	t.Run("unlinked ignores GaiaLoggedOut", func(t *testing.T) {
		link, host := openSafetyLink(t, nil)
		before := host.stateCount()

		link.queueEvent(&events.GaiaLoggedOut{})
		awaitProcessed(t, link, host)

		expectStatus(t, link, `"state":"unlinked"`)
		if host.stateCount() != before || host.hasEvent("link", "relink_needed") {
			t.Fatal("an unlinked link persisted or reported relink_needed")
		}
	})

	t.Run("pairing ignores a real account change", func(t *testing.T) {
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, statePairing, nil)

		link.queueEvent(&events.AccountChange{})
		awaitProcessed(t, link, host)

		expectStatus(t, link, `"state":"pairing"`)
		if host.hasEvent("link", "signed_out") {
			t.Fatal("a pairing link was signed out by a stale account change")
		}
	})

	t.Run("signed_out ignores a fatal listen error", func(t *testing.T) {
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateSignedOut, nil)

		link.queueEvent(&events.ListenFatalError{Error: events.ErrInvalidCredentials})
		awaitProcessed(t, link, host)

		expectStatus(t, link, `"state":"signed_out"`)
		if host.hasEvent("link", "signed_out") {
			t.Fatal("a signed-out link reported a second sign-out")
		}
	})
}

func TestFreshPairAccountMismatchRestoresPrevious(t *testing.T) {
	for _, previous := range []string{stateUnlinked, stateSignedOut} {
		t.Run("from "+previous, func(t *testing.T) {
			fixture := newPhoneFixture(t, nil)
			fixture.configure(func(knobs *fixtureKnobs) {
				knobs.configEmail = canaryAccount
				knobs.gaiaInit = true
				knobs.gaiaFinish = true
			})
			link, host := openSafetyLink(t, nil)
			setSafetyState(link, previous, nil)
			result := make(chan error, 1)
			go func() { result <- link.Pair(canaryCookieJSON(t)) }()

			err := awaitPairResult(t, link, result, "Pair")

			expectErrorCode(t, err, "account_mismatch")
			expectStatus(t, link, `"state":"`+previous+`"`, `"stop_code":"account_mismatch"`)
			if !host.hasEvent("link", "account_mismatch") {
				t.Fatal("account_mismatch was not reported to the host")
			}
			if n := fixture.countAction("UNPAIR_GAIA_PAIRING"); n != 0 {
				t.Fatalf("a fresh pair mismatch sent %d unpair requests, want none", n)
			}
		})
	}
}

func TestUnlinkDuringEmojiWaitStaysUnlinked(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = canaryAccount
		knobs.gaiaInit = true
	})
	link, host := openSafetyLink(t, nil)
	result := make(chan error, 1)
	go func() { result <- link.Pair(canaryCookieJSON(t)) }()
	awaitEmoji(t, host)

	if err := link.Unlink(); err != nil {
		t.Fatalf("Unlink error = %v, want nil for an unpaired link", err)
	}
	if err := awaitPairResult(t, link, result, "Pair"); err == nil {
		t.Fatal("Pair succeeded after Unlink")
	}
	time.Sleep(100 * time.Millisecond)
	expectStatus(t, link, `"state":"unlinked"`)
}

func TestCloseStopsPersistence(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateNeedsRelink, auth)
	result := make(chan error, 1)
	go func() { result <- link.Relink() }()
	awaitEmoji(t, host)

	link.Close()
	persistedAtClose := host.stateCount()
	err := awaitPairResult(t, link, result, "Relink")

	expectErrorCode(t, err, "pair_cancelled")
	if got := host.stateCount(); got != persistedAtClose {
		t.Fatalf("PersistState ran after Close: %d envelopes, want %d", got, persistedAtClose)
	}
}

func TestQueueEnqueuesOnlyHandledEvents(t *testing.T) {
	host := newSafetyHost()
	link := &Link{host: host, state: stateLinked, auth: libgm.NewAuthData(), changes: make(chan any, 2)}

	for range 100 {
		link.queueEvent(&events.HackySetActiveMayFail{})
	}
	if queued := len(link.changes); queued != 0 {
		t.Fatalf("%d unhandled events entered the queue, want none", queued)
	}

	for range 3 {
		link.queueEvent(&events.NoDataReceived{})
	}
	expectStatus(t, link, `"stop_code":"event_overflow"`)
	if !host.hasEvent("link", "event_overflow") {
		t.Fatal("event_overflow was not reported to the host")
	}
}

func TestCloseRefusesLaterCalls(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateNeedsRelink, nil)
	link.Close()

	expectErrorCode(t, link.Unlink(), "link_closed")
	expectErrorCode(t, link.Pair(canaryCookieJSON(t)), "link_closed")
	expectErrorCode(t, link.Relink(), "link_closed")
	expectErrorCode(t, link.Run(), "link_closed")
}

func TestPersistFailureUsesOneCode(t *testing.T) {
	host := newSafetyHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	setSafetyState(link, stateSignedOut, nil)
	host.setFailPersist(true)

	err = link.Unlink()

	expectErrorCode(t, err, "persist_failed")
	expectStatus(t, link, `"stop_code":"persist_failed"`)
}

func TestEmptyConfigEmailIsRetryable(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = "" })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)

	done := make(chan error, 1)
	go func() { done <- link.Run() }()
	err := awaitSafetyResult(t, done, "Run")

	expectErrorCode(t, err, "run_retryable")
	expectStatus(t, link, `"state":"linked"`)
	if host.hasEvent("link", "signed_out") {
		t.Fatal("an empty config email signed the link out")
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.auth.Browser == nil {
		t.Fatal("an empty config email wiped the stored auth")
	}
}

func TestRelinkFailureKeepsRotatedCookies(t *testing.T) {
	shortSafetyDeadlines(t)
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.rotateCookie = true
		knobs.hangSignIn = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateNeedsRelink, auth)
	result := make(chan error, 1)
	go func() { result <- link.Relink() }()

	err := awaitPairResult(t, link, result, "Relink")

	expectErrorCode(t, err, "pair_timeout")
	expectStatus(t, link, `"state":"needs_relink"`, `"stop_code":"pair_timeout"`)
	want := fmt.Sprintf("rotated-%d", fixture.rotations.Load())
	if got := persistedCookies(t, host)["SAPISID"]; got != want {
		t.Fatalf("persisted SAPISID = %q after a failed relink, want the rotated %q", got, want)
	}
}

func TestHealthyWithoutBrowserActiveAlert(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	startSafetyRun(t, link)

	if latency := link.CheckHealth(); latency < 0 {
		t.Fatalf("CheckHealth = %d, want a latency on a connected link", latency)
	}

	expectStatus(t, link, `"healthy":true`)
	if host.hasEvent("browser", "active") {
		t.Fatal("a healthy connected link reported the browser-active alert")
	}
}
