//! 清理引擎：計算規則可釋放的大小、實際刪除、執行清理指令。
//!
//! 安全原則（fail closed）：
//! - 刪除與測量的入口若是 reparse point（junction／symlink），或路徑上任何一層是連結，一律不進去
//! - is_protected() 先正規化路徑，並同時檢查「解析連結與 8.3 短檔名後」的實際路徑
//! - 遞迴前重新確認子資料夾不是剛被換成連結（縮小 TOCTOU 視窗）
//! - 雲端佔位檔（OneDrive）一律不刪

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
        // 溢位時用最保守的值（什麼都不算「夠舊」）
        (min_age_days as u64)
            .checked_mul(win::FILETIME_DAY)
            .map(|d| win::now_filetime().saturating_sub(d))
            .unwrap_or(0)
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
        if size > 0 || items > 0 {
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
            // 目標本身或路徑上任何一層是連結：不測量也不清理
            if win::has_reparse_on_path(&p) {
                continue;
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
                        m.errors += st.errors;
                        m.found.push(Found { path: p, mode: Mode::Info, bytes: st.bytes, files: st.files, errors: st.errors });
                    } else if let Some(sz) = file_size(&p) {
                        m.found.push(Found { path: p, mode: Mode::Info, bytes: sz, files: 1, errors: 0 });
                    }
                }
                Target::Files(_, pats) => {
                    let mut hits = Vec::new();
                    let res = win::list_dir(&p, |e| {
                        if e.is_dir() || e.is_reparse() || e.is_cloud() || e.mtime > cut {
                            return;
                        }
                        let n = e.name_os();
                        let ns = n.to_string_lossy();
                        if pats.iter().any(|pat| wildcard(pat, &ns)) {
                            hits.push(Found { path: p.join(&n), mode: Mode::File, bytes: e.local_size(), files: 1, errors: 0 });
                        }
                    });
                    if res.is_err() {
                        m.errors += 1;
                    }
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

/// 正規化：小寫、`/` 換成 `\`、去掉結尾 `\`。必須是 `X:\…` 絕對路徑，
/// 而且不能有 `.`、`..` 元件；不符合就回傳 None（呼叫端應視為受保護）。
fn normalize(path: &Path) -> Option<String> {
    let s = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    let b = s.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return None;
    }
    let parts: Vec<&str> = s[3..].split('\\').filter(|x| !x.is_empty()).collect();
    if parts.iter().any(|x| *x == "." || *x == "..") {
        return None;
    }
    let mut out = s[..3].to_string();
    out.push_str(&parts.join("\\"));
    Some(out.trim_end_matches('\\').to_string())
}

fn env_norm(name: &str) -> Option<String> {
    let v = std::env::var(name).ok().filter(|v| !v.trim().is_empty())?;
    normalize(Path::new(&v))
}

