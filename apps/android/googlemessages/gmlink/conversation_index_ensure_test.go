package gmlink

import (
	"testing"
	"time"
)

func TestEnsureConversationIndexKeepsACurrentCompleteIndex(t *testing.T) {
	link, _ := sendReadyIndexedLink(t, syntheticRcsChat("synthetic-chat", "+12025550123"))
	link.mu.Lock()
	revision := link.indexRevision
	link.mu.Unlock()

	// The link has no runtime client, so any rescan would report an unavailable link.
	if result := link.EnsureConversationIndex(); result != `{"code":"ok"}` {
		t.Fatalf("EnsureConversationIndex = %s", result)
	}

	link.mu.Lock()
	defer link.mu.Unlock()
	if !link.conversationIndexComplete || link.indexRevision != revision {
		t.Fatalf("index complete = %v, revision %d -> %d", link.conversationIndexComplete, revision, link.indexRevision)
	}
}

func TestEnsureConversationIndexRescansWhenTheIndexIsNotUsable(t *testing.T) {
	cases := map[string]func(*Link, *time.Time){
		"incomplete":     func(link *Link, _ *time.Time) { link.invalidateConversationCompletenessLocked() },
		"another stream": func(link *Link, _ *time.Time) { link.streamEpoch++ },
		"pending update": func(link *Link, _ *time.Time) { link.pendingConversationEvents = 1 },
		"too old":        func(_ *Link, now *time.Time) { *now = now.Add(conversationIndexMaxAge) },
	}
	for name, change := range cases {
		t.Run(name, func(t *testing.T) {
			now := time.Now()
			link, _ := sendReadyIndexedLink(t)
			link.mu.Lock()
			link.ticketNow = func() time.Time { return now }
			link.conversationIndexScannedAt = now
			change(link, &now)
			link.mu.Unlock()

			if result := link.EnsureConversationIndex(); result == `{"code":"ok"}` {
				t.Fatal("an unusable index was reported ready without a rescan")
			}
		})
	}
}
