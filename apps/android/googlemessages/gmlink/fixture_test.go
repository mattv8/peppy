package gmlink

import (
	"bytes"
	"crypto/ecdh"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"go.mau.fi/util/exhttp"
	"go.mau.fi/util/pblite"
	"google.golang.org/protobuf/proto"
)

const (
	fixtureConfigPath    = "/web/config"
	fixtureSignInSuffix  = ".Registration/SignInGaia"
	fixtureRefreshSuffix = ".Registration/RegisterRefresh"
	fixtureSendSuffix    = ".Messaging/SendMessage"
	fixturePollSuffix    = ".Messaging/ReceiveMessages"
	fixtureAckSuffix     = ".Messaging/AckMessages"
	fixtureSourceID      = "synthetic-source"
	fixtureClientVersion = "20261104"
	fixtureLoginURL      = "https://accounts.google.com/ServiceLogin"

	// Canaries are synthetic markers. Any of them reaching a status, event or
	// returned error fails the privacy tests.
	canaryCookie  = "canary-cookie-0001"
	canaryAccount = "canary-account-0001"
	canaryNumber  = "canary-number-0001"
)

func setTestOwnerVerification(link *Link) {
	link.mu.Lock()
	defer link.mu.Unlock()
	link.ownerVerification = &ownerVerification{generation: link.generation}
}

// fixtureKnobs select how the fake Google server answers. Tests change them
// through phoneFixture.configure, which holds the fixture lock.
type fixtureKnobs struct {
	configEmail    string
	configStatus   int
	configRedirect bool
	gateConfig     bool
	rotateCookie   bool
	hangConfig     bool
	hangSignIn     bool
	hangRefresh    bool
	gateRefresh    bool
	gaiaInit       bool
	gaiaFinish     bool
	failAction     gmproto.ActionType
	failStatus     int
	hangSend       bool
	hangAction     gmproto.ActionType
	gateSend       bool
	gateAction     gmproto.ActionType
	gateList       bool
}

// phoneFixture stands in for Google's servers on localhost. It answers config,
// sign-in, sends and the long poll, and records every request the link makes.
type phoneFixture struct {
	server      *httptest.Server
	auth        *libgm.AuthData
	rotations   atomic.Int32
	current     chan []byte
	pending     [][]byte
	stop        chan struct{}
	gate        chan struct{}
	gateOnce    sync.Once
	listOnce    sync.Once
	streamOnce  sync.Once
	listReached chan struct{}
	listRelease chan struct{}
	streamReady chan struct{}

	mu            sync.Mutex
	knobs         fixtureKnobs
	actions       []string
	signIns       int
	configs       int
	polls         int
	refreshes     int
	listCalls     []*gmproto.ListConversationsRequest
	conversations []*gmproto.Conversation
	sendRequests  []*gmproto.SendMessageRequest
	unexpected    []string
}

// newPhoneFixture serves on localhost and routes the production dialer to it.
// auth is the link's auth: the fixture decrypts its requests and encrypts replies.
func newPhoneFixture(t *testing.T, auth *libgm.AuthData) *phoneFixture {
	t.Helper()
	fixture := &phoneFixture{auth: auth, stop: make(chan struct{}), gate: make(chan struct{}), listReached: make(chan struct{}), listRelease: make(chan struct{}), streamReady: make(chan struct{})}
	fixture.server = httptest.NewTLSServer(http.HandlerFunc(fixture.serve))
	installSafetyDialer(t, fixture.server)
	t.Cleanup(fixture.server.Close)
	t.Cleanup(func() { close(fixture.stop) })
	t.Cleanup(func() {
		fixture.mu.Lock()
		unexpected := append([]string(nil), fixture.unexpected...)
		fixture.mu.Unlock()
		if len(unexpected) > 0 {
			t.Errorf("unexpected requests reached the fixture: %v", unexpected)
		}
	})
	return fixture
}

// installSafetyDialer points the production client settings at the local server
// for the test duration. newGuardedSettings reads these settings when a client is built.
func installSafetyDialer(t *testing.T, server *httptest.Server) {
	t.Helper()
	previous := exhttp.SensibleClientSettings
	exhttp.SensibleClientSettings = safetyDialSettings(server)
	t.Cleanup(func() { exhttp.SensibleClientSettings = previous })
}

