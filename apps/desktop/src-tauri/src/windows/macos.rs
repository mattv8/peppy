//! Public-AppKit implementation of native floating conversation heads.
//!
//! All AppKit objects live in the main-thread registry.  Callbacks are cloned
//! while that registry is borrowed and invoked after the borrow ends.

// Match the existing native-dialog adapter: cocoa is the retained public AppKit
// binding, and objc's macros still emit the legacy cargo-clippy feature check.
#![allow(deprecated, unexpected_cfgs)]

use std::{cell::RefCell, collections::HashMap, ffi::c_void, sync::OnceLock};

use cocoa::{
    appkit::{
        NSBackingStoreType, NSColor, NSPanel, NSTextField, NSView, NSWindow,
        NSWindowCollectionBehavior, NSWindowStyleMask,
    },
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
use tauri::{AppHandle, Monitor};

use super::{HeadCallback, HeadEvent, HeadFrame, HeadGlyph, HeadPosition, HeadSpec};

const HEAD_SIZE: f64 = 56.0;
const CIRCLE_SIZE: f64 = 48.0;
const BADGE_HEIGHT: f64 = 18.0;
const FLOATING_WINDOW_LEVEL: i64 = 3; // NSFloatingWindowLevel
const MAX_HEADS: usize = 8;
const DRAG_THRESHOLD: f64 = 4.0;
const NS_NONACTIVATING_PANEL_MASK: u64 = 1 << 7;
const MONITOR_GEOMETRY_TOLERANCE: f64 = 0.5;
const IMAGE_SCALING_PROPORTIONALLY_UP_OR_DOWN: i64 = 3;
const CG_IMAGE_ALPHA_NONE_SKIP_LAST: u32 = 5;

type CFTypeRef = *const c_void;
type CFDataRef = *const c_void;
type CFStringRef = *const c_void;
type CGColorSpaceRef = *mut c_void;
type CGDataProviderRef = *mut c_void;
type CGImageRef = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFDataRef;
    fn CFRelease(value: CFTypeRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGColorSpaceSRGB: CFStringRef;
    fn CGColorSpaceCreateWithName(name: CFStringRef) -> CGColorSpaceRef;
    fn CGDataProviderCreateWithCFData(data: CFDataRef) -> CGDataProviderRef;
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
        provider: CGDataProviderRef,
        decode: *const f64,
        should_interpolate: BOOL,
        intent: u32,
    ) -> CGImageRef;
}

struct Head {
    panel: id,
    views: HeadViews,
    spec: HeadSpec,
    frame: HeadFrame,
    events: HeadCallback,
    drag_origin: Option<NSPoint>,
    drag_frame: Option<HeadFrame>,
    dragged: bool,
}

#[derive(Clone, Copy)]
struct HeadViews {
    view: id,
    circle: id,
    initials: id,
    photo: id,
    person: id,
    group: id,
    badge: id,
    badge_label: id,
}

#[derive(Default)]
struct Registry {
    heads: HashMap<String, Head>,
    views: HashMap<usize, String>,
    screen_observer: Option<usize>,
}

thread_local! {
    static HEADS: RefCell<Registry> = RefCell::new(Registry::default());
}

static APP: OnceLock<AppHandle> = OnceLock::new();

fn with_head<R>(view: &Object, f: impl FnOnce(&mut Head) -> R) -> Option<R> {
    HEADS.with(|registry| {
        let mut registry = registry.borrow_mut();
        let conversation_id = registry
            .views
            .get(&(view as *const Object as usize))?
            .clone();
        registry.heads.get_mut(&conversation_id).map(f)
    })
}

fn emit(callback: HeadCallback, event: HeadEvent) {
    callback(event);
}

fn is_in_circle(point: NSPoint) -> bool {
    let dx = point.x - CIRCLE_SIZE / 2.0;
    let dy = point.y - (HEAD_SIZE - CIRCLE_SIZE / 2.0);
    dx * dx + dy * dy <= (CIRCLE_SIZE / 2.0) * (CIRCLE_SIZE / 2.0)
}

fn badge_width(unread: u64) -> f64 {
    match unread {
        0 => 0.0,
        1..=9 => 18.0,
        10..=99 => 22.0,
        _ => 26.0,
    }
}

fn badge_frame(unread: u64) -> NSRect {
    let width = badge_width(unread);
    NSRect::new(
        NSPoint::new(41.0 - width / 2.0, 6.0),
        NSSize::new(width, BADGE_HEIGHT),
    )
}

fn badge_label_frame(width: f64, label_height: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(0.0, (BADGE_HEIGHT - label_height) / 2.0),
        NSSize::new(width, label_height),
    )
}

fn is_in_badge(point: NSPoint, unread: u64) -> bool {
    if unread == 0 {
        return false;
    }
    let frame = badge_frame(unread);
    point.x >= frame.origin.x
        && point.x <= frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y <= frame.origin.y + frame.size.height
}

fn head_view_class() -> *const Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let mut decl = ClassDecl::new("PeppyCircularHeadView", class!(NSView))
            .expect("head view class must register once");
        decl.add_method(
            sel!(hitTest:),
            head_hit_test as extern "C" fn(&Object, Sel, NSPoint) -> id,
        );
        decl.add_method(
            sel!(acceptsFirstMouse:),
            accepts_first_mouse as extern "C" fn(&Object, Sel, id) -> BOOL,
        );
        decl.add_method(
            sel!(isAccessibilityElement),
            is_accessibility_element as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(accessibilityRole),
            accessibility_role as extern "C" fn(&Object, Sel) -> id,
        );
        decl.add_method(
            sel!(accessibilityActionNames),
            accessibility_action_names as extern "C" fn(&Object, Sel) -> id,
        );
        decl.add_method(
            sel!(accessibilityPerformAction:),
            accessibility_perform_action as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(accessibilityPerformPress),
            accessibility_press as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(accessibilityPerformShowMenu),
            accessibility_show_menu as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(mouseDown:),
            head_mouse_down as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(mouseDragged:),
            head_mouse_dragged as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(mouseUp:),
            head_mouse_up as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(rightMouseDown:),
            head_right_mouse_down as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(peppyActivate:),
            head_activate as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(peppyDismiss:),
            head_dismiss as extern "C" fn(&Object, Sel, id),
        );
        decl.register() as *const Class as usize
    }) as *const Class
}

