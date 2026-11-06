package gmlink

import (
	"errors"
	"strings"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestActualPostConnectActiveSessionKeepsOwningRunAlive(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)

	awaitSafety(t, "actual postConnect ActiveSessionSet event", func() bool {
		return host.hasEvent("session", "active_session_set")
	})
	select {
	case err := <-done:
		t.Fatalf("Run returned after normal ActiveSessionSet: %v", err)
	default:
	}
	expectStatus(t, link, `"running":true`, `"healthy":true`)
	link.mu.Lock()
	source, open := link.streamSource, link.streamOpen
	generation := link.generation
	link.mu.Unlock()
	if !open || source == (libgm.StreamSource{}) {
		t.Fatal("actual postConnect did not retain its physical stream source")
	}
	link.queueEventFor(generation, &libgm.StreamEvent{Source: source, Event: &libgm.WrappedSettings{Settings: settingsWithSIM("post-connect-source")}})
	awaitSafety(t, "postConnect same-source settings", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.settings != nil
	})
	link.queueEventFor(generation, &libgm.StreamEvent{Source: source, Event: &events.GaiaLoggedOut{}})
	awaitSafety(t, "postConnect same-source terminal", func() bool {
		return host.hasEvent("link", "relink_needed")
	})
	if err := awaitSafetyResult(t, done, "Run after same-source terminal"); err != nil {
		t.Fatalf("Run error after same-source terminal = %v", err)
	}
}

func TestRefusedPairDoesNotCancelHealthyRun(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)

	err := link.Pair(canaryCookieJSON(t))
	if err == nil || err.Error() != "pair_refused" {
		t.Fatalf("Pair error = %v, want pair_refused", err)
	}
	select {
	case runErr := <-done:
		t.Fatalf("refused Pair cancelled healthy Run: %v", runErr)
	default:
	}
	expectStatus(t, link, `"running":true`, `"healthy":true`)
	link.Stop()
	_ = awaitSafetyResult(t, done, "Run after Stop")
}

func TestGapIngressPreventsQueuedPreGapSettingsFromRestoringAuthority(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	reached, release := host.holdNextEvent("recovered")
	link.queueEvent(&events.ListenRecovered{})
	<-reached

	link.queueEvent(&libgm.WrappedSettings{Settings: completeSyntheticSettings()})
	beforeGap := time.Now()
	link.queueEvent(&events.StreamGap{})
	link.mu.Lock()
	if link.settings != nil {
		link.mu.Unlock()
		t.Fatal("gap ingress did not revoke settings immediately")
	}
	link.mu.Unlock()

	close(release)
	awaitSafety(t, "queued gap projection", func() bool { return host.hasEvent("conn", "temporary_error") })
	link.mu.Lock()
	settings, observedAt := link.settings, link.settingsAt
	link.mu.Unlock()
	if settings != nil {
		t.Fatal("queued pre-gap settings restored authority after the real gap")
	}
	if !observedAt.IsZero() && observedAt.After(beforeGap) {
		t.Fatalf("settings receipt was stamped at consumer time: %v", observedAt)
	}
}

func TestSettingsReceiptTimeIsCapturedAtIngress(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	reached, release := host.holdNextEvent("recovered")
	link.queueEvent(&events.ListenRecovered{})
	<-reached

	started := time.Now()
	link.queueEvent(&libgm.WrappedSettings{Settings: completeSyntheticSettings()})
	time.Sleep(100 * time.Millisecond)
	close(release)
	awaitSafety(t, "settings projection", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.settings != nil
	})
	link.mu.Lock()
	observedAt := link.settingsAt
	link.mu.Unlock()
	if observedAt.Sub(started) > 50*time.Millisecond {
		t.Fatalf("settings receipt time = %v after ingress, want ingress timestamp", observedAt.Sub(started))
	}
}

func TestTerminalEventsRevokeReadinessAtIngress(t *testing.T) {
	tests := []struct {
		name  string
		event any
	}{
		{"gaia logout", &events.GaiaLoggedOut{}},
		{"fatal listen", &events.ListenFatalError{Error: errors.New("synthetic")}},
		{"account change", &events.AccountChange{}},
		{"phone unresponsive", &events.PhoneNotResponding{}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			link, host := openSafetyLink(t, nil)
			reached, release := host.holdNextEvent("recovered")
			link.queueEvent(&events.ListenRecovered{})
			<-reached
			link.mu.Lock()
			link.settings = completeSyntheticSettings()
			link.settingsAt = time.Now()
			link.healthy = true
			link.mu.Unlock()

			link.queueEvent(test.event)
			link.mu.Lock()
			settings, healthy := link.settings, link.healthy
			link.mu.Unlock()
			close(release)
			if settings != nil || healthy {
				t.Fatalf("terminal ingress retained readiness: settings=%t healthy=%t", settings != nil, healthy)
			}
		})
	}
}

