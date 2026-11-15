//! 清理頁的結果快取：啟動時先顯示上次的規則大小與專案產物，背景再更新。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::cache::{app_dir, now_unix};
use crate::cleaner::{Found, Measured, Mode};
use crate::models::{Model, ModelDel};
use crate::projects::{ALL_KINDS, Artifact};

fn rules_file() -> PathBuf {
    app_dir().join("rules.cache")
}
fn projects_file() -> PathBuf {
    app_dir().join("projects.cache")
}

fn mode_char(m: Mode) -> char {
    match m {
        Mode::Contents => 'C',
        Mode::Dir => 'D',
        Mode::File => 'F',
        Mode::Info => 'I',
    }
}

fn char_mode(c: &str) -> Option<Mode> {
    Some(match c {
        "C" => Mode::Contents,
        "D" => Mode::Dir,
        "F" => Mode::File,
        "I" => Mode::Info,
        _ => return None,
    })
}

pub fn save_rules(results: &[(&str, &Measured)]) {
    let mut s = format!("v1\t{}\n", now_unix());
    for (id, m) in results {
        s.push_str(&format!("R\t{id}\t{}\t{}\t{}\n", m.bytes, m.files, m.errors));
        for f in &m.found {
            s.push_str(&format!(
                "F\t{}\t{}\t{}\t{}\t{}\n",
                mode_char(f.mode),
                f.bytes,
                f.files,
                f.errors,
                f.path.display()
            ));
        }
    }
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(rules_file(), s);
}

/// 回傳（規則 id → 結果, 快取年齡秒數）
pub fn load_rules() -> Option<(HashMap<String, Measured>, u64)> {
    let text = std::fs::read_to_string(rules_file()).ok()?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next()?.split('\t').collect();
    if head.first() != Some(&"v1") {
        return None;
    }
    let age = now_unix().saturating_sub(head.get(1)?.parse().ok()?);
    let mut map: HashMap<String, Measured> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in lines {
        let p: Vec<&str> = line.splitn(6, '\t').collect();
        match p.first() {
            Some(&"R") if p.len() >= 5 => {
                let m = Measured {
                    found: Vec::new(),
                    bytes: p[2].parse().unwrap_or(0),
                    files: p[3].parse().unwrap_or(0),
                    errors: p[4].parse().unwrap_or(0),
                };
                map.insert(p[1].to_string(), m);
                cur = Some(p[1].to_string());
            }
            Some(&"F") if p.len() == 6 => {
                if let (Some(id), Some(mode)) = (&cur, char_mode(p[1]))
                    && let Some(m) = map.get_mut(id)
                {
                    m.found.push(Found {
                        path: PathBuf::from(p[5]),
                        mode,
                        bytes: p[2].parse().unwrap_or(0),
                        files: p[3].parse().unwrap_or(0),
                        errors: p[4].parse().unwrap_or(0),
                    });
                }
            }
            _ => {}
        }
    }
    Some((map, age))
}

pub fn save_projects(items: &[&Artifact], models: &[&Model]) {
    let mut s = format!("v1\t{}\n", now_unix());
    for a in items {
        s.push_str(&format!("A\t{}\t{}\t{}\t{}\t{}\n", a.kind.id, a.bytes, a.files, a.newest, a.path.display()));
    }
    for m in models {
        let del = match m.del {
            ModelDel::Dir => "D",
            ModelDel::File => "F",
            ModelDel::Ollama => "O",
        };
        s.push_str(&format!("M\t{}\t{}\t{}\t{}\t{}\t{}\n", m.source, del, m.bytes, m.newest, m.name, m.path.display()));
    }
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(projects_file(), s);
}

/// 讀回上次的專案產物與模型（已不存在的會濾掉）。
pub fn load_projects() -> Option<(Vec<Artifact>, Vec<Model>, u64)> {
    let text = std::fs::read_to_string(projects_file()).ok()?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next()?.split('\t').collect();
    if head.first() != Some(&"v1") {
        return None;
    }
    let age = now_unix().saturating_sub(head.get(1)?.parse().ok()?);
    let mut out = Vec::new();
    let mut models = Vec::new();
    for line in lines {
        if let Some(rest) = line.strip_prefix("M\t") {
            let p: Vec<&str> = rest.splitn(6, '\t').collect();
            if p.len() == 6 {
                let path = PathBuf::from(p[5]);
                if path.exists() {
                    models.push(Model {
                        source: source_label(p[0]),
                        del: match p[1] {
                            "D" => ModelDel::Dir,
                            "O" => ModelDel::Ollama,
                            _ => ModelDel::File,
                        },
                        bytes: p[2].parse().unwrap_or(0),
                        newest: p[3].parse().unwrap_or(0),
                        name: p[4].to_string(),
                        path,
                    });
                }
            }
            continue;
        }
        let p: Vec<&str> = line.splitn(6, '\t').collect();
        if p.len() != 6 || p[0] != "A" {
            continue;
        }
        let Some(kind) = ALL_KINDS.iter().find(|k| k.id == p[1]) else { continue };
        let path = PathBuf::from(p[5]);
        if !path.is_dir() {
            continue;
        }
        out.push(Artifact {
            path,
            kind,
            bytes: p[2].parse().unwrap_or(0),
            files: p[3].parse().unwrap_or(0),
            newest: p[4].parse().unwrap_or(0),
        });
    }
    Some((out, models, age))
}

/// 把快取裡的來源字串對應回靜態字串
fn source_label(s: &str) -> &'static str {
    const ALL: [&str; 13] = [
        "Hugging Face", "Hugging Face 資料集", "Hugging Face Space", "Ollama", "LM Studio", "ComfyUI", "SD WebUI",
        "PyTorch", "Whisper", "Jan", "GPT4All", "InsightFace", "ModelScope",
    ];
    ALL.iter().find(|x| **x == s).copied().unwrap_or("模型檔")
}
