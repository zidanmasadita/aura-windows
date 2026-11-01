//! Standalone Aura Settings Window.
//! Matches macOS/iPadOS dark frosted floating settings window with left sidebar,
//! red traffic light close button, segmented size controls, and iOS-style switches.

use std::mem::ManuallyDrop;
use std::sync::Mutex;
use windows::core::{w, Interface, PCWSTR, Result};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::AuraSettings;
use crate::window;

pub const WIN_W: f32 = 740.0;
pub const WIN_H: f32 = 540.0;
pub const SIDEBAR_W: f32 = 210.0;

pub const WM_SETTINGS_UPDATED: u32 = WM_USER + 55;
pub const WM_SETTINGS_PREVIEW: u32 = WM_USER + 56;

static SETTINGS_HWND: Mutex<Option<isize>> = Mutex::new(None);
static MAIN_HWND: Mutex<Option<isize>> = Mutex::new(None);

pub fn open(main_hwnd: HWND) {
    let mut guard = SETTINGS_HWND.lock().unwrap();
    if let Some(raw) = *guard {
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        unsafe {
            if IsWindow(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                return;
            }
        }
    }
    *MAIN_HWND.lock().unwrap() = Some(main_hwnd.0 as isize);

    if let Ok(hwnd) = create_window() {
        *guard = Some(hwnd.0 as isize);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

pub fn notify_main() {
    if let Some(raw) = *MAIN_HWND.lock().unwrap() {
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        unsafe {
            let _ = PostMessageW(hwnd, WM_SETTINGS_UPDATED, WPARAM(0), LPARAM(0));
        }
    }
}

/// Ask the main pill to open itself so the user can preview a size/opacity change live.
/// HWND of the settings window, if open (used to exclude it from cover checks).
pub fn settings_hwnd() -> Option<HWND> {
    (*SETTINGS_HWND.lock().unwrap()).map(|raw| HWND(raw as *mut core::ffi::c_void))
}

pub fn notify_preview() {
    if let Some(raw) = *MAIN_HWND.lock().unwrap() {
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        unsafe {
            let _ = PostMessageW(hwnd, WM_SETTINGS_PREVIEW, WPARAM(0), LPARAM(0));
        }
    }
}

struct State {
    settings: AuraSettings,
    selected_tab: usize,
    hovered_tab: Option<usize>,
    hovered_close: bool,
    mouse_pt: Option<(f32, f32)>,
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    mem_dc: HDC,
    dib_bmp: HBITMAP,
    old_bmp: HGDIOBJ,
    target: Option<ID2D1RenderTarget>,
    fmt_title: Option<IDWriteTextFormat>,
    fmt_sec_hdr: Option<IDWriteTextFormat>,
    fmt_row_title: Option<IDWriteTextFormat>,
    fmt_row_sub: Option<IDWriteTextFormat>,
    fmt_sidebar: Option<IDWriteTextFormat>,
    fmt_seg: Option<IDWriteTextFormat>,
    tab_icons: [Option<ID2D1Bitmap>; 12],
    dragging_slider: Option<SliderKind>,
    slider_hover: Option<SliderKind>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SliderKind {
    Size,
    Opacity,
    DodgeRadius,
    DodgeDelay,
}

fn slider_tab(kind: SliderKind) -> usize {
    match kind {
        SliderKind::Size => 0,
        SliderKind::Opacity => 2,
        SliderKind::DodgeRadius | SliderKind::DodgeDelay => 10,
    }
}

fn slider_min_max(kind: SliderKind) -> (f32, f32) {
    match kind {
        SliderKind::Size => (0.8, 1.5),
        SliderKind::Opacity => (0.4, 1.0),
        SliderKind::DodgeRadius => (60.0, 300.0),
        SliderKind::DodgeDelay => (0.0, 1000.0),
    }
}

fn slider_tick(kind: SliderKind) -> Option<f32> {
    match kind {
        SliderKind::Size => Some(1.0),
        SliderKind::Opacity => None,
        SliderKind::DodgeRadius => Some(140.0),
        SliderKind::DodgeDelay => Some(350.0),
    }
}

/// (track_x, track_center_y, track_w) — fixed geometry per slider.
fn slider_geom(kind: SliderKind) -> (f32, f32, f32) {
    match kind {
        SliderKind::Size => (250.0, 218.0, 432.0),
        SliderKind::Opacity => (250.0, 148.0, 432.0),
        SliderKind::DodgeRadius => (250.0, 350.0, 432.0),
        SliderKind::DodgeDelay => (250.0, 410.0, 432.0),
    }
}

fn slider_value(s: &State, kind: SliderKind) -> f32 {
    match kind {
        SliderKind::Size => s.settings.notch_scale,
        SliderKind::Opacity => s.settings.pill_opacity,
        SliderKind::DodgeRadius => s.settings.dodge_radius,
        SliderKind::DodgeDelay => s.settings.dodge_delay_ms as f32,
    }
}

fn slider_format(kind: SliderKind, v: f32) -> String {
    match kind {
        SliderKind::Size | SliderKind::Opacity => format!("{:.0}%", v * 100.0),
        SliderKind::DodgeRadius => format!("{:.0} px", v),
        SliderKind::DodgeDelay => format!("{:.0} ms", v),
    }
}

fn slider_snap(kind: SliderKind, v: f32) -> f32 {
    if let Some(t) = slider_tick(kind) {
        let tol = match kind {
            SliderKind::Size => 0.025,
            SliderKind::DodgeRadius => 8.0,
            _ => 25.0,
        };
        if (v - t).abs() < tol {
            return t;
        }
    }
    v
}

fn apply_slider(state: &mut State, kind: SliderKind, x: f32) {
    let (sx, _, w) = slider_geom(kind);
    let (min, max) = slider_min_max(kind);
    let t = ((x - sx) / w).clamp(0.0, 1.0);
    let v = slider_snap(kind, min + t * (max - min));
    match kind {
        SliderKind::Size => state.settings.notch_scale = v,
        SliderKind::Opacity => state.settings.pill_opacity = v,
        SliderKind::DodgeRadius => state.settings.dodge_radius = v,
        SliderKind::DodgeDelay => state.settings.dodge_delay_ms = v.round() as u64,
    }
    state.settings.save();
    notify_main();
}

fn slider_hit(x: f32, y: f32) -> Option<SliderKind> {
    for kind in [
        SliderKind::Size,
        SliderKind::Opacity,
        SliderKind::DodgeRadius,
        SliderKind::DodgeDelay,
    ] {
        let (sx, cy, w) = slider_geom(kind);
        if x >= sx - 14.0 && x <= sx + w + 14.0 && y >= cy - 18.0 && y <= cy + 18.0 {
            return Some(kind);
        }
    }
    None
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

const WM_MOUSELEAVE_MSG: u32 = 0x02A3;

fn create_window() -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?.into();
        let class_name = w!("AuraSettingsWindow");

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(settings_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: crate::tray::load_aura_icon(64).unwrap_or_else(|| LoadIconW(None, IDI_APPLICATION).unwrap_or_default()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: crate::tray::load_aura_icon(32).unwrap_or_default(),
        };
        let _ = RegisterClassExW(&wc);

        let wa = window::work_area();
        let (ww, wh) = (WIN_W as i32, WIN_H as i32);
        let x = wa.left + (wa.right - wa.left - ww) / 2;
        let y = wa.top + (wa.bottom - wa.top - wh) / 2;

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            class_name,
            w!("Aura Settings"),
            WS_POPUP,
            x,
            y,
            ww,
            wh,
            None,
            None,
            hinstance,
            None,
        )?;

        let state = Box::new(State::new()?);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);

        // First paint:
        render_window(hwnd);

        Ok(hwnd)
    }
}

impl State {
    fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let make_fmt = |size: f32, weight: DWRITE_FONT_WEIGHT| -> Option<IDWriteTextFormat> {
                let fmt = dwrite
                    .CreateTextFormat(
                        w!("Segoe UI"),
                        None,
                        weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size,
                        w!("en-US"),
                    )
                    .ok()?;
                let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                Some(fmt)
            };

            let fmt_title = make_fmt(21.0, DWRITE_FONT_WEIGHT_BOLD);
            let fmt_sec_hdr = make_fmt(10.5, DWRITE_FONT_WEIGHT_SEMI_BOLD);
            let fmt_row_title = make_fmt(12.5, DWRITE_FONT_WEIGHT_MEDIUM);
            let fmt_row_sub = make_fmt(10.0, DWRITE_FONT_WEIGHT_NORMAL);
            let fmt_sidebar = make_fmt(11.5, DWRITE_FONT_WEIGHT_MEDIUM);
            let fmt_seg = make_fmt(10.5, DWRITE_FONT_WEIGHT_MEDIUM);

            let mem_dc = CreateCompatibleDC(None);
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = WIN_W as i32;
            bmi.bmiHeader.biHeight = -(WIN_H as i32); // top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;

            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let dib_bmp = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old_bmp = SelectObject(mem_dc, HGDIOBJ(dib_bmp.0));

            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let dc_target = factory.CreateDCRenderTarget(&props)?;
            let sub_rect = RECT {
                left: 0,
                top: 0,
                right: WIN_W as i32,
                bottom: WIN_H as i32,
            };
            dc_target.BindDC(mem_dc, &sub_rect)?;
            let target = dc_target.cast::<ID2D1RenderTarget>()?;
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

            Ok(Self {
                settings: AuraSettings::load(),
                selected_tab: 0,
                hovered_tab: None,
                hovered_close: false,
                mouse_pt: None,
                factory,
                dwrite,
                mem_dc,
                dib_bmp,
                old_bmp,
                target: Some(target),
                fmt_title,
                fmt_sec_hdr,
                fmt_row_title,
                fmt_row_sub,
                fmt_sidebar,
                fmt_seg,
                tab_icons: Default::default(),
                dragging_slider: None,
                slider_hover: None,
            })
        }
    }
}

