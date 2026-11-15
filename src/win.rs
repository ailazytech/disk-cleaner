//! Windows API 包裝：目錄列舉、磁碟資訊、權限、資源回收筒、檔案總管。

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetFileAttributesExW, GetFileExInfoStandard,
    GetLogicalDrives, GetVolumeInformationW, WIN32_FILE_ATTRIBUTE_DATA,
    WIN32_FIND_DATAW,
};

pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_NORMAL: u32 = 0x80;
pub const ATTR_REPARSE: u32 = 0x400;
pub const ATTR_OFFLINE: u32 = 0x1000;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x40000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

/// 雲端佔位檔（OneDrive 等）：實際沒有佔用本機空間。
pub const ATTR_CLOUD_MASK: u32 = ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_RAMDISK: u32 = 6;

/// 轉成以 NUL 結尾的 UTF-16。
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// 把路徑轉成 `\\?\` 長路徑格式（已經是 UNC/長路徑就不動）。
pub fn long_path(p: &Path) -> Vec<u16> {
    let s: Vec<u16> = p.as_os_str().encode_wide().collect();
    let starts = |pre: &str| {
        let pre: Vec<u16> = pre.encode_utf16().collect();
        s.len() >= pre.len() && s[..pre.len()] == pre[..]
    };
    let mut out = Vec::with_capacity(s.len() + 8);
    if starts(r"\\?\") || starts(r"\\.\") {
        // 已經是長路徑
    } else if starts(r"\\") {
        out.extend(r"\\?\UNC".encode_utf16());
        out.extend_from_slice(&s[1..]);
        out.push(0);
        return out;
    } else {
        out.extend(r"\\?\".encode_utf16());
    }
    out.extend_from_slice(&s);
    out.push(0);
    out
}

/// 目錄列舉的一筆資料（借用 FindFirstFile 的結果，不額外 syscall）。
pub struct Entry<'a> {
    pub name: &'a [u16],
    pub attrs: u32,
    pub size: u64,
    /// FILETIME（100ns，從 1601 年起算）
    pub mtime: u64,
}

impl Entry<'_> {
    pub fn is_dir(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.attrs & ATTR_REPARSE != 0
    }
    pub fn is_cloud(&self) -> bool {
        self.attrs & ATTR_CLOUD_MASK != 0
    }
    pub fn name_os(&self) -> OsString {
        OsString::from_wide(self.name)
    }
    /// 實際佔用本機的大小（雲端佔位檔算 0）。
    pub fn local_size(&self) -> u64 {
        if self.is_cloud() { 0 } else { self.size }
    }
}

struct FindHandle(HANDLE);
impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe { FindClose(self.0) };
    }
}

/// 列舉目錄（不含 `.` 與 `..`）。用 FindExInfoBasic + LARGE_FETCH，速度最快。
pub fn list_dir(dir: &Path, mut f: impl FnMut(&Entry)) -> io::Result<()> {
    let mut pattern = long_path(dir);
    pattern.pop(); // 去掉 NUL
    if pattern.last() != Some(&(b'\\' as u16)) {
        pattern.push(b'\\' as u16);
    }
    pattern.push(b'*' as u16);
    pattern.push(0);

    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let h = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        // ERROR_FILE_NOT_FOUND (2)：空目錄（根目錄以外不會發生）
        if err.raw_os_error() == Some(2) {
            return Ok(());
        }
        return Err(err);
    }
    let _guard = FindHandle(h);
    loop {
        let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(260);
        let name = &data.cFileName[..len];
        let dot = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
        if !dot {
            let e = Entry {
                name,
                attrs: data.dwFileAttributes,
                size: ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64,
                mtime: ((data.ftLastWriteTime.dwHighDateTime as u64) << 32)
                    | data.ftLastWriteTime.dwLowDateTime as u64,
            };
            f(&e);
        }
        if unsafe { FindNextFileW(h, &mut data) } == 0 {
            // 只有 ERROR_NO_MORE_FILES (18) 是正常結束；其他錯誤要回報，不能當成列舉完了
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(18) {
                break;
            }
            return Err(err);
        }
    }
    Ok(())
}

