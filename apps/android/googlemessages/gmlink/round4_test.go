package gmlink

import (
	"bytes"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

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
