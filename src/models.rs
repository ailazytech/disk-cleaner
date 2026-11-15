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

fn home() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
}

/// 在模型庫資料夾裡遞迴找模型檔（不跟隨連結）。
fn walk_files(dir: &Path, out: &mut Vec<Model>, cancel: &AtomicBool, depth: u32) {
    if cancel.load(Relaxed) || depth > 12 {
        return;
    }
    let mut subdirs = Vec::new();
    let _ = win::list_dir(dir, |e| {
        if e.is_dir() {
            if !e.is_reparse() {
                subdirs.push(dir.join(e.name_os()));
            }
        } else if e.local_size() >= MIN_MODEL {
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
    let hf_home = std::env::var_os("HF_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cache").join("huggingface"));
    let hub = std::env::var_os("HF_HUB_CACHE")
        .or_else(|| std::env::var_os("HUGGINGFACE_HUB_CACHE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| hf_home.join("hub"));
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

pub fn ollama_dir() -> PathBuf {
    std::env::var_os("OLLAMA_MODELS").map(PathBuf::from).unwrap_or_else(|| home().join(".ollama").join("models"))
}

/// 從 manifest 取出 (digest, size) 清單（不引入 JSON 函式庫，只做簡單掃描）。
fn manifest_layers(text: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("\"digest\"") {
        rest = &rest[i + 8..];
        let Some(q1) = rest.find('"') else { break };
        let after = &rest[q1 + 1..];
        let Some(q2) = after.find('"') else { break };
        let digest = after[..q2].to_string();
        // 同一個物件裡的 size
        let obj_end = after.find('}').unwrap_or(after.len());
        let obj = &after[..obj_end];
        let mut size = 0u64;
        if let Some(si) = obj.find("\"size\"") {
            let num: String = obj[si + 6..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
            size = num.parse().unwrap_or(0);
        }
        out.push((digest, size));
        rest = after;
    }
    out
}

fn blob_file(models: &Path, digest: &str) -> PathBuf {
    models.join("blobs").join(digest.replace(':', "-"))
}

/// 所有 manifest：(檔案路徑, 模型名稱)
fn ollama_manifests() -> Vec<(PathBuf, String)> {
    let root = ollama_dir().join("manifests");
    let mut out = Vec::new();
    fn rec(dir: &Path, comps: &mut Vec<String>, out: &mut Vec<(PathBuf, String)>) {
        let mut ents = Vec::new();
        let _ = win::list_dir(dir, |e| ents.push((e.name_os().to_string_lossy().into_owned(), e.is_dir())));
        for (name, is_dir) in ents {
            comps.push(name.clone());
            if is_dir {
                if comps.len() < 6 {
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
    let models = ollama_dir();
    for (path, name) in ollama_manifests() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let layers = manifest_layers(&text);
        let mut seen = HashSet::new();
        let mut bytes = 0;
        for (d, s) in &layers {
            if seen.insert(d.clone()) {
                // 以實際 blob 大小為準
                bytes += win::stat(&blob_file(&models, d)).map(|x| x.1).unwrap_or(*s);
            }
        }
        let newest = win::stat(&path).map(|x| x.2).unwrap_or(0);
        out.push(Model { name, source: "Ollama", path, bytes, newest, del: ModelDel::Ollama });
    }
}

/// 刪除 Ollama 模型：移除 manifest，再刪掉沒有其他 manifest 參照的 blob。回傳釋放的位元組數。
pub fn delete_ollama(manifest: &Path) -> Result<u64, String> {
    let models = ollama_dir();
    let text = std::fs::read_to_string(manifest).map_err(|e| e.to_string())?;
    let mine: Vec<String> = manifest_layers(&text).into_iter().map(|x| x.0).collect();
    let mut others: HashSet<String> = HashSet::new();
    for (p, _) in ollama_manifests() {
        if p == manifest {
            continue;
        }
        if let Ok(t) = std::fs::read_to_string(&p) {
            others.extend(manifest_layers(&t).into_iter().map(|x| x.0));
        }
    }
    std::fs::remove_file(manifest).map_err(|e| e.to_string())?;
    let mut freed = 0;
    for d in mine {
        if others.contains(&d) {
            continue;
        }
        let b = blob_file(&models, &d);
        let size = win::stat(&b).map(|x| x.1).unwrap_or(0);
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
