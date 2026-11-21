package gmlink

import (
	"reflect"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

type typingDiagnosticHost struct {
	ownedRecordHost
	events []eventCode
	order  []string
}

func (host *typingDiagnosticHost) Event(kind, code string, value, _, _ int64) {
	if value != 0 {
		panic("typing diagnostic value is not zero")
	}
	host.events = append(host.events, eventCode{kind, code})
	host.order = append(host.order, code)
}

func (host *typingDiagnosticHost) TypingEvent(record string, generation, revision, streamEpoch int64) {
	host.ownedRecordHost.TypingEvent(record, generation, revision, streamEpoch)
	host.order = append(host.order, "record")
}

func TestTypingUnknownTypeIgnored(t *testing.T) {
	data := typingData()
	data.Type = gmproto.TypingTypes(999)
	result := projectTypingData(data, false, []*gmproto.Conversation{typingConversation(data.ConversationID, true)})
	if result.Code != "typing_ignored" || result.Record != nil {
		t.Fatalf("projection = %+v", result)
	}
}

func TestTypingDiagnosticEvents(t *testing.T) {
	host := &typingDiagnosticHost{}
	link := typingLink(host, typingConversation("synthetic-conversation", true))
	queued := queuedEvent{generation: 7, streamEpoch: 3}
	data := typingData()
	link.projectTypingEventFor(queued, data)
	data.Type = gmproto.TypingTypes_STOPPED_TYPING
	link.projectTypingEventFor(queued, data)
	data.Type = gmproto.TypingTypes(999)
	link.projectTypingEventFor(queued, data)
	data.ConversationID = "unknown"
	link.projectTypingEventFor(queued, data)
	data.ConversationID = ""
	link.projectTypingEventFor(queued, data)
	queued.generation--
	link.projectTypingEventFor(queued, data)
	want := []eventCode{{"typing", "burst_started"}, {"typing", "burst_stopped"}, {"typing", "projection_ignored"}, {"typing", "conversation_unknown"}, {"typing", "conversation_unknown"}}
	if !reflect.DeepEqual(host.events, want) {
		t.Fatalf("events = %v, want %v", host.events, want)
	}
	if len(host.typingRecords) != 2 || host.typingRecords[0] != `{"google_conversation_id":"synthetic-conversation","state":"started","participant":"synthetic-participant"}` || host.typingRecords[1] != `{"google_conversation_id":"synthetic-conversation","state":"stopped","participant":"synthetic-participant"}` {
		t.Fatalf("typing records = %v", host.typingRecords)
	}
	wantOrder := []string{"record", "burst_started", "record", "burst_stopped", "projection_ignored", "conversation_unknown", "conversation_unknown"}
	if !reflect.DeepEqual(host.order, wantOrder) {
		t.Fatalf("output order = %v, want %v", host.order, wantOrder)
	}
}

func TestTypingDiagnosticEarlyReturnsSilent(t *testing.T) {
	for _, test := range []struct {
		name   string
		change func(*Link, *queuedEvent)
	}{
		{"closed", func(link *Link, _ *queuedEvent) { link.closed = true }},
		{"old generation", func(_ *Link, queued *queuedEvent) { queued.generation-- }},
		{"old stream", func(_ *Link, queued *queuedEvent) { queued.streamEpoch-- }},
		{"unverified", func(link *Link, _ *queuedEvent) { link.ownerVerification = nil }},
	} {
		t.Run(test.name, func(t *testing.T) {
			host := &typingDiagnosticHost{}
			link := typingLink(host, typingConversation("synthetic-conversation", true))
			queued := queuedEvent{generation: 7, streamEpoch: 3}
			test.change(link, &queued)
			link.projectTypingEventFor(queued, typingData())
			if len(host.events) != 0 || len(host.typingRecords) != 0 {
				t.Fatalf("events = %v, records = %v", host.events, host.typingRecords)
			}
		})
	}
}

func TestTypingSettingsReportedAgainAfterRunReset(t *testing.T) {
	host := &typingDiagnosticHost{}
	link := typingLink(host, typingConversation("synthetic-conversation", true))
	settings := typingSettings()
	settings.RCSSettings = &gmproto.RCSSettings{}
	event := &libgm.WrappedSettings{Settings: settings}
	link.recordSettingsFor(7, libgm.StreamSource{}, 3, time.Now(), event)
	link.mu.Lock()
	link.typingIndicators = nil // Run resets the remembered setting under the same lock.
	link.mu.Unlock()
	link.recordSettingsFor(7, libgm.StreamSource{}, 3, time.Now(), event)
	want := []eventCode{{"typing", "indicators_off"}, {"typing", "indicators_off"}}
	if !reflect.DeepEqual(host.events, want) {
		t.Fatalf("events = %v, want %v", host.events, want)
	}
}

func TestTypingSettingsDiagnostics(t *testing.T) {
	host := &typingDiagnosticHost{}
	link := typingLink(host, typingConversation("synthetic-conversation", true))
	settings := typingSettings()
	settings.RCSSettings = &gmproto.RCSSettings{}
	event := &libgm.WrappedSettings{Settings: settings}
	link.recordSettingsFor(7, libgm.StreamSource{}, 3, time.Now(), event)
	link.recordSettingsFor(7, libgm.StreamSource{}, 3, time.Now(), event)
	settings.RCSSettings.ShowTypingIndicators = true
	link.recordSettingsFor(7, libgm.StreamSource{}, 3, time.Now(), event)
	event.IsOld = true
	settings.RCSSettings.ShowTypingIndicators = false
	link.recordSettingsFor(7, libgm.StreamSource{}, 3, time.Now(), event)
	event.IsOld = false
	link.recordSettingsFor(6, libgm.StreamSource{}, 3, time.Now(), event)
	want := []eventCode{{"typing", "indicators_off"}, {"typing", "indicators_on"}}
	if !reflect.DeepEqual(host.events, want) {
		t.Fatalf("events = %v, want %v", host.events, want)
	}
}