unsafe extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let state = &mut *ptr;

    match msg {
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
            state.mouse_pt = Some((x, y));

            // Active slider drag: live value update
            if let Some(kind) = state.dragging_slider {
                apply_slider(state, kind, x);
                render_window(hwnd);
                return LRESULT(0);
            }

            // Slider knob hover
            let old_sh = state.slider_hover;
            state.slider_hover = slider_hit(x, y).filter(|k| slider_tab(*k) == state.selected_tab);

            let old_close = state.hovered_close;
            state.hovered_close = x >= 10.0 && x <= 32.0 && y >= 10.0 && y <= 32.0;

            let old_tab = state.hovered_tab;
            state.hovered_tab = None;
            if x < SIDEBAR_W && y >= 48.0 && y <= 48.0 + 11.0 * 34.0 {
                let idx = ((y - 48.0) / 34.0) as usize;
                if idx < 11 {
                    state.hovered_tab = Some(idx);
                }
            } else if x < SIDEBAR_W && y >= 490.0 && y <= 524.0 {
                state.hovered_tab = Some(11); // About
            }

            if old_close != state.hovered_close || old_tab != state.hovered_tab || old_sh != state.slider_hover {
                render_window(hwnd);
            }

            // Track mouse leave
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);

            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            state.mouse_pt = None;
            state.hovered_close = false;
            state.hovered_tab = None;
            render_window(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;

            // 1. Red close traffic light:
            if x >= 10.0 && x <= 32.0 && y >= 10.0 && y <= 32.0 {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return LRESULT(0);
            }

            // 2. Sidebar tab click:
            if x < SIDEBAR_W {
                if y >= 48.0 && y <= 48.0 + 11.0 * 34.0 {
                    let idx = ((y - 48.0) / 34.0) as usize;
                    if idx < 11 {
                        state.selected_tab = idx;
                        render_window(hwnd);
                        return LRESULT(0);
                    }
                } else if y >= 490.0 && y <= 524.0 {
                    state.selected_tab = 11; // About
                    render_window(hwnd);
                    return LRESULT(0);
                }
                // Dragging from empty sidebar header:
                if y < 48.0 {
                    let _ = ReleaseCapture();
                    let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
                }
                return LRESULT(0);
            }

            // 3. Header drag (content area header):
            if y < 48.0 {
                let _ = ReleaseCapture();
                let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
                return LRESULT(0);
            }

            // 4. Slider drag start (before generic content click):
            if let Some(kind) = slider_hit(x, y) {
                if slider_tab(kind) == state.selected_tab {
                    state.dragging_slider = Some(kind);
                    unsafe {
                        let _ = SetCapture(hwnd);
                    }
                    apply_slider(state, kind, x);
                    // Open the pill so the user sees the size/opacity change live.
                    if matches!(kind, SliderKind::Size | SliderKind::Opacity) {
                        notify_preview();
                    }
                    render_window(hwnd);
                    return LRESULT(0);
                }
            }

            // 5. Content Area Click based on active tab:
            handle_content_click(hwnd, state, x, y);

            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if state.dragging_slider.take().is_some() {
                unsafe {
                    let _ = ReleaseCapture();
                }
                render_window(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = Box::from_raw(state);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let mut guard = SETTINGS_HWND.lock().unwrap();
            *guard = None;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn handle_content_click(hwnd: HWND, state: &mut State, x: f32, y: f32) {
    if state.selected_tab == 0 {
        // Tab: General
        // Row 1: Start with Windows toggle (y: 84..126)
        if y >= 84.0 && y <= 126.0 && x >= 236.0 && x <= 696.0 {
            state.settings.start_with_windows = !state.settings.start_with_windows;
            crate::tray::set_autorun(state.settings.start_with_windows);
            state.settings.save();
            notify_main();
            render_window(hwnd);
            return;
        }

        // Minimal notch toggle: (y: 248..280)
        if y >= 248.0 && y <= 280.0 && x >= 236.0 && x <= 696.0 {
            state.settings.minimal_notch = !state.settings.minimal_notch;
            state.settings.save();
            notify_main();
            render_window(hwnd);
            return;
        }

        // Mascot toggle: (y: 282..314)
        if y >= 282.0 && y <= 314.0 && x >= 236.0 && x <= 696.0 {
            state.settings.eye_expressions = !state.settings.eye_expressions;
            state.settings.save();
            notify_main();
            render_window(hwnd);
            return;
        }

        // Reminder sounds toggle: (y: 346..400)
        if y >= 346.0 && y <= 400.0 && x >= 236.0 && x <= 696.0 {
            state.settings.sound_chimes = !state.settings.sound_chimes;
            state.settings.save();
            notify_main();
            render_window(hwnd);
            return;
        }

        // Hide in full screen toggle: (y: 432..486)
        if y >= 432.0 && y <= 486.0 && x >= 236.0 && x <= 696.0 {
            state.settings.hide_in_fullscreen = !state.settings.hide_in_fullscreen;
            state.settings.save();
            notify_main();
            render_window(hwnd);
            return;
        }
    } else if state.selected_tab == 1 {
        // Tab: Display
        // Show on all monitors toggle (y: 84..138)
        if y >= 84.0 && y <= 138.0 && x >= 236.0 && x <= 696.0 {
            state.settings.pill_show_all_monitors = !state.settings.pill_show_all_monitors;
            state.settings.save();
            notify_main();
            render_window(hwnd);
            return;
        }
        // Monitor map click
        if !state.settings.pill_show_all_monitors {
            for (i, (rx, ry, rw, rh)) in monitor_map_rects().iter().enumerate() {
                if x >= *rx && x <= rx + rw && y >= *ry && y <= ry + rh {
                    state.settings.pill_monitor = i as i32;
                    state.settings.save();
                    notify_main();
                    render_window(hwnd);
                    return;
                }
            }
        }
    } else if state.selected_tab == 3 {
        // Tab: Music and lyrics
        if y >= 84.0 && y <= 126.0 && x >= 236.0 && x <= 696.0 {
            state.settings.music_enabled = !state.settings.music_enabled;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 148.0 && y <= 190.0 && x >= 236.0 && x <= 696.0 {
            state.settings.lyrics_sync = !state.settings.lyrics_sync;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 212.0 && y <= 254.0 && x >= 236.0 && x <= 696.0 {
            state.settings.master_volume_control = !state.settings.master_volume_control;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        }
    } else if state.selected_tab == 4 {
        // Tab: To-do
        if y >= 84.0 && y <= 126.0 && x >= 236.0 && x <= 696.0 {
            state.settings.todo_enabled = !state.settings.todo_enabled;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 148.0 && y <= 190.0 && x >= 236.0 && x <= 696.0 {
            state.settings.sound_on_complete = !state.settings.sound_on_complete;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        }
    } else if state.selected_tab == 5 {
        // Tab: Pomodoro
        if y >= 84.0 && y <= 126.0 && x >= 236.0 && x <= 696.0 {
            state.settings.pomodoro_enabled = !state.settings.pomodoro_enabled;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 208.0 && y <= 238.0 && x >= 250.0 && x <= 682.0 {
            // Focus duration segmented
            if let Some(idx) = segmented_hit(x, 250.0, 432.0, 4) {
                state.settings.pomodoro_focus_mins = [15, 25, 45, 60][idx];
                state.settings.save();
                notify_main();
                render_window(hwnd);
            }
        }
    } else if state.selected_tab == 6 {
        // Tab: Water
        if y >= 84.0 && y <= 126.0 && x >= 236.0 && x <= 696.0 {
            state.settings.water_enabled = !state.settings.water_enabled;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 208.0 && y <= 238.0 && x >= 250.0 && x <= 682.0 {
            // Daily goal segmented
            if let Some(idx) = segmented_hit(x, 250.0, 432.0, 4) {
                state.settings.water_daily_goal = [1500, 2000, 2500, 3000][idx];
                state.settings.save();
                notify_main();
                render_window(hwnd);
            }
        }
    } else if state.selected_tab == 7 {
        // Tab: Shelf
        if y >= 84.0 && y <= 126.0 && x >= 236.0 && x <= 696.0 {
            state.settings.tray_enabled = !state.settings.tray_enabled;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        }
    } else if state.selected_tab == 8 {
        // Tab: Remove BG — model segmented (y: 132..162)
        if y >= 132.0 && y <= 162.0 && x >= 250.0 && x <= 682.0 {
            if let Some(idx) = segmented_hit(x, 250.0, 432.0, 3) {
                state.settings.removebg_model_idx = idx;
                state.settings.save();
                notify_main();
                render_window(hwnd);
            }
        }
    } else if state.selected_tab == 9 {
        // Tab: Convert — format segmented (y: 132..162)
        if y >= 132.0 && y <= 162.0 && x >= 250.0 && x <= 682.0 {
            if let Some(idx) = segmented_hit(x, 250.0, 432.0, 3) {
                state.settings.convert_format_idx = idx;
                state.settings.save();
                notify_main();
                render_window(hwnd);
            }
        }
    } else if state.selected_tab == 10 {
        // Tab: HUD & popups
        if y >= 84.0 && y <= 132.0 && x >= 236.0 && x <= 696.0 {
            state.settings.pill_alerts_enabled = !state.settings.pill_alerts_enabled;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 136.0 && y <= 180.0 && x >= 236.0 && x <= 696.0 {
            state.settings.alert_battery = !state.settings.alert_battery;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 184.0 && y <= 232.0 && x >= 236.0 && x <= 696.0 {
            state.settings.alert_capslock = !state.settings.alert_capslock;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 262.0 && y <= 308.0 && x >= 236.0 && x <= 696.0 {
            state.settings.dodge_enabled = !state.settings.dodge_enabled;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        } else if y >= 460.0 && y <= 514.0 && x >= 236.0 && x <= 696.0 {
            state.settings.hide_when_covered = !state.settings.hide_when_covered;
            state.settings.save();
            notify_main();
            render_window(hwnd);
        }
    }
}

fn render_window(hwnd: HWND) {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State };
    if ptr.is_null() {
        return;
    }
    let s = unsafe { &mut *ptr };
    ensure_tab_icons(s);
    let Some(target) = s.target.as_ref() else { return };

    unsafe {
        target.BeginDraw();
        target.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));

        // Window capsule background with 18.0 rounded corners
        let _win_rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: WIN_W,
                bottom: WIN_H,
            },
            radiusX: 18.0,
            radiusY: 18.0,
        };

        // Clip to rounded window shape
        let path = s.factory.CreatePathGeometry().unwrap();
        let sink = path.Open().unwrap();
        sink.BeginFigure(D2D_POINT_2F { x: 18.0, y: 0.0 }, D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(D2D_POINT_2F { x: WIN_W - 18.0, y: 0.0 });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: D2D_POINT_2F { x: WIN_W, y: 18.0 },
            size: D2D_SIZE_F { width: 18.0, height: 18.0 },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(D2D_POINT_2F { x: WIN_W, y: WIN_H - 18.0 });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: D2D_POINT_2F { x: WIN_W - 18.0, y: WIN_H },
            size: D2D_SIZE_F { width: 18.0, height: 18.0 },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(D2D_POINT_2F { x: 18.0, y: WIN_H });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: D2D_POINT_2F { x: 0.0, y: WIN_H - 18.0 },
            size: D2D_SIZE_F { width: 18.0, height: 18.0 },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(D2D_POINT_2F { x: 0.0, y: 18.0 });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: D2D_POINT_2F { x: 18.0, y: 0.0 },
            size: D2D_SIZE_F { width: 18.0, height: 18.0 },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        let _ = sink.Close();

        let layer_params = D2D1_LAYER_PARAMETERS {
            contentBounds: D2D_RECT_F { left: 0.0, top: 0.0, right: WIN_W, bottom: WIN_H },
            geometricMask: ManuallyDrop::new(Some(path.cast::<ID2D1Geometry>().unwrap())),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        target.PushLayer(&layer_params, None);

        // 1. Sidebar Background (#18181A)
        fill_rect(target, 0.0, 0.0, SIDEBAR_W, WIN_H, color(0.094, 0.094, 0.102, 0.98));

        // 2. Content Area Background (#1E1E22)
        fill_rect(target, SIDEBAR_W, 0.0, WIN_W - SIDEBAR_W, WIN_H, color(0.118, 0.118, 0.128, 0.98));

        // 3. Subtle vertical separator line between sidebar and content
        fill_rect(target, SIDEBAR_W, 0.0, 1.0, WIN_H, color(1.0, 1.0, 1.0, 0.06));

        // --- Left Sidebar Content ---

        // Traffic light: Red close circle (macOS #FF5F57)
        let red_c = if s.hovered_close { color(1.0, 0.25, 0.25, 1.0) } else { color(1.0, 0.37, 0.34, 1.0) };
        fill_oval(target, 15.0, 15.0, 12.0, 12.0, red_c);
        stroke_oval(target, 15.0, 15.0, 12.0, 12.0, 0.75, color(0.0, 0.0, 0.0, 0.25));
        if s.hovered_close {
            draw_close_cross(target, 21.0, 21.0, 3.5, color(0.3, 0.0, 0.0, 0.9));
        }

        // Sidebar Menu Items (PNG icons from assets/):
        let tabs = [
            "General",
            "Display",
            "Appearance",
            "Music and lyrics",
            "To-do",
            "Pomodoro",
            "Water",
            "Shelf",
            "Remove BG",
            "Convert",
            "HUD & popups",
        ];

        let mut item_y = 48.0f32;
        for (i, title) in tabs.iter().enumerate() {
            let is_sel = s.selected_tab == i;
            let is_hov = s.hovered_tab == Some(i);

            let bg_a = if is_sel { 0.14 } else if is_hov { 0.06 } else { 0.0 };
            if bg_a > 0.0 {
                fill_rounded(target, 10.0, item_y, 190.0, 30.0, 8.0, color(1.0, 1.0, 1.0, bg_a));
            }

            // PNG icon, 22x22
            if let Some(bmp) = s.tab_icons[i].as_ref() {
                let r = D2D_RECT_F { left: 16.0, top: item_y + 4.0, right: 38.0, bottom: item_y + 26.0 };
                target.DrawBitmap(bmp, Some(&r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }

            // Label
            let tc = if is_sel { color(1.0, 1.0, 1.0, 1.0) } else { color(1.0, 1.0, 1.0, 0.80) };
            if let Some(fmt) = s.fmt_sidebar.as_ref() {
                draw_text(target, title, fmt, 46.0, item_y + 5.0, tc);
            }

            item_y += 34.0;
        }

        // Bottom item: About (index 11)
        let about_sel = s.selected_tab == 11;
        let about_hov = s.hovered_tab == Some(11);
        let about_bg = if about_sel { 0.14 } else if about_hov { 0.06 } else { 0.0 };
        if about_bg > 0.0 {
            fill_rounded(target, 10.0, 492.0, 190.0, 30.0, 8.0, color(1.0, 1.0, 1.0, about_bg));
        }
        if let Some(bmp) = s.tab_icons[11].as_ref() {
            let r = D2D_RECT_F { left: 16.0, top: 496.0, right: 38.0, bottom: 518.0 };
            target.DrawBitmap(bmp, Some(&r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
        }
        if let Some(fmt) = s.fmt_sidebar.as_ref() {
            let tc = if about_sel { color(1.0, 1.0, 1.0, 1.0) } else { color(1.0, 1.0, 1.0, 0.80) };
            draw_text(target, "About", fmt, 46.0, 497.0, tc);
        }

        // --- Right Content Area ---
        match s.selected_tab {
            0 => draw_general_page(target, s),
            1 => draw_display_page(target, s),
            2 => draw_appearance_page(target, s),
            3 => draw_music_page(target, s),
            4 => draw_todo_page(target, s),
            5 => draw_pomodoro_page(target, s),
            6 => draw_water_page(target, s),
            7 => draw_shelf_page(target, s),
            8 => draw_removebg_page(target, s),
            9 => draw_convert_page(target, s),
            10 => draw_hud_page(target, s),
            _ => draw_about_page(target, s),
        }

        target.PopLayer();

        // Outer window stroke border
        stroke_rounded(target, 0.5, 0.5, WIN_W - 1.0, WIN_H - 1.0, 18.0, 1.0, color(1.0, 1.0, 1.0, 0.14));

        let _ = target.EndDraw(None, None);

        // Present via UpdateLayeredWindow
        let size = SIZE { cx: WIN_W as i32, cy: WIN_H as i32 };
        let pt_src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&size),
            s.mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

fn draw_general_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    // Header Title
    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "General", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    // 1. Startup
    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Startup", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 42.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Start with Windows", fmt, cx0 + 16.0, 94.0, color(1.0, 1.0, 1.0, 0.95));
    }
    draw_toggle(target, cx1 - 56.0, 93.0, s.settings.start_with_windows);

    // 2. Notch
    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Notch", fmt, cx0, 138.0, color(1.0, 1.0, 1.0, 0.45));
    }
    let notch_h = 156.0;
    fill_rounded(target, cx0, 156.0, card_w, notch_h, 10.0, color(0.16, 0.16, 0.18, 0.95));

    // Sub-item: Size (live slider)
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Size", fmt, cx0 + 16.0, 166.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Drag to resize the pill live", fmt, cx0 + 16.0, 184.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_slider(target, s, SliderKind::Size);

    // Divider
    fill_rect(target, cx0 + 14.0, 244.0, card_w - 28.0, 1.0, color(1.0, 1.0, 1.0, 0.05));

    // Sub-item: Minimal notch
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Minimal notch", fmt, cx0 + 16.0, 252.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Hide the pill, leaving only its tip when idle", fmt, cx0 + 16.0, 270.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 257.0, s.settings.minimal_notch);

    // Divider
    fill_rect(target, cx0 + 14.0, 290.0, card_w - 28.0, 1.0, color(1.0, 1.0, 1.0, 0.05));

    // Sub-item: Aura mascot
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Aura mascot", fmt, cx0 + 16.0, 298.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "A little friend that lives in the notch", fmt, cx0 + 16.0, 316.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 303.0, s.settings.eye_expressions);

    // 3. Sounds
    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Sounds", fmt, cx0, 344.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 362.0, card_w, 54.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Reminder sounds", fmt, cx0 + 16.0, 370.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Chimes for Pomodoro, To-do and Water", fmt, cx0 + 16.0, 390.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 375.0, s.settings.sound_chimes);

    // 4. Full screen
    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Full screen", fmt, cx0, 428.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 446.0, card_w, 54.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Hide in full screen", fmt, cx0 + 16.0, 454.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Games, videos and slides that fill the screen", fmt, cx0 + 16.0, 474.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 459.0, s.settings.hide_in_fullscreen);
}

fn draw_music_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Music and lyrics", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Media Controls", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 48.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Enable Music card", fmt, cx0 + 16.0, 92.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Show active song from Spotify, browser, Apple Music", fmt, cx0 + 16.0, 110.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 94.0, s.settings.music_enabled);

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Lyrics & Volume", fmt, cx0, 142.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 160.0, card_w, 98.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Synchronized lyrics", fmt, cx0 + 16.0, 168.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Auto-fetch live synced lyrics from LRCLIB", fmt, cx0 + 16.0, 186.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 172.0, s.settings.lyrics_sync);

    fill_rect(target, cx0 + 14.0, 208.0, card_w - 28.0, 1.0, color(1.0, 1.0, 1.0, 0.05));

    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Master volume control", fmt, cx0 + 16.0, 216.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Hover volume slider next to speaker icon", fmt, cx0 + 16.0, 234.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 220.0, s.settings.master_volume_control);
}

fn draw_todo_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "To-do", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Task Management", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 48.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Enable To-do card", fmt, cx0 + 16.0, 92.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Plan and check daily tasks directly from the notch", fmt, cx0 + 16.0, 110.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 94.0, s.settings.todo_enabled);

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Feedback", fmt, cx0, 142.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 160.0, card_w, 48.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Sound on task completion", fmt, cx0 + 16.0, 168.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Play rewarding sound when marking tasks done", fmt, cx0 + 16.0, 186.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 170.0, s.settings.sound_on_complete);
}

fn draw_pomodoro_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Pomodoro", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Focus Timer", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 48.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Enable Pomodoro card", fmt, cx0 + 16.0, 92.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "25-min focus sessions with 5-min short breaks", fmt, cx0 + 16.0, 110.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 94.0, s.settings.pomodoro_enabled);

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Duration", fmt, cx0, 142.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 160.0, card_w, 100.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Focus duration", fmt, cx0 + 16.0, 170.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Length of one focus session", fmt, cx0 + 16.0, 188.0, color(1.0, 1.0, 1.0, 0.45));
    }
    let sel = [15u32, 25, 45, 60].iter().position(|&v| v == s.settings.pomodoro_focus_mins).unwrap_or(1);
    draw_segmented(target, s, cx0 + 14.0, 208.0, card_w - 28.0, &["15 min", "25 min", "45 min", "60 min"], sel);
}

fn draw_water_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Water", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Hydration Reminder", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 48.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Enable Water card", fmt, cx0 + 16.0, 92.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Track daily intake and receive drink notifications", fmt, cx0 + 16.0, 110.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 94.0, s.settings.water_enabled);

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Goal", fmt, cx0, 142.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 160.0, card_w, 100.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Daily goal", fmt, cx0 + 16.0, 170.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Target water intake per day", fmt, cx0 + 16.0, 188.0, color(1.0, 1.0, 1.0, 0.45));
    }
    let sel = [1500u32, 2000, 2500, 3000].iter().position(|&v| v == s.settings.water_daily_goal).unwrap_or(1);
    draw_segmented(target, s, cx0 + 14.0, 208.0, card_w - 28.0, &["1.5 L", "2 L", "2.5 L", "3 L"], sel);
}

