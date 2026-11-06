package gmlink

import (
	"reflect"
	"sort"
	"testing"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	richIDLimit        = 256
	reactionLimit      = 32
	emojiByteLimit     = 32
	reactionActorLimit = 100
)

type replyProjection struct{ MessageID, ConversationID, ReplyToMessageID string }
type reactionProjection struct {
	MessageID, ConversationID, Emoji string
	Type                             int32
	ActorCount                       int
}
type receiptProjection struct {
	MessageID, ConversationID string
	Status                    int32
}

func TestM6RichProjectionsUseOnlyIdentityAndMetadata(t *testing.T) {
	message := validRichMessage()
	reply, reactions, receipt, code := projectRich(message)
	if code != "verified" || reply.ReplyToMessageID != "reply-opaque" || receipt.Status != int32(gmproto.MessageStatusType_OUTGOING_DELIVERED) || len(reactions) != 1 || reactions[0].ActorCount != 2 {
		t.Fatalf("code=%q reply=%#v reactions=%#v receipt=%#v", code, reply, reactions, receipt)
	}
}
func TestM6ReactionDescriptorsHaveNoDeltaFields(t *testing.T) {
	if got := fieldNames((&gmproto.ReactionEntry{}).ProtoReflect().Descriptor()); !reflect.DeepEqual(got, []string{"data", "participantIDs"}) {
		t.Fatalf("entry fields=%v", got)
	}
	if got := fieldNames((&gmproto.ReactionData{}).ProtoReflect().Descriptor()); !reflect.DeepEqual(got, []string{"unicode", "type", "customEmoji"}) {
		t.Fatalf("data fields=%v", got)
	}
}
func TestM6RichProjectionFailsClosed(t *testing.T) {
	many := validRichMessage()
	for index := 0; index < reactionLimit; index++ {
		many.Reactions = append(many.Reactions, &gmproto.ReactionEntry{Data: &gmproto.ReactionData{Unicode: string(rune(index + 33)), Type: gmproto.EmojiType_LIKE}, ParticipantIDs: []string{"actor"}})
	}
	for name, mutate := range map[string]func(*gmproto.Message){"missing_identity": func(m *gmproto.Message) { m.ConversationID = "" }, "nil_status": func(m *gmproto.Message) { m.MessageStatus = nil }, "unknown_status": func(m *gmproto.Message) { m.MessageStatus.Status = gmproto.MessageStatusType_STATUS_UNKNOWN }, "unrecognized_status": func(m *gmproto.Message) { m.MessageStatus.Status = gmproto.MessageStatusType(99) }, "reply_mismatch": func(m *gmproto.Message) { m.ReplyMessage.ConversationID = "other" }, "duplicate_emoji": func(m *gmproto.Message) { m.Reactions = append(m.Reactions, m.Reactions[0]) }, "duplicate_actor": func(m *gmproto.Message) { m.Reactions[0].ParticipantIDs = []string{"actor-a", "actor-a"} }, "unknown_emoji_type": func(m *gmproto.Message) { m.Reactions[0].Data.Type = gmproto.EmojiType_REACTION_TYPE_UNSPECIFIED }, "unrecognized_emoji_type": func(m *gmproto.Message) { m.Reactions[0].Data.Type = gmproto.EmojiType(99) }, "too_many_reactions": func(m *gmproto.Message) { *m = *many }, "empty_actors": func(m *gmproto.Message) { m.Reactions[0].ParticipantIDs = nil }} {
		t.Run(name, func(t *testing.T) {
			candidate := cloneRich(validRichMessage())
			mutate(candidate)
			if _, _, _, code := projectRich(candidate); code == "verified" {
				t.Fatal("accepted unsafe rich metadata")
			}
		})
	}
}
func projectRich(message *gmproto.Message) (replyProjection, []reactionProjection, receiptProjection, string) {
	if message == nil || !richID(message.GetMessageID()) || !richID(message.GetConversationID()) || message.GetMessageStatus() == nil || !knownStatus(message.GetMessageStatus().GetStatus()) {
		return replyProjection{}, nil, receiptProjection{}, "rich_projection_unverified"
	}
	reply := replyProjection{message.GetMessageID(), message.GetConversationID(), ""}
	if upstream := message.GetReplyMessage(); upstream != nil {
		if !richID(upstream.GetMessageID()) || upstream.GetConversationID() != message.GetConversationID() {
			return replyProjection{}, nil, receiptProjection{}, "rich_projection_unverified"
		}
		reply.ReplyToMessageID = upstream.GetMessageID()
	}
	reactions, code := projectReactions(message)
	if code != "verified" {
		return replyProjection{}, nil, receiptProjection{}, code
	}
	return reply, reactions, receiptProjection{message.GetMessageID(), message.GetConversationID(), int32(message.GetMessageStatus().GetStatus())}, "verified"
}
func projectReactions(message *gmproto.Message) ([]reactionProjection, string) {
	if len(message.GetReactions()) > reactionLimit {
		return nil, "rich_projection_unverified"
	}
	projections, emojis := make([]reactionProjection, 0, len(message.GetReactions())), map[string]struct{}{}
	for _, reaction := range message.GetReactions() {
		if reaction == nil || reaction.GetData() == nil || !richEmoji(reaction.GetData().GetUnicode()) || !knownEmoji(reaction.GetData().GetType()) || len(reaction.GetParticipantIDs()) == 0 || len(reaction.GetParticipantIDs()) > reactionActorLimit {
			return nil, "rich_projection_unverified"
		}
		if _, duplicate := emojis[reaction.GetData().GetUnicode()]; duplicate {
			return nil, "rich_projection_unverified"
		}
		emojis[reaction.GetData().GetUnicode()] = struct{}{}
		actors := map[string]struct{}{}
		for _, actor := range reaction.GetParticipantIDs() {
			if !richID(actor) {
				return nil, "rich_projection_unverified"
			}
			if _, duplicate := actors[actor]; duplicate {
				return nil, "rich_projection_unverified"
			}
			actors[actor] = struct{}{}
		}
		projections = append(projections, reactionProjection{message.GetMessageID(), message.GetConversationID(), reaction.GetData().GetUnicode(), int32(reaction.GetData().GetType()), len(actors)})
	}
	sort.Slice(projections, func(left, right int) bool { return projections[left].Emoji < projections[right].Emoji })
	return projections, "verified"
}
func richID(value string) bool {
	return value != "" && utf8.ValidString(value) && len(value) <= richIDLimit
}
func richEmoji(value string) bool {
	return value != "" && utf8.ValidString(value) && len(value) <= emojiByteLimit
}
func knownEmoji(value gmproto.EmojiType) bool {
	_, known := gmproto.EmojiType_name[int32(value)]
	return known && value != gmproto.EmojiType_REACTION_TYPE_UNSPECIFIED
}
func knownStatus(value gmproto.MessageStatusType) bool {
	_, known := gmproto.MessageStatusType_name[int32(value)]
	return known && value != gmproto.MessageStatusType_STATUS_UNKNOWN
}
func validRichMessage() *gmproto.Message {
	return &gmproto.Message{MessageID: "message-opaque", ConversationID: "conversation-opaque", ReplyMessage: &gmproto.ReplyMessage{MessageID: "reply-opaque", ConversationID: "conversation-opaque"}, MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_OUTGOING_DELIVERED}, Reactions: []*gmproto.ReactionEntry{{Data: &gmproto.ReactionData{Unicode: "👍", Type: gmproto.EmojiType_LIKE}, ParticipantIDs: []string{"actor-a", "actor-b"}}}}
}
func cloneRich(source *gmproto.Message) *gmproto.Message {
	clone := *source
	clone.Reactions = append([]*gmproto.ReactionEntry(nil), source.Reactions...)
	clone.MessageStatus = &gmproto.MessageStatus{Status: source.MessageStatus.GetStatus()}
	clone.ReplyMessage = &gmproto.ReplyMessage{MessageID: source.ReplyMessage.GetMessageID(), ConversationID: source.ReplyMessage.GetConversationID()}
	clone.Reactions[0] = &gmproto.ReactionEntry{Data: &gmproto.ReactionData{Unicode: source.Reactions[0].GetData().GetUnicode(), Type: source.Reactions[0].GetData().GetType()}, ParticipantIDs: append([]string(nil), source.Reactions[0].GetParticipantIDs()...)}
	return &clone
}
