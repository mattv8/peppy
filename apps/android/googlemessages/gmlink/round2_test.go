package gmlink

import (
	"testing"
	"time"
)

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
	select {
	case err := <-unlinkResult:
		close(release)
		t.Fatalf("Unlink returned (%v) before the in-flight commit finished", err)
	case <-time.After(100 * time.Millisecond):
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
