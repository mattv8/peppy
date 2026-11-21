//! Native Win32 conversation heads.  This module deliberately owns only the
//! shaped native windows; the host owns pins, composers, and generation fencing.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use tauri::AppHandle;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, BitBlt, CombineRgn, CreateCompatibleBitmap, CreateCompatibleDC,
            CreateEllipticRgn, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteDC,
            DrawTextW, EndPaint, EnumDisplayMonitors, FillRect, FillRgn, GetMonitorInfoW,
            GetSysColor, InvalidateRect, MonitorFromPoint, MonitorFromWindow, RestoreDC, SaveDC,
            SelectClipRgn, SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor,
            SetWindowRgn, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY,
            CLIP_DEFAULT_PRECIS, COLOR_WINDOW, COLOR_WINDOWTEXT, DEFAULT_CHARSET, DEFAULT_PITCH,
            DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_BOLD, HALFTONE,
            HDC, HGDIOBJ, HRGN, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
            OUT_DEFAULT_PRECIS, PAINTSTRUCT, RGN_ERROR, RGN_OR, SRCCOPY, TRANSPARENT,
        },
        System::{LibraryLoader::GetModuleHandleW, SystemInformation::GetTickCount64},
        UI::{
            Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, GetCursorPos, GetWindowRect, KillTimer, LoadCursorW, PostMessageW,
                RegisterClassW, SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW,
                ShowWindow, SystemParametersInfoW, TrackPopupMenu, HWND_TOPMOST, IDC_HAND,
                MF_STRING, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SWP_NOACTIVATE,
                SWP_SHOWWINDOW, SW_SHOWNA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, TPM_RETURNCMD,
                WM_CAPTURECHANGED, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NULL, WM_PAINT, WM_SETTINGCHANGE,
                WM_SYSCOLORCHANGE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
};

use crate::heads::MAX_HEADS;

use super::{HeadCallback, HeadEvent, HeadFrame, HeadGlyph, HeadPosition, HeadSpec};

const LOGICAL_SIZE: f64 = 56.0;
const CIRCLE_DIAMETER: f64 = 48.0;
const BADGE_HEIGHT: f64 = 18.0;
const BADGE_RING: f64 = 2.0;
// Any count in the 99+ tier: the typing pill takes that tier's slot and size.
const TYPING_PILL_UNREAD: u64 = 100;
const TYPING_DOT_SIZE: f64 = 4.0;
const TYPING_DOT_PITCH: f64 = 7.0;
const TYPING_DOT_COUNT: usize = 3;
const TYPING_TIMER_ID: usize = 1;
const TYPING_TIMER_MS: u32 = 50;
const TYPING_CYCLE_MS: u64 = 1200;
const TYPING_DOT_DELAY_MS: u64 = 160;
const TYPING_PEAK_MS: u64 = 240;
const TYPING_ACTIVE_MS: u64 = 600;
const TYPING_INK: [u8; 3] = [28, 28, 30];
const TYPING_FILL: [u8; 3] = [0xff; 3];
// Opacity is quantized to sixteenths; reduced motion shows 0.7.
const FULL_ALPHA: u8 = 16;
const STATIC_ALPHA: u8 = 11;
const DRAG_THRESHOLD: i32 = 4;
const DISMISS_MENU_ID: usize = 1;
const OPEN_MENU_ID: usize = 2;
const CLASS_NAME: &[u16] = &[
    b'O' as u16,
    b'p' as u16,
    b'e' as u16,
    b'n' as u16,
    b'P' as u16,
    b'u' as u16,
    b's' as u16,
    b'h' as u16,
    b'H' as u16,
    b'e' as u16,
    b'a' as u16,
    b'd' as u16,
    0,
];

#[derive(Clone)]
struct Head {
    conversation_id: String,
    generation: u64,
    glyph: HeadGlyph,
    photo_bgra: Option<Arc<[u8]>>,
    color: [u8; 3],
    label: String,
    unread: u64,
    typing: bool,
    // Tick count when the pill last became shown; drives the dot phase.
    typing_started_ms: u64,
    // Dot state last painted or scheduled for paint; the timer repaints only on change.
    dot_frame: Option<DotFrame>,
    timer_running: bool,
    // "Show animations in Windows" and High Contrast, re-read on setting changes.
    motion: bool,
    high_contrast: bool,
    title: Vec<u16>,
    callback: HeadCallback,
    drag_origin: Option<POINT>,
    grab_offset: Option<(f64, f64)>,
    dragging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DotFrame {
    tops: [i32; TYPING_DOT_COUNT],
    alphas: [u8; TYPING_DOT_COUNT],
}

#[derive(Default)]
struct Registry {
    heads: HashMap<isize, Head>,
    class_registered: bool,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}
fn key(hwnd: HWND) -> isize {
    hwnd.0 as isize
}
fn hwnd(key: isize) -> HWND {
    HWND(key as _)
}
// Win32 calls happen outside the registry lock; this only touches the head map.
fn with_head<T>(hwnd: HWND, action: impl FnOnce(&mut Head) -> T) -> Option<T> {
    let mut state = registry().lock().ok()?;
    let result = state.heads.get_mut(&key(hwnd)).map(action);
    result
}

struct MonitorLookup<'a> {
    wanted: &'a str,
    found: Option<(windows::Win32::Graphics::Gdi::HMONITOR, MONITORINFOEXW)>,
}

unsafe extern "system" fn find_monitor_callback(
    monitor: windows::Win32::Graphics::Gdi::HMONITOR,
    _: windows::Win32::Graphics::Gdi::HDC,
    _: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    let lookup = &mut *(data.0 as *mut MonitorLookup<'_>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool()
        && utf16_name(&info.szDevice) == lookup.wanted
    {
        lookup.found = Some((monitor, info));
        return false.into();
    }
    true.into()
}

fn with_ui_thread<T>(
    app: &AppHandle,
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    // Tauri dispatches this closure to the thread that owns its Win32 event loop.
    // A synchronous channel keeps the public API's error contract without ever
    // holding the registry lock while entering Tauri or Win32.
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = send.send(action());
    })
    .map_err(|error| error.to_string())?;
    receive
        .recv()
        .map_err(|_| "Windows UI thread stopped".to_string())?
}

pub fn show_head(app: &AppHandle, spec: &HeadSpec, events: HeadCallback) -> Result<(), String> {
    let spec = spec.clone();
    with_ui_thread(app, move || unsafe { show_on_ui_thread(&spec, events) })
}

pub fn update_head(app: &AppHandle, spec: &HeadSpec) -> Result<(), String> {
    let spec = spec.clone();
    with_ui_thread(app, move || unsafe { update_on_ui_thread(&spec) })
}

pub fn hide_head(app: &AppHandle, conversation_id: &str) -> Result<(), String> {
    let conversation_id = conversation_id.to_owned();
    with_ui_thread(app, move || unsafe {
        let window = registry()
            .lock()
            .map_err(|_| "native head registry is unavailable")?
            .heads
            .iter()
            .find_map(|(hwnd, head)| (head.conversation_id == conversation_id).then_some(*hwnd));
        if let Some(window) = window {
            stop_typing_timer(hwnd(window));
            DestroyWindow(hwnd(window)).map_err(|error| error.to_string())?;
        }
        Ok(())
    })
}

pub fn clear_heads(app: &AppHandle) {
    let _ = with_ui_thread(app, || unsafe {
        let windows = registry()
            .lock()
            .map_err(|_| "native head registry is unavailable")?
            .heads
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for window in windows {
            stop_typing_timer(hwnd(window));
            let _ = DestroyWindow(hwnd(window));
        }
        Ok(())
    });
}

