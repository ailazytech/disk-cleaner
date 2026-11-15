//! 開機優化：延後啟動（工作排程器）、第三方排程工作與服務、改動記錄與還原。
//!
//! 原則：只用 Windows 的標準機制，不裝常駐程式。工作管理員、工作排程器、服務管理員都看得到我們做了什麼，
//! 就算 DiskCleaner 被刪掉，使用者也能自己改回來。
//! - 延後啟動：只支援「目前使用者」的開機項目（HKCU Run、使用者的啟動資料夾）。在工作排程器 `\DiskCleaner\`
//!   底下建立「只在目前使用者登入時、延遲 N 秒」的工作（帶 DiskCleaner 標記），建立成功後才停用原本的開機自動啟動
//!   （StartupApproved，和工作管理員相同）；不刪 Run 值或捷徑。只刪除、覆蓋帶有我們標記的工作
//! - 排程工作：只停用／恢復（schtasks /change），不刪除；`\Microsoft\`、`\DiskCleaner\` 底下的不列出、不碰
//! - 服務：只在「自動」和「自動（延遲開始）」之間切換，不改成手動或停用；Windows 與 Microsoft 的服務不列出、不改
//! - 每一筆改動都先記在 `%LOCALAPPDATA%\DiskCleaner\boot_changes.txt` 再動手（中途當掉也看得到、還原得了），
//!   可以單筆或整批還原。所有修改都在同一個具名 mutex 底下進行，兩個 DiskCleaner 不會互相覆蓋記錄

use std::path::{Path, PathBuf};

use crate::boot::{self, Source, StartupItem};
use crate::win;

// ---------------------------------------------------------------------------
// 同時只有一個修改在進行（同一台電腦、同一個登入工作階段裡的所有 DiskCleaner）

struct OpLock(windows_sys::Win32::Foundation::HANDLE);

impl Drop for OpLock {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        unsafe {
            ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn op_lock() -> Result<OpLock, String> {
    use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    let name = win::wide("Local\\DiskCleaner-boot-optimize");
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if h.is_null() {
        return Err("無法建立同步鎖".into());
    }
    // WAIT_OBJECT_0 = 0；WAIT_ABANDONED = 0x80（上一個持有者當掉了，記錄檔是原子寫入，照常繼續）
    match unsafe { WaitForSingleObject(h, 15_000) } {
        0 | 0x80 => Ok(OpLock(h)),
        _ => {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
            Err("另一個 DiskCleaner 正在修改開機設定，請稍後再試".into())
        }
    }
}

// ---------------------------------------------------------------------------
// 改動記錄

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    /// 停用了開機自動啟動
    Disabled,
    /// 改成延後啟動（停用原本的＋建立排程工作）
    Delayed,
    /// 停用了第三方排程工作
    TaskDisabled,
    /// 服務從「自動」改成「自動（延遲開始）」
    ServiceDelayed,
}

impl ChangeKind {
    fn tag(self) -> &'static str {
        match self {
            ChangeKind::Disabled => "disabled",
            ChangeKind::Delayed => "delayed",
            ChangeKind::TaskDisabled => "task-disabled",
            ChangeKind::ServiceDelayed => "service-delayed",
        }
    }
    fn from_tag(s: &str) -> Option<ChangeKind> {
        Some(match s {
            "disabled" => ChangeKind::Disabled,
            "delayed" => ChangeKind::Delayed,
            "task-disabled" => ChangeKind::TaskDisabled,
            "service-delayed" => ChangeKind::ServiceDelayed,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::Disabled => "停用開機自動啟動",
            ChangeKind::Delayed => "延後啟動",
            ChangeKind::TaskDisabled => "停用排程工作",
            ChangeKind::ServiceDelayed => "服務改成延遲開始",
        }
    }
    /// 同一個對象的改動（停用與延後是同一個開機項目的兩種狀態）
    fn group(self) -> u8 {
        match self {
            ChangeKind::Disabled | ChangeKind::Delayed => 0,
            ChangeKind::TaskDisabled => 1,
            ChangeKind::ServiceDelayed => 2,
        }
    }
}

/// 一筆「目前和原本不一樣」的改動。還原就是把它改回去。
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub kind: ChangeKind,
    /// 改的時間（FILETIME）
    pub when: u64,
    /// 給人看的名稱
    pub label: String,
    /// 開機項目：來源代號（Source::tag）；排程工作：工作路徑；服務：服務名稱
    pub target: String,
    /// 開機項目：值名稱（或捷徑檔名）、Store 工作位置——和來源一起決定是哪一個項目
    pub name: String,
    /// 開機項目：當時的指令（只用來提示「程式更新過、指令變了」，不參與比對）
    pub command: String,
    pub store_key: String,
    /// 延後啟動：我們建立的排程工作名稱與延遲秒數
    pub task: String,
    pub delay: u32,
}

impl Change {
    /// 同一個對象：Windows 以「來源＋值名稱／檔名＋Store 工作」辨認開機項目，指令被程式更新改掉也還是同一個
    fn same_target(&self, o: &Change) -> bool {
        self.kind.group() == o.kind.group() && self.target == o.target && self.name == o.name && self.store_key == o.store_key
    }
    /// 這筆改動是不是指這個開機項目
    pub fn is_item(&self, it: &StartupItem) -> bool {
        self.kind.group() == 0 && self.target == it.source.tag() && self.name == it.name && self.store_key == it.store_key.clone().unwrap_or_default()
    }
    fn for_item(kind: ChangeKind, it: &StartupItem) -> Change {
        Change {
            kind,
            when: win::now_filetime(),
            label: it.display.clone(),
            target: it.source.tag().into(),
            name: it.name.clone(),
            command: it.command.clone(),
            store_key: it.store_key.clone().unwrap_or_default(),
            task: String::new(),
            delay: 0,
        }
    }
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c => o.push(c),
        }
    }
    o
}

fn unesc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('t') => o.push('\t'),
            Some('n') => o.push('\n'),
            Some('r') => o.push('\r'),
            Some(x) => o.push(x),
            None => {}
        }
    }
    o
}

pub fn log_path() -> PathBuf {
    crate::cache::app_dir().join("boot_changes.txt")
}

/// 延遲秒數的合理範圍
pub const MAX_DELAY: u32 = 3600;
const MAX_ENTRIES: usize = 2000;

