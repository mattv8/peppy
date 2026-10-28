//! Windows tray badge rasterization.
//!
//! The renderer is intentionally platform-independent so its pixel behavior is
//! covered by macOS-hosted tests. Only the tray and DPI wrapper is Windows-only.

use super::TrayBadge;
use std::num::NonZeroU64;

const UNREAD: [u8; 4] = [255, 59, 48, 255];
const LOCKED: [u8; 4] = [110, 110, 115, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const DIGITS: [[u8; 15]; 10] = [
    bits("111101101101111"),
    bits("010110010010111"),
    bits("111001111100111"),
    bits("111001111001111"),
    bits("101101111001001"),
    bits("111100111001111"),
    bits("111100111101111"),
    bits("111001001001001"),
    bits("111101111101111"),
    bits("111101111001111"),
];
const PLUS: [u8; 15] = bits("000010111010000");

const fn bits(pattern: &str) -> [u8; 15] {
    let bytes = pattern.as_bytes();
    let mut result = [0; 15];
    let mut index = 0;
    while index < 15 {
        result[index] = (bytes[index] == b'1') as u8;
        index += 1;
    }
    result
}

const fn lock_bits(pattern: &str) -> [u8; 25] {
    let bytes = pattern.as_bytes();
    let mut result = [0; 25];
    let mut index = 0;
    while index < 25 {
        result[index] = (bytes[index] == b'1') as u8;
        index += 1;
    }
    result
}

// Keep the lock bitmap separate because a const function cannot infer its size.
const LOCK: [u8; 25] = lock_bits("0111001010111111101111111");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pill {
    x: u32,
    width: u32,
    height: u32,
    ring: u32,
}

struct Glyph<'a> {
    bits: &'a [u8],
    width: u32,
    height: u32,
}

fn scale(size: u32) -> u32 {
    if size >= 32 {
        2
    } else {
        1
    }
}

fn pill_for(size: u32, tier: u8) -> Option<Pill> {
    let k = scale(size);
    let (width, height) = match (k, tier) {
        (_, 0) => return None,
        (1, 1 | 4) => (9, 9),
        (1, 2) => (13, 9),
        (1, 3) => (15, 9),
        (2, 1 | 4) => (14, 14),
        (2, 2) => (22, 14),
        (2, 3) => (28, 14),
        _ => return None,
    };
    Some(Pill {
        x: size.saturating_sub(width),
        width,
        height,
        ring: k,
    })
}

fn contains_capsule(x: f32, y: f32, pill: Pill, expansion: f32) -> bool {
    let left = pill.x as f32 - expansion;
    let right = (pill.x + pill.width) as f32 + expansion;
    let top = -expansion;
    let bottom = pill.height as f32 + expansion;
    let radius = (pill.height as f32 / 2.0) + expansion;
    let center_x = x.clamp(left + radius, right - radius);
    let center_y = y.clamp(top + radius, bottom - radius);
    let dx = x - center_x;
    let dy = y - center_y;
    dx * dx + dy * dy <= radius * radius
}

fn coverage(x: u32, y: u32, pill: Pill) -> u8 {
    let mut covered = 0;
    for sample_y in 0..4 {
        for sample_x in 0..4 {
            let point_x = x as f32 + (sample_x as f32 + 0.5) / 4.0;
            let point_y = y as f32 + (sample_y as f32 + 0.5) / 4.0;
            covered += contains_capsule(point_x, point_y, pill, 0.0) as u32;
        }
    }
    ((covered * 255 + 8) / 16) as u8
}

fn pixel_offset(size: u32, x: u32, y: u32) -> usize {
    ((y * size + x) * 4) as usize
}

fn clear_knockout(pixels: &mut [u8], size: u32, pill: Pill) {
    for y in 0..size.min(pill.height + pill.ring) {
        for x in pill.x.saturating_sub(pill.ring)..size {
            if contains_capsule(x as f32 + 0.5, y as f32 + 0.5, pill, pill.ring as f32) {
                let offset = pixel_offset(size, x, y);
                pixels[offset + 3] = 0;
            }
        }
    }
}

