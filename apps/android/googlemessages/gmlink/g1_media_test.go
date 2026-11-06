package gmlink

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestM5MediaProjectionExcludesSecrets(t *testing.T) {
	projection, code := projectM5Media(&gmproto.MediaContent{
		MimeType:      "image/png",
		Size:          42,
		Dimensions:    &gmproto.Dimensions{Width: 2, Height: 3},
		DecryptionKey: []byte{1, 2, 3},
	})
	if code != "verified" || projection.MIME != "image/png" || !projection.HasDecryptionKey || projection.Width != 2 || projection.Height != 3 {
		t.Fatalf("projection = %#v, code = %q", projection, code)
	}
}

func TestM5MediaProjectionRejectsUnsafeMetadata(t *testing.T) {
	for name, media := range map[string]*gmproto.MediaContent{
		"missing_mime": {Size: 1},
		"missing_size": {MimeType: "image/png"},
		"oversized":    {MimeType: "image/png", Size: m5MediaSizeLimit + 1},
	} {
		t.Run(name, func(t *testing.T) {
			if _, code := projectM5Media(media); code == "verified" {
				t.Fatal("accepted unsafe media metadata")
			}
		})
	}
}

func TestM5UploadMediaFixtureRecordsWholePlaintextBuffering(t *testing.T) {
	source, err := os.ReadFile(filepath.Join("..", "build", "third_party", "mautrix-gmessages", "pkg", "libgm", "media.go"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(source), "func (c *Client) UploadMedia(ctx context.Context, data []byte") || !strings.Contains(string(source), "cryptor.EncryptData(data)") {
		t.Fatal("pinned UploadMedia no longer proves whole-plaintext buffering")
	}
	if code := m5DurationCapability(); code != "media_duration_unavailable" {
		t.Fatalf("duration capability = %q", code)
	}
}
