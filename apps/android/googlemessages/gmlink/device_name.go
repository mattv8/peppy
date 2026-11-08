package gmlink

import "go.mau.fi/mautrix-gmessages/pkg/libgm/util"

// pairedDeviceName is how Google Messages lists this link under paired devices.
const pairedDeviceName = "Peppy"

func init() {
	util.BrowserDetailsMessage.OS = pairedDeviceName
}
