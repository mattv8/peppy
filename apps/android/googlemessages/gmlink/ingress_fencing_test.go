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

func awaitQueued(t *testing.T, description string, condition func() bool) {
	t.Helper()
	awaitSafety(t, description, condition)
}

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
	awaitSafety(t, "Run to become healthy", func() bool {
		return strings.Contains(link.Status(), `"healthy":true`)
	})

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

	link.queueEvent(&libgm.WrappedSettings{Settings: completeSyntheticSettings()})
	beforeRelease := time.Now()
	close(release)
	awaitSafety(t, "settings projection", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.settings != nil
	})
	link.mu.Lock()
	observedAt := link.settingsAt
	link.mu.Unlock()
	if !observedAt.Before(beforeRelease) {
		t.Fatalf("settings receipt time = %v, want before consumer release at %v", observedAt, beforeRelease)
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
	link.queueEvent(&events.NoDataReceived{})
	awaitSafety(t, "replacement queue drain", func() bool { return host.hasEvent("conn", "no_data") })
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

func (*reentrantCloseHost) RcsEvent(string, int64, int64, int64)    {}
func (*reentrantCloseHost) TypingEvent(string, int64, int64, int64) {}

func closeRound2Signal(signal chan struct{}) {
	defer func() { _ = recover() }()
	close(signal)
}

func settingsWithSIM(id string) *gmproto.Settings {
	return &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: id}}}}
}

func TestOldRunCleanupDoesNotSuppressTerminalPersistenceAndReport(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	reportReached, releaseReport := make(chan struct{}), make(chan struct{})
	t.Cleanup(func() { closeRound2Signal(releaseReport) })
	link.completionOutputReady = func() {
		close(reportReached)
		<-releaseReport
	}
	readyReached, releaseReady := host.holdNextEvent("ready")
	t.Cleanup(func() { closeRound2Signal(releaseReady) })
	runDone := startSafetyRun(t, link)
	<-readyReached

	persistReached, releasePersist := host.holdNextPersist(stateLinked)
	t.Cleanup(func() { closeRound2Signal(releasePersist) })
	persistDone := make(chan error, 1)
	go func() { persistDone <- link.persist() }()
	<-persistReached
	statesBeforeTerminal := host.stateCount()

	link.queueEvent(&events.GaiaLoggedOut{})
	awaitSafety(t, "terminal state mutation", func() bool {
		return strings.Contains(link.Status(), `"state":"needs_relink"`)
	})
	closeRound2Signal(releaseReady)
	link.mu.Lock()
	completion := link.pendingCompletion
	link.mu.Unlock()
	if completion == nil {
		t.Fatal("terminal completion is missing")
	}
	select {
	case <-completion.runWaited:
	case <-time.After(5 * time.Second):
		t.Fatal("owning Run did not wait for terminal persistence")
	}
	select {
	case err := <-runDone:
		t.Fatalf("Run returned before terminal persistence: %v", err)
	default:
	}
	closeRound2Signal(releasePersist)
	if err := awaitSafetyResult(t, runDone, "retired Run"); err != nil {
		t.Fatalf("Run error = %v", err)
	}
	if err := <-persistDone; err != nil {
		t.Fatalf("blocking persistence error = %v", err)
	}
	select {
	case <-reportReached:
	case <-time.After(5 * time.Second):
		t.Fatal("terminal report did not reach post-persistence barrier")
	}
	if host.hasEvent("link", "relink_needed") {
		t.Fatal("terminal report escaped its barrier")
	}
	closeRound2Signal(releaseReport)
	host.awaitEvent(t, "link", "relink_needed")
	processedBefore := host.countEvent("conn", "recovered")
	link.queueEvent(&events.ListenRecovered{})
	awaitSafety(t, "terminal event completion", func() bool {
		return host.countEvent("conn", "recovered") > processedBefore
	})

	persistedTerminal := false
	for _, state := range host.statesSince(statesBeforeTerminal) {
		if strings.Contains(string(state), `"state":"needs_relink"`) {
			persistedTerminal = true
		}
	}
	if !persistedTerminal {
		t.Error("old Run cleanup caused the terminal needs_relink persistence to be skipped")
	}
	if !host.hasEvent("link", "relink_needed") {
		t.Error("old Run cleanup suppressed the terminal relink_needed report")
	}
}