fn person_fallback_view_class() -> *const Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let mut decl = ClassDecl::new("PeppyPersonFallbackView", class!(NSView))
            .expect("person fallback class must register once");
        decl.add_method(
            sel!(drawRect:),
            draw_person_fallback as extern "C" fn(&Object, Sel, NSRect),
        );
        decl.register() as *const Class as usize
    }) as *const Class
}

extern "C" fn draw_person_fallback(_: &Object, _: Sel, _: NSRect) {
    unsafe {
        let white: id = msg_send![class!(NSColor), whiteColor];
        let _: () = msg_send![white, setFill];
        let head: id = msg_send![class!(NSBezierPath), bezierPathWithOvalInRect: NSRect::new(NSPoint::new(8.0, 13.0), NSSize::new(8.0, 8.0))];
        let _: () = msg_send![head, fill];
        let shoulders: id = msg_send![class!(NSBezierPath), bezierPathWithOvalInRect: NSRect::new(NSPoint::new(2.0, 3.0), NSSize::new(20.0, 12.0))];
        let _: () = msg_send![shoulders, fill];
    }
}

extern "C" fn accepts_first_mouse(_: &Object, _: Sel, _: id) -> BOOL {
    YES
}
extern "C" fn is_accessibility_element(_: &Object, _: Sel) -> BOOL {
    YES
}
extern "C" fn accessibility_role(_: &Object, _: Sel) -> id {
    unsafe { ns_string("AXButton") }
}
extern "C" fn accessibility_action_names(_: &Object, _: Sel) -> id {
    unsafe {
        let actions: id = msg_send![class!(NSMutableArray), array];
        let _: () = msg_send![actions, addObject: ns_string("AXPress")];
        let _: () = msg_send![actions, addObject: ns_string("AXShowMenu")];
        actions
    }
}
extern "C" fn accessibility_perform_action(view: &Object, _: Sel, action: id) {
    unsafe {
        let press: BOOL = msg_send![action, isEqualToString: ns_string("AXPress")];
        if press == YES {
            let _ = accessibility_press(view, sel!(accessibilityPerformPress));
            return;
        }
        let show_menu: BOOL = msg_send![action, isEqualToString: ns_string("AXShowMenu")];
        if show_menu == YES {
            let _ = accessibility_show_menu(view, sel!(accessibilityPerformShowMenu));
        }
    }
}
extern "C" fn accessibility_press(view: &Object, _: Sel) -> BOOL {
    emit_activation(view);
    YES
}
extern "C" fn accessibility_show_menu(view: &Object, _: Sel) -> BOOL {
    head_right_mouse_down(view, sel!(rightMouseDown:), nil);
    YES
}

fn screen_observer_class() -> *const Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let mut decl = ClassDecl::new("PeppyScreenObserver", class!(NSObject))
            .expect("screen observer class must register once");
        decl.add_method(
            sel!(screenParametersChanged:),
            screen_parameters_changed as extern "C" fn(&Object, Sel, id),
        );
        decl.register() as *const Class as usize
    }) as *const Class
}

extern "C" fn screen_parameters_changed(_: &Object, _: Sel, _: id) {
    if let Some(app) = APP.get() {
        let _ = reconcile_heads(app);
    }
}

extern "C" fn head_hit_test(view: &Object, _: Sel, point: NSPoint) -> id {
    let in_badge = with_head(view, |head| is_in_badge(point, head.spec.unread)).unwrap_or(false);
    if is_in_circle(point) || in_badge {
        view as *const Object as id
    } else {
        nil
    }
}

extern "C" fn head_mouse_down(view: &Object, _: Sel, _event: id) {
    unsafe {
        let point: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let _ = with_head(view, |head| {
            head.drag_origin = Some(point);
            head.drag_frame = Some(head.frame.clone());
            head.dragged = false;
        });
    }
}

extern "C" fn head_mouse_dragged(view: &Object, _: Sel, _event: id) {
    unsafe {
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let state = with_head(view, |head| {
            (head.drag_origin, head.drag_frame.clone(), head.dragged)
        });
        let Some((Some(origin), Some(start_frame), was_dragged)) = state else {
            return;
        };
        let dx = cursor.x - origin.x;
        let dy = cursor.y - origin.y;
        if !was_dragged && dx * dx + dy * dy < DRAG_THRESHOLD * DRAG_THRESHOLD {
            return;
        }
        let Some(app) = APP.get() else {
            return;
        };
        let Ok((position, updated)) = drag_frame_for_cursor(app, origin, cursor, &start_frame)
        else {
            return;
        };
        let event = with_head(view, |head| {
            head.dragged = true;
            head.spec.position = position.clone();
            head.frame = updated.clone();
            (
                head.panel,
                head.events.clone(),
                HeadEvent::Moved {
                    conversation_id: head.spec.conversation_id.clone(),
                    generation: head.spec.generation,
                    position,
                    frame: updated,
                    settled: false,
                },
            )
        });
        if let Some((panel, callback, event)) = event {
            set_panel_frame(
                panel,
                match &event {
                    HeadEvent::Moved { frame, .. } => frame,
                    _ => unreachable!(),
                },
            );
            emit(callback, event);
        }
    }
}

extern "C" fn head_mouse_up(view: &Object, _: Sel, _: id) {
    let event = with_head(view, |head| {
        let was_dragged = head.dragged;
        head.drag_origin = None;
        head.drag_frame = None;
        if was_dragged {
            let frame = head.frame.clone();
            Some((
                head.events.clone(),
                HeadEvent::Moved {
                    conversation_id: head.spec.conversation_id.clone(),
                    generation: head.spec.generation,
                    position: head.spec.position.clone(),
                    frame,
                    settled: true,
                },
            ))
        } else {
            Some((
                head.events.clone(),
                HeadEvent::Activate {
                    conversation_id: head.spec.conversation_id.clone(),
                    generation: head.spec.generation,
                },
            ))
        }
    });
    if let Some(Some((callback, event))) = event {
        emit(callback, event);
    }
}

