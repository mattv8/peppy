package gmlink

import (
	"context"
	"errors"
	"reflect"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestConversationPageDiagnosticsPreserveResultsAndUseClosedCodes(t *testing.T) {
	for _, folder := range []gmproto.ListConversationsRequest_Folder{
		gmproto.ListConversationsRequest_INBOX, gmproto.ListConversationsRequest_ARCHIVE, gmproto.ListConversationsRequest_SPAM_BLOCKED,
	} {
		for _, failed := range []bool{false, true} {
			host := &testHost{}
			link, err := Open(nil, host)
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(link.Close)
			request := &gmproto.ListConversationsRequest{Folder: folder, Count: 25, Cursor: &gmproto.Cursor{LastItemID: "private-cursor"}}
			response := &gmproto.ListConversationsResponse{}
			var loadError error
			if failed {
				loadError = errors.New("private-rpc-error")
			}
			got, gotError := link.loadConversationPageWithDiagnostics(context.Background(), link.generation, request, func(_ context.Context, received *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
				if received != request {
					t.Fatal("request was changed by diagnostics")
				}
				return response, loadError
			})
			if got != response || gotError != loadError {
				t.Fatal("RPC result was changed by diagnostics")
			}
			awaitSafety(t, "page diagnostics", func() bool {
				host.mu.Lock()
				defer host.mu.Unlock()
				return len(host.events) == 2
			})
			name := map[gmproto.ListConversationsRequest_Folder]string{gmproto.ListConversationsRequest_INBOX: "inbox", gmproto.ListConversationsRequest_ARCHIVE: "archive", gmproto.ListConversationsRequest_SPAM_BLOCKED: "spam"}[folder]
			outcome := "ok"
			if failed {
				outcome = "error"
			}
			host.mu.Lock()
			actual := append([]eventCode(nil), host.events...)
			host.mu.Unlock()
			expected := []eventCode{{"conversation_scan", name + "_start"}, {"conversation_scan", name + "_" + outcome}}
			if !reflect.DeepEqual(actual, expected) {
				t.Fatalf("diagnostics = %v, want %v", actual, expected)
			}
			link.Close()
		}
	}
}

func TestRPCDiagnosticStagesUseOnlyTheClosedVocabulary(t *testing.T) {
	want := []string{
		"list_annotation", "list_normal", "list_http_accepted", "list_http_failed", "list_unencrypted_ignored",
		"list_waiter_matched", "list_waiter_unmatched", "list_session_missing", "rpc_decrypt_failed",
	}
	for index, code := range want {
		if got := rpcDiagnosticCode(libgm.RPCDiagnosticStage(index + 1)); got != code || !isEventCode("conversation_rpc", got) {
			t.Fatalf("stage %d = %q, want closed code %q", index+1, got, code)
		}
	}
	for _, unknown := range []libgm.RPCDiagnosticStage{0, -1, 1000} {
		if rpcDiagnosticCode(unknown) != "" {
			t.Fatal("unknown diagnostic stage was admitted")
		}
	}
}

func TestRPCObserverIsInstalledOnPairingAndRuntimeClients(t *testing.T) {
	for _, runtime := range []bool{false, true} {
		host := &testHost{}
		link, err := Open(nil, host)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(link.Close)
		var client *libgm.Client
		if runtime {
			client = link.newRuntimeClient(libgm.NewAuthData(), link.generation)
		} else {
			client = link.newClient(libgm.NewAuthData(), func() error { return nil }, link.generation)
		}
		ctx, cancel := context.WithCancel(context.Background())
		cancel() // Exercise the real RPC entry without permitting a network request.
		if _, err := client.ListConversations(ctx, &gmproto.ListConversationsRequest{Count: 1, Folder: gmproto.ListConversationsRequest_INBOX}); err == nil {
			t.Fatal("cancelled request unexpectedly succeeded")
		}
		awaitSafety(t, "client RPC observer", func() bool {
			host.mu.Lock()
			defer host.mu.Unlock()
			for _, event := range host.events {
				if event == (eventCode{"conversation_rpc", "list_annotation"}) {
					return true
				}
			}
			return false
		})
		client.Disconnect()
		link.Close()
	}
}
