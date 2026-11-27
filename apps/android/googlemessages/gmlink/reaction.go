package gmlink

import (
	"context"
	"errors"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

const (
	maxReactionJSONBytes  = 2048
	maxReactionEmojiBytes = 32
)

type reactionRequest struct {
	RequestID            string `json:"request_id"`
	GoogleConversationID string `json:"google_conversation_id"`
	GoogleMessageID      string `json:"google_message_id"`
	Action               string `json:"action"`
	Emoji                string `json:"emoji"`
	OwnerGeneration      uint64 `json:"owner_generation"`
}

type preparedReactionTicket struct {
	expiresAt    time.Time
	generation   uint64
	streamEpoch  uint64
	revision     uint64
	request      reactionRequest
	payload      *gmproto.SendReactionRequest
	conversation *gmproto.Conversation
}

// PrepareReaction validates a one-shot native reaction without sending it.
func (link *Link) PrepareReaction(requestJSON string) (string, error) {
	request, ok := decodeReactionRequest(requestJSON)
	if !ok {
		return reactionJSON("invalid_request"), nil
	}
	if !validReactionEmoji(request.Emoji) {
		return reactionJSON("invalid_emoji"), nil
	}

	link.mu.Lock()
	defer link.mu.Unlock()
	now := link.nowForTicketLocked()
	if link.preparedReactionTicket != nil && !now.Before(link.preparedReactionTicket.expiresAt) {
		link.clearPreparedTicketLocked()
	}
	link.expireNativeTicketLocked(now)
	if link.preparedReactionTicket != nil || link.preparedTicket != nil || link.nativeTicket != nil || link.nativeInFlight != nil || link.sendAdmissionInFlight {
		return reactionJSON("busy"), nil
	}
	if link.closed || request.OwnerGeneration == 0 || request.OwnerGeneration != link.generation || link.runGeneration != link.generation || !link.hasSendVerificationLocked() {
		return reactionJSON("rejected"), nil
	}
	conversation := link.currentSupportedRcsConversationLocked(request.GoogleConversationID, link.generation, link.streamEpoch)
	if conversation == nil {
		return reactionJSON("rejected"), nil
	}
	payload, ok := reactionPayload(conversation, link.settings, request)
	if !ok {
		return reactionJSON("rejected"), nil
	}
	link.preparedReactionTicket = &preparedReactionTicket{
		expiresAt: now.Add(preparedRcsTicketTTL), generation: link.generation, streamEpoch: link.streamEpoch,
		revision: link.indexRevision, request: request, payload: payload, conversation: cloneConversation(conversation),
	}
	return reactionJSON("prepared"), nil
}

// CommitPreparedReaction consumes a prepared reaction before its sole RPC attempt.
func (link *Link) CommitPreparedReaction(requestJSON string) (string, error) {
	request, ok := decodeReactionRequest(requestJSON)
	if !ok || !validReactionEmoji(request.Emoji) {
		return reactionJSON("rejected"), nil
	}

	link.mu.Lock()
	now := link.nowForTicketLocked()
	ticket := link.preparedReactionTicket
	if ticket == nil || !now.Before(ticket.expiresAt) || ticket.request != request {
		if ticket != nil && !now.Before(ticket.expiresAt) {
			link.clearPreparedTicketLocked()
		}
		link.mu.Unlock()
		return reactionJSON("rejected"), nil
	}
	link.preparedReactionTicket = nil
	link.sendAdmissionInFlight = true
	if link.runCtx == nil {
		link.sendAdmissionInFlight = false
		link.mu.Unlock()
		return reactionJSON("rejected"), nil
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
		return reactionJSON("rejected"), nil
	}
	defer link.rpcMu.Unlock()
	client, _, generation, streamEpoch, active := link.activeRuntime()
	if !active || generation != ticket.generation || streamEpoch != ticket.streamEpoch || !link.reactionTicketCurrent(ticket) {
		return reactionJSON("rejected"), nil
	}
	fetched, code := link.getConversation(operationCtx, client, request.GoogleConversationID)
	if code != "" || !sameSendAuthority(fetched, ticket.conversation) {
		return reactionJSON("rejected"), nil
	}
	if !link.reactionTicketCurrent(ticket) {
		return reactionJSON("rejected"), nil
	}
	link.mu.Lock()
	current := link.currentSupportedRcsConversationLocked(request.GoogleConversationID, generation, streamEpoch)
	valid := current != nil && link.indexRevision == ticket.revision && link.hasSendVerificationLocked()
	payload, payloadValid := reactionPayload(current, link.settings, request)
	effect := link.sendReaction
	link.mu.Unlock()
	if !valid || !payloadValid || !proto.Equal(payload, ticket.payload) {
		return reactionJSON("rejected"), nil
	}
	if effect == nil {
		effect = client.SendReaction
	}
	ctx, cancel := context.WithCancel(operationCtx)
	ctx = context.WithValue(ctx, sendAdmissionContextKey{}, sendAdmission(func() bool { return link.reactionAdmissionCurrent(ticket) }))
	response, err := effect(ctx, payload)
	cancel()
	if errors.Is(err, errSendAdmissionRevoked) {
		return reactionJSON("rejected"), nil
	}
	if err != nil || response == nil || !response.GetSuccess() {
		return reactionJSON("outcome_unknown"), nil
	}
	return reactionJSON("accepted"), nil
}

func (link *Link) reactionTicketCurrent(ticket *preparedReactionTicket) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	return link.nowForTicketLocked().Before(ticket.expiresAt) && !link.closed && link.generation == ticket.generation && link.runGeneration == ticket.generation &&
		link.streamEpoch == ticket.streamEpoch && link.indexRevision == ticket.revision && link.hasSendVerificationLocked()
}

