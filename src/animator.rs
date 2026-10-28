//! Nusa-style substepped 2D spring physics animator (`nusa.exe` `0x14002C455..0x14002C907`
//! & `0x140032605..0x140032688`) + `aura-island` Tray damped-cosine bubble bounce spring
//! (`IslandView.swift` `traySpringScale`).

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpringPreset {
    Expand,
    Collapse,
    Morph,
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

    /// Set a new target `(to_w, to_h)` while preserving current position and smoothly
    /// turning around if reversing mid-flight. Returns `true` if animation needs to run.
    pub fn set_target(&mut self, to_w: f32, to_h: f32, preset: SpringPreset) -> bool {
        if self.active {
            // Advance physics to the exact moment of this target change:
            let _ = self.step();
        }
        let (stiffness, damping) = preset.params();
        self.stiffness = stiffness;
        self.damping = damping;
        self.target_w = to_w;
        self.target_h = to_h;

        // If reversing direction mid-flight, damp opposing velocity so rapid spam toggles
        // turn around immediately without fighting old momentum:
        if (to_w - self.pos_w) * self.vel_w < 0.0 {
            self.vel_w *= 0.25;
        }
        if (to_h - self.pos_h) * self.vel_h < 0.0 {
            self.vel_h *= 0.25;
        }

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

        self.last_tick = Instant::now();
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
