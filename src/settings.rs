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

fn default_convert_format() -> usize {
    1 // WebP
}

fn default_one_f32() -> f32 {
    1.0
}

fn default_dodge_radius() -> f32 {
    140.0
}

fn default_dodge_delay() -> u64 {
    350
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
    #[serde(default = "default_one_f32")]
    pub notch_scale: f32, // pill size multiplier, 0.8..=1.5 (slider, default 1.0)
    #[serde(default)]
    pub minimal_notch: bool, // peek mode: hide the pill, leaving only its tip visible when idle
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
    #[serde(default = "default_one_f32")]
    pub pill_opacity: f32, // 0.4..=1.0 (slider, default 1.0)
    #[serde(default)]
    pub pill_show_all_monitors: bool, // show the pill on every monitor
    #[serde(default)]
    pub hide_when_covered: bool, // auto-hide when a window covers the pill
    #[serde(default)]
    pub dodge_enabled: bool, // hide when the cursor comes close
    #[serde(default = "default_dodge_radius")]
    pub dodge_radius: f32, // px around the pill that triggers dodge
    #[serde(default = "default_dodge_delay")]
    pub dodge_delay_ms: u64, // how long the cursor must stay inside the radius
    #[serde(default)]
    pub removebg_model_idx: usize, // 0=Fast, 1=Balanced, 2=Quality
    #[serde(default = "default_convert_format")]
    pub convert_format_idx: usize, // 0=AVIF, 1=WebP, 2=PNG
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
            notch_scale: 1.0,
            minimal_notch: false,
            hide_in_fullscreen: true,
            lyrics_sync: true,
            master_volume_control: true,
            sound_on_complete: true,
            pill_monitor: 0,
            pill_opacity: 1.0,
            pill_show_all_monitors: false,
            hide_when_covered: false,
            dodge_enabled: false,
            dodge_radius: 140.0,
            dodge_delay_ms: 350,
            removebg_model_idx: 0,
            convert_format_idx: 1,
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
