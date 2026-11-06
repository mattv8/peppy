package gmlink

import (
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"unicode/utf8"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/reflect/protoreflect"
)

const (
	mediaSizeLimit   int64 = 64 << 20
	mediaStringLimit       = 127
	dimensionLimit   int64 = 32768
)

type mediaProjection struct {
	MIME                string
	Size, Width, Height int64
	HasDecryptionKey    bool
}

func TestM5MediaProjectionExcludesSecrets(t *testing.T) {
	projection, code := projectMedia(&gmproto.MediaContent{MimeType: "image/png", Size: 42, Dimensions: &gmproto.Dimensions{Width: 2, Height: 3}, DecryptionKey: []byte{1}})
	if code != "verified" || projection.MIME != "image/png" || !projection.HasDecryptionKey || projection.Width != 2 {
		t.Fatalf("projection=%#v code=%q", projection, code)
	}
}
func TestM5MediaProjectionRejectsUnsafeMetadata(t *testing.T) {
	for name, media := range map[string]*gmproto.MediaContent{"missing_mime": {Size: 1}, "missing_size": {MimeType: "image/png"}, "oversized": {MimeType: "image/png", Size: mediaSizeLimit + 1}, "empty_subtype": {MimeType: "image/", Size: 1}, "whitespace": {MimeType: "image/ png", Size: 1}, "zero_dimension": {MimeType: "image/png", Size: 1, Dimensions: &gmproto.Dimensions{}}, "negative_dimension": {MimeType: "image/png", Size: 1, Dimensions: &gmproto.Dimensions{Width: -1, Height: 1}}, "large_dimension": {MimeType: "image/png", Size: 1, Dimensions: &gmproto.Dimensions{Width: dimensionLimit + 1, Height: 1}}} {
		t.Run(name, func(t *testing.T) {
			if _, code := projectMedia(media); code == "verified" {
				t.Fatal("accepted unsafe media")
			}
		})
	}
}
func TestM5MediaDescriptorHasNoDuration(t *testing.T) {
	fields := fieldNames((&gmproto.MediaContent{}).ProtoReflect().Descriptor())
	expected := []string{"format", "mediaID", "mediaName", "size", "dimensions", "mediaData", "thumbnailMediaID", "decryptionKey", "thumbnailDecryptionKey", "mimeType"}
	if !reflect.DeepEqual(fields, expected) {
		t.Fatalf("fields=%v", fields)
	}
}
func TestM5UploadMediaBuffersWholePlaintext(t *testing.T) {
	path := filepath.Join("..", "build", "third_party", "mautrix-gmessages", "pkg", "libgm", "media.go")
	source, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	file, err := parser.ParseFile(token.NewFileSet(), path, source, 0)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	ast.Inspect(file, func(node ast.Node) bool {
		call, ok := node.(*ast.CallExpr)
		if ok && strings.Contains(exprName(call.Fun), "EncryptData") && len(call.Args) == 1 {
			if value, ok := call.Args[0].(*ast.Ident); ok && value.Name == "data" {
				found = true
			}
		}
		return true
	})
	if !found {
		t.Fatal("UploadMedia no longer encrypts its whole data parameter")
	}
}
func projectMedia(media *gmproto.MediaContent) (mediaProjection, string) {
	if media == nil || !validMIME(media.GetMimeType()) || media.GetSize() <= 0 || media.GetSize() > mediaSizeLimit {
		return mediaProjection{}, "media_projection_unverified"
	}
	projection := mediaProjection{media.GetMimeType(), media.GetSize(), 0, 0, len(media.GetDecryptionKey()) != 0}
	if d := media.GetDimensions(); d != nil {
		if d.GetWidth() <= 0 || d.GetHeight() <= 0 || d.GetWidth() > dimensionLimit || d.GetHeight() > dimensionLimit {
			return mediaProjection{}, "media_projection_unverified"
		}
		projection.Width, projection.Height = d.GetWidth(), d.GetHeight()
	}
	return projection, "verified"
}
func validMIME(value string) bool {
	parts := strings.Split(value, "/")
	return len(parts) == 2 && parts[0] != "" && parts[1] != "" && len(value) <= mediaStringLimit && utf8.ValidString(value) && !strings.ContainsAny(value, " \t\r\n\x00")
}
func fieldNames(descriptor protoreflect.MessageDescriptor) []string {
	names := make([]string, descriptor.Fields().Len())
	for index := range names {
		names[index] = string(descriptor.Fields().Get(index).Name())
	}
	return names
}
func exprName(expression ast.Expr) string {
	if selector, ok := expression.(*ast.SelectorExpr); ok {
		return selector.Sel.Name
	}
	if name, ok := expression.(*ast.Ident); ok {
		return name.Name
	}
	return ""
}
