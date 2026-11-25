package gmlink

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestReplyGoldenStatus(t *testing.T) {
	samples := make(map[string]json.RawMessage)
	for _, state := range []string{stateUnlinked, statePairing, stateLinked, stateNeedsRelink, stateSignedOut} {
		link := &Link{state: state, seq: 42}
		switch state {
		case stateNeedsRelink:
			link.stopCode = "relink_needed"
		case stateSignedOut:
			link.stopCode = "signed_out"
		}
		if state == stateLinked {
			link.running, link.healthy, link.responding, link.lastOKMS = true, true, true, 1700000000000
		}
		samples[state] = json.RawMessage(link.Status())
	}
	assertReplyGolden(t, "status.json", samples)
}

func TestReplyGoldenMedia(t *testing.T) {
	dir := t.TempDir()
	source := filepath.Join(dir, "source.jpg")
	if err := os.WriteFile(source, []byte("media"), 0600); err != nil {
		t.Fatal(err)
	}
	link := &Link{runCtx: context.Background()}
	link.uploadMedia = func(_ context.Context, data []byte, name, mime string) (*gmproto.MediaContent, error) {
		return &gmproto.MediaContent{MediaID: "id", DecryptionKey: bytes.Repeat([]byte{1}, 32), MimeType: mime, Size: int64(len(data)), MediaName: name}, nil
	}
	link.downloadMedia = func(_ context.Context, _ string, _ []byte) (io.ReadCloser, error) {
		return io.NopCloser(bytes.NewReader([]byte("media"))), nil
	}
	request, err := json.Marshal(rcsMediaUploadRequest{Path: source, MIME: "image/jpeg", Name: "source.jpg", Size: 5})
	if err != nil {
		t.Fatal(err)
	}
	uploadJSON := link.UploadRcsMedia(string(request))
	var upload rcsMediaResult
	if err := json.Unmarshal([]byte(uploadJSON), &upload); err != nil || upload.Code != "ok" {
		t.Fatalf("upload=%s err=%v", uploadJSON, err)
	}
	request, err = json.Marshal(rcsMediaDownloadRequest{MediaRef: upload.MediaRef, DestPath: filepath.Join(dir, "download.jpg"), MaxBytes: rcsMediaMaxBytes})
	if err != nil {
		t.Fatal(err)
	}
	tooSmallRequest, err := json.Marshal(rcsMediaDownloadRequest{MediaRef: upload.MediaRef, DestPath: filepath.Join(dir, "download.jpg"), MaxBytes: 4})
	if err != nil {
		t.Fatal(err)
	}
	samples := map[string]json.RawMessage{
		"upload_invalid":     json.RawMessage(link.UploadRcsMedia("{}")),
		"download_invalid":   json.RawMessage(link.DownloadRcsMedia("{}")),
		"upload_ok":          json.RawMessage(uploadJSON),
		"download_ok":        json.RawMessage(link.DownloadRcsMedia(string(request))),
		"download_too_large": json.RawMessage(link.DownloadRcsMedia(string(tooSmallRequest))),
	}
	assertReplyGolden(t, "media.json", samples)
}

func TestReplyGoldenNativeSend(t *testing.T) {
	link := &Link{}
	samples := map[string]json.RawMessage{
		"commit_rejected": json.RawMessage(link.CommitNativeRcs("", "{}")),
		"revoke_none":     json.RawMessage(link.RevokeNativeSend("")),
	}
	assertReplyGolden(t, "native-send.json", samples)
}

func TestReplyGoldenGapPage(t *testing.T) {
	link := fetchGapTestLink(func(_ context.Context, _ string, _ int64, _ *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		return &gmproto.ListMessagesResponse{
			Messages: []*gmproto.Message{rcsTextMessageAt("message-1", 9_000_000)},
			Cursor:   &gmproto.Cursor{LastItemID: "page-1", LastItemTimestamp: 9000},
		}, nil
	})
	assertReplyGolden(t, "gap-page.json", map[string]json.RawMessage{
		"page": json.RawMessage(link.FetchRcsGap("conversation-1", "")),
	})
}

