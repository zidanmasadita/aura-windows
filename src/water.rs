//! Water (Phase 2, part 2): daily intake, goal 1500 ml, glass 250 ml.
//! Persists to %APPDATA%/Aura/water.json ({date, ml, goal}).
//! New day (date != today) resets to 0 on load. Interval reminder
//! (every 60 min, PillAlert) is Phase 4, not here. No chart yet.

use std::fs;
use std::path::PathBuf;

pub struct Water {
    pub ml: i64,
    pub goal_ml: i64,
}

fn today_string() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn aura_dir() -> Option<PathBuf> {
    // %APPDATA%/Aura, fallback %USERPROFILE%/AppData/Roaming/Aura.
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
        let goal = 1500;
        if let Some(path) = Self::path() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                    let date = value.get("date").and_then(|v| v.as_str()).unwrap_or("");
                    if date == today_string() {
                        let ml = value
                            .get("ml")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0)
                            .max(0);
                        return Self { ml, goal_ml: goal };
                    }
                    // Different date: new day, reset (do not save yet;
                    // save happens on first +250/Reset).
                }
            }
        }
        Self { ml: 0, goal_ml: goal }
    }

    fn save(&self) {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let value = serde_json::json!({
                "date": today_string(),
                "ml": self.ml,
                "goal": self.goal_ml,
            });
            let _ = fs::write(&path, value.to_string());
        }
    }

    pub fn add_glass(&mut self) {
        self.ml += 250;
        self.save();
    }

    pub fn reset(&mut self) {
        self.ml = 0;
        self.save();
    }
}
