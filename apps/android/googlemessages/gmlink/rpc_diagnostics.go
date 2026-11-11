package gmlink

import (
	"context"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func (link *Link) observeRPCDiagnostics(client *libgm.Client, owner uint64) {
	client.SetRPCDiagnosticObserver(func(stage libgm.RPCDiagnosticStage) {
		if code := rpcDiagnosticCode(stage); code != "" {
			link.emitFor(owner, "conversation_rpc", code, 0)
		}
	})
}

func (link *Link) loadConversationPageWithDiagnostics(ctx context.Context, owner uint64, request *gmproto.ListConversationsRequest, load conversationPageLoader) (*gmproto.ListConversationsResponse, error) {
	folder := conversationScanFolderCode(request.GetFolder())
	if folder != "" {
		link.emitFor(owner, "conversation_scan", folder+"_start", 0)
	}
	response, err := load(ctx, request)
	if folder != "" {
		outcome := "ok"
		if err != nil || response == nil {
			outcome = "error"
		}
		link.emitFor(owner, "conversation_scan", folder+"_"+outcome, 0)
	}
	return response, err
}

func conversationScanFolderCode(folder gmproto.ListConversationsRequest_Folder) string {
	switch folder {
	case gmproto.ListConversationsRequest_INBOX:
		return "inbox"
	case gmproto.ListConversationsRequest_ARCHIVE:
		return "archive"
	case gmproto.ListConversationsRequest_SPAM_BLOCKED:
		return "spam"
	default:
		return ""
	}
}

func rpcDiagnosticCode(stage libgm.RPCDiagnosticStage) string {
	switch stage {
	case libgm.RPCDiagnosticListRequestAnnotation:
		return "list_annotation"
	case libgm.RPCDiagnosticListRequestNormal:
		return "list_normal"
	case libgm.RPCDiagnosticListHTTPAccepted:
		return "list_http_accepted"
	case libgm.RPCDiagnosticListHTTPFailed:
		return "list_http_failed"
	case libgm.RPCDiagnosticListUnencryptedIgnored:
		return "list_unencrypted_ignored"
	case libgm.RPCDiagnosticListWaiterMatched:
		return "list_waiter_matched"
	case libgm.RPCDiagnosticListWaiterUnmatched:
		return "list_waiter_unmatched"
	case libgm.RPCDiagnosticListSessionMissing:
		return "list_session_missing"
	case libgm.RPCDiagnosticDecryptFailed:
		return "rpc_decrypt_failed"
	default:
		return ""
	}
}
