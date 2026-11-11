package gmlink

import (
	"context"
	"fmt"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestConversationScanTimeoutPolicy(t *testing.T) {
	if conversationTimeout != 30*time.Second {
		t.Fatalf("targeted lookup timeout = %v, want 30s", conversationTimeout)
	}
	if conversationPageTimeout != 30*time.Second {
		t.Fatalf("page timeout = %v, want 30s", conversationPageTimeout)
	}
	if conversationScanTimeout != 5*time.Minute {
		t.Fatalf("scan timeout = %v, want 5m", conversationScanTimeout)
	}
}

func TestConversationScanUsesGlobalPageBudgetForTwentyOnePageInbox(t *testing.T) {
	var requests []*gmproto.ListConversationsRequest
	conversations, code, diagnostic := scanConversationPagesWithDiagnostic(context.Background(), func(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		requests = append(requests, request)
		if request.GetCount() != conversationPageSize {
			t.Fatalf("page count = %d, want %d", request.GetCount(), conversationPageSize)
		}
		if request.GetFolder() != gmproto.ListConversationsRequest_INBOX {
			return &gmproto.ListConversationsResponse{}, nil
		}
		page := len(requests)
		response := &gmproto.ListConversationsResponse{Conversations: []*gmproto.Conversation{stage2Conversation(fmt.Sprintf("inbox-%d", page), stage2Participant(fmt.Sprintf("target-%d", page), true, false))}}
		if page < 21 {
			response.Cursor = &gmproto.Cursor{LastItemID: fmt.Sprintf("cursor-%d", page), LastItemTimestamp: int64(page)}
		}
		return response, nil
	})
	if code != "" || diagnostic != "" {
		t.Fatalf("scan = (%q, %q)", code, diagnostic)
	}
	if len(conversations) != 21 {
		t.Fatalf("conversation count = %d, want 21", len(conversations))
	}
	if len(requests) != 23 {
		t.Fatalf("request count = %d, want 23", len(requests))
	}
	if requests[21].GetFolder() != gmproto.ListConversationsRequest_ARCHIVE || requests[22].GetFolder() != gmproto.ListConversationsRequest_SPAM_BLOCKED {
		t.Fatalf("terminal folders = (%v, %v), want archive then spam", requests[21].GetFolder(), requests[22].GetFolder())
	}
}

func TestConversationScanStopsAtGlobalPageBudget(t *testing.T) {
	calls := 0
	_, code, diagnostic := scanConversationPagesWithDiagnostic(context.Background(), func(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		return &gmproto.ListConversationsResponse{Cursor: &gmproto.Cursor{LastItemID: fmt.Sprintf("cursor-%d", calls), LastItemTimestamp: int64(calls)}}, nil
	})
	if code != "conversation_lookup_incomplete" || diagnostic != "page_limit" {
		t.Fatalf("scan = (%q, %q), want incomplete/page_limit", code, diagnostic)
	}
	if calls != 60 {
		t.Fatalf("calls = %d, want 60", calls)
	}
}

func TestConversationScanRefusesLaterFolderAfterTerminalBudgetPage(t *testing.T) {
	calls := 0
	_, code, diagnostic := scanConversationPagesWithDiagnostic(context.Background(), func(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		if calls < 59 {
			return &gmproto.ListConversationsResponse{Cursor: &gmproto.Cursor{LastItemID: fmt.Sprintf("cursor-%d", calls), LastItemTimestamp: int64(calls)}}, nil
		}
		if calls == 59 {
			if request.GetFolder() != gmproto.ListConversationsRequest_INBOX {
				t.Fatalf("59th request folder = %v, want inbox", request.GetFolder())
			}
			return &gmproto.ListConversationsResponse{}, nil
		}
		if request.GetFolder() != gmproto.ListConversationsRequest_ARCHIVE {
			t.Fatalf("60th request folder = %v, want archive", request.GetFolder())
		}
		return &gmproto.ListConversationsResponse{}, nil
	})
	if code != "conversation_lookup_incomplete" || diagnostic != "page_limit" {
		t.Fatalf("scan = (%q, %q), want incomplete/page_limit", code, diagnostic)
	}
	if calls != 60 {
		t.Fatalf("calls = %d, want 60", calls)
	}
}

func TestConversationScanPermitsTerminalLastBudgetPage(t *testing.T) {
	calls := 0
	conversations, code, diagnostic := scanConversationPagesWithDiagnostic(context.Background(), func(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		if calls < 58 {
			return &gmproto.ListConversationsResponse{Cursor: &gmproto.Cursor{LastItemID: fmt.Sprintf("cursor-%d", calls), LastItemTimestamp: int64(calls)}}, nil
		}
		if calls == 58 && request.GetFolder() != gmproto.ListConversationsRequest_INBOX {
			t.Fatalf("58th request folder = %v, want inbox", request.GetFolder())
		}
		return &gmproto.ListConversationsResponse{}, nil
	})
	if code != "" || diagnostic != "" || conversations == nil {
		t.Fatalf("scan = (%+v, %q, %q)", conversations, code, diagnostic)
	}
	if calls != 60 {
		t.Fatalf("calls = %d, want 60", calls)
	}
}

func TestConversationScanCancelsEachPageContextAfterLoad(t *testing.T) {
	var pageCtx context.Context
	_, code, diagnostic := scanConversationPagesWithDiagnostic(context.Background(), func(ctx context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		pageCtx = ctx
		deadline, ok := ctx.Deadline()
		const wantPageTimeout = 30 * time.Second
		if !ok || time.Until(deadline) > wantPageTimeout || time.Until(deadline) < wantPageTimeout-time.Second {
			t.Fatalf("page deadline = %v, want approximately %v", deadline, wantPageTimeout)
		}
		return &gmproto.ListConversationsResponse{}, nil
	})
	if code != "" || diagnostic != "" {
		t.Fatalf("scan = (%q, %q)", code, diagnostic)
	}
	if pageCtx == nil || pageCtx.Err() != context.Canceled {
		t.Fatalf("page context error = %v, want cancelled", pageCtx.Err())
	}
}

func TestConversationScanRejectsCancellationReturnedByLoader(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	calls := 0
	_, code, diagnostic := scanConversationPagesWithDiagnostic(ctx, func(_ context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		cancel()
		return &gmproto.ListConversationsResponse{}, nil
	})
	defer cancel()
	if code != "conversation_lookup_failed" || diagnostic != "cancelled" {
		t.Fatalf("scan = (%q, %q), want failed/cancelled", code, diagnostic)
	}
	if calls != 1 {
		t.Fatalf("calls = %d, want 1", calls)
	}
}

func TestConversationScanMakesNoRequestForCancelledParent(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	calls := 0
	_, code, diagnostic := scanConversationPagesWithDiagnostic(ctx, func(_ context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		return &gmproto.ListConversationsResponse{}, nil
	})
	if code != "conversation_lookup_failed" || diagnostic != "cancelled" {
		t.Fatalf("scan = (%q, %q), want failed/cancelled", code, diagnostic)
	}
	if calls != 0 {
		t.Fatalf("calls = %d, want 0", calls)
	}
}

func TestConversationScanCapsPageDeadlineByParent(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	parentDeadline, _ := ctx.Deadline()
	_, code, diagnostic := scanConversationPagesWithDiagnostic(ctx, func(pageCtx context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		pageDeadline, ok := pageCtx.Deadline()
		if !ok || pageDeadline.After(parentDeadline) {
			t.Fatalf("page deadline = %v, parent deadline = %v", pageDeadline, parentDeadline)
		}
		cancel()
		return &gmproto.ListConversationsResponse{}, nil
	})
	if code != "conversation_lookup_failed" || diagnostic != "cancelled" {
		t.Fatalf("scan = (%q, %q), want failed/cancelled", code, diagnostic)
	}
}
