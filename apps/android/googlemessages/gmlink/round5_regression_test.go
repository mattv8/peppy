package gmlink

import (
	"sync"
	"testing"
	"time"
)

type reentrantOperationHost struct {
	once      sync.Once
	operation func() error
	result    chan error
}

func (*reentrantOperationHost) PersistState([]byte) error { return nil }
func (*reentrantOperationHost) ShowEmoji(string)          {}
func (host *reentrantOperationHost) Event(string, string, int64) {
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
