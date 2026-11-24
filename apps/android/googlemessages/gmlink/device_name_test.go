package gmlink

import (
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/util"
)

func TestPairingAdvertisesPeppyAsTheDeviceName(t *testing.T) {
	details := util.BrowserDetailsMessage
	if details.GetOS() != "Peppy" {
		t.Fatalf("paired device name=%q", details.GetOS())
	}
}
