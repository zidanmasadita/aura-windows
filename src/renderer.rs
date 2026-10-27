//! Direct2D renderer (Phase 0): HWND render target + DirectWrite text.
//! Draws the pill/card: black 0.98 fill + white 0.15 stroke (1.0px) +
//! rounded corners + text (Segoe UI). Cover/gradient/waveform detail
//! arrives in Phase 1 (music card); Phase 0 draws placeholder content
//! (clock + decorative waveform bars in Compact, "Aura / Fase 0" in Expanded).
//!
//! Note on signatures: this file follows windows-rs 0.58 example patterns
//! (generic D2D1CreateFactory/DWriteCreateFactory, references for struct
//! params, None for optional params). If `cargo build` reports a signature
//! mismatch, paste it — Eve cannot compile Rust in this VM, so the first
//! build on the user's Windows PC is where signatures get corrected.

use windows::core::{Interface, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::ValidateRect;

use crate::state::IslandState;
use crate::window;

/// Music card control button rects (logical points, 40x40, centered).
/// Shared by renderer (drawing) and app (click hit-test) so they agree.
pub const MUSIC_PREV_RECT: (f32, f32, f32, f32) = (196.0, 124.0, 236.0, 164.0);
pub const MUSIC_PLAY_RECT: (f32, f32, f32, f32) = (240.0, 124.0, 280.0, 164.0);
pub const MUSIC_NEXT_RECT: (f32, f32, f32, f32) = (284.0, 124.0, 324.0, 164.0);
/// Bubble toggle on the music card (top-right) -> lyrics panel.
pub const MUSIC_LYRICS_BUBBLE_RECT: (f32, f32, f32, f32) = (440.0, 28.0, 496.0, 52.0);
/// Bubble toggle on the lyrics panel (top-right) -> music card.
pub const LYRICS_MUSIC_BUBBLE_RECT: (f32, f32, f32, f32) = (620.0, 28.0, 676.0, 52.0);
/// Nudge buttons on the lyrics panel (bottom-right, -0.5s / +0.5s).
pub const LYRICS_NUDGE_MINUS_RECT: (f32, f32, f32, f32) = (548.0, 148.0, 612.0, 172.0);
pub const LYRICS_NUDGE_PLUS_RECT: (f32, f32, f32, f32) = (620.0, 148.0, 676.0, 172.0);
/// Pomodoro card buttons (640x180): mode pills + Start/Pause.
pub const POMODORO_FOCUS_RECT: (f32, f32, f32, f32) = (220.0, 100.0, 300.0, 124.0);
pub const POMODORO_BREAK_RECT: (f32, f32, f32, f32) = (340.0, 100.0, 420.0, 124.0);
pub const POMODORO_START_RECT: (f32, f32, f32, f32) = (280.0, 140.0, 360.0, 164.0);
/// Section dots (4 sections: music, pomodoro, todo, water). Right edge,
/// vertical. Music card (520 wide): x 492-508. Other cards (640 wide):
/// x 612-628. Y for 180-tall cards: four 16px dots, 8px gap, centered
/// (top 46, 70, 94, 118). Todo card is 220 tall: same x, y 66, 90, 114, 138.
pub const MUSIC_DOT_MUSIC_RECT: (f32, f32, f32, f32) = (492.0, 46.0, 508.0, 62.0);
pub const MUSIC_DOT_POMODORO_RECT: (f32, f32, f32, f32) = (492.0, 70.0, 508.0, 86.0);
pub const MUSIC_DOT_TODO_RECT: (f32, f32, f32, f32) = (492.0, 94.0, 508.0, 110.0);
pub const MUSIC_DOT_WATER_RECT: (f32, f32, f32, f32) = (492.0, 118.0, 508.0, 134.0);
pub const POMODORO_DOT_MUSIC_RECT: (f32, f32, f32, f32) = (612.0, 46.0, 628.0, 62.0);
pub const POMODORO_DOT_POMODORO_RECT: (f32, f32, f32, f32) = (612.0, 70.0, 628.0, 86.0);
pub const POMODORO_DOT_TODO_RECT: (f32, f32, f32, f32) = (612.0, 94.0, 628.0, 110.0);
pub const POMODORO_DOT_WATER_RECT: (f32, f32, f32, f32) = (612.0, 118.0, 628.0, 134.0);
pub const TODO_DOT_MUSIC_RECT: (f32, f32, f32, f32) = (612.0, 66.0, 628.0, 82.0);
pub const TODO_DOT_POMODORO_RECT: (f32, f32, f32, f32) = (612.0, 90.0, 628.0, 106.0);
pub const TODO_DOT_TODO_RECT: (f32, f32, f32, f32) = (612.0, 114.0, 628.0, 130.0);
pub const TODO_DOT_WATER_RECT: (f32, f32, f32, f32) = (612.0, 138.0, 628.0, 154.0);
pub const WATER_DOT_MUSIC_RECT: (f32, f32, f32, f32) = (612.0, 46.0, 628.0, 62.0);
pub const WATER_DOT_POMODORO_RECT: (f32, f32, f32, f32) = (612.0, 70.0, 628.0, 86.0);
pub const WATER_DOT_TODO_RECT: (f32, f32, f32, f32) = (612.0, 94.0, 628.0, 110.0);
pub const WATER_DOT_WATER_RECT: (f32, f32, f32, f32) = (612.0, 118.0, 628.0, 134.0);
/// Todo card: in-pill input field (top row, right of the title) +
/// row geometry (x0, y0, x1, pitch). Click the field to type;
/// Enter adds (stays editing for rapid entry), Esc cancels.
pub const TODO_INPUT_RECT: (f32, f32, f32, f32) = (220.0, 16.0, 616.0, 36.0);
pub const TODO_ROW_GEOM: (f32, f32, f32, f32) = (24.0, 44.0, 600.0, 28.0);
pub const TODO_MAX_ROWS: usize = 6;
/// Water card buttons: +250 ml + Reset (centered pair, y 120-144).
pub const WATER_ADD_RECT: (f32, f32, f32, f32) = (230.0, 120.0, 310.0, 144.0);
pub const WATER_RESET_RECT: (f32, f32, f32, f32) = (330.0, 120.0, 410.0, 144.0);
/// Tray shelf card (640x280, Phase 3a): 4x2 grid, page size 8.
/// Tile 132x96, x pitch 148 (gap 16), y pitch 104 (gap 8), origin (24, 48).
/// Clear button top-right; page arrows bottom (only when >1 page).
/// 5th section dot on every card (tray), below the water dot.
pub const TRAY_PAGE_SIZE: usize = 8;
pub const TRAY_TILE_GEOM: (f32, f32, f32, f32, f32, f32) = (24.0, 48.0, 132.0, 96.0, 148.0, 104.0);
pub const TRAY_CLEAR_RECT: (f32, f32, f32, f32) = (560.0, 16.0, 616.0, 36.0);
pub const TRAY_PREV_RECT: (f32, f32, f32, f32) = (280.0, 252.0, 320.0, 272.0);
pub const TRAY_NEXT_RECT: (f32, f32, f32, f32) = (324.0, 252.0, 364.0, 272.0);
pub const MUSIC_DOT_TRAY_RECT: (f32, f32, f32, f32) = (492.0, 142.0, 508.0, 158.0);
pub const POMODORO_DOT_TRAY_RECT: (f32, f32, f32, f32) = (612.0, 142.0, 628.0, 158.0);
pub const TODO_DOT_TRAY_RECT: (f32, f32, f32, f32) = (612.0, 162.0, 628.0, 178.0);
pub const WATER_DOT_TRAY_RECT: (f32, f32, f32, f32) = (612.0, 142.0, 628.0, 158.0);
pub const TRAY_DOT_MUSIC_RECT: (f32, f32, f32, f32) = (612.0, 66.0, 628.0, 82.0);
pub const TRAY_DOT_POMODORO_RECT: (f32, f32, f32, f32) = (612.0, 90.0, 628.0, 106.0);
pub const TRAY_DOT_TODO_RECT: (f32, f32, f32, f32) = (612.0, 114.0, 628.0, 130.0);
pub const TRAY_DOT_WATER_RECT: (f32, f32, f32, f32) = (612.0, 138.0, 628.0, 154.0);
pub const TRAY_DOT_TRAY_RECT: (f32, f32, f32, f32) = (612.0, 162.0, 628.0, 178.0);

/// Format seconds as m:ss (e.g. 83.0 -> "1:23").
fn format_time(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

pub struct Renderer {
    factory: ID2D1Factory,
    hwnd_target: Option<ID2D1HwndRenderTarget>,
    target: Option<ID2D1RenderTarget>,
    dwrite: IDWriteFactory,
    text_format: Option<IDWriteTextFormat>,
    text_format_small: Option<IDWriteTextFormat>,
    text_format_center: Option<IDWriteTextFormat>,
    text_format_large: Option<IDWriteTextFormat>,
    black_brush: Option<ID2D1SolidColorBrush>,
    white_brush: Option<ID2D1SolidColorBrush>,
    dim_brush: Option<ID2D1SolidColorBrush>,
    gray_brush: Option<ID2D1SolidColorBrush>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            // Factory (single-threaded: all drawing happens on the UI thread).
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self {
                factory,
                hwnd_target: None,
                target: None,
                dwrite,
                text_format: None,
                text_format_small: None,
                text_format_center: None,
                text_format_large: None,
                black_brush: None,
                white_brush: None,
                dim_brush: None,
                gray_brush: None,
            })
        }
    }

    /// Resize the render target to a new PHYSICAL size (DIPs stay logical;
    /// D2D scales via the target DPI set at creation).
    pub fn resize(&mut self, width_px: i32, height_px: i32) {
        // Resize is HWND-target-specific (not on the base render target).
        if let Some(hwnd_target) = &self.hwnd_target {
            unsafe {
                let _ = hwnd_target.Resize(&D2D_SIZE_U {
                    width: width_px.max(1) as u32,
                    height: height_px.max(1) as u32,
                });
            }
        }
    }

    /// Create the render target + brushes + text formats on first draw.
    /// Returns false on failure (caller validates + bails, no crash).
    fn ensure_target(&mut self, hwnd: HWND, width_px: i32, height_px: i32, dpi: f32) -> bool {
        if self.target.is_some() {
            return true;
        }
        unsafe {
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
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U {
                    width: width_px.max(1) as u32,
                    height: height_px.max(1) as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let Ok(hwnd_target) = self.factory.CreateHwndRenderTarget(&props, &hwnd_props) else {
                return false;
            };
            // Drawing methods (Clear, Fill, Draw, DrawText, CreateSolidColorBrush,
            // Resize) live on the base ID2D1RenderTarget, not on ID2D1HwndRenderTarget
            // (windows-rs 0.58 does not Deref COM bases); cast once, store the base.
            let Ok(target) = hwnd_target.cast::<ID2D1RenderTarget>() else {
                return false;
            };
            // Explicit anti-aliasing for fills/strokes (default is already
            // per-primitive; make it explicit: the window region itself
            // cannot anti-alias, so D2D AA is what keeps inner edges smooth).
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            // Brushes (None = default brush properties: opacity 1.0, identity).
            if let Ok(b) = hwnd_target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.98,
                },
                None,
            ) {
                self.black_brush = Some(b);
            }
            if let Ok(b) = hwnd_target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            ) {
                self.white_brush = Some(b);
            }
            if let Ok(b) = hwnd_target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 0.15,
                },
                None,
            ) {
                self.dim_brush = Some(b);
            }
            if let Ok(b) = hwnd_target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 0.5,
                },
                None,
            ) {
                // Gray (secondary lyric lines around the current one).
                self.gray_brush = Some(b);
            }
            // Text formats (Segoe UI, the Windows system font replacing SF Pro).
            if let Ok(f) = self.dwrite.CreateTextFormat(
                windows::core::w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                windows::core::w!("en-US"),
            ) {
                self.text_format = Some(f);
            }
            if let Ok(f) = self.dwrite.CreateTextFormat(
                windows::core::w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                11.0,
                windows::core::w!("en-US"),
            ) {
                self.text_format_small = Some(f);
            }
            if let Ok(f) = self.dwrite.CreateTextFormat(
                windows::core::w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                windows::core::w!("en-US"),
            ) {
                // Centered (for the Zen pill label). Alignment failures are
                // non-fatal: text just falls back to left-aligned.
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                self.text_format_center = Some(f);
            }
            if let Ok(f) = self.dwrite.CreateTextFormat(
                windows::core::w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                20.0,
                windows::core::w!("en-US"),
            ) {
                // Large centered (for music-card control symbols).
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                self.text_format_large = Some(f);
            }
            self.hwnd_target = Some(hwnd_target);
            self.target = Some(target);
            true
        }
    }

    /// Draw one frame for the given state. Coordinates are LOGICAL points
    /// (Aura Mac points); D2D scales to physical pixels via the target DPI.
    pub fn draw(
        &mut self,
        hwnd: HWND,
        state: IslandState,
        clock_text: &str,
        media: Option<&crate::media::MediaInfo>,
        lyrics: Option<&crate::lyrics::LyricsCache>,
        pomodoro: &crate::pomodoro::Pomodoro,
        todo: &crate::todo::TodoList,
        water: &crate::water::Water,
        todo_editing: bool,
        todo_input: &str,
        shelf: &crate::shelf::Shelf,
        tray_page: usize,
        cur_w: f32,
        cur_h: f32,
        poll_status: &str,
    ) {
        // Current animated size, NOT the target size (double-pill fix
        // 2026-10-27): during a Tray (640x280) -> Music (520x180)
        // morph the window is intermediate-sized, but the old code
        // drew the target-size card (with its own stroke outline) on
        // top of the intermediate window's black clear, so it looked
        // like two overlapping pills. Drawing background + layout at
        // the current size makes the morph a single pill that shrinks.
        // At rest cur == target, so nothing changes then.
        let scale = window::dpi_scale(hwnd);
        let (lw, lh) = (cur_w, cur_h);
        let w_px = (lw * scale).round() as i32;
        let h_px = (lh * scale).round() as i32;
        if !self.ensure_target(hwnd, w_px, h_px, scale * 96.0) {
            // Render target creation failed; validate to avoid a paint loop, then bail.
            unsafe {
                let _ = ValidateRect(hwnd, None);
            }
            return;
        }
        let target = self.target.as_ref().unwrap();
        unsafe {
            target.BeginDraw();
            // Clear to transparent (outside the pill is clipped by the window region anyway).
            target.Clear(None);
            // Pill/card background: Aura notch shape (wide top merging
            // into the top edge/bezel, tucked sides, rounded bottom).
            // Drawn as a path
            // geometry so fill + stroke follow the shape exactly. Falls
            // back to a plain rect (clipped by the window region to the
            // same silhouette) if path creation fails.
            let rect = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: lw,
                bottom: lh,
            };
            let radius = if lh <= 33.0 { lh / 2.0 } else { 28.0 };
            if let Some(geometry) = self.nusa_geometry(lw, lh, radius) {
                if let Some(brush) = &self.black_brush {
                    target.FillGeometry(&geometry, brush, None);
                }
                if let Some(brush) = &self.dim_brush {
                    target.DrawGeometry(&geometry, brush, 1.0, None); // 1.0, not Mac 0.5: region clips outer half, so visible inner stroke is ~0.5px.
                }
            } else {
                // Fallback: plain rect (window region clips bottom to rounded).
                if let Some(brush) = &self.black_brush {
                    target.FillRectangle(&rect, brush);
                }
                if let Some(brush) = &self.dim_brush {
                    target.DrawRectangle(&rect, brush, 1.0, None); // Same 1.0 stroke (see above).
                }
            }
            // Content per state (Phase 0 placeholders).
            match state {
                IslandState::Zen => {
                    // Idle pill: centered "Aura" label (idle-eyes drawing
                    // arrives with the idle-eyes polish; shape + position
                    // are what Phase 0 proves).
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        self.draw_text(target, "Aura", fmt, &rect, brush);
                    }
                }
                IslandState::Compact => {
                    // Clock (left) + decorative waveform bars (right).
                    // Phase 0 has no battery yet (Phase 4), so: clock left, waveform right.
                    if let (Some(fmt), Some(brush)) = (&self.text_format, &self.white_brush)
                    {
                        let clock_rect = D2D_RECT_F {
                            left: 12.0,
                            top: 0.0,
                            right: 70.0,
                            bottom: lh,
                        };
                        self.draw_text(target, clock_text, fmt, &clock_rect, brush);
                    }
                    if let Some(brush) = &self.white_brush {
                        // 12 bars, each 2px wide + 2px gap = 4px per bar (48px total),
                        // right-aligned with a 12px right margin.
                        let pattern = [
                            4.0, 8.0, 12.0, 8.0, 4.0, 10.0, 6.0, 12.0, 8.0, 4.0, 8.0, 6.0,
                        ];
                        let mut x = lw - 12.0 - 48.0;
                        for h in pattern {
                            let bar = D2D_RECT_F {
                                left: x,
                                top: (lh - h) / 2.0,
                                right: x + 2.0,
                                bottom: (lh + h) / 2.0,
                            };
                            target.FillRectangle(&bar, brush);
                            x += 4.0;
                        }
                    }
                }
                IslandState::Expanded => {
                    match media {
                        Some(m) => {
                            // Cover placeholder box (56x56) + note glyph.
                            // (GSMTC thumbnail decode arrives after basic
                            // music works; placeholder until then.)
                            if let Some(brush) = &self.dim_brush {
                                let cover = D2D_RECT_F {
                                    left: 24.0,
                                    top: 28.0,
                                    right: 80.0,
                                    bottom: 84.0,
                                };
                                target.FillRectangle(&cover, brush);
                            }
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_center, &self.white_brush)
                            {
                                let cover = D2D_RECT_F {
                                    left: 24.0,
                                    top: 28.0,
                                    right: 80.0,
                                    bottom: 84.0,
                                };
                                self.draw_text(target, "♪", fmt, &cover, brush);
                            }
                            // Title + artist (right of cover).
                            let title = if m.title.is_empty() {
                                "Unknown title"
                            } else {
                                m.title.as_str()
                            };
                            let artist = if m.artist.is_empty() {
                                "Unknown artist"
                            } else {
                                m.artist.as_str()
                            };
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format, &self.white_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 96.0,
                                    top: 28.0,
                                    right: 432.0,
                                    bottom: 54.0,
                                };
                                self.draw_text(target, title, fmt, &r, brush);
                            }
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_small, &self.white_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 96.0,
                                    top: 54.0,
                                    right: 496.0,
                                    bottom: 76.0,
                                };
                                self.draw_text(target, artist, fmt, &r, brush);
                            }
                            // Progress track + fill + times.
                            let frac = if m.duration_secs > 0.0 {
                                (m.position_secs / m.duration_secs).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            if let Some(brush) = &self.dim_brush {
                                let track = D2D_RECT_F {
                                    left: 24.0,
                                    top: 96.0,
                                    right: 496.0,
                                    bottom: 100.0,
                                };
                                target.FillRectangle(&track, brush);
                            }
                            if let Some(brush) = &self.white_brush {
                                let fill = D2D_RECT_F {
                                    left: 24.0,
                                    top: 96.0,
                                    right: 24.0 + 472.0 * (frac as f32),
                                    bottom: 100.0,
                                };
                                target.FillRectangle(&fill, brush);
                            }
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_small, &self.white_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 24.0,
                                    top: 104.0,
                                    right: 300.0,
                                    bottom: 122.0,
                                };
                                let times = format!(
                                    "{} / {}",
                                    format_time(m.position_secs),
                                    format_time(m.duration_secs)
                                );
                                self.draw_text(target, &times, fmt, &r, brush);
                            }
                            // Controls (prev / play-pause / next).
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_large, &self.white_brush)
                            {
                                let prev = D2D_RECT_F {
                                    left: MUSIC_PREV_RECT.0,
                                    top: MUSIC_PREV_RECT.1,
                                    right: MUSIC_PREV_RECT.2,
                                    bottom: MUSIC_PREV_RECT.3,
                                };
                                self.draw_text(target, "⏮", fmt, &prev, brush);
                                let play = D2D_RECT_F {
                                    left: MUSIC_PLAY_RECT.0,
                                    top: MUSIC_PLAY_RECT.1,
                                    right: MUSIC_PLAY_RECT.2,
                                    bottom: MUSIC_PLAY_RECT.3,
                                };
                                self.draw_text(
                                    target,
                                    if m.playing { "⏸" } else { "▶" },
                                    fmt,
                                    &play,
                                    brush,
                                );
                                let next = D2D_RECT_F {
                                    left: MUSIC_NEXT_RECT.0,
                                    top: MUSIC_NEXT_RECT.1,
                                    right: MUSIC_NEXT_RECT.2,
                                    bottom: MUSIC_NEXT_RECT.3,
                                };
                                self.draw_text(target, "⏭", fmt, &next, brush);
                            }
                            // Bubble toggle to lyrics (top-right, dim pill + label).
                            if let Some(brush) = &self.dim_brush {
                                let bubble = D2D_RECT_F {
                                    left: MUSIC_LYRICS_BUBBLE_RECT.0,
                                    top: MUSIC_LYRICS_BUBBLE_RECT.1,
                                    right: MUSIC_LYRICS_BUBBLE_RECT.2,
                                    bottom: MUSIC_LYRICS_BUBBLE_RECT.3,
                                };
                                target.FillRectangle(&bubble, brush);
                            }
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_center, &self.white_brush)
                            {
                                let bubble = D2D_RECT_F {
                                    left: MUSIC_LYRICS_BUBBLE_RECT.0,
                                    top: MUSIC_LYRICS_BUBBLE_RECT.1,
                                    right: MUSIC_LYRICS_BUBBLE_RECT.2,
                                    bottom: MUSIC_LYRICS_BUBBLE_RECT.3,
                                };
                                self.draw_text(target, "Lirik", fmt, &bubble, brush);
                            }
                        }
                        None => {
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format, &self.white_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 24.0,
                                    top: 40.0,
                                    right: 496.0,
                                    bottom: 70.0,
                                };
                                self.draw_text(target, "No music playing", fmt, &r, brush);
                            }
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_small, &self.white_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 24.0,
                                    top: 70.0,
                                    right: 496.0,
                                    bottom: 100.0,
                                };
                                self.draw_text(
                                    target,
                                    "Play something in Spotify or YouTube Music",
                                    fmt,
                                    &r,
                                    brush,
                                );
                            }
                            // Diagnostic line (temporary, until detection
                            // is proven): last GSMTC poll status, so a
                            // screenshot shows whether the poller runs,
                            // finds no session, or fails at a step.
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_small, &self.gray_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 24.0,
                                    top: 100.0,
                                    right: 496.0,
                                    bottom: 130.0,
                                };
                                self.draw_text(
                                    target,
                                    &format!("GSMTC: {}", poll_status),
                                    fmt,
                                    &r,
                                    brush,
                                );
                            }
                        }
                    }
                    // Section dots (right edge, 4): music (current, white),
                    // others gray. Clickable (hit-test in app).
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        let dot_music = D2D_RECT_F {
                            left: lw - 28.0,
                            top: MUSIC_DOT_MUSIC_RECT.1,
                            right: lw - 12.0,
                            bottom: MUSIC_DOT_MUSIC_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_music, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_pomodoro = D2D_RECT_F {
                            left: lw - 28.0,
                            top: MUSIC_DOT_POMODORO_RECT.1,
                            right: lw - 12.0,
                            bottom: MUSIC_DOT_POMODORO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_pomodoro, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_todo = D2D_RECT_F {
                            left: lw - 28.0,
                            top: MUSIC_DOT_TODO_RECT.1,
                            right: lw - 12.0,
                            bottom: MUSIC_DOT_TODO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_todo, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_water = D2D_RECT_F {
                            left: lw - 28.0,
                            top: MUSIC_DOT_WATER_RECT.1,
                            right: lw - 12.0,
                            bottom: MUSIC_DOT_WATER_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_water, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_tray = D2D_RECT_F {
                            left: lw - 28.0,
                            top: MUSIC_DOT_TRAY_RECT.1,
                            right: lw - 12.0,
                            bottom: MUSIC_DOT_TRAY_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_tray, brush);
                    }
                }
                IslandState::Lyrics => {
                    // Lyrics panel (700x180). App only enters Lyrics after a
                    // fetch, so cache is Some in practice; None falls back to
                    // a loading line (never blank, never a crash).
                    match lyrics {
                        None => {
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_center, &self.white_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 24.0,
                                    top: 60.0,
                                    right: 676.0,
                                    bottom: 100.0,
                                };
                                self.draw_text(target, "Loading lyrics...", fmt, &r, brush);
                            }
                        }
                        Some(cache) => {
                            // Title line (song - artist, from current media).
                            let title_line = match media {
                                Some(m) => format!("{} - {}", m.title, m.artist),
                                None => String::new(),
                            };
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_small, &self.white_brush)
                            {
                                let r = D2D_RECT_F {
                                    left: 24.0,
                                    top: 16.0,
                                    right: 500.0,
                                    bottom: 36.0,
                                };
                                self.draw_text(target, &title_line, fmt, &r, brush);
                            }
                            // Bubble back to music (top-right, dim pill + label).
                            if let Some(brush) = &self.dim_brush {
                                let bubble = D2D_RECT_F {
                                    left: LYRICS_MUSIC_BUBBLE_RECT.0,
                                    top: LYRICS_MUSIC_BUBBLE_RECT.1,
                                    right: LYRICS_MUSIC_BUBBLE_RECT.2,
                                    bottom: LYRICS_MUSIC_BUBBLE_RECT.3,
                                };
                                target.FillRectangle(&bubble, brush);
                            }
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_center, &self.white_brush)
                            {
                                let bubble = D2D_RECT_F {
                                    left: LYRICS_MUSIC_BUBBLE_RECT.0,
                                    top: LYRICS_MUSIC_BUBBLE_RECT.1,
                                    right: LYRICS_MUSIC_BUBBLE_RECT.2,
                                    bottom: LYRICS_MUSIC_BUBBLE_RECT.3,
                                };
                                self.draw_text(target, "Musik", fmt, &bubble, brush);
                            }
                            if cache.lines.is_empty() {
                                if let (Some(fmt), Some(brush)) =
                                    (&self.text_format_center, &self.white_brush)
                                {
                                    let r = D2D_RECT_F {
                                        left: 24.0,
                                        top: 60.0,
                                        right: 676.0,
                                        bottom: 100.0,
                                    };
                                    self.draw_text(
                                        target,
                                        "Lyrics not available for this song",
                                        fmt,
                                        &r,
                                        brush,
                                    );
                                }
                            } else {
                                // Five lines around the current one (synced) or
                                // the first five (plain). Current is white,
                                // neighbours are gray (0.5); plain lines are
                                // all white (no current to highlight).
                                let position_ms = media
                                    .map(|m| (m.position_secs * 1000.0) as i64)
                                    .unwrap_or(0)
                                    + cache.nudge_ms;
                                let current = if cache.synced {
                                    crate::lyrics::current_index(&cache.lines, position_ms)
                                } else {
                                    usize::MAX
                                };
                                let start = if cache.synced {
                                    current.saturating_sub(2)
                                } else {
                                    0
                                };
                                for i in 0..5 {
                                    let idx = start + i;
                                    if idx >= cache.lines.len() {
                                        break;
                                    }
                                    let line = &cache.lines[idx];
                                    let brush = if !cache.synced || idx == current {
                                        &self.white_brush
                                    } else {
                                        &self.gray_brush
                                    };
                                    if let (Some(fmt), Some(brush)) =
                                        (&self.text_format_center, brush)
                                    {
                                        let top = 40.0 + (i as f32) * 20.0;
                                        let r = D2D_RECT_F {
                                            left: 24.0,
                                            top,
                                            right: 676.0,
                                            bottom: top + 20.0,
                                        };
                                        self.draw_text(target, &line.text, fmt, &r, brush);
                                    }
                                }
                            }
                            // Progress track + fill + times (when media exists).
                            if let Some(m) = media {
                                let frac = if m.duration_secs > 0.0 {
                                    (m.position_secs / m.duration_secs).clamp(0.0, 1.0)
                                } else {
                                    0.0
                                };
                                if let Some(brush) = &self.dim_brush {
                                    let track = D2D_RECT_F {
                                        left: 24.0,
                                        top: 144.0,
                                        right: 676.0,
                                        bottom: 148.0,
                                    };
                                    target.FillRectangle(&track, brush);
                                }
                                if let Some(brush) = &self.white_brush {
                                    let fill = D2D_RECT_F {
                                        left: 24.0,
                                        top: 144.0,
                                        right: 24.0 + 652.0 * (frac as f32),
                                        bottom: 148.0,
                                    };
                                    target.FillRectangle(&fill, brush);
                                }
                                if let (Some(fmt), Some(brush)) =
                                    (&self.text_format_small, &self.white_brush)
                                {
                                    let r = D2D_RECT_F {
                                        left: 24.0,
                                        top: 150.0,
                                        right: 200.0,
                                        bottom: 172.0,
                                    };
                                    let times = format!(
                                        "{} / {}",
                                        format_time(m.position_secs),
                                        format_time(m.duration_secs)
                                    );
                                    self.draw_text(target, &times, fmt, &r, brush);
                                }
                            }
                            // Nudge buttons (synced lyrics only).
                            if cache.synced {
                                if let Some(brush) = &self.dim_brush {
                                    let minus = D2D_RECT_F {
                                        left: LYRICS_NUDGE_MINUS_RECT.0,
                                        top: LYRICS_NUDGE_MINUS_RECT.1,
                                        right: LYRICS_NUDGE_MINUS_RECT.2,
                                        bottom: LYRICS_NUDGE_MINUS_RECT.3,
                                    };
                                    target.FillRectangle(&minus, brush);
                                    let plus = D2D_RECT_F {
                                        left: LYRICS_NUDGE_PLUS_RECT.0,
                                        top: LYRICS_NUDGE_PLUS_RECT.1,
                                        right: LYRICS_NUDGE_PLUS_RECT.2,
                                        bottom: LYRICS_NUDGE_PLUS_RECT.3,
                                    };
                                    target.FillRectangle(&plus, brush);
                                }
                                if let (Some(fmt), Some(brush)) =
                                    (&self.text_format_center, &self.white_brush)
                                {
                                    let minus = D2D_RECT_F {
                                        left: LYRICS_NUDGE_MINUS_RECT.0,
                                        top: LYRICS_NUDGE_MINUS_RECT.1,
                                        right: LYRICS_NUDGE_MINUS_RECT.2,
                                        bottom: LYRICS_NUDGE_MINUS_RECT.3,
                                    };
                                    self.draw_text(target, "-0.5s", fmt, &minus, brush);
                                    let plus = D2D_RECT_F {
                                        left: LYRICS_NUDGE_PLUS_RECT.0,
                                        top: LYRICS_NUDGE_PLUS_RECT.1,
                                        right: LYRICS_NUDGE_PLUS_RECT.2,
                                        bottom: LYRICS_NUDGE_PLUS_RECT.3,
                                    };
                                    self.draw_text(target, "+0.5s", fmt, &plus, brush);
                                }
                            }
                        }
                    }
                }
                IslandState::Pomodoro => {
                    // Pomodoro card (640x180, in-memory, wall-clock).
                    // Timer (large, centered) + mode pills + Start/Pause +
                    // section dots. Ring is a follow-up (not in this increment).
                    let remaining = pomodoro.remaining(chrono::Local::now());
                    let timer_text = format!("{:02}:{:02}", remaining / 60, remaining % 60);
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_large, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: 220.0,
                            top: 30.0,
                            right: 420.0,
                            bottom: 80.0,
                        };
                        self.draw_text(target, &timer_text, fmt, &r, brush);
                    }
                    // Mode pills: selected = white fill + black text,
                    // unselected = dim fill + white text.
                    let focus_selected =
                        pomodoro.mode == crate::pomodoro::PomodoroMode::Focus;
                    if focus_selected {
                        if let Some(brush) = &self.white_brush {
                            let r = D2D_RECT_F {
                                left: POMODORO_FOCUS_RECT.0,
                                top: POMODORO_FOCUS_RECT.1,
                                right: POMODORO_FOCUS_RECT.2,
                                bottom: POMODORO_FOCUS_RECT.3,
                            };
                            target.FillRectangle(&r, brush);
                        }
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, &self.black_brush)
                        {
                            let r = D2D_RECT_F {
                                left: POMODORO_FOCUS_RECT.0,
                                top: POMODORO_FOCUS_RECT.1,
                                right: POMODORO_FOCUS_RECT.2,
                                bottom: POMODORO_FOCUS_RECT.3,
                            };
                            self.draw_text(target, "Focus", fmt, &r, brush);
                        }
                    } else {
                        if let Some(brush) = &self.dim_brush {
                            let r = D2D_RECT_F {
                                left: POMODORO_FOCUS_RECT.0,
                                top: POMODORO_FOCUS_RECT.1,
                                right: POMODORO_FOCUS_RECT.2,
                                bottom: POMODORO_FOCUS_RECT.3,
                            };
                            target.FillRectangle(&r, brush);
                        }
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, &self.white_brush)
                        {
                            let r = D2D_RECT_F {
                                left: POMODORO_FOCUS_RECT.0,
                                top: POMODORO_FOCUS_RECT.1,
                                right: POMODORO_FOCUS_RECT.2,
                                bottom: POMODORO_FOCUS_RECT.3,
                            };
                            self.draw_text(target, "Focus", fmt, &r, brush);
                        }
                    }
                    // Break pill (mirror of Focus, selected state inverted).
                    if !focus_selected {
                        if let Some(brush) = &self.white_brush {
                            let r = D2D_RECT_F {
                                left: POMODORO_BREAK_RECT.0,
                                top: POMODORO_BREAK_RECT.1,
                                right: POMODORO_BREAK_RECT.2,
                                bottom: POMODORO_BREAK_RECT.3,
                            };
                            target.FillRectangle(&r, brush);
                        }
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, &self.black_brush)
                        {
                            let r = D2D_RECT_F {
                                left: POMODORO_BREAK_RECT.0,
                                top: POMODORO_BREAK_RECT.1,
                                right: POMODORO_BREAK_RECT.2,
                                bottom: POMODORO_BREAK_RECT.3,
                            };
                            self.draw_text(target, "Break", fmt, &r, brush);
                        }
                    } else {
                        if let Some(brush) = &self.dim_brush {
                            let r = D2D_RECT_F {
                                left: POMODORO_BREAK_RECT.0,
                                top: POMODORO_BREAK_RECT.1,
                                right: POMODORO_BREAK_RECT.2,
                                bottom: POMODORO_BREAK_RECT.3,
                            };
                            target.FillRectangle(&r, brush);
                        }
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, &self.white_brush)
                        {
                            let r = D2D_RECT_F {
                                left: POMODORO_BREAK_RECT.0,
                                top: POMODORO_BREAK_RECT.1,
                                right: POMODORO_BREAK_RECT.2,
                                bottom: POMODORO_BREAK_RECT.3,
                            };
                            self.draw_text(target, "Break", fmt, &r, brush);
                        }
                    }
                    // Start/Pause button (dim pill + white label).
                    if let Some(brush) = &self.dim_brush {
                        let r = D2D_RECT_F {
                            left: POMODORO_START_RECT.0,
                            top: POMODORO_START_RECT.1,
                            right: POMODORO_START_RECT.2,
                            bottom: POMODORO_START_RECT.3,
                        };
                        target.FillRectangle(&r, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: POMODORO_START_RECT.0,
                            top: POMODORO_START_RECT.1,
                            right: POMODORO_START_RECT.2,
                            bottom: POMODORO_START_RECT.3,
                        };
                        self.draw_text(target, pomodoro.button_label(), fmt, &r, brush);
                    }
                    // Section dots (4): music gray, pomodoro white
                    // (current), todo gray, water gray.
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_music = D2D_RECT_F {
                            left: lw - 28.0,
                            top: POMODORO_DOT_MUSIC_RECT.1,
                            right: lw - 12.0,
                            bottom: POMODORO_DOT_MUSIC_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_music, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        let dot_pomodoro = D2D_RECT_F {
                            left: lw - 28.0,
                            top: POMODORO_DOT_POMODORO_RECT.1,
                            right: lw - 12.0,
                            bottom: POMODORO_DOT_POMODORO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_pomodoro, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_todo = D2D_RECT_F {
                            left: lw - 28.0,
                            top: POMODORO_DOT_TODO_RECT.1,
                            right: lw - 12.0,
                            bottom: POMODORO_DOT_TODO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_todo, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_water = D2D_RECT_F {
                            left: lw - 28.0,
                            top: POMODORO_DOT_WATER_RECT.1,
                            right: lw - 12.0,
                            bottom: POMODORO_DOT_WATER_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_water, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_tray = D2D_RECT_F {
                            left: lw - 28.0,
                            top: POMODORO_DOT_TRAY_RECT.1,
                            right: lw - 12.0,
                            bottom: POMODORO_DOT_TRAY_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_tray, brush);
                    }
                }
                IslandState::Todo => {
                    // Todo card (640x220). Title + input field + up to 6 rows.
                    // Tasks from %APPDATA%/Aura/todo.txt (reloaded on entry).
                    // Click a row toggles done; click the field to type
                    // a task in the pill (Enter adds, Esc cancels).
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: 24.0,
                            top: 16.0,
                            right: 200.0,
                            bottom: 36.0,
                        };
                        self.draw_text(target, "Todo", fmt, &r, brush);
                    }
                    // Input field (dim pill + typed text / placeholder).
                    // Long input shows its tail (last chars) so the user
                    // always sees what they are typing; the full string
                    // is what gets saved on Enter / click-outside.
                    if let Some(brush) = &self.dim_brush {
                        let r = D2D_RECT_F {
                            left: TODO_INPUT_RECT.0,
                            top: TODO_INPUT_RECT.1,
                            right: TODO_INPUT_RECT.2,
                            bottom: TODO_INPUT_RECT.3,
                        };
                        target.FillRectangle(&r, brush);
                    }
                    {
                        let shown: String = if todo_editing {
                            let tail: String = todo_input
                                .chars()
                                .rev()
                                .take(36)
                                .collect::<Vec<char>>()
                                .into_iter()
                                .rev()
                                .collect();
                            if tail.is_empty() {
                                "|".to_string()
                            } else {
                                format!("{}|", tail)
                            }
                        } else {
                            "Add task...".to_string()
                        };
                        let brush = if todo_editing {
                            &self.white_brush
                        } else {
                            &self.gray_brush
                        };
                        if let (Some(fmt), Some(brush)) = (&self.text_format, brush) {
                            let r = D2D_RECT_F {
                                left: TODO_INPUT_RECT.0 + 8.0,
                                top: TODO_INPUT_RECT.1,
                                right: TODO_INPUT_RECT.2 - 8.0,
                                bottom: TODO_INPUT_RECT.3,
                            };
                            self.draw_text(target, &shown, fmt, &r, brush);
                        }
                    }
                    if todo.tasks.is_empty() {
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, &self.white_brush)
                        {
                            let r = D2D_RECT_F {
                                left: 24.0,
                                top: 80.0,
                                right: 616.0,
                                bottom: 100.0,
                            };
                            self.draw_text(
                                target,
                                "No tasks yet - click the field above to add one",
                                fmt,
                                &r,
                                brush,
                            );
                        }
                    } else {
                        for (i, task) in todo.tasks.iter().take(6).enumerate() {
                            let top = TODO_ROW_GEOM.1 + (i as f32) * TODO_ROW_GEOM.3;
                            let r = D2D_RECT_F {
                                left: TODO_ROW_GEOM.0,
                                top,
                                right: TODO_ROW_GEOM.2,
                                bottom: top + 24.0,
                            };
                            let checkbox = if task.done { "☑" } else { "☐" };
                            let text = format!("{} {}", checkbox, task.title);
                            let brush = if task.done {
                                &self.gray_brush
                            } else {
                                &self.white_brush
                            };
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format, brush)
                            {
                                self.draw_text(target, &text, fmt, &r, brush);
                            }
                        }
                    }
                    // Section dots (4, x 612-628, y 66/90/114/138).
                    // Music gray, pomodoro gray, todo white (current), water gray.
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_music = D2D_RECT_F {
                            left: lw - 28.0,
                            top: TODO_DOT_MUSIC_RECT.1,
                            right: lw - 12.0,
                            bottom: TODO_DOT_MUSIC_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_music, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_pomodoro = D2D_RECT_F {
                            left: lw - 28.0,
                            top: TODO_DOT_POMODORO_RECT.1,
                            right: lw - 12.0,
                            bottom: TODO_DOT_POMODORO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_pomodoro, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        let dot_todo = D2D_RECT_F {
                            left: lw - 28.0,
                            top: TODO_DOT_TODO_RECT.1,
                            right: lw - 12.0,
                            bottom: TODO_DOT_TODO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_todo, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_water = D2D_RECT_F {
                            left: lw - 28.0,
                            top: TODO_DOT_WATER_RECT.1,
                            right: lw - 12.0,
                            bottom: TODO_DOT_WATER_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_water, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_tray = D2D_RECT_F {
                            left: lw - 28.0,
                            top: TODO_DOT_TRAY_RECT.1,
                            right: lw - 12.0,
                            bottom: TODO_DOT_TRAY_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_tray, brush);
                    }
                }
                IslandState::Water => {
                    // Water card (640x180). Total / goal + progress + buttons.
                    // Goal 1500 ml, glass 250 ml (fixed for now).
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: 24.0,
                            top: 16.0,
                            right: 200.0,
                            bottom: 36.0,
                        };
                        self.draw_text(target, "Water", fmt, &r, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_large, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: 220.0,
                            top: 40.0,
                            right: 420.0,
                            bottom: 80.0,
                        };
                        let total_text = format!("{} / {} ml", water.ml, water.goal_ml);
                        self.draw_text(target, &total_text, fmt, &r, brush);
                    }
                    // Progress track + fill (like music progress).
                    let frac = if water.goal_ml > 0 {
                        (water.ml as f32 / water.goal_ml as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    if let Some(brush) = &self.dim_brush {
                        let track = D2D_RECT_F {
                            left: 24.0,
                            top: 100.0,
                            right: 616.0,
                            bottom: 104.0,
                        };
                        target.FillRectangle(&track, brush);
                    }
                    if let Some(brush) = &self.white_brush {
                        let fill = D2D_RECT_F {
                            left: 24.0,
                            top: 100.0,
                            right: 24.0 + 592.0 * frac,
                            bottom: 104.0,
                        };
                        target.FillRectangle(&fill, brush);
                    }
                    // +250 button + Reset button (dim pills + labels).
                    if let Some(brush) = &self.dim_brush {
                        let r = D2D_RECT_F {
                            left: WATER_ADD_RECT.0,
                            top: WATER_ADD_RECT.1,
                            right: WATER_ADD_RECT.2,
                            bottom: WATER_ADD_RECT.3,
                        };
                        target.FillRectangle(&r, brush);
                        let r2 = D2D_RECT_F {
                            left: WATER_RESET_RECT.0,
                            top: WATER_RESET_RECT.1,
                            right: WATER_RESET_RECT.2,
                            bottom: WATER_RESET_RECT.3,
                        };
                        target.FillRectangle(&r2, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: WATER_ADD_RECT.0,
                            top: WATER_ADD_RECT.1,
                            right: WATER_ADD_RECT.2,
                            bottom: WATER_ADD_RECT.3,
                        };
                        self.draw_text(target, "+250 ml", fmt, &r, brush);
                        let r2 = D2D_RECT_F {
                            left: WATER_RESET_RECT.0,
                            top: WATER_RESET_RECT.1,
                            right: WATER_RESET_RECT.2,
                            bottom: WATER_RESET_RECT.3,
                        };
                        self.draw_text(target, "Reset", fmt, &r2, brush);
                    }
                    // Section dots (4, x 612-628, y 46/70/94/118).
                    // Music gray, pomodoro gray, todo gray, water white.
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_music = D2D_RECT_F {
                            left: lw - 28.0,
                            top: WATER_DOT_MUSIC_RECT.1,
                            right: lw - 12.0,
                            bottom: WATER_DOT_MUSIC_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_music, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_pomodoro = D2D_RECT_F {
                            left: lw - 28.0,
                            top: WATER_DOT_POMODORO_RECT.1,
                            right: lw - 12.0,
                            bottom: WATER_DOT_POMODORO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_pomodoro, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_todo = D2D_RECT_F {
                            left: lw - 28.0,
                            top: WATER_DOT_TODO_RECT.1,
                            right: lw - 12.0,
                            bottom: WATER_DOT_TODO_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_todo, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        let dot_water = D2D_RECT_F {
                            left: lw - 28.0,
                            top: WATER_DOT_WATER_RECT.1,
                            right: lw - 12.0,
                            bottom: WATER_DOT_WATER_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_water, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.gray_brush)
                    {
                        let dot_tray = D2D_RECT_F {
                            left: lw - 28.0,
                            top: WATER_DOT_TRAY_RECT.1,
                            right: lw - 12.0,
                            bottom: WATER_DOT_TRAY_RECT.3,
                        };
                        self.draw_text(target, "•", fmt, &dot_tray, brush);
                    }
                }
                IslandState::Tray => {
                    // Tray shelf card (640x280, Phase 3a). Title + count,
                    // Clear button, 4x2 grid of references (or an empty
                    // drop-zone box), page arrows when >1 page, 5 dots.
                    // Click a tile opens it (app), right-click removes
                    // the reference (app); Clear removes all references.
                    // Nothing here deletes a real file. Drag-out + LAN
                    // share arrive in Phase 3b.
                    let title = format!("Tray ({})", shelf.files.len());
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: 24.0,
                            top: 16.0,
                            right: 400.0,
                            bottom: 36.0,
                        };
                        self.draw_text(target, &title, fmt, &r, brush);
                    }
                    if let Some(brush) = &self.dim_brush {
                        let r = D2D_RECT_F {
                            left: TRAY_CLEAR_RECT.0,
                            top: TRAY_CLEAR_RECT.1,
                            right: TRAY_CLEAR_RECT.2,
                            bottom: TRAY_CLEAR_RECT.3,
                        };
                        target.FillRectangle(&r, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_center, &self.white_brush)
                    {
                        let r = D2D_RECT_F {
                            left: TRAY_CLEAR_RECT.0,
                            top: TRAY_CLEAR_RECT.1,
                            right: TRAY_CLEAR_RECT.2,
                            bottom: TRAY_CLEAR_RECT.3,
                        };
                        self.draw_text(target, "Clear", fmt, &r, brush);
                    }
                    let page_count = shelf.page_count(TRAY_PAGE_SIZE);
                    let page = tray_page.min(page_count.saturating_sub(1));
                    if shelf.files.is_empty() {
                        if let Some(brush) = &self.dim_brush {
                            let r = D2D_RECT_F {
                                left: 24.0,
                                top: 48.0,
                                right: 600.0,
                                bottom: 248.0,
                            };
                            target.FillRectangle(&r, brush);
                        }
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, &self.white_brush)
                        {
                            let r = D2D_RECT_F {
                                left: 24.0,
                                top: 120.0,
                                right: 600.0,
                                bottom: 150.0,
                            };
                            self.draw_text(target, "Drag files here", fmt, &r, brush);
                        }
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, &self.gray_brush)
                        {
                            let r = D2D_RECT_F {
                                left: 24.0,
                                top: 150.0,
                                right: 600.0,
                                bottom: 175.0,
                            };
                            self.draw_text(
                                target,
                                "Drop a file from Explorer onto the pill",
                                fmt,
                                &r,
                                brush,
                            );
                        }
                    } else {
                        let (gx, gy, tw, th, px, py) = (
                            TRAY_TILE_GEOM.0,
                            TRAY_TILE_GEOM.1,
                            TRAY_TILE_GEOM.2,
                            TRAY_TILE_GEOM.3,
                            TRAY_TILE_GEOM.4,
                            TRAY_TILE_GEOM.5,
                        );
                        let start = page * TRAY_PAGE_SIZE;
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
                            if let Some(brush) = &self.dim_brush {
                                let r = D2D_RECT_F {
                                    left,
                                    top,
                                    right: left + tw,
                                    bottom: top + th,
                                };
                                target.FillRectangle(&r, brush);
                            }
                            let name = path
                                .rsplit(['\\', '/'])
                                .next()
                                .unwrap_or(path.as_str());
                            let name_chars: Vec<char> = name.chars().collect();
                            let shown = if name_chars.len() > 20 {
                                format!(
                                    "{}...",
                                    name_chars.iter().take(17).collect::<String>()
                                )
                            } else {
                                name.to_string()
                            };
                            let exists =
                                std::path::Path::new(path.as_str()).exists();
                            let brush = if exists {
                                &self.white_brush
                            } else {
                                &self.gray_brush
                            };
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format, brush)
                            {
                                let r = D2D_RECT_F {
                                    left: left + 8.0,
                                    top: top + 8.0,
                                    right: left + tw - 8.0,
                                    bottom: top + th - 8.0,
                                };
                                self.draw_text(target, &shown, fmt, &r, brush);
                            }
                            if !exists {
                                if let (Some(fmt), Some(brush)) =
                                    (&self.text_format_small, &self.gray_brush)
                                {
                                    let r = D2D_RECT_F {
                                        left: left + 8.0,
                                        top: top + th - 28.0,
                                        right: left + tw - 8.0,
                                        bottom: top + th - 8.0,
                                    };
                                    self.draw_text(target, "(missing)", fmt, &r, brush);
                                }
                            }
                        }
                        if page_count > 1 {
                            if let Some(brush) = &self.dim_brush {
                                let prev = D2D_RECT_F {
                                    left: TRAY_PREV_RECT.0,
                                    top: TRAY_PREV_RECT.1,
                                    right: TRAY_PREV_RECT.2,
                                    bottom: TRAY_PREV_RECT.3,
                                };
                                target.FillRectangle(&prev, brush);
                                let next = D2D_RECT_F {
                                    left: TRAY_NEXT_RECT.0,
                                    top: TRAY_NEXT_RECT.1,
                                    right: TRAY_NEXT_RECT.2,
                                    bottom: TRAY_NEXT_RECT.3,
                                };
                                target.FillRectangle(&next, brush);
                            }
                            if let (Some(fmt), Some(brush)) =
                                (&self.text_format_center, &self.white_brush)
                            {
                                let prev = D2D_RECT_F {
                                    left: TRAY_PREV_RECT.0,
                                    top: TRAY_PREV_RECT.1,
                                    right: TRAY_PREV_RECT.2,
                                    bottom: TRAY_PREV_RECT.3,
                                };
                                self.draw_text(target, "<", fmt, &prev, brush);
                                let next = D2D_RECT_F {
                                    left: TRAY_NEXT_RECT.0,
                                    top: TRAY_NEXT_RECT.1,
                                    right: TRAY_NEXT_RECT.2,
                                    bottom: TRAY_NEXT_RECT.3,
                                };
                                self.draw_text(target, ">", fmt, &next, brush);
                                let page_r = D2D_RECT_F {
                                    left: 200.0,
                                    top: 252.0,
                                    right: 276.0,
                                    bottom: 272.0,
                                };
                                let page_text =
                                    format!("{} / {}", page + 1, page_count);
                                self.draw_text(target, &page_text, fmt, &page_r, brush);
                            }
                        }
                    }
                    // Section dots (5, x 612-628, y 66/90/114/138/162):
                    // music, pomodoro, todo, water gray; tray white.
                    let dot_rects = [
                        TRAY_DOT_MUSIC_RECT,
                        TRAY_DOT_POMODORO_RECT,
                        TRAY_DOT_TODO_RECT,
                        TRAY_DOT_WATER_RECT,
                        TRAY_DOT_TRAY_RECT,
                    ];
                    for (i, dr) in dot_rects.iter().enumerate() {
                        let brush = if i == 4 {
                            &self.white_brush
                        } else {
                            &self.gray_brush
                        };
                        if let (Some(fmt), Some(brush)) =
                            (&self.text_format_center, brush)
                        {
                            // Dots follow current width (see music/pomodoro/
                            // todo/water dots above): right edge -28..-12.
                            let r = D2D_RECT_F {
                                left: lw - 28.0,
                                top: dr.1,
                                right: lw - 12.0,
                                bottom: dr.3,
                            };
                            self.draw_text(target, "•", fmt, &r, brush);
                        }
                    }
                }
            }
            let _ = target.EndDraw(None, None);
            let _ = ValidateRect(hwnd, None);
        }
    }

    /// Build a Nusa-shape path geometry (logical points): Aura notchPath
    /// (wide top merging into the top edge/bezel, tucked sides, boxy
    /// body, rounded bottom). Top edge full width, sides tuck inward
    /// 12pt below the top via tangent sweeps, bottom corners convex.
    /// Returns None if creation fails (caller falls back to a rect).
    fn nusa_geometry(&self, lw: f32, lh: f32, radius: f32) -> Option<ID2D1Geometry> {
        unsafe {
            let path: ID2D1PathGeometry = self.factory.CreatePathGeometry().ok()?;
            let sink: ID2D1GeometrySink = path.Open().ok()?;
            // Outline points (clockwise from top-left). Sweeps are
            // tangent arcs (12pt) from the full-width top to the tucked
            // sides, so the shape flows out of the bezel seamlessly.
            let r = radius.clamp(0.0, lh / 2.0);
            let t = 12.0f32.min(lw / 2.0).min(lh / 2.0);
            let mut pts: Vec<D2D_POINT_2F> = Vec::new();
            // Top edge (full width).
            pts.push(D2D_POINT_2F { x: 0.0, y: 0.0 });
            pts.push(D2D_POINT_2F { x: lw, y: 0.0 });
            // Top-right sweep (center (lw, t)): north -> west via northwest.
            let seg = 8i32;
            for i in 1..seg {
                let a = -std::f32::consts::FRAC_PI_2
                    * (1.0 + i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: lw + t * a.cos(),
                    y: t + t * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F { x: lw - t, y: t });
            pts.push(D2D_POINT_2F { x: lw - t, y: lh - r });
            // Bottom-right arc (center (lw-t-r, lh-r)): east -> south.
            for i in 1..seg {
                let a = std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: lw - t - r + r * a.cos(),
                    y: lh - r + r * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F { x: lw - t - r, y: lh });
            pts.push(D2D_POINT_2F { x: t + r, y: lh });
            // Bottom-left arc (center (t+r, lh-r)): south -> west.
            for i in 1..seg {
                let a = std::f32::consts::FRAC_PI_2
                    + std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: t + r + r * a.cos(),
                    y: lh - r + r * a.sin(),
                });
            }
            pts.push(D2D_POINT_2F { x: t, y: lh - r });
            pts.push(D2D_POINT_2F { x: t, y: t });
            // Top-left sweep (center (0, t)): east -> north via northeast.
            for i in 1..seg {
                let a = -std::f32::consts::FRAC_PI_2 * (i as f32 / seg as f32);
                pts.push(D2D_POINT_2F {
                    x: t * a.cos(),
                    y: t + t * a.sin(),
                });
            }
            // (EndFigure CLOSED closes back to (0, 0).)
            sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_FILLED);
            if pts.len() > 1 {
                sink.AddLines(&pts[1..]);
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_err() {
                return None;
            }
            path.cast::<ID2D1Geometry>().ok()
        }
    }

    /// Draw UTF-8 text with a DWrite format into a layout rect.
    fn draw_text(
        &self,
        target: &ID2D1RenderTarget,
        text: &str,
        format: &IDWriteTextFormat,
        rect: &D2D_RECT_F,
        brush: &ID2D1SolidColorBrush,
    ) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }
        unsafe {
            // windows-rs 0.58 DrawText takes &[u16] (length from slice), not
            // PCWSTR + length: 6 args (string, format, rect, brush, options, mode).
            target.DrawText(
                &wide,
                format,
                rect,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}
