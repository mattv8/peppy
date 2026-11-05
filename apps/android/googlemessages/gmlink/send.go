package gmlink

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const sendTimeout = 30 * time.Second

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
	TmpID                string `json:"tmp_id,omitempty"`
	GoogleConversationID string `json:"google_conversation_id,omitempty"`
	AckAtMS              int64  `json:"ack_at_ms,omitempty"`
}

// SendRcs invokes SendMessage once only after re-fetching the exact conversation
// and joining its outgoing id to a fresh settings event. An acknowledgement is
// transport metadata, not delivery evidence.
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
	return err == nil && parsed.Version() == 4
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
