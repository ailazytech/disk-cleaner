//! 開機加速：開機時間（Windows 的開機效能記錄）、拖慢開機的項目、開機自動啟動的程式、系統設定檢查。
//!
//! 會修改系統的只有兩件事，而且都可以還原：
//! - 啟用／停用開機自動啟動的程式：寫入 `Explorer\StartupApproved`（和工作管理員「啟動」分頁相同的機制），
//!   不刪除 Run 機碼、不動啟動資料夾裡的捷徑、不解除安裝任何東西
//! - 開啟／關閉「快速啟動」：`HiberbootEnabled`（開啟時順便 `powercfg /h on`）

use std::path::PathBuf;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_DWORD, REG_EXPAND_SZ, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

use crate::win;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";
const POWER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Power";
const HIBERNATE: &str = r"SYSTEM\CurrentControlSet\Control\Power";

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
    (unsafe { RegOpenKeyExW(root, w.as_ptr(), 0, access, &mut h) } == 0).then_some(Key(h))
}

fn from_wide_bytes(data: &[u8]) -> String {
    let u: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = u.iter().position(|&c| c == 0).unwrap_or(u.len());
    String::from_utf16_lossy(&u[..end])
}

/// 列出一個機碼裡所有的值：(名稱, 型別, 資料)
fn values(k: &Key) -> Vec<(String, u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0u32;
    loop {
        let mut name = vec![0u16; 16384];
        let mut nlen = name.len() as u32;
        let mut ty = 0u32;
        let mut data = vec![0u8; 8192];
        let mut dlen = data.len() as u32;
        let r = unsafe { RegEnumValueW(k.0, i, name.as_mut_ptr(), &mut nlen, std::ptr::null(), &mut ty, data.as_mut_ptr(), &mut dlen) };
        match r {
            0 => {
                data.truncate(dlen as usize);
                out.push((String::from_utf16_lossy(&name[..nlen as usize]), ty, data));
            }
            234 => {
                // ERROR_MORE_DATA：資料太長（不是一般的啟動項目），跳過這一筆
            }
            _ => break, // ERROR_NO_MORE_ITEMS 或其他錯誤
        }
        i += 1;
        if i > 10_000 {
            break;
        }
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

fn read_dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let k = open_key(root, path, KEY_QUERY_VALUE)?;
    match query(&k, name)? {
        (t, d) if t == REG_DWORD && d.len() >= 4 => Some(u32::from_le_bytes([d[0], d[1], d[2], d[3]])),
        _ => None,
    }
}

fn write_value(root: HKEY, path: &str, name: &str, ty: u32, data: &[u8]) -> Result<(), String> {
    let w = win::wide(path);
    let mut h: HKEY = std::ptr::null_mut();
    let r = unsafe { RegCreateKeyExW(root, w.as_ptr(), 0, std::ptr::null(), 0, KEY_SET_VALUE, std::ptr::null(), &mut h, std::ptr::null_mut()) };
    if r != 0 {
        return Err(if r == 5 { "需要系統管理員權限".into() } else { format!("無法開啟登錄機碼（錯誤 {r}）") });
    }
    let k = Key(h);
    let n = win::wide(name);
    let r = unsafe { RegSetValueExW(k.0, n.as_ptr(), 0, ty, data.as_ptr(), data.len() as u32) };
    if r == 0 { Ok(()) } else { Err(if r == 5 { "需要系統管理員權限".into() } else { format!("無法寫入登錄（錯誤 {r}）") }) }
}

/// 展開 %VAR%（REG_EXPAND_SZ 用）
fn expand_env(s: &str) -> String {
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
// 開機自動啟動的程式

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    UserRun,
    MachineRun,
    MachineRun32,
    UserFolder,
    CommonFolder,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::UserRun => "目前使用者",
            Source::MachineRun => "所有使用者",
            Source::MachineRun32 => "所有使用者（32 位元）",
            Source::UserFolder => "啟動資料夾",
            Source::CommonFolder => "啟動資料夾（所有使用者）",
        }
    }
    /// 改它要不要系統管理員權限
    pub fn needs_admin(self) -> bool {
        !matches!(self, Source::UserRun | Source::UserFolder)
    }
    fn approved(self) -> (HKEY, String) {
        match self {
            Source::UserRun => (HKEY_CURRENT_USER, format!(r"{APPROVED}\Run")),
            Source::MachineRun => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\Run")),
            Source::MachineRun32 => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\Run32")),
            Source::UserFolder => (HKEY_CURRENT_USER, format!(r"{APPROVED}\StartupFolder")),
            Source::CommonFolder => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\StartupFolder")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StartupItem {
    /// Run 機碼裡的值名稱，或啟動資料夾裡的檔名
    pub name: String,
    /// 執行的指令（啟動資料夾的項目是捷徑的完整路徑）
    pub command: String,
    /// 指令裡的執行檔
    pub exe: Option<PathBuf>,
    /// 執行檔還在不在（找不到通常是程式已經移除、只剩殘留的啟動項目）
    pub missing: bool,
    pub source: Source,
    pub enabled: bool,
    /// 開機效能記錄裡，這個程式平均讓開機變慢多少毫秒（有記錄才有）
    pub delay_ms: Option<u32>,
}

/// 從指令取出執行檔路徑："C:\a b\x.exe" -arg、C:\a\x.exe /x、%windir%\x.exe
fn exe_of(cmd: &str) -> Option<PathBuf> {
    let c = expand_env(cmd.trim());
    if let Some(rest) = c.strip_prefix('"') {
        return rest.find('"').map(|e| PathBuf::from(&rest[..e]));
    }
    let lower = c.to_ascii_lowercase();
    if let Some(i) = lower.find(".exe") {
        return Some(PathBuf::from(&c[..i + 4]));
    }
    c.split_whitespace().next().map(PathBuf::from)
}

/// StartupApproved 的值：第一個位元組是偶數（02、06）＝啟用，奇數（03、07）＝停用；沒有這個值＝啟用。
fn approved_enabled(source: Source, name: &str) -> bool {
    let (root, path) = source.approved();
    let Some(k) = open_key(root, &path, KEY_QUERY_VALUE) else { return true };
    match query(&k, name) {
        Some((t, d)) if t == REG_BINARY && !d.is_empty() => d[0] & 1 == 0,
        _ => true,
    }
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
        let exe = exe_of(&command);
        let missing = exe.as_ref().is_some_and(|e| e.is_absolute() && !e.exists());
        out.push(StartupItem { enabled: approved_enabled(source, &name), name, command, exe, missing, source, delay_ms: None });
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
        out.push(StartupItem {
            enabled: approved_enabled(source, &name),
            command: path.display().to_string(),
            exe: Some(path),
            missing: false,
            name,
            source,
            delay_ms: None,
        });
    }
}

