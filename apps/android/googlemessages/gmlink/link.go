package gmlink

import (
	"context"
	"encoding/json"
	"errors"
	"maps"
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
	"google.golang.org/protobuf/proto"
)

// Host is implemented by the Kotlin runtime.
type Host interface {
	PersistState(sealed []byte) error
	Event(kind, code string, value int64)
	ShowEmoji(emoji string)
}

var (
	errPersistFailed = errors.New("persist_failed")
	errLinkClosed    = errors.New("link_closed")
)

// Link owns one Google Messages link for the process.
type Link struct {
	host Host

	mu          sync.Mutex
	rpcMu       sync.Mutex
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
	closed      bool
	unlinking   bool
	stopRun     context.CancelFunc
	pairCancel  context.CancelFunc
	pairDone    chan struct{}

	changes            chan any
	conversationEvents []conversationEvent
	conversationIndex  map[string]conversationSummary
	settings           *gmproto.Settings
	settingsAt         time.Time
	settingsVersion    uint64

	// m2VerificationPassed is supplied by the Android host only after the owner-gated
	// on-device procedures recorded in g0-libgm-verification.md have passed. It is
	// deliberately runtime-only: a reconnect or relink must re-establish the proof.
	m2VerificationPassed bool
}

var openLink atomic.Pointer[Link]

// Call deadlines. These are variables only so tests can inject short deadlines
// against a fixture that never answers; production never reassigns them.
var (
	configTimeout   = 30 * time.Second
	unpairTimeout   = 30 * time.Second
	healthTimeout   = 30 * time.Second
	pairWaitTimeout = 5 * time.Minute
	// pairUnwindTimeout bounds how long Unlink waits for a cancelled pair to unwind.
	pairUnwindTimeout = 10 * time.Second
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
	link := &Link{host: host, state: decoded.State, stopCode: decoded.StopCode, seq: decoded.Seq, changes: make(chan any, 64), conversationIndex: make(map[string]conversationSummary)}
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
				return nil, errPersistFailed
			}
		}
	}
	return link, nil
}

// Close aborts work, stops Run, ends the event consumer, and releases the process-wide
// link slot. Once Close returns, the link persists nothing and reports nothing.
func (link *Link) Close() {
	link.mu.Lock()
	if !link.closed {
		link.closed = true
		close(link.changes)
	}
	if link.pairCancel != nil {
		link.pairCancel()
	}
	link.stopRunLocked()
	link.mu.Unlock()
	// Wait for a persist already in progress: none may reach the host after Close returns.
	link.persistMu.Lock()
	link.persistMu.Unlock()
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
	return link.pair(auth, false, "")
}

// Relink repairs an existing link using its stored cookies.
func (link *Link) Relink() error {
	link.mu.Lock()
	if link.closed {
		link.mu.Unlock()
		return errLinkClosed
	}
	if link.unlinking {
		link.mu.Unlock()
		return errors.New("unlink_in_progress")
	}
	if link.state != stateNeedsRelink {
		link.mu.Unlock()
		return errors.New("relink_refused")
	}
	auth := link.auth.CopyForRepair()
	storedSource := link.auth.Mobile.GetSourceID()
	link.mu.Unlock()
	return link.pair(auth, true, storedSource)
}

