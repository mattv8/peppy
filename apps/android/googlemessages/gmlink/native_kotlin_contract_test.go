package gmlink

import (
	"os"
	"path/filepath"
	"testing"
)

// Run after the Kotlin adapter test exports its real request builder output.
func TestNativePrepareAcceptsKotlinCarrierBindings(t *testing.T) {
	directory := os.Getenv("PEPPY_NATIVE_RCS_KOTLIN_FIXTURE_DIR")
	if directory == "" {
		t.Skip("Kotlin request fixtures were not exported")
	}
	for _, source := range []string{"SubscriptionManagerPhoneNumber", "TelephonyLine1Number"} {
		t.Run(source, func(t *testing.T) {
			input, err := os.ReadFile(filepath.Join(directory, source+".json"))
			if err != nil {
				t.Fatal(err)
			}
			request, ok := decodeNativeRcsPrepare(string(input))
			if !ok || !validNativeRcsPrepare(request) {
				t.Fatal("Kotlin-generated preparation request violates the native Go contract")
			}
			if request.CarrierBinding.OwnNumberSource != source {
				t.Fatal("Kotlin-generated source changed at the Go boundary")
			}
		})
	}
}
