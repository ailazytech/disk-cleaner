//! NTFS 極速掃描：直接循序讀取 $MFT 並解析，再用 USN 變更日誌做增量更新。
//! 需要系統管理員權限（開啟 `\\.\C:` 磁碟區）。

use std::io;
use std::sync::atomic::Ordering::Relaxed;

use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
};
use windows_sys::Win32::System::IO::{DeviceIoControl, OVERLAPPED};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_FILE_RECORD, FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL,
    FSCTL_READ_USN_JOURNAL,
};

use crate::scan::Progress;
use crate::tree::{Kind, NONE, RawNode, RawTree, classify, flag, push_name};
use crate::win;

pub const ROOT_RECORD: u32 = 5;
const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct VolumeData {
    pub serial: i64,
    pub number_sectors: i64,
    pub total_clusters: i64,
    pub free_clusters: i64,
    pub total_reserved: i64,
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u32,
    pub bytes_per_record: u32,
    pub clusters_per_record: u32,
    pub mft_valid_len: i64,
    pub mft_start_lcn: i64,
    pub mft2_start_lcn: i64,
    pub mft_zone_start: i64,
    pub mft_zone_end: i64,
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct JournalData {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    pub max_usn: i64,
    pub max_size: u64,
    pub alloc_delta: u64,
}

#[repr(C)]
struct ReadUsnData {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    journal_id: u64,
}

pub struct Volume {
    h: HANDLE,
    pub data: VolumeData,
}

unsafe impl Send for Volume {}
unsafe impl Sync for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.h) };
    }
}

/// 對齊到 4096 的緩衝區（磁碟區直接讀取需要）。
struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}
unsafe impl Send for AlignedBuf {}
impl AlignedBuf {
    fn new(len: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len, 4096).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null());
        AlignedBuf { ptr, len }
    }
    fn as_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        let layout = std::alloc::Layout::from_size_align(self.len, 4096).unwrap();
        unsafe { std::alloc::dealloc(self.ptr, layout) };
    }
}

impl Volume {
    pub fn open(letter: char) -> io::Result<Volume> {
        let path = win::wide(format!("\\\\.\\{letter}:"));
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut vol = Volume { h, data: VolumeData::default() };
        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h,
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null(),
                0,
                &mut vol.data as *mut _ as *mut _,
                std::mem::size_of::<VolumeData>() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if vol.data.bytes_per_record == 0 || vol.data.bytes_per_cluster == 0 {
            return Err(io::Error::other("無效的 NTFS 磁碟區資料"));
        }
        Ok(vol)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            let pos = off + done as u64;
            ov.Anonymous.Anonymous.Offset = pos as u32;
            ov.Anonymous.Anonymous.OffsetHigh = (pos >> 32) as u32;
            let want = (buf.len() - done).min(64 << 20) as u32;
            let mut read = 0u32;
            let ok = unsafe { ReadFile(self.h, buf[done..].as_mut_ptr(), want, &mut read, &mut ov) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            done += read as usize;
        }
        Ok(())
    }

    fn round_sector(&self, n: usize) -> usize {
        let s = self.data.bytes_per_sector.max(512) as usize;
        n.div_ceil(s) * s
    }

    pub fn query_journal(&self) -> Option<JournalData> {
        let mut jd = JournalData::default();
        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.h,
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                &mut jd as *mut _ as *mut _,
                std::mem::size_of::<JournalData>() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { None } else { Some(jd) }
    }

    /// 用 FSCTL_GET_NTFS_FILE_RECORD 取單一 MFT 紀錄。紀錄不在使用中回傳 None。
    fn fetch_record(&self, rec: u32) -> Option<Vec<u8>> {
        let rs = self.data.bytes_per_record as usize;
        let input: i64 = rec as i64;
        let mut out = vec![0u8; 12 + rs + 16];
        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.h,
                FSCTL_GET_NTFS_FILE_RECORD,
                &input as *const _ as *const _,
                8,
                out.as_mut_ptr() as *mut _,
                out.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        let frn = u64::from_le_bytes(out[0..8].try_into().unwrap()) & REF_MASK;
        if frn != rec as u64 {
            return None; // 回傳的是較前面的紀錄 → 這筆已不存在
        }
        let len = u32::from_le_bytes(out[8..12].try_into().unwrap()) as usize;
        let len = len.min(rs).min(out.len() - 12);
        Some(out[12..12 + len].to_vec())
    }
}