/// 取得單一檔案/資料夾的屬性、大小、修改時間。
pub fn stat(path: &Path) -> Option<(u32, u64, u64)> {
    let p = long_path(path);
    let mut d: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileAttributesExW(p.as_ptr(), GetFileExInfoStandard, &mut d as *mut _ as *mut _) };
    if ok == 0 {
        return None;
    }
    let size = ((d.nFileSizeHigh as u64) << 32) | d.nFileSizeLow as u64;
    let mtime = ((d.ftLastWriteTime.dwHighDateTime as u64) << 32) | d.ftLastWriteTime.dwLowDateTime as u64;
    Some((d.dwFileAttributes, size, mtime))
}

/// 路徑本身是不是 reparse point（junction、symlink…）。不會跟隨連結。
pub fn is_reparse(path: &Path) -> bool {
    stat(path).is_some_and(|(a, _, _)| a & ATTR_REPARSE != 0)
}

/// 路徑上（不含磁碟根目錄）有沒有任何一層是 reparse point，包含它自己。
pub fn has_reparse_on_path(path: &Path) -> bool {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.parent().is_none() {
            break;
        }
        if is_reparse(p) {
            return true;
        }
        cur = p.parent();
    }
    false
}

/// 解析成最終的實際路徑（展開 junction、8.3 短檔名），去掉 `\\?\` 前綴。
pub fn final_path(path: &Path) -> Option<PathBuf> {
    let c = std::fs::canonicalize(path).ok()?;
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Some(PathBuf::from(format!(r"\\{rest}")));
    }
    Some(PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s)))
}

/// 8.3 短路徑（磁碟關掉短檔名、或路徑不存在時回傳 None）。
pub fn short_path(p: &Path) -> Option<PathBuf> {
    let w = wide(p);
    let mut buf = [0u16; 1024];
    let n = unsafe { windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// 命令列模式（--boot-report 等）用：GUI 程式沒有主控台，標準輸出沒被導向時，接到啟動它的主控台上，
/// 讓 println! 看得到。已經導向到檔案或管線時不動。
pub fn attach_parent_console() {
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    let valid = |h: HANDLE| !h.is_null() && h != INVALID_HANDLE_VALUE;
    unsafe {
        if valid(GetStdHandle(STD_OUTPUT_HANDLE)) || AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        // 有些情況接上主控台後標準輸出仍是空的：直接開 CONOUT$
        if !valid(GetStdHandle(STD_OUTPUT_HANDLE)) {
            let name = wide("CONOUT$");
            let h = CreateFileW(name.as_ptr(), 0x4000_0000 | 0x8000_0000, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
            if valid(h) {
                SetStdHandle(STD_OUTPUT_HANDLE, h);
                SetStdHandle(STD_ERROR_HANDLE, h);
            }
        }
    }
}

/// 現在時間（FILETIME 單位）。
pub fn now_filetime() -> u64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64 / 100)
        .unwrap_or(0);
    unix + 116_444_736_000_000_000
}

pub const FILETIME_DAY: u64 = 24 * 3600 * 10_000_000;

// ---------------------------------------------------------------------------
// 磁碟

#[derive(Clone, Debug)]
pub struct DriveInfo {
    pub root: String, // "C:\\"
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub kind: &'static str,
}

impl DriveInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
    pub fn letter(&self) -> char {
        self.root.chars().next().unwrap_or('?')
    }
}

pub fn drives() -> Vec<DriveInfo> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let w = wide(&root);
        let t = unsafe { GetDriveTypeW(w.as_ptr()) };
        let kind = match t {
            DRIVE_FIXED => "本機磁碟",
            DRIVE_REMOVABLE => "卸除式磁碟",
            DRIVE_REMOTE => "網路磁碟",
            DRIVE_RAMDISK => "RAM 磁碟",
            _ => continue,
        };
        if let Some(d) = drive_info(&root, kind) {
            out.push(d);
        }
    }
    out
}