fn env_path(name: &str, rest: &str) -> Option<PathBuf> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty()).map(|v| PathBuf::from(v).join(rest))
}

pub fn startup_items() -> Vec<StartupItem> {
    let mut out = Vec::new();
    run_entries(HKEY_CURRENT_USER, RUN, Source::UserRun, &mut out);
    run_entries(HKEY_LOCAL_MACHINE, RUN, Source::MachineRun, &mut out);
    run_entries(HKEY_LOCAL_MACHINE, RUN32, Source::MachineRun32, &mut out);
    folder_entries(env_path("APPDATA", r"Microsoft\Windows\Start Menu\Programs\Startup"), Source::UserFolder, &mut out);
    folder_entries(env_path("ProgramData", r"Microsoft\Windows\Start Menu\Programs\StartUp"), Source::CommonFolder, &mut out);
    out
}

/// 啟用或停用一個開機自動啟動的程式（寫 StartupApproved，和工作管理員相同）。
/// 寫入前會重新確認這個項目還在原來的位置，名稱一字不差；不會刪除或改動 Run 機碼與捷徑本身。
pub fn set_enabled(item: &StartupItem, on: bool) -> Result<(), String> {
    let still_there = startup_items().iter().any(|i| i.source == item.source && i.name == item.name);
    if !still_there {
        return Err("這個啟動項目已經不在了，請重新整理".into());
    }
    let mut data = [0u8; 12];
    if on {
        data[0] = 0x02;
    } else {
        data[0] = 0x03;
        data[4..12].copy_from_slice(&win::now_filetime().to_le_bytes());
    }
    let (root, path) = item.source.approved();
    write_value(root, &path, &item.name, REG_BINARY, &data)
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
    /// 原始名稱（程式是執行檔名，用來對應開機自動啟動的項目）
    pub raw: String,
    /// 完整路徑（有的話；用來對應開機自動啟動的項目）
    pub path: String,
    pub count: u32,
    pub total_ms: u64,
    pub max_ms: u32,
}