fn draw_shelf_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Shelf", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "File Staging Area", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 48.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Enable Shelf tray", fmt, cx0 + 16.0, 92.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Drag files here to stage or transfer to Remove BG / Convert", fmt, cx0 + 16.0, 110.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 94.0, s.settings.tray_enabled);
}

fn segmented_hit(x: f32, seg_x: f32, seg_w: f32, n: usize) -> Option<usize> {
    if x < seg_x || x > seg_x + seg_w || n == 0 {
        return None;
    }
    let idx = ((x - seg_x) / (seg_w / n as f32)) as usize;
    if idx < n { Some(idx) } else { None }
}

fn draw_segmented(target: &ID2D1RenderTarget, s: &State, x: f32, y: f32, w: f32, labels: &[&str], selected: usize) {
    let h = 30.0;
    fill_rounded(target, x, y, w, h, 6.0, color(0.11, 0.11, 0.12, 1.0));
    let seg_w = w / labels.len() as f32;
    for (idx, &label) in labels.iter().enumerate() {
        let px = x + idx as f32 * seg_w;
        if idx == selected {
            fill_rounded(target, px + 2.0, y + 2.0, seg_w - 4.0, h - 4.0, 5.0, color(0.24, 0.24, 0.27, 1.0));
        }
        if let Some(fmt) = s.fmt_seg.as_ref() {
            let tc = if idx == selected { color(1.0, 1.0, 1.0, 1.0) } else { color(1.0, 1.0, 1.0, 0.60) };
            draw_centered_text(target, label, fmt, px + seg_w * 0.5, y + 6.0, tc);
        }
    }
}

