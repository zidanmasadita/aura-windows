//! Aura for Windows — entry point.
//! Phase 0: scaffold + pill (idle/compact/expanded placeholder).
//! Phase 1: GSMTC music card + lyrics panel.
//! Phase 2: Pomodoro + Todo + Water (dots navigation, 4 sections).
//! Windows subsystem (no console window when launched from Explorer).
#![windows_subsystem = "windows"]

mod animator;
mod app;
mod convert;
mod drag;
mod lan;
mod lyrics;
mod media;
mod pomodoro;
mod removebg;
mod renderer;
mod settings;
mod settings_window;
mod shelf;
mod state;
mod todo;
mod tray;
mod water;
mod window;

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("Aura panic: {:?}\n", info);
        let _ = std::fs::write("aura_crash.log", msg);
    }));
    if let Err(e) = app::run() {
        let _ = std::fs::write("aura_crash.log", format!("app::run error: {:?}", e));
    }
}
