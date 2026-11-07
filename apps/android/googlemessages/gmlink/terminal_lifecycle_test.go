package gmlink

import (
	"encoding/json"
	"fmt"
	"testing"

	"bytes"
	"errors"
	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"sync"
	"time"
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
	awaitSafety(t, "terminal completion report", func() bool {
		return host.hasEvent("link", "relink_needed")
	})
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
			host.awaitEvent(t, "link", "account_mismatch")
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
	if host.hasEvent("link", "event_overflow") {
		t.Fatal("overflow ingress called the host")
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

// TestLateRotationAfterUnlinkWritesNothing delivers a cookie rotation through a relink's
// client after the link was unlinked. The rotation must not reach the stored auth or the
// envelope.
func TestLateRotationAfterUnlinkWritesNothing(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.rotateCookie = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateNeedsRelink, auth)
	candidate := auth.CopyForRepair()
	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()
	client := link.newClient(candidate, link.persistRotatedFor(owner, candidate, make(chan struct{})), owner)
	t.Cleanup(client.Disconnect)

	if err := link.Unlink(); err != nil {
		t.Fatalf("Unlink error = %v, want nil", err)
	}
	if err := client.FetchConfig(t.Context()); err != nil {
		t.Fatalf("config fetch error = %v", err)
	}

	if fixture.rotations.Load() == 0 {
		t.Fatal("the rotation never reached the transport")
	}
	assertAuthWiped(t, host)
	expectStatus(t, link, `"state":"unlinked"`)
}

// TestUnlinkWaitsForCommittingPair holds a successful pairing inside its commit. Unlink
// arrives before the commit lands, so it must wait for the pair, then unpair the new
// pairing once and end unlinked.
func TestUnlinkWaitsForCommittingPair(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
		knobs.gaiaFinish = true
	})
	link, host := openSafetyLink(t, nil)
	reached, release := host.holdNextPersist("linked")
	pairResult := make(chan error, 1)
	go func() { pairResult <- link.Pair(canaryCookieJSON(t)) }()
	awaitEmoji(t, host)

	select {
	case <-reached:
	case <-time.After(10 * time.Second):
		t.Fatal("the pairing never reached its commit")
	}
	unlinkResult := make(chan error, 1)
	go func() { unlinkResult <- link.Unlink() }()
	awaitSafety(t, "Unlink admission", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.unlinking
	})
	select {
	case err := <-unlinkResult:
		close(release)
		t.Fatalf("Unlink returned (%v) before the in-flight commit finished", err)
	default:
	}
	close(release)

	if err := awaitPairResult(t, link, pairResult, "Pair"); err != nil {
		t.Fatalf("Pair error = %v, want nil: the commit had already succeeded", err)
	}
	if err := awaitSafetyResult(t, unlinkResult, "Unlink"); err != nil {
		t.Fatalf("Unlink error = %v, want nil", err)
	}
	if got := fixture.countAction("UNPAIR_GAIA_PAIRING"); got != 1 {
		t.Fatalf("unpair POSTs = %d, want exactly 1 for the committed pairing", got)
	}
	expectStatus(t, link, `"state":"unlinked"`)
	assertAuthWiped(t, host)
}

// TestUnlinkHoldRefusesPairAndSecondUnlink holds the Gaia unpair. While it is held, Pair,
// Relink, Run and a second Unlink are refused, and the second Unlink sends no request.
// After release the link ends unlinked, wiped, with no pairing persisted.
func TestUnlinkHoldRefusesPairAndSecondUnlink(t *testing.T) {
	auth := linkedSafetyAuth()
	auth.PairingID = uuid.New()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gateSend = true
		knobs.gateAction = gmproto.ActionType_UNPAIR_GAIA_PAIRING
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	unlinkResult := make(chan error, 1)
	go func() { unlinkResult <- link.Unlink() }()
	awaitSafety(t, "the unpair request to arrive", func() bool { return fixture.countAction("UNPAIR_GAIA_PAIRING") == 1 })
	before := host.stateCount()

	expectErrorCode(t, callWithin(t, "Pair during unlink", func() error { return link.Pair(canaryCookieJSON(t)) }), "unlink_in_progress")
	expectErrorCode(t, callWithin(t, "Relink during unlink", link.Relink), "unlink_in_progress")
	expectErrorCode(t, callWithin(t, "second Unlink", link.Unlink), "unlink_in_progress")
	expectErrorCode(t, callWithin(t, "Run during unlink", link.Run), "run_refused")

	fixture.openGate()
	if err := awaitSafetyResult(t, unlinkResult, "Unlink"); err != nil {
		t.Fatalf("Unlink error = %v, want nil", err)
	}
	if got := fixture.countAction("UNPAIR_GAIA_PAIRING"); got != 1 {
		t.Fatalf("unpair requests = %d, want exactly 1", got)
	}
	expectStatus(t, link, `"state":"unlinked"`, `"stop_code":""`)
	assertAuthWiped(t, host)
	for _, envelope := range host.statesSince(before) {
		if bytes.Contains(envelope, []byte(`"state":"pairing"`)) {
			t.Fatalf("a pairing was persisted during or after the unlink: %s", envelope)
		}
	}
}