fn emit_activation(view: &Object) {
    let event = with_head(view, |head| {
        (
            head.events.clone(),
            HeadEvent::Activate {
                conversation_id: head.spec.conversation_id.clone(),
                generation: head.spec.generation,
            },
        )
    });
    if let Some((callback, event)) = event {
        emit(callback, event);
    }
}

extern "C" fn head_activate(view: &Object, _: Sel, _: id) {
    emit_activation(view);
}

extern "C" fn head_dismiss(view: &Object, _: Sel, _: id) {
    let event = with_head(view, |head| {
        (
            head.events.clone(),
            HeadEvent::Dismiss {
                conversation_id: head.spec.conversation_id.clone(),
                generation: head.spec.generation,
            },
        )
    });
    if let Some((callback, event)) = event {
        emit(callback, event);
    }
}

extern "C" fn head_right_mouse_down(view: &Object, _: Sel, event: id) {
    unsafe {
        let menu: id = msg_send![class!(NSMenu), alloc];
        let menu: id = msg_send![menu, initWithTitle: ns_string("Conversation head")];
        add_menu_item(
            menu,
            "Open conversation",
            sel!(peppyActivate:),
            view as *const Object as id,
        );
        add_menu_item(
            menu,
            "Dismiss head",
            sel!(peppyDismiss:),
            view as *const Object as id,
        );
        let _: () = msg_send![menu, popUpMenuPositioningItem: nil atLocation: NSPoint::new(0.0, 0.0) inView: view];
        let _: () = msg_send![menu, release];
        let _: id = event;
    }
}

unsafe fn add_menu_item(menu: id, title: &str, action: Sel, target: id) {
    let item: id = msg_send![class!(NSMenuItem), alloc];
    let item: id = msg_send![item, initWithTitle: ns_string(title) action: action keyEquivalent: ns_string("")];
    let _: () = msg_send![item, setTarget: target];
    let _: () = msg_send![menu, addItem: item];
    let _: () = msg_send![item, release];
}

unsafe fn ns_string(value: &str) -> id {
    let value = cocoa::foundation::NSString::alloc(nil).init_str(value);
    let _: id = msg_send![value, autorelease];
    value
}

fn monitor_for(app: &AppHandle, requested: Option<&str>) -> Result<Monitor, String> {
    let monitors = app
        .available_monitors()
        .map_err(|error| error.to_string())?;
    if let Some(index) = unique_monitor_name(
        requested,
        monitors
            .iter()
            .map(|monitor| monitor.name().map(String::as_str)),
    ) {
        return Ok(monitors[index].clone());
    }
    // Fall back to primary monitor for missing, ambiguous, or unspecified names.
    app.primary_monitor()
        .ok()
        .flatten()
        .ok_or_else(|| "no monitor is available for the floating head".into())
}

fn unique_monitor_name<'a>(
    requested: Option<&str>,
    names: impl Iterator<Item = Option<&'a str>>,
) -> Option<usize> {
    let name = requested.filter(|name| !name.is_empty())?;
    let mut matches = names
        .enumerate()
        .filter(|(_, candidate)| *candidate == Some(name));
    let (index, _) = matches.next()?;
    matches.next().is_none().then_some(index)
}

#[derive(Clone, Copy)]
struct MonitorGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

fn monitor_geometry(monitor: &Monitor) -> MonitorGeometry {
    MonitorGeometry {
        x: monitor.position().x as f64,
        y: monitor.position().y as f64,
        width: monitor.size().width as f64,
        height: monitor.size().height as f64,
        scale: monitor.scale_factor(),
    }
}

fn matches_screen_geometry(
    screen: NSRect,
    screen_scale: f64,
    primary_height: f64,
    monitor: MonitorGeometry,
) -> bool {
    if !screen_scale.is_finite()
        || screen_scale <= 0.0
        || !monitor.scale.is_finite()
        || monitor.scale <= 0.0
        || (screen_scale - monitor.scale).abs() > 1e-6
    {
        return false;
    }
    let logical_y = primary_height - screen.origin.y - screen.size.height;
    (screen.origin.x - monitor.x / monitor.scale).abs() <= MONITOR_GEOMETRY_TOLERANCE
        && (logical_y - monitor.y / monitor.scale).abs() <= MONITOR_GEOMETRY_TOLERANCE
        && (screen.size.width - monitor.width / monitor.scale).abs() <= MONITOR_GEOMETRY_TOLERANCE
        && (screen.size.height - monitor.height / monitor.scale).abs() <= MONITOR_GEOMETRY_TOLERANCE
}

fn cursor_in_monitor(screen: NSRect, point: NSPoint, monitor: MonitorGeometry) -> (f64, f64) {
    (
        monitor.x + (point.x - screen.origin.x) * monitor.scale,
        monitor.y + (screen.origin.y + screen.size.height - point.y) * monitor.scale,
    )
}

unsafe fn screen_for_point(point: NSPoint) -> Option<(NSRect, f64)> {
    let screens: id = msg_send![class!(NSScreen), screens];
    let count: usize = msg_send![screens, count];
    for index in 0..count {
        let screen: id = msg_send![screens, objectAtIndex: index];
        let frame: NSRect = msg_send![screen, frame];
        if point.x >= frame.origin.x
            && point.x < frame.origin.x + frame.size.width
            && point.y >= frame.origin.y
            && point.y < frame.origin.y + frame.size.height
        {
            let scale: f64 = msg_send![screen, backingScaleFactor];
            return Some((frame, scale));
        }
    }
    None
}

fn monitor_for_geometry(
    app: &AppHandle,
    screen_frame: NSRect,
    screen_scale: f64,
    primary_height: f64,
) -> Result<Monitor, String> {
    let monitors = app
        .available_monitors()
        .map_err(|error| error.to_string())?;
    monitors
        .into_iter()
        .find(|monitor| {
            matches_screen_geometry(
                screen_frame,
                screen_scale,
                primary_height,
                monitor_geometry(monitor),
            )
        })
        .ok_or_else(|| "transient unmatched geometry during drag".into())
}

