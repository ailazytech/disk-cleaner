//! 專案建置產物搜尋：node_modules、target、.venv、build…
//! 只在有對應專案標記檔（package.json、Cargo.toml…）時才判定，避免誤刪。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use parking_lot::Mutex;

use crate::rules::Risk;
use crate::scan::dir_size;
use crate::win;

#[derive(Debug)]
pub struct ArtKind {
    pub id: &'static str,
    pub label: &'static str,
    pub risk: Risk,
    /// 刪掉之後怎麼重建
    pub hint: &'static str,
}

macro_rules! kind {
    ($name:ident, $id:literal, $label:literal, $risk:ident, $hint:literal) => {
        pub static $name: ArtKind = ArtKind { id: $id, label: $label, risk: Risk::$risk, hint: $hint };
    };
}

kind!(NODE, "node", "Node.js 相依套件 (node_modules)", Safe, "需要時執行 npm／pnpm／yarn install 就會照 lock 檔重新安裝。");
kind!(NODE_NOLOCK, "node_nolock", "node_modules（無 lock 檔）", Caution, "沒有 lock 檔，重新安裝時版本可能不同。");
kind!(RUST, "rust", "Rust 建置輸出 (target)", Safe, "cargo build 會重新編譯（大型專案要花幾分鐘）。");
kind!(VENV, "venv", "Python 虛擬環境", Caution, "可以依 requirements.txt／pyproject 重建；手動裝的套件要自己重裝。");
kind!(PYCACHE, "pycache", "Python 位元組碼 (__pycache__)", Safe, "Python 執行時會自動重新產生。");
kind!(PYTOOLS, "pytools", "Python 工具快取 (pytest/mypy/ruff/tox)", Safe, "測試與檢查工具下次執行時會重建。");
kind!(WEB, "web", "前端框架快取 (.next/.nuxt/.turbo…)", Safe, "下次 dev 或 build 會重新產生。");
kind!(WEB_BUILD, "web_build", "前端建置輸出 (dist/build)", Caution, "重新 build 就會產生；如果裡面有要部署的檔案請先確認。");
kind!(GRADLE, "gradle", "Gradle／Android 建置輸出", Safe, "下次 Gradle 建置會重新產生。");
kind!(FLUTTER, "flutter", "Flutter／Dart 建置輸出", Safe, "flutter pub get／build 會重新產生。");
kind!(CMAKE, "cmake", "CMake 建置目錄", Safe, "重新執行 CMake 設定與建置即可。");
kind!(CMAKE_OTHER, "cmake_other", "CMake 建置目錄（非常見名稱）", Caution, "重新執行 CMake 設定與建置即可；名稱不常見，請先確認裡面沒有原始碼。");
kind!(DOTNET_OBJ, "dotnet_obj", ".NET 中間檔 (obj)", Safe, "dotnet build 會重新產生。");
kind!(DOTNET_BIN, "dotnet_bin", ".NET 建置輸出 (bin)", Caution, "dotnet build 會重新產生；如果有手動放進去的檔案請先確認。");
kind!(UNITY, "unity", "Unity 專案快取 (Library)", Caution, "Unity 開啟專案時會重新匯入（大型專案要很久）。");
kind!(UNREAL, "unreal", "Unreal 中間檔", Safe, "Unreal 下次開啟或建置時會重新產生。");
kind!(ZIG, "zig", "Zig 建置快取", Safe, "zig build 會重新產生。");
kind!(HASKELL, "haskell", "Haskell 建置輸出", Safe, "stack／cabal build 會重新產生。");
kind!(ELIXIR, "elixir", "Elixir 建置輸出", Safe, "mix compile 會重新產生。");
kind!(ELIXIR_DEPS, "elixir_deps", "Elixir 相依套件 (deps)", Caution, "mix deps.get 會重新下載。");
kind!(TERRAFORM, "terraform", "Terraform 外掛 (.terraform)", Caution, "terraform init 會重新下載外掛。");
kind!(COMPOSER, "composer", "PHP 相依套件 (vendor)", Caution, "composer install 會重新安裝。");
kind!(GODOT, "godot", "Godot 匯入快取 (.godot)", Safe, "Godot 開啟專案時會重新匯入。");
kind!(CACHEDIR, "cachedir", "快取目錄 (CACHEDIR.TAG)", Safe, "這個資料夾自己標示為快取（CACHEDIR.TAG），刪除後會自動重建。");

