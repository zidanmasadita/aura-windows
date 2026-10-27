//! Todo (Phase 2, part 2): tasks from %APPDATA%/Aura/todo.txt.
//! One task per line. "[x] " = done, "[ ] " or plain = todo.
//!
//! The pill is NOACTIVATE (never takes keyboard focus), so in-pill
//! typing is impossible without stealing focus. Adding tasks is via
//! Edit (opens todo.txt in Notepad); the app re-reads the file on
//! entering the Todo card. Clicking a row toggles done (and saves).

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

    /// Ensure todo.txt exists, then open it in Notepad so the user can
    /// add tasks. Best-effort: if Notepad fails to launch, the user can
    /// open %APPDATA%/Aura/todo.txt manually. (Notepad via std::process,
    /// not ShellExecute, to avoid Win32 signature uncertainty.)
    pub fn open_in_editor(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if !path.exists() {
            let _ = fs::write(&path, "");
        }
        let _ = std::process::Command::new("notepad.exe")
            .arg(&path)
            .spawn();
    }
}