fn composite(pixels: &mut [u8], offset: usize, foreground: [u8; 4]) {
    let foreground_alpha = foreground[3] as u32;
    let background_alpha = pixels[offset + 3] as u32;
    let output_alpha = foreground_alpha + background_alpha * (255 - foreground_alpha) / 255;
    if output_alpha == 0 {
        pixels[offset..offset + 4].fill(0);
        return;
    }
    for channel in 0..3 {
        let foreground_premultiplied = foreground[channel] as u32 * foreground_alpha;
        let background_premultiplied = pixels[offset + channel] as u32 * background_alpha;
        let output_premultiplied =
            foreground_premultiplied + background_premultiplied * (255 - foreground_alpha) / 255;
        pixels[offset + channel] = ((output_premultiplied + output_alpha / 2) / output_alpha) as u8;
    }
    pixels[offset + 3] = output_alpha as u8;
}

fn unpremultiply(color: u64, alpha: NonZeroU64) -> u8 {
    let alpha = alpha.get();
    ((color + alpha / 2) / alpha) as u8
}

fn paint_pill(pixels: &mut [u8], size: u32, pill: Pill, color: [u8; 4]) {
    for y in 0..pill.height.min(size) {
        for x in pill.x..size {
            let alpha = coverage(x, y, pill);
            if alpha != 0 {
                let offset = pixel_offset(size, x, y);
                composite(pixels, offset, [color[0], color[1], color[2], alpha]);
            }
        }
    }
}

fn paint_bitmap(pixels: &mut [u8], size: u32, origin: (u32, u32), glyph: Glyph<'_>, scale: u32) {
    for row in 0..glyph.height {
        for column in 0..glyph.width {
            if glyph.bits[(row * glyph.width + column) as usize] == 0 {
                continue;
            }
            for scaled_y in 0..scale {
                for scaled_x in 0..scale {
                    let x = origin.0 + column * scale + scaled_x;
                    let y = origin.1 + row * scale + scaled_y;
                    if x < size && y < size {
                        let offset = pixel_offset(size, x, y);
                        composite(pixels, offset, WHITE);
                    }
                }
            }
        }
    }
}

fn paint_label(pixels: &mut [u8], size: u32, badge: &TrayBadge, pill: Pill) {
    let k = scale(size);
    if matches!(badge, TrayBadge::Locked) {
        paint_bitmap(
            pixels,
            size,
            (pill.x + 2, 2),
            Glyph {
                bits: &LOCK,
                width: 5,
                height: 5,
            },
            k,
        );
        return;
    }

    let Some(label) = badge.label() else {
        return;
    };
    let padding = match (k, badge.tier()) {
        (1, 3) => 2,
        (2, 3) => 3,
        (1, _) => 3,
        (2, _) => 4,
        _ => return,
    };
    for (index, character) in label.bytes().enumerate() {
        let bitmap = if character == b'+' {
            &PLUS
        } else {
            &DIGITS[(character - b'0') as usize]
        };
        paint_bitmap(
            pixels,
            size,
            (pill.x + padding + index as u32 * 4 * k, 2),
            Glyph {
                bits: bitmap,
                width: 3,
                height: 5,
            },
            k,
        );
    }
}

fn downsample(base: &[u8], size: u32) -> Vec<u8> {
    let mut output = vec![0; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut alpha = 0u64;
            let mut red = 0u64;
            let mut green = 0u64;
            let mut blue = 0u64;
            for source_y in 0..32 {
                let weight_y = ((y + 1) * 32)
                    .min((source_y + 1) * size)
                    .saturating_sub((y * 32).max(source_y * size));
                for source_x in 0..32 {
                    let weight_x = ((x + 1) * 32)
                        .min((source_x + 1) * size)
                        .saturating_sub((x * 32).max(source_x * size));
                    let weight = (weight_x * weight_y) as u64;
                    if weight == 0 {
                        continue;
                    }
                    let source = pixel_offset(32, source_x, source_y);
                    let source_alpha = base[source + 3] as u64;
                    alpha += source_alpha * weight;
                    red += base[source] as u64 * source_alpha * weight;
                    green += base[source + 1] as u64 * source_alpha * weight;
                    blue += base[source + 2] as u64 * source_alpha * weight;
                }
            }
            let destination = pixel_offset(size, x, y);
            let area = 1024;
            output[destination + 3] = ((alpha + area / 2) / area) as u8;
            if let Some(alpha) = NonZeroU64::new(alpha) {
                output[destination] = unpremultiply(red, alpha);
                output[destination + 1] = unpremultiply(green, alpha);
                output[destination + 2] = unpremultiply(blue, alpha);
            }
        }
    }
    output
}

