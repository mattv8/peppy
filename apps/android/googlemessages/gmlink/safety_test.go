package gmlink

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"go.mau.fi/util/exhttp"
)

// forbiddenSendActions are the read, open, typing, delete and send RPCs the link must never cause.
var forbiddenSendActions = []string{"SEND_MESSAGE", "GET_CONVERSATION", "MESSAGE_READ", "TYPING_UPDATES", "DELETE_MESSAGE", "UPDATE_CONVERSATION"}

func assertNoForbiddenSends(t *testing.T, fixture *phoneFixture) {
	t.Helper()
	for _, name := range forbiddenSendActions {
		if count := fixture.countAction(name); count != 0 {
			t.Fatalf("link caused forbidden %s %d times", name, count)
		}
	}
}

// awaitPairResult waits for a Pair or Relink result. A call still blocked on the
// phone is a failure: the link is closed so the test does not hang.
func awaitPairResult(t *testing.T, link *Link, result <-chan error, description string) error {
	t.Helper()
	select {
	case err := <-result:
		return err
	case <-time.After(5 * time.Second):
		link.Close()
		t.Fatalf("%s did not return; it was still waiting on the phone", description)
		return nil
	}
}

func expectErrorCode(t *testing.T, err error, code string) {
	t.Helper()
	if err == nil || err.Error() != code {
		t.Fatalf("error = %v, want %s", err, code)
	}
}

func expectStatus(t *testing.T, link *Link, parts ...string) {
	t.Helper()
	status := link.Status()
	for _, part := range parts {
		if !strings.Contains(status, part) {
			t.Fatalf("status %s lacks %s", status, part)
		}
	}
}

func TestPairNeverEchoesCookies(t *testing.T) {
	t.Run("invalid cookie set", func(t *testing.T) {
		link, host := openSafetyLink(t, nil)

		err := link.Pair([]byte(`{"SID":"` + canaryCookie + `"}`))

		expectErrorCode(t, err, "invalid_cookies")
		assertNoCanary(t, link, host, err)
	})

	t.Run("refused cookie set", func(t *testing.T) {
		fixture := newPhoneFixture(t, nil)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.configRedirect = true })
		link, host := openSafetyLink(t, nil)

		err := link.Pair(canaryCookieJSON(t))

		expectErrorCode(t, err, "cookie_refused")
		assertNoCanary(t, link, host, err)
	})
}

func TestPairChecksConfigAccountMatchesMobile(t *testing.T) {
	t.Run("empty config email refused before sign-in", func(t *testing.T) {
		fixture := newPhoneFixture(t, nil)
		link, host := openSafetyLink(t, nil)

		err := link.Pair(canaryCookieJSON(t))

		expectErrorCode(t, err, "pair_failed")
		if _, signIns, _ := fixture.counts(); signIns != 0 {
			t.Fatalf("sign-in requests = %d, want 0", signIns)
		}
		if len(fixture.allActions()) != 0 {
			t.Fatalf("actions sent = %v, want none", fixture.allActions())
		}
		expectStatus(t, link, `"state":"unlinked"`, `"stop_code":"pair_failed"`)
		if !host.hasEvent("link", "pair_failed") {
			t.Fatal("pair_failed was not reported to the host")
		}
	})

	t.Run("relink refuses a config account that differs from the stored phone", func(t *testing.T) {
		auth := linkedSafetyAuth()
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) {
			knobs.configEmail = canaryAccount
			knobs.gaiaInit = true
		})
		link, _ := openSafetyLink(t, nil)
		setSafetyState(link, stateNeedsRelink, auth)
		result := make(chan error, 1)
		go func() { result <- link.Relink() }()

		err := awaitPairResult(t, link, result, "Relink")

		expectErrorCode(t, err, "account_mismatch")
		if _, signIns, _ := fixture.counts(); signIns != 0 {
			t.Fatalf("sign-in requests = %d, want 0 before the mismatch", signIns)
		}
		if n := fixture.countAction("CREATE_GAIA_PAIRING_CLIENT_INIT"); n != 0 {
			t.Fatalf("pairing init sent %d times, want 0", n)
		}
		expectStatus(t, link, `"state":"signed_out"`, `"stop_code":"account_mismatch"`)
	})

	t.Run("run refuses a config account that differs from the paired phone", func(t *testing.T) {
		auth := linkedSafetyAuth()
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = canaryAccount })
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)
		result := make(chan error, 1)
		go func() { result <- link.Run() }()

		err := awaitSafetyResult(t, result, "Run")

		expectErrorCode(t, err, "account_mismatch")
		if _, _, polls := fixture.counts(); polls != 0 {
			t.Fatalf("long-poll requests = %d, want 0 after the mismatch", polls)
		}
		expectStatus(t, link, `"state":"signed_out"`, `"stop_code":"account_mismatch"`)
		if !host.hasEvent("link", "signed_out") {
			t.Fatal("signed_out was not reported to the host")
		}
	})
}