struct Dir<'a> {
    path: &'a Path,
    /// 同一層所有項目名稱（小寫）
    entries: &'a [(String, bool)],
}

impl Dir<'_> {
    fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == name)
    }
    fn has_dir(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, d)| *d && n == name)
    }
    fn has_ext(&self, ext: &str) -> bool {
        self.entries.iter().any(|(n, d)| !d && n.rsplit_once('.').is_some_and(|(_, e)| e == ext))
    }
    /// 子資料夾裡有沒有這個「檔案」（資料夾不算）
    fn child_has(&self, dir: &str, file: &str) -> bool {
        self.path.join(dir).join(file).is_file()
    }
    fn child_has_any(&self, dir: &str, names: &[&str]) -> bool {
        names.iter().any(|f| self.path.join(dir).join(f).exists())
    }
    fn gradle(&self) -> bool {
        ["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"].iter().any(|f| self.has(f))
    }
}

/// CACHEDIR.TAG 必須是一般檔案，而且開頭是規範的簽章（https://bford.info/cachedir/）。
fn valid_cachedir_tag(path: &Path) -> bool {
    use std::io::Read;
    const SIG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
    if !path.is_file() {
        return false;
    }
    let mut buf = [0u8; 43];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut buf)).is_ok() && buf == SIG
}

/// __pycache__ 裡應該只有 .pyc 檔
fn only_pyc(dir: &Path) -> bool {
    let mut ok = true;
    let mut any = false;
    let r = win::list_dir(dir, |e| {
        any = true;
        if e.is_dir() || !e.name_os().to_string_lossy().to_lowercase().ends_with(".pyc") {
            ok = false;
        }
    });
    r.is_ok() && ok && any
}

/// 判斷 dir.path 底下名為 name（原始大小寫）的子資料夾是不是建置產物。
fn detect(d: &Dir, name: &str) -> Option<&'static ArtKind> {
    // 有版本控制的資料夾一定是原始碼，絕對不是建置產物
    if d.child_has_any(name, &[".git", ".hg", ".svn"]) {
        return None;
    }
    let n = name.to_lowercase();
    let pkg = d.has("package.json");
    let k: Option<&'static ArtKind> = match n.as_str() {
        "node_modules" if pkg => {
            let lock = ["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock", "npm-shrinkwrap.json"]
                .iter()
                .any(|f| d.has(f));
            Some(if lock { &NODE } else { &NODE_NOLOCK })
        }
        "target" if d.has("cargo.toml") || valid_cachedir_tag(&d.path.join(name).join("CACHEDIR.TAG")) => Some(&RUST),
        ".venv" | "venv" | "env" | ".env" if d.child_has(name, "pyvenv.cfg") => Some(&VENV),
        "__pycache__" if only_pyc(&d.path.join(name)) => Some(&PYCACHE),
        ".pytest_cache" | ".mypy_cache" | ".ruff_cache" | ".hypothesis" | ".tox" | ".nox" => Some(&PYTOOLS),
        ".next" | ".nuxt" | ".svelte-kit" | ".turbo" | ".parcel-cache" | ".angular" | ".docusaurus" | ".expo"
        | ".output" | ".vite" | ".astro" | ".vercel_build_output"
            if pkg =>
        {
            Some(&WEB)
        }
        "build" | ".gradle" | ".cxx" if d.gradle() => Some(&GRADLE),
        "build" | ".dart_tool" if d.has("pubspec.yaml") => Some(&FLUTTER),
        "obj" if d.has_ext("csproj") || d.has_ext("fsproj") || d.has_ext("vbproj") => Some(&DOTNET_OBJ),
        "bin" if d.has_ext("csproj") || d.has_ext("fsproj") || d.has_ext("vbproj") => Some(&DOTNET_BIN),
        "library" if d.has_dir("projectsettings") && d.has_dir("assets") => Some(&UNITY),
        "intermediate" | "deriveddatacache" if d.has_ext("uproject") => Some(&UNREAL),
        ".zig-cache" | "zig-cache" | "zig-out" if d.has("build.zig") => Some(&ZIG),
        ".stack-work" if d.has("stack.yaml") => Some(&HASKELL),
        "dist-newstyle" if d.has("cabal.project") || d.has_ext("cabal") => Some(&HASKELL),
        "_build" if d.has("mix.exs") => Some(&ELIXIR),
        "deps" if d.has("mix.exs") => Some(&ELIXIR_DEPS),
        ".terraform" if d.has_ext("tf") => Some(&TERRAFORM),
        "vendor" if d.has("composer.json") => Some(&COMPOSER),
        ".godot" if d.has("project.godot") => Some(&GODOT),
        "dist" | "build" | "out" if pkg => Some(&WEB_BUILD),
        _ => None,
    };
    if k.is_some() {
        return k;
    }
    // 任何名稱的虛擬環境（uv 建的 venv 也會有 CACHEDIR.TAG，不能當成一般快取）
    if d.child_has(name, "pyvenv.cfg") {
        return Some(&VENV);
    }
    // 以下是只靠「內容特徵」的判斷：裡面有專案定義檔就是原始碼專案（例如在專案根目錄直接跑過 cmake）
    if d.child_has_any(name, &["CMakeLists.txt", "Cargo.toml", "package.json", "pyproject.toml", "setup.py", "go.mod", "pom.xml"]) {
        return None;
    }
    // CMake 建置目錄：有 CMakeCache.txt、而且沒有 CMakeLists.txt（上面已排除）。
    // 常見名稱算「安全」，其他名稱算「注意」。
    if n.starts_with("cmake-build-") {
        return Some(&CMAKE);
    }
    if d.child_has(name, "CMakeCache.txt") {
        let usual = n.starts_with("build") || n.starts_with("out") || n == "_build" || n == "bin";
        return Some(if usual { &CMAKE } else { &CMAKE_OTHER });
    }
    if valid_cachedir_tag(&d.path.join(name).join("CACHEDIR.TAG")) {
        return Some(&CACHEDIR);
    }
    None
}

