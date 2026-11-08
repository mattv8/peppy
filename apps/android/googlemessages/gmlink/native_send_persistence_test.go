package gmlink

import (
	"encoding/json"
	"sync"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
)

// evidencePersistHost keeps the existing safetyHost byte capture while allowing a test
// to prove a successful persistence has happened before the lifecycle call returns.
type evidencePersistHost struct {
	*safetyHost
	mu        sync.Mutex
	holdState string
	reached   chan struct{}
	release   chan struct{}
}

func newEvidencePersistHost() *evidencePersistHost {
	return &evidencePersistHost{safetyHost: newSafetyHost()}
}

func (host *evidencePersistHost) holdNextPersist(state string) (<-chan struct{}, chan<- struct{}) {
	host.mu.Lock()
	defer host.mu.Unlock()
	host.holdState = state
	host.reached = make(chan struct{})
	host.release = make(chan struct{})
	return host.reached, host.release
}

func (host *evidencePersistHost) PersistState(state []byte) error {
	if err := host.safetyHost.PersistState(state); err != nil {
		return err
	}
	host.mu.Lock()
	hold := host.holdState != "" && stateContains(state, host.holdState)
	reached, release := host.reached, host.release
	if hold {
		host.holdState = ""
	}
	host.mu.Unlock()
	if hold {
		close(reached)
		<-release
	}
	return nil
}

func stateContains(state []byte, want string) bool {
	var envelope persistedState
	return json.Unmarshal(state, &envelope) == nil && envelope.State == want
}

func closeEvidenceLink(t *testing.T, link *Link) {
	t.Helper()
	link.Close()
	awaitSafety(t, "link close", func() bool { return openLink.Load() != link })
}

func pairedEvidenceLink(t *testing.T) (*Link, *evidencePersistHost) {
	t.Helper()
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
		knobs.gaiaFinish = true
	})
	host := newEvidencePersistHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	candidate := libgm.NewAuthData()
	candidate.SetCookies(syntheticCookies())
	if err := awaitPair(t, link, func() error { return link.pair(candidate, false, "") }); err != nil {
		closeEvidenceLink(t, link)
		t.Fatalf("Pair() error = %v", err)
	}
	return link, host
}

func awaitPair(t *testing.T, link *Link, call func() error) error {
	t.Helper()
	result := make(chan error, 1)
	go func() { result <- call() }()
	return awaitPairResult(t, link, result, "pair lifecycle")
}

func requireEvidence(t *testing.T, state persistedState) string {
	t.Helper()
	if state.PairingEvidenceID == "" {
		t.Fatal("persisted pairing evidence is empty")
	}
	return state.PairingEvidenceID
}

func TestNativeSendPairingEvidencePersistsAcrossLifecycle(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
		knobs.gaiaFinish = true
	})
	host := newEvidencePersistHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	reached, release := host.holdNextPersist(stateLinked)
	var releaseOnce sync.Once
	releasePersist := func() { releaseOnce.Do(func() { close(release) }) }
	t.Cleanup(func() { releasePersist(); closeEvidenceLink(t, link) })
	candidate := libgm.NewAuthData()
	candidate.SetCookies(syntheticCookies())
	result := make(chan error, 1)
	go func() { result <- link.pair(candidate, false, "") }()
	select {
	case <-reached:
	case err := <-result:
		t.Fatalf("Pair returned before the linked persistence barrier: %v", err)
	case <-time.After(5 * time.Second):
		t.Fatal("Pair did not reach linked persistence")
	}
	state := host.lastEnvelope(t)
	if state.State != stateLinked {
		t.Fatalf("persisted state = %q, want linked", state.State)
	}
	evidence := requireEvidence(t, state)
	select {
	case err := <-result:
		t.Fatalf("Pair returned before its successful persistence completed: %v", err)
	default:
	}
	releasePersist()
	if err := awaitPairResult(t, link, result, "Pair"); err != nil {
		t.Fatal(err)
	}
	bytes := append([]byte(nil), host.statesSince(0)[host.stateCount()-1]...)
	link.mu.Lock()
	originalRuntimeID := link.runIncarnationID
	link.mu.Unlock()
	closeEvidenceLink(t, link)

	reopenedHost := newSafetyHost()
	reopened, err := Open(bytes, reopenedHost)
	if err != nil {
		t.Fatalf("Open(persisted state) error = %v", err)
	}
	defer closeEvidenceLink(t, reopened)
	reopened.mu.Lock()
	gotEvidence, runtimeID := reopened.pairingEvidenceID, reopened.runIncarnationID
	reopened.mu.Unlock()
	if gotEvidence != evidence {
		t.Fatalf("reopened evidence = %q, want %q", gotEvidence, evidence)
	}
	if runtimeID == "" || runtimeID == originalRuntimeID {
		t.Fatalf("reopened runtime incarnation = %q, want a new UUID distinct from %q", runtimeID, originalRuntimeID)
	}
}