func TestPairCookieRefusedAtLink(t *testing.T) {
	t.Run("from unlinked keeps the previous state", func(t *testing.T) {
		fixture := newPhoneFixture(t, nil)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.configRedirect = true })
		link, host := openSafetyLink(t, nil)

		err := link.Pair(canaryCookieJSON(t))

		expectErrorCode(t, err, "cookie_refused")
		if configs, _, _ := fixture.counts(); configs != 1 {
			t.Fatalf("config requests = %d, want 1 (redirect not followed)", configs)
		}
		expectStatus(t, link, `"state":"unlinked"`, `"stop_code":"cookie_refused"`)
		if !host.hasEvent("link", "cookie_refused") {
			t.Fatal("cookie_refused was not reported to the host")
		}
		if host.hasEvent("link", "signed_out") {
			t.Fatal("refused Pair from unlinked must not report signed_out")
		}
	})

	t.Run("from signed_out keeps signed_out", func(t *testing.T) {
		fixture := newPhoneFixture(t, nil)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.configRedirect = true })
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateSignedOut, nil)

		err := link.Pair(canaryCookieJSON(t))

		expectErrorCode(t, err, "cookie_refused")
		expectStatus(t, link, `"state":"signed_out"`, `"stop_code":"cookie_refused"`)
		if !host.hasEvent("link", "cookie_refused") {
			t.Fatal("cookie_refused was not reported to the host")
		}
	})

	t.Run("relink signs out and wipes the stored auth", func(t *testing.T) {
		auth := linkedSafetyAuth()
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.configRedirect = true })
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateNeedsRelink, auth)

		err := link.Relink()

		expectErrorCode(t, err, "cookie_refused")
		expectStatus(t, link, `"state":"signed_out"`, `"stop_code":"cookie_refused"`)
		if !host.hasEvent("link", "signed_out") {
			t.Fatal("signed_out was not reported to the host")
		}
		assertAuthWiped(t, host)
	})
}

func TestConfigRedirectSignsOut(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configRedirect = true })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)

	done := make(chan error, 1)
	go func() { done <- link.Run() }()
	err := awaitSafetyResult(t, done, "Run")

	expectErrorCode(t, err, "cookie_refused")
	expectStatus(t, link, `"state":"signed_out"`)
	if !host.hasEvent("link", "signed_out") {
		t.Fatal("signed_out was not reported to the host")
	}
	assertAuthWiped(t, host)
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.auth.Browser != nil || len(link.auth.Cookies) != 0 {
		t.Fatal("in-memory auth still holds the old link")
	}
}

func TestStopDuringRunNoRace(t *testing.T) {
	for iteration := range 5 {
		t.Run(fmt.Sprintf("iteration %d", iteration), func(t *testing.T) {
			auth := linkedSafetyAuth()
			fixture := newPhoneFixture(t, auth)
			fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
			link, _ := openSafetyLink(t, nil)
			setSafetyState(link, stateLinked, auth)
			done := startSafetyRun(t, link)
			client := runningSafetyClient(t, link)

			// Two concurrent Stops race each other and Run's exit; Stop must be idempotent.
			var stops sync.WaitGroup
			for range 2 {
				stops.Add(1)
				go func() {
					defer stops.Done()
					link.Stop()
				}()
			}
			stops.Wait()

			if err := awaitSafetyResult(t, done, "Run"); err != nil {
				t.Fatalf("Run error = %v, want nil after Stop", err)
			}
			expectStatus(t, link, `"state":"linked"`, `"running":false`)
			assertSafetyReleased(t, link, client)

			// Stop ends one Run without poisoning the link: a later Run keeps running until stopped.
			again := startSafetyRun(t, link)
			awaitSafety(t, "second Run readiness", func() bool { return strings.Contains(link.Status(), `"running":true`) })
			select {
			case err := <-again:
				t.Fatalf("second Run returned after reaching readiness: %v", err)
			default:
			}
			link.Stop()
			if err := awaitSafetyResult(t, again, "second Run"); err != nil {
				t.Fatalf("second Run error = %v, want nil", err)
			}
		})
	}
}