unsafe fn physical_cursor(app: &AppHandle, point: NSPoint) -> Result<(Monitor, f64, f64), String> {
    let (screen_frame, screen_scale) =
        screen_for_point(point).ok_or("cursor is not on an available screen")?;

    // Get primary screen height for coordinate conversion
    let screens: id = msg_send![class!(NSScreen), screens];
    let count: usize = msg_send![screens, count];
    if count == 0 {
        return Err("no monitor is available for the floating head".into());
    }
    let primary: id = msg_send![screens, objectAtIndex: 0usize];
    let primary_frame: NSRect = msg_send![primary, frame];
    let primary_height = primary_frame.size.height;

    // Use geometry-based matching instead of name or index fallback
    let monitor = monitor_for_geometry(app, screen_frame, screen_scale, primary_height)?;
    let (x, y) = cursor_in_monitor(screen_frame, point, monitor_geometry(&monitor));
    Ok((monitor, x, y))
}

fn drag_frame_in_work_area(
    monitor_name: Option<String>,
    work: (f64, f64, f64, f64),
    scale: f64,
    cursor: (f64, f64),
    grab_offset: (f64, f64),
) -> (HeadPosition, HeadFrame) {
    let size = HEAD_SIZE * scale;
    let mut x = cursor.0 - grab_offset.0 * scale;
    let mut y = cursor.1 - grab_offset.1 * scale;
    x = x.clamp(work.0, (work.0 + work.2 - size).max(work.0));
    y = y.clamp(work.1, (work.1 + work.3 - size).max(work.1));
    (
        HeadPosition {
            monitor: monitor_name,
            x: (x - work.0) / scale,
            y: (y - work.1) / scale,
        },
        HeadFrame {
            x,
            y,
            width: size,
            height: size,
            work_x: work.0,
            work_y: work.1,
            work_width: work.2,
            work_height: work.3,
            scale_factor: scale,
        },
    )
}

unsafe fn drag_frame_for_cursor(
    app: &AppHandle,
    origin: NSPoint,
    cursor: NSPoint,
    start: &HeadFrame,
) -> Result<(HeadPosition, HeadFrame), String> {
    let (_, origin_x, origin_y) = physical_cursor(app, origin)?;
    let (monitor, cursor_x, cursor_y) = physical_cursor(app, cursor)?;
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    Ok(drag_frame_in_work_area(
        monitor.name().cloned(),
        (
            work.position.x as f64,
            work.position.y as f64,
            work.size.width as f64,
            work.size.height as f64,
        ),
        scale,
        (cursor_x, cursor_y),
        (
            (origin_x - start.x) / start.scale_factor,
            (origin_y - start.y) / start.scale_factor,
        ),
    ))
}

fn resolved_position(
    app: &AppHandle,
    position: &HeadPosition,
) -> Result<(HeadPosition, HeadFrame), String> {
    let monitor = monitor_for(app, position.monitor.as_deref())?;
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    let logical_width = work.size.width as f64 / scale;
    let logical_height = work.size.height as f64 / scale;
    let x = if position.x.is_finite() {
        position.x.clamp(0.0, (logical_width - HEAD_SIZE).max(0.0))
    } else {
        0.0
    };
    let y = if position.y.is_finite() {
        position.y.clamp(0.0, (logical_height - HEAD_SIZE).max(0.0))
    } else {
        0.0
    };
    let frame = HeadFrame {
        x: work.position.x as f64 + x * scale,
        y: work.position.y as f64 + y * scale,
        width: HEAD_SIZE * scale,
        height: HEAD_SIZE * scale,
        work_x: work.position.x as f64,
        work_y: work.position.y as f64,
        work_width: work.size.width as f64,
        work_height: work.size.height as f64,
        scale_factor: scale,
    };
    Ok((
        HeadPosition {
            monitor: monitor.name().cloned(),
            x,
            y,
        },
        frame,
    ))
}

/// Converts top-left physical desktop coordinates to AppKit's bottom-left
/// desktop coordinate space. AppKit panel placement remains on its UI thread.
unsafe fn set_panel_frame(panel: id, frame: &HeadFrame) {
    let screens: id = msg_send![class!(NSScreen), screens];
    let primary: id = msg_send![screens, objectAtIndex: 0usize];
    let primary_frame: NSRect = msg_send![primary, frame];
    let point = NSPoint::new(
        frame.x / frame.scale_factor,
        primary_frame.size.height - (frame.y + frame.height) / frame.scale_factor,
    );
    let _: () = msg_send![panel, setFrameOrigin: point];
}

fn update_head_content(views: HeadViews, spec: &HeadSpec, previous_glyph: Option<&HeadGlyph>) {
    unsafe {
        apply_color(views.circle, views.badge, spec.color, &spec.glyph);
        apply_glyph(views, &spec.glyph, previous_glyph);
        let text = if spec.unread >= 100 {
            "99+".to_owned()
        } else {
            spec.unread.to_string()
        };
        let _: () = msg_send![views.badge_label, setStringValue: ns_string(&text)];
        let _: () = msg_send![views.badge, setFrame: badge_frame(spec.unread)];
        let _: () = msg_send![views.badge_label, sizeToFit];
        let natural_frame: NSRect = msg_send![views.badge_label, frame];
        let _: () = msg_send![views.badge_label, setFrame: badge_label_frame(badge_width(spec.unread), natural_frame.size.height)];
        let _: () = msg_send![views.badge, setHidden: if spec.unread == 0 { YES } else { NO }];
        let label = ns_string(&format!("{}, {} unread messages", spec.label, spec.unread));
        let _: () = msg_send![views.view, setAccessibilityLabel: label];
        let _: () = msg_send![views.view, setToolTip: label];
    }
}

