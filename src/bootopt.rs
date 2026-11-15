//! 開機優化：延後啟動（工作排程器）、第三方排程工作與服務、改動記錄與還原。
//!
//! 原則：只用 Windows 的標準機制，不裝常駐程式。工作管理員、工作排程器、服務管理員都看得到我們做了什麼，
//! 就算 DiskCleaner 被刪掉，使用者也能自己改回來。
//! - 延後啟動：在工作排程器 `\DiskCleaner\` 底下建立「只在目前使用者登入時、延遲 N 秒」的工作，
//!   建立成功後才停用原本的開機自動啟動（StartupApproved，和工作管理員相同）；不刪 Run 值或捷徑
//! - 排程工作：只停用／恢復（schtasks /change），不刪除；`\Microsoft\` 底下的不列出、不碰
//! - 服務：只在「自動」和「自動（延遲開始）」之間切換，不改成手動或停用；系統與 Microsoft 的服務不列出
//! - 每一筆改動都記在 `%LOCALAPPDATA%\DiskCleaner\boot_changes.txt`，可以單筆或整批還原

use std::path::{Path, PathBuf};

use crate::boot::{self, Source, StartupItem};
use crate::win;

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
    /// 開機項目：值名稱、指令、Store 工作位置（用來確認是同一個項目）
    pub name: String,
    pub command: String,
    pub store_key: String,
    /// 延後啟動：我們建立的排程工作名稱與延遲秒數
    pub task: String,
    pub delay: u32,
}

impl Change {
    fn same_target(&self, o: &Change) -> bool {
        self.kind.group() == o.kind.group() && self.target == o.target && self.name == o.name && self.command == o.command && self.store_key == o.store_key
    }
    /// 這筆改動是不是指這個開機項目
    pub fn is_item(&self, it: &StartupItem) -> bool {
        self.kind.group() == 0
            && self.target == it.source.tag()
            && self.name == it.name
            && self.command == it.command
            && self.store_key == it.store_key.clone().unwrap_or_default()
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

/// 讀取改動記錄（檔案不在或格式不對的行就略過）
pub fn log_load_from(path: &Path) -> Vec<Change> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let f: Vec<String> = line.split('\t').map(unesc).collect();
            if f.len() < 10 || f[0] != "v1" {
                return None;
            }
            Some(Change {
                kind: ChangeKind::from_tag(&f[1])?,
                when: f[2].parse().unwrap_or(0),
                label: f[3].clone(),
                target: f[4].clone(),
                name: f[5].clone(),
                command: f[6].clone(),
                store_key: f[7].clone(),
                task: f[8].clone(),
                delay: f[9].parse().unwrap_or(0),
            })
        })
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

fn log_put(path: &Path, c: Change) -> Result<(), String> {
    let mut v = log_load_from(path);
    v.retain(|x| !x.same_target(&c));
    v.push(c);
    log_save_to(path, &v)
}

fn log_drop(path: &Path, c: &Change) -> Result<(), String> {
    let mut v = log_load_from(path);
    let before = v.len();
    v.retain(|x| !x.same_target(c));
    if v.len() == before { Ok(()) } else { log_save_to(path, &v) }
}

// ---------------------------------------------------------------------------
// 工作排程器（schtasks.exe）

fn system32(exe: &str) -> PathBuf {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(windir).join("System32").join(exe)
}