func TestFakeAccountChangeDoesNotRevokeReadiness(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	link.settings = completeSyntheticSettings()
	link.settingsAt = time.Now()
	link.healthy = true
	link.mu.Unlock()

	link.queueEvent(&events.AccountChange{IsFake: true})
	link.mu.Lock()
	settings, healthy := link.settings, link.healthy
	link.mu.Unlock()
	if settings == nil || !healthy {
		t.Fatalf("fake account change revoked readiness: settings=%t healthy=%t", settings != nil, healthy)
	}
}

func TestRetiredHealthCompletionCannotMutateReplacementReadiness(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	link.generation = 7
	link.runGeneration = 7
	link.settings = completeSyntheticSettings()
	link.settingsAt = time.Now()
	link.healthy = true
	link.revokeLocked()
	link.settings = completeSyntheticSettings()
	link.settingsAt = time.Now()
	link.healthy = true
	link.mu.Unlock()

	link.recordHealthFor(7, false, 0)

	link.mu.Lock()
	settings, healthy := link.settings, link.healthy
	link.mu.Unlock()
	if settings == nil || !healthy {
		t.Fatalf("retired health completion mutated replacement readiness: settings=%t healthy=%t", settings != nil, healthy)
	}
}

func TestRunningClientDoesNotShareMutableAuthWithLinkState(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	client := runningSafetyClient(t, link)
	link.mu.Lock()
	storedAuth := link.auth
	link.mu.Unlock()
	if client.AuthData == storedAuth {
		t.Fatal("running transport shares mutable AuthData with replacement link state")
	}
	link.Stop()
	_ = awaitSafetyResult(t, done, "Run after Stop")
}

func TestOldQueuedPhoneEventCannotMutateNewRunState(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	link.mu.Lock()
	link.generation = 1
	link.running = true
	link.runGeneration = link.generation
	link.stopRun = func() {}
	link.mu.Unlock()
	reached, release := host.holdNextEvent("recovered")
	link.queueEvent(&events.ListenRecovered{})
	<-reached
	link.queueEvent(&events.PhoneNotResponding{})
	link.Stop()
	link.mu.Lock()
	link.responding = true
	link.mu.Unlock()
	close(release)
	time.Sleep(50 * time.Millisecond)
	link.mu.Lock()
	responding := link.responding
	link.mu.Unlock()
	if !responding {
		t.Fatal("old queued phone event mutated replacement state")
	}
}

func TestRetiredRuntimeCallbacksCannotMutateReplacement(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	replacement := linkedSafetyAuth()
	replacement.SetCookies(map[string]string{"SAPISID": "replacement"})
	oldCandidate, err := cloneAuthData(replacement)
	if err != nil {
		t.Fatal(err)
	}
	oldCandidate.SetCookies(map[string]string{"SAPISID": "retired"})
	link.mu.Lock()
	link.state = stateLinked
	link.auth = replacement
	link.generation = 12
	link.runGeneration = 12
	link.stopCode = ""
	link.mu.Unlock()

	link.transportStopFor(11, "cookie_refused")
	link.transportStopFor(11, "persist_failed")
	link.signOutIfFor(11, "account_mismatch", activeState)
	if err := link.persistRuntimeAuth(11, oldCandidate); err != nil {
		t.Fatalf("retired persist callback returned %v", err)
	}

	link.mu.Lock()
	state, stopCode, currentAuth := link.state, link.stopCode, link.auth
	link.mu.Unlock()
	currentAuth.CookiesLock.RLock()
	cookie := currentAuth.Cookies["SAPISID"]
	currentAuth.CookiesLock.RUnlock()
	if state != stateLinked || stopCode != "" || currentAuth != replacement || cookie != "replacement" {
		t.Fatalf("retired callback mutated replacement: state=%s stop=%s auth_same=%t cookie=%s", state, stopCode, currentAuth == replacement, cookie)
	}
}

