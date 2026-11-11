package gmlink

import (
	"context"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestOpaqueCursorBytesNeverProveCompleteConversationAbsence(t *testing.T) {
	calls := 0
	conversations, code, diagnostic := scanConversationPagesWithDiagnostic(context.Background(), func(context.Context, *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		return &gmproto.ListConversationsResponse{CursorBytes: []byte("opaque-private-cursor")}, nil
	})
	if conversations != nil || code != "conversation_lookup_incomplete" || diagnostic != "cursor_invalid" || calls != 1 {
		t.Fatalf("opaque continuation was treated as complete: code=%q diagnostic=%q calls=%d", code, diagnostic, calls)
	}
}
