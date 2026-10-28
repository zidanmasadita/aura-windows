//! Water Reminder: daily intake + hourly bar chart + notification toggle + settings,
//! matching `aura-island` (`IslandView.swift` `drawWater` / `WaterStore`).
//! Persists to `%APPDATA%/Aura/water.json`.

use std::fs;
use std::path::PathBuf;
use chrono::Timelike;

pub struct Water {
    pub ml: i64,
    pub goal_ml: i64,
    pub glass_ml: i64,
    pub interval_minutes: i64,
    pub notifications_enabled: bool,
    pub hourly: [i64; 24],
}

fn today_string() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn aura_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|p| PathBuf::from(p).join("AppData").join("Roaming"))
        })?;
    Some(base.join("Aura"))
}

impl Water {
    pub fn path() -> Option<PathBuf> {
        aura_dir().map(|d| d.join("water.json"))
    }

    pub fn load() -> Self {
        let mut goal_ml = 1500;
        let mut glass_ml = 250;
        let mut interval_minutes = 60;
        let mut notifications_enabled = true;
        let mut hourly = [0i64; 24];
        let mut ml = 0i64;

        if let Some(path) = Self::path() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(g) = value.get("goal").and_then(|v| v.as_i64()) {
                        if g > 0 {
                            goal_ml = g;
                        }
                    }
                    if let Some(gl) = value.get("glass_ml").and_then(|v| v.as_i64()) {
                        if gl > 0 {
                            glass_ml = gl;
                        }
                    }
                    if let Some(iv) = value.get("interval_minutes").and_then(|v| v.as_i64()) {
                        if iv > 0 {
                            interval_minutes = iv;
                        }
                    }
                    if let Some(ne) = value.get("notifications_enabled").and_then(|v| v.as_bool()) {
                        notifications_enabled = ne;
                    }
                    let date = value.get("date").and_then(|v| v.as_str()).unwrap_or("");
                    if date == today_string() {
                        if let Some(arr) = value.get("hourly").and_then(|v| v.as_array()) {
                            for (i, item) in arr.iter().take(24).enumerate() {
                                hourly[i] = item.as_i64().unwrap_or(0).max(0);
                            }
                            ml = hourly.iter().sum();
                        } else if let Some(saved_ml) = value.get("ml").and_then(|v| v.as_i64()) {
                            ml = saved_ml.max(0);
                            let h = (chrono::Local::now().hour() as usize).min(23);
                            hourly[h] = ml;
                        }
                    }
                }
            }
        }
        Self {
            ml,
            goal_ml,
            glass_ml,
            interval_minutes,
            notifications_enabled,
            hourly,
        }
    }

    pub fn save(&self) {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let hourly_vec: Vec<i64> = self.hourly.to_vec();
            let value = serde_json::json!({
                "date": today_string(),
                "ml": self.ml,
                "goal": self.goal_ml,
                "glass_ml": self.glass_ml,
                "interval_minutes": self.interval_minutes,
                "notifications_enabled": self.notifications_enabled,
                "hourly": hourly_vec,
            });
            let _ = fs::write(&path, value.to_string());
        }
    }

    pub fn add_glass(&mut self) {
        let h = (chrono::Local::now().hour() as usize).min(23);
        self.hourly[h] += self.glass_ml;
        self.ml = self.hourly.iter().sum();
        self.save();
    }

    pub fn toggle_notifications(&mut self) {
        self.notifications_enabled = !self.notifications_enabled;
        self.save();
    }

    pub fn set_goal(&mut self, goal_ml: i64) {
        self.goal_ml = goal_ml.max(250);
        self.save();
    }

    pub fn set_glass(&mut self, glass_ml: i64) {
        self.glass_ml = glass_ml.max(50);
        self.save();
    }

    pub fn set_interval(&mut self, interval_minutes: i64) {
        self.interval_minutes = interval_minutes.max(5);
        self.save();
    }

    pub fn reset(&mut self) {
        self.ml = 0;
        self.hourly = [0; 24];
        self.save();
    }

    /// Format milliliters in liters matching `IslandView.formatLiters` (`"0"`, `"1.5"`, `"0.25"`).
    pub fn format_liters(ml: i64) -> String {
        if ml % 1000 == 0 {
            format!("{}", ml / 1000)
        } else if ml % 100 == 0 {
            format!("{:.1}", ml as f64 / 1000.0)
        } else {
            format!("{:.2}", ml as f64 / 1000.0)
        }
    }
}