// openGate releases requests held by gateSend.
func (fixture *phoneFixture) openGate() {
	fixture.gateOnce.Do(func() { close(fixture.gate) })
}

func (fixture *phoneFixture) configure(change func(knobs *fixtureKnobs)) {
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	change(&fixture.knobs)
}

func (fixture *phoneFixture) snapshot() fixtureKnobs {
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	return fixture.knobs
}

// countAction returns how many SendMessage requests named the action.
func (fixture *phoneFixture) countAction(name string) int {
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	count := 0
	for _, action := range fixture.actions {
		if action == name {
			count++
		}
	}
	return count
}

func (fixture *phoneFixture) allActions() []string {
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	return append([]string(nil), fixture.actions...)
}

func (fixture *phoneFixture) counts() (configs, signIns, polls int) {
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	return fixture.configs, fixture.signIns, fixture.polls
}

func (fixture *phoneFixture) listRequests() []*gmproto.ListConversationsRequest {
	fixture.mu.Lock()
	defer fixture.mu.Unlock()
	return append([]*gmproto.ListConversationsRequest(nil), fixture.listCalls...)
}

func (fixture *phoneFixture) serve(writer http.ResponseWriter, request *http.Request) {
	knobs := fixture.snapshot()
	path := request.URL.Path
	switch {
	case request.Method == http.MethodGet && path == fixtureConfigPath:
		fixture.mu.Lock()
		fixture.configs++
		fixture.mu.Unlock()
		if knobs.hangConfig {
			fixture.hold(request)
			return
		}
		if knobs.gateConfig {
			select {
			case <-fixture.gate:
			case <-fixture.stop:
				return
			}
		}
		if knobs.configRedirect {
			writer.Header().Set("Location", fixtureLoginURL)
			writer.WriteHeader(http.StatusFound)
			return
		}
		if knobs.configStatus != 0 {
			writer.WriteHeader(knobs.configStatus)
			return
		}
		if knobs.rotateCookie {
			writer.Header().Add("Set-Cookie", fmt.Sprintf("SAPISID=rotated-%d; Path=/; Secure", fixture.rotations.Add(1)))
		}
		body, err := pblite.Marshal(&gmproto.Config{ClientVersion: fixtureClientVersion, DeviceInfo: &gmproto.Config_DeviceInfo{Email: knobs.configEmail}})
		if err != nil {
			writer.WriteHeader(http.StatusInternalServerError)
			return
		}
		writeFixtureBody(writer, body)
	case strings.HasSuffix(path, fixtureSignInSuffix):
		fixture.mu.Lock()
		fixture.signIns++
		fixture.mu.Unlock()
		if knobs.hangSignIn {
			fixture.hold(request)
			return
		}
		body, err := pblite.Marshal(gaiaSignInResponse([]string{primaryPhoneOne}))
		if err != nil {
			writer.WriteHeader(http.StatusInternalServerError)
			return
		}
		writeFixtureBody(writer, body)
	case strings.HasSuffix(path, fixtureRefreshSuffix):
		// The first refresh hangs when asked; later ones are refused as fatal so the
		// long poll's retry loop ends instead of outliving the test.
		fixture.mu.Lock()
		fixture.refreshes++
		first := fixture.refreshes == 1
		fixture.mu.Unlock()
		if knobs.hangRefresh && first {
			fixture.hold(request)
			return
		}
		if knobs.gateRefresh && first {
			// The first refresh waits for openGate, then fails transiently so Connect carries on.
			select {
			case <-fixture.gate:
			case <-fixture.stop:
				return
			}
			writer.WriteHeader(http.StatusInternalServerError)
			return
		}
		writer.WriteHeader(http.StatusUnauthorized)
	case strings.HasSuffix(path, fixtureSendSuffix):
		fixture.handleSend(writer, request, knobs)
	case strings.HasSuffix(path, fixturePollSuffix):
		fixture.mu.Lock()
		fixture.polls++
		fixture.mu.Unlock()
		fixture.streamLongPoll(writer, request)
	case strings.HasSuffix(path, fixtureAckSuffix):
		writeEmptyFixturePBLite(writer)
	default:
		fixture.mu.Lock()
		fixture.unexpected = append(fixture.unexpected, path)
		fixture.mu.Unlock()
		writer.WriteHeader(http.StatusNotFound)
	}
}

