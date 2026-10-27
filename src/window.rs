//! Window helpers: work area (excludes taskbar), DPI scale, top-center placement,
//! Nusa-shaped window region (boxy, square top + rounded bottom, via SetWindowRgn).
//!
//! Note (Phase 0 deviation, honest): shape uses SetWindowRgn (rounded rect),
//! not a DComp visual tree. D2D draws the pill; the region clips the window
//! to the rounded rect (so corners are transparent + click-through). DComp
//! migration is Phase 5 polish if visual quality demands it (Nusa itself used
//! SetWindowRgn for shape; timer-driven animation, not DComp keyframes).

use windows::Win32::Foundation::{BOOL, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ALTERNATE, CreatePolygonRgn, DeleteObject, HGDIOBJ, HRGN, SetWindowRgn,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
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

/// Set a Nusa-shaped window region (boxy, square top merging into
/// the top edge/bezel, rounded bottom): top corners square (radius 0,
/// flush with y=0), bottom corners convex (radius param: height/2
/// for short pills, 28pt*scale for tall cards). Built as a polygon
/// (bottom arcs, 8 segments each). Matches the D2D path geometry in
/// renderer.rs (same outline, physical pixels here).
/// The system owns the region after a successful SetWindowRgn (do not delete).
/// width/height/radius are PHYSICAL pixels.
pub fn set_pill_region(hwnd: HWND, width: i32, height: i32, radius: i32) {
    let w = width.max(1);
    let h = height.max(1);
    let r = radius.clamp(0, h / 2);
    // Polygon points (clockwise from top-left, square top).
    let mut pts: Vec<POINT> = Vec::new();
    let seg = 8i32;
    let mut push_arc = |pts: &mut Vec<POINT>, cx: i32, cy: i32, rad: i32, a0: f32, a1: f32| {
        for i in 1..seg {
            let a = a0 + (a1 - a0) * (i as f32 / seg as f32);
            pts.push(POINT {
                x: cx + (rad as f32 * a.cos()).round() as i32,
                y: cy + (rad as f32 * a.sin()).round() as i32,
            });
        }
    };
    // Top edge (square corners, flush with top edge/bezel).
    pts.push(POINT { x: 0, y: 0 });
    pts.push(POINT { x: w, y: 0 });
    // Right side.
    pts.push(POINT { x: w, y: h - r });
    // Bottom-right (convex, center (w-r, h-r)): 0deg -> 90deg.
    push_arc(&mut pts, w - r, h - r, r, 0.0, std::f32::consts::FRAC_PI_2);
    pts.push(POINT { x: w - r, y: h });
    // Bottom edge.
    pts.push(POINT { x: r, y: h });
    // Bottom-left (convex, center (r, h-r)): 90deg -> 180deg.
    push_arc(&mut pts, r, h - r, r, std::f32::consts::FRAC_PI_2, std::f32::consts::PI);
    pts.push(POINT { x: 0, y: h - r });
    // (Region auto-closes back to (0, 0) along the left side.)
    unsafe {
        let rgn: HRGN = CreatePolygonRgn(&pts, ALTERNATE);
        if rgn.0 == std::ptr::null_mut() {
            return;
        }
        let ok = SetWindowRgn(hwnd, rgn, BOOL(1));
        if ok == 0 {
            // System did not take ownership; free it to avoid a GDI leak.
            let _ = DeleteObject(HGDIOBJ(rgn.0));
        }
    }
}
