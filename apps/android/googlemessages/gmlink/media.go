package gmlink

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	rcsMediaMaxBytes        = 32 << 20
	rcsMediaTransferTimeout = 5 * time.Minute
)

var errRcsMediaInputChanged = errors.New("rcs_media_input_changed")

type rcsMediaResult struct {
	Code     string `json:"code"`
	MediaRef string `json:"media_ref,omitempty"`
	Size     int64  `json:"size,omitempty"`
}

type rcsMediaUploadRequest struct {
	Path string `json:"path"`
	MIME string `json:"mime"`
	Name string `json:"name"`
	Size int64  `json:"size"`
}

type rcsMediaDownloadRequest struct {
	MediaRef string `json:"media_ref"`
	DestPath string `json:"dest_path"`
	MaxBytes int64  `json:"max_bytes"`
}

func (link *Link) UploadRcsMedia(requestJSON string) string {
	var request rcsMediaUploadRequest
	if !strictMediaJSON(requestJSON, &request, "path", "mime", "name", "size") || request.Path == "" || request.MIME == "" || request.Name == "" || request.Size <= 0 {
		return mediaJSON(rcsMediaResult{Code: "invalid_request"})
	}
	if request.Size > rcsMediaMaxBytes {
		return mediaJSON(rcsMediaResult{Code: "too_large"})
	}
	if !allowedRcsMIME(request.MIME) {
		return mediaJSON(rcsMediaResult{Code: "not_allowed_mime"})
	}
	if !link.transferMu.TryLock() {
		return mediaJSON(rcsMediaResult{Code: "busy"})
	}
	defer link.transferMu.Unlock()
	ctx, cancel, upload := link.mediaRuntime()
	defer cancel()
	if upload == nil {
		return mediaJSON(rcsMediaResult{Code: "rejected"})
	}
	input, err := openRcsMediaInput(request.Path, request.Size, os.Open)
	if err != nil {
		return mediaJSON(rcsMediaResult{Code: "invalid_request"})
	}
	defer input.Close()
	data, err := readRcsMediaInput(ctx, input, request.Size)
	if err != nil {
		return mediaJSON(rcsMediaResult{Code: mediaErrorCode(ctx, err)})
	}
	media, err := upload(ctx, data, request.Name, request.MIME)
	if err != nil || media == nil {
		return mediaJSON(rcsMediaResult{Code: mediaErrorCode(ctx, err)})
	}
	ref, err := json.Marshal(rcsMediaReference{MediaID: media.GetMediaID(), Key: media.GetDecryptionKey(), MIME: media.GetMimeType(), Size: media.GetSize(), Name: media.GetMediaName()})
	if err != nil || !validNativeMedia(&rcsMediaReference{MediaID: media.GetMediaID(), Key: media.GetDecryptionKey(), MIME: media.GetMimeType(), Size: media.GetSize(), Name: media.GetMediaName()}) {
		return mediaJSON(rcsMediaResult{Code: "failed"})
	}
	return mediaJSON(rcsMediaResult{Code: "ok", MediaRef: string(ref)})
}