fn draw_display_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Display", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Monitors", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 54.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Show on all monitors", fmt, cx0 + 16.0, 92.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Mirror the pill on every display", fmt, cx0 + 16.0, 110.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 98.0, s.settings.pill_show_all_monitors);

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Layout", fmt, cx0, 150.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_monitor_map(target, s);
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        let hint = if s.settings.pill_show_all_monitors {
            "The pill is mirrored on every display"
        } else {
            "Click a display to move the pill there"
        };
        draw_text(target, hint, fmt, cx0, 384.0, color(1.0, 1.0, 1.0, 0.35));
    }
}

/// Map-area rects (x, y, w, h) for each monitor — shared by draw + hit-test.
fn monitor_map_rects() -> Vec<(f32, f32, f32, f32)> {
    let (mx, my, mw, mh) = (236.0, 168.0, 460.0, 200.0);
    let mons = crate::window::list_monitors();
    if mons.is_empty() {
        return Vec::new();
    }
    let mut l = i32::MAX;
    let mut t = i32::MAX;
    let mut r = i32::MIN;
    let mut b = i32::MIN;
    for m in &mons {
        l = l.min(m.work.left);
        t = t.min(m.work.top);
        r = r.max(m.work.right);
        b = b.max(m.work.bottom);
    }
    let bw = (r - l).max(1) as f32;
    let bh = (b - t).max(1) as f32;
    let scale = (mw / bw).min(mh / bh);
    let ox = mx + (mw - bw * scale) / 2.0;
    let oy = my + (mh - bh * scale) / 2.0;
    mons.iter()
        .map(|m| {
            (
                ox + (m.work.left - l) as f32 * scale,
                oy + (m.work.top - t) as f32 * scale,
                (m.work.right - m.work.left) as f32 * scale,
                (m.work.bottom - m.work.top) as f32 * scale,
            )
        })
        .collect()
}

