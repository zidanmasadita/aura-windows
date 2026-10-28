//! Todo: tasks persisted in `%APPDATA%/Aura/todo.txt`.
//! One task per line. "[x] " = done, "[ ] " or plain = todo.
//!
//! Layout and behavior match `aura-island` (`IslandView.swift` `drawTodo`):
//! - Left column shows `{done}/{total}` + `"done"` (or `"No tasks yet"`) and `"+ Add task"` pill.
//! - Right column shows up to 4 tasks (incomplete-first), each as a rounded pill
//!   with circular checkmark toggle on the left and `"×"` delete button on the right.

use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Task {
    pub title: String,
    pub done: bool,
}

pub struct TodoList {
    pub tasks: Vec<Task>,
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

    pub fn remove(&mut self, index: usize) {
        if index < self.tasks.len() {
            self.tasks.remove(index);
            self.save();
        }
    }

    /// Indices of tasks in Aura Mac display order (incomplete-first, up to 4 rows).
    pub fn visible_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..self.tasks.len()).collect();
        indices.sort_by_key(|&i| self.tasks[i].done);
        indices.truncate(4);
        indices
    }

    pub fn done_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.done).count()
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
