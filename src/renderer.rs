//! Direct2D renderer (Phase 0): HWND render target + DirectWrite text.
//! Draws the pill/card: black 0.98 fill + white 0.15 stroke (0.5px) +
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
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::ValidateRect;

use crate::state::IslandState;
use crate::window;

pub struct Renderer {
    factory: ID2D1Factory,
    target: Option<ID2D1RenderTarget>,
    dwrite: IDWriteFactory,
    text_format: Option<IDWriteTextFormat>,
    text_format_small: Option<IDWriteTextFormat>,
    text_format_center: Option<IDWriteTextFormat>,
    black_brush: Option<ID2D1SolidColorBrush>,
    white_brush: Option<ID2D1SolidColorBrush>,
    dim_brush: Option<ID2D1SolidColorBrush>,
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
                target: None,
                dwrite,
                text_format: None,
                text_format_small: None,
                text_format_center: None,
                black_brush: None,
                white_brush: None,
                dim_brush: None,
            })
        }
    }

    /// Resize the render target to a new PHYSICAL size (DIPs stay logical;
    /// D2D scales via the target DPI set at creation).
    pub fn resize(&mut self, width_px: i32, height_px: i32) {
        if let Some(target) = &self.target {
            unsafe {
                let _ = target.Resize(&D2D1_SIZE_U {
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
                pixelSize: D2D1_SIZE_U {
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
            // Brushes (None = default brush properties: opacity 1.0, identity).
            if let Ok(b) = target.CreateSolidColorBrush(
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
            if let Ok(b) = target.CreateSolidColorBrush(
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
            if let Ok(b) = target.CreateSolidColorBrush(
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
            self.target = Some(target);
            true
        }
    }

    /// Draw one frame for the given state. Coordinates are LOGICAL points
    /// (Aura Mac points); D2D scales to physical pixels via the target DPI.
    pub fn draw(&mut self, hwnd: HWND, state: IslandState, clock_text: &str) {
        let scale = window::dpi_scale(hwnd);
        let (lw, lh) = state.logical_size();
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
            // Pill/card background: rounded rect, black 0.98, white 0.15 stroke 0.5.
            let rect = D2D1_RECT_F {
                left: 0.0,
                top: 0.0,
                right: lw,
                bottom: lh,
            };
            let radius = if lh <= 33.0 { lh / 2.0 } else { 28.0 };
            let rounded = D2D1_ROUNDED_RECT {
                rect,
                radiusX: radius,
                radiusY: radius,
            };
            if let Some(brush) = &self.black_brush {
                target.FillRoundedRectangle(&rounded, brush);
            }
            if let Some(brush) = &self.dim_brush {
                target.DrawRoundedRectangle(&rounded, brush, 0.5, None);
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
                        let clock_rect = D2D1_RECT_F {
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
                            let bar = D2D1_RECT_F {
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
                    // Placeholder card: "Aura" header + Fase 0 note.
                    if let (Some(fmt), Some(brush)) = (&self.text_format, &self.white_brush)
                    {
                        let header = D2D1_RECT_F {
                            left: 24.0,
                            top: 20.0,
                            right: lw - 24.0,
                            bottom: 60.0,
                        };
                        self.draw_text(target, "Aura", fmt, &header, brush);
                    }
                    if let (Some(fmt), Some(brush)) =
                        (&self.text_format_small, &self.white_brush)
                    {
                        let sub = D2D1_RECT_F {
                            left: 24.0,
                            top: 60.0,
                            right: lw - 24.0,
                            bottom: 100.0,
                        };
                        self.draw_text(
                            target,
                            "Fase 0 — kartu musik menyusul di Fase 1",
                            fmt,
                            &sub,
                            brush,
                        );
                    }
                }
            }
            let _ = target.EndDraw(None, None);
            let _ = ValidateRect(hwnd, None);
        }
    }

    /// Draw UTF-8 text with a DWrite format into a layout rect.
    fn draw_text(
        &self,
        target: &ID2D1RenderTarget,
        text: &str,
        format: &IDWriteTextFormat,
        rect: &D2D1_RECT_F,
        brush: &ID2D1SolidColorBrush,
    ) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }
        unsafe {
            let text_ptr = windows::core::PCWSTR::from_raw(wide.as_ptr());
            target.DrawText(
                text_ptr,
                wide.len() as u32,
                format,
                rect,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}