fn draw_monitor_map(target: &ID2D1RenderTarget, s: &State) {
    let mons = crate::window::list_monitors();
    let rects = monitor_map_rects();
    if mons.is_empty() {
        if let Some(fmt) = s.fmt_row_sub.as_ref() {
            draw_text(target, "No displays detected", fmt, 236.0, 220.0, color(1.0, 1.0, 1.0, 0.45));
        }
        return;
    }
    for (i, m) in mons.iter().enumerate() {
        let (rx, ry, rw, rh) = rects[i];
        let sel = !s.settings.pill_show_all_monitors && s.settings.pill_monitor as usize == i;
        fill_rounded(target, rx, ry, rw, rh, 6.0, color(0.13, 0.13, 0.15, 1.0));
        let sc = if sel {
            color(0.30, 0.60, 1.0, 1.0)
        } else {
            color(1.0, 1.0, 1.0, 0.18)
        };
        stroke_rounded(target, rx, ry, rw, rh, 6.0, 2.0, sc);
        // pill marker at top-center
        let ma = if s.settings.pill_show_all_monitors || sel { 0.95 } else { 0.25 };
        fill_rounded(target, rx + rw / 2.0 - 14.0, ry + 4.0, 28.0, 7.0, 3.5, color(1.0, 1.0, 1.0, ma));
        if let Some(fmt) = s.fmt_row_sub.as_ref() {
            let res = format!("{}x{}", m.work.right - m.work.left, m.work.bottom - m.work.top);
            draw_centered_text(target, &res, fmt, rx + rw / 2.0, ry + rh / 2.0 - 7.0, color(1.0, 1.0, 1.0, 0.60));
            let name = if m.primary { "Primary".to_string() } else { m.device.clone() };
            draw_centered_text(target, &name, fmt, rx + rw / 2.0, ry + rh / 2.0 + 7.0, color(1.0, 1.0, 1.0, 0.40));
        }
    }
}

