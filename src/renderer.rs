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
use windows::core::PCWSTR;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, ValidateRect,
    GetDC, ReleaseDC, GetDIBits,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use crate::state::IslandState;
use crate::window;

// --- Interactive hit-test constants (exact `IslandView.swift` coordinates) ---

/// Speech bubble button on Music (`520x180`) and Lyrics (`700x180`) cards: `NSRect(x: 42, y: 126, width: 28, height: 28)`.
pub const MUSIC_BUBBLE_RECT: (f32, f32, f32, f32) = (68.0, 126.0, 100.0, 154.0);
/// Transport control X centers on Music card (`[208, 260, 312]`, `y: 126..154`, `half_w = 20`).
pub const MUSIC_CONTROL_CENTERS: [f32; 3] = [224.0, 270.0, 316.0];
/// Transport control X centers on Lyrics card (`[118, 170, 222]`, `y: 126..154`, `half_w = 20`).
pub const LYRICS_CONTROL_CENTERS: [f32; 3] = [170.0, 214.0, 258.0];
pub const MUSIC_CONTROLS_Y: (f32, f32) = (126.0, 154.0);
pub const MUSIC_SPEAKER_RECT: (f32, f32, f32, f32) = (454.0, 126.0, 490.0, 154.0);
pub const MUSIC_VOLUME_BAR_RECT: (f32, f32, f32, f32) = (358.0, 126.0, 452.0, 154.0);

/// Pomodoro (`520x180`) interactive rects (`pomodoroButtonRects`).
pub const POMODORO_RING_CX: f32 = 94.0;
pub const POMODORO_RING_CY: f32 = 104.0;
pub const POMODORO_RING_R: f32 = 32.0;
pub const POMODORO_COUNTDOWN_X: f32 = 156.0;
pub const POMODORO_COUNTDOWN_Y: f32 = 42.0;
pub const POMODORO_SUBTITLE_Y: f32 = 96.0;
pub const POMODORO_RESET_RECT: (f32, f32, f32, f32) = (304.0, 48.0, 332.0, 76.0);
pub const POMODORO_SHORT_BREAK_RECT: (f32, f32, f32, f32) = (156.0, 122.0, 264.0, 150.0);
pub const POMODORO_PLAY_RECT: (f32, f32, f32, f32) = (272.0, 122.0, 326.0, 150.0);
pub const POMODORO_FOCUS_RECT: (f32, f32, f32, f32) = (334.0, 122.0, 442.0, 150.0);

/// Todo (`520x180`) interactive rects (`todoAddPillRect`, `todoInputRect`, `todoVisibleRows`).
pub const TODO_ADD_PILL_RECT: (f32, f32, f32, f32) = (46.0, 120.0, 150.0, 148.0);
pub const TODO_INPUT_RECT: (f32, f32, f32, f32) = (46.0, 116.0, 150.0, 148.0);
pub const TODO_ROW_X: f32 = 180.0;
pub const TODO_ROW_W: f32 = 306.0;
pub const TODO_ROW_TOGGLE_RIGHT: f32 = 432.0;
pub const TODO_ROW_EDIT_LEFT: f32 = 436.0;
pub const TODO_ROW_EDIT_RIGHT: f32 = 460.0;
pub const TODO_ROW_DELETE_LEFT: f32 = 462.0;
pub const TODO_ROW_DELETE_RIGHT: f32 = 486.0;
pub const TODO_ROW_Y0: f32 = 38.0;
pub const TODO_ROW_H: f32 = 32.0;
pub const TODO_ROW_PITCH: f32 = 38.0;

/// Water (`520x180`) interactive rects (`waterButtonRects`).
pub const WATER_TOGGLE_RECT: (f32, f32, f32, f32) = (188.0, 52.0, 238.0, 80.0);
pub const WATER_DRINK_RECT: (f32, f32, f32, f32) = (54.0, 120.0, 142.0, 150.0);
pub const WATER_SETTINGS_RECT: (f32, f32, f32, f32) = (150.0, 120.0, 244.0, 150.0);

/// Tray (`520x180`) horizontal carousel geometry:

pub fn compute_tray_button_rects(hovers: &[f32; 4]) -> [(f32, f32, f32, f32); 4] {
    let base_w = 30.0f32;
    let extra_ws = [44.0f32, 46.0f32, 82.0f32, 64.0f32];
    let gap = 8.0f32;
    let y0 = 24.0f32;
    let y1 = 54.0f32;
    let mut x = 116.0f32;
    let mut rects = [(0.0, 0.0, 0.0, 0.0); 4];
    for i in 0..4 {
        let w = base_w + extra_ws[i] * hovers[i];
        rects[i] = (x, y0, x + w, y1);
        x += w + gap;
    }
    rects
}

pub fn compute_rb_button_rects(hovers: &[f32; 2], model_len: usize) -> [(f32, f32, f32, f32); 2] {
    let base_w = 30.0f32;
    let extra_ws = [50.0f32 + (model_len as f32) * 6.0, 44.0f32];
    let gap = 8.0f32;
    let y0 = 24.0f32;
    let y1 = 54.0f32;
    let mut x = 340.0f32;
    let mut rects = [(0.0, 0.0, 0.0, 0.0); 2];
    for i in 0..2 {
        let w = base_w + extra_ws[i] * hovers[i];
        rects[i] = (x, y0, x + w, y1);
        x += w + gap;
    }
    rects
}

pub fn compute_cv_button_rects(hovers: &[f32; 3], fmt_len: usize, has_format_btn: bool) -> [(f32, f32, f32, f32); 3] {
    let base_w = 30.0f32;
    let y0 = 24.0f32;
    let y1 = 54.0f32;

    // Button 0 (Back): at left (x = 46.0)
    let w0 = base_w + 42.0 * hovers[0];
    let r0 = (46.0, y0, 46.0 + w0, y1);

    if has_format_btn {
        // Button 1 (Format): at right
        let w1 = base_w + (40.0 + (fmt_len as f32) * 7.0) * hovers[1];
        let x1 = 360.0f32;
        let r1 = (x1, y0, x1 + w1, y1);

        // Button 2 (Reset): after Button 1
        let w2 = base_w + 44.0 * hovers[2];
        let x2 = x1 + w1 + 8.0;
        let r2 = (x2, y0, x2 + w2, y1);

        [r0, r1, r2]
    } else {
        // No Format button: Button 1 is dummy/empty
        let r1 = (0.0, 0.0, 0.0, 0.0);

        // Button 2 (Reset): placed stably on the right edge, expanding to the left without shifting
        let w2 = base_w + 44.0 * hovers[2];
        let x2 = 450.0 - 44.0 * hovers[2];
        let r2 = (x2, y0, x2 + w2, y1);

        [r0, r1, r2]
    }
}

#[allow(dead_code)]
pub const TRAY_CLEAR_RECT: (f32, f32, f32, f32) = (90.0, 14.0, 146.0, 42.0);
#[allow(dead_code)]
pub const TRAY_SELECT_RECT: (f32, f32, f32, f32) = (152.0, 14.0, 224.0, 42.0);
#[allow(dead_code)]
pub const TRAY_TO_REMOVEBG_RECT: (f32, f32, f32, f32) = (230.0, 14.0, 334.0, 42.0);
#[allow(dead_code)]
pub const TRAY_TO_CONVERT_RECT: (f32, f32, f32, f32) = (340.0, 14.0, 428.0, 42.0);

pub const TRAY_CAROUSEL_X: f32 = 54.0;
pub const TRAY_CAROUSEL_Y: f32 = 66.0;
pub const TRAY_CAROUSEL_W: f32 = 432.0;
pub const TRAY_CAROUSEL_H: f32 = 88.0;
pub const TRAY_TILE_W: f32 = 80.0;
pub const TRAY_TILE_H: f32 = 88.0;
pub const TRAY_TILE_PITCH: f32 = 90.0;

/// Remove BG card geometry (`540x184`):
pub const RB_QUEUE_PREV_RECT: (f32, f32, f32, f32) = (236.0, 24.0, 264.0, 52.0);
pub const RB_QUEUE_NEXT_RECT: (f32, f32, f32, f32) = (300.0, 24.0, 328.0, 52.0);
#[allow(dead_code)]
pub const RB_MODEL_RECT: (f32, f32, f32, f32) = (340.0, 24.0, 420.0, 54.0);
#[allow(dead_code)]
pub const RB_RESET_BTN_RECT: (f32, f32, f32, f32) = (428.0, 24.0, 458.0, 54.0);

pub const RB_DROP_ZONE_RECT: (f32, f32, f32, f32) = (54.0, 58.0, 486.0, 156.0);
pub const RB_LEFT_PREVIEW_RECT: (f32, f32, f32, f32) = (54.0, 58.0, 266.0, 156.0);
pub const RB_RIGHT_PREVIEW_RECT: (f32, f32, f32, f32) = (274.0, 58.0, 486.0, 156.0);
pub const RB_PROCESS_BTN_RECT: (f32, f32, f32, f32) = (448.0, 116.0, 480.0, 148.0);
pub const RB_SAVE_BTN_RECT: (f32, f32, f32, f32) = (448.0, 116.0, 480.0, 148.0);

/// Convert card geometry (`540x184`):
#[allow(dead_code)]
pub const CONVERT_APP_ICON_SIZE: f32 = 36.0;
pub const CONVERT_APP_CARD_W: f32 = 132.0;
pub const CONVERT_APP_CARD_H: f32 = 92.0;
pub const CONVERT_APP_START_X: f32 = 54.0;
pub const CONVERT_APP_Y: f32 = 58.0;
pub const CONVERT_APP_PITCH: f32 = 150.0;

#[allow(dead_code)]
pub const CONVERT_BACK_BTN_RECT: (f32, f32, f32, f32) = (46.0, 24.0, 76.0, 54.0);
pub const CONVERT_QUEUE_PREV_RECT: (f32, f32, f32, f32) = (236.0, 24.0, 264.0, 52.0);
pub const CONVERT_QUEUE_NEXT_RECT: (f32, f32, f32, f32) = (300.0, 24.0, 328.0, 52.0);
#[allow(dead_code)]
pub const CONVERT_FORMAT_PICKER_RECT: (f32, f32, f32, f32) = (360.0, 24.0, 424.0, 54.0);
#[allow(dead_code)]
pub const CONVERT_RESET_BTN_RECT: (f32, f32, f32, f32) = (432.0, 24.0, 462.0, 54.0);

pub const CONVERT_DROP_ZONE_RECT: (f32, f32, f32, f32) = (54.0, 58.0, 486.0, 110.0);
pub const CONVERT_LIST_VIEW_RECT: (f32, f32, f32, f32) = (54.0, 118.0, 486.0, 162.0);
pub const CONVERT_PROCESS_BTN_RECT: (f32, f32, f32, f32) = (448.0, 124.0, 480.0, 156.0);
pub const CONVERT_SAVE_BTN_RECT: (f32, f32, f32, f32) = (448.0, 124.0, 480.0, 156.0);

pub const MERGE_PDF_ADD_BTN: (f32, f32, f32, f32) = (330.0, 24.0, 420.0, 52.0);
pub const MERGE_PDF_RUN_BTN: (f32, f32, f32, f32) = (340.0, 134.0, 486.0, 166.0);

pub const PDF_MD_PORTAL_RECT: (f32, f32, f32, f32) = (368.0, 58.0, 486.0, 156.0);
pub const PDF_MD_CARD_START_X: f32 = 54.0;
pub const PDF_MD_CARD_Y: f32 = 58.0;
pub const PDF_MD_CARD_W: f32 = 78.0;
pub const PDF_MD_CARD_H: f32 = 98.0;
pub const PDF_MD_CARD_PITCH: f32 = 88.0;

pub const SETTINGS_CLOSE_BTN: (f32, f32, f32, f32) = (484.0, 22.0, 516.0, 50.0);