// hold keeps a request unanswered until the client gives up or the test ends.
// The body is drained first: Go's server only notices a client disconnect after
// the body is consumed, and server.Close would otherwise wait forever.
func (fixture *phoneFixture) hold(request *http.Request) {
	_, _ = io.Copy(io.Discard, request.Body)
	select {
	case <-request.Context().Done():
	case <-fixture.stop:
	}
}

func writeFixtureBody(writer http.ResponseWriter, body []byte) {
	writer.Header().Set("Content-Type", libgm.ContentTypePBLite)
	_, _ = writer.Write(body)
}

func writeEmptyFixturePBLite(writer http.ResponseWriter) {
	body, _ := pblite.Marshal(&gmproto.OutgoingRPCResponse{})
	writeFixtureBody(writer, body)
}

func (fixture *phoneFixture) handleSend(writer http.ResponseWriter, request *http.Request, knobs fixtureKnobs) {
	body, _ := io.ReadAll(request.Body)
	var outer gmproto.OutgoingRPCMessage
	if err := pblite.Unmarshal(body, &outer); err != nil {
		writer.WriteHeader(http.StatusBadRequest)
		return
	}
	var data gmproto.OutgoingRPCData
	if err := proto.Unmarshal(outer.GetData().GetMessageData(), &data); err != nil {
		writer.WriteHeader(http.StatusBadRequest)
		return
	}
	action := data.GetAction()
	fixture.mu.Lock()
	fixture.actions = append(fixture.actions, action.String())
	fixture.mu.Unlock()
	if knobs.failStatus != 0 && action == knobs.failAction {
		writer.WriteHeader(knobs.failStatus)
		return
	}
	if knobs.hangSend && action == knobs.hangAction {
		fixture.hold(request)
		return
	}
	if knobs.gateSend && action == knobs.gateAction {
		// The request waits until the test opens the gate, then is answered normally.
		select {
		case <-fixture.gate:
		case <-request.Context().Done():
			return
		case <-fixture.stop:
			return
		}
	}
	writeEmptyFixturePBLite(writer)
	switch action {
	case gmproto.ActionType_LIST_CONVERSATIONS:
		fixture.answerListConversations(&data)
	case gmproto.ActionType_GET_CONVERSATION:
		fixture.answerGetConversation(&data)
	case gmproto.ActionType_SEND_MESSAGE:
		fixture.answerSendMessage(&data)
	case gmproto.ActionType_CREATE_GAIA_PAIRING_CLIENT_INIT:
		if knobs.gaiaInit {
			fixture.deliverGaiaInit(data.GetRequestID())
		}
	case gmproto.ActionType_CREATE_GAIA_PAIRING_CLIENT_FINISHED:
		if knobs.gaiaFinish {
			// A successful finish: no error type, so the link derives its keys.
			fixture.queue(&gmproto.RPCMessageData{Action: gmproto.ActionType_CREATE_GAIA_PAIRING_CLIENT_FINISHED, SessionID: data.GetRequestID(), UnencryptedData: mustMarshal(&gmproto.GaiaPairingResponseContainer{})})
		}
	}
}

func (fixture *phoneFixture) answerListConversations(data *gmproto.OutgoingRPCData) {
	knobs := fixture.snapshot()
	plain, err := fixture.auth.RequestCrypto.Decrypt(data.GetEncryptedProtoData())
	if err != nil {
		panic(err)
	}
	var request gmproto.ListConversationsRequest
	if err := proto.Unmarshal(plain, &request); err != nil {
		panic(err)
	}
	fixture.mu.Lock()
	fixture.listCalls = append(fixture.listCalls, &request)
	fixture.mu.Unlock()
	if knobs.gateList && request.GetFolder() == gmproto.ListConversationsRequest_INBOX {
		fixture.listOnce.Do(func() { close(fixture.listReached) })
		select {
		case <-fixture.listRelease:
		case <-fixture.stop:
			return
		}
	}
	fixture.mu.Lock()
	conversations := append([]*gmproto.Conversation(nil), fixture.conversations...)
	fixture.mu.Unlock()
	encrypted, err := fixture.auth.RequestCrypto.Encrypt(mustMarshal(&gmproto.ListConversationsResponse{Conversations: conversations}))
	if err != nil {
		panic(err)
	}
	fixture.queue(&gmproto.RPCMessageData{Action: gmproto.ActionType_LIST_CONVERSATIONS, SessionID: data.GetRequestID(), EncryptedData: encrypted})
}

