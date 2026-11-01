//! Window helpers: work area (excludes taskbar), DPI scale, and fixed-canvas
//! top-center placement for per-pixel alpha layered rendering (`UpdateLayeredWindow`).
//!
//! Using a fixed-size transparent layered canvas (`CANVAS_W x CANVAS_H` = `720x300`
//! logical points) means the Win32 `HWND` never moves or resizes during animations.
//! Every pixel with `alpha == 0` outside the current `(cur_w, cur_h)` notch shape is
//! automatically transparent and click-through in Win32 (`ULW_ALPHA`), eliminating all
//! `SetWindowPos`, `SetWindowRgn`, and swapchain `Resize` stutter.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, FALSE, HINSTANCE, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, EnumWindows, GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible,
    SetWindowPos, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
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

/// One physical display. `list_monitors()` sorts primary first.
pub struct MonitorInfo {
    pub index: usize,
    pub primary: bool,
    pub device: String,
    pub work: RECT,
}

unsafe extern "system" fn enum_mon_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _rc: *mut RECT,
    lp: LPARAM,
) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<HMONITOR>);
    out.push(hmon);
    TRUE
}

/// All attached monitors, primary sorted first. Empty vec on failure.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let mut hmons: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_mon_proc),
            LPARAM(&mut hmons as *mut Vec<HMONITOR> as isize),
        );
    }
    let mut infos: Vec<MonitorInfo> = hmons
        .into_iter()
        .filter_map(|hmon| unsafe {
            let mut mi = MONITORINFOEXW::default();
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmon, &mut mi as *mut _ as *mut MONITORINFO).as_bool() {
                let name_len = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
                let device = String::from_utf16_lossy(&mi.szDevice[..name_len]);
                Some(MonitorInfo {
                    index: 0,
                    primary: (mi.monitorInfo.dwFlags & 1) != 0, // MONITORINFOF_PRIMARY
                    device,
                    work: mi.monitorInfo.rcWork,
                })
            } else {
                None
            }
        })
        .collect();
    infos.sort_by(|a, b| b.primary.cmp(&a.primary).then(a.device.cmp(&b.device)));
    for (i, m) in infos.iter_mut().enumerate() {
        m.index = i;
    }
    infos
}

/// True when the foreground window covers its entire monitor (borderless
/// fullscreen game, video or slideshow).
pub fn foreground_is_fullscreen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let mut rc = RECT::default();
        if GetWindowRect(fg, &mut rc).is_err() {
            return false;
        }
        let hmon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO::default();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return false;
        }
        let mr = mi.rcMonitor;
        rc.left == mr.left && rc.top == mr.top && rc.right == mr.right && rc.bottom == mr.bottom
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

/// Place the fixed-size transparent canvas at the top-center of the selected
/// monitor's work area (no activation). `monitor_idx` indexes `list_monitors()`
/// (primary first); out of range falls back to the primary monitor.
pub fn place_canvas_on_monitor(hwnd: HWND, monitor_idx: i32) -> (i32, i32, i32, i32) {
    let mons = list_monitors();
    let wa = mons
        .get(monitor_idx.max(0) as usize)
        .map(|m| m.work)
        .unwrap_or_else(|| {
            mons.iter()
                .find(|m| m.primary)
                .map(|m| m.work)
                .unwrap_or_else(work_area)
        });
    let scale = dpi_scale(hwnd);
    let width = (CANVAS_W * scale).round() as i32;
    let height = (CANVAS_H * scale).round() as i32;
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

/// Place the fixed-size transparent canvas at the top-center of the work area (no activation).
/// Called once at startup (and if DPI changes), never during animation frames.
pub fn place_canvas_top_center(hwnd: HWND) -> (i32, i32, i32, i32) {
    place_canvas_on_monitor(hwnd, 0)
}

/// Create an additional pill canvas window (same layered/topmost style as the main one).
/// The caller must set GWLP_USERDATA, position it and show it.
pub fn create_canvas_window(
    hinstance: HINSTANCE,
    class_name: PCWSTR,
) -> windows::core::Result<HWND> {
    unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name,
            w!("Aura"),
            WS_POPUP,
            0,
            0,
            760,
            330,
            None,
            None,
            hinstance,
            None,
        )
    }
}

/// Top-left position for a canvas on the given work rect (top-center), at `dpi` scale.
pub fn canvas_xy_for_work(work: &RECT, dpi: f32) -> (i32, i32) {
    let width = (CANVAS_W * dpi).round() as i32;
    let x = work.left + (work.right - work.left - width) / 2;
    (x, work.top)
}

/// True when any visible, non-minimized top-level window (other than `exclude`)
/// overlaps `pill` (screen pixels).
pub fn is_rect_covered_by_window(pill: RECT, exclude: &[HWND]) -> bool {
    struct Ctx {
        pill: RECT,
        exclude: Vec<isize>,
        covered: bool,
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        if ctx.covered {
            return FALSE;
        }
        if ctx.exclude.iter().any(|&e| e == hwnd.0 as isize) {
            return TRUE;
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }
        let mut rc = RECT::default();
        if GetWindowRect(hwnd, &mut rc).is_err() {
            return TRUE;
        }
        if rc.right <= rc.left || rc.bottom <= rc.top {
            return TRUE;
        }
        if rc.left < ctx.pill.right
            && rc.right > ctx.pill.left
            && rc.top < ctx.pill.bottom
            && rc.bottom > ctx.pill.top
        {
            ctx.covered = true;
            return FALSE;
        }
        TRUE
    }
    let mut ctx = Ctx {
        pill,
        exclude: exclude.iter().map(|h| h.0 as isize).collect(),
        covered: false,
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.covered
}
