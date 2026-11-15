//! AI 模型搜尋：Hugging Face、Ollama、LM Studio、ComfyUI 等模型庫，以及散落各處的大型模型檔。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use crate::scan::dir_size;
use crate::win;

/// 小於這個大小的模型檔不列出
pub const MIN_MODEL: u64 = 50 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum ModelDel {
    /// 刪整個資料夾（Hugging Face 的 models--x--y）
    Dir,
    /// 刪單一檔案
    File,
    /// Ollama 模型：刪 manifest 與沒有其他模型用到的 blob
    Ollama,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    pub source: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
    pub newest: u64,
    pub del: ModelDel,
}

const MODEL_EXTS: [&str; 10] = ["safetensors", "gguf", "ggml", "ckpt", "pt", "pth", "onnx", "tflite", "nemo", "llamafile"];

pub fn is_model_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.rsplit_once('.').is_some_and(|(_, e)| MODEL_EXTS.contains(&e))
}

/// 依路徑猜來源
pub fn guess_source(path: &Path) -> &'static str {
    let s = path.to_string_lossy().to_lowercase();
    if s.contains("lmstudio") || s.contains("lm-studio") {
        "LM Studio"
    } else if s.contains("comfy") {
        "ComfyUI"
    } else if s.contains("stable-diffusion-webui") || s.contains("forge") || s.contains("sd-webui") {
        "SD WebUI"
    } else if s.contains("\\torch\\") {
        "PyTorch"
    } else if s.contains("whisper") {
        "Whisper"
    } else if s.contains("\\jan\\") {
        "Jan"
    } else if s.contains("gpt4all") || s.contains("nomic.ai") {
        "GPT4All"
    } else if s.contains("insightface") {
        "InsightFace"
    } else if s.contains("modelscope") {
        "ModelScope"
    } else {
        "模型檔"
    }
}

/// 環境變數裡的路徑：空字串、相對路徑一律不用（避免變成「目前資料夾」）。
fn env_dir(name: &str) -> Option<PathBuf> {
    let v = std::env::var(name).ok()?.trim().replace('/', "\\");
    let b = v.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\').then(|| PathBuf::from(v))
}

fn home() -> Option<PathBuf> {
    env_dir("USERPROFILE")
}

/// 在模型庫資料夾裡遞迴找模型檔（不跟隨連結；起點本身是連結也不進去）。
fn walk_files(dir: &Path, out: &mut Vec<Model>, cancel: &AtomicBool, depth: u32) {
    if cancel.load(Relaxed) || depth > 12 {
        return;
    }
    if depth == 0 && win::has_reparse_on_path(dir) {
        return;
    }
    let mut subdirs = Vec::new();
    let _ = win::list_dir(dir, |e| {
        if e.is_dir() {
            if !e.is_reparse() {
                subdirs.push(dir.join(e.name_os()));
            }
        } else if !e.is_reparse() && !e.is_cloud() && e.local_size() >= MIN_MODEL {
            let name = e.name_os().to_string_lossy().into_owned();
            if is_model_file(&name) {
                let path = dir.join(&name);
                out.push(Model {
                    source: guess_source(&path),
                    name,
                    path,
                    bytes: e.local_size(),
                    newest: e.mtime,
                    del: ModelDel::File,
                });
            }
        }
    });
    for d in subdirs {
        walk_files(&d, out, cancel, depth + 1);
    }
}

