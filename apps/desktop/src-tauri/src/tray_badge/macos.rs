//! macOS menu bar unread badge.
//!
//! The template logo stays a native template image, so macOS tints it for
//! light, dark and highlighted menu bars. The count or lock pill is a
//! non-interactive subview of the status item button. Peppy never hides or
//! removes its status item, so the pill view is created once and then reused.
//!
//! Geometry is right-anchored to the button with an autoresizing mask, so AppKit
//! re-places the pill on every button width change without a stored frame.
#![allow(deprecated, unexpected_cfgs)]

use cocoa::{
    base::{id, nil, BOOL, NO, YES},
    foundation::{NSPoint, NSRect, NSSize, NSString},
};
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{Class, Object, Sel},
    sel, sel_impl,
};
use tauri::{image::Image, tray::TrayIcon, Wry};

use super::TrayBadge;

const LOGO_ASSET: &[u8] = include_bytes!("../../icons/tray/macos-template.png");
/// The menu bar draws the 36 px template at 18 pt, so one point is two pixels.
const LOGO_PT: f64 = 18.0;
const LOGO_PX_PER_PT: f64 = 2.0;
/// Measured on the status button (harness, macOS 26.6): the logo sits 8 pt from
/// the left edge and the button has 8 pt after it. The widened logo keeps its x
/// because the padding is symmetric and the image is left-aligned in it.
const TRAILING_PAD_PT: f64 = 8.0;

const PILL_HEIGHT: f64 = 12.0;
/// The pill's left edge sits this far inside the logo's right edge.
const PILL_LOGO_OVERLAP: f64 = 5.0;
/// Gap between the pill's right edge and the button's right edge.
const PILL_EDGE_INSET: f64 = 1.0;
const LABEL_FONT_SIZE: f64 = 9.0;
/// NSFontWeightBold.
const NS_FONT_WEIGHT_BOLD: f64 = 0.4;
const NS_TEXT_ALIGNMENT_CENTER: i64 = 2;
/// Fixed sRGB gray for the locked pill, #6E6E73.
const LOCK_FILL_RGB: [f64; 3] = [110.0 / 255.0, 110.0 / 255.0, 115.0 / 255.0];

/// Padlock glyph, bezier geometry in pill points (UX critique: a 7x5 body with a
/// shallow shackle read as a handbag). The shackle is a taller arch that starts on
/// the body's top edge through 0.75 pt legs.
const LOCK_BODY_WIDTH: f64 = 6.5;
const LOCK_BODY_HEIGHT: f64 = 4.5;
const LOCK_BODY_Y: f64 = 2.0;
const LOCK_SHACKLE_RADIUS: f64 = 1.75;
const LOCK_SHACKLE_CENTER_Y: f64 = 7.25;
const LOCK_SHACKLE_LINE_WIDTH: f64 = 1.25;

/// NSAutoresizingMaskOptions. The left margin is flexible, so the right gap stays fixed.
const AUTORESIZE_FLEXIBLE_MIN_X_MARGIN: u64 = 1;
/// Unflipped: the bottom gap is fixed, the top margin flexes.
const AUTORESIZE_FLEXIBLE_MAX_Y_MARGIN: u64 = 32;
/// Flipped: the bottom edge is maxY, so the minY (top) margin flexes.
const AUTORESIZE_FLEXIBLE_MIN_Y_MARGIN: u64 = 8;

const PILL_CLASS: &str = "PeppyTrayUnreadBadgeView";
const FRAME_CHANGED_NOTIFICATION: &str = "NSViewFrameDidChangeNotification";
const PILL_LOCKED_IVAR: &str = "pillLocked";
/// NSTextField label subview tag. NSView has no settable tag, but NSControl does.
const LABEL_TAG: i64 = 2;

/// Pill width in points for each tier from `TrayBadge::tier`.
fn pill_width(tier: u8) -> f64 {
    match tier {
        1 => 12.0,
        2 => 17.0,
        3 => 22.0,
        4 => 16.0,
        _ => 0.0,
    }
}

/// Extra points the logo needs so the button can hold the pill's overhang.
fn logo_widening(tier: u8) -> f64 {
    let width = pill_width(tier);
    if width == 0.0 {
        0.0
    } else {
        (width - PILL_LOGO_OVERLAP - (TRAILING_PAD_PT - PILL_EDGE_INSET)).max(0.0)
    }
}

