use base64::{engine::general_purpose::STANDARD, Engine};
use image::ImageDecoder;
use peppy_client_core::{BannerCandidate, NotificationPreferences, NotificationPreview};
use std::io::Cursor;
use tauri::{AppHandle, Manager};

mod files;
pub(crate) use files::Lease;

pub(crate) const MAX_BASE64_BYTES: usize = 10_924;
pub(crate) const MAX_PNG_BYTES: usize = 8_192;
const MAX_PNG_METADATA_BYTES: usize = 1_024;

#[derive(Clone)]
pub(crate) enum Icon {
    Message,
    App(Vec<u8>),
}

pub(crate) fn for_candidate(
    preferences: &NotificationPreferences,
    candidate: &BannerCandidate,
    locked: bool,
) -> Option<Icon> {
    if locked || (preferences.preview == NotificationPreview::Hidden && !preferences.show_source) {
        return None;
    }
    if candidate.kind == "message" {
        return Some(Icon::Message);
    }
    let allows_app_icon =
        preferences.preview == NotificationPreview::Full || preferences.show_source;
    if allows_app_icon {
        if let Some(png) = candidate
            .app_icon_png_base64
            .as_deref()
            .and_then(valid_png_base64)
        {
            return Some(Icon::App(png));
        }
    }
    None
}

pub(crate) fn materialize(app: &AppHandle, icon: &Icon) -> Option<Lease> {
    let bytes = icon_bytes(icon);
    let root = app.path().app_data_dir().ok()?.join("notification-icons");
    files::materialize(&root, bytes)
}

pub(crate) fn recover_at_startup(app: &AppHandle) {
    if let Ok(root) = app.path().app_data_dir() {
        files::recover(&root.join("notification-icons"));
    }
}

#[cfg(test)]
pub(crate) fn materialize_for_test(root: &std::path::Path, icon: &Icon) -> Option<Lease> {
    files::materialize(root, icon_bytes(icon))
}

fn icon_bytes(icon: &Icon) -> &[u8] {
    match icon {
        Icon::Message => include_bytes!("../icons/notification-message.png").as_slice(),
        Icon::App(bytes) => bytes.as_slice(),
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn inline_image(icon: Option<&Icon>) -> Option<notify_rust::Image> {
    let bytes = icon_bytes(icon?);
    let image = decode_png(bytes)?;
    notify_rust::Image::from_rgba(32, 32, image.into_rgba8().into_raw()).ok()
}

fn valid_png_base64(value: &str) -> Option<Vec<u8>> {
    if value.len() > MAX_BASE64_BYTES || !value.len().is_multiple_of(4) {
        return None;
    }
    let bytes = STANDARD.decode(value).ok()?;
    if bytes.len() > MAX_PNG_BYTES || !png_header_32(&bytes) {
        return None;
    }
    decode_png(&bytes).map(|_| bytes)
}

#[derive(Clone, Copy)]
struct PngMetadata {
    has_icc_profile: bool,
}

fn png_metadata(bytes: &[u8]) -> Option<PngMetadata> {
    let mut cursor = 8usize;
    let mut metadata = 0usize;
    let mut has_icc_profile = false;
    while cursor.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let length = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        let end = cursor.checked_add(12 + length)?;
        if end > bytes.len() {
            return None;
        }
        let kind = &bytes[cursor + 4..cursor + 8];
        if kind[0] & 0x20 != 0 && kind != b"IDAT" && kind != b"IEND" {
            metadata = metadata.saturating_add(length);
            if metadata > MAX_PNG_METADATA_BYTES {
                return None;
            }
        }
        has_icc_profile |= kind == b"iCCP";
        cursor = end;
        if kind == b"IEND" {
            return (cursor == bytes.len()).then_some(PngMetadata { has_icc_profile });
        }
    }
    None
}

fn decode_png(bytes: &[u8]) -> Option<image::DynamicImage> {
    decode_png_with_limits(bytes, decode_limits())
}

fn decode_png_with_limits(bytes: &[u8], limits: image::Limits) -> Option<image::DynamicImage> {
    let metadata = png_metadata(bytes)?;
    let mut decoder =
        image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes), limits).ok()?;
    if decoder.dimensions() != (32, 32)
        || (metadata.has_icc_profile && decoder.icc_profile().ok().flatten().is_none())
    {
        return None;
    }
    let image = image::DynamicImage::from_decoder(decoder).ok()?;
    (image.width() == 32 && image.height() == 32).then_some(image)
}

fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(32);
    limits.max_image_height = Some(32);
    limits.max_alloc = Some(32 * 32 * 4);
    limits
}