impl SlowItem {
    pub fn avg_ms(&self) -> u32 {
        (self.total_ms / self.count.max(1) as u64) as u32
    }
}

#[derive(Clone, Debug)]
pub enum BootLog {
    Ok { boots: Vec<BootRecord>, slow: Vec<SlowItem> },
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
    rest[s..e].trim().parse().ok()
}

/// TimeCreated SystemTime='2026-11-15T01:23:45.678Z' → FILETIME（UTC）
fn time_created(xml: &str) -> Option<u64> {
    let i = xml.find("SystemTime=")?;
    let s = &xml[i + 12..];
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse().ok() };
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // 公曆日期 → 1970 起算的天數
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    Some(((secs + 11_644_473_600) * 10_000_000) as u64)
}

fn query_events(query: &str, max: usize) -> Result<Vec<String>, u32> {
    use windows_sys::Win32::System::EventLog::{EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection, EvtRender, EvtRenderEventXml};
    let path = win::wide("Microsoft-Windows-Diagnostics-Performance/Operational");
    let q = win::wide(query);
    let h = unsafe { EvtQuery(0, path.as_ptr(), q.as_ptr(), EvtQueryChannelPath | EvtQueryReverseDirection) };
    if h == 0 {
        return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
    }
    let mut out = Vec::new();
    let mut buf: Vec<u16> = vec![0; 16384];
    while out.len() < max {
        let mut evs = [0isize; 32];
        let mut got = 0u32;
        if unsafe { EvtNext(h, evs.len() as u32, evs.as_mut_ptr(), 2000, 0, &mut got) } == 0 {
            break;
        }
        // 這一批每個事件 handle 都要關，就算已經拿夠了
        for &e in &evs[..got as usize] {
            if out.len() < max {
                let mut used = 0u32;
                let mut props = 0u32;
                let mut ok = unsafe { EvtRender(0, e, EvtRenderEventXml, (buf.len() * 2) as u32, buf.as_mut_ptr() as *mut _, &mut used, &mut props) };
                if ok == 0 && used as usize > buf.len() * 2 {
                    buf.resize(used as usize / 2 + 1, 0);
                    ok = unsafe { EvtRender(0, e, EvtRenderEventXml, (buf.len() * 2) as u32, buf.as_mut_ptr() as *mut _, &mut used, &mut props) };
                }
                if ok != 0 {
                    let n = (used as usize / 2).min(buf.len());
                    let end = buf[..n].iter().position(|&c| c == 0).unwrap_or(n);
                    out.push(String::from_utf16_lossy(&buf[..end]));
                }
            }
            unsafe { EvtClose(e) };
        }
    }
    unsafe { EvtClose(h) };
    Ok(out)
}

