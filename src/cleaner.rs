//! 清理引擎：計算規則可釋放的大小、實際刪除、執行清理指令。

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use parking_lot::Mutex;
use rayon::prelude::*;

use crate::rules::{Action, Rule, Target, wildcard};
use crate::scan::dir_size;
use crate::win;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Contents,
    Dir,
    File,
    Info,
}

#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    pub mode: Mode,
    pub bytes: u64,
    pub files: u64,
    pub errors: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Measured {
    pub found: Vec<Found>,
    pub bytes: u64,
    pub files: u64,
    pub errors: u64,
}

pub fn cutoff(min_age_days: u32) -> u64 {
    if min_age_days == 0 {
        u64::MAX
    } else {
        win::now_filetime().saturating_sub(min_age_days as u64 * win::FILETIME_DAY)
    }
}

/// 單一檔案的大小（hiberfil.sys 這類被鎖定的檔案改用目錄列舉取得）。
fn file_size(path: &Path) -> Option<u64> {
    if let Some((attrs, size, _)) = win::stat(path)
        && attrs & win::ATTR_DIRECTORY == 0
    {
        return Some(size);
    }
    let parent = path.parent()?;
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let mut out = None;
    let _ = win::list_dir(parent, |e| {
        if !e.is_dir() && e.name_os().to_string_lossy().to_lowercase() == name {
            out = Some(e.size);
        }
    });
    out
}

pub fn measure(rule: &Rule, cancel: &AtomicBool) -> Measured {
    let mut m = Measured::default();
    if let Action::RecycleBin = rule.action {
        let (size, items) = win::recycle_bin_size(None);
        m.bytes = size;
        m.files = items;
        if size > 0 {
            m.found.push(Found { path: PathBuf::from("資源回收筒"), mode: Mode::Info, bytes: size, files: items, errors: 0 });
        }
        return m;
    }
    let cut = cutoff(rule.min_age_days);
    for t in &rule.targets {
        for p in crate::rules::expand(t.template()) {
            if cancel.load(Relaxed) {
                return m;
            }
            match t {
                Target::Contents(_) | Target::Dir(_) => {
                    let st = dir_size(&p, cut, cancel);
                    let mode = if matches!(t, Target::Dir(_)) { Mode::Dir } else { Mode::Contents };
                    if st.bytes > 0 || st.files > 0 || mode == Mode::Dir {
                        m.found.push(Found { path: p, mode, bytes: st.bytes, files: st.files, errors: st.errors });
                    }
                    m.errors += st.errors;
                }
                Target::Info(_) => {
                    if p.is_dir() {
                        let st = dir_size(&p, u64::MAX, cancel);
                        m.found.push(Found { path: p, mode: Mode::Info, bytes: st.bytes, files: st.files, errors: st.errors });
                    } else if let Some(sz) = file_size(&p) {
                        m.found.push(Found { path: p, mode: Mode::Info, bytes: sz, files: 1, errors: 0 });
                    }
                }
                Target::Files(_, pats) => {
                    let mut hits = Vec::new();
                    let _ = win::list_dir(&p, |e| {
                        if e.is_dir() || e.mtime > cut {
                            return;
                        }
                        let n = e.name_os();
                        let ns = n.to_string_lossy();
                        if pats.iter().any(|pat| wildcard(pat, &ns)) {
                            hits.push(Found { path: p.join(&n), mode: Mode::File, bytes: e.local_size(), files: 1, errors: 0 });
                        }
                    });
                    m.found.extend(hits);
                }
            }
        }
    }
    // Info 不計入可釋放（例外：只有 Info 且動作是指令時，顯示其大小供參考）
    let only_info = m.found.iter().all(|f| f.mode == Mode::Info);
    for f in &m.found {
        if f.mode != Mode::Info || only_info {
            m.bytes += f.bytes;
            m.files += f.files;
        }
    }
    m
}

// ---------------------------------------------------------------------------
// 安全檢查

