package gmlink

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// Host is implemented by the Kotlin runtime.
type Host interface {
	PersistState(sealed []byte) error
	Event(kind, code string, value int64)
	ShowEmoji(emoji string)
}

// Link owns one Google Messages link for the process.
type Link struct {
	host Host

	mu          sync.Mutex
	persistMu   sync.Mutex
	state       string
	pairingFrom string
	stopCode    string
	auth        *libgm.AuthData
	seq         uint64
	client      *libgm.Client
	running     bool
	healthy     bool
	responding  bool
	lastOKMS    int64
	stop        chan struct{}
	stopOnce    sync.Once
	pairCancel  context.CancelFunc

	changes chan any
}

var openLink atomic.Pointer[Link]

const (
	configTimeout   = 30 * time.Second
	connectTimeout  = 30 * time.Second
	unpairTimeout   = 30 * time.Second
	pairWaitTimeout = 5 * time.Minute
)

// Open restores a persisted link, or creates an unlinked link for empty state.
func Open(state []byte, host Host) (*Link, error) {
	if host == nil {
		return nil, errors.New("host_required")
	}
	decoded, err := decodeState(state)
	if err != nil {
		return nil, err
	}
	zerolog.SetGlobalLevel(zerolog.Disabled)
	link := &Link{host: host, state: decoded.State, stopCode: decoded.StopCode, seq: decoded.Seq, changes: make(chan any, 64), stop: make(chan struct{})}
	if len(decoded.Auth) > 0 && string(decoded.Auth) != "null" {
		link.auth = libgm.NewAuthData()
		if err := json.Unmarshal(decoded.Auth, link.auth); err != nil {
			return nil, errors.New("invalid_state")
		}
	} else {
		link.auth = libgm.NewAuthData()
	}
	if !openLink.CompareAndSwap(nil, link) {
		return nil, errors.New("link_already_open")
	}
	go link.consumeEvents()
	if len(state) > 0 {
		var raw persistedState
		_ = json.Unmarshal(state, &raw)
		if raw.State == statePairing {
			if err := link.persist(); err != nil {
				link.Close()
				return nil, errors.New("state_persistence_failed")
			}
		}
	}
	return link, nil
}

// Close aborts work, stops Run, and releases the process-wide link slot.
func (link *Link) Close() {
	link.mu.Lock()
	if link.pairCancel != nil {
		link.pairCancel()
	}
	link.stopOnce.Do(func() { close(link.stop) })
	link.mu.Unlock()
	openLink.CompareAndSwap(link, nil)
}

// Pair links a newly signed-in account after the phone confirms its emoji.
func (link *Link) Pair(cookiesJSON []byte) error {
	defer clear(cookiesJSON)
	cookies := map[string]string{}
	if err := json.Unmarshal(cookiesJSON, &cookies); err != nil || !validCookies(cookies) {
		return errors.New("invalid_cookies")
	}
	auth := libgm.NewAuthData()
	auth.SetCookies(cookies)
	return link.pair(auth, false)
}

// Relink repairs an existing link using its stored cookies.
func (link *Link) Relink() error {
	link.mu.Lock()
	if link.state != stateNeedsRelink {
		link.mu.Unlock()
		return errors.New("relink_refused")
	}
	auth := link.auth.CopyForRepair()
	link.mu.Unlock()
	return link.pair(auth, true)
}

