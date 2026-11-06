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

	select {
	case <-host.secondEntered:
		close(host.releaseFirst)
		t.Fatal("later Host output entered before the earlier Host call returned")
	case <-time.After(100 * time.Millisecond):
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

	time.Sleep(100 * time.Millisecond)
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

	select {
	case <-secondCapture:
		candidate.SetCookies(map[string]string{"SAPISID": "newer"})
		close(releaseFirst)
	case <-time.After(100 * time.Millisecond):
		candidate.SetCookies(map[string]string{"SAPISID": "newer"})
		close(releaseFirst)
		<-secondCapture
	}
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
