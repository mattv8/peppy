package gmlink

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	gmcrypto "go.mau.fi/mautrix-gmessages/pkg/libgm/crypto"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestOpenRcsMediaInputRejectsReplacedPath(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "source.jpg")
	replacement := filepath.Join(dir, "replacement.jpg")
	if err := os.WriteFile(path, []byte("old"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(replacement, []byte("new"), 0600); err != nil {
		t.Fatal(err)
	}
	_, err := openRcsMediaInput(path, 3, func(name string) (*os.File, error) {
		if err := os.Rename(replacement, name); err != nil {
			return nil, err
		}
		return os.Open(name)
	})
	if !errors.Is(err, errRcsMediaInputChanged) {
		t.Fatalf("err=%v", err)
	}
}

func TestReadRcsMediaInputRejectsGrowth(t *testing.T) {
	path := filepath.Join(t.TempDir(), "source.jpg")
	if err := os.WriteFile(path, []byte("large"), 0600); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	_, err = readRcsMediaInput(context.Background(), file, 4)
	if !errors.Is(err, errRcsMediaInputChanged) {
		t.Fatalf("err=%v", err)
	}
}

func TestUploadRcsMediaUsesBoundedTransferContext(t *testing.T) {
	path := filepath.Join(t.TempDir(), "source.jpg")
	if err := os.WriteFile(path, []byte("media"), 0600); err != nil {
		t.Fatal(err)
	}
	link := &Link{runCtx: context.Background()}
	link.uploadMedia = func(ctx context.Context, _ []byte, _, _ string) (*gmproto.MediaContent, error) {
		deadline, ok := ctx.Deadline()
		if !ok {
			t.Fatal("upload context has no deadline")
		}
		remaining := time.Until(deadline)
		if remaining < 4*time.Minute || remaining > 6*time.Minute {
			t.Fatalf("upload deadline remaining=%s", remaining)
		}
		return &gmproto.MediaContent{MediaID: "id", DecryptionKey: bytes.Repeat([]byte{1}, 32), MimeType: "image/jpeg", Size: 5, MediaName: "source.jpg"}, nil
	}
	if result := link.UploadRcsMedia(`{"path":"` + path + `","mime":"image/jpeg","name":"source.jpg","size":5}`); result == `{"code":"unknown"}` {
		t.Fatalf("result=%s", result)
	}
}

func TestDownloadRcsMediaUsesBoundedTransferContext(t *testing.T) {
	destination := filepath.Join(t.TempDir(), "download.jpg")
	link := &Link{runCtx: context.Background()}
	link.downloadMedia = func(ctx context.Context, _ string, _ []byte) (io.ReadCloser, error) {
		deadline, ok := ctx.Deadline()
		if !ok {
			t.Fatal("download context has no deadline")
		}
		remaining := time.Until(deadline)
		if remaining < 4*time.Minute || remaining > 6*time.Minute {
			t.Fatalf("download deadline remaining=%s", remaining)
		}
		return io.NopCloser(bytes.NewReader([]byte("media"))), nil
	}
	mediaRef, _ := json.Marshal(rcsMediaReference{MediaID: "id", Key: bytes.Repeat([]byte{1}, 32), MIME: "image/jpeg", Size: 5, Name: "source.jpg"})
	request, _ := json.Marshal(rcsMediaDownloadRequest{MediaRef: string(mediaRef), DestPath: destination, MaxBytes: rcsMediaMaxBytes})
	if result := link.DownloadRcsMedia(string(request)); result != `{"code":"ok","size":5}` {
		t.Fatalf("result=%s", result)
	}
}

func TestUploadRcsMediaPassesCancellationToTransfer(t *testing.T) {
	path := filepath.Join(t.TempDir(), "source.jpg")
	if err := os.WriteFile(path, []byte("media"), 0600); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	called := false
	link := &Link{runCtx: ctx}
	link.uploadMedia = func(ctx context.Context, _ []byte, _, _ string) (*gmproto.MediaContent, error) {
		called = true
		return nil, ctx.Err()
	}
	if result := link.UploadRcsMedia(`{"path":"` + path + `","mime":"image/jpeg","name":"source.jpg","size":5}`); result != `{"code":"failed"}` {
		t.Fatalf("result=%s", result)
	}
	if called {
		t.Fatal("upload ran after cancellation")
	}
}

func TestUploadRcsMediaRejectsOversizedDeclarationWithoutReadingFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "source.jpg")
	if err := os.WriteFile(path, []byte("small"), 0600); err != nil {
		t.Fatal(err)
	}
	result := (&Link{}).UploadRcsMedia(`{"path":"` + path + `","mime":"image/jpeg","name":"source.jpg","size":33554433}`)
	if result != `{"code":"too_large"}` {
		t.Fatalf("result=%s", result)
	}
}

