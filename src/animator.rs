//! Size animator: easeOutCubic over 0.35s, sampled continuously on
//! the 16ms TIMER_ANIM driver (app.rs on_anim_tick).
//!
//! History (honest): a Nusa-style staged morph (4 stages x 160ms,
//! RE 2026-10-28) was tried in commit 3cec8d4 and REVERTED the same
//! night: the user reported it as laggy/choppy ("lag patah patah").
//! The staged holds made the pill visibly freeze-then-jump and the
//! 640ms total felt slow. Smooth 350ms sampling is the version that
//! feels right; do not reintroduce staging without the user's word.
//! The Nusa mechanism that DOES stay: timer-driven animation with
//! SetWindowPos + SetWindowRgn per step (window.rs), not DComp
//! keyframes — that part already matched Nusa before staging.

use std::time::{Duration, Instant};

pub const ANIM_DURATION: Duration = Duration::from_millis(350);

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

    /// Current interpolated size. Returns None when finished (caller stops the timer).
    pub fn tick(&self) -> Option<(f32, f32)> {
        let elapsed = self.start.elapsed();
        if elapsed >= ANIM_DURATION {
            return None;
        }
        let t = elapsed.as_secs_f32() / ANIM_DURATION.as_secs_f32();
        let e = ease_out_cubic(t.clamp(0.0, 1.0));
        Some((
            self.from_w + (self.to_w - self.from_w) * e,
            self.from_h + (self.to_h - self.from_h) * e,
        ))
    }
}