func TestCloseDuringRunNoRace(t *testing.T) {
	for iteration := range 5 {
		t.Run(fmt.Sprintf("iteration %d", iteration), func(t *testing.T) {
			auth := linkedSafetyAuth()
			fixture := newPhoneFixture(t, auth)
			fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
			link, _ := openSafetyLink(t, nil)
			setSafetyState(link, stateLinked, auth)
			done := startSafetyRun(t, link)
			client := runningSafetyClient(t, link)

			link.Close()

			if err := awaitSafetyResult(t, done, "Run"); err != nil {
				t.Fatalf("Run error = %v, want nil after Close", err)
			}
			assertSafetyReleased(t, link, client)
			reopened, err := Open(nil, newSafetyHost())
			if err != nil {
				t.Fatalf("process slot not freed by Close: %v", err)
			}
			reopened.Close()
		})
	}
}

func TestUnlinkCallsUnpairOnceThenWipes(t *testing.T) {
	t.Run("server failure makes one request and still wipes", func(t *testing.T) {
		auth := linkedSafetyAuth()
		auth.PairingID = uuid.New()
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) {
			knobs.failAction = gmproto.ActionType_UNPAIR_GAIA_PAIRING
			knobs.failStatus = http.StatusInternalServerError
		})
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)

		err := link.Unlink()

		expectErrorCode(t, err, "unlink_remote_failed")
		if got := fixture.countAction("UNPAIR_GAIA_PAIRING"); got != 1 {
			t.Fatalf("unpair POSTs = %d, want exactly 1", got)
		}
		if actions := fixture.allActions(); len(actions) != 1 {
			t.Fatalf("actions = %v, want only UNPAIR_GAIA_PAIRING", actions)
		}
		expectStatus(t, link, `"state":"unlinked"`, `"stop_code":"unlink_remote_failed"`)
		if !host.hasEvent("link", "unlink_remote_failed") {
			t.Fatal("unlink_remote_failed was not reported to the host")
		}
		assertAuthWiped(t, host)
	})

	t.Run("server success makes one request and clears the stop code", func(t *testing.T) {
		auth := linkedSafetyAuth()
		auth.PairingID = uuid.New()
		fixture := newPhoneFixture(t, auth)
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)

		if err := link.Unlink(); err != nil {
			t.Fatalf("Unlink error = %v, want nil", err)
		}
		if got := fixture.countAction("UNPAIR_GAIA_PAIRING"); got != 1 {
			t.Fatalf("unpair POSTs = %d, want exactly 1", got)
		}
		expectStatus(t, link, `"state":"unlinked"`, `"stop_code":""`)
		if !host.hasEvent("link", "unlinked") {
			t.Fatal("unlinked was not reported to the host")
		}
		assertAuthWiped(t, host)
	})

	t.Run("no pairing makes no request", func(t *testing.T) {
		auth := linkedSafetyAuth()
		fixture := newPhoneFixture(t, auth)
		link, _ := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)

		if err := link.Unlink(); err != nil {
			t.Fatalf("Unlink error = %v, want nil", err)
		}
		if actions := fixture.allActions(); len(actions) != 0 {
			t.Fatalf("actions = %v, want none without a pairing", actions)
		}
		expectStatus(t, link, `"state":"unlinked"`)
	})
}

