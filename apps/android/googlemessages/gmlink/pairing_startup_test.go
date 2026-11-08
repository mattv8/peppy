package gmlink

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/util/pblite"
)

func TestStartGaiaPairingCancelsStuckFirstPoll(t *testing.T) {
	pollReached := make(chan struct{})
	releasePoll := make(chan struct{})
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch {
		case strings.HasSuffix(request.URL.Path, fixtureSignInSuffix):
			body, err := pblite.Marshal(gaiaSignInResponse([]string{primaryPhoneOne}))
			if err != nil {
				t.Fatal(err)
			}
			writeFixtureBody(writer, body)
		case strings.HasSuffix(request.URL.Path, fixturePollSuffix):
			close(pollReached)
			select {
			case <-request.Context().Done():
			case <-releasePoll:
			}
		default:
			writer.WriteHeader(http.StatusNotFound)
		}
	}))
	defer server.Close()
	defer close(releasePoll)

	auth := libgm.NewAuthData()
	auth.SetCookies(map[string]string{"SAPISID": "synthetic"})
	guard := newGuardedTransport(auth, func() error { return nil }, func(string) {})
	client := libgm.NewClient(auth, nil, zerolog.Nop(), localSettings(server, guard))
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	result := make(chan error, 1)
	go func() {
		_, _, err := client.StartGaiaPairing(ctx, ctx)
		result <- err
	}()
	select {
	case <-pollReached:
	case <-time.After(3 * time.Second):
		t.Fatal("first ReceiveMessages request did not start")
	}
	cancel()
	select {
	case err := <-result:
		if err == nil {
			t.Fatal("stuck first poll reported successful pairing startup")
		}
	case <-time.After(3 * time.Second):
		t.Fatal("StartGaiaPairing did not return after its context was cancelled")
	}
}

func TestCloseReleasesPairStuckBeforeGaiaInit(t *testing.T) {
	fixture := newPhoneFixture(t, nil)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.hangPoll = true
	})
	link, _ := openSafetyLink(t, nil)
	result := make(chan error, 1)
	go func() { result <- link.Pair(canaryCookieJSON(t)) }()
	select {
	case <-fixture.pollReached:
	case <-time.After(3 * time.Second):
		t.Fatal("Pair did not start ReceiveMessages")
	}
	link.Close()
	if err := awaitSafetyResult(t, result, "cancelled Pair"); err == nil || err.Error() != "pair_cancelled" {
		t.Fatalf("Pair error = %v, want pair_cancelled", err)
	}
	if got := fixture.countAction("CREATE_GAIA_PAIRING_CLIENT_INIT"); got != 0 {
		t.Fatalf("pairing-init submissions = %d, want 0", got)
	}
}