fn huggingface(out: &mut Vec<Model>, cancel: &AtomicBool) {
    let Some(hf_home) = env_dir("HF_HOME").or_else(|| home().map(|h| h.join(".cache").join("huggingface"))) else { return };
    let hub = env_dir("HF_HUB_CACHE").or_else(|| env_dir("HUGGINGFACE_HUB_CACHE")).unwrap_or_else(|| hf_home.join("hub"));
    if win::has_reparse_on_path(&hub) {
        return;
    }
    let mut dirs = Vec::new();
    let _ = win::list_dir(&hub, |e| {
        if e.is_dir() && !e.is_reparse() {
            dirs.push(e.name_os().to_string_lossy().into_owned());
        }
    });
    for d in dirs {
        let (label, rest) = if let Some(r) = d.strip_prefix("models--") {
            ("Hugging Face", r)
        } else if let Some(r) = d.strip_prefix("datasets--") {
            ("Hugging Face 資料集", r)
        } else if let Some(r) = d.strip_prefix("spaces--") {
            ("Hugging Face Space", r)
        } else {
            continue;
        };
        let path = hub.join(&d);
        let st = dir_size(&path, u64::MAX, cancel);
        if st.bytes > 0 {
            out.push(Model {
                name: rest.replace("--", "/"),
                source: label,
                path,
                bytes: st.bytes,
                newest: st.newest,
                del: ModelDel::Dir,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Ollama

pub fn ollama_dir() -> Option<PathBuf> {
    env_dir("OLLAMA_MODELS").or_else(|| home().map(|h| h.join(".ollama").join("models")))
}

/// 從 manifest 取出 (digest, size) 清單（不引入 JSON 函式庫，只做簡單掃描）。
/// size 從 digest 所在的那個 `{ … }` 物件裡找，不管欄位順序。
fn manifest_layers(text: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(i) = text[pos..].find("\"digest\"") {
        let at = pos + i;
        let after = &text[at + 8..];
        let Some(q1) = after.find('"') else { break };
        let val = &after[q1 + 1..];
        let Some(q2) = val.find('"') else { break };
        let digest = val[..q2].to_string();
        let start = text[..at].rfind('{').unwrap_or(0);
        let end = text[at..].find('}').map(|e| at + e).unwrap_or(text.len());
        let obj = &text[start..end];
        let mut size = 0u64;
        if let Some(si) = obj.find("\"size\"") {
            let num: String = obj[si + 6..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
            size = num.parse().unwrap_or(0);
        }
        out.push((digest, size));
        pos = at + 8;
    }
    out
}

/// digest 只接受「演算法:十六進位」，例如 sha256:<64 個 hex>；擋掉 `..`、`\` 之類的路徑穿越。
pub fn valid_digest(d: &str) -> bool {
    let Some((alg, hex)) = d.split_once(':') else { return false };
    !alg.is_empty()
        && alg.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && (32..=128).contains(&hex.len())
        && hex.bytes().all(|c| c.is_ascii_hexdigit())
}

fn blob_file(models: &Path, digest: &str) -> Option<PathBuf> {
    valid_digest(digest).then(|| models.join("blobs").join(digest.replace(':', "-")))
}

/// 從其他 manifest 找出「可能被參照」的 digest：檔案裡任何長得像 digest 的字串都算（寧可多算）。
/// 統一成小寫：NTFS 檔名不分大小寫，`sha256:ABC…` 和 `sha256:abc…` 是同一個 blob。
fn referenced_digests(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == ':')).filter(|t| valid_digest(t)).map(|t| t.to_ascii_lowercase())
}

/// JSON 結構是否完整（大括號、中括號成對，字串有結束）。寫到一半或被截斷的 manifest 會回傳 false。
fn json_complete(text: &str) -> bool {
    let t = text.trim();
    if !t.starts_with('{') || !t.ends_with('}') {
        return false;
    }
    let mut stack = Vec::new();
    let (mut in_str, mut esc) = (false, false);
    for c in t.chars() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' => stack.push(c),
            '}' if stack.pop() != Some('{') => return false,
            ']' if stack.pop() != Some('[') => return false,
            _ => {}
        }
    }
    !in_str && stack.is_empty()
}

/// 所有 manifest：(檔案路徑, 模型名稱)。不跟隨連結。
fn ollama_manifests() -> Vec<(PathBuf, String)> {
    list_manifests(false).unwrap_or_default().into_iter().filter(|(_, name)| !name.is_empty()).collect()
}

/// strict：列舉失敗、遇到連結、資料夾太深都回傳錯誤，而且連不像 manifest 的檔案也列出（名稱留空）。
/// 刪 blob 之前要確定「其他模型用到哪些 blob」的清單是完整的。
fn list_manifests(strict: bool) -> Result<Vec<(PathBuf, String)>, String> {
    let models = ollama_dir().ok_or("找不到 Ollama 模型資料夾")?;
    let root = models.join("manifests");
    if win::has_reparse_on_path(&root) {
        return Err("manifest 資料夾經過連結".into());
    }
    let mut out = Vec::new();
    fn rec(dir: &Path, comps: &mut Vec<String>, out: &mut Vec<(PathBuf, String)>, strict: bool) -> Result<(), String> {
        let mut ents = Vec::new();
        let r = win::list_dir(dir, |e| ents.push((e.name_os().to_string_lossy().into_owned(), e.is_dir(), e.is_reparse())));
        if let Err(e) = r
            && strict
        {
            return Err(format!("無法列出 {}：{e}", dir.display()));
        }
        for (name, is_dir, reparse) in ents {
            if reparse {
                if strict {
                    return Err(format!("manifest 資料夾裡有連結：{}", dir.join(&name).display()));
                }
                continue;
            }
            comps.push(name.clone());
            if is_dir {
                if comps.len() < 16 {
                    rec(&dir.join(&name), comps, out, strict)?;
                } else if strict {
                    return Err(format!("manifest 資料夾太深：{}", dir.join(&name).display()));
                }
            } else if comps.len() < 3 {
                if strict {
                    out.push((dir.join(&name), String::new()));
                }
            } else {
                // registry / namespace / model / tag
                let n = comps.len();
                let (model, tag) = (&comps[n - 2], &comps[n - 1]);
                let prefix = &comps[..n - 2];
                let display = match prefix {
                    [reg, ns] if reg == "registry.ollama.ai" && ns == "library" => format!("{model}:{tag}"),
                    [reg, ns] if reg == "registry.ollama.ai" => format!("{ns}/{model}:{tag}"),
                    _ => format!("{}/{model}:{tag}", prefix.join("/")),
                };
                out.push((dir.join(&name), display));
            }
            comps.pop();
        }
        Ok(())
    }
    rec(&root, &mut Vec::new(), &mut out, strict)?;
    Ok(out)
}

fn ollama(out: &mut Vec<Model>) {
    let Some(models) = ollama_dir() else { return };
    for (path, name) in ollama_manifests() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let layers = manifest_layers(&text);
        let mut seen = HashSet::new();
        let mut bytes = 0;
        for (d, s) in &layers {
            if seen.insert(d.clone()) {
                // 以實際 blob 大小為準
                bytes += blob_file(&models, d).and_then(|b| win::stat(&b)).map(|x| x.1).unwrap_or(*s);
            }
        }
        let newest = win::stat(&path).map(|x| x.2).unwrap_or(0);
        out.push(Model { name, source: "Ollama", path, bytes, newest, del: ModelDel::Ollama });
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// 刪除 Ollama 模型：移除 manifest，再刪掉沒有其他 manifest 參照的 blob。回傳（釋放的位元組數, 刪不掉的 blob 數）。
/// 「其他模型用到哪些 blob」只要有一點不確定（Ollama 正在執行、資料夾列不出來、有連結、太深、
/// manifest 讀不到、不完整或有跳脫字元），就整個不刪。其他 manifest 裡任何長得像 digest 的字串都當成有參照。
/// `check_running`：Ollama 正在執行時拒絕（它隨時可能新增參照同一個 blob 的 manifest）；只有自我測試會關掉。
pub fn delete_ollama(manifest: &Path, check_running: bool) -> Result<(u64, u64), String> {
    if check_running && win::process_running(&["ollama.exe", "ollama app.exe"]) {
        return Err("Ollama 正在執行，它可能隨時新增用到同一份檔案的模型；請先結束 Ollama（含系統匣圖示）再刪".into());
    }
    let models = ollama_dir().ok_or("找不到 Ollama 模型資料夾")?;
    let blobs = models.join("blobs");
    let manifests = models.join("manifests");
    let inside = manifest
        .to_string_lossy()
        .to_lowercase()
        .starts_with(&format!("{}\\", manifests.to_string_lossy().to_lowercase()));
    if !inside {
        return Err("manifest 不在 Ollama 模型資料夾裡，已略過".into());
    }
    let (Some(man_dir), Some(man_name)) = (manifest.parent(), manifest.file_name()) else {
        return Err("manifest 路徑格式不對".into());
    };
    // 握住 manifest 所在資料夾與 blobs（含所有上層）：之後都以 handle 為基準打開、刪除，途中不能被換成連結
    let man_pins = win::pin_dir(man_dir).map_err(|e| format!("manifest 資料夾經過連結或無法鎖定（{e}），為了安全不刪"))?;
    let blob_pins = win::pin_dir(&blobs).map_err(|e| format!("blobs 資料夾經過連結或無法鎖定（{e}），為了安全不刪"))?;
    let text = std::fs::read_to_string(manifest).map_err(|e| e.to_string())?;
    let mine: Vec<String> = manifest_layers(&text).into_iter().map(|x| x.0).collect();
    if !json_complete(&text) || text.contains('\\') || mine.is_empty() || mine.iter().any(|d| !valid_digest(d)) {
        return Err("manifest 內容格式不對，為了安全不刪".into());
    }
    let all = list_manifests(true).map_err(|e| format!("{e}，無法確認其他模型用到哪些檔案，為了安全不刪"))?;
    let mut others: HashSet<String> = HashSet::new();
    let mut found_self = false;
    for (p, _) in all {
        if same_path(&p, manifest) {
            found_self = true;
            continue;
        }
        let t = std::fs::read_to_string(&p).map_err(|e| format!("讀不到其他模型的 manifest（{}）：{e}，為了安全不刪", p.display()))?;
        if !json_complete(&t) {
            return Err(format!("其他模型的 manifest 不完整（{}），可能正在下載或寫入，為了安全不刪", p.display()));
        }
        // JSON 跳脫字元（反斜線 u 加四位十六進位，代表一個普通字母）會讓 digest 比對漏掉，看不懂就不刪
        if t.contains('\\') {
            return Err(format!("其他模型的 manifest 有無法確認的寫法（{}），為了安全不刪", p.display()));
        }
        others.extend(referenced_digests(&t));
    }
    if !found_self {
        return Err("在 Ollama 模型清單裡找不到這個 manifest，已略過".into());
    }
    let mh = man_pins.last().expect("pin_dir 至少回傳一層").open_rel(man_name, win::Open::Delete).map_err(|e| e.to_string())?;
    let mi = mh.info().ok_or("讀不到 manifest 資訊")?;
    if mi.is_dir() || mi.is_reparse() || mi.is_cloud() {
        return Err("manifest 不是一般檔案，已略過".into());
    }
    mh.delete().map_err(|e| e.to_string())?;
    let bh = blob_pins.last().expect("pin_dir 至少回傳一層");
    let (mut freed, mut failed) = (0, 0);
    let mut done = HashSet::new();
    for d in mine {
        let d = d.to_ascii_lowercase();
        if others.contains(&d) || !done.insert(d.clone()) || !valid_digest(&d) {
            continue;
        }
        let name = d.replace(':', "-");
        let h = match bh.open_rel(std::ffi::OsStr::new(&name), win::Open::Delete) {
            Ok(h) => h,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let Some(i) = h.info() else {
            failed += 1;
            continue;
        };
        if i.is_dir() || i.is_reparse() || i.is_cloud() {
            continue;
        }
        let real = h.allocated().map_or(i.size, |a| a.min(i.size));
        match h.delete() {
            Ok(()) if i.links <= 1 => freed += real,
            Ok(()) => {}
            Err(_) => failed += 1,
        }
    }
    Ok((freed, failed))
}

/// 已知模型庫的位置（遞迴找模型檔）。
const STORES: [&str; 16] = [
    "{HOME}\\.lmstudio\\models",
    "{HOME}\\.cache\\lm-studio\\models",
    "{LOCALAPPDATA}\\Comfy-Desktop",
    "{HOME}\\.cache\\torch",
    "{HOME}\\.cache\\whisper",
    "{HOME}\\.cache\\modelscope",
    "{HOME}\\.cache\\clip",
    "{HOME}\\.cache\\gpt4all",
    "{HOME}\\.insightface",
    "{HOME}\\.u2net",
    "{HOME}\\.keras\\models",
    "{HOME}\\.deepface",
    "{HOME}\\jan",
    "{APPDATA}\\Jan",
    "{LOCALAPPDATA}\\nomic.ai",
    "{APPDATA}\\krita\\ai_diffusion",
];

pub fn find_known(cancel: &AtomicBool) -> Vec<Model> {
    let mut out = Vec::new();
    huggingface(&mut out, cancel);
    ollama(&mut out);
    for t in STORES {
        for p in crate::rules::expand(t) {
            walk_files(&p, &mut out, cancel, 0);
        }
    }
    out
}

/// 去除重複路徑
pub fn dedup(v: &mut Vec<Model>) {
    // 路徑不分大小寫：同一個檔案用不同大小寫加進來，不能被當成兩份
    let mut seen: HashMap<String, ()> = HashMap::new();
    v.retain(|m| seen.insert(m.path.to_string_lossy().to_lowercase(), ()).is_none());
}

/// 刪除「重複的模型」：要刪的（del）和保留的（keep）都以 handle 打開並鎖住（別人不能寫、不能刪、不能改名），
/// 確認兩個都是一般檔案（不是連結、不是雲端檔）、不是同一個實體檔案（大小寫不同的路徑、硬連結），
/// 逐位元組內容完全相同，最後用同一個 handle 刪掉 del；整個過程 keep 都鎖著。回傳釋放的位元組數。
/// 只比對檔名和大小不夠：同架構的不同微調結果大小常常一模一樣。
pub fn delete_duplicate(del: &Path, keep: &Path, cancel: &AtomicBool) -> Result<u64, String> {
    let open = |p: &Path, mode: win::Open, what: &str| -> Result<(Vec<win::Handle>, win::Handle, win::HInfo), String> {
        let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else {
            return Err(format!("{what}路徑格式不對"));
        };
        let pins = win::pin_dir(dir).map_err(|e| format!("{what}所在資料夾經過連結或無法鎖定（{e}）"))?;
        let h = pins.last().expect("pin_dir 至少回傳一層").open_rel(name, mode).map_err(|e| format!("{what}無法打開（{e}）"))?;
        let i = h.info().ok_or_else(|| format!("讀不到{what}的資訊"))?;
        if i.is_dir() || i.is_reparse() || i.is_cloud() {
            return Err(format!("{what}不是一般檔案（可能是連結或雲端檔）"));
        }
        Ok((pins, h, i))
    };
    let (_pd, hd, id) = open(del, win::Open::ReadDelete, "要刪的檔案")?;
    let (_pk, hk, ik) = open(keep, win::Open::ReadLocked, &format!("保留的那份（{}）", keep.display()))?;
    if (id.volume, id.index) == (ik.volume, ik.index) {
        return Err(format!("和 {} 其實是同一個檔案", keep.display()));
    }
    if id.size != ik.size {
        return Err(format!("和 {} 大小不同", keep.display()));
    }
    const CHUNK: usize = 8 << 20;
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let read_full = |h: &win::Handle, buf: &mut [u8]| -> Result<usize, String> {
        let mut n = 0;
        while n < buf.len() {
            match h.read(&mut buf[n..]).map_err(|e| e.to_string())? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    };
    loop {
        if cancel.load(Relaxed) {
            return Err("已取消".into());
        }
        let n = read_full(&hd, &mut ba)?;
        let m = read_full(&hk, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Err(format!("和 {} 內容不同，不是重複檔", keep.display()));
        }
        if n == 0 {
            break;
        }
    }
    let real = hd.allocated().map_or(id.size, |a| a.min(id.size));
    hd.delete().map_err(|e| format!("刪除失敗（{e}）"))?;
    Ok(if id.links <= 1 { real } else { 0 })
}
