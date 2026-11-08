package gmlink

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/util"
	"go.mau.fi/util/exhttp"
)

const (
	syntheticConfigURL = "https://messages.google.com/web/config"
	syntheticSendPath  = "/$rpc/google.internal.communications.instantmessaging.v1.Messaging/SendMessage"
)

func syntheticCookies() map[string]string {
	return map[string]string{"OSID": "synthetic", "SID": "synthetic", "HSID": "synthetic", "SSID": "synthetic", "APISID": "synthetic", "SAPISID": "synthetic"}
}

func safetyDialSettings(server *httptest.Server) exhttp.ClientSettings {
	roots := x509.NewCertPool()
	roots.AddCert(server.Certificate())
	return exhttp.SensibleClientSettings.WithDial(func(ctx context.Context, network, _ string) (net.Conn, error) {
		var dialer net.Dialer
		return dialer.DialContext(ctx, network, server.Listener.Addr().String())
	}).WithTLSConfig(&tls.Config{RootCAs: roots, ServerName: "example.com"})
}

func safetyClient(t *testing.T, server *httptest.Server, persist func() error, onStop func(string)) *libgm.Client {
	t.Helper()
	auth := libgm.NewAuthData()
	auth.SetCookies(syntheticCookies())
	settings := guardedSettings(safetyDialSettings(server), auth, persist, onStop)
	client := libgm.NewClient(auth, nil, zerolog.Nop(), settings)
	t.Cleanup(client.Disconnect)
	return client
}

func syntheticSendRequest() *gmproto.SendMessageRequest {
	return &gmproto.SendMessageRequest{ConversationID: "synthetic", MessagePayload: &gmproto.MessagePayload{TmpID: "synthetic", ConversationID: "synthetic"}, TmpID: "synthetic"}
}

func TestSend500ThroughLibgmIsOneServerSubmission(t *testing.T) {
	var posts atomic.Int32
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.Method == http.MethodPost && request.URL.Path == syntheticSendPath {
			posts.Add(1)
		}
		writer.WriteHeader(http.StatusInternalServerError)
	}))
	defer server.Close()
	_, err := safetyClient(t, server, nil, nil).SendMessage(context.Background(), syntheticSendRequest())
	if !errors.Is(err, errServerResponse) || posts.Load() != 1 {
		t.Fatalf("error=%v posts=%d", err, posts.Load())
	}
}

func TestNoHTTP2Negotiated(t *testing.T) {
	seen := make(chan int, 1)
	server := httptest.NewUnstartedServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		seen <- request.ProtoMajor
		writer.WriteHeader(http.StatusOK)
	}))
	server.EnableHTTP2 = true
	server.StartTLS()
	defer server.Close()
	_ = safetyClient(t, server, nil, nil).FetchConfig(context.Background())
	if got := <-seen; got != 1 {
		t.Fatalf("HTTP version=%d, want 1", got)
	}
}

func TestResetAfterBodyNotRetried(t *testing.T) {
	var posts atomic.Int32
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		posts.Add(1)
		_, _ = io.ReadAll(request.Body)
		connection, _, _ := http.NewResponseController(writer).Hijack()
		_ = connection.Close()
	}))
	defer server.Close()
	_, err := safetyClient(t, server, nil, nil).SendMessage(context.Background(), syntheticSendRequest())
	if err == nil || posts.Load() != 1 {
		t.Fatalf("error=%v posts=%d", err, posts.Load())
	}
}

func TestUnknownHostRefused(t *testing.T) {
	server := httptest.NewTLSServer(http.NotFoundHandler())
	defer server.Close()
	client := safetyClient(t, server, nil, nil)
	_, err := client.DownloadAvatar(context.Background(), "https://example.invalid/synthetic")
	if !errors.Is(err, errRequestRefused) {
		t.Fatalf("error=%v", err)
	}
}

func TestUploadAndRevokePathsRefused(t *testing.T) {
	var arrivals atomic.Int32
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		arrivals.Add(1)
		writer.WriteHeader(http.StatusNotFound)
	}))
	defer server.Close()
	client := safetyClient(t, server, nil, nil)
	client.AuthData.TachyonAuthToken = []byte("synthetic")
	client.AuthData.Browser = &gmproto.Device{}
	_, uploadErr := client.StartUploadMedia(context.Background(), []byte("synthetic"), "image/png")
	_, revokeErr := client.UnpairBugle()
	if !errors.Is(uploadErr, errRequestRefused) || !errors.Is(revokeErr, errRequestRefused) {
		t.Fatalf("upload=%v revoke=%v", uploadErr, revokeErr)
	}
	if arrivals.Load() != 0 {
		t.Fatalf("refused requests reached server %d times", arrivals.Load())
	}
}

func TestConfigLoginRedirectIsCookieRefusal(t *testing.T) {
	var hits atomic.Int32
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		hits.Add(1)
		writer.Header().Set("Location", "https://accounts.google.com/")
		writer.WriteHeader(http.StatusFound)
	}))
	defer server.Close()
	stopped := ""
	err := safetyClient(t, server, nil, func(code string) { stopped = code }).FetchConfig(context.Background())
	var statusErr transportStatusError
	if !errors.As(err, &statusErr) || statusErr.status != http.StatusFound || hits.Load() != 1 || stopped != "cookie_refused" {
		t.Fatalf("error=%v hits=%d stop=%q", err, hits.Load(), stopped)
	}
}

