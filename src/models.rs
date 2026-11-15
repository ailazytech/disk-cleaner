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

/// 所有 manifest：(檔案路徑, 模型名稱)。不跟隨連結。
fn ollama_manifests() -> Vec<(PathBuf, String)> {
    let Some(models) = ollama_dir() else { return Vec::new() };
    let root = models.join("manifests");
    if win::has_reparse_on_path(&root) {
        return Vec::new();
    }
    let mut out = Vec::new();
    fn rec(dir: &Path, comps: &mut Vec<String>, out: &mut Vec<(PathBuf, String)>) {
        let mut ents = Vec::new();
        let _ = win::list_dir(dir, |e| ents.push((e.name_os().to_string_lossy().into_owned(), e.is_dir(), e.is_reparse())));
        for (name, is_dir, reparse) in ents {
            if reparse {
                continue;
            }
            comps.push(name.clone());
            if is_dir {
                if comps.len() < 16 {
                    rec(&dir.join(&name), comps, out);
                }
            } else if comps.len() >= 3 {
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
    }
    rec(&root, &mut Vec::new(), &mut out);
    out
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

/// 刪除 Ollama 模型：移除 manifest，再刪掉沒有其他 manifest 參照的 blob。回傳釋放的位元組數。
/// 任何一個其他模型的 manifest 讀不到、manifest 不在 Ollama 目錄裡、digest 格式不對，都直接中止（不刪）。
pub fn delete_ollama(manifest: &Path) -> Result<u64, String> {
    let models = ollama_dir().ok_or("找不到 Ollama 模型資料夾")?;
    let blobs = models.join("blobs");
    let manifests = models.join("manifests");
    let inside = manifest
        .to_string_lossy()
        .to_lowercase()
        .starts_with(&format!("{}\\", manifests.to_string_lossy().to_lowercase()));
    if !inside || win::has_reparse_on_path(manifest) || win::has_reparse_on_path(&blobs) {
        return Err("manifest 不在 Ollama 模型資料夾裡，或路徑經過連結，已略過".into());
    }
    let text = std::fs::read_to_string(manifest).map_err(|e| e.to_string())?;
    let mine: Vec<String> = manifest_layers(&text).into_iter().map(|x| x.0).collect();
    if mine.is_empty() || mine.iter().any(|d| !valid_digest(d)) {
        return Err("manifest 內容格式不對，為了安全不刪".into());
    }
    let mut others: HashSet<String> = HashSet::new();
    let mut found_self = false;
    for (p, _) in ollama_manifests() {
        if same_path(&p, manifest) {
            found_self = true;
            continue;
        }
        let t = std::fs::read_to_string(&p).map_err(|e| format!("讀不到其他模型的 manifest（{}）：{e}，為了安全不刪", p.display()))?;
        others.extend(manifest_layers(&t).into_iter().map(|x| x.0));
    }
    if !found_self {
        return Err("在 Ollama 模型清單裡找不到這個 manifest，已略過".into());
    }
    std::fs::remove_file(manifest).map_err(|e| e.to_string())?;
    let mut freed = 0;
    for d in mine {
        if others.contains(&d) {
            continue;
        }
        let Some(b) = blob_file(&models, &d) else { continue };
        if !b.parent().is_some_and(|p| same_path(p, &blobs)) {
            continue;
        }
        let Some((attrs, size, _)) = win::stat(&b) else { continue };
        if attrs & (win::ATTR_DIRECTORY | win::ATTR_REPARSE) != 0 {
            continue;
        }
        if std::fs::remove_file(&b).is_ok() {
            freed += size;
        }
    }
    Ok(freed)
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
    let mut seen: HashMap<PathBuf, ()> = HashMap::new();
    v.retain(|m| seen.insert(m.path.clone(), ()).is_none());
}
