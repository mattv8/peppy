package gmlink

import (
	"reflect"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestM4GroupProjectionFiltersSortsAndBoundsParticipants(t *testing.T) {
	conversation := &gmproto.Conversation{
		ConversationID: "group-opaque",
		Name:           "group-label",
		IsGroupChat:    true,
		Type:           gmproto.ConversationType_RCS,
		Status:         gmproto.ConversationStatus_ACTIVE,
		SendMode:       gmproto.ConversationSendMode_SEND_MODE_AUTO,
		Participants: []*gmproto.Participant{
			{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-b"}},
			{IsVisible: false, ID: &gmproto.SmallInfo{ParticipantID: "hidden"}},
			{IsVisible: true, IsMe: true, ID: &gmproto.SmallInfo{ParticipantID: "self"}},
			{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "member-a"}},
		},
	}
	projection, code := projectM4Group(conversation)
	if code != "verified" {
		t.Fatalf("code = %q", code)
	}
	if !reflect.DeepEqual(projection.ParticipantIDs, []string{"member-a", "member-b"}) {
		t.Fatalf("participants = %#v", projection.ParticipantIDs)
	}
	if projection.Name != "group-label" || projection.ReadOnly || projection.Type != int32(gmproto.ConversationType_RCS) {
		t.Fatalf("projection = %#v", projection)
	}
}

func TestM4GroupProjectionFailsClosed(t *testing.T) {
	for name, conversation := range map[string]*gmproto.Conversation{
		"not_group":               {ConversationID: "id", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE},
		"unknown_type":            {ConversationID: "id", IsGroupChat: true, Status: gmproto.ConversationStatus_ACTIVE},
		"missing_member_identity": {ConversationID: "id", IsGroupChat: true, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true}}},
	} {
		t.Run(name, func(t *testing.T) {
			if _, code := projectM4Group(conversation); code == "verified" {
				t.Fatal("accepted ambiguous conversation")
			}
		})
	}
}

func TestM4RequestShapesAndEventLimitations(t *testing.T) {
	resolve, create := m4GroupRequestFixtures()
	if resolve.GetCreateRCSGroup() || resolve.GetRCSGroupName() != "" || resolve.ProtoReflect().Has(resolve.ProtoReflect().Descriptor().Fields().ByName("createRCSGroup")) {
		t.Fatal("resolve fixture must omit create-only fields")
	}
	if !create.GetCreateRCSGroup() || create.GetRCSGroupName() == "" {
		t.Fatal("create fixture must set create-only fields")
	}
	if got := m4ConversationEventFixture(); got.Code != "conversation_updated" || got.GoogleConversationID == "" {
		t.Fatalf("event fixture = %#v", got)
	}
	if fields := reflect.TypeOf(conversationEvent{}).NumField(); fields != 2 {
		t.Fatalf("conversation events must remain ID-only, fields = %d", fields)
	}
}
