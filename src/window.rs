//! Window helpers: work area (excludes taskbar), DPI scale, top-center placement,
//! rounded-rect window region (pill shape, like Nusa's SetWindowRgn approach).
//!
//! Note (Phase 0 deviation, honest): shape uses SetWindowRgn (rounded rect),
//! not a DComp visual tree. D2D draws the pill; the region clips the window
//! to the rounded rect (so corners are transparent + click-through). DComp
//! migration is Phase 5 polish if visual quality demands it (Nusa itself used
//! SetWindowRgn for shape; timer-driven animation, not DComp keyframes).

use windows::Win32::Foundation::{BOOL, HWND, RECT};
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, DeleteObject, HGDIOBJ, HRGN};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, SetWindowRgn, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

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
/// (1920x1080) if the call fails (should not happen on a normal desktop).
pub fn work_area() -> RECT {
    unsafe {
        let mut rect = RECT::default();
        let ok = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut rect as *mut RECT as *mut core::ffi::c_void,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        if ok.0 != 0 && rect.right > rect.left && rect.bottom > rect.top {
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

/// Place + resize the window top-center of the work area (no activation).
/// width/height are PHYSICAL pixels (already DPI-scaled by the caller).
pub fn place_top_center(hwnd: HWND, width: i32, height: i32) {
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
}

/// Set a rounded-rect window region (pill shape). radius = height/2 for short
/// pills (fully rounded), 28pt*scale for tall cards (Aura's 28pt bottom
/// rounding; top corners use the same radius as an approximation — the
/// concave tuck is drawn by D2D, the region only clips hit-testing).
/// The system owns the region after a successful SetWindowRgn (do not delete).
/// width/height/radius are PHYSICAL pixels.
pub fn set_pill_region(hwnd: HWND, width: i32, height: i32, radius: i32) {
    unsafe {
        let rgn: HRGN = CreateRoundRectRgn(0, 0, width, height, radius * 2, radius * 2);
        if rgn.0 == 0 {
            return;
        }
        let ok = SetWindowRgn(hwnd, rgn, BOOL(1));
        if ok.0 == 0 {
            // System did not take ownership; free it to avoid a GDI leak.
            let _ = DeleteObject(HGDIOBJ(rgn.0));
        }
    }
}
