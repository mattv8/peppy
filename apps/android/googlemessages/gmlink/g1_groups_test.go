package gmlink

import (
	"encoding/json"
	"reflect"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

func TestM4GroupProjectionFiltersSortsAndBoundsParticipants(t *testing.T) {
	conversation := validGroupConversation()
	conversation.Participants = append(conversation.Participants, &gmproto.Participant{IsVisible: false, ID: &gmproto.SmallInfo{ParticipantID: "hidden"}}, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-a"}})
	projection, code := projectGroup(conversation)
	if code != groupResolved || !reflect.DeepEqual(projection.ParticipantIDs, []string{"member-a", "member-b"}) || projection.Type != int32(gmproto.ConversationType_RCS) {
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
		"too_many": func(c *gmproto.Conversation) { c.Participants = tooMany.Participants },
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
			if _, code := projectGroup(candidate); code == groupResolved {
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

func validGroupConversation() *gmproto.Conversation {
	return &gmproto.Conversation{ConversationID: "group-opaque", IsGroupChat: true, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-b"}}, {IsVisible: true, IsMe: true, ID: &gmproto.SmallInfo{ParticipantID: "self"}}}}
}
func cloneGroup(source *gmproto.Conversation) *gmproto.Conversation {
	return proto.Clone(source).(*gmproto.Conversation)
}
func groupRequestFixtures() (resolve, create *gmproto.GetOrCreateConversationRequest) {
	resolve = &gmproto.GetOrCreateConversationRequest{Numbers: []*gmproto.ContactNumber{{Number: "opaque-a"}, {Number: "opaque-b"}}}
	name, createGroup := "fixture", true
	create = &gmproto.GetOrCreateConversationRequest{Numbers: resolve.Numbers, RCSGroupName: &name, CreateRCSGroup: &createGroup}
	return
}
