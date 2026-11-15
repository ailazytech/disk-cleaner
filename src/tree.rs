//! 掃描結果樹：扁平陣列，同一資料夾的子節點連續存放、依大小由大到小排序。

use std::borrow::Cow;
use std::collections::{BinaryHeap, HashMap};
use std::ffi::OsStr;
use std::path::PathBuf;

pub const NONE: u32 = u32::MAX;

pub mod flag {
    pub const DIR: u8 = 1;
    pub const REPARSE: u8 = 2;
    pub const ERROR: u8 = 4;
    pub const CLOUD: u8 = 8;
    pub const DELETED: u8 = 16;
}

// ---------------------------------------------------------------------------
// 檔案類型

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Dir = 0,
    Video,
    Image,
    Audio,
    Archive,
    DiskImage,
    Document,
    Code,
    Binary,
    Data,
    Game,
    Temp,
    System,
    Other,
}

pub const KIND_COUNT: usize = 14;

impl Kind {
    pub const ALL: [Kind; KIND_COUNT] = [
        Kind::Dir,
        Kind::Video,
        Kind::Image,
        Kind::Audio,
        Kind::Archive,
        Kind::DiskImage,
        Kind::Document,
        Kind::Code,
        Kind::Binary,
        Kind::Data,
        Kind::Game,
        Kind::Temp,
        Kind::System,
        Kind::Other,
    ];

    pub fn from_u8(v: u8) -> Kind {
        Kind::ALL.get(v as usize).copied().unwrap_or(Kind::Other)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Dir => "資料夾",
            Kind::Video => "影片",
            Kind::Image => "圖片",
            Kind::Audio => "音訊",
            Kind::Archive => "壓縮檔",
            Kind::DiskImage => "映像檔／虛擬磁碟",
            Kind::Document => "文件",
            Kind::Code => "程式碼／設定",
            Kind::Binary => "執行檔／函式庫",
            Kind::Data => "資料庫／資料",
            Kind::Game => "遊戲資源",
            Kind::Temp => "暫存／記錄",
            Kind::System => "系統檔",
            Kind::Other => "其他",
        }
    }

    pub fn color(self) -> [u8; 3] {
        match self {
            Kind::Dir => [90, 96, 110],
            Kind::Video => [232, 104, 74],
            Kind::Image => [246, 189, 22],
            Kind::Audio => [146, 112, 202],
            Kind::Archive => [255, 157, 77],
            Kind::DiskImage => [217, 79, 112],
            Kind::Document => [90, 216, 166],
            Kind::Code => [91, 143, 249],
            Kind::Binary => [109, 200, 236],
            Kind::Data => [38, 154, 153],
            Kind::Game => [160, 217, 17],
            Kind::Temp => [201, 162, 126],
            Kind::System => [140, 140, 150],
            Kind::Other => [126, 140, 154],
        }
    }
}

