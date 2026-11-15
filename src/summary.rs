//! 每顆磁碟的「空間組成」摘要：各檔案類型的大小。
//! 掃描完成時寫入 %LOCALAPPDATA%\DiskCleaner\drives.summary，儀表板開啟時直接讀取。

use std::collections::HashMap;

use crate::cache::{app_dir, now_unix};
use crate::tree::{KIND_COUNT, Tree};

#[derive(Clone, Debug)]
pub struct DriveSummary {
    pub letter: char,
    /// 掃描時間（Unix 秒）
    pub scanned_at: u64,
    /// 掃描到的檔案總大小
    pub indexed: u64,
    pub kinds: [u64; KIND_COUNT],
    pub counts: [u64; KIND_COUNT],
}

impl DriveSummary {
    pub fn from_tree(letter: char, tree: &Tree, node: u32) -> DriveSummary {
        let st = if node == 0 { tree.stats.clone() } else { tree.stats_of(node) };
        let mut kinds = [0u64; KIND_COUNT];
        let mut counts = [0u64; KIND_COUNT];
        for i in 0..KIND_COUNT {
            kinds[i] = st.kinds[i].0;
            counts[i] = st.kinds[i].1;
        }
        DriveSummary {
            letter: letter.to_ascii_uppercase(),
            scanned_at: now_unix(),
            indexed: tree.nodes[node as usize].size,
            kinds,
            counts,
        }
    }
}

fn file() -> std::path::PathBuf {
    app_dir().join("drives.summary")
}

pub fn load_all() -> HashMap<char, DriveSummary> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(file()) else { return out };
    for line in text.lines() {
        let p: Vec<&str> = line.split('\t').collect();
        if p.len() != 6 || p[0] != "S" {
            continue;
        }
        let Some(letter) = p[1].chars().next() else { continue };
        let parse = |s: &str| -> [u64; KIND_COUNT] {
            let mut a = [0u64; KIND_COUNT];
            for (i, v) in s.split(',').enumerate().take(KIND_COUNT) {
                a[i] = v.parse().unwrap_or(0);
            }
            a
        };
        out.insert(
            letter,
            DriveSummary {
                letter,
                scanned_at: p[2].parse().unwrap_or(0),
                indexed: p[3].parse().unwrap_or(0),
                kinds: parse(p[4]),
                counts: parse(p[5]),
            },
        );
    }
    out
}

pub fn save_all(all: &HashMap<char, DriveSummary>) {
    let mut s = String::new();
    let mut keys: Vec<&char> = all.keys().collect();
    keys.sort();
    for k in keys {
        let d = &all[k];
        let join = |a: &[u64; KIND_COUNT]| a.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
        s.push_str(&format!("S\t{}\t{}\t{}\t{}\t{}\n", d.letter, d.scanned_at, d.indexed, join(&d.kinds), join(&d.counts)));
    }
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(file(), s);
}
