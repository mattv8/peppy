package gmlink

import (
	"encoding/json"
	"reflect"
	"sort"
	"testing"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	groupParticipantLimit = 100
	groupStringLimit      = 256
)

type groupProjection struct {
	ConversationID string
	Type           int32
	Status         int32
	ReadOnly       bool
	SendMode       int32
	ParticipantIDs []string
}

func TestM4GroupProjectionFiltersSortsAndBoundsParticipants(t *testing.T) {
	conversation := validGroupConversation()
	conversation.Participants = append(conversation.Participants, &gmproto.Participant{IsVisible: false, ID: &gmproto.SmallInfo{ParticipantID: "hidden"}}, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-a"}})
	projection, code := projectGroup(conversation)
	if code != "verified" || !reflect.DeepEqual(projection.ParticipantIDs, []string{"member-a", "member-b"}) || projection.Type != int32(gmproto.ConversationType_RCS) {
		t.Fatalf("projection=%#v code=%q", projection, code)
	}
}

func TestM4GroupProjectionFailsClosed(t *testing.T) {
	tooMany := validGroupConversation()
	for index := 0; index < groupParticipantLimit; index++ {
		tooMany.Participants = append(tooMany.Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: string(rune(index + 33))}})
	}
	for name, mutate := range map[string]func(*gmproto.Conversation){
		"unknown_type":        func(c *gmproto.Conversation) { c.Type = gmproto.ConversationType_UNKNOWN_CONVERSATION_TYPE },
		"unrecognized_type":   func(c *gmproto.Conversation) { c.Type = gmproto.ConversationType(99) },
		"unknown_status":      func(c *gmproto.Conversation) { c.Status = gmproto.ConversationStatus_UNKNOWN_CONVERSATION_STATUS },
		"unrecognized_status": func(c *gmproto.Conversation) { c.Status = gmproto.ConversationStatus(99) },
		"missing_self":        func(c *gmproto.Conversation) { c.Participants = c.Participants[:1] },
		"duplicate": func(c *gmproto.Conversation) {
			c.Participants = append(c.Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-b"}})
		},
		"too_many": func(c *gmproto.Conversation) { *c = *tooMany },
		"long_id":  func(c *gmproto.Conversation) { c.ConversationID = string(make([]byte, groupStringLimit+1)) },
		"long_participant": func(c *gmproto.Conversation) {
			c.Participants[0].ID.ParticipantID = string(make([]byte, groupStringLimit+1))
		},
		"invalid_utf8": func(c *gmproto.Conversation) { c.ConversationID = string([]byte{0xff}) },
		"self_only":    func(c *gmproto.Conversation) { c.Participants = c.Participants[1:] },
	} {
		t.Run(name, func(t *testing.T) {
			candidate := cloneGroup(validGroupConversation())
			mutate(candidate)
			if _, code := projectGroup(candidate); code == "verified" {
				t.Fatal("accepted unsafe group")
			}
		})
	}
}

func TestM4RequestShapesAndActualDrainPath(t *testing.T) {
	resolve, create := groupRequestFixtures()
	resolveFields := resolve.ProtoReflect().Descriptor().Fields()
	createFields := create.ProtoReflect().Descriptor().Fields()
	if resolve.ProtoReflect().Has(resolveFields.ByName("createRCSGroup")) || resolve.ProtoReflect().Has(resolveFields.ByName("RCSGroupName")) || !create.GetCreateRCSGroup() || !create.ProtoReflect().Has(createFields.ByName("createRCSGroup")) || !create.ProtoReflect().Has(createFields.ByName("RCSGroupName")) {
		t.Fatal("request shapes changed")
	}
	link, _ := openSafetyLink(t, nil)
	setTestOwnerVerification(link)
	link.projectEvent(validGroupConversation())
	var events []map[string]string
	if err := json.Unmarshal([]byte(link.DrainConversationEvents()), &events); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(events, []map[string]string{{"code": "conversation_updated", "google_conversation_id": "group-opaque"}}) {
		t.Fatalf("drain=%#v", events)
	}
	eventType := reflect.TypeOf(conversationEvent{})
	if eventType.NumField() != 2 || eventType.Field(0).Tag.Get("json") != "code" || eventType.Field(1).Tag.Get("json") != "google_conversation_id" {
		t.Fatal("event is no longer ID-only")
	}
}

func projectGroup(c *gmproto.Conversation) (groupProjection, string) {
	if c == nil || !c.GetIsGroupChat() || !boundedGroupString(c.GetConversationID()) || c.GetType() == gmproto.ConversationType_UNKNOWN_CONVERSATION_TYPE || c.GetStatus() == gmproto.ConversationStatus_UNKNOWN_CONVERSATION_STATUS || !knownGroupType(c.GetType()) || !knownGroupStatus(c.GetStatus()) || !knownGroupSendMode(c.GetSendMode()) {
		return groupProjection{}, "group_projection_unverified"
	}
	ids, seen, selfCount := make([]string, 0, len(c.GetParticipants())), map[string]struct{}{}, 0
	for _, participant := range c.GetParticipants() {
		if participant == nil || !participant.GetIsVisible() {
			continue
		}
		if participant.GetIsMe() {
			selfCount++
			continue
		}
		id := participant.GetID().GetParticipantID()
		if !boundedGroupString(id) {
			return groupProjection{}, "group_projection_unverified"
		}
		if _, duplicate := seen[id]; duplicate {
			return groupProjection{}, "group_projection_unverified"
		}
		seen[id] = struct{}{}
		ids = append(ids, id)
		if len(ids) > groupParticipantLimit {
			return groupProjection{}, "group_projection_unverified"
		}
	}
	if len(ids) == 0 || selfCount != 1 {
		return groupProjection{}, "group_projection_unverified"
	}
	sort.Strings(ids)
	return groupProjection{c.GetConversationID(), int32(c.GetType()), int32(c.GetStatus()), c.GetReadOnly(), int32(c.GetSendMode()), ids}, "verified"
}
func boundedGroupString(value string) bool {
	return value != "" && utf8.ValidString(value) && len(value) <= groupStringLimit
}
func knownGroupType(value gmproto.ConversationType) bool {
	_, ok := gmproto.ConversationType_name[int32(value)]
	return ok
}
func knownGroupStatus(value gmproto.ConversationStatus) bool {
	_, ok := gmproto.ConversationStatus_name[int32(value)]
	return ok
}
func knownGroupSendMode(value gmproto.ConversationSendMode) bool {
	_, ok := gmproto.ConversationSendMode_name[int32(value)]
	return ok
}
func validGroupConversation() *gmproto.Conversation {
	return &gmproto.Conversation{ConversationID: "group-opaque", IsGroupChat: true, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-b"}}, {IsVisible: true, IsMe: true, ID: &gmproto.SmallInfo{ParticipantID: "self"}}}}
}
func cloneGroup(source *gmproto.Conversation) *gmproto.Conversation {
	clone := *source
	clone.Participants = append([]*gmproto.Participant(nil), source.Participants...)
	return &clone
}
func groupRequestFixtures() (resolve, create *gmproto.GetOrCreateConversationRequest) {
	resolve = &gmproto.GetOrCreateConversationRequest{Numbers: []*gmproto.ContactNumber{{Number: "opaque-a"}, {Number: "opaque-b"}}}
	name, createGroup := "fixture", true
	create = &gmproto.GetOrCreateConversationRequest{Numbers: resolve.Numbers, RCSGroupName: &name, CreateRCSGroup: &createGroup}
	return
}