unsafe fn apply_color(circle: id, badge: id, color: [u8; 3], glyph: &HeadGlyph) {
    let circle_color = srgb_color(color);
    let circle_color: id = msg_send![circle_color, CGColor];
    let circle_layer: id = msg_send![circle, layer];
    let _: () = msg_send![circle_layer, setBackgroundColor: circle_color];
    let badge_layer: id = msg_send![badge, layer];
    let badge_ring = srgb_color(badge_ring_color(glyph, color));
    let badge_ring: id = msg_send![badge_ring, CGColor];
    let _: () = msg_send![badge_layer, setBorderColor: badge_ring];
}

fn badge_ring_color(glyph: &HeadGlyph, color: [u8; 3]) -> [u8; 3] {
    matches!(glyph, HeadGlyph::Photo(_))
        .then_some([31, 31, 31])
        .unwrap_or(color)
}

unsafe fn apply_glyph(views: HeadViews, glyph: &HeadGlyph, previous: Option<&HeadGlyph>) {
    let mut visible = views.person;
    let mut keeps_photo = false;
    if let HeadGlyph::Initials(value) = glyph {
        if !value.is_empty() {
            let _: () = msg_send![views.initials, setStringValue: ns_string(value)];
            visible = views.initials;
        }
    } else if let HeadGlyph::Photo(photo_spec) = glyph {
        keeps_photo = matches!(previous, Some(HeadGlyph::Photo(previous_photo)) if previous_photo == photo_spec);
        if !keeps_photo {
            let image = photo_image(photo_spec.edge, &photo_spec.rgba);
            if image != nil {
                let _: () = msg_send![views.photo, setImage: image];
                let _: () = msg_send![image, release];
                keeps_photo = true;
            }
        }
        if keeps_photo {
            visible = views.photo;
        }
    } else if matches!(glyph, HeadGlyph::Group) {
        visible = views.group;
    }
    // Never retain a contact photo the head no longer shows.
    if !keeps_photo {
        let _: () = msg_send![views.photo, setImage: nil];
    }
    for glyph_view in [views.initials, views.photo, views.person, views.group] {
        let _: () = msg_send![glyph_view, setHidden: if glyph_view == visible { NO } else { YES }];
    }
}

fn create_head(
    app: &AppHandle,
    spec: HeadSpec,
    events: HeadCallback,
) -> Result<(HeadPosition, HeadFrame), String> {
    let (position, frame) = resolved_position(app, &spec.position)?;
    if HEADS.with(|registry| registry.borrow().heads.len() >= MAX_HEADS) {
        return Err(format!("native head limit ({MAX_HEADS}) reached"));
    }
    unsafe {
        let panel: id = NSPanel::alloc(nil).initWithContentRect_styleMask_backing_defer_(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(HEAD_SIZE, HEAD_SIZE)),
            NSWindowStyleMask::from_bits_truncate(NS_NONACTIVATING_PANEL_MASK),
            NSBackingStoreType::NSBackingStoreBuffered,
            NO,
        );
        panel.setOpaque_(NO);
        panel.setBackgroundColor_(NSColor::clearColor(nil));
        panel.setHasShadow_(YES);
        panel.setHidesOnDeactivate_(NO);
        panel.setLevel_(FLOATING_WINDOW_LEVEL);
        panel.setCollectionBehavior_(
            NSWindowCollectionBehavior::NSWindowCollectionBehaviorCanJoinAllSpaces
                | NSWindowCollectionBehavior::NSWindowCollectionBehaviorStationary,
        );
        let view: id = msg_send![head_view_class(), alloc];
        let view: id = msg_send![view, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(HEAD_SIZE, HEAD_SIZE))];
        let circle = NSView::initWithFrame_(
            NSView::alloc(nil),
            NSRect::new(
                NSPoint::new(0.0, 8.0),
                NSSize::new(CIRCLE_SIZE, CIRCLE_SIZE),
            ),
        );
        circle.setWantsLayer(YES);
        let circle_layer: id = msg_send![circle, layer];
        let _: () = msg_send![circle_layer, setMasksToBounds: YES];
        let _: () = msg_send![circle_layer, setCornerRadius: CIRCLE_SIZE / 2.0];
        let initials = label(NSRect::new(
            NSPoint::new(1.0, 10.0),
            NSSize::new(46.0, 28.0),
        ));
        style_initials(initials);
        let photo = photo_view();
        let person = person_icon();
        let group = group_icon();
        let badge = NSView::initWithFrame_(NSView::alloc(nil), badge_frame(spec.unread));
        style_badge_container(badge);
        let badge_label = label(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(badge_width(spec.unread), BADGE_HEIGHT),
        ));
        style_badge_label(badge_label);
        let _: () = msg_send![circle, addSubview: initials];
        let _: () = msg_send![initials, release];
        let _: () = msg_send![circle, addSubview: photo];
        let _: () = msg_send![photo, release];
        let _: () = msg_send![circle, addSubview: person];
        let _: () = msg_send![person, release];
        let _: () = msg_send![circle, addSubview: group];
        let _: () = msg_send![group, release];
        let _: () = msg_send![view, addSubview: circle];
        let _: () = msg_send![circle, release];
        let _: () = msg_send![badge, addSubview: badge_label];
        let _: () = msg_send![badge_label, release];
        let _: () = msg_send![view, addSubview: badge];
        let _: () = msg_send![badge, release];
        panel.setContentView_(view);
        let _: () = msg_send![view, release];
        set_panel_frame(panel, &frame);
        panel.orderFrontRegardless();
        let mut spec = spec;
        spec.position = position.clone();
        let head = Head {
            panel,
            views: HeadViews {
                view,
                circle,
                initials,
                photo,
                person,
                group,
                badge,
                badge_label,
            },
            spec,
            frame: frame.clone(),
            events,
            drag_origin: None,
            drag_frame: None,
            dragged: false,
        };
        update_head_content(head.views, &head.spec, None);
        HEADS.with(|registry| {
            let mut registry = registry.borrow_mut();
            registry
                .views
                .insert(head.views.view as usize, head.spec.conversation_id.clone());
            registry
                .heads
                .insert(head.spec.conversation_id.clone(), head);
        });
        ensure_screen_observer();
    }
    Ok((position, frame))
}

