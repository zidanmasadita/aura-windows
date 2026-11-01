//! Pill/card state + logical sizes (Aura Mac points, scaled by DPI at runtime).
//! Logical sizes are in Aura points (Mac). Physical pixels = logical * dpi_scale.
//! Card sizes are exact Aura Mac values (`IslandView.expandedSize` / `closedSize`).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IslandState {
    Zen,      // idle, 140x32 (two cursor-tracking eyes)
    Compact,  // closed pill: 180x32 (music), 160x32 (pomodoro active), 360x32 (lyrics mode or alert)
    Expanded, // music card, 540x184
    Lyrics,   // lyrics panel, 700x184 (compressed music left + lyrics right)
    Pomodoro, // pomodoro card, 540x184
    Todo,     // todo card, 540x184
    Water,    // water card, 540x184
    Tray,     // tray shelf card, 540x184
    RemoveBg, // remove-bg card, 540x184
    Convert,  // convert card, 540x184 (Photo, Merge PDF, MD)
    Settings, // aura island settings customization, 540x184
}

pub const SECTION_COUNT: usize = 7;

impl IslandState {
    pub fn logical_size(self) -> (f32, f32) {
        match self {
            IslandState::Zen => (104.0, 28.0),
            IslandState::Compact => (180.0, 30.0),
            IslandState::Expanded => (540.0, 184.0),
            IslandState::Lyrics => (700.0, 184.0),
            IslandState::Pomodoro => (540.0, 184.0),
            IslandState::Todo => (540.0, 184.0),
            IslandState::Water => (540.0, 184.0),
            IslandState::Tray => (540.0, 184.0),
            IslandState::RemoveBg => (540.0, 184.0),
            IslandState::Convert => (540.0, 184.0),
            IslandState::Settings => (540.0, 184.0),
        }
    }

    pub fn is_expanded(self) -> bool {
        !matches!(self, IslandState::Zen | IslandState::Compact)
    }

    pub fn has_top_header(self) -> bool {
        matches!(
            self,
            IslandState::Expanded
                | IslandState::Lyrics
                | IslandState::Pomodoro
                | IslandState::Todo
                | IslandState::Water
        )
    }

    /// Section index for the right-edge vertical page dots (0..6).
    /// Lyrics belongs to the Music section (0), matching `IslandView.sectionForDots`.
    pub fn section_index(self) -> usize {
        match self {
            IslandState::Expanded | IslandState::Lyrics | IslandState::Zen | IslandState::Compact => 0,
            IslandState::Pomodoro => 1,
            IslandState::Todo => 2,
            IslandState::Water => 3,
            IslandState::Tray => 4,
            IslandState::RemoveBg => 5,
            IslandState::Convert => 6,
            IslandState::Settings => 0,
        }
    }

    pub fn from_section_index(idx: usize) -> Self {
        match idx % SECTION_COUNT {
            0 => IslandState::Expanded,
            1 => IslandState::Pomodoro,
            2 => IslandState::Todo,
            3 => IslandState::Water,
            4 => IslandState::Tray,
            5 => IslandState::RemoveBg,
            _ => IslandState::Convert,
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