fn png_header_32(bytes: &[u8]) -> bool {
    bytes.len() >= 24
        && bytes[..8] == [137, 80, 78, 71, 13, 10, 26, 10]
        && bytes[12..16] == *b"IHDR"
        && bytes[16..20] == 32u32.to_be_bytes()
        && bytes[20..24] == 32u32.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{
        DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat, RgbaImage,
    };

    fn png(width: u32, height: u32) -> String {
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::new(width, height))
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        STANDARD.encode(bytes)
    }

    #[test]
    fn notification_icon_accepts_only_bounded_32_pixel_pngs() {
        assert!(valid_png_base64(&png(32, 32)).is_some());
        assert!(valid_png_base64("not-base64").is_none());
        assert!(valid_png_base64(&"A".repeat(MAX_BASE64_BYTES + 1)).is_none());
        let valid = STANDARD.decode(png(32, 32)).unwrap();
        assert!(valid_png_base64(&STANDARD.encode(&valid[..valid.len() - 1])).is_none());
        let wrong_dimensions = STANDARD.decode(png(33, 32)).unwrap();
        assert_eq!(
            image::ImageReader::with_format(
                Cursor::new(&wrong_dimensions),
                image::ImageFormat::Png,
            )
            .decode()
            .unwrap()
            .dimensions(),
            (33, 32)
        );
        assert!(valid_png_base64(&STANDARD.encode(wrong_dimensions)).is_none());
        assert!(valid_png_base64(&STANDARD.encode(vec![0; MAX_PNG_BYTES + 1])).is_none());
        let excessive_metadata = with_metadata(&valid, MAX_PNG_METADATA_BYTES + 1);
        assert!(image::ImageReader::with_format(
            Cursor::new(&excessive_metadata),
            image::ImageFormat::Png,
        )
        .decode()
        .is_ok());
        assert!(valid_png_base64(&STANDARD.encode(excessive_metadata)).is_none());
    }

    #[test]
    fn notification_icon_rejects_compressed_icc_profile_over_allocation_budget() {
        let expanded_profile = vec![b'a'; 5_000];
        assert!(expanded_profile.len() > decode_limits().max_alloc.unwrap() as usize);
        let metadata_heavy = png_with_icc_profile(&expanded_profile);
        assert!(metadata_heavy.len() <= MAX_PNG_BYTES);
        assert!(STANDARD.encode(&metadata_heavy).len() <= MAX_BASE64_BYTES);
        assert!(png_metadata(&metadata_heavy).unwrap().has_icc_profile);
        let bounded_profile = vec![b'a'; 512];
        assert!(
            valid_png_base64(&STANDARD.encode(png_with_icc_profile(&bounded_profile))).is_some()
        );

        let mut decoder =
            image::codecs::png::PngDecoder::new(Cursor::new(&metadata_heavy)).unwrap();
        assert_eq!(decoder.icc_profile().unwrap().unwrap(), expanded_profile);
        let image = DynamicImage::from_decoder(decoder).unwrap();
        assert_eq!((image.width(), image.height()), (32, 32));

        let mut relaxed_limits = decode_limits();
        relaxed_limits.max_alloc = None;
        assert!(decode_png_with_limits(&metadata_heavy, relaxed_limits).is_some());
        assert!(valid_png_base64(&STANDARD.encode(metadata_heavy)).is_none());
    }

    fn with_metadata(png: &[u8], size: usize) -> Vec<u8> {
        let mut result = png[..33].to_vec();
        let mut data = vec![b'a'; size];
        data[0] = b'k';
        data[1] = 0;
        let kind = b"tEXt";
        result.extend_from_slice(&(data.len() as u32).to_be_bytes());
        result.extend_from_slice(kind);
        result.extend_from_slice(&data);
        result
            .extend_from_slice(&crc32(&[kind.as_slice(), data.as_slice()].concat()).to_be_bytes());
        result.extend_from_slice(&png[33..]);
        result
    }

    fn png_with_icc_profile(profile: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder.set_icc_profile(profile.to_vec()).unwrap();
        encoder
            .write_image(
                &vec![0; 32 * 32 * 4],
                32,
                32,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut value = !0u32;
        for byte in bytes {
            value ^= u32::from(*byte);
            for _ in 0..8 {
                value = (value >> 1) ^ (0xedb8_8320 & value.wrapping_neg());
            }
        }
        !value
    }

    #[test]
    fn notification_icon_product_glyphs_are_valid_32_pixel_pngs() {
        let bytes = include_bytes!("../icons/notification-message.png").as_slice();
        assert!(decode_png(bytes).is_some());
    }

    #[test]
    fn notification_icon_policy_never_leaks_app_icons_when_hidden_or_locked() {
        let encoded = png(32, 32);
        let candidate = BannerCandidate {
            id: "candidate".into(),
            kind: "notification".into(),
            conversation_id: None,
            notification_target: None,
            title: "private title".into(),
            body: "private body".into(),
            created_at: 0,
            source_name: Some("Calendar".into()),
            app_icon_png_base64: Some(encoded),
            one_time_code: None,
        };
        let full = NotificationPreferences::default();
        assert!(matches!(
            for_candidate(&full, &candidate, false),
            Some(Icon::App(_))
        ));
        let source = NotificationPreferences {
            preview: NotificationPreview::Hidden,
            show_source: true,
            ..NotificationPreferences::default()
        };
        assert!(matches!(
            for_candidate(&source, &candidate, false),
            Some(Icon::App(_))
        ));
        let hidden = NotificationPreferences {
            preview: NotificationPreview::Hidden,
            ..NotificationPreferences::default()
        };
        assert!(for_candidate(&hidden, &candidate, false).is_none());
        assert!(for_candidate(&source, &candidate, true).is_none());
        let missing_icon = BannerCandidate {
            app_icon_png_base64: None,
            one_time_code: None,
            ..candidate.clone()
        };
        assert!(for_candidate(&full, &missing_icon, false).is_none());
        let invalid_icon = BannerCandidate {
            app_icon_png_base64: Some("not-base64".into()),
            one_time_code: None,
            ..candidate.clone()
        };
        assert!(for_candidate(&full, &invalid_icon, false).is_none());
        let message = BannerCandidate {
            kind: "message".into(),
            ..candidate
        };
        assert!(matches!(
            for_candidate(&full, &message, false),
            Some(Icon::Message)
        ));
        assert!(for_candidate(&hidden, &message, false).is_none());
        assert!(for_candidate(&full, &message, true).is_none());
    }
}