#[cfg(target_arch = "aarch64")]
const TEXT_ALIGNMENT_CENTER: i64 = 1;
#[cfg(not(target_arch = "aarch64"))]
const TEXT_ALIGNMENT_CENTER: i64 = 2;

unsafe fn label(frame: NSRect) -> id {
    let field = NSTextField::initWithFrame_(NSTextField::alloc(nil), frame);
    let _: () = msg_send![field, setEditable: NO];
    let _: () = msg_send![field, setSelectable: NO];
    let _: () = msg_send![field, setBezeled: NO];
    let _: () = msg_send![field, setDrawsBackground: NO];
    let _: () = msg_send![field, setAlignment: TEXT_ALIGNMENT_CENTER];
    let white: id = msg_send![class!(NSColor), whiteColor];
    let _: () = msg_send![field, setTextColor: white];
    field
}

unsafe fn style_initials(initials: id) {
    let font: id = msg_send![class!(NSFont), boldSystemFontOfSize: 18.0f64];
    let _: () = msg_send![initials, setFont: font];
}

unsafe fn srgb_color([red, green, blue]: [u8; 3]) -> id {
    msg_send![class!(NSColor), colorWithSRGBRed: red as f64 / 255.0f64 green: green as f64 / 255.0f64 blue: blue as f64 / 255.0f64 alpha: 1.0f64]
}

unsafe fn photo_view() -> id {
    let image: id = msg_send![class!(NSImageView), alloc];
    let image: id = msg_send![image, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(CIRCLE_SIZE, CIRCLE_SIZE))];
    let _: () = msg_send![image, setImageScaling: IMAGE_SCALING_PROPORTIONALLY_UP_OR_DOWN];
    image
}

unsafe fn photo_image(edge: u32, rgba: &[u8]) -> id {
    let edge = edge as usize;
    let Some(byte_count) = edge
        .checked_mul(edge)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return nil;
    };
    if edge == 0 || rgba.len() != byte_count || byte_count > isize::MAX as usize {
        return nil;
    }
    let data = CFDataCreate(std::ptr::null(), rgba.as_ptr(), byte_count as isize);
    if data.is_null() {
        return nil;
    }
    let color_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    let provider = CGDataProviderCreateWithCFData(data);
    let image = if color_space.is_null() || provider.is_null() {
        std::ptr::null_mut()
    } else {
        CGImageCreate(
            edge,
            edge,
            8,
            32,
            edge * 4,
            color_space,
            CG_IMAGE_ALPHA_NONE_SKIP_LAST,
            provider,
            std::ptr::null(),
            YES,
            0,
        )
    };
    if !provider.is_null() {
        CFRelease(provider.cast());
    }
    if !color_space.is_null() {
        CFRelease(color_space.cast());
    }
    CFRelease(data);
    if image.is_null() {
        return nil;
    }
    let ns_image: id = msg_send![class!(NSImage), alloc];
    let ns_image: id =
        msg_send![ns_image, initWithCGImage: image size: NSSize::new(CIRCLE_SIZE, CIRCLE_SIZE)];
    CFRelease(image.cast());
    ns_image
}

unsafe fn person_icon() -> id {
    symbol_icon("person.fill")
}

unsafe fn group_icon() -> id {
    symbol_icon("person.2.fill")
}

unsafe fn symbol_icon(name: &str) -> id {
    let frame = NSRect::new(NSPoint::new(12.0, 12.0), NSSize::new(24.0, 24.0));
    let symbol: BOOL = msg_send![class!(NSImage), respondsToSelector: sel!(imageWithSystemSymbolName:accessibilityDescription:)];
    if symbol == YES {
        let image: id = msg_send![class!(NSImage), imageWithSystemSymbolName: ns_string(name) accessibilityDescription: nil];
        if image != nil {
            let config: id = msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: 24.0f64 weight: 0.23f64 scale: 1i64];
            let image: id = msg_send![image, imageWithSymbolConfiguration: config];
            let icon: id = msg_send![class!(NSImageView), alloc];
            let icon: id = msg_send![icon, initWithFrame: frame];
            let _: () = msg_send![icon, setImage: image];
            let white: id = msg_send![class!(NSColor), whiteColor];
            let _: () = msg_send![icon, setContentTintColor: white];
            return icon;
        }
    }
    let fallback: id = msg_send![person_fallback_view_class(), alloc];
    msg_send![fallback, initWithFrame: frame]
}

unsafe fn style_badge_container(badge: id) {
    let _: () = msg_send![badge, setWantsLayer: YES];
    let layer: id = msg_send![badge, layer];
    let red: id = msg_send![class!(NSColor), systemRedColor];
    let red: id = msg_send![red, CGColor];
    let _: () = msg_send![layer, setBackgroundColor: red];
    let _: () = msg_send![layer, setCornerRadius: 9.0f64];
    let _: () = msg_send![layer, setBorderWidth: 2.0f64];
}

unsafe fn style_badge_label(badge_label: id) {
    let font: id = msg_send![class!(NSFont), boldSystemFontOfSize: 10.0f64];
    let _: () = msg_send![badge_label, setFont: font];
    let _: () = msg_send![badge_label, setAccessibilityElement: NO];
}

unsafe fn ensure_screen_observer() {
    HEADS.with(|registry| {
        let mut registry = registry.borrow_mut();
        if registry.screen_observer.is_some() {
            return;
        }
        let observer: id = msg_send![screen_observer_class(), new];
        let center: id = msg_send![class!(NSNotificationCenter), defaultCenter];
        let _: () = msg_send![center, addObserver: observer selector: sel!(screenParametersChanged:) name: ns_string("NSApplicationDidChangeScreenParametersNotification") object: nil];
        registry.screen_observer = Some(observer as usize);
    });
}

unsafe fn remove_screen_observer_if_unused() {
    let observer = HEADS.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry
            .heads
            .is_empty()
            .then(|| registry.screen_observer.take())
            .flatten()
    });
    if let Some(observer) = observer {
        let observer = observer as id;
        let center: id = msg_send![class!(NSNotificationCenter), defaultCenter];
        let _: () = msg_send![center, removeObserver: observer];
        let _: id = msg_send![observer, autorelease];
    }
}

