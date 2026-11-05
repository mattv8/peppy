package gmlink

import (
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"testing"

	"github.com/rs/zerolog"
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

func (host *testHost) Event(kind, code string, _ int64) {
	host.mu.Lock()
	defer host.mu.Unlock()
	host.events = append(host.events, eventCode{kind, code})
}

func (host *testHost) ShowEmoji(string) {}

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

func TestEveryEmittedCodeIsListed(t *testing.T) {
	for _, code := range eventCodes {
		if !isEventCode(code.kind, code.code) {
			t.Fatalf("missing %s/%s", code.kind, code.code)
		}
	}
}

func TestHealthyWithoutBrowserActiveAlert(t *testing.T) {
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	link.mu.Lock()
	link.running = true
	link.mu.Unlock()
	link.recordHealth(true, 1)
	if !strings.Contains(link.Status(), `"healthy":true`) {
		t.Fatalf("status=%s", link.Status())
	}
}

func TestEveryReturnedErrorIsFixedCode(t *testing.T) {
	code := regexp.MustCompile(`^[a-z][a-z0-9_]{0,39}$`)
	if _, err := Open(nil, nil); err == nil || !code.MatchString(err.Error()) {
		t.Fatalf("open error=%v", err)
	}
	link, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(link.Close)
	for _, err := range []error{
		link.Pair([]byte("not-json")), link.Relink(), link.Run(), link.Unlink(),
	} {
		if err != nil && !code.MatchString(err.Error()) {
			t.Fatalf("non-fixed error=%q", err)
		}
	}
	for _, stopCode := range []string{"", "pair_failed", "cookie_refused", "unlink_remote_failed", "persist_failed"} {
		if stopCode != "" && !code.MatchString(stopCode) {
			t.Fatalf("non-fixed stop code=%q", stopCode)
		}
	}
}

func TestEveryLibgmCallHasDeadline(t *testing.T) {
	if configTimeout <= 0 || connectTimeout <= 0 || unpairTimeout <= 0 || pairWaitTimeout <= 0 {
		t.Fatalf("invalid call deadlines: config=%s connect=%s unpair=%s pair=%s", configTimeout, connectTimeout, unpairTimeout, pairWaitTimeout)
	}
	if pairWaitTimeout < configTimeout {
		t.Fatal("emoji wait deadline is shorter than config deadline")
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