/// 讀取改動記錄。格式不對、數字讀不出來或超出範圍的行直接略過（不猜）；檔案太大只讀前面。
pub fn log_load_from(path: &Path) -> Vec<Change> {
    let Ok(meta) = std::fs::metadata(path) else { return Vec::new() };
    if meta.len() > 4 << 20 {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let f: Vec<String> = line.split('\t').map(unesc).collect();
            if f.len() < 10 || f[0] != "v1" {
                return None;
            }
            let delay: u32 = f[9].parse().ok()?;
            if delay > MAX_DELAY {
                return None;
            }
            Some(Change {
                kind: ChangeKind::from_tag(&f[1])?,
                when: f[2].parse().ok()?,
                label: f[3].clone(),
                target: f[4].clone(),
                name: f[5].clone(),
                command: f[6].clone(),
                store_key: f[7].clone(),
                task: f[8].clone(),
                delay,
            })
        })
        .take(MAX_ENTRIES)
        .collect()
}

fn log_save_to(path: &Path, v: &[Change]) -> Result<(), String> {
    let mut out = String::new();
    for c in v {
        let f = [
            "v1".to_string(),
            c.kind.tag().to_string(),
            c.when.to_string(),
            c.label.clone(),
            c.target.clone(),
            c.name.clone(),
            c.command.clone(),
            c.store_key.clone(),
            c.task.clone(),
            c.delay.to_string(),
        ];
        out += &f.iter().map(|s| esc(s)).collect::<Vec<_>>().join("\t");
        out.push('\n');
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 先寫到暫存檔再換名，寫到一半當掉也不會留下壞掉的記錄
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, out).map_err(|e| format!("無法寫入優化記錄：{e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("無法寫入優化記錄：{e}")
    })
}

/// 一次寫入好幾筆（同一個對象的舊記錄會被取代）
fn log_put_many(path: &Path, cs: Vec<Change>) -> Result<(), String> {
    if cs.is_empty() {
        return Ok(());
    }
    let mut v = log_load_from(path);
    v.retain(|x| !cs.iter().any(|c| x.same_target(c)));
    v.extend(cs);
    log_save_to(path, &v)
}

fn log_drop_many(path: &Path, cs: &[Change]) -> Result<(), String> {
    let mut v = log_load_from(path);
    let before = v.len();
    v.retain(|x| !cs.iter().any(|c| x.same_target(c)));
    if v.len() == before { Ok(()) } else { log_save_to(path, &v) }
}

fn log_put(path: &Path, c: Change) -> Result<(), String> {
    log_put_many(path, vec![c])
}

fn log_drop(path: &Path, c: &Change) -> Result<(), String> {
    log_drop_many(path, std::slice::from_ref(c))
}

// ---------------------------------------------------------------------------
// 工作排程器（schtasks.exe）

fn system32(exe: &str) -> PathBuf {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(windir).join("System32").join(exe)
}

/// 執行系統程式（不開主控台視窗、最多等 30 秒），回傳（成功, 標準輸出, 錯誤訊息）
fn run_tool(exe: &str, args: &[&str]) -> (bool, Vec<u8>, String) {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    let child = std::process::Command::new(system32(exe))
        .args(args)
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (false, Vec::new(), format!("無法執行 {exe}：{e}")),
    };
    // 輸出在另一條執行緒讀，避免管線塞滿讓對方卡住
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let t1 = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });
    let t2 = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(30)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = t1.join().unwrap_or_default();
    let err = t2.join().unwrap_or_default();
    match status {
        Some(s) if s.success() => (true, out, String::new()),
        Some(_) => {
            let text = format!("{}{}", crate::cleaner::decode_oem(&out), crate::cleaner::decode_oem(&err));
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (false, out, format!("{exe} 失敗：{}", text.chars().take(200).collect::<String>()))
        }
        None => (false, out, format!("{exe} 超過 30 秒沒有回應，已停止")),
    }
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let (ok, _, err) = run_tool("schtasks.exe", args);
    if ok { Ok(()) } else { Err(err) }
}

pub fn task_exists(name: &str) -> bool {
    schtasks(&["/query", "/tn", name]).is_ok()
}

/// 我們建立的工作都帶這個標記（在說明文字與 Source 裡）
const OWNER_MARK: &str = "[DiskCleaner-owned]";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    Missing,
    Ours,
    Foreign,
}

/// 這個名稱的工作在不在、是不是我們建立的
fn task_owner(name: &str) -> Owner {
    let (ok, out, _) = run_tool("schtasks.exe", &["/query", "/tn", name, "/xml"]);
    if !ok {
        return Owner::Missing;
    }
    let text = crate::cleaner::decode_oem(&out);
    if text.contains(OWNER_MARK) { Owner::Ours } else { Owner::Foreign }
}

/// 我們自己的工作名稱：\DiskCleaner\ 底下的單一層、不含奇怪字元
fn valid_own_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(r"\DiskCleaner\") else { return false };
    !rest.trim().is_empty() && !rest.contains(['\\', '/']) && !rest.contains("..") && !rest.chars().any(char::is_control)
}