/// Removes notifications before allowing AppKit to retire the panel after the
/// current event stack unwinds. This avoids releasing a view while one of its
/// Objective-C methods is executing.
unsafe fn destroy_head(head: Head) {
    head.panel.orderOut_(nil);
    let _: id = msg_send![head.panel, autorelease];
}

/// Public AppKit display-change notification path. It owns the one allowed
/// geometry correction path; ordinary unread/initial updates never move heads.
fn reconcile_heads(app: &AppHandle) -> Result<(), String> {
    let ids = HEADS.with(|registry| registry.borrow().heads.keys().cloned().collect::<Vec<_>>());
    let mut notifications = Vec::new();
    for conversation_id in ids {
        let current = HEADS.with(|registry| {
            registry
                .borrow()
                .heads
                .get(&conversation_id)
                .map(|head| head.spec.clone())
        });
        let Some(current) = current else {
            continue;
        };
        let (position, frame) = resolved_position(app, &current.position)?;
        let notification = HEADS.with(|registry| {
            let mut registry = registry.borrow_mut();
            registry.heads.get_mut(&conversation_id).map(|head| {
                head.spec.position = position.clone();
                head.frame = frame.clone();
                (
                    head.panel,
                    head.events.clone(),
                    HeadEvent::Moved {
                        conversation_id: head.spec.conversation_id.clone(),
                        generation: head.spec.generation,
                        position: position.clone(),
                        frame: frame.clone(),
                        settled: true,
                    },
                )
            })
        });
        if let Some((panel, callback, event)) = notification {
            unsafe {
                set_panel_frame(
                    panel,
                    match &event {
                        HeadEvent::Moved { frame, .. } => frame,
                        _ => unreachable!(),
                    },
                );
            }
            notifications.push((callback, event));
        }
    }
    for (callback, event) in notifications {
        emit(callback, event);
    }
    Ok(())
}

pub fn show_head(app: &AppHandle, spec: &HeadSpec, events: HeadCallback) -> Result<(), String> {
    let _ = APP.set(app.clone());
    let existing = HEADS.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.heads.get_mut(&spec.conversation_id).map(|head| {
            let previous_glyph = head.spec.glyph.clone();
            head.spec.glyph = spec.glyph.clone();
            head.spec.color = spec.color;
            head.spec.label = spec.label.clone();
            head.spec.unread = spec.unread;
            head.spec.generation = spec.generation;
            head.events = events.clone();
            (
                head.views,
                head.spec.clone(),
                previous_glyph,
                head.frame.clone(),
                head.events.clone(),
            )
        })
    });
    if let Some((views, current, previous_glyph, frame, callback)) = existing {
        update_head_content(views, &current, Some(&previous_glyph));
        emit(
            callback,
            HeadEvent::Moved {
                conversation_id: current.conversation_id,
                generation: current.generation,
                position: current.position,
                frame,
                settled: true,
            },
        );
        return Ok(());
    }
    let (position, frame) = create_head(app, spec.clone(), events.clone())?;
    emit(
        events,
        HeadEvent::Moved {
            conversation_id: spec.conversation_id.clone(),
            generation: spec.generation,
            position,
            frame,
            settled: true,
        },
    );
    Ok(())
}

pub fn update_head(app: &AppHandle, spec: &HeadSpec) -> Result<(), String> {
    let update = HEADS.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.heads.get_mut(&spec.conversation_id).map(|head| {
            let previous_glyph = head.spec.glyph.clone();
            head.spec.glyph = spec.glyph.clone();
            head.spec.color = spec.color;
            head.spec.label = spec.label.clone();
            head.spec.unread = spec.unread;
            head.spec.generation = spec.generation;
            (head.views, head.spec.clone(), previous_glyph)
        })
    });
    if let Some((views, current, previous_glyph)) = update {
        update_head_content(views, &current, Some(&previous_glyph));
    }
    let _ = app; // Updates are content-only; monitor reconciliation is notification-driven.
    Ok(())
}

pub fn hide_head(app: &AppHandle, conversation_id: &str) -> Result<(), String> {
    let head = HEADS.with(|registry| {
        let mut registry = registry.borrow_mut();
        let head = registry.heads.remove(conversation_id);
        if let Some(head) = &head {
            registry.views.remove(&(head.views.view as usize));
        }
        head
    });
    if let Some(head) = head {
        unsafe {
            destroy_head(head);
            remove_screen_observer_if_unused();
        }
    }
    let _ = app;
    Ok(())
}

