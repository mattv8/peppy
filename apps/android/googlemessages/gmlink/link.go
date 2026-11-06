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
	Event(kind, code string, value, generation, revision int64)
	RcsEvent(recordJSON string, generation, revision, streamEpoch int64)
	TypingEvent(recordJSON string, generation, revision, streamEpoch int64)
	ShowEmoji(emoji string, generation, revision int64)
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

	mu                  sync.Mutex
	rpcMu               sync.Mutex
	persistMu           sync.Mutex
	state               string
	pairingFrom         string
	stopCode            string
	auth                *libgm.AuthData
	seq                 uint64
	client              *libgm.Client
	running             bool
	healthy             bool
	responding          bool
	lastOKMS            int64
	closed              bool
	unlinking           bool
	stopRun             context.CancelFunc
	runCtx              context.Context
	generation          uint64
	runGeneration       uint64
	runExitCode         string
	pairCancel          context.CancelFunc
	pairDone            chan struct{}
	pairOwner           uint64
	unlinkOwner         uint64
	stateRevision       uint64
	pendingCompletion   *terminalCompletion
	completionWG        sync.WaitGroup
	authCaptureMu       sync.Mutex
	authCaptureRevision uint64
	authPersistRevision uint64
	authSnapshot        func(*libgm.AuthData) (*libgm.AuthData, error)
	fetchMessages       func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error)
	setTyping           func(context.Context, string, *gmproto.SIMPayload) error
	typingNow           func() time.Time

	outputMu              sync.Mutex
	outputCond            *sync.Cond
	outputQueue           []hostOutput
	outputClosed          bool
	outputActive          int
	closeReady            bool
	outputStart           func(hostOutput)
	completionOutputReady func()
	pairCleanupReady      func(uint64)

	changes                      chan any
	conversationEvents           []conversationEvent
	conversationCandidates       map[string][]conversationSummary
	conversationTargets          map[string][]string
	conversationRecords          map[string]*gmproto.Conversation
	indexRevision                uint64
	conversationIndexComplete    bool
	conversationIndexGeneration  uint64
	conversationIndexStreamEpoch uint64
	pendingConversationEvents    uint64
	settings                     *gmproto.Settings
	settingsAt                   time.Time
	settingsVersion              uint64
	streamEpoch                  uint64
	streamSource                 libgm.StreamSource
	streamOpen                   bool
	preparedTicket               *preparedRcsTicket
	sendAdmissionInFlight        bool
	beforeSendAdmission          func()
	ticketNow                    func() time.Time
	sendMessage                  func(context.Context, *gmproto.SendMessageRequest) (*gmproto.SendMessageResponse, error)

	ownerVerification *ownerVerification
}

// queuedEvent is fenced twice: before it enters the queue and immediately before
// it mutates Link state. A callback from a retired libgm client is never current.
type queuedEvent struct {
	generation               uint64
	streamEpoch              uint64
	streamSource             libgm.StreamSource
	receivedAt               time.Time
	pendingConversationEvent bool
	event                    any
}

type terminalCompletion struct {
	generation uint64
	revision   uint64
	state      persistedState
	kind       string
	code       string
	done       chan struct{}
}

