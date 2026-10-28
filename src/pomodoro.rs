//! Pomodoro (Phase 2, part 1): 25:00 focus / 5:00 break, wall-clock,
//! in-memory only (no persistence across restart, per plan — unlike
//! todo/water, which persist in Phase 2 part 2).
//!
//! Wall-clock (not tick count): start records ends_at = now + remaining,
//! and remaining() is ends_at - now, so a delayed tick or a slept PC
//! does not drift the timer. Pause freezes remaining; resume recomputes
//! ends_at from the frozen remaining. Finishing stops at 00:00 (no
//! auto-switch to the other mode — user picks via mode pills).
//! Ring (progress circle) is a follow-up, not in this increment.

use chrono::{DateTime, Duration, Local};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PomodoroMode {
    Focus,
    Break,
}

impl PomodoroMode {
    pub fn duration_secs(self) -> i64 {
        match self {
            PomodoroMode::Focus => 25 * 60,
            PomodoroMode::Break => 5 * 60,
        }
    }
}

pub struct Pomodoro {
    pub mode: PomodoroMode,
    pub running: bool,
    ends_at: Option<DateTime<Local>>,
    paused_remaining_secs: i64,
}

impl Pomodoro {
    pub fn new() -> Self {
        Self {
            mode: PomodoroMode::Focus,
            running: false,
            ends_at: None,
            paused_remaining_secs: PomodoroMode::Focus.duration_secs(),
        }
    }

    /// Select a mode (resets to that mode's full duration, stopped).
    pub fn select_mode(&mut self, mode: PomodoroMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.running = false;
        self.ends_at = None;
        self.paused_remaining_secs = mode.duration_secs();
    }

    /// Reset current mode timer to its full duration, stopped.
    pub fn reset(&mut self) {
        self.running = false;
        self.ends_at = None;
        self.paused_remaining_secs = self.mode.duration_secs();
    }

    /// True when a countdown is live (running, or paused mid-countdown),
    /// matching `IslandView.isPomodoroActive` on Aura Mac.
    pub fn is_active(&self, now: DateTime<Local>) -> bool {
        let rem = self.remaining(now);
        let dur = self.mode.duration_secs();
        self.running || (rem > 0 && rem < dur)
    }

    pub fn duration_secs(&self) -> i64 {
        self.mode.duration_secs()
    }

    /// Start (or resume) / pause toggle.
    pub fn toggle_start_pause(&mut self, now: DateTime<Local>) {
        if self.running {
            // Pause: freeze remaining.
            self.paused_remaining_secs = self.remaining(now);
            self.running = false;
            self.ends_at = None;
        } else {
            // Start/resume: if finished (0), reset to full duration first.
            if self.paused_remaining_secs <= 0 {
                self.paused_remaining_secs = self.mode.duration_secs();
            }
            self.ends_at = Some(now + Duration::seconds(self.paused_remaining_secs));
            self.running = true;
        }
    }

    /// Remaining seconds (wall-clock when running, frozen when paused).
    pub fn remaining(&self, now: DateTime<Local>) -> i64 {
        if self.running {
            if let Some(end) = self.ends_at {
                (end - now).num_seconds().max(0)
            } else {
                self.paused_remaining_secs
            }
        } else {
            self.paused_remaining_secs
        }
    }

    /// If running and time is up, stop (shows 00:00, button back to Start)
    /// and return true so the caller can play Nusa's procedural WAV chime.
    pub fn check_finished(&mut self, now: DateTime<Local>) -> bool {
        if self.running && self.remaining(now) <= 0 {
            self.running = false;
            self.ends_at = None;
            self.paused_remaining_secs = 0;
            return true;
        }
        false
    }

    #[allow(dead_code)]
    pub fn button_label(&self) -> &'static str {
        if self.running {
            "Pause"
        } else {
            "Start"
        }
    }
}
