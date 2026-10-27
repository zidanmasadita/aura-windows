//! Pill/card state + logical sizes (Aura Mac points, scaled by DPI at runtime).
//! Logical sizes are in Aura points (Mac). Physical pixels = logical * dpi_scale.
//! Card sizes are exact Aura Mac values (verified from IslandView.expandedSize).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IslandState {
    Zen,      // idle, 140x32
    Compact,  // music pill, 180x32 (Phase 0 placeholder: clock + waveform)
    Expanded, // placeholder card, 520x180 (Phase 0; real music card in Phase 1)
}

impl IslandState {
    pub fn logical_size(self) -> (f32, f32) {
        match self {
            IslandState::Zen => (140.0, 32.0),
            IslandState::Compact => (180.0, 32.0),
            IslandState::Expanded => (520.0, 180.0),
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
