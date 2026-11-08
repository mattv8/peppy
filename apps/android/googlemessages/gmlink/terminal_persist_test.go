package gmlink

import (
	"encoding/json"
	"errors"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
)

type terminalPersistFailureHost struct {
	persistReached chan struct{}
	releasePersist chan struct{}
	ready          chan struct{}
	events         chan eventCode
}

func newTerminalPersistFailureHost() *terminalPersistFailureHost {
	return &terminalPersistFailureHost{
		persistReached: make(chan struct{}),
		releasePersist: make(chan struct{}),
		ready:          make(chan struct{}),
		events:         make(chan eventCode, 1),
	}
}

func (host *terminalPersistFailureHost) PersistState([]byte) error {
	close(host.persistReached)
	<-host.releasePersist
	return errors.New("synthetic persistence failure")
}

func (host *terminalPersistFailureHost) Event(kind, code string, _, _, _ int64) {
	if kind == "conn" && code == "ready" {
		close(host.ready)
	}
	if kind == "link" {
		host.events <- eventCode{kind, code}
	}
}

func (*terminalPersistFailureHost) RcsEvent(string, int64, int64, int64)    {}
func (*terminalPersistFailureHost) TypingEvent(string, int64, int64, int64) {}
func (*terminalPersistFailureHost) ShowEmoji(string, int64, int64)          {}

func TestTerminalIngressPersistsBeforeCloseReturns(t *testing.T) {
	host := newSafetyHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	setSafetyState(link, stateLinked, linkedSafetyAuth())

	link.persistMu.Lock()
	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()
	link.queueEventFor(owner, &events.AccountChange{})
	awaitSafety(t, "terminal completion", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.pendingCompletion != nil
	})
	closed := make(chan struct{})
	go func() {
		link.Close()
		close(closed)
	}()
	awaitSafety(t, "Close state fence", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.closed
	})
	link.persistMu.Unlock()
	select {
	case <-closed:
	case <-time.After(5 * time.Second):
		t.Fatal("Close did not drain terminal persistence")
	}

	states := host.statesSince(0)
	if len(states) != 1 {
		t.Fatalf("persisted envelopes = %d, want 1", len(states))
	}
	var terminal persistedState
	if err := json.Unmarshal(states[0], &terminal); err != nil {
		t.Fatal(err)
	}
	if terminal.State != stateSignedOut || terminal.StopCode != "signed_out" {
		t.Fatalf("terminal envelope = state %q stop_code %q", terminal.State, terminal.StopCode)
	}
	if host.countEvent("link", "signed_out") != 0 {
		t.Fatal("terminal callback escaped after Close")
	}

	reopened, err := Open(states[0], newSafetyHost())
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	if reopened.state != stateSignedOut || reopened.auth.Browser != nil {
		t.Fatal("reopened terminal envelope retained linked credentials")
	}
}

func TestConfigSignOutPersistsBeforeCloseReturns(t *testing.T) {
	for _, test := range []struct {
		name      string
		configure func(*fixtureKnobs)
	}{
		{"unauthorized", func(knobs *fixtureKnobs) { knobs.configStatus = 401 }},
		{"forbidden", func(knobs *fixtureKnobs) { knobs.configStatus = 403 }},
		{"redirect", func(knobs *fixtureKnobs) { knobs.configRedirect = true }},
	} {
		t.Run(test.name, func(t *testing.T) {
			auth := linkedSafetyAuth()
			fixture := newPhoneFixture(t, auth)
			fixture.configure(test.configure)
			host := newSafetyHost()
			link, err := Open(nil, host)
			if err != nil {
				t.Fatal(err)
			}
			setSafetyState(link, stateLinked, auth)

			link.persistMu.Lock()
			runDone := make(chan error, 1)
			go func() { runDone <- link.Run() }()
			awaitSafety(t, "in-memory sign-out", func() bool {
				link.mu.Lock()
				defer link.mu.Unlock()
				return link.state == stateSignedOut && link.auth.Browser == nil
			})
			closed := make(chan struct{})
			go func() {
				link.Close()
				close(closed)
			}()
			awaitSafety(t, "Close state fence", func() bool {
				link.mu.Lock()
				defer link.mu.Unlock()
				return link.closed
			})
			link.persistMu.Unlock()
			if err := awaitSafetyResult(t, runDone, "Run after config sign-out"); err == nil {
				t.Fatal("Run accepted a terminal config response")
			}
			select {
			case <-closed:
			case <-time.After(5 * time.Second):
				t.Fatal("Close did not drain config terminal persistence")
			}

			states := host.statesSince(0)
			if len(states) != 1 {
				t.Fatalf("persisted envelopes = %d, want 1", len(states))
			}
			var terminal persistedState
			if err := json.Unmarshal(states[0], &terminal); err != nil {
				t.Fatal(err)
			}
			if terminal.State != stateSignedOut || len(terminal.Auth) == 0 || string(terminal.Auth) == "null" {
				t.Fatalf("terminal envelope = state %q auth %s", terminal.State, terminal.Auth)
			}
			reopened, err := Open(states[0], newSafetyHost())
			if err != nil {
				t.Fatal(err)
			}
			defer reopened.Close()
			if err := reopened.Run(); err == nil {
				t.Fatal("reopened terminal envelope accepted Run")
			}
		})
	}
}