/// Pill x in button coordinates. It overlaps the logo's right edge by
/// `PILL_LOGO_OVERLAP`, and is clamped so it never leaves the button.
fn pill_origin_x(logo_right: f64, button_width: f64, width: f64) -> f64 {
    (logo_right - PILL_LOGO_OVERLAP).min(button_width - PILL_EDGE_INSET - width)
}

/// The pill hangs one point below the logo's visual bottom edge. Flipped
/// buttons measure y from the top, so the same visual position is converted.
fn pill_origin_y(button_height: f64, flipped: bool) -> f64 {
    let bottom_up = snap_half((button_height - LOGO_PT) / 2.0 - 1.0);
    if flipped {
        button_height - bottom_up - PILL_HEIGHT
    } else {
        bottom_up
    }
}

fn pill_autoresizing_mask(flipped: bool) -> u64 {
    let vertical = if flipped {
        AUTORESIZE_FLEXIBLE_MIN_Y_MARGIN
    } else {
        AUTORESIZE_FLEXIBLE_MAX_Y_MARGIN
    };
    AUTORESIZE_FLEXIBLE_MIN_X_MARGIN | vertical
}

fn snap_half(value: f64) -> f64 {
    (value * 2.0).round() / 2.0
}

/// Appends transparent columns to the right of each row of RGBA pixels.
fn pad_right(rgba: &[u8], width: usize, height: usize, pad_px: usize) -> Vec<u8> {
    let source_row = width * 4;
    let target_row = (width + pad_px) * 4;
    debug_assert_eq!(rgba.len(), source_row * height);
    let mut padded = vec![0u8; target_row * height];
    for (source, target) in rgba
        .chunks_exact(source_row)
        .zip(padded.chunks_exact_mut(target_row))
    {
        target[..source_row].copy_from_slice(source);
    }
    padded
}

/// Original logo at tier zero; otherwise the logo padded on the right so the
/// status item widens and the logo keeps its x position.
fn logo_image(tier: u8) -> tauri::Result<Image<'static>> {
    let pad_px = (logo_widening(tier) * LOGO_PX_PER_PT) as usize;
    if pad_px == 0 {
        return Image::from_bytes(LOGO_ASSET);
    }
    let source = Image::from_bytes(LOGO_ASSET)?;
    let width = source.width() as usize;
    let height = source.height() as usize;
    let rgba = pad_right(source.rgba(), width, height, pad_px);
    Ok(Image::new_owned(
        rgba,
        (width + pad_px) as u32,
        height as u32,
    ))
}

#[derive(Debug, PartialEq)]
struct Pill {
    text: Option<String>,
    locked: bool,
    width: f64,
}

fn pill_for(badge: &TrayBadge) -> Option<Pill> {
    let width = pill_width(badge.tier());
    match badge {
        TrayBadge::Hidden => None,
        TrayBadge::Locked => Some(Pill {
            text: None,
            locked: true,
            width,
        }),
        TrayBadge::Unread(_) => Some(Pill {
            text: badge.label(),
            locked: false,
            width,
        }),
    }
}

/// Applies the logo, accessibility label and pill for `badge`. The core sets the
/// tooltip, so it is not set here. The native work runs on the main thread inside
/// `with_inner_tray_icon`.
pub(super) fn apply(tray: &TrayIcon<Wry>, badge: &TrayBadge) -> tauri::Result<()> {
    tray.set_icon_with_as_template(Some(logo_image(badge.tier())?), true)?;
    let accessibility_label = badge.tooltip();
    let pill = pill_for(badge);
    tray.with_inner_tray_icon(move |inner| match inner.ns_status_item() {
        Some(item) => unsafe {
            let status_item = &*item as *const _ as id;
            let button: id = msg_send![status_item, button];
            place_pill(button, &accessibility_label, pill.as_ref())
        },
        None => Err("tray status item is unavailable".to_owned()),
    })?
    .map_err(|message| tauri::Error::Io(std::io::Error::other(message)))
}

