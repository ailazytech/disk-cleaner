//! 掃描結果快取：lz4 壓縮的二進位檔，存在 %LOCALAPPDATA%\DiskCleaner\cache。

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::tree::{RawNode, RawTree};

// 版本 3：MFT 大小扣掉沒有稀疏旗標的空洞（舊索引的 $BadClus 會是整顆磁碟大小）
const MAGIC: &[u8; 8] = b"DCIDX\0\0\x03";

pub const METHOD_WALK: u8 = 0;
pub const METHOD_MFT: u8 = 1;

#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub method: u8,
    pub root_index: u32,
    pub root_path: String,
    pub serial: u64,
    pub journal_id: u64,
    pub next_usn: i64,
    /// 建立時間（Unix 秒）
    pub created: u64,
    pub errors: u64,
}

pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("DiskCleaner")
}

pub fn cache_dir() -> PathBuf {
    app_dir().join("cache")
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn key_volume(letter: char, serial: u64) -> String {
    format!("vol-{}-{:016x}", letter.to_ascii_uppercase(), serial)
}

pub fn key_path(path: &Path) -> String {
    format!("path-{:016x}", fnv(&path.to_string_lossy()))
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_for(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.idx"))
}

pub fn save(key: &str, meta: &Meta, raw: &RawTree) -> io::Result<()> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    let path = file_for(key);
    let tmp = path.with_extension("tmp");
    {
        let mut f = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        f.write_all(MAGIC)?;
        let mut enc = lz4_flex::frame::FrameEncoder::new(f);
        let mut hdr = Vec::with_capacity(256);
        hdr.push(meta.method);
        hdr.extend_from_slice(&meta.root_index.to_le_bytes());
        hdr.extend_from_slice(&(meta.root_path.len() as u32).to_le_bytes());
        hdr.extend_from_slice(meta.root_path.as_bytes());
        hdr.extend_from_slice(&meta.serial.to_le_bytes());
        hdr.extend_from_slice(&meta.journal_id.to_le_bytes());
        hdr.extend_from_slice(&meta.next_usn.to_le_bytes());
        hdr.extend_from_slice(&meta.created.to_le_bytes());
        hdr.extend_from_slice(&meta.errors.to_le_bytes());
        hdr.extend_from_slice(&(raw.nodes.len() as u64).to_le_bytes());
        hdr.extend_from_slice(&(raw.names.len() as u64).to_le_bytes());
        enc.write_all(&hdr)?;

        let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);
        for n in &raw.nodes {
            buf.extend_from_slice(&n.size.to_le_bytes());
            buf.extend_from_slice(&n.parent.to_le_bytes());
            buf.extend_from_slice(&n.name_start.to_le_bytes());
            buf.extend_from_slice(&n.name_len.to_le_bytes());
            buf.push(n.flags);
            buf.push(n.kind);
            if buf.len() >= (1 << 20) - 32 {
                enc.write_all(&buf)?;
                buf.clear();
            }
        }
        enc.write_all(&buf)?;
        enc.write_all(&raw.names)?;
        let mut f = enc.finish().map_err(io::Error::other)?;
        f.flush()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load(key: &str) -> io::Result<(Meta, RawTree)> {
    let path = file_for(key);
    let mut f = BufReader::with_capacity(1 << 20, File::open(&path)?);
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::other("快取格式不符"));
    }
    let mut dec = lz4_flex::frame::FrameDecoder::new(f);
    let mut b1 = [0u8; 1];
    let mut b4 = [0u8; 4];
    let mut b8 = [0u8; 8];
    let mut meta = Meta::default();
    dec.read_exact(&mut b1)?;
    meta.method = b1[0];
    dec.read_exact(&mut b4)?;
    meta.root_index = u32::from_le_bytes(b4);
    dec.read_exact(&mut b4)?;
    let rl = u32::from_le_bytes(b4) as usize;
    if rl > 4096 {
        return Err(io::Error::other("快取損毀"));
    }
    let mut rp = vec![0u8; rl];
    dec.read_exact(&mut rp)?;
    meta.root_path = String::from_utf8_lossy(&rp).into_owned();
    let mut r8 = |dec: &mut dyn Read| -> io::Result<u64> {
        dec.read_exact(&mut b8)?;
        Ok(u64::from_le_bytes(b8))
    };
    meta.serial = r8(&mut dec)?;
    meta.journal_id = r8(&mut dec)?;
    meta.next_usn = r8(&mut dec)? as i64;
    meta.created = r8(&mut dec)?;
    meta.errors = r8(&mut dec)?;
    let count = r8(&mut dec)? as usize;
    let names_len = r8(&mut dec)? as usize;
    if count > 200_000_000 || names_len > 16 << 30 {
        return Err(io::Error::other("快取損毀"));
    }

    const REC: usize = 20;
    let mut nodes = Vec::with_capacity(count);
    let mut buf = vec![0u8; REC * 65536];
    let mut left = count;
    while left > 0 {
        let k = left.min(65536);
        let chunk = &mut buf[..k * REC];
        dec.read_exact(chunk)?;
        for r in chunk.chunks_exact(REC) {
            nodes.push(RawNode {
                size: u64::from_le_bytes(r[0..8].try_into().unwrap()),
                parent: u32::from_le_bytes(r[8..12].try_into().unwrap()),
                name_start: u32::from_le_bytes(r[12..16].try_into().unwrap()),
                name_len: u16::from_le_bytes(r[16..18].try_into().unwrap()),
                flags: r[18],
                kind: r[19],
            });
        }
        left -= k;
    }
    let mut names = vec![0u8; names_len];
    dec.read_exact(&mut names)?;
    // 驗證名稱範圍
    for n in &nodes {
        if n.name_start as usize + n.name_len as usize > names.len() {
            return Err(io::Error::other("快取損毀"));
        }
    }
    Ok((meta, RawTree { nodes, names }))
}

/// 快取資料夾總大小
pub fn total_size() -> u64 {
    std::fs::read_dir(cache_dir())
        .map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

pub fn clear_all() {
    let _ = std::fs::remove_dir_all(cache_dir());
}