fn draw_appearance_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Appearance", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Pill", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 100.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Pill opacity", fmt, cx0 + 16.0, 94.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Transparency of the floating pill", fmt, cx0 + 16.0, 112.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_slider(target, s, SliderKind::Opacity);
}

fn draw_removebg_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Remove BG", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Model", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 100.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Model", fmt, cx0 + 16.0, 94.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Fast is instant, Quality is cleaner but slower", fmt, cx0 + 16.0, 112.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_segmented(target, s, cx0 + 14.0, 132.0, card_w - 28.0, &["Fast", "Balanced", "Quality"], s.settings.removebg_model_idx.min(2));
}

fn draw_convert_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "Convert", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Photo", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 100.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Default format", fmt, cx0 + 16.0, 94.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Used when converting photos", fmt, cx0 + 16.0, 112.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_segmented(target, s, cx0 + 14.0, 132.0, card_w - 28.0, &["AVIF", "WebP", "PNG"], s.settings.convert_format_idx.min(2));
}

fn draw_hud_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "HUD & popups", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Pill Alerts", fmt, cx0, 66.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 84.0, card_w, 148.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Pill alerts", fmt, cx0 + 16.0, 94.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Battery, charging and Caps Lock popups", fmt, cx0 + 16.0, 112.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 98.0, s.settings.pill_alerts_enabled);

    fill_rect(target, cx0 + 14.0, 134.0, card_w - 28.0, 1.0, color(1.0, 1.0, 1.0, 0.05));

    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Battery alerts", fmt, cx0 + 16.0, 142.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Low battery and charging notifications", fmt, cx0 + 16.0, 160.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 146.0, s.settings.alert_battery);

    fill_rect(target, cx0 + 14.0, 182.0, card_w - 28.0, 1.0, color(1.0, 1.0, 1.0, 0.05));

    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Caps Lock alerts", fmt, cx0 + 16.0, 190.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Show a popup when Caps Lock toggles", fmt, cx0 + 16.0, 208.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 194.0, s.settings.alert_capslock);

    // --- Dodge ---
    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Dodge", fmt, cx0, 244.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 262.0, card_w, 168.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Dodge", fmt, cx0 + 16.0, 272.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Hide the pill when the cursor comes close", fmt, cx0 + 16.0, 290.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 276.0, s.settings.dodge_enabled);

    fill_rect(target, cx0 + 14.0, 308.0, card_w - 28.0, 1.0, color(1.0, 1.0, 1.0, 0.05));

    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Radius", fmt, cx0 + 16.0, 316.0, color(1.0, 1.0, 1.0, 0.95));
    }
    draw_slider(target, s, SliderKind::DodgeRadius);

    fill_rect(target, cx0 + 14.0, 368.0, card_w - 28.0, 1.0, color(1.0, 1.0, 1.0, 0.05));

    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Delay", fmt, cx0 + 16.0, 376.0, color(1.0, 1.0, 1.0, 0.95));
    }
    draw_slider(target, s, SliderKind::DodgeDelay);

    // --- Overlap ---
    if let Some(fmt) = s.fmt_sec_hdr.as_ref() {
        draw_text(target, "Overlap", fmt, cx0, 442.0, color(1.0, 1.0, 1.0, 0.45));
    }
    fill_rounded(target, cx0, 460.0, card_w, 54.0, 10.0, color(0.16, 0.16, 0.18, 0.95));
    if let Some(fmt) = s.fmt_row_title.as_ref() {
        draw_text(target, "Hide when covered", fmt, cx0 + 16.0, 470.0, color(1.0, 1.0, 1.0, 0.95));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_text(target, "Auto-hide when a window is behind the pill", fmt, cx0 + 16.0, 488.0, color(1.0, 1.0, 1.0, 0.45));
    }
    draw_toggle(target, cx1 - 56.0, 474.0, s.settings.hide_when_covered);
}