// pair runs one pairing. A relink must name the same account as storedSource
// before any pairing request is sent.
func (link *Link) pair(auth *libgm.AuthData, relink bool, storedSource string) (err error) {
	link.mu.Lock()
	previous := link.state
	if link.closed {
		link.mu.Unlock()
		return errLinkClosed
	}
	if link.unlinking {
		link.mu.Unlock()
		return errors.New("unlink_in_progress")
	}
	if (!relink && previous != stateUnlinked && previous != stateSignedOut) || (relink && previous != stateNeedsRelink) {
		link.mu.Unlock()
		return errors.New("pair_refused")
	}
	link.state, link.pairingFrom, link.stopCode = statePairing, previous, ""
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	link.pairCancel, link.pairDone = cancel, done
	link.mu.Unlock()
	completed := false
	defer func() {
		cancel()
		link.mu.Lock()
		// Clear only this pair's own handles: a newer pair may already own them.
		if link.pairDone == done {
			link.pairCancel, link.pairDone = nil, nil
		}
		link.mu.Unlock()
		if !completed {
			code := pairCode(err)
			if link.restorePair(previous, code) {
				link.emit("link", code, 0)
			}
		}
		// Unlink waits on done, so it sees the restored or committed state.
		close(done)
	}()
	if err = link.persist(); err != nil {
		return pairFailure("persist_failed")
	}
	persist := link.persist
	if relink {
		persist = link.persistRotated(auth, done)
	}
	client := link.newClient(auth, persist)
	defer client.Disconnect()
	configCtx, cancelConfig := context.WithTimeout(ctx, configTimeout)
	err = client.FetchConfig(configCtx)
	cancelConfig()
	if err != nil {
		return link.pairResultError(err, relink)
	}
	email := client.Config.GetDeviceInfo().GetEmail()
	if email == "" {
		return pairFailure("pair_failed")
	}
	if relink && !strings.EqualFold(email, storedSource) {
		link.signOutIf("account_mismatch", pairingState)
		return pairFailure("account_mismatch")
	}
	// The pairing long poll lives as long as the pair, so the phone's replies can arrive.
	startCtx, cancelStart := context.WithTimeout(ctx, configTimeout)
	emoji, pairing, err := client.StartGaiaPairing(startCtx, ctx)
	cancelStart()
	if err != nil {
		return link.pairResultError(err, relink)
	}
	link.host.ShowEmoji(emoji)
	finishCtx, cancelFinish := context.WithTimeout(ctx, pairWaitTimeout)
	_, err = client.FinishGaiaPairing(finishCtx, pairing)
	cancelFinish()
	if err != nil {
		return link.pairResultError(err, relink)
	}
	if !strings.EqualFold(email, auth.Mobile.GetSourceID()) {
		// A fresh pair restores the previous state; a relink signs out, as Run does.
		if relink {
			link.signOutIf("account_mismatch", pairingState)
		}
		return pairFailure("account_mismatch")
	}
	link.mu.Lock()
	if link.state != statePairing {
		// Unlink or Close superseded this pair while it was finishing; commit nothing.
		link.mu.Unlock()
		return pairFailure("pair_cancelled")
	}
	link.auth, link.state, link.pairingFrom, link.stopCode = auth, stateLinked, "", ""
	link.mu.Unlock()
	if err = link.persist(); err != nil {
		link.stopFatal("persist_failed", false)
		return errPersistFailed
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
	// A closed link writes nothing and keeps the pair's own reason as its stop code.
	if err := link.persist(); err != nil && !errors.Is(err, errLinkClosed) {
		link.stopFatal("persist_failed", false)
	}
	return true
}

// Run keeps the linked client connected until Stop, Close or a terminal state.
func (link *Link) Run() error {
	runCtx, cancelRun := context.WithCancel(context.Background())
	defer cancelRun()
	link.mu.Lock()
	if link.closed {
		link.mu.Unlock()
		return errLinkClosed
	}
	if link.unlinking {
		link.mu.Unlock()
		return errors.New("run_refused")
	}
	if link.running || link.state != stateLinked || link.auth.Browser == nil {
		link.mu.Unlock()
		return errors.New("run_refused")
	}
	auth := link.auth
	client := link.newClient(auth, link.persist)
	link.client, link.running, link.stopRun = client, true, cancelRun
	link.mu.Unlock()
	defer func() {
		link.mu.Lock()
		if link.client == client {
			link.client = nil
		}
		link.running = false
		link.healthy = false
		link.stopRun = nil
		link.mu.Unlock()
		client.Disconnect()
	}()
	client.SetPingInterval(20 * time.Minute)
	configCtx, cancelConfig := context.WithTimeout(runCtx, configTimeout)
	err := client.FetchConfig(configCtx)
	cancelConfig()
	if err != nil {
		if isCookieRefusal(err) {
			// The transport has already signed the link out for the /web/config redirect.
			return errors.New("cookie_refused")
		}
		if runCtx.Err() != nil {
			return nil
		}
		link.emit("conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	email := client.Config.GetDeviceInfo().GetEmail()
	if email == "" {
		// A blank config answer is not evidence of a different account: retry, keep the auth.
		link.emit("conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	if !strings.EqualFold(email, auth.Mobile.GetSourceID()) {
		link.signOutIf("account_mismatch", activeState)
		return errors.New("account_mismatch")
	}
	// libgm sends its token refresh inside Connect without a context, so that refresh is
	// bounded only by libgm's 5-minute client timeout. This is a documented exception (ruling R34).
	// The long poll takes the run context, so Stop ends it.
	if err := client.Connect(runCtx); err != nil {
		if isFatalAuthError(err) {
			link.signOutIf("signed_out", activeState)
			return errors.New("fatal_refresh")
		}
		link.emit("conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	link.mu.Lock()
	link.healthy = true
	link.mu.Unlock()
	link.emit("conn", "ready", 0)
	<-runCtx.Done()
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
	ctx, cancel := context.WithTimeout(context.Background(), healthTimeout)
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

// RefreshConversationIndex refreshes the bounded local conversation index.
// The pinned libgm facts have not been verified offline, so indexing remains
// unavailable until the host records the required owner-gated evidence.
func (link *Link) RefreshConversationIndex() error {
	if !link.hasM2Verification() {
		return errors.New("m2_verification_required")
	}
	client := link.activeClient()
	if client == nil {
		return errors.New("link_unavailable")
	}
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	for _, folder := range []gmproto.ListConversationsRequest_Folder{gmproto.ListConversationsRequest_INBOX, gmproto.ListConversationsRequest_ARCHIVE, gmproto.ListConversationsRequest_SPAM_BLOCKED} {
		var cursor *gmproto.Cursor
		for page := 0; page < conversationPageLimit; page++ {
			ctx, cancel := context.WithTimeout(context.Background(), conversationTimeout)
			response, err := client.ListConversations(ctx, &gmproto.ListConversationsRequest{Count: conversationPageSize, Folder: folder, Cursor: cursor})
			cancel()
			if err != nil || response == nil {
				return errors.New("conversation_lookup_failed")
			}
			link.indexConversations(response.GetConversations())
			cursor = response.GetCursor()
			if cursor == nil {
				break
			}
			if page == conversationPageLimit-1 {
				return errors.New("conversation_lookup_incomplete")
			}
		}
	}
	return nil
}

// GetConversation returns only fixed-code local lookup status. It never exposes
// Google participant data through the gomobile boundary.
func (link *Link) GetConversation(googleConversationID string) string {
	if googleConversationID == "" {
		return conversationJSON("invalid_request")
	}
	if !link.hasM2Verification() {
		return conversationJSON("m2_verification_required")
	}
	client := link.activeClient()
	if client == nil {
		return conversationJSON("link_unavailable")
	}
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	conversation, code := link.getConversation(client, googleConversationID)
	if code != "" {
		return conversationJSON(code)
	}
	link.indexConversations([]*gmproto.Conversation{conversation})
	return conversationJSON("conversation_found")
}

// ResolveExistingConversation refuses implicit creation. A target is accepted
// only by a future verified local index; it is never sent to a create-capable RPC.
func (link *Link) ResolveExistingConversation(target string) string {
	if target == "" {
		return conversationJSON("invalid_request")
	}
	if !link.hasM2Verification() {
		return conversationJSON("m2_verification_required")
	}
	link.mu.Lock()
	summary, found := link.conversationIndex[target]
	link.mu.Unlock()
	if found {
		return resolvedConversationJSON(summary.id)
	}
	return conversationJSON("implicit_creation_refused")
}

// DrainConversationEvents returns local, content-free event correlations.
func (link *Link) DrainConversationEvents() string {
	link.mu.Lock()
	events := append([]conversationEvent(nil), link.conversationEvents...)
	link.conversationEvents = nil
	link.mu.Unlock()
	encoded, _ := json.Marshal(events)
	return string(encoded)
}

func (link *Link) hasM2Verification() bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	return !link.closed && link.m2VerificationPassed
}

func (link *Link) activeClient() *libgm.Client {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || link.state != stateLinked || link.client == nil {
		return nil
	}
	return link.client
}

// Unlink cancels any in-flight pair or relink and waits, bounded, for it to unwind. Then
// it unpairs the pairing the link ends up with, once, and wipes the local link. A pair
// that commits first is unpaired; one that loses commits nothing.
func (link *Link) Unlink() error {
	link.mu.Lock()
	if link.closed {
		link.mu.Unlock()
		return errLinkClosed
	}
	if link.unlinking {
		link.mu.Unlock()
		return errors.New("unlink_in_progress")
	}
	// From here until the final block, Pair, Relink and Run are refused.
	link.unlinking = true
	var pairDone chan struct{}
	if link.pairCancel != nil {
		link.pairCancel()
		pairDone = link.pairDone
	}
	link.mu.Unlock()
	if pairDone != nil {
		select {
		case <-pairDone:
		case <-time.After(pairUnwindTimeout):
		}
	}
	link.Stop()
	link.mu.Lock()
	if link.closed {
		// Close landed while the pair unwound: no remote call, nothing persisted.
		link.unlinking = false
		link.mu.Unlock()
		return errLinkClosed
	}
	auth := link.auth
	paired := auth != nil && auth.PairingID != uuid.Nil
	if link.state == statePairing {
		// A pair that outlived the bound must not commit over the unlink.
		link.state, link.pairingFrom = stateUnlinked, ""
	}
	link.mu.Unlock()
	var err error
	if paired {
		client := link.newClient(auth, link.persist)
		ctx, cancel := context.WithTimeout(context.Background(), unpairTimeout)
		err = client.UnpairGaia(ctx)
		cancel()
		client.Disconnect()
	}
	link.mu.Lock()
	link.state, link.pairingFrom, link.auth = stateUnlinked, "", libgm.NewAuthData()
	link.healthy, link.responding, link.lastOKMS = false, false, 0
	link.unlinking = false
	if err != nil {
		link.stopCode = "unlink_remote_failed"
	} else {
		link.stopCode = ""
	}
	link.mu.Unlock()
	if persistErr := link.persist(); persistErr != nil {
		if errors.Is(persistErr, errLinkClosed) {
			return errLinkClosed
		}
		link.stopFatal("persist_failed", false)
		return errPersistFailed
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
	link.stopRunLocked()
	link.mu.Unlock()
}

// stopRunLocked cancels the current Run, if any. Callers hold link.mu.
func (link *Link) stopRunLocked() {
	if link.stopRun != nil {
		link.stopRun()
	}
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

// newClient builds a libgm client whose transport persists through persist.
func (link *Link) newClient(auth *libgm.AuthData, persist func() error) *libgm.Client {
	client := libgm.NewClient(auth, nil, zerolog.Nop(), newGuardedSettings(auth, persist, link.transportStop))
	client.SetEventHandler(link.queueEvent)
	return client
}

// persistRotated persists a relink's rotated cookies. The stored auth takes them, so a
// relink that fails keeps the rotations. Only the pair that created the client, while it
// still owns the link, takes them: a rotation from an earlier pair, or one that lands
// after sign-out or unlink, writes nothing.
func (link *Link) persistRotated(candidate *libgm.AuthData, owner chan struct{}) func() error {
	return func() error {
		candidate.CookiesLock.RLock()
		cookies := maps.Clone(candidate.Cookies)
		candidate.CookiesLock.RUnlock()
		link.mu.Lock()
		if link.state != statePairing || link.pairDone != owner {
			link.mu.Unlock()
			return nil
		}
		link.auth.SetCookies(cookies)
		link.mu.Unlock()
		return link.persist()
	}
}

// queueEvent hands one event to the consumer. Only events the consumer handles are
// queued, and a full queue stops the link with its own code.
func (link *Link) queueEvent(event any) {
	if !relevantEvent(event) {
		return
	}
	event = cloneQueuedEvent(event)
	link.mu.Lock()
	if link.closed {
		link.mu.Unlock()
		return
	}
	select {
	case link.changes <- event:
		link.mu.Unlock()
	default:
		link.mu.Unlock()
		link.stopFatal("event_overflow", false)
	}
}

func cloneQueuedEvent(event any) any {
	settings, ok := event.(*libgm.WrappedSettings)
	if !ok || settings == nil || settings.Settings == nil {
		return event
	}
	cloned, ok := proto.Clone(settings.Settings).(*gmproto.Settings)
	if !ok {
		return &libgm.WrappedSettings{IsOld: settings.IsOld}
	}
	return &libgm.WrappedSettings{Settings: cloned, IsOld: settings.IsOld}
}

// relevantEvent reports whether projectEvent handles the event.
func relevantEvent(event any) bool {
	switch event.(type) {
	case *events.AuthTokenRefreshed, *events.GaiaLoggedOut, *events.ListenFatalError, *events.AccountChange,
		*events.ListenTemporaryError, *events.ListenRecovered, *events.PhoneNotResponding,
		*events.PhoneRespondingAgain, *events.NoDataReceived, *events.ActiveSessionSet, *gmproto.UserAlertEvent,
		*gmproto.Conversation, *libgm.WrappedSettings:
		return true
	}
	return false
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
		link.transition(stateNeedsRelink, "relink_needed")
	case *events.ListenFatalError:
		if isFatalAuthError(value.Error) {
			link.signOutIf("signed_out", activeState)
		} else {
			link.transition(stateNeedsRelink, "relink_needed")
		}
	case *events.AccountChange:
		if !value.IsFake {
			link.signOutIf("signed_out", activeState)
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
	case *gmproto.Conversation:
		link.recordConversationEvent(value)
	case *libgm.WrappedSettings:
		link.recordSettings(value)
	case *gmproto.Settings:
		link.mu.Lock()
		link.invalidateSettingsLocked()
		link.mu.Unlock()
	}
}

// transportStop receives the transport's stop requests. A /web/config redirect
// signs out a linked or needs_relink link. During Pair or Relink the outcome is
// decided by the error that call returns.
func (link *Link) transportStop(code string) {
	switch code {
	case "cookie_refused":
		link.signOutIf(code, activeState)
	case "persist_failed":
		link.stopFatal(code, false)
	}
}

// transition moves a linked link to needs_relink, persists and reports it, then ends Run.
// Any other state is left alone, and nothing is persisted or reported unless it changed.
func (link *Link) transition(state, code string) {
	link.mu.Lock()
	if link.state != stateLinked || link.unlinking {
		link.mu.Unlock()
		return
	}
	link.state, link.stopCode, link.healthy, link.responding = state, "", false, false
	link.conversationIndex, link.conversationEvents, link.settings, link.settingsAt = make(map[string]conversationSummary), nil, nil, time.Time{}
	link.mu.Unlock()
	if err := link.persist(); err != nil {
		link.stopFatal("persist_failed", false)
		return
	}
	link.emit("link", code, 0)
	link.Stop()
}

// activeState is a state a sign-out may leave: linked, or needs relink.
func activeState(state string) bool {
	return state == stateLinked || state == stateNeedsRelink
}

// pairingState is the state a pair or relink holds while it runs.
func pairingState(state string) bool {
	return state == statePairing
}

// signOutIf wipes the link and signs it out, but only from a state allowed() accepts.
// The check and the change happen under one lock, so an event cannot act on a state
// that changed under it.
func (link *Link) signOutIf(code string, allowed func(state string) bool) {
	link.mu.Lock()
	// An unlink in progress owns the state change; events from stopped clients must not undo it.
	if !allowed(link.state) || link.unlinking {
		link.mu.Unlock()
		return
	}
	link.state, link.pairingFrom, link.stopCode, link.auth = stateSignedOut, "", code, libgm.NewAuthData()
	link.healthy, link.responding, link.lastOKMS = false, false, 0
	link.conversationIndex, link.conversationEvents, link.settings, link.settingsAt = make(map[string]conversationSummary), nil, nil, time.Time{}
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
	link.stopRunLocked()
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

// persist writes the envelope. It writes nothing once the link is closed.
func (link *Link) persist() error {
	link.persistMu.Lock()
	defer link.persistMu.Unlock()
	link.mu.Lock()
	if link.closed {
		link.mu.Unlock()
		return errLinkClosed
	}
	link.seq++
	auth, err := json.Marshal(link.auth)
	state := persistedState{Version: stateVersion, Seq: link.seq, State: link.state, PairingFrom: link.pairingFrom, StopCode: link.stopCode, Auth: auth}
	encoded, marshalErr := json.Marshal(state)
	link.mu.Unlock()
	if err != nil || marshalErr != nil {
		return errPersistFailed
	}
	if err := link.host.PersistState(encoded); err != nil {
		return errPersistFailed
	}
	return nil
}

// emit reports a listed event to the host. Nothing reaches the host after Close.
func (link *Link) emit(kind, code string, value int64) {
	if !isEventCode(kind, code) {
		return
	}
	link.mu.Lock()
	closed := link.closed
	link.mu.Unlock()
	if !closed {
		link.host.Event(kind, code, value)
	}
}

// pairFailure is a fixed pair outcome. Its text is the reason code the host sees.
type pairFailure string

func (code pairFailure) Error() string { return string(code) }

// pairResultError maps a failed pair step to its fixed code. A relink the phone refuses
// for its cookies signs out, because the stored cookies are dead.
func (link *Link) pairResultError(err error, relink bool) error {
	if relink && isCookieRefusal(err) {
		link.signOutIf("cookie_refused", pairingState)
	}
	return pairFailure(pairCode(err))
}

func pairCode(err error) string {
	var failure pairFailure
	switch {
	case errors.As(err, &failure):
		return string(failure)
	case isCookieRefusal(err):
		return "cookie_refused"
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
	case errors.Is(err, libgm.ErrPairingTimeout), errors.Is(err, libgm.ErrPairingInitTimeout), errors.Is(err, context.DeadlineExceeded):
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
