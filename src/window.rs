//! Window helpers: work area (excludes taskbar), DPI scale, and fixed-canvas
//! top-center placement for per-pixel alpha layered rendering (`UpdateLayeredWindow`).
//!
//! Using a fixed-size transparent layered canvas (`CANVAS_W x CANVAS_H` = `720x300`
//! logical points) means the Win32 `HWND` never moves or resizes during animations.
//! Every pixel with `alpha == 0` outside the current `(cur_w, cur_h)` notch shape is
//! automatically transparent and click-through in Win32 (`ULW_ALPHA`), eliminating all
//! `SetWindowPos`, `SetWindowRgn`, and swapchain `Resize` stutter.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// Logical width of the fixed transparent canvas (accommodates `700pt` Lyrics + `1.15x` Tray bounce headroom).
pub const CANVAS_W: f32 = 760.0;
/// Logical height of the fixed transparent canvas (accommodates `280pt` Tray * `1.15x` bounce headroom).
pub const CANVAS_H: f32 = 330.0;

/// DPI scale for a window (1.0 at 96 DPI = 100%).
pub fn dpi_scale(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// Primary monitor work area (excludes taskbar). Falls back to full screen
/// (1920x1080) if the call fails.
pub fn work_area() -> RECT {
    unsafe {
        let mut rect = RECT::default();
        let result = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        if result.is_ok() && rect.right > rect.left && rect.bottom > rect.top {
            return rect;
        }
    }
    RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    }
}

/// Place the fixed-size transparent canvas at the top-center of the work area (no activation).
/// Called once at startup (and if DPI changes), never during animation frames.
pub fn place_canvas_top_center(hwnd: HWND) -> (i32, i32, i32, i32) {
    let scale = dpi_scale(hwnd);
    let width = (CANVAS_W * scale).round() as i32;
    let height = (CANVAS_H * scale).round() as i32;
    let wa = work_area();
    let x = wa.left + (wa.right - wa.left - width) / 2;
    let y = wa.top;
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    (x, y, width, height)
}
