//! Direct2D + DirectWrite renderer matching `aura-island` (`IslandView.swift`) 1:1:
//! - Notch pill/card geometry (`tuck = 12pt`, `bottomRadius = 28pt` for cards, `lh / 2` for pills)
//!   with `0.98` black fill, `0.15` white border (`0.5pt`), and darkened dominant album-art
//!   vertical gradient on the Lyrics page and Lyrics-mode closed pill.
//! - `Zen` (`140x32`): two white eyes (`13x16` at `x = 50, 77`) with cursor-tracking black pupils
//!   (`6x6`) and periodic blinking (`13x2`).
//! - `Compact` (`180x32` music / `160x32` active Pomodoro / `360x32` lyrics pill or system alert):
//!   - Alert mode (`360x32`): icon + message + tinted detail text / level bar.
//!   - Pomodoro active (`160x32`): progress ring (`r = 8`) + countdown.
//!   - Lyrics mode (`360x32`): musical note icon + current lyric line + album-art gradient.
//!   - Music mode (`180x32`): iOS battery icon (left) + `HH:mm` clock (center) + 5-bar waveform (right).
//! - `Expanded` (`520x180`): card header (green dot + `"EEE d MMM yyyy HH:mm"` + waveform),
//!   `56x56` rounded (`12pt`) cover art, `25pt` bold title, `16pt` artist, progress bar with
//!   `MM:SS` / `-MM:SS`, speech bubble lyrics toggle + transport controls, right-edge vertical dots.
//! - `Lyrics` (`700x180`): compressed music card on the left (`0..340`) + vertical divider (`x = 340`)
//!   + 3-line wrapped lyrics panel on the right (`352..660`) with 60fps slot-machine roll animation.
//! - `Pomodoro` (`640x180`): card header, `"Pomodoro"` title, `r = 42` progress ring + stopwatch icon,
//!   `44pt` bold countdown, circular reset button, mode pills (`Short Break`, `Play/Pause`, `Focus Time`).
//! - `Todo` (`640x220`): card header, `"Todo"` title, `{done}/{total}` counter, `"+ Add task"` pill,
//!   inline input bar when editing, right-column rounded task pills (`x = 280..600`) with circular
//!   checkmark, strikethrough done text, and `"×"` delete button.
//! - `Water` (`640x180`): `"Water Reminder"` header with status ring + date/time + waveform,
//!   `44pt` bold liter count `/ 1.5 L`, notification toggle switch with bell knob, `"Today · X%"`,
//!   gradient progress bar, `"Drink"` & `"Settings"` pills, and 24-hour intake bar chart.
//! - `Tray` (`640x280`): card header, `"Tray"` title + `"Clear"` pill, `4x2` grid of `136x100` tiles
//!   with folder/document vector icons, filename, file size, `"×"` delete button, pagination arrows/dots,
//!   and dashed empty drop zone.

use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::time::Instant;

use windows::core::{w, Interface, Result};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, ValidateRect,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use crate::state::IslandState;
use crate::window;

// --- Interactive hit-test constants (exact `IslandView.swift` coordinates) ---

/// Speech bubble button on Music (`520x180`) and Lyrics (`700x180`) cards: `NSRect(x: 42, y: 126, width: 28, height: 28)`.
pub const MUSIC_BUBBLE_RECT: (f32, f32, f32, f32) = (42.0, 126.0, 70.0, 154.0);
/// Transport control X centers on Music card (`[208, 260, 312]`, `y: 126..154`, `half_w = 20`).
pub const MUSIC_CONTROL_CENTERS: [f32; 3] = [208.0, 260.0, 312.0];
/// Transport control X centers on Lyrics card (`[118, 170, 222]`, `y: 126..154`, `half_w = 20`).
pub const LYRICS_CONTROL_CENTERS: [f32; 3] = [118.0, 170.0, 222.0];
pub const MUSIC_CONTROLS_Y: (f32, f32) = (126.0, 154.0);

/// Pomodoro (`640x180`) interactive rects (`pomodoroButtonRects`).
pub const POMODORO_SHORT_BREAK_RECT: (f32, f32, f32, f32) = (220.0, 130.0, 344.0, 158.0);
pub const POMODORO_PLAY_RECT: (f32, f32, f32, f32) = (352.0, 130.0, 408.0, 158.0);
pub const POMODORO_FOCUS_RECT: (f32, f32, f32, f32) = (416.0, 130.0, 538.0, 158.0);
pub const POMODORO_RESET_RECT: (f32, f32, f32, f32) = (362.0, 58.0, 394.0, 90.0);

/// Todo (`640x220`) interactive rects (`todoAddPillRect`, `todoInputRect`, `todoVisibleRows`).
pub const TODO_ADD_PILL_RECT: (f32, f32, f32, f32) = (28.0, 120.0, 132.0, 148.0);
pub const TODO_INPUT_RECT: (f32, f32, f32, f32) = (28.0, 160.0, 612.0, 192.0);
pub const TODO_ROW_X: f32 = 280.0;
pub const TODO_ROW_W: f32 = 320.0;
pub const TODO_ROW_TOGGLE_RIGHT: f32 = 568.0;
pub const TODO_ROW_DELETE_LEFT: f32 = 576.0;
pub const TODO_ROW_DELETE_RIGHT: f32 = 600.0;
pub const TODO_ROW_Y0: f32 = 52.0;
pub const TODO_ROW_H: f32 = 32.0;
pub const TODO_ROW_PITCH: f32 = 40.0;

/// Water (`640x180`) interactive rects (`waterButtonRects`).
pub const WATER_TOGGLE_RECT: (f32, f32, f32, f32) = (284.0, 54.0, 340.0, 86.0);
pub const WATER_DRINK_RECT: (f32, f32, f32, f32) = (48.0, 132.0, 132.0, 160.0);
pub const WATER_SETTINGS_RECT: (f32, f32, f32, f32) = (140.0, 132.0, 242.0, 160.0);

/// Tray (`640x280`) interactive rects (`trayClearPillRect`, `trayGridTileRects`, `trayGridArrowRects`).
pub const TRAY_PAGE_SIZE: usize = 8;
pub const TRAY_CLEAR_RECT: (f32, f32, f32, f32) = (82.0, 10.0, 142.0, 38.0);
pub const TRAY_TILE_GEOM: (f32, f32, f32, f32, f32, f32) =
    (30.0, 44.0, 136.0, 100.0, 148.0, 112.0);
pub const TRAY_LEFT_ARROW_RECT: (f32, f32, f32, f32) = (26.0, 128.0, 70.0, 172.0);
pub const TRAY_RIGHT_ARROW_RECT: (f32, f32, f32, f32) = (570.0, 128.0, 614.0, 172.0);

/// Hit test for the vertical section dots on the right edge (`IslandView.dotIndex(at:)`).
pub fn dot_index_at(x: f32, y: f32, lw: f32, lh: f32) -> Option<usize> {
    let count = 5usize;
    if x < lw - 32.0 || x > lw - 10.0 {
        return None;
    }
    let top = (lh - (count as f32) * 18.0) / 2.0;
    if y < top || y > top + (count as f32) * 18.0 {
        return None;
    }
    let idx = ((y - top) / 18.0) as usize;
    if idx < count {
        Some(idx)
    } else {
        None
    }
}

/// Hit test for the 3 transport controls (`0 = prev`, `1 = play/pause`, `2 = next`).
pub fn control_hit_at(x: f32, y: f32, centers: &[f32; 3]) -> Option<usize> {
    if y < MUSIC_CONTROLS_Y.0 || y > MUSIC_CONTROLS_Y.1 {
        return None;
    }
    for (i, &cx) in centers.iter().enumerate() {
        if x >= cx - 20.0 && x <= cx + 20.0 {
            return Some(i);
        }
    }
    None
}

/// Alert icon kind for `PillAlert` (`360x32` closed alert pill).
#[derive(Clone, Debug)]
pub enum AlertIcon {
    Battery { percent: f32, charging: bool },
    CapsLock { on: bool },
    Stopwatch,
    Checkmark,
}

/// Live pill alert matching `IslandView.PillAlert`.
#[derive(Clone, Debug)]
pub struct PillAlert {
    pub icon: AlertIcon,
    pub message: String,
    pub detail_text: Option<String>,
    pub detail_color: Option<(f32, f32, f32)>,
}

/// Visual state passed into `Renderer::draw`.
pub struct RenderContext<'a> {
    pub state: IslandState,
    pub lyrics_mode: bool,
    pub eye_look: (f32, f32),
    pub eyes_closed: bool,
    pub battery_percent: Option<f32>,
    pub battery_charging: bool,
    pub media: Option<&'a crate::media::MediaInfo>,
    pub lyrics: Option<&'a crate::lyrics::LyricsCache>,
    pub pomodoro: &'a crate::pomodoro::Pomodoro,
    pub todo: &'a crate::todo::TodoList,
    pub water: &'a crate::water::Water,
    pub todo_editing: bool,
    pub todo_input: &'a str,
    pub shelf: &'a crate::shelf::Shelf,
    pub tray_page: usize,
    pub tray_drop_highlight: bool,
    pub tray_drag_pt: Option<(f32, f32)>,
    pub tray_spring_scale: f32,
    pub cur_w: f32,
    pub cur_h: f32,
    pub alert: Option<&'a PillAlert>,
}

