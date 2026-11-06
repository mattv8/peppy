package gmlink

import (
	"sync"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
)

type reentrantOperationHost struct {
	once      sync.Once
	operation func() error
	result    chan error
}

func (*reentrantOperationHost) PersistState([]byte) error      { return nil }
func (*reentrantOperationHost) ShowEmoji(string, int64, int64) {}
func (host *reentrantOperationHost) Event(string, string, int64, int64, int64) {
	host.once.Do(func() { host.result <- host.operation() })
}

func TestHostCallbackCanReenterPair(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configRedirect = true })
	host := &reentrantOperationHost{result: make(chan error, 1)}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	setSafetyState(link, stateSignedOut, nil)
	host.operation = func() error { return link.Pair(canaryCookieJSON(t)) }

	go link.emit("conn", "recovered", 0)
	select {
	case err := <-host.result:
		expectErrorCode(t, err, "cookie_refused")
	case <-time.After(5 * time.Second):
		t.Fatal("Host callback deadlocked while reentering Pair")
	}
	link.Close()
	awaitSafety(t, "reentrant Pair output drain", func() bool { return openLink.Load() != link })
}

func TestHostCallbackCanReenterUnlink(t *testing.T) {
	host := &reentrantOperationHost{result: make(chan error, 1)}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	setSafetyState(link, stateLinked, nil)
	host.operation = link.Unlink

	go link.emit("conn", "recovered", 0)
	select {
	case err := <-host.result:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Host callback deadlocked while reentering Unlink")
	}
	link.Close()
	awaitSafety(t, "reentrant Unlink output drain", func() bool { return openLink.Load() != link })
}

type ownedIdentityHost struct {
	identity chan [2]int64
}

func (*ownedIdentityHost) PersistState([]byte) error { return nil }
func (host *ownedIdentityHost) Event(_ string, _ string, _, generation, revision int64) {
	host.identity <- [2]int64{generation, revision}
}
func (*ownedIdentityHost) ShowEmoji(string, int64, int64) {}

func TestTerminalCompletionCarriesRetirementIdentity(t *testing.T) {
	host := &ownedIdentityHost{identity: make(chan [2]int64, 1)}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	defer link.Close()
	setSafetyState(link, stateLinked, nil)
	link.mu.Lock()
	link.generation = 7
	completion := link.commitTerminalLocked(7, &events.AccountChange{})
	currentGeneration := link.generation
	link.mu.Unlock()
	if completion == nil {
		t.Fatal("terminal completion was not allocated")
	}
	identityAccepted := link.AcceptsOutput(int64(completion.generation), int64(completion.revision))
	link.completeTerminal(completion)
	if completion.generation != currentGeneration {
		t.Fatalf("terminal generation = %d, want retirement generation %d", completion.generation, currentGeneration)
	}
	if !identityAccepted {
		t.Fatal("current terminal identity was rejected")
	}
	select {
	case identity := <-host.identity:
		if identity != [2]int64{int64(completion.generation), int64(completion.revision)} {
			t.Fatalf("delivered terminal identity = %v", identity)
		}
	case <-time.After(time.Second):
		t.Fatal("current terminal output was not delivered")
	}

	link.mu.Lock()
	link.stateRevision++
	link.revokeLocked()
	link.mu.Unlock()
	if link.AcceptsOutput(int64(completion.generation), int64(completion.revision)) {
		t.Fatal("superseded terminal identity was accepted")
	}
}

func (*reentrantOperationHost) RcsEvent(string, int64, int64, int64)    {}
func (*ownedIdentityHost) RcsEvent(string, int64, int64, int64)         {}
func (*reentrantOperationHost) TypingEvent(string, int64, int64, int64) {}
func (*ownedIdentityHost) TypingEvent(string, int64, int64, int64)      {}