func (link *Link) reactionAdmissionCurrent(ticket *preparedReactionTicket) bool {
	link.mu.Lock()
	defer link.mu.Unlock()
	if !link.nowForTicketLocked().Before(ticket.expiresAt) || link.closed || link.generation != ticket.generation ||
		link.runGeneration != ticket.generation || link.streamEpoch != ticket.streamEpoch || link.indexRevision != ticket.revision ||
		!link.hasSendVerificationLocked() {
		return false
	}
	current := link.currentSupportedRcsConversationLocked(ticket.request.GoogleConversationID, ticket.generation, ticket.streamEpoch)
	payload, valid := reactionPayload(current, link.settings, ticket.request)
	return valid && sameSendAuthority(current, ticket.conversation) && proto.Equal(payload, ticket.payload)
}

func decodeReactionRequest(input string) (reactionRequest, bool) {
	var request reactionRequest
	if len(input) > maxReactionJSONBytes || !utf8.ValidString(input) {
		return reactionRequest{}, false
	}
	if !decodeFixedReactionJSON(input, &request) || request.RequestID == "" || len(request.RequestID) > maxOpaqueIDBytes ||
		request.GoogleConversationID == "" || len(request.GoogleConversationID) > maxOpaqueIDBytes ||
		!validGoogleMessageID(request.GoogleMessageID) ||
		(request.Action != "add" && request.Action != "switch" && request.Action != "remove") {
		return reactionRequest{}, false
	}
	return request, true
}

// validGoogleMessageID is the one opaque-id rule for Google message ids: non-empty, bounded, valid UTF-8,
// and free of control characters (Unicode Cc), the same rule the core applies to a receive reply id.
func validGoogleMessageID(id string) bool {
	return id != "" && len(id) <= maxMessageIDBytes && utf8.ValidString(id) && strings.IndexFunc(id, unicode.IsControl) < 0
}

func reactionPayload(conversation *gmproto.Conversation, settings *gmproto.Settings, request reactionRequest) (*gmproto.SendReactionRequest, bool) {
	if conversation == nil || conversation.GetConversationID() != request.GoogleConversationID ||
		conversation.GetType() != gmproto.ConversationType_RCS || conversation.GetStatus() != gmproto.ConversationStatus_ACTIVE ||
		conversation.GetReadOnly() || conversation.GetSendMode() != gmproto.ConversationSendMode_SEND_MODE_AUTO {
		return nil, false
	}
	simPayload := reactionSIMPayload(conversation, settings)
	if simPayload == nil {
		return nil, false
	}
	action := map[string]gmproto.SendReactionRequest_Action{
		"add": gmproto.SendReactionRequest_ADD, "switch": gmproto.SendReactionRequest_SWITCH, "remove": gmproto.SendReactionRequest_REMOVE,
	}[request.Action]
	return &gmproto.SendReactionRequest{MessageID: request.GoogleMessageID, ReactionData: gmproto.MakeReactionData(request.Emoji), Action: action, SIMPayload: simPayload}, true
}

func reactionSIMPayload(conversation *gmproto.Conversation, settings *gmproto.Settings) *gmproto.SIMPayload {
	outgoingID := conversation.GetDefaultOutgoingID()
	if outgoingID == "" {
		return nil
	}
	var payload *gmproto.SIMPayload
	for _, card := range settings.GetSIMCards() {
		if card.GetSIMParticipant().GetID() == outgoingID && card.GetRCSChats().GetEnabled() {
			if payload != nil || card.GetSIMData().GetSIMPayload() == nil {
				return nil
			}
			payload = card.GetSIMData().GetSIMPayload()
		}
	}
	return payload
}

