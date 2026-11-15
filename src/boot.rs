//! 開機加速：開機時間（Windows 的開機效能記錄）、拖慢開機的項目、開機自動啟動的程式、系統設定檢查。
//!
//! 會修改系統的只有兩件事，而且都可以還原：
//! - 啟用／停用開機自動啟動的程式：
//!   - Run 機碼與啟動資料夾的項目寫 `Explorer\StartupApproved`（和工作管理員「啟動」分頁相同），
//!     不刪除、不修改 Run 值與捷徑本身
//!   - Microsoft Store 應用程式的啟動工作寫它自己的 `State`（1＝使用者停用、2＝啟用），受原則管理的不能改
//! - 開啟／關閉「快速啟動」：`HiberbootEnabled`（開啟時順便 `powercfg /h on`，休眠原本關著就用較小的休眠檔）

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_DWORD, REG_EXPAND_SZ, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

use crate::{bootopt, win};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";
const STORE: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData";
const POWER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Power";
const HIBERNATE: &str = r"SYSTEM\CurrentControlSet\Control\Power";
/// 一律用 64 位元的登錄檔視圖（WOW6432Node 直接寫在路徑裡）
const KEY_WOW64_64KEY: u32 = 0x0100;

// ---------------------------------------------------------------------------
// 登錄檔

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn open_key(root: HKEY, path: &str, access: u32) -> Option<Key> {
    let w = win::wide(path);
    let mut h: HKEY = std::ptr::null_mut();
    (unsafe { RegOpenKeyExW(root, w.as_ptr(), 0, access | KEY_WOW64_64KEY, &mut h) } == 0).then_some(Key(h))
}

fn from_wide_bytes(data: &[u8]) -> String {
    let u: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = u.iter().position(|&c| c == 0).unwrap_or(u.len());
    String::from_utf16_lossy(&u[..end])
}

/// 列出一個機碼裡所有的值：(名稱, 型別, 資料)。資料太長時依回報的大小重試（上限 1 MB）。
fn values(k: &Key) -> Vec<(String, u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0u32;
    let mut cap = 8192usize;
    while i < 10_000 {
        let mut name = vec![0u16; 16384];
        let mut nlen = name.len() as u32;
        let mut ty = 0u32;
        let mut data = vec![0u8; cap];
        let mut dlen = data.len() as u32;
        let r = unsafe { RegEnumValueW(k.0, i, name.as_mut_ptr(), &mut nlen, std::ptr::null(), &mut ty, data.as_mut_ptr(), &mut dlen) };
        match r {
            0 => {
                data.truncate(dlen as usize);
                out.push((String::from_utf16_lossy(&name[..nlen as usize]), ty, data));
                cap = 8192;
                i += 1;
            }
            234 if (dlen as usize) > cap && dlen as usize <= 1 << 20 => cap = dlen as usize, // ERROR_MORE_DATA：放大後重試同一筆
            234 => i += 1,
            _ => break, // ERROR_NO_MORE_ITEMS 或其他錯誤
        }
    }
    out
}

fn subkeys(k: &Key) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..10_000u32 {
        let mut name = vec![0u16; 512];
        let mut nlen = name.len() as u32;
        let r = unsafe {
            RegEnumKeyExW(k.0, i, name.as_mut_ptr(), &mut nlen, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        if r != 0 {
            break;
        }
        out.push(String::from_utf16_lossy(&name[..nlen as usize]));
    }
    out
}

fn query(k: &Key, name: &str) -> Option<(u32, Vec<u8>)> {
    let w = win::wide(name);
    let mut ty = 0u32;
    let mut data = vec![0u8; 4096];
    let mut len = data.len() as u32;
    let r = unsafe { RegQueryValueExW(k.0, w.as_ptr(), std::ptr::null(), &mut ty, data.as_mut_ptr(), &mut len) };
    (r == 0).then(|| {
        data.truncate(len as usize);
        (ty, data)
    })
}

fn dword(v: Option<(u32, Vec<u8>)>) -> Option<u32> {
    match v? {
        (t, d) if t == REG_DWORD && d.len() == 4 => Some(u32::from_le_bytes([d[0], d[1], d[2], d[3]])),
        _ => None,
    }
}

fn read_dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
    dword(query(&open_key(root, path, KEY_QUERY_VALUE)?, name))
}

fn reg_err(r: u32, what: &str) -> String {
    if r == 5 { "需要系統管理員權限".into() } else { format!("無法{what}（錯誤 {r}）") }
}

fn write_value(root: HKEY, path: &str, name: &str, ty: u32, data: &[u8]) -> Result<(), String> {
    let w = win::wide(path);
    let mut h: HKEY = std::ptr::null_mut();
    let r = unsafe {
        RegCreateKeyExW(root, w.as_ptr(), 0, std::ptr::null(), 0, KEY_SET_VALUE | KEY_WOW64_64KEY, std::ptr::null(), &mut h, std::ptr::null_mut())
    };
    if r != 0 {
        return Err(reg_err(r, "開啟登錄機碼"));
    }
    let k = Key(h);
    let n = win::wide(name);
    let r = unsafe { RegSetValueExW(k.0, n.as_ptr(), 0, ty, data.as_ptr(), data.len() as u32) };
    if r == 0 { Ok(()) } else { Err(reg_err(r, "寫入登錄")) }
}

