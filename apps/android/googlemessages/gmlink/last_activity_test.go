package gmlink

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// Google reports LastMessageTimestamp in microseconds; the export must report milliseconds.
const lastActivityMicros int64 = 1735689600123456
const lastActivityMillis int64 = 1735689600123

func lastActivityConversation(id string, lastMessageMicros int64) *gmproto.Conversation {
	conversation := stage2Conversation(id, stage2Participant("+12025550123", true, false))
	conversation.LastMessageTimestamp = lastMessageMicros
	return conversation
}

type lastActivityResult struct {
	Code          string           `json:"code"`
	Complete      bool             `json:"complete"`
	LastMessageMS map[string]int64 `json:"last_message_ms"`
}

func decodeLastActivity(t *testing.T, encoded string) lastActivityResult {
	t.Helper()
	var result lastActivityResult
	if err := json.Unmarshal([]byte(encoded), &result); err != nil {
		t.Fatalf("decode %q: %v", encoded, err)
	}
	return result
}

func TestCandidateSnapshotCarriesLastMessageMillis(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link,
		lastActivityConversation("active", lastActivityMicros),
		stage2Conversation("never-messaged", stage2Participant("+12025550124", true, false)),
	)
	var snapshot struct {
		Conversations []struct {
			GoogleConversationID string `json:"google_conversation_id"`
			LastMessageMS        int64  `json:"last_message_ms"`
		} `json:"conversations"`
	}
	if err := json.Unmarshal([]byte(link.ConversationCandidateSnapshot()), &snapshot); err != nil {
		t.Fatal(err)
	}
	got := map[string]int64{}
	for _, record := range snapshot.Conversations {
		got[record.GoogleConversationID] = record.LastMessageMS
	}
	if got["active"] != lastActivityMillis || got["never-messaged"] != 0 {
		t.Fatalf("snapshot last_message_ms = %v", got)
	}
}

func TestConversationLastActivityExportsMillisFromCompleteIndex(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link,
		lastActivityConversation("active", lastActivityMicros),
		stage2Conversation("never-messaged", stage2Participant("+12025550124", true, false)),
	)
	result := decodeLastActivity(t, link.ConversationLastActivity(`["active","never-messaged","not-indexed"]`))
	if result.Code != "ok" || !result.Complete {
		t.Fatalf("result = %+v", result)
	}
	if len(result.LastMessageMS) != 2 || result.LastMessageMS["active"] != lastActivityMillis || result.LastMessageMS["never-messaged"] != 0 {
		t.Fatalf("last_message_ms = %v", result.LastMessageMS)
	}
	if _, indexed := result.LastMessageMS["not-indexed"]; indexed {
		t.Fatalf("unindexed conversation reported: %v", result.LastMessageMS)
	}
}

func TestConversationLastActivityRefusesIncompleteIndex(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	generation, streamEpoch := stage2SeedCompleteIndex(t, link, lastActivityConversation("active", lastActivityMicros))
	if _, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch); !ok {
		t.Fatal("refresh did not start")
	}
	want := `{"code":"conversation_last_activity_unavailable","complete":false,"last_message_ms":{}}`
	if got := link.ConversationLastActivity(`["active"]`); got != want {
		t.Fatalf("incomplete export = %s", got)
	}
}

func TestConversationLastActivityBoundsInput(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link, lastActivityConversation("active", lastActivityMicros))

	ids := make([]string, maxLastActivityIDs+1)
	for index := range ids {
		ids[index] = fmt.Sprintf("conversation-%d", index)
	}
	oversized, _ := json.Marshal(ids)
	if result := decodeLastActivity(t, link.ConversationLastActivity(string(oversized))); result.Code != "invalid_request" || result.Complete {
		t.Fatalf("oversized result = %+v", result)
	}
	for _, malformed := range []string{"", "not-json", `[""]`, `{"id":1}`} {
		if result := decodeLastActivity(t, link.ConversationLastActivity(malformed)); result.Code != "invalid_request" {
			t.Fatalf("malformed %q result = %+v", malformed, result)
		}
	}
	atLimit := append([]string{"active"}, ids[:maxLastActivityIDs-1]...)
	bounded, _ := json.Marshal(atLimit)
	if result := decodeLastActivity(t, link.ConversationLastActivity(string(bounded))); result.Code != "ok" || result.LastMessageMS["active"] != lastActivityMillis {
		t.Fatalf("limit result = %+v", result)
	}
	if strings.Contains(link.ConversationLastActivity(string(bounded)), "conversation-0") {
		t.Fatal("unindexed ids leaked into the export")
	}
}
