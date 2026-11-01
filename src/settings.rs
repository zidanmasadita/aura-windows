//! Aura Settings — configuration and full personalization.
//! Persisted to `%APPDATA%/Aura/settings.json`.

use std::fs;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_water_goal() -> u32 {
    2000
}

fn default_pomodoro_mins() -> u32 {
    25
}

fn default_size_idx() -> usize {
    1 // 100%
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuraSettings {
    #[serde(default = "default_true")]
    pub music_enabled: bool,
    #[serde(default = "default_true")]
    pub pomodoro_enabled: bool,
    #[serde(default = "default_true")]
    pub todo_enabled: bool,
    #[serde(default = "default_true")]
    pub water_enabled: bool,
    #[serde(default = "default_true")]
    pub tray_enabled: bool,
    #[serde(default = "default_true")]
    pub removebg_enabled: bool,
    #[serde(default = "default_true")]
    pub convert_enabled: bool,
    #[serde(default)]
    pub start_with_windows: bool,
    #[serde(default = "default_true")]
    pub sound_chimes: bool,
    #[serde(default = "default_true")]
    pub high_refresh_120fps: bool,
    #[serde(default = "default_true")]
    pub eye_expressions: bool,
    #[serde(default = "default_water_goal")]
    pub water_daily_goal: u32,
    #[serde(default = "default_pomodoro_mins")]
    pub pomodoro_focus_mins: u32,

    // Standalone Settings Page additions:
    #[serde(default = "default_size_idx")]
    pub notch_size_idx: usize, // 0=Auto, 1=100%, 2=115%, 3=130%, 4=150%
    #[serde(default)]
    pub minimal_notch: bool, // A smaller pill when nothing is playing
    #[serde(default = "default_true")]
    pub hide_in_fullscreen: bool, // Games, videos and slides that fill the screen
    #[serde(default = "default_true")]
    pub lyrics_sync: bool,
    #[serde(default = "default_true")]
    pub master_volume_control: bool,
    #[serde(default = "default_true")]
    pub sound_on_complete: bool,
    #[serde(default)]
    pub pill_monitor: i32, // 0-based index into window::list_monitors() (primary first)
    #[serde(default)]
    pub pill_opacity_idx: usize, // 0=100%, 1=85%, 2=70%, 3=55%
    #[serde(default = "default_true")]
    pub pill_alerts_enabled: bool,
    #[serde(default = "default_true")]
    pub alert_battery: bool,
    #[serde(default = "default_true")]
    pub alert_capslock: bool,
}

impl Default for AuraSettings {
    fn default() -> Self {
        Self {
            music_enabled: true,
            pomodoro_enabled: true,
            todo_enabled: true,
            water_enabled: true,
            tray_enabled: true,
            removebg_enabled: true,
            convert_enabled: true,
            start_with_windows: false,
            sound_chimes: true,
            high_refresh_120fps: true,
            eye_expressions: true,
            water_daily_goal: 2000,
            pomodoro_focus_mins: 25,
            notch_size_idx: 1, // 100%
            minimal_notch: false,
            hide_in_fullscreen: true,
            lyrics_sync: true,
            master_volume_control: true,
            sound_on_complete: true,
            pill_monitor: 0,
            pill_opacity_idx: 0,
            pill_alerts_enabled: true,
            alert_battery: true,
            alert_capslock: true,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|p| PathBuf::from(p).join("AppData").join("Roaming"))
        })?;
    Some(base.join("Aura").join("settings.json"))
}

impl AuraSettings {
    pub fn load() -> Self {
        if let Some(path) = settings_path() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(cfg) = serde_json::from_str::<AuraSettings>(&content) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        if let Some(path) = settings_path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = fs::write(&path, json);
            }
        }
    }
}