func TestUnlinkWhileRunning(t *testing.T) {
	auth := linkedSafetyAuth()
	auth.PairingID = uuid.New()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)

	if err := link.Unlink(); err != nil {
		t.Fatalf("Unlink error = %v, want nil", err)
	}

	if err := awaitSafetyResult(t, done, "Run"); err != nil {
		t.Fatalf("Run error = %v, want nil after Unlink", err)
	}
	if got := fixture.countAction("UNPAIR_GAIA_PAIRING"); got != 1 {
		t.Fatalf("unpair POSTs = %d, want exactly 1", got)
	}
	assertNoForbiddenSends(t, fixture)
	expectStatus(t, link, `"state":"unlinked"`, `"running":false`)
	assertAuthWiped(t, host)
}

func TestCheckHealthUsesListConversationsCountOne(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	startSafetyRun(t, link)

	latency := link.CheckHealth()

	if latency < 0 {
		t.Fatalf("CheckHealth = %d, want a latency", latency)
	}
	requests := fixture.listRequests()
	if len(requests) != 1 {
		t.Fatalf("LIST_CONVERSATIONS requests = %d, want exactly 1", len(requests))
	}
	if requests[0].GetCount() != 1 || requests[0].GetFolder() != gmproto.ListConversationsRequest_INBOX {
		t.Fatalf("LIST_CONVERSATIONS request count=%d folder=%v, want count 1 INBOX", requests[0].GetCount(), requests[0].GetFolder())
	}
	expectStatus(t, link, `"healthy":true`)

	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.failAction = gmproto.ActionType_LIST_CONVERSATIONS
		knobs.failStatus = http.StatusInternalServerError
	})
	if latency := link.CheckHealth(); latency != -1 {
		t.Fatalf("failed CheckHealth = %d, want -1", latency)
	}
	expectStatus(t, link, `"state":"linked"`, `"stop_code":""`, `"healthy":false`)
}

func TestCanariesNeverEscape(t *testing.T) {
	t.Run("pair with a canary cookie", func(t *testing.T) {
		fixture := newPhoneFixture(t, nil)
		fixture.configure(func(knobs *fixtureKnobs) {
			knobs.configRedirect = true
			knobs.configEmail = canaryAccount
		})
		link, host := openSafetyLink(t, nil)

		err := link.Pair(canaryCookieJSON(t))

		expectErrorCode(t, err, "cookie_refused")
		assertNoCanary(t, link, host, err)
	})

	t.Run("run with canary account and phone", func(t *testing.T) {
		auth := linkedSafetyAuth()
		auth.SetCookies(map[string]string{"SAPISID": canaryCookie})
		auth.Mobile = &gmproto.Device{SourceID: canaryNumber}
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = canaryAccount })
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)

		done := make(chan error, 1)
		go func() { done <- link.Run() }()
		err := awaitSafetyResult(t, done, "Run")

		expectErrorCode(t, err, "account_mismatch")
		assertNoCanary(t, link, host, err)
	})
}

func TestRotationRace(t *testing.T) {
	auth := libgm.NewAuthData()
	auth.SetCookies(syntheticCookies())
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.rotateCookie = true
	})
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	link.mu.Lock()
	owner := link.generation
	link.mu.Unlock()

	var wait sync.WaitGroup
	for range 4 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			client := link.newClient(auth, link.persist, owner)
			defer client.Disconnect()
			for range 8 {
				if err := client.FetchConfig(t.Context()); err != nil {
					t.Errorf("config rotation failed: %v", err)
					return
				}
			}
		}()
	}
	for range 4 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			for range 8 {
				link.mu.Lock()
				if link.state == stateLinked {
					link.state = stateNeedsRelink
				} else {
					link.state = stateLinked
				}
				link.mu.Unlock()
				if err := link.persist(); err != nil {
					t.Errorf("persist failed: %v", err)
				}
			}
		}()
	}
	wait.Wait()

	host.mu.Lock()
	states := append([][]byte(nil), host.states...)
	host.mu.Unlock()
	var previous uint64
	for _, encoded := range states {
		var envelope persistedState
		if err := json.Unmarshal(encoded, &envelope); err != nil || envelope.Seq <= previous {
			t.Fatalf("persisted seq %d after %d (err %v), want strictly increasing", envelope.Seq, previous, err)
		}
		previous = envelope.Seq
	}
	if fixture.rotations.Load() == 0 {
		t.Fatal("no cookie rotation reached the transport")
	}
	var last struct {
		Cookies map[string]string `json:"cookies"`
	}
	if err := json.Unmarshal(lastPersistedAuth(t, host), &last); err != nil {
		t.Fatal(err)
	}
	auth.CookiesLock.RLock()
	final := auth.Cookies["SAPISID"]
	auth.CookiesLock.RUnlock()
	if last.Cookies["SAPISID"] != final {
		t.Fatalf("last persisted SAPISID = %q, want the latest %q", last.Cookies["SAPISID"], final)
	}
}