// ---------------------------------------------------------------------------
// 紀錄解析

#[inline]
fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
#[inline]
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// 套用 update sequence array（修正每個磁區最後兩個位元組）。
/// strict=false 時，對不上就當作已修正過（FSCTL 取回的紀錄）。
fn fixup(rec: &mut [u8], strict: bool) -> bool {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return false;
    }
    let usa_off = u16_at(rec, 4) as usize;
    let usa_cnt = u16_at(rec, 6) as usize;
    if usa_cnt < 2 || usa_off + usa_cnt * 2 > rec.len() {
        return !strict;
    }
    let stride = rec.len() / (usa_cnt - 1);
    if stride < 512 {
        return !strict;
    }
    let usn = u16_at(rec, usa_off);
    // 先檢查
    for i in 1..usa_cnt {
        let end = i * stride - 2;
        if end + 2 > rec.len() || u16_at(rec, end) != usn {
            return !strict;
        }
    }
    for i in 1..usa_cnt {
        let end = i * stride - 2;
        let src = usa_off + 2 * i;
        rec[end] = rec[src];
        rec[end + 1] = rec[src + 1];
    }
    true
}

#[derive(Clone, Copy)]
struct Parsed {
    rec: u32,
    base: u32,
    parent: u32,
    name_off: u32,
    name_len: u16,
    flags: u8,
    kind: u8,
    size: u64,
    has_name: bool,
    has_attr_list: bool,
}