/// 刪除前重新確認：這個資料夾現在還是同一種建置產物嗎？
pub fn revalidate(path: &Path) -> Option<&'static ArtKind> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    if win::has_reparse_on_path(path) {
        return None;
    }
    let mut entries: Vec<(String, bool)> = Vec::new();
    win::list_dir(parent, |e| entries.push((e.name_os().to_string_lossy().to_lowercase(), e.is_dir()))).ok()?;
    detect(&Dir { path: parent, entries: &entries }, &name)
}

/// 任何深度都不進去的資料夾
fn skip_any(n: &str) -> bool {
    matches!(
        n,
        "node_modules" | "$recycle.bin" | "system volume information" | "appdata" | "site-packages"
            | "windowsapps" | "windows.old" | "scoop" | "steamapps" | "resources"
    ) || n.starts_with('.')
}

/// 磁碟根目錄下不進去的資料夾
fn skip_root(n: &str) -> bool {
    matches!(
        n,
        "windows" | "program files" | "program files (x86)" | "programdata" | "recovery" | "$windows.~bt"
            | "$windows.~ws" | "$winreagent" | "perflogs" | "msocache" | "$sysreset" | "config.msi"
            | "onedrivetemp" | "intel" | "nvidia" | "amd" | "esd" | "$getcurrent"
    )
}

#[derive(Clone, Debug)]
pub struct Artifact {
    pub path: PathBuf,
    pub kind: &'static ArtKind,
    pub bytes: u64,
    pub files: u64,
    /// 裡面最新檔案的修改時間（FILETIME）
    pub newest: u64,
}

#[derive(Default)]
pub struct FindProgress {
    pub dirs: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
    pub results: Mutex<Vec<Artifact>>,
    pub models: Mutex<Vec<crate::models::Model>>,
    pub pending: AtomicU64,
}

pub fn find(roots: &[PathBuf], prog: &FindProgress) {
    // 搜尋位置本身是連結（或路徑經過連結）就不搜，避免跑到連結另一端
    let mut rs: Vec<PathBuf> = roots.iter().filter(|p| p.is_dir() && !win::has_reparse_on_path(p)).cloned().collect();
    // 不分大小寫去重複，並去掉被其他根包含的根
    let key = |p: &PathBuf| p.to_string_lossy().to_lowercase().trim_end_matches('\\').to_string();
    rs.sort_by_key(key);
    rs.dedup_by_key(|p| key(p));
    let keys: Vec<String> = rs.iter().map(key).collect();
    let rs: Vec<PathBuf> = rs
        .iter()
        .zip(&keys)
        .filter(|(_, k)| !keys.iter().any(|q| q != *k && k.starts_with(&format!("{q}\\"))))
        .map(|(p, _)| p.clone())
        .collect();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(crate::scan::worker_threads())
        .build()
        .expect("pool");
    pool.scope(|s| {
        // 已知的模型庫（Hugging Face、Ollama、LM Studio…）
        s.spawn(|_| {
            let ms = crate::models::find_known(&prog.cancel);
            prog.models.lock().extend(ms);
        });
        for r in rs {
            let is_root = r.parent().is_none();
            s.spawn(move |s| visit(s, r, is_root, prog));
        }
    });
}

