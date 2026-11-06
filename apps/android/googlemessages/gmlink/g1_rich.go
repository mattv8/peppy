package gmlink

import (
	"sort"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	m6RichIDLimit    = 256
	m6ReactionLimit  = 32
	m6EmojiByteLimit = 32
)

type m6ReplyProjection struct {
	MessageID        string
	ConversationID   string
	ReplyToMessageID string
}

type m6ReactionProjection struct {
	MessageID      string
	ConversationID string
	Emoji          string
	Type           int32
	ActorCount     int
}

type m6ReceiptProjection struct {
	MessageID      string
	ConversationID string
	Status         int32
}

func projectM6Rich(message *gmproto.Message) (m6ReplyProjection, []m6ReactionProjection, m6ReceiptProjection, string) {
	if message == nil || !m6ID(message.GetMessageID()) || !m6ID(message.GetConversationID()) {
		return m6ReplyProjection{}, nil, m6ReceiptProjection{}, "rich_projection_unverified"
	}
	reply := m6ReplyProjection{MessageID: message.GetMessageID(), ConversationID: message.GetConversationID()}
	if upstreamReply := message.GetReplyMessage(); upstreamReply != nil {
		if !m6ID(upstreamReply.GetMessageID()) || !m6ID(upstreamReply.GetConversationID()) || upstreamReply.GetConversationID() != message.GetConversationID() {
			return m6ReplyProjection{}, nil, m6ReceiptProjection{}, "rich_projection_unverified"
		}
		reply.ReplyToMessageID = upstreamReply.GetMessageID()
	}
	reactions, code := projectM6Reactions(message)
	if code != "verified" {
		return m6ReplyProjection{}, nil, m6ReceiptProjection{}, code
	}
	receipt := m6ReceiptProjection{MessageID: message.GetMessageID(), ConversationID: message.GetConversationID()}
	if status := message.GetMessageStatus(); status != nil {
		receipt.Status = int32(status.GetStatus())
	}
	return reply, reactions, receipt, "verified"
}

func projectM6Reactions(message *gmproto.Message) ([]m6ReactionProjection, string) {
	if len(message.GetReactions()) > m6ReactionLimit {
		return nil, "rich_projection_unverified"
	}
	projections := make([]m6ReactionProjection, 0, len(message.GetReactions()))
	for _, reaction := range message.GetReactions() {
		if reaction == nil || reaction.GetData() == nil || !m6Emoji(reaction.GetData().GetUnicode()) || len(reaction.GetParticipantIDs()) == 0 || len(reaction.GetParticipantIDs()) > m4GroupParticipantLimit {
			return nil, "rich_projection_unverified"
		}
		projections = append(projections, m6ReactionProjection{MessageID: message.GetMessageID(), ConversationID: message.GetConversationID(), Emoji: reaction.GetData().GetUnicode(), Type: int32(reaction.GetData().GetType()), ActorCount: len(reaction.GetParticipantIDs())})
	}
	sort.Slice(projections, func(left, right int) bool { return projections[left].Emoji < projections[right].Emoji })
	return projections, "verified"
}

func m6ID(value string) bool {
	return value != "" && utf8.ValidString(value) && len(value) <= m6RichIDLimit
}

func m6Emoji(value string) bool {
	return value != "" && utf8.ValidString(value) && len(value) <= m6EmojiByteLimit
}

// The pinned reaction entry is a snapshot of emoji plus participant IDs. It
// has no revision, timestamp, or removal action, so delta semantics are unsafe.
func m6ReactionDeltaCapability() string {
	return "reaction_delta_unverified"
}