func TestUnlinkPersistsBeforeCloseReturns(t *testing.T) {
	host := newSafetyHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	setSafetyState(link, stateLinked, libgm.NewAuthData())

	link.persistMu.Lock()
	unlinked := make(chan error, 1)
	go func() { unlinked <- link.Unlink() }()
	awaitSafety(t, "in-memory unlink", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.state == stateUnlinked && link.auth.Browser == nil
	})
	closed := make(chan struct{})
	go func() {
		link.Close()
		close(closed)
	}()
	awaitSafety(t, "Close state fence", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.closed
	})
	link.persistMu.Unlock()
	if err := awaitSafetyResult(t, unlinked, "Unlink after Close"); err != errLinkClosed {
		t.Fatalf("Unlink error = %v, want %v", err, errLinkClosed)
	}
	select {
	case <-closed:
	case <-time.After(5 * time.Second):
		t.Fatal("Close did not drain unlink persistence")
	}

	states := host.statesSince(0)
	if len(states) != 1 {
		t.Fatalf("persisted envelopes = %d, want 1", len(states))
	}
	var terminal persistedState
	if err := json.Unmarshal(states[0], &terminal); err != nil {
		t.Fatal(err)
	}
	if terminal.State != stateUnlinked || len(terminal.Auth) == 0 || string(terminal.Auth) == "null" {
		t.Fatalf("unlink envelope = state %q auth %s", terminal.State, terminal.Auth)
	}
}

func TestTerminalPersistenceFailureReportsPersistFailed(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	host := newSafetyHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	defer link.Close()
	setSafetyState(link, stateLinked, auth)
	runDone := startSafetyRun(t, link)
	host.setFailPersist(true)

	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()
	link.queueEventFor(owner, &events.GaiaLoggedOut{})
	host.awaitEvent(t, "link", "persist_failed")
	if host.countEvent("link", "persist_failed") != 1 {
		t.Fatalf("persist_failed outcomes = %d, want 1", host.countEvent("link", "persist_failed"))
	}
	if err := awaitSafetyResult(t, runDone, "Run after terminal persistence failure"); err != errPersistFailed {
		t.Fatalf("Run result = %v, want %v", err, errPersistFailed)
	}
}

func TestTerminalPersistenceFailureWithoutRunReportsPersistFailed(t *testing.T) {
	host := newSafetyHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	defer link.Close()
	setSafetyState(link, stateLinked, linkedSafetyAuth())
	host.setFailPersist(true)
	link.mu.Lock()
	owner, runGeneration := link.generation, link.runGeneration
	link.mu.Unlock()
	if runGeneration != 0 {
		t.Fatal("test unexpectedly has an owning Run")
	}
	link.queueEventFor(owner, &events.GaiaLoggedOut{})
	host.awaitEvent(t, "link", "persist_failed")
	link.Close()
	if got := host.countEvent("link", "persist_failed"); got != 1 {
		t.Fatalf("persist_failed outcomes = %d, want 1", got)
	}
	if host.countEvent("link", "relink_needed") != 0 || host.countEvent("link", "signed_out") != 0 {
		t.Fatal("failed terminal persistence reported success")
	}
}

func TestRunWaitsForTerminalPersistenceFailure(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	host := newTerminalPersistFailureHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	defer link.Close()
	setSafetyState(link, stateLinked, auth)
	runDone := startSafetyRun(t, link)
	select {
	case <-host.ready:
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not report ready")
	}

	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()
	link.queueEventFor(owner, &events.GaiaLoggedOut{})
	select {
	case <-host.persistReached:
	case <-time.After(5 * time.Second):
		t.Fatal("terminal persistence did not begin")
	}
	link.mu.Lock()
	completion := link.pendingCompletion
	link.mu.Unlock()
	if completion == nil {
		t.Fatal("terminal completion is missing")
	}
	select {
	case <-completion.runWaited:
	case <-time.After(5 * time.Second):
		t.Fatal("owning Run did not wait for terminal completion")
	}
	close(host.releasePersist)
	if err := awaitSafetyResult(t, runDone, "Run after released terminal persistence failure"); err != errPersistFailed {
		t.Fatalf("Run result = %v, want %v", err, errPersistFailed)
	}
	select {
	case event := <-host.events:
		if event != (eventCode{"link", "persist_failed"}) {
			t.Fatalf("terminal failure event = %v", event)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("persist_failed outcome was not reported")
	}
}

func TestClosedTerminalPersistenceFailureReachesOwningRun(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	host := newTerminalPersistFailureHost()
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	setSafetyState(link, stateLinked, auth)
	runDone := startSafetyRun(t, link)

	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()
	link.queueEventFor(owner, &events.GaiaLoggedOut{})
	select {
	case <-host.persistReached:
	case <-time.After(5 * time.Second):
		t.Fatal("terminal persistence did not begin")
	}
	link.mu.Lock()
	completion := link.pendingCompletion
	link.mu.Unlock()
	if completion == nil {
		t.Fatal("terminal completion is missing")
	}
	closed := make(chan struct{})
	go func() {
		link.Close()
		close(closed)
	}()
	awaitSafety(t, "Close state fence", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.closed
	})
	select {
	case <-completion.runWaited:
	case <-time.After(5 * time.Second):
		t.Fatal("owning Run did not wait for terminal completion")
	}
	close(host.releasePersist)
	if err := awaitSafetyResult(t, runDone, "Run after closed terminal persistence failure"); err != errPersistFailed {
		t.Fatalf("Run result = %v, want %v", err, errPersistFailed)
	}
	select {
	case <-closed:
	case <-time.After(5 * time.Second):
		t.Fatal("Close did not drain terminal persistence")
	}
	select {
	case event := <-host.events:
		t.Fatalf("callback escaped Close: %v", event)
	default:
	}
}

