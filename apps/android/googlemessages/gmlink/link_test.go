package gmlink

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

type testHost struct {
	mu     sync.Mutex
	states [][]byte
	events []eventCode
}

func (host *testHost) PersistState(state []byte) error {
	host.mu.Lock()
	defer host.mu.Unlock()
	host.states = append(host.states, append([]byte(nil), state...))
	return nil
}

func (host *testHost) Event(kind, code string, _, _, _ int64) {
	host.mu.Lock()
	defer host.mu.Unlock()
	host.events = append(host.events, eventCode{kind, code})
}

func (host *testHost) ShowEmoji(string, int64, int64) {}

func TestOpenEmptyIsUnlinked(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	if !strings.Contains(link.Status(), `"state":"unlinked"`) {
		t.Fatalf("status=%s", link.Status())
	}
}

func TestOpenCorruptFailsClosed(t *testing.T) {
	if _, err := Open([]byte("garbage"), &testHost{}); err == nil {
		t.Fatal("corrupt state opened")
	}
}

func TestOpenPairingReopensAsPrevious(t *testing.T) {
	host := &testHost{}
	state := []byte(`{"version":1,"seq":4,"state":"pairing","pairing_from":"needs_relink","stop_code":"","auth":{}}`)
	link, err := Open(state, host)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	if !strings.Contains(link.Status(), `"state":"needs_relink"`) {
		t.Fatalf("status=%s", link.Status())
	}
	host.mu.Lock()
	defer host.mu.Unlock()
	if len(host.states) != 1 || strings.Contains(string(host.states[0]), `"state":"pairing"`) {
		t.Fatalf("reopened state=%q", host.states)
	}
}

func TestSecondOpenRefusedUntilClose(t *testing.T) {
	first, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Open(nil, &testHost{}); err == nil || err.Error() != "link_already_open" {
		t.Fatalf("second open error=%v", err)
	}
	first.Close()
	second, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	second.Close()
}

func TestPairOnlyFromUnlinkedOrSignedOut(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()
	if err := link.Pair([]byte(`{"OSID":"synthetic","SID":"synthetic","HSID":"synthetic","SSID":"synthetic","APISID":"synthetic","SAPISID":"synthetic"}`)); err == nil || err.Error() != "pair_refused" {
		t.Fatalf("error=%v", err)
	}
}

func TestPairRefusesMissingCookie(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	if err := link.Pair([]byte(`{"OSID":"synthetic"}`)); err == nil || err.Error() != "invalid_cookies" {
		t.Fatalf("error=%v", err)
	}
}

func TestPairAcceptsOptionalPSIDTSAndRejectsUnknownCookie(t *testing.T) {
	cookies := []byte(`{"OSID":"synthetic","SID":"synthetic","HSID":"synthetic","SSID":"synthetic","APISID":"synthetic","SAPISID":"synthetic","__Secure-1PSIDTS":"synthetic"}`)
	var parsed map[string]string
	if err := json.Unmarshal(cookies, &parsed); err != nil || !validCookies(parsed) {
		t.Fatalf("optional cookie rejected: %v", err)
	}
	parsed["unexpected"] = "synthetic"
	if validCookies(parsed) {
		t.Fatal("unknown cookie accepted")
	}
}

func TestConcurrentPairSecondRefused(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state = statePairing
	link.mu.Unlock()
	if err := link.Pair([]byte(`{"OSID":"synthetic","SID":"synthetic","HSID":"synthetic","SSID":"synthetic","APISID":"synthetic","SAPISID":"synthetic"}`)); err == nil || err.Error() != "pair_refused" {
		t.Fatalf("error=%v", err)
	}
}

func TestGaiaLoggedOutNeedsRelink(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()
	link.projectEvent(&events.GaiaLoggedOut{})
	if !strings.Contains(link.Status(), `"state":"needs_relink"`) {
		t.Fatalf("status=%s", link.Status())
	}
}

func TestListenFatalAuthErrorSignsOut(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()
	link.projectEvent(&events.ListenFatalError{Error: events.ErrInvalidCredentials})
	if !strings.Contains(link.Status(), `"state":"signed_out"`) {
		t.Fatalf("status=%s", link.Status())
	}
}

func TestFakeAccountChangeIgnored(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()
	link.projectEvent(&events.AccountChange{IsFake: true})
	if !strings.Contains(link.Status(), `"state":"linked"`) {
		t.Fatalf("status=%s", link.Status())
	}
}

func TestRealAccountChangeSignsOut(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()
	link.projectEvent(&events.AccountChange{})
	if !strings.Contains(link.Status(), `"state":"signed_out"`) {
		t.Fatalf("status=%s", link.Status())
	}
}

func TestRelinkOnlyFromNeedsRelinkAndUsesExistingCookies(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	if err := link.Relink(); err == nil || err.Error() != "relink_refused" {
		t.Fatalf("error=%v", err)
	}
}

func TestRunRefusesSecondConcurrentRun(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state, link.running, link.auth.Browser = stateLinked, true, &gmproto.Device{}
	link.mu.Unlock()
	if err := link.Run(); err == nil || err.Error() != "run_refused" {
		t.Fatalf("error=%v", err)
	}
}

func TestRunTransientErrorKeepsState(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.state = stateLinked
	link.mu.Unlock()
	link.emit("conn", "temporary_error", 0)
	if !strings.Contains(link.Status(), `"state":"linked"`) {
		t.Fatalf("status=%s", link.Status())
	}
}

func TestEventHandlerNeverBlocks(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	done := make(chan struct{})
	go func() {
		link.queueEvent(&events.PhoneNotResponding{})
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("event handler blocked")
	}
}

func TestOpenDisablesUpstreamLoggers(t *testing.T) {
	previous := zerolog.GlobalLevel()
	t.Cleanup(func() { zerolog.SetGlobalLevel(previous) })
	zerolog.SetGlobalLevel(zerolog.InfoLevel)
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	link.Close()
	if zerolog.GlobalLevel() != zerolog.Disabled {
		t.Fatalf("global log level=%v", zerolog.GlobalLevel())
	}
}

func TestPersistOrderingSeqMonotonic(t *testing.T) {
	host := &testHost{}
	link, err := Open(nil, host)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	if err := link.persist(); err != nil {
		t.Fatal(err)
	}
	if err := link.persist(); err != nil {
		t.Fatal(err)
	}
	host.mu.Lock()
	defer host.mu.Unlock()
	var previous uint64
	for _, encoded := range host.states {
		var state persistedState
		if err := json.Unmarshal(encoded, &state); err != nil || state.Seq <= previous {
			t.Fatalf("state=%+v previous=%d err=%v", state, previous, err)
		}
		previous = state.Seq
	}
}

func TestNoLocalTimeUse(t *testing.T) {
	files, err := filepath.Glob("*.go")
	if err != nil {
		t.Fatal(err)
	}
	for _, file := range files {
		if strings.HasSuffix(file, "_test.go") {
			continue
		}
		contents, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(string(contents), "time.Local") {
			t.Fatalf("%s references time.Local", file)
		}
	}
}

func (*testHost) RcsEvent(string, int64, int64)    {}
func (*testHost) TypingEvent(string, int64, int64) {}
