//! 開機項目的建議（開機不啟用／延後啟用／保留），以及一鍵複製「問 AI」的提問文字。
//!
//! 建議只是依執行檔名稱與路徑比對常見的程式，認不得的標「不確定」，交給使用者自己判斷或問 AI。
//! 這裡不會改任何設定。複製出去的提問：使用者資料夾換成 %USERPROFILE%、其他使用者的資料夾名稱與
//! email 遮掉、命令列參數只留開關名稱（值可能是帳號、權杖或網址）。

use std::path::Path;
use std::sync::OnceLock;

use crate::boot::{BootData, BootLog, SlowItem, Source, StartupItem};
use crate::{fmt, win};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rec {
    /// 開機時用不到：停用它的自動啟動，要用時再手動打開
    Off,
    /// 需要它常駐，但不必一開機就跟系統搶資源（目前做法：停用自動啟動、開機後自己打開）
    Later,
    /// 系統、防毒、硬體或登入後馬上要用的，留著
    Keep,
}

impl Rec {
    pub fn label(self) -> &'static str {
        match self {
            Rec::Off => "開機不啟用",
            Rec::Later => "延後啟用",
            Rec::Keep => "保留",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Advice {
    pub rec: Rec,
    /// 這是什麼
    pub what: &'static str,
    /// 建議怎麼做
    pub how: &'static str,
}

struct Entry {
    /// 執行檔名稱（小寫、不含 .exe）完全相同才算
    stems: &'static [&'static str],
    /// 名稱、程式位置或 Store 套件名稱（小寫）包含這些字就算；不比對命令列參數
    subs: &'static [&'static str],
    advice: Advice,
}

const fn a(rec: Rec, what: &'static str, how: &'static str) -> Advice {
    Advice { rec, what, how }
}

/// 指向不存在檔案的項目
pub const LEFTOVER: Advice = a(
    Rec::Off,
    "找不到要執行的檔案",
    "指令指向的檔案現在不在。如果它在外接硬碟、網路磁碟或還沒同步的雲端資料夾上，先確認接上後是不是還找不到；\
     確定找不到，多半是解除安裝後的殘留，停用它不會有影響。",
);

