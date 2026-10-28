//! Bounded raster decoding for the native Copy image command.
//!
//! The command that calls this module owns the authorized `NativePlaintextFile` guard. It must
//! keep that guard alive until this helper and the clipboard writer both return.
use crate::media::MAX_IMAGE_SOURCE_BYTES;
use image::{ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

pub(crate) const MAX_DECODED_RGBA_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ClipboardRgbaImage {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

pub(crate) trait ClipboardImageWriter {
    type Error;

    fn write_rgba(&mut self, image: ClipboardRgbaImage) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardImageError {
    Unsupported,
    SourceTooLarge,
    DecodedTooLarge,
    Invalid,
    ClipboardUnavailable,
}

#[derive(Clone, Copy)]
struct DecodeLimits {
    source_bytes: u64,
    decoded_rgba_bytes: u64,
}

const PRODUCTION_LIMITS: DecodeLimits = DecodeLimits {
    source_bytes: MAX_IMAGE_SOURCE_BYTES,
    decoded_rgba_bytes: MAX_DECODED_RGBA_BYTES,
};

/// Reads, validates, and writes an image from a native-only plaintext path.
///
/// Callers must retain the plaintext guard while this runs. The writer boundary deliberately has
/// no OS clipboard type so the decoder can be tested without a real clipboard.
pub(crate) fn copy_authorized_image_file(
    path: &Path,
    media_type: &str,
    writer: &mut impl ClipboardImageWriter,
) -> Result<(), ClipboardImageError> {
    let image = decode_authorized_image_file(path, media_type)?;
    write_rgba(writer, image)
}

/// Decodes an image from a path covered by a caller-owned plaintext guard.
pub(crate) fn decode_authorized_image_file(
    path: &Path,
    media_type: &str,
) -> Result<ClipboardRgbaImage, ClipboardImageError> {
    let file = File::open(path).map_err(|_| ClipboardImageError::Invalid)?;
    decode_rgba(file, media_type, PRODUCTION_LIMITS)
}

pub(crate) fn write_rgba(
    writer: &mut impl ClipboardImageWriter,
    image: ClipboardRgbaImage,
) -> Result<(), ClipboardImageError> {
    writer
        .write_rgba(image)
        .map_err(|_| ClipboardImageError::ClipboardUnavailable)
}

#[cfg(test)]
fn copy_image_reader(
    reader: impl Read,
    media_type: &str,
    writer: &mut impl ClipboardImageWriter,
    limits: DecodeLimits,
) -> Result<(), ClipboardImageError> {
    let image = decode_rgba(reader, media_type, limits)?;
    write_rgba(writer, image)
}

fn decode_rgba(
    mut reader: impl Read,
    media_type: &str,
    limits: DecodeLimits,
) -> Result<ClipboardRgbaImage, ClipboardImageError> {
    let expected_format = image_format_for_media_type(media_type)?;
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limits.source_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ClipboardImageError::Invalid)?;
    if bytes.len() as u64 > limits.source_bytes {
        return Err(ClipboardImageError::SourceTooLarge);
    }

    let actual_format = image::guess_format(&bytes).map_err(|_| ClipboardImageError::Invalid)?;
    if actual_format != expected_format {
        return Err(ClipboardImageError::Unsupported);
    }

    let (source_width, source_height) = image_dimensions(&bytes, actual_format, limits)?;
    checked_rgba_bytes(source_width, source_height, limits.decoded_rgba_bytes)?;
    let image = decode_image(&bytes, actual_format, limits)?;
    let (width, height) = (image.width(), image.height());
    let rgba_bytes = checked_rgba_bytes(width, height, limits.decoded_rgba_bytes)?;
    let bytes = image.into_rgba8().into_raw();
    if bytes.len() != rgba_bytes {
        return Err(ClipboardImageError::Invalid);
    }
    Ok(ClipboardRgbaImage {
        width,
        height,
        bytes,
    })
}

fn decode_image(
    bytes: &[u8],
    format: ImageFormat,
    limits: DecodeLimits,
) -> Result<image::DynamicImage, ClipboardImageError> {
    let mut decoder = bounded_reader(bytes, format, limits)
        .into_decoder()
        .map_err(map_image_error)?;
    let orientation = decoder.orientation().ok();
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(map_image_error)?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(image)
}

fn image_dimensions(
    bytes: &[u8],
    format: ImageFormat,
    limits: DecodeLimits,
) -> Result<(u32, u32), ClipboardImageError> {
    bounded_reader(bytes, format, limits)
        .into_dimensions()
        .map_err(map_image_error)
}

fn map_image_error(error: ImageError) -> ClipboardImageError {
    match error {
        ImageError::Limits(_) => ClipboardImageError::DecodedTooLarge,
        _ => ClipboardImageError::Invalid,
    }
}

fn bounded_reader(
    bytes: &[u8],
    format: ImageFormat,
    limits: DecodeLimits,
) -> ImageReader<Cursor<&[u8]>> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut image_limits = Limits::default();
    image_limits.max_alloc = Some(limits.decoded_rgba_bytes);
    reader.limits(image_limits);
    reader
}

fn checked_rgba_bytes(width: u32, height: u32, maximum: u64) -> Result<usize, ClipboardImageError> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ClipboardImageError::DecodedTooLarge)?;
    if width == 0 || height == 0 || bytes > maximum {
        return Err(ClipboardImageError::DecodedTooLarge);
    }
    usize::try_from(bytes).map_err(|_| ClipboardImageError::DecodedTooLarge)
}