func lastPersistedAuth(t *testing.T, host *safetyHost) []byte {
	t.Helper()
	return host.lastEnvelope(t).Auth
}

func TestCloseAbortsBlockedPair(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = canaryAccount
		knobs.gaiaInit = true
	})
	link, host := openSafetyLink(t, nil)
	result := make(chan error, 1)
	go func() { result <- link.Pair(canaryCookieJSON(t)) }()
	awaitEmoji(t, host)
	expectStatus(t, link, `"state":"pairing"`)

	link.Close()

	err := awaitPairResult(t, link, result, "Pair")
	expectErrorCode(t, err, "pair_cancelled")
	expectStatus(t, link, `"state":"unlinked"`, `"stop_code":"pair_cancelled"`)
	// Once Close returns, the link reports nothing to the host; the reason stays in Status.
	if host.hasEvent("link", "pair_cancelled") {
		t.Fatal("pair_cancelled reached the host after Close")
	}
}

func TestPairLinksAfterEmojiConfirmed(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gaiaInit = true
		knobs.gaiaFinish = true
	})
	link, host := openSafetyLink(t, nil)
	result := make(chan error, 1)
	go func() { result <- link.Pair(canaryCookieJSON(t)) }()
	awaitEmoji(t, host)

	// The phone's answer to the confirmed emoji must reach the waiting pair over the long poll.
	if err := awaitPairResult(t, link, result, "Pair"); err != nil {
		t.Fatalf("Pair error = %v, want nil after the emoji is confirmed", err)
	}
	expectStatus(t, link, `"state":"linked"`, `"stop_code":""`)
	if !host.hasEvent("link", "pair_ok") {
		t.Fatal("pair_ok was not reported to the host")
	}
	assertNoCanary(t, link, host)
}

func TestStatusNeverBlocksDuringPair(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = canaryAccount
		knobs.gaiaInit = true
	})
	link, host := openSafetyLink(t, nil)
	result := make(chan error, 1)
	go func() { result <- link.Pair(canaryCookieJSON(t)) }()
	awaitEmoji(t, host)

	for range 20 {
		started := time.Now()
		status := link.Status()
		if elapsed := time.Since(started); elapsed > 100*time.Millisecond {
			t.Fatalf("Status took %s while Pair waited for the emoji", elapsed)
		}
		if !strings.Contains(status, `"state":"pairing"`) {
			t.Fatalf("status during pair = %s, want pairing", status)
		}
	}

	link.Close()
	awaitPairResult(t, link, result, "Pair")
}

// shortSafetyDeadlines injects short call deadlines for one subtest.
func shortSafetyDeadlines(t *testing.T) {
	t.Helper()
	previousConfig, previousUnpair, previousHealth := configTimeout, unpairTimeout, healthTimeout
	configTimeout, unpairTimeout, healthTimeout = 100*time.Millisecond, 100*time.Millisecond, 100*time.Millisecond
	t.Cleanup(func() {
		configTimeout, unpairTimeout, healthTimeout = previousConfig, previousUnpair, previousHealth
	})
}

