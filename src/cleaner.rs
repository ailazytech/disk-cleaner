//! 清理引擎：計算規則可釋放的大小、實際刪除、執行清理指令。
//!
//! 安全原則（fail closed）：
//! - 刪除與測量的入口若是 reparse point（junction／symlink），或路徑上任何一層是連結，一律不進去
//! - is_protected() 先正規化路徑，並同時檢查「解析連結與 8.3 短檔名後」的實際路徑；解析失敗視為受保護
//! - 刪除時從磁碟根目錄逐層「不跟隨連結」地打開並握住每一層資料夾（別人無法在刪除途中把它換成連結），
//!   每個項目也是先打開 handle、從 handle 確認不是連結，再透過同一個 handle 刪除
//! - 遞迴途中遇到受保護的資料夾（例如 TEMP 底下剛好是 ProgramData）整個略過
//! - 雲端佔位檔（OneDrive）一律不刪；有其他硬連結的檔案不計入釋放空間

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

/// 受保護位置的清單（環境變數與 Known Folder 只在第一次用到時讀取）。
struct Protected {
    windir: String,
    exact: Vec<String>,
    users_roots: Vec<String>,
}

fn protected_set() -> &'static Protected {
    static P: std::sync::OnceLock<Protected> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let sysdrive = std::env::var("SystemDrive").ok().map(|d| d.to_lowercase()).unwrap_or_else(|| "c:".into());
        let windir = env_norm("SystemRoot").or_else(|| env_norm("WINDIR")).unwrap_or_else(|| format!("{sysdrive}\\windows"));
        // 完全相等就不行的位置
        // （TEMP 不能放這裡：「使用者暫存資料夾」規則就是要清它的內容）
        let mut exact: Vec<String> = vec![windir.clone(), format!("{windir}\\installer")];
        for v in [
            "USERPROFILE", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "LOCALAPPDATA", "APPDATA", "PUBLIC",
            "OneDrive", "OneDriveCommercial", "OneDriveConsumer",
        ] {
            if let Some(x) = env_norm(v) {
                exact.push(x);
            }
        }
        // 文件、桌面、下載…的實際位置（使用者可能把它們搬到別的磁碟）
        exact.extend(win::known_folders().iter().filter_map(|p| normalize(p)));
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
        Protected { windir, exact, users_roots }
    })
}