/// Places or hides the pill on the status item's button. Runs on the main thread.
unsafe fn place_pill(
    button: id,
    accessibility_label: &str,
    pill: Option<&Pill>,
) -> Result<(), String> {
    if button == nil {
        return Err("tray status item has no button".to_owned());
    }
    let _: () = msg_send![button, setAccessibilityLabel: ns_string(accessibility_label)];

    let existing = find_pill_view(button);
    let Some(pill) = pill else {
        if existing != nil {
            let _: () = msg_send![existing, setHidden: YES];
        }
        return Ok(());
    };
    let view = if existing != nil {
        existing
    } else {
        create_pill_view(button)
    };

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(pill.width, PILL_HEIGHT));
    let _: () = msg_send![view, setFrame: frame];
    let locked = if pill.locked { YES } else { NO };
    let _: () = msg_send![view, setPillLocked: locked];
    let _: () = msg_send![view, setHidden: NO];
    configure_label(view, pill.text.as_deref(), pill.width);
    position_pill(view);
    let _: () = msg_send![view, setNeedsDisplay: YES];
    Ok(())
}

/// Places the pill from the button's current geometry: the logo's real image rect
/// and the button bounds. Runs on the first apply and on every button frame change.
unsafe fn position_pill(view: id) {
    let button: id = msg_send![view, superview];
    if button == nil {
        return;
    }
    // A frame-changed notification can arrive before the cell has laid out its image
    // for the new bounds, so lay the button out before reading the logo rect.
    let _: () = msg_send![button, layoutSubtreeIfNeeded];
    let bounds: NSRect = msg_send![button, bounds];
    let cell: id = msg_send![button, cell];
    let image_rect: NSRect = msg_send![cell, imageRectForBounds: bounds];
    let logo_right = image_rect.origin.x + LOGO_PT;
    let frame: NSRect = msg_send![view, frame];
    let flipped: BOOL = msg_send![button, isFlipped];
    let flipped = flipped != NO;
    let origin = NSPoint::new(
        pill_origin_x(logo_right, bounds.size.width, frame.size.width),
        pill_origin_y(bounds.size.height, flipped),
    );
    let _: () = msg_send![view, setAutoresizingMask: pill_autoresizing_mask(flipped)];
    let _: () = msg_send![view, setFrameOrigin: origin];
}

/// Observer callback: the button's frame changed, so re-place the pill.
extern "C" fn button_frame_changed(this: &Object, _: Sel, _notification: id) {
    unsafe {
        let view = this as *const Object as id;
        position_pill(view);
    }
}

unsafe fn find_pill_view(button: id) -> id {
    let class = pill_view_class() as *const Class as id;
    let subviews: id = msg_send![button, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let view: id = msg_send![subviews, objectAtIndex: index];
        let is_pill: BOOL = msg_send![view, isKindOfClass: class];
        if is_pill != NO {
            return view;
        }
    }
    nil
}

/// Creates the pill view once. The button retains it, so it is released here.
unsafe fn create_pill_view(button: id) -> id {
    let allocated: id = msg_send![pill_view_class(), alloc];
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, PILL_HEIGHT));
    let view: id = msg_send![allocated, initWithFrame: frame];
    let _: () = msg_send![view, setWantsLayer: YES];
    let _: () = msg_send![view, setAccessibilityElement: NO];
    // The pill observes its button, so AppKit layout changes re-place it.
    let _: () = msg_send![button, setPostsFrameChangedNotifications: YES];
    let center: id = msg_send![class!(NSNotificationCenter), defaultCenter];
    let _: () = msg_send![
        center,
        addObserver: view
        selector: sel!(peppyButtonFrameChanged:)
        name: ns_string(FRAME_CHANGED_NOTIFICATION)
        object: button
    ];

    let label: id = msg_send![class!(NSTextField), labelWithString: ns_string("")];
    let _: () = msg_send![label, setTag: LABEL_TAG];
    let _: () = msg_send![label, setAccessibilityElement: NO];
    let _: () = msg_send![view, addSubview: label];

    let _: () = msg_send![button, addSubview: view];
    let _: () = msg_send![view, release];
    view
}

