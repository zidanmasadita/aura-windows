//! Aura for Windows — entry point.
//! Phase 0: scaffold + pill (idle/compact/expanded placeholder).
//! Phase 1: GSMTC music card (lyrics follow in the next increment).
//! Windows subsystem (no console window when launched from Explorer).
#![windows_subsystem = "windows"]

mod animator;
mod app;
mod media;
mod renderer;
mod state;
mod tray;
mod window;

use windows::core::Result;

fn main() -> Result<()> {
    app::run()
}
