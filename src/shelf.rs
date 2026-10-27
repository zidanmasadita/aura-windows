//! Tray shelf (Phase 3a): file references dragged in from Explorer.
//! References only — Clear / Remove never delete the actual file.
//! Persisted as JSON %APPDATA%/Aura/tray.json: {"files": [paths]}.
//! Stale references (file moved/deleted) are kept and drawn dim, like
//! Aura Mac; they can be removed but not opened. Drag-out (copy to
//! Explorer) and LAN share arrive in Phase 3b; this increment is
//! drag-in + grid + open + remove + clear.

use std::fs;
use std::path::{Path, PathBuf};

pub struct Shelf {
    pub files: Vec<String>,
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

impl Shelf {
    pub fn path() -> Option<PathBuf> {
        aura_dir().map(|d| d.join("tray.json"))
    }

    pub fn load() -> Self {
        let mut files = Vec::new();
        if let Some(path) = Self::path() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(arr) = value.get("files").and_then(|v| v.as_array()) {
                        for item in arr {
                            if let Some(s) = item.as_str() {
                                if !s.is_empty() && !files.contains(&s.to_string()) {
                                    files.push(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        Self { files }
    }

    pub fn save(&self) {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let arr: Vec<serde_json::Value> = self
                .files
                .iter()
                .map(|f| serde_json::Value::String(f.clone()))
                .collect();
            let obj = serde_json::json!({ "files": arr });
            let _ = fs::write(&path, serde_json::to_string(&obj).unwrap_or_default());
        }
    }

    /// Add a dropped path (file or folder) if it exists and is not
    /// already referenced. Returns true when a new reference was added.
    pub fn add(&mut self, path: &str) -> bool {
        let trimmed = path.trim();
        if trimmed.is_empty() || !Path::new(trimmed).exists() {
            return false;
        }
        if self.files.iter().any(|f| f == trimmed) {
            return false;
        }
        self.files.push(trimmed.to_string());
        self.save();
        true
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.files.len() {
            self.files.remove(index);
            self.save();
        }
    }

    pub fn clear(&mut self) {
        if !self.files.is_empty() {
            self.files.clear();
            self.save();
        }
    }

    pub fn page_count(&self, page_size: usize) -> usize {
        if page_size == 0 || self.files.is_empty() {
            1
        } else {
            self.files.len().div_ceil(page_size)
        }
    }
}