fn format_time_02(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn format_pomodoro_time(secs: i64) -> String {
    let s = secs.max(0);
    format!("{}:{:02}", s / 60, s % 60)
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

const SYSTEM_GREEN: (f32, f32, f32) = (0.204, 0.780, 0.349); // #34C759
const SYSTEM_RED: (f32, f32, f32) = (1.000, 0.231, 0.188); // #FF3B30
const SYSTEM_BLUE: (f32, f32, f32) = (0.039, 0.518, 1.000); // #0A84FF
const LIGHT_BLUE: (f32, f32, f32) = (0.500, 0.800, 1.000);

struct LyricRollItem {
    lines: Vec<String>,
    is_bold_19: bool,
    start_top: f32,
    height: f32,
    scale_from: f32,
    scale_to: f32,
    alpha_from: f32,
    alpha_to: f32,
}

struct LyricRollState {
    track_key: String,
    current_line: String,
    prev_line: Option<String>,
    next_line: Option<String>,
    items: Vec<LyricRollItem>,
    static_entries: Vec<(Vec<String>, bool, f32, f32)>,
    distance: f32,
    started_at: Instant,
}

pub struct Renderer {
    factory: ID2D1Factory,
    dc_target: Option<ID2D1DCRenderTarget>,
    target: Option<ID2D1RenderTarget>,
    mem_dc: HDC,
    dib_bmp: HBITMAP,
    old_bmp: HGDIOBJ,
    canvas_w_px: i32,
    canvas_h_px: i32,
    dwrite: IDWriteFactory,
    // Cached fonts matching `IslandView.swift` sizes & weights:
    fmt_10_med: Option<IDWriteTextFormat>,
    fmt_11_reg: Option<IDWriteTextFormat>,
    fmt_12_reg: Option<IDWriteTextFormat>,
    fmt_13_reg: Option<IDWriteTextFormat>,
    fmt_13_med: Option<IDWriteTextFormat>,
    fmt_13_bold: Option<IDWriteTextFormat>,
    fmt_14_reg: Option<IDWriteTextFormat>,
    fmt_14_med: Option<IDWriteTextFormat>,
    fmt_14_semi: Option<IDWriteTextFormat>,
    fmt_15_reg: Option<IDWriteTextFormat>,
    fmt_15_semi: Option<IDWriteTextFormat>,
    fmt_16_reg: Option<IDWriteTextFormat>,
    fmt_18_med: Option<IDWriteTextFormat>,
    fmt_19_bold: Option<IDWriteTextFormat>,
    fmt_20_bold: Option<IDWriteTextFormat>,
    fmt_25_bold: Option<IDWriteTextFormat>,
    fmt_28_reg: Option<IDWriteTextFormat>,
    fmt_28_bold: Option<IDWriteTextFormat>,
    fmt_44_bold: Option<IDWriteTextFormat>,
    // Stroke styles:
    stroke_round_cap: Option<ID2D1StrokeStyle>,
    stroke_dashed: Option<ID2D1StrokeStyle>,
    stroke_dotted: Option<ID2D1StrokeStyle>,
    // Cover art & dominant color cache:
    cover_bitmap: Option<ID2D1Bitmap>,
    cover_arc_ptr: usize,
    dominant_rgb: Option<(f32, f32, f32)>,
    // Lyric slot-machine roll animation state:
    lyric_roll: Option<LyricRollState>,
    started_at: Instant,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
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

            let stroke_round_cap = factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_ROUND,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        miterLimit: 10.0,
                        dashStyle: D2D1_DASH_STYLE_SOLID,
                        dashOffset: 0.0,
                    },
                    None,
                )
                .ok();

            let dashes_8_6 = [5.33f32, 4.0f32];
            let stroke_dashed = factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_ROUND,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        miterLimit: 10.0,
                        dashStyle: D2D1_DASH_STYLE_CUSTOM,
                        dashOffset: 0.0,
                    },
                    Some(&dashes_8_6),
                )
                .ok();

            let dashes_1_3 = [1.0f32, 3.0f32];
            let stroke_dotted = factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_ROUND,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        miterLimit: 10.0,
                        dashStyle: D2D1_DASH_STYLE_CUSTOM,
                        dashOffset: 0.0,
                    },
                    Some(&dashes_1_3),
                )
                .ok();

            Ok(Self {
                factory,
                dc_target: None,
                target: None,
                mem_dc: HDC::default(),
                dib_bmp: HBITMAP::default(),
                old_bmp: HGDIOBJ::default(),
                canvas_w_px: 0,
                canvas_h_px: 0,
                fmt_10_med: make_fmt(10.0, DWRITE_FONT_WEIGHT_MEDIUM),
                fmt_11_reg: make_fmt(11.0, DWRITE_FONT_WEIGHT_NORMAL),
                fmt_12_reg: make_fmt(12.0, DWRITE_FONT_WEIGHT_NORMAL),
                fmt_13_reg: make_fmt(13.0, DWRITE_FONT_WEIGHT_NORMAL),
                fmt_13_med: make_fmt(13.0, DWRITE_FONT_WEIGHT_MEDIUM),
                fmt_13_bold: make_fmt(13.0, DWRITE_FONT_WEIGHT_BOLD),
                fmt_14_reg: make_fmt(14.0, DWRITE_FONT_WEIGHT_NORMAL),
                fmt_14_med: make_fmt(14.0, DWRITE_FONT_WEIGHT_MEDIUM),
                fmt_14_semi: make_fmt(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
                fmt_15_reg: make_fmt(15.0, DWRITE_FONT_WEIGHT_NORMAL),
                fmt_15_semi: make_fmt(15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
                fmt_16_reg: make_fmt(16.0, DWRITE_FONT_WEIGHT_NORMAL),
                fmt_18_med: make_fmt(18.0, DWRITE_FONT_WEIGHT_MEDIUM),
                fmt_19_bold: make_fmt(19.0, DWRITE_FONT_WEIGHT_BOLD),
                fmt_20_bold: make_fmt(20.0, DWRITE_FONT_WEIGHT_BOLD),
                fmt_25_bold: make_fmt(25.0, DWRITE_FONT_WEIGHT_BOLD),
                fmt_28_reg: make_fmt(28.0, DWRITE_FONT_WEIGHT_NORMAL),
                fmt_28_bold: make_fmt(28.0, DWRITE_FONT_WEIGHT_BOLD),
                fmt_44_bold: make_fmt(44.0, DWRITE_FONT_WEIGHT_BOLD),
                dwrite,
                stroke_round_cap,
                stroke_dashed,
                stroke_dotted,
                cover_bitmap: None,
                cover_arc_ptr: 0,
                dominant_rgb: None,
                lyric_roll: None,
                started_at: Instant::now(),
            })
        }
    }

    /// True when the 60fps lyric slot-machine roll animation is currently running.
    pub fn is_lyric_rolling(&self) -> bool {
        self.lyric_roll
            .as_ref()
            .map(|r| r.started_at.elapsed().as_secs_f32() < 0.70)
            .unwrap_or(false)
    }

    fn ensure_target(&mut self, canvas_w_px: i32, canvas_h_px: i32, dpi: f32) -> bool {
        let w_px = canvas_w_px.max(1);
        let h_px = canvas_h_px.max(1);
        if self.target.is_some() && self.canvas_w_px == w_px && self.canvas_h_px == h_px {
            if let Some(t) = &self.target {
                unsafe {
                    t.SetDpi(dpi, dpi);
                }
            }
            return true;
        }
        unsafe {
            self.cover_bitmap = None;
            self.target = None;
            self.dc_target = None;
            if !self.mem_dc.0.is_null() {
                if !self.old_bmp.0.is_null() {
                    let _ = SelectObject(self.mem_dc, self.old_bmp);
                    self.old_bmp = HGDIOBJ::default();
                }
                if !self.dib_bmp.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(self.dib_bmp.0));
                    self.dib_bmp = HBITMAP::default();
                }
                let _ = DeleteDC(self.mem_dc);
                self.mem_dc = HDC::default();
            }

            let mem_dc = CreateCompatibleDC(None);
            if mem_dc.0.is_null() {
                return false;
            }
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w_px;
            bmi.bmiHeader.biHeight = -h_px; // top-down DIB
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;

            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let Ok(dib_bmp) =
                CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            else {
                let _ = DeleteDC(mem_dc);
                return false;
            };
            if dib_bmp.0.is_null() {
                let _ = DeleteDC(mem_dc);
                return false;
            }
            let old_bmp = SelectObject(mem_dc, HGDIOBJ(dib_bmp.0));

            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi,
                dpiY: dpi,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let Ok(dc_target) = self.factory.CreateDCRenderTarget(&props) else {
                let _ = SelectObject(mem_dc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(dib_bmp.0));
                let _ = DeleteDC(mem_dc);
                return false;
            };
            let sub_rect = RECT {
                left: 0,
                top: 0,
                right: w_px,
                bottom: h_px,
            };
            if dc_target.BindDC(mem_dc, &sub_rect).is_err() {
                let _ = SelectObject(mem_dc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(dib_bmp.0));
                let _ = DeleteDC(mem_dc);
                return false;
            }
            let Ok(target) = dc_target.cast::<ID2D1RenderTarget>() else {
                let _ = SelectObject(mem_dc, old_bmp);
                let _ = DeleteObject(HGDIOBJ(dib_bmp.0));
                let _ = DeleteDC(mem_dc);
                return false;
            };
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.mem_dc = mem_dc;
            self.dib_bmp = dib_bmp;
            self.old_bmp = old_bmp;
            self.canvas_w_px = w_px;
            self.canvas_h_px = h_px;
            self.dc_target = Some(dc_target);
            self.target = Some(target);
            true
        }
    }

    /// Build a 112x112 (2x supersampled for 56x56) aspect-fill cropped, 12pt rounded-corner
    /// premultiplied BGRA bitmap + compute darkened dominant color (`IslandView.dominantColor` + `darkenForText`).
    fn update_cover_bitmap(&mut self, media: Option<&crate::media::MediaInfo>) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        let Some(m) = media else {
            self.cover_bitmap = None;
            self.cover_arc_ptr = 0;
            self.dominant_rgb = None;
            return;
        };
        let Some(bgra) = m.cover_bgra.as_ref() else {
            self.cover_bitmap = None;
            self.cover_arc_ptr = 0;
            self.dominant_rgb = None;
            return;
        };
        if m.cover_w == 0 || m.cover_h == 0 || bgra.len() < (m.cover_w * m.cover_h * 4) as usize {
            self.cover_bitmap = None;
            self.cover_arc_ptr = 0;
            self.dominant_rgb = None;
            return;
        }
        let ptr = Arc::as_ptr(bgra) as usize;
        if ptr == self.cover_arc_ptr && self.cover_bitmap.is_some() {
            return;
        }

        let src_w = m.cover_w as f32;
        let src_h = m.cover_h as f32;
        let dst_size: u32 = 112; // 2x of 56pt
        let dst_f = dst_size as f32;
        let radius = 24.0f32; // 2x of 12pt corner radius

        // Aspect-fill crop box in source coordinates:
        let scale = (dst_f / src_w).max(dst_f / src_h);
        let crop_w = dst_f / scale;
        let crop_h = dst_f / scale;
        let crop_x0 = (src_w - crop_w) * 0.5;
        let crop_y0 = (src_h - crop_h) * 0.5;

        let mut out = vec![0u8; (dst_size * dst_size * 4) as usize];
        let mut sum_r: u64 = 0;
        let mut sum_g: u64 = 0;
        let mut sum_b: u64 = 0;
        let mut count_px: u64 = 0;

        for dy in 0..dst_size {
            let py = dy as f32 + 0.5;
            for dx in 0..dst_size {
                let px = dx as f32 + 0.5;
                // Signed distance to rounded rect [0..dst_f, 0..dst_f] with corner `radius`:
                let qx = (px - dst_f * 0.5).abs() - (dst_f * 0.5 - radius);
                let qy = (py - dst_f * 0.5).abs() - (dst_f * 0.5 - radius);
                let dist = (qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0)) - radius;
                let mask_alpha = (0.5 - dist).clamp(0.0, 1.0);
                if mask_alpha <= 0.0 {
                    continue;
                }
                let sx = (crop_x0 + (px / dst_f) * crop_w).clamp(0.0, src_w - 1.0) as u32;
                let sy = (crop_y0 + (py / dst_f) * crop_h).clamp(0.0, src_h - 1.0) as u32;
                let s_idx = ((sy * m.cover_w + sx) * 4) as usize;
                let b = bgra[s_idx] as f32;
                let g = bgra[s_idx + 1] as f32;
                let r = bgra[s_idx + 2] as f32;
                let a = bgra[s_idx + 3] as f32;

                sum_r += r as u64;
                sum_g += g as u64;
                sum_b += b as u64;
                count_px += 1;

                let d_idx = ((dy * dst_size + dx) * 4) as usize;
                out[d_idx] = (b * mask_alpha).round() as u8;
                out[d_idx + 1] = (g * mask_alpha).round() as u8;
                out[d_idx + 2] = (r * mask_alpha).round() as u8;
                out[d_idx + 3] = (a * mask_alpha).round() as u8;
            }
        }

        if count_px > 0 {
            let mut r = (sum_r as f32 / count_px as f32) / 255.0;
            let mut g = (sum_g as f32 / count_px as f32) / 255.0;
            let mut b = (sum_b as f32 / count_px as f32) / 255.0;
            // `darkenForText`: luminance <= 0.28 so white text stays crisp.
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            if lum > 0.28 {
                let s = 0.28 / lum;
                r *= s;
                g *= s;
                b *= s;
            }
            self.dominant_rgb = Some((r, g, b));
        } else {
            self.dominant_rgb = None;
        }

        unsafe {
            let bmp_props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            if let Ok(bmp) = target.CreateBitmap(
                D2D_SIZE_U {
                    width: dst_size,
                    height: dst_size,
                },
                Some(out.as_ptr() as *const core::ffi::c_void),
                dst_size * 4,
                &bmp_props,
            ) {
                self.cover_bitmap = Some(bmp);
                self.cover_arc_ptr = ptr;
            }
        }
    }

    // --- Primitive & Text Drawing Helpers ---

    fn brush(&self, target: &ID2D1RenderTarget, c: D2D1_COLOR_F) -> Option<ID2D1SolidColorBrush> {
        unsafe { target.CreateSolidColorBrush(&c, None).ok() }
    }

    fn fill_rounded(&self, target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, r: f32, c: D2D1_COLOR_F) {
        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: x,
                            top: y,
                            right: x + w,
                            bottom: y + h,
                        },
                        radiusX: r,
                        radiusY: r,
                    },
                    &b,
                );
            }
        }
    }

    fn stroke_rounded(
        &self,
        target: &ID2D1RenderTarget,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        stroke_w: f32,
        c: D2D1_COLOR_F,
        style: Option<&ID2D1StrokeStyle>,
    ) {
        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: x,
                            top: y,
                            right: x + w,
                            bottom: y + h,
                        },
                        radiusX: r,
                        radiusY: r,
                    },
                    &b,
                    stroke_w,
                    style,
                );
            }
        }
    }

    fn fill_oval(&self, target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, c: D2D1_COLOR_F) {
        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: D2D_POINT_2F {
                            x: x + w * 0.5,
                            y: y + h * 0.5,
                        },
                        radiusX: w * 0.5,
                        radiusY: h * 0.5,
                    },
                    &b,
                );
            }
        }
    }

    fn stroke_oval(&self, target: &ID2D1RenderTarget, x: f32, y: f32, w: f32, h: f32, stroke_w: f32, c: D2D1_COLOR_F) {
        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.DrawEllipse(
                    &D2D1_ELLIPSE {
                        point: D2D_POINT_2F {
                            x: x + w * 0.5,
                            y: y + h * 0.5,
                        },
                        radiusX: w * 0.5,
                        radiusY: h * 0.5,
                    },
                    &b,
                    stroke_w,
                    None,
                );
            }
        }
    }

    fn measure_text(&self, text: &str, fmt: &IDWriteTextFormat) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide, fmt, 4096.0, 256.0) else {
                return (0.0, 0.0);
            };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_ok() {
                (metrics.widthIncludingTrailingWhitespace, metrics.height)
            } else {
                (0.0, 0.0)
            }
        }
    }

    fn truncate_text(&self, text: &str, max_width: f32, fmt: &IDWriteTextFormat) -> String {
        if self.measure_text(text, fmt).0 <= max_width {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let candidate = format!("{}…", chars.iter().collect::<String>());
            if self.measure_text(&candidate, fmt).0 <= max_width {
                return candidate;
            }
        }
        "…".to_string()
    }

    fn draw_text_at(
        &self,
        target: &ID2D1RenderTarget,
        text: &str,
        fmt: Option<&IDWriteTextFormat>,
        c: D2D1_COLOR_F,
        x: f32,
        y: f32,
    ) {
        let Some(fmt) = fmt else { return };
        if text.is_empty() {
            return;
        }
        let Some(b) = self.brush(target, c) else { return };
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            if let Ok(layout) = self.dwrite.CreateTextLayout(&wide, fmt, 2048.0, 256.0) {
                target.DrawTextLayout(
                    D2D_POINT_2F { x, y },
                    &layout,
                    &b,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }
    }

    fn draw_strikethrough_text_at(
        &self,
        target: &ID2D1RenderTarget,
        text: &str,
        fmt: Option<&IDWriteTextFormat>,
        c: D2D1_COLOR_F,
        x: f32,
        y: f32,
    ) {
        let Some(fmt) = fmt else { return };
        self.draw_text_at(target, text, Some(fmt), c, x, y);
        let (w, h) = self.measure_text(text, fmt);
        if w > 0.0 {
            if let Some(b) = self.brush(target, c) {
                let line_y = y + h * 0.55;
                unsafe {
                    target.DrawLine(
                        D2D_POINT_2F { x, y: line_y },
                        D2D_POINT_2F { x: x + w, y: line_y },
                        &b,
                        1.2,
                        None,
                    );
                }
            }
        }
    }

    fn draw_centered_text(
        &self,
        target: &ID2D1RenderTarget,
        text: &str,
        fmt: Option<&IDWriteTextFormat>,
        c: D2D1_COLOR_F,
        lw: f32,
        y: f32,
    ) {
        let Some(fmt) = fmt else { return };
        let (w, _) = self.measure_text(text, fmt);
        self.draw_text_at(target, text, Some(fmt), c, (lw - w) * 0.5, y);
    }

    fn draw_right_aligned_text(
        &self,
        target: &ID2D1RenderTarget,
        text: &str,
        fmt: Option<&IDWriteTextFormat>,
        c: D2D1_COLOR_F,
        right_x: f32,
        y: f32,
    ) {
        let Some(fmt) = fmt else { return };
        let (w, _) = self.measure_text(text, fmt);
        self.draw_text_at(target, text, Some(fmt), c, right_x - w, y);
    }

    // --- Vector Symbol Icons matching SF Symbols in `IslandView.swift` ---

    fn fill_polygon(&self, target: &ID2D1RenderTarget, pts: &[D2D_POINT_2F], c: D2D1_COLOR_F) {
        if pts.len() < 3 {
            return;
        }
        let Some(b) = self.brush(target, c) else { return };
        unsafe {
            let Ok(path) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&pts[1..]);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            let _ = sink.Close();
            target.FillGeometry(&path, &b, None);
        }
    }

    fn stroke_polyline(
        &self,
        target: &ID2D1RenderTarget,
        pts: &[D2D_POINT_2F],
        stroke_w: f32,
        c: D2D1_COLOR_F,
    ) {
        if pts.len() < 2 {
            return;
        }
        let Some(b) = self.brush(target, c) else { return };
        unsafe {
            let Ok(path) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLines(&pts[1..]);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            target.DrawGeometry(&path, &b, stroke_w, self.stroke_round_cap.as_ref());
        }
    }

    /// Speech bubble with 3 dots (`ellipsis.bubble.fill`) centered at `(cx, cy)`.
    fn draw_icon_bubble(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        self.fill_rounded(target, cx - 10.0, cy - 8.5, 20.0, 15.0, 6.0, c);
        let tail = [
            D2D_POINT_2F { x: cx - 5.0, y: cy + 5.5 },
            D2D_POINT_2F { x: cx - 8.5, y: cy + 9.5 },
            D2D_POINT_2F { x: cx - 1.0, y: cy + 6.5 },
        ];
        self.fill_polygon(target, &tail, c);
        let dot_c = color(0.0, 0.0, 0.0, 0.95);
        for dx in [-4.5f32, 0.0, 4.5] {
            self.fill_oval(target, cx + dx - 1.3, cy - 2.3, 2.6, 2.6, dot_c);
        }
    }

    /// Play triangle (`play.fill`) centered at `(cx, cy)`.
    fn draw_icon_play(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, s: f32, c: D2D1_COLOR_F) {
        let r = s * 0.42;
        let pts = [
            D2D_POINT_2F { x: cx - r * 0.7, y: cy - r },
            D2D_POINT_2F { x: cx + r * 0.95, y: cy },
            D2D_POINT_2F { x: cx - r * 0.7, y: cy + r },
        ];
        self.fill_polygon(target, &pts, c);
    }

    /// Pause bars (`pause.fill`) centered at `(cx, cy)`.
    fn draw_icon_pause(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, s: f32, c: D2D1_COLOR_F) {
        let h = s * 0.78;
        let w = s * 0.22;
        let gap = s * 0.16;
        self.fill_rounded(target, cx - gap * 0.5 - w, cy - h * 0.5, w, h, w * 0.4, c);
        self.fill_rounded(target, cx + gap * 0.5, cy - h * 0.5, w, h, w * 0.4, c);
    }

    /// Backward (`backward.fill`) centered at `(cx, cy)`.
    fn draw_icon_backward(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        let t1 = [
            D2D_POINT_2F { x: cx - 9.0, y: cy },
            D2D_POINT_2F { x: cx - 0.5, y: cy - 6.5 },
            D2D_POINT_2F { x: cx - 0.5, y: cy + 6.5 },
        ];
        let t2 = [
            D2D_POINT_2F { x: cx - 0.5, y: cy },
            D2D_POINT_2F { x: cx + 8.0, y: cy - 6.5 },
            D2D_POINT_2F { x: cx + 8.0, y: cy + 6.5 },
        ];
        self.fill_polygon(target, &t1, c);
        self.fill_polygon(target, &t2, c);
    }

    /// Forward (`forward.fill`) centered at `(cx, cy)`.
    fn draw_icon_forward(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        let t1 = [
            D2D_POINT_2F { x: cx + 0.5, y: cy },
            D2D_POINT_2F { x: cx - 8.0, y: cy - 6.5 },
            D2D_POINT_2F { x: cx - 8.0, y: cy + 6.5 },
        ];
        let t2 = [
            D2D_POINT_2F { x: cx + 9.0, y: cy },
            D2D_POINT_2F { x: cx + 0.5, y: cy - 6.5 },
            D2D_POINT_2F { x: cx + 0.5, y: cy + 6.5 },
        ];
        self.fill_polygon(target, &t1, c);
        self.fill_polygon(target, &t2, c);
    }

    /// Stopwatch (`stopwatch`) centered at `(cx, cy)`.
    fn draw_icon_stopwatch(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, size: f32, c: D2D1_COLOR_F) {
        let r = size * 0.38;
        self.stroke_oval(target, cx - r, cy - r + 2.0, r * 2.0, r * 2.0, 2.2, c);
        // Top stem + button:
        self.fill_rounded(target, cx - 3.5, cy - r - 3.0, 7.0, 2.2, 1.0, c);
        self.fill_rounded(target, cx - 1.2, cy - r - 1.0, 2.4, 3.0, 1.0, c);
        // Clock hands:
        let hands = [
            D2D_POINT_2F { x: cx, y: cy + 2.0 - r * 0.55 },
            D2D_POINT_2F { x: cx, y: cy + 2.0 },
            D2D_POINT_2F { x: cx + r * 0.42, y: cy + 2.0 + r * 0.25 },
        ];
        self.stroke_polyline(target, &hands, 2.2, c);
    }

    /// Clockwise reset arrow (`arrow.clockwise`) centered at `(cx, cy)`.
    fn draw_icon_reset(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        let r = 6.2f32;
        let mut pts = Vec::new();
        let steps = 20;
        let a0 = -std::f32::consts::FRAC_PI_2;
        let a1 = a0 + std::f32::consts::PI * 1.65;
        for i in 0..=steps {
            let a = a0 + (a1 - a0) * (i as f32 / steps as f32);
            pts.push(D2D_POINT_2F {
                x: cx + r * a.cos(),
                y: cy + r * a.sin(),
            });
        }
        self.stroke_polyline(target, &pts, 1.8, c);
        let head = [
            D2D_POINT_2F { x: cx - 0.5, y: cy - r - 3.0 },
            D2D_POINT_2F { x: cx + 4.0, y: cy - r },
            D2D_POINT_2F { x: cx - 0.5, y: cy - r + 3.0 },
        ];
        self.fill_polygon(target, &head, c);
    }

    /// Cup (`cup.and.saucer`) centered at `(cx, cy)`.
    fn draw_icon_cup(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        self.stroke_rounded(target, cx - 5.5, cy - 4.5, 9.0, 7.5, 2.0, 1.5, c, None);
        self.stroke_oval(target, cx + 2.0, cy - 3.5, 4.5, 5.0, 1.4, c);
        self.fill_rounded(target, cx - 6.5, cy + 4.0, 12.0, 1.5, 0.75, c);
    }

    /// Target (`target`) centered at `(cx, cy)`.
    fn draw_icon_target(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        self.stroke_oval(target, cx - 6.5, cy - 6.5, 13.0, 13.0, 1.5, c);
        self.stroke_oval(target, cx - 3.5, cy - 3.5, 7.0, 7.0, 1.4, c);
        self.fill_oval(target, cx - 1.3, cy - 1.3, 2.6, 2.6, c);
    }

    /// Checkmark (`checkmark`) centered at `(cx, cy)`.
    fn draw_icon_checkmark(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        let pts = [
            D2D_POINT_2F { x: cx - 4.2, y: cy + 0.2 },
            D2D_POINT_2F { x: cx - 1.2, y: cy + 3.2 },
            D2D_POINT_2F { x: cx + 4.5, y: cy - 3.0 },
        ];
        self.stroke_polyline(target, &pts, 2.0, c);
    }

    /// Water drop (`drop.fill`) centered at `(cx, cy)`.
    fn draw_icon_drop(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        let top = [
            D2D_POINT_2F { x: cx, y: cy - 6.5 },
            D2D_POINT_2F { x: cx - 4.8, y: cy + 1.0 },
            D2D_POINT_2F { x: cx + 4.8, y: cy + 1.0 },
        ];
        self.fill_polygon(target, &top, c);
        self.fill_oval(target, cx - 4.8, cy - 2.2, 9.6, 8.8, c);
    }

    /// Bell (`bell.fill`) centered at `(cx, cy)`.
    fn draw_icon_bell(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        self.fill_oval(target, cx - 4.2, cy - 5.2, 8.4, 8.0, c);
        self.fill_rounded(target, cx - 5.5, cy - 1.0, 11.0, 4.2, 1.5, c);
        self.fill_oval(target, cx - 1.6, cy + 3.2, 3.2, 2.6, c);
    }

    /// Sliders (`slider.horizontal.3`) centered at `(cx, cy)`.
    fn draw_icon_sliders(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        for (dy, kx) in [(-4.0f32, -2.0f32), (0.0, 2.5), (4.0, -1.0)] {
            self.fill_rounded(target, cx - 6.0, cy + dy - 0.7, 12.0, 1.4, 0.7, c);
            self.fill_oval(target, cx + kx - 1.8, cy + dy - 1.8, 3.6, 3.6, c);
        }
    }

    /// Folder (`folder.fill`) centered at `(cx, cy)`.
    fn draw_icon_folder(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, s: f32, c: D2D1_COLOR_F) {
        let w = s * 0.85;
        let h = s * 0.62;
        self.fill_rounded(target, cx - w * 0.5, cy - h * 0.5 - 3.0, w * 0.42, 7.0, 2.5, c);
        self.fill_rounded(target, cx - w * 0.5, cy - h * 0.5, w, h, 4.0, c);
    }

    /// Document (`doc.fill`) centered at `(cx, cy)`.
    fn draw_icon_doc(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, s: f32, c: D2D1_COLOR_F) {
        let w = s * 0.65;
        let h = s * 0.82;
        self.fill_rounded(target, cx - w * 0.5, cy - h * 0.5, w, h, 3.5, c);
        let line_c = color(0.0, 0.0, 0.0, 0.45);
        for dy in [-3.5f32, 1.5, 6.5] {
            self.fill_rounded(target, cx - w * 0.3, cy + dy, w * 0.6, 1.8, 0.9, line_c);
        }
    }

    /// Chevron (`chevron.left` / `chevron.right`) centered at `(cx, cy)`.
    fn draw_icon_chevron(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, right: bool, c: D2D1_COLOR_F) {
        let dx = if right { 3.5f32 } else { -3.5f32 };
        let pts = [
            D2D_POINT_2F { x: cx - dx, y: cy - 5.5 },
            D2D_POINT_2F { x: cx + dx, y: cy },
            D2D_POINT_2F { x: cx - dx, y: cy + 5.5 },
        ];
        self.stroke_polyline(target, &pts, 2.0, c);
    }

    // --- Shared Aura Mac Pieces (`IslandView.swift`) ---

    /// Idle eyes (`drawEyes`, `IslandView.swift` lines 1509-1531).
    fn draw_eyes(&self, target: &ID2D1RenderTarget, lw: f32, lh: f32, eye_look: (f32, f32), eyes_closed: bool) {
        let mid_x = lw * 0.5;
        let mid_y = lh * 0.5;
        // At 140x32: mid_x = 70, so mid_x - 20 = 50 and mid_x + 7 = 77; mid_y - 8 = 8.
        let xs = [mid_x - 20.0, mid_x + 7.0];
        let white = color(1.0, 1.0, 1.0, 1.0);
        for &x in &xs {
            if eyes_closed {
                self.fill_oval(target, x, mid_y - 1.0, 13.0, 2.0, white);
            } else {
                self.fill_oval(target, x, mid_y - 8.0, 13.0, 16.0, white);
            }
        }
        if eyes_closed {
            return;
        }
        let black = color(0.0, 0.0, 0.0, 1.0);
        for &x in &xs {
            let cx = x + 6.5 + eye_look.0.clamp(-1.0, 1.0) * 2.0;
            let cy = mid_y + eye_look.1.clamp(-1.0, 1.0) * 3.0;
            self.fill_oval(target, cx - 3.0, cy - 3.0, 6.0, 6.0, black);
        }
    }

    /// iOS battery icon (`drawBatteryIcon`, `IslandView.swift` lines 1539-1564).
    fn draw_battery_icon(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, percent: Option<f32>, charging: bool) {
        let Some(pct) = percent else { return };
        let level = pct.clamp(0.0, 1.0);
        let w = 24.0f32;
        let h = 12.0f32;
        let x = cx - w * 0.5;
        let y = cy - h * 0.5;
        let white = color(1.0, 1.0, 1.0, 1.0);
        self.stroke_rounded(target, x, y, w, h, 3.5, 1.5, white, None);
        self.fill_rounded(target, x + w + 1.0, cy - 2.5, 2.0, 5.0, 1.0, white);
        let fill_c = if charging {
            color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0)
        } else if level <= 0.20 {
            color(SYSTEM_RED.0, SYSTEM_RED.1, SYSTEM_RED.2, 1.0)
        } else {
            white
        };
        let fill_w = (w - 4.0) * level;
        if fill_w > 0.0 {
            let r = 1.5f32.min(fill_w * 0.5);
            self.fill_rounded(target, x + 2.0, y + 2.0, fill_w, h - 4.0, r, fill_c);
        }
    }

    /// 5-bar animated waveform (`drawWaveform`, `IslandView.swift` lines 1623-1641).
    fn draw_waveform(&self, target: &ID2D1RenderTarget, rx: f32, ry: f32, rw: f32, rh: f32, playing: bool) {
        let bars = 5usize;
        let bar_w = 3.0f32;
        let gap = 3.0f32;
        let total = (bars as f32) * bar_w + ((bars - 1) as f32) * gap;
        let mut x = rx + (rw - total) * 0.5;
        let mid_y = ry + rh * 0.5;
        let wave_phase = self.started_at.elapsed().as_secs_f64() * 10.0;
        let white = color(1.0, 1.0, 1.0, 1.0);
        for i in 0..bars {
            let sine = (wave_phase * 0.9 + (i as f64) * 1.7).sin() as f32;
            let amp = if playing {
                0.35 + 0.65 * (0.5 + 0.5 * sine)
            } else {
                0.18
            };
            let h = (rh * amp).max(2.0);
            self.fill_rounded(target, x, mid_y - h * 0.5, bar_w, h, 1.5, white);
            x += bar_w + gap;
        }
    }

    /// Progress ring (`drawProgressRing`, `IslandView.swift` lines 2326-2347).
    fn draw_progress_ring(
        &self,
        target: &ID2D1RenderTarget,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        fraction: f32,
        c: D2D1_COLOR_F,
    ) {
        self.stroke_oval(
            target,
            cx - radius,
            cy - radius,
            radius * 2.0,
            radius * 2.0,
            width,
            color(1.0, 1.0, 1.0, 0.15),
        );
        let frac = fraction.clamp(0.0, 1.0);
        if frac <= 0.001 {
            return;
        }
        let steps = ((frac * 72.0) as usize).max(2);
        let mut pts = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let theta = -std::f32::consts::FRAC_PI_2
                + (i as f32 / steps as f32) * frac * 2.0 * std::f32::consts::PI;
            pts.push(D2D_POINT_2F {
                x: cx + radius * theta.cos(),
                y: cy + radius * theta.sin(),
            });
        }
        self.stroke_polyline(target, &pts, width, c);
    }

    /// Card header (`drawCardHeader`, `IslandView.swift` lines 1849-1861).
    fn draw_card_header(&self, target: &ID2D1RenderTarget, width: f32, playing: bool) {
        let date_str = chrono::Local::now()
            .format("%a %-d %b %Y %H:%M")
            .to_string();
        let Some(fmt) = self.fmt_13_med.as_ref() else { return };
        let (date_w, _) = self.measure_text(&date_str, fmt);
        let group_w = date_w + 16.0;
        let group_x = (width - group_w) * 0.5;
        self.fill_oval(
            target,
            group_x,
            17.0,
            8.0,
            8.0,
            color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0),
        );
        self.draw_text_at(
            target,
            &date_str,
            Some(fmt),
            color(1.0, 1.0, 1.0, 1.0),
            group_x + 16.0,
            14.0,
        );
        self.draw_waveform(target, width - 68.0, 14.0, 48.0, 16.0, playing);
    }

    /// Vertical page dots on the right edge (`drawPageDots`, `IslandView.swift` lines 554-565, 2117-2130).
    fn draw_page_dots(&self, target: &ID2D1RenderTarget, state: IslandState, lw: f32, lh: f32) {
        let count = 5usize;
        let active = state.section_index();
        let top = (lh - (count as f32) * 18.0) * 0.5;
        let x = lw - 24.0;
        for i in 0..count {
            let y = top + (i as f32) * 18.0;
            if i == active {
                self.fill_rounded(target, x, y + 1.0, 6.0, 16.0, 3.0, color(1.0, 1.0, 1.0, 1.0));
            } else {
                self.fill_oval(target, x, y + 6.0, 6.0, 6.0, color(1.0, 1.0, 1.0, 0.30));
            }
        }
    }

    // --- Closed State (`drawPillAlert` & `drawClosed`) ---

    fn draw_pill_alert(&self, target: &ID2D1RenderTarget, lw: f32, lh: f32, alert: &PillAlert) {
        let mid_y = lh * 0.5;
        let Some(fmt) = self.fmt_14_semi.as_ref() else { return };
        let (msg_w, _) = self.measure_text(&alert.message, fmt);
        let detail_w = alert
            .detail_text
            .as_ref()
            .map(|s| self.measure_text(s, fmt).0)
            .unwrap_or(0.0);
        let icon_w = match alert.icon {
            AlertIcon::Battery { .. } => 27.0,
            _ => 20.0,
        };
        let total = icon_w
            + 10.0
            + msg_w
            + if alert.detail_text.is_some() {
                8.0 + detail_w
            } else {
                0.0
            };
        let start_x = (lw - total) * 0.5;
        match alert.icon {
            AlertIcon::Battery { percent, charging } => {
                self.draw_battery_icon(target, start_x + 12.0, mid_y, Some(percent), charging);
            }
            AlertIcon::CapsLock { on } => {
                let c = if on {
                    color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0)
                } else {
                    color(1.0, 1.0, 1.0, 0.7)
                };
                self.fill_rounded(target, start_x, mid_y - 9.0, 20.0, 18.0, 4.0, color(1.0, 1.0, 1.0, 0.14));
                self.draw_text_at(target, "⇪", self.fmt_13_bold.as_ref(), c, start_x + 4.5, mid_y - 9.0);
            }
            AlertIcon::Stopwatch => {
                self.draw_icon_stopwatch(target, start_x + 10.0, mid_y, 20.0, color(1.0, 1.0, 1.0, 1.0));
            }
            AlertIcon::Checkmark => {
                self.draw_icon_checkmark(
                    target,
                    start_x + 10.0,
                    mid_y,
                    color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0),
                );
            }
        }
        let msg_x = start_x + icon_w + 10.0;
        self.draw_text_at(
            target,
            &alert.message,
            Some(fmt),
            color(1.0, 1.0, 1.0, 1.0),
            msg_x,
            mid_y - 9.5,
        );
        if let Some(detail) = &alert.detail_text {
            let dc = alert
                .detail_color
                .map(|(r, g, b)| color(r, g, b, 1.0))
                .unwrap_or(color(1.0, 1.0, 1.0, 1.0));
            self.draw_text_at(target, detail, Some(fmt), dc, msg_x + msg_w + 8.0, mid_y - 9.5);
        }
    }

    fn draw_closed(&self, target: &ID2D1RenderTarget, ctx: &RenderContext) {
        let lw = ctx.cur_w;
        let lh = ctx.cur_h;
        let mid_y = lh * 0.5;
        let now = chrono::Local::now();

        // 1. Live Pomodoro takes priority (`160x32`):
        if ctx.pomodoro.is_active(now) {
            let rem = ctx.pomodoro.remaining(now);
            let dur = ctx.pomodoro.duration_secs().max(1);
            let frac = (rem as f32 / dur as f32).clamp(0.0, 1.0);
            let alpha = if ctx.pomodoro.running { 1.0 } else { 0.5 };
            let rgb = match ctx.pomodoro.mode {
                crate::pomodoro::PomodoroMode::Focus => SYSTEM_RED,
                crate::pomodoro::PomodoroMode::Break => SYSTEM_GREEN,
            };
            self.draw_progress_ring(
                target,
                30.0,
                mid_y,
                8.0,
                2.0,
                frac,
                color(rgb.0, rgb.1, rgb.2, alpha),
            );
            let text_alpha = if ctx.pomodoro.running { 1.0 } else { 0.7 };
            self.draw_centered_text(
                target,
                &format_pomodoro_time(rem),
                self.fmt_15_semi.as_ref(),
                color(1.0, 1.0, 1.0, text_alpha),
                lw,
                mid_y - 10.0,
            );
            return;
        }

        // 2. Lyrics-mode closed pill (`360x32`, when closed from Lyrics page):
        if ctx.lyrics_mode {
            if let Some(m) = ctx.media {
                self.draw_text_at(
                    target,
                    "♪",
                    self.fmt_18_med.as_ref(),
                    color(1.0, 1.0, 1.0, 1.0),
                    22.0,
                    mid_y - 12.0,
                );
                let line = Self::current_lyric_triple(Some(m), ctx.lyrics)
                    .1
                    .unwrap_or_else(|| m.title.clone());
                if let Some(fmt) = self.fmt_13_med.as_ref() {
                    let shown = self.truncate_text(&line, 236.0, fmt);
                    self.draw_centered_text(
                        target,
                        &shown,
                        Some(fmt),
                        color(1.0, 1.0, 1.0, 1.0),
                        lw,
                        mid_y - 9.0,
                    );
                }
                return;
            }
        }

        // 3. Plain music / hover pill (`180x32`): battery icon + HH:mm clock + 5-bar waveform.
        self.draw_battery_icon(target, 30.0, mid_y, ctx.battery_percent, ctx.battery_charging);
        let hm = now.format("%H:%M").to_string();
        self.draw_centered_text(
            target,
            &hm,
            self.fmt_15_semi.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            lw,
            mid_y - 10.0,
        );
        let wave_h = if lh <= 40.0 { 14.0 } else { 20.0 };
        let playing = ctx.media.map(|m| m.playing).unwrap_or(false);
        self.draw_waveform(target, lw - 50.0, mid_y - wave_h * 0.5, 28.0, wave_h, playing);
    }

    // --- Music Card (`Expanded`, `520x180`) & Lyrics Card (`Lyrics`, `700x180`) ---

    fn draw_music_card(
        &self,
        target: &ID2D1RenderTarget,
        m: &crate::media::MediaInfo,
        lyrics_t: f32,
    ) {
        if lyrics_t < 0.5 {
            self.draw_card_header(target, 520.0, m.playing);
        }

        // Cover art (`x: 36, y: 36, w: 56, h: 56, cornerRadius: 12`):
        let cover_rect = D2D_RECT_F {
            left: 36.0,
            top: 36.0,
            right: 92.0,
            bottom: 92.0,
        };
        if let Some(bmp) = &self.cover_bitmap {
            unsafe {
                target.DrawBitmap(
                    bmp,
                    Some(&cover_rect),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        } else {
            self.fill_rounded(target, 36.0, 36.0, 56.0, 56.0, 12.0, color(1.0, 1.0, 1.0, 0.12));
            if let Some(fmt) = self.fmt_28_reg.as_ref() {
                let (w, h) = self.measure_text("♪", fmt);
                self.draw_text_at(
                    target,
                    "♪",
                    Some(fmt),
                    color(1.0, 1.0, 1.0, 0.70),
                    64.0 - w * 0.5,
                    64.0 - h * 0.5,
                );
            }
        }

        // Smoothly interpolate widths & positions between `520` (`lyrics_t = 0.0`) and `700` (`lyrics_t = 1.0`).
        // Truncate at stable target widths (`212` when settled in Lyrics, `340` otherwise) and clip
        // with `PushAxisAlignedClip` so characters never jump/flicker during `520 <-> 700` morphs.
        let text_w = 340.0 + (212.0 - 340.0) * lyrics_t;
        let trunc_w = if lyrics_t >= 0.99 { 212.0 } else { 340.0 };
        let title_raw = if m.title.is_empty() { "Unknown title" } else { m.title.as_str() };
        let artist_raw = if m.artist.is_empty() { "Unknown artist" } else { m.artist.as_str() };
        unsafe {
            target.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 116.0,
                    top: 30.0,
                    right: 116.0 + text_w,
                    bottom: 96.0,
                },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
        }
        if let Some(fmt) = self.fmt_25_bold.as_ref() {
            let shown = self.truncate_text(title_raw, trunc_w, fmt);
            self.draw_text_at(target, &shown, Some(fmt), color(1.0, 1.0, 1.0, 1.0), 116.0, 34.0);
        }
        if let Some(fmt) = self.fmt_16_reg.as_ref() {
            let shown = self.truncate_text(artist_raw, trunc_w, fmt);
            self.draw_text_at(target, &shown, Some(fmt), color(1.0, 1.0, 1.0, 0.65), 116.0, 68.0);
        }
        unsafe {
            target.PopAxisAlignedClip();
        }

        // Progress row (`y: 104`):
        let bar_w = 324.0 + (164.0 - 324.0) * lyrics_t;
        let right_x = 484.0 + (320.0 - 484.0) * lyrics_t;
        let pos = m.position_secs.max(0.0);
        let dur = m.duration_secs.max(0.0);
        self.draw_text_at(
            target,
            &format_time_02(pos),
            self.fmt_12_reg.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            36.0,
            102.0,
        );
        if dur > 0.0 {
            let rem = (dur - pos).max(0.0);
            self.draw_right_aligned_text(
                target,
                &format!("-{}", format_time_02(rem)),
                self.fmt_12_reg.as_ref(),
                color(1.0, 1.0, 1.0, 1.0),
                right_x,
                102.0,
            );
        }
        self.fill_rounded(target, 96.0, 110.0, bar_w, 4.0, 2.0, color(1.0, 1.0, 1.0, 0.25));
        if dur > 0.0 {
            let filled = (pos / dur).clamp(0.0, 1.0) as f32;
            let fill_w = bar_w * filled;
            if fill_w > 0.0 {
                self.fill_rounded(target, 96.0, 110.0, fill_w, 4.0, 2.0, color(1.0, 1.0, 1.0, 1.0));
            }
        }

        // Controls row (`y: 140`):
        let white = color(1.0, 1.0, 1.0, 1.0);
        self.draw_icon_bubble(target, 56.0, 140.0, white);
        let c0 = MUSIC_CONTROL_CENTERS[0] + (LYRICS_CONTROL_CENTERS[0] - MUSIC_CONTROL_CENTERS[0]) * lyrics_t;
        let c1 = MUSIC_CONTROL_CENTERS[1] + (LYRICS_CONTROL_CENTERS[1] - MUSIC_CONTROL_CENTERS[1]) * lyrics_t;
        let c2 = MUSIC_CONTROL_CENTERS[2] + (LYRICS_CONTROL_CENTERS[2] - MUSIC_CONTROL_CENTERS[2]) * lyrics_t;
        self.draw_icon_backward(target, c0, 140.0, white);
        if m.playing {
            self.draw_icon_pause(target, c1, 140.0, 18.0, white);
        } else {
            self.draw_icon_play(target, c1, 140.0, 18.0, white);
        }
        self.draw_icon_forward(target, c2, 140.0, white);
    }

    fn current_lyric_triple(
        media: Option<&crate::media::MediaInfo>,
        lyrics: Option<&crate::lyrics::LyricsCache>,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let Some(cache) = lyrics else {
            return (None, None, None);
        };
        if cache.lines.is_empty() {
            return (None, None, None);
        }
        let clean = |s: &str| s.replace("\r\n", " ").replace('\n', " ").replace('\r', " ");
        if !cache.synced {
            let c0 = cache.lines.first().map(|l| clean(&l.text));
            let c1 = cache.lines.get(1).map(|l| clean(&l.text));
            let c2 = cache.lines.get(2).map(|l| clean(&l.text));
            return (c0, c1, c2);
        }
        let pos_ms = media
            .map(|m| (m.position_secs * 1000.0) as i64)
            .unwrap_or(0)
            + cache.nudge_ms;
        let idx = crate::lyrics::current_index(&cache.lines, pos_ms);
        let prev = if idx > 0 {
            cache.lines.get(idx - 1).map(|l| clean(&l.text))
        } else {
            None
        };
        let curr = cache.lines.get(idx).map(|l| clean(&l.text));
        let next = cache.lines.get(idx + 1).map(|l| clean(&l.text));
        (prev, curr, next)
    }

    /// Word-wrap a lyric line into at most `max_lines` visual rows (`IslandView.wrappedLines`).
    fn wrapped_lines(&self, text: &str, fmt: &IDWriteTextFormat, width: f32, max_lines: usize) -> Vec<String> {
        let max_lines = max_lines.max(1);
        let normalized = text.split_whitespace().collect::<Vec<&str>>().join(" ");
        if self.measure_text(&normalized, fmt).0 <= width {
            return vec![normalized];
        }
        let mut pieces: Vec<String> = Vec::new();
        for word in normalized.split_whitespace() {
            let mut rem = word.to_string();
            while self.measure_text(&rem, fmt).0 > width {
                let mut filled = String::new();
                for ch in rem.chars() {
                    let cand = format!("{}{}", filled, ch);
                    if self.measure_text(&cand, fmt).0 <= width {
                        filled.push(ch);
                    } else {
                        break;
                    }
                }
                if filled.is_empty() {
                    if let Some(ch) = rem.chars().next() {
                        filled.push(ch);
                    }
                }
                let drop_bytes = filled.len();
                pieces.push(filled);
                rem = rem[drop_bytes..].to_string();
            }
            if !rem.is_empty() {
                pieces.push(rem);
            }
        }
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for piece in pieces {
            if current.is_empty() {
                current = piece;
            } else {
                let cand = format!("{} {}", current, piece);
                if self.measure_text(&cand, fmt).0 <= width {
                    current = cand;
                } else {
                    lines.push(current);
                    if lines.len() == max_lines {
                        if let Some(last) = lines.last_mut() {
                            *last = self.truncate_text(&format!("{}…", last), width, fmt);
                        }
                        return lines;
                    }
                    current = piece;
                }
            }
        }
        if !current.is_empty() {
            if lines.len() < max_lines {
                lines.push(current);
            } else if let Some(last) = lines.last_mut() {
                *last = self.truncate_text(&format!("{}…", last), width, fmt);
            }
        }
        lines
    }

    fn lyric_line_height(is_bold_19: bool) -> f32 {
        if is_bold_19 { 24.0 } else { 19.0 }
    }

    fn lyric_block_layout(&self, entries: &[(&str, bool)], width: f32) -> (Vec<f32>, Vec<f32>, f32) {
        let mut heights = Vec::with_capacity(entries.len());
        for &(text, is_bold_19) in entries {
            let fmt = if is_bold_19 {
                self.fmt_19_bold.as_ref()
            } else {
                self.fmt_15_reg.as_ref()
            };
            let n = fmt
                .map(|f| self.wrapped_lines(text, f, width, 2).len())
                .unwrap_or(1);
            heights.push((n as f32) * Self::lyric_line_height(is_bold_19));
        }
        let text_h: f32 = heights.iter().sum();
        let gap_count = entries.len().saturating_sub(1);
        let gap = if gap_count > 0 {
            ((130.0 - text_h) / (gap_count as f32)).clamp(0.0, 10.0)
        } else {
            0.0
        };
        let block_top = 28.0 + (130.0 - (text_h + gap * (gap_count as f32))) * 0.5;
        let mut tops = Vec::with_capacity(entries.len());
        let mut y = block_top;
        for &h in &heights {
            tops.push(y);
            y += h + gap;
        }
        (tops, heights, gap)
    }

    fn update_lyric_roll(&mut self, media: Option<&crate::media::MediaInfo>, lyrics: Option<&crate::lyrics::LyricsCache>) {
        let track_key = media
            .map(|m| crate::lyrics::key_for(&m.title, &m.artist))
            .unwrap_or_default();
        let (prev, curr, next) = Self::current_lyric_triple(media, lyrics);
        let Some(curr_str) = curr else {
            self.lyric_roll = None;
            return;
        };

        if let Some(existing) = &self.lyric_roll {
            if existing.track_key == track_key
                && existing.current_line == curr_str
                && existing.prev_line == prev
                && existing.next_line == next
            {
                return;
            }
        }

        let width = 308.0f32;

        // Pre-compute static wrapped lines once per lyric line change so `draw_lyrics_panel`
        // does zero `CreateTextLayout` word-wrap work during `520 <-> 700` morph frames:
        let mut raw_static: Vec<(&str, bool, f32)> = Vec::new();
        if let Some(p) = prev.as_deref() {
            raw_static.push((p, false, 0.35));
        }
        raw_static.push((curr_str.as_str(), true, 1.0));
        if let Some(n) = next.as_deref() {
            raw_static.push((n, false, 0.45));
        }
        let static_refs: Vec<(&str, bool)> =
            raw_static.iter().map(|&(s, b, _)| (s, b)).collect();
        let (static_tops, _, _) = self.lyric_block_layout(&static_refs, width);
        let mut static_entries = Vec::with_capacity(raw_static.len());
        for (i, &(text, is_bold_19, base_alpha)) in raw_static.iter().enumerate() {
            let fmt = if is_bold_19 {
                self.fmt_19_bold.as_ref()
            } else {
                self.fmt_15_reg.as_ref()
            };
            let wrapped = fmt
                .map(|f| self.wrapped_lines(text, f, width, 2))
                .unwrap_or_else(|| vec![text.to_string()]);
            static_entries.push((wrapped, is_bold_19, base_alpha, static_tops[i]));
        }

        // Check if this is a 1-line advance from the previous triple (`old_next == new_current`):
        let mut new_items = Vec::new();
        let mut distance = 0.0f32;
        if let Some(old) = &self.lyric_roll {
            if old.track_key == track_key && old.next_line.as_deref() == Some(curr_str.as_str()) {
                let mut old_entries: Vec<(&str, bool)> = Vec::new();
                if let Some(op) = old.prev_line.as_deref() {
                    old_entries.push((op, false));
                }
                old_entries.push((old.current_line.as_str(), true));
                if let Some(on) = old.next_line.as_deref() {
                    old_entries.push((on, false));
                }

                let (old_tops, old_heights, old_gap) = self.lyric_block_layout(&old_entries, width);
                let old_c_idx = old_entries.len().saturating_sub(1);
                let new_c_idx = if prev.is_some() { 1 } else { 0 };
                if old_c_idx < old_tops.len() && new_c_idx < static_tops.len() {
                    let d = old_tops[old_c_idx] - static_tops[new_c_idx];
                    if d > 0.0 {
                        distance = d;
                        let mut oi = 0usize;
                        if let (Some(op), Some(fmt15)) = (old.prev_line.as_deref(), self.fmt_15_reg.as_ref()) {
                            new_items.push(LyricRollItem {
                                lines: self.wrapped_lines(op, fmt15, width, 2),
                                is_bold_19: false,
                                start_top: old_tops[oi],
                                height: old_heights[oi],
                                scale_from: 1.0,
                                scale_to: 1.0,
                                alpha_from: 0.35,
                                alpha_to: 0.0,
                            });
                            oi += 1;
                        }
                        if let Some(fmt19) = self.fmt_19_bold.as_ref() {
                            new_items.push(LyricRollItem {
                                lines: self.wrapped_lines(&old.current_line, fmt19, width, 2),
                                is_bold_19: true,
                                start_top: old_tops[oi],
                                height: old_heights[oi],
                                scale_from: 1.0,
                                scale_to: 15.0 / 19.0,
                                alpha_from: 1.0,
                                alpha_to: 0.35,
                            });
                            oi += 1;
                            let c_wrapped = self.wrapped_lines(&curr_str, fmt19, width, 2);
                            let c_h = (c_wrapped.len() as f32) * Self::lyric_line_height(true);
                            let c_top = old_tops[oi];
                            new_items.push(LyricRollItem {
                                lines: c_wrapped,
                                is_bold_19: true,
                                start_top: c_top,
                                height: c_h,
                                scale_from: 15.0 / 19.0,
                                scale_to: 1.0,
                                alpha_from: 0.45,
                                alpha_to: 1.0,
                            });
                            if let (Some(nn), Some(fmt15)) = (next.as_deref(), self.fmt_15_reg.as_ref()) {
                                let d_wrapped = self.wrapped_lines(nn, fmt15, width, 2);
                                let d_h = (d_wrapped.len() as f32) * Self::lyric_line_height(false);
                                new_items.push(LyricRollItem {
                                    lines: d_wrapped,
                                    is_bold_19: false,
                                    start_top: c_top + c_h + old_gap,
                                    height: d_h,
                                    scale_from: 1.0,
                                    scale_to: 1.0,
                                    alpha_from: 0.0,
                                    alpha_to: 0.45,
                                });
                            }
                        }
                    }
                }
            }
        }

        self.lyric_roll = Some(LyricRollState {
            track_key,
            current_line: curr_str,
            prev_line: prev,
            next_line: next,
            items: new_items,
            static_entries,
            distance,
            started_at: Instant::now(),
        });
    }

    /// Lyrics right-hand panel (`drawLyricsPanel`, `IslandView.swift` lines 1946-2035).
    fn draw_lyrics_panel(
        &self,
        target: &ID2D1RenderTarget,
        _media: Option<&crate::media::MediaInfo>,
        lyrics: Option<&crate::lyrics::LyricsCache>,
        panel_alpha: f32,
        lw: f32,
    ) {
        let clip_right = (lw - 26.0).max(340.0);
        if panel_alpha <= 0.01 || clip_right <= 341.0 {
            return;
        }
        unsafe {
            target.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 340.0,
                    top: 20.0,
                    right: clip_right,
                    bottom: 166.0,
                },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
        }

        // Vertical divider at `x = 340, y = 28..158`:
        self.fill_rounded(
            target,
            340.0,
            28.0,
            1.0,
            130.0,
            0.5,
            color(1.0, 1.0, 1.0, 0.12 * panel_alpha),
        );

        let x = 352.0f32;

        let Some(cache) = lyrics else {
            self.draw_text_at(
                target,
                "Loading lyrics…",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40 * panel_alpha),
                x,
                81.0,
            );
            unsafe {
                target.PopAxisAlignedClip();
            }
            return;
        };

        if cache.lines.is_empty() {
            self.draw_text_at(
                target,
                "Lyrics unavailable",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40 * panel_alpha),
                x,
                81.0,
            );
            unsafe {
                target.PopAxisAlignedClip();
            }
            return;
        }

        if let Some(roll) = &self.lyric_roll {
            let elapsed = roll.started_at.elapsed().as_secs_f32();
            if !roll.items.is_empty() && roll.distance > 0.0 && elapsed < 0.70 {
                let p = (elapsed / 0.70).clamp(0.0, 1.0);
                let prog = if p < 0.5 {
                    4.0 * p * p * p
                } else {
                    1.0 - (-2.0 * p + 2.0).powi(3) * 0.5
                };
                for item in &roll.items {
                    let y = item.start_top - roll.distance * prog;
                    let scale = item.scale_from + (item.scale_to - item.scale_from) * prog;
                    let alpha = (item.alpha_from + (item.alpha_to - item.alpha_from) * prog)
                        .clamp(0.0, 1.0)
                        * panel_alpha;
                    if alpha <= 0.01 {
                        continue;
                    }
                    let fmt = if item.is_bold_19 {
                        self.fmt_19_bold.as_ref()
                    } else {
                        self.fmt_15_reg.as_ref()
                    };
                    let lh = Self::lyric_line_height(item.is_bold_19) * scale;
                    let mut ly = y + item.height * 0.5 * (1.0 - scale);
                    for line in &item.lines {
                        self.draw_text_at(target, line, fmt, color(1.0, 1.0, 1.0, alpha), x, ly);
                        ly += lh;
                    }
                }
                unsafe {
                    target.PopAxisAlignedClip();
                }
                return;
            }

            if !roll.static_entries.is_empty() {
                for (wrapped, is_bold_19, base_alpha, top_y) in &roll.static_entries {
                    let fmt = if *is_bold_19 {
                        self.fmt_19_bold.as_ref()
                    } else {
                        self.fmt_15_reg.as_ref()
                    };
                    let alpha = (*base_alpha * panel_alpha).clamp(0.0, 1.0);
                    let mut y = *top_y;
                    for line in wrapped {
                        self.draw_text_at(target, line, fmt, color(1.0, 1.0, 1.0, alpha), x, y);
                        y += Self::lyric_line_height(*is_bold_19);
                    }
                }
                unsafe {
                    target.PopAxisAlignedClip();
                }
                return;
            }
        }

        self.draw_text_at(
            target,
            "Lyrics unavailable",
            self.fmt_14_reg.as_ref(),
            color(1.0, 1.0, 1.0, 0.40 * panel_alpha),
            x,
            81.0,
        );
        unsafe {
            target.PopAxisAlignedClip();
        }
    }

    // --- Pomodoro Section (`drawPomodoro`, `640x180`, `IslandView.swift` lines 2349-2392) ---

    fn draw_pomodoro(&self, target: &ID2D1RenderTarget, pomodoro: &crate::pomodoro::Pomodoro, playing: bool) {
        self.draw_card_header(target, 640.0, playing);
        self.draw_text_at(
            target,
            "Pomodoro",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            28.0,
            10.0,
        );

        let now = chrono::Local::now();
        let rem = pomodoro.remaining(now);
        let dur = pomodoro.duration_secs().max(1);
        let frac = (rem as f32 / dur as f32).clamp(0.0, 1.0);
        let is_focus = pomodoro.mode == crate::pomodoro::PomodoroMode::Focus;
        let rgb = if is_focus { SYSTEM_RED } else { SYSTEM_GREEN };
        let mode_c = color(rgb.0, rgb.1, rgb.2, 1.0);

        // Left ring (`center: (130, 100), radius: 42, width: 8`) + tinted interior + stopwatch:
        self.draw_progress_ring(target, 130.0, 100.0, 42.0, 8.0, frac, mode_c);
        self.fill_oval(target, 97.0, 67.0, 66.0, 66.0, color(rgb.0, rgb.1, rgb.2, 0.15));
        self.draw_icon_stopwatch(target, 130.0, 100.0, 36.0, color(1.0, 1.0, 1.0, 1.0));

        // Countdown (`44pt bold` at `(220, 48)`) + Reset circle (`(362, 58, 32, 32)`) + subtitle:
        self.draw_text_at(
            target,
            &format_pomodoro_time(rem),
            self.fmt_44_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            220.0,
            42.0,
        );
        self.fill_oval(
            target,
            POMODORO_RESET_RECT.0,
            POMODORO_RESET_RECT.1,
            32.0,
            32.0,
            color(1.0, 1.0, 1.0, 0.12),
        );
        self.draw_icon_reset(target, 378.0, 74.0, color(1.0, 1.0, 1.0, 1.0));
        self.draw_text_at(
            target,
            if is_focus { "Focus time" } else { "Short break" },
            self.fmt_16_reg.as_ref(),
            color(1.0, 1.0, 1.0, 0.60),
            220.0,
            104.0,
        );

        // Mode pills (`y: 130, h: 28`):
        // 1. Short Break pill
        let sb_active = !is_focus;
        self.fill_rounded(
            target,
            POMODORO_SHORT_BREAK_RECT.0,
            POMODORO_SHORT_BREAK_RECT.1,
            POMODORO_SHORT_BREAK_RECT.2 - POMODORO_SHORT_BREAK_RECT.0,
            28.0,
            14.0,
            color(1.0, 1.0, 1.0, if sb_active { 0.22 } else { 0.10 }),
        );
        self.draw_icon_cup(
            target,
            POMODORO_SHORT_BREAK_RECT.0 + 20.0,
            144.0,
            color(1.0, 1.0, 1.0, if sb_active { 1.0 } else { 0.75 }),
        );
        self.draw_text_at(
            target,
            "Short Break",
            self.fmt_14_med.as_ref(),
            color(1.0, 1.0, 1.0, if sb_active { 1.0 } else { 0.75 }),
            POMODORO_SHORT_BREAK_RECT.0 + 34.0,
            134.5,
        );

        // 2. Play/Pause pill (`width: 56`, mode color)
        self.fill_rounded(
            target,
            POMODORO_PLAY_RECT.0,
            POMODORO_PLAY_RECT.1,
            56.0,
            28.0,
            14.0,
            mode_c,
        );
        if pomodoro.running {
            self.draw_icon_pause(target, 380.0, 144.0, 15.0, color(1.0, 1.0, 1.0, 1.0));
        } else {
            self.draw_icon_play(target, 380.0, 144.0, 15.0, color(1.0, 1.0, 1.0, 1.0));
        }

        // 3. Focus Time pill
        let fc_active = is_focus;
        self.fill_rounded(
            target,
            POMODORO_FOCUS_RECT.0,
            POMODORO_FOCUS_RECT.1,
            POMODORO_FOCUS_RECT.2 - POMODORO_FOCUS_RECT.0,
            28.0,
            14.0,
            color(1.0, 1.0, 1.0, if fc_active { 0.22 } else { 0.10 }),
        );
        self.draw_icon_target(
            target,
            POMODORO_FOCUS_RECT.0 + 20.0,
            144.0,
            color(1.0, 1.0, 1.0, if fc_active { 1.0 } else { 0.75 }),
        );
        self.draw_text_at(
            target,
            "Focus Time",
            self.fmt_14_med.as_ref(),
            color(1.0, 1.0, 1.0, if fc_active { 1.0 } else { 0.75 }),
            POMODORO_FOCUS_RECT.0 + 34.0,
            134.5,
        );
    }

    // --- Todo Section (`drawTodo`, `640x220`, `IslandView.swift` lines 2472-2541) ---

    fn draw_todo(
        &self,
        target: &ID2D1RenderTarget,
        todo: &crate::todo::TodoList,
        todo_editing: bool,
        todo_input: &str,
        playing: bool,
    ) {
        self.draw_card_header(target, 640.0, playing);
        self.draw_text_at(
            target,
            "Todo",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            28.0,
            10.0,
        );

        let total = todo.tasks.len();
        let done_count = todo.done_count();
        if total > 0 {
            self.draw_text_at(
                target,
                &format!("{}/{}", done_count, total),
                self.fmt_28_bold.as_ref(),
                color(1.0, 1.0, 1.0, 1.0),
                28.0,
                48.0,
            );
            self.draw_text_at(
                target,
                "done",
                self.fmt_12_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.60),
                28.0,
                88.0,
            );
        } else {
            self.draw_text_at(
                target,
                "No tasks yet",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.60),
                28.0,
                60.0,
            );
        }

        // `"+ Add task"` pill (`x: 28, y: 120, w: 104, h: 28`):
        self.fill_rounded(
            target,
            TODO_ADD_PILL_RECT.0,
            TODO_ADD_PILL_RECT.1,
            TODO_ADD_PILL_RECT.2 - TODO_ADD_PILL_RECT.0,
            28.0,
            14.0,
            color(1.0, 1.0, 1.0, 0.10),
        );
        self.draw_text_at(
            target,
            "+  Add task",
            self.fmt_14_reg.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            40.0,
            124.5,
        );

        let visible = todo.visible_indices();
        if todo_editing {
            self.fill_rounded(
                target,
                TODO_INPUT_RECT.0,
                TODO_INPUT_RECT.1,
                TODO_INPUT_RECT.2 - TODO_INPUT_RECT.0,
                32.0,
                10.0,
                color(1.0, 1.0, 1.0, 0.10),
            );
            let (shown, c) = if todo_input.is_empty() {
                ("New task...".to_string(), color(1.0, 1.0, 1.0, 0.40))
            } else {
                (format!("{}|", todo_input), color(1.0, 1.0, 1.0, 1.0))
            };
            self.draw_text_at(
                target,
                &shown,
                self.fmt_14_reg.as_ref(),
                c,
                TODO_INPUT_RECT.0 + 12.0,
                TODO_INPUT_RECT.1 + 6.5,
            );
            self.draw_text_at(
                target,
                "Enter to add · Esc to cancel",
                self.fmt_11_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                28.0,
                196.0,
            );
        } else if total > visible.len() {
            self.draw_text_at(
                target,
                &format!("+{} more", total - visible.len()),
                self.fmt_11_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                28.0,
                158.0,
            );
        }

        // Right column task rows (`x: 280, w: 320, y: 52 + i * 40, h: 32`):
        for (row_i, &task_idx) in visible.iter().enumerate() {
            let task = &todo.tasks[task_idx];
            let y = TODO_ROW_Y0 + (row_i as f32) * TODO_ROW_PITCH;
            let cy = y + 16.0;
            self.fill_rounded(
                target,
                TODO_ROW_X,
                y,
                TODO_ROW_W,
                TODO_ROW_H,
                10.0,
                color(1.0, 1.0, 1.0, 0.08),
            );
            if task.done {
                self.fill_oval(
                    target,
                    292.0,
                    cy - 10.0,
                    20.0,
                    20.0,
                    color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0),
                );
                self.draw_icon_checkmark(target, 302.0, cy, color(1.0, 1.0, 1.0, 1.0));
            } else {
                self.stroke_oval(
                    target,
                    292.0,
                    cy - 10.0,
                    20.0,
                    20.0,
                    1.5,
                    color(1.0, 1.0, 1.0, 0.60),
                );
            }
            if let Some(fmt) = self.fmt_14_reg.as_ref() {
                let text = self.truncate_text(&task.title, 248.0, fmt);
                if task.done {
                    self.draw_strikethrough_text_at(
                        target,
                        &text,
                        Some(fmt),
                        color(1.0, 1.0, 1.0, 0.50),
                        320.0,
                        y + 6.5,
                    );
                } else {
                    self.draw_text_at(
                        target,
                        &text,
                        Some(fmt),
                        color(1.0, 1.0, 1.0, 1.0),
                        320.0,
                        y + 6.5,
                    );
                }
            }
            self.draw_text_at(
                target,
                "×",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                584.0,
                cy - 9.5,
            );
        }
    }

    // --- Water Reminder Section (`drawWater`, `640x180`, `IslandView.swift` lines 4576-4732) ---

    fn draw_water(&self, target: &ID2D1RenderTarget, water: &crate::water::Water, playing: bool) {
        // Header: `"Water"` white + `"Reminder"` blue + status ring + date + time + waveform:
        if let Some(fmt20) = self.fmt_20_bold.as_ref() {
            self.draw_text_at(target, "Water", Some(fmt20), color(1.0, 1.0, 1.0, 1.0), 48.0, 10.0);
            let (water_w, _) = self.measure_text("Water ", fmt20);
            self.draw_text_at(
                target,
                "Reminder",
                Some(fmt20),
                color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
                48.0 + water_w,
                10.0,
            );
            let (rem_w, _) = self.measure_text("Reminder", fmt20);
            let ring_cx = 48.0 + water_w + rem_w + 20.0;
            let ring_c = if water.notifications_enabled {
                color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0)
            } else {
                color(1.0, 1.0, 1.0, 0.30)
            };
            self.stroke_oval(target, ring_cx - 8.0, 14.0, 16.0, 16.0, 2.0, ring_c);

            let now = chrono::Local::now();
            let date_str = now.format("%a %-d %b %Y").to_string();
            let time_str = now.format("%H:%M").to_string();
            let date_x = ring_cx + 20.0;
            if let Some(fmt_d) = self.fmt_13_med.as_ref() {
                self.draw_text_at(target, &date_str, Some(fmt_d), color(1.0, 1.0, 1.0, 1.0), date_x, 14.0);
                let (dw, _) = self.measure_text(&date_str, fmt_d);
                self.draw_text_at(
                    target,
                    &time_str,
                    self.fmt_13_bold.as_ref(),
                    color(1.0, 1.0, 1.0, 1.0),
                    date_x + dw + 6.0,
                    14.0,
                );
            }
        }
        self.draw_waveform(target, 640.0 - 68.0, 14.0, 48.0, 16.0, playing);

        // Left column: big liter number + `/ 1.5 L`:
        let total_str = crate::water::Water::format_liters(water.ml);
        let goal_str = format!("/ {} L", crate::water::Water::format_liters(water.goal_ml));
        if let Some(fmt44) = self.fmt_44_bold.as_ref() {
            self.draw_text_at(target, &total_str, Some(fmt44), color(1.0, 1.0, 1.0, 1.0), 48.0, 42.0);
            let (tw, _) = self.measure_text(&total_str, fmt44);
            self.draw_text_at(
                target,
                &goal_str,
                self.fmt_18_med.as_ref(),
                color(1.0, 1.0, 1.0, 0.50),
                48.0 + tw + 8.0,
                61.0,
            );
        }

        // Notification toggle (`x: 284, y: 54, w: 56, h: 32`):
        let on = water.notifications_enabled;
        let toggle_fill = if on {
            color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0)
        } else {
            color(1.0, 1.0, 1.0, 0.20)
        };
        self.fill_rounded(target, 284.0, 54.0, 56.0, 32.0, 16.0, toggle_fill);
        let knob_x = if on { 312.0 } else { 288.0 };
        self.fill_oval(target, knob_x, 58.0, 24.0, 24.0, color(1.0, 1.0, 1.0, 1.0));
        let bell_c = if on {
            color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0)
        } else {
            color(0.0, 0.0, 0.0, 0.45)
        };
        self.draw_icon_bell(target, knob_x + 12.0, 70.0, bell_c);

        // `"Today · {percent}%"` at `y = 100`:
        let pct = if water.goal_ml > 0 {
            ((water.ml as f64 / water.goal_ml as f64) * 100.0).round() as i64
        } else {
            0
        };
        if let (Some(f_med), Some(f_semi)) = (self.fmt_14_med.as_ref(), self.fmt_14_semi.as_ref()) {
            let mut lx = 48.0f32;
            let s1 = "Today · ";
            self.draw_text_at(target, s1, Some(f_med), color(1.0, 1.0, 1.0, 0.60), lx, 98.0);
            lx += self.measure_text(s1, f_med).0;
            let s2 = format!("{}%", pct);
            self.draw_text_at(
                target,
                &s2,
                Some(f_semi),
                color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
                lx,
                98.0,
            );
            lx += self.measure_text(&s2, f_semi).0;
            if pct >= 100 {
                self.draw_text_at(
                    target,
                    " · Goal reached",
                    Some(f_med),
                    color(1.0, 1.0, 1.0, 0.60),
                    lx,
                    98.0,
                );
            }
        }

        // Progress bar (`x: 48, y: 118, w: 292, h: 8`):
        self.fill_rounded(target, 48.0, 118.0, 292.0, 8.0, 4.0, color(1.0, 1.0, 1.0, 0.15));
        let frac = if water.goal_ml > 0 {
            (water.ml as f32 / water.goal_ml as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if frac > 0.0 {
            let fill_w = 292.0 * frac;
            let r = 4.0f32.min(fill_w * 0.5);
            self.fill_linear_gradient_rounded(
                target,
                48.0,
                118.0,
                fill_w,
                8.0,
                r,
                (48.0, 118.0),
                (48.0 + fill_w, 118.0),
                color(LIGHT_BLUE.0, LIGHT_BLUE.1, LIGHT_BLUE.2, 1.0),
                color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
            );
        }

        // Bottom pills: `Drink` (blue) + `Settings` (dark):
        self.fill_rounded(
            target,
            WATER_DRINK_RECT.0,
            WATER_DRINK_RECT.1,
            WATER_DRINK_RECT.2 - WATER_DRINK_RECT.0,
            28.0,
            14.0,
            color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
        );
        self.draw_icon_drop(target, WATER_DRINK_RECT.0 + 19.0, 146.0, color(1.0, 1.0, 1.0, 1.0));
        self.draw_text_at(
            target,
            "Drink",
            self.fmt_14_med.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            WATER_DRINK_RECT.0 + 32.0,
            136.5,
        );

        self.fill_rounded(
            target,
            WATER_SETTINGS_RECT.0,
            WATER_SETTINGS_RECT.1,
            WATER_SETTINGS_RECT.2 - WATER_SETTINGS_RECT.0,
            28.0,
            14.0,
            color(1.0, 1.0, 1.0, 0.10),
        );
        self.draw_icon_sliders(
            target,
            WATER_SETTINGS_RECT.0 + 19.0,
            146.0,
            color(1.0, 1.0, 1.0, 0.75),
        );
        self.draw_text_at(
            target,
            "Settings",
            self.fmt_14_med.as_ref(),
            color(1.0, 1.0, 1.0, 0.75),
            WATER_SETTINGS_RECT.0 + 32.0,
            136.5,
        );

        // Right column: 24-hour bar chart (`drawWaterChart`, `x: 390..592, y: 50..150`):
        let max_hour = water.hourly.iter().copied().max().unwrap_or(0);
        let scale = max_hour.max(400);
        let bars_left = 390.0f32;
        let bars_w = 202.0f32;
        let baseline = 150.0f32;
        let chart_h = 100.0f32;
        let slot_w = bars_w / 24.0;
        let label_c = color(1.0, 1.0, 1.0, 0.50);

        self.draw_right_aligned_text(
            target,
            &format!("{}", scale),
            self.fmt_10_med.as_ref(),
            label_c,
            385.0,
            baseline - chart_h - 6.0,
        );
        self.draw_right_aligned_text(
            target,
            &format!("{}", scale / 2),
            self.fmt_10_med.as_ref(),
            label_c,
            385.0,
            baseline - chart_h * 0.5 - 6.0,
        );
        self.draw_right_aligned_text(
            target,
            "0 ml",
            self.fmt_10_med.as_ref(),
            label_c,
            385.0,
            baseline - 6.0,
        );

        if let Some(b_grid) = self.brush(target, color(1.0, 1.0, 1.0, 0.15)) {
            for gy in [baseline - chart_h, baseline - chart_h * 0.5] {
                unsafe {
                    target.DrawLine(
                        D2D_POINT_2F { x: bars_left, y: gy },
                        D2D_POINT_2F {
                            x: bars_left + bars_w,
                            y: gy,
                        },
                        &b_grid,
                        1.0,
                        self.stroke_dotted.as_ref(),
                    );
                }
            }
        }
        if let Some(b_axis) = self.brush(target, color(1.0, 1.0, 1.0, 0.25)) {
            unsafe {
                target.DrawLine(
                    D2D_POINT_2F {
                        x: bars_left,
                        y: baseline,
                    },
                    D2D_POINT_2F {
                        x: bars_left + bars_w,
                        y: baseline,
                    },
                    &b_axis,
                    1.0,
                    None,
                );
            }
        }

        for (hour, &ml) in water.hourly.iter().enumerate() {
            if ml <= 0 {
                continue;
            }
            let slot_x = bars_left + (hour as f32) * slot_w;
            let bar_w = 6.0f32;
            let bar_x = slot_x + (slot_w - bar_w) * 0.5;
            let h = ((ml as f32 / scale as f32) * chart_h).clamp(3.0, chart_h);
            let r = 3.0f32.min(h * 0.5);
            self.fill_linear_gradient_rounded(
                target,
                bar_x,
                baseline - h,
                bar_w,
                h,
                r,
                (bar_x, baseline),
                (bar_x, baseline - h),
                color(LIGHT_BLUE.0, LIGHT_BLUE.1, LIGHT_BLUE.2, 1.0),
                color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
            );
        }

        if let Some(fmt10) = self.fmt_10_med.as_ref() {
            for hour in [0usize, 6, 12, 18] {
                let s = format!("{:02}", hour);
                let (w, _) = self.measure_text(&s, fmt10);
                let cx = bars_left + ((hour as f32) + 0.5) * slot_w;
                self.draw_text_at(target, &s, Some(fmt10), label_c, cx - w * 0.5, 154.0);
            }
        }
    }

    // --- Tray Section (`drawTray` & `drawTrayDropZone`, `640x280`, `IslandView.swift` lines 4014-4267) ---

    fn draw_tray_drop_zone(
        &self,
        target: &ID2D1RenderTarget,
        shelf: &crate::shelf::Shelf,
        tray_page: usize,
        playing: bool,
        tray_drag_pt: Option<(f32, f32)>,
    ) {
        self.draw_card_header(target, 640.0, playing);
        self.draw_text_at(
            target,
            "Tray",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            28.0,
            10.0,
        );

        if !shelf.files.is_empty() {
            self.fill_rounded(
                target,
                TRAY_CLEAR_RECT.0,
                TRAY_CLEAR_RECT.1,
                TRAY_CLEAR_RECT.2 - TRAY_CLEAR_RECT.0,
                28.0,
                14.0,
                color(1.0, 1.0, 1.0, 0.10),
            );
            if let Some(fmt14) = self.fmt_14_reg.as_ref() {
                let (cw, _) = self.measure_text("Clear", fmt14);
                let mid = (TRAY_CLEAR_RECT.0 + TRAY_CLEAR_RECT.2) * 0.5;
                self.draw_text_at(
                    target,
                    "Clear",
                    Some(fmt14),
                    color(1.0, 1.0, 1.0, 1.0),
                    mid - cw * 0.5,
                    14.5,
                );
            }
        }

        // Drop zone (`NSRect(x: 28, y: 44, width: 584, height: 212)`, `cornerRadius: 20`):
        // Radial cursor-following light glow (`startRadius: 0, endRadius: 160`, center `white @ 0.25` -> `white @ 0.0`)
        // clipped to the rounded drop zone:
        if let Some((gx, gy)) = tray_drag_pt {
            unsafe {
                let stops = [
                    D2D1_GRADIENT_STOP {
                        position: 0.0,
                        color: color(1.0, 1.0, 1.0, 0.25),
                    },
                    D2D1_GRADIENT_STOP {
                        position: 1.0,
                        color: color(1.0, 1.0, 1.0, 0.0),
                    },
                ];
                if let Ok(stop_col) = target.CreateGradientStopCollection(
                    &stops,
                    D2D1_GAMMA_2_2,
                    D2D1_EXTEND_MODE_CLAMP,
                ) {
                    let rprops = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                        center: D2D_POINT_2F { x: gx, y: gy },
                        gradientOriginOffset: D2D_POINT_2F { x: 0.0, y: 0.0 },
                        radiusX: 160.0,
                        radiusY: 160.0,
                    };
                    if let Ok(rbrush) = target.CreateRadialGradientBrush(&rprops, None, &stop_col) {
                        target.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: 28.0,
                                    top: 44.0,
                                    right: 612.0,
                                    bottom: 256.0,
                                },
                                radiusX: 20.0,
                                radiusY: 20.0,
                            },
                            &rbrush,
                        );
                    }
                }
            }
        }

        self.stroke_rounded(
            target,
            28.0,
            44.0,
            584.0,
            212.0,
            20.0,
            1.5,
            color(1.0, 1.0, 1.0, 0.50),
            self.stroke_dashed.as_ref(),
        );
        self.draw_centered_text(
            target,
            "Release to add",
            self.fmt_14_reg.as_ref(),
            color(1.0, 1.0, 1.0, 0.70),
            640.0,
            143.0,
        );

        let page_count = shelf.page_count(TRAY_PAGE_SIZE);
        let page = tray_page.min(page_count.saturating_sub(1));
        if page_count > 1 {
            let start_x = (640.0 - (page_count as f32) * 18.0) * 0.5;
            for i in 0..page_count {
                let slot_cx = start_x + (i as f32) * 18.0 + 9.0;
                if i == page {
                    self.fill_rounded(
                        target,
                        slot_cx - 6.0,
                        265.0,
                        12.0,
                        6.0,
                        3.0,
                        color(1.0, 1.0, 1.0, 1.0),
                    );
                } else {
                    self.fill_oval(
                        target,
                        slot_cx - 3.0,
                        265.0,
                        6.0,
                        6.0,
                        color(1.0, 1.0, 1.0, 0.40),
                    );
                }
            }
        }

        // Cursor-following `"+ Copy"` pill badge (`drawDragCopyBadge`, `IslandView.swift` lines 2709-2736):
        if let Some((gx, gy)) = tray_drag_pt {
            if let Some(fmt) = self.fmt_13_bold.as_ref() {
                let label = "+ Copy";
                let (tw, th) = self.measure_text(label, fmt);
                let pad_x = 8.0f32;
                let badge_w = tw + pad_x * 2.0;
                let badge_h = 22.0f32;
                let bx = (gx + 14.0).clamp(8.0, 640.0 - badge_w - 8.0);
                let by = (gy + 6.0).clamp(8.0, 280.0 - badge_h - 8.0);
                self.fill_rounded(
                    target,
                    bx,
                    by,
                    badge_w,
                    badge_h,
                    badge_h * 0.5,
                    color(1.0, 1.0, 1.0, 1.0),
                );
                self.draw_text_at(
                    target,
                    label,
                    Some(fmt),
                    color(0.0, 0.0, 0.0, 1.0),
                    bx + pad_x,
                    by + (badge_h - th) * 0.5 - 0.5,
                );
            }
        }
    }

    fn draw_tray(
        &self,
        target: &ID2D1RenderTarget,
        shelf: &crate::shelf::Shelf,
        tray_page: usize,
        playing: bool,
        tray_drop_highlight: bool,
        tray_drag_pt: Option<(f32, f32)>,
    ) {
        if tray_drop_highlight {
            self.draw_tray_drop_zone(target, shelf, tray_page, playing, tray_drag_pt);
            return;
        }
        self.draw_card_header(target, 640.0, playing);
        self.draw_text_at(
            target,
            "Tray",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            28.0,
            10.0,
        );

        if shelf.files.is_empty() {
            self.stroke_rounded(
                target,
                28.0,
                44.0,
                584.0,
                212.0,
                20.0,
                1.5,
                color(1.0, 1.0, 1.0, 0.50),
                self.stroke_dashed.as_ref(),
            );
            self.draw_icon_doc(target, 320.0, 165.0, 48.0, color(1.0, 1.0, 1.0, 0.60));
            self.draw_centered_text(
                target,
                "Drag files here",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.60),
                640.0,
                115.0,
            );
            return;
        }

        // `"Clear"` pill right after `"Tray"`:
        self.fill_rounded(
            target,
            TRAY_CLEAR_RECT.0,
            TRAY_CLEAR_RECT.1,
            TRAY_CLEAR_RECT.2 - TRAY_CLEAR_RECT.0,
            28.0,
            14.0,
            color(1.0, 1.0, 1.0, 0.10),
        );
        if let Some(fmt14) = self.fmt_14_reg.as_ref() {
            let (cw, _) = self.measure_text("Clear", fmt14);
            let mid = (TRAY_CLEAR_RECT.0 + TRAY_CLEAR_RECT.2) * 0.5;
            self.draw_text_at(
                target,
                "Clear",
                Some(fmt14),
                color(1.0, 1.0, 1.0, 1.0),
                mid - cw * 0.5,
                14.5,
            );
        }

        let page_count = shelf.page_count(TRAY_PAGE_SIZE);
        let page = tray_page.min(page_count.saturating_sub(1));
        let start = page * TRAY_PAGE_SIZE;
        let (gx, gy, tw, th, px, py) = (
            TRAY_TILE_GEOM.0,
            TRAY_TILE_GEOM.1,
            TRAY_TILE_GEOM.2,
            TRAY_TILE_GEOM.3,
            TRAY_TILE_GEOM.4,
            TRAY_TILE_GEOM.5,
        );

        for (i, path) in shelf
            .files
            .iter()
            .skip(start)
            .take(TRAY_PAGE_SIZE)
            .enumerate()
        {
            let col = (i % 4) as f32;
            let row = (i / 4) as f32;
            let left = gx + col * px;
            let top = gy + row * py;
            let cx = left + tw * 0.5;
            let info = crate::shelf::file_info(path);
            let alpha = if info.exists { 1.0 } else { 0.40 };

            self.fill_rounded(target, left, top, tw, th, 10.0, color(1.0, 1.0, 1.0, 0.08));
            if info.is_dir {
                self.draw_icon_folder(target, cx, top + 28.0, 36.0, color(1.0, 1.0, 1.0, alpha));
            } else {
                self.draw_icon_doc(target, cx, top + 28.0, 36.0, color(1.0, 1.0, 1.0, alpha));
            }

            if let Some(fmt13) = self.fmt_13_reg.as_ref() {
                let shown = self.truncate_text(&info.name, tw - 16.0, fmt13);
                let (nw, _) = self.measure_text(&shown, fmt13);
                self.draw_text_at(
                    target,
                    &shown,
                    Some(fmt13),
                    color(1.0, 1.0, 1.0, alpha),
                    cx - nw * 0.5,
                    top + 60.0,
                );
            }
            if let Some(fmt11) = self.fmt_11_reg.as_ref() {
                let (sw, _) = self.measure_text(&info.size_text, fmt11);
                self.draw_text_at(
                    target,
                    &info.size_text,
                    Some(fmt11),
                    color(1.0, 1.0, 1.0, 0.60),
                    cx - sw * 0.5,
                    top + 80.0,
                );
            }

            // Delete `"×"` at top-right of tile:
            self.draw_text_at(
                target,
                "×",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                left + tw - 16.0,
                top + 2.0,
            );
        }

        // Pagination arrows + bottom page dots when `page_count > 1`:
        if page_count > 1 {
            self.fill_oval(target, 30.0, 132.0, 36.0, 36.0, color(1.0, 1.0, 1.0, 0.15));
            self.fill_oval(target, 574.0, 132.0, 36.0, 36.0, color(1.0, 1.0, 1.0, 0.15));
            self.draw_icon_chevron(target, 48.0, 150.0, false, color(1.0, 1.0, 1.0, 1.0));
            self.draw_icon_chevron(target, 592.0, 150.0, true, color(1.0, 1.0, 1.0, 1.0));

            let start_x = (640.0 - (page_count as f32) * 18.0) * 0.5;
            for i in 0..page_count {
                let slot_cx = start_x + (i as f32) * 18.0 + 9.0;
                if i == page {
                    self.fill_rounded(target, slot_cx - 6.0, 265.0, 12.0, 6.0, 3.0, color(1.0, 1.0, 1.0, 1.0));
                } else {
                    self.fill_oval(target, slot_cx - 3.0, 265.0, 6.0, 6.0, color(1.0, 1.0, 1.0, 0.40));
                }
            }
        }
    }

    fn fill_linear_gradient_rounded(
        &self,
        target: &ID2D1RenderTarget,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        start: (f32, f32),
        end: (f32, f32),
        c0: D2D1_COLOR_F,
        c1: D2D1_COLOR_F,
    ) {
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: c0,
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: c1,
                },
            ];
            let Ok(stop_col) = target.CreateGradientStopCollection(
                &stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            ) else {
                self.fill_rounded(target, x, y, w, h, r, c1);
                return;
            };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: D2D_POINT_2F {
                    x: start.0,
                    y: start.1,
                },
                endPoint: D2D_POINT_2F { x: end.0, y: end.1 },
            };
            let Ok(grad_brush) = target.CreateLinearGradientBrush(&props, None, &stop_col) else {
                self.fill_rounded(target, x, y, w, h, r, c1);
                return;
            };
            target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                    },
                    radiusX: r,
                    radiusY: r,
                },
                &grad_brush,
            );
        }
    }

    // --- Main Draw Entrypoint ---

    pub fn draw(&mut self, hwnd: HWND, ctx: &RenderContext) {
        let scale = window::dpi_scale(hwnd);
        let canvas_w_px = (window::CANVAS_W * scale).round() as i32;
        let canvas_h_px = (window::CANVAS_H * scale).round() as i32;
        if !self.ensure_target(canvas_w_px, canvas_h_px, scale * 96.0) {
            unsafe {
                let _ = ValidateRect(hwnd, None);
            }
            return;
        }
        self.update_cover_bitmap(ctx.media);
        let (lw, lh) = (ctx.cur_w, ctx.cur_h);
        let lyrics_t = if ctx.state == IslandState::Expanded || ctx.state == IslandState::Lyrics {
            ((lw - 520.0) / (700.0 - 520.0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if ctx.state == IslandState::Lyrics || lyrics_t > 0.05 {
            self.update_lyric_roll(ctx.media, ctx.lyrics);
        }

        let target = self.target.as_ref().unwrap().clone();
        unsafe {
            target.BeginDraw();
            target.SetTransform(&Matrix3x2::identity());
            target.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));

            // Apply `tray_spring_scale` (`1.03` on drag hover, `1.12 -> 1.0` damped cosine bounce on drop)
            // anchored at the top-center bezel `(window::CANVAS_W * 0.5, 0.0)`:
            let s = if ctx.state == IslandState::Tray {
                ctx.tray_spring_scale
            } else {
                1.0
            };
            let tx = window::CANVAS_W * 0.5 - (lw * 0.5) * s;
            let base_xform = Matrix3x2 {
                M11: s,
                M12: 0.0,
                M21: 0.0,
                M22: s,
                DX: tx,
                DY: 0.0,
            };
            target.SetTransform(&base_xform);

            // Smoothly interpolate bottom corner radius between pill (`lh * 0.5`) and expanded card (`28.0`):
            let expand_t = ((lh - 32.0) / (180.0 - 32.0)).clamp(0.0, 1.0);
            let radius = (lh * 0.5) * (1.0 - expand_t) + 28.0 * expand_t;

            let grad_alpha = if !ctx.state.is_expanded() && ctx.lyrics_mode && ctx.alert.is_none() {
                0.98
            } else {
                0.98 * lyrics_t
            };

            let geometry_opt = self.nusa_geometry(lw, lh, radius);
            if let Some(geometry) = &geometry_opt {
                if let Some(b_black) = self.brush(&target, color(0.0, 0.0, 0.0, 0.98)) {
                    target.FillGeometry(geometry, &b_black, None);
                }
                if grad_alpha > 0.01 {
                    if let Some((dr, dg, db)) = self.dominant_rgb {
                        let stops = [
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: color(0.0, 0.0, 0.0, grad_alpha),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 1.0,
                                color: color(dr, dg, db, grad_alpha),
                            },
                        ];
                        if let Ok(stop_col) = target.CreateGradientStopCollection(
                            &stops,
                            D2D1_GAMMA_2_2,
                            D2D1_EXTEND_MODE_CLAMP,
                        ) {
                            let gprops = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                                startPoint: D2D_POINT_2F { x: 0.0, y: 0.0 },
                                endPoint: D2D_POINT_2F { x: 0.0, y: lh },
                            };
                            if let Ok(gbrush) =
                                target.CreateLinearGradientBrush(&gprops, None, &stop_col)
                            {
                                target.FillGeometry(geometry, &gbrush, None);
                            }
                        }
                    }
                }
            }

            // Only push a geometric mask layer when expanding/collapsing vertically (`lh < 175.0`).
            // Horizontal `520 <-> 700` morphs use hardware `PushAxisAlignedClip` in `draw_music_card`
            // and `draw_lyrics_panel` so spam-toggling lyrics never incurs software mask rasterization.
            let mut layer_pushed = false;
            if lh > 36.0 && lh < 175.0 {
                if let Some(geometry) = &geometry_opt {
                    let layer_params = D2D1_LAYER_PARAMETERS {
                        contentBounds: D2D_RECT_F {
                            left: 0.0,
                            top: 0.0,
                            right: lw,
                            bottom: lh,
                        },
                        geometricMask: ManuallyDrop::new(Some(geometry.clone())),
                        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                        maskTransform: Matrix3x2::identity(),
                        opacity: 1.0,
                        opacityBrush: ManuallyDrop::new(None),
                        layerOptions: D2D1_LAYER_OPTIONS_NONE,
                    };
                    target.PushLayer(&layer_params, None);
                    layer_pushed = true;
                }
            }

            let playing = ctx.media.map(|m| m.playing).unwrap_or(false);

            if !ctx.state.is_expanded() && ctx.alert.is_some() {
                self.draw_pill_alert(&target, lw, lh, ctx.alert.unwrap());
            } else {
                match ctx.state {
                    IslandState::Zen => {
                        self.draw_eyes(&target, lw, lh, ctx.eye_look, ctx.eyes_closed);
                    }
                    IslandState::Compact => {
                        self.draw_closed(&target, ctx);
                    }
                    IslandState::Expanded | IslandState::Lyrics => {
                        if let Some(m) = ctx.media {
                            self.draw_music_card(&target, m, lyrics_t);
                            let panel_alpha = ((lyrics_t - 0.12) / 0.88).clamp(0.0, 1.0);
                            if panel_alpha > 0.01 {
                                self.draw_lyrics_panel(&target, Some(m), ctx.lyrics, panel_alpha, lw);
                            }
                        } else {
                            self.draw_centered_text(
                                &target,
                                "No media playing",
                                self.fmt_13_reg.as_ref(),
                                color(1.0, 1.0, 1.0, 0.70),
                                lw,
                                lh * 0.5 - 9.0,
                            );
                        }
                        self.draw_page_dots(&target, ctx.state, lw, lh);
                    }
                    IslandState::Pomodoro => {
                        self.draw_pomodoro(&target, ctx.pomodoro, playing);
                        self.draw_page_dots(&target, ctx.state, lw, lh);
                    }
                    IslandState::Todo => {
                        self.draw_todo(&target, ctx.todo, ctx.todo_editing, ctx.todo_input, playing);
                        self.draw_page_dots(&target, ctx.state, lw, lh);
                    }
                    IslandState::Water => {
                        self.draw_water(&target, ctx.water, playing);
                        self.draw_page_dots(&target, ctx.state, lw, lh);
                    }
                    IslandState::Tray => {
                        self.draw_tray(
                            &target,
                            ctx.shelf,
                            ctx.tray_page,
                            playing,
                            ctx.tray_drop_highlight,
                            ctx.tray_drag_pt,
                        );
                        self.draw_page_dots(&target, ctx.state, lw, lh);
                    }
                }
            }

            if layer_pushed {
                target.PopLayer();
            }

            if let Some(geometry) = &geometry_opt {
                if let Some(b_border) = self.brush(&target, color(1.0, 1.0, 1.0, 0.15)) {
                    target.DrawGeometry(geometry, &b_border, 0.75, None);
                }
            }

            target.SetTransform(&Matrix3x2::identity());
            let _ = target.EndDraw(None, None);

            let size = SIZE {
                cx: self.canvas_w_px,
                cy: self.canvas_h_px,
            };
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
                self.mem_dc,
                Some(&pt_src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            let _ = ValidateRect(hwnd, None);
        }
    }

    fn nusa_geometry(&self, lw: f32, lh: f32, radius: f32) -> Option<ID2D1Geometry> {
        unsafe {
            let path: ID2D1PathGeometry = self.factory.CreatePathGeometry().ok()?;
            let sink: ID2D1GeometrySink = path.Open().ok()?;
            let r = radius.clamp(0.0, lh / 2.0);
            let t = 12.0f32.min(lw / 2.0).min(lh / 2.0);
            let mut pts: Vec<D2D_POINT_2F> = Vec::new();
            pts.push(D2D_POINT_2F { x: 0.0, y: 0.0 });
            pts.push(D2D_POINT_2F { x: lw, y: 0.0 });
            let seg = 20i32;
            for i in 1..seg {
                let a = -std::f32::consts::FRAC_PI_2 * (1.0 + i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: lw + t * a.cos(),
                    y: t + t * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F { x: lw - t, y: t });
            pts.push(D2D_POINT_2F {
                x: lw - t,
                y: lh - r,
            });
            for i in 1..seg {
                let a = std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: (lw - t - r) + r * a.cos(),
                    y: (lh - r) + r * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F {
                x: lw - t - r,
                y: lh,
            });
            pts.push(D2D_POINT_2F { x: t + r, y: lh });
            for i in 1..seg {
                let a = std::f32::consts::FRAC_PI_2 * (1.0 + i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: (t + r) + r * a.cos(),
                    y: (lh - r) + r * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F { x: t, y: lh - r });
            pts.push(D2D_POINT_2F { x: t, y: t });
            for i in 1..seg {
                let a = -std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: t * a.cos(),
                    y: t + t * a.sin(),
                });
            }
            sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&pts[1..]);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            path.cast().ok()
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            self.cover_bitmap = None;
            self.target = None;
            self.dc_target = None;
            if !self.mem_dc.0.is_null() {
                if !self.old_bmp.0.is_null() {
                    let _ = SelectObject(self.mem_dc, self.old_bmp);
                }
                if !self.dib_bmp.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(self.dib_bmp.0));
                }
                let _ = DeleteDC(self.mem_dc);
            }
        }
    }
}