fn draw_about_page(target: &ID2D1RenderTarget, s: &State) {
    let (cx0, cx1) = (236.0, 696.0);
    let card_w = cx1 - cx0;

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_text(target, "About Aura", fmt, cx0, 26.0, color(1.0, 1.0, 1.0, 1.0));
    }

    fill_rounded(target, cx0, 84.0, card_w, 200.0, 12.0, color(0.16, 0.16, 0.18, 0.95));

    // Draw little flame mascot inside About
    fill_oval(target, cx0 + card_w * 0.5 - 28.0, 112.0, 56.0, 56.0, color(0.95, 0.25, 0.25, 0.95));
    // Eyes:
    fill_oval(target, cx0 + card_w * 0.5 - 12.0, 134.0, 7.0, 12.0, color(1.0, 1.0, 1.0, 1.0));
    fill_oval(target, cx0 + card_w * 0.5 + 5.0, 134.0, 7.0, 12.0, color(1.0, 1.0, 1.0, 1.0));

    if let Some(fmt) = s.fmt_title.as_ref() {
        draw_centered_text(target, "Aura for Windows", fmt, cx0 + card_w * 0.5, 186.0, color(1.0, 1.0, 1.0, 1.0));
    }
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        draw_centered_text(target, "Version 1.0.0 (Release x86_64)", fmt, cx0 + card_w * 0.5, 218.0, color(1.0, 1.0, 1.0, 0.65));
        draw_centered_text(target, "Native Rust + Direct2D Dynamic Island", fmt, cx0 + card_w * 0.5, 238.0, color(1.0, 1.0, 1.0, 0.45));
    }
}

fn draw_slider(target: &ID2D1RenderTarget, s: &State, kind: SliderKind) {
    let (x, cy, w) = slider_geom(kind);
    let (min, max) = slider_min_max(kind);
    let v = slider_value(s, kind).clamp(min, max);
    let t = (v - min) / (max - min);
    let kx = x + t * w;
    let active = s.dragging_slider == Some(kind);
    let hover = s.slider_hover == Some(kind) || active;

    // track
    fill_rounded(target, x, cy - 3.0, w, 6.0, 3.0, color(0.10, 0.10, 0.11, 1.0));
    // filled portion
    if kx > x + 2.0 {
        fill_rounded(target, x, cy - 3.0, kx - x, 6.0, 3.0, color(0.30, 0.60, 1.0, 0.90));
    }
    // default tick mark
    if let Some(tick_v) = slider_tick(kind) {
        let tx = x + (tick_v - min) / (max - min) * w;
        fill_rect(target, tx - 1.0, cy - 9.0, 2.0, 18.0, color(1.0, 1.0, 1.0, 0.45));
    }
    // knob
    let r = if active { 12.0 } else if hover { 11.0 } else { 10.0 };
    if hover {
        fill_oval(target, kx - r - 3.0, cy - r - 3.0, (r + 3.0) * 2.0, (r + 3.0) * 2.0, color(0.30, 0.60, 1.0, 0.25));
    }
    fill_oval(target, kx - r, cy - r, r * 2.0, r * 2.0, color(1.0, 1.0, 1.0, 1.0));
    // value label, top-right
    if let Some(fmt) = s.fmt_row_sub.as_ref() {
        let label = slider_format(kind, v);
        let tw = label.len() as f32 * 6.5;
        draw_text(target, &label, fmt, x + w - tw, cy - 32.0, color(1.0, 1.0, 1.0, 0.65));
    }
}