func TestPhonePingIngressRecordsCurrentHealthOnly(t *testing.T) {
	host := newSafetyHost()
	current := libgm.StreamSource{ListenID: 7, PollAttempt: 3}
	link := &Link{
		host:                   host,
		state:                  stateLinked,
		auth:                   libgm.NewAuthData(),
		generation:             7,
		runGeneration:          7,
		streamEpoch:            4,
		streamSource:           current,
		streamOpen:             true,
		changes:                make(chan any, 1),
		conversationCandidates: make(map[string][]conversationSummary),
		conversationTargets:    make(map[string][]string),
	}

	link.queueEventFor(7, &libgm.StreamEvent{Source: current, Event: &events.PhonePingSucceeded{}})
	if len(link.changes) != 1 {
		t.Fatal("current ping was not queued")
	}
	link.consumeQueuedEvent(<-link.changes)
	host.awaitEvent(t, "health", "ok")
	link.mu.Lock()
	responding, healthy, lastOKMS := link.responding, link.healthy, link.lastOKMS
	link.mu.Unlock()
	if !responding || !healthy || lastOKMS == 0 {
		t.Fatalf("current ping health = responding:%t healthy:%t lastOKMS:%d", responding, healthy, lastOKMS)
	}

	link.mu.Lock()
	link.responding, link.healthy, link.lastOKMS = false, false, 0
	link.mu.Unlock()
	link.queueEventFor(6, &libgm.StreamEvent{Source: current, Event: &events.PhonePingSucceeded{}})
	link.queueEventFor(7, &libgm.StreamEvent{Source: libgm.StreamSource{ListenID: 6, PollAttempt: 2}, Event: &events.PhonePingSucceeded{}})
	if len(link.changes) != 0 {
		t.Fatal("retired ping was queued")
	}
	link.mu.Lock()
	responding, healthy, lastOKMS = link.responding, link.healthy, link.lastOKMS
	link.mu.Unlock()
	if responding || healthy || lastOKMS != 0 {
		t.Fatalf("retired ping mutated health = responding:%t healthy:%t lastOKMS:%d", responding, healthy, lastOKMS)
	}
}

func TestQueuedPhonePingDoesNotRestoreRetiredStreamHealth(t *testing.T) {
	for _, retired := range []struct {
		name  string
		event func(libgm.StreamSource) any
	}{
		{"stream retired", func(source libgm.StreamSource) any { return &libgm.StreamRetired{Source: source} }},
		{"continuity lost", func(source libgm.StreamSource) any { return &libgm.ContinuityLost{Source: source} }},
	} {
		t.Run(retired.name, func(t *testing.T) {
			host := newSafetyHost()
			current := libgm.StreamSource{ListenID: 7, PollAttempt: 3}
			link := &Link{
				host:                   host,
				state:                  stateLinked,
				auth:                   libgm.NewAuthData(),
				generation:             7,
				runGeneration:          7,
				streamEpoch:            4,
				streamSource:           current,
				streamOpen:             true,
				changes:                make(chan any, 2),
				conversationCandidates: make(map[string][]conversationSummary),
				conversationTargets:    make(map[string][]string),
			}

			link.queueEventFor(7, &libgm.StreamEvent{Source: current, Event: &events.PhonePingSucceeded{}})
			link.queueEventFor(7, retired.event(current))
			link.consumeQueuedEvent(<-link.changes)
			link.mu.Lock()
			responding, healthy, lastOKMS := link.responding, link.healthy, link.lastOKMS
			link.mu.Unlock()
			if responding || healthy || lastOKMS != 0 {
				t.Fatalf("retired stream ping health = responding:%t healthy:%t lastOKMS:%d", responding, healthy, lastOKMS)
			}
			if host.countEvent("health", "ok") != 0 {
				t.Fatal("retired stream ping emitted health/ok")
			}
		})
	}
}
