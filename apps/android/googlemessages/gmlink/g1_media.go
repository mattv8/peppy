package gmlink

import (
	"strings"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const (
	m5MediaSizeLimit   int64 = 64 << 20
	m5MediaStringLimit       = 127
	m5DimensionLimit   int64 = 32768
)

// m5MediaProjection deliberately excludes IDs, URLs, names, and key bytes.
type m5MediaProjection struct {
	MIME             string
	Size             int64
	Width            int64
	Height           int64
	HasDecryptionKey bool
}

func projectM5Media(media *gmproto.MediaContent) (m5MediaProjection, string) {
	if media == nil || !m5MIME(media.GetMimeType()) || media.GetSize() <= 0 || media.GetSize() > m5MediaSizeLimit {
		return m5MediaProjection{}, "media_projection_unverified"
	}
	projection := m5MediaProjection{MIME: media.GetMimeType(), Size: media.GetSize(), HasDecryptionKey: len(media.GetDecryptionKey()) != 0}
	if dimensions := media.GetDimensions(); dimensions != nil {
		if dimensions.GetWidth() <= 0 || dimensions.GetWidth() > m5DimensionLimit || dimensions.GetHeight() <= 0 || dimensions.GetHeight() > m5DimensionLimit {
			return m5MediaProjection{}, "media_projection_unverified"
		}
		projection.Width, projection.Height = dimensions.GetWidth(), dimensions.GetHeight()
	}
	return projection, "verified"
}

func m5MIME(value string) bool {
	return utf8.ValidString(value) && len(value) <= m5MediaStringLimit && strings.Count(value, "/") == 1 && !strings.ContainsAny(value, "\r\n\x00")
}

// The pinned MediaContent proto has dimensions but no duration field.
func m5DurationCapability() string {
	return "media_duration_unavailable"
}