/// 絕對不能整個清空或刪除的位置。
pub fn is_protected(path: &Path) -> bool {
    let s = path.to_string_lossy().trim_end_matches('\\').to_lowercase();
    // 必須是絕對路徑，且至少在磁碟根目錄下兩層
    let b = s.as_bytes();
    if b.len() < 3 || b[1] != b':' {
        return true;
    }
    let depth = s.split('\\').filter(|x| !x.is_empty()).count();
    if depth < 2 {
        return true;
    }
    let mut deny: Vec<String> = Vec::new();
    for v in ["USERPROFILE", "SystemRoot", "WINDIR", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "LOCALAPPDATA", "APPDATA", "PUBLIC", "OneDrive"] {
        if let Ok(x) = std::env::var(v) {
            deny.push(x.trim_end_matches('\\').to_lowercase());
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        let h = home.trim_end_matches('\\').to_lowercase();
        for sub in ["documents", "desktop", "downloads", "pictures", "videos", "music", "appdata", "appdata\\locallow", "onedrive", ".cargo", ".rustup", "source", "projects"] {
            deny.push(format!("{h}\\{sub}"));
        }
        if let Some(users) = Path::new(&h).parent() {
            deny.push(users.to_string_lossy().to_lowercase());
        }
    }
    if let Ok(w) = std::env::var("SystemRoot") {
        let w = w.to_lowercase();
        for sub in ["system32", "syswow64", "winsxs", "installer", "servicing", "boot", "fonts"] {
            deny.push(format!("{w}\\{sub}"));
        }
    }
    deny.iter().any(|d| *d == s)
}

// ---------------------------------------------------------------------------
// 刪除

#[derive(Default)]
pub struct DelProgress {
    pub freed: AtomicU64,
    pub files: AtomicU64,
    pub failed: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
    pub log: Mutex<Vec<String>>,
    pub step: AtomicU64,
    pub steps: AtomicU64,
}

impl DelProgress {
    pub fn log(&self, s: impl Into<String>) {
        self.log.lock().push(s.into());
    }
}

fn remove_file_robust(path: &Path, attrs: u32) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied || attrs & win::ATTR_READONLY != 0 => {
            win::clear_readonly(path);
            std::fs::remove_file(path).map_err(|_| e)
        }
        r => r,
    }
}

fn remove_dir_robust(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            win::clear_readonly(path);
            std::fs::remove_dir(path).map_err(|_| e)
        }
        r => r,
    }
}

struct Ent {
    name: OsString,
    attrs: u32,
    size: u64,
    mtime: u64,
}

/// 刪除資料夾內容（保留資料夾本身）。只刪 mtime <= cutoff 的檔案；不跟隨連結。
pub fn delete_children(dir: &Path, cut: u64, p: &DelProgress) {
    if p.cancel.load(Relaxed) {
        return;
    }
    let mut ents: Vec<Ent> = Vec::new();
    if win::list_dir(dir, |e| {
        ents.push(Ent { name: e.name_os(), attrs: e.attrs, size: e.local_size(), mtime: e.mtime });
    })
    .is_err()
    {
        p.failed.fetch_add(1, Relaxed);
        return;
    }
    if let Some(mut g) = p.current.try_lock() {
        *g = dir.to_string_lossy().into_owned();
    }
    ents.par_iter().for_each(|e| {
        if p.cancel.load(Relaxed) {
            return;
        }
        let path = dir.join(&e.name);
        if e.attrs & win::ATTR_DIRECTORY != 0 {
            if e.attrs & win::ATTR_REPARSE != 0 {
                // 連結（junction／symlink）：只移除連結本身
                if std::fs::remove_dir(&path).is_err() && std::fs::remove_file(&path).is_err() {
                    p.failed.fetch_add(1, Relaxed);
                }
            } else {
                delete_children(&path, cut, p);
                let _ = remove_dir_robust(&path); // 裡面還有保留的檔案時會失敗，屬正常
            }
        } else {
            if e.mtime > cut || e.attrs & win::ATTR_CLOUD_MASK != 0 {
                return;
            }
            match remove_file_robust(&path, e.attrs) {
                Ok(()) => {
                    p.freed.fetch_add(e.size, Relaxed);
                    p.files.fetch_add(1, Relaxed);
                }
                Err(_) => {
                    p.failed.fetch_add(1, Relaxed);
                }
            }
        }
    });
}

/// 刪除整個資料夾。
pub fn delete_dir(dir: &Path, p: &DelProgress) -> bool {
    if is_protected(dir) {
        p.log(format!("⛔ 受保護的位置，已略過：{}", dir.display()));
        return false;
    }
    if let Some((attrs, _, _)) = win::stat(dir)
        && attrs & win::ATTR_REPARSE != 0
    {
        return std::fs::remove_dir(dir).is_ok();
    }
    delete_children(dir, u64::MAX, p);
    remove_dir_robust(dir).is_ok()
}