/// 執行系統程式（不開主控台視窗），失敗時回傳它的訊息
fn run_tool(exe: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new(system32(exe))
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("無法執行 {exe}：{e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let text = format!("{}{}", crate::cleaner::decode_oem(&out.stdout), crate::cleaner::decode_oem(&out.stderr));
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Err(format!("{exe} 失敗：{}", text.chars().take(200).collect::<String>()))
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let a: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    run_tool("schtasks.exe", &a)
}

pub fn task_exists(name: &str) -> bool {
    schtasks(&["/query", "/tn", name]).is_ok()
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

/// 「登入後延遲 N 秒執行」的工作定義：只在 sid 這個使用者登入時、以一般權限在他的桌面上執行
pub fn task_xml(sid: &str, command: &str, args: &str, workdir: Option<&str>, delay: u32, description: &str) -> String {
    let wd = workdir.map(|w| format!("<WorkingDirectory>{}</WorkingDirectory>", xml_escape(w))).unwrap_or_default();
    let args = if args.trim().is_empty() { String::new() } else { format!("<Arguments>{}</Arguments>", xml_escape(args.trim())) };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Author>DiskCleaner</Author><Description>{desc}</Description></RegistrationInfo>
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
        sid = xml_escape(sid),
        delay = iso_delay(delay),
        cmd = xml_escape(command),
    )
}

/// 建立（或覆蓋同名的）排程工作
fn create_task(name: &str, xml: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
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

/// 刪除我們建立的工作；本來就不在也算成功
fn delete_task(name: &str) -> Result<(), String> {
    if !name.starts_with(r"\DiskCleaner\") {
        return Err("只能刪除 DiskCleaner 自己建立的工作".into());
    }
    if !task_exists(name) {
        return Ok(());
    }
    schtasks(&["/delete", "/tn", name, "/f"])
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

fn fnv(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// 延後啟動工作的名稱：\DiskCleaner\延後啟動 <名稱> (<雜湊>)
pub fn delay_task_name(it: &StartupItem) -> String {
    let label: String = it
        .display
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control())
        .take(50)
        .collect();
    let label = label.trim();
    let h = fnv(&format!("{}\n{}\n{}", it.source.tag(), it.name, it.command));
    format!(r"\DiskCleaner\延後啟動 {} ({h:08x})", if label.is_empty() { "程式" } else { label })
}

/// 這個開機項目在工作排程器裡要怎麼執行：（程式, 參數, 工作目錄）。不支援的回傳原因。
pub fn launch_spec(it: &StartupItem) -> Result<(String, String, Option<String>), &'static str> {
    if it.source == Source::StoreApp {
        return Err("Microsoft Store 應用程式不支援延後，只能停用");
    }
    if it.missing {
        return Err("找不到要執行的程式");
    }
    let (exe, args, wd) = match it.source {
        Source::UserFolder | Source::CommonFolder if it.command.to_ascii_lowercase().ends_with(".lnk") => {
            let l = boot::lnk_details(Path::new(&it.command)).ok_or("讀不出捷徑指向的程式")?;
            (l.target, l.args, l.workdir)
        }
        _ => (it.exe.clone().ok_or("讀不出要執行的程式")?, it.args.clone(), None),
    };
    let ext = exe.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let relative_name = !exe.is_absolute() && exe.components().count() == 1;
    if !(matches!(ext.as_str(), "exe" | "com" | "bat" | "cmd") || (relative_name && ext.is_empty())) {
        return Err("這種指令（例如 .vbs、.ps1 指令碼）不支援延後，只能停用");
    }
    let wd = wd.or_else(|| exe.is_absolute().then(|| exe.parent().map(Path::to_path_buf)).flatten());
    Ok((exe.to_string_lossy().into_owned(), args, wd.map(|w| w.to_string_lossy().into_owned())))
}

/// 下一個延後項目的秒數：第一個 2 分鐘，之後每個再晚 30 秒（錯開，避免一起搶資源）
pub fn next_delay(existing: &[u32]) -> u32 {
    existing.iter().copied().max().map_or(FIRST_DELAY, |m| m + STEP_DELAY)
}

/// 把一組開機項目（同一個指令，上面清單的一列）改成延後啟動：先建工作，成功才停用原本的；任何一步失敗就復原。
pub fn delay_with(items: &[StartupItem], secs: u32, log: &Path) -> Result<(), String> {
    let Some(first) = items.iter().find(|i| i.enabled).or(items.first()) else { return Err("沒有項目".into()) };
    let (cmd, args, wd) = launch_spec(first).map_err(String::from)?;
    let sid = win::current_user_sid().ok_or("讀不到目前使用者的 SID")?;
    let name = delay_task_name(first);
    let desc = format!("DiskCleaner：登入後延遲啟動「{}」（原本登記在{}）。刪除這個工作並在工作管理員重新啟用它就能還原。", first.display, first.source.label());
    create_task(&name, &task_xml(&sid, &cmd, &args, wd.as_deref(), secs, &desc))?;
    let (ok, err) = boot::set_enabled_many(items, false);
    if let Some(e) = err {
        // 停用失敗：刪掉剛建的工作，已經停用的再打開，回到原狀
        let _ = delete_task(&name);
        if ok > 0 {
            let _ = boot::set_enabled_many(&items[..ok], true);
        }
        return Err(e);
    }
    let mut changed = Vec::new();
    for it in items {
        let mut c = Change::for_item(ChangeKind::Delayed, it);
        c.task = name.clone();
        c.delay = secs;
        changed.push(c);
    }
    for c in changed {
        if let Err(e) = log_put(log, c) {
            // 沒記下來就沒辦法還原：整個退回去
            let _ = delete_task(&name);
            let _ = boot::set_enabled_many(items, true);
            return Err(e);
        }
    }
    Ok(())
}

/// 取消延後：刪掉工作；enable＝重新啟用原本的開機自動啟動（否則維持停用，記成「停用」）
pub fn undelay_with(items: &[StartupItem], enable: bool, log: &Path) -> Result<(), String> {
    let entries = log_load_from(log);
    let mut tasks: Vec<String> = items.iter().filter_map(|it| entries.iter().find(|c| c.kind == ChangeKind::Delayed && c.is_item(it)).map(|c| c.task.clone())).collect();
    if tasks.is_empty()
        && let Some(it) = items.first()
    {
        tasks.push(delay_task_name(it));
    }
    tasks.sort();
    tasks.dedup();
    for t in &tasks {
        delete_task(t)?;
    }
    if enable {
        let (_, err) = boot::set_enabled_many(items, true);
        if let Some(e) = err {
            return Err(format!("延後的工作已刪除，但重新啟用失敗：{e}"));
        }
        for it in items {
            log_drop(log, &Change::for_item(ChangeKind::Disabled, it))?;
        }
    } else {
        for it in items {
            log_put(log, Change::for_item(ChangeKind::Disabled, it))?;
        }
    }
    tidy_folder();
    Ok(())
}

/// 一般的啟用／停用之後更新記錄：停用就記下來（可以還原），重新啟用就不用記了
pub fn note_toggle(items: &[StartupItem], on: bool) -> Result<(), String> {
    let log = log_path();
    for it in items {
        if on {
            log_drop(&log, &Change::for_item(ChangeKind::Disabled, it))?;
        } else {
            log_put(&log, Change::for_item(ChangeKind::Disabled, it))?;
        }
    }
    Ok(())
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
    pub command: String,
    pub args: String,
    pub author: String,
}

#[derive(Clone, Debug)]
pub enum TaskList {
    Ok(Vec<SchedTask>),
    NeedAdmin,
    Err(String),
}

fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let i = xml.find(&open)? + open.len();
    let j = xml[i..].find(&format!("</{tag}>"))? + i;
    Some(xml[i..j].replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&").trim().to_string())
}

fn decode_task_file(b: &[u8]) -> String {
    if b.len() >= 2 && b[0] == 0xFF && b[1] == 0xFE {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    String::from_utf8_lossy(b).into_owned()
}

/// 解析一個工作定義；不是開機／登入時執行的回傳 None
pub fn parse_task(path: &str, xml: &str) -> Option<SchedTask> {
    let logon = xml.contains("<LogonTrigger");
    let boot = xml.contains("<BootTrigger");
    if !logon && !boot {
        return None;
    }
    // 工作本身的啟用狀態在 <Settings> 裡的 <Enabled>
    let enabled = match (xml.find("<Settings>"), xml.find("</Settings>")) {
        (Some(a), Some(b)) if a < b => !xml[a..b].contains("<Enabled>false</Enabled>"),
        _ => true,
    };
    Some(SchedTask {
        path: path.to_string(),
        enabled,
        logon,
        boot,
        command: xml_tag(xml, "Command").unwrap_or_default(),
        args: xml_tag(xml, "Arguments").unwrap_or_default(),
        author: xml_tag(xml, "Author").unwrap_or_default(),
    })
}

/// 列出開機／登入時執行的第三方排程工作（略過 \Microsoft\ 與我們自己的 \DiskCleaner\）
pub fn sched_tasks() -> TaskList {
    let root = system32("Tasks");
    let mut out = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && depth == 0 => return TaskList::NeedAdmin,
            Err(e) if depth == 0 => return TaskList::Err(format!("讀不到工作排程器的資料夾：{e}")),
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if depth == 0 && (name.eq_ignore_ascii_case("Microsoft") || name.eq_ignore_ascii_case("DiskCleaner")) {
                    continue;
                }
                if depth < 8 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.len() > 1 << 20 {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let rel = p.strip_prefix(&root).map(|r| format!(r"\{}", r.display())).unwrap_or(name);
            if let Some(t) = parse_task(&rel, &decode_task_file(&bytes)) {
                out.push(t);
            }
        }
    }
    out.sort_by_key(|t| t.path.to_lowercase());
    TaskList::Ok(out)
}

/// 停用或恢復第三方排程工作（需要系統管理員），並記錄下來
pub fn set_task_enabled(t: &SchedTask, on: bool) -> Result<(), String> {
    if t.path.to_ascii_lowercase().starts_with(r"\microsoft\") {
        return Err("Microsoft 的排程工作不在這裡改".into());
    }
    schtasks(&["/change", "/tn", &t.path, if on { "/enable" } else { "/disable" }])?;
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
    if on { log_drop(&log_path(), &c) } else { log_put(&log_path(), c) }
}

// ---------------------------------------------------------------------------
// 第三方的自動啟動服務

#[derive(Clone, Debug)]
pub struct Service {
    pub name: String,
    pub display: String,
    /// 執行檔路徑（從 ImagePath 取出）
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

/// ImagePath 裡的執行檔：有引號取引號內，沒有就取到 .exe 為止
pub fn image_exe(image: &str) -> String {
    let s = image.trim();
    if let Some(rest) = s.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    let l = s.to_ascii_lowercase();
    match l.find(".exe") {
        Some(i) => s[..i + 4].to_string(),
        None => s.split(' ').next().unwrap_or("").to_string(),
    }
}

/// 是不是 Windows 或 Microsoft 的服務（這些一律不列出）
fn system_service(exe: &str, company: &str) -> bool {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_ascii_lowercase();
    let e = exe.to_ascii_lowercase();
    e.is_empty()
        || e.starts_with(&windir)
        || e.starts_with(r"\systemroot\")
        || e.starts_with("system32\\")
        || e.contains(r"\svchost.exe")
        || company.to_ascii_lowercase().contains("microsoft")
}

struct Sc(windows_sys::Win32::System::Services::SC_HANDLE);
impl Drop for Sc {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::System::Services::CloseServiceHandle(self.0) };
    }
}

/// 列出設成「自動」的第三方服務（列出不需要系統管理員）
pub fn services() -> Result<Vec<Service>, String> {
    use windows_sys::Win32::System::Services::*;
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE) };
    if scm.is_null() {
        return Err(format!("無法開啟服務管理員（錯誤 {}）", std::io::Error::last_os_error()));
    }
    let scm = Sc(scm);
    let mut buf: Vec<u64> = vec![0; 64 * 1024 / 8];
    let mut list: Vec<(String, String, u32)> = Vec::new();
    let mut resume = 0u32;
    loop {
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
            buf.resize((need as usize).div_ceil(8), 0);
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
        let mut need = 0u32;
        unsafe { QueryServiceConfigW(h.0, std::ptr::null_mut(), 0, &mut need) };
        if need == 0 || need > 64 * 1024 {
            continue;
        }
        let mut cfg: Vec<u64> = vec![0; (need as usize).div_ceil(8)];
        if unsafe { QueryServiceConfigW(h.0, cfg.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW, (cfg.len() * 8) as u32, &mut need) } == 0 {
            continue;
        }
        let c = unsafe { &*(cfg.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
        if c.dwStartType != SERVICE_AUTO_START {
            continue;
        }
        let exe = image_exe(&wstr(c.lpBinaryPathName));
        let company = win::file_company(Path::new(&exe));
        if system_service(&exe, &company) {
            continue;
        }
        let group = wstr(c.lpLoadOrderGroup);
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

/// 服務在「自動」和「自動（延遲開始）」之間切換（需要系統管理員），並記錄下來
pub fn set_service_delayed(name: &str, label: &str, delayed: bool) -> Result<(), String> {
    use windows_sys::Win32::System::Services::*;
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if scm.is_null() {
        return Err("無法開啟服務管理員".into());
    }
    let scm = Sc(scm);
    let w = win::wide(name);
    let h = unsafe { OpenServiceW(scm.0, w.as_ptr(), SERVICE_CHANGE_CONFIG | SERVICE_QUERY_CONFIG) };
    if h.is_null() {
        let e = std::io::Error::last_os_error();
        return Err(if e.raw_os_error() == Some(5) { "需要系統管理員權限".into() } else { format!("無法開啟服務 {name}：{e}") });
    }
    let h = Sc(h);
    // 只動「自動」啟動的服務
    let mut need = 0u32;
    unsafe { QueryServiceConfigW(h.0, std::ptr::null_mut(), 0, &mut need) };
    let mut cfg: Vec<u64> = vec![0; (need as usize).div_ceil(8).max(1)];
    if unsafe { QueryServiceConfigW(h.0, cfg.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW, (cfg.len() * 8) as u32, &mut need) } == 0 {
        return Err(format!("讀不到服務 {name} 的設定"));
    }
    let c = unsafe { &*(cfg.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
    if c.dwStartType != SERVICE_AUTO_START {
        return Err(format!("{label} 已經不是「自動」啟動，沒有改"));
    }
    let mut info = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: delayed as i32 };
    if unsafe { ChangeServiceConfig2W(h.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, &mut info as *mut _ as *mut _) } == 0 {
        let e = std::io::Error::last_os_error();
        return Err(if e.raw_os_error() == Some(5) { "需要系統管理員權限（有些防毒或系統服務也不允許修改）".into() } else { format!("無法修改服務 {label}：{e}") });
    }
    let ch = Change {
        kind: ChangeKind::ServiceDelayed,
        when: win::now_filetime(),
        label: label.to_string(),
        target: name.to_string(),
        name: String::new(),
        command: String::new(),
        store_key: String::new(),
        task: String::new(),
        delay: 0,
    };
    if delayed { log_put(&log_path(), ch) } else { log_drop(&log_path(), &ch) }
}

// ---------------------------------------------------------------------------
// 還原

/// 把一筆改動改回去。開機項目用目前讀到的清單找回同一個項目（找不到就只清掉記錄）。
pub fn restore(c: &Change, now: &[StartupItem]) -> Result<(), String> {
    let log = log_path();
    match c.kind {
        ChangeKind::Disabled | ChangeKind::Delayed => {
            if c.kind == ChangeKind::Delayed && !c.task.is_empty() {
                delete_task(&c.task)?;
                tidy_folder();
            }
            if let Some(it) = now.iter().find(|it| c.is_item(it)) {
                let (_, err) = boot::set_enabled_many(std::slice::from_ref(it), true);
                if let Some(e) = err {
                    return Err(e);
                }
            }
            log_drop(&log, c)
        }
        ChangeKind::TaskDisabled => {
            let t = SchedTask { path: c.target.clone(), enabled: false, logon: false, boot: false, command: String::new(), args: String::new(), author: String::new() };
            set_task_enabled(&t, true)
        }
        ChangeKind::ServiceDelayed => set_service_delayed(&c.target, &c.label, false),
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
        task: r"\DiskCleaner\延後啟動 A (01234567)".into(),
        delay: 150,
    };
    let saved = log_put(&log, weird.clone()).is_ok() && log_put(&log, Change { delay: 180, ..weird.clone() }).is_ok();
    let back = log_load_from(&log);
    let roundtrip = saved && back.len() == 1 && back[0] == Change { delay: 180, ..weird.clone() };
    let dropped = log_drop(&log, &weird).is_ok() && log_load_from(&log).is_empty();
    let xml = task_xml("S-1-5-21-1", r"C:\A&B\app.exe", r#"--name "<x>""#, Some(r"C:\A&B"), 150, "說明");
    vec![
        ("優化記錄：特殊字元寫入再讀回、同一個項目只留一筆", roundtrip),
        ("優化記錄：還原後刪掉那一筆", dropped),
        ("延後啟動：延遲時間格式", iso_delay(150) == "PT2M30S" && iso_delay(120) == "PT2M" && iso_delay(45) == "PT45S"),
        (
            "延後啟動：工作定義的跳脫與登入觸發",
            xml.contains(r"<Command>C:\A&amp;B\app.exe</Command>")
                && xml.contains("<Arguments>--name &quot;&lt;x&gt;&quot;</Arguments>")
                && xml.contains("<UserId>S-1-5-21-1</UserId><Delay>PT2M30S</Delay>")
                && xml.contains("<RunLevel>LeastPrivilege</RunLevel>"),
        ),
        ("延後啟動：錯開時間", next_delay(&[]) == 120 && next_delay(&[120, 150]) == 180),
        ("服務：從 ImagePath 取出執行檔", image_exe(r#""C:\Program Files\X\svc.exe" -k"#) == r"C:\Program Files\X\svc.exe" && image_exe(r"C:\Program Files\Y\y.exe /s") == r"C:\Program Files\Y\y.exe"),
        (
            "排程工作：讀出登入觸發與停用狀態",
            parse_task(r"\T", "<Task><Triggers><LogonTrigger/></Triggers><Settings><Enabled>false</Enabled></Settings><Actions><Exec><Command>a.exe</Command></Exec></Actions></Task>")
                .is_some_and(|t| t.logon && !t.boot && !t.enabled && t.command == "a.exe")
                && parse_task(r"\T", "<Task><Triggers><TimeTrigger/></Triggers></Task>").is_none(),
        ),
    ]
}

/// 端對端：在目前使用者的 Run 機碼放一個測試項目 → 延後 → 確認工作、停用狀態、記錄 → 取消延後 → 全部清乾淨。
/// 只動這次自己建立的值與工作。
pub fn delay_roundtrip(base: &Path) -> Vec<(&'static str, bool)> {
    let log = base.join("boot_changes_roundtrip.txt");
    let _ = std::fs::remove_file(&log);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = format!("DiskCleaner delay selftest {} {nanos} (safe to delete)", std::process::id());
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let cmd = format!(r#""{sysroot}\System32\cmd.exe" /c exit 0"#);
    let Some(guard) = boot::TestRunValue::create(&name, &cmd) else {
        return vec![("延後啟動：建立測試項目", false)];
    };
    let find = || boot::startup_items().into_iter().find(|i| i.source == Source::UserRun && i.name == name);
    let Some(item) = find() else { return vec![("延後啟動：讀到測試項目", false)] };
    let task = delay_task_name(&item);
    let delayed = delay_with(std::slice::from_ref(&item), 150, &log);
    let after = find();
    let ok_delay = delayed.is_ok()
        && task_exists(&task)
        && after.as_ref().is_some_and(|i| !i.enabled)
        && log_load_from(&log).iter().any(|c| c.kind == ChangeKind::Delayed && c.task == task && c.delay == 150);
    let undone = after.as_ref().is_some_and(|i| undelay_with(std::slice::from_ref(i), true, &log).is_ok());
    let ok_undo = undone && !task_exists(&task) && find().is_some_and(|i| i.enabled) && log_load_from(&log).is_empty();
    // 不管成功與否都清掉自己建立的東西
    let _ = delete_task(&task);
    tidy_folder();
    let cleaned = guard.remove() && !task_exists(&task);
    let _ = std::fs::remove_file(&log);
    vec![
        ("延後啟動：建立登入後延遲的工作並停用原本的", ok_delay),
        ("延後啟動：取消後工作刪除、重新啟用", ok_undo),
        ("延後啟動：測試項目與工作清乾淨", cleaned),
    ]
}
