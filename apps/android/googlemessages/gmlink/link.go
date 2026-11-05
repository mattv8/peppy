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
	errPersistFailed                = errors.New("persist_failed")
	errLinkClosed                   = errors.New("link_closed")
	errOwnerVerificationInvalid     = errors.New("owner_verification_invalid")
	errOwnerVerificationUnavailable = errors.New("owner_verification_unavailable")
)

// Link owns one Google Messages link for the process.
type Link struct {
	host Host

	mu            sync.Mutex
	rpcMu         sync.Mutex
	persistMu     sync.Mutex
	state         string
	pairingFrom   string
	stopCode      string
	auth          *libgm.AuthData
	seq           uint64
	client        *libgm.Client
	running       bool
	healthy       bool
	responding    bool
	lastOKMS      int64
	closed        bool
	unlinking     bool
	stopRun       context.CancelFunc
	runCtx        context.Context
	generation    uint64
	runGeneration uint64
	pairCancel    context.CancelFunc
	pairDone      chan struct{}

	changes            chan any
	conversationEvents []conversationEvent
	conversationIndex  map[string]conversationSummary
	settings           *gmproto.Settings
	settingsAt         time.Time
	settingsVersion    uint64
	streamEpoch        uint64

	ownerVerification *ownerVerification
}