func (fixture *phoneFixture) answerGetConversation(data *gmproto.OutgoingRPCData) {
	plain, err := fixture.auth.RequestCrypto.Decrypt(data.GetEncryptedProtoData())
	if err != nil {
		panic(err)
	}
	var request gmproto.GetConversationRequest
	if err := proto.Unmarshal(plain, &request); err != nil {
		panic(err)
	}
	fixture.mu.Lock()
	var conversation *gmproto.Conversation
	for _, candidate := range fixture.conversations {
		if candidate.GetConversationID() == request.GetConversationID() {
			conversation = candidate
			break
		}
	}
	fixture.mu.Unlock()
	encrypted, err := fixture.auth.RequestCrypto.Encrypt(mustMarshal(&gmproto.GetConversationResponse{Conversation: conversation}))
	if err != nil {
		panic(err)
	}
	fixture.queue(&gmproto.RPCMessageData{Action: gmproto.ActionType_GET_CONVERSATION, SessionID: data.GetRequestID(), EncryptedData: encrypted})
}

func (fixture *phoneFixture) answerSendMessage(data *gmproto.OutgoingRPCData) {
	plain, err := fixture.auth.RequestCrypto.Decrypt(data.GetEncryptedProtoData())
	if err != nil {
		panic(err)
	}
	var request gmproto.SendMessageRequest
	if err := proto.Unmarshal(plain, &request); err != nil {
		panic(err)
	}
	fixture.mu.Lock()
	fixture.sendRequests = append(fixture.sendRequests, &request)
	fixture.mu.Unlock()
	encrypted, err := fixture.auth.RequestCrypto.Encrypt(mustMarshal(&gmproto.SendMessageResponse{Status: gmproto.SendMessageResponse_SUCCESS}))
	if err != nil {
		panic(err)
	}
	fixture.queue(&gmproto.RPCMessageData{Action: gmproto.ActionType_SEND_MESSAGE, SessionID: data.GetRequestID(), EncryptedData: encrypted})
}

// deliverGaiaInit answers CREATE_GAIA_PAIRING_CLIENT_INIT with a valid P-256
// server init, so the link can derive its emoji and reach the emoji wait.
func (fixture *phoneFixture) deliverGaiaInit(requestID string) {
	key, err := ecdh.P256().GenerateKey(rand.Reader)
	if err != nil {
		panic(err)
	}
	random := make([]byte, 32)
	if _, err := rand.Read(random); err != nil {
		panic(err)
	}
	public := key.PublicKey().Bytes()
	serverInit := &gmproto.Ukey2ServerInit{
		Version:         1,
		Random:          random,
		HandshakeCipher: gmproto.Ukey2HandshakeCipher_P256_SHA512,
		PublicKey: &gmproto.GenericPublicKey{
			Type: gmproto.PublicKeyType_EC_P256,
			PublicKey: &gmproto.GenericPublicKey_EcP256PublicKey{
				EcP256PublicKey: &gmproto.EcP256PublicKey{X: public[1:33], Y: public[33:65]},
			},
		},
	}
	ukey := &gmproto.Ukey2Message{MessageType: gmproto.Ukey2Message_SERVER_INIT, MessageData: mustMarshal(serverInit)}
	container := &gmproto.GaiaPairingResponseContainer{
		Data:                             mustMarshal(ukey),
		ConfirmedKeyDerivationVersion:    1,
		ConfirmedVerificationCodeVersion: 1,
	}
	fixture.queue(&gmproto.RPCMessageData{Action: gmproto.ActionType_CREATE_GAIA_PAIRING_CLIENT_INIT, SessionID: requestID, UnencryptedData: mustMarshal(container)})
}

// queue delivers one DataEvent to the most recently connected long-poll stream. Only
// the newest stream receives replies: an older stream belongs to a finished client.
// Replies sent before any stream connects wait for the next one.
func (fixture *phoneFixture) queue(data *gmproto.RPCMessageData) {
	messageData := mustMarshal(data)
	incoming := &gmproto.IncomingRPCMessage{ResponseID: uuid.NewString(), BugleRoute: gmproto.BugleRoute_DataEvent, MessageData: messageData}
	block, err := pblite.Marshal(&gmproto.LongPollingPayload{Data: incoming})
	if err != nil {
		panic(err)
	}
	fixture.queueBlock(block)
}