fn bilinear(base: &[u8], size: u32) -> Vec<u8> {
    let mut output = vec![0; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let source_x = ((x * 2 + 1) * 32 * 65_536 / (size * 2)).saturating_sub(32_768);
            let source_y = ((y * 2 + 1) * 32 * 65_536 / (size * 2)).saturating_sub(32_768);
            let left = (source_x / 65_536).min(31);
            let top = (source_y / 65_536).min(31);
            let fraction_x = source_x % 65_536;
            let fraction_y = source_y % 65_536;
            let weights = [
                (65_536 - fraction_x) as u64 * (65_536 - fraction_y) as u64,
                fraction_x as u64 * (65_536 - fraction_y) as u64,
                (65_536 - fraction_x) as u64 * fraction_y as u64,
                fraction_x as u64 * fraction_y as u64,
            ];
            let coordinates = [
                (left, top),
                ((left + 1).min(31), top),
                (left, (top + 1).min(31)),
                ((left + 1).min(31), (top + 1).min(31)),
            ];
            let mut alpha = 0u64;
            let mut colors = [0u64; 3];
            for ((source_x, source_y), weight) in coordinates.into_iter().zip(weights) {
                let source = pixel_offset(32, source_x, source_y);
                let source_alpha = base[source + 3] as u64;
                alpha += source_alpha * weight;
                for channel in 0..3 {
                    colors[channel] += base[source + channel] as u64 * source_alpha * weight;
                }
            }
            let destination = pixel_offset(size, x, y);
            let divisor = 65_536u64 * 65_536;
            output[destination + 3] = ((alpha + divisor / 2) / divisor) as u8;
            if let Some(alpha) = NonZeroU64::new(alpha) {
                for channel in 0..3 {
                    output[destination + channel] = unpremultiply(colors[channel], alpha);
                }
            }
        }
    }
    output
}

fn resized_base(size: u32) -> tauri::Result<Vec<u8>> {
    let base = image::load_from_memory(crate::tray::WINDOWS_TRAY_ICON)?
        .into_rgba8()
        .into_raw();
    Ok(match size {
        32 => base,
        0 => Vec::new(),
        1..=31 => downsample(&base, size),
        _ => bilinear(&base, size),
    })
}

fn render(size: u32, badge: &TrayBadge) -> tauri::Result<Vec<u8>> {
    let mut pixels = resized_base(size)?;
    let Some(pill) = pill_for(size, badge.tier()) else {
        return Ok(pixels);
    };
    clear_knockout(&mut pixels, size, pill);
    paint_pill(
        &mut pixels,
        size,
        pill,
        if matches!(badge, TrayBadge::Locked) {
            LOCKED
        } else {
            UNREAD
        },
    );
    paint_label(&mut pixels, size, badge, pill);
    Ok(pixels)
}

#[cfg(windows)]
fn icon_size() -> u32 {
    use windows::Win32::UI::{
        HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi},
        WindowsAndMessaging::SM_CXSMICON,
    };

    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
    u32::try_from(size)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(32)
}