/// 先比對所有項目的執行檔名稱，都沒有才比對名稱與位置裡的字（各自由上往下，第一個符合的就是答案）。
const TABLE: &[Entry] = &[
    Entry {
        stems: &["tiworker", "trustedinstaller", "wuauclt", "musnotification", "musnotificationux", "usoclient", "mousocoreworker", "setuphost", "windows10upgraderapp"],
        subs: &[],
        advice: a(Rec::Keep, "Windows 更新", "Windows 正在安裝更新的那幾次開機會特別慢，裝完就會恢復，不需要處理。"),
    },
    Entry {
        stems: &[
            "svchost", "explorer", "services", "lsass", "csrss", "wininit", "winlogon", "smss", "dwm", "sihost", "ctfmon", "taskhostw",
            "runtimebroker", "searchindexer", "searchhost", "searchapp", "searchui", "searchprotocolhost", "startmenuexperiencehost",
            "shellexperiencehost", "textinputhost", "fontdrvhost", "audiodg", "spoolsv", "wmiprvse", "dllhost", "conhost", "userinit", "logonui",
            "lockapp", "systemsettings", "backgroundtaskhost", "sgrmbroker", "wudfhost", "system", "registry", "msmpeng", "nissrv",
            "mpdefendercoreservice", "securityhealthservice", "securityhealthsystray", "smartscreen", "mpcmdrun", "compattelrunner", "sppsvc",
            "ngen", "ngentask", "mscorsvw",
        ],
        subs: &[],
        advice: a(
            Rec::Keep,
            "Windows 本身的元件",
            "這是 Windows 自己需要的（包括內建防毒 Microsoft Defender），不能也不需要停用。它拖慢開機通常是那次開機有別的程式或更新在背景忙，先處理清單裡其他項目。",
        ),
    },
    Entry {
        stems: &["vmcompute", "vmms", "vmwp"],
        subs: &[],
        advice: a(
            Rec::Keep,
            "Hyper-V 的系統服務（WSL 2、Docker、Windows 沙箱都會用到）",
            "這是 Windows 的虛擬化服務，不要在「服務」裡停用。它會忙通常是有虛擬機在開機時被叫起來，處理帶起虛擬機的那個程式就好。",
        ),
    },
    Entry {
        stems: &[
            "avp", "avpui", "egui", "ekrn", "mbam", "mbamtray", "mbamservice", "avastui", "avastsvc", "avgui", "avgsvc", "coreserviceshell",
            "coreframeworkhost", "pccntmon", "uiseagnt", "bdagent", "vsserv", "bdservicehost", "ccsvchst", "nortonsecurity", "mcshield",
            "mfemms", "mfefire", "mfevtps", "mcuicnt", "mcapexe", "modulecoreservice", "mmsshost", "360tray", "360sd", "zhudongfangyu",
            "hipstray", "qqpcrtp", "qqpctray", "savservice", "sophosui",
        ],
        subs: &[
            "mcafee", "norton", "symantec", "\\avast", "\\avg\\", "kaspersky", "\\eset\\", "bitdefender", "trend micro", "trendmicro", "sophos",
            "malwarebytes", "f-secure", "avira", "webroot", "\\comodo", "huorong", "panda security",
        ],
        advice: a(
            Rec::Keep,
            "第三方防毒軟體",
            "防毒要一開機就在跑才有即時保護，保留。一台電腦只需要一套防毒：如果這是你自己管理的電腦，而它是買電腦時附的試用版、你也沒有付費，\
             可以在「設定 → 應用程式」解除安裝，Windows 會改用內建的 Microsoft Defender（移除後到「Windows 安全性」確認即時保護是開著的）。\
             公司或學校的電腦請不要自己移除。",
        ),
    },
    // 驅動程式小幫手之前，先挑出廠商的更新工具（路徑一樣在 Intel、NVIDIA 底下）
    Entry {
        stems: &["dsatray", "dsaservice", "dsaupdateservice", "nvsphelper64", "nvidia share", "nvidia geforce experience"],
        subs: &["driver & support assistant", "driver and support assistant", "geforce experience"],
        advice: a(
            Rec::Off,
            "顯示卡或驅動程式廠商的更新工具",
            "只是在背景檢查驅動程式更新、顯示通知或遊戲錄影功能，開機時用不到。停用後驅動程式照常運作，需要更新時再打開它就好。",
        ),
    },
    Entry {
        stems: &[
            "rtkauduservice64", "rtkauduservice", "rthdvcpl", "ravcpl64", "ravbg64", "rtkngui64", "syntpenh", "syntphelper", "etdctrl",
            "etdservice", "igfxem", "igfxtray", "igfxhk", "igfxext", "igfxcuiservice", "hkcmd", "igfxpers", "persistence", "nvcontainer",
            "nvdisplay.container", "radeonsoftware", "amdrsserv", "atieclxx", "atiesrxx", "wavessvc64", "wavessyssvc64", "dax3api",
            "dolbydax2api", "nahimicsvc64", "nahimicsvc32", "rtkbtmanserv", "ibtsiva",
        ],
        subs: &["realtek", "synaptics", "\\elantech", "\\elan\\", "nvidia corporation\\display", "dolby", "\\waves\\", "nahimic", "conexant", "\\amd\\cnext"],
        advice: a(
            Rec::Keep,
            "硬體驅動程式的小幫手（音效、觸控板、顯示卡、藍牙）",
            "負責音效、觸控板手勢、快捷鍵之類的功能。保留比較保險，停用可能讓某些按鍵或音效設定失效。",
        ),
    },
    Entry {
        stems: &["weaselserver", "sgtool", "pimelauncher"],
        subs: &["\\rime\\", "weasel", "sogou", "baidupinyin", "googlepinyin", "\\pime\\"],
        advice: a(Rec::Keep, "輸入法", "保留，停用後可能沒辦法打中文或切換輸入法。"),
    },
    Entry {
        stems: &[
            "keepass", "keepassxc", "1password", "bitwarden", "nordvpn", "expressvpn", "protonvpn", "openvpnconnect", "openvpn-gui", "surfshark",
            "tailscale", "tailscale-ipn", "wireguard", "autohotkey", "autohotkey64", "autohotkeyu64", "anydesk", "teamviewer", "teamviewer_service",
            "rustdesk", "parsecd", "remoting_host", "todesk", "sunloginclient",
        ],
        subs: &[
            "1password", "bitwarden", "keepass", "nordvpn", "expressvpn", "protonvpn", "surfshark", "tailscale", "anydesk", "teamviewer", "rustdesk",
            "\\parsec\\", "splashtop", "chrome remote desktop", "todesk", "sunlogin",
        ],
        advice: a(
            Rec::Keep,
            "VPN、密碼管理、自動化腳本或遠端桌面",
            "登入後馬上就要用的話保留（連公司網路、自動填密碼、自訂快捷鍵、讓別人在你不在時遠端連進來）。確定很少用，才停用它的自動啟動、要用時再打開。",
        ),
    },
    Entry {
        stems: &["vboxheadless", "virtualbox", "virtualboxvm", "vboxsvc", "vboxsds"],
        subs: &["\\virtualbox\\", "virtualbox headless"],
        advice: a(
            Rec::Off,
            "VirtualBox 虛擬機（VBoxHeadless 是在背景執行、沒有視窗的虛擬機）",
            "有虛擬機被設成開機自動啟動（VirtualBox 的自動啟動、Vagrant 或其他工具）。用不到時在它的設定關掉自動啟動、要用再開，\
             可以省下大量記憶體和磁碟讀寫。",
        ),
    },
    Entry {
        stems: &["vmmem", "vmmemwsl"],
        subs: &[],
        advice: a(
            Rec::Later,
            "WSL 2 或 Hyper-V 虛擬機佔用的記憶體（vmmem）",
            "它本身不能停用，是有程式在開機時把虛擬機叫起來：Docker Desktop 的登入時啟動、設成自動啟動的 Hyper-V 虛擬機，或一開機就執行 WSL 的工具。\
             用得到就保留；不需要一開機就跑的話，把那個程式改成要用時再開。",
        ),
    },
    Entry {
        stems: &["docker desktop", "com.docker.backend", "com.docker.build", "dockerd"],
        subs: &["docker desktop", "\\docker\\docker\\"],
        advice: a(
            Rec::Later,
            "Docker Desktop",
            "有容器要開機就跑的話保留；不然到 Docker Desktop 的設定關掉「Start Docker Desktop when you sign in」，要用時再開。",
        ),
    },
    Entry {
        stems: &["crosvm", "wsaclient", "wsaservice", "hd-player", "bluestacks", "noxvmhandle", "nox", "dnplayer", "memu", "memuheadless"],
        subs: &["google play games", "googleplaygames", "bluestacks", "\\nox\\", "ldplayer", "microvirt"],
        advice: a(
            Rec::Off,
            "Android 模擬器或虛擬機（crosvm 常見於 Google Play 遊戲電腦版）",
            "開機就把虛擬機開起來，很吃記憶體和磁碟。到帶起它的程式（例如 Google Play 遊戲、BlueStacks）的設定關掉開機自動啟動，要用時再開。",
        ),
    },
    Entry {
        stems: &["msedge", "chrome", "firefox", "opera", "brave", "vivaldi", "browser_assistant"],
        subs: &["opera browser assistant"],
        advice: a(
            Rec::Off,
            "瀏覽器",
            "如果它是開機時在背景預先載入（Edge 的「啟動加速」、Chrome 的「關閉 Google Chrome 後繼續執行背景應用程式」），停用後第一次打開瀏覽器會稍慢，\
             靠瀏覽器在背景執行的功能（網頁通知、網頁應用程式）要等你打開瀏覽器才會運作。Edge 可以在「設定 → 系統與效能」關掉「啟動加速」。",
        ),
    },
    Entry {
        stems: &[
            "adobearm", "armsvc", "acrotray", "adobeupdateservice", "adobegcclient", "agsservice", "agcinvokerutility", "creative cloud",
            "adobe desktop service", "ccxprocess", "coresync", "adobecollabsync", "jusched", "jucheck", "googleupdate", "softwareupdate",
            "ituneshelper", "apsdaemon", "ccleaner", "ccleaner64", "driverbooster", "wshelper",
        ],
        subs: &["\\adobe\\arm\\", "\\adobe\\oobe\\", "\\adobe desktop common\\", "\\java\\java update", "ccleaner", "\\iobit\\", "wondershare"],
        advice: a(
            Rec::Off,
            "自動更新、授權檢查或小工具程式",
            "只是在背景檢查更新、顯示通知或保持登入，開機時用不到。停用後打開主程式時通常還是會自己檢查更新。",
        ),
    },
    Entry {
        stems: &[
            "code", "code - insiders", "cursor", "windsurf", "codeium", "antigravity", "zed", "devenv", "idea64", "pycharm64", "webstorm64",
            "rider64", "clion64", "goland64", "phpstorm64", "datagrip64", "rustrover64", "jetbrains-toolbox", "sublime_text", "notepad++",
            "language_server_windows_x64", "githubdesktop", "sourcetree", "postman", "kiro", "trae", "claude", "chatgpt", "lm studio", "codex",
            "copilot", "perplexity", "figma", "figma_agent",
        ],
        subs: &[
            "windsurf", "codeium", "antigravity", "\\programs\\cursor\\", "jetbrains", "microsoft vs code", "\\githubdesktop\\", "openai.",
            "microsoft.copilot", "anthropic", "perplexity", "figmaagent", "\\figma\\",
        ],
        advice: a(
            Rec::Off,
            "程式編輯器、開發、設計或 AI 工具",
            "開機時不需要它。如果是你一登入就自己打開的，Windows 也會算進開機時間：等開機完成一、兩分鐘再開，整體會順很多。\
             如果是上次沒關、登入時被自動重新打開，可以到「設定 → 帳戶 → 登入選項」關掉「自動儲存我可重新啟動的應用程式」那個選項。",
        ),
    },
    Entry {
        stems: &[
            "discord", "line", "linelauncher", "telegram", "skype", "ms-teams", "teams", "slack", "wechat", "weixin", "whatsapp", "messenger",
            "zoom", "webex", "ciscocollabhost", "kakaotalk", "signal",
        ],
        subs: &["\\discord\\", "telegram desktop", "\\line\\", "\\slack\\", "\\zoom\\", "whatsapp", "\\skype", "\\signal\\", "signal-desktop"],
        advice: a(
            Rec::Later,
            "通訊或會議軟體",
            "開機時會登入、同步訊息。不介意晚幾分鐘才收到訊息，就停用自動啟動，開機一陣子後再打開；工作上需要一開機就收訊息的話保留。",
        ),
    },
    Entry {
        stems: &[
            "steam", "steamwebhelper", "steamservice", "epicgameslauncher", "epicwebhelper", "battle.net", "eadesktop", "eabackgroundservice",
            "origin", "upc", "ubisoftconnect", "galaxyclient", "riotclientservices", "riotclientux", "garena", "wegame", "overwolf", "medal",
        ],
        subs: &["epic games\\launcher", "battle.net", "\\ea desktop\\", "ubisoft game launcher", "gog galaxy", "riot client", "\\garena", "overwolf"],
        advice: a(Rec::Off, "遊戲平台", "開機時會自動登入、檢查更新，跟系統搶磁碟和網路。要玩時再打開就好。"),
    },
    Entry {
        stems: &["spotify", "itunes", "kkbox", "applemusic", "amazon music", "deezer", "tidal", "foobar2000", "vlc"],
        subs: &["spotify", "kkbox"],
        advice: a(Rec::Off, "音樂或影音軟體", "開機時用不到，要聽或看的時候再打開。"),
    },
    Entry {
        stems: &[
            "onedrive", "googledrivefs", "googledrivesync", "dropbox", "icloudservices", "icloudckks", "icloudhome", "megasync", "nextcloud",
            "pcloud", "baidunetdisk", "synologydrive",
        ],
        // 只比對程式本身的位置與檔名：放在 OneDrive、Dropbox、iCloud 同步資料夾裡的其他程式不算
        subs: &[
            "microsoft\\onedrive\\", "onedrive.exe", "dropbox.exe", "google\\drive", "googledrivefs", "appleinc.icloud", "\\internet services\\icloud",
            "synology drive", "nextcloud.exe", "megasync", "pcloud.exe", "baidunetdisk",
        ],
        advice: a(
            Rec::Later,
            "雲端硬碟同步",
            "需要它在背景同步，但不必一開機就跟系統搶資源。可以停用自動啟動、開機後過幾分鐘再打開，打開後會自動補同步。\
             注意：它沒開的期間檔案不會同步。",
        ),
    },
    Entry {
        stems: &[
            "lghub", "lghub_agent", "lghub_updater", "logioptions", "logioptionsplus", "logioptionsplus_agent", "lightingservice", "icue",
            "steelseriesgg", "steelseriesengine", "myasus", "armourycrate",
        ],
        subs: &["\\logitech\\", "\\logi\\", "\\razer\\", "\\corsair\\", "\\steelseries\\", "armoury crate", "lenovo vantage"],
        advice: a(
            Rec::Later,
            "滑鼠、鍵盤或電腦廠商的工具",
            "自訂按鍵、燈光、風扇或電池設定要它開著才有作用。常用這些功能就保留；只用預設功能的話可以停用。",
        ),
    },
    Entry {
        stems: &[
            "powertoys", "everything", "sharex", "snipaste", "ditto", "flux", "greenshot", "lightshot", "ollama app", "ollama", "quicklook",
            "translucenttb", "rainmeter", "sandman",
        ],
        subs: &["powertoys", "\\everything\\", "sharex", "snipaste", "\\ollama\\", "sandboxie"],
        advice: a(
            Rec::Later,
            "常駐小工具（剪貼簿、截圖、搜尋、沙箱、本機 AI 模型等）",
            "天天會用就保留；偶爾才用的話，可以停用自動啟動，要用時再打開。",
        ),
    },
    Entry {
        stems: &["phoneexperiencehost", "yourphone", "cortana", "skypeapp", "skypebridge", "gamebar", "gamebarftserver", "xboxapp", "onenotem", "widgets"],
        subs: &[
            "microsoft.yourphone", "cortana", "microsoft.549981c3f5f10", "microsoft.xboxgamingoverlay", "microsoft.skypeapp", "microsoft.gamingapp",
            "microsoft.xboxapp",
        ],
        advice: a(Rec::Off, "Windows 或 Office 附帶的 App", "沒在用就停用，要用時照樣可以從開始功能表打開。"),
    },
    Entry {
        stems: &[],
        subs: &["servisign", "hicos", "nhiicc", "moica", "\\twca", "chtsign", "webatm"],
        advice: a(
            Rec::Later,
            "網路銀行、報稅或憑證卡（自然人憑證、健保卡）的簽章元件",
            "用網路銀行、報稅或插卡簽章時，網頁要靠它和讀卡機溝通。常用就保留；偶爾才用的話可以停用，要用時再從開始功能表打開（網頁通常也會提示你啟動它）。",
        ),
    },
];