type hostOutput struct {
	kind              string
	code              string
	value             int64
	emoji             string
	rcsRecord         string
	typingRecord      string
	generation        uint64
	streamEpoch       uint64
	revision          uint64
	requireGeneration bool
	requirePair       bool
	requireRevision   bool
	requireStream     bool
	done              chan struct{}
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
	link := &Link{host: host, state: decoded.State, stopCode: decoded.StopCode, seq: decoded.Seq, changes: make(chan any, 64), conversationCandidates: make(map[string][]conversationSummary), conversationTargets: make(map[string][]string), conversationRecords: make(map[string]*gmproto.Conversation)}
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
	link.outputCond = sync.NewCond(&link.outputMu)
	go link.consumeOutputs()
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

// Close aborts work, stops Run, ends admission, and fences queued Host output. If Host
// reenters Close, the process slot is released when that already-admitted callback returns.
func (link *Link) Close() {
	link.outputMu.Lock()
	link.mu.Lock()
	if !link.closed {
		link.closed = true
		link.stateRevision++
		if link.state == statePairing {
			link.state, link.pairingFrom, link.stopCode = link.pairingFrom, "", "pair_cancelled"
		}
		link.revokeLocked()
		close(link.changes)
	}
	if link.pairCancel != nil {
		link.pairCancel()
	}
	link.stopRunLocked()
	link.mu.Unlock()
	link.outputClosed = true
	for _, output := range link.outputQueue {
		close(output.done)
	}
	link.outputQueue = nil
	if link.outputCond != nil {
		link.outputCond.Broadcast()
	}
	link.outputMu.Unlock()

	// Persistence and immutable terminal work finish independently of Host output.
	link.persistMu.Lock()
	link.persistMu.Unlock()
	link.completionWG.Wait()

	link.outputMu.Lock()
	link.closeReady = true
	link.finalizeCloseLocked()
	link.outputMu.Unlock()
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
	for {
		link.outputMu.Lock()
		link.mu.Lock()
		pending := link.pendingCompletion
		if pending == nil {
			break
		}
		link.mu.Unlock()
		link.outputMu.Unlock()
		<-pending.done
	}
	previous := link.state
	if link.closed {
		link.mu.Unlock()
		link.outputMu.Unlock()
		return errLinkClosed
	}
	if link.unlinking {
		link.mu.Unlock()
		link.outputMu.Unlock()
		return errors.New("unlink_in_progress")
	}
	if (!relink && previous != stateUnlinked && previous != stateSignedOut) || (relink && previous != stateNeedsRelink) {
		link.mu.Unlock()
		link.outputMu.Unlock()
		return errors.New("pair_refused")
	}
	link.stateRevision++
	link.revokeLocked()
	owner := link.generation
	link.state, link.pairingFrom, link.stopCode = statePairing, previous, ""
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	link.pairCancel, link.pairDone, link.pairOwner = cancel, done, owner
	link.mu.Unlock()
	link.outputMu.Unlock()
	completed := false
	defer func() {
		cancel()
		if !completed {
			code := pairCode(err)
			if link.restorePairFor(owner, previous, code) {
				link.emitFor(owner, "link", code, 0)
			}
		}
		if link.pairCleanupReady != nil {
			link.pairCleanupReady(owner)
		}
		link.mu.Lock()
		// Clear only this pair's own handles: a newer pair may already own them.
		if link.pairDone == done && link.pairOwner == owner {
			link.pairCancel, link.pairDone, link.pairOwner = nil, nil, 0
		}
		link.mu.Unlock()
		// Unlink waits on done, so it sees the restored or committed state.
		close(done)
	}()
	if err = link.persistIfGeneration(owner); err != nil {
		return pairFailure("persist_failed")
	}
	persist := func() error { return nil }
	if relink {
		persist = link.persistRotatedFor(owner, auth, done)
	}
	client := link.newClient(auth, persist, owner)
	defer client.Disconnect()
	configCtx, cancelConfig := context.WithTimeout(ctx, configTimeout)
	err = client.FetchConfig(configCtx)
	cancelConfig()
	if err != nil {
		return link.pairResultError(owner, err, relink)
	}
	email := client.Config.GetDeviceInfo().GetEmail()
	if email == "" {
		return pairFailure("pair_failed")
	}
	if relink && !strings.EqualFold(email, storedSource) {
		link.signOutIfFor(owner, "account_mismatch", pairingState)
		return pairFailure("account_mismatch")
	}
	// The pairing long poll lives as long as the pair, so the phone's replies can arrive.
	startCtx, cancelStart := context.WithTimeout(ctx, configTimeout)
	emoji, pairing, err := client.StartGaiaPairing(startCtx, ctx)
	cancelStart()
	if err != nil {
		return link.pairResultError(owner, err, relink)
	}
	link.showEmojiFor(owner, emoji)
	finishCtx, cancelFinish := context.WithTimeout(ctx, pairWaitTimeout)
	_, err = client.FinishGaiaPairing(finishCtx, pairing)
	cancelFinish()
	if err != nil {
		return link.pairResultError(owner, err, relink)
	}
	if !strings.EqualFold(email, auth.Mobile.GetSourceID()) {
		// A fresh pair restores the previous state; a relink signs out, as Run does.
		if relink {
			link.signOutIfFor(owner, "account_mismatch", pairingState)
		}
		return pairFailure("account_mismatch")
	}
	link.mu.Lock()
	if link.state != statePairing || link.generation != owner || link.pairOwner != owner {
		// Unlink or Close superseded this pair while it was finishing; commit nothing.
		link.mu.Unlock()
		return pairFailure("pair_cancelled")
	}
	published, snapshotErr := cloneAuthData(auth)
	if snapshotErr != nil {
		link.mu.Unlock()
		return errPersistFailed
	}
	link.auth, link.state, link.pairingFrom, link.stopCode = published, stateLinked, "", ""
	link.mu.Unlock()
	if err = link.persistIfGeneration(owner); err != nil {
		link.stopFatalFor(owner, "persist_failed", false)
		return errPersistFailed
	}
	completed = true
	if relink {
		link.emitFor(owner, "link", "relink_ok", 0)
	} else {
		link.emitFor(owner, "link", "pair_ok", 0)
	}
	return nil
}

func (link *Link) restorePairFor(owner uint64, previous, code string) bool {
	link.mu.Lock()
	restored := false
	if link.state == statePairing && link.generation == owner && link.pairOwner == owner {
		link.state, link.pairingFrom, link.stopCode = previous, "", code
		restored = true
	}
	link.mu.Unlock()
	if !restored {
		return false
	}
	// A closed link writes nothing and keeps the pair's own reason as its stop code.
	if err := link.persistIfGeneration(owner); err != nil && !errors.Is(err, errLinkClosed) {
		link.stopFatalFor(owner, "persist_failed", false)
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
	link.retirePhysicalSourceLocked()
	generation := link.generation
	client := link.newRuntimeClient(auth, generation)
	link.client, link.running, link.stopRun, link.runCtx, link.runGeneration, link.runExitCode = client, true, cancelRun, runCtx, generation, ""
	link.mu.Unlock()
	defer link.cleanupOwnedRun(client, generation)
	client.SetPingInterval(20 * time.Minute)
	configCtx, cancelConfig := context.WithTimeout(runCtx, configTimeout)
	err = client.FetchConfig(configCtx)
	cancelConfig()
	if err != nil {
		if isFatalAuthError(err) {
			link.signOutIfFor(generation, "signed_out", activeState)
			return errors.New("fatal_refresh")
		}
		if isCookieRefusal(err) {
			// The transport has already signed the link out for the /web/config redirect.
			return errors.New("cookie_refused")
		}
		if runCtx.Err() != nil {
			return link.runErrorFor(generation)
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
		if runCtx.Err() != nil {
			return link.runErrorFor(generation)
		}
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
		return link.runErrorFor(generation)
	}
	link.healthy = true
	link.mu.Unlock()
	link.emitFor(generation, "conn", "ready", 0)
	<-runCtx.Done()
	return link.runErrorFor(generation)
}

func (link *Link) runErrorFor(generation uint64) error {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.runGeneration == generation && link.runExitCode == "event_overflow" {
		return errors.New("event_overflow")
	}
	return nil
}

func (link *Link) cleanupOwnedRun(client *libgm.Client, owner uint64) {
	link.mu.Lock()
	if link.client == client && link.runGeneration == owner {
		if link.generation == owner {
			link.revokeLocked()
		}
		link.client = nil
		link.runCtx = nil
		link.runGeneration = 0
		link.runExitCode = ""
		link.stopRun = nil
		link.healthy = false
		link.running = false
	}
	link.mu.Unlock()
	client.Disconnect()
}

// CheckHealth checks one inbox conversation and returns its latency in milliseconds, or -1.
func (link *Link) CheckHealth() int64 {
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	client, runCtx, generation, _, ok := link.activeRuntime()
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
	client, runCtx, generation, streamEpoch, ok := link.activeRuntime()
	if !ok {
		return errors.New("link_unavailable")
	}
	if !link.hasM2Verification() {
		return errors.New("m2_verification_required")
	}
	indexRevision, started := link.beginConversationIndexRefreshFor(generation, streamEpoch)
	if !started {
		return errors.New("conversation_lookup_stale")
	}
	ctx, cancel := context.WithTimeout(runCtx, conversationTimeout)
	defer cancel()
	conversations, code := scanConversationPages(ctx, func(ctx context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		return client.ListConversations(ctx, request)
	})
	if code != "" {
		return errors.New(code)
	}
	if !link.replaceConversationIndexForContext(ctx, generation, streamEpoch, indexRevision, conversations) {
		return errors.New("conversation_lookup_stale")
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
	client, runCtx, generation, streamEpoch, ok := link.activeRuntime()
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
	if !link.indexConversationsFor(generation, streamEpoch, []*gmproto.Conversation{conversation}) {
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
	defer link.mu.Unlock()
	if !link.hasOwnerVerificationLocked() {
		return conversationJSON("m2_verification_required")
	}
	if !link.conversationIndexComplete || link.conversationIndexGeneration != link.generation || link.conversationIndexStreamEpoch != link.streamEpoch || link.runGeneration != link.generation {
		return conversationJSON("conversation_lookup_unknown")
	}
	for _, conversation := range link.conversationRecords {
		if conversationBlocksCompleteAbsence(conversation) {
			return conversationJSON("conversation_lookup_unknown")
		}
	}
	found := make(map[string]struct{})
	for _, summary := range link.conversationCandidates[target] {
		if summary.self {
			return conversationJSON("conversation_unsupported")
		}
		if summary.eligible {
			found[summary.id] = struct{}{}
		}
	}
	if len(found) > 1 {
		return conversationJSON("conversation_ambiguous")
	}
	for rawTarget := range link.conversationCandidates {
		if rawTarget != target {
			return conversationJSON("conversation_lookup_unknown")
		}
	}
	for id := range found {
		return resolvedConversationJSON(id)
	}
	return conversationJSON("conversation_not_found_complete")
}

// DrainConversationEvents returns local, content-free event correlations.
func (link *Link) DrainConversationEvents() string {
	link.mu.Lock()
	events := append([]conversationEvent(nil), link.conversationEvents...)
	if len(link.conversationEvents) != 1 || link.conversationEvents[0].Code != "conversation_resync" {
		link.conversationEvents = nil
	}
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
func (link *Link) activeRuntime() (*libgm.Client, context.Context, uint64, uint64, bool) {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || link.state != stateLinked || !link.running || link.client == nil || link.runCtx == nil || link.runGeneration != link.generation {
		return nil, nil, 0, 0, false
	}
	return link.client, link.runCtx, link.runGeneration, link.streamEpoch, true
}

// OwnerGeneration returns the currently running generation used to fence effects.
// Gomobile exposes signed 64-bit integers; zero means no active generation.
func (link *Link) OwnerGeneration() int64 {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || !link.running || link.runGeneration == 0 || link.runGeneration != link.generation {
		return 0
	}
	return int64(link.runGeneration)
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
	link.stateRevision++
	link.revokeLocked()
	owner := link.generation
	link.unlinkOwner = owner
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
		link.unlinkOwner = 0
		link.mu.Unlock()
		return errLinkClosed
	}
	auth, snapshotErr := cloneAuthData(link.auth)
	if snapshotErr != nil {
		link.unlinking = false
		link.unlinkOwner = 0
		link.mu.Unlock()
		return errPersistFailed
	}
	paired := auth != nil && auth.PairingID != uuid.Nil
	if link.state == statePairing {
		// A pair that outlived the bound must not commit over the unlink.
		link.state, link.pairingFrom = stateUnlinked, ""
	}
	link.mu.Unlock()
	var err error
	if paired {
		client := link.newClient(auth, func() error { return nil }, owner)
		ctx, cancel := context.WithTimeout(context.Background(), unpairTimeout)
		err = client.UnpairGaia(ctx)
		cancel()
		client.Disconnect()
	}
	link.mu.Lock()
	if link.generation != owner || link.unlinkOwner != owner {
		link.mu.Unlock()
		return errors.New("unlink_cancelled")
	}
	link.state, link.pairingFrom, link.auth = stateUnlinked, "", libgm.NewAuthData()
	link.healthy, link.responding, link.lastOKMS = false, false, 0
	if err != nil {
		link.stopCode = "unlink_remote_failed"
	} else {
		link.stopCode = ""
	}
	link.mu.Unlock()
	if persistErr := link.persistIfGeneration(owner); persistErr != nil {
		link.stopFatalFor(owner, "persist_failed", false)
		link.mu.Lock()
		if link.unlinkOwner == owner {
			link.unlinking = false
			link.unlinkOwner = 0
		}
		link.mu.Unlock()
		return errPersistFailed
	}
	link.mu.Lock()
	if link.closed || link.generation != owner || link.unlinkOwner != owner {
		closed := link.closed
		link.mu.Unlock()
		if closed {
			return errLinkClosed
		}
		return errors.New("unlink_cancelled")
	}
	link.unlinking = false
	link.unlinkOwner = 0
	link.mu.Unlock()
	if err != nil {
		link.emitFor(owner, "link", "unlink_remote_failed", 0)
		return errors.New("unlink_remote_failed")
	}
	link.emitFor(owner, "link", "unlinked", 0)
	return nil
}

// Stop ends Run without changing link state.
func (link *Link) Stop() {
	link.mu.Lock()
	if link.running && link.runGeneration != 0 && link.runGeneration == link.generation {
		link.revokeLocked()
	} else {
		link.stopRunLocked()
	}
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
func (link *Link) invalidateEvidenceLocked() {
	link.streamEpoch++
	link.invalidateSettingsLocked()
	link.invalidateConversationIndexLocked()
}

func (link *Link) retirePhysicalSourceLocked() {
	link.invalidateEvidenceLocked()
	link.streamSource = libgm.StreamSource{}
	link.streamOpen = false
}

func (link *Link) streamSourceMatchesLocked(source libgm.StreamSource) bool {
	return source == (libgm.StreamSource{}) || (link.streamOpen && link.streamSource == source)
}

func (link *Link) revokeLocked() {
	link.generation++
	link.ownerVerification = nil
	link.retirePhysicalSourceLocked()
	link.clearConversationIndexLocked()
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
func (link *Link) newClient(auth *libgm.AuthData, persist func() error, owner uint64) *libgm.Client {
	stop := func(code string) { link.transportStopFor(owner, code) }
	client := libgm.NewClient(auth, nil, zerolog.Nop(), newGuardedSettings(auth, persist, stop))
	client.SetEventHandler(func(event any) { link.queueEventFor(owner, event) })
	return client
}

func (link *Link) newRuntimeClient(auth *libgm.AuthData, generation uint64) *libgm.Client {
	persist := func() error { return link.persistRuntimeAuth(generation, auth) }
	stop := func(code string) { link.transportStopFor(generation, code) }
	client := libgm.NewClient(auth, nil, zerolog.Nop(), newGuardedSettings(auth, persist, stop))
	client.SetEventHandler(func(event any) {
		if _, ok := event.(*events.AuthTokenRefreshed); ok {
			revision, snapshot, err := link.captureRuntimeAuth(auth)
			go func() {
				if err == nil {
					err = link.persistRuntimeAuthSnapshot(generation, revision, snapshot)
				}
				if err != nil {
					link.stopFatalFor(generation, "persist_failed", false)
				}
			}()
			return
		}
		link.queueEventFor(generation, event)
	})
	return client
}

func cloneAuthData(auth *libgm.AuthData) (*libgm.AuthData, error) {
	if auth == nil {
		return libgm.NewAuthData(), nil
	}
	return auth.Snapshot()
}

func (link *Link) captureRuntimeAuth(candidate *libgm.AuthData) (uint64, *libgm.AuthData, error) {
	link.authCaptureMu.Lock()
	defer link.authCaptureMu.Unlock()
	link.authCaptureRevision++
	revision := link.authCaptureRevision
	snapshot := link.authSnapshot
	if snapshot == nil {
		snapshot = cloneAuthData
	}
	captured, err := snapshot(candidate)
	return revision, captured, err
}

func (link *Link) persistRuntimeAuth(generation uint64, candidate *libgm.AuthData) error {
	revision, snapshot, err := link.captureRuntimeAuth(candidate)
	if err != nil {
		return errPersistFailed
	}
	return link.persistRuntimeAuthSnapshot(generation, revision, snapshot)
}

func (link *Link) persistRuntimeAuthSnapshot(generation, revision uint64, snapshot *libgm.AuthData) error {
	link.persistMu.Lock()
	defer link.persistMu.Unlock()
	link.mu.Lock()
	if link.closed || generation != link.generation || link.runGeneration != generation || revision <= link.authPersistRevision {
		link.mu.Unlock()
		return nil
	}
	link.auth = snapshot
	link.authPersistRevision = revision
	link.seq++
	auth, authErr := json.Marshal(link.auth)
	state := persistedState{Version: stateVersion, Seq: link.seq, State: link.state, PairingFrom: link.pairingFrom, StopCode: link.stopCode, Auth: auth}
	encoded, stateErr := json.Marshal(state)
	link.mu.Unlock()
	if authErr != nil || stateErr != nil {
		return errPersistFailed
	}
	if err := link.host.PersistState(encoded); err != nil {
		return errPersistFailed
	}
	return nil
}

// persistRotated persists a relink's rotated cookies. The stored auth takes them, so a
// relink that fails keeps the rotations. Only the pair that created the client, while it
// still owns the link, takes them: a rotation from an earlier pair, or one that lands
// after sign-out or unlink, writes nothing.
func (link *Link) persistRotatedFor(operation uint64, candidate *libgm.AuthData, owner chan struct{}) func() error {
	return func() error {
		candidate.CookiesLock.RLock()
		cookies := maps.Clone(candidate.Cookies)
		candidate.CookiesLock.RUnlock()
		link.mu.Lock()
		if link.state != statePairing || link.pairDone != owner || link.pairOwner != operation || link.generation != operation {
			link.mu.Unlock()
			return nil
		}
		link.auth.SetCookies(cookies)
		link.mu.Unlock()
		return link.persistIfGeneration(operation)
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
	var source libgm.StreamSource
	switch value := event.(type) {
	case *libgm.StreamOpened:
		if value == nil || value.Source == (libgm.StreamSource{}) {
			return
		}
		link.mu.Lock()
		if link.closed || generation != link.generation {
			link.mu.Unlock()
			return
		}
		if link.streamOpen {
			link.retirePhysicalSourceLocked()
		}
		link.streamSource = value.Source
		link.streamOpen = true
		link.ownerVerification = nil
		link.clearConversationIndexLocked()
		link.mu.Unlock()
		return
	case *libgm.StreamRetired:
		if value == nil {
			return
		}
		source = value.Source
	case *libgm.ContinuityLost:
		if value == nil {
			return
		}
		source = value.Source
	case *libgm.StreamEvent:
		if value == nil {
			return
		}
		source, event = value.Source, value.Event
	}
	if !relevantEvent(event) {
		return
	}
	event = cloneQueuedEvent(event)
	receivedAt := time.Now()
	link.mu.Lock()
	if link.closed || generation != link.generation || !link.streamSourceMatchesLocked(source) {
		link.mu.Unlock()
		return
	}
	switch event.(type) {
	case *libgm.StreamRetired, *libgm.ContinuityLost:
		link.retirePhysicalSourceLocked()
		link.ownerVerification = nil
	default:
		if invalidatesAtIngress(event) {
			link.invalidateEvidenceLocked()
		}
		if settings, ok := event.(*libgm.WrappedSettings); ok && settings != nil && !settings.IsOld && settings.Settings != nil && settings.Settings.GetSIMCards() != nil {
			link.invalidateEvidenceLocked()
			link.settingsVersion++
		}
	}
	var completion *terminalCompletion
	if terminalAtIngress(event) {
		link.invalidateEvidenceLocked()
		link.healthy, link.responding, link.lastOKMS = false, false, 0
		completion = link.commitTerminalLocked(generation, event)
	}
	if _, ok := event.(*events.PhoneNotResponding); ok {
		link.invalidateEvidenceLocked()
		link.healthy, link.responding, link.lastOKMS = false, false, 0
	}
	if invalidatesOwnerEvidence(event) {
		link.ownerVerification = nil
	}
	if completion != nil {
		link.mu.Unlock()
		go link.completeTerminal(completion)
		return
	}
	pendingConversationEvent := false
	if conversation, ok := event.(*gmproto.Conversation); ok && conversation != nil && conversation.GetConversationID() != "" {
		link.invalidateConversationCompletenessLocked()
		link.indexRevision++
		link.pendingConversationEvents++
		pendingConversationEvent = true
	}
	queued := queuedEvent{
		generation:               generation,
		streamEpoch:              link.streamEpoch,
		streamSource:             source,
		receivedAt:               receivedAt,
		pendingConversationEvent: pendingConversationEvent,
		event:                    event,
	}
	select {
	case link.changes <- queued:
		link.mu.Unlock()
	default:
		link.retireFatalLocked(generation, "event_overflow", false)
		link.mu.Unlock()
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
	case *gmproto.Conversation:
		cloned, ok := proto.Clone(settings).(*gmproto.Conversation)
		if ok {
			return cloned
		}
	case *libgm.WrappedMessage:
		if settings == nil || settings.Message == nil {
			return event
		}
		cloned, ok := proto.Clone(settings.Message).(*gmproto.Message)
		if ok {
			return &libgm.WrappedMessage{Message: cloned, IsOld: settings.IsOld}
		}
	case *gmproto.TypingData:
		cloned, ok := proto.Clone(settings).(*gmproto.TypingData)
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
		*gmproto.Conversation, *libgm.WrappedMessage, *gmproto.TypingData, *libgm.WrappedSettings, *gmproto.Settings, *events.StreamGap,
		*libgm.StreamRetired, *libgm.ContinuityLost:
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
	case *events.GaiaLoggedOut, *events.ListenFatalError:
		return true
	case *events.AccountChange:
		return !value.IsFake
	default:
		return false
	}
}

func invalidatesOwnerEvidence(event any) bool {
	if _, ok := event.(*events.PhoneNotResponding); ok {
		return true
	}
	return invalidatesAtIngress(event) || terminalAtIngress(event)
}

func (link *Link) commitTerminalLocked(owner uint64, event any) *terminalCompletion {
	if owner != link.generation || link.state != stateLinked || link.unlinking {
		return nil
	}
	state, code := "", ""
	switch value := event.(type) {
	case *events.GaiaLoggedOut:
		state, code = stateNeedsRelink, "relink_needed"
	case *events.ListenFatalError:
		if isFatalAuthError(value.Error) {
			state, code = stateSignedOut, "signed_out"
		} else {
			state, code = stateNeedsRelink, "relink_needed"
		}
	case *events.AccountChange:
		if value.IsFake {
			return nil
		}
		state, code = stateSignedOut, "signed_out"
	default:
		return nil
	}
	link.stateRevision++
	link.state, link.pairingFrom, link.stopCode = state, "", code
	link.healthy, link.responding, link.lastOKMS = false, false, 0
	if state == stateSignedOut {
		link.auth = libgm.NewAuthData()
	}
	link.revokeLocked()
	auth, err := json.Marshal(link.auth)
	if err != nil {
		return nil
	}
	completion := &terminalCompletion{
		generation: link.generation,
		revision:   link.stateRevision,
		state: persistedState{
			Version:     stateVersion,
			State:       state,
			PairingFrom: "",
			StopCode:    code,
			Auth:        auth,
		},
		kind: "link",
		code: code,
		done: make(chan struct{}),
	}
	link.pendingCompletion = completion
	link.completionWG.Add(1)
	return completion
}

func (link *Link) completeTerminal(completion *terminalCompletion) {
	link.persistMu.Lock()
	link.mu.Lock()
	current := !link.closed && link.stateRevision == completion.revision && link.pendingCompletion == completion
	var encoded []byte
	var err error
	if current {
		link.seq++
		state := completion.state
		state.Seq = link.seq
		encoded, err = json.Marshal(state)
	}
	link.mu.Unlock()
	if current && err == nil {
		err = link.host.PersistState(encoded)
	}
	link.persistMu.Unlock()

	link.mu.Lock()
	if link.pendingCompletion == completion {
		link.pendingCompletion = nil
	}
	link.mu.Unlock()
	close(completion.done)
	link.completionWG.Done()
	if current && err == nil && link.completionOutputReady != nil {
		link.completionOutputReady()
	}
	if current && err == nil {
		link.enqueueOutput(hostOutput{kind: completion.kind, code: completion.code, generation: completion.generation, revision: completion.revision, requireRevision: true})
	}
}

func (link *Link) projectQueuedEvent(queued queuedEvent) {
	switch value := queued.event.(type) {
	case *libgm.WrappedSettings:
		link.recordSettingsFor(queued.generation, queued.streamSource, queued.streamEpoch, queued.receivedAt, value)
	case *gmproto.Conversation:
		link.recordQueuedConversationEventFor(queued.generation, queued.streamSource, queued.streamEpoch, value, queued.pendingConversationEvent)
	case *libgm.WrappedMessage:
		link.projectRcsEventFor(queued, value)
	case *gmproto.TypingData:
		link.projectTypingEventFor(queued, value)
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
	case *events.ListenTemporaryError, *events.StreamGap, *libgm.StreamRetired, *libgm.ContinuityLost:
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

func (link *Link) invalidateDirectEvidence(clearOwner bool) {
	link.mu.Lock()
	link.invalidateEvidenceLocked()
	if clearOwner {
		link.ownerVerification = nil
	}
	link.mu.Unlock()
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
		link.invalidateDirectEvidence(true)
		link.emit("conn", "temporary_error", 0)
	case *events.StreamGap:
		link.invalidateDirectEvidence(true)
		link.emit("conn", "temporary_error", 0)
	case *events.ListenRecovered:
		link.emit("conn", "recovered", 0)
	case *events.PhoneNotResponding:
		link.mu.Lock()
		link.responding = false
		link.invalidateEvidenceLocked()
		link.ownerVerification = nil
		link.mu.Unlock()
		link.emit("conn", "phone_not_responding", 0)
	case *events.PhoneRespondingAgain:
		link.mu.Lock()
		link.responding = true
		link.mu.Unlock()
		link.emit("conn", "phone_responding", 0)
	case *events.NoDataReceived:
		link.invalidateDirectEvidence(true)
		link.emit("conn", "no_data", 0)
	case *events.ActiveSessionSet:
		link.invalidateDirectEvidence(true)
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
		link.invalidateDirectEvidence(value == nil || value.IsOld || value.Settings == nil || value.Settings.GetSIMCards() == nil)
		link.recordSettings(value)
	case *gmproto.Settings:
		link.invalidateDirectEvidence(true)
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
	if !isEventCode(kind, code) {
		return
	}
	link.enqueueOutput(hostOutput{kind: kind, code: code, value: value, generation: generation, revision: link.outputRevision(), requireGeneration: true})
}

func (link *Link) showEmojiFor(owner uint64, emoji string) {
	link.enqueueOutput(hostOutput{emoji: emoji, generation: owner, revision: link.outputRevision(), requirePair: true})
}

func (link *Link) outputRevision() uint64 {
	link.mu.Lock()
	defer link.mu.Unlock()
	return link.stateRevision
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
	link.retireFatalFor(generation, code, wipe)
}

func (link *Link) retireFatalFor(generation uint64, code string, wipe bool) (uint64, bool) {
	link.mu.Lock()
	retirementGeneration, retired := link.retireFatalLocked(generation, code, wipe)
	link.mu.Unlock()
	if retired {
		link.emitFor(retirementGeneration, "link", code, 0)
	}
	return retirementGeneration, retired
}

func (link *Link) retireFatalLocked(generation uint64, code string, wipe bool) (uint64, bool) {
	if generation != link.generation {
		return 0, false
	}
	link.stopCode = code
	if code == "event_overflow" && link.runGeneration == generation {
		link.runExitCode = code
	}
	if wipe {
		link.state, link.pairingFrom, link.auth = stateSignedOut, "", libgm.NewAuthData()
	}
	link.revokeLocked()
	retirementGeneration := link.generation
	return retirementGeneration, true
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
		link.invalidateEvidenceLocked()
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

func (link *Link) enqueueOutput(output hostOutput) {
	output.done = make(chan struct{})
	link.outputMu.Lock()
	if link.outputCond == nil {
		link.outputMu.Unlock()
		link.deliverUnmanagedOutput(output)
		return
	}
	if link.outputClosed {
		link.outputMu.Unlock()
		return
	}
	link.outputQueue = append(link.outputQueue, output)
	wait := link.outputActive == 0
	link.outputCond.Signal()
	link.outputMu.Unlock()
	if wait {
		<-output.done
	}
}

func (link *Link) consumeOutputs() {
	for {
		link.outputMu.Lock()
		for len(link.outputQueue) == 0 && !link.outputClosed {
			link.outputCond.Wait()
		}
		if len(link.outputQueue) == 0 && link.outputClosed {
			link.finalizeCloseLocked()
			link.outputMu.Unlock()
			return
		}
		output := link.outputQueue[0]
		link.outputQueue = link.outputQueue[1:]
		if !link.outputAdmitted(output) {
			close(output.done)
			link.outputMu.Unlock()
			continue
		}
		link.outputActive++
		link.outputMu.Unlock()
		link.deliverOutput(output)
	}
}

func (link *Link) outputAdmitted(output hostOutput) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	admitted := !link.closed
	if output.requireGeneration {
		admitted = admitted && link.generation == output.generation
	}
	if output.requirePair {
		admitted = admitted && link.generation == output.generation && link.pairOwner == output.generation
	}
	if output.requireRevision {
		admitted = admitted && link.stateRevision == output.revision
	}
	if output.requireStream {
		admitted = admitted && link.streamEpoch == output.streamEpoch && link.hasOwnerVerificationLocked()
	}
	return admitted
}

func (link *Link) deliverUnmanagedOutput(output hostOutput) {
	if link.outputAdmitted(output) {
		link.callHost(output)
	}
}

func (link *Link) deliverOutput(output hostOutput) {
	if link.outputStart != nil {
		link.outputStart(output)
	}
	if link.outputAdmitted(output) {
		link.callHost(output)
	}
	close(output.done)

	link.outputMu.Lock()
	link.outputActive--
	link.finalizeCloseLocked()
	link.outputMu.Unlock()
}

func (link *Link) callHost(output hostOutput) {
	switch {
	case output.emoji != "":
		link.host.ShowEmoji(output.emoji, int64(output.generation), int64(output.revision))
	case output.rcsRecord != "":
		link.host.RcsEvent(output.rcsRecord, int64(output.generation), int64(output.revision), int64(output.streamEpoch))
	case output.typingRecord != "":
		link.host.TypingEvent(output.typingRecord, int64(output.generation), int64(output.revision), int64(output.streamEpoch))
	default:
		link.host.Event(output.kind, output.code, output.value, int64(output.generation), int64(output.revision))
	}
}

// AcceptsOutput lets an asynchronous platform receiver validate the originating operation.
func (link *Link) AcceptsOutput(generation, revision int64) bool {
	if generation < 0 || revision < 0 {
		return false
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	return !link.closed && link.generation == uint64(generation) && link.stateRevision == uint64(revision)
}

// AcceptsStreamOutput additionally validates the stream evidence that produced a receive callback.
func (link *Link) AcceptsStreamOutput(generation, revision, streamEpoch int64) bool {
	if generation < 0 || revision < 0 || streamEpoch < 0 {
		return false
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	return !link.closed && link.generation == uint64(generation) && link.stateRevision == uint64(revision) && link.streamEpoch == uint64(streamEpoch) && link.hasOwnerVerificationLocked()
}

func (link *Link) finalizeCloseLocked() {
	if link.outputClosed && link.closeReady && link.outputActive == 0 {
		openLink.CompareAndSwap(link, nil)
	}
}

// emit reports a listed event to the host. Nothing reaches the host after Close.
func (link *Link) emit(kind, code string, value int64) {
	if !isEventCode(kind, code) {
		return
	}
	link.mu.Lock()
	generation, revision := link.generation, link.stateRevision
	link.mu.Unlock()
	link.enqueueOutput(hostOutput{kind: kind, code: code, value: value, generation: generation, revision: revision})
}

// pairFailure is a fixed pair outcome. Its text is the reason code the host sees.
type pairFailure string

func (code pairFailure) Error() string { return string(code) }

// pairResultError maps a failed pair step to its fixed code. A relink the phone refuses
// for its cookies signs out, because the stored cookies are dead.
func (link *Link) pairResultError(owner uint64, err error, relink bool) error {
	if relink && isCookieRefusal(err) {
		link.signOutIfFor(owner, "cookie_refused", pairingState)
	} else if relink && isFatalAuthError(err) {
		link.signOutIfFor(owner, "signed_out", pairingState)
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