/// 依檔名判斷類型（name 為 UTF-8/WTF-8 位元組）。
pub fn classify(name: &[u8]) -> Kind {
    // 特殊系統檔
    if name.len() >= 11 && name.len() <= 12 {
        let lower: Vec<u8> = name.iter().map(|c| c.to_ascii_lowercase()).collect();
        match &lower[..] {
            b"pagefile.sys" | b"hiberfil.sys" | b"swapfile.sys" => return Kind::System,
            _ => {}
        }
    }
    let Some(dot) = name.iter().rposition(|&c| c == b'.') else {
        return Kind::Other;
    };
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 10 {
        return Kind::Other;
    }
    let mut buf = [0u8; 10];
    for (i, c) in ext.iter().enumerate() {
        buf[i] = c.to_ascii_lowercase();
    }
    let ext = &buf[..ext.len()];
    match ext {
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"flv" | b"webm" | b"m4v" | b"ts" | b"mts"
        | b"m2ts" | b"mpg" | b"mpeg" | b"3gp" | b"vob" | b"rmvb" | b"rm" | b"braw" | b"r3d" => {
            Kind::Video
        }
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"bmp" | b"webp" | b"tif" | b"tiff" | b"heic"
        | b"heif" | b"raw" | b"cr2" | b"cr3" | b"nef" | b"arw" | b"dng" | b"orf" | b"rw2"
        | b"psd" | b"psb" | b"ai" | b"svg" | b"ico" | b"exr" | b"hdr" | b"tga" | b"dds"
        | b"avif" | b"jxl" | b"kra" | b"xcf" | b"clip" => Kind::Image,
        b"mp3" | b"flac" | b"wav" | b"aac" | b"ogg" | b"m4a" | b"wma" | b"opus" | b"aiff"
        | b"aif" | b"ape" | b"mid" | b"midi" | b"dsf" | b"dff" => Kind::Audio,
        b"zip" | b"rar" | b"7z" | b"tar" | b"gz" | b"tgz" | b"bz2" | b"xz" | b"zst" | b"lz4"
        | b"cab" | b"lzma" | b"z" | b"001" | b"crate" | b"nupkg" | b"whl" | b"apk" | b"xapk" => {
            Kind::Archive
        }
        b"iso" | b"img" | b"vhd" | b"vhdx" | b"avhdx" | b"vmdk" | b"vdi" | b"qcow2" | b"wim"
        | b"esd" | b"swm" | b"vmem" | b"vmsn" | b"vmss" | b"nrg" | b"bin" | b"dmg" => {
            Kind::DiskImage
        }
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"xlsm" | b"ppt" | b"pptx" | b"odt"
        | b"ods" | b"odp" | b"txt" | b"md" | b"rtf" | b"epub" | b"mobi" | b"csv" | b"pages"
        | b"key" | b"numbers" | b"one" | b"hwp" | b"chm" => Kind::Document,
        b"rs" | b"c" | b"h" | b"cc" | b"cpp" | b"hpp" | b"cxx" | b"cs" | b"java" | b"kt"
        | b"kts" | b"py" | b"pyi" | b"js" | b"mjs" | b"cjs" | b"jsx" | b"tsx" | b"go" | b"rb"
        | b"php" | b"swift" | b"json" | b"toml" | b"yaml" | b"yml" | b"xml" | b"html"
        | b"htm" | b"css" | b"scss" | b"less" | b"vue" | b"svelte" | b"sql" | b"sh"
        | b"ps1" | b"bat" | b"cmd" | b"lua" | b"dart" | b"ini" | b"cfg" | b"conf" | b"map"
        | b"gradle" | b"lock" | b"proto" | b"zig" | b"ex" | b"exs" | b"hs" | b"scala"
        | b"r" | b"ipynb" | b"glsl" | b"hlsl" | b"wgsl" => Kind::Code,
        b"exe" | b"dll" | b"sys" | b"msi" | b"msix" | b"msp" | b"appx" | b"appxbundle"
        | b"msixbundle" | b"pdb" | b"lib" | b"a" | b"o" | b"obj" | b"so" | b"dylib" | b"node"
        | b"pyd" | b"jar" | b"class" | b"wasm" | b"rlib" | b"rmeta" | b"ocx" | b"mui"
        | b"efi" | b"drv" | b"pch" | b"ipch" | b"ilk" | b"exp" | b"pyc" | b"winmd" => {
            Kind::Binary
        }
        b"db" | b"sqlite" | b"sqlite3" | b"mdb" | b"accdb" | b"ldf" | b"mdf" | b"ndf"
        | b"edb" | b"dat" | b"ldb" | b"sst" | b"parquet" | b"npy" | b"npz" | b"h5" | b"hdf5"
        | b"pt" | b"pth" | b"ckpt" | b"safetensors" | b"gguf" | b"onnx" | b"pb" | b"tflite"
        | b"bak" | b"pst" | b"ost" | b"blob" | b"idx" | b"pack" | b"jsonl" => Kind::Data,
        b"pak" | b"upk" | b"uasset" | b"umap" | b"ucas" | b"utoc" | b"bsa" | b"ba2" | b"vpk"
        | b"forge" | b"bundle" | b"assets" | b"ress" | b"unity3d" | b"bnk" | b"wem" | b"pck"
        | b"rpf" | b"big" | b"arc" | b"cpk" | b"xnb" => Kind::Game,
        b"tmp" | b"temp" | b"log" | b"etl" | b"dmp" | b"mdmp" | b"cache" | b"old" | b"crdownload"
        | b"part" | b"partial" | b"evtx" => Kind::Temp,
        _ => Kind::Other,
    }
}