pub fn boot_log() -> BootLog {
    let boots_xml = match query_events("*[System[(EventID=100)]]", 30) {
        Ok(v) => v,
        Err(5) => return BootLog::NeedAdmin,
        Err(15007) => return BootLog::Unavailable("這台電腦沒有開機效能記錄".into()),
        Err(e) => return BootLog::Unavailable(format!("無法讀取開機記錄（錯誤 {e}）")),
    };
    let num = |x: &str, n: &str| data_field(x, n).and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0) as u32;
    let mut boots: Vec<BootRecord> = boots_xml
        .iter()
        .filter_map(|x| {
            let total = num(x, "BootTime");
            (total > 0).then(|| BootRecord {
                when: time_created(x).unwrap_or(0),
                total_ms: total,
                main_ms: num(x, "MainPathBootTime"),
                post_ms: num(x, "BootPostBootTime"),
                degraded: data_field(x, "BootIsDegradation").is_some_and(|v| v.trim() == "true"),
            })
        })
        .collect();
    boots.sort_by_key(|b| b.when);

    let mut slow: Vec<SlowItem> = Vec::new();
    if let Ok(list) = query_events("*[System[(EventID>=101 and EventID<=110)]]", 400) {
        for x in &list {
            let id = event_id(x).unwrap_or(0);
            let kind = match id {
                101 => "程式",
                102 => "驅動程式",
                103 => "服務",
                106 => "背景最佳化",
                107 | 108 => "群組原則",
                109 => "裝置",
                110 => "工作階段管理",
                _ => "其他",
            };
            let raw = data_field(x, "Name").unwrap_or_default();
            let friendly = data_field(x, "FriendlyName").filter(|s| !s.trim().is_empty()).unwrap_or_else(|| raw.clone());
            let name = if friendly.trim().is_empty() { kind.to_string() } else { friendly };
            let ms = [num(x, "DegradationTime"), num(x, "TotalTime")].into_iter().find(|&v| v > 0).unwrap_or(0);
            if ms == 0 {
                continue;
            }
            match slow.iter_mut().find(|s| s.kind == kind && s.name == name) {
                Some(s) => {
                    s.count += 1;
                    s.total_ms += ms as u64;
                    s.max_ms = s.max_ms.max(ms);
                }
                None => slow.push(SlowItem { kind, name, raw, path: data_field(x, "Path").unwrap_or_default(), count: 1, total_ms: ms as u64, max_ms: ms }),
            }
        }
    }
    slow.sort_by(|a, b| (b.total_ms).cmp(&a.total_ms));
    slow.truncate(30);
    BootLog::Ok { boots, slow }
}

// ---------------------------------------------------------------------------
// 系統設定檢查

#[derive(Clone, Debug)]
pub struct SysCheck {
    /// 快速啟動（HiberbootEnabled）有沒有開；None＝讀不到
    pub fast_startup: Option<bool>,
    /// 休眠有沒有開（快速啟動需要它）
    pub hibernate: Option<bool>,
    pub sys_drive: String,
    pub sys_free: u64,
    pub sys_total: u64,
    /// 系統磁碟是 SSD（沒有尋軌延遲）；None＝查不到
    pub sys_ssd: Option<bool>,
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
    let letter = drive.chars().next().unwrap_or('C');
    let info = win::drives().into_iter().find(|d| d.root.starts_with(letter));
    SysCheck {
        fast_startup: read_dword(HKEY_LOCAL_MACHINE, POWER, "HiberbootEnabled").map(|v| v != 0),
        hibernate: read_dword(HKEY_LOCAL_MACHINE, HIBERNATE, "HibernateEnabled").map(|v| v != 0),
        sys_drive: format!("{letter}:"),
        sys_free: info.as_ref().map_or(0, |d| d.free),
        sys_total: info.as_ref().map_or(0, |d| d.total),
        sys_ssd: seek_penalty(letter).map(|p| !p),
    }
}