/// 檢查單一（已正規化）路徑。
fn protected_norm(s: &str) -> bool {
    let depth = s.split('\\').filter(|x| !x.is_empty()).count();
    // 磁碟根目錄本身不行（根目錄下的 Windows、Program Files 等由下面的清單保護）
    if depth < 2 {
        return true;
    }
    let drive = &s[..2];
    let Protected { windir, exact, users_roots } = protected_set();

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

    for u in users_roots {
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

/// 絕對不能整個清空或刪除的位置。路徑格式不對、或存在卻無法解析實際位置時，一律視為受保護。
pub fn is_protected(path: &Path) -> bool {
    let Some(s) = normalize(path) else { return true };
    if protected_norm(&s) {
        return true;
    }
    // 解析 junction、symlink、8.3 短檔名之後的實際位置也要檢查
    match win::final_path(path) {
        Some(real) => normalize(&real).is_none_or(|r| protected_norm(&r)),
        // 不存在就沒有東西可刪；存在卻解析不了（權限、I/O 錯誤）就不冒險
        None => std::fs::symlink_metadata(path).is_ok(),
    }
}

/// 遞迴途中的子資料夾：路徑來自列舉結果（長檔名、上層都已握住不會變），只需比對字串。
fn protected_child(path: &Path) -> bool {
    normalize(path).is_none_or(|s| protected_norm(&s))
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

/// 握住 dir 與它的所有上層。失敗時記錄原因；已經不存在就安靜地略過。
fn pin(dir: &Path, p: &DelProgress) -> Option<Vec<win::Handle>> {
    match win::pin_dir(dir) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            p.log(format!("⛔ 路徑經過連結（junction／symlink）或無法鎖定，為了安全已略過：{}（{e}）", dir.display()));
            p.failed.fetch_add(1, Relaxed);
            None
        }
    }
}

/// 打開要刪的項目。已經不存在回傳 None（不算失敗）；其他錯誤算失敗。
fn open_for_delete(path: &Path, p: &DelProgress) -> Option<(win::Handle, win::HInfo)> {
    match win::Handle::open(path, win::Open::Delete) {
        Ok(h) => match h.info() {
            Some(i) => Some((h, i)),
            None => {
                p.failed.fetch_add(1, Relaxed);
                None
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            p.failed.fetch_add(1, Relaxed);
            None
        }
    }
}

/// 刪掉一個檔案（透過已確認過的 handle）。有其他硬連結時空間不會真的釋放，不計入。
fn delete_handle_file(h: &win::Handle, i: &win::HInfo, p: &DelProgress) -> bool {
    match h.delete() {
        Ok(()) => {
            if i.links <= 1 {
                p.freed.fetch_add(i.size, Relaxed);
            }
            p.files.fetch_add(1, Relaxed);
            true
        }
        Err(_) => {
            p.failed.fetch_add(1, Relaxed);
            false
        }
    }
}

/// 刪掉一個（應該已經清空的）資料夾。裡面還有刻意保留的項目時不算失敗。
fn delete_handle_dir(h: &win::Handle, p: &DelProgress) -> bool {
    match h.delete() {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(win::ERROR_DIR_NOT_EMPTY) => false,
        Err(_) => {
            p.failed.fetch_add(1, Relaxed);
            false
        }
    }
}

/// 刪除資料夾內容（保留資料夾本身）。只刪 mtime <= cutoff 的項目；不跟隨連結。
pub fn delete_children(dir: &Path, cut: u64, p: &DelProgress) {
    if p.cancel.load(Relaxed) {
        return;
    }
    // 入口與所有上層都握住；任何一層是連結就拒絕（否則會清掉連結另一端的內容）
    let Some(_pins) = pin(dir, p) else { return };
    clean_dir(dir, cut, p);
}

/// dir 與它的所有上層都已經被握住（不會在途中被換掉）。
fn clean_dir(dir: &Path, cut: u64, p: &DelProgress) {
    if p.cancel.load(Relaxed) {
        return;
    }
    let mut ents: Vec<(OsString, u32, u64)> = Vec::new();
    if win::list_dir(dir, |e| ents.push((e.name_os(), e.attrs, e.mtime))).is_err() {
        p.failed.fetch_add(1, Relaxed);
        return;
    }
    if let Some(mut g) = p.current.try_lock() {
        *g = dir.to_string_lossy().into_owned();
    }
    ents.par_iter().for_each(|(name, attrs, mtime)| {
        if p.cancel.load(Relaxed) {
            return;
        }
        // 先用列舉結果過濾，省得打開不需要的項目（打開後還會從 handle 再確認一次）
        if attrs & win::ATTR_CLOUD_MASK != 0 {
            return;
        }
        let is_dir = attrs & win::ATTR_DIRECTORY != 0 && attrs & win::ATTR_REPARSE == 0;
        if !is_dir && *mtime > cut {
            return;
        }
        let path = dir.join(name);
        if is_dir && protected_child(&path) {
            p.log(format!("⛔ 受保護的位置，已略過：{}", path.display()));
            return;
        }
        let Some((h, i)) = open_for_delete(&path, p) else { return };
        if i.is_cloud() {
            return;
        }
        if i.is_reparse() {
            // 連結（或列舉之後才被換成連結）：只移除連結本身，不碰另一端
            if i.mtime <= cut && h.delete().is_err() {
                p.failed.fetch_add(1, Relaxed);
            }
            return;
        }
        if i.is_dir() {
            if protected_child(&path) {
                return;
            }
            // h 握在手上：遞迴期間這個資料夾不能被換掉
            clean_dir(&path, cut, p);
            // 資料夾本身是最近才建立或變動的就保留（mtime 是刪內容之前取的）
            if i.mtime <= cut {
                delete_handle_dir(&h, p);
            }
        } else if i.mtime <= cut {
            delete_handle_file(&h, &i, p);
        }
    });
}

/// 刪除整個資料夾。資料夾本身是連結時只移除連結；路徑上層有連結則拒絕。
pub fn delete_dir(dir: &Path, p: &DelProgress) -> bool {
    if is_protected(dir) {
        p.log(format!("⛔ 受保護的位置，已略過：{}", dir.display()));
        p.failed.fetch_add(1, Relaxed);
        return false;
    }
    let Some(parent) = dir.parent() else { return false };
    let Some(_pins) = pin(parent, p) else { return false };
    let Some((h, i)) = open_for_delete(dir, p) else { return false };
    if i.is_cloud() {
        p.log(format!("☁ 雲端資料夾不刪（刪了會連雲端一起刪）：{}", dir.display()));
        p.failed.fetch_add(1, Relaxed);
        return false;
    }
    if i.is_reparse() {
        return delete_handle_dir(&h, p);
    }
    if !i.is_dir() {
        return false;
    }
    clean_dir(dir, u64::MAX, p);
    match h.delete() {
        Ok(()) => true,
        Err(_) => {
            p.failed.fetch_add(1, Relaxed);
            false
        }
    }
}

/// 刪除單一檔案（只刪 mtime <= cut 的）。雲端佔位檔、受保護位置、經過連結的路徑都拒絕。
pub fn delete_file(path: &Path, cut: u64, p: &DelProgress) -> bool {
    if is_protected(path) || path.parent().is_some_and(protected_subtree) {
        p.log(format!("⛔ 受保護的位置，已略過：{}", path.display()));
        p.failed.fetch_add(1, Relaxed);
        return false;
    }
    let Some(parent) = path.parent() else { return false };
    let Some(_pins) = pin(parent, p) else { return false };
    let Some((h, i)) = open_for_delete(path, p) else { return false };
    if i.is_dir() && !i.is_reparse() {
        return false;
    }
    if i.is_cloud() {
        p.log(format!("☁ 雲端佔位檔不刪（刪了會連雲端一起刪）：{}", path.display()));
        p.failed.fetch_add(1, Relaxed);
        return false;
    }
    if i.mtime > cut {
        return false;
    }
    if i.is_reparse() {
        return h.delete().is_ok();
    }
    delete_handle_file(&h, &i, p)
}

/// 刪除前共用的檢查（分析頁的「移到資源回收筒」也要走這裡）：受保護位置、路徑經過連結、雲端檔。
/// 通過回傳 Ok，否則回傳要顯示的原因。
pub fn precheck(path: &Path) -> Result<(), String> {
    if is_protected(path) || path.parent().is_some_and(protected_subtree) {
        return Err("這是受保護的位置，不能刪除".into());
    }
    if path.parent().is_some_and(win::has_reparse_on_path) {
        return Err("路徑經過連結（junction／symlink），為了安全不刪".into());
    }
    if let Some((attrs, _, _)) = win::stat(path)
        && attrs & win::ATTR_CLOUD_MASK != 0
    {
        return Err("雲端佔位檔（OneDrive 等），刪了會連雲端一起刪".into());
    }
    Ok(())
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

/// 所有本機磁碟的剩餘空間總和（同一個磁碟區有多個代號時只算一次）
fn total_free() -> u64 {
    let mut seen = std::collections::HashSet::new();
    win::drives()
        .iter()
        .filter(|d| d.kind == "本機磁碟")
        .filter(|d| seen.insert(win::volume_name(&d.root).unwrap_or_else(|| d.root.clone())))
        .map(|d| d.free)
        .sum()
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
                            p.failed.fetch_add(1, Relaxed);
                            continue;
                        }
                        // 路徑上的連結由 delete_children 握住每一層時檢查
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
        line.push_str(&format!("（{failed} 個項目使用中、無權限、失敗，或為了安全略過）"));
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
