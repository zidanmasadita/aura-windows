//! Todo (Phase 2, part 2): tasks from %APPDATA%/Aura/todo.txt.
//! One task per line. "[x] " = done, "[ ] " or plain = todo.
//!
//! Tasks are typed directly in the pill (user request 2026-10-27:
//! no Notepad). The pill is normally NOACTIVATE, so while editing
//! app.rs temporarily clears WS_EX_NOACTIVATE, activates the window
//! and takes keyboard focus; leaving edit mode restores NOACTIVATE.
//! Clicking a row toggles done (and saves).

use std::fs;
use std::path::PathBuf;

pub struct Task {
    pub title: String,
    pub done: bool,
}

pub struct TodoList {
    pub tasks: Vec<Task>,
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

impl TodoList {
    pub fn path() -> Option<PathBuf> {
        aura_dir().map(|d| d.join("todo.txt"))
    }

    pub fn load() -> Self {
        let mut tasks = Vec::new();
        if let Some(path) = Self::path() {
            if let Ok(content) = fs::read_to_string(&path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let (done, title) = if let Some(rest) = line
                        .strip_prefix("[x] ")
                        .or_else(|| line.strip_prefix("[X] "))
                    {
                        (true, rest.trim())
                    } else if let Some(rest) = line.strip_prefix("[ ] ") {
                        (false, rest.trim())
                    } else {
                        (false, line)
                    };
                    if !title.is_empty() {
                        tasks.push(Task {
                            title: title.to_string(),
                            done,
                        });
                    }
                }
            }
        }
        Self { tasks }
    }

    pub fn save(&self) {
        // Best-effort (never panic): create dir + write file.
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let mut out = String::new();
            for task in &self.tasks {
                let mark = if task.done { "[x]" } else { "[ ]" };
                out.push_str(&format!("{} {}\n", mark, task.title));
            }
            let _ = fs::write(&path, out);
        }
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(task) = self.tasks.get_mut(index) {
            task.done = !task.done;
            self.save();
        }
    }

    /// Append a task (trimmed, ignored when empty) and save.
    pub fn add(&mut self, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        self.tasks.push(Task {
            title: title.to_string(),
            done: false,
        });
        self.save();
    }

}