/// 解析一筆（已修正過的）FILE 紀錄。不在使用中回傳 None。
fn parse_record(rec: &[u8], rec_no: u32, bpc: u64, names: &mut Vec<u8>) -> Option<Parsed> {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return None;
    }
    let rflags = u16_at(rec, 22);
    if rflags & 1 == 0 {
        return None;
    }
    let base_ref = u64_at(rec, 32) & REF_MASK;
    let used = (u32_at(rec, 24) as usize).min(rec.len());
    let mut p = u16_at(rec, 20) as usize;

    let mut size = 0u64;
    let mut si_attrs = 0u32;
    let mut reparse = false;
    let mut has_attr_list = false;
    let mut name: Option<(u32, u8, usize, usize)> = None; // (parent, ns, off, len)

    while p + 16 <= used {
        let t = u32_at(rec, p);
        if t == 0xFFFF_FFFF || t == 0 {
            break;
        }
        let len = u32_at(rec, p + 4) as usize;
        if len < 16 || p + len > used {
            break;
        }
        let nonres = rec[p + 8] != 0;
        let aflags = u16_at(rec, p + 12);
        match t {
            0x10 if !nonres => {
                let v = p + u16_at(rec, p + 20) as usize;
                if v + 36 <= used {
                    si_attrs = u32_at(rec, v + 32);
                }
            }
            0x20 => has_attr_list = true,
            0x30 if !nonres => {
                let v = p + u16_at(rec, p + 20) as usize;
                if v + 66 <= used {
                    let parent = (u64_at(rec, v) & REF_MASK) as u32;
                    let nlen = rec[v + 64] as usize;
                    let ns = rec[v + 65];
                    if v + 66 + nlen * 2 <= used {
                        let better = match name {
                            None => true,
                            Some((_, cur_ns, _, _)) => cur_ns == 2 && ns != 2,
                        };
                        if better {
                            name = Some((parent, ns, v + 66, nlen));
                        }
                    }
                }
            }
            0x80 => {
                if nonres {
                    if len >= 64 && u64_at(rec, p + 16) == 0 {
                        if aflags & (0x0001 | 0x8000) != 0 && len >= 72 {
                            // 稀疏或壓縮：用實際配置的大小
                            size += u64_at(rec, p + 64);
                        } else {
                            // 配置大小，但扣掉沒有實體位置的空洞。$BadClus:$Bad 沒有稀疏旗標，
                            // 帳面大小卻等於整顆磁碟，實際上整段都是空洞。
                            let mut holes = 0u64;
                            each_run(rec, p, |r| {
                                if r.lcn.is_none() {
                                    holes = holes.saturating_add(r.len);
                                }
                            });
                            size += u64_at(rec, p + 40).saturating_sub(holes.saturating_mul(bpc));
                        }
                    }
                } else if len >= 24 {
                    size += u32_at(rec, p + 16) as u64;
                }
            }
            0xC0 => reparse = true,
            _ => {}
        }
        p += len;
    }

    let mut out = Parsed {
        rec: rec_no,
        base: if base_ref != 0 { base_ref as u32 } else { NONE },
        parent: NONE,
        name_off: 0,
        name_len: 0,
        flags: 0,
        kind: 0,
        size,
        has_name: false,
        has_attr_list,
    };
    if rflags & 2 != 0 {
        out.flags |= flag::DIR;
    }
    if reparse || si_attrs & win::ATTR_REPARSE != 0 {
        out.flags |= flag::REPARSE;
    }
    if si_attrs & win::ATTR_CLOUD_MASK != 0 {
        out.flags |= flag::CLOUD;
    }
    if let Some((parent, _, off, nlen)) = name {
        let mut wbuf = [0u16; 255];
        for (i, w) in wbuf.iter_mut().enumerate().take(nlen) {
            *w = u16_at(rec, off + i * 2);
        }
        let start = names.len();
        let l = push_name(names, &wbuf[..nlen]);
        out.parent = parent;
        out.name_off = start as u32;
        out.name_len = l as u16;
        out.has_name = true;
        if out.flags & flag::DIR == 0 {
            out.kind = classify(&names[start..start + l]) as u8;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// $MFT 本身的位置（data runs）

#[derive(Clone, Copy, Debug)]
struct Run {
    vcn: u64,
    lcn: Option<u64>,
    len: u64,
}

fn decode_runs(rec: &[u8], attr: usize, out: &mut Vec<Run>) {
    each_run(rec, attr, |r| out.push(r));
}

fn each_run(rec: &[u8], attr: usize, mut f: impl FnMut(Run)) {
    let start_vcn = u64_at(rec, attr + 16);
    let mut p = attr + u16_at(rec, attr + 32) as usize;
    let end = attr + u32_at(rec, attr + 4) as usize;
    let mut vcn = start_vcn;
    let mut lcn: i64 = 0;
    while p < end && rec[p] != 0 {
        let h = rec[p];
        let ls = (h & 0xF) as usize;
        let os = (h >> 4) as usize;
        if ls == 0 || ls > 8 || os > 8 || p + 1 + ls + os > end {
            break;
        }
        let mut length = 0u64;
        for i in 0..ls {
            length |= (rec[p + 1 + i] as u64) << (8 * i);
        }
        let run_lcn = if os == 0 {
            None
        } else {
            let mut off = 0i64;
            for i in 0..os {
                off |= (rec[p + 1 + ls + i] as i64) << (8 * i);
            }
            // 符號延伸
            let shift = 64 - 8 * os as u32;
            off = (off << shift) >> shift;
            lcn += off;
            Some(lcn as u64)
        };
        f(Run { vcn, lcn: run_lcn, len: length });
        vcn += length;
        p += 1 + ls + os;
    }
}

fn find_data_attrs(rec: &[u8], out: &mut Vec<Run>, attr_list: &mut Option<(bool, usize)>) {
    let used = (u32_at(rec, 24) as usize).min(rec.len());
    let mut p = u16_at(rec, 20) as usize;
    while p + 16 <= used {
        let t = u32_at(rec, p);
        if t == 0xFFFF_FFFF || t == 0 {
            break;
        }
        let len = u32_at(rec, p + 4) as usize;
        if len < 16 || p + len > used {
            break;
        }
        let nonres = rec[p + 8] != 0;
        let name_len = rec[p + 9];
        if t == 0x80 && name_len == 0 && nonres {
            decode_runs(rec, p, out);
        }
        if t == 0x20 {
            *attr_list = Some((nonres, p));
        }
        p += len;
    }
}

fn mft_runs(vol: &Volume) -> io::Result<Vec<Run>> {
    let rs = vol.data.bytes_per_record as usize;
    let bpc = vol.data.bytes_per_cluster as u64;
    let mut buf = AlignedBuf::new(vol.round_sector(rs).max(4096));
    let off = vol.data.mft_start_lcn as u64 * bpc;
    let n = vol.round_sector(rs);
    vol.read_at(off, &mut buf.as_mut()[..n])?;
    let rec = &mut buf.as_mut()[..rs];
    if !fixup(rec, true) {
        return Err(io::Error::other("無法解析 $MFT 紀錄"));
    }
    let mut runs = Vec::new();
    let mut alist = None;
    find_data_attrs(rec, &mut runs, &mut alist);

    // $MFT 很破碎時，其餘的 data runs 在 attribute list 指向的延伸紀錄裡
    if let Some((nonres, p)) = alist {
        let list: Vec<u8> = if !nonres {
            let v = p + u16_at(rec, p + 20) as usize;
            let vl = u32_at(rec, p + 16) as usize;
            rec[v..(v + vl).min(rs)].to_vec()
        } else {
            let mut lr = Vec::new();
            decode_runs(rec, p, &mut lr);
            let real = u64_at(rec, p + 48) as usize;
            let mut data = Vec::new();
            for r in lr {
                if let Some(lcn) = r.lcn {
                    let bytes = (r.len * bpc) as usize;
                    let mut b = AlignedBuf::new(vol.round_sector(bytes).max(4096));
                    vol.read_at(lcn * bpc, &mut b.as_mut()[..vol.round_sector(bytes)])?;
                    data.extend_from_slice(&b.as_mut()[..bytes]);
                }
            }
            data.truncate(real);
            data
        };
        let mut q = 0usize;
        let mut ext_recs = Vec::new();
        while q + 26 <= list.len() {
            let t = u32_at(&list, q);
            let elen = u16_at(&list, q + 4) as usize;
            if elen < 26 {
                break;
            }
            let name_len = list[q + 6];
            let r = (u64_at(&list, q + 16) & REF_MASK) as u32;
            if t == 0x80 && name_len == 0 && r != 0 {
                ext_recs.push(r);
            }
            q += elen;
        }
        for r in ext_recs {
            // 用目前已知的 runs 找出這筆紀錄的位置
            let byte = r as u64 * rs as u64;
            let vcn = byte / bpc;
            let Some(run) = runs.iter().find(|x| x.vcn <= vcn && vcn < x.vcn + x.len) else { continue };
            let Some(lcn) = run.lcn else { continue };
            let pos = (lcn + (vcn - run.vcn)) * bpc + byte % bpc;
            let mut b = AlignedBuf::new(vol.round_sector(rs).max(4096));
            let aligned = pos / 512 * 512;
            let skip = (pos - aligned) as usize;
            let need = vol.round_sector(skip + rs);
            let mut bb = AlignedBuf::new(need.max(4096));
            vol.read_at(aligned, &mut bb.as_mut()[..need])?;
            b.as_mut()[..rs].copy_from_slice(&bb.as_mut()[skip..skip + rs]);
            let er = &mut b.as_mut()[..rs];
            if fixup(er, true) {
                let mut dummy = None;
                find_data_attrs(er, &mut runs, &mut dummy);
            }
        }
        runs.sort_by_key(|r| r.vcn);
        runs.dedup_by_key(|r| r.vcn);
    }
    if runs.is_empty() {
        return Err(io::Error::other("找不到 $MFT 的資料位置"));
    }
    Ok(runs)
}

// ---------------------------------------------------------------------------
// 完整掃描

struct Merge {
    nodes: Vec<RawNode>,
    names: Vec<u8>,
    state: Vec<u8>, // bit0: 使用中的基本紀錄, bit1: 有名稱
    ext: Vec<Parsed>,
}

const EMPTY: RawNode = RawNode { size: 0, parent: NONE, name_start: 0, name_len: 0, flags: 0, kind: 0 };

impl Merge {
    fn put(&mut self, chunk_names: &[u8], items: &[Parsed]) {
        let base = self.names.len() as u32;
        self.names.extend_from_slice(chunk_names);
        for p in items {
            let mut p = *p;
            p.name_off += base;
            if p.base == NONE {
                let i = p.rec as usize;
                if i >= self.nodes.len() {
                    continue;
                }
                self.nodes[i] = RawNode {
                    size: p.size,
                    parent: if p.has_name { p.parent } else { NONE },
                    name_start: p.name_off,
                    name_len: p.name_len,
                    flags: p.flags,
                    kind: p.kind,
                };
                self.state[i] = 1 | if p.has_name { 2 } else { 0 };
            } else {
                self.ext.push(p);
            }
        }
    }

    fn finish(mut self, root_name: &str) -> RawTree {
        for e in std::mem::take(&mut self.ext) {
            let b = e.base as usize;
            if b >= self.nodes.len() || self.state[b] & 1 == 0 {
                continue;
            }
            self.nodes[b].size += e.size;
            if self.state[b] & 2 == 0 && e.has_name {
                let n = &mut self.nodes[b];
                n.parent = e.parent;
                n.name_start = e.name_off;
                n.name_len = e.name_len;
                if n.flags & flag::DIR == 0 {
                    n.kind = e.kind;
                }
                self.state[b] |= 2;
            }
        }
        let mut raw = RawTree { nodes: self.nodes, names: self.names };
        set_root(&mut raw, root_name);
        raw
    }
}

fn set_root(raw: &mut RawTree, root_name: &str) {
    let r = ROOT_RECORD as usize;
    if r >= raw.nodes.len() {
        return;
    }
    let start = raw.names.len() as u32;
    raw.names.extend_from_slice(root_name.as_bytes());
    let n = &mut raw.nodes[r];
    n.parent = NONE;
    n.name_start = start;
    n.name_len = root_name.len() as u16;
    n.flags = flag::DIR;
    n.kind = Kind::Dir as u8;
    n.size = 0;
}

/// 讀取整個 $MFT，回傳以 MFT 紀錄編號為索引的原始樹（根 = 紀錄 5）。
pub fn scan_volume(vol: &Volume, root_name: &str, prog: &Progress) -> io::Result<RawTree> {
    let rs = vol.data.bytes_per_record as usize;
    let bpc = vol.data.bytes_per_cluster as u64;
    let runs = mft_runs(vol)?;
    let total = (vol.data.mft_valid_len.max(0) as u64 / rs as u64) as usize;
    prog.total.store(total as u64, Relaxed);
    prog.done.store(0, Relaxed);

    let merge = Mutex::new(Merge {
        nodes: vec![EMPTY; total],
        names: Vec::with_capacity(total * 24),
        state: vec![0u8; total],
        ext: Vec::new(),
    });

    const CHUNK: usize = 4 << 20;
    const NBUF: usize = 12;
    let (free_tx, free_rx) = crossbeam_channel::bounded::<AlignedBuf>(NBUF);
    for _ in 0..NBUF {
        free_tx.send(AlignedBuf::new(CHUNK)).unwrap();
    }
    let mut err: Option<io::Error> = None;

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 16))
        .build()
        .map_err(io::Error::other)?;

    pool.in_place_scope(|s| {
        'runs: for run in &runs {
            let Some(lcn) = run.lcn else { continue };
            let run_bytes = run.len * bpc;
            let first_rec = (run.vcn * bpc) / rs as u64;
            if first_rec as usize >= total {
                continue;
            }
            let max_bytes = (total as u64 - first_rec) * rs as u64;
            let mut remaining = run_bytes.min(max_bytes);
            let mut off = lcn * bpc;
            let mut rec = first_rec as usize;
            while remaining > 0 {
                if prog.cancelled() {
                    break 'runs;
                }
                let n = (remaining as usize).min(CHUNK);
                let n = n / rs * rs;
                if n == 0 {
                    break;
                }
                let mut buf = free_rx.recv().unwrap();
                let rd = vol.round_sector(n).min(CHUNK);
                if let Err(e) = vol.read_at(off, &mut buf.as_mut()[..rd]) {
                    err = Some(e);
                    let _ = free_tx.send(buf);
                    break 'runs;
                }
                let tx = free_tx.clone();
                let merge = &merge;
                let first = rec;
                s.spawn(move |_| {
                    let data = buf.as_mut();
                    let mut names = Vec::with_capacity(n / rs * 24);
                    let mut items = Vec::with_capacity(n / rs);
                    for i in 0..n / rs {
                        let r = &mut data[i * rs..(i + 1) * rs];
                        if !fixup(r, true) {
                            continue;
                        }
                        if let Some(p) = parse_record(r, (first + i) as u32, bpc, &mut names) {
                            items.push(p);
                        }
                    }
                    merge.lock().put(&names, &items);
                    prog.done.fetch_add((n / rs) as u64, Relaxed);
                    prog.files.fetch_add(items.len() as u64, Relaxed);
                    let _ = tx.send(buf);
                });
                off += n as u64;
                rec += n / rs;
                remaining -= n as u64;
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    if prog.cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "已取消"));
    }
    Ok(merge.into_inner().finish(root_name))
}