pub fn drive_info(root: &str, kind: &'static str) -> Option<DriveInfo> {
    let w = wide(root);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
        return None;
    }
    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            w.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    let to_s = |b: &[u16]| {
        let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf16_lossy(&b[..n])
    };
    Some(DriveInfo {
        root: root.to_string(),
        label: if ok != 0 { to_s(&label) } else { String::new() },
        fs: if ok != 0 { to_s(&fs) } else { String::new() },
        total,
        free,
        kind,
    })
}

// ---------------------------------------------------------------------------
// 權限

pub fn is_elevated() -> bool {
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev: TOKEN_ELEVATION = std::mem::zeroed();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elev as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        );
        CloseHandle(token);
        ok != 0 && elev.TokenIsElevated != 0
    }
}

/// 以系統管理員身分重新啟動自己。成功回傳 true（呼叫端應結束程式）。
pub fn relaunch_as_admin() -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let Ok(exe) = std::env::current_exe() else { return false };
    let verb = wide("runas");
    let file = wide(exe.as_os_str());
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    r as usize > 32
}

// ---------------------------------------------------------------------------
// 資源回收筒

pub fn recycle_bin_size(root: Option<&str>) -> (u64, u64) {
    use windows_sys::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
    let w = root.map(wide);
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    let p = w.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null());
    let hr = unsafe { SHQueryRecycleBinW(p, &mut info) };
    if hr < 0 {
        return (0, 0);
    }
    (info.i64Size.max(0) as u64, info.i64NumItems.max(0) as u64)
}

pub fn empty_recycle_bin() -> bool {
    use windows_sys::Win32::UI::Shell::SHEmptyRecycleBinW;
    // SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND
    let hr = unsafe { SHEmptyRecycleBinW(std::ptr::null_mut(), std::ptr::null(), 0x1 | 0x2 | 0x4) };
    // 回收筒本來就空時會回傳 E_UNEXPECTED，也當作成功
    hr >= 0 || hr == 0x8000FFFFu32 as i32
}

/// 移到資源回收筒（可復原）。
/// 移到資源回收筒。無法回收（太大、回收筒停用）時會跳出警告詢問，不會靜默永久刪除。
/// SHFileOperation 不支援超過 MAX_PATH 的路徑，這種情況直接回傳 false。
pub fn move_to_recycle_bin(paths: &[PathBuf]) -> bool {
    use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW};
    if paths.is_empty() {
        return true;
    }
    if paths.iter().any(|p| p.as_os_str().len() >= 259) {
        return false;
    }
    let mut from: Vec<u16> = Vec::new();
    for p in paths {
        from.extend(p.as_os_str().encode_wide());
        from.push(0);
    }
    from.push(0);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE as u32;
    op.pFrom = from.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOF_SILENT | FOF_NOERRORUI) as u16;
    let r = unsafe { SHFileOperationW(&mut op) };
    r == 0 && op.fAnyOperationsAborted == 0
}

// ---------------------------------------------------------------------------
// 檔案總管

pub fn reveal_in_explorer(path: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn();
}

