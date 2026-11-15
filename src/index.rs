//! 掃描調度：依權限與檔案系統選最快的方式，並利用快取。
//!
//! 1. 系統管理員 + NTFS：讀快取 → 用 USN 日誌撈出變動的紀錄增量更新（通常 < 1 秒）；
//!    沒有快取或日誌已截斷時，直接讀整個 $MFT（數秒）。
//! 2. 其他情況：平行目錄列舉；有快取時先秒開舊結果，再由使用者決定是否重掃。

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::cache::{self, METHOD_MFT, METHOD_WALK, Meta};
use crate::mft::{self, Changes, ROOT_RECORD};
use crate::scan::{self, Progress};
use crate::tree::{Tree, finalize};
use crate::win;

#[derive(Clone, Debug)]
pub enum Origin {
    FullMft,
    Incremental { changes: usize },
    FullWalk,
    CachedWalk { age_secs: u64 },
}

impl Origin {
    pub fn describe(&self) -> String {
        match self {
            Origin::FullMft => "完整讀取 MFT".into(),
            Origin::Incremental { changes } => format!("快取 + USN 增量更新（{changes} 筆變動）"),
            Origin::FullWalk => "目錄列舉".into(),
            Origin::CachedWalk { age_secs } => format!("快取（{} 前的結果）", fmt_age(*age_secs)),
        }
    }
    pub fn is_stale(&self) -> bool {
        matches!(self, Origin::CachedWalk { .. })
    }
}

pub fn fmt_age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} 秒"),
        60..=3599 => format!("{} 分鐘", secs / 60),
        3600..=86399 => format!("{} 小時", secs / 3600),
        _ => format!("{} 天", secs / 86400),
    }
}

pub struct ScanOutput {
    pub tree: Tree,
    /// 背景寫快取的執行緒（需要時可 join）
    pub saver: Option<std::thread::JoinHandle<()>>,
    pub origin: Origin,
    /// 使用者要求的路徑在樹中的節點
    pub view: u32,
    pub target: PathBuf,
    pub secs: f32,
}

fn normalize(target: &Path) -> PathBuf {
    let s = target.to_string_lossy().replace('/', "\\");
    let mut s = s.trim_end_matches('\\').to_string();
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        let first = s[..1].to_ascii_uppercase();
        s.replace_range(..1, &first);
    }
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    PathBuf::from(s)
}

fn drive_letter(p: &Path) -> Option<char> {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        Some((b[0] as char).to_ascii_uppercase())
    } else {
        None
    }
}

/// 這個路徑能不能用 MFT 極速掃描。
pub fn mft_capable(target: &Path) -> bool {
    let Some(letter) = drive_letter(target) else { return false };
    if !win::is_elevated() {
        return false;
    }
    win::drive_info(&format!("{letter}:\\"), "")
        .map(|d| d.fs.eq_ignore_ascii_case("NTFS"))
        .unwrap_or(false)
}