// TestUnlinkClosedBeforeUnpairSendsNothing: a pair unwinds while Unlink waits; Close
// lands before the unwind finishes. Unlink must return link_closed with no unpair request.
func TestUnlinkClosedBeforeUnpairSendsNothing(t *testing.T) {
	auth := linkedSafetyAuth()
	auth.PairingID = uuid.New()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateNeedsRelink, auth)
	reached, release := host.holdNextPersist(statePairing)
	released := false
	defer func() {
		if !released {
			close(release)
		}
	}()
	relinkResult := make(chan error, 1)
	go func() { relinkResult <- link.Relink() }()
	select {
	case <-reached:
	case <-time.After(10 * time.Second):
		t.Fatal("the relink never reached pairing persistence")
	}

	unlinkResult := make(chan error, 1)
	go func() { unlinkResult <- link.Unlink() }()
	awaitSafety(t, "the unlink to own cancellation", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.unlinking
	})
	closed := make(chan struct{})
	go func() {
		link.Close()
		close(closed)
	}()
	awaitSafety(t, "Close to revoke the link", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.closed
	})
	expectErrorCode(t, callWithin(t, "Pair after Close", func() error { return link.Pair(canaryCookieJSON(t)) }), "link_closed")
	released = true
	close(release)
	<-closed

	expectErrorCode(t, awaitSafetyResult(t, unlinkResult, "Unlink"), "link_closed")
	if got := fixture.countAction("UNPAIR_GAIA_PAIRING"); got != 0 {
		t.Fatalf("unpair requests = %d after Close, want none", got)
	}
	if err := awaitPairResult(t, link, relinkResult, "Relink"); err == nil {
		t.Fatal("Relink succeeded, want it cancelled")
	}
}

// currentPairOwner returns the token of the pair in progress.
func currentPairOwner(link *Link) chan struct{} {
	link.mu.Lock()
	defer link.mu.Unlock()
	return link.pairDone
}

// TestStaleRelinkRotationCannotWriteIntoNewPair: a relink client bound to an earlier pair
// delivers a cookie rotation while a newer pair is pairing. The newer pair's stored auth
// must not take the stale cookies.
func TestStaleRelinkRotationCannotWriteIntoNewPair(t *testing.T) {
	previousWait := pairWaitTimeout
	t.Cleanup(func() { pairWaitTimeout = previousWait })

	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateNeedsRelink, auth)

	// First relink: the phone never confirms the emoji, so the short wait ends it.
	pairWaitTimeout = 100 * time.Millisecond
	first := make(chan error, 1)
	go func() { first <- link.Relink() }()
	awaitEmoji(t, host)
	firstOwner := currentPairOwner(link)
	link.mu.Lock()
	firstOperation := link.pairOwner
	link.mu.Unlock()
	stale := auth.CopyForRepair()
	staleClient := link.newClient(stale, link.persistRotatedFor(firstOperation, stale, firstOwner), firstOperation)
	t.Cleanup(staleClient.Disconnect)
	if err := awaitPairResult(t, link, first, "first Relink"); err == nil {
		t.Fatal("first Relink succeeded, want pair_timeout")
	}
	pairWaitTimeout = previousWait

	// Second relink: pairing again, waiting on the emoji.
	second := make(chan error, 1)
	go func() { second <- link.Relink() }()
	awaitEmoji(t, host)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.rotateCookie = true })
	if err := staleClient.FetchConfig(t.Context()); err != nil {
		t.Fatalf("stale config fetch error = %v", err)
	}
	if fixture.rotations.Load() == 0 {
		t.Fatal("the stale rotation never reached the transport")
	}

	link.mu.Lock()
	link.auth.CookiesLock.RLock()
	storedSAPISID := link.auth.Cookies["SAPISID"]
	link.auth.CookiesLock.RUnlock()
	link.mu.Unlock()
	if storedSAPISID != "synthetic" {
		t.Fatalf("the newer pair's stored SAPISID = %q, want the original: a stale rotation wrote into it", storedSAPISID)
	}
	link.Close()
	_ = awaitPairResult(t, link, second, "second Relink")
}