unsafe fn show_on_ui_thread(spec: &HeadSpec, callback: HeadCallback) -> Result<(), String> {
    let existing = {
        let state = registry()
            .lock()
            .map_err(|_| "native head registry is unavailable")?;
        state.heads.iter().find_map(|(hwnd, head)| {
            (head.conversation_id == spec.conversation_id).then_some(*hwnd)
        })
    };
    if let Some(window) = existing {
        update_existing(hwnd(window), spec, Some(callback))?;
        return Ok(());
    }
    {
        let state = registry()
            .lock()
            .map_err(|_| "native head registry is unavailable")?;
        if state.heads.len() >= MAX_HEADS {
            return Err("native head limit (8) reached".into());
        }
    }
    ensure_class()?;
    let (x, y, scale) = restore_position(&spec.position);
    let title = accessible_title(&spec.label, spec.unread, spec.typing);
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        PCWSTR(CLASS_NAME.as_ptr()),
        PCWSTR(title.as_ptr()),
        WS_POPUP,
        x,
        y,
        (LOGICAL_SIZE * scale).round() as i32,
        (LOGICAL_SIZE * scale).round() as i32,
        None,
        None,
        Some(
            GetModuleHandleW(None)
                .map_err(|error| error.to_string())?
                .into(),
        ),
        None,
    )
    .map_err(|error| error.to_string())?;
    let head = Head {
        conversation_id: spec.conversation_id.clone(),
        generation: spec.generation,
        glyph: spec.glyph.clone(),
        photo_bgra: photo_bgra(&spec.glyph),
        color: spec.color,
        label: spec.label.clone(),
        unread: spec.unread,
        typing: spec.typing,
        typing_started_ms: GetTickCount64(),
        dot_frame: None,
        timer_running: false,
        motion: true,
        high_contrast: false,
        title: title.clone(),
        callback,
        drag_origin: None,
        grab_offset: None,
        dragging: false,
    };
    registry()
        .lock()
        .map_err(|_| "native head registry is unavailable")?
        .heads
        .insert(key(hwnd), head);
    refresh_typing_settings(hwnd);
    let size = physical_size(hwnd);
    if !apply_shape(hwnd, size) {
        let _ = DestroyWindow(hwnd);
        return Err("Windows could not apply the circular input region".into());
    }
    let (x, y) = clamp_to_work_area(POINT { x, y }, size);
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        x,
        y,
        size,
        size,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    let _ = ShowWindow(hwnd, SW_SHOWNA);
    sync_typing_timer(hwnd);
    emit_moved(hwnd, true);
    Ok(())
}

unsafe fn update_on_ui_thread(spec: &HeadSpec) -> Result<(), String> {
    let window = {
        let state = registry()
            .lock()
            .map_err(|_| "native head registry is unavailable")?;
        state.heads.iter().find_map(|(hwnd, head)| {
            (head.conversation_id == spec.conversation_id).then_some(*hwnd)
        })
    };
    let Some(window) = window else {
        return Err("native head does not exist".into());
    };
    update_existing(hwnd(window), spec, None)
}

unsafe fn update_existing(
    hwnd: HWND,
    spec: &HeadSpec,
    replacement: Option<HeadCallback>,
) -> Result<(), String> {
    let replacing_callback = replacement.is_some();
    let (label, shape_changed, started_typing) = {
        let mut state = registry()
            .lock()
            .map_err(|_| "native head registry is unavailable")?;
        let head = state
            .heads
            .get_mut(&key(hwnd))
            .ok_or("native head does not exist")?;
        let before_tier = displayed_badge_tier(head.unread, head.typing);
        let before_typing = head.typing;
        head.generation = spec.generation;
        head.glyph = spec.glyph.clone();
        head.photo_bgra = photo_bgra(&spec.glyph);
        head.color = spec.color;
        head.label = spec.label.clone();
        head.unread = spec.unread;
        head.typing = spec.typing;
        let started_typing = spec.typing && !before_typing;
        if started_typing {
            head.typing_started_ms = GetTickCount64();
        }
        if before_typing != spec.typing {
            head.dot_frame = None;
        }
        let changed = before_tier != displayed_badge_tier(head.unread, head.typing)
            || before_typing != head.typing;
        if let Some(callback) = replacement {
            head.callback = callback;
        }
        (head.label.clone(), changed, started_typing)
    };
    // Effects run in the order the pure planner gives: settings, then the timer, then
    // the region. A failing region must never leave the timer running with typing off.
    for effect in typing_update_effects(started_typing, shape_changed) {
        match effect {
            TypingEffect::ReadSettings => refresh_typing_settings(hwnd),
            TypingEffect::SyncTimer => sync_typing_timer(hwnd),
            TypingEffect::RebuildRegion => {
                if !apply_shape(hwnd, physical_size(hwnd)) {
                    return Err("Windows could not update the head input region".into());
                }
            }
        }
    }
    let _ = InvalidateRect(Some(hwnd), None, true);
    // Typing toggles often; touch the title only when its text actually changed.
    let title = accessible_title(&label, spec.unread, spec.typing);
    let title_changed = with_head(hwnd, |head| {
        let changed = head.title != title;
        if changed {
            head.title = title.clone();
        }
        changed
    })
    .unwrap_or(false);
    if title_changed {
        let _ = SetWindowTextW(hwnd, PCWSTR(title.as_ptr()));
    }
    // Updates must never activate a head. Position is owned by native drag and
    // display reconciliation, so a stale async update cannot move it backwards.
    if replacing_callback {
        emit_moved(hwnd, false);
    }
    Ok(())
}

