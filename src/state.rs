//! Pill/card state + logical sizes (Aura Mac points, scaled by DPI at runtime).
//! Logical sizes are in Aura points (Mac). Physical pixels = logical * dpi_scale.
//! Card sizes are exact Aura Mac values (`IslandView.expandedSize` / `closedSize`).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IslandState {
    Zen,      // idle, 140x32 (two cursor-tracking eyes)
    Compact,  // closed pill: 180x32 (music), 160x32 (pomodoro active), 360x32 (lyrics mode or alert)
    Expanded, // music card, 520x180
    Lyrics,   // lyrics panel, 700x180 (compressed music left + lyrics right)
    Pomodoro, // pomodoro card, 640x180
    Todo,     // todo card, 640x220
    Water,    // water card, 640x180
    Tray,     // tray shelf card, 640x280
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

    pub fn is_expanded(self) -> bool {
        !matches!(self, IslandState::Zen | IslandState::Compact)
    }

    /// Section index for the right-edge vertical page dots (0..5).
    /// Lyrics belongs to the Music section (0), matching `IslandView.sectionForDots`.
    pub fn section_index(self) -> usize {
        match self {
            IslandState::Expanded | IslandState::Lyrics | IslandState::Zen | IslandState::Compact => 0,
            IslandState::Pomodoro => 1,
            IslandState::Todo => 2,
            IslandState::Water => 3,
            IslandState::Tray => 4,
        }
    }

    pub fn from_section_index(idx: usize) -> Self {
        match idx % 5 {
            0 => IslandState::Expanded,
            1 => IslandState::Pomodoro,
            2 => IslandState::Todo,
            3 => IslandState::Water,
            _ => IslandState::Tray,
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
}