/// 別人的工作路徑：\ 開頭、不含 / 或 ..，不在 \Microsoft\、\DiskCleaner\ 底下
fn valid_foreign_path(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    l.starts_with('\\')
        && l.len() > 1
        && !l.contains('/')
        && !l.contains("..")
        && !l.chars().any(char::is_control)
        && !l.starts_with(r"\microsoft\")
        && !l.starts_with(r"\diskcleaner\")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// ISO 8601 的期間，例如 150 秒 → PT2M30S
pub fn iso_delay(secs: u32) -> String {
    match (secs / 60, secs % 60) {
        (0, s) => format!("PT{s}S"),
        (m, 0) => format!("PT{m}M"),
        (m, s) => format!("PT{m}M{s}S"),
    }
}

/// 「登入後延遲 N 秒執行」的工作定義：只在 sid 這個使用者登入時、以一般權限在他的桌面上執行。
/// 路徑有空白就加上引號（不然可能被當成 C:\Program 之類）。
pub fn task_xml(sid: &str, command: &str, args: &str, workdir: Option<&str>, delay: u32, description: &str) -> String {
    let wd = workdir.map(|w| format!("<WorkingDirectory>{}</WorkingDirectory>", xml_escape(w))).unwrap_or_default();
    let args = if args.trim().is_empty() { String::new() } else { format!("<Arguments>{}</Arguments>", xml_escape(args.trim())) };
    let command = if command.contains(' ') && !command.starts_with('"') { format!("\"{command}\"") } else { command.to_string() };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Author>DiskCleaner</Author><Source>DiskCleaner</Source><Description>{desc} {mark}</Description></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId><Delay>{delay}</Delay></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>false</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{cmd}</Command>{args}{wd}</Exec>
  </Actions>
</Task>
"#,
        desc = xml_escape(description),
        mark = OWNER_MARK,
        sid = xml_escape(sid),
        delay = iso_delay(delay.min(MAX_DELAY)),
        cmd = xml_escape(&command),
    )
}

/// 建立（或覆蓋我們自己的同名）排程工作。同名的工作不是我們建立的就拒絕。
fn create_task(name: &str, xml: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    if !valid_own_name(name) {
        return Err("工作名稱不合法".into());
    }
    if task_owner(name) == Owner::Foreign {
        return Err("工作排程器裡已經有同名、不是 DiskCleaner 建立的工作，沒有覆蓋它".into());
    }
    let dir = crate::cache::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("task-{}-{}.xml", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    // schtasks 讀 UTF-16 的 XML 最穩：加上 BOM
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(|c| c.to_le_bytes()));
    std::fs::write(&file, bytes).map_err(|e| format!("無法寫入暫存檔：{e}"))?;
    let r = schtasks(&["/create", "/tn", name, "/xml", &file.to_string_lossy(), "/f"]);
    let _ = std::fs::remove_file(&file);
    r
}

/// 刪除我們建立的工作；本來就不在也算成功；不是我們的（沒有標記）就拒絕
fn delete_task(name: &str) -> Result<(), String> {
    if !valid_own_name(name) {
        return Err("只能刪除 DiskCleaner 自己建立的工作".into());
    }
    match task_owner(name) {
        Owner::Missing => Ok(()),
        Owner::Foreign => Err(format!("{name} 不是 DiskCleaner 建立的工作，沒有刪除")),
        Owner::Ours => schtasks(&["/delete", "/tn", name, "/f"]),
    }
}

/// 刪掉 `\DiskCleaner\` 資料夾（裡面沒有工作時才會成功；失敗不要緊）
fn tidy_folder() {
    let _ = schtasks(&["/delete", "/tn", r"\DiskCleaner", "/f"]);
}

// ---------------------------------------------------------------------------
// 延後啟動

pub const FIRST_DELAY: u32 = 120;
pub const STEP_DELAY: u32 = 30;
pub const DELAY_CHOICES: [u32; 6] = [60, 120, 180, 300, 600, 900];

fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 延後啟動工作的名稱：\DiskCleaner\延後啟動 <名稱> (<雜湊>)。雜湊用「來源＋值名稱＋Store 工作」，程式更新改了指令也不變。
pub fn delay_task_name(it: &StartupItem) -> String {
    let label: String = it
        .display
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control())
        .take(50)
        .collect();
    let label = label.trim().replace("..", ".");
    let h = fnv64(&format!("{}\n{}\n{}", it.source.tag(), it.name, it.store_key.clone().unwrap_or_default()));
    format!(r"\DiskCleaner\延後啟動 {} ({h:016x})", if label.is_empty() { "程式" } else { &label })
}

/// 程式要怎麼放進工作：.bat／.cmd 用 cmd.exe /c 執行，其他直接執行。回傳（程式, 參數）。
pub fn launch_command(exe: &Path, args: &str) -> (String, String) {
    let ext = exe.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if ext == "bat" || ext == "cmd" {
        let inner = if args.trim().is_empty() { format!("\"{}\"", exe.display()) } else { format!("\"{}\" {}", exe.display(), args.trim()) };
        (system32("cmd.exe").to_string_lossy().into_owned(), format!("/c \"{inner}\""))
    } else {
        (exe.to_string_lossy().into_owned(), args.trim().to_string())
    }
}

/// 這個開機項目在工作排程器裡要怎麼執行：（程式, 參數, 工作目錄）。不支援的回傳原因。
pub fn launch_spec(it: &StartupItem) -> Result<(String, String, Option<String>), &'static str> {
    match it.source {
        Source::StoreApp => return Err("Microsoft Store 應用程式不支援延後，只能停用"),
        Source::MachineRun | Source::MachineRun32 | Source::CommonFolder => {
            return Err("「所有使用者」的項目不能延後（延後的工作只能綁一個使用者，其他人登入時就不會啟動），只能停用");
        }
        Source::UserRun | Source::UserFolder => {}
    }
    if it.missing {
        return Err("找不到要執行的程式");
    }
    let (exe, args, wd) = if it.source == Source::UserFolder && it.command.to_ascii_lowercase().ends_with(".lnk") {
        let l = boot::lnk_details(Path::new(&it.command)).ok_or("讀不出捷徑指向的程式")?;
        (l.target, l.args, l.workdir)
    } else {
        (it.exe.clone().ok_or("讀不出要執行的程式")?, it.args.clone(), None)
    };
    // 相對名稱（例如 rundll32）只接受 System32 裡找得到的
    let exe = if exe.is_absolute() {
        exe
    } else {
        let n = exe.to_string_lossy().to_string();
        let cand = [system32(&n), system32(&format!("{n}.exe"))];
        if exe.components().count() != 1 {
            return Err("要執行的程式沒有完整路徑，不支援延後");
        }
        cand.into_iter().find(|p| p.is_file()).ok_or("要執行的程式沒有完整路徑，不支援延後")?
    };
    let ext = exe.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if !matches!(ext.as_str(), "exe" | "com" | "bat" | "cmd") {
        return Err("這種指令（例如 .vbs、.ps1 指令碼）不支援延後，只能停用");
    }
    let wd = wd.or_else(|| exe.parent().map(Path::to_path_buf));
    let (cmd, args) = launch_command(&exe, &args);
    Ok((cmd, args, wd.map(|w| w.to_string_lossy().into_owned())))
}

/// 下一個延後項目的秒數：第一個 2 分鐘，之後每個再晚 30 秒（錯開，避免一起搶資源）
pub fn next_delay(existing: &[u32]) -> u32 {
    existing.iter().copied().max().map_or(FIRST_DELAY, |m| m.saturating_add(STEP_DELAY)).min(MAX_DELAY)
}

/// 把一組開機項目（同一個值名稱或指令，上面清單的一列）改成延後啟動，或改延後的時間。
/// 順序：先寫記錄 → 建立工作 → 停用原本的；任何一步失敗就把前面的退回去（記錄還原成原本的內容）。
/// 中途當掉時：記錄已經有了，重新整理就會看到「延後的工作不在」或「會啟動兩次」，可以從優化記錄還原。
pub fn delay_with(items: &[StartupItem], secs: u32, log: &Path) -> Result<(), String> {
    let _g = op_lock()?;
    let secs = secs.clamp(10, MAX_DELAY);
    let Some(first) = items.iter().find(|i| i.enabled).or(items.first()) else { return Err("沒有項目".into()) };
    let (cmd, args, wd) = launch_spec(first).map_err(String::from)?;
    let sid = win::current_user_sid().ok_or("讀不到目前使用者的 SID")?;
    let name = delay_task_name(first);
    // 已經延後、原本的也都停用了：只是改時間（重建工作，不必再停用一次）
    let only_retime = items.iter().all(|i| i.delayed.is_some() && !i.enabled);
    let backup = log_load_from(log);
    let entries: Vec<Change> = items
        .iter()
        .map(|it| {
            let mut c = Change::for_item(ChangeKind::Delayed, it);
            c.task = name.clone();
            c.delay = secs;
            c
        })
        .collect();
    log_put_many(log, entries)?;
    let desc = format!("DiskCleaner：登入後延遲啟動「{}」（原本登記在{}）。在 DiskCleaner 的優化記錄還原，或刪除這個工作並在工作管理員重新啟用它。", first.display, first.source.label());
    if let Err(e) = create_task(&name, &task_xml(&sid, &cmd, &args, wd.as_deref(), secs, &desc)) {
        let _ = log_save_to(log, &backup);
        return Err(e);
    }
    if only_retime {
        return Ok(());
    }
    let (ok, err) = boot::set_enabled_many(items, false);
    if let Some(e) = err {
        // 停用失敗：刪掉剛建的工作，已經停用的再打開，記錄退回去
        let _ = delete_task(&name);
        if ok > 0 {
            let _ = boot::set_enabled_many(&items[..ok], true);
        }
        let _ = log_save_to(log, &backup);
        return Err(e);
    }
    Ok(())
}