pub(crate) fn is_supported_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}

fn image_format_for_media_type(media_type: &str) -> Result<ImageFormat, ClipboardImageError> {
    if !is_supported_media_type(media_type) {
        return Err(ClipboardImageError::Unsupported);
    }
    match media_type {
        "image/png" => Ok(ImageFormat::Png),
        "image/jpeg" => Ok(ImageFormat::Jpeg),
        "image/webp" => Ok(ImageFormat::WebP),
        "image/gif" => Ok(ImageFormat::Gif),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{
        codecs::gif::GifEncoder, Delay, DynamicImage, Frame, ImageFormat, Rgba, RgbaImage,
    };

    #[derive(Default)]
    struct FakeClipboard {
        written: Option<ClipboardRgbaImage>,
        fail: bool,
    }

    impl ClipboardImageWriter for FakeClipboard {
        type Error = ();

        fn write_rgba(&mut self, image: ClipboardRgbaImage) -> Result<(), Self::Error> {
            if self.fail {
                return Err(());
            }
            self.written = Some(image);
            Ok(())
        }
    }

    fn encoded_image(format: ImageFormat) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, Rgba([1, 2, 3, 4])));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn animated_gif() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = GifEncoder::new(&mut bytes);
        encoder
            .encode_frame(Frame::from_parts(
                RgbaImage::from_pixel(2, 1, Rgba([11, 22, 33, 255])),
                0,
                0,
                Delay::from_numer_denom_ms(1, 1),
            ))
            .unwrap();
        encoder
            .encode_frame(Frame::from_parts(
                RgbaImage::from_pixel(2, 1, Rgba([44, 55, 66, 255])),
                0,
                0,
                Delay::from_numer_denom_ms(1, 1),
            ))
            .unwrap();
        drop(encoder);
        bytes
    }

    fn media_type(format: ImageFormat) -> &'static str {
        match format {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Gif => "image/gif",
            _ => unreachable!(),
        }
    }

    fn test_limits() -> DecodeLimits {
        DecodeLimits {
            source_bytes: 1024,
            decoded_rgba_bytes: 64,
        }
    }

    #[test]
    fn writes_supported_raster_formats_as_rgba() {
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::WebP,
            ImageFormat::Gif,
        ] {
            let mut clipboard = FakeClipboard::default();
            copy_image_reader(
                Cursor::new(encoded_image(format)),
                media_type(format),
                &mut clipboard,
                test_limits(),
            )
            .unwrap();
            let image = clipboard.written.unwrap();
            assert_eq!((image.width, image.height), (3, 2));
            assert_eq!(image.bytes.len(), 3 * 2 * 4);
        }
    }

    #[test]
    fn invalid_or_mismatched_content_never_reaches_the_clipboard() {
        let mut clipboard = FakeClipboard::default();
        let result = copy_image_reader(
            Cursor::new(encoded_image(ImageFormat::Png)),
            "image/jpeg",
            &mut clipboard,
            test_limits(),
        );
        assert_eq!(result, Err(ClipboardImageError::Unsupported));
        assert!(clipboard.written.is_none());

        let result = copy_image_reader(
            Cursor::new(b"not an image"),
            "image/png",
            &mut clipboard,
            test_limits(),
        );
        assert_eq!(result, Err(ClipboardImageError::Invalid));
        assert!(clipboard.written.is_none());
    }

    #[test]
    fn animated_gif_writes_its_first_frame() {
        let mut clipboard = FakeClipboard::default();
        copy_image_reader(
            Cursor::new(animated_gif()),
            "image/gif",
            &mut clipboard,
            test_limits(),
        )
        .unwrap();
        assert_eq!(clipboard.written.unwrap().bytes[..4], [11, 22, 33, 255]);
    }

    #[test]
    fn source_and_decoded_limits_prevent_a_clipboard_write() {
        let mut clipboard = FakeClipboard::default();
        let result = copy_image_reader(
            Cursor::new(vec![0; 1025]),
            "image/png",
            &mut clipboard,
            test_limits(),
        );
        assert_eq!(result, Err(ClipboardImageError::SourceTooLarge));
        assert!(clipboard.written.is_none());

        let result = copy_image_reader(
            Cursor::new(encoded_image(ImageFormat::Png)),
            "image/png",
            &mut clipboard,
            DecodeLimits {
                source_bytes: 1024,
                decoded_rgba_bytes: 23,
            },
        );
        assert_eq!(result, Err(ClipboardImageError::DecodedTooLarge));
        assert!(clipboard.written.is_none());
    }

    #[test]
    fn clipboard_failure_is_reported_after_a_valid_decode() {
        let mut clipboard = FakeClipboard {
            written: None,
            fail: true,
        };
        let result = copy_image_reader(
            Cursor::new(encoded_image(ImageFormat::Png)),
            "image/png",
            &mut clipboard,
            test_limits(),
        );
        assert_eq!(result, Err(ClipboardImageError::ClipboardUnavailable));
        assert!(clipboard.written.is_none());
    }
}