// queuedEvent is fenced twice: before it enters the queue and immediately before
// it mutates Link state. A callback from a retired libgm client is never current.
type queuedEvent struct {
	generation  uint64
	streamEpoch uint64
	receivedAt  time.Time
	event       any
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
		link.revokeLocked()
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
	link.revokeLocked()
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
	auth, err := cloneAuthData(link.auth)
	if err != nil {
		link.mu.Unlock()
		return errPersistFailed
	}
	link.generation++
	generation := link.generation
	client := link.newRuntimeClient(auth, generation)
	link.client, link.running, link.stopRun, link.runCtx, link.runGeneration = client, true, cancelRun, runCtx, generation
	link.mu.Unlock()
	defer func() {
		link.mu.Lock()
		if link.client == client && link.runGeneration == generation {
			link.generation++
			link.invalidateStreamLocked()
			link.ownerVerification = nil
			link.conversationIndex = make(map[string]conversationSummary)
			link.conversationEvents = nil
			link.client = nil
			link.runCtx = nil
			link.runGeneration = 0
			link.stopRun = nil
			link.healthy = false
			link.running = false
		}
		link.mu.Unlock()
		client.Disconnect()
	}()
	client.SetPingInterval(20 * time.Minute)
	configCtx, cancelConfig := context.WithTimeout(runCtx, configTimeout)
	err = client.FetchConfig(configCtx)
	cancelConfig()
	if err != nil {
		if isCookieRefusal(err) {
			// The transport has already signed the link out for the /web/config redirect.
			return errors.New("cookie_refused")
		}
		if runCtx.Err() != nil {
			return nil
		}
		link.emitFor(generation, "conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	email := client.Config.GetDeviceInfo().GetEmail()
	if email == "" {
		// A blank config answer is not evidence of a different account: retry, keep the auth.
		link.emitFor(generation, "conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	if !strings.EqualFold(email, auth.Mobile.GetSourceID()) {
		link.signOutIfFor(generation, "account_mismatch", activeState)
		return errors.New("account_mismatch")
	}
	// libgm sends its token refresh inside Connect without a context, so that refresh is
	// bounded only by libgm's 5-minute client timeout. This is a documented exception (ruling R34).
	// The long poll takes the run context, so Stop ends it.
	if err := client.Connect(runCtx); err != nil {
		if isFatalAuthError(err) {
			link.signOutIfFor(generation, "signed_out", activeState)
			return errors.New("fatal_refresh")
		}
		link.emitFor(generation, "conn", "temporary_error", 0)
		return errors.New("run_retryable")
	}
	link.mu.Lock()
	if link.generation != generation || link.client != client || link.runGeneration != generation {
		link.mu.Unlock()
		return nil
	}
	link.healthy = true
	link.mu.Unlock()
	link.emitFor(generation, "conn", "ready", 0)
	<-runCtx.Done()
	return nil
}

// CheckHealth checks one inbox conversation and returns its latency in milliseconds, or -1.
func (link *Link) CheckHealth() int64 {
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	client, runCtx, generation, ok := link.activeRuntime()
	if !ok {
		return -1
	}
	ctx, cancel := context.WithTimeout(runCtx, healthTimeout)
	defer cancel()
	started := time.Now()
	_, err := client.ListConversations(ctx, &gmproto.ListConversationsRequest{Count: 1, Folder: gmproto.ListConversationsRequest_INBOX})
	if err != nil {
		link.recordHealthFor(generation, false, 0)
		return -1
	}
	latency := time.Since(started).Milliseconds()
	link.recordHealthFor(generation, true, latency)
	return latency
}

// RefreshConversationIndex refreshes the bounded local conversation index.
// The pinned libgm facts have not been verified offline, so indexing remains
// unavailable until the host records the required owner-gated evidence.
func (link *Link) RefreshConversationIndex() error {
	if !link.hasM2Verification() {
		return errors.New("m2_verification_required")
	}
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	client, runCtx, generation, ok := link.activeRuntime()
	if !ok {
		return errors.New("link_unavailable")
	}
	if !link.hasM2Verification() {
		return errors.New("m2_verification_required")
	}
	for _, folder := range []gmproto.ListConversationsRequest_Folder{gmproto.ListConversationsRequest_INBOX, gmproto.ListConversationsRequest_ARCHIVE, gmproto.ListConversationsRequest_SPAM_BLOCKED} {
		var cursor *gmproto.Cursor
		for page := 0; page < conversationPageLimit; page++ {
			ctx, cancel := context.WithTimeout(runCtx, conversationTimeout)
			response, err := client.ListConversations(ctx, &gmproto.ListConversationsRequest{Count: conversationPageSize, Folder: folder, Cursor: cursor})
			cancel()
			if err != nil || response == nil {
				return errors.New("conversation_lookup_failed")
			}
			if !link.indexConversationsFor(generation, response.GetConversations()) {
				return errors.New("link_unavailable")
			}
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
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	client, runCtx, generation, ok := link.activeRuntime()
	if !ok {
		return conversationJSON("link_unavailable")
	}
	if !link.hasM2Verification() {
		return conversationJSON("m2_verification_required")
	}
	conversation, code := link.getConversation(runCtx, client, googleConversationID)
	if code != "" {
		return conversationJSON(code)
	}
	if !link.indexConversationsFor(generation, []*gmproto.Conversation{conversation}) {
		return conversationJSON("link_unavailable")
	}
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
	return link.hasOwnerVerificationLocked()
}

// activeRuntime is called only after rpcMu is acquired. It selects the client
// and its cancellable generation-owned context atomically with revalidation.
func (link *Link) activeRuntime() (*libgm.Client, context.Context, uint64, bool) {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || link.state != stateLinked || !link.running || link.client == nil || link.runCtx == nil || link.runGeneration != link.generation {
		return nil, nil, 0, false
	}
	return link.client, link.runCtx, link.runGeneration, true
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
	link.revokeLocked()
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
	link.revokeLocked()
	link.mu.Unlock()
}

// stopRunLocked cancels the current Run, if any. Callers hold link.mu.
func (link *Link) stopRunLocked() {
	if link.stopRun != nil {
		link.stopRun()
	}
}

// revokeLocked is the single admission linearization point for a runtime
// lifetime. It cancels all run-derived RPC contexts before any retired callback
// can observe settings or readiness. It cannot recall bytes already admitted to
// libgm's writer; callers must treat an admitted request as outcome-unknown.
func (link *Link) invalidateStreamLocked() {
	link.streamEpoch++
	link.invalidateSettingsLocked()
}

func (link *Link) revokeLocked() {
	link.generation++
	link.ownerVerification = nil
	link.conversationIndex = make(map[string]conversationSummary)
	link.conversationEvents = nil
	link.invalidateStreamLocked()
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
func (link *Link) newClient(auth *libgm.AuthData, persist func() error, generations ...uint64) *libgm.Client {
	client := libgm.NewClient(auth, nil, zerolog.Nop(), newGuardedSettings(auth, persist, link.transportStop))
	var generation uint64
	if len(generations) == 1 {
		generation = generations[0]
	} else {
		link.mu.Lock()
		generation = link.generation
		link.mu.Unlock()
	}
	client.SetEventHandler(func(event any) { link.queueEventFor(generation, event) })
	return client
}

func (link *Link) newRuntimeClient(auth *libgm.AuthData, generation uint64) *libgm.Client {
	persist := func() error { return link.persistRuntimeAuth(generation, auth) }
	stop := func(code string) { link.transportStopFor(generation, code) }
	client := libgm.NewClient(auth, nil, zerolog.Nop(), newGuardedSettings(auth, persist, stop))
	client.SetEventHandler(func(event any) {
		if _, ok := event.(*events.AuthTokenRefreshed); ok {
			if err := link.persistRuntimeAuth(generation, auth); err != nil {
				link.stopFatalFor(generation, "persist_failed", false)
			}
			return
		}
		link.queueEventFor(generation, event)
	})
	return client
}

func cloneAuthData(auth *libgm.AuthData) (*libgm.AuthData, error) {
	encoded, err := json.Marshal(auth)
	if err != nil {
		return nil, err
	}
	cloned := libgm.NewAuthData()
	if err := json.Unmarshal(encoded, cloned); err != nil {
		return nil, err
	}
	return cloned, nil
}

func (link *Link) persistRuntimeAuth(generation uint64, candidate *libgm.AuthData) error {
	cloned, err := cloneAuthData(candidate)
	if err != nil {
		return errPersistFailed
	}
	link.mu.Lock()
	if generation != link.generation || link.runGeneration != generation {
		link.mu.Unlock()
		return nil
	}
	link.auth = cloned
	link.mu.Unlock()
	return link.persistForGeneration(generation)
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
	link.mu.Lock()
	generation := link.generation
	link.mu.Unlock()
	link.queueEventFor(generation, event)
}

func (link *Link) queueEventFor(generation uint64, event any) {
	if !relevantEvent(event) {
		return
	}
	event = cloneQueuedEvent(event)
	receivedAt := time.Now()
	link.mu.Lock()
	if link.closed || generation != link.generation {
		link.mu.Unlock()
		return
	}
	if invalidatesAtIngress(event) {
		link.invalidateStreamLocked()
		link.conversationIndex = make(map[string]conversationSummary)
	}
	if terminalAtIngress(event) {
		link.invalidateSettingsLocked()
		link.conversationIndex = make(map[string]conversationSummary)
		link.ownerVerification = nil
		link.healthy = false
	}
	if invalidatesOwnerEvidence(event) {
		link.ownerVerification = nil
	}
	queued := queuedEvent{generation: generation, streamEpoch: link.streamEpoch, receivedAt: receivedAt, event: event}
	select {
	case link.changes <- queued:
		link.mu.Unlock()
	default:
		link.mu.Unlock()
		link.stopFatalFor(generation, "event_overflow", false)
	}
}

func cloneQueuedEvent(event any) any {
	switch settings := event.(type) {
	case *libgm.WrappedSettings:
		if settings == nil || settings.Settings == nil {
			return event
		}
		cloned, ok := proto.Clone(settings.Settings).(*gmproto.Settings)
		if !ok {
			return &libgm.WrappedSettings{IsOld: settings.IsOld}
		}
		return &libgm.WrappedSettings{Settings: cloned, IsOld: settings.IsOld}
	case *gmproto.Settings:
		cloned, ok := proto.Clone(settings).(*gmproto.Settings)
		if ok {
			return cloned
		}
	}
	return event
}

// relevantEvent reports whether projectEvent handles the event.
func relevantEvent(event any) bool {
	switch event.(type) {
	case *events.AuthTokenRefreshed, *events.GaiaLoggedOut, *events.ListenFatalError, *events.AccountChange,
		*events.ListenTemporaryError, *events.ListenRecovered, *events.PhoneNotResponding,
		*events.PhoneRespondingAgain, *events.NoDataReceived, *events.ActiveSessionSet, *gmproto.UserAlertEvent,
		*gmproto.Conversation, *libgm.WrappedSettings, *gmproto.Settings, *events.StreamGap:
		return true
	}
	return false
}

func (link *Link) consumeEvents() {
	for event := range link.changes {
		link.consumeQueuedEvent(event)
	}
}

func (link *Link) consumeQueuedEvent(queued any) {
	event, ok := queued.(queuedEvent)
	if !ok {
		return
	}
	link.projectQueuedEvent(event)
}

func invalidatesAtIngress(event any) bool {
	switch event.(type) {
	case *events.ListenTemporaryError, *events.NoDataReceived, *events.ActiveSessionSet,
		*events.StreamGap, *gmproto.Settings:
		return true
	case *libgm.WrappedSettings:
		value := event.(*libgm.WrappedSettings)
		return value == nil || value.IsOld || value.Settings == nil || value.Settings.GetSIMCards() == nil
	default:
		return false
	}
}

func terminalAtIngress(event any) bool {
	switch value := event.(type) {
	case *events.GaiaLoggedOut, *events.ListenFatalError, *events.PhoneNotResponding:
		return true
	case *events.AccountChange:
		return !value.IsFake
	default:
		return false
	}
}

func invalidatesOwnerEvidence(event any) bool {
	return invalidatesAtIngress(event) || terminalAtIngress(event)
}

func (link *Link) projectQueuedEvent(queued queuedEvent) {
	switch value := queued.event.(type) {
	case *libgm.WrappedSettings:
		link.recordSettingsFor(queued.generation, queued.streamEpoch, queued.receivedAt, value)
	case *gmproto.Conversation:
		link.recordConversationEventFor(queued.generation, value)
	case *events.GaiaLoggedOut:
		link.transitionFor(queued.generation, stateNeedsRelink, "relink_needed")
	case *events.ListenFatalError:
		if isFatalAuthError(value.Error) {
			link.signOutIfFor(queued.generation, "signed_out", activeState)
		} else {
			link.transitionFor(queued.generation, stateNeedsRelink, "relink_needed")
		}
	case *events.AccountChange:
		if !value.IsFake {
			link.signOutIfFor(queued.generation, "signed_out", activeState)
		}
	case *events.PhoneNotResponding:
		if link.setRespondingFor(queued.generation, false) {
			link.emitFor(queued.generation, "conn", "phone_not_responding", 0)
		}
	case *events.PhoneRespondingAgain:
		if link.setRespondingFor(queued.generation, true) {
			link.emitFor(queued.generation, "conn", "phone_responding", 0)
		}
	case *events.ListenTemporaryError, *events.StreamGap:
		link.emitFor(queued.generation, "conn", "temporary_error", 0)
	case *events.ListenRecovered:
		link.emitFor(queued.generation, "conn", "recovered", 0)
	case *events.NoDataReceived:
		link.emitFor(queued.generation, "conn", "no_data", 0)
	case *events.ActiveSessionSet:
		link.emitFor(queued.generation, "session", "active_session_set", 0)
	case *gmproto.UserAlertEvent:
		if value.GetAlertType() == gmproto.AlertType_BROWSER_ACTIVE {
			link.emitFor(queued.generation, "browser", "active", 0)
		} else if value.GetAlertType() == gmproto.AlertType_BROWSER_INACTIVE {
			link.emitFor(queued.generation, "browser", "inactive", 0)
		}
	}
}

func (link *Link) setRespondingFor(generation uint64, responding bool) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || generation != link.generation {
		return false
	}
	link.responding = responding
	return true
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
	case *events.StreamGap:
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
func (link *Link) transportStopFor(generation uint64, code string) {
	switch code {
	case "cookie_refused":
		link.signOutIfFor(generation, code, activeState)
	case "persist_failed":
		link.stopFatalFor(generation, code, false)
	}
}

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
func (link *Link) emitFor(generation uint64, kind, code string, value int64) {
	link.mu.Lock()
	current := !link.closed && generation == link.generation
	link.mu.Unlock()
	if current {
		link.emit(kind, code, value)
	}
}

func (link *Link) transitionFor(generation uint64, state, code string) {
	link.mu.Lock()
	if generation != link.generation || link.state != stateLinked || link.unlinking {
		link.mu.Unlock()
		return
	}
	link.state, link.stopCode, link.healthy, link.responding = state, "", false, false
	link.revokeLocked()
	retirementGeneration := link.generation
	link.mu.Unlock()
	if err := link.persistIfGeneration(retirementGeneration); err != nil {
		link.stopFatalFor(retirementGeneration, "persist_failed", false)
		return
	}
	link.emitFor(retirementGeneration, "link", code, 0)
}

func (link *Link) signOutIfFor(generation uint64, code string, allowed func(string) bool) {
	link.mu.Lock()
	if generation != link.generation || !allowed(link.state) || link.unlinking {
		link.mu.Unlock()
		return
	}
	link.state, link.pairingFrom, link.stopCode, link.auth = stateSignedOut, "", code, libgm.NewAuthData()
	link.healthy, link.responding, link.lastOKMS = false, false, 0
	link.revokeLocked()
	retirementGeneration := link.generation
	link.mu.Unlock()
	if err := link.persistIfGeneration(retirementGeneration); err != nil {
		link.stopFatalFor(retirementGeneration, "persist_failed", false)
		return
	}
	link.emitFor(retirementGeneration, "link", "signed_out", 0)
}

func (link *Link) stopFatalFor(generation uint64, code string, wipe bool) {
	link.mu.Lock()
	if generation != link.generation {
		link.mu.Unlock()
		return
	}
	link.stopCode = code
	if wipe {
		link.state, link.pairingFrom, link.auth = stateSignedOut, "", libgm.NewAuthData()
	}
	link.revokeLocked()
	retirementGeneration := link.generation
	link.mu.Unlock()
	link.emitFor(retirementGeneration, "link", code, 0)
}

func (link *Link) transition(state, code string) {
	link.mu.Lock()
	if link.state != stateLinked || link.unlinking {
		link.mu.Unlock()
		return
	}
	link.state, link.stopCode, link.healthy, link.responding = state, "", false, false
	link.revokeLocked()
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
	link.revokeLocked()
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
	link.revokeLocked()
	link.mu.Unlock()
	link.emit("link", code, 0)
}

func (link *Link) recordHealthFor(generation uint64, ok bool, latency int64) {
	link.mu.Lock()
	if generation != link.generation || link.runGeneration != generation {
		link.mu.Unlock()
		return
	}
	link.healthy = ok
	if !ok {
		link.invalidateStreamLocked()
		link.conversationIndex = make(map[string]conversationSummary)
		link.ownerVerification = nil
	} else {
		link.lastOKMS = time.Now().UnixMilli()
	}
	link.mu.Unlock()
	if ok {
		link.emitFor(generation, "health", "ok", latency)
	} else {
		link.emitFor(generation, "health", "failed", 0)
	}
}

// persist writes the envelope. It writes nothing once the link is closed.
func (link *Link) persistIfGeneration(generation uint64) error {
	link.persistMu.Lock()
	defer link.persistMu.Unlock()
	link.mu.Lock()
	if link.closed || generation != link.generation {
		link.mu.Unlock()
		return nil
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

func (link *Link) persistForGeneration(generation uint64) error {
	link.persistMu.Lock()
	defer link.persistMu.Unlock()
	link.mu.Lock()
	if link.closed || generation != link.generation || link.runGeneration != generation {
		link.mu.Unlock()
		return nil
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
