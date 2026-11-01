//! Nusa-style substepped 2D spring physics animator (`nusa.exe` `0x14002C455..0x14002C907`
//! & `0x140032605..0x140032688`) + `aura-island` Tray damped-cosine bubble bounce spring
//! (`IslandView.swift` `traySpringScale`).

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpringPreset {
    Expand,
    Collapse,
    Morph,
    Bulge,
}

impl SpringPreset {
    pub fn params(self) -> (f32, f32) {
        match self {
            // Slightly underdamped (zeta ~ 0.76) for organic Dynamic Island expansion:
            SpringPreset::Expand => (185.0, 20.8),
            // Exact Nusa collapse constants (`0x140092FF8 = 200.0, 0x140093080 = 29.0`, zeta ~ 1.02):
            SpringPreset::Collapse => (200.0, 29.0),
            // Critically damped high-stiffness morph (`zeta = 31 / (2 * sqrt(240)) = 1.00`)
            // so rapid section or Music <-> Lyrics spam toggles glide cleanly with zero overshoot:
            SpringPreset::Morph => (240.0, 31.0),
            // Tactile responsive spring for hover wait-bulge:
            SpringPreset::Bulge => (260.0, 24.0),
        }
    }
}

pub struct Spring2D {
    pub pos_w: f32,
    pub vel_w: f32,
    pub target_w: f32,
    pub pos_h: f32,
    pub vel_h: f32,
    pub target_h: f32,
    pub stiffness: f32,
    pub damping: f32,
    pub last_tick: Instant,
    pub active: bool,
}

impl Spring2D {
    pub fn new(w: f32, h: f32) -> Self {
        let (stiffness, damping) = SpringPreset::Morph.params();
        Self {
            pos_w: w,
            vel_w: 0.0,
            target_w: w,
            pos_h: h,
            vel_h: 0.0,
            target_h: h,
            stiffness,
            damping,
            last_tick: Instant::now(),
            active: false,
        }
    }

    /// Set a new target `(to_w, to_h)` while preserving current position AND velocity
    /// (`nusa.exe` `0x14002C853..0x14002C907`: a state change only rewrites the target,
    /// it never touches velocity nor steps the spring). Returns `true` if animation
    /// needs to run.
    ///
    /// Spam-click fix: the old code (1) stepped the spring here, moving `pos_w` without
    /// the app's `pill.width` following (one stale frame per click), and (2) multiplied
    /// opposing velocity by `0.25` on every reversal, so each rapid click froze the card
    /// in place for a frame -> visible stutter. Momentum is now continuous: a reversal
    /// decelerates naturally through the spring force instead of stopping dead.
    pub fn set_target(&mut self, to_w: f32, to_h: f32, preset: SpringPreset) -> bool {
        let (stiffness, damping) = preset.params();
        self.stiffness = stiffness;
        self.damping = damping;
        self.target_w = to_w;
        self.target_h = to_h;

        if (self.pos_w - to_w).abs() < 0.05
            && (self.pos_h - to_h).abs() < 0.05
            && self.vel_w.abs() < 0.5
            && self.vel_h.abs() < 0.5
        {
            self.pos_w = to_w;
            self.pos_h = to_h;
            self.vel_w = 0.0;
            self.vel_h = 0.0;
            self.active = false;
            return false;
        }

        // Keep the running clock when already animating (the next tick integrates the
        // real elapsed time); only a fresh start resets it.
        if !self.active {
            self.last_tick = Instant::now();
        }
        self.active = true;
        true
    }

    /// Step the spring physics forward to `now` using Nusa's `4ms` (`0.004s`) substep loop
    /// (`0x14002C498..0x14002C907`). Returns `(cur_w, cur_h, still_active)`.
    pub fn step(&mut self) -> (f32, f32, bool) {
        if !self.active {
            return (self.pos_w, self.pos_h, false);
        }
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.0005, 0.050);
        self.last_tick = now;

        // Nusa substep size: `0.004s` (`[0x140092FA0] = 0.004`):
        let steps = ((dt / 0.004).ceil() as usize).max(1);
        let sub_dt = dt / (steps as f32);

        for _ in 0..steps {
            let aw = -(self.pos_w - self.target_w) * self.stiffness - self.vel_w * self.damping;
            self.vel_w += aw * sub_dt;
            self.pos_w += self.vel_w * sub_dt;

            let ah = -(self.pos_h - self.target_h) * self.stiffness - self.vel_h * self.damping;
            self.vel_h += ah * sub_dt;
            self.pos_h += self.vel_h * sub_dt;
        }

        // Settling threshold (`0x14002F85E..0x14002F8EF`):
        let settled_w = (self.pos_w - self.target_w).abs() < 0.25 && self.vel_w.abs() < 1.0;
        let settled_h = (self.pos_h - self.target_h).abs() < 0.25 && self.vel_h.abs() < 1.0;
        if settled_w && settled_h {
            self.pos_w = self.target_w;
            self.vel_w = 0.0;
            self.pos_h = self.target_h;
            self.vel_h = 0.0;
            self.active = false;
        }

        (self.pos_w.max(32.0), self.pos_h.max(20.0), self.active)
    }
}