// TestOldPairCleanupKeepsNewPairOwnership parks an old pair inside its success emit, after
// its commit and while its deferred cleanup is still pending. The link is signed out and a
// new pair starts. When the old pair then cleans up, the new pair must still own the link:
// its rotations persist and Close cancels it.
func TestOldPairCleanupKeepsNewPairOwnership(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
		knobs.gaiaFinish = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateSignedOut, nil)
	reached, release := make(chan struct{}), make(chan struct{})
	var cleanupOnce sync.Once
	link.pairCleanupReady = func(uint64) {
		cleanupOnce.Do(func() {
			close(reached)
			<-release
		})
	}

	oldResult := make(chan error, 1)
	go func() { oldResult <- link.Pair(canaryCookieJSON(t)) }()
	awaitEmoji(t, host)
	select {
	case <-reached:
	case <-time.After(10 * time.Second):
		t.Fatal("the old pair never reached deferred cleanup")
	}

	// Sign the link out while the old pair still holds its cleanup.
	link.queueEvent(&events.AccountChange{})
	awaitSafety(t, "the sign-out to land", func() bool { return host.countEvent("link", "signed_out") == 1 })

	// The new pair must not complete on its own: it stays pairing until the test closes the link.
	fixture.configure(func(knobs *fixtureKnobs) { knobs.gaiaFinish = false })
	newResult := make(chan error, 1)
	go func() { newResult <- link.Pair(canaryCookieJSON(t)) }()
	awaitEmoji(t, host)
	newOwner := currentPairOwner(link)
	link.mu.Lock()
	newOperation := link.pairOwner
	link.mu.Unlock()

	close(release)
	if err := awaitPairResult(t, link, oldResult, "old Pair"); err != nil {
		t.Fatalf("old Pair error = %v, want nil: its commit had succeeded", err)
	}
	expectStatus(t, link, `"state":"pairing"`)

	// The new pair's own client rotates cookies; they must still persist.
	fixture.configure(func(knobs *fixtureKnobs) { knobs.rotateCookie = true })
	candidate := libgm.NewAuthData()
	candidate.SetCookies(syntheticCookies())
	client := link.newClient(candidate, link.persistRotatedFor(newOperation, candidate, newOwner), newOperation)
	t.Cleanup(client.Disconnect)
	if err := client.FetchConfig(t.Context()); err != nil {
		t.Fatalf("config fetch error = %v", err)
	}
	link.mu.Lock()
	link.auth.CookiesLock.RLock()
	stored := link.auth.Cookies["SAPISID"]
	link.auth.CookiesLock.RUnlock()
	link.mu.Unlock()
	if want := fmt.Sprintf("rotated-%d", fixture.rotations.Load()); stored != want {
		t.Fatalf("the new pair's rotation did not persist after the old cleanup: stored SAPISID = %q, want %q", stored, want)
	}

	// Close must still reach the new pair.
	link.Close()
	err := awaitPairResult(t, link, newResult, "new Pair")
	expectErrorCode(t, err, "pair_cancelled")
}

// TestEventsIgnoredWhileUnlinking holds the unpair. Events queued meanwhile, from a stopped
// Run client, must not persist needs_relink or signed_out nor report relink_needed or
// signed_out: the link must go from linked to unlinked.
func TestEventsIgnoredWhileUnlinking(t *testing.T) {
	auth := linkedSafetyAuth()
	auth.PairingID = uuid.New()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gateSend = true
		knobs.gateAction = gmproto.ActionType_UNPAIR_GAIA_PAIRING
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	unlinkResult := make(chan error, 1)
	go func() { unlinkResult <- link.Unlink() }()
	awaitSafety(t, "the unpair request to arrive", func() bool { return fixture.countAction("UNPAIR_GAIA_PAIRING") == 1 })
	before := host.stateCount()
	relinkBefore := host.countEvent("link", "relink_needed")
	signedBefore := host.countEvent("link", "signed_out")

	link.queueEvent(&events.GaiaLoggedOut{})
	link.queueEvent(&events.ListenFatalError{Error: errors.New("synthetic transient failure")})
	link.queueEvent(&events.AccountChange{})
	awaitProcessed(t, link, host)

	for _, envelope := range host.statesSince(before) {
		if bytes.Contains(envelope, []byte(`"state":"needs_relink"`)) || bytes.Contains(envelope, []byte(`"state":"signed_out"`)) {
			t.Fatalf("an event persisted a state during unlink: %s", envelope)
		}
	}
	if host.countEvent("link", "relink_needed") != relinkBefore || host.countEvent("link", "signed_out") != signedBefore {
		t.Fatal("an event reported relink_needed or signed_out during unlink")
	}

	fixture.openGate()
	if err := awaitSafetyResult(t, unlinkResult, "Unlink"); err != nil {
		t.Fatalf("Unlink error = %v, want nil", err)
	}
	expectStatus(t, link, `"state":"unlinked"`)
	assertAuthWiped(t, host)
}