unsafe fn configure_label(view: id, text: Option<&str>, width: f64) {
    let label: id = msg_send![view, viewWithTag: LABEL_TAG];
    if label == nil {
        return;
    }
    let text = text.unwrap_or("");
    let _: () = msg_send![label, setStringValue: ns_string(text)];
    let font: id = msg_send![
        class!(NSFont),
        monospacedDigitSystemFontOfSize: LABEL_FONT_SIZE
        weight: NS_FONT_WEIGHT_BOLD
    ];
    let _: () = msg_send![label, setFont: font];
    let white: id = msg_send![class!(NSColor), whiteColor];
    let _: () = msg_send![label, setTextColor: white];
    let _: () = msg_send![label, setAlignment: NS_TEXT_ALIGNMENT_CENTER];
    let _: () = msg_send![label, sizeToFit];
    let fitted: NSRect = msg_send![label, frame];
    let height = fitted.size.height;
    let frame = NSRect::new(
        NSPoint::new(0.0, (PILL_HEIGHT - height) / 2.0),
        NSSize::new(width, height),
    );
    let _: () = msg_send![label, setFrame: frame];
    let hidden = if text.is_empty() { YES } else { NO };
    let _: () = msg_send![label, setHidden: hidden];
}

fn pill_view_class() -> &'static Class {
    if let Some(class) = Class::get(PILL_CLASS) {
        return class;
    }
    let mut decl = ClassDecl::new(PILL_CLASS, class!(NSView)).expect("pill class name is unique");
    unsafe {
        decl.add_ivar::<BOOL>(PILL_LOCKED_IVAR);
        decl.add_method(
            sel!(drawRect:),
            draw_pill as extern "C" fn(&Object, Sel, NSRect),
        );
        decl.add_method(
            sel!(hitTest:),
            no_hit_test as extern "C" fn(&Object, Sel, NSPoint) -> id,
        );
        decl.add_method(
            sel!(isAccessibilityElement),
            not_accessibility_element as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(setPillLocked:),
            set_pill_locked as extern "C" fn(&mut Object, Sel, BOOL),
        );
        decl.add_method(
            sel!(pillLocked),
            pill_locked as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(peppyButtonFrameChanged:),
            button_frame_changed as extern "C" fn(&Object, Sel, id),
        );
    }
    decl.register()
}

/// Pass-through: the pill never intercepts clicks, so the status item keeps
/// its click, menu and hover behavior.
extern "C" fn no_hit_test(_: &Object, _: Sel, _point: NSPoint) -> id {
    nil
}

extern "C" fn not_accessibility_element(_: &Object, _: Sel) -> BOOL {
    NO
}

extern "C" fn set_pill_locked(this: &mut Object, _: Sel, locked: BOOL) {
    // SAFETY: the receiver is the `&mut` self AppKit passes for this method, and
    // PILL_LOCKED_IVAR is registered as a BOOL ivar on this class.
    unsafe { this.set_ivar::<BOOL>(PILL_LOCKED_IVAR, locked) };
}

extern "C" fn pill_locked(this: &Object, _: Sel) -> BOOL {
    unsafe { *this.get_ivar::<BOOL>(PILL_LOCKED_IVAR) }
}

extern "C" fn draw_pill(this: &Object, _: Sel, _dirty: NSRect) {
    unsafe {
        let view = this as *const Object as id;
        let bounds: NSRect = msg_send![view, bounds];
        let locked = pill_locked(this, sel!(pillLocked)) != NO;
        let fill: id = if locked {
            srgb(LOCK_FILL_RGB)
        } else {
            msg_send![class!(NSColor), systemRedColor]
        };
        let _: () = msg_send![fill, setFill];
        let radius = bounds.size.height / 2.0;
        let capsule: id = msg_send![
            class!(NSBezierPath),
            bezierPathWithRoundedRect: bounds
            xRadius: radius
            yRadius: radius
        ];
        let _: () = msg_send![capsule, fill];
        if locked {
            draw_lock_glyph(bounds);
        }
    }
}

