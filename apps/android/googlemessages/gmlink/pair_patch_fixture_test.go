package gmlink

import (
	"context"
	"crypto/tls"
	"errors"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"go.mau.fi/util/exhttp"
	"go.mau.fi/util/pblite"
)

const (
	primaryPhoneOne = "11111111-1111-1111-1111-111111111111"
	primaryPhoneTwo = "22222222-2222-2222-2222-222222222222"
)

type gaiaPairingFixture struct {
	server          *httptest.Server
	sendMessages    atomic.Int32
	receiveMessages atomic.Int32
}

func newGaiaPairingFixture(t *testing.T, phoneIDs []string) *gaiaPairingFixture {
	t.Helper()
	response, err := pblite.Marshal(gaiaSignInResponse(phoneIDs))
	if err != nil {
		t.Fatal(err)
	}
	fixture := &gaiaPairingFixture{}
	fixture.server = httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch {
		case strings.HasSuffix(request.URL.Path, ".Registration/SignInGaia"):
			writer.Header().Set("Content-Type", libgm.ContentTypePBLite)
			_, _ = writer.Write(response)
		case strings.HasSuffix(request.URL.Path, ".Messaging/SendMessage"):
			fixture.sendMessages.Add(1)
			writer.WriteHeader(http.StatusOK)
		case strings.HasSuffix(request.URL.Path, ".Messaging/ReceiveMessages"):
			fixture.receiveMessages.Add(1)
			writer.WriteHeader(http.StatusOK)
			writer.(http.Flusher).Flush()
			<-request.Context().Done()
		default:
			t.Errorf("unexpected request: %s", request.URL.Path)
			writer.WriteHeader(http.StatusNotFound)
		}
	}))
	t.Cleanup(fixture.server.Close)
	return fixture
}

func gaiaSignInResponse(phoneIDs []string) *gmproto.SignInGaiaResponse {
	devices := make([]*gmproto.RPCGaiaData_UnknownContainer_Item2_Item1, 0, len(phoneIDs))
	deviceDetails := make([]*gmproto.RPCGaiaData_UnknownContainer_Item4, 0, len(phoneIDs))
	for index, phoneID := range phoneIDs {
		timestamp := uint64(index + 1)
		devices = append(devices, &gmproto.RPCGaiaData_UnknownContainer_Item2_Item1{
			DestOrSourceUUID: phoneID,
			UnknownInt4:      1,
			UnknownBigInt7:   uint64(index + 1),
		})
		deviceDetails = append(deviceDetails, &gmproto.RPCGaiaData_UnknownContainer_Item4{
			DestOrSourceUUID: phoneID,
			Item8:            &gmproto.RPCGaiaData_UnknownContainer_Item4_Item8{UnknownTimestamp: timestamp},
		})
	}
	return &gmproto.SignInGaiaResponse{
		DeviceData: &gmproto.SignInGaiaResponse_DeviceData{
			DeviceWrapper: &gmproto.SignInGaiaResponse_DeviceData_DeviceWrapper{Device: &gmproto.Device{SourceID: "synthetic-source"}},
			UnknownItems2: devices,
			UnknownItems3: deviceDetails,
		},
	}
}

func (fixture *gaiaPairingFixture) client() *libgm.Client {
	auth := libgm.NewAuthData()
	auth.SetCookies(map[string]string{"SAPISID": "synthetic"})
	guard := newGuardedTransport(auth, func() error { return nil }, func(string) {})
	return libgm.NewClient(auth, nil, zerolog.Nop(), localSettings(fixture.server, guard))
}

func localSettings(server *httptest.Server, guard *transportGuard) exhttp.ClientSettings {
	settings := exhttp.SensibleClientSettings.WithTLSConfig(&tls.Config{InsecureSkipVerify: true})
	settings = settings.WithDial(func(ctx context.Context, network, address string) (net.Conn, error) {
		return net.Dial(network, server.Listener.Addr().String())
	})
	return guardedSettings(settings, guard.auth, guard.persist, guard.onStop)
}

func startGaiaPairing(t *testing.T, client *libgm.Client) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
	defer cancel()
	_, _, err := client.StartGaiaPairing(ctx, ctx)
	return err
}

func TestGaiaTwoPrimaryPhonesSendsNoInit(t *testing.T) {
	fixture := newGaiaPairingFixture(t, []string{primaryPhoneOne, primaryPhoneTwo})

	err := startGaiaPairing(t, fixture.client())

	if !errors.Is(err, libgm.ErrHadMultipleDevices) {
		t.Fatalf("error = %v, want ErrHadMultipleDevices", err)
	}
	if got := fixture.sendMessages.Load(); got != 0 {
		t.Fatalf("SendMessage requests = %d, want 0", got)
	}
	if got := fixture.receiveMessages.Load(); got != 0 {
		t.Fatalf("ReceiveMessages requests = %d, want 0", got)
	}
}

func TestGaiaNoPrimaryPhoneRefuses(t *testing.T) {
	fixture := newGaiaPairingFixture(t, nil)

	err := startGaiaPairing(t, fixture.client())

	if !errors.Is(err, libgm.ErrNoDevicesFound) {
		t.Fatalf("error = %v, want ErrNoDevicesFound", err)
	}
	if got := fixture.sendMessages.Load(); got != 0 {
		t.Fatalf("SendMessage requests = %d, want 0", got)
	}
}

func TestGaiaOnePrimaryPhoneProceeds(t *testing.T) {
	fixture := newGaiaPairingFixture(t, []string{primaryPhoneOne})

	_ = startGaiaPairing(t, fixture.client())

	if fixture.sendMessages.Load() == 0 && fixture.receiveMessages.Load() == 0 {
		t.Fatal("pairing did not attempt ReceiveMessages or SendMessage")
	}
}

func TestGaiaRepairNeedsSavedPhone(t *testing.T) {
	t.Run("matching primary proceeds", func(t *testing.T) {
		fixture := newGaiaPairingFixture(t, []string{primaryPhoneOne, primaryPhoneTwo})
		client := fixture.client()
		client.AuthData.DestPhoneID = 1

		_ = startGaiaPairing(t, client)

		if fixture.sendMessages.Load() == 0 && fixture.receiveMessages.Load() == 0 {
			t.Fatal("repair did not attempt ReceiveMessages or SendMessage")
		}
	})

	t.Run("missing saved phone refuses", func(t *testing.T) {
		fixture := newGaiaPairingFixture(t, []string{primaryPhoneOne, primaryPhoneTwo})
		client := fixture.client()
		client.AuthData.DestPhoneID = 3

		err := startGaiaPairing(t, client)

		if !errors.Is(err, libgm.ErrPhoneNotFound) {
			t.Fatalf("error = %v, want ErrPhoneNotFound", err)
		}
		if got := fixture.sendMessages.Load(); got != 0 {
			t.Fatalf("SendMessage requests = %d, want 0", got)
		}
	})
}