// ---------------------------------------------------------------------------
// 原始樹（掃描器輸出，只有 parent 指標，順序不限）

#[derive(Clone, Copy)]
pub struct RawNode {
    pub size: u64,
    pub parent: u32,
    pub name_start: u32,
    pub name_len: u16,
    pub flags: u8,
    pub kind: u8,
}

pub struct RawTree {
    pub nodes: Vec<RawNode>,
    pub names: Vec<u8>,
}

impl RawTree {
    pub fn with_root(root: &str) -> Self {
        let mut t = RawTree { nodes: Vec::with_capacity(1 << 16), names: Vec::with_capacity(1 << 20) };
        t.names.extend_from_slice(root.as_bytes());
        t.nodes.push(RawNode {
            size: 0,
            parent: NONE,
            name_start: 0,
            name_len: root.len() as u16,
            flags: flag::DIR,
            kind: Kind::Dir as u8,
        });
        t
    }
}

// ---------------------------------------------------------------------------
// 完成的樹

#[derive(Clone, Copy)]
pub struct Node {
    pub size: u64,
    pub parent: u32,
    pub first_child: u32,
    pub child_count: u32,
    /// 子樹中的檔案數
    pub files: u32,
    name_start: u32,
    name_len: u16,
    pub flags: u8,
    pub kind: u8,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.flags & flag::DIR != 0
    }
    pub fn kind(&self) -> Kind {
        Kind::from_u8(self.kind)
    }
    pub fn children(&self) -> std::ops::Range<u32> {
        self.first_child..self.first_child + self.child_count
    }
}

#[derive(Clone)]
pub struct ExtStat {
    pub ext: String,
    pub size: u64,
    pub count: u64,
    pub kind: Kind,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    names: Vec<u8>,
    pub stats: Stats,
    pub errors: u64,
    #[allow(dead_code)]
    pub method: &'static str,
}

impl Tree {
    pub fn name_bytes(&self, i: u32) -> &[u8] {
        let n = &self.nodes[i as usize];
        &self.names[n.name_start as usize..n.name_start as usize + n.name_len as usize]
    }

    pub fn name_os(&self, i: u32) -> &OsStr {
        // SAFETY: 名稱位元組都來自 OsStr::as_encoded_bytes 或合法 UTF-8。
        unsafe { OsStr::from_encoded_bytes_unchecked(self.name_bytes(i)) }
    }

    pub fn name(&self, i: u32) -> Cow<'_, str> {
        self.name_os(i).to_string_lossy()
    }

    pub fn path(&self, i: u32) -> PathBuf {
        let mut chain = Vec::new();
        let mut cur = i;
        while cur != NONE {
            chain.push(cur);
            cur = self.nodes[cur as usize].parent;
        }
        let mut p = PathBuf::new();
        for &c in chain.iter().rev() {
            p.push(self.name_os(c));
        }
        p
    }

    /// 由根到 i 的節點鏈（含兩端）。
    pub fn ancestry(&self, i: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut cur = i;
        while cur != NONE {
            chain.push(cur);
            cur = self.nodes[cur as usize].parent;
        }
        chain.reverse();
        chain
    }

    /// 刪除後更新：節點標記刪除，祖先扣除大小。
    pub fn mark_deleted(&mut self, i: u32) {
        let (size, files) = {
            let n = &self.nodes[i as usize];
            (n.size, n.files)
        };
        let mut cur = self.nodes[i as usize].parent;
        while cur != NONE {
            let n = &mut self.nodes[cur as usize];
            n.size = n.size.saturating_sub(size);
            n.files = n.files.saturating_sub(files);
            cur = n.parent;
        }
        let n = &mut self.nodes[i as usize];
        n.size = 0;
        n.files = 0;
        n.flags |= flag::DELETED;
        n.child_count = 0;
    }

    /// 找子節點（依名稱，不分大小寫）。
    pub fn find_child(&self, parent: u32, name: &str) -> Option<u32> {
        self.nodes[parent as usize]
            .children()
            .find(|&c| self.name(c).eq_ignore_ascii_case(name))
    }

    /// 依完整路徑找節點（路徑需在根之下）。
    pub fn find_path(&self, path: &std::path::Path) -> Option<u32> {
        let root = PathBuf::from(self.name_os(0));
        let rel = path.strip_prefix(&root).ok()?;
        let mut cur = 0u32;
        for comp in rel.components() {
            let s = comp.as_os_str().to_string_lossy();
            cur = self.find_child(cur, &s)?;
        }
        Some(cur)
    }
}

