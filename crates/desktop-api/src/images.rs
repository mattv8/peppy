//! Bounded, metadata-free raster transformations for native hosts.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{
    DynamicImage, ImageDecoder, ImageFormat, Limits, codecs::jpeg::JpegEncoder,
    imageops::FilterType,
};
use std::io::Cursor;

pub const MAX_IMAGE_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_PUBLIC_COPY_BYTES: usize = 10 * 1024 * 1024;
const MAX_DECODE_EDGE: u32 = 8192;
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;
const PREVIEW_EDGE: u32 = 320;
const PUBLIC_EDGE: u32 = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedImageError {
    Unsupported,
    TooLarge,
}

pub struct PublicImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Media type from magic bytes, never from the file name.
pub fn sniff_media_type(prefix: &[u8]) -> &'static str {
    match image::guess_format(prefix) {
        Ok(ImageFormat::Png) => "image/png",
        Ok(ImageFormat::Jpeg) => "image/jpeg",
        Ok(ImageFormat::WebP) => "image/webp",
        Ok(ImageFormat::Gif) => "image/gif",
        _ if prefix.starts_with(b"%PDF-") => "application/pdf",
        _ => "application/octet-stream",
    }
}

pub fn is_previewable(media_type: &str) -> bool {
    matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}

fn decode_limited(bytes: &[u8]) -> Option<(DynamicImage, ImageFormat)> {
    if bytes.len() as u64 > MAX_IMAGE_SOURCE_BYTES {
        return None;
    }
    let format = image::guess_format(bytes).ok()?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        return None;
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Some((image, format))
}

fn encode_png(image: &DynamicImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// A small re-encoded PNG thumbnail as a data URL, or `None` for anything not safely decodable.
pub fn preview_data_url(bytes: &[u8]) -> Option<String> {
    thumbnail_data_url(bytes, PREVIEW_EDGE).map(|(url, _, _)| url)
}

/// Re-encoded PNG data URL fitting in `edge`×`edge`, with its pixel size.
pub fn thumbnail_data_url(bytes: &[u8], edge: u32) -> Option<(String, u32, u32)> {
    let (image, _) = decode_limited(bytes)?;
    let thumbnail = image.thumbnail(edge, edge);
    let png = encode_png(&thumbnail)?;
    Some((
        format!("data:image/png;base64,{}", STANDARD.encode(png)),
        thumbnail.width(),
        thumbnail.height(),
    ))
}

/// Small re-encoded JPEG avatar data URL fitting in `edge`×`edge` and at most `max_bytes`.
pub fn avatar_data_url(bytes: &[u8], edge: u32, max_bytes: usize) -> Option<String> {
    let (image, _) = decode_limited(bytes)?;
    let rgb = DynamicImage::ImageRgb8(image.thumbnail(edge, edge).to_rgb8());
    for quality in [80, 65, 50, 35] {
        let mut out = Vec::new();
        rgb.write_with_encoder(JpegEncoder::new_with_quality(&mut out, quality))
            .ok()?;
        if out.len() <= max_bytes {
            return Some(format!("data:image/jpeg;base64,{}", STANDARD.encode(out)));
        }
    }
    None
}

/// Decodes and re-encodes an image into a new, metadata-free derivative.
pub fn reencode_public(bytes: &[u8]) -> Result<PublicImage, SharedImageError> {
    if bytes.len() as u64 > MAX_IMAGE_SOURCE_BYTES {
        return Err(SharedImageError::TooLarge);
    }
    let (image, format) = decode_limited(bytes).ok_or(SharedImageError::Unsupported)?;
    for edge in [PUBLIC_EDGE, 1024, 512] {
        let scaled = if image.width() > edge || image.height() > edge {
            image.resize(edge, edge, FilterType::Triangle)
        } else {
            image.clone()
        };
        let (bytes, extension) = if format == ImageFormat::Jpeg {
            let mut out = Vec::new();
            DynamicImage::ImageRgb8(scaled.to_rgb8())
                .write_with_encoder(JpegEncoder::new_with_quality(&mut out, 85))
                .map_err(|_| SharedImageError::Unsupported)?;
            (out, "jpg")
        } else {
            (
                encode_png(&scaled).ok_or(SharedImageError::Unsupported)?,
                "png",
            )
        };
        if bytes.len() <= MAX_PUBLIC_COPY_BYTES {
            return Ok(PublicImage {
                bytes,
                extension,
                width: scaled.width(),
                height: scaled.height(),
            });
        }
    }
    Err(SharedImageError::TooLarge)
}

/// Server-safe public filename (`[A-Za-z0-9._-]`, at most 100 bytes) with the derivative's extension.
pub fn public_name(display_name: &str, extension: &str) -> String {
    let stem = display_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .rsplit_once('.')
        .map_or(display_name, |(stem, _)| stem);
    let cleaned: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(60)
        .collect();
    format!(
        "{}.{extension}",
        if cleaned.is_empty() {
            "image"
        } else {
            &cleaned
        }
    )
}