unsafe fn ensure_class() -> Result<(), String> {
    if registry()
        .lock()
        .map_err(|_| "native head registry is unavailable")?
        .class_registered
    {
        return Ok(());
    }
    let instance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        hCursor: LoadCursorW(None, IDC_HAND).map_err(|error| error.to_string())?,
        lpszClassName: PCWSTR(CLASS_NAME.as_ptr()),
        ..Default::default()
    };
    if RegisterClassW(&class) == 0 {
        return Err("Windows could not register the head window class".into());
    }
    registry()
        .lock()
        .map_err(|_| "native head registry is unavailable")?
        .class_registered = true;
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            if let Ok(mut state) = registry().lock() {
                if let Some(head) = state.heads.get_mut(&key(hwnd)) {
                    let mut rect = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut rect);
                    let scale = GetDpiForWindow(hwnd) as f64 / 96.0;
                    head.drag_origin = Some(point);
                    head.grab_offset = Some((
                        (point.x - rect.left) as f64 / scale,
                        (point.y - rect.top) as f64 / scale,
                    ));
                    head.dragging = false;
                }
            }
            let _ = SetCapture(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            move_drag(hwnd, (_wparam.0 & 1) != 0);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            finish_drag_or_activate(hwnd);
            return LRESULT(0);
        }
        WM_CONTEXTMENU => {
            show_context_menu(hwnd);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            apply_dpi_change(hwnd, _lparam);
            return LRESULT(0);
        }
        WM_TIMER => {
            if _wparam.0 == TYPING_TIMER_ID {
                advance_typing_frame(hwnd);
            }
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE => {
            reconcile(hwnd);
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            reconcile(hwnd);
            refresh_typing_appearance(hwnd);
            return LRESULT(0);
        }
        WM_SYSCOLORCHANGE => {
            refresh_typing_appearance(hwnd);
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            cancel_drag(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            stop_typing_timer(hwnd);
            if let Ok(mut state) = registry().lock() {
                state.heads.remove(&key(hwnd));
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, message, _wparam, _lparam)
}

unsafe fn move_drag(hwnd: HWND, left_button_down: bool) {
    if !left_button_down {
        cancel_drag(hwnd);
        return;
    }
    let mut cursor = POINT::default();
    if GetCursorPos(&mut cursor).is_err() {
        return;
    }
    let drag = registry().lock().ok().and_then(|mut state| {
        let head = state.heads.get_mut(&key(hwnd))?;
        let origin = head.drag_origin?;
        if drag_started(
            cursor.x - origin.x,
            cursor.y - origin.y,
            drag_threshold(hwnd),
        ) {
            head.dragging = true;
        }
        head.dragging.then_some(head.grab_offset?)
    });
    if let Some(grab_offset) = drag {
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let Some(info) = monitor_info(monitor) else {
            return;
        };
        let scale = monitor_scale(monitor);
        let (x, y, size) = drag_geometry(cursor, grab_offset, info.monitorInfo.rcWork, scale);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, size, size, SWP_NOACTIVATE);
        let _ = apply_shape(hwnd, size);
        emit_moved(hwnd, false);
    }
}

unsafe fn cancel_drag(hwnd: HWND) {
    let was_drag = registry().lock().ok().and_then(|mut state| {
        state.heads.get_mut(&key(hwnd)).map(|head| {
            let dragging = head.dragging;
            head.drag_origin = None;
            head.grab_offset = None;
            head.dragging = false;
            dragging
        })
    });
    if was_drag == Some(true) {
        emit_moved(hwnd, true);
    }
}

unsafe fn finish_drag_or_activate(hwnd: HWND) {
    let was_drag = registry()
        .lock()
        .ok()
        .and_then(|mut state| {
            state.heads.get_mut(&key(hwnd)).map(|head| {
                let dragging = head.dragging;
                head.drag_origin = None;
                head.grab_offset = None;
                head.dragging = false;
                dragging
            })
        })
        .unwrap_or(false);
    let _ = ReleaseCapture();
    if was_drag {
        emit_moved(hwnd, true);
    } else {
        emit(hwnd, EventKind::Activate);
    }
}

unsafe fn show_context_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let open: Vec<u16> = "Open conversation\0".encode_utf16().collect();
    let dismiss: Vec<u16> = "Dismiss floating conversation\0".encode_utf16().collect();
    let _ = AppendMenuW(menu, MF_STRING, OPEN_MENU_ID, PCWSTR(open.as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, DISMISS_MENU_ID, PCWSTR(dismiss.as_ptr()));
    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    let _ = SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(menu, TPM_RETURNCMD, point.x, point.y, None, hwnd, None);
    let _ = DestroyMenu(menu);
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    match command.0 as usize {
        OPEN_MENU_ID => emit(hwnd, EventKind::Activate),
        DISMISS_MENU_ID => emit(hwnd, EventKind::Dismiss),
        _ => {}
    }
}

unsafe fn reconcile(hwnd: HWND) {
    let size = physical_size(hwnd);
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_ok() {
        let (x, y) = clamp_to_work_area(
            POINT {
                x: rect.left,
                y: rect.top,
            },
            size,
        );
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, size, size, SWP_NOACTIVATE);
        let _ = apply_shape(hwnd, size);
        emit_moved(hwnd, true);
    }
}

unsafe fn apply_dpi_change(hwnd: HWND, lparam: LPARAM) {
    // WM_DPICHANGED owns a RECT suggested by Windows. Copy it before any other
    // window call, then clamp it to the destination work area and rebuild HRGN.
    // Do not clamp or signal settled while a drag is active; emit unsettled for
    // current frame during active drag.
    let is_dragging = registry()
        .lock()
        .ok()
        .and_then(|state| state.heads.get(&key(hwnd)).map(|h| h.dragging))
        .unwrap_or(false);
    let suggested = (lparam.0 as *const RECT).as_ref().copied();
    let Some(suggested) = suggested else {
        return reconcile(hwnd);
    };
    let size = physical_size(hwnd);
    if is_dragging {
        let _ = apply_shape(hwnd, size);
        emit_moved(hwnd, false);
    } else {
        let (x, y) = clamp_to_work_area(
            POINT {
                x: suggested.left,
                y: suggested.top,
            },
            size,
        );
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, size, size, SWP_NOACTIVATE);
        let _ = apply_shape(hwnd, size);
        emit_moved(hwnd, true);
    }
}

unsafe fn apply_shape(hwnd: HWND, size: i32) -> bool {
    // SetWindowRgn takes ownership only on success; DeleteObject prevents a
    // failed shape update leaking the freshly allocated HRGN.
    let scale = size as f64 / LOGICAL_SIZE;
    let circle_size = scaled(CIRCLE_DIAMETER, scale);
    let region = CreateEllipticRgn(0, 0, circle_size, circle_size);
    let (unread, typing) = registry()
        .lock()
        .ok()
        .and_then(|state| {
            state
                .heads
                .get(&key(hwnd))
                .map(|head| (head.unread, head.typing))
        })
        .unwrap_or_default();
    if displayed_badge_tier(unread, typing) > 0 {
        let badge_rect = badge_rect(badge_slot_unread(unread, typing), scale);
        let badge = CreateRoundRectRgn(
            badge_rect.left,
            badge_rect.top,
            badge_rect.right,
            badge_rect.bottom,
            scaled(BADGE_HEIGHT, scale),
            scaled(BADGE_HEIGHT, scale),
        );
        if CombineRgn(Some(region), Some(region), Some(badge), RGN_OR).0 == 0 {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(badge.into());
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(region.into());
            return false;
        }
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(badge.into());
    }
    if SetWindowRgn(hwnd, Some(region), true) != 0 {
        true
    } else {
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(region.into());
        false
    }
}

fn scaled(logical: f64, scale: f64) -> i32 {
    (logical * scale).round().max(1.0) as i32
}

fn badge_tier(unread: u64) -> u8 {
    match unread {
        0 => 0,
        1..=9 => 1,
        10..=99 => 2,
        _ => 3,
    }
}

fn badge_width(unread: u64) -> f64 {
    match badge_tier(unread) {
        1 => 18.0,
        2 => 22.0,
        3 => 26.0,
        _ => 0.0,
    }
}

fn badge_rect(unread: u64, scale: f64) -> RECT {
    let width = scaled(badge_width(unread), scale);
    let height = scaled(BADGE_HEIGHT, scale);
    let center = scaled(41.0, scale);
    let left = center - width / 2;
    let top = center - height / 2;
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

fn badge_ring_color(has_photo: bool, conversation_color: [u8; 3]) -> [u8; 3] {
    if has_photo {
        [0x1f; 3]
    } else {
        conversation_color
    }
}

fn badge_text(unread: u64) -> String {
    if unread >= 100 {
        "99+".to_string()
    } else {
        unread.to_string()
    }
}

// While typing, the pill takes the 99+ slot, so the badge tier is 3.
fn badge_slot_unread(unread: u64, typing: bool) -> u64 {
    if typing {
        TYPING_PILL_UNREAD
    } else {
        unread
    }
}

fn displayed_badge_tier(unread: u64, typing: bool) -> u8 {
    badge_tier(badge_slot_unread(unread, typing))
}

fn inset_rect(rect: RECT, by: i32) -> RECT {
    RECT {
        left: rect.left + by,
        top: rect.top + by,
        right: rect.right - by,
        bottom: rect.bottom - by,
    }
}

// Offsets are relative to the pill's top-left; `rest_top` is where the dots sit when idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DotGeometry {
    diameter: i32,
    left: i32,
    pitch: i32,
    rest_top: i32,
    travel: i32,
    static_top: i32,
}