// ---------------------------------------------------------------------------
// USN 增量更新

pub enum Changes {
    /// 變動的 MFT 紀錄編號，以及讀到的最新 USN
    List(Vec<u32>, i64),
    /// 日誌已被截斷或重建，需要完整重掃
    NeedFull,
}

/// 讀取 from_usn 之後的所有變更。
pub fn read_changes(vol: &Volume, jd: &JournalData, from_usn: i64) -> Changes {
    if from_usn < jd.first_usn || from_usn > jd.next_usn {
        return Changes::NeedFull;
    }
    let mut recs: Vec<u32> = Vec::new();
    let mut start = from_usn;
    let mut buf = vec![0u8; 1 << 20];
    while start < jd.next_usn {
        let input = ReadUsnData {
            start_usn: start,
            reason_mask: 0xFFFF_FFFF,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait_for: 0,
            journal_id: jd.id,
        };
        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                vol.h,
                FSCTL_READ_USN_JOURNAL,
                &input as *const _ as *const _,
                std::mem::size_of::<ReadUsnData>() as u32,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Changes::NeedFull;
        }
        let ret = ret as usize;
        if ret < 8 {
            break;
        }
        let next = i64::from_le_bytes(buf[0..8].try_into().unwrap());
        let mut p = 8usize;
        while p + 60 <= ret {
            let len = u32_at(&buf, p) as usize;
            if len < 60 || p + len > ret {
                break;
            }
            // V2 與 V3 的 FileReferenceNumber 都從偏移 8 開始（V3 取低 64 位元即 NTFS FRN）
            let frn = u64_at(&buf, p + 8);
            recs.push((frn & REF_MASK) as u32);
            p += len;
        }
        if recs.len() > 2_000_000 {
            return Changes::NeedFull;
        }
        if next <= start {
            break;
        }
        start = next;
    }
    recs.sort_unstable();
    recs.dedup();
    Changes::List(recs, start)
}