func (link *Link) pair(auth *libgm.AuthData, relink bool) (err error) {
	link.mu.Lock()
	previous := link.state
	if (!relink && previous != stateUnlinked && previous != stateSignedOut) || (relink && previous != stateNeedsRelink) {
		link.mu.Unlock()
		return errors.New("pair_refused")
	}
	link.state, link.pairingFrom, link.stopCode = statePairing, previous, ""
	ctx, cancel := context.WithCancel(context.Background())
	link.pairCancel = cancel
	link.mu.Unlock()
	if err := link.persist(); err != nil {
		cancel()
		link.restorePair(previous, "persist_failed")
		return errors.New("state_persistence_failed")
	}
	completed := false
	defer func() {
		cancel()
		link.mu.Lock()
		link.pairCancel = nil
		link.mu.Unlock()
		if !completed {
			code := pairCode(err)
			if link.restorePair(previous, code) {
				link.emit("link", code, 0)
			}
		}
	}()
	client := link.newClient(auth)
	defer client.Disconnect()
	configCtx, cancelConfig := context.WithTimeout(ctx, configTimeout)
	err = client.FetchConfig(configCtx)
	cancelConfig()
	if err != nil {
		return link.pairResultError(err)
	}
	email := client.Config.GetDeviceInfo().GetEmail()
	if email == "" {
		return errors.New("pair_failed")
	}
	startCtx, cancelStart := context.WithTimeout(ctx, configTimeout)
	emoji, pairing, err := client.StartGaiaPairing(startCtx, startCtx)
	cancelStart()
	if err != nil {
		return link.pairResultError(err)
	}
	link.host.ShowEmoji(emoji)
	finishCtx, cancelFinish := context.WithTimeout(ctx, pairWaitTimeout)
	_, err = client.FinishGaiaPairing(finishCtx, pairing)
	cancelFinish()
	if err != nil {
		return link.pairResultError(err)
	}
	if !strings.EqualFold(email, auth.Mobile.GetSourceID()) {
		link.signOut("account_mismatch")
		return errors.New("account_mismatch")
	}
	link.mu.Lock()
	link.auth, link.state, link.pairingFrom, link.stopCode = auth, stateLinked, "", ""
	link.mu.Unlock()
	if err = link.persist(); err != nil {
		link.stopFatal("persist_failed", false)
		return errors.New("state_persistence_failed")
	}
	completed = true
	if relink {
		link.emit("link", "relink_ok", 0)
	} else {
		link.emit("link", "pair_ok", 0)
	}
	return nil
}

func (link *Link) restorePair(previous, code string) bool {
	link.mu.Lock()
	restored := false
	if link.state == statePairing {
		link.state, link.pairingFrom, link.stopCode = previous, "", code
		restored = true
	}
	link.mu.Unlock()
	if !restored {
		return false
	}
	if err := link.persist(); err != nil {
		link.stopFatal("persist_failed", false)
	}
	return true
}