/// 取消延後：enable＝改回開機就啟動，否則改成停用（並記成「停用」）。只刪除帶我們標記的工作。
/// 順序：先改開機項目 → 刪工作 → 改記錄；刪工作失敗就把開機項目改回去。
pub fn undelay_with(items: &[StartupItem], enable: bool, log: &Path) -> Result<(), String> {
    let _g = op_lock()?;
    let entries = log_load_from(log);
    let mut tasks: Vec<String> = items
        .iter()
        .filter_map(|it| entries.iter().find(|c| c.kind == ChangeKind::Delayed && c.is_item(it)).map(|c| c.task.clone()))
        .filter(|t| !t.is_empty())
        .collect();
    if tasks.is_empty()
        && let Some(it) = items.first()
    {
        tasks.push(delay_task_name(it));
    }
    tasks.sort();
    tasks.dedup();
    let was_on: Vec<bool> = items.iter().map(|i| i.enabled).collect();
    let (_, err) = boot::set_enabled_many(items, enable);
    if let Some(e) = err {
        return Err(e);
    }
    for t in &tasks {
        if let Err(e) = delete_task(t) {
            // 退回：開機項目恢復原本的狀態
            for (it, on) in items.iter().zip(&was_on) {
                let _ = boot::set_enabled_many(std::slice::from_ref(it), *on);
            }
            return Err(e);
        }
    }
    let changes: Vec<Change> = items.iter().map(|it| Change::for_item(ChangeKind::Disabled, it)).collect();
    if enable {
        log_drop_many(log, &changes)?;
    } else {
        log_put_many(log, changes)?;
    }
    tidy_folder();
    Ok(())
}

/// 一般的啟用／停用，連同記錄：停用先記下來再改（記不下來就不改）；啟用改完再把記錄拿掉。回傳（改了幾筆, 第一個錯誤）。
pub fn toggle(items: &[StartupItem], on: bool) -> (usize, Option<String>) {
    let _g = match op_lock() {
        Ok(g) => g,
        Err(e) => return (0, Some(e)),
    };
    let log = log_path();
    let backup = log_load_from(&log);
    let changes: Vec<Change> = items.iter().map(|it| Change::for_item(ChangeKind::Disabled, it)).collect();
    if !on && let Err(e) = log_put_many(&log, changes.clone()) {
        return (0, Some(e));
    }
    let (ok, err) = boot::set_enabled_many(items, on);
    if on {
        if let Err(e) = log_drop_many(&log, &changes[..ok]) {
            return (ok, Some(err.unwrap_or(format!("已啟用，但優化記錄沒有更新：{e}"))));
        }
    } else if ok < items.len() {
        // 沒改成功的不要留記錄
        let mut keep = backup;
        keep.retain(|x| !changes[..ok].iter().any(|c| x.same_target(c)));
        keep.extend(changes[..ok].iter().cloned());
        let _ = log_save_to(&log, &keep);
    }
    (ok, err)
}

pub fn delay(items: &[StartupItem], secs: u32) -> Result<(), String> {
    delay_with(items, secs, &log_path())
}

pub fn undelay(items: &[StartupItem], enable: bool) -> Result<(), String> {
    undelay_with(items, enable, &log_path())
}

// ---------------------------------------------------------------------------
// 第三方排程工作（只有系統管理員讀得到別人的工作定義）

#[derive(Clone, Debug)]
pub struct SchedTask {
    /// 工作路徑，例如 \GoogleUpdateTaskMachineCore
    pub path: String,
    pub enabled: bool,
    pub logon: bool,
    pub boot: bool,
    /// 第一個動作的程式（去掉引號、展開環境變數）與參數
    pub command: String,
    pub args: String,
    pub author: String,
    /// 動作數量；不是剛好一個「執行程式」動作的（多個動作、COM 動作），停用時可能連帶停掉別的東西，不提供停用
    pub actions: usize,
}