/// 檢查單一（已正規化）路徑。
fn protected_norm(s: &str) -> bool {
    let depth = s.split('\\').filter(|x| !x.is_empty()).count();
    // 磁碟根目錄本身不行（根目錄下的 Windows、Program Files 等由下面的清單保護）
    if depth < 2 {
        return true;
    }
    let drive = &s[..2];
    let sysdrive = std::env::var("SystemDrive").ok().map(|d| d.to_lowercase()).unwrap_or_else(|| "c:".into());
    let windir = env_norm("SystemRoot").or_else(|| env_norm("WINDIR")).unwrap_or_else(|| format!("{sysdrive}\\windows"));

    // 任何磁碟根目錄下的系統資料夾，以及它們的整個子樹
    for root_sub in ["$recycle.bin", "system volume information", "recovery", "boot", "efi", "config.msi"] {
        let r = format!("{drive}\\{root_sub}");
        if s == r || s.starts_with(&format!("{r}\\")) {
            return true;
        }
    }

    // 任何磁碟根目錄下這些名稱本身不能整個刪（例如裝在 D 槽的 Program Files）
    for root_sub in ["windows", "program files", "program files (x86)", "programdata", "users"] {
        if s == format!("{drive}\\{root_sub}") {
            return true;
        }
    }

    // Windows 核心：整個子樹都不能動
    for sub in [
        "system32", "syswow64", "winsxs", "servicing", "boot", "fonts", "assembly", "microsoft.net", "systemapps",
        "systemresources", "inf", "policydefinitions", "globalization", "immersivecontrolpanel", "shellexperiences",
    ] {
        let r = format!("{windir}\\{sub}");
        if s == r || s.starts_with(&format!("{r}\\")) {
            return true;
        }
    }

    // 完全相等就不行的位置
    // （TEMP 不能放這裡：「使用者暫存資料夾」規則就是要清它的內容）
    let mut exact: Vec<String> = vec![windir.clone(), format!("{windir}\\installer")];
    for v in [
        "USERPROFILE", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "LOCALAPPDATA", "APPDATA", "PUBLIC", "OneDrive",
        "OneDriveCommercial", "OneDriveConsumer",
    ] {
        if let Some(x) = env_norm(v) {
            exact.push(x);
        }
    }
    // 所有使用者設定檔：Users 本身、Users 底下每一個帳號資料夾
    let mut users_roots: Vec<String> = vec![format!("{sysdrive}\\users")];
    if let Some(home) = env_norm("USERPROFILE") {
        if let Some((parent, _)) = home.rsplit_once('\\') {
            users_roots.push(parent.to_string());
        }
        for sub in [
            "documents", "desktop", "downloads", "pictures", "videos", "music", "appdata", "appdata\\local", "appdata\\roaming",
            "appdata\\locallow", "onedrive", ".cargo", ".rustup", ".ssh", ".gnupg", "source", "projects", "favorites", "contacts",
            "saved games", "searches", "links", "3d objects",
        ] {
            exact.push(format!("{home}\\{sub}"));
        }
    }
    for u in &users_roots {
        if s == u {
            return true;
        }
        if let Some(rest) = s.strip_prefix(&format!("{u}\\"))
            && !rest.contains('\\')
        {
            return true; // C:\Users\<任何人>
        }
    }
    exact.iter().any(|d| d == s)
}

