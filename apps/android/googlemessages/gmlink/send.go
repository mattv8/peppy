package gmlink

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

const (
	preparedRcsTicketTTL = 30 * time.Second
	rcsSendDeadline      = 5 * time.Second
	maxRcsBodyBytes      = 4096
	maxPrepareJSONBytes  = 4608
	maxCommitJSONBytes   = 5120
	maxOpaqueIDBytes     = 256
	snapshotTokenBytes   = 64
	proofDigestBytes     = 44
)

type rcsSendRequest struct {
	CommandID            string `json:"command_id"`
	TmpID                string `json:"tmp_id"`
	GoogleConversationID string `json:"google_conversation_id"`
	MappingRevision      uint64 `json:"mapping_revision"`
	ProofDigest          string `json:"proof_digest"`
	Content              string `json:"content"`
}

type rcsSendResult struct {
	Code                 string `json:"code"`
	TicketID             string `json:"ticket_id,omitempty"`
	TmpID                string `json:"tmp_id,omitempty"`
	GoogleConversationID string `json:"google_conversation_id,omitempty"`
}

type prepareRcsRequest struct {
	TmpID                string `json:"tmp_id"`
	GoogleConversationID string `json:"google_conversation_id"`
	SnapshotToken        string `json:"snapshot_token"`
	Content              string `json:"content"`
}

type commitRcsPermit struct {
	CommandID            string `json:"command_id"`
	TmpID                string `json:"tmp_id"`
	GoogleConversationID string `json:"google_conversation_id"`
	SnapshotToken        string `json:"snapshot_token"`
	MappingRevision      uint64 `json:"mapping_revision"`
	ProofDigest          string `json:"proof_digest"`
	Content              string `json:"content"`
}

type preparedRcsTicket struct {
	id           string
	expiresAt    time.Time
	generation   uint64
	streamEpoch  uint64
	revision     uint64
	request      prepareRcsRequest
	payload      *gmproto.SendMessageRequest
	conversation *gmproto.Conversation
}

// SendRcs parses and validates a legacy request without dispatching a carrier effect.
func (link *Link) SendRcs(requestJSON string) string {
	var request rcsSendRequest
	if err := json.Unmarshal([]byte(requestJSON), &request); err != nil || !validRcsRequest(request) {
		return sendJSON(rcsSendResult{Code: "invalid_request"})
	}
	// Stage 1 intentionally leaves the legacy request inert. It lacks the
	// one-use core permit and exact prepared target required for effect admission.
	return sendJSON(rcsSendResult{Code: "pending_prepare", TmpID: request.TmpID, GoogleConversationID: request.GoogleConversationID})
}

func validRcsRequest(request rcsSendRequest) bool {
	if request.CommandID == "" || request.GoogleConversationID == "" || request.ProofDigest == "" || request.MappingRevision == 0 || request.Content == "" {
		return false
	}
	parsed, err := uuid.Parse(request.TmpID)
	return err == nil && parsed.Version() == 4 && parsed.Variant() == uuid.RFC4122 && parsed.String() == request.TmpID
}

