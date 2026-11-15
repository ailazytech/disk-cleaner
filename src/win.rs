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
// 以 handle 為基礎的刪除：不跟隨連結；握住 handle 的期間，別人不能改名、刪除或替換它

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

pub const ERROR_DIR_NOT_EMPTY: i32 = 145;

/// 打開 handle 的用途
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// 只讀屬性（不影響其他程式，也不擋任何操作）
    Info,
    /// 握住資料夾：其他程式不能改名、刪除或替換它
    Pin,
    /// 之後要用 [`Handle::delete`] 刪掉它（同時也有 Pin 的效果）
    Delete,
}

impl Handle {
    /// 不跟隨連結地打開檔案或資料夾（連結就打開連結本身）。
    ///
    /// 分享模式不含 DELETE。注意：只要求屬性存取權的 handle 不參與分享檢查，
    /// 所以 Pin 必須要求 FILE_LIST_DIRECTORY，「不讓別人刪除／改名」才會生效。
    pub fn open(path: &Path, mode: Open) -> io::Result<Handle> {
        let p = long_path(path);
        let access = fsys::FILE_READ_ATTRIBUTES
            | fsys::SYNCHRONIZE
            | match mode {
                Open::Info => 0,
                Open::Pin => fsys::FILE_LIST_DIRECTORY,
                Open::Delete => fsys::DELETE | fsys::FILE_WRITE_ATTRIBUTES,
            };
        let h = unsafe {
            fsys::CreateFileW(
                p.as_ptr(),
                access,
                fsys::FILE_SHARE_READ | fsys::FILE_SHARE_WRITE,
                std::ptr::null(),
                fsys::OPEN_EXISTING,
                fsys::FILE_FLAG_BACKUP_SEMANTICS | fsys::FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE { Err(io::Error::last_os_error()) } else { Ok(Handle(h)) }
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

    fn clear_readonly(&self) -> bool {
        let mut b: fsys::FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
        let sz = std::mem::size_of::<fsys::FILE_BASIC_INFO>() as u32;
        if unsafe { fsys::GetFileInformationByHandleEx(self.0, fsys::FileBasicInfo, &mut b as *mut _ as *mut _, sz) } == 0 {
            return false;
        }
        if b.FileAttributes & ATTR_READONLY == 0 {
            return true;
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
        if err.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY) {
            return Err(err);
        }
        // 舊版 Windows 或非 NTFS 不支援 Ex：清掉唯讀後用一般的刪除旗標
        self.clear_readonly();
        let d = fsys::FILE_DISPOSITION_INFO { DeleteFile: true };
        let ok = unsafe {
            fsys::SetFileInformationByHandle(
                self.0,
                fsys::FileDispositionInfo,
                &d as *const _ as *const _,
                std::mem::size_of::<fsys::FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok != 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    }
}

/// 從磁碟根目錄下一層開始，逐層不跟隨連結地打開並握住 `dir` 與它的所有上層，
/// 確認每一層都是真的資料夾、不是連結。回傳的 handles drop 之前，這些資料夾都不能被改名或替換。
pub fn pin_dir(dir: &Path) -> io::Result<Vec<Handle>> {
    let mut chain: Vec<&Path> = dir.ancestors().filter(|p| p.parent().is_some()).collect();
    chain.reverse();
    let mut out = Vec::with_capacity(chain.len());
    for p in chain {
        let h = Handle::open(p, Open::Pin)?;
        let i = h.info().ok_or_else(io::Error::last_os_error)?;
        if i.is_reparse() {
            return Err(io::Error::other(format!("路徑經過連結：{}", p.display())));
        }
        if !i.is_dir() {
            return Err(io::Error::other(format!("不是資料夾：{}", p.display())));
        }
        out.push(h);
    }
    Ok(out)
}

/// 檔案的實體身分（磁碟區序號、檔案編號）。同一個檔案用不同路徑（大小寫、硬連結）也會相同。
pub fn file_id(path: &Path) -> Option<(u32, u64)> {
    let h = Handle::open(path, Open::Info).ok()?;
    h.info().map(|i| (i.volume, i.index))
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

/// Known Folder 的實際位置（文件、桌面…被搬到別的磁碟也找得到）。
pub fn known_folders() -> Vec<PathBuf> {
    use windows_sys::Win32::UI::Shell as sh;
    let ids = [
        sh::FOLDERID_Documents,
        sh::FOLDERID_Desktop,
        sh::FOLDERID_Downloads,
        sh::FOLDERID_Pictures,
        sh::FOLDERID_Music,
        sh::FOLDERID_Videos,
    ];
    let mut out = Vec::new();
    for id in ids {
        let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
        let hr = unsafe { sh::SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut p) };
        if hr >= 0 && !p.is_null() {
            let mut n = 0;
            unsafe {
                while *p.add(n) != 0 {
                    n += 1;
                }
                out.push(PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(p, n))));
            }
        }
        if !p.is_null() {
            unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(p as *const _) };
        }
    }
    out
}