// Run keeps the linked client connected until Stop or Close.
func (link *Link) Run() error {
	link.mu.Lock()
	if link.running || link.state != stateLinked || link.auth.Browser == nil {
		link.mu.Unlock()
		return errors.New("run_refused")
	}
	auth := link.auth
	client := link.newClient(auth)
	link.client, link.running = client, true
	stop := link.stop
	link.mu.Unlock()
	defer func() {
		link.mu.Lock()
		if link.client == client {
			link.client = nil
		}
		link.running = false
		link.mu.Unlock()
		client.Disconnect()
	}()
	client.SetPingInterval(20 * time.Minute)
	configCtx, cancelConfig := context.WithTimeout(context.Background(), configTimeout)
	err := client.FetchConfig(configCtx)
	cancelConfig()
	if err != nil {
		if isCookieRefusal(err) {
			link.signOut("cookie_refused")
			return errors.New("cookie_refused")
		}
		link.emit("conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	if email := client.Config.GetDeviceInfo().GetEmail(); email == "" || !strings.EqualFold(email, auth.Mobile.GetSourceID()) {
		link.signOut("account_mismatch")
		return errors.New("account_mismatch")
	}
	connectCtx, cancelConnect := context.WithTimeout(context.Background(), connectTimeout)
	err = client.Connect(connectCtx)
	cancelConnect()
	if err != nil {
		if isFatalAuthError(err) {
			link.signOut("signed_out")
			return errors.New("fatal_refresh")
		}
		link.emit("conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	link.mu.Lock()
	link.healthy = true
	link.mu.Unlock()
	link.emit("conn", "ready", 0)
	<-stop
	return nil
}

// CheckHealth checks one inbox conversation and returns its latency in milliseconds, or -1.
func (link *Link) CheckHealth() int64 {
	link.mu.Lock()
	client := link.client
	link.mu.Unlock()
	if client == nil {
		link.recordHealth(false, 0)
		return -1
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	started := time.Now()
	_, err := client.ListConversations(ctx, &gmproto.ListConversationsRequest{Count: 1, Folder: gmproto.ListConversationsRequest_INBOX})
	if err != nil {
		link.recordHealth(false, 0)
		return -1
	}
	latency := time.Since(started).Milliseconds()
	link.recordHealth(true, latency)
	return latency
}

// Unlink makes one Gaia unpair request when a pairing exists, then wipes the local link.
func (link *Link) Unlink() error {
	link.Stop()
	link.mu.Lock()
	auth := link.auth
	paired := auth != nil && auth.PairingID != uuid.Nil
	link.mu.Unlock()
	var err error
	if paired {
		client := link.newClient(auth)
		ctx, cancel := context.WithTimeout(context.Background(), unpairTimeout)
		err = client.UnpairGaia(ctx)
		cancel()
		client.Disconnect()
	}
	link.mu.Lock()
	link.state, link.pairingFrom, link.auth = stateUnlinked, "", libgm.NewAuthData()
	link.healthy, link.responding, link.lastOKMS = false, false, 0
	if err != nil {
		link.stopCode = "unlink_remote_failed"
	} else {
		link.stopCode = ""
	}
	link.mu.Unlock()
	if persistErr := link.persist(); persistErr != nil {
		return errors.New("state_persistence_failed")
	}
	if err != nil {
		link.emit("link", "unlink_remote_failed", 0)
		return errors.New("unlink_remote_failed")
	}
	link.emit("link", "unlinked", 0)
	return nil
}

// Stop ends Run without changing link state.
func (link *Link) Stop() {
	link.mu.Lock()
	link.stopOnce.Do(func() { close(link.stop) })
	link.mu.Unlock()
}

// Status returns the public, non-secret link status JSON.
func (link *Link) Status() string {
	link.mu.Lock()
	status := struct {
		Seq             uint64 `json:"seq"`
		State           string `json:"state"`
		Running         bool   `json:"running"`
		Healthy         bool   `json:"healthy"`
		PhoneResponding bool   `json:"phone_responding"`
		LastOKMS        int64  `json:"last_ok_ms"`
		StopCode        string `json:"stop_code"`
	}{link.seq, link.state, link.running, link.healthy, link.responding, link.lastOKMS, link.stopCode}
	link.mu.Unlock()
	encoded, _ := json.Marshal(status)
	return string(encoded)
}

func (link *Link) newClient(auth *libgm.AuthData) *libgm.Client {
	client := libgm.NewClient(auth, nil, zerolog.Nop(), newGuardedSettings(auth, link.persist, link.transportStop))
	client.SetEventHandler(link.queueEvent)
	return client
}

func (link *Link) queueEvent(event any) {
	select {
	case link.changes <- event:
	default:
		link.stopFatal("persist_failed", false)
	}
}

func (link *Link) consumeEvents() {
	for event := range link.changes {
		link.projectEvent(event)
	}
}

func (link *Link) projectEvent(event any) {
	switch value := event.(type) {
	case *events.AuthTokenRefreshed:
		if err := link.persist(); err != nil {
			link.stopFatal("persist_failed", false)
		}
	case *events.GaiaLoggedOut:
		link.transition(stateNeedsRelink, "relink_needed", "link")
	case *events.ListenFatalError:
		if isFatalAuthError(value.Error) {
			link.signOut("signed_out")
		} else {
			link.transition(stateNeedsRelink, "relink_needed", "link")
		}
	case *events.AccountChange:
		if !value.IsFake {
			link.signOut("signed_out")
		}
	case *events.ListenTemporaryError:
		link.emit("conn", "temporary_error", 0)
	case *events.ListenRecovered:
		link.emit("conn", "recovered", 0)
	case *events.PhoneNotResponding:
		link.mu.Lock()
		link.responding = false
		link.mu.Unlock()
		link.emit("conn", "phone_not_responding", 0)
	case *events.PhoneRespondingAgain:
		link.mu.Lock()
		link.responding = true
		link.mu.Unlock()
		link.emit("conn", "phone_responding", 0)
	case *events.NoDataReceived:
		link.emit("conn", "no_data", 0)
	case *events.ActiveSessionSet:
		link.emit("session", "active_session_set", 0)
	case *gmproto.UserAlertEvent:
		if value.GetAlertType() == gmproto.AlertType_BROWSER_ACTIVE {
			link.emit("browser", "active", 0)
		} else if value.GetAlertType() == gmproto.AlertType_BROWSER_INACTIVE {
			link.emit("browser", "inactive", 0)
		}
	}
}

func (link *Link) transportStop(code string) {
	if code == "cookie_refused" {
		link.signOut("signed_out")
		return
	}
	if code == "persist_failed" {
		link.stopFatal(code, false)
	}
}

func (link *Link) transition(state, code, kind string) {
	link.mu.Lock()
	if link.state == stateLinked {
		link.state, link.stopCode = state, ""
	}
	link.mu.Unlock()
	if err := link.persist(); err != nil {
		link.stopFatal("persist_failed", false)
		return
	}
	link.emit(kind, code, 0)
}

func (link *Link) signOut(code string) {
	link.mu.Lock()
	link.state, link.pairingFrom, link.stopCode, link.auth = stateSignedOut, "", code, libgm.NewAuthData()
	link.mu.Unlock()
	if err := link.persist(); err != nil {
		link.stopFatal("persist_failed", false)
		return
	}
	link.emit("link", "signed_out", 0)
	link.Stop()
}

func (link *Link) stopFatal(code string, wipe bool) {
	link.mu.Lock()
	link.stopCode = code
	if wipe {
		link.state, link.pairingFrom, link.auth = stateSignedOut, "", libgm.NewAuthData()
	}
	link.stopOnce.Do(func() { close(link.stop) })
	link.mu.Unlock()
	link.emit("link", code, 0)
}

func (link *Link) recordHealth(ok bool, latency int64) {
	link.mu.Lock()
	link.healthy = ok
	if ok {
		link.lastOKMS = time.Now().UnixMilli()
	}
	link.mu.Unlock()
	if ok {
		link.emit("health", "ok", latency)
	} else {
		link.emit("health", "failed", 0)
	}
}

func (link *Link) persist() error {
	link.persistMu.Lock()
	defer link.persistMu.Unlock()
	link.mu.Lock()
	link.seq++
	auth, err := json.Marshal(link.auth)
	state := persistedState{Version: stateVersion, Seq: link.seq, State: link.state, PairingFrom: link.pairingFrom, StopCode: link.stopCode, Auth: auth}
	encoded, marshalErr := json.Marshal(state)
	link.mu.Unlock()
	if err != nil || marshalErr != nil {
		return errors.New("state_persistence_failed")
	}
	if err := link.host.PersistState(encoded); err != nil {
		return errors.New("state_persistence_failed")
	}
	return nil
}

func (link *Link) emit(kind, code string, value int64) {
	if isEventCode(kind, code) {
		link.host.Event(kind, code, value)
	}
}

func (link *Link) pairResultError(err error) error {
	if isCookieRefusal(err) {
		link.signOut("cookie_refused")
		return errors.New("cookie_refused")
	}
	return errors.New(pairCode(err))
}

func pairCode(err error) string {
	switch {
	case errors.Is(err, context.Canceled):
		return "pair_cancelled"
	case errors.Is(err, libgm.ErrNoDevicesFound):
		return "pair_no_phone"
	case errors.Is(err, libgm.ErrHadMultipleDevices):
		return "pair_multiple_phones"
	case errors.Is(err, libgm.ErrIncorrectEmoji):
		return "pair_wrong_emoji"
	case errors.Is(err, libgm.ErrPairingCancelled):
		return "pair_cancelled"
	case errors.Is(err, libgm.ErrPairingTimeout), errors.Is(err, libgm.ErrPairingInitTimeout):
		return "pair_timeout"
	default:
		return "pair_failed"
	}
}

func isCookieRefusal(err error) bool {
	var transportErr transportStatusError
	return errors.As(err, &transportErr) && transportErr.status >= http.StatusMultipleChoices && transportErr.status < http.StatusBadRequest
}

func isFatalAuthError(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, events.ErrInvalidCredentials) || errors.Is(err, events.ErrRequestedEntityNotFound) {
		return true
	}
	var httpErr events.HTTPError
	return errors.As(err, &httpErr) && httpErr.Resp != nil && (httpErr.Resp.StatusCode == http.StatusUnauthorized || httpErr.Resp.StatusCode == http.StatusForbidden || httpErr.Resp.StatusCode == http.StatusNotFound)
}

func validCookies(cookies map[string]string) bool {
	allowed := map[string]bool{"OSID": true, "SID": true, "HSID": true, "SSID": true, "APISID": true, "SAPISID": true, "__Secure-1PSIDTS": true}
	for name, value := range cookies {
		if !allowed[name] || value == "" {
			return false
		}
	}
	for name := range allowed {
		if name != "__Secure-1PSIDTS" && cookies[name] == "" {
			return false
		}
	}
	return true
}