func (fixture *phoneFixture) queueSettings(settings *gmproto.Settings) {
	encrypted, err := fixture.auth.RequestCrypto.Encrypt(mustMarshal(&gmproto.UpdateEvents{Event: &gmproto.UpdateEvents_SettingsEvent{SettingsEvent: settings}}))
	if err != nil {
		panic(err)
	}
	fixture.queue(&gmproto.RPCMessageData{Action: gmproto.ActionType_GET_UPDATES, EncryptedData: encrypted})
}

func (fixture *phoneFixture) queueOldEvent() {
	count := int32(1)
	block, err := pblite.Marshal(&gmproto.LongPollingPayload{Ack: &gmproto.StartAckMessage{Count: &count}})
	if err != nil {
		panic(err)
	}
	fixture.queueBlock(block)
}

func (fixture *phoneFixture) queueBlock(block []byte) {
	fixture.mu.Lock()
	current := fixture.current
	if current == nil {
		fixture.pending = append(fixture.pending, block)
		fixture.mu.Unlock()
		return
	}
	fixture.mu.Unlock()
	// The lock is released before the send, so a stream that exits meanwhile cannot deadlock on it.
	select {
	case current <- block:
	case <-fixture.stop:
	}
}

func (fixture *phoneFixture) streamLongPoll(writer http.ResponseWriter, request *http.Request) {
	flusher, ok := writer.(http.Flusher)
	if !ok {
		return
	}
	// Drain the body so the server notices when this client disconnects.
	_, _ = io.Copy(io.Discard, request.Body)
	blocks := make(chan []byte, 16)
	fixture.mu.Lock()
	fixture.current = blocks
	for _, block := range fixture.pending {
		blocks <- block
	}
	fixture.pending = nil
	fixture.mu.Unlock()
	fixture.streamOnce.Do(func() { close(fixture.streamReady) })
	defer func() {
		fixture.mu.Lock()
		if fixture.current == blocks {
			fixture.current = nil
		}
		fixture.mu.Unlock()
	}()
	_, _ = writer.Write([]byte("[["))
	flusher.Flush()
	first := true
	for {
		select {
		case block := <-blocks:
			if !first {
				_, _ = writer.Write([]byte(","))
			}
			_, _ = writer.Write(block)
			flusher.Flush()
			first = false
		case <-request.Context().Done():
			return
		case <-fixture.stop:
			return
		}
	}
}

func (fixture *phoneFixture) awaitStreamReady(t *testing.T) {
	t.Helper()
	select {
	case <-fixture.streamReady:
	case <-time.After(5 * time.Second):
		t.Fatal("stream did not open")
	}
}

func mustMarshal(message proto.Message) []byte {
	encoded, err := proto.Marshal(message)
	if err != nil {
		panic(err)
	}
	return encoded
}

// safetyHost records what the link persists and reports to the host.
type safetyHost struct {
	mu          sync.Mutex
	states      [][]byte
	events      []eventCode
	delivered   chan eventCode
	emoji       chan string
	failPersist bool
	holdState   string
	holdReached chan struct{}
	release     chan struct{}

	holdEvent        string
	holdEventReached chan struct{}
	holdEventRelease chan struct{}
	onEvent          func(kind, code string)
}

func newSafetyHost() *safetyHost {
	return &safetyHost{emoji: make(chan string, 4), delivered: make(chan eventCode, 128)}
}

func (host *safetyHost) PersistState(state []byte) error {
	host.mu.Lock()
	if host.failPersist {
		host.mu.Unlock()
		return errors.New("synthetic persistence failure")
	}
	hold := host.holdState != "" && bytes.Contains(state, []byte(`"state":"`+host.holdState+`"`))
	reached, release := host.holdReached, host.release
	if hold {
		host.holdState = ""
	}
	host.mu.Unlock()
	if hold {
		close(reached)
		<-release
	}
	host.mu.Lock()
	defer host.mu.Unlock()
	host.states = append(host.states, append([]byte(nil), state...))
	return nil
}

// holdNextPersist makes the next envelope with the given state block inside
// PersistState until the returned release channel is closed. reached closes when the
// host is holding it.
func (host *safetyHost) holdNextPersist(state string) (reached, release chan struct{}) {
	host.mu.Lock()
	defer host.mu.Unlock()
	host.holdState = state
	host.holdReached = make(chan struct{})
	host.release = make(chan struct{})
	return host.holdReached, host.release
}

