//! 開機項目的建議（開機不啟用／延後啟用／保留），以及一鍵複製「問 AI」的提問文字。
//!
//! 建議只是依執行檔名稱與路徑比對常見的程式，認不得的標「不確定」，交給使用者自己判斷或問 AI。
//! 這裡不會改任何設定；複製出去的提問會把使用者資料夾換成 %USERPROFILE%。

use crate::boot::{BootData, BootLog, Source, SlowItem, StartupItem};
use crate::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rec {
    /// 開機時用不到：停用它的自動啟動，要用時再手動打開
    Off,
    /// 需要它常駐，但不必一開機就跟系統搶資源
    Later,
    /// 系統、防毒或硬體需要，留著
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
    /// 名稱或路徑（小寫）包含這些字就算
    subs: &'static [&'static str],
    advice: Advice,
}

const fn a(rec: Rec, what: &'static str, how: &'static str) -> Advice {
    Advice { rec, what, how }
}

/// 解除安裝後留下、指向不存在檔案的項目
pub const LEFTOVER: Advice = a(Rec::Off, "解除安裝後留下的殘留", "要執行的程式已經不在了，開機時只會執行失敗。停用它不會有任何影響。");

/// 由上往下比對，第一個符合的就是答案（越特定的放越前面）。
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
            "防毒要一開機就在跑才有即時保護，保留。不過一台電腦只需要一套防毒：如果它是買電腦時附的試用版、你也沒有付費，\
             可以在「設定 → 應用程式」解除安裝，Windows 會自動改用內建的 Microsoft Defender，開機通常會快很多。",
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
        stems: &["vboxheadless", "virtualbox", "virtualboxvm", "vboxsvc", "vboxsds"],
        subs: &["virtualbox"],
        advice: a(
            Rec::Off,
            "VirtualBox 虛擬機（VBoxHeadless 是在背景執行、沒有視窗的虛擬機）",
            "有虛擬機被設成開機自動啟動（VirtualBox 的自動啟動、Vagrant 或其他工具）。用不到時在它的設定關掉自動啟動、要用再開，\
             可以省下大量記憶體和磁碟讀寫。",
        ),
    },
    Entry {
        stems: &["vmmem", "vmmemwsl", "vmcompute", "docker desktop", "com.docker.backend", "com.docker.build", "dockerd"],
        subs: &["docker desktop", "\\docker\\"],
        advice: a(
            Rec::Off,
            "WSL 2、Docker Desktop 或 Hyper-V 的虛擬機",
            "通常是 Docker Desktop 設成「登入時啟動」，或有程式在開機時把 WSL、Hyper-V 虛擬機叫起來。\
             到 Docker Desktop 的設定關掉「Start Docker Desktop when you sign in」，要用時再開。",
        ),
    },
    Entry {
        stems: &["crosvm", "wsaclient", "wsaservice", "hd-player", "bluestacks", "noxvmhandle", "nox", "dnplayer", "memu", "memuheadless"],
        subs: &["google play games", "googleplaygames", "bluestacks", "\\nox\\", "ldplayer", "microvirt"],
        advice: a(
            Rec::Off,
            "Android 模擬器或 Google Play 遊戲（電腦版）的虛擬機",
            "開機就把 Android 虛擬機開起來，很吃記憶體和磁碟。到它的設定關掉開機自動啟動，要玩時再開。",
        ),
    },
    Entry {
        stems: &["msedge", "chrome", "firefox", "opera", "brave", "vivaldi", "browser_assistant"],
        subs: &["opera browser assistant"],
        advice: a(
            Rec::Off,
            "瀏覽器的背景預先載入",
            "Edge 的「啟動加速」、Chrome 的「關閉 Google Chrome 後繼續執行背景應用程式」會讓瀏覽器一開機就先載入。\
             停用後第一次打開瀏覽器會慢一兩秒，其他不受影響。Edge 也可以在「設定 → 系統與效能」關掉「啟動加速」。",
        ),
    },
    Entry {
        stems: &[
            "adobearm", "acrotray", "adobeupdateservice", "adobegcclient", "agsservice", "agcinvokerutility", "creative cloud",
            "adobe desktop service", "ccxprocess", "coresync", "jusched", "jucheck", "googleupdate", "softwareupdate", "ituneshelper",
            "apsdaemon", "ccleaner", "ccleaner64", "driverbooster", "wshelper",
        ],
        subs: &["\\adobe\\", "\\java\\java update", "ccleaner", "\\iobit\\", "wondershare"],
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
        subs: &["\\discord\\", "telegram desktop", "\\line\\", "\\slack\\", "\\zoom\\", "whatsapp", "\\skype", "\\signal\\"],
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
        subs: &["\\steam\\", "epic games", "battle.net", "electronic arts", "\\ubisoft", "gog galaxy", "riot games", "\\garena", "overwolf"],
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
        // 只比對程式本身的位置與檔名：放在 OneDrive、Dropbox 同步資料夾裡的其他程式不算
        subs: &[
            "microsoft\\onedrive\\", "onedrive.exe", "dropbox.exe", "google\\drive", "googledrivefs", "icloud", "synology drive", "nextcloud.exe",
            "megasync", "pcloud.exe", "baidunetdisk",
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
        subs: &["logitech", "\\logi\\", "razer", "corsair", "steelseries", "armoury crate", "lenovo vantage", "\\msi\\"],
        advice: a(
            Rec::Later,
            "滑鼠、鍵盤或電腦廠商的工具",
            "自訂按鍵、燈光、風扇或電池設定要它開著才有作用。常用這些功能就保留；只用預設功能的話可以停用。",
        ),
    },
    Entry {
        stems: &[
            "powertoys", "everything", "sharex", "snipaste", "ditto", "flux", "greenshot", "lightshot", "autohotkey", "autohotkey64",
            "autohotkeyu64", "keepass", "keepassxc", "1password", "bitwarden", "nordvpn", "expressvpn", "protonvpn", "openvpnconnect",
            "openvpn-gui", "ollama app", "ollama", "quicklook", "translucenttb", "rainmeter", "surfshark", "tailscale", "tailscale-ipn", "sandman",
        ],
        subs: &[
            "powertoys", "\\everything\\", "sharex", "snipaste", "1password", "bitwarden", "keepass", "nordvpn", "expressvpn", "protonvpn",
            "\\ollama\\", "surfshark", "tailscale", "sandboxie",
        ],
        advice: a(
            Rec::Later,
            "常駐小工具（剪貼簿、截圖、搜尋、密碼管理、VPN 等）",
            "天天會用就保留；偶爾才用的話，可以停用自動啟動，要用時再打開。",
        ),
    },
    Entry {
        stems: &["phoneexperiencehost", "yourphone", "cortana", "skypeapp", "skypebridge", "gamebar", "gamebarftserver", "xboxapp", "onenotem", "widgets"],
        subs: &[
            "microsoft.yourphone", "cortana", "microsoft.549981c3f5f10", "microsoft.xboxgamingoverlay", "microsoft.skypeapp", "microsoft.gamingapp",
            "microsoft.xboxapp",
        ],
        advice: a(Rec::Off, "Windows 附帶的 App", "沒在用就停用，要用時照樣可以從開始功能表打開。"),
    },
    Entry {
        stems: &["anydesk", "teamviewer", "teamviewer_service", "rustdesk", "parsecd", "remoting_host", "todesk", "sunloginclient"],
        subs: &["anydesk", "teamviewer", "rustdesk", "\\parsec\\", "splashtop", "chrome remote desktop", "todesk", "sunlogin"],
        advice: a(
            Rec::Later,
            "遠端桌面",
            "要在沒人顧的時候從別的地方連進這台電腦，就保留；只是偶爾讓別人連進來幫忙，可以停用，要用時再打開。",
        ),
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

/// 依執行檔名稱（file）、顯示名稱（name）與路徑或指令（path）找建議。
pub fn lookup(file: &str, name: &str, path: &str) -> Option<Advice> {
    let file = file.trim().to_lowercase();
    let stem = file.strip_suffix(".exe").unwrap_or(&file);
    let hay = format!("{}\n{}", name.to_lowercase(), path.to_lowercase());
    TABLE.iter().find(|e| (!stem.is_empty() && e.stems.contains(&stem)) || e.subs.iter().any(|s| hay.contains(s))).map(|e| e.advice)
}

fn file_of(p: &std::path::Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn for_startup(it: &StartupItem) -> Option<Advice> {
    if it.missing {
        return Some(LEFTOVER);
    }
    let file = it.exe.as_deref().map(file_of).unwrap_or_default();
    let path = format!("{}\n{}", it.command, it.exe.as_ref().map(|e| e.display().to_string()).unwrap_or_default());
    lookup(&file, &format!("{}\n{}", it.display, it.name), &path)
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
        _ => lookup(&s.raw, &s.name, &s.path),
    }
}

// ---------------------------------------------------------------------------
// 問 AI 的提問

fn secs(ms: u64) -> String {
    format!("{:.1} 秒", ms as f64 / 1000.0)
}

/// 把路徑裡的使用者資料夾（例如 C:\Users\名字）換成 %USERPROFILE%，不把使用者名稱貼給 AI。
pub fn scrub_with(s: &str, home: &str) -> String {
    let home = home.trim_end_matches('\\');
    if home.len() < 4 {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(w) = s.get(i..i + home.len())
            && w.eq_ignore_ascii_case(home)
            // 只換完整的資料夾名稱：C:\Users\Lan 不能換掉 C:\Users\Lancelot 的開頭
            && s[i + home.len()..].chars().next().is_none_or(|c| matches!(c, '\\' | '/' | '"' | '\'' | ' ' | '\n' | ';' | ','))
        {
            out.push_str("%USERPROFILE%");
            i += home.len();
            continue;
        }
        let c = s[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8().max(1);
    }
    out
}

fn scrub(s: &str) -> String {
    match std::env::var("USERPROFILE") {
        Ok(h) => scrub_with(s, &h),
        Err(_) => s.to_string(),
    }
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
不確定的地方請直接說不確定，不要猜。請用繁體中文回答。";

fn advice_line(adv: Option<Advice>) -> String {
    match adv {
        Some(a) => format!("- 我用的工具初步判斷：{}（{}）\n", a.rec.label(), a.what),
        None => "- 我用的工具認不出這是什麼\n".into(),
    }
}

/// 一組開機自動啟動項目（指令相同、合成一列的）的提問
pub fn startup_prompt(group: &[&StartupItem], os: &str) -> String {
    let Some(it) = group.first() else { return String::new() };
    let on = group.iter().filter(|i| i.enabled).count();
    let mut s = format!("我在用 {os}，想讓開機快一點，正在整理「開機自動啟動」的程式。請幫我看看下面這一項：\n\n");
    s += &format!("- 名稱：{}\n", it.display);
    if it.source == Source::StoreApp {
        s += &format!("- 套件：{}（啟動工作：{}）\n", it.command, it.name);
    } else {
        s += &format!("- 指令：{}\n", it.command);
        if let Some(e) = &it.exe
            && e.display().to_string() != it.command
        {
            s += &format!("- 實際執行的檔案：{}\n", e.display());
        }
    }
    s += &format!("- 登記在：{}\n", it.source.location());
    if group.len() > 1 {
        s += &format!("- 同樣的指令重複登記了 {} 筆，其中 {on} 筆啟用\n", group.len());
    } else {
        s += &format!("- 現在：{}\n", if on > 0 { "會自動啟動" } else { "已停用" });
    }
    if it.missing {
        s += "- 找不到要執行的檔案（可能是解除安裝後的殘留）\n";
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
        s += &format!("- 最近一次記錄：{}\n", fmt::ago(si.last));
    }
    let linked: Vec<&StartupItem> = si.startup.iter().filter_map(|&i| items.get(i)).collect();
    if let Some(it) = linked.first() {
        let on = linked.iter().any(|i| i.enabled);
        s += &format!("- 它有登記成開機自動啟動：{}（{}），現在{}\n", it.display, it.source.location(), if on { "會自動啟動" } else { "已停用" });
    } else if si.kind == "程式" {
        s += "- 它沒有登記在 Run 機碼、啟動資料夾或 Store 啟動工作裡（可能是服務、工作排程器、別的程式帶起來的，或是我開機後自己打開的）\n";
    }
    s += &advice_line(for_slow(si));
    s += "\n";
    s += ASK;
    scrub(&s)
}

/// 整頁的資料寫成一段提問，請 AI 排出處理順序。
pub fn full_report(d: &BootData) -> String {
    let mut s = format!(
        "我在用 {}，開機很慢，想請你幫我排出處理順序。以下是我用工具整理的資料（路徑裡的使用者資料夾已換成 %USERPROFILE%）。\n\n",
        d.sys.os
    );
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
            (Some(true), _) => "已開啟",
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

    // 開機自動啟動：指令相同的合成一筆
    let mut seen: Vec<(Source, &str)> = Vec::new();
    let mut on_lines = Vec::new();
    let mut off_names = Vec::new();
    for it in &d.items {
        if it.store_key.is_none() {
            if seen.iter().any(|(src, c)| *src == it.source && c.trim() == it.command.trim()) {
                continue;
            }
            seen.push((it.source, &it.command));
        }
        let same: Vec<&StartupItem> = if it.store_key.is_none() {
            d.items.iter().filter(|o| o.store_key.is_none() && o.source == it.source && o.command.trim() == it.command.trim()).collect()
        } else {
            vec![it]
        };
        if same.iter().any(|i| i.enabled) {
            let mut extra = vec![it.source.label().to_string()];
            if same.len() > 1 {
                extra.push(format!("重複登記 {} 筆", same.len()));
            }
            if let Some(ms) = same.iter().filter_map(|i| i.delay_ms).max() {
                extra.push(format!("開機記錄平均多花 {}", secs(ms as u64)));
            }
            if it.missing {
                extra.push("找不到程式".into());
            }
            if let Some(a) = for_startup(it) {
                extra.push(format!("工具初判：{}", a.rec.label()));
            }
            on_lines.push(format!("- {}：{}（{}）\n", it.display, it.command.trim(), extra.join("；")));
        } else {
            off_names.push(it.display.clone());
        }
    }
    s += &format!("\n## 開機自動啟動的程式（{} 個會啟動）\n", on_lines.len());
    for l in &on_lines {
        s += l;
    }
    if !off_names.is_empty() {
        s += &format!("- 已停用：{}\n", off_names.join("、"));
    }

    if let BootLog::Ok { slow, .. } = &d.log
        && !slow.is_empty()
    {
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
    s += "\n請：\n\
1. 依照對開機速度的影響，排出建議的處理順序。\n\
2. 每一項說明它是什麼，建議「開機不啟用」、「延後啟用」還是「保留」，以及具體做法。\n\
3. 看起來重複、殘留，或可以直接解除安裝的軟體也請指出來。\n\
不確定的請標示不確定，不要猜。請用繁體中文回答。";
    scrub(&s)
}

// ---------------------------------------------------------------------------

pub fn selftest() -> Vec<(&'static str, bool)> {
    use crate::boot::{BootRecord, SysCheck};
    use std::path::PathBuf;
    let rec = |file: &str, name: &str, path: &str| lookup(file, name, path).map(|a| a.rec);

    // 模擬一份系統管理員才讀得到的資料，檢查提問的內容，以及使用者資料夾有沒有被換掉
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users\someone".into());
    let exe = format!(r"{home}\AppData\Local\Programs\Windsurf\Windsurf.exe");
    let now = crate::win::now_filetime();
    let day = 86_400 * 10_000_000u64;
    let it = StartupItem {
        name: "Windsurf".into(),
        display: "Windsurf".into(),
        command: format!("\"{exe}\" --hidden"),
        exe: Some(PathBuf::from(&exe)),
        missing: false,
        source: Source::UserRun,
        enabled: true,
        policy: false,
        store_key: None,
        delay_ms: Some(127_100),
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
    let clean = |s: &str| !s.to_lowercase().contains(&home.to_lowercase());
    vec![
        ("問 AI：完整報告有開機時間、啟動項目與拖慢項目", report.contains("88.5 秒") && report.contains("- Windsurf：") && report.contains("程式｜Windsurf")),
        ("問 AI：拖慢項目的提問", one.contains("事件 101") && one.contains("多花 127.1 秒") && one.contains("有登記成開機自動啟動") && one.contains("開機不啟用")),
        ("問 AI：提問裡沒有使用者資料夾", clean(&report) && clean(&one) && clean(&start) && start.contains(r"%USERPROFILE%\AppData")),
        ("開機建議：AI 編輯器開機不啟用", rec("Windsurf.exe", "Windsurf", r"C:\Users\x\AppData\Local\Programs\Windsurf\Windsurf.exe") == Some(Rec::Off)),
        ("開機建議：防毒保留", rec("mc-fw-host.exe", "McAfee Framework Host Service", r"C:\Program Files\McAfee\Framework\mc-fw-host.exe") == Some(Rec::Keep)),
        ("開機建議：Windows 元件保留", rec("svchost.exe", "Windows Services 的主機處理程序", r"C:\Windows\System32\svchost.exe") == Some(Rec::Keep)),
        ("開機建議：雲端同步延後", rec("OneDrive.exe", "OneDrive", r"C:\Users\x\AppData\Local\Microsoft\OneDrive\OneDrive.exe /background") == Some(Rec::Later)),
        ("開機建議：短名稱只比對完整檔名", rec("pipeline.exe", "Pipeline Tool", r"C:\Tools\pipeline.exe") .is_none() && rec("line.exe", "LINE", "") == Some(Rec::Later)),
        ("開機建議：輸入法保留", rec("PIMELauncher.exe", "PIMELauncher", r"C:\Program Files (x86)\PIME\PIMELauncher.exe") == Some(Rec::Keep)),
        ("開機建議：認不得的不亂猜", rec("unknown_helper.exe", "Unknown Helper", r"D:\Apps\unknown_helper.exe").is_none()),
        ("開機建議：放在 OneDrive 資料夾裡的程式不算雲端同步", rec("tool.exe", "Tool", r"C:\Users\x\OneDrive\Tools\tool.exe").is_none()),
        ("問 AI：使用者資料夾換成 %USERPROFILE%", scrub_with(r#""C:\Users\Lan\a.exe" C:\USERS\LAN\b"#, r"C:\Users\Lan") == r#""%USERPROFILE%\a.exe" %USERPROFILE%\b"#),
        ("問 AI：名稱開頭相同的資料夾不會被換掉", scrub_with(r"C:\Users\Lancelot\a.exe", r"C:\Users\Lan") == r"C:\Users\Lancelot\a.exe"),
        ("問 AI：中文路徑不會被切壞", scrub_with(r"C:\使用者\小明\檔案", r"C:\Users\Lan") == r"C:\使用者\小明\檔案"),
    ]
}