func TestOldRunCleanupDoesNotRetireNewPair(t *testing.T) {
	oldAuth := linkedSafetyAuth()
	link, _ := openSafetyLink(t, nil)
	oldClient := link.newClient(oldAuth, func() error { return nil }, 1)
	replacement := libgm.NewAuthData()
	replacement.SetCookies(map[string]string{"SAPISID": "new-pair"})
	link.mu.Lock()
	link.client = oldClient
	link.runGeneration = 1
	link.running = true
	link.generation = 2
	link.pairOwner = 2
	link.state = statePairing
	link.auth = replacement
	link.mu.Unlock()

	link.cleanupOwnedRun(oldClient, 1)

	link.mu.Lock()
	generation, pairOwner, state, currentAuth := link.generation, link.pairOwner, link.state, link.auth
	link.mu.Unlock()
	if generation != 2 || pairOwner != 2 || state != statePairing || currentAuth != replacement {
		t.Fatalf("old Run cleanup retired new Pair: generation=%d pair_owner=%d state=%s auth_same=%t", generation, pairOwner, state, currentAuth == replacement)
	}
}

func TestPhoneNotRespondingFencesQueuedSettingsBeforeConsumerDrain(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	link.recordSettings(&libgm.WrappedSettings{Settings: settingsWithSIM("replacement-authoritative")})
	recoveredReached, releaseRecovered := host.holdNextEvent("recovered")
	t.Cleanup(func() { closeRound2Signal(releaseRecovered) })
	link.queueEvent(&events.ListenRecovered{})
	<-recoveredReached

	link.queueEvent(&libgm.WrappedSettings{Settings: settingsWithSIM("queued-before-terminal")})
	link.queueEvent(&events.PhoneNotResponding{})
	closeRound2Signal(releaseRecovered)
	awaitSafety(t, "phone-not-responding projection", func() bool {
		return host.hasEvent("conn", "phone_not_responding")
	})

	link.mu.Lock()
	settings := link.settings
	link.mu.Unlock()
	if settings != nil {
		t.Fatalf("queued settings restored authority after PhoneNotResponding: SIM=%q", settings.GetSIMCards()[0].GetSIMParticipant().GetID())
	}
}

func TestTrueTerminalIngressCancelsRunBeforeConsumerDrain(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	runDone := startSafetyRun(t, link)
	awaitSafety(t, "ready report", func() bool { return host.hasEvent("conn", "ready") })

	recoveredReached, releaseRecovered := host.holdNextEvent("recovered")
	t.Cleanup(func() { closeRound2Signal(releaseRecovered) })
	link.queueEvent(&events.ListenRecovered{})
	<-recoveredReached
	link.mu.Lock()
	runCtx := link.runCtx
	link.mu.Unlock()

	link.queueEvent(&events.GaiaLoggedOut{})
	if runCtx == nil || runCtx.Err() == nil {
		closeRound2Signal(releaseRecovered)
		link.Stop()
		_ = awaitSafetyResult(t, runDone, "Run cleanup")
		t.Fatal("true terminal ingress left the Run RPC context admitted behind the blocked consumer")
	}
	closeRound2Signal(releaseRecovered)
	_ = awaitSafetyResult(t, runDone, "terminal Run")
}

func TestRetiredPairRedirectCannotSignOutReplacement(t *testing.T) {
	oldAuth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, oldAuth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.gateConfig = true
		knobs.configRedirect = true
	})
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()
	client := link.newClient(oldAuth, link.persist, owner)
	t.Cleanup(client.Disconnect)
	result := make(chan error, 1)
	go func() { result <- client.FetchConfig(t.Context()) }()
	awaitSafety(t, "retired Pair config request", func() bool {
		configs, _, _ := fixture.counts()
		return configs == 1
	})

	replacement := linkedSafetyAuth()
	replacement.Mobile.SourceID = "replacement-mobile"
	link.mu.Lock()
	link.state = stateLinked
	link.auth = replacement
	link.generation++
	link.mu.Unlock()
	fixture.openGate()
	if err := <-result; !errors.Is(err, errRedirectResponse) {
		t.Fatalf("FetchConfig error = %v, want redirect refusal", err)
	}

	link.mu.Lock()
	state, currentAuth := link.state, link.auth
	link.mu.Unlock()
	if state != stateLinked || currentAuth != replacement {
		t.Fatalf("retired Pair redirect mutated replacement: state=%s auth_same=%t", state, currentAuth == replacement)
	}
}