pub fn open_in_explorer(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

/// 在 PATH 裡找執行檔（含 .exe/.cmd/.bat）。
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in ["exe", "cmd", "bat"] {
            let p = dir.join(format!("{name}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 以 handle 為基礎的刪除
//
// 從磁碟根目錄開始，每一層都「以上一層的 handle 為基準」打開下一層（NtOpenFile + RootDirectory），
// 列舉也直接對 handle 做（GetFileInformationByHandleEx），完全不再經過路徑字串。
// 所以就算某個資料夾在途中被改名、被換掉，或空資料夾被原地設成 junction，
// 我們看到、打開、刪除的都還是原本那個資料夾物件裡的東西。

use windows_sys::Win32::Storage::FileSystem as fsys;

pub struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: 檔案 handle 可以跨執行緒使用
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

/// 從 handle 取得的資訊（不經過路徑，不會被換掉）。
#[derive(Clone, Copy, Debug)]
pub struct HInfo {
    pub attrs: u32,
    pub size: u64,
    pub mtime: u64,
    pub links: u32,
    pub volume: u32,
    pub index: u64,
}

impl HInfo {
    pub fn is_dir(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.attrs & ATTR_REPARSE != 0
    }
    pub fn is_cloud(&self) -> bool {
        self.attrs & ATTR_CLOUD_MASK != 0
    }
}

/// 刪除時需要的資訊，一次系統呼叫拿齊（FileStatInformation）。
#[derive(Clone, Copy, Debug)]
pub struct HStat {
    pub attrs: u32,
    pub size: u64,
    /// 實際占用的大小（稀疏、壓縮檔會比 size 小）
    pub alloc: u64,
    pub mtime: u64,
    pub links: u32,
}

impl HStat {
    pub fn is_dir(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.attrs & ATTR_REPARSE != 0
    }
    pub fn is_cloud(&self) -> bool {
        self.attrs & ATTR_CLOUD_MASK != 0
    }
}

/// handle 列舉出來的一筆
#[derive(Clone, Debug)]
pub struct DirEnt {
    pub name: OsString,
    pub attrs: u32,
    pub mtime: u64,
    /// 建立、修改、變更（含改名、屬性）時間中最新的一個
    pub touched: u64,
}

impl DirEnt {
    /// 一般資料夾（不是連結）
    pub fn is_dir(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0 && self.attrs & ATTR_REPARSE == 0
    }
}

pub const ERROR_DIR_NOT_EMPTY: i32 = 145;
const ERROR_ACCESS_DENIED: i32 = 5;

/// 打開 handle 的用途
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Open {
    /// 只讀屬性（不影響其他程式，也不擋任何操作）
    Info,
    /// 握住資料夾並可以列舉：其他程式不能改名、刪除或替換它。
    /// 注意：只要求屬性存取權的 handle 不參與分享檢查，所以必須要求 FILE_LIST_DIRECTORY 才擋得住。
    Pin,
    /// 檔案：之後要用 [`Handle::delete`] 刪掉
    Delete,
    /// 資料夾：可以列舉，之後要刪掉
    DeleteDir,
    /// 讀內容並鎖住（別人不能寫、不能刪、不能改名）：比對重複模型時保留的那份
    ReadLocked,
    /// 讀內容、鎖住，之後要刪掉：比對重複模型時要刪的那份
    ReadDelete,
}

impl Open {
    fn access(self) -> u32 {
        let base = fsys::FILE_READ_ATTRIBUTES | fsys::SYNCHRONIZE;
        base | match self {
            Open::Info => 0,
            Open::Pin => fsys::FILE_LIST_DIRECTORY,
            Open::Delete => fsys::DELETE | fsys::FILE_WRITE_ATTRIBUTES,
            Open::DeleteDir => fsys::DELETE | fsys::FILE_LIST_DIRECTORY | fsys::FILE_WRITE_ATTRIBUTES,
            Open::ReadLocked => fsys::FILE_READ_DATA,
            Open::ReadDelete => fsys::FILE_READ_DATA | fsys::DELETE,
        }
    }
    fn share(self) -> u32 {
        match self {
            Open::ReadLocked | Open::ReadDelete => fsys::FILE_SHARE_READ,
            _ => fsys::FILE_SHARE_READ | fsys::FILE_SHARE_WRITE,
        }
    }
}

fn nt_err(status: i32) -> io::Error {
    let code = unsafe { windows_sys::Win32::Foundation::RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

impl Handle {
    /// 用路徑打開（不跟隨連結，連結就打開連結本身）。只用在磁碟根目錄或不需要防競態的地方，
    /// 其他一律用 [`pin_dir`] + [`Handle::open_rel`]。
    pub fn open(path: &Path, mode: Open) -> io::Result<Handle> {
        let p = long_path(path);
        let h = unsafe {
            fsys::CreateFileW(
                p.as_ptr(),
                mode.access(),
                mode.share(),
                std::ptr::null(),
                fsys::OPEN_EXISTING,
                fsys::FILE_FLAG_BACKUP_SEMANTICS | fsys::FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE { Err(io::Error::last_os_error()) } else { Ok(Handle(h)) }
    }

    /// 以這個資料夾 handle 為基準，打開裡面名為 `name` 的項目（不跟隨連結）。
    /// 不經過路徑字串：這個資料夾就算被改名或原地設成 junction，打開的仍是它裡面的項目。
    pub fn open_rel(&self, name: &OsStr, mode: Open) -> io::Result<Handle> {
        match self.open_rel_access(name, mode, mode.access()) {
            // 改屬性的權限只有舊版 Windows 的備用刪除路徑才需要；被拒絕就不要求
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) && matches!(mode, Open::Delete | Open::DeleteDir) => {
                self.open_rel_access(name, mode, mode.access() & !fsys::FILE_WRITE_ATTRIBUTES)
            }
            r => r,
        }
    }

    fn open_rel_access(&self, name: &OsStr, mode: Open, access: u32) -> io::Result<Handle> {
        use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
        use windows_sys::Wdk::Storage::FileSystem::NtOpenFile;
        use windows_sys::Win32::Foundation::UNICODE_STRING;
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
        let mut w: Vec<u16> = name.encode_wide().collect();
        // 只接受單一層名稱
        let bad = w.is_empty() || w.len() > 32000 || w.iter().any(|&c| c == b'\\' as u16 || c == b'/' as u16) || name == "." || name == "..";
        if bad {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "不是單一層的名稱"));
        }
        let us = UNICODE_STRING { Length: (w.len() * 2) as u16, MaximumLength: (w.len() * 2) as u16, Buffer: w.as_mut_ptr() };
        let oa = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.0,
            ObjectName: &us,
            Attributes: 0x40, // OBJ_CASE_INSENSITIVE
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        let mut h: HANDLE = std::ptr::null_mut();
        // FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_SYNCHRONOUS_IO_NONALERT
        let opts = 0x0020_0000 | 0x0000_4000 | 0x0000_0020;
        let st = unsafe { NtOpenFile(&mut h, access, &oa, &mut iosb, mode.share(), opts) };
        if st < 0 { Err(nt_err(st)) } else { Ok(Handle(h)) }
    }

    pub fn info(&self) -> Option<HInfo> {
        let mut d: fsys::BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { fsys::GetFileInformationByHandle(self.0, &mut d) } == 0 {
            return None;
        }
        Some(HInfo {
            attrs: d.dwFileAttributes,
            size: ((d.nFileSizeHigh as u64) << 32) | d.nFileSizeLow as u64,
            mtime: ((d.ftLastWriteTime.dwHighDateTime as u64) << 32) | d.ftLastWriteTime.dwLowDateTime as u64,
            links: d.nNumberOfLinks,
            volume: d.dwVolumeSerialNumber,
            index: ((d.nFileIndexHigh as u64) << 32) | d.nFileIndexLow as u64,
        })
    }

    /// 刪除需要的資訊（屬性、大小、實際占用、修改時間、硬連結數）。Windows 10 1709 以後一次呼叫就拿齊，
    /// 比 info()＋allocated() 少一次系統呼叫；舊版不支援時自動退回兩次呼叫。
    pub fn stat(&self) -> Option<HStat> {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        use windows_sys::Wdk::Storage::FileSystem::{FILE_STAT_INFORMATION, FileStatInformation, NtQueryInformationFile};
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
        static UNSUPPORTED: AtomicBool = AtomicBool::new(false);
        if !UNSUPPORTED.load(Relaxed) {
            let mut s: FILE_STAT_INFORMATION = unsafe { std::mem::zeroed() };
            let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
            let st = unsafe {
                NtQueryInformationFile(self.0, &mut iosb, &mut s as *mut _ as *mut _, std::mem::size_of::<FILE_STAT_INFORMATION>() as u32, FileStatInformation)
            };
            if st >= 0 {
                return Some(HStat {
                    attrs: s.FileAttributes,
                    size: s.EndOfFile.max(0) as u64,
                    alloc: s.AllocationSize.max(0) as u64,
                    mtime: s.LastWriteTime.max(0) as u64,
                    links: s.NumberOfLinks,
                });
            }
            // STATUS_INVALID_INFO_CLASS／STATUS_NOT_IMPLEMENTED：這版 Windows 沒有，之後都走舊方法
            if st == 0xC000_0003u32 as i32 || st == 0xC000_0002u32 as i32 {
                UNSUPPORTED.store(true, Relaxed);
            }
        }
        let i = self.info()?;
        Some(HStat { attrs: i.attrs, size: i.size, alloc: self.allocated().unwrap_or(i.size), mtime: i.mtime, links: i.links })
    }

    /// 實際占用的大小（稀疏、壓縮檔會比邏輯大小小）。
    pub fn allocated(&self) -> Option<u64> {
        let mut s: fsys::FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
        let sz = std::mem::size_of::<fsys::FILE_STANDARD_INFO>() as u32;
        let ok = unsafe { fsys::GetFileInformationByHandleEx(self.0, fsys::FileStandardInfo, &mut s as *mut _ as *mut _, sz) };
        (ok != 0).then_some(s.AllocationSize.max(0) as u64)
    }

    /// 建立、修改、變更（含改名、屬性）時間中最新的一個。
    pub fn touched(&self) -> Option<u64> {
        let mut b: fsys::FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
        let sz = std::mem::size_of::<fsys::FILE_BASIC_INFO>() as u32;
        let ok = unsafe { fsys::GetFileInformationByHandleEx(self.0, fsys::FileBasicInfo, &mut b as *mut _ as *mut _, sz) };
        (ok != 0).then(|| b.CreationTime.max(b.LastWriteTime).max(b.ChangeTime).max(0) as u64)
    }

    /// 這個 handle 實際指向的完整路徑（長檔名，不含 8.3 短檔名、不含 `\\?\`）。
    pub fn real_path(&self) -> Option<PathBuf> {
        let mut buf = vec![0u16; 1024];
        loop {
            let n = unsafe { fsys::GetFinalPathNameByHandleW(self.0, buf.as_mut_ptr(), buf.len() as u32, 0) } as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                let s = String::from_utf16_lossy(&buf[..n]);
                if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
                    return Some(PathBuf::from(format!(r"\\{rest}")));
                }
                return Some(PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s)));
            }
            buf.resize(n + 1, 0);
        }
    }

    /// 直接對資料夾 handle 列舉內容（不經過路徑）。handle 需要 FILE_LIST_DIRECTORY（Pin／DeleteDir）。
    pub fn list(&self) -> io::Result<Vec<DirEnt>> {
        const NO_MORE_FILES: i32 = 18;
        let mut out = Vec::new();
        let mut buf = vec![0u64; 64 * 1024 / 8]; // 8 位元組對齊
        let mut class = fsys::FileFullDirectoryRestartInfo;
        loop {
            let ok = unsafe { fsys::GetFileInformationByHandleEx(self.0, class, buf.as_mut_ptr() as *mut _, (buf.len() * 8) as u32) };
            if ok == 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(NO_MORE_FILES) {
                    return Ok(out);
                }
                return Err(e);
            }
            class = fsys::FileFullDirectoryInfo;
            let base = buf.as_ptr() as *const u8;
            let mut off = 0usize;
            loop {
                // SAFETY: 系統填好的連續 FILE_FULL_DIR_INFO，NextEntryOffset 指向下一筆
                let e = unsafe { &*(base.add(off) as *const fsys::FILE_FULL_DIR_INFO) };
                let n = e.FileNameLength as usize / 2;
                let name = unsafe { std::slice::from_raw_parts(e.FileName.as_ptr(), n) };
                let dot = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
                if !dot {
                    out.push(DirEnt {
                        name: OsString::from_wide(name),
                        attrs: e.FileAttributes,
                        mtime: e.LastWriteTime.max(0) as u64,
                        touched: e.CreationTime.max(e.LastWriteTime).max(e.ChangeTime).max(0) as u64,
                    });
                }
                if e.NextEntryOffset == 0 {
                    break;
                }
                off += e.NextEntryOffset as usize;
            }
        }
    }

    /// 讀內容（handle 需要 FILE_READ_DATA）。
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0u32;
        let len = buf.len().min(u32::MAX as usize) as u32;
        let ok = unsafe { fsys::ReadFile(self.0, buf.as_mut_ptr(), len, &mut n, std::ptr::null_mut()) };
        if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
    }

    fn clear_readonly(&self) -> bool {
        let mut b: fsys::FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
        let sz = std::mem::size_of::<fsys::FILE_BASIC_INFO>() as u32;
        if unsafe { fsys::GetFileInformationByHandleEx(self.0, fsys::FileBasicInfo, &mut b as *mut _ as *mut _, sz) } == 0 {
            return false;
        }
        if b.FileAttributes & ATTR_READONLY == 0 {
            return false;
        }
        b.FileAttributes &= !ATTR_READONLY;
        if b.FileAttributes == 0 {
            b.FileAttributes = ATTR_NORMAL;
        }
        // 時間欄位填 0 代表不更動
        b.CreationTime = 0;
        b.LastAccessTime = 0;
        b.LastWriteTime = 0;
        b.ChangeTime = 0;
        unsafe { fsys::SetFileInformationByHandle(self.0, fsys::FileBasicInfo, &b as *const _ as *const _, sz) != 0 }
    }

    /// 透過這個 handle 刪除它（檔案、空資料夾，或連結本身）。唯讀屬性一併處理。
    pub fn delete(&self) -> io::Result<()> {
        let ex = fsys::FILE_DISPOSITION_INFO_EX {
            Flags: fsys::FILE_DISPOSITION_FLAG_DELETE
                | fsys::FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
                | fsys::FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        };
        let ok = unsafe {
            fsys::SetFileInformationByHandle(
                self.0,
                fsys::FileDispositionInfoEx,
                &ex as *const _ as *const _,
                std::mem::size_of::<fsys::FILE_DISPOSITION_INFO_EX>() as u32,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // 只有「不支援 Ex」（舊版 Windows、FAT32／exFAT、網路磁碟）才改用舊方法；
        // 權限不足、使用中之類的錯誤直接回報，不要先動到唯讀屬性
        const UNSUPPORTED: [i32; 4] = [1, 50, 87, 124]; // INVALID_FUNCTION、NOT_SUPPORTED、INVALID_PARAMETER、INVALID_LEVEL
        if !err.raw_os_error().is_some_and(|c| UNSUPPORTED.contains(&c)) {
            return Err(err);
        }
        let d = fsys::FILE_DISPOSITION_INFO { DeleteFile: true };
        let set = |h: HANDLE| unsafe {
            fsys::SetFileInformationByHandle(
                h,
                fsys::FileDispositionInfo,
                &d as *const _ as *const _,
                std::mem::size_of::<fsys::FILE_DISPOSITION_INFO>() as u32,
            ) != 0
        };
        if set(self.0) {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // 舊方法遇到唯讀檔會被拒絕：這時才清掉唯讀再試一次
        if err.raw_os_error() == Some(ERROR_ACCESS_DENIED) && self.clear_readonly() && set(self.0) {
            return Ok(());
        }
        Err(err)
    }
}

/// 從磁碟根目錄開始，逐層「以上一層 handle 為基準」不跟隨連結地打開並握住 `dir` 與它的所有上層，
/// 確認每一層都是真的資料夾、不是連結。回傳的最後一個就是 `dir` 本身（Pin，可列舉）。
/// 這些 handles drop 之前，這些資料夾都不能被改名或替換。磁碟根目錄本身不接受。
pub fn pin_dir(dir: &Path) -> io::Result<Vec<Handle>> {
    use std::path::{Component, Prefix};
    let mut comps = dir.components();
    let root = match (comps.next(), comps.next()) {
        (Some(Component::Prefix(p)), Some(Component::RootDir)) => match p.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => format!("{}:\\", l as char),
            _ => return Err(io::Error::other("只支援本機磁碟路徑")),
        },
        _ => return Err(io::Error::other("必須是完整路徑")),
    };
    let names: Vec<&OsStr> = comps
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(io::Error::other("路徑裡不能有 . 或 ..")),
        })
        .collect::<io::Result<_>>()?;
    if names.is_empty() {
        return Err(io::Error::other("不能是磁碟根目錄"));
    }
    let mut out: Vec<Handle> = Vec::with_capacity(names.len() + 1);
    out.push(Handle::open(Path::new(&root), Open::Pin)?);
    for n in names {
        let h = out.last().unwrap().open_rel(n, Open::Pin)?;
        let i = h.info().ok_or_else(io::Error::last_os_error)?;
        if i.is_reparse() {
            return Err(io::Error::other(format!("路徑經過連結：{}", n.to_string_lossy())));
        }
        if !i.is_dir() {
            return Err(io::Error::other(format!("不是資料夾：{}", n.to_string_lossy())));
        }
        out.push(h);
    }
    Ok(out)
}

