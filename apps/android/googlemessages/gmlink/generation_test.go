package gmlink

import (
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestRetiredEventIsRejectedBeforeAndAfterQueueing(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	old := link.generation
	link.revokeLocked()
	link.mu.Unlock()
	link.queueEventFor(old, &events.NoDataReceived{})
	if got := len(link.changes); got != 0 {
		t.Fatalf("retired event queued: %d", got)
	}
	link.changes <- queuedEvent{generation: old, event: &gmproto.Settings{}}
	link.consumeQueuedEvent(<-link.changes)
	if link.settings != nil {
		t.Fatal("retired event mutated settings")
	}
}

func TestLegacySendStaysFailClosed(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	result := decodeSendCode(t, link.SendRcs(`{"command_id":"synthetic-command","tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","mapping_revision":1,"proof_digest":"synthetic-proof","content":"synthetic-content","m2_verification_passed":true}`))
	if result != "pending_prepare" {
		t.Fatalf("legacy send result = %q", result)
	}
}