fn dot_geometry(pill_width: i32, scale: f64) -> DotGeometry {
    let diameter = scaled(TYPING_DOT_SIZE, scale);
    let pitch = scaled(TYPING_DOT_PITCH, scale);
    let run = 2 * pitch + diameter;
    DotGeometry {
        diameter,
        left: (pill_width - run) / 2,
        pitch,
        rest_top: scaled(9.0, scale),
        travel: scaled(3.0, scale),
        static_top: scaled(7.0, scale),
    }
}

// Drive value 0..1 for one dot at `local_ms` into its own cycle (spec section 3).
fn typing_drive(local_ms: u64) -> f64 {
    if local_ms < TYPING_PEAK_MS {
        let l = local_ms as f64 / TYPING_PEAK_MS as f64;
        1.0 - (1.0 - l) * (1.0 - l)
    } else if local_ms < TYPING_ACTIVE_MS {
        let u = (local_ms - TYPING_PEAK_MS) as f64 / (TYPING_ACTIVE_MS - TYPING_PEAK_MS) as f64;
        1.0 - (3.0 * u * u - 2.0 * u * u * u)
    } else {
        0.0
    }
}

// Dots start one stagger apart, so a dot is at rest until its own start.
fn typing_dot_drive(elapsed_ms: u64, dot: usize) -> f64 {
    let start = dot as u64 * TYPING_DOT_DELAY_MS;
    if elapsed_ms < start {
        0.0
    } else {
        typing_drive((elapsed_ms - start) % TYPING_CYCLE_MS)
    }
}

fn quantize_alpha(alpha: f64) -> u8 {
    (alpha * FULL_ALPHA as f64)
        .round()
        .clamp(0.0, FULL_ALPHA as f64) as u8
}

fn typing_frame(geometry: &DotGeometry, elapsed_ms: u64, high_contrast: bool) -> DotFrame {
    let mut frame = DotFrame {
        tops: [geometry.rest_top; TYPING_DOT_COUNT],
        alphas: [FULL_ALPHA; TYPING_DOT_COUNT],
    };
    for dot in 0..TYPING_DOT_COUNT {
        let drive = typing_dot_drive(elapsed_ms, dot);
        frame.tops[dot] = geometry.rest_top - (geometry.travel as f64 * drive).round() as i32;
        frame.alphas[dot] = if high_contrast {
            FULL_ALPHA
        } else {
            quantize_alpha(0.5 + 0.5 * drive)
        };
    }
    frame
}

// Reduced motion: no movement, centered dots, and 0.7 opacity unless High Contrast.
fn static_frame(geometry: &DotGeometry, high_contrast: bool) -> DotFrame {
    DotFrame {
        tops: [geometry.static_top; TYPING_DOT_COUNT],
        alphas: [if high_contrast {
            FULL_ALPHA
        } else {
            STATIC_ALPHA
        }; TYPING_DOT_COUNT],
    }
}

// GDI has no alpha, so opacity is a per-channel mix of fill and ink in sixteenths.
fn blend(fill: [u8; 3], ink: [u8; 3], alpha: u8) -> [u8; 3] {
    let a = alpha.min(FULL_ALPHA) as u32;
    let mix = |f: u8, i: u8| ((f as u32 * (16 - a) + i as u32 * a + 8) / 16) as u8;
    [
        mix(fill[0], ink[0]),
        mix(fill[1], ink[1]),
        mix(fill[2], ink[2]),
    ]
}

// `GetSysColor` returns COLORREF order (0x00BBGGRR); convert back to RGB.
fn rgb_from_colorref(value: u32) -> [u8; 3] {
    [value as u8, (value >> 8) as u8, (value >> 16) as u8]
}

unsafe fn clamp_to_work_area(point: POINT, size: i32) -> (i32, i32) {
    let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
    let Some(info) = monitor_info(monitor) else {
        return (point.x, point.y);
    };
    let work = info.monitorInfo.rcWork;
    (
        clamp_axis(point.x, work.left, work.right, size),
        clamp_axis(point.y, work.top, work.bottom, size),
    )
}

fn clamp_axis(value: i32, start: i32, end: i32, size: i32) -> i32 {
    value.clamp(start, (end - size).max(start))
}

fn drag_geometry(
    cursor: POINT,
    grab_offset: (f64, f64),
    work: RECT,
    scale: f64,
) -> (i32, i32, i32) {
    let size = (LOGICAL_SIZE * scale).round().max(1.0) as i32;
    let requested_x = (cursor.x as f64 - grab_offset.0 * scale).round() as i32;
    let requested_y = (cursor.y as f64 - grab_offset.1 * scale).round() as i32;
    (
        clamp_axis(requested_x, work.left, work.right, size),
        clamp_axis(requested_y, work.top, work.bottom, size),
        size,
    )
}

fn drag_started(dx: i32, dy: i32, threshold: i32) -> bool {
    dx.abs().max(dy.abs()) >= threshold
}

unsafe fn restore_position(position: &HeadPosition) -> (i32, i32, f64) {
    let fallback = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
    let selected = position
        .monitor
        .as_deref()
        .and_then(|name| find_named_monitor(name))
        .or_else(|| monitor_info(fallback).map(|info| (fallback, info)));
    let Some((monitor, info)) = selected else {
        return (0, 0, 1.0);
    };
    let scale = monitor_scale(monitor);
    let work = info.monitorInfo.rcWork;
    let x = work.left + (finite_or_zero(position.x) * scale).round() as i32;
    let y = work.top + (finite_or_zero(position.y) * scale).round() as i32;
    (x, y, scale)
}

