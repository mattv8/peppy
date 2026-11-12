package gmlink

import (
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

func syntheticSendTarget() *gmproto.Conversation {
	return &gmproto.Conversation{
		ConversationID:    "synthetic-conversation",
		Type:              gmproto.ConversationType_RCS,
		Status:            gmproto.ConversationStatus_ACTIVE,
		SendMode:          gmproto.ConversationSendMode_SEND_MODE_AUTO,
		DefaultOutgoingID: "synthetic-sim",
		Participants: []*gmproto.Participant{
			{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-self", Number: "+12025550101", Type: gmproto.IdentifierType_PHONE}},
			{IsVisible: true, FormattedNumber: "(202) 555-0123", ID: &gmproto.SmallInfo{ParticipantID: "synthetic-peer", Number: "+12025550123", Type: gmproto.IdentifierType_PHONE}},
		},
	}
}

func TestSendAuthorityIgnoresDisplayState(t *testing.T) {
	indexed, fetched := syntheticSendTarget(), syntheticSendTarget()
	fetched.Name = "Synthetic contact"
	fetched.Unread = true
	fetched.Pinned = true
	fetched.LastMessageTimestamp = 1_700_000_000_000_000
	fetched.LatestMessageID = "synthetic-message"
	fetched.AvatarHexColor = "#000000"
	fetched.UnknownTimestamp = 7
	fetched.Participants[1].FullName = "Synthetic contact"
	fetched.Participants[1].AvatarHexColor = "#ffffff"
	fetched.Participants[0], fetched.Participants[1] = fetched.Participants[1], fetched.Participants[0]

	if !sameSendAuthority(indexed, fetched) {
		t.Fatal("display state changed the send authority")
	}
}

func TestSendAuthorityRejectsEveryRoutingChange(t *testing.T) {
	if sameSendAuthority(syntheticSendTarget(), nil) || sameSendAuthority(nil, nil) {
		t.Fatal("a missing record carried send authority")
	}
	for name, change := range map[string]func(*gmproto.Conversation){
		"conversation id":  func(c *gmproto.Conversation) { c.ConversationID = "synthetic-other" },
		"type":             func(c *gmproto.Conversation) { c.Type = gmproto.ConversationType_SMS },
		"status":           func(c *gmproto.Conversation) { c.Status = gmproto.ConversationStatus_ARCHIVED },
		"send mode":        func(c *gmproto.Conversation) { c.SendMode = gmproto.ConversationSendMode_SEND_MODE_XMS },
		"read only":        func(c *gmproto.Conversation) { c.ReadOnly = true },
		"group flag":       func(c *gmproto.Conversation) { c.IsGroupChat = true },
		"outgoing sim":     func(c *gmproto.Conversation) { c.DefaultOutgoingID = "synthetic-other-sim" },
		"peer number":      func(c *gmproto.Conversation) { c.Participants[1].ID.Number = "+12025550124" },
		"peer id":          func(c *gmproto.Conversation) { c.Participants[1].ID.ParticipantID = "synthetic-other-peer" },
		"identifier type":  func(c *gmproto.Conversation) { c.Participants[1].ID.Type = gmproto.IdentifierType_EMAIL },
		"formatted number": func(c *gmproto.Conversation) { c.Participants[1].FormattedNumber = "(202) 555-0124" },
		"self flag":        func(c *gmproto.Conversation) { c.Participants[1].IsMe = true },
		"visibility":       func(c *gmproto.Conversation) { c.Participants[1].IsVisible = false },
		"added participant": func(c *gmproto.Conversation) {
			c.Participants = append(c.Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-third", Number: "+12025550125"}})
		},
		"removed participant": func(c *gmproto.Conversation) { c.Participants = c.Participants[:1] },
		"duplicated participant": func(c *gmproto.Conversation) {
			c.Participants = append(c.Participants, proto.Clone(c.Participants[1]).(*gmproto.Participant))
		},
	} {
		t.Run(name, func(t *testing.T) {
			changed := syntheticSendTarget()
			change(changed)
			if sameSendAuthority(syntheticSendTarget(), changed) {
				t.Fatal("a routing change kept the send authority")
			}
		})
	}
}

func TestNativeSendBindTargetAcceptsFetchedDisplayStateChange(t *testing.T) {
	link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
	defer cleanup()
	phone.mu.Lock()
	changed := proto.Clone(phone.conversations[0]).(*gmproto.Conversation)
	changed.Unread = !changed.Unread
	changed.LastMessageTimestamp++
	changed.LatestMessageID = "synthetic-newer-message"
	phone.conversations[0] = changed
	phone.mu.Unlock()

	if got := stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))); got != "target_bound" {
		t.Fatalf("bind code with newer display state = %s", got)
	}
}
