//! 一般權限的平行目錄掃描（FindFirstFileExW + LARGE_FETCH，每個資料夾一個 rayon 工作）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use parking_lot::Mutex;

use crate::tree::{RawNode, RawTree, classify, flag, push_name};
use crate::win;

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    /// 有確定總量時（MFT 筆數）用於進度條
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub stage: Mutex<String>,
    pub current: Mutex<String>,
}

impl Progress {
    pub fn set_stage(&self, s: impl Into<String>) {
        *self.stage.lock() = s.into();
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }
}

struct Shared<'a> {
    raw: Mutex<RawTree>,
    prog: &'a Progress,
}

struct Item {
    off: u32,
    len: u16,
    flags: u8,
    kind: u8,
    size: u64,
}

pub fn worker_threads() -> usize {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    // 目錄列舉是 I/O 密集，執行緒多一點可以增加佇列深度
    (n * 2).clamp(4, 32)
}

/// 掃描 root 底下所有檔案。回傳原始樹（根在索引 0）。
pub fn walk(root: &Path, prog: &Progress) -> RawTree {
    let root_str = root.to_string_lossy().to_string();
    let shared = Shared { raw: Mutex::new(RawTree::with_root(&root_str)), prog };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads())
        .build()
        .expect("thread pool");
    pool.scope(|s| visit(s, root.to_path_buf(), 0, &shared));
    shared.raw.into_inner()
}

fn visit<'s>(s: &rayon::Scope<'s>, path: PathBuf, idx: u32, sh: &'s Shared<'s>) {
    if sh.prog.cancelled() {
        return;
    }
    let mut names: Vec<u8> = Vec::with_capacity(512);
    let mut items: Vec<Item> = Vec::with_capacity(32);
    let mut subdirs: Vec<usize> = Vec::new();
    let mut bytes = 0u64;
    let res = win::list_dir(&path, |e| {
        let off = names.len() as u32;
        let len = push_name(&mut names, e.name) as u16;
        let is_dir = e.is_dir();
        let mut flags = 0u8;
        if is_dir {
            flags |= flag::DIR;
        }
        if e.is_reparse() {
            flags |= flag::REPARSE;
        }
        if e.is_cloud() {
            flags |= flag::CLOUD;
        }
        let (kind, size) = if is_dir {
            (0u8, 0u64)
        } else {
            let nm = &names[off as usize..off as usize + len as usize];
            (classify(nm) as u8, e.local_size())
        };
        bytes += size;
        if is_dir && !e.is_reparse() {
            subdirs.push(items.len());
        }
        items.push(Item { off, len, flags, kind, size });
    });
    if res.is_err() {
        sh.prog.errors.fetch_add(1, Relaxed);
        sh.raw.lock().nodes[idx as usize].flags |= flag::ERROR;
        return;
    }
    let nfiles = (items.len() - subdirs.len()) as u64;
    sh.prog.files.fetch_add(nfiles, Relaxed);
    sh.prog.bytes.fetch_add(bytes, Relaxed);
    let d = sh.prog.dirs.fetch_add(1, Relaxed);
    if d % 128 == 0
        && let Some(mut g) = sh.prog.current.try_lock()
    {
        *g = path.to_string_lossy().into_owned();
    }

    let base = {
        let mut raw = sh.raw.lock();
        let name_base = raw.names.len() as u32;
        raw.names.extend_from_slice(&names);
        let base = raw.nodes.len() as u32;
        raw.nodes.extend(items.iter().map(|it| RawNode {
            size: it.size,
            parent: idx,
            name_start: name_base + it.off,
            name_len: it.len,
            flags: it.flags,
            kind: it.kind,
        }));
        base
    };

    for i in subdirs {
        let it = &items[i];
        let nm = &names[it.off as usize..it.off as usize + it.len as usize];
        // SAFETY: nm 由 push_name 產生（合法 UTF-8 或 OsStr 編碼位元組）
        let os = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(nm) };
        let child = path.join(os);
        let cidx = base + i as u32;
        s.spawn(move |s| visit(s, child, cidx, sh));
    }
}

// ---------------------------------------------------------------------------
// 單純計算大小（清理規則用）

#[derive(Default, Clone, Copy, Debug)]
pub struct SizeStat {
    pub bytes: u64,
    pub files: u64,
    pub errors: u64,
    /// 最新的檔案修改時間（FILETIME）
    pub newest: u64,
}

impl SizeStat {
    pub fn add(mut self, o: SizeStat) -> SizeStat {
        self.bytes += o.bytes;
        self.files += o.files;
        self.errors += o.errors;
        self.newest = self.newest.max(o.newest);
        self
    }
}

/// 平行計算資料夾大小，只算修改時間 <= cutoff 的檔案（cutoff = u64::MAX 表示全部）。
pub fn dir_size(path: &Path, cutoff: u64, cancel: &AtomicBool) -> SizeStat {
    use rayon::prelude::*;
    if cancel.load(Relaxed) {
        return SizeStat::default();
    }
    let mut st = SizeStat::default();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    let res = win::list_dir(path, |e| {
        if e.is_dir() {
            if !e.is_reparse() {
                subdirs.push(path.join(e.name_os()));
            }
        } else if e.mtime <= cutoff {
            st.bytes += e.local_size();
            st.files += 1;
            st.newest = st.newest.max(e.mtime);
        }
    });
    if res.is_err() {
        st.errors += 1;
        return st;
    }
    let sub = subdirs
        .par_iter()
        .map(|p| dir_size(p, cutoff, cancel))
        .reduce(SizeStat::default, SizeStat::add);
    st.add(sub)
}
