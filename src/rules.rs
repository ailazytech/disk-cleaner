//! 清理規則目錄：哪些位置可以清、風險多高、怎麼清。

use std::path::PathBuf;

use crate::win;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum Cat {
    SysTemp,
    SysUpdate,
    Browser,
    Apps,
    DevCache,
    Recycle,
}

impl Cat {
    pub const ALL: [Cat; 6] = [Cat::DevCache, Cat::SysTemp, Cat::SysUpdate, Cat::Browser, Cat::Apps, Cat::Recycle];
    pub fn label(self) -> &'static str {
        match self {
            Cat::SysTemp => "系統暫存檔",
            Cat::SysUpdate => "系統更新與安裝殘留",
            Cat::Browser => "瀏覽器快取",
            Cat::Apps => "應用程式快取",
            Cat::DevCache => "開發工具快取",
            Cat::Recycle => "資源回收筒",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Cat::SysTemp => "🗑",
            Cat::SysUpdate => "🔄",
            Cat::Browser => "🌐",
            Cat::Apps => "📦",
            Cat::DevCache => "🛠",
            Cat::Recycle => "♻",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Risk {
    /// 純快取，刪了會自動重建
    Safe,
    /// 刪了要重新下載／重建，或可能影響功能
    Caution,
}

#[derive(Clone, Debug)]
pub enum Target {
    /// 清空資料夾內容（保留資料夾本身）
    Contents(String),
    /// 刪除整個資料夾
    Dir(String),
    /// 資料夾內符合樣式的檔案（不遞迴）
    Files(String, Vec<String>),
    /// 只計算大小（由指令清理）
    Info(String),
}

impl Target {
    pub fn template(&self) -> &str {
        match self {
            Target::Contents(s) | Target::Dir(s) | Target::Files(s, _) | Target::Info(s) => s,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Delete,
    /// 執行外部指令清理
    Command { program: String, args: Vec<String> },
    RecycleBin,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub id: &'static str,
    pub cat: Cat,
    pub name: String,
    pub desc: String,
    pub risk: Risk,
    pub admin: bool,
    /// 只清除 N 天前修改的檔案
    pub min_age_days: u32,
    pub targets: Vec<Target>,
    pub action: Action,
}

struct B {
    r: Rule,
}

fn rule(id: &'static str, cat: Cat, name: &str, desc: &str) -> B {
    B {
        r: Rule {
            id,
            cat,
            name: name.into(),
            desc: desc.into(),
            risk: Risk::Safe,
            admin: false,
            min_age_days: 0,
            targets: Vec::new(),
            action: Action::Delete,
        },
    }
}

impl B {
    fn caution(mut self) -> Self {
        self.r.risk = Risk::Caution;
        self
    }
    fn admin(mut self) -> Self {
        self.r.admin = true;
        self
    }
    fn age(mut self, d: u32) -> Self {
        self.r.min_age_days = d;
        self
    }
    fn contents(mut self, p: &str) -> Self {
        self.r.targets.push(Target::Contents(p.into()));
        self
    }
    fn dir(mut self, p: &str) -> Self {
        self.r.targets.push(Target::Dir(p.into()));
        self
    }
    fn files(mut self, p: &str, pats: &[&str]) -> Self {
        self.r.targets.push(Target::Files(p.into(), pats.iter().map(|s| s.to_string()).collect()));
        self
    }
    fn info(mut self, p: &str) -> Self {
        self.r.targets.push(Target::Info(p.into()));
        self
    }
    fn cmd(mut self, program: &str, args: &[&str]) -> Self {
        self.r.action = Action::Command { program: program.into(), args: args.iter().map(|s| s.to_string()).collect() };
        self
    }
    fn recycle(mut self) -> Self {
        self.r.action = Action::RecycleBin;
        self
    }
    fn done(self) -> Rule {
        self.r
    }
}

/// Chromium 系瀏覽器每個設定檔的快取子資料夾
fn chromium(b: B, user_data: &str) -> B {
    let mut b = b;
    for sub in [
        "Cache",
        "Code Cache",
        "GPUCache",
        "DawnCache",
        "DawnGraphiteCache",
        "DawnWebGPUCache",
        "Service Worker\\CacheStorage",
        "Service Worker\\ScriptCache",
    ] {
        b = b.contents(&format!("{user_data}\\*\\{sub}"));
    }
    for top in ["GrShaderCache", "ShaderCache", "GraphiteDawnCache"] {
        b = b.contents(&format!("{user_data}\\{top}"));
    }
    b
}

pub fn catalog() -> Vec<Rule> {
    use Cat::*;
    let mut v = Vec::new();

    // ---------------- 開發工具快取 ----------------
    v.push(rule("npm", DevCache, "npm 快取", "npm 下載過的套件快取（_cacache、_npx）。下次安裝會重新下載。")
        .contents("{LOCALAPPDATA}\\npm-cache").contents("{APPDATA}\\npm-cache").done());
    v.push(rule("pnpm", DevCache, "pnpm 套件庫 (store)", "pnpm 的全域套件庫。專案的 node_modules 用硬連結，刪除不影響已安裝的專案，但實際釋放的空間可能比顯示少；下次安裝會重新下載。")
        .caution()
        .contents("{LOCALAPPDATA}\\pnpm\\store").contents("{LOCALAPPDATA}\\pnpm-cache").contents("{ANYDRIVE}\\.pnpm-store").done());
    v.push(rule("yarn", DevCache, "Yarn 快取", "Yarn v1 與 Berry 的全域快取。")
        .contents("{LOCALAPPDATA}\\Yarn\\Cache").contents("{LOCALAPPDATA}\\Yarn\\Berry\\cache").done());
    v.push(rule("bun", DevCache, "Bun 快取", "Bun 安裝套件時的快取。")
        .contents("{HOME}\\.bun\\install\\cache").done());
    v.push(rule("deno", DevCache, "Deno 快取", "Deno 下載的遠端模組與編譯快取。")
        .contents("{LOCALAPPDATA}\\deno").done());
    v.push(rule("cargo", DevCache, "Rust Cargo 下載快取", "~/.cargo 裡下載的 crate 壓縮檔、解壓原始碼與 git 相依。不會動到已安裝的工具（bin）。")
        .contents("{CARGO_HOME}\\registry\\cache").contents("{CARGO_HOME}\\registry\\src")
        .contents("{CARGO_HOME}\\git\\checkouts").contents("{CARGO_HOME}\\git\\db").done());
    v.push(rule("rustup_tmp", DevCache, "rustup 下載暫存", "rustup 更新工具鏈時留下的下載檔。")
        .contents("{RUSTUP_HOME}\\downloads").contents("{RUSTUP_HOME}\\tmp").done());
    v.push(rule("sccache", DevCache, "sccache 編譯快取", "sccache 的本機編譯結果快取。")
        .contents("{LOCALAPPDATA}\\Mozilla\\sccache").done());
    v.push(rule("pip", DevCache, "pip 快取", "pip 下載與建置的 wheel 快取。")
        .contents("{LOCALAPPDATA}\\pip\\cache").done());
    v.push(rule("uv", DevCache, "uv 快取", "uv 的套件快取。虛擬環境用硬連結，刪除不影響已建立的環境；不會動到 uv 安裝的 Python 與工具。")
        .contents("{LOCALAPPDATA}\\uv\\cache").done());
    v.push(rule("poetry", DevCache, "Poetry 快取", "Poetry 的套件與 artifacts 快取。")
        .contents("{LOCALAPPDATA}\\pypoetry\\Cache").done());
    if let Some(conda) = win::which("conda") {
        v.push(rule("conda", DevCache, "Conda 套件快取", "用 conda clean --all 清除未使用的套件與壓縮檔。")
            .caution()
            .info("{HOME}\\anaconda3\\pkgs").info("{HOME}\\miniconda3\\pkgs").info("{HOME}\\miniforge3\\pkgs").info("{HOME}\\.conda\\pkgs")
            .cmd(&conda.to_string_lossy(), &["clean", "--all", "-y"]).done());
    }
    v.push(rule("gradle", DevCache, "Gradle 快取", "Gradle 相依套件、wrapper 與 daemon 記錄。建議先關閉 Android Studio／IDE。")
        .contents("{HOME}\\.gradle\\caches").contents("{HOME}\\.gradle\\wrapper\\dists").contents("{HOME}\\.gradle\\daemon").done());
    v.push(rule("maven", DevCache, "Maven 本機倉庫", "~/.m2/repository。若有手動 install 的本機套件會一併刪除。")
        .caution().contents("{HOME}\\.m2\\repository").done());
    v.push(rule("nuget", DevCache, "NuGet 快取", "NuGet 全域套件與 HTTP 快取。")
        .contents("{HOME}\\.nuget\\packages").contents("{LOCALAPPDATA}\\NuGet\\v3-cache")
        .contents("{LOCALAPPDATA}\\NuGet\\plugins-cache").done());
    v.push(rule("go", DevCache, "Go 模組與建置快取", "GOMODCACHE 與 GOCACHE。")
        .contents("{HOME}\\go\\pkg\\mod").contents("{LOCALAPPDATA}\\go-build").done());
    v.push(rule("composer", DevCache, "Composer 快取", "PHP Composer 下載快取。")
        .contents("{LOCALAPPDATA}\\Composer").done());
    v.push(rule("pub", DevCache, "Dart／Flutter pub 快取", "pub 套件快取。全域啟用（pub global activate）的工具也在這裡。")
        .caution().contents("{LOCALAPPDATA}\\Pub\\Cache").done());
    v.push(rule("playwright", DevCache, "Playwright 瀏覽器", "測試用瀏覽器，之後需要 npx playwright install 重新安裝。")
        .caution().contents("{LOCALAPPDATA}\\ms-playwright").done());
    v.push(rule("puppeteer", DevCache, "Puppeteer 瀏覽器", "Puppeteer 下載的 Chrome。")
        .caution().contents("{HOME}\\.cache\\puppeteer").done());
    v.push(rule("cypress", DevCache, "Cypress 執行檔快取", "Cypress 二進位檔，下次會重新下載。")
        .caution().contents("{LOCALAPPDATA}\\Cypress\\Cache").done());
    v.push(rule("electron_dl", DevCache, "Electron 下載快取", "electron 與 electron-builder 下載的執行環境。")
        .contents("{LOCALAPPDATA}\\electron\\Cache").contents("{LOCALAPPDATA}\\electron-builder\\Cache").done());
    v.push(rule("node_gyp", DevCache, "node-gyp 標頭檔快取", "原生模組編譯用的 Node 標頭檔。")
        .contents("{LOCALAPPDATA}\\node-gyp\\Cache").done());
    v.push(rule("ts", DevCache, "TypeScript 型別快取", "tsserver 自動取得的型別定義。")
        .contents("{LOCALAPPDATA}\\Microsoft\\TypeScript").done());
    v.push(rule("vcpkg", DevCache, "vcpkg 二進位快取", "vcpkg 的 binary caching 封存檔。")
        .contents("{LOCALAPPDATA}\\vcpkg\\archives").done());
    v.push(rule("vs_cmc", DevCache, "Visual Studio 元件快取", "ComponentModelCache，VS 下次啟動會重建。")
        .contents("{LOCALAPPDATA}\\Microsoft\\VisualStudio\\*\\ComponentModelCache").done());
    v.push(rule("jetbrains", DevCache, "JetBrains IDE 快取與索引", "IntelliJ、PyCharm、Rider 等的 caches／index／log，下次開啟會重建索引（會比較久）。")
        .caution()
        .contents("{LOCALAPPDATA}\\JetBrains\\*\\caches").contents("{LOCALAPPDATA}\\JetBrains\\*\\index")
        .contents("{LOCALAPPDATA}\\JetBrains\\*\\log").done());
    v.push(rule("android_cache", DevCache, "Android SDK 快取", "~/.android/cache。")
        .contents("{HOME}\\.android\\cache").done());
    v.push(rule("unity_cache", DevCache, "Unity 全域快取", "Unity 套件與下載快取。")
        .contents("{LOCALAPPDATA}\\Unity\\cache").done());
    v.push(rule("precommit", DevCache, "pre-commit 環境快取", "pre-commit 建立的 hook 環境。")
        .contents("{HOME}\\.cache\\pre-commit").done());
    if let Some(docker) = win::which("docker") {
        v.push(rule("docker", DevCache, "Docker 未使用的資源", "docker system prune：移除停止的容器、未使用的網路、懸空映像與建置快取。注意：WSL 虛擬磁碟檔不會自動縮小，需另外壓縮。")
            .caution()
            .info("{LOCALAPPDATA}\\Docker\\wsl\\disk\\docker_data.vhdx").info("{LOCALAPPDATA}\\Docker\\wsl\\data\\ext4.vhdx")
            .cmd(&docker.to_string_lossy(), &["system", "prune", "-f"]).done());
    }

    // ---------------- 系統暫存 ----------------
    v.push(rule("user_temp", SysTemp, "使用者暫存資料夾 (%TEMP%)", "程式留下的暫存檔。只清 1 天以前的檔案，使用中的會自動略過。")
        .age(1).contents("{TEMP}").done());
    v.push(rule("win_temp", SysTemp, "Windows 暫存資料夾", "C:\\Windows\\Temp。只清 1 天以前的檔案。")
        .admin().age(1).contents("{WINDIR}\\Temp").done());
    v.push(rule("wer", SysTemp, "Windows 錯誤報告", "程式當掉時產生的報告與傾印。")
        .contents("{LOCALAPPDATA}\\Microsoft\\Windows\\WER\\ReportArchive").contents("{LOCALAPPDATA}\\Microsoft\\Windows\\WER\\ReportQueue")
        .contents("{PROGRAMDATA}\\Microsoft\\Windows\\WER\\ReportArchive").contents("{PROGRAMDATA}\\Microsoft\\Windows\\WER\\ReportQueue")
        .contents("{PROGRAMDATA}\\Microsoft\\Windows\\WER\\Temp").done());
    v.push(rule("dumps", SysTemp, "當機傾印檔", "程式與系統當機的記憶體傾印（除非要除錯，否則用不到）。")
        .contents("{LOCALAPPDATA}\\CrashDumps").contents("{WINDIR}\\Minidump").files("{WINDIR}", &["MEMORY.DMP"])
        .contents("{WINDIR}\\LiveKernelReports").done());
    v.push(rule("thumbs", SysTemp, "縮圖與圖示快取", "檔案總管的縮圖資料庫，會自動重建。使用中的檔案會略過。")
        .files("{LOCALAPPDATA}\\Microsoft\\Windows\\Explorer", &["thumbcache_*.db", "iconcache_*.db"]).done());
    v.push(rule("shader", SysTemp, "顯示卡著色器快取", "DirectX、NVIDIA、AMD、Intel 的著色器快取。遊戲第一次啟動會稍慢。")
        .contents("{LOCALAPPDATA}\\D3DSCache").contents("{LOCALAPPDATA}\\NVIDIA\\DXCache").contents("{LOCALAPPDATA}\\NVIDIA\\GLCache")
        .contents("{LOCALLOW}\\NVIDIA\\PerDriverVersion\\DXCache").contents("{LOCALAPPDATA}\\AMD\\DxCache")
        .contents("{LOCALAPPDATA}\\AMD\\DxcCache").contents("{LOCALAPPDATA}\\AMD\\GLCache").contents("{LOCALAPPDATA}\\AMD\\VkCache")
        .contents("{LOCALAPPDATA}\\Intel\\ShaderCache").done());
    v.push(rule("inetcache", SysTemp, "系統網路快取 (INetCache)", "WebView／IE 元件的網路快取。")
        .contents("{LOCALAPPDATA}\\Microsoft\\Windows\\INetCache").done());
    v.push(rule("store_temp", SysTemp, "Microsoft Store 應用程式暫存", "各 UWP 應用的 INetCache、Temp 與 TempState。")
        .contents("{LOCALAPPDATA}\\Packages\\*\\AC\\INetCache").contents("{LOCALAPPDATA}\\Packages\\*\\AC\\Temp")
        .contents("{LOCALAPPDATA}\\Packages\\*\\TempState").done());
    v.push(rule("delivery", SysTemp, "傳遞最佳化快取", "Windows Update 點對點傳遞的快取。")
        .admin().contents("{WINDIR}\\ServiceProfiles\\NetworkService\\AppData\\Local\\Microsoft\\Windows\\DeliveryOptimization\\Cache").done());
    v.push(rule("cbs_logs", SysTemp, "Windows 系統記錄檔", "CBS 與 DISM 的記錄檔（有時會長到數 GB）。")
        .admin().files("{WINDIR}\\Logs\\CBS", &["*.log", "*.cab"]).files("{WINDIR}\\Logs\\DISM", &["*.log"]).done());

    // ---------------- 系統更新與安裝 ----------------
    v.push(rule("wu", SysUpdate, "Windows Update 下載快取", "已安裝更新的下載檔，Windows 需要時會重新下載。")
        .admin().contents("{WINDIR}\\SoftwareDistribution\\Download").done());
    v.push(rule("win_old", SysUpdate, "舊版 Windows (Windows.old)", "升級前的舊系統。刪除後就無法退回前一版 Windows。若權限不足無法刪除，請改用系統的「磁碟清理」。")
        .caution().admin().dir("{SYSDRIVE}\\Windows.old").done());
    v.push(rule("win_upgrade", SysUpdate, "Windows 升級暫存", "$Windows.~BT、$Windows.~WS、ESD 等升級時留下的檔案。")
        .admin().dir("{SYSDRIVE}\\$Windows.~BT").dir("{SYSDRIVE}\\$Windows.~WS").dir("{SYSDRIVE}\\$WINDOWS.~Q")
        .dir("{SYSDRIVE}\\ESD").dir("{SYSDRIVE}\\$GetCurrent").done());
    v.push(rule("drivers", SysUpdate, "驅動程式安裝暫存", "NVIDIA／AMD 安裝驅動時解壓縮的檔案（C:\\NVIDIA、C:\\AMD）與下載的安裝檔。")
        .admin().dir("{SYSDRIVE}\\NVIDIA").dir("{SYSDRIVE}\\AMD")
        .contents("{PROGRAMDATA}\\NVIDIA Corporation\\Downloader").done());
    v.push(rule("patchcache", SysUpdate, "Windows Installer 修補快取", "$PatchCache$。刪除後部分軟體修補或解除安裝時可能會要求原始安裝檔。")
        .caution().admin().contents("{WINDIR}\\Installer\\$PatchCache$").done());
    v.push(rule("pkgcache", SysUpdate, "安裝程式套件快取 (Package Cache)", "Visual Studio、.NET、C++ 執行階段等安裝程式的快取。刪除後修復或解除安裝時可能要求原始安裝檔。")
        .caution().admin().contents("{PROGRAMDATA}\\Package Cache").contents("{PROGRAMDATA}\\Microsoft\\VisualStudio\\Packages").done());
    v.push(rule("winsxs", SysUpdate, "元件存放區清理 (WinSxS)", "執行 DISM /StartComponentCleanup 移除被取代的舊版系統元件，可能需要幾分鐘。")
        .admin().cmd("Dism.exe", &["/Online", "/Cleanup-Image", "/StartComponentCleanup"]).done());
    v.push(rule("hiberfil", SysUpdate, "休眠檔 (hiberfil.sys)", "執行 powercfg /h off 關閉休眠與快速啟動，休眠檔會被移除。")
        .caution().admin().info("{SYSDRIVE}\\hiberfil.sys").cmd("powercfg.exe", &["/h", "off"]).done());
    v.push(rule("dl_installers", SysUpdate, "下載資料夾中的安裝檔", "下載資料夾裡 7 天以前的 .exe／.msi／.iso 等安裝檔。請先確認不再需要。")
        .caution().age(7)
        .files("{HOME}\\Downloads", &["*.exe", "*.msi", "*.msix", "*.msixbundle", "*.appx", "*.iso"]).done());

    // ---------------- 瀏覽器 ----------------
    let b = rule("chrome", Browser, "Google Chrome 快取", "網頁、程式碼與 GPU 快取。不會刪除書籤、密碼、Cookie。建議先關閉瀏覽器。");
    v.push(chromium(b, "{LOCALAPPDATA}\\Google\\Chrome\\User Data").done());
    let b = rule("edge", Browser, "Microsoft Edge 快取", "網頁、程式碼與 GPU 快取。不會刪除書籤、密碼、Cookie。建議先關閉瀏覽器。");
    v.push(chromium(b, "{LOCALAPPDATA}\\Microsoft\\Edge\\User Data").done());
    let b = rule("brave", Browser, "Brave 快取", "網頁、程式碼與 GPU 快取。");
    v.push(chromium(b, "{LOCALAPPDATA}\\BraveSoftware\\Brave-Browser\\User Data").done());
    let b = rule("vivaldi", Browser, "Vivaldi 快取", "網頁、程式碼與 GPU 快取。");
    v.push(chromium(b, "{LOCALAPPDATA}\\Vivaldi\\User Data").done());
    v.push(rule("opera", Browser, "Opera 快取", "網頁與 GPU 快取。")
        .contents("{LOCALAPPDATA}\\Opera Software\\Opera Stable\\Cache").contents("{APPDATA}\\Opera Software\\Opera Stable\\Code Cache")
        .contents("{APPDATA}\\Opera Software\\Opera Stable\\GPUCache").done());
    v.push(rule("firefox", Browser, "Firefox 快取", "cache2、startupCache 與縮圖。")
        .contents("{LOCALAPPDATA}\\Mozilla\\Firefox\\Profiles\\*\\cache2").contents("{LOCALAPPDATA}\\Mozilla\\Firefox\\Profiles\\*\\startupCache")
        .contents("{LOCALAPPDATA}\\Mozilla\\Firefox\\Profiles\\*\\thumbnails").done());

    // ---------------- 應用程式 ----------------
    let mut b = rule("electron", Apps, "Electron 應用程式快取", "Discord、VS Code、Slack、Teams、Notion、Obsidian 等以 Electron／WebView 製作的應用快取。建議先關閉這些程式。");
    for sub in ["Cache", "Code Cache", "GPUCache", "DawnCache", "DawnGraphiteCache", "DawnWebGPUCache", "Service Worker\\CacheStorage"] {
        b = b.contents(&format!("{{APPDATA}}\\*\\{sub}"));
    }
    v.push(b.done());
    let mut b = rule("vscode", Apps, "VS Code 系列快取與記錄", "VS Code、Cursor、Windsurf 的 CachedData、擴充套件安裝暫存與記錄檔。");
    for app in ["Code", "Code - Insiders", "Cursor", "Windsurf", "VSCodium"] {
        b = b.contents(&format!("{{APPDATA}}\\{app}\\CachedData"))
            .contents(&format!("{{APPDATA}}\\{app}\\CachedExtensionVSIXs"))
            .contents(&format!("{{APPDATA}}\\{app}\\CachedProfilesData"))
            .contents(&format!("{{APPDATA}}\\{app}\\logs"));
    }
    v.push(b.done());
    v.push(rule("steam", Apps, "Steam 網頁快取與記錄", "Steam 用戶端的 httpcache、depotcache、記錄與傾印。")
        .contents("{PF86}\\Steam\\appcache\\httpcache").contents("{PF86}\\Steam\\depotcache")
        .contents("{PF86}\\Steam\\logs").contents("{PF86}\\Steam\\dumps").done());
    v.push(rule("adobe_media", Apps, "Adobe 媒體快取", "Premiere／After Effects 的 Media Cache，可能非常大。開啟專案時會重建。")
        .contents("{APPDATA}\\Adobe\\Common\\Media Cache Files").contents("{APPDATA}\\Adobe\\Common\\Media Cache")
        .contents("{APPDATA}\\Adobe\\Common\\Peak Files").done());
    v.push(rule("spotify", Apps, "Spotify 快取", "串流快取；離線下載的歌曲也會被清除。")
        .caution().contents("{LOCALAPPDATA}\\Spotify\\Storage").contents("{LOCALAPPDATA}\\Spotify\\Data").done());

    // ---------------- 資源回收筒 ----------------
    v.push(rule("recycle", Recycle, "資源回收筒（所有磁碟）", "清空後無法復原。")
        .caution().recycle().done());

    v
}

// ---------------------------------------------------------------------------
// 路徑展開

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn token(t: &str) -> Option<Vec<String>> {
    let home = env("USERPROFILE");
    let one = |s: Option<String>| s.map(|x| vec![x]);
    match t {
        "LOCALAPPDATA" => one(env("LOCALAPPDATA")),
        "APPDATA" => one(env("APPDATA")),
        "HOME" => one(home),
        "TEMP" => one(env("TEMP").or_else(|| env("TMP"))),
        "WINDIR" => one(env("SystemRoot").or_else(|| env("WINDIR"))),
        "SYSDRIVE" => one(env("SystemDrive")),
        "PROGRAMDATA" => one(env("ProgramData")),
        "PF86" => one(env("ProgramFiles(x86)")),
        "PF" => one(env("ProgramFiles")),
        "LOCALLOW" => one(home.map(|h| format!("{h}\\AppData\\LocalLow"))),
        "CARGO_HOME" => one(env("CARGO_HOME").or_else(|| home.map(|h| format!("{h}\\.cargo")))),
        "RUSTUP_HOME" => one(env("RUSTUP_HOME").or_else(|| home.map(|h| format!("{h}\\.rustup")))),
        "ANYDRIVE" => Some(
            win::drives()
                .into_iter()
                .filter(|d| d.kind == "本機磁碟")
                .map(|d| d.root.trim_end_matches('\\').to_string())
                .collect(),
        ),
        _ => None,
    }
}

/// 不分大小寫的萬用字元比對（* 與 ?）。
pub fn wildcard(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let t: Vec<char> = s.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 把範本（含 {TOKEN} 與 * 萬用字元）展開成實際存在的路徑。
pub fn expand(template: &str) -> Vec<PathBuf> {
    // 先代換 token
    let mut bases: Vec<String> = vec![String::new()];
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        let name = &rest[i + 1..i + j];
        let Some(vals) = token(name) else { return Vec::new() };
        let lit = &rest[..i];
        bases = bases
            .iter()
            .flat_map(|b| vals.iter().map(move |v| format!("{b}{lit}{v}")))
            .collect();
        rest = &rest[i + j + 1..];
    }
    let bases: Vec<String> = bases.into_iter().map(|b| format!("{b}{rest}")).collect();

    let mut out = Vec::new();
    for b in bases {
        let parts: Vec<&str> = b.split('\\').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let mut cur: Vec<PathBuf> = vec![PathBuf::from(format!("{}\\", parts[0]))];
        for part in &parts[1..] {
            let mut next = Vec::new();
            if part.contains('*') || part.contains('?') {
                for c in &cur {
                    let _ = win::list_dir(c, |e| {
                        if e.is_dir() && !e.is_reparse() {
                            let n = e.name_os();
                            if wildcard(part, &n.to_string_lossy()) {
                                next.push(c.join(n));
                            }
                        }
                    });
                }
            } else {
                for c in &cur {
                    next.push(c.join(part));
                }
            }
            cur = next;
            if cur.is_empty() {
                break;
            }
        }
        out.extend(cur.into_iter().filter(|p| p.exists()));
    }
    out.sort();
    out.dedup();
    out
}