func TestRetiredUnlinkPersistFailureCannotStopReplacement(t *testing.T) {
	oldAuth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, oldAuth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.gateConfig = true
		knobs.rotateCookie = true
	})
	link, host := openSafetyLink(t, nil)
	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()
	client := link.newClient(oldAuth, link.persist, owner)
	t.Cleanup(client.Disconnect)
	result := make(chan error, 1)
	go func() { result <- client.FetchConfig(t.Context()) }()
	awaitSafety(t, "retired Unlink config request", func() bool {
		configs, _, _ := fixture.counts()
		return configs == 1
	})

	replacement := linkedSafetyAuth()
	replacement.Mobile.SourceID = "replacement-mobile"
	link.mu.Lock()
	link.state = stateLinked
	link.auth = replacement
	link.stopCode = ""
	link.generation++
	replacementGeneration := link.generation
	link.mu.Unlock()
	host.setFailPersist(true)
	fixture.openGate()
	if err := <-result; !errors.Is(err, errPersistFailed) {
		t.Fatalf("FetchConfig error = %v, want persist_failed", err)
	}

	link.mu.Lock()
	state, stopCode, currentAuth, generation := link.state, link.stopCode, link.auth, link.generation
	link.mu.Unlock()
	if state != stateLinked || stopCode != "" || currentAuth != replacement || generation != replacementGeneration {
		t.Fatalf("retired Unlink callback mutated replacement: state=%s stop=%s auth_same=%t generation=%d want=%d", state, stopCode, currentAuth == replacement, generation, replacementGeneration)
	}
}

func TestSuccessfulPairPublishesIndependentAuthSnapshot(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
		knobs.gaiaFinish = true
	})
	link, _ := openSafetyLink(t, nil)
	candidate := libgm.NewAuthData()
	candidate.SetCookies(syntheticCookies())
	result := make(chan error, 1)
	go func() { result <- link.pair(candidate, false, "") }()
	if err := <-result; err != nil {
		t.Fatalf("Pair error = %v", err)
	}

	link.mu.Lock()
	published := link.auth
	link.mu.Unlock()
	if published == candidate {
		t.Fatal("successful Pair published its transport-owned mutable AuthData directly")
	}
	candidate.SetCookies(map[string]string{"SAPISID": "retired-pair-mutation"})
	published.CookiesLock.RLock()
	publishedCookie := published.Cookies["SAPISID"]
	published.CookiesLock.RUnlock()
	if publishedCookie == "retired-pair-mutation" {
		t.Fatal("retired Pair transport mutation changed published replacement auth")
	}
}

func TestValidSettingsIngressWithdrawsPreviousAuthorityBeforeConsumerDrain(t *testing.T) {
	link, host := openSafetyLink(t, nil)
	link.recordSettings(&libgm.WrappedSettings{Settings: settingsWithSIM("previous-authoritative")})
	link.mu.Lock()
	previousVersion := link.settingsVersion
	link.mu.Unlock()
	recoveredReached, releaseRecovered := host.holdNextEvent("recovered")
	t.Cleanup(func() { closeRound2Signal(releaseRecovered) })
	link.queueEvent(&events.ListenRecovered{})
	<-recoveredReached

	link.queueEvent(&libgm.WrappedSettings{Settings: settingsWithSIM("changed-current")})
	link.mu.Lock()
	settings, version := link.settings, link.settingsVersion
	link.mu.Unlock()
	closeRound2Signal(releaseRecovered)

	if settings != nil {
		t.Errorf("valid changed settings ingress retained previous authority: SIM=%q", settings.GetSIMCards()[0].GetSIMParticipant().GetID())
	}
	if version <= previousVersion {
		t.Errorf("valid changed settings ingress retained revision %d, previous=%d", version, previousVersion)
	}
}