func TestUploadRcsMediaRejectsHEIC(t *testing.T) {
	path := filepath.Join(t.TempDir(), "photo.heic")
	if err := os.WriteFile(path, []byte("x"), 0600); err != nil {
		t.Fatal(err)
	}
	result := (&Link{}).UploadRcsMedia(`{"path":"` + path + `","mime":"image/heic","name":"photo.heic","size":1}`)
	if result != `{"code":"not_allowed_mime"}` {
		t.Fatalf("result=%s", result)
	}
}

func TestRcsMediaTransferRoundTrip(t *testing.T) {
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
	var upload rcsMediaResult
	if err := json.Unmarshal([]byte(link.UploadRcsMedia(`{"path":"`+source+`","mime":"image/jpeg","name":"source.jpg","size":5}`)), &upload); err != nil || upload.Code != "ok" {
		t.Fatalf("upload=%#v err=%v", upload, err)
	}
	dest := filepath.Join(dir, "download.jpg")
	request, _ := json.Marshal(rcsMediaDownloadRequest{MediaRef: upload.MediaRef, DestPath: dest, MaxBytes: rcsMediaMaxBytes})
	if result := link.DownloadRcsMedia(string(request)); result != `{"code":"ok","size":5}` {
		t.Fatalf("download=%s", result)
	}
	if got, err := os.ReadFile(dest); err != nil || string(got) != "media" {
		t.Fatalf("file=%q err=%v", got, err)
	}
	if info, err := os.Stat(dest); err != nil || info.Mode().Perm() != 0600 {
		t.Fatalf("mode=%#o err=%v", info.Mode().Perm(), err)
	}
}

func TestDownloadRcsMediaUsesGuardedLibgmDownload(t *testing.T) {
	plaintext := []byte("media")
	key := bytes.Repeat([]byte{1}, 32)
	cryptor, err := gmcrypto.NewAESGCMHelper(key)
	if err != nil {
		t.Fatal(err)
	}
	ciphertext, err := cryptor.EncryptData(plaintext)
	if err != nil {
		t.Fatal(err)
	}
	var requests atomic.Int32
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.Method != http.MethodGet || request.URL.Path != "/upload" {
			t.Fatalf("method=%s path=%q", request.Method, request.URL.Path)
		}
		requests.Add(1)
		_, _ = writer.Write(ciphertext)
	}))
	defer server.Close()
	client := safetyClient(t, server, nil, nil)
	client.AuthData.TachyonAuthToken = []byte("synthetic")
	client.AuthData.Browser = &gmproto.Device{}
	link := &Link{client: client, runCtx: context.Background()}
	destination := filepath.Join(t.TempDir(), "download.jpg")
	mediaRef, err := json.Marshal(rcsMediaReference{MediaID: "media", Key: key, MIME: "image/jpeg", Size: int64(len(plaintext)), Name: "source.jpg"})
	if err != nil {
		t.Fatal(err)
	}
	request, err := json.Marshal(rcsMediaDownloadRequest{MediaRef: string(mediaRef), DestPath: destination, MaxBytes: rcsMediaMaxBytes})
	if err != nil {
		t.Fatal(err)
	}
	if result := link.DownloadRcsMedia(string(request)); result != `{"code":"ok","size":5}` {
		t.Fatalf("result=%s", result)
	}
	if got, err := os.ReadFile(destination); err != nil || !bytes.Equal(got, plaintext) || requests.Load() != 1 {
		t.Fatalf("file=%q err=%v requests=%d", got, err, requests.Load())
	}
}