unsafe fn draw_lock_glyph(bounds: NSRect) {
    let center_x = bounds.size.width / 2.0;
    let white: id = msg_send![class!(NSColor), whiteColor];
    let _: () = msg_send![white, setFill];
    let _: () = msg_send![white, setStroke];

    let body = NSRect::new(
        NSPoint::new(center_x - LOCK_BODY_WIDTH / 2.0, LOCK_BODY_Y),
        NSSize::new(LOCK_BODY_WIDTH, LOCK_BODY_HEIGHT),
    );
    let body_path: id = msg_send![
        class!(NSBezierPath),
        bezierPathWithRoundedRect: body
        xRadius: 1.0f64
        yRadius: 1.0f64
    ];
    let _: () = msg_send![body_path, fill];

    let shackle: id = msg_send![class!(NSBezierPath), bezierPath];
    let _: () = msg_send![shackle, setLineWidth: LOCK_SHACKLE_LINE_WIDTH];
    let left_leg_foot = NSPoint::new(center_x - LOCK_SHACKLE_RADIUS, lock_body_top());
    let _: () = msg_send![shackle, moveToPoint: left_leg_foot];
    let _: () = msg_send![
        shackle,
        appendBezierPathWithArcWithCenter: NSPoint::new(center_x, LOCK_SHACKLE_CENTER_Y)
        radius: LOCK_SHACKLE_RADIUS
        startAngle: 180.0f64
        endAngle: 0.0f64
        clockwise: YES
    ];
    let right_leg_foot = NSPoint::new(center_x + LOCK_SHACKLE_RADIUS, lock_body_top());
    let _: () = msg_send![shackle, lineToPoint: right_leg_foot];
    let _: () = msg_send![shackle, stroke];
}

/// Top edge of the lock body; the shackle legs start here.
fn lock_body_top() -> f64 {
    LOCK_BODY_Y + LOCK_BODY_HEIGHT
}

/// Vertical centre of the drawn glyph, including half the shackle stroke.
#[cfg(test)]
fn lock_glyph_centre_y() -> f64 {
    let bottom = LOCK_BODY_Y;
    let top = LOCK_SHACKLE_CENTER_Y + LOCK_SHACKLE_RADIUS + LOCK_SHACKLE_LINE_WIDTH / 2.0;
    (bottom + top) / 2.0
}

unsafe fn srgb(rgb: [f64; 3]) -> id {
    msg_send![
        class!(NSColor),
        colorWithSRGBRed: rgb[0]
        green: rgb[1]
        blue: rgb[2]
        alpha: 1.0f64
    ]
}