func TestRetiredOverflowCannotStopReplacement(t *testing.T) {
	host := newSafetyHost()
	link := &Link{host: host, state: stateLinked, auth: libgm.NewAuthData(), generation: 4, changes: make(chan any)}
	link.queueEventFor(3, &events.NoDataReceived{})
	expectStatus(t, link, `"stop_code":""`)
	if host.hasEvent("link", "event_overflow") {
		t.Fatal("retired queue overflow stopped replacement")
	}
}

func TestRunExitRetiresItsOwnedRuntime(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = ""
		knobs.gateConfig = true
	})
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := make(chan error, 1)
	go func() { done <- link.Run() }()
	awaitSafety(t, "gated config request", func() bool {
		configs, _, _ := fixture.counts()
		return configs > 0
	})
	link.mu.Lock()
	ownedGeneration := link.runGeneration
	link.mu.Unlock()
	fixture.openGate()
	if err := awaitSafetyResult(t, done, "retryable Run"); err == nil || !strings.Contains(err.Error(), "run_retryable") {
		t.Fatalf("Run error = %v", err)
	}
	link.mu.Lock()
	generation := link.generation
	link.mu.Unlock()
	if generation == ownedGeneration {
		t.Fatalf("Run exit left generation %d admitted", ownedGeneration)
	}
}

func completeSyntheticSettings() *gmproto.Settings {
	return &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-sim"}}}}
}

func TestRetiredLookupResponseCannotPublishIntoReplacementIndex(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	link.generation = 20
	link.runGeneration = 20
	link.conversationCandidates["replacement"] = []conversationSummary{{id: "replacement-conversation"}}
	link.mu.Unlock()
	oldConversation := &gmproto.Conversation{
		ConversationID: "retired-conversation",
		Participants: []*gmproto.Participant{{
			ID:        &gmproto.SmallInfo{Number: "retired-target"},
			IsVisible: true,
		}},
	}

	if link.indexConversationsFor(19, 0, []*gmproto.Conversation{oldConversation}) {
		t.Fatal("retired lookup response reported successful publication")
	}
	link.mu.Lock()
	_, oldPublished := link.conversationCandidates["retired-target"]
	replacement := link.conversationCandidates["replacement"]
	link.mu.Unlock()
	if oldPublished || len(replacement) != 1 || replacement[0].id != "replacement-conversation" {
		t.Fatal("retired lookup response mutated replacement index")
	}
}

func TestHealthFailureIngressFencesQueuedSettings(t *testing.T) {
	host := newSafetyHost()
	link := &Link{
		host:                   host,
		state:                  stateLinked,
		auth:                   libgm.NewAuthData(),
		generation:             5,
		runGeneration:          5,
		healthy:                true,
		changes:                make(chan any, 1),
		conversationCandidates: make(map[string][]conversationSummary),
		conversationTargets:    make(map[string][]string),
	}
	link.queueEventFor(5, &libgm.WrappedSettings{Settings: completeSyntheticSettings()})
	queued := <-link.changes

	link.recordHealthFor(5, false, 0)
	link.consumeQueuedEvent(queued)

	link.mu.Lock()
	settings := link.settings
	link.mu.Unlock()
	if settings != nil {
		t.Fatal("pre-health-failure queued settings restored authority")
	}
}

type reentrantCloseHost struct {
	link   *Link
	closed chan struct{}
}

func (*reentrantCloseHost) PersistState([]byte) error { return nil }

func (host *reentrantCloseHost) Event(_, _ string, _, _, _ int64) {
	host.link.Close()
	close(host.closed)
}

func (*reentrantCloseHost) ShowEmoji(string, int64, int64) {}

func TestHostEventCanReenterClose(t *testing.T) {
	host := &reentrantCloseHost{closed: make(chan struct{})}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	host.link = link

	done := make(chan struct{})
	go func() {
		link.emit("conn", "recovered", 0)
		close(done)
	}()
	select {
	case <-host.closed:
	case <-time.After(time.Second):
		t.Fatal("Host.Event reentrant Close deadlocked")
	}
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("event callback did not return after reentrant Close")
	}
}

func TestTerminalHostEventCanReenterClose(t *testing.T) {
	host := &reentrantCloseHost{closed: make(chan struct{})}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	host.link = link
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()

	link.queueEvent(&events.GaiaLoggedOut{})
	select {
	case <-host.closed:
	case <-time.After(time.Second):
		t.Fatal("terminal Host.Event reentrant Close deadlocked")
	}
}

func (*reentrantCloseHost) RcsEvent(string, int64, int64)    {}
func (*reentrantCloseHost) TypingEvent(string, int64, int64) {}