pub fn dot_index_at(x: f32, y: f32, lw: f32, lh: f32) -> Option<usize> {
    let count = crate::state::SECTION_COUNT;
    // Bottom horizontal dots: y is within [lh - 26.0, lh - 2.0]
    if y < lh - 26.0 || y > lh - 2.0 {
        return None;
    }
    let spacing = 16.0f32;
    let total_w = (count as f32 - 1.0) * spacing;
    let start_x = (lw - total_w) * 0.5;
    if x < start_x - 12.0 || x > start_x + total_w + 12.0 {
        return None;
    }
    let idx = ((x - (start_x - spacing * 0.5)) / spacing).floor() as isize;
    if idx >= 0 && (idx as usize) < count {
        Some(idx as usize)
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
    #[allow(dead_code)]
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
    pub removebg: &'a crate::removebg::RemoveBgState,
    pub removebg_drop_highlight: bool,
    #[allow(dead_code)]
    pub tray_page: usize,
    pub tray_drop_highlight: bool,
    pub tray_drag_pt: Option<(f32, f32)>,
    pub tray_spring_scale: f32,
    pub cur_w: f32,
    pub cur_h: f32,
    pub alert: Option<&'a PillAlert>,
    pub eye_openness: [f32; 2],
    pub eye_slant: [f32; 2],
    pub convert: &'a crate::convert::ConvertState,
    pub music_button_zooms: [f32; 4],
    pub music_bar_hover: bool,
    pub music_seek_pos: Option<f64>,
    pub master_volume: f32,
    #[allow(dead_code)]
    pub volume_bar_hover: bool,
    pub volume_bar_zoom: f32,
    #[allow(dead_code)]
    pub card_expanded: bool,
    pub todo_scroll: f32,
    pub todo_editing_idx: Option<usize>,
    pub tray_scroll: f32,
    pub transition_active: bool,
    pub transition_progress: f32,
    pub transition_dir: f32,
    #[allow(dead_code)]
    pub transition_from: IslandState,
    #[allow(dead_code)]
    pub transition_to: IslandState,
    pub dot_indicator_bounds: (f32, f32),
    pub hovered_dot: Option<usize>,
    pub music_bar_zoom: f32,
    pub tray_genie_progress: Option<f32>,
    pub mouse_pt: Option<(f32, f32)>,
    pub header_active: bool,
    pub tray_btn_hovers: [f32; 4],
    pub rb_btn_hovers: [f32; 2],
    pub cv_btn_hovers: [f32; 3],
    pub settings: &'a crate::settings::AuraSettings,
    pub pdf_md_drag: Option<(usize, f32, f32)>,
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
    removebg_in_bitmap: Option<ID2D1Bitmap>,
    removebg_in_key: (String, usize),
    removebg_bitmap: Option<ID2D1Bitmap>,
    removebg_gen: u64,
    convert_in_bitmap: Option<ID2D1Bitmap>,
    convert_in_key: (String, usize),
    convert_bitmap: Option<ID2D1Bitmap>,
    convert_gen: u64,
    convert_app_bitmaps: [Option<ID2D1Bitmap>; 3],
    dot_icons: [Option<ID2D1Bitmap>; 7],
    tray_thumb_cache: std::collections::HashMap<String, ID2D1Bitmap>,
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
                removebg_in_bitmap: None,
                removebg_in_key: (String::new(), 0),
                removebg_bitmap: None,
                removebg_gen: 0,
                convert_in_bitmap: None,
                convert_in_key: (String::new(), 0),
                convert_bitmap: None,
                convert_gen: 0,
                convert_app_bitmaps: [None, None, None],
                dot_icons: [None, None, None, None, None, None, None],
                tray_thumb_cache: std::collections::HashMap::new(),
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

    /// Immediately cancels any active lyric rolling animation.
    pub fn cancel_lyric_roll(&mut self) {
        self.lyric_roll = None;
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
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
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
            if dpi > 0.0 {
                target.SetDpi(dpi, dpi);
            }
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

    fn bitmap_from_pixels(
        &self,
        target: &ID2D1RenderTarget,
        pixels: &[u8],
        w: u32,
        h: u32,
    ) -> Option<ID2D1Bitmap> {
        unsafe {
            target
                .CreateBitmap(
                    D2D_SIZE_U {
                        width: w,
                        height: h,
                    },
                    Some(pixels.as_ptr() as *const core::ffi::c_void),
                    w * 4,
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

    /// Refresh cached Remove BG bitmaps from the current section state
    /// (mirror of `update_cover_bitmap`): input keyed by path+length,
    /// result keyed by run generation.
    fn update_removebg_bitmaps(&mut self, rb: &crate::removebg::RemoveBgState) {
        let Some(target) = self.target.clone() else {
            return;
        };
        match &rb.input_preview {
            Some((pixels, w, h)) => {
                let key = (rb.input_path.clone().unwrap_or_default(), pixels.len());
                if self.removebg_in_key != key || self.removebg_in_bitmap.is_none() {
                    if let Some(bmp) = self.bitmap_from_pixels(&target, pixels, *w, *h) {
                        self.removebg_in_bitmap = Some(bmp);
                        self.removebg_in_key = key;
                    }
                }
            }
            None => {
                self.removebg_in_bitmap = None;
                self.removebg_in_key = (String::new(), 0);
            }
        }
        match &rb.result_preview {
            Some((pixels, w, h)) => {
                if self.removebg_gen != rb.run_gen || self.removebg_bitmap.is_none() {
                    if let Some(bmp) = self.bitmap_from_pixels(&target, pixels, *w, *h) {
                        self.removebg_bitmap = Some(bmp);
                        self.removebg_gen = rb.run_gen;
                    }
                }
            }
            None => {
                self.removebg_bitmap = None;
            }
        }
    }

    fn update_convert_bitmaps(&mut self, cv: &crate::convert::ConvertState) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let st = cv.tool_state(crate::convert::ConvertTool::Photo);
        match &st.input_preview {
            Some((pixels, w, h)) => {
                let key = (st.input_path.clone().unwrap_or_default(), pixels.len());
                if self.convert_in_key != key || self.convert_in_bitmap.is_none() {
                    if let Some(bmp) = self.bitmap_from_pixels(&target, pixels, *w, *h) {
                        self.convert_in_bitmap = Some(bmp);
                        self.convert_in_key = key;
                    }
                }
            }
            None => {
                self.convert_in_bitmap = None;
                self.convert_in_key = (String::new(), 0);
            }
        }
        match &st.result_preview {
            Some((pixels, w, h)) => {
                if self.convert_gen != st.run_gen || self.convert_bitmap.is_none() {
                    if let Some(bmp) = self.bitmap_from_pixels(&target, pixels, *w, *h) {
                        self.convert_bitmap = Some(bmp);
                        self.convert_gen = st.run_gen;
                    }
                }
            }
            None => {
                self.convert_bitmap = None;
            }
        }
    }

    fn decode_image_bytes(
        &self,
        target: &ID2D1RenderTarget,
        bytes: &[u8],
        max_size: u32,
    ) -> Option<ID2D1Bitmap> {
        unsafe {
            let factory: IWICImagingFactory = CoCreateInstance(
                &CLSID_WICImagingFactory,
                None,
                CLSCTX_INPROC_SERVER,
            ).ok()?;
            let stream: IWICStream = factory.CreateStream().ok()?;
            stream.InitializeFromMemory(bytes).ok()?;
            let decoder = factory
                .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                .ok()?;
            let frame: IWICBitmapFrameDecode = decoder.GetFrame(0).ok()?;
            let src: IWICBitmapSource = frame.cast().ok()?;
            let mut w = 0u32;
            let mut h = 0u32;
            src.GetSize(&mut w, &mut h).ok()?;
            if w == 0 || h == 0 {
                return None;
            }
            let scale = ((max_size as f32) / (w as f32)).min((max_size as f32) / (h as f32)).min(1.0);
            let tw = ((w as f32) * scale).round().max(1.0) as u32;
            let th = ((h as f32) * scale).round().max(1.0) as u32;

            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&src, tw, th, WICBitmapInterpolationModeFant)
                .ok()?;
            let converter: IWICFormatConverter = factory.CreateFormatConverter().ok()?;
            let scaler_src: IWICBitmapSource = scaler.cast().ok()?;
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
            let mut pixels = vec![0u8; (tw * th * 4) as usize];
            converter
                .CopyPixels(
                    std::ptr::null(),
                    tw * 4,
                    &mut pixels,
                )
                .ok()?;

            self.bitmap_from_pixels(target, &pixels, tw, th)
        }
    }

    fn ensure_convert_app_bitmaps(&mut self, target: &ID2D1RenderTarget) {
        const ICON_PHOTO_PNG: &[u8] = include_bytes!("../assets/convert_photo.png");
        const ICON_MERGE_PNG: &[u8] = include_bytes!("../assets/convert_merge.png");
        const ICON_PDF_PNG: &[u8] = include_bytes!("../assets/convert_pdf.png");

        if self.convert_app_bitmaps[0].is_none() {
            self.convert_app_bitmaps[0] = self.decode_image_bytes(target, ICON_PHOTO_PNG, 128);
        }
        if self.convert_app_bitmaps[1].is_none() {
            self.convert_app_bitmaps[1] = self.decode_image_bytes(target, ICON_MERGE_PNG, 128);
        }
        if self.convert_app_bitmaps[2].is_none() {
            self.convert_app_bitmaps[2] = self.decode_image_bytes(target, ICON_PDF_PNG, 128);
        }
    }

    fn ensure_dot_icons(&mut self, target: &ID2D1RenderTarget) {
        const ICON_MUSIC: &[u8] = include_bytes!("../assets/music_icon.png");
        const ICON_FOCUS: &[u8] = include_bytes!("../assets/focus_icon.png");
        const ICON_TODO: &[u8] = include_bytes!("../assets/todo_icon.png");
        const ICON_WATER: &[u8] = include_bytes!("../assets/water_icon.png");
        const ICON_SHELF: &[u8] = include_bytes!("../assets/shelf_icon.png");
        const ICON_REMOVEBG: &[u8] = include_bytes!("../assets/removebg_icon.png");
        const ICON_CONVERT: &[u8] = include_bytes!("../assets/convert_icon.png");

        let icons = [
            ICON_MUSIC,
            ICON_FOCUS,
            ICON_TODO,
            ICON_WATER,
            ICON_SHELF,
            ICON_REMOVEBG,
            ICON_CONVERT,
        ];
        for i in 0..7 {
            if self.dot_icons[i].is_none() {
                self.dot_icons[i] = self.decode_image_bytes(target, icons[i], 48);
            }
        }
    }

    fn load_shell_icon(target: &ID2D1RenderTarget, path: &str) -> Option<ID2D1Bitmap> {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, DestroyIcon, ICONINFO};

            #[repr(C)]
            struct ShFileInfoW {
                h_icon: windows::Win32::UI::WindowsAndMessaging::HICON,
                i_icon: i32,
                dw_attributes: u32,
                sz_display_name: [u16; 260],
                sz_type_name: [u16; 80],
            }

            #[link(name = "shell32")]
            extern "system" {
                fn SHGetFileInfoW(
                    psz_path: PCWSTR,
                    dw_file_attributes: u32,
                    psfi: *mut ShFileInfoW,
                    cb_file_info: u32,
                    u_flags: u32,
                ) -> usize;
            }

            let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sfi = ShFileInfoW {
                h_icon: windows::Win32::UI::WindowsAndMessaging::HICON(std::ptr::null_mut()),
                i_icon: 0,
                dw_attributes: 0,
                sz_display_name: [0; 260],
                sz_type_name: [0; 80],
            };
            const SHGFI_ICON: u32 = 0x000000100;
            const SHGFI_LARGEICON: u32 = 0x000000000;
            let ret = SHGetFileInfoW(
                PCWSTR(path_wide.as_ptr()),
                0,
                &mut sfi,
                std::mem::size_of::<ShFileInfoW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            );
            if ret == 0 || sfi.h_icon.0.is_null() {
                return None;
            }
            let hicon = sfi.h_icon;

            let mut info = ICONINFO::default();
            if GetIconInfo(hicon, &mut info).is_err() {
                let _ = DestroyIcon(hicon);
                return None;
            }
            let hbm = if !info.hbmColor.0.is_null() {
                info.hbmColor
            } else {
                info.hbmMask
            };
            let hdc_screen = GetDC(HWND(std::ptr::null_mut()));
            let hdc_mem = CreateCompatibleDC(hdc_screen);
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 32,
                    biHeight: -32,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = vec![0u8; 32 * 32 * 4];
            let old_bmp = SelectObject(hdc_mem, HGDIOBJ(hbm.0));
            GetDIBits(
                hdc_mem,
                hbm,
                0,
                32,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            SelectObject(hdc_mem, old_bmp);
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(HWND(std::ptr::null_mut()), hdc_screen);
            if !info.hbmColor.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            }
            if !info.hbmMask.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
            }
            let _ = DestroyIcon(hicon);

            let has_alpha = pixels.chunks_exact(4).any(|p| p[3] > 0);
            if !has_alpha {
                for chunk in pixels.chunks_exact_mut(4) {
                    if chunk[0] > 0 || chunk[1] > 0 || chunk[2] > 0 {
                        chunk[3] = 255;
                    }
                }
            }

            target
                .CreateBitmap(
                    D2D_SIZE_U {
                        width: 32,
                        height: 32,
                    },
                    Some(pixels.as_ptr() as *const core::ffi::c_void),
                    32 * 4,
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

    fn ensure_tray_thumbnails(&mut self, target: &ID2D1RenderTarget, shelf: &crate::shelf::Shelf) {
        for path in &shelf.files {
            if !self.tray_thumb_cache.contains_key(path) {
                let p = std::path::Path::new(path);
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                let is_img = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "ico");
                let bmp = if is_img {
                    if let Ok(bytes) = std::fs::read(path) {
                        if bytes.len() <= 20 * 1024 * 1024 {
                            self.decode_image_bytes(target, &bytes, 72)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    Self::load_shell_icon(target, path)
                };
                if let Some(b) = bmp {
                    self.tray_thumb_cache.insert(path.clone(), b);
                }
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
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return String::new();
        }
        let mut low = 0;
        let mut high = chars.len();
        let mut best = 0;
        while low <= high {
            let mid = (low + high) / 2;
            let candidate: String = chars[..mid].iter().collect::<String>() + "\u{2026}";
            if self.measure_text(&candidate, fmt).0 <= max_width {
                best = mid;
                low = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                high = mid - 1;
            }
        }
        if best > 0 {
            chars[..best].iter().collect::<String>() + "\u{2026}"
        } else {
            "\u{2026}".to_string()
        }
    }

    fn fill_circle(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, r: f32, c: D2D1_COLOR_F) {
        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: D2D_POINT_2F { x: cx, y: cy },
                        radiusX: r,
                        radiusY: r,
                    },
                    &b,
                );
            }
        }
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

    fn draw_card_content(
        &self,
        target: &ID2D1RenderTarget,
        state: IslandState,
        ctx: &RenderContext,
        playing: bool,
        base_xform: &Matrix3x2,
        s: f32,
        tx: f32,
        lw: f32,
        lh: f32,
        lyrics_t: f32,
    ) {
        if state.is_expanded() && lh <= 32.0 {
            return;
        }
        match state {
            IslandState::Zen => {
                if ctx.settings.eye_expressions {
                    self.draw_eyes(target, lw, lh, ctx.eye_look, ctx.eye_openness, ctx.eye_slant);
                }
            }
            IslandState::Compact => {
                if lh <= 32.0 {
                    self.draw_closed(target, ctx);
                }
            }
            IslandState::Expanded | IslandState::Lyrics => {
                if lh <= 44.0 {
                    return;
                } else {
                    self.draw_music_card(
                        target,
                        ctx.media,
                        lyrics_t,
                        ctx.music_button_zooms,
                        ctx.music_bar_hover,
                        ctx.music_seek_pos,
                        base_xform,
                        s,
                        tx,
                        ctx.music_bar_zoom,
                        ctx.master_volume,
                        ctx.volume_bar_zoom,
                        ctx.settings.master_volume_control,
                    );
                    let panel_alpha = ((lyrics_t - 0.12) / 0.88).clamp(0.0, 1.0);
                    if panel_alpha > 0.01 {
                        self.draw_lyrics_panel(target, ctx.media, ctx.lyrics, panel_alpha, lw);
                    }
                }
            }
            IslandState::Pomodoro => {
                self.draw_pomodoro(target, ctx.pomodoro, playing);
            }
            IslandState::Todo => {
                self.draw_todo(
                    target,
                    ctx.todo,
                    ctx.todo_editing,
                    ctx.todo_input,
                    playing,
                    ctx.todo_scroll,
                    ctx.todo_editing_idx,
                );
            }
            IslandState::Water => {
                self.draw_water(target, ctx.water, playing);
            }
            IslandState::Tray => {
                self.draw_tray(
                    target,
                    ctx.shelf,
                    ctx.tray_scroll,
                    playing,
                    ctx.tray_drop_highlight,
                    ctx.tray_drag_pt,
                    lh,
                    ctx.tray_genie_progress,
                    &ctx.tray_btn_hovers,
                );
            }
            IslandState::RemoveBg => {
                self.draw_removebg(
                    target,
                    ctx.removebg,
                    ctx.shelf,
                    ctx.removebg_drop_highlight,
                    ctx.mouse_pt,
                    ctx,
                );
            }
            IslandState::Convert => {
                self.draw_convert(target, ctx.convert, ctx.shelf, playing, ctx.mouse_pt, ctx);
            }
            IslandState::Settings => {
                self.draw_settings(target, ctx.settings, ctx.mouse_pt);
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

    /// Speaker icon with sound waves or mute slash, centered at `(cx, cy)`.
    fn draw_icon_speaker(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, volume: f32, c: D2D1_COLOR_F) {
        // Speaker body (bolder, rounded):
        self.fill_rounded(target, cx - 8.0, cy - 4.5, 5.0, 9.0, 2.0, c);
        // Speaker cone:
        let cone = [
            D2D_POINT_2F { x: cx - 3.0, y: cy - 4.5 },
            D2D_POINT_2F { x: cx + 3.5, y: cy - 8.0 },
            D2D_POINT_2F { x: cx + 3.5, y: cy + 8.0 },
            D2D_POINT_2F { x: cx - 3.0, y: cy + 4.5 },
        ];
        self.fill_polygon(target, &cone, c);

        if volume <= 0.01 {
            // Mute "x" (bold):
            let x0 = cx + 7.0;
            let x1 = cx + 13.0;
            let l1 = [D2D_POINT_2F { x: x0, y: cy - 3.5 }, D2D_POINT_2F { x: x1, y: cy + 3.5 }];
            let l2 = [D2D_POINT_2F { x: x0, y: cy + 3.5 }, D2D_POINT_2F { x: x1, y: cy - 3.5 }];
            self.stroke_polyline(target, &l1, 2.2, c);
            self.stroke_polyline(target, &l2, 2.2, c);
        } else {
            // Wave arcs grow with the volume:
            let w1 = [
                D2D_POINT_2F { x: cx + 6.5, y: cy - 4.5 },
                D2D_POINT_2F { x: cx + 9.0, y: cy },
                D2D_POINT_2F { x: cx + 6.5, y: cy + 4.5 },
            ];
            self.stroke_polyline(target, &w1, 2.0, c);
            if volume > 0.33 {
                let w2 = [
                    D2D_POINT_2F { x: cx + 10.0, y: cy - 6.5 },
                    D2D_POINT_2F { x: cx + 13.5, y: cy },
                    D2D_POINT_2F { x: cx + 10.0, y: cy + 6.5 },
                ];
                self.stroke_polyline(target, &w2, 2.0, c);
            }
            if volume > 0.66 {
                let w3 = [
                    D2D_POINT_2F { x: cx + 13.5, y: cy - 8.5 },
                    D2D_POINT_2F { x: cx + 18.0, y: cy },
                    D2D_POINT_2F { x: cx + 13.5, y: cy + 8.5 },
                ];
                self.stroke_polyline(target, &w3, 2.0, c);
            }
        }
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
    #[allow(dead_code)]
    fn draw_icon_chevron(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, right: bool, c: D2D1_COLOR_F) {
        let dx = if right { 3.5f32 } else { -3.5f32 };
        let pts = [
            D2D_POINT_2F { x: cx - dx, y: cy - 5.5 },
            D2D_POINT_2F { x: cx + dx, y: cy },
            D2D_POINT_2F { x: cx - dx, y: cy + 5.5 },
        ];
        self.stroke_polyline(target, &pts, 2.0, c);
    }

    /// Trash can icon centered at (cx, cy).
    fn draw_icon_trash(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        self.stroke_rounded(target, cx - 4.5, cy - 2.5, 9.0, 9.0, 1.5, 1.4, c, None);
        self.fill_rounded(target, cx - 6.0, cy - 4.2, 12.0, 1.5, 0.75, c);
        self.stroke_rounded(target, cx - 2.0, cy - 6.2, 4.0, 2.0, 1.0, 1.2, c, None);
        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.DrawLine(D2D_POINT_2F { x: cx - 1.8, y: cy - 0.5 }, D2D_POINT_2F { x: cx - 1.8, y: cy + 4.5 }, &b, 1.0, None);
                target.DrawLine(D2D_POINT_2F { x: cx + 1.8, y: cy - 0.5 }, D2D_POINT_2F { x: cx + 1.8, y: cy + 4.5 }, &b, 1.0, None);
            }
        }
    }

    /// Scissors icon centered at (cx, cy).
    fn draw_icon_scissors(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        self.stroke_oval(target, cx - 4.5, cy + 1.5, 3.5, 3.5, 1.2, c);
        self.stroke_oval(target, cx + 1.0, cy + 1.5, 3.5, 3.5, 1.2, c);
        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.DrawLine(D2D_POINT_2F { x: cx - 3.0, y: cy + 1.5 }, D2D_POINT_2F { x: cx + 4.5, y: cy - 5.5 }, &b, 1.3, None);
                target.DrawLine(D2D_POINT_2F { x: cx + 3.0, y: cy + 1.5 }, D2D_POINT_2F { x: cx - 4.5, y: cy - 5.5 }, &b, 1.3, None);
            }
        }
    }

    /// Convert/sync circular arrows centered at (cx, cy).
    fn draw_icon_sync(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, c: D2D1_COLOR_F) {
        let r = 5.0f32;
        let mut pts1 = Vec::new();
        for i in 0..=12 {
            let a = -std::f32::consts::FRAC_PI_2 + (std::f32::consts::PI * 0.8) * (i as f32 / 12.0);
            pts1.push(D2D_POINT_2F { x: cx + r * a.cos(), y: cy + r * a.sin() });
        }
        self.stroke_polyline(target, &pts1, 1.4, c);
        let head1 = [
            D2D_POINT_2F { x: cx + 5.0, y: cy - 1.0 },
            D2D_POINT_2F { x: cx + 6.5, y: cy + 3.5 },
            D2D_POINT_2F { x: cx + 2.0, y: cy + 2.5 },
        ];
        self.fill_polygon(target, &head1, c);

        let mut pts2 = Vec::new();
        for i in 0..=12 {
            let a = std::f32::consts::FRAC_PI_2 + (std::f32::consts::PI * 0.8) * (i as f32 / 12.0);
            pts2.push(D2D_POINT_2F { x: cx + r * a.cos(), y: cy + r * a.sin() });
        }
        self.stroke_polyline(target, &pts2, 1.4, c);
        let head2 = [
            D2D_POINT_2F { x: cx - 5.0, y: cy + 1.0 },
            D2D_POINT_2F { x: cx - 6.5, y: cy - 3.5 },
            D2D_POINT_2F { x: cx - 2.0, y: cy - 2.5 },
        ];
        self.fill_polygon(target, &head2, c);
    }

    /// Download arrow into tray (`arrow.down.to.line`) centered at `(cx, cy)`.
    fn draw_icon_download(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, s: f32, c: D2D1_COLOR_F) {
        let arrow = [
            D2D_POINT_2F { x: cx, y: cy - s * 0.4 },
            D2D_POINT_2F { x: cx, y: cy + s * 0.15 },
        ];
        self.stroke_polyline(target, &arrow, 1.8, c);
        let head = [
            D2D_POINT_2F { x: cx - s * 0.28, y: cy - s * 0.05 },
            D2D_POINT_2F { x: cx, y: cy + s * 0.22 },
            D2D_POINT_2F { x: cx + s * 0.28, y: cy - s * 0.05 },
        ];
        self.stroke_polyline(target, &head, 1.8, c);
        let tray = [
            D2D_POINT_2F { x: cx - s * 0.35, y: cy + s * 0.4 },
            D2D_POINT_2F { x: cx + s * 0.35, y: cy + s * 0.4 },
        ];
        self.stroke_polyline(target, &tray, 1.8, c);
    }

    /// Lightning bolt / process icon centered at `(cx, cy)`.
    fn draw_icon_bolt(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, s: f32, c: D2D1_COLOR_F) {
        let pts = [
            D2D_POINT_2F { x: cx + s * 0.08, y: cy - s * 0.45 },
            D2D_POINT_2F { x: cx - s * 0.32, y: cy + s * 0.02 },
            D2D_POINT_2F { x: cx - s * 0.02, y: cy + s * 0.02 },
            D2D_POINT_2F { x: cx - s * 0.08, y: cy + s * 0.45 },
            D2D_POINT_2F { x: cx + s * 0.32, y: cy - s * 0.02 },
            D2D_POINT_2F { x: cx + s * 0.02, y: cy - s * 0.02 },
        ];
        self.fill_polygon(target, &pts, c);
    }

    fn draw_icon_lines_three(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, w: f32, c: D2D1_COLOR_F) {
        let half = w * 0.5;
        for &dy in &[-3.5f32, 0.0, 3.5] {
            self.fill_rounded(target, cx - half, cy + dy - 0.75, w, 1.5, 0.75, c);
        }
    }

    fn draw_icon_markdown(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, s: f32, c: D2D1_COLOR_F) {
        let w = s * 1.3;
        let h = s * 0.8;
        let x = cx - w * 0.5;
        let y = cy - h * 0.5;
        self.stroke_rounded(target, x, y, w, h, 6.0, 1.8, c, None);

        let mx0 = x + 6.0;
        let mw = w * 0.44;
        let my0 = y + 6.0;
        let mh = h - 12.0;

        if let Some(b) = self.brush(target, c) {
            unsafe {
                target.DrawLine(D2D_POINT_2F { x: mx0, y: my0 + mh }, D2D_POINT_2F { x: mx0, y: my0 }, &b, 1.8, None);
                target.DrawLine(D2D_POINT_2F { x: mx0, y: my0 }, D2D_POINT_2F { x: mx0 + mw * 0.5, y: my0 + mh * 0.6 }, &b, 1.8, None);
                target.DrawLine(D2D_POINT_2F { x: mx0 + mw * 0.5, y: my0 + mh * 0.6 }, D2D_POINT_2F { x: mx0 + mw, y: my0 }, &b, 1.8, None);
                target.DrawLine(D2D_POINT_2F { x: mx0 + mw, y: my0 }, D2D_POINT_2F { x: mx0 + mw, y: my0 + mh }, &b, 1.8, None);

                let ax = x + w - 10.0;
                target.DrawLine(D2D_POINT_2F { x: ax, y: my0 }, D2D_POINT_2F { x: ax, y: my0 + mh }, &b, 1.8, None);
                target.DrawLine(D2D_POINT_2F { x: ax - 3.5, y: my0 + mh - 3.5 }, D2D_POINT_2F { x: ax, y: my0 + mh }, &b, 1.8, None);
                target.DrawLine(D2D_POINT_2F { x: ax + 3.5, y: my0 + mh - 3.5 }, D2D_POINT_2F { x: ax, y: my0 + mh }, &b, 1.8, None);
            }
        }
    }

    fn draw_tooltip(&self, target: &ID2D1RenderTarget, text: &str, cx: f32, cy: f32) {
        if let Some(fmt) = self.fmt_11_reg.as_ref() {
            let (tw, th) = self.measure_text(text, fmt);
            let pad_h = 7.0f32;
            let pad_v = 3.5f32;
            let w = tw + pad_h * 2.0;
            let h = th + pad_v * 2.0;
            let x = (cx - w * 0.5).clamp(8.0, 532.0 - w);
            let y = cy - h * 0.5;
            self.fill_rounded(target, x, y, w, h, 6.0, color(0.12, 0.12, 0.14, 0.96));
            self.stroke_rounded(target, x, y, w, h, 6.0, 1.0, color(1.0, 1.0, 1.0, 0.28), None);
            self.draw_text_at(target, text, Some(fmt), color(1.0, 1.0, 1.0, 1.0), x + pad_h, y + pad_v - 0.5);
        }
    }

    fn draw_icon_button(
        &self,
        target: &ID2D1RenderTarget,
        rect: (f32, f32, f32, f32),
        is_hovered: bool,
        tooltip: &str,
        primary: bool,
        draw_icon: &dyn Fn(f32, f32, D2D1_COLOR_F),
    ) {
        let (x0, y0, x1, y1) = rect;
        let w = x1 - x0;
        let h = y1 - y0;
        let cx = x0 + w * 0.5;
        let cy = y0 + h * 0.5;
        let r = 10.0f32.min(h * 0.5);

        let (bg_color, stroke_color, icon_color) = if primary {
            if is_hovered {
                (color(1.0, 1.0, 1.0, 1.0), color(1.0, 1.0, 1.0, 1.0), color(0.0, 0.0, 0.0, 1.0))
            } else {
                (color(1.0, 1.0, 1.0, 0.90), color(1.0, 1.0, 1.0, 0.90), color(0.0, 0.0, 0.0, 1.0))
            }
        } else {
            if is_hovered {
                (color(1.0, 1.0, 1.0, 0.24), color(1.0, 1.0, 1.0, 0.45), color(1.0, 1.0, 1.0, 1.0))
            } else {
                (color(1.0, 1.0, 1.0, 0.10), color(1.0, 1.0, 1.0, 0.20), color(1.0, 1.0, 1.0, 0.85))
            }
        };

        self.fill_rounded(target, x0, y0, w, h, r, bg_color);
        self.stroke_rounded(target, x0, y0, w, h, r, 1.0, stroke_color, None);
        draw_icon(cx, cy, icon_color);

        if is_hovered && !tooltip.is_empty() {
            let tip_y = if y0 < 45.0 { y1 + 14.0 } else { y0 - 14.0 };
            self.draw_tooltip(target, tooltip, cx, tip_y);
        }
    }

    /// Expanding oval button on hover:
    /// At hover_t = 0.0: circle (w = h) with icon centered.
    /// At hover_t > 0.0: expands horizontally to an oval/pill revealing label text beside icon.
    fn draw_expanding_button(
        &self,
        target: &ID2D1RenderTarget,
        rect: (f32, f32, f32, f32),
        hover_t: f32,
        label: &str,
        primary: bool,
        draw_icon: &dyn Fn(f32, f32, D2D1_COLOR_F),
    ) {
        let (x0, y0, x1, y1) = rect;
        let w = x1 - x0;
        let h = y1 - y0;
        let r = h * 0.5;

        let (bg_color, stroke_color, content_color) = if primary {
            (
                color(1.0, 1.0, 1.0, 0.90 + 0.10 * hover_t),
                color(1.0, 1.0, 1.0, 0.90 + 0.10 * hover_t),
                color(0.0, 0.0, 0.0, 1.0),
            )
        } else {
            (
                color(1.0, 1.0, 1.0, 0.08 + 0.12 * hover_t),
                color(1.0, 1.0, 1.0, 0.18 + 0.25 * hover_t),
                color(1.0, 1.0, 1.0, 0.85 + 0.15 * hover_t),
            )
        };

        self.fill_rounded(target, x0, y0, w, h, r, bg_color);
        self.stroke_rounded(target, x0, y0, w, h, r, 1.0, stroke_color, None);

        // Icon position: centered in circle portion (left h x h)
        let icon_cx = x0 + h * 0.5;
        let icon_cy = y0 + h * 0.5;
        draw_icon(icon_cx, icon_cy, content_color);

        // Label text smoothly fades and reveals when expanding:
        if hover_t > 0.03 && !label.is_empty() {
            unsafe {
                let clip = D2D_RECT_F {
                    left: x0 + h,
                    top: y0,
                    right: x1 - 4.0,
                    bottom: y1,
                };
                target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            if let Some(fmt) = self.fmt_12_reg.as_ref() {
                let text_alpha = (hover_t * 1.5).clamp(0.0, 1.0);
                let text_c = if primary {
                    color(0.0, 0.0, 0.0, text_alpha)
                } else {
                    color(1.0, 1.0, 1.0, text_alpha)
                };
                self.draw_text_at(target, label, Some(fmt), text_c, x0 + h + 2.0, y0 + (h - 14.0) * 0.5);
            }
            unsafe {
                target.PopAxisAlignedClip();
            }
        }
    }

    // --- Shared Aura Mac Pieces (`IslandView.swift`) ---

    /// Idle eyes (`drawEyes`) with morphing, winking, and asymmetrical inquisitive tilt.
    fn draw_eyes(
        &self,
        target: &ID2D1RenderTarget,
        lw: f32,
        lh: f32,
        eye_look: (f32, f32),
        eye_openness: [f32; 2],
        eye_slant: [f32; 2],
    ) {
        let mid_x = lw * 0.5;
        let mid_y = lh * 0.5;
        let eye_gap = 13.0f32;
        let w = 15.0f32;
        let centers = [mid_x - (w + eye_gap) * 0.5, mid_x + (w + eye_gap) * 0.5];
        let white = color(1.0, 1.0, 1.0, 1.0);

        for (i, &cx_base) in centers.iter().enumerate() {
            let openness = eye_openness[i].clamp(0.06, 1.0);
            let slant = eye_slant[i];
            let h = 2.5 + (13.5 - 2.5) * openness;
            let cx = cx_base + eye_look.0.clamp(-1.0, 1.0) * 2.0;
            let cy = mid_y + slant + eye_look.1.clamp(-1.0, 1.0) * 2.0;
            let r = (4.0f32).min(h * 0.5);

            if slant.abs() > 0.05 {
                let angle_deg = if i == 0 { -slant * 3.5 } else { slant * 3.5 };
                let angle_rad = angle_deg * std::f32::consts::PI / 180.0;
                let sin_a = angle_rad.sin();
                let cos_a = angle_rad.cos();
                let rot = Matrix3x2 {
                    M11: cos_a,
                    M12: sin_a,
                    M21: -sin_a,
                    M22: cos_a,
                    M31: cx * (1.0 - cos_a) + cy * sin_a,
                    M32: cy * (1.0 - cos_a) - cx * sin_a,
                };
                unsafe {
                    target.SetTransform(&rot);
                }
                self.fill_rounded(target, cx - w * 0.5, cy - h * 0.5, w, h, r, white);
                unsafe {
                    target.SetTransform(&Matrix3x2::identity());
                }
            } else {
                self.fill_rounded(target, cx - w * 0.5, cy - h * 0.5, w, h, r, white);
            }
        }
    }

    /// iOS battery icon (`drawBatteryIcon`, `IslandView.swift` lines 1539-1564).
    fn draw_battery_icon(&self, target: &ID2D1RenderTarget, cx: f32, cy: f32, percent: Option<f32>, charging: bool) {
        let level = percent.unwrap_or(1.0).clamp(0.0, 1.0);
        let is_charging = charging || percent.is_none();
        let w = 20.0f32;
        let h = 10.0f32;
        let x = cx - w * 0.5;
        let y = cy - h * 0.5;
        let white = color(1.0, 1.0, 1.0, 1.0);
        self.stroke_rounded(target, x, y, w, h, 2.8, 1.2, white, None);
        self.fill_rounded(target, x + w + 0.8, cy - 2.0, 1.5, 4.0, 0.75, white);
        let fill_c = if is_charging {
            color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0)
        } else if level <= 0.20 {
            color(SYSTEM_RED.0, SYSTEM_RED.1, SYSTEM_RED.2, 1.0)
        } else {
            white
        };
        let fill_w = (w - 3.6) * level;
        if fill_w > 0.0 {
            let r = 1.2f32.min(fill_w * 0.5);
            self.fill_rounded(target, x + 1.8, y + 1.8, fill_w, h - 3.6, r, fill_c);
        }
    }

    /// Animated waveform (5 bars in expanded headers, 4 slim bars in compact pill).
    fn draw_waveform(&self, target: &ID2D1RenderTarget, rx: f32, ry: f32, rw: f32, rh: f32, playing: bool) {
        let is_compact = rh <= 10.0;
        let (bars, bar_w, gap) = if is_compact {
            (4usize, 2.0f32, 2.0f32)
        } else {
            (5usize, 3.0f32, 3.0f32)
        };
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
            self.fill_rounded(target, x, mid_y - h * 0.5, bar_w, h, 1.0, white);
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

    /// Card header: battery indicator (progress/loading) + centered date/time + right waveform.
    /// Statically rendered on top across ALL expanded sections.
    /// Continuous morphing header between compact pill and expanded card.
    /// Battery icon, clock/date, and waveform smoothly glide from compact pill coordinates
    /// to expanded card header coordinates as `lh` animates from 30px to 184px (and back).
    fn draw_morphing_header(
        &self,
        target: &ID2D1RenderTarget,
        lw: f32,
        _lh: f32,
        expand_t: f32,
        playing: bool,
        battery_percent: Option<f32>,
        battery_charging: bool,
        header_active: bool,
    ) {
        let mid_y = 15.0f32;
        let now = chrono::Local::now();

        // 1. Compact coordinates (lw ~ 180, mid_y = 15.0):
        // Left: Battery centered at cx = 35.0, cy = 15.0 (left edge = 24.0)
        let compact_bat_cx = 35.0f32;
        let compact_bat_cy = mid_y;
        // Center: Time centered at cx = lw * 0.5, y = 7.5
        let compact_time_y = mid_y - 7.5;
        // Right: Waveform right edge at lw - 24.0 (width = 14.0, height = 9.0)
        let compact_wave_w = 14.0f32;
        let compact_wave_h = 9.0f32;
        let compact_wave_x = lw - 24.0 - compact_wave_w;
        let compact_wave_y = mid_y - compact_wave_h * 0.5;

        if !header_active {
            // For sections without top header (Tray, RemoveBg, Convert):
            // Compact pill elements smoothly fade in as the pill closes:
            let fade_alpha = (1.0 - expand_t / 0.35).clamp(0.0, 1.0);
            if fade_alpha > 0.01 {
                self.draw_battery_icon(target, compact_bat_cx, compact_bat_cy, battery_percent, battery_charging);
                let hm = now.format("%H:%M").to_string();
                if let Some(fmt) = self.fmt_13_med.as_ref() {
                    self.draw_centered_text(
                        target,
                        &hm,
                        Some(fmt),
                        color(1.0, 1.0, 1.0, fade_alpha),
                        lw,
                        compact_time_y,
                    );
                }
                self.draw_waveform(target, compact_wave_x, compact_wave_y, compact_wave_w, compact_wave_h, playing);
            }
            return;
        }

        // Expanded sections with header (Music, Pomodoro, Todo, Water):
        // Perfectly symmetrical 3-part layout across entire width:
        // Left: Battery (left margin = 54.0, center = 65.0, cy = 20.0)
        let header_bat_cx = 65.0f32;
        let header_bat_cy = 20.0f32;

        // Center: Date & Time (centered at cx = lw * 0.5, y = 12.5)
        let header_time_y = 12.5f32;

        // Right: Waveform (right margin = 54.0, width = 27.0, height = 12.0, ending at lw - 54.0)
        let header_wave_w = 27.0f32;
        let header_wave_h = 12.0f32;
        let header_wave_x = lw - 54.0 - header_wave_w;
        let header_wave_y = 20.0 - header_wave_h * 0.5;

        // 1. Battery continuous glide along single smooth trajectory:
        let cur_bat_x = compact_bat_cx + (header_bat_cx - compact_bat_cx) * expand_t;
        let cur_bat_y = compact_bat_cy + (header_bat_cy - compact_bat_cy) * expand_t;
        self.draw_battery_icon(target, cur_bat_x, cur_bat_y, battery_percent, battery_charging);

        // 2. Clock / Date continuous glide & crisp non-overlapping crossfade:
        let cur_text_y = compact_time_y + (header_time_y - compact_time_y) * expand_t;
        let date_str = now.format("%a %-d %b %Y %H:%M").to_string();
        let time_str = now.format("%H:%M").to_string();

        if let Some(fmt) = self.fmt_13_med.as_ref() {
            if expand_t <= 0.35 {
                let time_alpha = (1.0 - expand_t / 0.35).clamp(0.0, 1.0);
                self.draw_centered_text(
                    target,
                    &time_str,
                    Some(fmt),
                    color(1.0, 1.0, 1.0, time_alpha),
                    lw,
                    cur_text_y,
                );
            } else {
                let date_alpha = ((expand_t - 0.35) / 0.65).clamp(0.0, 1.0);
                self.draw_centered_text(
                    target,
                    &date_str,
                    Some(fmt),
                    color(1.0, 1.0, 1.0, date_alpha),
                    lw,
                    cur_text_y,
                );
            }
        }

        // 3. Waveform continuous glide & scale:
        let cur_wave_x = compact_wave_x + (header_wave_x - compact_wave_x) * expand_t;
        let cur_wave_y = compact_wave_y + (header_wave_y - compact_wave_y) * expand_t;
        let cur_wave_w = compact_wave_w + (header_wave_w - compact_wave_w) * expand_t;
        let cur_wave_h = compact_wave_h + (header_wave_h - compact_wave_h) * expand_t;
        self.draw_waveform(target, cur_wave_x, cur_wave_y, cur_wave_w, cur_wave_h, playing);
    }

    #[allow(dead_code)]
    fn draw_card_header(
        &self,
        target: &ID2D1RenderTarget,
        width: f32,
        playing: bool,
        battery_percent: Option<f32>,
        battery_charging: bool,
    ) {
        let date_str = chrono::Local::now()
            .format("%a %-d %b %Y %H:%M")
            .to_string();
        let Some(fmt) = self.fmt_13_med.as_ref() else { return };
        let (date_w, _) = self.measure_text(&date_str, fmt);
        let bat_w = 18.0f32;
        let gap = 9.0f32;
        let group_w = bat_w + gap + date_w;
        let group_x = (width - group_w) * 0.5;

        // Battery loading/progress indicator replacing green dot:
        self.draw_battery_icon(
            target,
            group_x + bat_w * 0.5,
            21.0,
            battery_percent,
            battery_charging,
        );

        self.draw_text_at(
            target,
            &date_str,
            Some(fmt),
            color(1.0, 1.0, 1.0, 1.0),
            group_x + bat_w + gap,
            14.0,
        );
        self.draw_waveform(target, width - 68.0, 14.0, 48.0, 16.0, playing);
    }

    /// Horizontal interactive page dots at the bottom center of the card.
    fn draw_page_dots(
        &self,
        target: &ID2D1RenderTarget,
        state: IslandState,
        lw: f32,
        lh: f32,
        dot_bounds: (f32, f32),
        hovered_dot: Option<usize>,
        lyrics_t: f32,
    ) {
        let dots_alpha = (1.0 - lyrics_t * 1.5).clamp(0.0, 1.0);
        if dots_alpha <= 0.01 {
            return;
        }
        let count = crate::state::SECTION_COUNT;
        let active = state.section_index();
        let spacing = 16.0f32;
        let total_w = (count as f32 - 1.0) * spacing;
        let base_w = if lw > 540.0 { 540.0 } else { lw };
        let start_x = (base_w - total_w) * 0.5;
        let y = lh - 14.0;

        let (pill_left, pill_w) = if dot_bounds.1 > 0.0 {
            (start_x + dot_bounds.0, dot_bounds.1)
        } else {
            let cx = start_x + (active as f32) * spacing;
            (cx - 7.0, 14.0)
        };
        let pill_right = pill_left + pill_w;

        // 1. Draw inactive slot dots (or morph into section icon if hovered):
        for i in 0..count {
            let cx = start_x + (i as f32) * spacing;
            let is_hovered = hovered_dot == Some(i);
            let base_alpha = if is_hovered { 0.95 } else { 0.28 };
            let r = if is_hovered { 2.75 } else { 2.25 };

            let overlap = if cx >= pill_left - r && cx <= pill_right + r {
                0.0f32
            } else {
                1.0f32
            };

            if is_hovered {
                if let Some(bmp) = &self.dot_icons[i] {
                    let isz = 16.0f32;
                    let dst = D2D_RECT_F {
                        left: cx - isz * 0.5,
                        top: y - isz * 0.5,
                        right: cx + isz * 0.5,
                        bottom: y + isz * 0.5,
                    };
                    unsafe {
                        target.DrawBitmap(
                            bmp,
                            Some(&dst),
                            dots_alpha,
                            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            None,
                        );
                    }
                } else {
                    self.fill_oval(target, cx - r, y - r, r * 2.0, r * 2.0, color(1.0, 1.0, 1.0, base_alpha * dots_alpha));
                }
            } else {
                let alpha = base_alpha * overlap * dots_alpha;
                if alpha > 0.01 {
                    self.fill_oval(target, cx - r, y - r, r * 2.0, r * 2.0, color(1.0, 1.0, 1.0, alpha));
                }
            }
        }

        // 2. Draw animated active indicator capsule (if not hovered):
        if hovered_dot.map(|h| h != active).unwrap_or(true) {
            let pill_h = 4.5f32;
            self.fill_rounded(
                target,
                pill_left,
                y - pill_h * 0.5,
                pill_w,
                pill_h,
                pill_h * 0.5,
                color(1.0, 1.0, 1.0, dots_alpha),
            );
        }

        // 3. Mini icon/label badge for hovered dot (Nusa style)
        if let Some(h_idx) = hovered_dot {
            let h_cx = start_x + (h_idx as f32) * spacing;
            let label = match h_idx {
                0 => "Music",
                1 => "Focus",
                2 => "Todo",
                3 => "Water",
                4 => "Shelf",
                5 => "Remove BG",
                6 => "Convert",
                _ => "",
            };
            if let Some(fmt) = self.fmt_10_med.as_ref() {
                let (tw, _th) = self.measure_text(label, fmt);
                let badge_w = tw + 14.0;
                let badge_h = 16.0;
                let badge_x = (h_cx - badge_w * 0.5).clamp(16.0, lw - badge_w - 16.0);
                let badge_y = y - 22.0;
                self.fill_rounded(
                    target,
                    badge_x,
                    badge_y,
                    badge_w,
                    badge_h,
                    4.0,
                    color(0.10, 0.10, 0.12, 0.94),
                );
                self.stroke_rounded(
                    target,
                    badge_x,
                    badge_y,
                    badge_w,
                    badge_h,
                    4.0,
                    0.5,
                    color(1.0, 1.0, 1.0, 0.16),
                    None,
                );
                self.draw_centered_text(
                    target,
                    label,
                    Some(fmt),
                    color(1.0, 1.0, 1.0, 0.95),
                    badge_x * 2.0 + badge_w,
                    badge_y + 1.5,
                );
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

        // 1. Live Pomodoro takes priority:
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
                24.0,
                mid_y,
                6.5,
                2.0,
                frac,
                color(rgb.0, rgb.1, rgb.2, alpha),
            );
            let text_alpha = if ctx.pomodoro.running { 1.0 } else { 0.7 };
            self.draw_centered_text(
                target,
                &format_pomodoro_time(rem),
                self.fmt_13_bold.as_ref(),
                color(1.0, 1.0, 1.0, text_alpha),
                lw,
                mid_y - 8.0,
            );
            return;
        }

        // 2. Lyrics-mode closed pill:
        if ctx.lyrics_mode {
            if let Some(m) = ctx.media {
                self.draw_text_at(
                    target,
                    "♪",
                    self.fmt_14_semi.as_ref(),
                    color(1.0, 1.0, 1.0, 1.0),
                    28.0,
                    mid_y - 9.0,
                );
                let line = Self::current_lyric_triple(Some(m), ctx.lyrics)
                    .1
                    .unwrap_or_else(|| m.title.clone());
                if let Some(fmt) = self.fmt_13_med.as_ref() {
                    let shown = self.truncate_text(&line, lw - 72.0, fmt);
                    self.draw_centered_text(
                        target,
                        &shown,
                        Some(fmt),
                        color(1.0, 1.0, 1.0, 1.0),
                        lw,
                        mid_y - 8.0,
                    );
                }
                return;
            }
        }

        // 3. Plain minimalist compact pill: battery + clock + waveform with comfortable padding:
        let compact_bat_cx = 35.0f32;
        self.draw_battery_icon(target, compact_bat_cx, mid_y, ctx.battery_percent, ctx.battery_charging);
        let hm = now.format("%H:%M").to_string();
        self.draw_centered_text(
            target,
            &hm,
            self.fmt_13_med.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            lw,
            mid_y - 7.5,
        );
        let wave_h = 9.0f32;
        let playing = ctx.media.map(|m| m.playing).unwrap_or(false);
        let wave_w = 14.0f32;
        let wave_x = lw - 24.0 - wave_w;
        self.draw_waveform(target, wave_x, mid_y - wave_h * 0.5, wave_w, wave_h, playing);
    }

    // --- Music Card (`Expanded`, `520x180`) & Lyrics Card (`Lyrics`, `700x180`) ---

    fn draw_music_card(
        &self,
        target: &ID2D1RenderTarget,
        m_opt: Option<&crate::media::MediaInfo>,
        lyrics_t: f32,
        button_zooms: [f32; 4],
        _bar_hover: bool,
        seek_pos: Option<f64>,
        base_xform: &Matrix3x2,
        s: f32,
        tx: f32,
        music_bar_zoom: f32,
        master_volume: f32,
        volume_bar_zoom: f32,
        volume_enabled: bool,
    ) {
        // Cover art (`x: 54, y: 36, w: 60, h: 60, cornerRadius: 14`):
        let cover_rect = D2D_RECT_F {
            left: 54.0,
            top: 36.0,
            right: 114.0,
            bottom: 96.0,
        };
        if m_opt.is_some() && self.cover_bitmap.is_some() {
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
            }
        } else {
            self.fill_rounded(target, 54.0, 36.0, 60.0, 60.0, 14.0, color(1.0, 1.0, 1.0, 0.12));
            if let Some(fmt) = self.fmt_28_reg.as_ref() {
                let (w, h) = self.measure_text("♪", fmt);
                self.draw_text_at(
                    target,
                    "♪",
                    Some(fmt),
                    color(1.0, 1.0, 1.0, 0.70),
                    84.0 - w * 0.5,
                    66.0 - h * 0.5,
                );
            }
        }

        // Smoothly interpolate widths & positions between `520` (`lyrics_t = 0.0`) and `700` (`lyrics_t = 1.0`).
        // Truncate at stable target widths (`212` when settled in Lyrics, `340` otherwise) and clip
        // with `PushAxisAlignedClip` so characters never jump/flicker during `520 <-> 700` morphs.
        let text_w = 340.0 + (212.0 - 340.0) * lyrics_t;
        let trunc_w = if lyrics_t >= 0.99 { 212.0 } else { 340.0 };
        let (title_raw, artist_raw, title_c, artist_c) = match m_opt {
            Some(m) => {
                let t = if m.title.is_empty() { "Unknown title" } else { m.title.as_str() };
                let a = if m.artist.is_empty() { "Unknown artist" } else { m.artist.as_str() };
                (t, a, color(1.0, 1.0, 1.0, 1.0), color(1.0, 1.0, 1.0, 0.65))
            }
            None => (
                "Not Playing",
                "No media active",
                color(1.0, 1.0, 1.0, 0.85),
                color(1.0, 1.0, 1.0, 0.38),
            ),
        };
        unsafe {
            target.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: 130.0,
                    top: 30.0,
                    right: 130.0 + text_w,
                    bottom: 96.0,
                },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
        }
        if let Some(fmt) = self.fmt_25_bold.as_ref() {
            let shown = self.truncate_text(title_raw, trunc_w, fmt);
            self.draw_text_at(target, &shown, Some(fmt), title_c, 130.0, 34.0);
        }
        if let Some(fmt) = self.fmt_16_reg.as_ref() {
            let shown = self.truncate_text(artist_raw, trunc_w, fmt);
            self.draw_text_at(target, &shown, Some(fmt), artist_c, 130.0, 68.0);
        }
        unsafe {
            target.PopAxisAlignedClip();
        }

        // Progress row (`y: 104`):
        let bar_w = 340.0 + (164.0 - 340.0) * lyrics_t;
        let right_x = 486.0 + (320.0 - 486.0) * lyrics_t;
        let (pos, dur) = match m_opt {
            Some(m) => (seek_pos.unwrap_or_else(|| m.position_secs.max(0.0)), m.duration_secs.max(0.0)),
            None => (0.0, 0.0),
        };
        let time_c = if m_opt.is_some() { color(1.0, 1.0, 1.0, 1.0) } else { color(1.0, 1.0, 1.0, 0.35) };
        self.draw_text_at(
            target,
            &format_time_02(pos),
            self.fmt_12_reg.as_ref(),
            time_c,
            54.0,
            102.0,
        );
        if dur > 0.0 {
            let rem = (dur - pos).max(0.0);
            self.draw_right_aligned_text(
                target,
                &format!("-{}", format_time_02(rem)),
                self.fmt_12_reg.as_ref(),
                time_c,
                right_x,
                102.0,
            );
        }
        let bar_h = 4.0 + 3.0 * music_bar_zoom;
        let bar_y = 110.0 - (bar_h - 4.0) * 0.5;
        self.fill_rounded(target, 98.0, bar_y, bar_w, bar_h, bar_h * 0.5, color(1.0, 1.0, 1.0, 0.20));
        if dur > 0.0 {
            let filled = (pos / dur).clamp(0.0, 1.0) as f32;
            let fill_w = bar_w * filled;
            if fill_w > 0.0 {
                self.fill_rounded(target, 98.0, bar_y, fill_w, bar_h, bar_h * 0.5, color(1.0, 1.0, 1.0, 1.0));
            }
        }

        // Controls row (`y: 140`):
        let ctrl_c = if m_opt.is_some() { color(1.0, 1.0, 1.0, 1.0) } else { color(1.0, 1.0, 1.0, 0.28) };
        let draw_scaled = |cx: f32, cy: f32, zoom: f32, f: &dyn Fn()| {
            if (zoom - 1.0).abs() > 0.005 {
                let mat = Matrix3x2 {
                    M11: zoom * s,
                    M12: 0.0,
                    M21: 0.0,
                    M22: zoom * s,
                    M31: cx * (1.0 - zoom) * s + tx,
                    M32: cy * (1.0 - zoom) * s,
                };
                unsafe {
                    target.SetTransform(&mat);
                }
                f();
                unsafe {
                    target.SetTransform(base_xform);
                }
            } else {
                f();
            }
        };

        draw_scaled(84.0, 140.0, button_zooms[0], &|| {
            self.draw_icon_bubble(target, 84.0, 140.0, ctrl_c);
        });
        let c0 = MUSIC_CONTROL_CENTERS[0] + (LYRICS_CONTROL_CENTERS[0] - MUSIC_CONTROL_CENTERS[0]) * lyrics_t;
        let c1 = MUSIC_CONTROL_CENTERS[1] + (LYRICS_CONTROL_CENTERS[1] - MUSIC_CONTROL_CENTERS[1]) * lyrics_t;
        let c2 = MUSIC_CONTROL_CENTERS[2] + (LYRICS_CONTROL_CENTERS[2] - MUSIC_CONTROL_CENTERS[2]) * lyrics_t;
        draw_scaled(c0, 140.0, button_zooms[1], &|| {
            self.draw_icon_backward(target, c0, 140.0, ctrl_c);
        });
        draw_scaled(c1, 140.0, button_zooms[2], &|| {
            let is_playing = m_opt.map(|m| m.playing).unwrap_or(false);
            if is_playing {
                self.draw_icon_pause(target, c1, 140.0, 18.0, ctrl_c);
            } else {
                self.draw_icon_play(target, c1, 140.0, 18.0, ctrl_c);
            }
        });
        draw_scaled(c2, 140.0, button_zooms[3], &|| {
            self.draw_icon_forward(target, c2, 140.0, ctrl_c);
        });

        // Volume control (only speaker icon initially; expands bar to the left on hover):
        let vol_alpha = (1.0 - lyrics_t * 2.5).clamp(0.0, 1.0);
        if volume_enabled && vol_alpha > 0.01 {
            self.draw_icon_speaker(target, 472.0, 140.0, master_volume, color(1.0, 1.0, 1.0, 0.85 * vol_alpha));
            if volume_bar_zoom > 0.005 {
                let max_w = 94.0f32;
                let cur_w = max_w * volume_bar_zoom;
                let bar_right = 452.0f32;
                let bar_left = bar_right - cur_w;
                let vbar_h = 4.0;
                let vbar_y = 140.0 - vbar_h * 0.5;
                let anim_alpha = vol_alpha * volume_bar_zoom;
                self.fill_rounded(target, bar_left, vbar_y, cur_w, vbar_h, vbar_h * 0.5, color(1.0, 1.0, 1.0, 0.20 * anim_alpha));
                let fill_w = (cur_w * master_volume.clamp(0.0, 1.0)).max(0.0);
                if fill_w > 0.0 {
                    self.fill_rounded(target, bar_left, vbar_y, fill_w, vbar_h, vbar_h * 0.5, color(1.0, 1.0, 1.0, 0.95 * anim_alpha));
                }
            }
        }
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
            return (None, None, None);
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
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return vec![];
        }
        if self.measure_text(trimmed, fmt).0 <= width {
            return vec![trimmed.to_string()];
        }
        let words: Vec<&str> = trimmed.split_whitespace().collect();
        let mut lines = Vec::new();
        let mut current = String::new();
        for word in words {
            let cand = if current.is_empty() {
                word.to_string()
            } else {
                format!("{} {}", current, word)
            };
            if self.measure_text(&cand, fmt).0 <= width {
                current = cand;
            } else {
                if !current.is_empty() {
                    lines.push(current);
                    if lines.len() == max_lines {
                        if let Some(last) = lines.last_mut() {
                            *last = self.truncate_text(&format!("{}…", last), width, fmt);
                        }
                        return lines;
                    }
                }
                current = word.to_string();
            }
        }
        if !current.is_empty() && lines.len() < max_lines {
            lines.push(current);
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

        if cache.lines.is_empty() || !cache.synced {
            self.draw_text_at(
                target,
                "No lyrics found",
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
            if !roll.items.is_empty() && roll.distance > 0.0 && elapsed < 0.70 && panel_alpha > 0.98 {
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

    // --- Pomodoro Section (`drawPomodoro`, `520x180`, `IslandView.swift` lines 2349-2392) ---

    fn draw_pomodoro(&self, target: &ID2D1RenderTarget, pomodoro: &crate::pomodoro::Pomodoro, _playing: bool) {
        self.draw_text_at(
            target,
            "Pomodoro",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            54.0,
            36.0,
        );

        let now = chrono::Local::now();
        let rem = pomodoro.remaining(now);
        let dur = pomodoro.duration_secs().max(1);
        let frac = (rem as f32 / dur as f32).clamp(0.0, 1.0);
        let is_focus = pomodoro.mode == crate::pomodoro::PomodoroMode::Focus;
        let rgb = if is_focus { SYSTEM_RED } else { SYSTEM_GREEN };
        let mode_c = color(rgb.0, rgb.1, rgb.2, 1.0);

        // Compact left progress ring: center (72, 96), radius 32, width 6 + tinted interior + stopwatch icon:
        self.draw_progress_ring(target, POMODORO_RING_CX, POMODORO_RING_CY, POMODORO_RING_R, 6.0, frac, mode_c);
        self.fill_oval(target, POMODORO_RING_CX - 24.0, POMODORO_RING_CY - 24.0, 48.0, 48.0, color(rgb.0, rgb.1, rgb.2, 0.15));
        self.draw_icon_stopwatch(target, POMODORO_RING_CX, POMODORO_RING_CY, 26.0, color(1.0, 1.0, 1.0, 1.0));

        // Countdown + Reset button + subtitle:
        self.draw_text_at(
            target,
            &format_pomodoro_time(rem),
            self.fmt_44_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            POMODORO_COUNTDOWN_X,
            POMODORO_COUNTDOWN_Y,
        );
        let reset_w = POMODORO_RESET_RECT.2 - POMODORO_RESET_RECT.0;
        let reset_h = POMODORO_RESET_RECT.3 - POMODORO_RESET_RECT.1;
        self.fill_oval(
            target,
            POMODORO_RESET_RECT.0,
            POMODORO_RESET_RECT.1,
            reset_w,
            reset_h,
            color(1.0, 1.0, 1.0, 0.12),
        );
        self.draw_icon_reset(
            target,
            POMODORO_RESET_RECT.0 + reset_w * 0.5,
            POMODORO_RESET_RECT.1 + reset_h * 0.5,
            color(1.0, 1.0, 1.0, 1.0),
        );
        self.draw_text_at(
            target,
            if is_focus { "Focus time" } else { "Short break" },
            self.fmt_14_reg.as_ref(),
            color(1.0, 1.0, 1.0, 0.60),
            POMODORO_COUNTDOWN_X,
            POMODORO_SUBTITLE_Y,
        );

        // Mode pills (`y: 126..154`):
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
            POMODORO_SHORT_BREAK_RECT.0 + 16.0,
            POMODORO_SHORT_BREAK_RECT.1 + 14.0,
            color(1.0, 1.0, 1.0, if sb_active { 1.0 } else { 0.75 }),
        );
        self.draw_text_at(
            target,
            "Short Break",
            self.fmt_13_med.as_ref(),
            color(1.0, 1.0, 1.0, if sb_active { 1.0 } else { 0.75 }),
            POMODORO_SHORT_BREAK_RECT.0 + 28.0,
            POMODORO_SHORT_BREAK_RECT.1 + 5.0,
        );

        // 2. Play/Pause pill (`width: 54`, mode color)
        self.fill_rounded(
            target,
            POMODORO_PLAY_RECT.0,
            POMODORO_PLAY_RECT.1,
            POMODORO_PLAY_RECT.2 - POMODORO_PLAY_RECT.0,
            28.0,
            14.0,
            mode_c,
        );
        let play_cx = POMODORO_PLAY_RECT.0 + (POMODORO_PLAY_RECT.2 - POMODORO_PLAY_RECT.0) * 0.5;
        let play_cy = POMODORO_PLAY_RECT.1 + 14.0;
        if pomodoro.running {
            self.draw_icon_pause(target, play_cx, play_cy, 14.0, color(1.0, 1.0, 1.0, 1.0));
        } else {
            self.draw_icon_play(target, play_cx, play_cy, 14.0, color(1.0, 1.0, 1.0, 1.0));
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
            POMODORO_FOCUS_RECT.0 + 16.0,
            POMODORO_FOCUS_RECT.1 + 14.0,
            color(1.0, 1.0, 1.0, if fc_active { 1.0 } else { 0.75 }),
        );
        self.draw_text_at(
            target,
            "Focus Time",
            self.fmt_13_med.as_ref(),
            color(1.0, 1.0, 1.0, if fc_active { 1.0 } else { 0.75 }),
            POMODORO_FOCUS_RECT.0 + 28.0,
            POMODORO_FOCUS_RECT.1 + 5.0,
        );
    }

    // --- Todo Section (`drawTodo`, `520x180`, `IslandView.swift` lines 2472-2541) ---

    fn draw_todo(
        &self,
        target: &ID2D1RenderTarget,
        todo: &crate::todo::TodoList,
        todo_editing: bool,
        todo_input: &str,
        _playing: bool,
        todo_scroll: f32,
        _todo_editing_idx: Option<usize>,
    ) {
        self.draw_text_at(
            target,
            "Todo",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            54.0,
            36.0,
        );

        let total = todo.tasks.len();
        let done_count = todo.done_count();
        if total > 0 {
            self.draw_text_at(
                target,
                &format!("{}/{}", done_count, total),
                self.fmt_28_bold.as_ref(),
                color(1.0, 1.0, 1.0, 1.0),
                46.0,
                58.0,
            );
            self.draw_text_at(
                target,
                "done",
                self.fmt_13_med.as_ref(),
                color(1.0, 1.0, 1.0, 0.60),
                46.0,
                92.0,
            );
        } else {
            self.draw_text_at(
                target,
                "0/0",
                self.fmt_28_bold.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                46.0,
                58.0,
            );
            self.draw_text_at(
                target,
                "tasks",
                self.fmt_13_med.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                46.0,
                92.0,
            );
        }

        // Left control: "+ Add task" pill OR Form input (never overlaps, hides button when input active):
        if !todo_editing {
            let px = TODO_ADD_PILL_RECT.0;
            let py = TODO_ADD_PILL_RECT.1;
            let pw = TODO_ADD_PILL_RECT.2 - px;
            let ph = TODO_ADD_PILL_RECT.3 - py;
            self.fill_rounded(
                target,
                px,
                py,
                pw,
                ph,
                ph * 0.5,
                color(1.0, 1.0, 1.0, 0.10),
            );
            let btn_txt = "+ Add task";
            if let Some(fmt) = self.fmt_13_med.as_ref() {
                let (tw, th) = self.measure_text(btn_txt, fmt);
                self.draw_text_at(
                    target,
                    btn_txt,
                    Some(fmt),
                    color(1.0, 1.0, 1.0, 1.0),
                    px + (pw - tw) * 0.5,
                    py + (ph - th) * 0.5,
                );
            }
        } else {
            // Input form replaces the button cleanly:
            let input_w = TODO_INPUT_RECT.2 - TODO_INPUT_RECT.0;
            self.fill_rounded(
                target,
                TODO_INPUT_RECT.0,
                TODO_INPUT_RECT.1,
                input_w,
                32.0,
                10.0,
                color(1.0, 1.0, 1.0, 0.12),
            );
            self.stroke_rounded(
                target,
                TODO_INPUT_RECT.0,
                TODO_INPUT_RECT.1,
                input_w,
                32.0,
                10.0,
                1.0,
                color(1.0, 1.0, 1.0, 0.30),
                None,
            );
            let (shown, c) = if todo_input.is_empty() {
                ("Task name...".to_string(), color(1.0, 1.0, 1.0, 0.40))
            } else {
                (format!("{}|", todo_input), color(1.0, 1.0, 1.0, 1.0))
            };
            if let Some(fmt) = self.fmt_13_reg.as_ref() {
                let text_s = self.truncate_text(&shown, input_w - 16.0, fmt);
                self.draw_text_at(
                    target,
                    &text_s,
                    Some(fmt),
                    c,
                    TODO_INPUT_RECT.0 + 8.0,
                    TODO_INPUT_RECT.1 + 7.5,
                );
            }
            self.draw_text_at(
                target,
                "Enter: save · Esc: cancel",
                self.fmt_10_med.as_ref(),
                color(1.0, 1.0, 1.0, 0.45),
                TODO_INPUT_RECT.0,
                TODO_INPUT_RECT.1 + 36.0,
            );
        }

        // Empty state illustration when zero tasks:
        if total == 0 && !todo_editing {
            let card_x = 166.0f32;
            let card_y = 44.0f32;
            let card_w = 320.0f32;
            let card_h = 104.0f32;
            self.fill_rounded(target, card_x, card_y, card_w, card_h, 14.0, color(1.0, 1.0, 1.0, 0.05));
            self.stroke_rounded(
                target,
                card_x,
                card_y,
                card_w,
                card_h,
                14.0,
                1.0,
                color(1.0, 1.0, 1.0, 0.14),
                self.stroke_dashed.as_ref(),
            );
            let icon_cx = card_x + 36.0;
            let icon_cy = card_y + card_h * 0.5;
            self.fill_oval(
                target,
                icon_cx - 16.0,
                icon_cy - 16.0,
                32.0,
                32.0,
                color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 0.18),
            );
            self.stroke_oval(
                target,
                icon_cx - 16.0,
                icon_cy - 16.0,
                32.0,
                32.0,
                1.5,
                color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 0.60),
            );
            self.draw_icon_checkmark(
                target,
                icon_cx,
                icon_cy,
                color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0),
            );
            let text_x = card_x + 64.0;
            self.draw_text_at(
                target,
                "All caught up!",
                self.fmt_15_semi.as_ref(),
                color(1.0, 1.0, 1.0, 1.0),
                text_x,
                card_y + 24.0,
            );
            self.draw_text_at(
                target,
                "No tasks pending for today",
                self.fmt_12_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.60),
                text_x,
                card_y + 46.0,
            );
            self.draw_text_at(
                target,
                "Click \"+ Add task\" to plan ahead",
                self.fmt_11_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                text_x,
                card_y + 66.0,
            );
            return;
        }

        // Right column scrollable task list:
        let list_clip = D2D_RECT_F {
            left: 156.0,
            top: 42.0,
            right: 494.0,
            bottom: 158.0,
        };
        unsafe {
            target.PushAxisAlignedClip(&list_clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }

        for (row_i, task) in todo.tasks.iter().enumerate() {
            let y = TODO_ROW_Y0 + (row_i as f32) * TODO_ROW_PITCH - todo_scroll;
            if y + TODO_ROW_H < 40.0 || y > 160.0 {
                continue;
            }
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
            let cb_x = TODO_ROW_X + 12.0;
            if task.done {
                self.fill_oval(
                    target,
                    cb_x,
                    cy - 10.0,
                    20.0,
                    20.0,
                    color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 1.0),
                );
                self.draw_icon_checkmark(target, cb_x + 10.0, cy, color(1.0, 1.0, 1.0, 1.0));
            } else {
                self.stroke_oval(
                    target,
                    cb_x,
                    cy - 10.0,
                    20.0,
                    20.0,
                    1.5,
                    color(1.0, 1.0, 1.0, 0.60),
                );
            }
            if let Some(fmt) = self.fmt_14_reg.as_ref() {
                let text = self.truncate_text(&task.title, TODO_ROW_W - 84.0, fmt);
                let text_x = cb_x + 28.0;
                if task.done {
                    self.draw_strikethrough_text_at(
                        target,
                        &text,
                        Some(fmt),
                        color(1.0, 1.0, 1.0, 0.50),
                        text_x,
                        y + 6.5,
                    );
                } else {
                    self.draw_text_at(
                        target,
                        &text,
                        Some(fmt),
                        color(1.0, 1.0, 1.0, 1.0),
                        text_x,
                        y + 6.5,
                    );
                }
            }
            // Edit button "✎"
            self.draw_text_at(
                target,
                "✎",
                self.fmt_12_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.45),
                TODO_ROW_EDIT_LEFT + 6.0,
                cy - 8.0,
            );
            // Delete button "×"
            self.draw_text_at(
                target,
                "×",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.40),
                TODO_ROW_DELETE_LEFT + 6.0,
                cy - 9.5,
            );
        }

        unsafe {
            target.PopAxisAlignedClip();
        }
    }

    // --- Water Reminder Section (`drawWater`, `520x180`) ---

    fn draw_water(&self, target: &ID2D1RenderTarget, water: &crate::water::Water, _playing: bool) {
        // Header Title: "Water " white + "Reminder" blue:
        if let Some(fmt18) = self.fmt_18_med.as_ref() {
            let left_x = 54.0f32;
            let top_y = 36.0f32;
            self.draw_text_at(target, "Water ", Some(fmt18), color(1.0, 1.0, 1.0, 1.0), left_x, top_y);
            let (water_w, _) = self.measure_text("Water ", fmt18);
            self.draw_text_at(
                target,
                "Reminder",
                Some(fmt18),
                color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
                left_x + water_w,
                top_y,
            );
        }

        // --- Left column (Intake, toggle, progress, buttons) ---
        // Big liter number + `/ 2.0 L`:
        let total_str = crate::water::Water::format_liters(water.ml);
        let goal_str = format!("/ {} L", crate::water::Water::format_liters(water.goal_ml));
        if let Some(fmt28) = self.fmt_28_bold.as_ref() {
            self.draw_text_at(target, &total_str, Some(fmt28), color(1.0, 1.0, 1.0, 1.0), 54.0, 56.0);
            let (tw, _) = self.measure_text(&total_str, fmt28);
            self.draw_text_at(
                target,
                &goal_str,
                self.fmt_14_med.as_ref(),
                color(1.0, 1.0, 1.0, 0.50),
                54.0 + tw + 6.0,
                66.0,
            );
        }

        // Notification toggle (`WATER_TOGGLE_RECT: 188..238, 52..80`):
        let on = water.notifications_enabled;
        let toggle_fill = if on {
            color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0)
        } else {
            color(1.0, 1.0, 1.0, 0.18)
        };
        let (tg_x0, tg_y0, tg_x1, tg_y1) = WATER_TOGGLE_RECT;
        let tg_w = tg_x1 - tg_x0;
        let tg_h = tg_y1 - tg_y0;
        self.fill_rounded(target, tg_x0, tg_y0, tg_w, tg_h, tg_h * 0.5, toggle_fill);
        let knob_size = tg_h - 4.0;
        let knob_x = if on { tg_x1 - 2.0 - knob_size } else { tg_x0 + 2.0 };
        let knob_y = tg_y0 + 2.0;
        self.fill_oval(target, knob_x, knob_y, knob_size, knob_size, color(1.0, 1.0, 1.0, 1.0));
        let bell_c = if on {
            color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0)
        } else {
            color(0.0, 0.0, 0.0, 0.45)
        };
        self.draw_icon_bell(target, knob_x + knob_size * 0.5, knob_y + knob_size * 0.5, bell_c);

        // `"Today · {percent}%"` at `y = 86`:
        let pct = if water.goal_ml > 0 {
            ((water.ml as f64 / water.goal_ml as f64) * 100.0).round() as i64
        } else {
            0
        };
        if let (Some(f_med), Some(f_bold)) = (self.fmt_13_med.as_ref(), self.fmt_13_bold.as_ref()) {
            let mut lx = 54.0f32;
            let s1 = "Today · ";
            self.draw_text_at(target, s1, Some(f_med), color(1.0, 1.0, 1.0, 0.60), lx, 86.0);
            lx += self.measure_text(s1, f_med).0;
            let s2 = format!("{}%", pct);
            self.draw_text_at(
                target,
                &s2,
                Some(f_bold),
                color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
                lx,
                86.0,
            );
            lx += self.measure_text(&s2, f_bold).0;
            if pct >= 100 {
                self.draw_text_at(
                    target,
                    " · Goal reached",
                    Some(f_med),
                    color(SYSTEM_GREEN.0, SYSTEM_GREEN.1, SYSTEM_GREEN.2, 0.90),
                    lx,
                    86.0,
                );
            }
        }

        // Progress bar (`x: 54, y: 106, w: 186, h: 5`):
        self.fill_rounded(target, 54.0, 106.0, 186.0, 5.0, 2.5, color(1.0, 1.0, 1.0, 0.12));
        let frac = if water.goal_ml > 0 {
            (water.ml as f32 / water.goal_ml as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if frac > 0.0 {
            let fill_w = 186.0 * frac;
            let r = 2.5f32.min(fill_w * 0.5);
            self.fill_linear_gradient_rounded(
                target,
                54.0,
                106.0,
                fill_w,
                5.0,
                r,
                (54.0, 106.0),
                (54.0 + fill_w, 106.0),
                color(LIGHT_BLUE.0, LIGHT_BLUE.1, LIGHT_BLUE.2, 1.0),
                color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
            );
        }

        // Bottom pills: `Drink` (blue) + `Settings` (dark):
        let (dr_x0, dr_y0, dr_x1, dr_y1) = WATER_DRINK_RECT;
        let dr_w = dr_x1 - dr_x0;
        let dr_h = dr_y1 - dr_y0;
        self.fill_rounded(
            target,
            dr_x0,
            dr_y0,
            dr_w,
            dr_h,
            dr_h * 0.5,
            color(SYSTEM_BLUE.0, SYSTEM_BLUE.1, SYSTEM_BLUE.2, 1.0),
        );
        self.draw_icon_drop(target, dr_x0 + 16.0, dr_y0 + dr_h * 0.5, color(1.0, 1.0, 1.0, 1.0));
        self.draw_text_at(
            target,
            "Drink",
            self.fmt_13_med.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            dr_x0 + 29.0,
            dr_y0 + (dr_h - 14.0) * 0.5 - 1.0,
        );

        let (st_x0, st_y0, st_x1, st_y1) = WATER_SETTINGS_RECT;
        let st_w = st_x1 - st_x0;
        let st_h = st_y1 - st_y0;
        self.fill_rounded(
            target,
            st_x0,
            st_y0,
            st_w,
            st_h,
            st_h * 0.5,
            color(1.0, 1.0, 1.0, 0.10),
        );
        self.draw_icon_sliders(
            target,
            st_x0 + 17.0,
            st_y0 + st_h * 0.5,
            color(1.0, 1.0, 1.0, 0.75),
        );
        self.draw_text_at(
            target,
            "Settings",
            self.fmt_13_med.as_ref(),
            color(1.0, 1.0, 1.0, 0.75),
            st_x0 + 31.0,
            st_y0 + (st_h - 14.0) * 0.5 - 1.0,
        );

        // Vertical divider between columns:
        if let Some(b_div) = self.brush(target, color(1.0, 1.0, 1.0, 0.08)) {
            unsafe {
                target.DrawLine(
                    D2D_POINT_2F { x: 252.0, y: 52.0 },
                    D2D_POINT_2F { x: 252.0, y: 148.0 },
                    &b_div,
                    1.0,
                    None,
                );
            }
        }

        // --- Right column: 24-hour bar chart (scaled down and padded) ---
        let max_hour = water.hourly.iter().copied().max().unwrap_or(0);
        let scale = max_hour.max(400);
        let bars_left = 314.0f32;
        let bars_w = 168.0f32;
        let baseline = 136.0f32;
        let chart_h = 58.0f32;
        let slot_w = bars_w / 24.0;
        let label_c = color(1.0, 1.0, 1.0, 0.45);

        self.draw_right_aligned_text(
            target,
            &format!("{}", scale),
            self.fmt_10_med.as_ref(),
            label_c,
            308.0,
            baseline - chart_h - 5.0,
        );
        self.draw_right_aligned_text(
            target,
            &format!("{}", scale / 2),
            self.fmt_10_med.as_ref(),
            label_c,
            308.0,
            baseline - chart_h * 0.5 - 5.0,
        );
        self.draw_right_aligned_text(
            target,
            "0 ml",
            self.fmt_10_med.as_ref(),
            label_c,
            308.0,
            baseline - 5.0,
        );

        if let Some(b_grid) = self.brush(target, color(1.0, 1.0, 1.0, 0.12)) {
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
        if let Some(b_axis) = self.brush(target, color(1.0, 1.0, 1.0, 0.22)) {
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
            let bar_w = 4.8f32;
            let bar_x = slot_x + (slot_w - bar_w) * 0.5;
            let h = ((ml as f32 / scale as f32) * chart_h).clamp(2.5, chart_h);
            let r = 2.0f32.min(h * 0.5);
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
                self.draw_text_at(target, &s, Some(fmt10), label_c, cx - w * 0.5, 142.0);
            }
        }
    }

    // --- Tray Section (`drawTray` & `drawTrayDropZone`, `640x280`, `IslandView.swift` lines 4014-4267) ---

    fn draw_tray_drop_zone(
        &self,
        target: &ID2D1RenderTarget,
        shelf: &crate::shelf::Shelf,
        _playing: bool,
        tray_drag_pt: Option<(f32, f32)>,
        lh: f32,
    ) {
        self.draw_text_at(
            target,
            "Tray",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            54.0,
            26.0,
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

        // Drop zone within pill boundaries (x: 54, y: 62, width: 432, height: zone_h):
        let zone_top = 62.0f32;
        let zone_bottom = (lh - 24.0).max(zone_top);
        let zone_h = (zone_bottom - zone_top).min(110.0);
        let zone_r = 14.0f32.min(zone_h * 0.5);

        // Radial cursor-following light glow
        if let Some((gx, gy)) = tray_drag_pt {
            if zone_h > 10.0 {
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
                                        left: 54.0,
                                        top: zone_top,
                                        right: 486.0,
                                        bottom: zone_top + zone_h,
                                    },
                                    radiusX: zone_r,
                                    radiusY: zone_r,
                                },
                                &rbrush,
                            );
                        }
                    }
                }
            }
        }

        if zone_h > 10.0 {
            self.stroke_rounded(
                target,
                54.0,
                zone_top,
                432.0,
                zone_h,
                zone_r,
                1.5,
                color(1.0, 1.0, 1.0, 0.50),
                self.stroke_dashed.as_ref(),
            );
            if zone_h > 50.0 {
                self.draw_centered_text(
                    target,
                    "Release to add",
                    self.fmt_14_reg.as_ref(),
                    color(1.0, 1.0, 1.0, 0.70),
                    540.0,
                    zone_top + zone_h * 0.5 - 7.0,
                );
            }
        }

        // Cursor-following "+ Copy" pill badge:
        if let Some((gx, gy)) = tray_drag_pt {
            if let Some(fmt) = self.fmt_13_bold.as_ref() {
                let label = "+ Copy";
                let (tw, th) = self.measure_text(label, fmt);
                let pad_x = 8.0f32;
                let badge_w = tw + pad_x * 2.0;
                let badge_h = 22.0f32;
                let bx = (gx + 14.0).clamp(8.0, 520.0 - badge_w - 8.0);
                let by = (gy + 6.0).clamp(8.0, 180.0 - badge_h - 8.0);
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

    /// Aspect-fit rect for a `w`x`h` image inside a box, centered.
    fn fit_rect(bx0: f32, by0: f32, bx1: f32, by1: f32, w: u32, h: u32) -> D2D_RECT_F {
        if w == 0 || h == 0 {
            return D2D_RECT_F {
                left: bx0,
                top: by0,
                right: bx1,
                bottom: by1,
            };
        }
        let bw = bx1 - bx0;
        let bh = by1 - by0;
        let s = (bw / w as f32).min(bh / h as f32);
        let dw = w as f32 * s;
        let dh = h as f32 * s;
        let x0 = bx0 + (bw - dw) * 0.5;
        let y0 = by0 + (bh - dh) * 0.5;
        D2D_RECT_F {
            left: x0,
            top: y0,
            right: x0 + dw,
            bottom: y0 + dh,
        }
    }

    fn draw_checkerboard(
        &self,
        target: &ID2D1RenderTarget,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) {
        let sq = 12.0;
        let mut row = 0;
        let mut y = y0;
        while y < y1 {
            let mut col = 0;
            let mut x = x0;
            while x < x1 {
                let light = (row + col) % 2 == 0;
                let c = if light {
                    color(1.0, 1.0, 1.0, 0.10)
                } else {
                    color(1.0, 1.0, 1.0, 0.04)
                };
                let w = (x + sq).min(x1) - x;
                let h = (y + sq).min(y1) - y;
                self.fill_rounded(target, x, y, w, h, 0.0, c);
                x += sq;
                col += 1;
            }
            y += sq;
            row += 1;
        }
    }

    fn draw_removebg(
        &self,
        target: &ID2D1RenderTarget,
        rb: &crate::removebg::RemoveBgState,
        shelf: &crate::shelf::Shelf,
        drop_highlight: bool,
        mouse_pt: Option<(f32, f32)>,
        ctx: &RenderContext,
    ) {
        let in_rect = |r: (f32, f32, f32, f32)| {
            mouse_pt.map(|(mx, my)| mx >= r.0 && mx <= r.2 && my >= r.1 && my <= r.3).unwrap_or(false)
        };

        self.draw_text_at(
            target,
            "Remove BG",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            54.0,
            26.0,
        );

        let _tray_usable = !crate::removebg::tray_images(shelf).is_empty();

        // Header controls (Model + Reset expanding oval buttons):
        if rb.input_path.is_some() || rb.result_bytes.is_some() {
            let rb_rects = compute_rb_button_rects(&ctx.rb_btn_hovers, rb.model.label().len());
            let model_label = format!("Model: {}", rb.model.label());
            self.draw_expanding_button(target, rb_rects[0], ctx.rb_btn_hovers[0], &model_label, false, &|cx, cy, c| {
                self.draw_icon_sliders(target, cx, cy, c);
            });
            self.draw_expanding_button(target, rb_rects[1], ctx.rb_btn_hovers[1], "Reset", false, &|cx, cy, c| {
                self.draw_icon_trash(target, cx, cy, c);
            });
        }

        if rb.queue.len() > 1 {
            let pq_hov = in_rect(RB_QUEUE_PREV_RECT);
            self.draw_icon_button(target, RB_QUEUE_PREV_RECT, pq_hov, "Previous File", false, &|cx, cy, c| {
                self.draw_icon_chevron(target, cx, cy, false, c);
            });

            let queue_str = format!("{}/{}", rb.queue_idx + 1, rb.queue.len());
            if let Some(f12) = self.fmt_12_reg.as_ref() {
                let (qw, qh) = self.measure_text(&queue_str, f12);
                let mid_x = (RB_QUEUE_PREV_RECT.2 + RB_QUEUE_NEXT_RECT.0) * 0.5;
                let mid_y = (RB_QUEUE_PREV_RECT.1 + RB_QUEUE_PREV_RECT.3) * 0.5;
                self.draw_text_at(target, &queue_str, Some(f12), color(1.0, 1.0, 1.0, 0.90), mid_x - qw * 0.5, mid_y - qh * 0.5);
            }

            let nq_hov = in_rect(RB_QUEUE_NEXT_RECT);
            self.draw_icon_button(target, RB_QUEUE_NEXT_RECT, nq_hov, "Next File", false, &|cx, cy, c| {
                self.draw_icon_chevron(target, cx, cy, true, c);
            });
        }

        if rb.input_path.is_none() {
            // Initial dashed drop zone
            let (dx0, dy0, dx1, dy1) = RB_DROP_ZONE_RECT;
            let dw = dx1 - dx0;
            let dh = dy1 - dy0;
            let fill_a = if drop_highlight { 0.15 } else { 0.05 };
            self.fill_rounded(target, dx0, dy0, dw, dh, 16.0, color(1.0, 1.0, 1.0, fill_a));
            self.stroke_rounded(target, dx0, dy0, dw, dh, 16.0, 1.5, color(1.0, 1.0, 1.0, 0.40), self.stroke_dashed.as_ref());
            let cx = dx0 + dw * 0.5;
            let cy = dy0 + dh * 0.5;
            self.draw_icon_doc(target, cx, cy - 20.0, 30.0, color(1.0, 1.0, 1.0, 0.60));
            if let Some(f14) = self.fmt_14_reg.as_ref() {
                let msg = "Drop image here or click to pick";
                let (w, _) = self.measure_text(msg, f14);
                self.draw_text_at(target, msg, Some(f14), color(1.0, 1.0, 1.0, 0.85), cx - w * 0.5, cy + 2.0);
            }
            if let Some(f12) = self.fmt_12_reg.as_ref() {
                let sub = "PNG, JPG, WEBP, BMP";
                let (w, _) = self.measure_text(sub, f12);
                self.draw_text_at(target, sub, Some(f12), color(1.0, 1.0, 1.0, 0.45), cx - w * 0.5, cy + 22.0);
            }
            return;
        }

        // Left side: original image preview
        let (lx0, ly0, lx1, ly1) = RB_LEFT_PREVIEW_RECT;
        let lw = lx1 - lx0;
        let lh_box = ly1 - ly0;
        self.fill_rounded(target, lx0, ly0, lw, lh_box, 12.0, color(1.0, 1.0, 1.0, 0.06));
        self.stroke_rounded(target, lx0, ly0, lw, lh_box, 12.0, 1.0, color(1.0, 1.0, 1.0, 0.20), None);
        if let Some((_, pw, ph)) = &rb.input_preview {
            if let Some(bmp) = &self.removebg_in_bitmap {
                let rect = Self::fit_rect(lx0 + 6.0, ly0 + 6.0, lx1 - 6.0, ly1 - 6.0, *pw, *ph);
                unsafe {
                    target.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
            }
        }

        // Right side: result / process button
        let (rx0, ry0, rx1, ry1) = RB_RIGHT_PREVIEW_RECT;
        let rw = rx1 - rx0;
        let rh_box = ry1 - ry0;
        let rcx = rx0 + rw * 0.5;
        let rcy = ry0 + rh_box * 0.5;

        if let Some((_, pw, ph)) = &rb.result_preview {
            // Cutout result ready
            self.draw_checkerboard(target, rx0, ry0, rx1, ry1);
            self.stroke_rounded(target, rx0, ry0, rw, rh_box, 12.0, 1.0, color(1.0, 1.0, 1.0, 0.30), None);
            if let Some(bmp) = &self.removebg_bitmap {
                let rect = Self::fit_rect(rx0 + 6.0, ry0 + 6.0, rx1 - 6.0, ry1 - 6.0, *pw, *ph);
                unsafe {
                    target.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
            }
            let save_hov = in_rect(RB_SAVE_BTN_RECT);
            self.draw_icon_button(target, RB_SAVE_BTN_RECT, save_hov, "Save Image", true, &|cx, cy, c| {
                self.draw_icon_download(target, cx, cy, 18.0, c);
            });
        } else if rb.uploading {
            self.fill_rounded(target, rx0, ry0, rw, rh_box, 12.0, color(1.0, 1.0, 1.0, 0.06));
            self.stroke_rounded(target, rx0, ry0, rw, rh_box, 12.0, 1.0, color(1.0, 1.0, 1.0, 0.20), None);
            if let Some(f14) = self.fmt_14_reg.as_ref() {
                let msg = "Removing background...";
                let (w, _) = self.measure_text(msg, f14);
                self.draw_text_at(target, msg, Some(f14), color(1.0, 1.0, 1.0, 0.90), rcx - w * 0.5, rcy - 8.0);
            }
        } else if let Some(err) = &rb.error {
            self.fill_rounded(target, rx0, ry0, rw, rh_box, 12.0, color(1.0, 0.2, 0.2, 0.10));
            self.stroke_rounded(target, rx0, ry0, rw, rh_box, 12.0, 1.0, color(1.0, 0.3, 0.3, 0.40), None);
            if let Some(f13) = self.fmt_13_reg.as_ref() {
                let shown = self.truncate_text(err, rw - 24.0, f13);
                let (w, _) = self.measure_text(&shown, f13);
                self.draw_text_at(target, &shown, Some(f13), color(1.0, 0.45, 0.45, 1.0), rcx - w * 0.5, rcy - 8.0);
            }
        } else {
            // File loaded, ready to process
            self.fill_rounded(target, rx0, ry0, rw, rh_box, 12.0, color(1.0, 1.0, 1.0, 0.04));
            self.stroke_rounded(target, rx0, ry0, rw, rh_box, 12.0, 1.0, color(1.0, 1.0, 1.0, 0.15), None);

            let proc_hov = in_rect(RB_PROCESS_BTN_RECT);
            self.draw_icon_button(target, RB_PROCESS_BTN_RECT, proc_hov, "Remove Background", true, &|cx, cy, c| {
                self.draw_icon_bolt(target, cx, cy, 20.0, c);
            });
            if let Some(f12) = self.fmt_12_reg.as_ref() {
                let hint = "Ready to process";
                let (hw, _) = self.measure_text(hint, f12);
                self.draw_text_at(target, hint, Some(f12), color(1.0, 1.0, 1.0, 0.50), rcx - hw * 0.5, rcy + 26.0);
            }
        }
    }

    fn draw_convert(
        &self,
        target: &ID2D1RenderTarget,
        cv: &crate::convert::ConvertState,
        _shelf: &crate::shelf::Shelf,
        _playing: bool,
        mouse_pt: Option<(f32, f32)>,
        ctx: &RenderContext,
    ) {
        let in_rect = |r: (f32, f32, f32, f32)| {
            mouse_pt.map(|(mx, my)| mx >= r.0 && mx <= r.2 && my >= r.1 && my <= r.3).unwrap_or(false)
        };

        if cv.view == crate::convert::ConvertView::AppGrid {
            self.draw_text_at(
                target,
                "Convert Tools",
                self.fmt_20_bold.as_ref(),
                color(1.0, 1.0, 1.0, 1.0),
                46.0,
                28.0,
            );

            // 3 App Cards with user-provided Flame Character icons:
            let apps = [
                ("Photo", "Convert images", 0usize),
                ("Merge PDF", "Combine docs", 1usize),
                ("PDF to MD", "Extract text", 2usize),
            ];

            for (i, (title, subtitle, icon_idx)) in apps.iter().enumerate() {
                let card_x = CONVERT_APP_START_X + (i as f32) * CONVERT_APP_PITCH;
                let card_y = CONVERT_APP_Y;
                let card_w = CONVERT_APP_CARD_W;
                let card_h = CONVERT_APP_CARD_H;

                let is_hovered = mouse_pt.map(|(mx, my)| mx >= card_x && mx <= card_x + card_w && my >= card_y && my <= card_y + card_h).unwrap_or(false);
                let bg_alpha = if is_hovered { 0.14 } else { 0.07 };
                let stroke_alpha = if is_hovered { 0.35 } else { 0.18 };

                self.fill_rounded(target, card_x, card_y, card_w, card_h, 16.0, color(1.0, 1.0, 1.0, bg_alpha));
                self.stroke_rounded(target, card_x, card_y, card_w, card_h, 16.0, 1.0, color(1.0, 1.0, 1.0, stroke_alpha), None);

                let icon_size = CONVERT_APP_ICON_SIZE;
                let icon_x = card_x + (card_w - icon_size) * 0.5;
                let icon_y = card_y + 8.0;
                if let Some(bmp) = &self.convert_app_bitmaps[*icon_idx] {
                    let dst = D2D_RECT_F {
                        left: icon_x,
                        top: icon_y,
                        right: icon_x + icon_size,
                        bottom: icon_y + icon_size,
                    };
                    unsafe {
                        target.DrawBitmap(
                            bmp,
                            Some(&dst),
                            1.0,
                            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            None,
                        );
                    }
                }

                if let Some(f13b) = self.fmt_13_bold.as_ref() {
                    let (tw, _) = self.measure_text(title, f13b);
                    self.draw_text_at(target, title, Some(f13b), color(1.0, 1.0, 1.0, 1.0), card_x + (card_w - tw) * 0.5, icon_y + icon_size + 4.0);
                }
                if let Some(f11) = self.fmt_11_reg.as_ref() {
                    let (sw, _) = self.measure_text(subtitle, f11);
                    self.draw_text_at(target, subtitle, Some(f11), color(1.0, 1.0, 1.0, 0.55), card_x + (card_w - sw) * 0.5, icon_y + icon_size + 22.0);
                }
            }
            return;
        }

        // Tool Detail View:
        let tool = cv.active_tool;
        if tool == crate::convert::ConvertTool::MergePdf {
            self.draw_merge_pdf_view(target, cv, mouse_pt, ctx);
            return;
        }
        if tool == crate::convert::ConvertTool::PdfToMd {
            self.draw_pdf_to_md_view(target, cv, mouse_pt, ctx);
            return;
        }
        let st = cv.tool_state(tool);

        let cv_rects = compute_cv_button_rects(
            &ctx.cv_btn_hovers,
            cv.photo_format.display_name().len(),
            tool == crate::convert::ConvertTool::Photo,
        );

        // Button 0: Back
        self.draw_expanding_button(target, cv_rects[0], ctx.cv_btn_hovers[0], "Back", false, &|cx, cy, c| {
            self.draw_icon_chevron(target, cx, cy, false, c);
        });

        let title_name = match tool {
            crate::convert::ConvertTool::Photo => "Photo Converter",
            crate::convert::ConvertTool::MergePdf => "Merge PDFs",
            crate::convert::ConvertTool::PdfToMd => "PDF to Markdown",
            _ => "Converter",
        };
        let title_x = cv_rects[0].2 + 14.0;
        self.draw_text_at(target, title_name, self.fmt_18_med.as_ref(), color(1.0, 1.0, 1.0, 1.0), title_x, 28.0);

        // Queue switcher if queue has multiple files:
        if st.queue.len() > 1 {
            let pq_hov = in_rect(CONVERT_QUEUE_PREV_RECT);
            self.draw_icon_button(target, CONVERT_QUEUE_PREV_RECT, pq_hov, "Previous File", false, &|cx, cy, c| {
                self.draw_icon_chevron(target, cx, cy, false, c);
            });

            let queue_str = format!("{}/{}", st.queue_idx + 1, st.queue.len());
            if let Some(f12) = self.fmt_12_reg.as_ref() {
                let (qw, qh) = self.measure_text(&queue_str, f12);
                let mid_x = (CONVERT_QUEUE_PREV_RECT.2 + CONVERT_QUEUE_NEXT_RECT.0) * 0.5;
                let mid_y = (CONVERT_QUEUE_PREV_RECT.1 + CONVERT_QUEUE_PREV_RECT.3) * 0.5;
                self.draw_text_at(target, &queue_str, Some(f12), color(1.0, 1.0, 1.0, 0.90), mid_x - qw * 0.5, mid_y - qh * 0.5);
            }

            let nq_hov = in_rect(CONVERT_QUEUE_NEXT_RECT);
            self.draw_icon_button(target, CONVERT_QUEUE_NEXT_RECT, nq_hov, "Next File", false, &|cx, cy, c| {
                self.draw_icon_chevron(target, cx, cy, true, c);
            });
        }

        if tool == crate::convert::ConvertTool::Photo {
            let fmt_label = format!("Format: {}", cv.photo_format.display_name());
            self.draw_expanding_button(target, cv_rects[1], ctx.cv_btn_hovers[1], &fmt_label, false, &|cx, cy, c| {
                self.draw_icon_sliders(target, cx, cy, c);
            });
        }

        self.draw_expanding_button(target, cv_rects[2], ctx.cv_btn_hovers[2], "Reset", false, &|cx, cy, c| {
            self.draw_icon_trash(target, cx, cy, c);
        });

        let has_content = cv.has_active_content();

        // Top: Dashed Drop Zone
        let (zx0, zy0, zx1, zy1) = if has_content {
            CONVERT_DROP_ZONE_RECT
        } else {
            (54.0, 58.0, 486.0, 154.0)
        };
        let zw = zx1 - zx0;
        let zh = zy1 - zy0;
        let zcx = zx0 + zw * 0.5;
        let zcy = zy0 + zh * 0.5;
        self.fill_rounded(target, zx0, zy0, zw, zh, 14.0, color(1.0, 1.0, 1.0, 0.04));
        self.stroke_rounded(target, zx0, zy0, zw, zh, 14.0, 1.5, color(1.0, 1.0, 1.0, 0.35), self.stroke_dashed.as_ref());

        let drop_msg = match tool {
            crate::convert::ConvertTool::Photo => "Drop image here or click to pick",
            crate::convert::ConvertTool::MergePdf => "Drop PDFs here or click to pick",
            crate::convert::ConvertTool::PdfToMd => "Drop PDF here or click to pick",
            _ => "Drop files here",
        };
        if let Some(f13) = self.fmt_13_med.as_ref() {
            let (w, _) = self.measure_text(drop_msg, f13);
            self.draw_text_at(target, drop_msg, Some(f13), color(1.0, 1.0, 1.0, 0.85), zcx - w * 0.5, zcy - 8.0);
        }

        // Bottom: List View / Queue + Action Buttons (ONLY when file is present)
        if has_content {
            let (lx0, ly0, lx1, ly1) = CONVERT_LIST_VIEW_RECT;
            let lw_box = lx1 - lx0;
            let lh_box = ly1 - ly0;
            self.fill_rounded(target, lx0, ly0, lw_box, lh_box, 12.0, color(1.0, 1.0, 1.0, 0.06));
            self.stroke_rounded(target, lx0, ly0, lw_box, lh_box, 12.0, 1.0, color(1.0, 1.0, 1.0, 0.15), None);

            if let Some(p) = &st.input_path {
                let p_name = std::path::Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or(p);
                if let Some(f13) = self.fmt_13_bold.as_ref() {
                    let shown = self.truncate_text(p_name, lw_box - 70.0, f13);
                    self.draw_text_at(target, &shown, Some(f13), color(1.0, 1.0, 1.0, 1.0), lx0 + 12.0, ly0 + 8.0);
                }
                if st.status_is_error {
                    if let Some(f12) = self.fmt_12_reg.as_ref() {
                        self.draw_text_at(target, &st.status_text, Some(f12), color(1.0, 0.35, 0.35, 1.0), lx0 + 12.0, ly0 + 26.0);
                    }
                } else if st.uploading {
                    if let Some(f12) = self.fmt_12_reg.as_ref() {
                        self.draw_text_at(target, "Converting...", Some(f12), color(1.0, 1.0, 1.0, 0.70), lx0 + 12.0, ly0 + 26.0);
                    }
                } else if st.result_bytes.is_some() {
                    if let Some(f12) = self.fmt_12_reg.as_ref() {
                        self.draw_text_at(target, "Ready! Click Save icon to export.", Some(f12), color(0.4, 1.0, 0.5, 1.0), lx0 + 12.0, ly0 + 26.0);
                    }
                } else {
                    if let Some(f12) = self.fmt_12_reg.as_ref() {
                        self.draw_text_at(target, "Ready to convert. Click Process icon.", Some(f12), color(1.0, 1.0, 1.0, 0.60), lx0 + 12.0, ly0 + 26.0);
                    }
                }
            } else if !cv.merge_files.is_empty() {
                let count_str = format!("{} PDFs queued for merge", cv.merge_files.len());
                if let Some(f13) = self.fmt_13_bold.as_ref() {
                    self.draw_text_at(target, &count_str, Some(f13), color(1.0, 1.0, 1.0, 1.0), lx0 + 12.0, ly0 + 8.0);
                }
                if let Some(f12) = self.fmt_12_reg.as_ref() {
                    self.draw_text_at(target, "Ready to merge. Click Process icon.", Some(f12), color(1.0, 1.0, 1.0, 0.60), lx0 + 12.0, ly0 + 26.0);
                }
            }

            // Action Icon Buttons (Process / Save):
            if (st.input_path.is_some() || !cv.merge_files.is_empty()) && !st.uploading && st.result_bytes.is_none() {
                let proc_hov = in_rect(CONVERT_PROCESS_BTN_RECT);
                self.draw_icon_button(target, CONVERT_PROCESS_BTN_RECT, proc_hov, "Convert File", true, &|cx, cy, c| {
                    self.draw_icon_bolt(target, cx, cy, 18.0, c);
                });
            }

            if st.result_bytes.is_some() {
                let save_hov = in_rect(CONVERT_SAVE_BTN_RECT);
                self.draw_icon_button(target, CONVERT_SAVE_BTN_RECT, save_hov, "Save Output", true, &|cx, cy, c| {
                    self.draw_icon_download(target, cx, cy, 18.0, c);
                });
            }
        }
    }

    fn draw_merge_pdf_view(
        &self,
        target: &ID2D1RenderTarget,
        cv: &crate::convert::ConvertState,
        mouse_pt: Option<(f32, f32)>,
        ctx: &RenderContext,
    ) {
        let in_rect = |r: (f32, f32, f32, f32)| {
            mouse_pt.map(|(mx, my)| mx >= r.0 && mx <= r.2 && my >= r.1 && my <= r.3).unwrap_or(false)
        };

        let cv_rects = compute_cv_button_rects(&ctx.cv_btn_hovers, 0, false);

        // Button 0: Back
        self.draw_expanding_button(target, cv_rects[0], ctx.cv_btn_hovers[0], "Back", false, &|cx, cy, c| {
            self.draw_icon_chevron(target, cx, cy, false, c);
        });

        let title_x = cv_rects[0].2 + 14.0;
        self.draw_text_at(target, "Merge PDFs", self.fmt_18_med.as_ref(), color(1.0, 1.0, 1.0, 1.0), title_x, 28.0);

        // "+ Add PDFs" button in header
        let add_hov = in_rect(MERGE_PDF_ADD_BTN);
        let add_alpha = if add_hov { 0.20 } else { 0.10 };
        self.fill_rounded(target, MERGE_PDF_ADD_BTN.0, MERGE_PDF_ADD_BTN.1, MERGE_PDF_ADD_BTN.2 - MERGE_PDF_ADD_BTN.0, MERGE_PDF_ADD_BTN.3 - MERGE_PDF_ADD_BTN.1, 14.0, color(1.0, 1.0, 1.0, add_alpha));
        self.stroke_rounded(target, MERGE_PDF_ADD_BTN.0, MERGE_PDF_ADD_BTN.1, MERGE_PDF_ADD_BTN.2 - MERGE_PDF_ADD_BTN.0, MERGE_PDF_ADD_BTN.3 - MERGE_PDF_ADD_BTN.1, 14.0, 1.0, color(1.0, 1.0, 1.0, 0.25), None);
        if let Some(f12) = self.fmt_13_med.as_ref() {
            let (aw, ah) = self.measure_text("+ Add PDFs", f12);
            let mid_x = (MERGE_PDF_ADD_BTN.0 + MERGE_PDF_ADD_BTN.2) * 0.5;
            let mid_y = (MERGE_PDF_ADD_BTN.1 + MERGE_PDF_ADD_BTN.3) * 0.5;
            self.draw_text_at(target, "+ Add PDFs", Some(f12), color(1.0, 1.0, 1.0, 0.95), mid_x - aw * 0.5, mid_y - ah * 0.5);
        }

        // Reset / Clear button
        self.draw_expanding_button(target, cv_rects[2], ctx.cv_btn_hovers[2], "Clear", false, &|cx, cy, c| {
            self.draw_icon_trash(target, cx, cy, c);
        });

        let st = cv.tool_state(crate::convert::ConvertTool::MergePdf);

        if cv.merge_files.is_empty() {
            // Empty drop zone
            let (zx0, zy0, zx1, zy1) = (54.0, 58.0, 486.0, 154.0);
            let zw = zx1 - zx0;
            let zh = zy1 - zy0;
            self.fill_rounded(target, zx0, zy0, zw, zh, 14.0, color(1.0, 1.0, 1.0, 0.04));
            self.stroke_rounded(target, zx0, zy0, zw, zh, 14.0, 1.5, color(1.0, 1.0, 1.0, 0.35), self.stroke_dashed.as_ref());
            let cy = zy0 + zh * 0.5;
            self.draw_icon_doc(target, zx0 + zw * 0.5, cy - 10.0, 28.0, color(1.0, 0.35, 0.35, 0.85));
            self.draw_centered_text(target, "Drop PDF files here, or click to pick", self.fmt_13_med.as_ref(), color(1.0, 1.0, 1.0, 0.85), 540.0, cy + 12.0);
            return;
        }

        // Ordered list of PDF files (thumbnail on left, name in middle, three-line icon on right):
        let list_top = 58.0f32;
        let row_h = 24.0f32;
        let row_spacing = 3.0f32;
        let visible_count = cv.merge_files.len().min(3);

        for (i, (name, _bytes)) in cv.merge_files.iter().take(visible_count).enumerate() {
            let ry = list_top + (i as f32) * (row_h + row_spacing);
            let rx = 54.0f32;
            let rw = 432.0f32;

            let row_hov = mouse_pt.map(|(mx, my)| mx >= rx && mx <= rx + rw && my >= ry && my <= ry + row_h).unwrap_or(false);
            let bg_alpha = if row_hov { 0.12 } else { 0.06 };
            self.fill_rounded(target, rx, ry, rw, row_h, 8.0, color(1.0, 1.0, 1.0, bg_alpha));
            self.stroke_rounded(target, rx, ry, rw, row_h, 8.0, 1.0, color(1.0, 1.0, 1.0, 0.15), None);

            // 1. Thumbnail on left (red PDF badge):
            let badge_x = rx + 8.0;
            let badge_y = ry + 3.0;
            let badge_w = 26.0;
            let badge_h = 18.0;
            self.fill_rounded(target, badge_x, badge_y, badge_w, badge_h, 4.0, color(0.92, 0.25, 0.25, 0.85));
            if let Some(f9) = self.fmt_10_med.as_ref() {
                let (tw, th) = self.measure_text("PDF", f9);
                self.draw_text_at(target, "PDF", Some(f9), color(1.0, 1.0, 1.0, 1.0), badge_x + (badge_w - tw) * 0.5, badge_y + (badge_h - th) * 0.5);
            }

            // 2. Name in middle:
            let name_x = badge_x + badge_w + 10.0;
            if let Some(f12) = self.fmt_12_reg.as_ref() {
                let avail_w = rw - 90.0;
                let display_name = self.truncate_text(name, avail_w, f12);
                self.draw_text_at(target, &display_name, Some(f12), color(1.0, 1.0, 1.0, 0.95), name_x, ry + 4.0);
            }

            // 3. Delete '×' button:
            let del_x = rx + rw - 36.0;
            let del_hov = mouse_pt.map(|(mx, my)| mx >= del_x - 4.0 && mx <= del_x + 12.0 && my >= ry && my <= ry + row_h).unwrap_or(false);
            let del_c = if del_hov { color(1.0, 0.35, 0.35, 1.0) } else { color(1.0, 1.0, 1.0, 0.40) };
            if let Some(f13) = self.fmt_13_bold.as_ref() {
                self.draw_text_at(target, "×", Some(f13), del_c, del_x, ry + 2.0);
            }

            // 4. Three-line icon (☰) on the right:
            let bar_x = rx + rw - 14.0;
            let bar_cy = ry + row_h * 0.5;
            self.draw_icon_lines_three(target, bar_x, bar_cy, 12.0, color(1.0, 1.0, 1.0, 0.55));
        }

        if cv.merge_files.len() > 3 {
            let more_text = format!("+ {} more files in queue", cv.merge_files.len() - 3);
            if let Some(f11) = self.fmt_11_reg.as_ref() {
                self.draw_text_at(target, &more_text, Some(f11), color(1.0, 1.0, 1.0, 0.55), 58.0, 142.0);
            }
        }

        // Action button area (Merge / Saving / Result):
        if st.uploading {
            let msg = "Merging PDFs...";
            if let Some(f13) = self.fmt_13_bold.as_ref() {
                self.draw_text_at(target, msg, Some(f13), color(0.3, 0.8, 1.0, 1.0), 360.0, 144.0);
            }
        } else if st.result_bytes.is_some() {
            let r_btn = MERGE_PDF_RUN_BTN;
            let r_hov = in_rect(r_btn);
            let bg_c = if r_hov { color(0.25, 0.75, 0.45, 0.95) } else { color(0.20, 0.65, 0.38, 0.85) };
            self.fill_rounded(target, r_btn.0, r_btn.1, r_btn.2 - r_btn.0, r_btn.3 - r_btn.1, 14.0, bg_c);
            if let Some(f12) = self.fmt_13_bold.as_ref() {
                let (tw, th) = self.measure_text("✓ Save Merged PDF", f12);
                let cx = (r_btn.0 + r_btn.2) * 0.5;
                let cy = (r_btn.1 + r_btn.3) * 0.5;
                self.draw_text_at(target, "✓ Save Merged PDF", Some(f12), color(1.0, 1.0, 1.0, 1.0), cx - tw * 0.5, cy - th * 0.5);
            }
        } else if cv.merge_files.len() >= 2 {
            let r_btn = MERGE_PDF_RUN_BTN;
            let r_hov = in_rect(r_btn);
            let bg_c = if r_hov { color(0.18, 0.55, 0.95, 0.95) } else { color(0.12, 0.45, 0.85, 0.85) };
            self.fill_rounded(target, r_btn.0, r_btn.1, r_btn.2 - r_btn.0, r_btn.3 - r_btn.1, 14.0, bg_c);
            self.stroke_rounded(target, r_btn.0, r_btn.1, r_btn.2 - r_btn.0, r_btn.3 - r_btn.1, 14.0, 1.0, color(1.0, 1.0, 1.0, 0.35), None);
            let label = format!("Merge {} PDFs", cv.merge_files.len());
            if let Some(f12) = self.fmt_13_bold.as_ref() {
                let (tw, th) = self.measure_text(&label, f12);
                let cx = (r_btn.0 + r_btn.2) * 0.5;
                let cy = (r_btn.1 + r_btn.3) * 0.5;
                self.draw_text_at(target, &label, Some(f12), color(1.0, 1.0, 1.0, 1.0), cx - tw * 0.5, cy - th * 0.5);
            }
        } else if cv.merge_files.len() == 1 {
            if let Some(f12) = self.fmt_12_reg.as_ref() {
                self.draw_text_at(target, "Add at least 1 more PDF to merge", Some(f12), color(1.0, 1.0, 1.0, 0.65), 58.0, 144.0);
            }
        }
    }

    fn draw_pdf_to_md_view(
        &self,
        target: &ID2D1RenderTarget,
        cv: &crate::convert::ConvertState,
        mouse_pt: Option<(f32, f32)>,
        ctx: &RenderContext,
    ) {
        let in_rect = |r: (f32, f32, f32, f32)| {
            mouse_pt.map(|(mx, my)| mx >= r.0 && mx <= r.2 && my >= r.1 && my <= r.3).unwrap_or(false)
        };

        let cv_rects = compute_cv_button_rects(&ctx.cv_btn_hovers, 0, false);

        // Button 0: Back
        self.draw_expanding_button(target, cv_rects[0], ctx.cv_btn_hovers[0], "Back", false, &|cx, cy, c| {
            self.draw_icon_chevron(target, cx, cy, false, c);
        });

        let title_x = cv_rects[0].2 + 14.0;
        self.draw_text_at(target, "PDF to Markdown", self.fmt_18_med.as_ref(), color(1.0, 1.0, 1.0, 1.0), title_x, 28.0);

        // Reset / Clear button
        self.draw_expanding_button(target, cv_rects[2], ctx.cv_btn_hovers[2], "Clear", false, &|cx, cy, c| {
            self.draw_icon_trash(target, cx, cy, c);
        });

        let st = cv.tool_state(crate::convert::ConvertTool::PdfToMd);

        // Right Side: Markdown Portal Target
        let (px0, py0, px1, py1) = PDF_MD_PORTAL_RECT;
        let pw = px1 - px0;
        let ph = py1 - py0;
        let pcx = px0 + pw * 0.5;
        let pcy = py0 + ph * 0.5;

        let is_dragging = ctx.pdf_md_drag.is_some();
        let drag_near = mouse_pt.map(|(mx, my)| mx >= px0 - 40.0 && mx <= px1 + 20.0 && my >= py0 - 20.0 && my <= py1 + 20.0).unwrap_or(false);
        let portal_hov = in_rect(PDF_MD_PORTAL_RECT);
        let target_glow = (is_dragging && drag_near) || (!is_dragging && portal_hov);

        let portal_bg = if target_glow {
            color(0.15, 0.45, 0.85, 0.40)
        } else {
            color(1.0, 1.0, 1.0, 0.05)
        };
        let portal_stroke = if target_glow {
            color(0.35, 0.80, 1.0, 0.90)
        } else {
            color(1.0, 1.0, 1.0, 0.20)
        };
        let stroke_width = if target_glow { 2.0 } else { 1.0 };

        self.fill_rounded(target, px0, py0, pw, ph, 16.0, portal_bg);
        self.stroke_rounded(target, px0, py0, pw, ph, 16.0, stroke_width, portal_stroke, None);

        // Markdown Emblem inside Target [M ↓]:
        let emblem_c = if target_glow { color(0.35, 0.85, 1.0, 1.0) } else { color(1.0, 1.0, 1.0, 0.85) };
        self.draw_icon_markdown(target, pcx, pcy - 12.0, 36.0, emblem_c);

        let portal_sub = if target_glow {
            "RELEASE TO CONVERT"
        } else if is_dragging {
            "DROP HERE"
        } else if st.result_bytes.is_some() {
            "DONE! CLICK SAVE"
        } else {
            "SLIDE PDF HERE"
        };
        if let Some(f10b) = self.fmt_10_med.as_ref() {
            let (tw, _) = self.measure_text(portal_sub, f10b);
            self.draw_text_at(target, portal_sub, Some(f10b), emblem_c, pcx - tw * 0.5, pcy + 22.0);
        }

        // Left Side: Character Select Carousel
        let queue = &st.queue;
        let selected_idx = st.queue_idx;

        if queue.is_empty() {
            // Slot 0: Add PDF slot
            let card_x = PDF_MD_CARD_START_X;
            let card_y = PDF_MD_CARD_Y;
            let card_w = PDF_MD_CARD_W;
            let card_h = PDF_MD_CARD_H;

            let card_hov = mouse_pt.map(|(mx, my)| mx >= card_x && mx <= card_x + card_w && my >= card_y && my <= card_y + card_h).unwrap_or(false);
            let bg_a = if card_hov { 0.10 } else { 0.05 };
            self.fill_rounded(target, card_x, card_y, card_w, card_h, 14.0, color(1.0, 1.0, 1.0, bg_a));
            self.stroke_rounded(target, card_x, card_y, card_w, card_h, 14.0, 1.5, color(1.0, 1.0, 1.0, 0.35), self.stroke_dashed.as_ref());

            let ccx = card_x + card_w * 0.5;
            let ccy = card_y + card_h * 0.5;
            self.draw_icon_doc(target, ccx, ccy - 10.0, 24.0, color(1.0, 1.0, 1.0, 0.60));
            if let Some(f11b) = self.fmt_13_bold.as_ref() {
                let (tw, _) = self.measure_text("+ Add PDF", f11b);
                self.draw_text_at(target, "+ Add PDF", Some(f11b), color(1.0, 1.0, 1.0, 0.85), ccx - tw * 0.5, ccy + 14.0);
            }

            // Arrow hint pointing towards Markdown portal:
            if let Some(f12) = self.fmt_13_med.as_ref() {
                self.draw_text_at(target, "Drop or pick a PDF to select character ──►", Some(f12), color(1.0, 1.0, 1.0, 0.50), card_x + card_w + 14.0, card_y + card_h * 0.5 - 8.0);
            }
            return;
        }

        // Render character select cards:
        let visible_cards = queue.len().min(3);
        for i in 0..visible_cards {
            let is_selected = i == selected_idx;
            let is_this_dragging = ctx.pdf_md_drag.map(|(d_idx, _, _)| d_idx == i).unwrap_or(false);

            let (drag_ox, drag_oy) = if is_this_dragging {
                if let (Some((_, mx, my)), Some(mpt)) = (ctx.pdf_md_drag, mouse_pt) {
                    ((mpt.0 - mx).max(0.0), mpt.1 - my)
                } else {
                    (0.0, 0.0)
                }
            } else {
                (0.0, 0.0)
            };

            let card_x = PDF_MD_CARD_START_X + (i as f32) * PDF_MD_CARD_PITCH + drag_ox;
            let card_y = PDF_MD_CARD_Y + drag_oy;
            let card_w = PDF_MD_CARD_W;
            let card_h = PDF_MD_CARD_H;

            let card_hov = mouse_pt.map(|(mx, my)| mx >= card_x && mx <= card_x + card_w && my >= card_y && my <= card_y + card_h).unwrap_or(false);

            let card_bg = if is_selected {
                color(0.18, 0.45, 0.85, 0.25)
            } else if card_hov {
                color(1.0, 1.0, 1.0, 0.12)
            } else {
                color(1.0, 1.0, 1.0, 0.06)
            };

            let card_stroke = if is_selected {
                color(0.40, 0.85, 1.0, 0.95)
            } else {
                color(1.0, 1.0, 1.0, 0.20)
            };

            self.fill_rounded(target, card_x, card_y, card_w, card_h, 14.0, card_bg);
            self.stroke_rounded(target, card_x, card_y, card_w, card_h, 14.0, if is_selected { 2.0 } else { 1.0 }, card_stroke, None);

            // Character badge / label above card if selected:
            if is_selected {
                let tag = "READY";
                if let Some(f9b) = self.fmt_10_med.as_ref() {
                    let (tw, _th) = self.measure_text(tag, f9b);
                    let tag_w = tw + 8.0;
                    let tag_h = 13.0;
                    let tag_x = card_x + (card_w - tag_w) * 0.5;
                    let tag_y = card_y - 6.0;
                    self.fill_rounded(target, tag_x, tag_y, tag_w, tag_h, 4.0, color(0.35, 0.80, 1.0, 1.0));
                    self.draw_text_at(target, tag, Some(f9b), color(0.0, 0.0, 0.0, 1.0), tag_x + 4.0, tag_y + 1.0);
                }
            }

            // Thumbnail art inside card:
            let ccx = card_x + card_w * 0.5;
            let ccy = card_y + 36.0;
            self.draw_icon_doc(target, ccx, ccy, 28.0, if is_selected { color(1.0, 0.35, 0.35, 1.0) } else { color(1.0, 1.0, 1.0, 0.70) });

            // PDF text badge:
            if let Some(f9b) = self.fmt_10_med.as_ref() {
                let (tw, _) = self.measure_text("PDF", f9b);
                self.draw_text_at(target, "PDF", Some(f9b), color(1.0, 1.0, 1.0, 0.90), ccx - tw * 0.5, ccy + 16.0);
            }

            // Filename at bottom:
            let path = &queue[i];
            let fname = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path);
            if let Some(f10) = self.fmt_10_med.as_ref() {
                let short = self.truncate_text(fname, card_w - 8.0, f10);
                let (sw, _) = self.measure_text(&short, f10);
                self.draw_text_at(target, &short, Some(f10), color(1.0, 1.0, 1.0, 0.90), card_x + (card_w - sw) * 0.5, card_y + card_h - 18.0);
            }
        }

        // Add extra card button slot if queue < 3:
        if queue.len() < 3 {
            let add_i = queue.len();
            let card_x = PDF_MD_CARD_START_X + (add_i as f32) * PDF_MD_CARD_PITCH;
            let card_y = PDF_MD_CARD_Y;
            let card_w = PDF_MD_CARD_W;
            let card_h = PDF_MD_CARD_H;

            let card_hov = mouse_pt.map(|(mx, my)| mx >= card_x && mx <= card_x + card_w && my >= card_y && my <= card_y + card_h).unwrap_or(false);
            let bg_a = if card_hov { 0.08 } else { 0.03 };
            self.fill_rounded(target, card_x, card_y, card_w, card_h, 14.0, color(1.0, 1.0, 1.0, bg_a));
            self.stroke_rounded(target, card_x, card_y, card_w, card_h, 14.0, 1.0, color(1.0, 1.0, 1.0, 0.25), self.stroke_dashed.as_ref());

            let ccx = card_x + card_w * 0.5;
            let ccy = card_y + card_h * 0.5;
            if let Some(f14) = self.fmt_14_reg.as_ref() {
                let (tw, th) = self.measure_text("+", f14);
                self.draw_text_at(target, "+", Some(f14), color(1.0, 1.0, 1.0, 0.60), ccx - tw * 0.5, ccy - th * 0.5);
            }
        }

        // Prompt hint underneath character carousel:
        if !is_dragging {
            let hint = "Slide thumbnail to the Markdown portal to convert ──►";
            if let Some(f11) = self.fmt_11_reg.as_ref() {
                self.draw_text_at(target, hint, Some(f11), color(1.0, 1.0, 1.0, 0.55), 56.0, 166.0);
            }
        }
    }

    fn draw_settings(
        &self,
        target: &ID2D1RenderTarget,
        settings: &crate::settings::AuraSettings,
        mouse_pt: Option<(f32, f32)>,
    ) {
        let in_rect = |r: (f32, f32, f32, f32)| {
            mouse_pt.map(|(mx, my)| mx >= r.0 && mx <= r.2 && my >= r.1 && my <= r.3).unwrap_or(false)
        };

        // Header
        self.draw_text_at(
            target,
            "Aura Settings",
            self.fmt_18_med.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            54.0,
            24.0,
        );

        // Close button at top right
        let close_hov = in_rect(SETTINGS_CLOSE_BTN);
        let close_bg = if close_hov { color(1.0, 1.0, 1.0, 0.18) } else { color(1.0, 1.0, 1.0, 0.08) };
        self.fill_rounded(target, SETTINGS_CLOSE_BTN.0, SETTINGS_CLOSE_BTN.1, SETTINGS_CLOSE_BTN.2 - SETTINGS_CLOSE_BTN.0, SETTINGS_CLOSE_BTN.3 - SETTINGS_CLOSE_BTN.1, 14.0, close_bg);
        if let Some(f14) = self.fmt_13_bold.as_ref() {
            let (tw, th) = self.measure_text("×", f14);
            let cx = (SETTINGS_CLOSE_BTN.0 + SETTINGS_CLOSE_BTN.2) * 0.5;
            let cy = (SETTINGS_CLOSE_BTN.1 + SETTINGS_CLOSE_BTN.3) * 0.5;
            self.draw_text_at(target, "×", Some(f14), color(1.0, 1.0, 1.0, 0.90), cx - tw * 0.5, cy - th * 0.5);
        }

        // Left Column: Active Sections
        if let Some(f11b) = self.fmt_13_bold.as_ref() {
            self.draw_text_at(target, "SECTIONS", Some(f11b), color(1.0, 1.0, 1.0, 0.50), 54.0, 54.0);
        }

        let section_items = [
            ("Music", settings.music_enabled),
            ("Focus", settings.pomodoro_enabled),
            ("Todo", settings.todo_enabled),
            ("Water", settings.water_enabled),
            ("Shelf", settings.tray_enabled),
            ("Remove BG", settings.removebg_enabled),
            ("Convert", settings.convert_enabled),
        ];

        // 2-column chips for sections:
        for (i, (name, on)) in section_items.iter().enumerate() {
            let col = i % 2;
            let row = i / 2;
            let cx = 54.0 + (col as f32) * 110.0;
            let cy = 70.0 + (row as f32) * 25.0;
            let cw = 102.0;
            let ch = 21.0;

            let hov = mouse_pt.map(|(mx, my)| mx >= cx && mx <= cx + cw && my >= cy && my <= cy + ch).unwrap_or(false);
            let bg_c = if *on {
                if hov { color(0.18, 0.58, 0.32, 0.40) } else { color(0.18, 0.58, 0.32, 0.25) }
            } else {
                if hov { color(1.0, 1.0, 1.0, 0.10) } else { color(1.0, 1.0, 1.0, 0.04) }
            };
            let border_c = if *on { color(0.35, 0.85, 0.45, 0.80) } else { color(1.0, 1.0, 1.0, 0.15) };
            self.fill_rounded(target, cx, cy, cw, ch, 6.0, bg_c);
            self.stroke_rounded(target, cx, cy, cw, ch, 6.0, 1.0, border_c, None);

            // Indicator dot on left of chip:
            let dot_c = if *on { color(0.35, 0.85, 0.45, 1.0) } else { color(1.0, 1.0, 1.0, 0.30) };
            self.fill_oval(target, cx + 8.0, cy + 6.5, 8.0, 8.0, dot_c);

            if let Some(f11) = self.fmt_10_med.as_ref() {
                self.draw_text_at(target, name, Some(f11), color(1.0, 1.0, 1.0, if *on { 0.95 } else { 0.50 }), cx + 22.0, cy + 3.5);
            }
        }

        // Right Column: Preferences & System
        let rx = 290.0f32;
        if let Some(f11b) = self.fmt_13_bold.as_ref() {
            self.draw_text_at(target, "SYSTEM & PREFERENCES", Some(f11b), color(1.0, 1.0, 1.0, 0.50), rx, 54.0);
        }

        let pref_items = [
            ("Start with Windows", settings.start_with_windows),
            ("Sound Chimes", settings.sound_chimes),
            ("120 FPS High Refresh", settings.high_refresh_120fps),
            ("Morphing Zen Eyes", settings.eye_expressions),
        ];

        for (i, (name, on)) in pref_items.iter().enumerate() {
            let py = 72.0 + (i as f32) * 24.0;
            if let Some(f11) = self.fmt_11_reg.as_ref() {
                self.draw_text_at(target, name, Some(f11), color(1.0, 1.0, 1.0, 0.90), rx, py + 2.0);
            }

            // iOS switch capsule at rx + 164.0
            let sw_x = rx + 164.0;
            let sw_y = py;
            let sw_w = 28.0;
            let sw_h = 16.0;

            let sw_hov = mouse_pt.map(|(mx, my)| mx >= sw_x && mx <= sw_x + sw_w && my >= sw_y && my <= sw_y + sw_h).unwrap_or(false);
            let bg = if *on {
                if sw_hov { color(0.25, 0.82, 0.45, 1.0) } else { color(0.20, 0.75, 0.38, 1.0) }
            } else {
                if sw_hov { color(0.40, 0.40, 0.42, 1.0) } else { color(0.30, 0.30, 0.32, 1.0) }
            };
            self.fill_rounded(target, sw_x, sw_y, sw_w, sw_h, sw_h * 0.5, bg);

            let knob_r = (sw_h - 4.0) * 0.5;
            let knob_x = if *on { sw_x + sw_w - sw_h + 2.0 } else { sw_x + 2.0 };
            self.fill_oval(target, knob_x, sw_y + 2.0, knob_r * 2.0, knob_r * 2.0, color(1.0, 1.0, 1.0, 1.0));
        }
    }

    fn draw_tray(
        &self,
        target: &ID2D1RenderTarget,
        shelf: &crate::shelf::Shelf,
        tray_scroll: f32,
        playing: bool,
        tray_drop_highlight: bool,
        tray_drag_pt: Option<(f32, f32)>,
        lh: f32,
        genie_progress: Option<f32>,
        tray_btn_hovers: &[f32; 4],
    ) {
        if tray_drop_highlight {
            self.draw_tray_drop_zone(target, shelf, playing, tray_drag_pt, lh);
            return;
        }
        self.draw_text_at(
            target,
            "Tray",
            self.fmt_20_bold.as_ref(),
            color(1.0, 1.0, 1.0, 1.0),
            54.0,
            26.0,
        );

        if shelf.files.is_empty() && genie_progress.is_none() {
            let zone_top = 62.0f32;
            let zone_h = 88.0f32;
            let zone_r = 14.0f32;
            self.stroke_rounded(
                target,
                54.0,
                zone_top,
                432.0,
                zone_h,
                zone_r,
                1.5,
                color(1.0, 1.0, 1.0, 0.40),
                self.stroke_dashed.as_ref(),
            );
            let cy = zone_top + zone_h * 0.5;
            self.draw_icon_doc(target, 270.0, cy - 10.0, 28.0, color(1.0, 1.0, 1.0, 0.60));
            self.draw_centered_text(
                target,
                "Drag files here",
                self.fmt_14_reg.as_ref(),
                color(1.0, 1.0, 1.0, 0.70),
                540.0,
                cy + 14.0,
            );
            return;
        }

        // Header Action Expanding Oval Buttons on hover:
        let rects = compute_tray_button_rects(tray_btn_hovers);

        // 1. Clear button
        self.draw_expanding_button(target, rects[0], tray_btn_hovers[0], "Clear", false, &|cx, cy, c| {
            self.draw_icon_trash(target, cx, cy, c);
        });

        // 2. Select button
        let sel_label = if shelf.select_mode { "Done" } else { "Select" };
        self.draw_expanding_button(target, rects[1], tray_btn_hovers[1], sel_label, shelf.select_mode, &|cx, cy, c| {
            self.draw_icon_checkmark(target, cx, cy, c);
        });

        // 3. Remove BG transfer button
        self.draw_expanding_button(target, rects[2], tray_btn_hovers[2], "Remove BG", false, &|cx, cy, c| {
            self.draw_icon_scissors(target, cx, cy, c);
        });

        // 4. Convert transfer button
        self.draw_expanding_button(target, rects[3], tray_btn_hovers[3], "Convert", false, &|cx, cy, c| {
            self.draw_icon_sync(target, cx, cy, c);
        });

        // Horizontal carousel with clip:
        let carousel_clip = D2D_RECT_F {
            left: TRAY_CAROUSEL_X,
            top: TRAY_CAROUSEL_Y,
            right: TRAY_CAROUSEL_X + TRAY_CAROUSEL_W,
            bottom: TRAY_CAROUSEL_Y + TRAY_CAROUSEL_H,
        };
        unsafe {
            target.PushAxisAlignedClip(&carousel_clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }

        for (i, path) in shelf.files.iter().enumerate() {
            let left = TRAY_CAROUSEL_X + (i as f32) * TRAY_TILE_PITCH - tray_scroll;
            let top = TRAY_CAROUSEL_Y;
            let tw = TRAY_TILE_W;
            let th = TRAY_TILE_H;
            if left + tw < TRAY_CAROUSEL_X - 50.0 || left > TRAY_CAROUSEL_X + TRAY_CAROUSEL_W + 50.0 {
                continue;
            }
            let cx = left + tw * 0.5;
            let cy = top + th * 0.5;
            let info = shelf.get_info(path);
            let mut alpha = if info.exists { 1.0 } else { 0.40 };

            let mut pop_xform = false;
            if let Some(p) = genie_progress {
                let p_clamped = p.clamp(0.0, 1.0);
                let (dest_x, dest_y) = (108.0f32, 24.0f32);
                let cur_cx = cx + (dest_x - cx) * (p_clamped * p_clamped);
                let cur_cy = cy + (dest_y - cy) * (p_clamped * p_clamped * p_clamped);
                let scale = (1.0 - p_clamped).powf(1.4).max(0.001);
                alpha *= (1.0 - p_clamped).clamp(0.0, 1.0);

                let mat = Matrix3x2 {
                    M11: scale,
                    M12: 0.0,
                    M21: 0.0,
                    M22: scale,
                    M31: cur_cx - cx * scale,
                    M32: cur_cy - cy * scale,
                };
                unsafe {
                    target.SetTransform(&mat);
                }
                pop_xform = true;
            }

            // Pure minimal: NO background box and NO border unless selected!
            if shelf.is_selected(i) {
                self.fill_rounded(target, left, top, tw, th, 12.0, color(1.0, 1.0, 1.0, 0.12 * alpha));
                self.stroke_rounded(target, left, top, tw, th, 12.0, 1.5, color(1.0, 1.0, 1.0, 0.55 * alpha), None);
            }

            // Real thumbnail preview:
            if let Some(bmp) = self.tray_thumb_cache.get(path) {
                let preview_rect = D2D_RECT_F {
                    left: cx - 18.0,
                    top: top + 10.0,
                    right: cx + 18.0,
                    bottom: top + 46.0,
                };
                unsafe {
                    target.DrawBitmap(
                        bmp,
                        Some(&preview_rect),
                        alpha,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
            } else if info.is_dir {
                self.draw_icon_folder(target, cx, top + 26.0, 32.0, color(1.0, 1.0, 1.0, alpha));
            } else {
                self.draw_icon_doc(target, cx, top + 26.0, 32.0, color(1.0, 1.0, 1.0, alpha));
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
                    top + 52.0,
                );
            }
            if let Some(fmt11) = self.fmt_11_reg.as_ref() {
                let (sw, _) = self.measure_text(&info.size_text, fmt11);
                self.draw_text_at(
                    target,
                    &info.size_text,
                    Some(fmt11),
                    color(1.0, 1.0, 1.0, 0.55 * alpha),
                    cx - sw * 0.5,
                    top + 72.0,
                );
            }

            // Checkbox selection indicator in select mode:
            if shelf.select_mode {
                let chk_x = left + 14.0;
                let chk_y = top + 14.0;
                let chk_r = 7.0;
                if shelf.is_selected(i) {
                    self.fill_circle(target, chk_x, chk_y, chk_r, color(1.0, 1.0, 1.0, alpha));
                    self.draw_icon_checkmark(target, chk_x, chk_y, color(0.0, 0.0, 0.0, alpha));
                } else {
                    self.stroke_rounded(target, chk_x - chk_r, chk_y - chk_r, chk_r * 2.0, chk_r * 2.0, chk_r, 1.0, color(1.0, 1.0, 1.0, 0.50 * alpha), None);
                }
            } else {
                // Delete "×" button at top-right of tile:
                self.draw_text_at(
                    target,
                    "×",
                    self.fmt_14_reg.as_ref(),
                    color(1.0, 1.0, 1.0, 0.40 * alpha),
                    left + tw - 16.0,
                    top + 4.0,
                );
            }

            if pop_xform {
                unsafe {
                    target.SetTransform(&Matrix3x2::identity());
                }
            }
        }
        unsafe {
            target.PopAxisAlignedClip();
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

    pub fn draw(&mut self, hwnd: HWND, ctx: &RenderContext) -> bool {
        let scale = window::dpi_scale(hwnd);
        let canvas_w_px = (window::CANVAS_W * scale).round() as i32;
        let canvas_h_px = (window::CANVAS_H * scale).round() as i32;
        if !self.ensure_target(canvas_w_px, canvas_h_px, scale * 96.0) {
            unsafe {
                let _ = ValidateRect(hwnd, None);
            }
            return false;
        }
        self.update_cover_bitmap(ctx.media);
        self.update_removebg_bitmaps(ctx.removebg);
        self.update_convert_bitmaps(ctx.convert);
        if let Some(target) = self.target.as_ref() {
            let target_clone = target.clone();
            self.ensure_dot_icons(&target_clone);
            if ctx.state == IslandState::Convert {
                self.ensure_convert_app_bitmaps(&target_clone);
            }
            if ctx.state == IslandState::Tray {
                self.ensure_tray_thumbnails(&target_clone, ctx.shelf);
            }
        }
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
                M31: tx,
                M32: 0.0,
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
                // Subtle dark ambient backdrop shadow around notch edges
                if let Some(b_shadow) = self.brush(&target, color(0.0, 0.0, 0.0, 0.08)) {
                    target.DrawGeometry(geometry, &b_shadow, 6.0, None);
                }
                if let Some(b_shadow2) = self.brush(&target, color(0.0, 0.0, 0.0, 0.15)) {
                    target.DrawGeometry(geometry, &b_shadow2, 2.5, None);
                }
                if let Some(b_black) = self.brush(&target, color(0.0, 0.0, 0.0, 1.0)) {
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

            // Hardware-accelerated axis-aligned clip with zero offscreen surface allocation
            let clip_bounds = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: lw,
                bottom: lh,
            };
            target.PushAxisAlignedClip(&clip_bounds, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

            let playing = ctx.media.map(|m| m.playing).unwrap_or(false);

            if !ctx.state.is_expanded() && ctx.alert.is_some() {
                self.draw_pill_alert(&target, lw, lh, ctx.alert.unwrap());
            } else if ctx.transition_active && ctx.state.is_expanded() {
                let p = ctx.transition_progress.clamp(0.0, 1.0);
                let ease = 1.0 - (1.0 - p).powi(3);
                let dir = ctx.transition_dir;

                // Push hard axis-aligned clip to ensure NO pixel can ever escape the notch boundary:
                let clip_rect = D2D_RECT_F {
                    left: 14.0,
                    top: 0.0,
                    right: lw - 14.0,
                    bottom: lh,
                };
                target.PushAxisAlignedClip(&clip_rect, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

                let max_drift = 24.0f32;

                // 1. Departing section slides out and fades away smoothly:
                if ctx.transition_from != ctx.state && ctx.transition_from.is_expanded() {
                    let from_alpha = (1.0 - p).powi(2).clamp(0.0, 1.0);
                    if from_alpha > 0.01 {
                        let from_offset = -dir * ease * max_drift;
                        let trans_from = Matrix3x2 {
                            M11: s,
                            M12: 0.0,
                            M21: 0.0,
                            M22: s,
                            M31: tx + from_offset,
                            M32: 0.0,
                        };
                        let layer_params = D2D1_LAYER_PARAMETERS {
                            contentBounds: clip_rect,
                            geometricMask: ManuallyDrop::new(None),
                            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                            maskTransform: Matrix3x2::identity(),
                            opacity: from_alpha,
                            opacityBrush: ManuallyDrop::new(None),
                            layerOptions: D2D1_LAYER_OPTIONS_NONE,
                        };
                        target.PushLayer(&layer_params, None);
                        target.SetTransform(&trans_from);
                        self.draw_card_content(&target, ctx.transition_from, ctx, playing, &base_xform, s, tx, lw, lh, lyrics_t);
                        target.PopLayer();
                    }
                }

                // 2. Incoming section slides in and fades in smoothly:
                let to_alpha = p.powf(0.8).clamp(0.0, 1.0);
                let to_offset = dir * (1.0 - ease) * max_drift;
                let trans_to = Matrix3x2 {
                    M11: s,
                    M12: 0.0,
                    M21: 0.0,
                    M22: s,
                    M31: tx + to_offset,
                    M32: 0.0,
                };
                let layer_params = D2D1_LAYER_PARAMETERS {
                    contentBounds: clip_rect,
                    geometricMask: ManuallyDrop::new(None),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: to_alpha,
                    opacityBrush: ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS_NONE,
                };
                target.PushLayer(&layer_params, None);
                target.SetTransform(&trans_to);
                self.draw_card_content(&target, ctx.state, ctx, playing, &base_xform, s, tx, lw, lh, lyrics_t);
                target.PopLayer();

                target.PopAxisAlignedClip();
            } else {
                target.SetTransform(&base_xform);
                self.draw_card_content(&target, ctx.state, ctx, playing, &base_xform, s, tx, lw, lh, lyrics_t);
            }

            let has_header = ctx.header_active;

            let expand_t = ((lh - 30.0) / (184.0 - 30.0)).clamp(0.0, 1.0);

            if (ctx.state.is_expanded() || lh > 32.0) && ctx.state != IslandState::Zen {
                target.SetTransform(&base_xform);
                self.draw_morphing_header(
                    &target,
                    lw,
                    lh,
                    expand_t,
                    playing,
                    ctx.battery_percent,
                    ctx.battery_charging,
                    has_header,
                );
            }

            if ctx.state.is_expanded() && ctx.state != IslandState::Settings && lh > 60.0 {
                self.draw_page_dots(&target, ctx.state, lw, lh, ctx.dot_indicator_bounds, ctx.hovered_dot, lyrics_t);
            }

            target.PopAxisAlignedClip();

            target.SetTransform(&Matrix3x2::identity());
            let _ = target.EndDraw(None, None);

            let alpha = (ctx.settings.pill_opacity.clamp(0.4, 1.0) * 255.0).round() as u8;
            self.present_to(hwnd, alpha);
            return true;
        }
        false
    }

    /// Present the last rendered frame onto another canvas HWND
    /// (used to mirror the pill on every monitor).
    pub fn present_to(&self, hwnd: HWND, alpha: u8) {
        unsafe {
            let size = SIZE {
                cx: self.canvas_w_px,
                cy: self.canvas_h_px,
            };
            let pt_src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: alpha,
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
            let (tw, th) = if lh > 60.0 {
                (26.0f32.min(lw / 4.0), 18.0f32.min(lh / 4.0))
            } else {
                (14.0f32.min(lw / 4.0), 12.0f32.min(lh / 4.0))
            };
            let mut pts: Vec<D2D_POINT_2F> = Vec::new();
            pts.push(D2D_POINT_2F { x: 0.0, y: 0.0 });
            pts.push(D2D_POINT_2F { x: lw, y: 0.0 });
            let seg = 20i32;
            for i in 1..seg {
                let a = -std::f32::consts::FRAC_PI_2 * (1.0 + i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: lw + tw * a.cos(),
                    y: th + th * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F { x: lw - tw, y: th });
            pts.push(D2D_POINT_2F {
                x: lw - tw,
                y: lh - r,
            });
            for i in 1..seg {
                let a = std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: (lw - tw - r) + r * a.cos(),
                    y: (lh - r) + r * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F {
                x: lw - tw - r,
                y: lh,
            });
            pts.push(D2D_POINT_2F { x: tw + r, y: lh });
            for i in 1..seg {
                let a = std::f32::consts::FRAC_PI_2 * (1.0 + i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: (tw + r) + r * a.cos(),
                    y: (lh - r) + r * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F { x: tw, y: lh - r });
            pts.push(D2D_POINT_2F { x: tw, y: th });
            for i in 1..seg {
                let a = -std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: tw * a.cos(),
                    y: th + th * a.sin(),
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