impl SchedTask {
    pub fn simple(&self) -> bool {
        self.actions == 1 && !self.command.is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum TaskList {
    Ok(Vec<SchedTask>),
    NeedAdmin,
    Err(String),
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// 去掉 XML 註解
fn strip_comments(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// 不分大小寫找 <tag ...>…</tag> 的內容（tag 不能有屬性以外的變化）；回傳所有出現的內容
fn tag_contents(xml: &str, low: &str, tag: &str) -> Vec<String> {
    let open = format!("<{}", tag.to_ascii_lowercase());
    let close = format!("</{}>", tag.to_ascii_lowercase());
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = low[from..].find(&open).map(|i| i + from) {
        // 確認是完整的標籤名稱（<Enabled 不要配到 <EnabledX）
        let after = low[i + open.len()..].chars().next();
        if !matches!(after, Some('>' | ' ' | '/' | '\t' | '\r' | '\n')) {
            from = i + open.len();
            continue;
        }
        let Some(gt) = low[i..].find('>').map(|g| g + i) else { break };
        if low[..gt].ends_with('/') {
            out.push(String::new());
            from = gt + 1;
            continue;
        }
        let Some(end) = low[gt..].find(&close).map(|e| e + gt) else { break };
        out.push(xml[gt + 1..end].to_string());
        from = end + close.len();
    }
    out
}

fn is_false(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0")
}

fn decode_task_file(b: &[u8]) -> String {
    if b.len() >= 2 && b[0] == 0xFF && b[1] == 0xFE {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    String::from_utf8_lossy(b).into_owned()
}

/// 工作定義裡的程式路徑：去掉引號、展開環境變數
fn clean_command(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').and_then(|x| x.split('"').next()).unwrap_or(s);
    boot::expand_env(s)
}

/// 解析一個工作定義；不是開機／登入時執行（或那些觸發條件都停用了）的回傳 None
pub fn parse_task(path: &str, xml: &str) -> Option<SchedTask> {
    let xml = strip_comments(xml);
    let low = xml.to_ascii_lowercase();
    let trig_on = |tag: &str| tag_contents(&xml, &low, tag).iter().any(|b| tag_contents(b, &b.to_ascii_lowercase(), "Enabled").first().is_none_or(|v| !is_false(v)));
    let logon = trig_on("LogonTrigger");
    let boot = trig_on("BootTrigger");
    if !logon && !boot {
        return None;
    }
    // 工作本身的啟用狀態在 <Settings> 底下的 <Enabled>
    let enabled = tag_contents(&xml, &low, "Settings")
        .first()
        .map(|s| {
            // 只看 Settings 直接底下的 Enabled：先拿掉巢狀的 IdleSettings 等區塊
            let sl = s.to_ascii_lowercase();
            let mut plain = s.clone();
            for inner in ["IdleSettings", "NetworkSettings", "MaintenanceSettings", "RestartOnFailure"] {
                for c in tag_contents(s, &sl, inner) {
                    plain = plain.replace(&c, "");
                }
            }
            tag_contents(&plain, &plain.to_ascii_lowercase(), "Enabled").first().is_none_or(|v| !is_false(v))
        })
        .unwrap_or(true);
    let actions_block = tag_contents(&xml, &low, "Actions").first().cloned().unwrap_or_default();
    let al = actions_block.to_ascii_lowercase();
    let execs = tag_contents(&actions_block, &al, "Exec");
    let others = ["ComHandler", "SendEmail", "ShowMessage"].iter().map(|t| tag_contents(&actions_block, &al, t).len()).sum::<usize>();
    let first = execs.first().cloned().unwrap_or_default();
    let fl = first.to_ascii_lowercase();
    let pick = |tag: &str| tag_contents(&first, &fl, tag).first().map(|v| xml_unescape(v).trim().to_string()).unwrap_or_default();
    Some(SchedTask {
        path: path.to_string(),
        enabled,
        logon,
        boot,
        command: clean_command(&pick("Command")),
        args: pick("Arguments"),
        author: tag_contents(&xml, &low, "Author").first().map(|v| xml_unescape(v).trim().to_string()).unwrap_or_default(),
        actions: execs.len() + others,
    })
}

/// 列出開機／登入時執行的第三方排程工作（略過 \Microsoft\ 與我們自己的 \DiskCleaner\，不跟隨任何重新解析點）
pub fn sched_tasks() -> TaskList {
    use std::os::windows::fs::MetadataExt;
    const REPARSE: u32 = 0x400;
    let root = system32("Tasks");
    let mut out = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && depth == 0 => return TaskList::NeedAdmin,
            Err(e) if depth == 0 => return TaskList::Err(format!("讀不到工作排程器的資料夾：{e}")),
            Err(_) => continue,
        };
        for e in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                break;
            }
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
            if meta.file_attributes() & REPARSE != 0 {
                continue;
            }
            if meta.is_dir() {
                if depth == 0 && (name.eq_ignore_ascii_case("Microsoft") || name.eq_ignore_ascii_case("DiskCleaner")) {
                    continue;
                }
                if depth < 8 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            if meta.len() > 1 << 20 {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let rel = p.strip_prefix(&root).map(|r| format!(r"\{}", r.display())).unwrap_or(name);
            if !valid_foreign_path(&rel) {
                continue;
            }
            if let Some(t) = parse_task(&rel, &decode_task_file(&bytes)) {
                out.push(t);
            }
        }
    }
    out.sort_by_key(|t| t.path.to_lowercase());
    TaskList::Ok(out)
}

/// 停用或恢復第三方排程工作（需要系統管理員），並記錄下來（停用先記再改）
pub fn set_task_enabled(t: &SchedTask, on: bool) -> Result<(), String> {
    if !valid_foreign_path(&t.path) {
        return Err("這個工作不在這裡改（Microsoft 與 DiskCleaner 的工作、或路徑不合法）".into());
    }
    let _g = op_lock()?;
    let c = Change {
        kind: ChangeKind::TaskDisabled,
        when: win::now_filetime(),
        label: t.path.rsplit('\\').next().unwrap_or(&t.path).to_string(),
        target: t.path.clone(),
        name: String::new(),
        command: String::new(),
        store_key: String::new(),
        task: String::new(),
        delay: 0,
    };
    let log = log_path();
    if on {
        schtasks(&["/change", "/tn", &t.path, "/enable"])?;
        log_drop(&log, &c)
    } else {
        log_put(&log, c.clone())?;
        if let Err(e) = schtasks(&["/change", "/tn", &t.path, "/disable"]) {
            let _ = log_drop(&log, &c);
            return Err(e);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 第三方的自動啟動服務

#[derive(Clone, Debug)]
pub struct Service {
    pub name: String,
    pub display: String,
    /// 執行檔路徑（從 ImagePath 取出、展開環境變數）
    pub exe: String,
    pub company: String,
    pub delayed: bool,
    pub running: bool,
    /// 載入順序群組；有的話不能設延遲開始（Windows 的限制）
    pub group: String,
}

fn wstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

/// ImagePath 裡的執行檔：展開環境變數；有引號取引號內，沒有引號就取到「.exe 後面接空白或結尾」為止
pub fn image_exe(image: &str) -> String {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let s = image.trim();
    let s = s.strip_prefix(r"\??\").unwrap_or(s);
    let s = boot::expand_env(s);
    let s = if s.to_ascii_lowercase().starts_with(r"\systemroot\") { format!("{windir}{}", &s[11..]) } else { s };
    if let Some(rest) = s.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    let l = s.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = l[from..].find(".exe").map(|i| i + from) {
        let end = i + 4;
        if l[end..].chars().next().is_none_or(|c| c.is_whitespace()) {
            return s[..end].to_string();
        }
        from = end;
    }
    s.split(' ').next().unwrap_or("").to_string()
}

/// 只看路徑就知道是 Windows 的服務（或沒辦法確認的）：這些不讀版本資訊、不列出
fn system_path(exe: &str) -> bool {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_ascii_lowercase();
    let e = exe.to_ascii_lowercase();
    e.is_empty()
        || e.starts_with(r"\\")
        || !Path::new(exe).is_absolute()
        || e == windir
        || e.starts_with(&format!("{windir}\\"))
        || e.contains("%")
        || e.ends_with(r"\svchost.exe")
        // Microsoft 的元件不一定有正確的公司名稱（例如 Microsoft GameInput）：資料夾名稱以 Microsoft 開頭的也算
        || e.split('\\').any(|seg| seg.starts_with("microsoft"))
}

/// 是不是 Windows 或 Microsoft 的服務（這些一律不列出、不改）
fn system_service(exe: &str) -> (bool, String) {
    if system_path(exe) {
        return (true, String::new());
    }
    let company = win::file_company(Path::new(exe));
    (company.to_ascii_lowercase().contains("microsoft"), company)
}

struct Sc(windows_sys::Win32::System::Services::SC_HANDLE);
impl Drop for Sc {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::System::Services::CloseServiceHandle(self.0) };
    }
}

/// 讀服務的設定：（啟動類型, ImagePath, 載入順序群組）
fn service_config(h: &Sc) -> Option<(u32, String, String)> {
    use windows_sys::Win32::System::Services::*;
    let mut need = 0u32;
    unsafe { QueryServiceConfigW(h.0, std::ptr::null_mut(), 0, &mut need) };
    if need == 0 || need > 64 * 1024 {
        return None;
    }
    let mut cfg: Vec<u64> = vec![0; (need as usize).div_ceil(8)];
    if unsafe { QueryServiceConfigW(h.0, cfg.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW, (cfg.len() * 8) as u32, &mut need) } == 0 {
        return None;
    }
    let c = unsafe { &*(cfg.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
    Some((c.dwStartType, wstr(c.lpBinaryPathName), wstr(c.lpLoadOrderGroup)))
}

/// 列出設成「自動」的第三方服務（列出不需要系統管理員）
pub fn services() -> Result<Vec<Service>, String> {
    use windows_sys::Win32::System::Services::*;
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE) };
    if scm.is_null() {
        return Err(format!("無法開啟服務管理員：{}", std::io::Error::last_os_error()));
    }
    let scm = Sc(scm);
    let mut buf: Vec<u64> = vec![0; 64 * 1024 / 8];
    let mut list: Vec<(String, String, u32)> = Vec::new();
    let mut resume = 0u32;
    for _ in 0..64 {
        let (mut need, mut count) = (0u32, 0u32);
        let ok = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buf.as_mut_ptr() as *mut u8,
                (buf.len() * 8) as u32,
                &mut need,
                &mut count,
                &mut resume,
                std::ptr::null(),
            )
        };
        let more = ok == 0 && std::io::Error::last_os_error().raw_os_error() == Some(234);
        if ok == 0 && !more {
            return Err(format!("無法列出服務：{}", std::io::Error::last_os_error()));
        }
        let items = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, count as usize) };
        for s in items {
            list.push((wstr(s.lpServiceName), wstr(s.lpDisplayName), s.ServiceStatusProcess.dwCurrentState));
        }
        if !more {
            break;
        }
        if need as usize > buf.len() * 8 {
            buf.resize((need as usize).div_ceil(8).min(16 << 20), 0);
        }
    }
    let mut out = Vec::new();
    for (name, display, state) in list {
        let w = win::wide(&name);
        let h = unsafe { OpenServiceW(scm.0, w.as_ptr(), SERVICE_QUERY_CONFIG) };
        if h.is_null() {
            continue;
        }
        let h = Sc(h);
        let Some((start, image, group)) = service_config(&h) else { continue };
        if start != SERVICE_AUTO_START {
            continue;
        }
        let exe = image_exe(&image);
        let (sys, company) = system_service(&exe);
        if sys {
            continue;
        }
        let mut info = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: 0 };
        let mut n2 = 0u32;
        let delayed = unsafe {
            QueryServiceConfig2W(
                h.0,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                &mut info as *mut _ as *mut u8,
                std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>() as u32,
                &mut n2,
            ) != 0
                && info.fDelayedAutostart != 0
        };
        out.push(Service { name, display, exe, company, delayed, running: state == SERVICE_RUNNING, group });
    }
    out.sort_by_key(|s| s.display.to_lowercase());
    Ok(out)
}

enum SvcErr {
    /// 服務已經不在了（ERROR_SERVICE_DOES_NOT_EXIST）
    Missing,
    Other(String),
}

fn change_service_delayed(name: &str, label: &str, delayed: bool) -> Result<(), SvcErr> {
    use windows_sys::Win32::System::Services::*;
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if scm.is_null() {
        return Err(SvcErr::Other("無法開啟服務管理員".into()));
    }
    let scm = Sc(scm);
    let w = win::wide(name);
    let h = unsafe { OpenServiceW(scm.0, w.as_ptr(), SERVICE_CHANGE_CONFIG | SERVICE_QUERY_CONFIG) };
    if h.is_null() {
        let e = std::io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(1060) => SvcErr::Missing,
            Some(5) => SvcErr::Other("需要系統管理員權限".into()),
            _ => SvcErr::Other(format!("無法開啟服務 {name}：{e}")),
        });
    }
    let h = Sc(h);
    // 改之前再確認一次：還是「自動」、還是第三方的服務
    let Some((start, image, group)) = service_config(&h) else { return Err(SvcErr::Other(format!("讀不到服務 {label} 的設定"))) };
    if start != SERVICE_AUTO_START {
        return Err(SvcErr::Other(format!("{label} 已經不是「自動」啟動，沒有改")));
    }
    if delayed {
        if system_service(&image_exe(&image)).0 {
            return Err(SvcErr::Other(format!("{label} 是 Windows 或 Microsoft 的服務，不在這裡改")));
        }
        if !group.is_empty() {
            return Err(SvcErr::Other(format!("{label} 屬於載入順序群組，Windows 不允許設成延遲開始")));
        }
    }
    let mut info = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: delayed as i32 };
    if unsafe { ChangeServiceConfig2W(h.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, &mut info as *mut _ as *mut _) } == 0 {
        let e = std::io::Error::last_os_error();
        return Err(SvcErr::Other(if e.raw_os_error() == Some(5) {
            "需要系統管理員權限（有些防毒或系統服務也不允許修改）".into()
        } else {
            format!("無法修改服務 {label}：{e}")
        }));
    }
    Ok(())
}

