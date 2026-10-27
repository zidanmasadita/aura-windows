//! Pill/card state + logical sizes (Aura Mac points, scaled by DPI at runtime).
//! Logical sizes are in Aura points (Mac). Physical pixels = logical * dpi_scale.
//! Card sizes are exact Aura Mac values (verified from IslandView.expandedSize).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IslandState {
    Zen,      // idle, 140x32
    Compact,  // music pill, 180x32 (Phase 0 placeholder: clock + waveform)
    Expanded, // music card, 520x180
    Lyrics,   // lyrics panel, 700x180 (same height as music, wider)
    Pomodoro, // pomodoro card, 640x180
    Todo, // todo card, 640x220
    Water, // water card, 640x180
    Tray, // tray shelf card, 640x280 (Phase 3a)
}

impl IslandState {
    pub fn logical_size(self) -> (f32, f32) {
        match self {
            IslandState::Zen => (140.0, 32.0),
            IslandState::Compact => (180.0, 32.0),
            IslandState::Expanded => (520.0, 180.0),
            IslandState::Lyrics => (700.0, 180.0),
            IslandState::Pomodoro => (640.0, 180.0),
            IslandState::Todo => (640.0, 220.0),
            IslandState::Water => (640.0, 180.0),
            IslandState::Tray => (640.0, 280.0),
        }
    }
}

/// Pill state owned by App: current state + current (animated) size in logical points.
pub struct PillState {
    pub state: IslandState,
    /// Current size in logical points (animator interpolates this).
    pub width: f32,
    pub height: f32,
}

impl PillState {
    pub fn new() -> Self {
        let (w, h) = IslandState::Zen.logical_size();
        Self {
            state: IslandState::Zen,
            width: w,
            height: h,
        }
    }

    pub fn target_size(&self) -> (f32, f32) {
        self.state.logical_size()
    }
}