/// 展開 %VAR%（REG_EXPAND_SZ 用）
pub(crate) fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) => {
                let name = &after[..j];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// 指令與捷徑解析

const RUNNABLE: [&str; 9] = [".exe", ".com", ".bat", ".cmd", ".vbs", ".js", ".ps1", ".msc", ".lnk"];

fn has_runnable_ext(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    RUNNABLE.iter().any(|e| l.ends_with(e))
}

/// 只有「確定不存在」才算找不到；權限不足之類的錯誤當成存在（不亂報）
fn not_found(p: &Path) -> bool {
    std::fs::metadata(p).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// 從指令取出要執行的檔案，並判斷它是不是確定不存在。仿照 Windows 的規則：
/// 有引號就取引號內；沒有引號就由短到長嘗試「前幾個片段」，有存在的檔案（或補上 .exe 後存在）就用它；
/// 都不存在時，取第一個以可執行副檔名結尾的片段。相對路徑（rundll32.exe、cmd）交給系統的 PATH，不判斷存不存在。
fn resolve_exe(cmd: &str) -> (Option<PathBuf>, bool) {
    let (exe, missing, _) = parse_command(cmd);
    (exe, missing)
}

/// 同 resolve_exe，另外回傳程式後面的參數（已展開環境變數）。
fn parse_command(cmd: &str) -> (Option<PathBuf>, bool, String) {
    let c = expand_env(cmd.trim());
    if let Some(rest) = c.strip_prefix('"') {
        let Some(e) = rest.find('"') else { return (None, false, String::new()) };
        let (s, args) = (&rest[..e], rest[e + 1..].trim().to_string());
        let p = PathBuf::from(s);
        if !p.is_absolute() {
            return (Some(p), false, args);
        }
        if p.is_file() || has_runnable_ext(s) {
            let missing = not_found(&p);
            return (Some(p), missing, args);
        }
        // 和 CreateProcess 一樣：沒寫副檔名時補上 .exe 再找
        let with_exe = PathBuf::from(format!("{s}.exe"));
        if with_exe.is_file() {
            return (Some(with_exe), false, args);
        }
        let missing = not_found(&p) && not_found(&with_exe);
        return (Some(p), missing, args);
    }
    // 沒有引號：由短到長，在每個空白（或 tab）處切一刀試試看
    let mut cuts: Vec<usize> = c.char_indices().filter(|&(_, ch)| ch == ' ' || ch == '\t').map(|(i, _)| i).collect();
    cuts.push(c.len());
    let first_end = cuts[0];
    let rest_from = |end: usize| c[end..].trim().to_string();
    let mut first_named: Option<(String, usize)> = None;
    for &end in &cuts {
        let cand = c[..end].trim_end();
        if cand.is_empty() {
            continue;
        }
        let p = Path::new(cand);
        if !p.is_absolute() {
            // 相對名稱：只看第一個片段
            return (Some(PathBuf::from(&c[..first_end])), false, rest_from(first_end));
        }
        if p.is_file() {
            return (Some(p.to_path_buf()), false, rest_from(end));
        }
        let with_exe = PathBuf::from(format!("{cand}.exe"));
        if !has_runnable_ext(cand) && with_exe.is_file() {
            return (Some(with_exe), false, rest_from(end));
        }
        if first_named.is_none() && has_runnable_ext(cand) {
            first_named = Some((cand.to_string(), end));
        }
    }
    match first_named {
        Some((n, end)) => {
            let p = PathBuf::from(n);
            let missing = not_found(&p);
            (Some(p), missing, rest_from(end))
        }
        // 判斷不出來：當成存在，不亂報
        None => (Some(PathBuf::from(&c[..first_end])), false, rest_from(first_end)),
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// ANSI（系統字碼頁）位元組 → 字串
fn ansi(s: &[u8]) -> Option<String> {
    use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
    if s.is_empty() {
        return Some(String::new());
    }
    let mut w = vec![0u16; s.len() + 1];
    let n = unsafe { MultiByteToWideChar(CP_ACP, 0, s.as_ptr(), s.len() as i32, w.as_mut_ptr(), w.len() as i32) };
    (n > 0).then(|| String::from_utf16_lossy(&w[..n as usize]))
}

fn cstr_ansi(b: &[u8], o: usize) -> Option<String> {
    let s = b.get(o..)?;
    let end = s.iter().position(|&c| c == 0)?;
    ansi(&s[..end])
}

fn cstr_utf16(b: &[u8], o: usize) -> Option<String> {
    let s = b.get(o..)?;
    let u: Vec<u16> = s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    Some(String::from_utf16_lossy(&u))
}

/// 捷徑（.lnk，MS-SHLLINK 格式）指向的本機檔案路徑：LinkInfo 的 LocalBasePath＋CommonPathSuffix。
/// 只讀檔案內容，不透過 Shell 解析（不會觸發任何搜尋或網路存取）。
fn lnk_target_bytes(d: &[u8]) -> Option<PathBuf> {
    lnk_parse(d).map(|(t, _, _)| t)
}

/// 回傳（目標, StringData 的位置, 旗標）
fn lnk_parse(d: &[u8]) -> Option<(PathBuf, usize, u32)> {
    if u32_at(d, 0)? != 0x4C {
        return None;
    }
    // LinkCLSID 00021401-0000-0000-C000-000000000046
    const CLSID: [u8; 16] = [0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    if d.get(4..20)? != CLSID {
        return None;
    }
    let flags = u32_at(d, 0x14)?;
    let mut off = 0x4Cusize;
    if flags & 0x1 != 0 {
        off = off.checked_add(2 + u16_at(d, off)? as usize)?;
    }
    if flags & 0x2 == 0 {
        return None; // 沒有 LinkInfo
    }
    let li = off;
    let li_size = u32_at(d, li)? as usize;
    let hdr = u32_at(d, li + 4)? as usize;
    if li_size < hdr || li.checked_add(li_size)? > d.len() {
        return None;
    }
    let li_flags = u32_at(d, li + 8)?;
    if li_flags & 0x1 == 0 {
        return None; // 不是本機路徑（網路分享）
    }
    let info = &d[li..li + li_size];
    // 標頭夠大而且真的填了 Unicode 位移（不是 0）才讀 Unicode 版本，否則讀 ANSI 版本
    let (ub, us) = if hdr >= 0x24 { (u32_at(info, 28)? as usize, u32_at(info, 32)? as usize) } else { (0, 0) };
    let (base, suffix) = if ub >= hdr && us >= hdr && ub > 0 {
        (cstr_utf16(info, ub)?, cstr_utf16(info, us)?)
    } else {
        (cstr_ansi(info, u32_at(info, 16)? as usize)?, cstr_ansi(info, u32_at(info, 24)? as usize)?)
    };
    let full = format!("{base}{suffix}");
    (!full.is_empty()).then(|| (PathBuf::from(full), li + li_size, flags))
}

/// 捷徑的目標、參數與工作目錄（延後啟動要照原樣執行）
#[derive(Clone, Debug, PartialEq)]
pub struct LnkInfo {
    pub target: PathBuf,
    pub args: String,
    pub workdir: Option<PathBuf>,
}

/// 讀出捷徑的 StringData（名稱、相對路徑、工作目錄、參數…）。讀不完整就回傳 None，不猜。
fn lnk_info_bytes(d: &[u8]) -> Option<LnkInfo> {
    let (target, mut p, flags) = lnk_parse(d)?;
    let unicode = flags & 0x80 != 0;
    let mut read = || -> Option<String> {
        let n = u16_at(d, p)? as usize;
        p += 2;
        if unicode {
            let b = d.get(p..p + n * 2)?;
            p += n * 2;
            let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Some(String::from_utf16_lossy(&u))
        } else {
            let b = d.get(p..p + n)?;
            p += n;
            ansi(b)
        }
    };
    let mut workdir = None;
    let mut args = String::new();
    if flags & 0x04 != 0 {
        read()?; // 名稱（說明文字）
    }
    if flags & 0x08 != 0 {
        read()?; // 相對路徑
    }
    if flags & 0x10 != 0 {
        let w = read()?;
        workdir = (!w.trim().is_empty()).then(|| PathBuf::from(expand_env(w.trim())));
    }
    if flags & 0x20 != 0 {
        args = expand_env(read()?.trim());
    }
    Some(LnkInfo { target: PathBuf::from(expand_env(&target.to_string_lossy())), args, workdir })
}

fn lnk_target(p: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > 1 << 20 {
        return None;
    }
    lnk_target_bytes(&std::fs::read(p).ok()?).map(|t| PathBuf::from(expand_env(&t.to_string_lossy())))
}

pub fn lnk_details(p: &Path) -> Option<LnkInfo> {
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > 1 << 20 {
        return None;
    }
    lnk_info_bytes(&std::fs::read(p).ok()?)
}

// ---------------------------------------------------------------------------
// 開機自動啟動的程式

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    UserRun,
    MachineRun,
    MachineRun32,
    UserFolder,
    CommonFolder,
    StoreApp,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::UserRun => "目前使用者",
            Source::MachineRun => "所有使用者",
            Source::MachineRun32 => "所有使用者（32 位元）",
            Source::UserFolder => "啟動資料夾",
            Source::CommonFolder => "啟動資料夾（所有使用者）",
            Source::StoreApp => "Microsoft Store 應用程式",
        }
    }
    /// 改它要不要系統管理員權限
    pub fn needs_admin(self) -> bool {
        matches!(self, Source::MachineRun | Source::MachineRun32 | Source::CommonFolder)
    }
    /// 寫進優化記錄的代號
    pub fn tag(self) -> &'static str {
        match self {
            Source::UserRun => "UserRun",
            Source::MachineRun => "MachineRun",
            Source::MachineRun32 => "MachineRun32",
            Source::UserFolder => "UserFolder",
            Source::CommonFolder => "CommonFolder",
            Source::StoreApp => "StoreApp",
        }
    }
    /// 登記的位置（給「問 AI」的提問用）
    pub fn location(self) -> &'static str {
        match self {
            Source::UserRun => r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            Source::MachineRun => r"HKLM\Software\Microsoft\Windows\CurrentVersion\Run",
            Source::MachineRun32 => r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
            Source::UserFolder => "目前使用者的「啟動」資料夾",
            Source::CommonFolder => "所有使用者共用的「啟動」資料夾",
            Source::StoreApp => "Microsoft Store 應用程式的啟動工作",
        }
    }
    fn approved(self) -> Option<(HKEY, String)> {
        Some(match self {
            Source::UserRun => (HKEY_CURRENT_USER, format!(r"{APPROVED}\Run")),
            Source::MachineRun => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\Run")),
            Source::MachineRun32 => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\Run32")),
            Source::UserFolder => (HKEY_CURRENT_USER, format!(r"{APPROVED}\StartupFolder")),
            Source::CommonFolder => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\StartupFolder")),
            Source::StoreApp => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct StartupItem {
    /// Run 機碼裡的值名稱、啟動資料夾裡的檔名，或 Store 應用程式的工作代碼
    pub name: String,
    /// 給人看的名稱
    pub display: String,
    /// 執行的指令（啟動資料夾是捷徑路徑；Store 應用程式是套件名稱）
    pub command: String,
    /// 實際執行的檔案（捷徑會解析出目標）
    pub exe: Option<PathBuf>,
    /// 確定找不到要執行的檔案（通常是解除安裝後的殘留）
    pub missing: bool,
    pub source: Source,
    pub enabled: bool,
    /// 受系統原則管理，不能在這裡改（Store 應用程式）
    pub policy: bool,
    /// Store 應用程式啟動工作在登錄檔的位置（HKCU 底下）
    pub store_key: Option<String>,
    /// 開機效能記錄裡，這個程式平均讓開機多花的毫秒數（有記錄才有）
    pub delay_ms: Option<u32>,
    /// 程式後面的參數（Run 機碼才有；給「問 AI」判斷要不要省略）
    pub args: String,
    /// 已經改成延後啟動：（DiskCleaner 建立的排程工作, 延遲秒數, 程式更新後指令變了、工作還是舊的）
    pub delayed: Option<(String, u32, bool)>,
}

/// StartupApproved 的值：第一個位元組是偶數（02、06）＝啟用，奇數（03、07）＝停用；沒有這個值＝啟用。
fn approved_enabled(source: Source, name: &str) -> bool {
    let Some((root, path)) = source.approved() else { return true };
    let Some(k) = open_key(root, &path, KEY_QUERY_VALUE) else { return true };
    match query(&k, name) {
        Some((t, d)) if t == REG_BINARY && !d.is_empty() => d[0] & 1 == 0,
        _ => true,
    }
}

fn pretty(name: &str) -> String {
    if name.starts_with("MicrosoftEdgeAutoLaunch_") {
        return "Microsoft Edge（開機時預先載入）".into();
    }
    let n = name.strip_prefix("electron.app.").unwrap_or(name);
    let n = n.strip_suffix(".lnk").unwrap_or(n);
    n.to_string()
}

fn run_entries(root: HKEY, path: &str, source: Source, out: &mut Vec<StartupItem>) {
    let Some(k) = open_key(root, path, KEY_READ) else { return };
    for (name, ty, data) in values(&k) {
        if name.is_empty() || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
            continue;
        }
        let command = from_wide_bytes(&data);
        if command.trim().is_empty() {
            continue;
        }
        let (exe, missing, args) = parse_command(&command);
        out.push(StartupItem {
            enabled: approved_enabled(source, &name),
            display: pretty(&name),
            name,
            command,
            exe,
            missing,
            source,
            policy: false,
            store_key: None,
            delay_ms: None,
            args,
            delayed: None,
        });
    }
}

fn folder_entries(dir: Option<PathBuf>, source: Source, out: &mut Vec<StartupItem>) {
    let Some(dir) = dir else { return };
    let mut names = Vec::new();
    let _ = win::list_dir(&dir, |e| {
        if !e.is_dir() {
            names.push(e.name_os().to_string_lossy().into_owned());
        }
    });
    for name in names {
        if name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let path = dir.join(&name);
        // 捷徑：解析出真正要執行的程式（對應開機記錄、判斷是不是殘留）
        let (exe, missing) = if name.to_ascii_lowercase().ends_with(".lnk") {
            match lnk_target(&path) {
                Some(t) => {
                    let m = not_found(&t);
                    (Some(t), m)
                }
                None => (Some(path.clone()), false),
            }
        } else {
            (Some(path.clone()), false)
        };
        out.push(StartupItem {
            enabled: approved_enabled(source, &name),
            display: pretty(&name),
            command: path.display().to_string(),
            exe,
            missing,
            name,
            source,
            policy: false,
            store_key: None,
            delay_ms: None,
            args: String::new(),
            delayed: None,
        });
    }
}

/// Store 應用程式的套件名稱與工作代碼 → 好讀的名稱
fn store_display(pkg: &str, task: &str) -> String {
    let name = pkg.split('_').next().unwrap_or(pkg);
    let short = name.rsplit_once('.').map_or(name, |(_, s)| s);
    let looks_code = short.len() >= 8 && short.chars().all(|c| c.is_ascii_hexdigit());
    if looks_code {
        let t = task.trim_end_matches("Id").trim_end_matches("Task");
        let t = t.trim_end_matches("Startup").trim_end_matches('.');
        if !t.is_empty() {
            return t.to_string();
        }
    }
    short.to_string()
}

fn store_entries(out: &mut Vec<StartupItem>) {
    let Some(root) = open_key(HKEY_CURRENT_USER, STORE, KEY_READ) else { return };
    for pkg in subkeys(&root) {
        let pkg_path = format!(r"{STORE}\{pkg}");
        let Some(pk) = open_key(HKEY_CURRENT_USER, &pkg_path, KEY_READ) else { continue };
        for task in subkeys(&pk) {
            let task_path = format!(r"{pkg_path}\{task}");
            let Some(tk) = open_key(HKEY_CURRENT_USER, &task_path, KEY_QUERY_VALUE) else { continue };
            // 0＝停用、1＝使用者停用、2＝啟用、3＝原則停用、4＝原則啟用
            let Some(state) = dword(query(&tk, "State")) else { continue };
            if state > 4 {
                continue;
            }
            out.push(StartupItem {
                display: store_display(&pkg, &task),
                command: pkg.clone(),
                exe: None,
                missing: false,
                source: Source::StoreApp,
                enabled: state == 2 || state == 4,
                policy: state >= 3,
                store_key: Some(task_path),
                delay_ms: None,
                args: String::new(),
                delayed: None,
                name: task,
            });
        }
    }
}

pub fn startup_items() -> Vec<StartupItem> {
    use windows_sys::Win32::UI::Shell::{FOLDERID_CommonStartup, FOLDERID_Startup};
    let mut out = Vec::new();
    run_entries(HKEY_CURRENT_USER, RUN, Source::UserRun, &mut out);
    run_entries(HKEY_LOCAL_MACHINE, RUN, Source::MachineRun, &mut out);
    run_entries(HKEY_LOCAL_MACHINE, RUN32, Source::MachineRun32, &mut out);
    // 啟動資料夾可能被重新導向，用 Known Folder 取得實際位置
    folder_entries(win::known_folder(&FOLDERID_Startup), Source::UserFolder, &mut out);
    folder_entries(win::known_folder(&FOLDERID_CommonStartup), Source::CommonFolder, &mut out);
    store_entries(&mut out);
    out
}

fn same_item(a: &StartupItem, b: &StartupItem) -> bool {
    a.source == b.source && a.name == b.name && a.command == b.command && a.store_key == b.store_key
}

fn write_enabled(item: &StartupItem, on: bool) -> Result<(), String> {
    if item.policy {
        return Err("這個項目由系統原則管理，不能在這裡改".into());
    }
    if let Some(key) = &item.store_key {
        // 和工作管理員相同：停用寫 1（使用者停用），啟用寫 2
        let v: u32 = if on { 2 } else { 1 };
        return write_value(HKEY_CURRENT_USER, key, "State", REG_DWORD, &v.to_le_bytes());
    }
    let Some((root, path)) = item.source.approved() else { return Err("不支援的來源".into()) };
    let mut data = [0u8; 12];
    if on {
        data[0] = 0x02;
    } else {
        data[0] = 0x03;
        data[4..12].copy_from_slice(&win::now_filetime().to_le_bytes());
    }
    write_value(root, &path, &item.name, REG_BINARY, &data)
}

/// 啟用或停用一組開機自動啟動的程式。寫入前重新讀取一次清單，確認每一項都還在原來的位置、
/// 名稱與指令一字不差，並且用「重新讀到的那一筆」判斷能不能改（例如 Store 工作剛被原則鎖定）；
/// 不會刪除或改動 Run 值、捷徑或 Store 套件本身。回傳（成功數, 第一個錯誤）。
pub fn set_enabled_many(items: &[StartupItem], on: bool) -> (usize, Option<String>) {
    let now = startup_items();
    let mut ok = 0;
    for it in items {
        let Some(fresh) = now.iter().find(|n| same_item(n, it)) else {
            return (ok, Some(format!("{} 已經不在了，請重新整理", it.display)));
        };
        match write_enabled(fresh, on) {
            Ok(()) => ok += 1,
            Err(e) => return (ok, Some(e)),
        }
    }
    (ok, None)
}

// ---------------------------------------------------------------------------
// 開機效能記錄（Microsoft-Windows-Diagnostics-Performance/Operational）

#[derive(Clone, Debug)]
pub struct BootRecord {
    pub when: u64,
    pub total_ms: u32,
    /// 到桌面出現
    pub main_ms: u32,
    /// 桌面出現之後，系統還在忙的時間
    pub post_ms: u32,
    pub degraded: bool,
}

#[derive(Clone, Debug)]
pub struct SlowItem {
    pub kind: &'static str,
    pub name: String,
    /// 原始名稱（程式是執行檔名）
    pub raw: String,
    /// 完整路徑（有的話；用來對應開機自動啟動的項目）
    pub path: String,
    pub count: u32,
    /// 比平常多花的時間（DegradationTime）總和
    pub degr_ms: u64,
    /// 啟動花的總時間（TotalTime）總和
    pub total_ms: u64,
    pub max_degr_ms: u32,
    /// 最早、最近一次記錄的時間（FILETIME；0＝不明）
    pub first: u64,
    pub last: u64,
    /// 對應到的開機自動啟動項目（`BootData::items` 的索引；只有「程式」類會對）
    pub startup: Vec<usize>,
}

impl SlowItem {
    pub fn avg_degr(&self) -> u32 {
        (self.degr_ms / self.count.max(1) as u64) as u32
    }
    pub fn avg_total(&self) -> u32 {
        (self.total_ms / self.count.max(1) as u64) as u32
    }
}

#[derive(Clone, Debug)]
pub enum BootLog {
    /// note：讀取中途有錯，結果可能不完整
    Ok { boots: Vec<BootRecord>, slow: Vec<SlowItem>, note: Option<String> },
    NeedAdmin,
    Unavailable(String),
}

/// XML 裡 <Data Name='x'>值</Data> 的值
fn data_field(xml: &str, name: &str) -> Option<String> {
    for q in ['\'', '"'] {
        let tag = format!("<Data Name={q}{name}{q}>");
        if let Some(i) = xml.find(&tag) {
            let rest = &xml[i + tag.len()..];
            let end = rest.find("</Data>")?;
            return Some(unescape(&rest[..end]));
        }
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn event_id(xml: &str) -> Option<u32> {
    let i = xml.find("<EventID")?;
    let rest = &xml[i..];
    let s = rest.find('>')? + 1;
    let e = rest.find("</EventID>")?;
    rest.get(s..e)?.trim().parse().ok()
}

/// TimeCreated SystemTime='2026-11-15T01:23:45.678Z' → FILETIME（UTC）
fn time_created(xml: &str) -> Option<u64> {
    let i = xml.find("SystemTime=")?;
    let s = xml.get(i + 12..)?;
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse().ok() };
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // 公曆日期 → 1970 起算的天數
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    u64::try_from((secs + 11_644_473_600) * 10_000_000).ok()
}

/// 回傳（事件 XML, 是否完整讀完）。查詢本身失敗回傳錯誤碼。
fn query_events(query: &str, max: usize) -> Result<(Vec<String>, bool), u32> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::EventLog::{EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection, EvtRender, EvtRenderEventXml};
    const NO_MORE_ITEMS: u32 = 259;
    let path = win::wide("Microsoft-Windows-Diagnostics-Performance/Operational");
    let q = win::wide(query);
    let h = unsafe { EvtQuery(0, path.as_ptr(), q.as_ptr(), EvtQueryChannelPath | EvtQueryReverseDirection) };
    if h == 0 {
        return Err(unsafe { GetLastError() });
    }
    let mut out = Vec::new();
    let mut complete = true;
    let mut buf: Vec<u16> = vec![0; 16384];
    while out.len() < max {
        let mut evs = [0isize; 32];
        let mut got = 0u32;
        if unsafe { EvtNext(h, evs.len() as u32, evs.as_mut_ptr(), 5000, 0, &mut got) } == 0 {
            // 只有「沒有更多了」才算正常讀完
            complete = unsafe { GetLastError() } == NO_MORE_ITEMS;
            break;
        }
        // 這一批每個事件 handle 都要關，就算已經拿夠了
        for &e in &evs[..got as usize] {
            if out.len() < max {
                let mut used = 0u32;
                let mut props = 0u32;
                let mut ok = unsafe { EvtRender(0, e, EvtRenderEventXml, (buf.len() * 2) as u32, buf.as_mut_ptr() as *mut _, &mut used, &mut props) };
                if ok == 0 && used as usize > buf.len() * 2 && used <= 4 << 20 {
                    buf.resize(used as usize / 2 + 1, 0);
                    ok = unsafe { EvtRender(0, e, EvtRenderEventXml, (buf.len() * 2) as u32, buf.as_mut_ptr() as *mut _, &mut used, &mut props) };
                }
                if ok != 0 {
                    let n = (used as usize / 2).min(buf.len());
                    let end = buf[..n].iter().position(|&c| c == 0).unwrap_or(n);
                    out.push(String::from_utf16_lossy(&buf[..end]));
                } else {
                    complete = false;
                }
            }
            unsafe { EvtClose(e) };
        }
    }
    unsafe { EvtClose(h) };
    Ok((out, complete))
}

fn num(x: &str, n: &str) -> u32 {
    data_field(x, n).and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0).min(u32::MAX as u64) as u32
}

pub fn boot_log() -> BootLog {
    let (boots_xml, boots_complete) = match query_events("*[System[(EventID=100)]]", 30) {
        Ok(v) => v,
        Err(5) => return BootLog::NeedAdmin,
        Err(15007) => return BootLog::Unavailable("這台電腦沒有開機效能記錄".into()),
        Err(e) => return BootLog::Unavailable(format!("無法讀取開機記錄（錯誤 {e}）")),
    };
    let mut boots: Vec<BootRecord> = boots_xml
        .iter()
        .filter_map(|x| {
            let total = num(x, "BootTime");
            (total > 0).then(|| BootRecord {
                when: time_created(x).unwrap_or(0),
                total_ms: total,
                main_ms: num(x, "MainPathBootTime"),
                post_ms: num(x, "BootPostBootTime"),
                degraded: data_field(x, "BootIsDegradation").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
            })
        })
        .collect();
    boots.sort_by_key(|b| b.when);

    let mut note = (!boots_complete).then(|| "開機記錄沒有完整讀完，顯示的可能不是全部".to_string());
    let mut slow: Vec<SlowItem> = Vec::new();
    // 記錄檔預設最大 1 MB，通常只有幾百到一兩千筆；讀到上限就說明只讀了最近幾筆
    const SLOW_MAX: usize = 3000;
    match query_events("*[System[(EventID>=101 and EventID<=110)]]", SLOW_MAX) {
        Ok((list, complete)) => {
            if !complete {
                note = Some("拖慢開機的記錄沒有完整讀完，顯示的可能不是全部".into());
            } else if list.len() >= SLOW_MAX {
                note = Some(format!("拖慢開機的記錄很多，只讀了最近 {SLOW_MAX} 筆"));
            }
            for x in &list {
                let kind = match event_id(x).unwrap_or(0) {
                    101 => "程式",
                    102 => "驅動程式",
                    103 => "服務",
                    106 => "背景最佳化",
                    107 | 108 => "群組原則",
                    109 => "裝置",
                    110 => "工作階段管理",
                    _ => "其他",
                };
                let degr = num(x, "DegradationTime");
                let total = num(x, "TotalTime");
                if degr == 0 && total == 0 {
                    continue;
                }
                let raw = data_field(x, "Name").unwrap_or_default();
                let path = data_field(x, "Path").unwrap_or_default();
                let friendly = data_field(x, "FriendlyName").filter(|s| !s.trim().is_empty()).unwrap_or_else(|| raw.clone());
                let name = if friendly.trim().is_empty() { kind.to_string() } else { friendly };
                // 以種類＋完整路徑分組（沒有路徑才用名稱），同名但不同的程式不會被合併
                let key = |s: &SlowItem| if !s.path.is_empty() { s.path.to_lowercase() } else if !s.raw.is_empty() { s.raw.to_lowercase() } else { s.name.to_lowercase() };
                let when = time_created(x).unwrap_or(0);
                let me = SlowItem {
                    kind,
                    name,
                    raw,
                    path,
                    count: 1,
                    degr_ms: degr as u64,
                    total_ms: total as u64,
                    max_degr_ms: degr,
                    first: when,
                    last: when,
                    startup: Vec::new(),
                };
                let k = key(&me);
                match slow.iter_mut().find(|s| s.kind == kind && key(s) == k) {
                    Some(s) => {
                        s.count += 1;
                        s.degr_ms += degr as u64;
                        s.total_ms += total as u64;
                        s.max_degr_ms = s.max_degr_ms.max(degr);
                        if when != 0 {
                            s.first = if s.first == 0 { when } else { s.first.min(when) };
                            s.last = s.last.max(when);
                        }
                    }
                    None => slow.push(me),
                }
            }
        }
        Err(e) => note = Some(format!("讀不到拖慢開機的記錄（錯誤 {e}）")),
    }
    slow.sort_by(|a, b| b.degr_ms.cmp(&a.degr_ms).then(b.total_ms.cmp(&a.total_ms)));
    BootLog::Ok { boots, slow, note }
}

// ---------------------------------------------------------------------------
// 系統設定檢查

#[derive(Clone, Debug)]
pub struct SysCheck {
    /// 快速啟動的設定（HiberbootEnabled）；None＝讀不到
    pub fast_startup: Option<bool>,
    /// 休眠有沒有開（快速啟動需要它）；None＝無法確定
    pub hibernate: Option<bool>,
    pub sys_drive: String,
    pub sys_free: u64,
    pub sys_total: u64,
    /// 系統磁碟沒有回報尋軌延遲（通常是 SSD）；None＝查不到
    pub sys_ssd: Option<bool>,
    /// 例如「Windows 10 Pro 22H2（組建 19045）」，給「問 AI」的提問用
    pub os: String,
}

/// Windows 版本名稱。Windows 11 的 ProductName 仍然寫 Windows 10，要看組建編號。
fn os_name() -> String {
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let Some(k) = open_key(HKEY_LOCAL_MACHINE, NT, KEY_QUERY_VALUE) else { return "Windows".into() };
    let s = |n: &str| query(&k, n).filter(|(t, _)| *t == REG_SZ).map(|(_, d)| from_wide_bytes(&d).trim().to_string()).unwrap_or_default();
    let build: u32 = s("CurrentBuildNumber").parse().unwrap_or(0);
    let mut product = s("ProductName");
    if product.is_empty() {
        product = "Windows".into();
    }
    if build >= 22000 {
        product = product.replacen("Windows 10", "Windows 11", 1);
    }
    let ver = s("DisplayVersion");
    match (ver.is_empty(), build) {
        (_, 0) => product,
        (true, b) => format!("{product}（組建 {b}）"),
        (false, b) => format!("{product} {ver}（組建 {b}）"),
    }
}

fn seek_penalty(letter: char) -> Option<bool> {
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY, StorageDeviceSeekPenaltyProperty,
    };
    let p = win::wide(format!(r"\\.\{letter}:"));
    let h = unsafe { CreateFileW(p.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let q = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceSeekPenaltyProperty, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
    let mut d: DEVICE_SEEK_PENALTY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &q as *const _ as *const _,
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            &mut d as *mut _ as *mut _,
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(h) };
    (ok != 0 && ret as usize >= std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>()).then_some(d.IncursSeekPenalty)
}

pub fn sys_check() -> SysCheck {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let letter = drive.chars().next().filter(|c| c.is_ascii_alphabetic()).unwrap_or('C').to_ascii_uppercase();
    let info = win::drives().into_iter().find(|d| d.root.starts_with(letter));
    // 休眠：先看 HibernateEnabled；沒有這個值就看休眠檔在不在（用列舉根目錄判斷：休眠檔被系統獨占開著，
    // 直接查屬性會遇到共用違規），最後才看預設值 HibernateEnabledDefault
    let hiberfil = || {
        let mut found = false;
        let _ = win::list_dir(Path::new(&format!(r"{letter}:\")), |e| {
            if e.name_os().to_string_lossy().eq_ignore_ascii_case("hiberfil.sys") {
                found = true;
            }
        });
        found.then_some(true)
    };
    let hibernate = read_dword(HKEY_LOCAL_MACHINE, HIBERNATE, "HibernateEnabled")
        .map(|v| v != 0)
        .or_else(hiberfil)
        .or_else(|| read_dword(HKEY_LOCAL_MACHINE, HIBERNATE, "HibernateEnabledDefault").map(|v| v != 0));
    SysCheck {
        fast_startup: read_dword(HKEY_LOCAL_MACHINE, POWER, "HiberbootEnabled").map(|v| v != 0),
        hibernate,
        sys_drive: format!("{letter}:"),
        sys_free: info.as_ref().map_or(0, |d| d.free),
        sys_total: info.as_ref().map_or(0, |d| d.total),
        sys_ssd: seek_penalty(letter).map(|p| !p),
        os: os_name(),
    }
}

fn powercfg(args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let exe = PathBuf::from(windir).join(r"System32\powercfg.exe");
    let out = std::process::Command::new(exe)
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("無法執行 powercfg：{e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let text = format!("{}{}", crate::cleaner::decode_oem(&out.stdout), crate::cleaner::decode_oem(&out.stderr));
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Err(format!("powercfg {} 失敗（結束代碼 {}）：{}", args.join(" "), out.status.code().unwrap_or(-1), text.chars().take(200).collect::<String>()))
}

/// 開啟或關閉快速啟動（需要系統管理員）。開啟時先確保休眠可用；休眠原本關著，就改用較小的休眠檔
/// （reduced：只給快速啟動用，約記憶體的 20%）。關閉只改 HiberbootEnabled，不動休眠。
pub fn set_fast_startup(on: bool) -> Result<(), String> {
    if on {
        match sys_check().hibernate {
            Some(true) => {}
            Some(false) => {
                powercfg(&["/h", "on"])?;
                powercfg(&["/h", "/type", "reduced"])?;
            }
            // 判斷不出休眠開沒開：只確保它開著，不動休眠檔類型（原本可能是完整休眠）
            None => powercfg(&["/h", "on"])?,
        }
    }
    write_value(HKEY_LOCAL_MACHINE, POWER, "HiberbootEnabled", REG_DWORD, &(on as u32).to_le_bytes())
}

// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct BootData {
    pub log: BootLog,
    pub items: Vec<StartupItem>,
    pub sys: SysCheck,
    /// 優化記錄：目前和原本不一樣的地方。第二個值＝延後啟動的排程工作已經不在了（被別人刪掉）
    pub changes: Vec<(bootopt::Change, bool)>,
    /// 開機／登入時執行的第三方排程工作（要系統管理員才讀得到）
    pub tasks: bootopt::TaskList,
    /// 設成自動啟動的第三方服務
    pub services: Result<Vec<bootopt::Service>, String>,
}

/// 在背景執行的動作
#[derive(Clone, Debug)]
pub enum Action {
    Reload,
    /// 一組啟動項目全部設成啟用／停用（延後中的會一併取消延後）
    Toggle(Vec<StartupItem>, bool),
    FastStartup(bool),
    /// 一組啟動項目改成登入後延遲 N 秒啟動（已經延後的就是改延遲時間）
    Delay(Vec<StartupItem>, u32),
    /// 取消延後：true＝恢復成開機就啟動，false＝改成停用
    Undelay(Vec<StartupItem>, bool),
    /// 一鍵優化：每一組 None＝停用、Some(秒)＝延後
    Optimize(Vec<(Vec<StartupItem>, Option<u32>)>),
    /// 還原優化記錄裡的改動
    Restore(Vec<bootopt::Change>),
    /// 停用（false）或恢復（true）第三方排程工作
    TaskEnabled(bootopt::SchedTask, bool),
    /// 服務改成延遲開始（true）或改回自動（false）：（服務名稱, 顯示名稱, 延遲）
    ServiceDelayed(String, String, bool),
}

/// 延遲秒數的說法，例如 150 → 2 分 30 秒
pub fn delay_text(secs: u32) -> String {
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s} 秒"),
        (m, 0) => format!("{m} 分鐘"),
        (m, s) => format!("{m} 分 {s} 秒"),
    }
}

/// 執行動作，回傳要顯示的訊息（訊息, 是不是錯誤）。之後呼叫端會重新 load()。
pub fn run(action: Action) -> Option<(String, bool)> {
    let first = |items: &[StartupItem]| items.first().map(|i| i.display.clone()).unwrap_or_default();
    match action {
        Action::Reload => None,
        Action::Toggle(items, on) => {
            let name = first(&items);
            if items.iter().any(|i| i.delayed.is_some()) {
                return Some(match bootopt::undelay(&items, on) {
                    Ok(()) => (format!("已取消延後並{} {name}，下次登入生效。", if on { "改回開機就啟動" } else { "停用" }), false),
                    Err(e) => (format!("{name}：{e}"), true),
                });
            }
            let (ok, err) = bootopt::toggle(&items, on);
            Some(match err {
                None => (format!("已{} {name}，下次登入生效。", if on { "啟用" } else { "停用" }), false),
                Some(e) if ok > 0 => (format!("{name}：{} 筆中改了 {ok} 筆，其餘失敗：{e}", items.len()), true),
                Some(e) => (format!("{name}：{e}"), true),
            })
        }
        Action::FastStartup(on) => Some(match set_fast_startup(on) {
            Ok(()) => (format!("已{}快速啟動，下次關機再開機時生效。", if on { "開啟" } else { "關閉" }), false),
            Err(e) => (e, true),
        }),
        Action::Delay(items, secs) => {
            let name = first(&items);
            Some(match bootopt::delay(&items, secs) {
                Ok(()) => (format!("{name} 改成登入後 {} 才啟動，下次登入生效。", delay_text(secs)), false),
                Err(e) => (format!("{name}：沒有改成延後（{e}）"), true),
            })
        }
        Action::Undelay(items, enable) => {
            let name = first(&items);
            Some(match bootopt::undelay(&items, enable) {
                Ok(()) => (format!("已取消 {name} 的延後，{}。", if enable { "改回開機就啟動" } else { "改成停用" }), false),
                Err(e) => (format!("{name}：{e}"), true),
            })
        }
        Action::Optimize(plan) => {
            let (mut off, mut later, mut errs) = (0, 0, Vec::new());
            for (items, delay) in &plan {
                let r = match delay {
                    Some(s) => bootopt::delay(items, *s),
                    None => match bootopt::toggle(items, false) {
                        (_, Some(e)) => Err(e),
                        _ => Ok(()),
                    },
                };
                match r {
                    Ok(()) if delay.is_some() => later += 1,
                    Ok(()) => off += 1,
                    Err(e) => errs.push(format!("{}：{e}", first(items))),
                }
            }
            let mut m = format!("一鍵優化完成：停用 {off} 項、延後 {later} 項，下次登入生效。可以在「優化記錄」還原。");
            if !errs.is_empty() {
                m += &format!(" 失敗 {} 項：{}", errs.len(), errs.join("；"));
            }
            Some((m, !errs.is_empty()))
        }
        Action::Restore(list) => {
            let now = startup_items();
            let (mut errs, mut notes) = (Vec::new(), Vec::new());
            for c in &list {
                match bootopt::restore(c, &now) {
                    Ok(Some(n)) => notes.push(n),
                    Ok(None) => {}
                    Err(e) => errs.push(format!("{}：{e}", c.label)),
                }
            }
            let done = list.len() - errs.len() - notes.len();
            let mut m = format!("已還原 {done} 項，下次登入生效。");
            if !notes.is_empty() {
                m += &format!(" {}。", notes.join("；"));
            }
            if !errs.is_empty() {
                m += &format!(" 失敗 {} 項：{}", errs.len(), errs.join("；"));
            }
            Some((m, !errs.is_empty()))
        }
        Action::TaskEnabled(t, on) => Some(match bootopt::set_task_enabled(&t, on) {
            Ok(()) => (format!("已{}排程工作 {}。", if on { "恢復" } else { "停用" }, t.path), false),
            Err(e) => (format!("{}：{e}", t.path), true),
        }),
        Action::ServiceDelayed(name, label, delayed) => Some(match bootopt::set_service_delayed(&name, &label, delayed) {
            Ok(()) => (format!("服務 {label} 已改成「{}」，下次開機生效。", if delayed { "自動（延遲開始）" } else { "自動" }), false),
            Err(e) => (format!("{label}：{e}"), true),
        }),
    }
}

/// 會被很多不同項目共用的宿主程式（指令列決定實際做什麼），不能用路徑對應到單一個開機項目
fn is_host(p: &Path) -> bool {
    const HOSTS: &[&str] = &[
        "cmd.exe", "powershell.exe", "pwsh.exe", "wscript.exe", "cscript.exe", "rundll32.exe", "mshta.exe", "regsvr32.exe", "msiexec.exe",
        "explorer.exe", "conhost.exe", "dllhost.exe", "svchost.exe", "java.exe", "javaw.exe", "node.exe", "py.exe", "pyw.exe",
    ];
    let f = p.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
    HOSTS.contains(&f.as_str()) || (f.starts_with("python") && f.ends_with(".exe"))
}

/// 讀取全部（在背景執行緒呼叫）
pub fn load() -> BootData {
    let mut log = boot_log();
    let mut items = startup_items();
    // 開機記錄裡「程式」類的項目，只用完整路徑對到開機自動啟動的程式（兩邊都先換成實際的長路徑）。
    // 記錄沒有路徑時不比檔名（不同資料夾的同名程式會被錯連）；cmd、powershell、rundll32 之類的宿主程式
    // 會被很多項目共用，也不對應。兩邊都記下對應關係：啟動清單顯示「多花幾秒」，拖慢清單可以直接停用。
    if let BootLog::Ok { slow, .. } = &mut log {
        let norm = |p: &Path| win::final_path(p).unwrap_or_else(|| p.to_path_buf()).to_string_lossy().to_lowercase();
        let keys: Vec<Option<String>> = items.iter().map(|it| it.exe.as_deref().filter(|e| e.is_absolute() && !is_host(e)).map(norm)).collect();
        for s in slow.iter_mut().filter(|s| s.kind == "程式" && !s.path.is_empty()) {
            let p = Path::new(&s.path);
            if !p.is_absolute() || is_host(p) {
                continue;
            }
            let path = norm(p);
            for (i, k) in keys.iter().enumerate() {
                if k.as_deref() == Some(path.as_str()) {
                    s.startup.push(i);
                    if s.avg_degr() > 0 {
                        items[i].delay_ms = Some(items[i].delay_ms.unwrap_or(0).max(s.avg_degr()));
                    }
                }
            }
        }
    }
    // 優化記錄：延後中的項目標上它的工作與延遲；工作被別人刪掉的，記錄裡標出來（還原時只要重新啟用）
    let mut changes = Vec::new();
    // （每一筆延後要問一次工作排程器：最多查 100 筆，避免記錄異常時讀很久）
    for (n, c) in bootopt::log_load_from(&bootopt::log_path()).into_iter().enumerate() {
        let gone = c.kind == bootopt::ChangeKind::Delayed && (n >= 100 || !bootopt::task_exists(&c.task));
        if c.kind == bootopt::ChangeKind::Delayed && !gone {
            // 用來源＋名稱對應；程式更新後指令變了，標出來（延後的工作還是舊指令，要重建）
            for it in items.iter_mut().filter(|it| c.is_item(it)) {
                it.delayed = Some((c.task.clone(), c.delay, it.command != c.command));
            }
        }
        changes.push((c, gone));
    }
    // 別人的排程工作定義只有系統管理員讀得到；一般權限讀到的資料夾是空的，不能當成「沒有」
    let tasks = if win::is_elevated() { bootopt::sched_tasks() } else { bootopt::TaskList::NeedAdmin };
    BootData { log, items, sys: sys_check(), changes, tasks, services: bootopt::services() }
}

/// 示範資料（`--demo-boot`，截圖、看版面用）：全部是假的，不讀也不改系統。
pub fn demo() -> BootData {
    let now = win::now_filetime();
    let day = 86_400 * 10_000_000u64;
    let totals = [92.0, 90.0, 108.0, 99.0, 95.0, 88.0, 104.0, 101.0, 106.0, 106.0, 94.0, 90.0, 100.0, 107.0, 88.5];
    let boots = totals
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            let total = (t * 1000.0) as u32;
            let main = if i + 1 == totals.len() { 56_600 } else { 24_000 + (i as u32 % 4) * 2_500 };
            BootRecord { when: now - (200 - i as u64 * 5) * day, total_ms: total, main_ms: main, post_ms: total - main, degraded: i + 1 == totals.len() }
        })
        .collect();
    let item = |display: &str, command: &str, exe: &str, source: Source, missing: bool| StartupItem {
        name: display.into(),
        display: display.into(),
        command: command.into(),
        exe: Some(PathBuf::from(exe)),
        missing,
        source,
        enabled: true,
        policy: false,
        store_key: None,
        delay_ms: None,
        args: String::new(),
        delayed: None,
    };
    let steam = r"C:\Program Files (x86)\Steam\steam.exe";
    let mut items = vec![
        item("Steam", &format!("\"{steam}\" -silent"), steam, Source::UserRun, false),
        item("OneDrive", r"C:\Users\demo\AppData\Local\Microsoft\OneDrive\OneDrive.exe /background", r"C:\Users\demo\AppData\Local\Microsoft\OneDrive\OneDrive.exe", Source::UserRun, false),
        item("SecurityHealth", r"%windir%\system32\SecurityHealthSystray.exe", r"C:\Windows\system32\SecurityHealthSystray.exe", Source::MachineRun, false),
        item("Helper", r"D:\Tools\helper\helper.exe --tray", r"D:\Tools\helper\helper.exe", Source::UserRun, false),
        item("CMNotify", r"D:\Program Files (x86)\CMoney\CMNotify.exe", r"D:\Program Files (x86)\CMoney\CMNotify.exe", Source::UserRun, true),
        item("Helper Sync", r"D:\Tools\helper\helper.exe --sync", r"D:\Tools\helper\helper.exe", Source::UserRun, false),
    ];
    items[0].delay_ms = Some(30_000);
    let slow_item = |kind: &'static str, name: &str, raw: &str, path: &str, count: u32, avg: u32, startup: Vec<usize>| SlowItem {
        kind,
        name: name.into(),
        raw: raw.into(),
        path: path.into(),
        count,
        degr_ms: avg as u64 * count as u64,
        total_ms: (avg as u64 + 5_000) * count as u64,
        max_degr_ms: avg + avg / 3,
        first: now - 200 * day,
        last: now - 130 * day,
        startup,
    };
    let mut slow = vec![
        slow_item("程式", "Windsurf", "Windsurf.exe", r"C:\Users\demo\AppData\Local\Programs\Windsurf\Windsurf.exe", 6, 127_100, vec![]),
        slow_item("程式", "McAfee Framework Host Service", "mc-fw-host.exe", r"C:\Program Files\McAfee\Framework\mc-fw-host.exe", 4, 36_400, vec![]),
        slow_item("程式", "McAfee Framework Host Service", "mc-sec-host.exe", r"C:\Program Files\McAfee\Security\mc-sec-host.exe", 5, 27_500, vec![]),
        slow_item("程式", "VirtualBox Headless Frontend", "VBoxHeadless.exe", r"C:\Program Files\Oracle\VirtualBox\VBoxHeadless.exe", 2, 66_500, vec![]),
        slow_item("程式", "vmmem", "vmmem", "", 3, 38_700, vec![]),
        slow_item("程式", "Steam", "steam.exe", steam, 3, 30_000, vec![0]),
        slow_item("程式", "Codeium", "language_server_windows_x64.exe", r"C:\Users\demo\.codeium\language_server_windows_x64.exe", 4, 29_000, vec![]),
        slow_item("程式", "crosvm.exe", "crosvm.exe", r"C:\Program Files\Google\Play Games\current\emulator\crosvm.exe", 1, 98_300, vec![]),
        slow_item("程式", "Windows Services 的主機處理程序", "svchost.exe", r"C:\Windows\System32\svchost.exe", 17, 4_700, vec![]),
        slow_item("服務", "Demo Sync Service", "demosync.exe", r"C:\Program Files\Demo\demosync.exe", 2, 8_000, vec![]),
        slow_item("程式", "Helper", "helper.exe", r"D:\Tools\helper\helper.exe", 2, 12_000, vec![3, 5]),
        slow_item("驅動程式", "Realtek PCIe GbE Family Controller", "rt640x64.sys", "", 3, 3_000, vec![]),
    ];
    slow.sort_by(|a, b| b.degr_ms.cmp(&a.degr_ms));
    let sys = SysCheck {
        fast_startup: Some(true),
        hibernate: Some(true),
        sys_drive: "C:".into(),
        sys_free: 325 << 30,
        sys_total: 1024 << 30,
        sys_ssd: Some(true),
        os: "Windows 10 Pro 22H2（組建 19045）".into(),
    };
    // OneDrive 示範成已延後；優化記錄裡有它和一個停用的項目、一個停用的排程工作
    items[1].enabled = false;
    items[1].delayed = Some((r"\DiskCleaner\延後啟動 OneDrive (0d1e2f3a)".into(), 120, false));
    let ch = |kind, label: &str, target: &str, name: &str, command: &str, task: &str, delay| bootopt::Change {
        kind,
        when: now - day,
        label: label.into(),
        target: target.into(),
        name: name.into(),
        command: command.into(),
        store_key: String::new(),
        task: task.into(),
        delay,
    };
    let changes = vec![
        (ch(bootopt::ChangeKind::Delayed, "OneDrive", "UserRun", "OneDrive", &items[1].command, r"\DiskCleaner\延後啟動 OneDrive (0d1e2f3a)", 120), false),
        (ch(bootopt::ChangeKind::Disabled, "Edge 開機預先載入", "UserRun", "MicrosoftEdgeAutoLaunch_0", "msedge.exe", "", 0), false),
        (ch(bootopt::ChangeKind::TaskDisabled, "AdobeAAMUpdater-1.0", r"\AdobeAAMUpdater-1.0", "", "", "", 0), false),
    ];
    let task = |path: &str, enabled, logon, boot, command: &str| bootopt::SchedTask {
        path: path.into(),
        enabled,
        logon,
        boot,
        command: command.into(),
        args: String::new(),
        author: String::new(),
        actions: 1,
    };
    let tasks = bootopt::TaskList::Ok(vec![
        task(r"\AdobeAAMUpdater-1.0", false, true, false, r"C:\Program Files (x86)\Common Files\Adobe\OOBE\PDApp\UWA\UpdaterStartupUtility.exe"),
        task(r"\GoogleUpdateTaskMachineCore", true, true, false, r"C:\Program Files (x86)\Google\Update\GoogleUpdate.exe"),
        task(r"\Demo\Backup at logon", true, true, true, r"C:\Program Files\Demo Backup\backup.exe"),
    ]);
    let svc = |name: &str, display: &str, exe: &str, delayed, running| bootopt::Service {
        name: name.into(),
        display: display.into(),
        exe: exe.into(),
        company: String::new(),
        delayed,
        running,
        group: String::new(),
    };
    let services = Ok(vec![
        svc("DemoSync", "Demo Sync Service", r"C:\Program Files\Demo\demosync.exe", false, true),
        svc("McAfeeFramework", "McAfee Framework Host", r"C:\Program Files\McAfee\Framework\mc-fw-host.exe", false, true),
        svc("gupdate", "Google Update Service", r"C:\Program Files (x86)\Google\Update\GoogleUpdate.exe", true, false),
    ]);
    BootData { log: BootLog::Ok { boots, slow, note: None }, items, sys, changes, tasks, services }
}