/// 把變動的紀錄重新讀取並套用到原始樹。回傳實際套用的筆數。
pub fn apply_changes(vol: &Volume, raw: &mut RawTree, recs: &[u32], root_name: &str) -> usize {
    use rayon::prelude::*;
    let bpc = vol.data.bytes_per_cluster as u64;
    // 平行取回紀錄並解析
    let parsed: Vec<(u32, Option<(Vec<u8>, Parsed)>)> = recs
        .par_iter()
        .map(|&r| {
            let res = vol.fetch_record(r).and_then(|mut buf| {
                fixup(&mut buf, false);
                let mut names = Vec::new();
                parse_record(&buf, r, bpc, &mut names).map(|p| (names, p))
            });
            (r, res)
        })
        .collect();

    let mut applied = 0;
    for (r, res) in parsed {
        let i = r as usize;
        if r == ROOT_RECORD {
            continue;
        }
        if i >= raw.nodes.len() {
            raw.nodes.resize(i + 1, EMPTY);
        }
        match res {
            Some((names, p)) if p.base == NONE => {
                let base = raw.names.len() as u32;
                raw.names.extend_from_slice(&names);
                let old = raw.nodes[i];
                let keep_size = p.has_attr_list && p.size == 0;
                raw.nodes[i] = RawNode {
                    size: if keep_size { old.size } else { p.size },
                    parent: if p.has_name { p.parent } else { old.parent },
                    name_start: if p.has_name { base + p.name_off } else { old.name_start },
                    name_len: if p.has_name { p.name_len } else { old.name_len },
                    flags: p.flags,
                    kind: if p.has_name { p.kind } else { old.kind },
                };
                applied += 1;
            }
            Some(_) => {} // 延伸紀錄，略過
            None => {
                raw.nodes[i].parent = NONE;
                applied += 1;
            }
        }
    }
    // 根名稱維持
    let r = ROOT_RECORD as usize;
    if r < raw.nodes.len() && raw.nodes[r].parent != NONE {
        set_root(raw, root_name);
    }
    applied
}