#[cfg(windows)]
pub(super) fn apply(
    tray: &tauri::tray::TrayIcon<tauri::Wry>,
    badge: &TrayBadge,
) -> tauri::Result<()> {
    if matches!(badge, TrayBadge::Hidden) {
        return tray.set_icon(Some(tauri::image::Image::from_bytes(
            crate::tray::WINDOWS_TRAY_ICON,
        )?));
    }
    let size = icon_size();
    let pixels = render(size, badge)?;
    tray.set_icon(Some(
        tauri::image::Image::new(&pixels, size, size).to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_geometry_matches_the_fixed_tiers() {
        assert_eq!(pill_for(16, 1).unwrap().width, 9);
        assert_eq!(pill_for(16, 2).unwrap().width, 13);
        assert_eq!(pill_for(16, 3).unwrap().width, 15);
        assert_eq!(pill_for(16, 4).unwrap().width, 9);
        assert_eq!(pill_for(32, 1).unwrap().width, 14);
        assert_eq!(pill_for(32, 2).unwrap().width, 22);
        assert_eq!(pill_for(32, 3).unwrap().width, 28);
        assert_eq!(pill_for(32, 4).unwrap().width, 14);
    }

    #[test]
    fn unread_badge_is_upper_right_with_a_knockout_ring() {
        let pixels = render(16, &TrayBadge::Unread(5)).unwrap();
        let pill = pill_for(16, 1).unwrap();
        assert!(pixels[pixel_offset(16, 15, 0) + 3] < 255);
        assert_eq!(&pixels[pixel_offset(16, 11, 4)..][..4], &WHITE);
        assert_eq!(pixels[pixel_offset(16, 6, 4) + 3], 0);
        assert_eq!(pixels[pixel_offset(16, 0, 15) + 3], 0);
        assert_eq!(pill.x, 7);
    }

    #[test]
    fn digit_and_label_boundaries_use_the_approved_bitmap() {
        let single = render(16, &TrayBadge::Unread(5)).unwrap();
        assert_eq!(&single[pixel_offset(16, 10, 2)..][..4], &WHITE);
        assert_eq!(&single[pixel_offset(16, 12, 6)..][..4], &WHITE);

        let ninetynine_plus = render(16, &TrayBadge::Unread(100)).unwrap();
        assert_eq!(&ninetynine_plus[pixel_offset(16, 3, 2)..][..4], &WHITE);
        assert_eq!(&ninetynine_plus[pixel_offset(16, 12, 3)..][..4], &WHITE);
        assert_eq!(
            render(16, &TrayBadge::Unread(u64::MAX)).unwrap(),
            ninetynine_plus
        );
    }

    #[test]
    fn locked_uses_the_gray_pill_and_white_lock() {
        let pixels = render(16, &TrayBadge::Locked).unwrap();
        assert_eq!(&pixels[pixel_offset(16, 8, 4)..][..4], &LOCKED);
        assert_eq!(&pixels[pixel_offset(16, 10, 2)..][..4], &WHITE);
    }

    #[test]
    fn hidden_is_the_original_32_pixel_icon() {
        let rendered = render(32, &TrayBadge::Hidden).unwrap();
        let original = image::load_from_memory(crate::tray::WINDOWS_TRAY_ICON)
            .unwrap()
            .into_rgba8()
            .into_raw();
        assert_eq!(rendered, original);
    }

    #[test]
    fn upscaled_icons_preserve_base_alpha_and_render_every_badge_kind() {
        for size in [40, 48, 64] {
            let base = resized_base(size).unwrap();
            assert_eq!(render(size, &TrayBadge::Hidden).unwrap(), base);
            assert!(base.as_chunks::<4>().0.iter().any(|pixel| pixel[3] == 0));
            assert!(base.as_chunks::<4>().0.iter().any(|pixel| pixel[3] == 255));

            for badge in [TrayBadge::Unread(42), TrayBadge::Locked] {
                let rendered = render(size, &badge).unwrap();
                let pill = pill_for(size, badge.tier()).unwrap();
                let center = pixel_offset(size, pill.x + pill.width / 2, pill.height / 2);
                assert_eq!(rendered.len(), (size * size * 4) as usize);
                assert_eq!(rendered[center + 3], 255);
            }
        }
    }
}