/// 自我測試用：在目前使用者的 Run 機碼放一個名稱唯一的測試值。remove() 只刪自己建立的值（含 StartupApproved），
/// 並確認真的刪掉了。
pub struct TestRunValue {
    name: String,
    data: Vec<u8>,
    done: bool,
}

impl TestRunValue {
    pub fn create(name: &str, command: &str) -> Option<TestRunValue> {
        let approved = format!(r"{APPROVED}\Run");
        let exists = |path: &str| open_key(HKEY_CURRENT_USER, path, KEY_QUERY_VALUE).and_then(|k| query(&k, name)).is_some();
        if exists(RUN) || exists(&approved) {
            return None;
        }
        let data: Vec<u8> = command.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect();
        write_value(HKEY_CURRENT_USER, RUN, name, REG_SZ, &data).ok()?;
        Some(TestRunValue { name: name.into(), data, done: false })
    }

    fn cleanup(&mut self) -> bool {
        self.done = true;
        let approved = format!(r"{APPROVED}\Run");
        let get = |path: &str| open_key(HKEY_CURRENT_USER, path, KEY_QUERY_VALUE).and_then(|k| query(&k, &self.name));
        // Run 值還是我們寫的內容才刪（被別人改掉就不碰）
        if get(RUN).is_some_and(|(_, d)| d != self.data) {
            return false;
        }
        let del = |path: &str| {
            let p = win::wide(path);
            let n = win::wide(&self.name);
            get(path).is_none() || unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, p.as_ptr(), n.as_ptr()) == 0 }
        };
        del(RUN) && del(&approved) && get(RUN).is_none() && get(&approved).is_none()
    }

    pub fn remove(mut self) -> bool {
        self.cleanup()
    }
}