/// 名稱緩衝區壓實（增量更新後舊名稱會留下垃圾）。
pub fn compact_names(raw: &mut RawTree) {
    let live: usize = raw.nodes.iter().filter(|n| n.parent != NONE || n.name_len > 0).map(|n| n.name_len as usize).sum();
    if raw.names.len() < live * 3 / 2 + (1 << 20) {
        return;
    }
    let mut names = Vec::with_capacity(live);
    for n in raw.nodes.iter_mut() {
        let s = n.name_start as usize;
        let e = s + n.name_len as usize;
        let start = names.len() as u32;
        names.extend_from_slice(&raw.names[s..e]);
        n.name_start = start;
    }
    raw.names = names;
}

// ---------------------------------------------------------------------------
// 自我測試

/// 組一筆只有一個非常駐 $DATA 的檔案紀錄。runs：(叢集數, 是否有實體位置)。
fn test_record(runs: &[(u32, bool)], attr_flags: u16, bpc: u64) -> Vec<u8> {
    let mut r = vec![0u8; 1024];
    r[0..4].copy_from_slice(b"FILE");
    r[20..22].copy_from_slice(&56u16.to_le_bytes());
    r[22..24].copy_from_slice(&1u16.to_le_bytes());
    let mut rl = Vec::new();
    let mut clusters = 0u64;
    for &(len, real) in runs {
        if real {
            rl.push(0x44);
            rl.extend(len.to_le_bytes());
            rl.extend(1000i32.to_le_bytes());
        } else {
            rl.push(0x04);
            rl.extend(len.to_le_bytes());
        }
        clusters += len as u64;
    }
    rl.push(0);
    let a = 56;
    let alen = (64 + rl.len()).div_ceil(8) * 8;
    r[a..a + 4].copy_from_slice(&0x80u32.to_le_bytes());
    r[a + 4..a + 8].copy_from_slice(&(alen as u32).to_le_bytes());
    r[a + 8] = 1;
    r[a + 12..a + 14].copy_from_slice(&attr_flags.to_le_bytes());
    r[a + 24..a + 32].copy_from_slice(&(clusters.saturating_sub(1)).to_le_bytes());
    r[a + 32..a + 34].copy_from_slice(&64u16.to_le_bytes());
    for o in [40, 48, 56] {
        r[a + o..a + o + 8].copy_from_slice(&(clusters * bpc).to_le_bytes());
    }
    r[a + 64..a + 64 + rl.len()].copy_from_slice(&rl);
    let end = a + alen;
    r[end..end + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    r[24..28].copy_from_slice(&((end + 8) as u32).to_le_bytes());
    r
}

pub fn selftest() -> Vec<(&'static str, bool)> {
    let bpc = 4096u64;
    let size = |runs: &[(u32, bool)]| {
        let mut names = Vec::new();
        parse_record(&test_record(runs, 0, bpc), 100, bpc, &mut names).map(|p| p.size)
    };
    vec![
        ("MFT：一般檔案用配置大小", size(&[(10, true)]) == Some(10 * bpc)),
        ("MFT：沒有稀疏旗標的空洞不算（$BadClus:$Bad）", size(&[(500_000_000, false)]) == Some(0)),
        ("MFT：資料與空洞混合時只算資料", size(&[(10, true), (90, false), (5, true)]) == Some(15 * bpc)),
    ]
}
