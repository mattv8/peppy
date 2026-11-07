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
