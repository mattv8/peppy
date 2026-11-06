package gmlink

import (
	"encoding/base64"
	"encoding/binary"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

type rcsReceiveRecord struct {
	GoogleMessageID      string `json:"google_message_id"`
	GoogleConversationID string `json:"google_conversation_id"`
	Cursor               string `json:"cursor,omitempty"`
	Direction            string `json:"direction"`
	Body                 string `json:"body"`
	SentAtMS             int64  `json:"sent_at_ms"`
	Status               string `json:"status"`
	TmpID                string `json:"tmp_id,omitempty"`
	RcsReported          bool   `json:"rcs_reported"`
	E2EEReported         *bool  `json:"e2ee_reported,omitempty"`
}

type rcsProjection struct {
	Code                 string            `json:"code"`
	GoogleConversationID string            `json:"google_conversation_id,omitempty"`
	Record               *rcsReceiveRecord `json:"record,omitempty"`
}

type rcsPageProjection struct {
	Code       string             `json:"code"`
	Records    []rcsReceiveRecord `json:"records"`
	NextCursor string             `json:"next_cursor,omitempty"`
	HasMore    bool               `json:"has_more"`
}

func projectRcsMessage(wrapped *libgm.WrappedMessage, conversation *gmproto.Conversation) rcsProjection {
	if wrapped == nil || wrapped.Message == nil {
		return rcsProjection{Code: "message_missing"}
	}
	message := wrapped.Message
	if code := protocolSwitchCode(message.GetMessageStatus().GetStatus()); code != "" {
		if message.GetConversationID() == "" {
			return rcsProjection{Code: "conversation_missing"}
		}
		return rcsProjection{Code: code, GoogleConversationID: message.GetConversationID()}
	}
	if message.GetMessageID() == "" || message.GetConversationID() == "" {
		return rcsProjection{Code: "identity_missing"}
	}
	if conversation == nil || conversation.GetConversationID() != message.GetConversationID() || conversation.GetType() != gmproto.ConversationType_RCS {
		return rcsProjection{Code: "conversation_unproven"}
	}
	direction, status, ok := projectRcsStatus(message.GetMessageStatus().GetStatus())
	if !ok {
		return rcsProjection{Code: "status_unproven"}
	}
	body, ok := textBody(message.GetMessageInfo())
	if !ok {
		return rcsProjection{Code: "unsupported_message"}
	}
	return rcsProjection{Code: "ok", Record: &rcsReceiveRecord{
		GoogleMessageID:      message.GetMessageID(),
		GoogleConversationID: message.GetConversationID(),
		Direction:            direction,
		Body:                 body,
		SentAtMS:             message.GetTimestamp(),
		Status:               status,
		TmpID:                message.GetTmpID(),
		RcsReported:          true,
	}}
}

func projectRcsPage(page *gmproto.ListMessagesResponse, conversation *gmproto.Conversation) rcsPageProjection {
	if page == nil || page.GetCursor() == nil {
		return rcsPageProjection{Code: "cursor_missing"}
	}
	cursor := opaquePageToken(page.GetCursor())
	if cursor == "" {
		return rcsPageProjection{Code: "cursor_missing"}
	}
	result := rcsPageProjection{Code: "ok", Records: make([]rcsReceiveRecord, 0, len(page.GetMessages())), NextCursor: cursor}
	for _, message := range page.GetMessages() {
		projection := projectRcsMessage(&libgm.WrappedMessage{Message: message}, conversation)
		if projection.Code != "ok" {
			return rcsPageProjection{Code: projection.Code}
		}
		projection.Record.Cursor = cursor
		result.Records = append(result.Records, *projection.Record)
	}
	return result
}

func textBody(infos []*gmproto.MessageInfo) (string, bool) {
	if len(infos) != 1 || infos[0] == nil || infos[0].GetMessageContent() == nil {
		return "", false
	}
	return infos[0].GetMessageContent().GetContent(), true
}

func projectRcsStatus(status gmproto.MessageStatusType) (direction, projection string, ok bool) {
	switch status {
	case gmproto.MessageStatusType_INCOMING_COMPLETE:
		return "incoming", "sent", true
	case gmproto.MessageStatusType_OUTGOING_SENDING:
		return "outgoing", "sending", true
	case gmproto.MessageStatusType_OUTGOING_COMPLETE:
		return "outgoing", "sent", true
	case gmproto.MessageStatusType_OUTGOING_DELIVERED:
		return "outgoing", "delivered", true
	case gmproto.MessageStatusType_OUTGOING_DISPLAYED:
		return "outgoing", "read", true
	default:
		if isTerminalRcsFailure(status) {
			return "outgoing", "failed", true
		}
		return "", "", false
	}
}

func isTerminalRcsFailure(status gmproto.MessageStatusType) bool {
	switch status {
	case gmproto.MessageStatusType_OUTGOING_FAILED_GENERIC,
		gmproto.MessageStatusType_OUTGOING_FAILED_EMERGENCY_NUMBER,
		gmproto.MessageStatusType_OUTGOING_FAILED_TOO_LARGE,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_RCS,
		gmproto.MessageStatusType_OUTGOING_FAILED_NO_RETRY_NO_FALLBACK,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_ENCRYPTION,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT_NO_MORE_RETRY,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_NEGATIVE_DELIVERY,
		gmproto.MessageStatusType_MESSAGE_STATUS_OUTGOING_FAILED_EMERGENCY_PROTOCOL_DETERMINATION_MESSAGE,
		gmproto.MessageStatusType_OUTGOING_RESTRICTED,
		gmproto.MessageStatusType_OUTGOING_FAILED_TO_ENCRYPT:
		return true
	default:
		return false
	}
}

func protocolSwitchCode(status gmproto.MessageStatusType) string {
	switch status {
	case gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_TEXT,
		gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_RCS,
		gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_ENCRYPTED_RCS,
		gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_E2EE_TO_TEXT:
		return "protocol_switch"
	default:
		return ""
	}
}

func opaquePageToken(cursor *gmproto.Cursor) string {
	if cursor == nil || cursor.GetLastItemID() == "" {
		return ""
	}
	data := make([]byte, 8+len(cursor.GetLastItemID()))
	binary.BigEndian.PutUint64(data, uint64(cursor.GetLastItemTimestamp()))
	copy(data[8:], cursor.GetLastItemID())
	return base64.RawURLEncoding.EncodeToString(data)
}