pub fn delete_file(path: &Path, p: &DelProgress) -> bool {
    let (attrs, size) = win::stat(path).map(|(a, s, _)| (a, s)).unwrap_or((0, 0));
    if attrs & win::ATTR_DIRECTORY != 0 {
        return false;
    }
    match remove_file_robust(path, attrs) {
        Ok(()) => {
            p.freed.fetch_add(size, Relaxed);
            p.files.fetch_add(1, Relaxed);
            true
        }
        Err(_) => {
            p.failed.fetch_add(1, Relaxed);
            false
        }
    }
}

/// 把指令輸出（OEM 字碼頁，例如 cp950）轉成字串。
fn decode_oem(b: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    use windows_sys::Win32::Globalization::{CP_OEMCP, MultiByteToWideChar};
    unsafe {
        let n = MultiByteToWideChar(CP_OEMCP, 0, b.as_ptr(), b.len() as i32, std::ptr::null_mut(), 0);
        if n <= 0 {
            return String::from_utf8_lossy(b).into_owned();
        }
        let mut w = vec![0u16; n as usize];
        MultiByteToWideChar(CP_OEMCP, 0, b.as_ptr(), b.len() as i32, w.as_mut_ptr(), n);
        String::from_utf16_lossy(&w)
    }
}

pub fn run_command(program: &str, args: &[String], p: &DelProgress) -> bool {
    use std::os::windows::process::CommandExt;
    p.log(format!("▶ {} {}", program, args.join(" ")));
    *p.current.lock() = format!("執行 {program}…");
    let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let before = win::free_space(Path::new(&format!("{sys}\\")));
    let out = std::process::Command::new(program)
        .args(args)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
    match out {
        Ok(o) => {
            let text = format!("{}{}", decode_oem(&o.stdout), decode_oem(&o.stderr));
            let lines: Vec<&str> = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
            for l in lines.iter().rev().take(6).rev() {
                p.log(format!("   {l}"));
            }
            if let (Some(b), Some(a)) = (before, win::free_space(Path::new(&format!("{sys}\\"))))
                && a > b
            {
                p.freed.fetch_add(a - b, Relaxed);
            }
            if !o.status.success() {
                p.log(format!("   ⚠ 結束代碼 {}", o.status.code().unwrap_or(-1)));
            }
            o.status.success()
        }
        Err(e) => {
            p.log(format!("   ⚠ 無法執行：{e}"));
            false
        }
    }
}

/// 執行一條規則的清理（found 為之前 measure 的結果）。
pub fn clean_rule(rule: &Rule, found: &[Found], p: &DelProgress) {
    let before = (p.freed.load(Relaxed), p.failed.load(Relaxed));
    match &rule.action {
        Action::RecycleBin => {
            let (size, _) = win::recycle_bin_size(None);
            if win::empty_recycle_bin() {
                p.freed.fetch_add(size, Relaxed);
            } else {
                p.failed.fetch_add(1, Relaxed);
            }
        }
        Action::Command { program, args } => {
            run_command(program, args, p);
        }
        Action::Delete => {
            let cut = cutoff(rule.min_age_days);
            for f in found {
                if p.cancel.load(Relaxed) {
                    break;
                }
                match f.mode {
                    Mode::Contents => {
                        if is_protected(&f.path) {
                            p.log(format!("⛔ 受保護的位置，已略過：{}", f.path.display()));
                            continue;
                        }
                        delete_children(&f.path, cut, p);
                    }
                    Mode::Dir => {
                        delete_dir(&f.path, p);
                    }
                    Mode::File => {
                        delete_file(&f.path, p);
                    }
                    Mode::Info => {}
                }
            }
        }
    }
    let freed = p.freed.load(Relaxed) - before.0;
    let failed = p.failed.load(Relaxed) - before.1;
    let mut line = format!("✔ {}：釋放 {}", rule.name, crate::fmt::size(freed));
    if failed > 0 {
        line.push_str(&format!("（{failed} 個項目使用中或無權限，已略過）"));
    }
    p.log(line);
}

/// 所有規則平行計算大小，每完成一條就回呼一次。
pub fn measure_all(rules: &[Rule], cancel: &AtomicBool, on_done: impl Fn(usize, Measured) + Sync) {
    // 目錄列舉是 I/O 密集，用執行緒較多的專用 pool（巢狀的 par_iter 也會跑在這裡）
    let pool = rayon::ThreadPoolBuilder::new().num_threads(crate::scan::worker_threads()).build();
    let run = || {
        rules.par_iter().enumerate().for_each(|(i, r)| {
            let m = measure(r, cancel);
            on_done(i, m);
        });
    };
    match pool {
        Ok(p) => p.install(run),
        Err(_) => run(),
    }
}