/// 把原始樹整理成完成的樹：計算子樹大小、依大小排序、子節點連續排列。
pub fn finalize(raw_tree: &RawTree, root: u32, errors: u64, method: &'static str) -> Tree {
    let raw = &raw_tree.nodes;
    let names = raw_tree.names.clone();
    let n = raw.len();

    // CSR：每個節點的子節點清單
    let mut offs = vec![0u32; n + 1];
    for r in raw.iter() {
        if r.parent != NONE {
            offs[r.parent as usize + 1] += 1;
        }
    }
    for i in 0..n {
        offs[i + 1] += offs[i];
    }
    let mut fill = offs.clone();
    let mut kids = vec![0u32; offs[n] as usize];
    for (i, r) in raw.iter().enumerate() {
        if r.parent != NONE {
            let p = r.parent as usize;
            kids[fill[p] as usize] = i as u32;
            fill[p] += 1;
        }
    }
    drop(fill);

    // BFS 順序（父在子之前）
    let mut order = Vec::with_capacity(n);
    order.push(root);
    let mut head = 0;
    while head < order.len() {
        let x = order[head] as usize;
        order.extend_from_slice(&kids[offs[x] as usize..offs[x + 1] as usize]);
        head += 1;
    }

    // 子樹大小與檔案數
    let mut size = vec![0u64; n];
    let mut files = vec![0u32; n];
    for (i, r) in raw.iter().enumerate() {
        if r.flags & flag::DIR == 0 {
            size[i] = r.size;
            files[i] = 1;
        }
    }
    for &x in order.iter().rev() {
        let p = raw[x as usize].parent;
        if p != NONE {
            size[p as usize] += size[x as usize];
            files[p as usize] += files[x as usize];
        }
    }

    // 每個節點的子節點依大小排序
    for x in 0..n {
        let s = &mut kids[offs[x] as usize..offs[x + 1] as usize];
        if s.len() > 1 {
            s.sort_unstable_by(|&a, &b| size[b as usize].cmp(&size[a as usize]));
        }
    }

    // 第二次 BFS：輸出最終陣列
    let mut out: Vec<Node> = Vec::with_capacity(order.len());
    let mut old_of_new: Vec<u32> = Vec::with_capacity(order.len());
    drop(order);
    let mk = |old: u32, parent: u32| {
        let r = &raw[old as usize];
        Node {
            size: size[old as usize],
            parent,
            first_child: NONE,
            child_count: 0,
            files: files[old as usize],
            name_start: r.name_start,
            name_len: r.name_len,
            flags: r.flags,
            kind: r.kind,
        }
    };
    out.push(mk(root, NONE));
    old_of_new.push(root);
    let mut head = 0usize;
    while head < out.len() {
        let old = old_of_new[head] as usize;
        let ks = &kids[offs[old] as usize..offs[old + 1] as usize];
        out[head].first_child = out.len() as u32;
        out[head].child_count = ks.len() as u32;
        for &k in ks {
            out.push(mk(k, head as u32));
            old_of_new.push(k);
        }
        head += 1;
    }

    let mut tree = Tree { nodes: out, names, stats: Stats::default(), errors, method };
    tree.stats = tree.stats_of(0);
    tree
}

