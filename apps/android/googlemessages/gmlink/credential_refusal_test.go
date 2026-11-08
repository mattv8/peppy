package gmlink

import (
	"strings"
	"testing"
)

func TestCredentialRefusalSignsOutThroughConfigFetch(t *testing.T) {
	for _, status := range []int{401, 403} {
		t.Run("run_"+httpStatusName(status), func(t *testing.T) {
			auth := linkedSafetyAuth()
			link, host := openSafetyLink(t, nil)
			setSafetyState(link, stateLinked, auth)
			fixture := newPhoneFixture(t, auth)
			fixture.configure(func(knobs *fixtureKnobs) { knobs.configStatus = status })

			err := link.Run()
			assertCredentialRefusalSignsOut(t, link, host, fixture, err)
		})
		t.Run("relink_"+httpStatusName(status), func(t *testing.T) {
			auth := linkedSafetyAuth()
			link, host := openSafetyLink(t, nil)
			setSafetyState(link, stateNeedsRelink, auth)
			fixture := newPhoneFixture(t, auth)
			fixture.configure(func(knobs *fixtureKnobs) { knobs.configStatus = status })

			err := link.Relink()
			assertCredentialRefusalSignsOut(t, link, host, fixture, err)
		})
	}
}

func TestConfigServiceFailureRemainsRetryable(t *testing.T) {
	auth := linkedSafetyAuth()
	link, host := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configStatus = 503 })

	err := link.Run()
	if err == nil || err.Error() != "run_retryable" || !strings.Contains(link.Status(), `"state":"linked"`) {
		t.Fatalf("service failure status=%s error=%v", link.Status(), err)
	}
	configs, _, _ := fixture.counts()
	if configs != 1 {
		t.Fatalf("config requests = %d, want 1", configs)
	}
	if host.stateCount() != 0 {
		t.Fatalf("retryable failure persisted %d states", host.stateCount())
	}
}

func assertCredentialRefusalSignsOut(t *testing.T, link *Link, host *safetyHost, fixture *phoneFixture, err error) {
	t.Helper()
	if err == nil || !strings.Contains(link.Status(), `"state":"signed_out"`) {
		t.Fatalf("credential refusal status=%s error=%v", link.Status(), err)
	}
	assertAuthWiped(t, host)
	configs, _, _ := fixture.counts()
	if configs != 1 {
		t.Fatalf("config requests = %d, want 1", configs)
	}
}

func httpStatusName(status int) string {
	if status == 401 {
		return "unauthorized"
	}
	return "forbidden"
}