// callWithin runs a call that must return promptly; a call still blocked fails the test.
func callWithin(t *testing.T, description string, call func() error) error {
	t.Helper()
	result := make(chan error, 1)
	go func() { result <- call() }()
	select {
	case err := <-result:
		return err
	case <-time.After(3 * time.Second):
		t.Fatalf("%s blocked; it should return promptly", description)
		return nil
	}
}

// statesSince returns the envelopes the host received after the first n.
func (host *safetyHost) statesSince(n int) [][]byte {
	host.mu.Lock()
	defer host.mu.Unlock()
	return append([][]byte(nil), host.states[n:]...)
}

func (host *safetyHost) setFailPersist(fail bool) {
	host.mu.Lock()
	defer host.mu.Unlock()
	host.failPersist = fail
}

// stateCount is how many envelopes the host has received.
func (host *safetyHost) stateCount() int {
	host.mu.Lock()
	defer host.mu.Unlock()
	return len(host.states)
}

func (host *safetyHost) countEvent(kind, code string) int {
	host.mu.Lock()
	defer host.mu.Unlock()
	count := 0
	for _, event := range host.events {
		if event.kind == kind && event.code == code {
			count++
		}
	}
	return count
}

func (host *safetyHost) Event(kind, code string, _, _, _ int64) {
	host.mu.Lock()
	hold := host.holdEvent == code
	reached, release := host.holdEventReached, host.holdEventRelease
	if hold {
		host.holdEvent = ""
	}
	host.mu.Unlock()
	if hold {
		close(reached)
		<-release
	}
	host.mu.Lock()
	host.events = append(host.events, eventCode{kind, code})
	hook := host.onEvent
	host.mu.Unlock()
	if hook != nil {
		hook(kind, code)
	}
	select {
	case host.delivered <- eventCode{kind, code}:
	default:
	}
}

func (host *safetyHost) awaitEvent(t *testing.T, kind, code string) {
	t.Helper()
	timeout := time.NewTimer(5 * time.Second)
	defer timeout.Stop()
	for {
		select {
		case event := <-host.delivered:
			if event.kind == kind && event.code == code {
				return
			}
		case <-timeout.C:
			t.Fatalf("timed out waiting for %s/%s", kind, code)
		}
	}
}

// holdNextEvent makes the next event with the given code block inside Event until the
// returned release channel is closed. reached closes when the host is holding it.
func (host *safetyHost) holdNextEvent(code string) (reached, release chan struct{}) {
	host.mu.Lock()
	defer host.mu.Unlock()
	host.holdEvent = code
	host.holdEventReached = make(chan struct{})
	host.holdEventRelease = make(chan struct{})
	return host.holdEventReached, host.holdEventRelease
}

func (host *safetyHost) ShowEmoji(emoji string, _, _ int64) {
	select {
	case host.emoji <- emoji:
	default:
	}
}

func (host *safetyHost) hasEvent(kind, code string) bool {
	host.mu.Lock()
	defer host.mu.Unlock()
	for _, event := range host.events {
		if event.kind == kind && event.code == code {
			return true
		}
	}
	return false
}

func (host *safetyHost) eventText() string {
	host.mu.Lock()
	defer host.mu.Unlock()
	var builder strings.Builder
	for _, event := range host.events {
		builder.WriteString(event.kind + "/" + event.code + "\n")
	}
	return builder.String()
}

// lastEnvelope decodes the most recent persisted state.
func (host *safetyHost) lastEnvelope(t *testing.T) persistedState {
	t.Helper()
	host.mu.Lock()
	defer host.mu.Unlock()
	if len(host.states) == 0 {
		t.Fatal("nothing was persisted")
	}
	var envelope persistedState
	if err := json.Unmarshal(host.states[len(host.states)-1], &envelope); err != nil {
		t.Fatalf("persisted envelope does not decode: %v", err)
	}
	return envelope
}

// openSafetyLink opens a link on the process slot and closes it at cleanup.
func openSafetyLink(t *testing.T, state []byte) (*Link, *safetyHost) {
	t.Helper()
	host := newSafetyHost()
	link, err := Open(state, host)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		link.Close()
		awaitSafety(t, "link output drain", func() bool { return openLink.Load() != link })
	})
	return link, host
}

