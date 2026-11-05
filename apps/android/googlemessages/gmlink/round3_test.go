package gmlink

import (
	"bytes"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

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
	reached, release := host.holdNextPersist("needs_relink")
	relinkResult := make(chan error, 1)
	go func() { relinkResult <- link.Relink() }()
	awaitEmoji(t, host)

	unlinkResult := make(chan error, 1)
	go func() { unlinkResult <- link.Unlink() }()
	select {
	case <-reached:
	case <-time.After(10 * time.Second):
		t.Fatal("the relink never began to unwind")
	}
	closed := make(chan struct{})
	go func() {
		link.Close()
		close(closed)
	}()
	// Pair refuses only after Close has set closed, so this call is the sync point.
	expectErrorCode(t, callWithin(t, "Pair after Close", func() error { return link.Pair(canaryCookieJSON(t)) }), "link_closed")
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
	stale := auth.CopyForRepair()
	staleClient := link.newClient(stale, link.persistRotated(stale, firstOwner))
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