unsafe fn find_named_monitor(
    name: &str,
) -> Option<(windows::Win32::Graphics::Gdi::HMONITOR, MONITORINFOEXW)> {
    let mut lookup = MonitorLookup {
        wanted: name,
        found: None,
    };
    let _ = EnumDisplayMonitors(
        None,
        None,
        Some(find_monitor_callback),
        LPARAM((&mut lookup as *mut MonitorLookup<'_>).cast::<std::ffi::c_void>() as isize),
    );
    lookup.found
}

unsafe fn monitor_info(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<MONITORINFOEXW> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    GetMonitorInfoW(monitor, &mut info.monitorInfo)
        .as_bool()
        .then_some(info)
}

unsafe fn monitor_scale(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> f64 {
    let mut dpi_x = 96;
    let mut dpi_y = 96;
    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    dpi_x as f64 / 96.0
}

fn utf16_name(value: &[u16]) -> String {
    String::from_utf16_lossy(
        &value[..value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len())],
    )
}

unsafe fn physical_size(hwnd: HWND) -> i32 {
    ((LOGICAL_SIZE * GetDpiForWindow(hwnd) as f64 / 96.0).round() as i32).max(1)
}

unsafe fn drag_threshold(hwnd: HWND) -> i32 {
    ((DRAG_THRESHOLD as f64 * GetDpiForWindow(hwnd) as f64 / 96.0).round() as i32).max(1)
}

unsafe fn frame(hwnd: HWND) -> Option<(HeadPosition, HeadFrame)> {
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return None;
    }
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let Some(info) = monitor_info(monitor) else {
        return None;
    };
    let work = info.monitorInfo.rcWork;
    let scale = monitor_scale(monitor);
    Some((
        HeadPosition {
            monitor: Some(utf16_name(&info.szDevice)),
            x: (rect.left - work.left) as f64 / scale,
            y: (rect.top - work.top) as f64 / scale,
        },
        HeadFrame {
            x: rect.left as f64,
            y: rect.top as f64,
            width: (rect.right - rect.left) as f64,
            height: (rect.bottom - rect.top) as f64,
            work_x: work.left as f64,
            work_y: work.top as f64,
            work_width: (work.right - work.left) as f64,
            work_height: (work.bottom - work.top) as f64,
            scale_factor: scale,
        },
    ))
}

enum EventKind {
    Activate,
    Dismiss,
}
unsafe fn emit(hwnd: HWND, kind: EventKind) {
    let entry = registry().lock().ok().and_then(|state| {
        state.heads.get(&key(hwnd)).map(|head| {
            (
                head.callback.clone(),
                head.conversation_id.clone(),
                head.generation,
            )
        })
    });
    if let Some((callback, conversation_id, generation)) = entry {
        callback(match kind {
            EventKind::Activate => HeadEvent::Activate {
                conversation_id,
                generation,
            },
            EventKind::Dismiss => HeadEvent::Dismiss {
                conversation_id,
                generation,
            },
        });
    }
}
unsafe fn emit_moved(hwnd: HWND, settled: bool) {
    let entry = registry().lock().ok().and_then(|state| {
        state.heads.get(&key(hwnd)).map(|head| {
            (
                head.callback.clone(),
                head.conversation_id.clone(),
                head.generation,
            )
        })
    });
    if let (Some((callback, conversation_id, generation)), Some((position, frame))) =
        (entry, frame(hwnd))
    {
        callback(HeadEvent::Moved {
            conversation_id,
            generation,
            position,
            frame,
            settled,
        });
    }
}

unsafe fn create_scaled_font(
    logical_height: f64,
    scale: f64,
) -> windows::Win32::Graphics::Gdi::HFONT {
    create_scaled_font_for_face(logical_height, scale, "Segoe UI")
}

unsafe fn create_scaled_font_for_face(
    logical_height: f64,
    scale: f64,
    face_name: &str,
) -> windows::Win32::Graphics::Gdi::HFONT {
    let face: Vec<u16> = format!("{face_name}\0").encode_utf16().collect();
    CreateFontW(
        -(logical_height * scale).round() as i32,
        0,
        0,
        0,
        FW_BOLD.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
        PCWSTR(face.as_ptr()),
    )
}

unsafe fn draw_centered_text(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    value: &str,
    rect: &mut RECT,
    logical_height: f64,
    scale: f64,
) {
    let font = create_scaled_font(logical_height, scale);
    let old = (!font.is_invalid()).then(|| SelectObject(hdc, font.into()));
    let mut text: Vec<u16> = value.encode_utf16().collect();
    let _ = DrawTextW(hdc, &mut text, rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    if let Some(old) = old {
        let _ = SelectObject(hdc, old);
    }
    if !font.is_invalid() {
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(font.into());
    }
}

unsafe fn draw_icon_glyph(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    scale: f64,
    glyph: &str,
) -> bool {
    let icon_size = scaled(22.0, scale);
    let circle_size = scaled(CIRCLE_DIAMETER, scale);
    let mut rect = RECT {
        left: (circle_size - icon_size) / 2,
        top: (circle_size - icon_size) / 2,
        right: (circle_size + icon_size) / 2,
        bottom: (circle_size + icon_size) / 2,
    };
    let mdl2_font = create_scaled_font_for_face(22.0, scale, "Segoe MDL2 Assets");
    if !mdl2_font.is_invalid() {
        let old = SelectObject(hdc, mdl2_font.into());
        let mut glyph_units: Vec<u16> = glyph.encode_utf16().collect();
        let _ = DrawTextW(
            hdc,
            &mut glyph_units,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        let _ = SelectObject(hdc, old);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(mdl2_font.into());
        return true;
    }

    let fluent_font = create_scaled_font_for_face(22.0, scale, "Segoe Fluent Icons");
    if !fluent_font.is_invalid() {
        let old = SelectObject(hdc, fluent_font.into());
        let mut glyph_units: Vec<u16> = glyph.encode_utf16().collect();
        let _ = DrawTextW(
            hdc,
            &mut glyph_units,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        let _ = SelectObject(hdc, old);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(fluent_font.into());
        return true;
    }

    false
}

unsafe fn draw_person_icon(hdc: windows::Win32::Graphics::Gdi::HDC, scale: f64) {
    if draw_icon_glyph(hdc, scale, "\u{e77b}") {
        return;
    }

    let center = scaled(CIRCLE_DIAMETER / 2.0, scale);
    let head_radius = scaled(6.0, scale);
    let head_center_y = scaled(18.0, scale);
    let shoulder_half_width = scaled(10.0, scale);
    let shoulder_half_height = scaled(7.0, scale);
    let shoulder_center_y = scaled(31.0, scale);
    let white = CreateSolidBrush(COLORREF(0xFFFFFF));
    let head = CreateEllipticRgn(
        center - head_radius,
        head_center_y - head_radius,
        center + head_radius,
        head_center_y + head_radius,
    );
    let shoulders = CreateEllipticRgn(
        center - shoulder_half_width,
        shoulder_center_y - shoulder_half_height,
        center + shoulder_half_width,
        shoulder_center_y + shoulder_half_height,
    );
    let _ = FillRgn(hdc, head, white);
    let _ = FillRgn(hdc, shoulders, white);
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(head.into());
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(shoulders.into());
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(white.into());
}

unsafe fn draw_group_icon(hdc: windows::Win32::Graphics::Gdi::HDC, scale: f64) {
    if !draw_icon_glyph(hdc, scale, "\u{e716}") {
        draw_person_icon(hdc, scale);
    }
}

fn colorref([red, green, blue]: [u8; 3]) -> COLORREF {
    COLORREF(red as u32 | (green as u32) << 8 | (blue as u32) << 16)
}

fn photo_bgra(glyph: &HeadGlyph) -> Option<Arc<[u8]>> {
    let HeadGlyph::Photo(photo) = glyph else {
        return None;
    };
    let pixel_count = usize::try_from(photo.edge)
        .ok()?
        .checked_mul(photo.edge as usize)?;
    let byte_count = pixel_count.checked_mul(4)?;
    if photo.rgba.len() != byte_count {
        return None;
    }
    let mut bgra = Vec::with_capacity(byte_count);
    for pixel in photo.rgba.chunks_exact(4) {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    Some(Arc::from(bgra))
}

unsafe fn draw_photo(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    circle_size: i32,
    edge: u32,
    bgra: &[u8],
) {
    let Ok(edge) = i32::try_from(edge) else {
        return;
    };
    let bitmap = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: edge,
            biHeight: -edge,
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    let clip = CreateEllipticRgn(0, 0, circle_size, circle_size);
    let saved_dc = SaveDC(hdc);
    let _ = SelectClipRgn(hdc, Some(clip));
    let _ = SetStretchBltMode(hdc, HALFTONE);
    let _ = SetBrushOrgEx(hdc, 0, 0, None);
    let _ = StretchDIBits(
        hdc,
        0,
        0,
        circle_size,
        circle_size,
        0,
        0,
        edge,
        edge,
        Some(bgra.as_ptr().cast()),
        &bitmap,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
    if saved_dc != 0 {
        let _ = RestoreDC(hdc, saved_dc);
    } else {
        let _ = SelectClipRgn(hdc, None);
    }
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(clip.into());
}

unsafe fn paint(hwnd: HWND) {
    let Some(head) = registry()
        .lock()
        .ok()
        .and_then(|state| state.heads.get(&key(hwnd)).cloned())
    else {
        return;
    };
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let dpi_scale = GetDpiForWindow(hwnd) as f64 / 96.0;
    let circle_size = scaled(CIRCLE_DIAMETER, dpi_scale);
    let circle = CreateEllipticRgn(0, 0, circle_size, circle_size);
    let color_brush = CreateSolidBrush(colorref(head.color));
    let _ = FillRgn(hdc, circle, color_brush);
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(circle.into());
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(color_brush.into());
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, COLORREF(0xFFFFFF));
    let has_photo = matches!(&head.glyph, HeadGlyph::Photo(_));
    match head.glyph.clone() {
        HeadGlyph::Photo(photo) => match head.photo_bgra.clone() {
            Some(bgra) => draw_photo(hdc, circle_size, photo.edge, &bgra),
            None => draw_person_icon(hdc, dpi_scale),
        },
        HeadGlyph::Initials(initials) => {
            let inset = scaled(4.0, dpi_scale);
            let mut text = RECT {
                left: inset,
                top: inset,
                right: circle_size - inset,
                bottom: circle_size - inset,
            };
            draw_centered_text(
                hdc,
                &initials.chars().take(2).collect::<String>(),
                &mut text,
                18.0,
                dpi_scale,
            );
        }
        HeadGlyph::Person => draw_person_icon(hdc, dpi_scale),
        HeadGlyph::Group => draw_group_icon(hdc, dpi_scale),
    }
    if head.typing {
        let frame = paint_typing_pill(hdc, &head, dpi_scale, has_photo);
        let _ = with_head(hwnd, |state| state.dot_frame = Some(frame));
    } else if head.unread > 0 {
        let unread = head.unread;
        let mut badge_rect = badge_rect(unread, dpi_scale);
        let badge = CreateRoundRectRgn(
            badge_rect.left,
            badge_rect.top,
            badge_rect.right,
            badge_rect.bottom,
            scaled(BADGE_HEIGHT, dpi_scale),
            scaled(BADGE_HEIGHT, dpi_scale),
        );
        let ring_color = CreateSolidBrush(colorref(badge_ring_color(has_photo, head.color)));
        let _ = FillRgn(hdc, badge, ring_color);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(badge.into());
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(ring_color.into());
        let ring = scaled(BADGE_RING, dpi_scale);
        let inner = CreateRoundRectRgn(
            badge_rect.left + ring,
            badge_rect.top + ring,
            badge_rect.right - ring,
            badge_rect.bottom - ring,
            scaled(BADGE_HEIGHT - BADGE_RING * 2.0, dpi_scale),
            scaled(BADGE_HEIGHT - BADGE_RING * 2.0, dpi_scale),
        );
        let red = CreateSolidBrush(COLORREF(0x2835D9));
        let _ = FillRgn(hdc, inner, red);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(inner.into());
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(red.into());
        let mut inner_rect = RECT {
            left: badge_rect.left + ring,
            top: badge_rect.top + ring,
            right: badge_rect.right - ring,
            bottom: badge_rect.bottom - ring,
        };
        draw_centered_text(hdc, &badge_text(unread), &mut inner_rect, 10.0, dpi_scale);
    }
    let _ = EndPaint(hwnd, &ps);
}

// Same ring and inner fill as the count badge, then three dots; no count text.
// Returns the frame painted so the timer can skip unchanged frames.
unsafe fn paint_typing_pill(hdc: HDC, head: &Head, dpi_scale: f64, has_photo: bool) -> DotFrame {
    let pill = badge_rect(TYPING_PILL_UNREAD, dpi_scale);
    let (fill, ink, ring_color) = if head.high_contrast {
        let text = rgb_from_colorref(GetSysColor(COLOR_WINDOWTEXT));
        (rgb_from_colorref(GetSysColor(COLOR_WINDOW)), text, text)
    } else {
        (
            TYPING_FILL,
            TYPING_INK,
            badge_ring_color(has_photo, head.color),
        )
    };
    let geometry = dot_geometry(pill.right - pill.left, dpi_scale);
    let frame = if head.typing && head.motion {
        let elapsed = GetTickCount64().saturating_sub(head.typing_started_ms);
        typing_frame(&geometry, elapsed, head.high_contrast)
    } else {
        static_frame(&geometry, head.high_contrast)
    };
    let art = PillArt {
        pill,
        geometry,
        frame,
        fill,
        ink,
    };
    let corner = scaled(BADGE_HEIGHT, dpi_scale);
    fill_region(hdc, round_rect_region(pill, corner), ring_color);
    let inner = inset_rect(pill, scaled(BADGE_RING, dpi_scale));
    let inner_corner = scaled(BADGE_HEIGHT - BADGE_RING * 2.0, dpi_scale);
    // Offscreen composition is preferred; the direct path runs only when the memory DC
    // or bitmap is unavailable, and that check happens before anything is drawn on hdc.
    if !draw_pill_offscreen(hdc, &art, inner, inner_corner) {
        fill_region(hdc, round_rect_region(inner, inner_corner), fill);
        draw_typing_dots(hdc, &art, (0, 0));
    }
    frame
}

struct PillArt {
    pill: RECT,
    geometry: DotGeometry,
    frame: DotFrame,
    fill: [u8; 3],
    ink: [u8; 3],
}

// Composes the inner fill and the dots in a memory DC sized to the inner rect, then copies
// it once through the inner rounded region. The clip keeps every bitmap pixel outside
// that shape (the bitmap's square corners) off the window, so the ring is never painted
// over. Returns false, with nothing drawn on `hdc`, if a resource is unavailable.
unsafe fn draw_pill_offscreen(hdc: HDC, art: &PillArt, inner: RECT, inner_corner: i32) -> bool {
    let width = inner.right - inner.left;
    let height = inner.bottom - inner.top;
    let memory = CreateCompatibleDC(Some(hdc));
    if memory.0.is_null() {
        return false;
    }
    let bitmap = CreateCompatibleBitmap(hdc, width, height);
    let previous = if bitmap.0.is_null() {
        HGDIOBJ(std::ptr::null_mut())
    } else {
        SelectObject(memory, bitmap.into())
    };
    let saved = SaveDC(hdc);
    let region = round_rect_region(inner, inner_corner);
    let copied = !bitmap.0.is_null()
        && !previous.0.is_null()
        && saved != 0
        && !region.0.is_null()
        && compose_and_copy(hdc, memory, art, inner, region);
    // Cleanup runs on every path. The bitmap is restored from the DC only after the copy.
    if saved != 0 {
        let _ = RestoreDC(hdc, saved);
    }
    if !region.0.is_null() {
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(region.into());
    }
    if !previous.0.is_null() {
        let _ = SelectObject(memory, previous);
    }
    if !bitmap.0.is_null() {
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(bitmap.into());
    }
    let _ = DeleteDC(memory);
    copied
}

// Copies only after the background is known to be initialized and the clip is set.
// Any failure before the copy returns false with nothing drawn on `hdc`; the caller
// restores the clip.
unsafe fn compose_and_copy(
    hdc: HDC,
    memory: HDC,
    art: &PillArt,
    inner: RECT,
    region: HRGN,
) -> bool {
    let width = inner.right - inner.left;
    let height = inner.bottom - inner.top;
    let background = CreateSolidBrush(colorref(art.fill));
    if background.0.is_null() {
        return false;
    }
    let whole = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let filled = FillRect(memory, &whole, background) != 0;
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(background.into());
    if !filled {
        return false;
    }
    draw_typing_dots(memory, art, (inner.left, inner.top));
    // SelectClipRgn reports failure as RGN_ERROR; an unclipped copy would overwrite the ring.
    if SelectClipRgn(hdc, Some(region)) == RGN_ERROR {
        return false;
    }
    BitBlt(
        hdc,
        inner.left,
        inner.top,
        width,
        height,
        Some(memory),
        0,
        0,
        SRCCOPY,
    )
    .is_ok()
}

// Dots are placed relative to the pill and translated by `origin`, so the same code paints
// the window DC (origin 0,0) and the inner-rect bitmap (origin = inner top-left).
unsafe fn draw_typing_dots(target: HDC, art: &PillArt, origin: (i32, i32)) {
    let diameter = art.geometry.diameter;
    for dot in 0..TYPING_DOT_COUNT {
        let left = art.pill.left - origin.0 + art.geometry.left + dot as i32 * art.geometry.pitch;
        let top = art.pill.top - origin.1 + art.frame.tops[dot];
        fill_region(
            target,
            CreateEllipticRgn(left, top, left + diameter, top + diameter),
            blend(art.fill, art.ink, art.frame.alphas[dot]),
        );
    }
}

unsafe fn round_rect_region(rect: RECT, corner: i32) -> HRGN {
    CreateRoundRectRgn(rect.left, rect.top, rect.right, rect.bottom, corner, corner)
}

// Fills and releases the region and its brush on every path.
unsafe fn fill_region(hdc: HDC, region: HRGN, color: [u8; 3]) {
    let brush = CreateSolidBrush(colorref(color));
    let _ = FillRgn(hdc, region, brush);
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(region.into());
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(brush.into());
}

// Reads "Show animations in Windows" and High Contrast. A failed read keeps animation
// on (the Windows default) and High Contrast off.
unsafe fn refresh_typing_settings(hwnd: HWND) {
    let mut animations: i32 = 0;
    let motion = SystemParametersInfoW(
        SPI_GETCLIENTAREAANIMATION,
        0,
        Some((&mut animations as *mut i32).cast::<std::ffi::c_void>()),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    )
    .is_err()
        || animations != 0;
    let mut contrast = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    let high_contrast = SystemParametersInfoW(
        SPI_GETHIGHCONTRAST,
        std::mem::size_of::<HIGHCONTRASTW>() as u32,
        Some((&mut contrast as *mut HIGHCONTRASTW).cast::<std::ffi::c_void>()),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    )
    .is_ok()
        && contrast.dwFlags.contains(HCF_HIGHCONTRASTON);
    let _ = with_head(hwnd, |head| {
        head.motion = motion;
        head.high_contrast = high_contrast;
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypingEffect {
    ReadSettings,
    SyncTimer,
    RebuildRegion,
}

// Ordered effects for one head update. Settings come first because the timer depends
// on motion; the timer comes before the fallible region rebuild, so a failed region
// never leaves the timer running after typing has stopped.
fn typing_update_effects(started_typing: bool, shape_changed: bool) -> Vec<TypingEffect> {
    let mut effects = Vec::with_capacity(3);
    if started_typing {
        effects.push(TypingEffect::ReadSettings);
    }
    effects.push(TypingEffect::SyncTimer);
    if shape_changed {
        effects.push(TypingEffect::RebuildRegion);
    }
    effects
}

// The timer runs only while the pill animates: typing shown and motion allowed.
unsafe fn sync_typing_timer(hwnd: HWND) {
    let transition = with_head(hwnd, |head| {
        let wanted = head.typing && head.motion;
        if wanted == head.timer_running {
            None
        } else {
            head.timer_running = wanted;
            Some(wanted)
        }
    })
    .flatten();
    match transition {
        Some(true) => {
            if SetTimer(Some(hwnd), TYPING_TIMER_ID, TYPING_TIMER_MS, None) == 0 {
                let _ = with_head(hwnd, |head| head.timer_running = false);
            }
        }
        Some(false) => {
            let _ = KillTimer(Some(hwnd), TYPING_TIMER_ID);
        }
        None => {}
    }
}

unsafe fn stop_typing_timer(hwnd: HWND) {
    let was_running = with_head(hwnd, |head| {
        std::mem::replace(&mut head.timer_running, false)
    })
    .unwrap_or(false);
    if was_running {
        let _ = KillTimer(Some(hwnd), TYPING_TIMER_ID);
    }
}

// Invalidates only the pill's inner rect, and only when a dot's painted state changed.
unsafe fn advance_typing_frame(hwnd: HWND) {
    let dpi_scale = GetDpiForWindow(hwnd) as f64 / 96.0;
    let pill = badge_rect(TYPING_PILL_UNREAD, dpi_scale);
    let geometry = dot_geometry(pill.right - pill.left, dpi_scale);
    let changed = with_head(hwnd, |head| {
        if !(head.typing && head.motion) {
            return false;
        }
        let elapsed = GetTickCount64().saturating_sub(head.typing_started_ms);
        let frame = typing_frame(&geometry, elapsed, head.high_contrast);
        let changed = head.dot_frame != Some(frame);
        head.dot_frame = Some(frame);
        changed
    })
    .unwrap_or(false);
    if changed {
        let inner = inset_rect(pill, scaled(BADGE_RING, dpi_scale));
        let _ = InvalidateRect(Some(hwnd), Some(&inner as *const RECT), false);
    }
}

// Called on WM_SYSCOLORCHANGE and WM_SETTINGCHANGE: re-read the settings, sync the
// timer, and repaint if the pill is up.
unsafe fn refresh_typing_appearance(hwnd: HWND) {
    refresh_typing_settings(hwnd);
    sync_typing_timer(hwnd);
    let typing = with_head(hwnd, |head| {
        head.dot_frame = None;
        head.typing
    })
    .unwrap_or(false);
    if typing {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}
fn accessible_title(label: &str, unread: u64, typing: bool) -> Vec<u16> {
    let mut title = if label.is_empty() || label == "Conversation" {
        "Floating conversation".to_owned()
    } else {
        format!("Floating conversation {label}")
    };
    if typing {
        title.push_str(", typing");
        if unread > 0 {
            title.push_str(&format!(", {unread} unread messages"));
        }
    } else {
        title.push_str(&format!(", {unread} unread messages"));
    }
    title.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn clamps_heads_inside_normal_and_tiny_work_areas() {
        assert_eq!(super::clamp_axis(-20, 10, 210, 56), 10);
        assert_eq!(super::clamp_axis(500, 10, 210, 56), 154);
        assert_eq!(super::clamp_axis(500, 10, 30, 56), 10);
    }

    #[test]
    fn drag_threshold_distinguishes_click_from_drag() {
        assert!(!super::drag_started(3, 0, 4));
        assert!(super::drag_started(4, 0, 4));
        assert!(super::drag_started(0, -4, 4));
    }

    #[test]
    fn badge_rect_centers_each_tier_at_logical_41_across_dpi_scales() {
        assert_eq!(super::badge_tier(0), 0);
        assert_eq!(super::badge_tier(9), 1);
        assert_eq!(super::badge_tier(10), 2);
        assert_eq!(super::badge_tier(100), 3);
        for (scale, unread, expected) in [
            (1.0, 1, (32, 32, 50, 50)),
            (1.0, 10, (30, 32, 52, 50)),
            (1.0, 100, (28, 32, 54, 50)),
            (1.25, 1, (40, 40, 63, 63)),
            (1.25, 10, (37, 40, 65, 63)),
            (1.25, 100, (35, 40, 68, 63)),
            (1.5, 1, (49, 49, 76, 76)),
            (1.5, 10, (46, 49, 79, 76)),
            (1.5, 100, (43, 49, 82, 76)),
            (2.0, 1, (64, 64, 100, 100)),
            (2.0, 10, (60, 64, 104, 100)),
            (2.0, 100, (56, 64, 108, 100)),
        ] {
            let rect = super::badge_rect(unread, scale);
            assert_eq!(
                (rect.left, rect.top, rect.right, rect.bottom),
                expected,
                "unread {unread} at {scale}x"
            );
        }
        assert_eq!(super::badge_text(100), "99+");
    }

    #[test]
    fn photo_badges_use_a_neutral_ring() {
        assert_eq!(super::badge_ring_color(true, [0x24, 0x4a, 0xa3]), [0x1f; 3]);
        assert_eq!(
            super::badge_ring_color(false, [0x24, 0x4a, 0xa3]),
            [0x24, 0x4a, 0xa3]
        );
    }

    #[test]
    fn generic_labels_omit_the_redundant_conversation_name_in_accessibility_title() {
        let title = String::from_utf16_lossy(&super::accessible_title("", 3, false));
        assert_eq!(
            title.trim_end_matches('\0'),
            "Floating conversation, 3 unread messages"
        );
        let title = String::from_utf16_lossy(&super::accessible_title("Conversation", 3, false));
        assert_eq!(
            title.trim_end_matches('\0'),
            "Floating conversation, 3 unread messages"
        );
    }

    fn title_text(label: &str, unread: u64, typing: bool) -> String {
        let title = super::accessible_title(label, unread, typing);
        String::from_utf16_lossy(&title)
            .trim_end_matches('\0')
            .to_owned()
    }

    #[test]
    fn typing_title_names_the_state_before_the_unread_count() {
        assert_eq!(
            title_text("Ada", 0, true),
            "Floating conversation Ada, typing"
        );
        assert_eq!(
            title_text("Ada", 3, true),
            "Floating conversation Ada, typing, 3 unread messages"
        );
        assert_eq!(
            title_text("", 3, true),
            "Floating conversation, typing, 3 unread messages"
        );
        assert_eq!(
            title_text("Ada", 3, false),
            "Floating conversation Ada, 3 unread messages"
        );
    }

    #[test]
    fn typing_takes_the_99_plus_slot_even_without_unread() {
        assert_eq!(super::displayed_badge_tier(0, false), 0);
        assert_eq!(super::displayed_badge_tier(5, false), 1);
        assert_eq!(super::displayed_badge_tier(0, true), 3);
        assert_eq!(super::displayed_badge_tier(5, true), 3);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let slot = super::badge_rect(super::badge_slot_unread(0, true), scale);
            let pill = super::badge_rect(super::TYPING_PILL_UNREAD, scale);
            assert_eq!(
                (slot.left, slot.top, slot.right, slot.bottom),
                (pill.left, pill.top, pill.right, pill.bottom)
            );
        }
    }

    #[test]
    fn typing_stop_kills_the_timer_before_any_region_rebuild() {
        use super::TypingEffect::{ReadSettings, RebuildRegion, SyncTimer};
        assert_eq!(
            super::typing_update_effects(true, true),
            [ReadSettings, SyncTimer, RebuildRegion]
        );
        // Typing ending with a region change: the timer is synced first, so a failing
        // region rebuild (which returns early) cannot leave it running.
        assert_eq!(
            super::typing_update_effects(false, true),
            [SyncTimer, RebuildRegion]
        );
        assert_eq!(super::typing_update_effects(false, false), [SyncTimer]);
    }

    #[test]
    fn typing_drive_follows_the_spec_curve() {
        for (local_ms, expected) in [
            (0, 0.0),
            (60, 0.44),
            (240, 1.0),
            (420, 0.5),
            (600, 0.0),
            (1200, 0.0),
        ] {
            let drive = super::typing_drive(local_ms);
            assert!(
                (drive - expected).abs() < 0.01,
                "local {local_ms}: {drive} vs {expected}"
            );
        }
    }

    #[test]
    fn dots_stagger_by_160_ms_and_wrap_each_cycle() {
        assert_eq!(super::typing_dot_drive(0, 1), 0.0);
        assert_eq!(super::typing_dot_drive(159, 1), 0.0);
        assert_eq!(super::typing_dot_drive(160, 1), super::typing_drive(0));
        assert_eq!(super::typing_dot_drive(160 + 240, 1), 1.0);
        assert_eq!(super::typing_dot_drive(240, 0), 1.0);
        assert_eq!(super::typing_dot_drive(1200 + 160 + 240, 1), 1.0);
    }

    #[test]
    fn dots_stay_inside_the_pill_inner_fill_at_every_scale() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let pill = super::badge_rect(super::TYPING_PILL_UNREAD, scale);
            let width = pill.right - pill.left;
            let height = pill.bottom - pill.top;
            let ring = super::scaled(super::BADGE_RING, scale);
            let geometry = super::dot_geometry(width, scale);
            for dot in 0..super::TYPING_DOT_COUNT {
                let left = geometry.left + dot as i32 * geometry.pitch;
                for top in [
                    geometry.rest_top,
                    geometry.rest_top - geometry.travel,
                    geometry.static_top,
                ] {
                    let d = geometry.diameter;
                    assert!(left >= ring && top >= ring, "scale {scale} dot {dot}");
                    assert!(
                        left + d <= width - ring && top + d <= height - ring,
                        "scale {scale} dot {dot} top {top}"
                    );
                }
            }
        }
    }

    #[test]
    fn typing_frames_quantize_opacity_and_respect_high_contrast() {
        let geometry = super::dot_geometry(26, 1.0);
        let rest = super::typing_frame(&geometry, 0, false);
        assert_eq!(rest.tops, [9, 9, 9]);
        assert_eq!(rest.alphas, [8, 8, 8]);
        let peak = super::typing_frame(&geometry, 240, false);
        assert_eq!(peak.tops[0], 6);
        assert_eq!(peak.alphas[0], 16);
        let contrast = super::typing_frame(&geometry, 0, true);
        assert_eq!(contrast.alphas, [16, 16, 16]);
        let still = super::static_frame(&geometry, false);
        assert_eq!(still.tops, [7, 7, 7]);
        assert_eq!(still.alphas, [11, 11, 11]);
        assert_eq!(super::static_frame(&geometry, true).alphas, [16, 16, 16]);
    }

    #[test]
    fn blend_mixes_ink_into_fill_in_sixteenths() {
        let white = [0xff; 3];
        let ink = super::TYPING_INK;
        assert_eq!(super::blend(white, ink, 16), ink);
        assert_eq!(super::blend(white, ink, 0), white);
        assert_eq!(super::blend([255; 3], [0; 3], 8), [128; 3]);
    }

    #[test]
    fn colorref_round_trips_through_rgb() {
        assert_eq!(super::rgb_from_colorref(0x1E1C1C), [0x1C, 0x1C, 0x1E]);
        assert_eq!(super::colorref([0x1C, 0x1C, 0x1E]).0, 0x1E1C1C);
    }

    #[test]
    fn colorref_uses_windows_bgr_byte_order() {
        assert_eq!(super::colorref([0x24, 0x4a, 0xa3]).0, 0xA34A24);
    }

    #[test]
    fn cross_monitor_drag_uses_cursor_monitor_scale_and_negative_origin() {
        let (x, y, size) = super::drag_geometry(
            windows::Win32::Foundation::POINT { x: -900, y: 300 },
            (20.0, 12.0),
            windows::Win32::Foundation::RECT {
                left: -1600,
                top: -200,
                right: 0,
                bottom: 700,
            },
            1.25,
        );
        assert_eq!((x, y, size), (-925, 285, 70));
    }

    #[test]
    fn cross_monitor_drag_clamps_with_destination_dpi_size() {
        let (x, y, size) = super::drag_geometry(
            windows::Win32::Foundation::POINT { x: 2200, y: 500 },
            (28.0, 28.0),
            windows::Win32::Foundation::RECT {
                left: 1440,
                top: 0,
                right: 2040,
                bottom: 400,
            },
            2.0,
        );
        assert_eq!((x, y, size), (1928, 288, 112));
    }
}
