//! 簡單的設定檔（%LOCALAPPDATA%\DiskCleaner\settings.txt）。

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    pub roots: Vec<PathBuf>,
    pub show_empty: bool,
}

fn file() -> PathBuf {
    crate::cache::app_dir().join("settings.txt")
}

impl Settings {
    pub fn load() -> Settings {
        let mut s = Settings { roots: Vec::new(), show_empty: false };
        let mut has_roots = false;
        if let Ok(text) = std::fs::read_to_string(file()) {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("root=") {
                    s.roots.push(PathBuf::from(v));
                    has_roots = true;
                } else if line == "roots=none" {
                    has_roots = true;
                } else if let Some(v) = line.strip_prefix("show_empty=") {
                    s.show_empty = v == "1";
                }
            }
        }
        if !has_roots {
            s.roots = crate::projects::default_roots();
        }
        s
    }

    pub fn save(&self) {
        let mut out = String::new();
        if self.roots.is_empty() {
            out.push_str("roots=none\n");
        }
        for r in &self.roots {
            out.push_str(&format!("root={}\n", r.display()));
        }
        out.push_str(&format!("show_empty={}\n", if self.show_empty { 1 } else { 0 }));
        let _ = std::fs::create_dir_all(crate::cache::app_dir());
        let _ = std::fs::write(file(), out);
    }
}
