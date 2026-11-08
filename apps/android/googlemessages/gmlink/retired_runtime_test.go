package gmlink

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
)

func TestActiveSessionInvalidationKeepsPhysicalSourceAdmitted(t *testing.T) {
	auth := linkedSafetyAuth()
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	link.mu.Lock()
	link.generation = 9
	link.runGeneration = 9
	link.mu.Unlock()
	client := link.newRuntimeClient(auth, 9)
	source := libgm.StreamSource{ListenID: 7, PollAttempt: 3}
	link.queueEventFor(9, &libgm.StreamOpened{Source: source})

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_ = client.SetActiveSession(ctx)
	link.queueEventFor(9, &libgm.StreamEvent{Source: source, Event: &libgm.WrappedSettings{Settings: settingsWithSIM("same-source")}})
	awaitSafety(t, "same-source settings", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.settings != nil
	})
	link.queueEventFor(9, &libgm.StreamEvent{Source: source, Event: &events.GaiaLoggedOut{}})
	awaitSafety(t, "same-source terminal completion", func() bool {
		return host.hasEvent("link", "relink_needed")
	})
	expectStatus(t, link, `"state":"needs_relink"`)
}

func TestSupersededTerminalCompletionCannotOverwriteUnlink(t *testing.T) {
	auth := linkedSafetyAuth()
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	link.mu.Lock()
	completion := link.commitTerminalLocked(link.generation, &events.GaiaLoggedOut{})
	link.mu.Unlock()
	if completion == nil {
		t.Fatal("terminal completion was not allocated")
	}
	if err := link.Unlink(); err != nil {
		t.Fatal(err)
	}
	link.completeTerminal(completion)

	last := host.lastEnvelope(t)
	if last.State != stateUnlinked {
		t.Fatalf("last persisted state = %s, want unlinked", last.State)
	}
	var stored libgm.AuthData
	if err := json.Unmarshal(last.Auth, &stored); err != nil {
		t.Fatal(err)
	}
	stored.CookiesLock.RLock()
	storedCookie := stored.Cookies["SAPISID"]
	stored.CookiesLock.RUnlock()
	if stored.Browser != nil || stored.Mobile != nil || storedCookie != "" {
		t.Fatal("superseded terminal completion restored old auth after unlink")
	}
}

type reentrantQueuedCompletionHost struct {
	link       *Link
	reached    chan struct{}
	allowClose chan struct{}
	closed     chan struct{}
}

func (*reentrantQueuedCompletionHost) PersistState([]byte) error      { return nil }
func (*reentrantQueuedCompletionHost) ShowEmoji(string, int64, int64) {}
func (host *reentrantQueuedCompletionHost) Event(_, code string, _, _, _ int64) {
	if code != "recovered" {
		return
	}
	close(host.reached)
	<-host.allowClose
	host.link.Close()
	close(host.closed)
}

func TestReentrantCloseDoesNotDependOnQueuedTerminalConsumer(t *testing.T) {
	host := &reentrantQueuedCompletionHost{
		reached:    make(chan struct{}),
		allowClose: make(chan struct{}),
		closed:     make(chan struct{}),
	}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	host.link = link
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()
	link.queueEvent(&events.ListenRecovered{})
	<-host.reached
	link.queueEvent(&events.GaiaLoggedOut{})
	close(host.allowClose)
	select {
	case <-host.closed:
	case <-time.After(time.Second):
		t.Fatal("reentrant Close waited on a terminal completion queued behind itself")
	}
}

func TestDelayedTokenSnapshotCannotOverwriteLaterCookieRotation(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	newer := linkedSafetyAuth()
	newer.SetCookies(map[string]string{"SAPISID": "newer"})
	older := linkedSafetyAuth()
	older.SetCookies(map[string]string{"SAPISID": "older"})
	link.mu.Lock()
	link.state = stateLinked
	link.generation = 4
	link.runGeneration = 4
	link.auth = newer
	link.mu.Unlock()

	if err := link.persistRuntimeAuthSnapshot(4, 2, newer); err != nil {
		t.Fatal(err)
	}
	if err := link.persistRuntimeAuthSnapshot(4, 1, older); err != nil {
		t.Fatal(err)
	}
	persisted := persistedCookies(t, host)
	if persisted["SAPISID"] != "newer" {
		t.Fatalf("persisted cookie = %q, want newer", persisted["SAPISID"])
	}
}

func TestPhoneNotRespondingRevokesOwnerEvidenceAtIngress(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	setTestOwnerVerification(link)
	link.queueEvent(&events.PhoneNotResponding{})
	link.mu.Lock()
	evidence := link.ownerVerification
	link.mu.Unlock()
	if evidence != nil {
		t.Fatal("PhoneNotResponding retained owner evidence")
	}
}

type blockingEmojiHost struct {
	entered   chan struct{}
	release   chan struct{}
	delivered chan struct{}
}

func (*blockingEmojiHost) PersistState([]byte) error                 { return nil }
func (*blockingEmojiHost) Event(string, string, int64, int64, int64) {}
func (host *blockingEmojiHost) ShowEmoji(string, int64, int64) {
	close(host.entered)
	<-host.release
	close(host.delivered)
}

func TestCloseKeepsReplacementFencedUntilAdmittedEmojiReturns(t *testing.T) {
	host := &blockingEmojiHost{entered: make(chan struct{}), release: make(chan struct{}), delivered: make(chan struct{})}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	link.mu.Lock()
	link.generation = 3
	link.pairOwner = 3
	link.mu.Unlock()
	go link.showEmojiFor(3, "synthetic-emoji")
	<-host.entered

	link.Close()
	replacement, err := Open(nil, newSafetyHost())
	if err == nil {
		replacement.Close()
		close(host.release)
		t.Fatal("replacement opened while an old admitted emoji callback was still running")
	}
	if err.Error() != "link_already_open" {
		close(host.release)
		t.Fatalf("replacement Open error = %v", err)
	}
	close(host.release)
	<-host.delivered
	awaitSafety(t, "old output slot release", func() bool {
		replacement, openErr := Open(nil, newSafetyHost())
		if openErr != nil {
			return false
		}
		replacement.Close()
		return true
	})
}

func (*reentrantQueuedCompletionHost) RcsEvent(string, int64, int64, int64)    {}
func (*blockingEmojiHost) RcsEvent(string, int64, int64, int64)                {}
func (*reentrantQueuedCompletionHost) TypingEvent(string, int64, int64, int64) {}
func (*blockingEmojiHost) TypingEvent(string, int64, int64, int64)             {}