/// 依執行檔名稱（file）、顯示名稱（name）與程式位置或 Store 套件名稱（path，呼叫端先遮掉使用者資料夾）找建議。
/// 先比對執行檔名稱，再比對名稱與位置裡的字；命令列參數不參與比對。
pub fn lookup(file: &str, name: &str, path: &str) -> Option<Advice> {
    let file = file.trim().to_lowercase();
    let stem = file.strip_suffix(".exe").unwrap_or(&file);
    if !stem.is_empty()
        && let Some(e) = TABLE.iter().find(|e| e.stems.contains(&stem))
    {
        return Some(e.advice);
    }
    let hay = format!("{}\n{}", name.to_lowercase(), path.to_lowercase());
    TABLE.iter().find(|e| e.subs.iter().any(|s| hay.contains(s))).map(|e| e.advice)
}

fn file_of(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 給比對與提問用的「程式位置」：有執行檔就用它（已遮掉使用者資料夾），Store 項目用套件名稱
fn program_of(it: &StartupItem) -> String {
    match &it.exe {
        Some(e) if it.source != Source::StoreApp => scrub(&e.display().to_string()),
        _ => it.command.clone(),
    }
}

pub fn for_startup(it: &StartupItem) -> Option<Advice> {
    if it.missing {
        return Some(LEFTOVER);
    }
    let file = it.exe.as_deref().map(file_of).unwrap_or_default();
    lookup(&file, &format!("{}\n{}", it.display, it.name), &program_of(it))
}

pub fn for_slow(s: &SlowItem) -> Option<Advice> {
    match s.kind {
        "驅動程式" | "裝置" => Some(a(
            Rec::Keep,
            "驅動程式或裝置",
            "不能直接停用。通常要更新驅動程式（電腦或裝置廠商的網站，或 Windows Update 的「選用更新」）；\
             用不到的裝置（舊印表機、虛擬網卡等）可以在「裝置管理員」停用。",
        )),
        "群組原則" => Some(a(Rec::Keep, "群組原則（公司或學校網域的設定）", "這是公司或學校電腦的登入設定，要請 IT 處理。")),
        "背景最佳化" | "工作階段管理" => Some(a(
            Rec::Keep,
            "Windows 自己的開機最佳化",
            "偶爾出現不用處理；每次都很慢的話，通常是系統磁碟太滿或太慢。",
        )),
        _ => lookup(&s.raw, &s.name, &scrub(&s.path)),
    }
}

// ---------------------------------------------------------------------------
// 遮掉個人資料

/// 比較用：斜線一律當反斜線、不分大小寫（含非英文字母）
fn fold(c: char) -> char {
    if c == '/' { '\\' } else { c.to_lowercase().next().unwrap_or(c) }
}

/// cs[i..] 開頭是不是使用者資料夾 home（而且後面是路徑的邊界），是的話回傳 home 的字數
fn match_home(cs: &[char], i: usize, home: &[char]) -> Option<usize> {
    if home.is_empty() || i + home.len() > cs.len() || !(0..home.len()).all(|k| fold(cs[i + k]) == fold(home[k])) {
        return None;
    }
    let j = i + home.len();
    match cs.get(j) {
        None => Some(home.len()),
        Some('\\' | '/' | '"' | '\'' | ')' | ']' | ';' | ',' | '=' | '\r' | '\n' | '\t') => Some(home.len()),
        // 空白後面如果還接著路徑（例如 C:\Users\Ann Marie\...），那是另一個資料夾，不換
        Some(' ') => {
            let next: String = cs[j + 1..].iter().take_while(|c| !c.is_whitespace()).collect();
            (!next.contains(['\\', '/'])).then_some(home.len())
        }
        _ => None,
    }
}

/// X:\Users\<名稱> 裡其他使用者的名稱換成 <使用者>（Public、Default 這些共用資料夾不換）
fn mask_other_users(cs: &[char]) -> Vec<char> {
    const PAT: &str = ":\\users\\";
    let pat: Vec<char> = PAT.chars().collect();
    let stop = |c: char| matches!(c, '\\' | '/' | '"' | '\'' | '\r' | '\n' | '\t' | ';' | ',');
    let mut out = Vec::with_capacity(cs.len());
    let mut i = 0;
    while i < cs.len() {
        if i + pat.len() <= cs.len() && (0..pat.len()).all(|k| fold(cs[i + k]) == pat[k]) {
            out.extend_from_slice(&cs[i..i + pat.len()]);
            let j = i + pat.len();
            let mut k = j;
            while k < cs.len() && !stop(cs[k]) {
                k += 1;
            }
            let name: String = cs[j..k].iter().collect::<String>().to_lowercase();
            let shared = ["public", "default", "default user", "all users"].contains(&name.trim());
            if k > j && !shared && !name.starts_with(['%', '<']) {
                out.extend("<使用者>".chars());
            } else {
                out.extend_from_slice(&cs[j..k]);
            }
            i = k;
            continue;
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

/// 看起來像 email 的換成 <email>
fn mask_emails(cs: &[char]) -> Vec<char> {
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let dom = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for i in 0..cs.len() {
        if cs[i] != '@' || spans.last().is_some_and(|&(_, e)| i < e) {
            continue;
        }
        let mut a = i;
        while a > 0 && local(cs[a - 1]) {
            a -= 1;
        }
        let mut b = i + 1;
        while b < cs.len() && dom(cs[b]) {
            b += 1;
        }
        while b > i + 1 && cs[b - 1] == '.' {
            b -= 1;
        }
        let domain = &cs[i + 1..b];
        if a < i && domain.contains(&'.') && domain.first() != Some(&'.') {
            spans.push((a, b));
        }
    }
    let mut out = Vec::with_capacity(cs.len());
    let mut i = 0;
    for (a, b) in spans {
        out.extend_from_slice(&cs[i..a]);
        out.extend("<email>".chars());
        i = b;
    }
    out.extend_from_slice(&cs[i..]);
    out
}

/// 遮掉個人資料：homes 是使用者資料夾的各種寫法（長路徑、8.3 短路徑…），換成 %USERPROFILE%；
/// 再把其他使用者的資料夾名稱和 email 遮掉。
pub fn scrub_with(s: &str, homes: &[&str]) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut hs: Vec<Vec<char>> = homes.iter().map(|h| h.trim_end_matches(['\\', '/']).chars().collect::<Vec<char>>()).filter(|h| h.len() >= 4).collect();
    // 長的先比（C:\Users\Ann 不能搶走 C:\Users\Ann.Lee 的開頭；後者的邊界檢查也會擋）
    hs.sort_by_key(|h| std::cmp::Reverse(h.len()));
    let mut out: Vec<char> = Vec::with_capacity(cs.len());
    let mut i = 0;
    'outer: while i < cs.len() {
        for h in &hs {
            if let Some(n) = match_home(&cs, i, h) {
                out.extend("%USERPROFILE%".chars());
                i += n;
                continue 'outer;
            }
        }
        out.push(cs[i]);
        i += 1;
    }
    mask_emails(&mask_other_users(&out)).into_iter().collect()
}

/// 這台電腦上使用者資料夾的各種寫法（只算一次）
fn homes() -> &'static [String] {
    static H: OnceLock<Vec<String>> = OnceLock::new();
    H.get_or_init(|| {
        let Ok(h) = std::env::var("USERPROFILE") else { return Vec::new() };
        let p = Path::new(&h);
        let mut v = vec![h.clone()];
        v.extend(win::final_path(p).map(|f| f.to_string_lossy().into_owned()));
        v.extend(win::short_path(p).map(|f| f.to_string_lossy().into_owned()));
        v
    })
}

fn scrub(s: &str) -> String {
    let hs: Vec<&str> = homes().iter().map(|s| s.as_str()).collect();
    scrub_with(s, &hs)
}

/// 命令列參數只留開關名稱（--hidden、/background、-silent），值換成「…」：值可能是帳號、權杖或網址
pub fn safe_args(args: &str) -> String {
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in args.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    let mut out: Vec<String> = Vec::new();
    for t in toks {
        let (key, has_val) = match t.split_once(['=', ':']) {
            Some((k, _)) if !k.is_empty() => (k.to_string(), true),
            _ => (t.clone(), false),
        };
        let body = key.strip_prefix("--").or_else(|| key.strip_prefix('-')).or_else(|| key.strip_prefix('/'));
        let switch = body.is_some_and(|b| {
            !b.is_empty() && b.len() <= 32 && b.starts_with(|c: char| c.is_ascii_alphabetic()) && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
        let w = match (switch, has_val) {
            (true, false) => key,
            (true, true) => format!("{key}=…"),
            (false, _) => "…".to_string(),
        };
        if !(w == "…" && out.last().is_some_and(|l| l == "…")) {
            out.push(w);
        }
    }
    out.join(" ")
}

// ---------------------------------------------------------------------------
// 問 AI 的提問

fn secs(ms: u64) -> String {
    format!("{:.1} 秒", ms as f64 / 1000.0)
}

fn stale(when: u64) -> bool {
    when != 0 && win::now_filetime().saturating_sub(when) > 30 * 86_400 * 10_000_000
}

fn kind_event(kind: &str) -> &'static str {
    match kind {
        "程式" => "事件 101：程式",
        "驅動程式" => "事件 102：驅動程式",
        "服務" => "事件 103：服務",
        "背景最佳化" => "事件 106：背景最佳化",
        "群組原則" => "事件 107／108：群組原則",
        "裝置" => "事件 109：裝置",
        "工作階段管理" => "事件 110：工作階段管理",
        _ => "其他",
    }
}

const ASK: &str = "請告訴我：\n\
1. 這是什麼？通常是哪個軟體裝的？\n\
2. 開機時不讓它自動啟動，會有什麼影響？\n\
3. 你建議「開機不啟用」、「延後啟用」還是「保留」？為什麼？\n\
4. 要停用或延後的話，具體要怎麼做（在哪個設定、點哪些選項）？\n\
不確定的地方請直接說不確定，不要猜。請用繁體中文回答。\n\
（註：路徑裡的使用者資料夾寫成 %USERPROFILE%，參數的值以「…」省略。）";

fn advice_line(adv: Option<Advice>) -> String {
    match adv {
        Some(a) => format!("- 我用的工具初步判斷：{}（{}）\n", a.rec.label(), a.what),
        None => "- 我用的工具認不出這是什麼\n".into(),
    }
}

/// 一個開機項目要執行什麼（給提問用）：程式位置＋只留開關的參數
fn program_line(it: &StartupItem) -> String {
    let mut s = match it.source {
        Source::StoreApp => format!("Microsoft Store 應用程式 {}（啟動工作 {}）", it.command, it.name),
        Source::UserFolder | Source::CommonFolder => {
            let lnk = Path::new(&it.command).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{}（「啟動」資料夾裡的捷徑 {lnk}）", program_of(it))
        }
        _ => program_of(it),
    };
    let args = safe_args(&it.args);
    if !args.is_empty() {
        s += &format!("，參數 {args}");
    }
    s
}

/// 一組開機自動啟動項目（指令相同、合成一列的）的提問
pub fn startup_prompt(group: &[&StartupItem], os: &str) -> String {
    let Some(&first) = group.first() else { return String::new() };
    let on = group.iter().filter(|i| i.enabled).count();
    let it = group.iter().copied().find(|i| i.enabled).unwrap_or(first);
    let mut s = format!("我在用 {os}，想讓開機快一點，正在整理「開機自動啟動」的程式。請幫我看看下面這一項：\n\n");
    s += &format!("- 名稱：{}\n", it.display);
    s += &format!("- 執行：{}\n", program_line(it));
    s += &format!("- 登記在：{}\n", it.source.location());
    if group.len() > 1 {
        s += &format!("- 同樣的指令重複登記了 {} 筆，其中 {on} 筆啟用\n", group.len());
    } else {
        s += &format!("- 現在：{}\n", if on > 0 { "會自動啟動" } else { "已停用" });
    }
    if it.policy {
        s += "- 由系統原則管理（不能自己改）\n";
    }
    if it.missing {
        s += "- 找不到要執行的檔案（可能是解除安裝後的殘留，或在沒接上的磁碟上）\n";
    }
    if let Some(d) = group.iter().filter_map(|i| i.delay_ms).max() {
        s += &format!("- Windows 的開機效能記錄：它平均讓開機多花 {}\n", secs(d as u64));
    }
    s += &advice_line(for_startup(it));
    s += "\n";
    s += ASK;
    scrub(&s)
}

/// 拖慢開機的一個項目的提問。items 是 BootData::items（看它有沒有登記成開機自動啟動）。
pub fn slow_prompt(si: &SlowItem, items: &[StartupItem], os: &str) -> String {
    let mut s = format!(
        "我在用 {os}，想讓開機快一點。Windows 的開機效能記錄（事件檢視器：Microsoft-Windows-Diagnostics-Performance/Operational）\
         說下面這一項拖慢了開機：\n\n"
    );
    s += &format!("- 名稱：{}\n", si.name);
    s += &format!("- 類型：{}\n", kind_event(si.kind));
    if !si.raw.is_empty() && si.raw != si.name {
        s += &format!("- 檔案：{}\n", si.raw);
    }
    if !si.path.is_empty() {
        s += &format!("- 路徑：{}\n", si.path);
    }
    s += &format!(
        "- 記錄到 {} 次，平均每次讓開機多花 {}（啟動平均要 {}，最多一次多花 {}）\n",
        si.count,
        secs(si.avg_degr() as u64),
        secs(si.avg_total() as u64),
        secs(si.max_degr_ms as u64)
    );
    if si.last != 0 {
        s += &format!("- 最近一次記錄：{}{}\n", fmt::ago(si.last), if stale(si.last) { "（之後的開機沒有新記錄，可能是比較舊的狀況）" } else { "" });
    }
    let linked: Vec<&StartupItem> = si.startup.iter().filter_map(|&i| items.get(i)).collect();
    if let Some(it) = linked.first() {
        let on = linked.iter().any(|i| i.enabled);
        s += &format!("- 它有登記成開機自動啟動：{}（{}），現在{}\n", it.display, it.source.location(), if on { "會自動啟動" } else { "已停用" });
    } else if si.kind == "程式" {
        s += "- 沒有找到它登記在 Run 機碼、啟動資料夾或 Store 啟動工作裡（可能是服務、工作排程器、別的程式帶起來的，或是我開機後自己打開的）\n";
    }
    s += &advice_line(for_slow(si));
    s += "\n";
    s += ASK;
    scrub(&s)
}

/// 整頁的資料寫成一段提問，請 AI 排出處理順序。
pub fn full_report(d: &BootData) -> String {
    let mut s = format!("我在用 {}，開機很慢，想請你幫我排出處理順序。以下是我用工具整理的資料。\n\n", d.sys.os);
    s += "## 開機時間（Windows 的開機效能記錄）\n";
    match &d.log {
        BootLog::Ok { boots, .. } if !boots.is_empty() => {
            let last = boots.last().unwrap();
            s += &format!(
                "- 最近一筆記錄：總共 {}（到桌面出現 {}，桌面出現後還在忙 {}），{}\n",
                secs(last.total_ms as u64),
                secs(last.main_ms as u64),
                secs(last.post_ms as u64),
                fmt::ago(last.when)
            );
            if stale(last.when) {
                s += "- 注意：Windows 不是每次開機都寫這份記錄，之後的開機都沒有記錄，這可能是比較舊的狀況\n";
            }
            let recent: Vec<_> = boots.iter().rev().take(10).collect();
            let avg = recent.iter().map(|b| b.total_ms as u64).sum::<u64>() / recent.len().max(1) as u64;
            s += &format!("- 最近 {} 筆平均：{}\n", recent.len(), secs(avg));
        }
        BootLog::Ok { .. } => s += "- 沒有開機記錄\n",
        BootLog::NeedAdmin => s += "- 讀不到（需要系統管理員權限）\n",
        BootLog::Unavailable(m) => s += &format!("- 讀不到：{m}\n"),
    }
    s += "\n## 系統\n";
    s += &format!(
        "- 快速啟動：{}\n",
        match (d.sys.fast_startup, d.sys.hibernate) {
            (Some(true), Some(false)) => "設定是開啟，但休眠已關閉，所以沒有作用",
            (Some(true), Some(true)) => "已開啟",
            (Some(true), None) => "已開啟（休眠狀態不明）",
            (Some(false), _) => "已關閉",
            (None, _) => "讀不到",
        }
    );
    s += &format!(
        "- {} 剩餘空間：{}／{}（{:.0}%）\n",
        d.sys.sys_drive,
        fmt::size(d.sys.sys_free),
        fmt::size(d.sys.sys_total),
        fmt::pct(d.sys.sys_free, d.sys.sys_total) * 100.0
    );
    s += &format!(
        "- 系統磁碟：{}\n",
        match d.sys.sys_ssd {
            Some(true) => "SSD",
            Some(false) => "傳統硬碟",
            None => "查不到",
        }
    );

    // 開機自動啟動：指令相同的合成一筆（和畫面上的分組相同），代表那筆優先用啟用中的
    let mut done = vec![false; d.items.len()];
    let mut on_lines = Vec::new();
    let mut off_names = Vec::new();
    for (i, it) in d.items.iter().enumerate() {
        if done[i] {
            continue;
        }
        let same: Vec<usize> = if it.store_key.is_none() {
            (i..d.items.len())
                .filter(|&k| !done[k] && d.items[k].store_key.is_none() && d.items[k].source == it.source && d.items[k].command.trim() == it.command.trim())
                .collect()
        } else {
            vec![i]
        };
        for &k in &same {
            done[k] = true;
        }
        let group: Vec<&StartupItem> = same.iter().map(|&k| &d.items[k]).collect();
        let on = group.iter().filter(|g| g.enabled).count();
        let Some(&rep) = group.iter().find(|g| g.enabled) else {
            off_names.push(it.display.clone());
            continue;
        };
        let mut extra = vec![rep.source.label().to_string()];
        if group.len() > 1 {
            extra.push(format!("重複登記 {} 筆，{on} 筆啟用", group.len()));
        }
        if let Some(ms) = group.iter().filter_map(|g| g.delay_ms).max() {
            extra.push(format!("開機記錄平均多花 {}", secs(ms as u64)));
        }
        if rep.missing {
            extra.push("找不到程式".into());
        }
        if rep.policy {
            extra.push("由系統原則管理".into());
        }
        if let Some(a) = for_startup(rep) {
            extra.push(format!("工具初判：{}", a.rec.label()));
        }
        on_lines.push(format!("- {}：{}（{}）\n", rep.display, program_line(rep), extra.join("；")));
    }
    s += &format!("\n## 開機自動啟動的程式（{} 個會啟動）\n", on_lines.len());
    for l in &on_lines {
        s += l;
    }
    if !off_names.is_empty() {
        s += &format!("- 已停用：{}\n", off_names.join("、"));
    }

    if let BootLog::Ok { slow, note, .. } = &d.log {
        if let Some(n) = note {
            s += &format!("\n（讀取開機記錄時：{n}）\n");
        }
        if !slow.is_empty() {
            let first = slow.iter().map(|x| x.first).filter(|&t| t != 0).min();
            let last = slow.iter().map(|x| x.last).max().filter(|&t| t != 0);
            s += "\n## Windows 記錄到拖慢開機的項目（依總共多花的時間排序）\n";
            if let (Some(f), Some(l)) = (first, last) {
                s += &format!("記錄期間：{} ～ {}\n", fmt::ago(f), fmt::ago(l));
            }
            for x in slow.iter().take(30) {
                let mut line = format!("- {}｜{}", x.kind, x.name);
                if !x.raw.is_empty() && x.raw != x.name {
                    line += &format!("（{}）", x.raw);
                }
                line += &format!("：平均多花 {}，{} 次", secs(x.avg_degr() as u64), x.count);
                if x.last != 0 {
                    line += &format!("，最近一次 {}", fmt::ago(x.last));
                }
                if !x.path.is_empty() {
                    line += &format!("；路徑 {}", x.path);
                }
                if !x.startup.is_empty() {
                    line += "；有登記成開機自動啟動";
                }
                if let Some(a) = for_slow(x) {
                    line += &format!("；工具初判：{}", a.rec.label());
                }
                s += &line;
                s += "\n";
            }
            if slow.len() > 30 {
                s += &format!("（另外還有 {} 項，影響較小）\n", slow.len() - 30);
            }
        }
    }
    s += "\n請：\n\
1. 依照對開機速度的影響，排出建議的處理順序（比較舊的記錄請考慮可能已經不準）。\n\
2. 每一項說明它是什麼，建議「開機不啟用」、「延後啟用」還是「保留」，以及具體做法。\n\
3. 看起來重複、殘留，或可以直接解除安裝的軟體也請指出來。\n\
不確定的請標示不確定，不要猜。請用繁體中文回答。\n\
（註：路徑裡的使用者資料夾寫成 %USERPROFILE%，其他使用者名稱寫成 <使用者>，參數的值以「…」省略。）";
    scrub(&s)
}

// ---------------------------------------------------------------------------

pub fn selftest() -> Vec<(&'static str, bool)> {
    use crate::boot::{BootRecord, SysCheck};
    use std::path::PathBuf;
    let rec = |file: &str, name: &str, path: &str| lookup(file, name, path).map(|a| a.rec);

    // 模擬一份系統管理員才讀得到的資料，檢查提問的內容，以及使用者資料夾、參數有沒有被遮掉
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users\someone".into());
    let exe = format!(r"{home}\AppData\Local\Programs\Windsurf\Windsurf.exe");
    let now = win::now_filetime();
    let day = 86_400 * 10_000_000u64;
    let it = StartupItem {
        name: "Windsurf".into(),
        display: "Windsurf".into(),
        command: format!("\"{exe}\" --hidden --token SECRET123"),
        exe: Some(PathBuf::from(&exe)),
        missing: false,
        source: Source::UserRun,
        enabled: true,
        policy: false,
        store_key: None,
        delay_ms: Some(127_100),
        args: "--hidden --token SECRET123".into(),
    };
    let si = SlowItem {
        kind: "程式",
        name: "Windsurf".into(),
        raw: "Windsurf.exe".into(),
        path: exe.clone(),
        count: 2,
        degr_ms: 254_200,
        total_ms: 300_000,
        max_degr_ms: 140_000,
        first: now - 200 * day,
        last: now - 190 * day,
        startup: vec![0],
    };
    let boot = BootRecord { when: now - 190 * day, total_ms: 88_500, main_ms: 56_600, post_ms: 31_900, degraded: true };
    let sys = SysCheck { fast_startup: Some(true), hibernate: Some(true), sys_drive: "C:".into(), sys_free: 1, sys_total: 4, sys_ssd: Some(true), os: "Windows 10".into() };
    let data = BootData { log: BootLog::Ok { boots: vec![boot], slow: vec![si.clone()], note: None }, items: vec![it], sys };
    let report = full_report(&data);
    let one = slow_prompt(&si, &data.items, "Windows 10");
    let start = startup_prompt(&data.items.iter().collect::<Vec<_>>(), "Windows 10");
    let clean = |s: &str| !s.to_lowercase().contains(&home.to_lowercase()) && !s.contains("SECRET123");
    let ann = [r"C:\Users\Ann"];
    vec![
        ("問 AI：完整報告有開機時間、啟動項目與拖慢項目", report.contains("88.5 秒") && report.contains("- Windsurf：") && report.contains("程式｜Windsurf")),
        ("問 AI：拖慢項目的提問", one.contains("事件 101") && one.contains("多花 127.1 秒") && one.contains("有登記成開機自動啟動") && one.contains("開機不啟用")),
        (
            "問 AI：提問裡沒有使用者資料夾和參數的值",
            clean(&report) && clean(&one) && clean(&start) && start.contains(r"%USERPROFILE%\AppData") && start.contains("--hidden --token …"),
        ),
        ("開機建議：AI 編輯器開機不啟用", rec("Windsurf.exe", "Windsurf", r"%USERPROFILE%\AppData\Local\Programs\Windsurf\Windsurf.exe") == Some(Rec::Off)),
        ("開機建議：防毒保留", rec("mc-fw-host.exe", "McAfee Framework Host Service", r"C:\Program Files\McAfee\Framework\mc-fw-host.exe") == Some(Rec::Keep)),
        ("開機建議：Windows 元件保留", rec("svchost.exe", "Windows Services 的主機處理程序", r"C:\Windows\System32\svchost.exe") == Some(Rec::Keep)),
        ("開機建議：Hyper-V 服務保留、vmmem 不叫人關 Docker", rec("vmcompute.exe", "vmcompute", "") == Some(Rec::Keep) && rec("vmmem", "vmmem", "") == Some(Rec::Later)),
        ("開機建議：雲端同步延後", rec("OneDrive.exe", "OneDrive", r"%USERPROFILE%\AppData\Local\Microsoft\OneDrive\OneDrive.exe") == Some(Rec::Later)),
        ("開機建議：VPN、密碼管理保留", rec("Bitwarden.exe", "Bitwarden", r"C:\Program Files\anthropic-tools\Bitwarden.exe") == Some(Rec::Keep)),
        ("開機建議：短名稱只比對完整檔名", rec("pipeline.exe", "Pipeline Tool", r"C:\Tools\pipeline.exe").is_none() && rec("line.exe", "LINE", "") == Some(Rec::Later)),
        ("開機建議：輸入法保留", rec("PIMELauncher.exe", "PIMELauncher", r"C:\Program Files (x86)\PIME\PIMELauncher.exe") == Some(Rec::Keep)),
        ("開機建議：認不得的不亂猜", rec("unknown_helper.exe", "Unknown Helper", r"D:\Apps\unknown_helper.exe").is_none()),
        (
            "開機建議：Adobe 主程式、Steam 裡的工具不會被誤標",
            rec("Photoshop.exe", "Adobe Photoshop", r"C:\Program Files\Adobe\Adobe Photoshop 2025\Photoshop.exe").is_none()
                && rec("wallpaper32.exe", "Wallpaper Engine", r"D:\Steam\steamapps\common\wallpaper_engine\wallpaper32.exe").is_none(),
        ),
        ("開機建議：放在 OneDrive 資料夾裡的程式不算雲端同步", rec("tool.exe", "Tool", r"%USERPROFILE%\OneDrive\Tools\tool.exe").is_none()),
        ("開機建議：使用者名稱不會撞到規則", rec("tool.exe", "Tool", &scrub_with(r"C:\Users\bitwarden\AppData\tool.exe", &[r"C:\Users\bitwarden"])).is_none()),
        ("問 AI：使用者資料夾換成 %USERPROFILE%", scrub_with(r#""C:\Users\Ann\a.exe" C:\USERS\ANN\b"#, &ann) == r#""%USERPROFILE%\a.exe" %USERPROFILE%\b"#),
        ("問 AI：正斜線與 8.3 短路徑", scrub_with(r"c:/users/ann/x C:\Users\ANNLON~1\y", &[r"C:\Users\Ann", r"C:\Users\ANNLON~1"]) == r"%USERPROFILE%/x %USERPROFILE%\y"),
        ("問 AI：非英文名稱不分大小寫", scrub_with(r"C:\Users\élodie\x", &[r"C:\Users\Élodie"]) == r"%USERPROFILE%\x"),
        ("問 AI：路徑後面接 tab 或括號也換", scrub_with("C:\\Users\\Ann\t(C:\\Users\\Ann)", &ann) == "%USERPROFILE%\t(%USERPROFILE%)"),
        (
            "問 AI：名稱開頭相同的資料夾換成 <使用者>，不會錯換",
            scrub_with(r"C:\Users\Annabel\a.exe C:\Users\Ann Marie\b.exe", &ann) == r"C:\Users\<使用者>\a.exe C:\Users\<使用者>\b.exe",
        ),
        ("問 AI：共用的 Public 資料夾不遮", scrub_with(r"C:\Users\Public\x.exe", &ann) == r"C:\Users\Public\x.exe"),
        ("問 AI：email 遮掉", scrub_with("寄給 me.x+1@mail.example.com。", &ann) == "寄給 <email>。"),
        ("問 AI：參數只留開關", safe_args(r#"--token SECRET --hidden /background --user=a@b.com "C:\x y\z.txt""#) == "--token … --hidden /background --user=… …"),
        ("問 AI：中文路徑不會被切壞", scrub_with(r"C:\使用者\小明\檔案", &ann) == r"C:\使用者\小明\檔案"),
    ]
}