fn decode_png_bytes(
    target: &ID2D1RenderTarget,
    bytes: &[u8],
    max_size: u32,
) -> Option<ID2D1Bitmap> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder =
            factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let src: windows::Win32::Graphics::Imaging::IWICBitmapSource = frame.cast().ok()?;
        let mut w = 0u32;
        let mut h = 0u32;
        src.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let scale = ((max_size as f32) / (w as f32))
            .min((max_size as f32) / (h as f32))
            .min(1.0);
        let tw = ((w as f32) * scale).round().max(1.0) as u32;
        let th = ((h as f32) * scale).round().max(1.0) as u32;
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler.Initialize(&src, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let converter = factory.CreateFormatConverter().ok()?;
        let scaler_src: windows::Win32::Graphics::Imaging::IWICBitmapSource = scaler.cast().ok()?;
        converter
            .Initialize(
                &scaler_src,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;
        let conv_src: windows::Win32::Graphics::Imaging::IWICBitmapSource =
            converter.cast().ok()?;
        let mut pixels = vec![0u8; (tw * th * 4) as usize];
        conv_src.CopyPixels(std::ptr::null(), tw * 4, &mut pixels).ok()?;
        target
            .CreateBitmap(
                D2D_SIZE_U { width: tw, height: th },
                Some(pixels.as_ptr() as *const core::ffi::c_void),
                tw * 4,
                &D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                },
            )
            .ok()
    }
}

const TAB_ICON_BYTES: [&[u8]; 12] = [
    include_bytes!("../assets/general_icon.png"),
    include_bytes!("../assets/display_icon.png"),
    include_bytes!("../assets/appearance_icon.png"),
    include_bytes!("../assets/music_icon.png"),
    include_bytes!("../assets/todo_icon.png"),
    include_bytes!("../assets/focus_icon.png"),
    include_bytes!("../assets/water_icon.png"),
    include_bytes!("../assets/shelf_icon.png"),
    include_bytes!("../assets/removebg_icon.png"),
    include_bytes!("../assets/convert_icon.png"),
    include_bytes!("../assets/hud_icon.png"),
    include_bytes!("../assets/about_icon.png"),
];

fn ensure_tab_icons(s: &mut State) {
    let Some(target) = s.target.as_ref() else { return };
    for i in 0..12 {
        if s.tab_icons[i].is_none() {
            s.tab_icons[i] = decode_png_bytes(target, TAB_ICON_BYTES[i], 64);
        }
    }
}

fn draw_toggle(target: &ID2D1RenderTarget, x: f32, y: f32, on: bool) {
    let (w, h) = (42.0, 24.0);
    let bg_c = if on {
        color(0.20, 0.78, 0.35, 1.0) // Apple green #34C759
    } else {
        color(0.24, 0.24, 0.26, 1.0) // Inactive dark gray
    };
    fill_rounded(target, x, y, w, h, 12.0, bg_c);

    let k_r = 9.5;
    let k_x = if on { x + w - k_r - 2.5 } else { x + k_r + 2.5 };
    let k_y = y + h * 0.5;

    // Knob with soft shadow
    fill_oval(target, k_x - k_r, k_y - k_r + 0.5, k_r * 2.0, k_r * 2.0, color(0.0, 0.0, 0.0, 0.20));
    fill_oval(target, k_x - k_r, k_y - k_r, k_r * 2.0, k_r * 2.0, color(1.0, 1.0, 1.0, 1.0));
}

fn fill_rect(target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, c: D2D1_COLOR_F) {
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            target.FillRectangle(&D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }, &b);
        }
    }
}

fn fill_rounded(target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, r: f32, c: D2D1_COLOR_F) {
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h },
                radiusX: r,
                radiusY: r,
            };
            target.FillRoundedRectangle(&rr, &b);
        }
    }
}

fn stroke_rounded(target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, r: f32, stroke_w: f32, c: D2D1_COLOR_F) {
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h },
                radiusX: r,
                radiusY: r,
            };
            target.DrawRoundedRectangle(&rr, &b, stroke_w, None);
        }
    }
}

fn fill_oval(target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, c: D2D1_COLOR_F) {
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            let el = D2D1_ELLIPSE {
                point: D2D_POINT_2F { x: x + w * 0.5, y: y + h * 0.5 },
                radiusX: w * 0.5,
                radiusY: h * 0.5,
            };
            target.FillEllipse(&el, &b);
        }
    }
}

fn stroke_oval(target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, stroke_w: f32, c: D2D1_COLOR_F) {
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            let el = D2D1_ELLIPSE {
                point: D2D_POINT_2F { x: x + w * 0.5, y: y + h * 0.5 },
                radiusX: w * 0.5,
                radiusY: h * 0.5,
            };
            target.DrawEllipse(&el, &b, stroke_w, None);
        }
    }
}

fn draw_close_cross(target: &ID2D1RenderTarget, cx: f32, cy: f32, r: f32, c: D2D1_COLOR_F) {
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            target.DrawLine(
                D2D_POINT_2F { x: cx - r, y: cy - r },
                D2D_POINT_2F { x: cx + r, y: cy + r },
                &b,
                1.2,
                None,
            );
            target.DrawLine(
                D2D_POINT_2F { x: cx + r, y: cy - r },
                D2D_POINT_2F { x: cx - r, y: cy + r },
                &b,
                1.2,
                None,
            );
        }
    }
}

fn draw_text(target: &ID2D1RenderTarget, text: &str, fmt: &IDWriteTextFormat, x: f32, y: f32, c: D2D1_COLOR_F) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() { return; }
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            let layout = D2D_RECT_F { left: x, top: y, right: x + 600.0, bottom: y + 40.0 };
            target.DrawText(
                &wide,
                fmt,
                &layout,
                &b,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}

fn draw_centered_text(target: &ID2D1RenderTarget, text: &str, fmt: &IDWriteTextFormat, cx: f32, y: f32, c: D2D1_COLOR_F) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() { return; }
    unsafe {
        if let Ok(b) = target.CreateSolidColorBrush(&c, None) {
            let half = 150.0;
            let layout = D2D_RECT_F { left: cx - half, top: y, right: cx + half, bottom: y + 40.0 };
            target.DrawText(
                &wide,
                fmt,
                &layout,
                &b,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}