impl Drop for TestRunValue {
    /// 測試中途失敗提早返回時也會清掉
    fn drop(&mut self) {
        if !self.done {
            self.cleanup();
        }
    }
}

// ---------------------------------------------------------------------------
// 自我測試

pub fn selftest() -> Vec<(&'static str, bool)> {
    let xml = "<Event><System><EventID>100</EventID><TimeCreated SystemTime='2026-11-15T01:02:03.4567890Z'/></System>\
               <EventData><Data Name='BootTime'>42345</Data><Data Name='MainPathBootTime'>28000</Data>\
               <Data Name='FriendlyName'>A &amp; B</Data></EventData></Event>";
    // 2026-11-15 01:02:03 UTC 的 FILETIME
    let expect = (1_794_704_523u64 + 11_644_473_600) * 10_000_000;
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let cmd_exe = PathBuf::from(&sysroot).join(r"System32\cmd.exe");
    // 最小的 .lnk：只有 LinkInfo（ANSI 路徑）
    let mut lnk = vec![0u8; 0x4C];
    lnk[0] = 0x4C;
    lnk[4..20].copy_from_slice(&[0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
    lnk[0x14] = 0x02; // HasLinkInfo
    let base = b"C:\\Apps\\Tool\\tool.exe\0";
    let li_size = 0x1C + base.len() + 1;
    let mut li = vec![0u8; li_size];
    li[0..4].copy_from_slice(&(li_size as u32).to_le_bytes());
    li[4..8].copy_from_slice(&0x1Cu32.to_le_bytes());
    li[8..12].copy_from_slice(&1u32.to_le_bytes());
    li[16..20].copy_from_slice(&0x1Cu32.to_le_bytes());
    li[24..28].copy_from_slice(&((0x1C + base.len()) as u32).to_le_bytes());
    li[0x1C..0x1C + base.len()].copy_from_slice(base);
    lnk.extend(&li);
    vec![
        ("開機記錄：讀出 BootTime", data_field(xml, "BootTime").as_deref() == Some("42345")),
        ("開機記錄：XML 跳脫字元還原", data_field(xml, "FriendlyName").as_deref() == Some("A & B")),
        ("開機記錄：事件編號", event_id(xml) == Some(100)),
        ("開機記錄：時間換算", time_created(xml) == Some(expect)),
        ("開機記錄：截斷的 XML 不會當掉", time_created("<x SystemTime='2026").is_none()),
        ("啟動項目：引號裡的執行檔", resolve_exe(r#""C:\Program Files\X\x.exe" -silent"#).0 == Some(PathBuf::from(r"C:\Program Files\X\x.exe"))),
        (
            "啟動項目：沒有引號、路徑有空白",
            resolve_exe(r"C:\Program Files\OpenVPN Connect\OpenVPNConnect.exe --minimize").0 == Some(PathBuf::from(r"C:\Program Files\OpenVPN Connect\OpenVPNConnect.exe")),
        ),
        ("啟動項目：資料夾名稱含 .exe", resolve_exe(r"C:\Tools.exe.backup\agent.exe -silent").0 == Some(PathBuf::from(r"C:\Tools.exe.backup\agent.exe"))),
        ("啟動項目：參數裡的 .exe 不會被當成程式", resolve_exe(r"C:\Scripts\boot.cmd --log C:\logs\x.exe").0 == Some(PathBuf::from(r"C:\Scripts\boot.cmd"))),
        ("啟動項目：省略 .exe 也找得到", resolve_exe(&format!(r"{}\System32\cmd /c echo", sysroot)) == (Some(cmd_exe), false)),
        ("啟動項目：相對路徑不算找不到", resolve_exe("rundll32.exe shell32.dll,Control_RunDLL").1 == false),
        (
            "啟動項目：有引號但省略 .exe 也找得到",
            parse_command(&format!(r#""{sysroot}\System32\cmd" /c echo"#)) == (Some(PathBuf::from(&sysroot).join(r"System32\cmd.exe")), false, "/c echo".into()),
        ),
        ("啟動項目：tab 也算分隔", parse_command(&format!("{sysroot}\\System32\\cmd.exe\t/c")).2 == "/c"),
        ("啟動項目：取出參數", parse_command(r#""C:\__dc_none__\x.exe" --a b"#).2 == "--a b" && parse_command(r"C:\__dc_none__\y.exe -q").2 == "-q"),
        ("啟動項目：宿主程式不對應到單一項目", is_host(Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")) && is_host(Path::new(r"C:\Python312\python3.12.exe")) && !is_host(Path::new(r"C:\Apps\steam.exe"))),
        ("啟動項目：解析捷徑目標", lnk_target_bytes(&lnk) == Some(PathBuf::from(r"C:\Apps\Tool\tool.exe"))),
        ("啟動項目：壞掉的捷徑不會當掉", lnk_target_bytes(&lnk[..0x50]).is_none()),
        ("啟動項目：讀出捷徑的參數與工作目錄", {
            // 同一個捷徑加上 Unicode 的 StringData：工作目錄、參數
            let mut l2 = lnk.clone();
            l2[0x14] |= 0x10 | 0x20 | 0x80;
            for s in [r"C:\Apps\Tool", "--tray --x"] {
                let u: Vec<u16> = s.encode_utf16().collect();
                l2.extend((u.len() as u16).to_le_bytes());
                l2.extend(u.iter().flat_map(|c| c.to_le_bytes()));
            }
            lnk_info_bytes(&l2)
                == Some(LnkInfo { target: PathBuf::from(r"C:\Apps\Tool\tool.exe"), args: "--tray --x".into(), workdir: Some(PathBuf::from(r"C:\Apps\Tool")) })
                && lnk_info_bytes(&l2[..l2.len() - 4]).is_none()
        }),
        ("啟動項目：Store 應用程式名稱", store_display("5319275A.WhatsAppDesktop_cv1g1gvanyjgm", "x") == "WhatsAppDesktop" && store_display("Microsoft.549981C3F5F10_8wekyb3d8bbwe", "CortanaStartupId") == "Cortana"),
    ]
    .into_iter()
    .chain(registry_roundtrip())
    .collect()
}

/// 在目前使用者的 Run 機碼放一個這次專用、名稱唯一的測試項目，停用、再啟用，確認 StartupApproved 的寫法。
/// 事前確認兩個位置都沒有同名的值；最後只刪除這次自己建立的值，並確認真的刪掉了。
fn registry_roundtrip() -> Vec<(&'static str, bool)> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = format!("DiskCleaner selftest {} {nanos} (safe to delete)", std::process::id());
    let approved = format!(r"{APPROVED}\Run");
    let exists = |path: &str| open_key(HKEY_CURRENT_USER, path, KEY_QUERY_VALUE).and_then(|k| query(&k, &name)).is_some();
    let del = |path: &str| -> bool {
        let p = win::wide(path);
        let n = win::wide(&name);
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, p.as_ptr(), n.as_ptr()) == 0 }
    };
    if exists(RUN) || exists(&approved) {
        return vec![("啟動項目：測試名稱已被使用，略過登錄測試", false)];
    }
    let cmd: Vec<u8> = r#""C:\__dc_selftest__\none.exe" -x"#.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect();
    let created = write_value(HKEY_CURRENT_USER, RUN, &name, REG_SZ, &cmd).is_ok();
    let find = || startup_items().into_iter().find(|i| i.source == Source::UserRun && i.name == name);
    let first = find();
    let off = first.as_ref().is_some_and(|it| set_enabled_many(std::slice::from_ref(it), false) == (1, None)) && find().is_some_and(|i| !i.enabled);
    let raw_off = open_key(HKEY_CURRENT_USER, &approved, KEY_QUERY_VALUE)
        .and_then(|k| query(&k, &name))
        .is_some_and(|(t, d)| t == REG_BINARY && d.len() == 12 && d[0] == 3);
    let on = first.as_ref().is_some_and(|it| set_enabled_many(std::slice::from_ref(it), true) == (1, None)) && find().is_some_and(|i| i.enabled);
    // 只刪這次自己建立的值
    let removed_run = !created || del(RUN);
    let removed_approved = !exists(&approved) || del(&approved);
    let gone = find().is_none() && !exists(&approved);
    vec![
        ("啟動項目：測試項目讀得到、標成找不到程式", created && first.is_some_and(|i| i.enabled && i.missing)),
        ("啟動項目：停用（StartupApproved 寫 03＋時間）", off && raw_off),
        ("啟動項目：再啟用", on),
        ("啟動項目：測試項目清乾淨", removed_run && removed_approved && gone),
    ]
}