pub fn clear_heads(app: &AppHandle) {
    let heads = HEADS.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.views.clear();
        std::mem::take(&mut registry.heads)
    });
    for (_, head) in heads {
        unsafe {
            destroy_head(head);
        }
    }
    unsafe {
        remove_screen_observer_if_unused();
    }
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn circle_rejects_transparent_corner() {
        assert!(!is_in_circle(NSPoint::new(0.0, 0.0)));
        assert!(!is_in_circle(NSPoint::new(0.0, 8.0)));
        assert!(is_in_circle(NSPoint::new(24.0, 32.0)));
        assert!(!is_in_circle(NSPoint::new(55.0, 55.0)));
        assert!(is_in_circle(NSPoint::new(28.0, 28.0)));
    }

    #[test]
    fn badge_frame_centers_each_width_at_the_top_left_center() {
        for (unread, width) in [(1, 18.0), (10, 22.0), (100, 26.0)] {
            let frame = badge_frame(unread);
            assert_eq!(frame.origin.x, 41.0 - width / 2.0);
            assert_eq!(frame.origin.y, 6.0);
            assert_eq!((frame.size.width, frame.size.height), (width, BADGE_HEIGHT));
            assert_eq!(frame.origin.x + frame.size.width / 2.0, 41.0);
            assert!(is_in_badge(NSPoint::new(41.0, 15.0), unread,));
        }
    }

    #[test]
    fn hidden_badge_is_not_clickable() {
        assert!(!is_in_badge(NSPoint::new(41.0, 15.0), 0));
    }

    #[test]
    fn badge_label_frame_centers_natural_text_heights() {
        for label_height in [12.0, 14.0] {
            let frame = badge_label_frame(22.0, label_height);
            assert_eq!(
                (frame.origin.x, frame.origin.y),
                (0.0, (BADGE_HEIGHT - label_height) / 2.0)
            );
            assert_eq!((frame.size.width, frame.size.height), (22.0, label_height));
        }
    }

    #[test]
    fn only_photo_badges_use_the_neutral_ring() {
        let photo = HeadGlyph::Photo(super::super::HeadPhoto {
            edge: 1,
            rgba: std::sync::Arc::from([0, 0, 0, 255]),
        });
        assert_eq!(badge_ring_color(&photo, [12, 34, 56]), [31, 31, 31]);
        assert_eq!(
            badge_ring_color(&HeadGlyph::Initials("P".into()), [12, 34, 56]),
            [12, 34, 56]
        );
        assert_eq!(
            badge_ring_color(&HeadGlyph::Person, [12, 34, 56]),
            [12, 34, 56]
        );
        assert_eq!(
            badge_ring_color(&HeadGlyph::Group, [12, 34, 56]),
            [12, 34, 56]
        );
    }

    #[test]
    fn photo_image_preserves_rgba_channel_order() {
        unsafe {
            let image = photo_image(1, &[20, 127, 227, 255]);
            assert_ne!(image, nil);
            let data: id = msg_send![image, TIFFRepresentation];
            let bitmap: id = msg_send![class!(NSBitmapImageRep), imageRepWithData: data];
            let color: id = msg_send![bitmap, colorAtX: 0usize y: 0usize];
            let red: f64 = msg_send![color, redComponent];
            let green: f64 = msg_send![color, greenComponent];
            let blue: f64 = msg_send![color, blueComponent];
            let _: () = msg_send![image, release];

            assert_eq!(
                (red, green, blue),
                (20.0 / 255.0, 127.0 / 255.0, 227.0 / 255.0)
            );
        }
    }

    #[test]
    fn cross_monitor_drag_uses_destination_scale_and_negative_work_origin() {
        let (position, frame) = drag_frame_in_work_area(
            Some("secondary".into()),
            (-1600.0, -200.0, 1600.0, 900.0),
            1.25,
            (-1000.0, 100.0),
            (20.0, 12.0),
        );
        assert_eq!(position.monitor.as_deref(), Some("secondary"));
        assert_eq!(frame.scale_factor, 1.25);
        assert_eq!((frame.width, frame.height), (70.0, 70.0));
        assert_eq!((frame.x, frame.y), (-1025.0, 85.0));
        assert_eq!((position.x, position.y), (460.0, 228.0));
    }

    #[test]
    fn destination_work_area_clamps_after_scale_change() {
        let (position, frame) = drag_frame_in_work_area(
            Some("retina".into()),
            (1440.0, 0.0, 600.0, 400.0),
            2.0,
            (2200.0, 500.0),
            (28.0, 28.0),
        );
        assert_eq!((frame.x, frame.y), (1928.0, 288.0));
        assert_eq!((position.x, position.y), (244.0, 144.0));
    }
    #[test]
    fn geometry_matching_ignores_display_order_and_uses_both_origins() {
        let upper = MonitorGeometry {
            x: 250.0,
            y: -1350.0,
            width: 2400.0,
            height: 1350.0,
            scale: 1.25,
        };
        let primary = MonitorGeometry {
            x: 0.0,
            y: 0.0,
            width: 2880.0,
            height: 1800.0,
            scale: 2.0,
        };
        let left = MonitorGeometry {
            x: -1920.0,
            y: 100.0,
            width: 1920.0,
            height: 1080.0,
            scale: 1.0,
        };
        let monitors = [upper, primary, left];
        for (rect, scale, expected) in [
            (
                NSRect::new(NSPoint::new(200.0, 900.0), NSSize::new(1920.0, 1080.0)),
                1.25,
                0,
            ),
            (
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)),
                2.0,
                1,
            ),
            (
                NSRect::new(NSPoint::new(-1920.0, -280.0), NSSize::new(1920.0, 1080.0)),
                1.0,
                2,
            ),
        ] {
            assert_eq!(
                monitors
                    .iter()
                    .position(|m| matches_screen_geometry(rect, scale, 900.0, *m)),
                Some(expected)
            );
        }
        let rect = NSRect::new(NSPoint::new(-1920.0, -280.0), NSSize::new(1920.0, 1080.0));
        assert!(!matches_screen_geometry(
            rect,
            1.0,
            900.0,
            MonitorGeometry { y: 999.0, ..left }
        ));
        assert!(!matches_screen_geometry(rect, 2.0, 900.0, left));
    }

    #[test]
    fn cursor_mapping_uses_screen_local_top_offset_at_destination_scale() {
        let rect = NSRect::new(NSPoint::new(200.0, 900.0), NSSize::new(1920.0, 1080.0));
        let monitor = MonitorGeometry {
            x: 250.0,
            y: -1350.0,
            width: 2400.0,
            height: 1350.0,
            scale: 1.25,
        };
        assert_eq!(
            cursor_in_monitor(rect, NSPoint::new(220.0, 1960.0), monitor),
            (275.0, -1325.0)
        );
    }

    #[test]
    fn saved_name_requires_a_unique_nonempty_match() {
        let names = [
            Some("same-model"),
            Some("other"),
            Some("same-model"),
            None,
            Some(""),
        ];
        assert_eq!(
            unique_monitor_name(Some("same-model"), names.into_iter()),
            None
        );
        assert_eq!(
            unique_monitor_name(Some("other"), names.into_iter()),
            Some(1)
        );
        assert_eq!(
            unique_monitor_name(Some("missing"), names.into_iter()),
            None
        );
        assert_eq!(unique_monitor_name(None, names.into_iter()), None);
        assert_eq!(unique_monitor_name(Some(""), names.into_iter()), None);
    }
}