func TestGuardedMediaResponsesReturnFixedFailureCodes(t *testing.T) {
	key := bytes.Repeat([]byte{1}, 32)
	for _, test := range []struct {
		name   string
		status int
		body   []byte
	}{
		{name: "non-success status", status: http.StatusNotFound, body: []byte("not found")},
		{name: "chunk exponent overflow", status: http.StatusOK, body: []byte{0, 63, 0}},
	} {
		t.Run(test.name, func(t *testing.T) {
			server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
				if request.Method != http.MethodGet || request.URL.Path != "/upload" {
					t.Fatalf("method=%s path=%q", request.Method, request.URL.Path)
				}
				writer.WriteHeader(test.status)
				_, _ = writer.Write(test.body)
			}))
			defer server.Close()
			client := safetyClient(t, server, nil, nil)
			client.AuthData.TachyonAuthToken = []byte("synthetic")
			client.AuthData.Browser = &gmproto.Device{}
			link := &Link{client: client, runCtx: context.Background()}
			mediaRef, err := json.Marshal(rcsMediaReference{MediaID: "media", Key: key, MIME: "image/jpeg", Size: 1, Name: "source.jpg"})
			if err != nil {
				t.Fatal(err)
			}
			destination := filepath.Join(t.TempDir(), "download.jpg")
			request, err := json.Marshal(rcsMediaDownloadRequest{MediaRef: string(mediaRef), DestPath: destination, MaxBytes: rcsMediaMaxBytes})
			if err != nil {
				t.Fatal(err)
			}
			if result := link.DownloadRcsMedia(string(request)); result != `{"code":"failed"}` {
				t.Fatalf("result=%s", result)
			}
			if _, err := os.Stat(destination); !errors.Is(err, os.ErrNotExist) {
				t.Fatalf("destination err=%v", err)
			}
		})
	}
}

func TestGuardedFinalizeRejectsMissingMedia(t *testing.T) {
	var requests atomic.Int32
	server := httptest.NewTLSServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/upload" {
			t.Fatalf("path=%q", request.URL.Path)
		}
		switch requests.Add(1) {
		case 1:
			writer.Header().Set("x-goog-upload-chunk-granularity", "1")
			writer.Header().Set("x-goog-upload-url", "https://instantmessaging-pa.googleapis.com/upload")
		case 2:
			_, _ = writer.Write([]byte{})
		}
	}))
	defer server.Close()
	client := safetyClient(t, server, nil, nil)
	client.AuthData.TachyonAuthToken = []byte("synthetic")
	client.AuthData.Browser = &gmproto.Device{}
	if _, err := client.UploadMedia(context.Background(), []byte("media"), "source.jpg", "image/jpeg"); err == nil || requests.Load() != 2 {
		t.Fatalf("err=%v requests=%d", err, requests.Load())
	}
}

func TestDownloadRcsMediaRejectsExistingDestination(t *testing.T) {
	dir := t.TempDir()
	destination := filepath.Join(dir, "download.jpg")
	if err := os.WriteFile(destination, []byte("existing"), 0600); err != nil {
		t.Fatal(err)
	}
	link := &Link{runCtx: context.Background()}
	link.downloadMedia = func(_ context.Context, _ string, _ []byte) (io.ReadCloser, error) {
		return io.NopCloser(bytes.NewReader([]byte("media"))), nil
	}
	mediaRef, _ := json.Marshal(rcsMediaReference{MediaID: "id", Key: bytes.Repeat([]byte{1}, 32), MIME: "image/jpeg", Size: 5, Name: "source.jpg"})
	request, _ := json.Marshal(rcsMediaDownloadRequest{MediaRef: string(mediaRef), DestPath: destination, MaxBytes: rcsMediaMaxBytes})
	if result := link.DownloadRcsMedia(string(request)); result != `{"code":"invalid_request"}` {
		t.Fatalf("result=%s", result)
	}
	if contents, err := os.ReadFile(destination); err != nil || string(contents) != "existing" {
		t.Fatalf("contents=%q err=%v", contents, err)
	}
}