unsafe fn ns_string(value: &str) -> id {
    let string: id = NSString::alloc(nil).init_str(value);
    msg_send![string, autorelease]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_TIERS: [u8; 5] = [0, 1, 2, 3, 4];
    /// Left inset of the logo in the measured button.
    const LOGO_LEFT: f64 = 8.0;

    /// Button width with the widened logo image: image + symmetric inset.
    fn widened_button_width(tier: u8) -> f64 {
        LOGO_LEFT + LOGO_PT + logo_widening(tier) + TRAILING_PAD_PT
    }

    #[test]
    fn pill_widths_follow_the_ux_tiers() {
        let widths = ALL_TIERS.map(pill_width);
        assert_eq!(widths, [0.0, 12.0, 17.0, 22.0, 16.0]);
    }

    #[test]
    fn logo_widening_matches_measured_padding() {
        let widening = ALL_TIERS.map(logo_widening);
        assert_eq!(widening, [0.0, 0.0, 5.0, 10.0, 4.0]);
    }

    #[test]
    fn widened_button_puts_the_pill_on_the_logo_overlap_for_every_tier() {
        let logo_right = LOGO_LEFT + LOGO_PT;
        for tier in 1..=4 {
            let button_width = widened_button_width(tier);
            let width = pill_width(tier);
            let x = pill_origin_x(logo_right, button_width, width);
            assert_eq!(x, logo_right - PILL_LOGO_OVERLAP, "tier {tier}");
            assert_eq!(button_width - (x + width), PILL_EDGE_INSET, "tier {tier}");
        }
    }

    #[test]
    fn pill_follows_the_logo_when_the_button_is_wider_than_the_logo_needs() {
        // AppKit centres the image in a wider button, so the logo moves right.
        // The pill must follow the logo, not the button edge.
        let width = pill_width(2);
        let logo_right = 29.0;
        let wide_button = 50.0;
        let x = pill_origin_x(logo_right, wide_button, width);
        assert_eq!(x, logo_right - PILL_LOGO_OVERLAP);
    }

    #[test]
    fn pill_is_clamped_inside_a_button_too_narrow_for_the_overlap() {
        let logo_right = LOGO_LEFT + LOGO_PT;
        let button_width = LOGO_LEFT + LOGO_PT + TRAILING_PAD_PT;
        let width = pill_width(3);
        let x = pill_origin_x(logo_right, button_width, width);
        assert_eq!(x + width, button_width - PILL_EDGE_INSET);
    }

    #[test]
    fn pill_hangs_below_the_logo_in_unflipped_coordinates() {
        let button_height = 24.0;
        let logo_bottom = (button_height - LOGO_PT) / 2.0;
        let y = pill_origin_y(button_height, false);
        assert_eq!(y, logo_bottom - 1.0);
        assert_eq!(y + PILL_HEIGHT, logo_bottom + 11.0);
        assert!(y + PILL_HEIGHT > logo_bottom);
    }

    #[test]
    fn flipped_button_places_the_pill_at_the_same_visual_position() {
        for height in [22.0, 24.0] {
            let unflipped_y = pill_origin_y(height, false);
            let flipped_y = pill_origin_y(height, true);
            // Visual bottom in unflipped coordinates equals the top-origin measurement.
            assert_eq!(
                height - (flipped_y + PILL_HEIGHT),
                unflipped_y,
                "height {height}"
            );
        }
    }

    #[test]
    fn measured_22_point_button_puts_the_pill_one_point_below_the_logo() {
        // Measured 22 pt button (flipped): logo spans 2..20 from the top, pill 9..21.
        assert_eq!(pill_origin_y(22.0, true), 9.0);
        assert_eq!(pill_origin_y(22.0, false), 1.0);
    }

    #[test]
    fn autoresizing_keeps_the_right_gap_and_the_visual_bottom_gap() {
        assert_eq!(pill_autoresizing_mask(false), 1 | 32);
        assert_eq!(pill_autoresizing_mask(true), 1 | 8);
    }

    #[test]
    fn lock_glyph_is_vertically_centred_in_the_pill() {
        // The glyph spans from the body bottom to the shackle's outer top. Its centre
        // must sit within a quarter point of the 12 pt pill's centre line.
        assert!(
            (lock_glyph_centre_y() - PILL_HEIGHT / 2.0).abs() <= 0.25,
            "glyph centre {}",
            lock_glyph_centre_y()
        );
    }

    #[test]
    fn lock_glyph_has_the_specified_legs_and_clearance() {
        assert_eq!(
            LOCK_SHACKLE_CENTER_Y - lock_body_top(),
            0.75,
            "shackle leg length"
        );
        let top = LOCK_SHACKLE_CENTER_Y + LOCK_SHACKLE_RADIUS + LOCK_SHACKLE_LINE_WIDTH / 2.0;
        assert!(top <= PILL_HEIGHT - 1.0, "top clearance {top}");
    }

    #[test]
    fn vertical_origin_snaps_to_half_points() {
        assert_eq!(pill_origin_y(23.0, false), 1.5);
        assert_eq!(snap_half(2.3), 2.5);
        assert_eq!(snap_half(2.2), 2.0);
    }

    #[test]
    fn pill_for_maps_each_badge_state() {
        assert_eq!(pill_for(&TrayBadge::Hidden), None);
        assert_eq!(
            pill_for(&TrayBadge::Locked),
            Some(Pill {
                text: None,
                locked: true,
                width: 16.0,
            })
        );
        assert_eq!(
            pill_for(&TrayBadge::Unread(7)),
            Some(Pill {
                text: Some("7".into()),
                locked: false,
                width: 12.0,
            })
        );
        assert_eq!(
            pill_for(&TrayBadge::Unread(u64::MAX)),
            Some(Pill {
                text: Some("99+".into()),
                locked: false,
                width: 22.0,
            })
        );
    }

    #[test]
    fn pad_right_keeps_pixels_and_adds_transparent_columns() {
        let source = [1, 2, 3, 4, 5, 6, 7, 8];
        let padded = pad_right(&source, 2, 1, 1);
        assert_eq!(padded, [1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0]);
    }

    #[test]
    fn pad_right_pads_every_row() {
        let source = [1, 2, 3, 4, 9, 9, 9, 9];
        let padded = pad_right(&source, 1, 2, 1);
        assert_eq!(padded, [1, 2, 3, 4, 0, 0, 0, 0, 9, 9, 9, 9, 0, 0, 0, 0]);
    }

    #[test]
    fn embedded_logo_is_the_36_pixel_template() {
        assert_eq!(&LOGO_ASSET[..8], b"\x89PNG\r\n\x1a\n");
        let width = u32::from_be_bytes(LOGO_ASSET[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(LOGO_ASSET[20..24].try_into().unwrap());
        assert_eq!((width, height), (36, 36));
    }
}
