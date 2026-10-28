//! Size animator, Nusa-style staged morph (RE 2026-10-28).
//!
//! What the RE proved (static-findings.md): Nusa animates
//! timer-driven, NOT with DComp keyframes — its easing timer is
//! RE-ARMED at 250ms/160ms intervals, i.e. the morph proceeds in a
//! few discrete stages, and each step resizes via SetWindowPos +
//! SetWindowRgn (region-based shape). Aura already matched that
//! mechanism (16ms timer + SetWindowPos + SetWindowRgn per step,
//! window.rs); this adopts the staged part too: the easeOutCubic
//! curve is sampled at stage boundaries instead of gliding
//! continuously, so the pill pops through stages Nusa-style.
//!
//! Honest limit: Nusa's exact stage count and total duration were
//! NOT recoverable from the stripped binary (only the re-arm
//! intervals were). 4 stages x 160ms = 640ms total is a chosen
//! approximation. If the staged feel is too choppy, revert tick()
//! to continuous sampling (see git history for the smooth version).

use std::time::{Duration, Instant};

/// Total morph time: stages x interval.
pub const ANIM_DURATION: Duration = Duration::from_millis(640);
/// Number of discrete morph stages (Nusa: a handful, exact count unknown).
pub const ANIM_STAGES: u32 = 4;
/// Hold time per stage (Nusa's easing timer re-arm interval).
pub const ANIM_STAGE_INTERVAL: Duration = Duration::from_millis(160);

/// easeOutCubic: 1 - (1-t)^3. Fast start, gentle landing (Aura Mac's curve).
pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

pub struct SizeAnimation {
    pub from_w: f32,
    pub from_h: f32,
    pub to_w: f32,
    pub to_h: f32,
    pub start: Instant,
}

impl SizeAnimation {
    pub fn new(from_w: f32, from_h: f32, to_w: f32, to_h: f32) -> Self {
        Self {
            from_w,
            from_h,
            to_w,
            to_h,
            start: Instant::now(),
        }
    }

    /// Current staged size. Returns None when finished (caller stops the timer).
    /// The caller samples this every 16ms (TIMER_ANIM); between stage
    /// boundaries the same size is returned, so the pill holds each
    /// stage, then jumps to the next — Nusa's re-armed-timer pattern.
    pub fn tick(&self) -> Option<(f32, f32)> {
        let elapsed = self.start.elapsed();
        if elapsed >= ANIM_DURATION {
            return None;
        }
        let stage = (elapsed.as_millis() / ANIM_STAGE_INTERVAL.as_millis()) as u32;
        let t = ((stage + 1) as f32 / ANIM_STAGES as f32).clamp(0.0, 1.0);
        let e = ease_out_cubic(t);
        Some((
            self.from_w + (self.to_w - self.from_w) * e,
            self.from_h + (self.to_h - self.from_h) * e,
        ))
    }
}