func TestDownloadRcsMediaDoesNotOverwriteDestinationCreatedDuringTransfer(t *testing.T) {
	dir := t.TempDir()
	destination := filepath.Join(dir, "download.jpg")
	reader, writer := io.Pipe()
	opened := make(chan struct{})
	link := &Link{runCtx: context.Background()}
	link.downloadMedia = func(_ context.Context, _ string, _ []byte) (io.ReadCloser, error) {
		close(opened)
		return reader, nil
	}
	mediaRef, err := json.Marshal(rcsMediaReference{MediaID: "id", Key: bytes.Repeat([]byte{1}, 32), MIME: "image/jpeg", Size: 5, Name: "source.jpg"})
	if err != nil {
		t.Fatal(err)
	}
	request, err := json.Marshal(rcsMediaDownloadRequest{MediaRef: string(mediaRef), DestPath: destination, MaxBytes: rcsMediaMaxBytes})
	if err != nil {
		t.Fatal(err)
	}
	result := make(chan string, 1)
	go func() { result <- link.DownloadRcsMedia(string(request)) }()
	<-opened
	if err := os.WriteFile(destination, []byte("existing"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := writer.Write([]byte("media")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	if got := <-result; got != `{"code":"invalid_request"}` {
		t.Fatalf("result=%s", got)
	}
	contents, err := os.ReadFile(destination)
	if err != nil || string(contents) != "existing" {
		t.Fatalf("contents=%q err=%v", contents, err)
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if entry.Name() != "download.jpg" {
			t.Fatalf("temporary file was not removed: %s", entry.Name())
		}
	}
}

func TestProjectRcsPartsCaptionAndAttachments(t *testing.T) {
	key := bytes.Repeat([]byte{1}, 32)
	body, attachments, ok := projectRcsParts([]*gmproto.MessageInfo{
		{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "caption"}}},
		{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{MediaID: "a", DecryptionKey: key, MimeType: "application/x-unknown", Size: 1, MediaName: "a"}}},
		{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{MediaID: "b", DecryptionKey: key, MimeType: "image/jpeg", Size: rcsMediaMaxBytes + 1, MediaName: "b"}}},
	})
	if !ok || body != "caption" || len(attachments) != 2 || attachments[0].Index != 0 || attachments[0].MediaRef == "" || !attachments[1].TooLarge || attachments[1].MediaRef != "" {
		t.Fatalf("body=%q attachments=%#v", body, attachments)
	}
}

func TestValidatedPayloadAddsDeterministicMediaPart(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "self", Participants: []*gmproto.Participant{{IsVisible: true, IsMe: true}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "peer"}}}}
	settings := &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: "self"}, RCSChats: &gmproto.RCSChats{Enabled: true}, SIMData: &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{}}}}}
	media := &rcsMediaReference{MediaID: "media", Key: bytes.Repeat([]byte{1}, 32), MIME: "image/jpeg", Size: 1, Name: "photo.jpg"}
	for _, content := range []string{"", "caption"} {
		payload, code := validatedPayload(conversation, settings, rcsSendRequest{TmpID: "tmp", GoogleConversationID: "conversation", Content: content, Media: media}, false)
		if code != "" || !payload.GetForceRCS() || len(payload.GetMessagePayload().GetMessageInfo()) != map[bool]int{true: 2, false: 1}[content != ""] {
			t.Fatalf("content=%q payload=%#v code=%q", content, payload, code)
		}
		infos := payload.GetMessagePayload().GetMessageInfo()
		if content != "" && infos[0].GetMessageContent().GetContent() != content {
			t.Fatal("caption not first")
		}
		if infos[len(infos)-1].GetMediaContent().GetMediaID() != media.MediaID {
			t.Fatal("missing media")
		}
	}
}