func TestReplyGoldenCandidateAndNativeSend(t *testing.T) {
	link, _, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	now := time.UnixMilli(1700000000000)
	link.mu.Lock()
	link.runIncarnationID = "123e4567-e89b-12d3-a456-426614174002"
	link.pairingEvidenceID = "123e4567-e89b-12d3-a456-426614174003"
	link.ticketNow = func() time.Time { return now }
	link.settingsAt = now
	link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = "+12025550101"
	link.conversationRecords["synthetic-conversation"].Participants[1].ID.Number = "+12025550102"
	link.mu.Unlock()
	snapshot := link.ConversationCandidateSnapshot()
	t.Run("candidate-snapshot", func(t *testing.T) {
		assertReplyGolden(t, "candidate-snapshot.json", map[string]json.RawMessage{"snapshot": json.RawMessage(snapshot)})
	})
	request := `{"command_id":"123e4567-e89b-12d3-a456-426614174001","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"android-subscription-1","android_subscription_id":1,"own_number_raw":"+12025550101","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":1700000000000}}`
	prepareInvalid := link.PrepareNativeRcs("{}")
	routeRejected := link.BindNativeRcsRoute("", 11, 17, 23, 29, now.UnixMilli())
	preparedJSON := link.PrepareNativeRcs(request)
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(preparedJSON), &prepared); err != nil || prepared.Code != "prepared" {
		t.Fatalf("prepare=%s err=%v", preparedJSON, err)
	}
	if _, err := uuid.Parse(prepared.TicketID); err != nil {
		t.Fatalf("invalid ticket ID: %v", err)
	}
	route := link.BindNativeRcsRoute(prepared.TicketID, 11, 17, 23, 29, now.UnixMilli())
	var result struct {
		Code        string `json:"code"`
		Observation string `json:"observation"`
	}
	if err := json.Unmarshal([]byte(route), &result); err != nil || result.Code != "route_bound" {
		t.Fatalf("route=%s err=%v", route, err)
	}
	var observation struct {
		NativeEvidence struct {
			LookupID string `json:"lookup_id"`
		} `json:"native_evidence"`
	}
	if err := json.Unmarshal([]byte(result.Observation), &observation); err != nil {
		t.Fatal(err)
	}
	if _, err := uuid.Parse(observation.NativeEvidence.LookupID); err != nil {
		t.Fatalf("invalid lookup ID: %v", err)
	}
	// Replace only randomly generated IDs, retaining the production-encoded shape.
	preparedJSON = strings.ReplaceAll(preparedJSON, prepared.TicketID, "123e4567-e89b-42d3-a456-426614174000")
	route = strings.ReplaceAll(route, observation.NativeEvidence.LookupID, "123e4567-e89b-42d3-a456-426614174004")
	assertReplyGolden(t, "native-send-success.json", map[string]json.RawMessage{
		"prepared":        json.RawMessage(preparedJSON),
		"route":           json.RawMessage(route),
		"prepare_invalid": json.RawMessage(prepareInvalid),
		"route_rejected":  json.RawMessage(routeRejected),
	})
}

func assertReplyGolden(t *testing.T, name string, samples map[string]json.RawMessage) {
	t.Helper()
	path := filepath.Join("..", "..", "..", "..", "packages", "contracts", "fixtures", "gmlink-replies", name)
	encoded, err := json.Marshal(samples)
	if err != nil {
		t.Fatal(err)
	}
	actual := canonicalReplyJSON(t, encoded)
	if os.Getenv("PEPPY_UPDATE_GOLDENS") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, actual, 0644); err != nil {
			t.Fatal(err)
		}
		t.Logf("rewrote golden %s", path)
		t.Skip("golden updated; comparison not performed")
	}
	expected, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	expected = canonicalReplyJSON(t, expected)
	if !bytes.Equal(actual, expected) {
		t.Errorf("%s differs from production output\n got: %s\nwant: %s", name, actual, expected)
	}
}

func canonicalReplyJSON(t *testing.T, encoded []byte) []byte {
	t.Helper()
	decoder := json.NewDecoder(bytes.NewReader(encoded))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		t.Fatal(err)
	}
	canonical, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	return append(canonical, '\n')
}