#[derive(Clone, Default)]
pub struct Stats {
    pub kinds: [(u64, u64); KIND_COUNT],
    pub exts: Vec<ExtStat>,
    pub largest: Vec<u32>,
    pub dirs: u64,
    pub files: u64,
}

impl Tree {
    /// 計算某個子樹的類型分布、副檔名統計與最大檔案。
    pub fn stats_of(&self, root: u32) -> Stats {
        let mut kinds = [(0u64, 0u64); KIND_COUNT];
        let mut exts: HashMap<[u8; 12], (u64, u64, u8)> = HashMap::new();
        let mut heap: BinaryHeap<std::cmp::Reverse<(u64, u32)>> = BinaryHeap::new();
        const TOP: usize = 500;
        let mut dirs = 0u64;
        let mut files = 0u64;
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            let n = self.nodes[i as usize];
            if n.flags & flag::DELETED != 0 {
                continue;
            }
            if n.is_dir() {
                dirs += 1;
                stack.extend(n.children());
                continue;
            }
            files += 1;
            let k = n.kind as usize;
            kinds[k].0 += n.size;
            kinds[k].1 += 1;

            let name = self.name_bytes(i);
            let mut key = [0u8; 12];
            if let Some(dot) = name.iter().rposition(|&c| c == b'.') {
                let ext = &name[dot + 1..];
                if !ext.is_empty() && ext.len() <= 12 && dot > 0 {
                    for (j, c) in ext.iter().enumerate() {
                        key[j] = c.to_ascii_lowercase();
                    }
                }
            }
            let e = exts.entry(key).or_insert((0, 0, n.kind));
            e.0 += n.size;
            e.1 += 1;

            if heap.len() < TOP {
                heap.push(std::cmp::Reverse((n.size, i)));
            } else if let Some(min) = heap.peek()
                && n.size > min.0.0
            {
                heap.pop();
                heap.push(std::cmp::Reverse((n.size, i)));
            }
        }
        let mut largest: Vec<(u64, u32)> = heap.into_iter().map(|r| r.0).collect();
        largest.sort_unstable_by(|a, b| b.0.cmp(&a.0));

        let mut ext_stats: Vec<ExtStat> = exts
            .into_iter()
            .map(|(k, (size, count, kind))| {
                let len = k.iter().position(|&c| c == 0).unwrap_or(12);
                let ext = if len == 0 {
                    "（無副檔名）".to_string()
                } else {
                    format!(".{}", String::from_utf8_lossy(&k[..len]))
                };
                ExtStat { ext, size, count, kind: Kind::from_u8(kind) }
            })
            .collect();
        ext_stats.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        ext_stats.truncate(300);
        Stats { kinds, exts: ext_stats, largest: largest.into_iter().map(|x| x.1).collect(), dirs, files }
    }
}

/// 把 UTF-16 名稱寫進 WTF-8 緩衝區。合法 UTF-16 直接轉 UTF-8；否則交給 OsString。
pub fn push_name(buf: &mut Vec<u8>, name: &[u16]) -> usize {
    let start = buf.len();
    // 快速路徑：全 ASCII
    if name.iter().all(|&c| c < 0x80) {
        buf.extend(name.iter().map(|&c| c as u8));
        return buf.len() - start;
    }
    let mut ok = true;
    for r in char::decode_utf16(name.iter().copied()) {
        match r {
            Ok(ch) => {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
            }
            Err(_) => {
                ok = false;
                break;
            }
        }
    }
    if !ok {
        buf.truncate(start);
        use std::os::windows::ffi::OsStringExt;
        let os = std::ffi::OsString::from_wide(name);
        buf.extend_from_slice(os.as_encoded_bytes());
    }
    buf.len() - start
}
