package gmlink

import (
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/util"
)

func TestPairingAdvertisesPeppyAsTheDeviceName(t *testing.T) {
	details := util.BrowserDetailsMessage
	if details.GetOS() != "Peppy" {
		t.Fatalf("paired device name=%q", details.GetOS())
	}
	if details.GetBrowserType() != gmproto.BrowserType_OTHER || details.GetDeviceType() != gmproto.DeviceType_TABLET {
		t.Fatalf("browser details changed beyond the name: %+v", details)
	}
}