// PrepareRcs accepts only the core-selected current Stage 2 target. It creates
// no transport effect and retains no caller data after a ticket is consumed.
func (link *Link) PrepareRcs(requestJSON string) string {
	var request prepareRcsRequest
	if !decodeFixedJSON(requestJSON, &request) || !validPrepareRcsRequest(request) {
		return sendJSON(rcsSendResult{Code: "invalid_request"})
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	now := link.nowForTicketLocked()
	if link.preparedTicket != nil && !now.Before(link.preparedTicket.expiresAt) {
		link.clearPreparedTicketLocked()
	}
	if link.preparedTicket != nil || link.sendAdmissionInFlight {
		return sendJSON(rcsSendResult{Code: "busy"})
	}
	if link.closed || !link.hasOwnerVerificationLocked() || link.runGeneration != link.generation {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	if request.SnapshotToken != snapshotToken(link.generation, link.streamEpoch, link.indexRevision) {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	conversation := link.currentSupportedRcsConversationLocked(request.GoogleConversationID, link.generation, link.streamEpoch)
	if conversation == nil {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	payload, code := validatedPayload(conversation, link.settings, rcsSendRequest{TmpID: request.TmpID, GoogleConversationID: request.GoogleConversationID, Content: request.Content})
	if code != "" {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	ticketID := uuid.NewString()
	link.preparedTicket = &preparedRcsTicket{id: ticketID, expiresAt: now.Add(preparedRcsTicketTTL), generation: link.generation, streamEpoch: link.streamEpoch, revision: link.indexRevision, request: request, payload: proto.Clone(payload).(*gmproto.SendMessageRequest), conversation: cloneConversation(conversation)}
	return sendJSON(rcsSendResult{Code: "prepared", TicketID: ticketID})
}

// CommitPreparedRcs consumes a prepared ticket before dispatch. A transport
// error after that point is deliberately outcome_unknown rather than retried.
func (link *Link) CommitPreparedRcs(ticketID, permitJSON string) string {
	var permit commitRcsPermit
	if ticketID == "" || !decodeFixedJSON(permitJSON, &permit) || !validCommitRcsPermit(permit) {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	link.mu.Lock()
	now := link.nowForTicketLocked()
	ticket := link.preparedTicket
	if ticket == nil || ticket.id != ticketID || !now.Before(ticket.expiresAt) || !permitMatchesTicket(permit, ticket) {
		if ticket != nil && !now.Before(ticket.expiresAt) {
			link.clearPreparedTicketLocked()
		}
		link.mu.Unlock()
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	link.preparedTicket = nil
	link.sendAdmissionInFlight = true
	if link.runCtx == nil {
		link.sendAdmissionInFlight = false
		link.mu.Unlock()
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	operationCtx, operationCancel := context.WithTimeout(link.runCtx, rcsSendDeadline)
	link.mu.Unlock()
	defer operationCancel()
	defer func() {
		link.mu.Lock()
		link.sendAdmissionInFlight = false
		link.mu.Unlock()
	}()

	if !link.lockRPC(operationCtx) {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	defer link.rpcMu.Unlock()
	client, _, generation, streamEpoch, ok := link.activeRuntime()
	if !ok || generation != ticket.generation || streamEpoch != ticket.streamEpoch {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	if !link.ticketCurrent(ticket) {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	fetched, fetchCode := link.getConversation(operationCtx, client, ticket.request.GoogleConversationID)
	if fetchCode != "" || !proto.Equal(fetched, ticket.conversation) {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	link.mu.Lock()
	current := link.currentSupportedRcsConversationLocked(ticket.request.GoogleConversationID, generation, streamEpoch)
	if current == nil || link.indexRevision != ticket.revision || !link.hasOwnerVerificationLocked() {
		link.mu.Unlock()
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	payload, code := validatedPayload(current, link.settings, rcsSendRequest{TmpID: permit.TmpID, GoogleConversationID: permit.GoogleConversationID, Content: permit.Content})
	effect := link.sendMessage
	link.mu.Unlock()
	if code != "" || !proto.Equal(payload, ticket.payload) {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	if effect == nil {
		effect = client.SendMessage
	}
	ctx, cancel := context.WithCancel(operationCtx)
	ctx = context.WithValue(ctx, sendAdmissionContextKey{}, sendAdmission(func() bool { return link.sendAdmissionCurrent(ticket) }))
	response, err := effect(ctx, payload)
	cancel()
	if errors.Is(err, errSendAdmissionRevoked) {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	if err != nil || response == nil {
		return sendJSON(rcsSendResult{Code: "outcome_unknown"})
	}
	if response.GetGoogleAccountSwitch() != nil {
		return sendJSON(rcsSendResult{Code: "outcome_unknown"})
	}
	if response.GetStatus() == gmproto.SendMessageResponse_SUCCESS {
		return sendJSON(rcsSendResult{Code: "accepted"})
	}
	return sendJSON(rcsSendResult{Code: "outcome_unknown"})
}

func (link *Link) lockRPC(ctx context.Context) bool {
	ticker := time.NewTicker(time.Millisecond)
	defer ticker.Stop()
	for {
		if link.rpcMu.TryLock() {
			return true
		}
		select {
		case <-ctx.Done():
			return false
		case <-ticker.C:
		}
	}
}

func (link *Link) ticketCurrent(ticket *preparedRcsTicket) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	return link.nowForTicketLocked().Before(ticket.expiresAt) && !link.closed && link.generation == ticket.generation && link.runGeneration == ticket.generation && link.streamEpoch == ticket.streamEpoch && link.indexRevision == ticket.revision && link.hasOwnerVerificationLocked()
}

func (link *Link) sendAdmissionCurrent(ticket *preparedRcsTicket) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	if !link.nowForTicketLocked().Before(ticket.expiresAt) || link.closed || link.generation != ticket.generation || link.runGeneration != ticket.generation || link.streamEpoch != ticket.streamEpoch || link.indexRevision != ticket.revision || !link.hasOwnerVerificationLocked() {
		return false
	}
	current := link.currentSupportedRcsConversationLocked(ticket.request.GoogleConversationID, ticket.generation, ticket.streamEpoch)
	if current == nil || !proto.Equal(current, ticket.conversation) {
		return false
	}
	payload, code := validatedPayload(current, link.settings, rcsSendRequest{TmpID: ticket.request.TmpID, GoogleConversationID: ticket.request.GoogleConversationID, Content: ticket.request.Content})
	return code == "" && proto.Equal(payload, ticket.payload)
}

// CancelPreparedRcs removes an unconsumed ticket. It is intentionally idempotent
// only in its refusal: a consumed or unknown ticket cannot be revived.
func (link *Link) CancelPreparedRcs(ticketID string) string {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.preparedTicket == nil || link.preparedTicket.id != ticketID {
		return sendJSON(rcsSendResult{Code: "rejected"})
	}
	link.clearPreparedTicketLocked()
	return sendJSON(rcsSendResult{Code: "cancelled"})
}

func (link *Link) clearPreparedTicketLocked() { link.preparedTicket = nil }

func (link *Link) nowForTicketLocked() time.Time {
	if link.ticketNow != nil {
		return link.ticketNow()
	}
	return time.Now()
}

func decodeFixedJSON(input string, output any) bool {
	maxBytes, fields := 0, map[string]struct{}(nil)
	switch output.(type) {
	case *prepareRcsRequest:
		maxBytes, fields = maxPrepareJSONBytes, requiredJSONFields("tmp_id", "google_conversation_id", "snapshot_token", "content")
	case *commitRcsPermit:
		maxBytes, fields = maxCommitJSONBytes, requiredJSONFields("command_id", "tmp_id", "google_conversation_id", "snapshot_token", "mapping_revision", "proof_digest", "content")
	default:
		return false
	}
	if len(input) > maxBytes {
		return false
	}
	decoder := json.NewDecoder(bytes.NewBufferString(input))
	token, err := decoder.Token()
	if err != nil || token != json.Delim('{') {
		return false
	}
	values := make(map[string]json.RawMessage, len(fields))
	for decoder.More() {
		keyToken, err := decoder.Token()
		key, ok := keyToken.(string)
		if err != nil || !ok {
			return false
		}
		if _, allowed := fields[key]; !allowed {
			return false
		}
		if _, duplicate := values[key]; duplicate {
			return false
		}
		var value json.RawMessage
		if decoder.Decode(&value) != nil {
			return false
		}
		values[key] = value
	}
	if token, err = decoder.Token(); err != nil || token != json.Delim('}') || len(values) != len(fields) || decoder.Decode(&struct{}{}) != io.EOF {
		return false
	}
	encoded, err := json.Marshal(values)
	return err == nil && json.Unmarshal(encoded, output) == nil
}

func requiredJSONFields(names ...string) map[string]struct{} {
	fields := make(map[string]struct{}, len(names))
	for _, name := range names {
		fields[name] = struct{}{}
	}
	return fields
}

func validPrepareRcsRequest(request prepareRcsRequest) bool {
	parsed, err := uuid.Parse(request.TmpID)
	return err == nil && parsed.Version() == 4 && parsed.Variant() == uuid.RFC4122 && parsed.String() == request.TmpID && request.GoogleConversationID != "" && len(request.GoogleConversationID) <= maxOpaqueIDBytes && request.SnapshotToken != "" && len(request.SnapshotToken) <= snapshotTokenBytes && request.Content != "" && len(request.Content) <= maxRcsBodyBytes
}

func validCommitRcsPermit(permit commitRcsPermit) bool {
	if len(permit.CommandID) == 0 || len(permit.CommandID) > maxOpaqueIDBytes || len(permit.ProofDigest) != proofDigestBytes {
		return false
	}
	digest, err := base64.StdEncoding.DecodeString(permit.ProofDigest)
	return err == nil && len(digest) == 32 && permit.MappingRevision != 0 && validPrepareRcsRequest(prepareRcsRequest{TmpID: permit.TmpID, GoogleConversationID: permit.GoogleConversationID, SnapshotToken: permit.SnapshotToken, Content: permit.Content})
}

func permitMatchesTicket(permit commitRcsPermit, ticket *preparedRcsTicket) bool {
	return permit.TmpID == ticket.request.TmpID && permit.GoogleConversationID == ticket.request.GoogleConversationID && permit.SnapshotToken == ticket.request.SnapshotToken && permit.Content == ticket.request.Content
}

func snapshotToken(generation, streamEpoch, revision uint64) string {
	return fmt.Sprintf("%d:%d:%d", generation, streamEpoch, revision)
}

func (link *Link) currentSettings() (*gmproto.Settings, string) {
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.settings == nil || link.settingsAt.IsZero() {
		return nil, "settings_unavailable"
	}
	return link.settings, ""
}

func validatedPayload(conversation *gmproto.Conversation, settings *gmproto.Settings, request rcsSendRequest) (*gmproto.SendMessageRequest, string) {
	if conversation.GetConversationID() != request.GoogleConversationID || conversation.GetType() != gmproto.ConversationType_RCS || conversation.GetStatus() != gmproto.ConversationStatus_ACTIVE {
		return nil, "conversation_changed"
	}
	if conversation.GetReadOnly() || conversation.GetSendMode() != gmproto.ConversationSendMode_SEND_MODE_AUTO {
		return nil, "conversation_not_writable"
	}
	others := visibleOtherParticipants(conversation)
	if len(others) != 1 {
		return nil, "conversation_not_one_to_one"
	}
	outgoingID := conversation.GetDefaultOutgoingID()
	if outgoingID == "" {
		return nil, "sim_unavailable"
	}
	var simPayload *gmproto.SIMPayload
	for _, card := range settings.GetSIMCards() {
		if card.GetSIMParticipant().GetID() == outgoingID && card.GetRCSChats().GetEnabled() {
			if simPayload != nil || card.GetSIMData().GetSIMPayload() == nil {
				return nil, "sim_unavailable"
			}
			simPayload = card.GetSIMData().GetSIMPayload()
		}
	}
	if simPayload == nil {
		return nil, "sim_unavailable"
	}
	return &gmproto.SendMessageRequest{
		ConversationID: conversation.GetConversationID(), SIMPayload: simPayload, TmpID: request.TmpID, ForceRCS: false,
		MessagePayload: &gmproto.MessagePayload{TmpID: request.TmpID, TmpID2: request.TmpID, ConversationID: conversation.GetConversationID(), ParticipantID: outgoingID,
			MessageInfo: []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: request.Content}}}}},
	}, ""
}

func sendJSON(result rcsSendResult) string {
	encoded, _ := json.Marshal(result)
	return string(encoded)
}