/// 目前有沒有在執行這些程式（不分大小寫的執行檔名稱）。
pub fn process_running(names: &[&str]) -> bool {
    use windows_sys::Win32::System::Diagnostics::ToolHelp as th;
    let snap = unsafe { th::CreateToolhelp32Snapshot(th::TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return false;
    }
    let _guard = Handle(snap);
    let mut e: th::PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<th::PROCESSENTRY32W>() as u32;
    let mut ok = unsafe { th::Process32FirstW(snap, &mut e) } != 0;
    while ok {
        let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
        let exe = String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase();
        if names.iter().any(|x| exe == x.to_lowercase()) {
            return true;
        }
        ok = unsafe { th::Process32NextW(snap, &mut e) } != 0;
    }
    false
}

/// 這個磁碟代號是不是 SUBST 出來的（指向某個資料夾，不是獨立的磁碟區）。
pub fn is_subst(root: &str) -> bool {
    let dev = wide(root.trim_end_matches('\\'));
    let mut buf = [0u16; 512];
    let n = unsafe { fsys::QueryDosDeviceW(dev.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    n > 0 && String::from_utf16_lossy(&buf[..n.min(buf.len())]).starts_with(r"\??\")
}


/// 磁碟區的唯一名稱（\?\Volume{GUID}\），同一個磁碟區有多個代號時用來去重。
pub fn volume_name(root: &str) -> Option<String> {
    let w = wide(root);
    let mut buf = [0u16; 64];
    let ok = unsafe { fsys::GetVolumeNameForVolumeMountPointW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// 一個 Known Folder 的實際位置（不會建立資料夾）。
pub fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use windows_sys::Win32::UI::Shell as sh;
    let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
    let hr = unsafe { sh::SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut p) };
    let mut out = None;
    if hr >= 0 && !p.is_null() {
        let mut n = 0;
        unsafe {
            while *p.add(n) != 0 {
                n += 1;
            }
            out = Some(PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(p, n))));
        }
    }
    if !p.is_null() {
        unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(p as *const _) };
    }
    out
}

/// Known Folder 的實際位置（文件、桌面…被搬到別的磁碟也找得到）。
pub fn known_folders() -> Vec<PathBuf> {
    use windows_sys::Win32::UI::Shell as sh;
    [sh::FOLDERID_Documents, sh::FOLDERID_Desktop, sh::FOLDERID_Downloads, sh::FOLDERID_Pictures, sh::FOLDERID_Music, sh::FOLDERID_Videos]
        .iter()
        .filter_map(known_folder)
        .collect()
}