pub fn run(target: &Path, prog: &Progress, prefer_cache: bool) -> Result<ScanOutput, String> {
    let t0 = Instant::now();
    let target = normalize(target);
    if !target.exists() {
        return Err(format!("找不到路徑：{}", target.display()));
    }

    if mft_capable(&target) {
        match run_mft(&target, prog) {
            Ok((tree, origin, saver)) => {
                let view = tree.find_path(&target).unwrap_or(0);
                return Ok(ScanOutput { tree, saver, origin, view, target, secs: t0.elapsed().as_secs_f32() });
            }
            Err(e) => {
                if prog.cancelled() {
                    return Err("已取消".into());
                }
                prog.set_stage(format!("MFT 讀取失敗（{e}），改用目錄列舉…"));
            }
        }
    }

    // 目錄列舉
    let key = cache::key_path(&target);
    if prefer_cache {
        prog.set_stage("讀取快取…");
        if let Ok((meta, raw)) = cache::load(&key)
            && meta.method == METHOD_WALK
        {
            let tree = finalize(&raw, 0, meta.errors, "目錄列舉（快取）");
            let age = cache::now_unix().saturating_sub(meta.created);
            return Ok(ScanOutput {
                tree,
                saver: None,
                origin: Origin::CachedWalk { age_secs: age },
                view: 0,
                target,
                secs: t0.elapsed().as_secs_f32(),
            });
        }
    }
    prog.set_stage("列舉資料夾…");
    let raw = scan::walk(&target, prog);
    if prog.cancelled() {
        return Err("已取消".into());
    }
    let errors = prog.errors.load(Relaxed);
    prog.set_stage("整理結果…");
    let tree = finalize(&raw, 0, errors, "目錄列舉");
    let meta = Meta {
        method: METHOD_WALK,
        root_index: 0,
        root_path: target.to_string_lossy().into_owned(),
        created: cache::now_unix(),
        errors,
        ..Default::default()
    };
    let saver = std::thread::spawn(move || {
        let _ = cache::save(&key, &meta, &raw);
    });
    Ok(ScanOutput { tree, saver: Some(saver), origin: Origin::FullWalk, view: 0, target, secs: t0.elapsed().as_secs_f32() })
}

type Saver = Option<std::thread::JoinHandle<()>>;

fn run_mft(target: &Path, prog: &Progress) -> Result<(Tree, Origin, Saver), String> {
    let letter = drive_letter(target).ok_or("不是磁碟機路徑")?;
    let root_name = format!("{letter}:\\");
    prog.set_stage(format!("開啟 {letter}: 磁碟區…"));
    let vol = mft::Volume::open(letter).map_err(|e| e.to_string())?;
    let serial = vol.data.serial as u64;
    let key = cache::key_volume(letter, serial);
    let jd = vol.query_journal();

    // 先試增量更新
    if let Some(jd) = jd {
        prog.set_stage("讀取索引快取…");
        if let Ok((meta, mut raw)) = cache::load(&key)
            && meta.method == METHOD_MFT
            && meta.journal_id == jd.id
            && meta.serial == serial
        {
            prog.set_stage("讀取 USN 變更日誌…");
            if let Changes::List(recs, new_usn) = mft::read_changes(&vol, &jd, meta.next_usn)
                && recs.len() < 400_000
            {
                prog.set_stage(format!("套用 {} 筆變動…", recs.len()));
                let applied = mft::apply_changes(&vol, &mut raw, &recs, &root_name);
                prog.set_stage("整理結果…");
                let tree = finalize(&raw, ROOT_RECORD, 0, "MFT + USN");
                let mut saver = None;
                if !recs.is_empty() {
                    mft::compact_names(&mut raw);
                    let meta = Meta { next_usn: new_usn, created: cache::now_unix(), ..meta };
                    saver = Some(std::thread::spawn(move || {
                        let _ = cache::save(&key, &meta, &raw);
                    }));
                }
                return Ok((tree, Origin::Incremental { changes: applied }, saver));
            }
        }
    }

    // 完整讀取
    prog.set_stage(format!("讀取 {letter}: 的 MFT…"));
    let raw = mft::scan_volume(&vol, &root_name, prog).map_err(|e| e.to_string())?;
    prog.set_stage("整理結果…");
    let tree = finalize(&raw, ROOT_RECORD, 0, "MFT");
    let mut saver = None;
    if let Some(jd) = jd {
        let meta = Meta {
            method: METHOD_MFT,
            root_index: ROOT_RECORD,
            root_path: root_name,
            serial,
            journal_id: jd.id,
            next_usn: jd.next_usn,
            created: cache::now_unix(),
            errors: 0,
        };
        saver = Some(std::thread::spawn(move || {
            let _ = cache::save(&key, &meta, &raw);
        }));
    }
    Ok((tree, Origin::FullMft, saver))
}