func TestRedirectNotFollowedAndCookiePersistedBeforeReturn(t *testing.T) {
	var redirected atomic.Bool
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path == "/synthetic" {
			redirected.Store(true)
		}
		writer.Header().Add("Set-Cookie", "SAPISID=rotated; Secure")
		writer.Header().Set("Location", "/synthetic")
		writer.WriteHeader(http.StatusTemporaryRedirect)
	}))
	defer server.Close()
	persisted := false
	var client *libgm.Client
	client = safetyClient(t, server, func() error {
		persisted = client.AuthData.Cookies["SAPISID"] == "rotated"
		return nil
	}, nil)
	err := client.FetchConfig(context.Background())
	if !errors.Is(err, errRedirectResponse) || !persisted || redirected.Load() {
		t.Fatalf("error=%v persisted=%t redirected=%t", err, persisted, redirected.Load())
	}
}

func TestPersistFailureStopsLink(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Add("Set-Cookie", "SAPISID=rotated; Secure")
		writer.WriteHeader(http.StatusOK)
	}))
	defer server.Close()
	stopped := ""
	err := safetyClient(t, server, func() error { return errors.New("synthetic") }, func(code string) { stopped = code }).FetchConfig(context.Background())
	if err == nil || stopped != "persist_failed" {
		t.Fatalf("error=%v stop=%q", err, stopped)
	}
}

type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (buffer *syncBuffer) Write(value []byte) (int, error) {
	buffer.mu.Lock()
	defer buffer.mu.Unlock()
	return buffer.buf.Write(value)
}

func (buffer *syncBuffer) String() string {
	buffer.mu.Lock()
	defer buffer.mu.Unlock()
	return buffer.buf.String()
}

func captureUpstreamOutput(t *testing.T) (*syncBuffer, func()) {
	t.Helper()
	captured := &syncBuffer{}
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan struct{})
	go func() {
		_, _ = io.Copy(captured, reader)
		close(done)
	}()
	stdout, stderr := os.Stdout, os.Stderr
	os.Stdout, os.Stderr = writer, writer
	stdLog := log.Writer()
	log.SetOutput(captured)
	// The zerolog globals are not swapped here: libgm reads them from goroutines
	// that can outlive a test, so a write would race with those reads. Clients
	// use zerolog.Nop() and Open disables the global level, so stdout, stderr and
	// the std log are the sinks that can actually receive upstream output.
	return captured, func() {
		log.SetOutput(stdLog)
		os.Stdout, os.Stderr = stdout, stderr
		_ = writer.Close()
		<-done
		_ = reader.Close()
	}
}

func TestUpstreamLoggersSilent(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.WriteHeader(http.StatusForbidden)
		_, _ = writer.Write([]byte("synthetic"))
	}))
	defer server.Close()
	previousLevel := zerolog.GlobalLevel()
	zerolog.SetGlobalLevel(zerolog.Disabled)
	t.Cleanup(func() { zerolog.SetGlobalLevel(previousLevel) })
	captured, restore := captureUpstreamOutput(t)
	if err := safetyClient(t, server, nil, nil).FetchConfig(context.Background()); err == nil {
		t.Fatal("FetchConfig unexpectedly succeeded")
	}
	restore()
	if output := captured.String(); output != "" {
		t.Fatalf("upstream wrote output: %q", output)
	}
}

func TestBugleRevokePathAlwaysRefused(t *testing.T) {
	var arrivals atomic.Int32
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path == "/$rpc/google.internal.communications.instantmessaging.v1.Pairing/RevokeRelayPairing" {
			arrivals.Add(1)
		}
		writer.WriteHeader(http.StatusOK)
	}))
	defer server.Close()
	client := safetyClient(t, server, nil, nil)
	client.AuthData.TachyonAuthToken = []byte("synthetic")
	client.AuthData.Browser = &gmproto.Device{}
	_, err := client.UnpairBugle()
	if !errors.Is(err, errRequestRefused) || arrivals.Load() != 0 {
		t.Fatalf("error=%v arrivals=%d", err, arrivals.Load())
	}
	request, err := http.NewRequest(http.MethodPost, util.RevokeRelayPairingURL, nil)
	if err != nil {
		t.Fatal(err)
	}
	if isAllowedRequest(request) {
		t.Fatal("Bugle revoke path is allowed")
	}
}

func TestOnlyConfigRedirectStopsLink(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("Location", "/synthetic")
		writer.WriteHeader(http.StatusFound)
	}))
	defer server.Close()
	stopped := ""
	_, err := safetyClient(t, server, nil, func(code string) { stopped = code }).SendMessage(context.Background(), syntheticSendRequest())
	var statusErr transportStatusError
	if !errors.As(err, &statusErr) || stopped != "" {
		t.Fatalf("error=%v stop=%q", err, stopped)
	}
}

func TestSettingsReuseBuildsIndependentGuards(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.WriteHeader(http.StatusOK)
	}))
	defer server.Close()
	auth := libgm.NewAuthData()
	auth.SetCookies(syntheticCookies())
	settings := guardedSettings(safetyDialSettings(server), auth, nil, nil)
	var wait sync.WaitGroup
	for range 2 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			client := libgm.NewClient(auth, nil, zerolog.Nop(), settings)
			defer client.Disconnect()
			_ = client.FetchConfig(context.Background())
		}()
	}
	wait.Wait()
}