func validReactionEmoji(emoji string) bool {
	if emoji == "" || !utf8.ValidString(emoji) || len(emoji) > maxReactionEmojiBytes {
		return false
	}
	runes := []rune(emoji)
	if containsForbiddenEmojiRune(runes) || runes[0] == '\u200d' || runes[len(runes)-1] == '\u200d' {
		return false
	}
	if len(runes) == 2 && isRegionalIndicator(runes[0]) && isRegionalIndicator(runes[1]) {
		return true
	}
	return isPictographicEmojiSequence(runes)
}

func containsForbiddenEmojiRune(runes []rune) bool {
	for _, value := range runes {
		if value == '\ufe0e' || value == '\u20e3' || (value >= 0xe0020 && value <= 0xe007f) {
			return true
		}
	}
	return false
}

func isPictographicEmojiSequence(runes []rune) bool {
	for index := 0; index < len(runes); {
		if !isExtendedPictographic(runes[index]) {
			return false
		}
		index++
		if index < len(runes) && runes[index] == '\ufe0f' {
			index++
		}
		if index < len(runes) && isEmojiModifier(runes[index]) {
			index++
		}
		if index < len(runes) && runes[index] == '\ufe0f' {
			index++
		}
		if index == len(runes) {
			return true
		}
		if runes[index] != '\u200d' {
			return false
		}
		index++
	}
	return false
}

type emojiRange struct{ first, last rune }

// extendedPictographicRanges are Unicode 16.0 emoji-data.txt Extended_Pictographic
// ranges, matching the core reaction validator.
var extendedPictographicRanges = []emojiRange{
	{0x00A9, 0x00A9}, {0x00AE, 0x00AE}, {0x203C, 0x203C}, {0x2049, 0x2049},
	{0x2122, 0x2122}, {0x2139, 0x2139}, {0x2194, 0x2199}, {0x21A9, 0x21AA},
	{0x231A, 0x231B}, {0x2328, 0x2328}, {0x23CF, 0x23CF}, {0x23E9, 0x23F3},
	{0x23F8, 0x23FA}, {0x24C2, 0x24C2}, {0x25AA, 0x25AB}, {0x25B6, 0x25B6},
	{0x25C0, 0x25C0}, {0x25FB, 0x25FE}, {0x2600, 0x27BF}, {0x2934, 0x2935},
	{0x2B05, 0x2B07}, {0x2B1B, 0x2B1C}, {0x2B50, 0x2B50}, {0x2B55, 0x2B55},
	{0x3030, 0x3030}, {0x303D, 0x303D}, {0x3297, 0x3297}, {0x3299, 0x3299},
	{0x1F000, 0x1F0FF}, {0x1F10D, 0x1F10F}, {0x1F12F, 0x1F12F}, {0x1F16C, 0x1F171},
	{0x1F17E, 0x1F17F}, {0x1F18E, 0x1F18E}, {0x1F191, 0x1F19A}, {0x1F1AD, 0x1F1E5},
	{0x1F201, 0x1F20F}, {0x1F21A, 0x1F21A}, {0x1F22F, 0x1F22F}, {0x1F232, 0x1F23A},
	{0x1F23C, 0x1F23F}, {0x1F249, 0x1F3FA}, {0x1F400, 0x1F53D}, {0x1F546, 0x1F64F},
	{0x1F680, 0x1F6FF}, {0x1F774, 0x1F77F}, {0x1F7D5, 0x1F7FF}, {0x1F80C, 0x1F80F},
	{0x1F848, 0x1F84F}, {0x1F85A, 0x1F85F}, {0x1F888, 0x1F88F}, {0x1F8AE, 0x1F8FF},
	{0x1F90C, 0x1F93A}, {0x1F93C, 0x1F945}, {0x1F947, 0x1FAFF}, {0x1FC00, 0x1FFFD},
}

func isExtendedPictographic(value rune) bool {
	for _, candidate := range extendedPictographicRanges {
		if value >= candidate.first && value <= candidate.last {
			return true
		}
	}
	return false
}

func isEmojiModifier(value rune) bool { return value >= 0x1F3FB && value <= 0x1F3FF }

func isRegionalIndicator(value rune) bool { return value >= 0x1F1E6 && value <= 0x1F1FF }

func reactionJSON(code string) string { return `{"code":"` + code + `"}` }

func decodeFixedReactionJSON(input string, output *reactionRequest) bool {
	return decodeFixedJSON(input, output)
}