func TestNativeSendPairingEvidenceRelinkUnlinkAndRePair(t *testing.T) {
	link, host := pairedEvidenceLink(t)
	defer closeEvidenceLink(t, link)
	original := requireEvidence(t, host.lastEnvelope(t))

	link.mu.Lock()
	link.state = stateNeedsRelink
	link.mu.Unlock()
	failedFixture := newPhoneFixture(t, nil)
	failedFixture.configure(func(knobs *fixtureKnobs) { knobs.configRedirect = true })
	if err := awaitPair(t, link, link.Relink); err == nil {
		t.Fatal("failed Relink succeeded")
	}
	if got := requireEvidence(t, host.lastEnvelope(t)); got != original {
		t.Fatalf("failed Relink evidence = %q, want %q", got, original)
	}
	// A refused cookie redirect signs the link out. A successful repair must start
	// from a separate genuine linked registration, not relabel that terminal state.
	closeEvidenceLink(t, link)
	link, host = pairedEvidenceLink(t)
	defer closeEvidenceLink(t, link)
	original = requireEvidence(t, host.lastEnvelope(t))
	link.mu.Lock()
	link.state = stateNeedsRelink
	link.mu.Unlock()

	successFixture := newPhoneFixture(t, nil)
	successFixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
		knobs.gaiaFinish = true
	})
	if err := awaitPair(t, link, link.Relink); err != nil {
		t.Fatalf("successful Relink error = %v", err)
	}
	if got := requireEvidence(t, host.lastEnvelope(t)); got != original {
		t.Fatalf("successful Relink evidence = %q, want %q", got, original)
	}
	if err := link.Unlink(); err != nil {
		t.Fatalf("Unlink error = %v", err)
	}
	if got := host.lastEnvelope(t).PairingEvidenceID; got != "" {
		t.Fatalf("Unlink evidence = %q, want empty", got)
	}
	replacement := libgm.NewAuthData()
	replacement.SetCookies(syntheticCookies())
	if err := awaitPair(t, link, func() error { return link.pair(replacement, false, "") }); err != nil {
		t.Fatalf("fresh Pair error = %v", err)
	}
	if got := requireEvidence(t, host.lastEnvelope(t)); got == original {
		t.Fatalf("fresh Pair evidence = %q, want rotation from %q", got, original)
	}
}

func TestNativeSendPairingEvidenceSurvivesTerminalAndRuntimePersistence(t *testing.T) {
	link, host := pairedEvidenceLink(t)
	defer closeEvidenceLink(t, link)
	evidence := requireEvidence(t, host.lastEnvelope(t))

	link.mu.Lock()
	link.runGeneration = link.generation
	generation := link.generation
	link.mu.Unlock()
	if err := link.persistRuntimeAuthSnapshot(generation, 1, linkedSafetyAuth()); err != nil {
		t.Fatalf("runtime auth persistence error = %v", err)
	}
	if got := requireEvidence(t, host.lastEnvelope(t)); got != evidence {
		t.Fatalf("runtime auth evidence = %q, want %q", got, evidence)
	}
	link.queueEventFor(generation, &events.AccountChange{})
	awaitSafety(t, "terminal persistence", func() bool { return host.lastEnvelope(t).State == stateSignedOut })
	if got := requireEvidence(t, host.lastEnvelope(t)); got != evidence {
		t.Fatalf("terminal evidence = %q, want %q", got, evidence)
	}
}

func TestNativeSendLegacyLinkedStateFailsClosedWithoutWriting(t *testing.T) {
	link, host := pairedEvidenceLink(t)
	bytes := append([]byte(nil), host.statesSince(0)[host.stateCount()-1]...)
	closeEvidenceLink(t, link)
	var legacy map[string]json.RawMessage
	if err := json.Unmarshal(bytes, &legacy); err != nil {
		t.Fatal(err)
	}
	delete(legacy, "pairing_evidence_id")
	legacyBytes, err := json.Marshal(legacy)
	if err != nil {
		t.Fatal(err)
	}
	hostAfterOpen := newSafetyHost()
	if _, err := Open(legacyBytes, hostAfterOpen); err == nil || err.Error() != "missing_pairing_evidence" {
		t.Fatalf("Open(legacy linked state) error = %v, want missing_pairing_evidence", err)
	}
	if got := hostAfterOpen.stateCount(); got != 0 {
		t.Fatalf("legacy Open persisted %d states, want none", got)
	}
}
