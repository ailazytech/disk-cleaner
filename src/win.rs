//! Windows API 包裝：目錄列舉、磁碟資訊、權限、資源回收筒、檔案總管。

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetFileAttributesExW, GetFileExInfoStandard,
    GetLogicalDrives, GetVolumeInformationW, SetFileAttributesW, WIN32_FILE_ATTRIBUTE_DATA,
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
            break;
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

pub fn clear_readonly(path: &Path) -> bool {
    let p = long_path(path);
    unsafe { SetFileAttributesW(p.as_ptr(), ATTR_NORMAL) != 0 }
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

/// 指定路徑所在磁碟的剩餘空間。
pub fn free_space(path: &Path) -> Option<u64> {
    let w = wide(path);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
        return None;
    }
    Some(free)
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
pub fn move_to_recycle_bin(paths: &[PathBuf]) -> bool {
    use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
    if paths.is_empty() {
        return true;
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
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
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
