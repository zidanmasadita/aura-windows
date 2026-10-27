//! Aura for Windows — entry point.
//! Phase 0: scaffold + pill (idle/compact/expanded placeholder).
//! Phase 1: GSMTC music card + lyrics panel.
//! Phase 2: Pomodoro + Todo + Water (dots navigation, 4 sections).
//! Windows subsystem (no console window when launched from Explorer).
#![windows_subsystem = "windows"]

mod animator;
mod app;
mod lyrics;
mod media;
mod pomodoro;
mod renderer;
mod state;
mod todo;
mod tray;
mod water;
mod window;

use windows::core::Result;

fn main() -> Result<()> {
    app::run()
}