fn visit<'s>(s: &rayon::Scope<'s>, dir: PathBuf, is_drive_root: bool, prog: &'s FindProgress) {
    if prog.cancel.load(Relaxed) {
        return;
    }
    let d = prog.dirs.fetch_add(1, Relaxed);
    if d % 64 == 0
        && let Some(mut g) = prog.current.try_lock()
    {
        *g = dir.to_string_lossy().into_owned();
    }
    let mut entries: Vec<(String, bool)> = Vec::new();
    let mut subdirs: Vec<(String, std::ffi::OsString)> = Vec::new();
    let mut models: Vec<crate::models::Model> = Vec::new();
    let res = win::list_dir(&dir, |e| {
        let os = e.name_os();
        let lower = os.to_string_lossy().to_lowercase();
        let is_dir = e.is_dir();
        if is_dir && !e.is_reparse() {
            subdirs.push((lower.clone(), os));
        } else if !is_dir && e.local_size() >= crate::models::MIN_MODEL && crate::models::is_model_file(&lower) {
            let path = dir.join(&os);
            models.push(crate::models::Model {
                name: os.to_string_lossy().into_owned(),
                source: crate::models::guess_source(&path),
                path,
                bytes: e.local_size(),
                newest: e.mtime,
                del: crate::models::ModelDel::File,
            });
            entries.push((lower, is_dir));
            return;
        }
        entries.push((lower, is_dir));
    });
    if !models.is_empty() {
        prog.models.lock().extend(models);
    }
    if res.is_err() {
        return;
    }
    let ctx = Dir { path: &dir, entries: &entries };
    for (lower, os) in subdirs {
        let name = os.to_string_lossy();
        if let Some(kind) = detect(&ctx, &name) {
            let path = dir.join(&os);
            // Electron 應用裡的 node_modules 不算
            let ps = path.to_string_lossy().to_lowercase();
            if ps.contains("app.asar.unpacked") {
                continue;
            }
            prog.pending.fetch_add(1, Relaxed);
            s.spawn(move |_| {
                let st = dir_size(&path, u64::MAX, &prog.cancel);
                if st.bytes > 0 {
                    prog.results.lock().push(Artifact { path, kind, bytes: st.bytes, files: st.files, newest: st.newest });
                }
                prog.pending.fetch_sub(1, Relaxed);
            });
            continue;
        }
        if skip_any(&lower) || (is_drive_root && skip_root(&lower)) || lower == ".git" {
            continue;
        }
        let child = dir.join(&os);
        s.spawn(move |s| visit(s, child, false, prog));
    }
}

/// 預設的搜尋位置：使用者資料夾 + 系統磁碟以外的本機磁碟根目錄。
pub fn default_roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(h) = std::env::var("USERPROFILE") {
        v.push(PathBuf::from(h));
    }
    let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
    for d in win::drives() {
        if d.kind == "本機磁碟" && !d.root.to_uppercase().starts_with(&sys) {
            v.push(PathBuf::from(&d.root));
        }
    }
    v
}

/// 所有產物類型（快取讀回時用 id 對應）
pub static ALL_KINDS: [&ArtKind; 24] = [
    &NODE, &NODE_NOLOCK, &RUST, &VENV, &PYCACHE, &PYTOOLS, &WEB, &WEB_BUILD, &GRADLE, &FLUTTER, &CMAKE, &CMAKE_OTHER, &DOTNET_OBJ,
    &DOTNET_BIN, &UNITY, &UNREAL, &ZIG, &HASKELL, &ELIXIR, &ELIXIR_DEPS, &TERRAFORM, &COMPOSER, &GODOT, &CACHEDIR,
];