func (link *Link) DownloadRcsMedia(requestJSON string) string {
	var request rcsMediaDownloadRequest
	var media rcsMediaReference
	if !strictMediaJSON(requestJSON, &request, "media_ref", "dest_path", "max_bytes") || request.MediaRef == "" || request.DestPath == "" || request.MaxBytes <= 0 || request.MaxBytes > rcsMediaMaxBytes || json.Unmarshal([]byte(request.MediaRef), &media) != nil || !validNativeMedia(&media) {
		return mediaJSON(rcsMediaResult{Code: "invalid_request"})
	}
	if media.Size > request.MaxBytes {
		return mediaJSON(rcsMediaResult{Code: "too_large"})
	}
	if !link.transferMu.TryLock() {
		return mediaJSON(rcsMediaResult{Code: "busy"})
	}
	defer link.transferMu.Unlock()
	if !filepath.IsAbs(request.DestPath) {
		return mediaJSON(rcsMediaResult{Code: "invalid_request"})
	}
	ctx, cancel, download := link.mediaDownloadRuntime()
	defer cancel()
	if download == nil {
		return mediaJSON(rcsMediaResult{Code: "rejected"})
	}
	reader, err := download(ctx, media.MediaID, media.Key)
	if err != nil || reader == nil {
		return mediaJSON(rcsMediaResult{Code: mediaErrorCode(ctx, err)})
	}
	defer reader.Close()
	temporary, err := os.CreateTemp(filepath.Dir(request.DestPath), ".rcs-media-*")
	if err != nil {
		return mediaJSON(rcsMediaResult{Code: "failed"})
	}
	temporaryPath := temporary.Name()
	defer os.Remove(temporaryPath)
	written, copyErr := io.Copy(temporary, io.LimitReader(contextReader{ctx: ctx, reader: reader}, request.MaxBytes+1))
	closeErr := temporary.Close()
	if copyErr != nil || closeErr != nil {
		return mediaJSON(rcsMediaResult{Code: mediaErrorCode(ctx, copyErr)})
	}
	if written > request.MaxBytes || written != media.Size {
		return mediaJSON(rcsMediaResult{Code: "too_large"})
	}
	source, err := os.Open(temporaryPath)
	if err != nil {
		return mediaJSON(rcsMediaResult{Code: "failed"})
	}
	defer source.Close()
	destination, err := os.OpenFile(request.DestPath, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if errors.Is(err, os.ErrExist) {
		return mediaJSON(rcsMediaResult{Code: "invalid_request"})
	}
	if err != nil {
		return mediaJSON(rcsMediaResult{Code: "failed"})
	}
	copied, copyErr := io.Copy(destination, contextReader{ctx: ctx, reader: source})
	closeErr = destination.Close()
	if copyErr != nil || closeErr != nil || copied != written {
		return mediaJSON(rcsMediaResult{Code: mediaErrorCode(ctx, copyErr)})
	}
	return mediaJSON(rcsMediaResult{Code: "ok", Size: written})
}

func (link *Link) mediaRuntime() (context.Context, context.CancelFunc, func(context.Context, []byte, string, string) (*gmproto.MediaContent, error)) {
	link.mu.Lock()
	defer link.mu.Unlock()
	ctx := link.runCtx
	if ctx == nil || (link.client == nil && link.uploadMedia == nil) {
		return context.Background(), func() {}, nil
	}
	upload := link.uploadMedia
	if upload == nil {
		client := link.client
		upload = client.UploadMedia
	}
	bounded, cancel := context.WithTimeout(ctx, rcsMediaTransferTimeout)
	return bounded, cancel, upload
}

func (link *Link) mediaDownloadRuntime() (context.Context, context.CancelFunc, func(context.Context, string, []byte) (io.ReadCloser, error)) {
	link.mu.Lock()
	defer link.mu.Unlock()
	ctx := link.runCtx
	if ctx == nil || (link.client == nil && link.downloadMedia == nil) {
		return context.Background(), func() {}, nil
	}
	download := link.downloadMedia
	if download == nil {
		client := link.client
		download = client.DownloadMedia
	}
	bounded, cancel := context.WithTimeout(ctx, rcsMediaTransferTimeout)
	return bounded, cancel, download
}

func openRcsMediaInput(path string, expectedSize int64, openFile func(string) (*os.File, error)) (*os.File, error) {
	pathInfo, err := os.Lstat(path)
	if err != nil || !pathInfo.Mode().IsRegular() || pathInfo.Size() != expectedSize {
		return nil, errRcsMediaInputChanged
	}
	file, err := openFile(path)
	if err != nil {
		return nil, err
	}
	fileInfo, err := file.Stat()
	if err != nil || !fileInfo.Mode().IsRegular() || fileInfo.Size() != expectedSize || !os.SameFile(pathInfo, fileInfo) {
		file.Close()
		return nil, errRcsMediaInputChanged
	}
	return file, nil
}

func readRcsMediaInput(ctx context.Context, file *os.File, expectedSize int64) ([]byte, error) {
	data := make([]byte, 0, expectedSize)
	buffer := make([]byte, 32*1024)
	for int64(len(data)) <= expectedSize {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		remaining := expectedSize + 1 - int64(len(data))
		readSize := min(int64(len(buffer)), remaining)
		n, err := file.Read(buffer[:readSize])
		data = append(data, buffer[:n]...)
		if contextErr := ctx.Err(); contextErr != nil {
			return nil, contextErr
		}
		if int64(len(data)) > expectedSize {
			return nil, errRcsMediaInputChanged
		}
		if err == io.EOF {
			if int64(len(data)) != expectedSize {
				return nil, errRcsMediaInputChanged
			}
			return data, nil
		}
		if err != nil {
			return nil, err
		}
	}
	return nil, errRcsMediaInputChanged
}

type contextReader struct {
	ctx    context.Context
	reader io.Reader
}

func (reader contextReader) Read(data []byte) (int, error) {
	if err := reader.ctx.Err(); err != nil {
		return 0, err
	}
	n, err := reader.reader.Read(data)
	if contextErr := reader.ctx.Err(); contextErr != nil {
		return n, contextErr
	}
	return n, err
}

func allowedRcsMIME(mime string) bool {
	mime = strings.ToLower(mime)
	if mime == "image/heic" || mime == "image/heif" {
		return false
	}
	if mediaType, ok := libgm.MimeToMediaType[mime]; ok && mediaType.Type != 0 {
		return true
	}
	_, ok := libgm.MimeToMediaType[strings.Split(mime, "/")[0]]
	return ok && strings.Count(mime, "/") == 1
}

func strictMediaJSON(input string, output any, fields ...string) bool {
	decoder := json.NewDecoder(strings.NewReader(input))
	if token, err := decoder.Token(); err != nil || token != json.Delim('{') {
		return false
	}
	wanted := make(map[string]struct{}, len(fields))
	for _, field := range fields {
		wanted[field] = struct{}{}
	}
	values := make(map[string]json.RawMessage, len(fields))
	for decoder.More() {
		token, err := decoder.Token()
		field, ok := token.(string)
		if err != nil || !ok {
			return false
		}
		if _, ok := wanted[field]; !ok {
			return false
		}
		if _, duplicate := values[field]; duplicate {
			return false
		}
		var value json.RawMessage
		if decoder.Decode(&value) != nil || string(value) == "null" {
			return false
		}
		values[field] = value
	}
	if token, err := decoder.Token(); err != nil || token != json.Delim('}') || len(values) != len(fields) {
		return false
	}
	encoded, err := json.Marshal(values)
	return err == nil && decoder.Decode(&struct{}{}) == io.EOF && json.Unmarshal(encoded, output) == nil
}

func mediaErrorCode(ctx context.Context, err error) string {
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		return "unknown"
	}
	if errors.Is(err, os.ErrNotExist) {
		return "not_found"
	}
	return "failed"
}

func mediaJSON(result rcsMediaResult) string { value, _ := json.Marshal(result); return string(value) }