/// 絕對不能整個清空或刪除的位置。路徑格式不對時一律視為受保護。
pub fn is_protected(path: &Path) -> bool {
    let Some(s) = normalize(path) else { return true };
    if protected_norm(&s) {
        return true;
    }
    // 解析 junction、symlink、8.3 短檔名之後的實際位置也要檢查
    if let Some(real) = win::final_path(path) {
        match normalize(&real) {
            Some(r) => {
                if protected_norm(&r) {
                    return true;
                }
            }
            None => return true,
        }
    }
    false
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

/// 只移除連結本身（junction／目錄 symlink／檔案 symlink），不碰連結指向的內容。
fn remove_link(path: &Path) -> bool {
    std::fs::remove_dir(path).is_ok() || std::fs::remove_file(path).is_ok()
}

struct Ent {
    name: OsString,
    attrs: u32,
    size: u64,
    mtime: u64,
}

/// 刪除資料夾內容（保留資料夾本身）。只刪 mtime <= cutoff 的項目；不跟隨連結。
pub fn delete_children(dir: &Path, cut: u64, p: &DelProgress) {
    if p.cancel.load(Relaxed) {
        return;
    }
    // 入口本身是連結：拒絕（否則會清掉連結另一端的內容）
    if win::is_reparse(dir) {
        p.log(format!("⛔ 這個位置是連結（junction／symlink），為了安全已略過：{}", dir.display()));
        p.failed.fetch_add(1, Relaxed);
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
                if e.mtime > cut {
                    return;
                }
                if !remove_link(&path) {
                    p.failed.fetch_add(1, Relaxed);
                }
            } else {
                // 列舉後到遞迴前，資料夾可能被換成連結：再確認一次
                if win::is_reparse(&path) {
                    if e.mtime <= cut && !remove_link(&path) {
                        p.failed.fetch_add(1, Relaxed);
                    }
                    return;
                }
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

/// 刪除整個資料夾。資料夾本身是連結時只移除連結；路徑上層有連結則拒絕。
pub fn delete_dir(dir: &Path, p: &DelProgress) -> bool {
    if is_protected(dir) {
        p.log(format!("⛔ 受保護的位置，已略過：{}", dir.display()));
        return false;
    }
    if let Some(parent) = dir.parent()
        && win::has_reparse_on_path(parent)
    {
        p.log(format!("⛔ 路徑經過連結（junction／symlink），為了安全已略過：{}", dir.display()));
        return false;
    }
    if win::is_reparse(dir) {
        return remove_link(dir);
    }
    delete_children(dir, u64::MAX, p);
    remove_dir_robust(dir).is_ok()
}

/// 刪除單一檔案（只刪 mtime <= cut 的）。雲端佔位檔、受保護位置、經過連結的路徑都拒絕。
pub fn delete_file(path: &Path, cut: u64, p: &DelProgress) -> bool {
    let Some((attrs, size, mtime)) = win::stat(path) else {
        return false; // 已經不在了
    };
    if attrs & win::ATTR_DIRECTORY != 0 {
        return false;
    }
    if attrs & win::ATTR_CLOUD_MASK != 0 {
        p.log(format!("☁ 雲端佔位檔不刪（刪了會連雲端一起刪）：{}", path.display()));
        return false;
    }
    if mtime > cut {
        return false;
    }
    if is_protected(path) || path.parent().is_some_and(|d| is_protected(d) && protected_subtree(d)) {
        p.log(format!("⛔ 受保護的位置，已略過：{}", path.display()));
        return false;
    }
    if let Some(parent) = path.parent()
        && win::has_reparse_on_path(parent)
    {
        p.log(format!("⛔ 路徑經過連結（junction／symlink），為了安全已略過：{}", path.display()));
        return false;
    }
    if attrs & win::ATTR_REPARSE != 0 {
        return remove_link(path);
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

/// 父資料夾是否屬於「整個子樹都受保護」的區域（System32 等）。
fn protected_subtree(dir: &Path) -> bool {
    let Some(s) = normalize(dir) else { return true };
    // 用一個一定不存在的子路徑測試：子樹保護會擋，完全相等的保護不會
    protected_norm(&format!("{s}\\__dc_probe__"))
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

/// 所有本機磁碟的剩餘空間總和
fn total_free() -> u64 {
    win::drives().iter().filter(|d| d.kind == "本機磁碟").map(|d| d.free).sum()
}

pub fn run_command(program: &str, args: &[String], p: &DelProgress) -> bool {
    use std::os::windows::process::CommandExt;
    p.log(format!("▶ {} {}", program, args.join(" ")));
    *p.current.lock() = format!("執行 {program}…");
    let before = total_free();
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
            // 所有本機磁碟剩餘空間的變化（其他程式同時寫入也會影響，只能當估計）
            let after = total_free();
            if after > before {
                p.freed.fetch_add(after - before, Relaxed);
            }
            if !o.status.success() {
                p.log(format!("   ⚠ 結束代碼 {}", o.status.code().unwrap_or(-1)));
                p.failed.fetch_add(1, Relaxed);
            }
            o.status.success()
        }
        Err(e) => {
            p.log(format!("   ⚠ 無法執行：{e}"));
            p.failed.fetch_add(1, Relaxed);
            false
        }
    }
}

/// 執行一條規則的清理（found 為「剛剛」measure 的結果）。
pub fn clean_rule(rule: &Rule, found: &[Found], p: &DelProgress) {
    let before = (p.freed.load(Relaxed), p.failed.load(Relaxed), p.files.load(Relaxed));
    match &rule.action {
        Action::RecycleBin => {
            let (size, items) = win::recycle_bin_size(None);
            let ok = win::empty_recycle_bin();
            // 以清空前後的差值計算（部分檔案使用中時也能算對）
            let (size2, items2) = win::recycle_bin_size(None);
            p.freed.fetch_add(size.saturating_sub(size2), Relaxed);
            p.files.fetch_add(items.saturating_sub(items2), Relaxed);
            if !ok || items2 > 0 {
                p.failed.fetch_add(items2.max(1), Relaxed);
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
                        if win::has_reparse_on_path(&f.path) {
                            p.log(format!("⛔ 路徑經過連結（junction／symlink），為了安全已略過：{}", f.path.display()));
                            continue;
                        }
                        delete_children(&f.path, cut, p);
                    }
                    Mode::Dir => {
                        delete_dir(&f.path, p);
                    }
                    Mode::File => {
                        delete_file(&f.path, cut, p);
                    }
                    Mode::Info => {}
                }
            }
        }
    }
    let freed = p.freed.load(Relaxed) - before.0;
    let failed = p.failed.load(Relaxed) - before.1;
    let mark = if failed > 0 && freed == 0 && p.files.load(Relaxed) == before.2 { "⚠" } else { "✔" };
    let mut line = format!("{mark} {}：釋放 {}", rule.name, crate::fmt::size(freed));
    if failed > 0 {
        line.push_str(&format!("（{failed} 個項目使用中、無權限或失敗，已略過）"));
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