func setSafetyState(link *Link, state string, auth *libgm.AuthData) {
	link.mu.Lock()
	defer link.mu.Unlock()
	link.state = state
	if auth != nil {
		link.auth = auth
	}
}

// linkedSafetyAuth returns credentials that let Run connect without a token refresh.
func linkedSafetyAuth() *libgm.AuthData {
	auth := libgm.NewAuthData()
	auth.SetCookies(syntheticCookies())
	auth.TachyonAuthToken = []byte("synthetic-token")
	auth.TachyonExpiry = time.Now().Add(24 * time.Hour)
	auth.Browser = &gmproto.Device{SourceID: "browser-source"}
	auth.Mobile = &gmproto.Device{SourceID: fixtureSourceID}
	return auth
}

func canaryCookieJSON(t *testing.T) []byte {
	t.Helper()
	cookies := map[string]string{"OSID": canaryCookie, "SID": canaryCookie, "HSID": canaryCookie, "SSID": canaryCookie, "APISID": canaryCookie, "SAPISID": canaryCookie}
	encoded, err := json.Marshal(cookies)
	if err != nil {
		t.Fatal(err)
	}
	return encoded
}

func awaitSafety(t *testing.T, description string, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for !condition() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", description)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// startSafetyRun starts Run on its own goroutine and waits until it is running.
func startSafetyRun(t *testing.T, link *Link) <-chan error {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- link.Run() }()
	awaitSafety(t, "Run to start", func() bool { return strings.Contains(link.Status(), `"running":true`) })
	return done
}

func awaitSafetyResult(t *testing.T, done <-chan error, description string) error {
	t.Helper()
	select {
	case err := <-done:
		return err
	case <-time.After(5 * time.Second):
		t.Fatalf("%s did not return", description)
		return nil
	}
}

func runningSafetyClient(t *testing.T, link *Link) *libgm.Client {
	t.Helper()
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.client == nil {
		t.Fatal("no client owned by the running link")
	}
	return link.client
}

// assertSafetyReleased checks that Run's client is gone from the link and disconnected.
func assertSafetyReleased(t *testing.T, link *Link, client *libgm.Client) {
	t.Helper()
	link.mu.Lock()
	owned, running, healthy := link.client, link.running, link.healthy
	link.mu.Unlock()
	if owned != nil || running {
		t.Fatal("link still owns a running client after Run returned")
	}
	if healthy {
		t.Fatal("link reports healthy after Run returned")
	}
	if client.IsConnected() {
		t.Fatal("client still connected after Run returned")
	}
}

// assertAuthWiped checks that the last persisted envelope holds no credentials.
func assertAuthWiped(t *testing.T, host *safetyHost) {
	t.Helper()
	envelope := host.lastEnvelope(t)
	var auth map[string]json.RawMessage
	if len(envelope.Auth) > 0 {
		if err := json.Unmarshal(envelope.Auth, &auth); err != nil {
			t.Fatalf("persisted auth does not decode: %v", err)
		}
	}
	for _, key := range []string{"cookies", "browser", "mobile", "tachyon_token"} {
		if _, present := auth[key]; present {
			t.Fatalf("persisted auth still holds %q after wipe", key)
		}
	}
}

// assertNoCanary fails if any canary reaches the status, events or returned errors.
func assertNoCanary(t *testing.T, link *Link, host *safetyHost, errs ...error) {
	t.Helper()
	surfaces := map[string]string{"status": link.Status(), "events": host.eventText()}
	for index, err := range errs {
		if err != nil {
			surfaces[fmt.Sprintf("error %d", index)] = err.Error()
		}
	}
	for name, text := range surfaces {
		for _, canary := range []string{canaryCookie, canaryAccount, canaryNumber} {
			if strings.Contains(text, canary) {
				t.Fatalf("canary reached %s: %s", name, text)
			}
		}
	}
}

// awaitEmoji waits for Pair to show the emoji, which means it is blocked on the phone.
func awaitEmoji(t *testing.T, host *safetyHost) {
	t.Helper()
	select {
	case <-host.emoji:
	case <-time.After(10 * time.Second):
		t.Fatal("Pair never reached the emoji wait")
	}
}

func (*safetyHost) RcsEvent(string, int64, int64) {}
