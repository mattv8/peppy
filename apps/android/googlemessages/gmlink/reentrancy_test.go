package gmlink

import (
	"encoding/json"
	"sync"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
)

type orderedOutputHost struct {
	firstEntered  chan struct{}
	releaseFirst  chan struct{}
	secondEntered chan struct{}
}

func (*orderedOutputHost) PersistState([]byte) error      { return nil }
func (*orderedOutputHost) ShowEmoji(string, int64, int64) {}
func (host *orderedOutputHost) Event(_, code string, _, _, _ int64) {
	switch code {
	case "recovered":
		close(host.firstEntered)
		<-host.releaseFirst
	case "temporary_error":
		close(host.secondEntered)
	}
}

func TestHostOutputsPreserveAdmissionOrder(t *testing.T) {
	host := &orderedOutputHost{
		firstEntered:  make(chan struct{}),
		releaseFirst:  make(chan struct{}),
		secondEntered: make(chan struct{}),
	}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	defer link.Close()
	go link.emit("conn", "recovered", 0)
	<-host.firstEntered
	go link.emit("conn", "temporary_error", 0)

	awaitSafety(t, "later output admission", func() bool {
		link.outputMu.Lock()
		defer link.outputMu.Unlock()
		return len(link.outputQueue) == 1
	})
	select {
	case <-host.secondEntered:
		close(host.releaseFirst)
		t.Fatal("later Host output entered before the earlier Host call returned")
	default:
	}
	close(host.releaseFirst)
	select {
	case <-host.secondEntered:
	case <-time.After(time.Second):
		t.Fatal("later Host output did not run after the earlier Host call returned")
	}
}

type replacementFenceHost struct {
	mu         sync.Mutex
	sequence   []string
	oldEntered chan struct{}
	releaseOld chan struct{}
}

func (*replacementFenceHost) PersistState([]byte) error      { return nil }
func (*replacementFenceHost) ShowEmoji(string, int64, int64) {}
func (host *replacementFenceHost) Event(_, code string, _, _, _ int64) {
	host.mu.Lock()
	host.sequence = append(host.sequence, code)
	host.mu.Unlock()
	if code == "recovered" {
		close(host.oldEntered)
		<-host.releaseOld
	}
}
func (host *replacementFenceHost) index(value string) int {
	host.mu.Lock()
	defer host.mu.Unlock()
	for index, item := range host.sequence {
		if item == value {
			return index
		}
	}
	return -1
}

func TestOldOutputEntersHostBeforeReplacementPairAdmission(t *testing.T) {
	host := &replacementFenceHost{oldEntered: make(chan struct{}), releaseOld: make(chan struct{})}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	defer link.Close()
	setSafetyState(link, stateLinked, nil)
	go link.emitFor(link.generation, "conn", "recovered", 0)
	<-host.oldEntered

	unlinkDone := make(chan error, 1)
	go func() { unlinkDone <- link.Unlink() }()
	if err := <-unlinkDone; err != nil {
		t.Fatal(err)
	}

	candidate := libgm.NewAuthData()
	candidate.SetCookies(syntheticCookies())
	fixture := newPhoneFixture(t, candidate)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gateConfig = true
	})
	pairDone := make(chan error, 1)
	go func() { pairDone <- link.pair(candidate, false, "") }()
	awaitSafety(t, "replacement Pair admission", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.state == statePairing
	})
	host.mu.Lock()
	host.sequence = append(host.sequence, "pair_admitted")
	host.mu.Unlock()
	close(host.releaseOld)
	link.Close()
	<-pairDone

	oldIndex := host.index("recovered")
	pairIndex := host.index("pair_admitted")
	if oldIndex < 0 || pairIndex < 0 || oldIndex > pairIndex {
		t.Fatalf("Host sequence = %v, old output entered after replacement Pair admission", host.sequence)
	}
}

func TestReplacementPairInvalidatesDelayedTerminalOutput(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, nil)
	link.mu.Lock()
	completion := link.commitTerminalLocked(link.generation, &events.AccountChange{})
	link.mu.Unlock()
	if completion == nil {
		t.Fatal("terminal completion was not allocated")
	}

	completionReady := make(chan struct{})
	releaseCompletion := make(chan struct{})
	link.completionOutputReady = func() {
		close(completionReady)
		<-releaseCompletion
	}
	go link.completeTerminal(completion)
	<-completion.done
	<-completionReady

	candidate := libgm.NewAuthData()
	candidate.SetCookies(syntheticCookies())
	fixture := newPhoneFixture(t, candidate)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gateConfig = true
	})
	pairDone := make(chan error, 1)
	go func() { pairDone <- link.pair(candidate, false, "") }()
	awaitSafety(t, "replacement Pair admission", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.state == statePairing
	})
	close(releaseCompletion)

	<-completion.outputQueued
	if host.hasEvent("link", "signed_out") {
		t.Fatal("delayed terminal output crossed replacement Pair admission")
	}
	link.Close()
	<-pairDone
}

func TestAuthRevisionIsAllocatedWithSnapshotCapture(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	candidate := linkedSafetyAuth()
	candidate.SetCookies(map[string]string{"SAPISID": "older"})
	link.mu.Lock()
	link.state = stateLinked
	link.generation = 4
	link.runGeneration = 4
	link.auth = candidate
	link.mu.Unlock()

	firstCapture := make(chan struct{})
	releaseFirst := make(chan struct{})
	secondCapture := make(chan struct{})
	var captureMu sync.Mutex
	captureCount := 0
	link.authSnapshot = func(auth *libgm.AuthData) (*libgm.AuthData, error) {
		captureMu.Lock()
		captureCount++
		call := captureCount
		captureMu.Unlock()
		if call == 1 {
			close(firstCapture)
			<-releaseFirst
		} else if call == 2 {
			close(secondCapture)
		}
		return cloneAuthData(auth)
	}

	firstDone := make(chan error, 1)
	go func() { firstDone <- link.persistRuntimeAuth(4, candidate) }()
	<-firstCapture
	secondDone := make(chan error, 1)
	go func() { secondDone <- link.persistRuntimeAuth(4, candidate) }()

	candidate.SetCookies(map[string]string{"SAPISID": "newer"})
	close(releaseFirst)
	<-secondCapture
	if err := <-firstDone; err != nil {
		t.Fatal(err)
	}
	if err := <-secondDone; err != nil {
		t.Fatal(err)
	}
	persisted := persistedCookies(t, host)
	if persisted["SAPISID"] != "newer" {
		encoded, _ := json.Marshal(persisted)
		t.Fatalf("persisted auth = %s, want snapshot captured after the revision boundary", encoded)
	}
}

func (*orderedOutputHost) RcsEvent(string, int64, int64, int64)       {}
func (*replacementFenceHost) RcsEvent(string, int64, int64, int64)    {}
func (*orderedOutputHost) TypingEvent(string, int64, int64, int64)    {}
func (*replacementFenceHost) TypingEvent(string, int64, int64, int64) {}

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
