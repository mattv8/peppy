package gmlink

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// rcsTextMessageAt returns a one-to-one RCS text message; timestampMicros is Google's native unit.
func rcsTextMessageAt(id string, timestampMicros int64) *gmproto.Message {
	return &gmproto.Message{
		MessageID: id, ConversationID: "conversation-1", Timestamp: timestampMicros, Type: rcsMessageType,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "synthetic body"}}}},
	}
}

func TestFetchRcsGapWalksBackwardPagesWithGoogleCursors(t *testing.T) {
	pages := []*gmproto.ListMessagesResponse{
		{Messages: []*gmproto.Message{rcsTextMessageAt("m-9", 9_000_000), rcsTextMessageAt("m-8", 8_000_000)}, Cursor: &gmproto.Cursor{LastItemID: "page-1", LastItemTimestamp: 8000}},
		{Messages: []*gmproto.Message{rcsTextMessageAt("m-7", 7_000_000), rcsTextMessageAt("m-6", 6_000_000)}, Cursor: &gmproto.Cursor{LastItemID: "page-2", LastItemTimestamp: 6000}},
		{Messages: []*gmproto.Message{rcsTextMessageAt("m-5", 5_000_000)}, Cursor: &gmproto.Cursor{LastItemID: "page-3", LastItemTimestamp: 5000}},
	}
	var counts []int64
	var cursors []*gmproto.Cursor
	link := fetchGapTestLink(func(_ context.Context, _ string, count int64, cursor *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		counts = append(counts, count)
		cursors = append(cursors, cursor)
		return pages[len(counts)-1], nil
	})

	token := ""
	var ids []string
	for range pages {
		page := decodeGapResult(t, link.FetchRcsGap("conversation-1", token))
		if page.Code != "ok" {
			t.Fatalf("page code = %q", page.Code)
		}
		for _, record := range page.Records {
			ids = append(ids, record.GoogleMessageID)
		}
		token = page.NextCursor
	}

	for index, count := range counts {
		if count != 100 {
			t.Fatalf("request %d count = %d, want 100", index, count)
		}
	}
	if cursors[0] != nil {
		t.Fatalf("first cursor = %#v, want nil", cursors[0])
	}
	if cursors[1].GetLastItemID() != "page-1" || cursors[1].GetLastItemTimestamp() != 8000 {
		t.Fatalf("second cursor = %#v", cursors[1])
	}
	if cursors[2].GetLastItemID() != "page-2" || cursors[2].GetLastItemTimestamp() != 6000 {
		t.Fatalf("third cursor = %#v", cursors[2])
	}
	if strings.Join(ids, ",") != "m-9,m-8,m-7,m-6,m-5" {
		t.Fatalf("record order = %v", ids)
	}
}

func TestRcsPageSkipsUnsupportedRecordsAndCountsThem(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	media := rcsTextMessageAt("m-media", 2_500_000)
	media.MessageInfo = []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{}}}}
	unknownStatus := rcsTextMessageAt("m-unknown-status", 2_400_000)
	unknownStatus.MessageStatus = &gmproto.MessageStatus{Status: gmproto.MessageStatusType(999)}
	nonTextType := rcsTextMessageAt("m-non-text", 2_300_000)
	nonTextType.Type = 2
	page := projectRcsPage(&gmproto.ListMessagesResponse{
		Messages: []*gmproto.Message{
			rcsTextMessageAt("m-first", 3_000_000), media, unknownStatus, nonTextType, rcsTextMessageAt("m-last", 2_000_000),
		},
		Cursor: &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 2000},
	}, conversation)

	if page.Code != "ok" || page.Skipped != 3 || !page.HasMore {
		t.Fatalf("page = %#v", page)
	}
	if len(page.Records) != 2 || page.Records[0].GoogleMessageID != "m-first" || page.Records[1].GoogleMessageID != "m-last" {
		t.Fatalf("records = %#v", page.Records)
	}
	if page.NextCursor != opaquePageToken(&gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 2000}) {
		t.Fatalf("next cursor = %q", page.NextCursor)
	}
	encoded, err := json.Marshal(page)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(encoded), `"skipped":3`) {
		t.Fatalf("envelope = %s", encoded)
	}
}

func TestRcsPageWithOnlySkippedRecordsStillAdvances(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	media := rcsTextMessageAt("m-media", 2_500_000)
	media.MessageInfo = []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{}}}}
	page := projectRcsPage(&gmproto.ListMessagesResponse{
		Messages: []*gmproto.Message{media},
		Cursor:   &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 2000},
	}, conversation)
	if page.Code != "ok" || page.Skipped != 1 || len(page.Records) != 0 || !page.HasMore || page.NextCursor == "" {
		t.Fatalf("page = %#v", page)
	}
}