/// Damped-cosine bubble bounce spring for the Tray (`IslandView.swift` lines 2725-2778):
/// `scale(t) = to + (from - to) * e^(-3t) * cos(4 * pi * t)`.
/// - Hovering a file over Tray triggers `to = 1.03` (slight expand).
/// - Dropping a file into Tray triggers `to = 1.0, from = 1.12` (bubble pop + bounce).
/// - Drag leaving Tray triggers `to = 1.0`.
pub struct TrayBounceSpring {
    pub from_scale: f32,
    pub to_scale: f32,
    pub started_at: Option<Instant>,
}

impl TrayBounceSpring {
    pub fn new() -> Self {
        Self {
            from_scale: 1.0,
            to_scale: 1.0,
            started_at: None,
        }
    }

    pub fn scale(&self) -> f32 {
        let Some(start) = self.started_at else {
            return self.to_scale;
        };
        let t = start.elapsed().as_secs_f32();
        let decay = (-3.0 * t).exp();
        if decay * (self.from_scale - self.to_scale).abs() < 0.002 {
            return self.to_scale;
        }
        let osc = (4.0 * std::f32::consts::PI * t).cos();
        self.to_scale + (self.from_scale - self.to_scale) * decay * osc
    }

    pub fn is_active(&mut self) -> bool {
        let Some(start) = self.started_at else {
            return false;
        };
        let t = start.elapsed().as_secs_f32();
        let decay = (-3.0 * t).exp();
        if decay * (self.from_scale - self.to_scale).abs() < 0.002 {
            self.from_scale = self.to_scale;
            self.started_at = None;
            return false;
        }
        true
    }

    pub fn trigger(&mut self, to: f32, impulse: Option<f32>) {
        let cur = impulse.unwrap_or_else(|| self.scale());
        self.from_scale = cur;
        self.to_scale = to;
        self.started_at = Some(Instant::now());
    }

    pub fn cancel(&mut self) {
        self.from_scale = 1.0;
        self.to_scale = 1.0;
        self.started_at = None;
    }
}

/// Dual-edge fluid spring for the active section pill capsule indicator (Image 1):
/// The leading edge accelerates forward first, causing the capsule to stretch elastically,
/// and the trailing edge follows with slightly delayed damping, snapping smoothly into place.
pub struct FluidDotIndicator {
    pub left: f32,
    pub right: f32,
    pub vel_left: f32,
    pub vel_right: f32,
    pub target_cx: f32,
    pub last_tick: Instant,
    pub active: bool,
}

impl FluidDotIndicator {
    pub fn new(initial_slot: usize) -> Self {
        let cx = (initial_slot as f32) * 16.0;
        Self {
            left: cx - 7.0,
            right: cx + 7.0,
            vel_left: 0.0,
            vel_right: 0.0,
            target_cx: cx,
            last_tick: Instant::now(),
            active: false,
        }
    }

    pub fn set_target_slot(&mut self, slot: usize) {
        let target_cx = (slot as f32) * 16.0;
        if (self.target_cx - target_cx).abs() > 0.1 {
            self.target_cx = target_cx;
            self.last_tick = Instant::now();
            self.active = true;
        }
    }

    pub fn step(&mut self) -> (f32, f32, bool) {
        if !self.active {
            return (self.left, (self.right - self.left).max(14.0), false);
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().clamp(0.001, 0.050);
        self.last_tick = now;

        let target_left = self.target_cx - 7.0;
        let target_right = self.target_cx + 7.0;

        let moving_right = self.target_cx > (self.left + self.right) * 0.5;

        // Substep integration (0.004s):
        let steps = ((dt / 0.004).ceil() as usize).max(1);
        let sub_dt = dt / (steps as f32);

        // Leading edge has high stiffness for instant stretch; trailing edge lags behind:
        let (k_lead, d_lead) = (280.0f32, 23.0f32);
        let (k_trail, d_trail) = (180.0f32, 19.0f32);

        let (k_l, d_l, k_r, d_r) = if moving_right {
            (k_trail, d_trail, k_lead, d_lead)
        } else {
            (k_lead, d_lead, k_trail, d_trail)
        };

        for _ in 0..steps {
            let a_l = -(self.left - target_left) * k_l - self.vel_left * d_l;
            self.vel_left += a_l * sub_dt;
            self.left += self.vel_left * sub_dt;

            let a_r = -(self.right - target_right) * k_r - self.vel_right * d_r;
            self.vel_right += a_r * sub_dt;
            self.right += self.vel_right * sub_dt;
        }

        // Keep capsule width >= 14.0 (resting capsule width)
        if self.right - self.left < 14.0 {
            let mid = (self.left + self.right) * 0.5;
            self.left = mid - 7.0;
            self.right = mid + 7.0;
        }

        let settled = (self.left - target_left).abs() < 0.2
            && (self.right - target_right).abs() < 0.2
            && self.vel_left.abs() < 0.5
            && self.vel_right.abs() < 0.5;

        if settled {
            self.left = target_left;
            self.right = target_right;
            self.vel_left = 0.0;
            self.vel_right = 0.0;
            self.active = false;
        }

        (self.left, (self.right - self.left).max(14.0), self.active)
    }

    pub fn bounds(&self) -> (f32, f32) {
        (self.left, (self.right - self.left).max(14.0))
    }
}