/// 開啟或關閉快速啟動。開啟時會先 `powercfg /h on`（快速啟動需要休眠）。需要系統管理員。
pub fn set_fast_startup(on: bool) -> Result<(), String> {
    if on {
        use std::os::windows::process::CommandExt;
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let powercfg = PathBuf::from(windir).join(r"System32\powercfg.exe");
        let st = std::process::Command::new(powercfg)
            .args(["/h", "on"])
            .creation_flags(0x0800_0000)
            .status()
            .map_err(|e| format!("無法執行 powercfg：{e}"))?;
        if !st.success() {
            return Err("powercfg /h on 失敗（需要系統管理員權限）".into());
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
}

/// 讀取全部（在背景執行緒呼叫）
pub fn load() -> BootData {
    let log = boot_log();
    let mut items = startup_items();
    // 開機記錄裡「程式」類的項目，用執行檔名對到開機自動啟動的程式
    if let BootLog::Ok { slow, .. } = &log {
        for it in &mut items {
            let Some(exe) = it.exe.as_ref() else { continue };
            let full = exe.to_string_lossy().to_lowercase();
            let Some(file) = exe.file_name().map(|f| f.to_string_lossy().to_lowercase()) else { continue };
            // 先比完整路徑，沒有路徑的記錄才退回比檔名
            let hit = slow.iter().filter(|s| s.kind == "程式").find(|s| {
                if s.path.is_empty() { s.raw.to_lowercase() == file } else { s.path.to_lowercase() == full }
            });
            if let Some(s) = hit {
                it.delay_ms = Some(s.avg_ms());
            }
        }
    }
    BootData { log, items, sys: sys_check() }
}

// ---------------------------------------------------------------------------
// 自我測試（只測解析，不碰登錄檔）

pub fn selftest() -> Vec<(&'static str, bool)> {
    let xml = "<Event><System><EventID>100</EventID><TimeCreated SystemTime='2026-11-15T01:02:03.4567890Z'/></System>\
               <EventData><Data Name='BootTime'>42345</Data><Data Name='MainPathBootTime'>28000</Data>\
               <Data Name='FriendlyName'>A &amp; B</Data></EventData></Event>";
    let t = time_created(xml);
    // 2026-11-15 01:02:03 UTC 的 FILETIME
    let expect = (1_794_704_523u64 + 11_644_473_600) * 10_000_000;
    vec![
        ("開機記錄：讀出 BootTime", data_field(xml, "BootTime").as_deref() == Some("42345")),
        ("開機記錄：XML 跳脫字元還原", data_field(xml, "FriendlyName").as_deref() == Some("A & B")),
        ("開機記錄：事件編號", event_id(xml) == Some(100)),
        ("開機記錄：時間換算", t == Some(expect)),
        ("啟動項目：引號裡的執行檔", exe_of(r#""C:\Program Files\X\x.exe" -silent"#) == Some(PathBuf::from(r"C:\Program Files\X\x.exe"))),
        ("啟動項目：沒有引號、路徑有空白", exe_of(r"C:\Program Files\OpenVPN Connect\OpenVPNConnect.exe --minimize") == Some(PathBuf::from(r"C:\Program Files\OpenVPN Connect\OpenVPNConnect.exe"))),
        ("啟動項目：展開環境變數", exe_of(r"%NO_SUCH_VAR_DC%\x.exe").is_some_and(|p| p.to_string_lossy().starts_with("%NO_SUCH_VAR_DC%"))),
    ]
    .into_iter()
    .chain(registry_roundtrip())
    .collect()
}

/// 在目前使用者的 Run 機碼放一個明顯是測試用的假項目，停用、再啟用，確認 StartupApproved 的寫法；最後全部刪掉。
fn registry_roundtrip() -> Vec<(&'static str, bool)> {
    const NAME: &str = "DiskCleaner selftest (safe to delete)";
    let del = |path: &str| {
        let p = win::wide(path);
        let n = win::wide(NAME);
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, p.as_ptr(), n.as_ptr()) };
    };
    let approved = format!(r"{APPROVED}\Run");
    let cmd: Vec<u8> = r#""C:\__dc_selftest__\none.exe" -x"#.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect();
    let created = write_value(HKEY_CURRENT_USER, RUN, NAME, REG_SZ, &cmd).is_ok();
    let find = || startup_items().into_iter().find(|i| i.source == Source::UserRun && i.name == NAME);
    let first = find();
    let off = first.as_ref().is_some_and(|it| set_enabled(it, false).is_ok()) && find().is_some_and(|i| !i.enabled);
    let raw_off = open_key(HKEY_CURRENT_USER, &approved, KEY_QUERY_VALUE)
        .and_then(|k| query(&k, NAME))
        .is_some_and(|(t, d)| t == REG_BINARY && d.len() == 12 && d[0] == 3);
    let on = first.as_ref().is_some_and(|it| set_enabled(it, true).is_ok()) && find().is_some_and(|i| i.enabled);
    // 不管前面成功與否都清乾淨
    del(RUN);
    del(&approved);
    let gone = find().is_none();
    vec![
        ("啟動項目：測試項目讀得到、標成找不到程式", created && first.is_some_and(|i| i.enabled && i.missing)),
        ("啟動項目：停用（StartupApproved 寫 03＋時間）", off && raw_off),
        ("啟動項目：再啟用", on),
        ("啟動項目：測試項目清乾淨", gone),
    ]
}
