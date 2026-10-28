//! Tray shelf: file references dragged in from Explorer, matching `aura-island` (`IslandView.swift` `drawTray`).
//! References only — Clear / Remove never delete the actual file.
//! Persisted as JSON `%APPDATA%/Aura/tray.json`: `{"files": [paths]}`.

use std::fs;
use std::path::{Path, PathBuf};

pub struct Shelf {
    pub files: Vec<String>,
}

pub struct ShelfFileInfo {
    pub name: String,
    pub is_dir: bool,
    pub size_text: String,
    pub exists: bool,
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

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    }
}

pub fn file_info(path: &str) -> ShelfFileInfo {
    let p = Path::new(path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| path.rsplit(['\\', '/']).next().unwrap_or(path))
        .to_string();
    match fs::metadata(p) {
        Ok(meta) => {
            let is_dir = meta.is_dir();
            let size_text = if is_dir {
                "Folder".to_string()
            } else {
                format_bytes(meta.len())
            };
            ShelfFileInfo {
                name,
                is_dir,
                size_text,
                exists: true,
            }
        }
        Err(_) => ShelfFileInfo {
            name: format!("{} (missing)", name),
            is_dir: false,
            size_text: "0 B".to_string(),
            exists: false,
        },
    }
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
