//! Image handling for previews and public copies. Decoding is bounded (dimensions and
//! allocation), only raster PNG/JPEG/WebP/GIF are accepted, and every output is a fresh
//! re-encode, so EXIF/XMP/ICC/text metadata from the source is never carried over. SVG/HTML and
//! other formats are never decoded or rendered.
use crate::error::{BridgeError, BridgeResult};
pub use peppy_desktop_api::images::{
    MAX_IMAGE_SOURCE_BYTES, PublicImage, avatar_data_url, is_previewable, preview_data_url,
    public_name, sniff_media_type, thumbnail_data_url,
};
use std::{fs, io::Read, path::Path};

/// Reads the first bytes of a file for sniffing.
pub fn read_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut prefix = Vec::with_capacity(32);
    fs::File::open(path)?.take(32).read_to_end(&mut prefix)?;
    Ok(prefix)
}

/// Reads a native plaintext file with a hard cap.
pub fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_IMAGE_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_IMAGE_SOURCE_BYTES).then_some(bytes)
}

fn unsupported() -> BridgeError {
    BridgeError::new(
        "public-copy-unsupported",
        "Only PNG, JPEG, WebP or GIF images can be shared as a public copy.",
    )
}

/// Decodes and re-encodes an image into a new, metadata-free derivative (JPEG for JPEG sources,
/// PNG otherwise), downscaled to at most 2048 px and within the server's size limit.
pub fn reencode_public(bytes: &[u8]) -> BridgeResult<PublicImage> {
    peppy_desktop_api::images::reencode_public(bytes).map_err(|error| match error {
        peppy_desktop_api::images::SharedImageError::Unsupported => unsupported(),
        peppy_desktop_api::images::SharedImageError::TooLarge => BridgeError::new(
            "public-copy-too-large",
            "The image is too large for a public copy even after downscaling.",
        ),
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use image::{DynamicImage, ImageFormat, codecs::jpeg::JpegEncoder};
    use std::io::Cursor;

    pub fn sample_png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 255) as u8, (y % 255) as u8, 90, 255])
        }));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn jpeg_with_exif() -> Vec<u8> {
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            40,
            30,
            image::Rgb([200, 10, 10]),
        ))
        .write_with_encoder(JpegEncoder::new_with_quality(&mut jpeg, 90))
        .unwrap();
        let payload = b"Exif\0\0GPS-SECRET-LOCATION-MARKER";
        let mut segment = vec![0xff, 0xe1];
        segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(payload);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&segment);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn public_copy_is_reencoded_without_source_metadata() {
        let source = jpeg_with_exif();
        assert!(contains(&source, b"GPS-SECRET"));
        assert!(preview_data_url(&source).is_some());
        let public = reencode_public(&source).unwrap();
        assert_eq!(public.extension, "jpg");
        assert!(!contains(&public.bytes, b"GPS-SECRET"));
        assert!(!contains(&public.bytes, b"Exif"));
        assert_eq!(
            image::guess_format(&public.bytes).unwrap(),
            ImageFormat::Jpeg
        );

        let png = reencode_public(&sample_png(3000, 20)).unwrap();
        assert_eq!(png.extension, "png");
        let decoded = image::load_from_memory(&png.bytes).unwrap();
        assert_eq!(decoded.width(), 2048);
    }

    #[test]
    fn non_raster_and_oversized_inputs_are_refused() {
        assert_eq!(
            reencode_public(
                b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>"
            )
            .err()
            .unwrap()
            .code,
            "public-copy-unsupported"
        );
        assert!(reencode_public(b"<html><body>x</body></html>").is_err());
        assert!(preview_data_url(b"%PDF-1.7 not an image").is_none());
        assert!(preview_data_url(&sample_png(8193, 2)).is_none());
    }

    #[test]
    fn avatars_are_bounded_jpeg_data_urls() {
        let url = avatar_data_url(&sample_png(256, 256), 128, 16 * 1024).unwrap();
        let encoded = url.strip_prefix("data:image/jpeg;base64,").unwrap();
        let bytes = STANDARD.decode(encoded).unwrap();
        assert!(bytes.len() <= 16 * 1024);
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (128, 128));
        assert!(avatar_data_url(b"<svg/>", 128, 16 * 1024).is_none());
    }

    #[test]
    fn previews_are_png_data_urls_and_names_are_sanitized() {
        let preview = preview_data_url(&sample_png(900, 600)).unwrap();
        assert!(preview.starts_with("data:image/png;base64,"));
        let decoded = image::load_from_memory(
            &STANDARD
                .decode(&preview["data:image/png;base64,".len()..])
                .unwrap(),
        )
        .unwrap();
        assert!(decoded.width() <= 320 && decoded.height() <= 320);
        assert_eq!(sniff_media_type(&sample_png(2, 2)), "image/png");
        assert_eq!(sniff_media_type(b"<svg"), "application/octet-stream");
        assert_eq!(public_name("../My Photo (1).HEIC", "jpg"), "MyPhoto1.jpg");
        assert_eq!(public_name("???", "png"), "image.png");
    }
}
