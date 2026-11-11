package gmlink

import (
	"errors"
	"net/http"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/util"
	"go.mau.fi/util/exhttp"
)

var (
	errRedirectResponse = errors.New("redirect_refused")
	errServerResponse   = errors.New("server_response_refused")
	errRequestRefused   = errors.New("request_refused")
)

type transportStatusError struct {
	cause  error
	status int
}

func (err transportStatusError) Error() string { return err.cause.Error() }
func (err transportStatusError) Unwrap() error { return err.cause }

type transportGuard struct {
	base    http.RoundTripper
	auth    *libgm.AuthData
	persist func() error
	onStop  func(string)
}

type sendAdmissionContextKey struct{}

var errSendAdmissionRevoked = errors.New("send_admission_revoked")

type sendAdmission func() bool

func newGuardedSettings(auth *libgm.AuthData, persist func() error, onStop func(code string)) exhttp.ClientSettings {
	return guardedSettings(exhttp.SensibleClientSettings, auth, persist, onStop)
}

func guardedSettings(settings exhttp.ClientSettings, auth *libgm.AuthData, persist func() error, onStop func(string)) exhttp.ClientSettings {
	settings.TransportOverride = func(config exhttp.ClientSettings) http.RoundTripper {
		guard := newGuardedTransport(auth, persist, onStop)
		guard.setBase(config)
		return guard
	}
	return settings
}

func newGuardedTransport(auth *libgm.AuthData, persist func() error, onStop func(string)) *transportGuard {
	return &transportGuard{auth: auth, persist: persist, onStop: onStop}
}

func (guard *transportGuard) setBase(settings exhttp.ClientSettings) {
	transport := &http.Transport{}
	guard.base = settings.Configure(transport)
	transport.Protocols = new(http.Protocols)
	transport.Protocols.SetHTTP1(true)
}

func (guard *transportGuard) RoundTrip(request *http.Request) (*http.Response, error) {
	if guard.base == nil || !isAllowedRequest(request) {
		return nil, errRequestRefused
	}
	if admission, _ := request.Context().Value(sendAdmissionContextKey{}).(sendAdmission); admission != nil && !admission() {
		return nil, errSendAdmissionRevoked
	}
	request = request.Clone(request.Context())
	request.GetBody = nil
	response, err := guard.base.RoundTrip(request)
	if err != nil || response == nil {
		return response, err
	}
	if guard.updateCookies(response) && guard.persist != nil {
		if err := guard.persist(); err != nil {
			_ = response.Body.Close()
			guard.stop("persist_failed")
			return nil, errPersistFailed
		}
	}
	if response.StatusCode >= http.StatusMultipleChoices && response.StatusCode < http.StatusBadRequest {
		_ = response.Body.Close()
		if request.URL.Path == "/web/config" {
			guard.stop("cookie_refused")
		}
		return nil, transportStatusError{cause: errRedirectResponse, status: response.StatusCode}
	}
	if response.StatusCode >= http.StatusInternalServerError {
		_ = response.Body.Close()
		return nil, transportStatusError{cause: errServerResponse, status: response.StatusCode}
	}
	return response, nil
}

func (guard *transportGuard) stop(code string) {
	if guard.onStop != nil {
		guard.onStop(code)
	}
}

func (guard *transportGuard) updateCookies(response *http.Response) bool {
	if guard.auth == nil {
		return false
	}
	guard.auth.CookiesLock.Lock()
	defer guard.auth.CookiesLock.Unlock()
	if guard.auth.Cookies == nil {
		guard.auth.Cookies = make(map[string]string)
	}
	changed := false
	for _, cookie := range response.Cookies() {
		if guard.auth.Cookies[cookie.Name] != cookie.Value {
			guard.auth.Cookies[cookie.Name] = cookie.Value
			changed = true
		}
	}
	return changed
}

func isAllowedRequest(request *http.Request) bool {
	if request.URL.Scheme != "https" {
		return false
	}
	if request.URL.RawQuery != "" {
		return request.Method == http.MethodPost && libgm.ValidUploadSessionURL(request.URL.String())
	}
	path := request.URL.Path
	if request.URL.Hostname() == "messages.google.com" {
		return path == "/web/config" || path == "/web/authentication" || path == "/web/timesource"
	}
	host := request.URL.Hostname()
	if host != "instantmessaging-pa.googleapis.com" && host != "instantmessaging-pa.clients6.google.com" {
		return false
	}
	if request.URL.String() == util.RevokeRelayPairingURL {
		return false
	}
	if path == "/upload" {
		return isAllowedMediaRequest(request)
	}
	switch path {
	case "/$rpc/google.internal.communications.instantmessaging.v1.Pairing/RegisterPhoneRelay",
		"/$rpc/google.internal.communications.instantmessaging.v1.Pairing/RefreshPhoneRelay",
		"/$rpc/google.internal.communications.instantmessaging.v1.Pairing/GetWebEncryptionKey",
		"/$rpc/google.internal.communications.instantmessaging.v1.Messaging/ReceiveMessages",
		"/$rpc/google.internal.communications.instantmessaging.v1.Messaging/SendMessage",
		"/$rpc/google.internal.communications.instantmessaging.v1.Messaging/AckMessages",
		"/$rpc/google.internal.communications.instantmessaging.v1.Registration/SignInGaia",
		"/$rpc/google.internal.communications.instantmessaging.v1.Registration/RegisterRefresh":
		return true
	default:
		return false
	}
}

func isAllowedMediaRequest(request *http.Request) bool {
	return request.URL.String() == util.UploadMediaURL && request.URL.Host == "instantmessaging-pa.googleapis.com" && (request.Method == http.MethodGet || request.Method == http.MethodPost)
}
