package gmlink

import (
	"testing"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func productionVerificationLink() *Link {
	link := &Link{
		host: &testHost{}, state: stateLinked, running: true, generation: 7, runGeneration: 7,
		streamOpen: true, streamEpoch: 3, pairingEvidenceID: uuid.NewString(),
		conversationCandidates: make(map[string][]conversationSummary),
		conversationTargets:    make(map[string][]string),
		conversationRecords:    make(map[string]*gmproto.Conversation),
	}
	link.mintRuntimeVerificationLocked()
	return link
}

func TestProductionMintGrantsReceiveIndexSendAndTyping(t *testing.T) {
	link := productionVerificationLink()
	if !link.hasReceiveVerificationLocked() || !link.hasIndexVerificationLocked() || !link.hasSendVerificationLocked() || !link.hasTypingOutboundVerificationLocked() {
		t.Fatalf("unexpected capability set: %#v", link.ownerVerification)
	}
	if !link.hasGroupsVerificationLocked() {
		t.Fatal("production mint did not enable groups")
	}
}

func TestProductionMintRotationRemintsCurrentEpoch(t *testing.T) {
	link := productionVerificationLink()
	previous := link.ownerVerification
	link.streamEpoch++
	link.mintRuntimeVerificationLocked()
	if link.ownerVerification == previous || !link.hasReceiveVerificationLocked() || !link.hasIndexVerificationLocked() || !link.hasSendVerificationLocked() || !link.hasTypingOutboundVerificationLocked() {
		t.Fatal("rotation did not replace current stream capabilities")
	}
}

func TestProductionStreamRotationRemintsTyping(t *testing.T) {
	link := productionVerificationLink()
	previous := link.ownerVerification
	link.queueEventFor(link.generation, &libgm.StreamOpened{Source: libgm.StreamSource{ListenID: 2, PollAttempt: 1}})
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.ownerVerification == previous || link.streamEpoch == 3 || !link.hasTypingOutboundVerificationLocked() {
		t.Fatalf("stream rotation did not re-mint typing: epoch=%d caps=%#v", link.streamEpoch, link.ownerVerification)
	}
}

func TestIndexStaleEmitsOnceForSettingsEpoch(t *testing.T) {
	host := &testHost{}
	link := productionVerificationLink()
	link.host = host
	link.changes = make(chan any, 4)
	settings := &libgm.WrappedSettings{Settings: &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{}}}}
	link.queueEventFor(link.generation, settings)
	link.mu.Lock()
	epoch := link.streamEpoch
	link.mu.Unlock()
	link.queueEventFor(link.generation, settings)
	host.mu.Lock()
	defer host.mu.Unlock()
	count := 0
	for _, event := range host.events {
		if event == (eventCode{kind: "index", code: "stale"}) {
			count++
		}
	}
	if count != 2 || epoch == 0 {
		t.Fatalf("index stale events=%d epoch=%d", count, epoch)
	}
}
