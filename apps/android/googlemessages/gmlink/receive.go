package gmlink

import (
	"context"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"time"

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
	Signals    []rcsPageSignal    `json:"signals"`
	NextCursor string             `json:"next_cursor,omitempty"`
	HasMore    bool               `json:"has_more"`
}

type rcsPageSignal struct {
	Code                 string `json:"code"`
	GoogleConversationID string `json:"google_conversation_id"`
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
		SentAtMS:             time.UnixMicro(message.GetTimestamp()).UnixMilli(),
		Status:               status,
		TmpID:                message.GetTmpID(),
		RcsReported:          true,
	}}
}

func projectRcsPage(page *gmproto.ListMessagesResponse, conversation *gmproto.Conversation) rcsPageProjection {
	if page == nil {
		return rcsPageFailure("page_missing")
	}
	messages := page.GetMessages()
	if len(messages) == 0 {
		return rcsPageProjection{Code: "ok", Records: []rcsReceiveRecord{}, Signals: []rcsPageSignal{}}
	}
	cursor := opaquePageToken(page.GetCursor())
	if cursor == "" {
		// The pinned backfill consumer uses the final response message when no cursor is returned.
		lastMessage := messages[len(messages)-1]
		cursor = opaquePageToken(&gmproto.Cursor{
			LastItemID:        lastMessage.GetMessageID(),
			LastItemTimestamp: time.UnixMicro(lastMessage.GetTimestamp()).UnixMilli(),
		})
		if cursor == "" {
			return rcsPageFailure("cursor_missing")
		}
	}
	result := rcsPageProjection{
		Code:       "ok",
		Records:    make([]rcsReceiveRecord, 0, len(messages)),
		Signals:    make([]rcsPageSignal, 0),
		NextCursor: cursor,
		HasMore:    true,
	}
	for _, message := range messages {
		projection := projectRcsMessage(&libgm.WrappedMessage{Message: message}, conversation)
		if projection.Code == "protocol_switch" {
			result.Signals = append(result.Signals, rcsPageSignal{Code: projection.Code, GoogleConversationID: projection.GoogleConversationID})
			continue
		}
		if projection.Code != "ok" {
			return rcsPageFailure(projection.Code)
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

func (link *Link) projectRcsEventFor(queued queuedEvent, wrapped *libgm.WrappedMessage) {
	if wrapped == nil || wrapped.Message == nil {
		return
	}
	link.mu.Lock()
	if link.closed || queued.generation != link.generation || queued.streamEpoch != link.streamEpoch || !link.streamSourceMatchesLocked(queued.streamSource) || !link.hasOwnerVerificationLocked() {
		link.mu.Unlock()
		return
	}
	conversation := link.currentSupportedRcsConversationLocked(wrapped.Message.GetConversationID(), queued.generation, queued.streamEpoch)
	revision := link.stateRevision
	link.mu.Unlock()
	projection := rcsProjection{Code: "conversation_unsupported"}
	if conversation != nil {
		projection = projectRcsMessage(wrapped, conversation)
	}
	encoded, _ := json.Marshal(projection)
	link.enqueueOutput(hostOutput{
		rcsRecord:         string(encoded),
		generation:        queued.generation,
		revision:          revision,
		streamEpoch:       queued.streamEpoch,
		requireGeneration: true,
		requireRevision:   true,
		requireStream:     true,
	})
}

const rcsGapPageSize int64 = 100

var rcsGapPageTimeout = 5 * time.Second

func decodeOpaquePageToken(token string) (*gmproto.Cursor, bool) {
	if token == "" {
		return nil, true
	}
	data, err := base64.RawURLEncoding.DecodeString(token)
	if err != nil || len(data) <= 8 {
		return nil, false
	}
	cursor := &gmproto.Cursor{
		LastItemTimestamp: int64(binary.BigEndian.Uint64(data[:8])),
		LastItemID:        string(data[8:]),
	}
	if cursor.GetLastItemID() == "" || opaquePageToken(cursor) != token {
		return nil, false
	}
	return cursor, true
}

// FetchRcsGap returns one bounded history page projected through the receive contract.
func (link *Link) FetchRcsGap(googleConversationID, pageToken string) string {
	cursor, ok := decodeOpaquePageToken(pageToken)
	if googleConversationID == "" || !ok {
		return encodeRcsPage(rcsPageFailure("invalid_page_token"))
	}

	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	client, runCtx, generation, streamEpoch, ok := link.activeRuntime()
	if !ok {
		return encodeRcsPage(rcsPageFailure("gap_unavailable"))
	}

	link.mu.Lock()
	owned := link.hasOwnerVerificationLocked()
	conversation := link.currentSupportedRcsConversationLocked(googleConversationID, generation, streamEpoch)
	fetch := link.fetchMessages
	link.mu.Unlock()
	if !owned {
		return encodeRcsPage(rcsPageFailure("conversation_unproven"))
	}
	if conversation == nil {
		return encodeRcsPage(rcsPageFailure("conversation_unsupported"))
	}
	if fetch == nil {
		fetch = func(ctx context.Context, conversationID string, count int64, cursor *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
			return client.FetchMessages(ctx, conversationID, count, cursor)
		}
	}

	ctx, cancel := context.WithTimeout(runCtx, rcsGapPageTimeout)
	page, err := fetch(ctx, googleConversationID, rcsGapPageSize, cursor)
	cancel()
	if err != nil {
		switch {
		case errors.Is(err, context.DeadlineExceeded):
			return encodeRcsPage(rcsPageFailure("gap_deadline"))
		case errors.Is(err, context.Canceled):
			return encodeRcsPage(rcsPageFailure("gap_cancelled"))
		default:
			return encodeRcsPage(rcsPageFailure("gap_failed"))
		}
	}
	if !link.rcsGapOwned(generation, streamEpoch) {
		return encodeRcsPage(rcsPageFailure("stale_generation"))
	}
	return encodeRcsPage(projectRcsPage(page, conversation))
}

func (link *Link) rcsGapOwned(generation, streamEpoch uint64) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	return !link.closed && link.generation == generation && link.runGeneration == generation && link.streamEpoch == streamEpoch && link.hasOwnerVerificationLocked()
}

func rcsPageFailure(code string) rcsPageProjection {
	return rcsPageProjection{Code: code, Records: []rcsReceiveRecord{}, Signals: []rcsPageSignal{}}
}

func encodeRcsPage(result rcsPageProjection) string {
	encoded, _ := json.Marshal(result)
	return string(encoded)
}