fn service_change(name: &str, label: &str) -> Change {
    Change {
        kind: ChangeKind::ServiceDelayed,
        when: win::now_filetime(),
        label: label.to_string(),
        target: name.to_string(),
        name: String::new(),
        command: String::new(),
        store_key: String::new(),
        task: String::new(),
        delay: 0,
    }
}

/// 服務在「自動」和「自動（延遲開始）」之間切換（需要系統管理員），並記錄下來（改成延遲先記再改）
pub fn set_service_delayed(name: &str, label: &str, delayed: bool) -> Result<(), String> {
    let _g = op_lock()?;
    let log = log_path();
    let ch = service_change(name, label);
    if delayed {
        log_put(&log, ch.clone())?;
    }
    match change_service_delayed(name, label, delayed) {
        Ok(()) if delayed => Ok(()),
        Ok(()) => log_drop(&log, &ch),
        Err(e) => {
            if delayed {
                let _ = log_drop(&log, &ch);
            }
            Err(match e {
                SvcErr::Missing => format!("服務 {label} 已經不在了"),
                SvcErr::Other(m) => m,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// 還原

/// 把一筆改動改回去。開機項目用目前讀到的清單找回同一個項目（用來源＋名稱，程式更新改了指令也找得到）。
/// 回傳 Ok(Some(說明))＝原本的對象已經不在了，只清掉記錄。
pub fn restore(c: &Change, now: &[StartupItem]) -> Result<Option<String>, String> {
    let _g = op_lock()?;
    let log = log_path();
    match c.kind {
        ChangeKind::Disabled | ChangeKind::Delayed => {
            if c.kind == ChangeKind::Delayed && !c.task.is_empty() {
                delete_task(&c.task)?;
                tidy_folder();
            }
            let note = match now.iter().find(|it| c.is_item(it)) {
                Some(it) => {
                    let (_, err) = boot::set_enabled_many(std::slice::from_ref(it), true);
                    if let Some(e) = err {
                        return Err(e);
                    }
                    None
                }
                None => Some(format!("{} 已經不在開機項目裡了，只清掉記錄", c.label)),
            };
            log_drop(&log, c)?;
            Ok(note)
        }
        ChangeKind::TaskDisabled => {
            if !valid_foreign_path(&c.target) {
                log_drop(&log, c)?;
                return Ok(Some(format!("{} 的路徑不合法，只清掉記錄", c.label)));
            }
            if !task_exists(&c.target) {
                log_drop(&log, c)?;
                return Ok(Some(format!("排程工作 {} 已經不在了，只清掉記錄", c.label)));
            }
            schtasks(&["/change", "/tn", &c.target, "/enable"])?;
            log_drop(&log, c)?;
            Ok(None)
        }
        ChangeKind::ServiceDelayed => match change_service_delayed(&c.target, &c.label, false) {
            Ok(()) => log_drop(&log, c).map(|_| None),
            Err(SvcErr::Missing) => log_drop(&log, c).map(|_| Some(format!("服務 {} 已經不在了，只清掉記錄", c.label))),
            Err(SvcErr::Other(m)) => Err(m),
        },
    }
}

// ---------------------------------------------------------------------------
// 自我測試

pub fn selftest(base: &Path) -> Vec<(&'static str, bool)> {
    let log = base.join("boot_changes_selftest.txt");
    let _ = std::fs::remove_file(&log);
    let weird = Change {
        kind: ChangeKind::Delayed,
        when: 42,
        label: "A\tB\\C\nD".into(),
        target: "UserRun".into(),
        name: "n".into(),
        command: r#""C:\x y\z.exe" --a"#.into(),
        store_key: String::new(),
        task: r"\DiskCleaner\延後啟動 A (0123456789abcdef)".into(),
        delay: 150,
    };
    // 同一個項目（指令被程式更新改掉）只留最新一筆
    let saved = log_put(&log, weird.clone()).is_ok() && log_put(&log, Change { delay: 180, command: "new".into(), ..weird.clone() }).is_ok();
    let back = log_load_from(&log);
    let roundtrip = saved && back.len() == 1 && back[0] == Change { delay: 180, command: "new".into(), ..weird.clone() };
    let dropped = log_drop(&log, &weird).is_ok() && log_load_from(&log).is_empty();
    // 壞掉的行略過
    let _ = std::fs::write(&log, "v1\tdelayed\tx\ta\tUserRun\tn\tc\t\t\\DiskCleaner\\t\t150\nv1\tdelayed\t1\ta\tUserRun\tn\tc\t\tt\t99999999\n");
    let bad_skipped = log_load_from(&log).is_empty();
    let _ = std::fs::remove_file(&log);
    let xml = task_xml("S-1-5-21-1", r"C:\A&B C\app.exe", r#"--name "<x>""#, Some(r"C:\A&B C"), 150, "說明");
    let (bc, ba) = launch_command(Path::new(r"C:\My Dir\run me.bat"), "--x");
    let task = |xml: &str| parse_task(r"\T", xml);
    vec![
        ("優化記錄：特殊字元寫入再讀回、同一個項目（指令改了）只留一筆", roundtrip),
        ("優化記錄：還原後刪掉那一筆", dropped),
        ("優化記錄：數字壞掉或超出範圍的行略過", bad_skipped),
        ("延後啟動：延遲時間格式", iso_delay(150) == "PT2M30S" && iso_delay(120) == "PT2M" && iso_delay(45) == "PT45S"),
        (
            "延後啟動：工作定義的跳脫、引號、標記與登入觸發",
            xml.contains(r"<Command>&quot;C:\A&amp;B C\app.exe&quot;</Command>")
                && xml.contains("<Arguments>--name &quot;&lt;x&gt;&quot;</Arguments>")
                && xml.contains("<UserId>S-1-5-21-1</UserId><Delay>PT2M30S</Delay>")
                && xml.contains("<RunLevel>LeastPrivilege</RunLevel>")
                && xml.contains(OWNER_MARK),
        ),
        ("延後啟動：批次檔用 cmd.exe /c 執行", bc.to_ascii_lowercase().ends_with(r"\system32\cmd.exe") && ba == r#"/c ""C:\My Dir\run me.bat" --x""#),
        ("延後啟動：錯開時間（有上限）", next_delay(&[]) == 120 && next_delay(&[120, 150]) == 180 && next_delay(&[u32::MAX]) == MAX_DELAY),
        (
            "延後啟動：只接受 \\DiskCleaner\\ 底下單層的工作名稱",
            valid_own_name(r"\DiskCleaner\延後啟動 A (1)") && !valid_own_name(r"\DiskCleaner\..\X") && !valid_own_name(r"\DiskCleaner\a\b") && !valid_own_name(r"\Other\X"),
        ),
        (
            "排程工作：不碰 Microsoft、DiskCleaner 與不合法的路徑",
            valid_foreign_path(r"\GoogleUpdateTaskMachineCore")
                && !valid_foreign_path(r"\Microsoft\Windows\X")
                && !valid_foreign_path(r"Microsoft\Windows\X")
                && !valid_foreign_path(r"\DiskCleaner\X")
                && !valid_foreign_path(r"\A\..\Microsoft\X"),
        ),
        (
            "服務：ImagePath 展開環境變數、.exe 後面要接空白",
            image_exe(r#""C:\Program Files\X\svc.exe" -k"#) == r"C:\Program Files\X\svc.exe"
                && image_exe(r"C:\Program Files\Y\y.exe /s") == r"C:\Program Files\Y\y.exe"
                && image_exe(r"C:\Tools\a.exe.dir\b.exe -x") == r"C:\Tools\a.exe.dir\b.exe"
                && system_path(&image_exe(r"%SystemRoot%\System32\alg.exe"))
                && system_path(&image_exe(r"\SystemRoot\System32\drivers\x.sys"))
                && system_path(r"\\server\share\svc.exe")
                && !system_path(r"C:\WindowsApps2\x.exe")
                && system_path(r"C:\Program Files\Microsoft GameInput\x64\GameInputRedistService.exe")
                && !system_path(r"C:\Program Files\Tailscale\tailscaled.exe"),
        ),
        (
            "排程工作：讀出登入觸發與停用狀態（不分大小寫、略過註解）",
            task("<Task><Triggers><LogonTrigger/></Triggers><Settings><Enabled>FALSE</Enabled></Settings><Actions><Exec><Command>\"C:\\a b\\a.exe\"</Command></Exec></Actions></Task>")
                .is_some_and(|t| t.logon && !t.boot && !t.enabled && t.command == r"C:\a b\a.exe" && t.simple())
                && task("<Task><Triggers><TimeTrigger/></Triggers></Task>").is_none()
                && task("<Task><!-- <LogonTrigger/> --><Triggers><TimeTrigger/></Triggers></Task>").is_none()
                && task("<Task><Triggers><LogonTrigger><Enabled>false</Enabled></LogonTrigger></Triggers></Task>").is_none()
                && task("<Task><Triggers><BootTrigger/></Triggers><Settings><IdleSettings><Enabled>false</Enabled></IdleSettings></Settings><Actions><Exec><Command>a</Command></Exec><Exec><Command>b</Command></Exec></Actions></Task>")
                    .is_some_and(|t| t.enabled && t.actions == 2 && !t.simple()),
        ),
    ]
}

/// 自我測試用：結束時一定刪掉（只刪帶我們標記的）工作
struct TaskGuard(String);
impl Drop for TaskGuard {
    fn drop(&mut self) {
        let _ = delete_task(&self.0);
        tidy_folder();
    }
}

/// 端對端：在目前使用者的 Run 機碼放一個測試項目 → 延後 → 確認工作、停用狀態、記錄 → 取消延後 → 全部清乾淨。
/// 再實際執行兩個路徑有空白的工作（程式、批次檔），確認引號與 cmd /c 的寫法真的跑得起來（CI 沒有互動式登入，略過這一段）。
/// 只動這次自己建立的值、檔案與工作。
pub fn delay_roundtrip(base: &Path) -> Vec<(&'static str, bool)> {
    let log = base.join("boot_changes_roundtrip.txt");
    let _ = std::fs::remove_file(&log);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = format!("DiskCleaner delay selftest {} {nanos} (safe to delete)", std::process::id());
    let cmd = format!(r#""{}" /c exit 0"#, system32("cmd.exe").display());
    let Some(guard) = boot::TestRunValue::create(&name, &cmd) else {
        return vec![("延後啟動：建立測試項目", false)];
    };
    let find = || boot::startup_items().into_iter().find(|i| i.source == Source::UserRun && i.name == name);
    let Some(item) = find() else { return vec![("延後啟動：讀到測試項目", false)] };
    let task = delay_task_name(&item);
    let _tg = TaskGuard(task.clone());
    let delayed = delay_with(std::slice::from_ref(&item), 150, &log);
    let after = find();
    let ok_delay = delayed.is_ok()
        && task_owner(&task) == Owner::Ours
        && after.as_ref().is_some_and(|i| !i.enabled)
        && log_load_from(&log).iter().any(|c| c.kind == ChangeKind::Delayed && c.task == task && c.delay == 150);
    // 已延後的改時間：只重建工作
    let retimed = after.as_ref().is_some_and(|i| {
        let mut i = i.clone();
        i.delayed = Some((task.clone(), 150, false));
        delay_with(std::slice::from_ref(&i), 300, &log).is_ok()
    }) && log_load_from(&log).iter().any(|c| c.task == task && c.delay == 300);
    let undone = find().is_some_and(|i| undelay_with(std::slice::from_ref(&i), true, &log).is_ok());
    let ok_undo = undone && task_owner(&task) == Owner::Missing && find().is_some_and(|i| i.enabled) && log_load_from(&log).is_empty();
    drop(_tg);
    let cleaned = guard.remove() && task_owner(&task) == Owner::Missing;
    let _ = std::fs::remove_file(&log);
    let mut out = vec![
        ("延後啟動：建立登入後延遲的工作並停用原本的", ok_delay),
        ("延後啟動：改延遲時間只重建工作", retimed),
        ("延後啟動：取消後工作刪除、重新啟用", ok_undo),
        ("延後啟動：測試項目與工作清乾淨", cleaned),
    ];
    if std::env::var_os("CI").is_none() {
        out.push(("延後啟動：路徑有空白的程式與批次檔真的跑得起來", run_probe(base, &nanos.to_string())));
    }
    out
}

/// 建兩個工作（路徑有空白的 exe 複本、批次檔），手動執行，看它們有沒有寫出記號檔
fn run_probe(base: &Path, tag: &str) -> bool {
    let dir = base.join(format!("run probe {tag}"));
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let Some(sid) = win::current_user_sid() else { return false };
    let exe = dir.join("c md copy.exe");
    let bat = dir.join("run me.bat");
    let (m1, m2) = (dir.join("exe done.txt"), dir.join("bat done.txt"));
    let ok_files = std::fs::copy(system32("cmd.exe"), &exe).is_ok() && std::fs::write(&bat, format!("@echo ok> \"{}\"\r\n", m2.display())).is_ok();
    let t1 = format!(r"\DiskCleaner\selftest exe {tag}");
    let t2 = format!(r"\DiskCleaner\selftest bat {tag}");
    let (g1, g2) = (TaskGuard(t1.clone()), TaskGuard(t2.clone()));
    let mut ok = ok_files;
    if ok {
        let (c1, a1) = launch_command(&exe, &format!("/c echo ok> \"{}\"", m1.display()));
        let (c2, a2) = launch_command(&bat, "");
        let wd = dir.to_string_lossy().into_owned();
        ok = create_task(&t1, &task_xml(&sid, &c1, &a1, Some(&wd), 60, "selftest")).is_ok()
            && create_task(&t2, &task_xml(&sid, &c2, &a2, Some(&wd), 60, "selftest")).is_ok()
            && schtasks(&["/run", "/tn", &t1]).is_ok()
            && schtasks(&["/run", "/tn", &t2]).is_ok();
    }
    if ok {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !(m1.exists() && m2.exists()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        ok = m1.exists() && m2.exists();
    }
    drop((g1, g2));
    // 工作結束後才刪得掉 exe 複本
    for _ in 0..20 {
        if std::fs::remove_dir_all(&dir).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    ok
}
