use crate::error::{BridgeError, BridgeResult};
use serde::Deserialize;
use tauri::WebviewWindow;

#[cfg(target_os = "windows")]
pub const HOVER_EVENT: &str = "peppy://maximize-button-hover";

const MAX_REGION_COORDINATE: f64 = 100_000.0;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaximizeButtonRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn set_region(
    _window: WebviewWindow,
    region: Option<MaximizeButtonRegion>,
) -> BridgeResult<()> {
    if let Some(region) = region {
        validate_region(&region)
            .map_err(|message| BridgeError::new("invalid-maximize-button-region", message))?;
    }

    #[cfg(target_os = "windows")]
    unsafe {
        native::set_region(&_window, region)
    }

    #[cfg(not(target_os = "windows"))]
    Ok(())
}

fn validate_region(region: &MaximizeButtonRegion) -> Result<(), &'static str> {
    let values = [region.x, region.y, region.width, region.height];
    if values
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err("The maximize button region must contain finite, non-negative values.");
    }
    if region.x + region.width > MAX_REGION_COORDINATE
        || region.y + region.height > MAX_REGION_COORDINATE
    {
        return Err("The maximize button region is too large.");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
mod native {
    use super::{BridgeError, BridgeResult, MaximizeButtonRegion, HOVER_EVENT};
    use std::sync::{Mutex, OnceLock};
    use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
    use windows::{
        core::{s, w, PCWSTR},
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, WPARAM},
            Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT},
            System::{
                LibraryLoader::{GetModuleHandleW, GetProcAddress},
                SystemInformation::OSVERSIONINFOW,
            },
            UI::{
                Input::KeyboardAndMouse::{
                    TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
                },
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, IsZoomed, PostMessageW, RegisterClassW,
                    SetWindowPos, ShowWindow, HTMAXBUTTON, HWND_TOP, SC_MAXIMIZE, SC_RESTORE,
                    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WM_DESTROY, WM_ERASEBKGND,
                    WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
                    WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_PAINT, WM_SYSCOMMAND, WNDCLASSW, WS_CHILD,
                    WS_CLIPSIBLINGS, WS_VISIBLE,
                },
            },
        },
    };

    const CLASS_NAME: &[u16] = &[
        b'P' as u16,
        b'e' as u16,
        b'p' as u16,
        b'p' as u16,
        b'y' as u16,
        b'S' as u16,
        b'n' as u16,
        b'a' as u16,
        b'p' as u16,
        b'L' as u16,
        b'a' as u16,
        b'y' as u16,
        b'o' as u16,
        b'u' as u16,
        b't' as u16,
        0,
    ];

    /// Window handles are stored as integers because `HWND` wraps a raw pointer and is not
    /// `Send`, which a process-wide `Mutex` requires.
    struct Overlay {
        hwnd: isize,
        parent: isize,
        app: AppHandle,
        hovered: bool,
        pressed: bool,
    }

    fn key(hwnd: HWND) -> isize {
        hwnd.0 as isize
    }

    fn handle(key: isize) -> HWND {
        HWND(key as *mut std::ffi::c_void)
    }

    #[derive(Default)]
    struct Registry {
        class_registered: bool,
        overlay: Option<Overlay>,
    }

    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    static WINDOWS_11_OR_LATER: OnceLock<bool> = OnceLock::new();

    fn registry() -> &'static Mutex<Registry> {
        REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
    }

    pub(super) unsafe fn set_region(
        window: &WebviewWindow,
        region: Option<MaximizeButtonRegion>,
    ) -> BridgeResult<()> {
        if !is_windows_11_or_later() {
            return Ok(());
        }

        match region {
            Some(region) => {
                let hwnd = window
                    .hwnd()
                    .map_err(|error| BridgeError::new("window", error.to_string()))?;
                let scale = window
                    .scale_factor()
                    .map_err(|error| BridgeError::new("window", error.to_string()))?;
                let overlay = ensure_overlay(hwnd, window.app_handle().clone())?;
                let x = physical(region.x, scale)?;
                let y = physical(region.y, scale)?;
                let width = physical(region.width, scale)?;
                let height = physical(region.height, scale)?;
                let _ = SetWindowPos(
                    overlay,
                    Some(HWND_TOP),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            None => {
                let overlay = registry()
                    .lock()
                    .ok()
                    .and_then(|state| state.overlay.as_ref().map(|overlay| handle(overlay.hwnd)));
                let Some(overlay) = overlay else {
                    return Ok(());
                };
                let was_hovered = registry()
                    .lock()
                    .ok()
                    .and_then(|mut state| {
                        state.overlay.as_mut().map(|overlay| {
                            overlay.pressed = false;
                            let hovered = overlay.hovered;
                            overlay.hovered = false;
                            hovered
                        })
                    })
                    .unwrap_or(false);
                let _ = ShowWindow(overlay, SW_HIDE);
                if was_hovered {
                    emit_hover(overlay, false);
                }
            }
        }
        Ok(())
    }

    fn is_windows_11_or_later() -> bool {
        *WINDOWS_11_OR_LATER.get_or_init(|| unsafe {
            let Ok(ntdll) = GetModuleHandleW(w!("ntdll.dll")) else {
                return false;
            };
            let Some(rtl_get_version) = GetProcAddress(ntdll, s!("RtlGetVersion")) else {
                return false;
            };
            type RtlGetVersion = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
            let rtl_get_version: RtlGetVersion = std::mem::transmute(rtl_get_version);
            let mut version = OSVERSIONINFOW {
                dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
                ..Default::default()
            };
            rtl_get_version(&mut version) >= 0 && version.dwBuildNumber >= 22_000
        })
    }

    unsafe fn ensure_overlay(parent: HWND, app: AppHandle) -> BridgeResult<HWND> {
        if let Some(hwnd) = registry()
            .lock()
            .map_err(|_| BridgeError::new("window", "The snap layout registry is unavailable."))?
            .overlay
            .as_ref()
            .map(|overlay| handle(overlay.hwnd))
        {
            return Ok(hwnd);
        }
        ensure_class()?;
        let hwnd = CreateWindowExW(
            Default::default(),
            PCWSTR(CLASS_NAME.as_ptr()),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(
                GetModuleHandleW(None)
                    .map_err(|error| BridgeError::new("window", error.to_string()))?
                    .into(),
            ),
            None,
        )
        .map_err(|error| BridgeError::new("window", error.to_string()))?;
        registry()
            .lock()
            .map_err(|_| BridgeError::new("window", "The snap layout registry is unavailable."))?
            .overlay = Some(Overlay {
            hwnd: key(hwnd),
            parent: key(parent),
            app,
            hovered: false,
            pressed: false,
        });
        Ok(hwnd)
    }

    unsafe fn ensure_class() -> BridgeResult<()> {
        if registry()
            .lock()
            .map_err(|_| BridgeError::new("window", "The snap layout registry is unavailable."))?
            .class_registered
        {
            return Ok(());
        }
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: GetModuleHandleW(None)
                .map_err(|error| BridgeError::new("window", error.to_string()))?
                .into(),
            lpszClassName: PCWSTR(CLASS_NAME.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(BridgeError::new(
                "window",
                "Windows could not register the snap layout overlay class.",
            ));
        }
        registry()
            .lock()
            .map_err(|_| BridgeError::new("window", "The snap layout registry is unavailable."))?
            .class_registered = true;
        Ok(())
    }

    fn physical(value: f64, scale: f64) -> BridgeResult<i32> {
        let value = value * scale;
        if !value.is_finite() || value > i32::MAX as f64 {
            return Err(BridgeError::new(
                "invalid-maximize-button-region",
                "The maximize button region is too large.",
            ));
        }
        Ok(value.round() as i32)
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let this = key(hwnd);
        let over_max_button = wparam.0 == HTMAXBUTTON as usize;
        match message {
            WM_NCHITTEST => return LRESULT(HTMAXBUTTON as isize),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_NCMOUSEMOVE => {
                let should_emit = registry().lock().ok().and_then(|mut state| {
                    let overlay = state.overlay.as_mut()?;
                    if overlay.hwnd != this || overlay.hovered {
                        return Some(false);
                    }
                    overlay.hovered = true;
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    let _ = TrackMouseEvent(&mut track);
                    Some(true)
                });
                if should_emit == Some(true) {
                    emit_hover(hwnd, true);
                }
                return LRESULT(0);
            }
            WM_NCMOUSELEAVE => {
                let should_emit = registry().lock().ok().and_then(|mut state| {
                    let overlay = state.overlay.as_mut()?;
                    if overlay.hwnd != this || !overlay.hovered {
                        return Some(false);
                    }
                    overlay.hovered = false;
                    overlay.pressed = false;
                    Some(true)
                });
                if should_emit == Some(true) {
                    emit_hover(hwnd, false);
                }
                return LRESULT(0);
            }
            WM_NCLBUTTONDOWN if over_max_button => {
                if let Ok(mut state) = registry().lock() {
                    if let Some(overlay) = state
                        .overlay
                        .as_mut()
                        .filter(|overlay| overlay.hwnd == this)
                    {
                        overlay.pressed = true;
                    }
                }
                return LRESULT(0);
            }
            WM_NCLBUTTONUP if over_max_button => {
                let parent = registry().lock().ok().and_then(|mut state| {
                    let overlay = state
                        .overlay
                        .as_mut()
                        .filter(|overlay| overlay.hwnd == this)?;
                    let pressed = overlay.pressed;
                    overlay.pressed = false;
                    pressed.then_some(handle(overlay.parent))
                });
                if let Some(parent) = parent {
                    let command = if IsZoomed(parent).as_bool() {
                        SC_RESTORE
                    } else {
                        SC_MAXIMIZE
                    };
                    let _ = PostMessageW(
                        Some(parent),
                        WM_SYSCOMMAND,
                        WPARAM(command as usize),
                        LPARAM(0),
                    );
                }
                return LRESULT(0);
            }
            WM_NCLBUTTONDBLCLK if over_max_button => return LRESULT(0),
            WM_DESTROY => {
                if let Ok(mut state) = registry().lock() {
                    if state
                        .overlay
                        .as_ref()
                        .is_some_and(|overlay| overlay.hwnd == this)
                    {
                        state.overlay = None;
                    }
                }
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }

    fn emit_hover(hwnd: HWND, hovered: bool) {
        let this = key(hwnd);
        let app = registry().lock().ok().and_then(|state| {
            state
                .overlay
                .as_ref()
                .filter(|overlay| overlay.hwnd == this)
                .map(|overlay| overlay.app.clone())
        });
        if let Some(app) = app {
            let _ = app.emit_to(crate::tray::MAIN, HOVER_EVENT, hovered);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_region, MaximizeButtonRegion};

    fn region(x: f64, y: f64, width: f64, height: f64) -> MaximizeButtonRegion {
        MaximizeButtonRegion {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn accepts_finite_non_negative_regions_within_the_overlay_limit() {
        assert!(validate_region(&region(10.5, 20.25, 32.0, 28.0)).is_ok());
        assert!(validate_region(&region(0.0, 0.0, 0.0, 0.0)).is_ok());
    }

    #[test]
    fn rejects_invalid_or_oversized_regions() {
        for invalid in [
            region(f64::NAN, 0.0, 1.0, 1.0),
            region(f64::INFINITY, 0.0, 1.0, 1.0),
            region(-1.0, 0.0, 1.0, 1.0),
            region(99_999.0, 0.0, 2.0, 1.0),
            region(0.0, 99_999.0, 1.0, 2.0),
        ] {
            assert!(validate_region(&invalid).is_err());
        }
    }
}