func TestEveryLibgmCallHasDeadline(t *testing.T) {
	const bound = 5 * time.Second

	t.Run("config during Pair", func(t *testing.T) {
		shortSafetyDeadlines(t)
		fixture := newPhoneFixture(t, nil)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.hangConfig = true })
		link, _ := openSafetyLink(t, nil)
		started := time.Now()

		err := link.Pair(canaryCookieJSON(t))

		expectErrorCode(t, err, "pair_timeout")
		if time.Since(started) > bound {
			t.Fatalf("Pair took %s against a silent config", time.Since(started))
		}
	})

	t.Run("sign-in during pairing start", func(t *testing.T) {
		shortSafetyDeadlines(t)
		fixture := newPhoneFixture(t, nil)
		fixture.configure(func(knobs *fixtureKnobs) {
			knobs.configEmail = canaryAccount
			knobs.hangSignIn = true
		})
		link, _ := openSafetyLink(t, nil)
		started := time.Now()

		err := link.Pair(canaryCookieJSON(t))

		expectErrorCode(t, err, "pair_timeout")
		if time.Since(started) > bound {
			t.Fatalf("Pair took %s against a silent sign-in", time.Since(started))
		}
	})

	t.Run("unpair", func(t *testing.T) {
		shortSafetyDeadlines(t)
		auth := linkedSafetyAuth()
		auth.PairingID = uuid.New()
		newPhoneFixture(t, auth).configure(func(knobs *fixtureKnobs) {
			knobs.hangSend = true
			knobs.hangAction = gmproto.ActionType_UNPAIR_GAIA_PAIRING
		})
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)
		started := time.Now()

		err := link.Unlink()

		expectErrorCode(t, err, "unlink_remote_failed")
		if time.Since(started) > bound {
			t.Fatalf("Unlink took %s against a silent server", time.Since(started))
		}
		assertAuthWiped(t, host)
	})

	t.Run("list conversations", func(t *testing.T) {
		shortSafetyDeadlines(t)
		auth := linkedSafetyAuth()
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) {
			knobs.configEmail = fixtureSourceID
			knobs.hangSend = true
			knobs.hangAction = gmproto.ActionType_LIST_CONVERSATIONS
		})
		link, _ := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)
		startSafetyRun(t, link)
		started := time.Now()

		latency := link.CheckHealth()

		if latency != -1 {
			t.Fatalf("CheckHealth = %d, want -1 against a silent server", latency)
		}
		if time.Since(started) > bound {
			t.Fatalf("CheckHealth took %s against a silent server", time.Since(started))
		}
	})

	t.Run("config during Run", func(t *testing.T) {
		shortSafetyDeadlines(t)
		auth := linkedSafetyAuth()
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) { knobs.hangConfig = true })
		link, _ := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)
		started := time.Now()

		done := make(chan error, 1)
		go func() { done <- link.Run() }()
		err := awaitSafetyResult(t, done, "Run")

		expectErrorCode(t, err, "run_retryable")
		if time.Since(started) > bound {
			t.Fatalf("Run took %s against a silent config", time.Since(started))
		}
	})

	t.Run("connect token refresh", func(t *testing.T) {
		// An expired token makes Connect refresh before the long poll starts. libgm
		// sends that refresh without a context, so the client's global timeout bounds it.
		auth := linkedSafetyAuth()
		auth.TachyonExpiry = time.Now().Add(-time.Minute)
		fixture := newPhoneFixture(t, auth)
		fixture.configure(func(knobs *fixtureKnobs) {
			knobs.configEmail = fixtureSourceID
			knobs.hangRefresh = true
		})
		previous := exhttp.SensibleClientSettings
		exhttp.SensibleClientSettings = previous.WithGlobalTimeout(100 * time.Millisecond)
		t.Cleanup(func() { exhttp.SensibleClientSettings = previous })
		link, host := openSafetyLink(t, nil)
		setSafetyState(link, stateLinked, auth)
		started := time.Now()
		done := make(chan error, 1)
		go func() { done <- link.Run() }()

		awaitSafety(t, "Connect to return after its refresh timed out", func() bool { return host.hasEvent("conn", "ready") })
		if elapsed := time.Since(started); elapsed > bound {
			t.Fatalf("Connect took %s against a silent token refresh", elapsed)
		}
		// The long poll's own refresh is refused as fatal, which signs the link out and ends Run.
		if err := awaitSafetyResult(t, done, "Run"); err != nil {
			t.Fatalf("Run error = %v, want nil after the fatal refresh", err)
		}
		expectStatus(t, link, `"state":"signed_out"`)
		fixture.mu.Lock()
		refreshes := fixture.refreshes
		fixture.mu.Unlock()
		if refreshes < 2 {
			t.Fatalf("token refreshes = %d, want the hung one and the fatal one", refreshes)
		}
	})
}
