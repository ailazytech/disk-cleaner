//! 應用程式外框：狀態、背景工作、上方工具列。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::cleaner::{self, DelProgress, Found, Measured};
use crate::index::{self, Origin, ScanOutput};
use crate::models::Model;
use crate::projects::{Artifact, FindProgress};
use crate::rules::{self, Risk, Rule};
use crate::scan::Progress;
use crate::settings::Settings;
use crate::tree::{Kind, Stats, Tree};
use crate::treemap::Tile;
use crate::win::{self, DriveInfo};
use crate::summary::DriveSummary;
use crate::{fmt, ui_analyze, ui_boot, ui_clean, ui_dash};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dash,
    Clean,
    Analyze,
    Boot,
}

/// 開機加速分頁
#[derive(Default)]
pub struct BootState {
    pub data: Option<crate::boot::BootData>,
    pub job: Option<crossbeam_channel::Receiver<crate::boot::BootData>>,
    /// （訊息, 是不是錯誤）
    pub msg: Option<(String, bool)>,
}

// ---------------------------------------------------------------------------
// 儀表板狀態

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DashMode {
    /// 空間組成：各檔案類型＋可用空間
    Composition,
    /// 可清理：各清理類別＋其他已用＋可用
    Cleanable,
}

pub struct DashJob {
    pub prog: Arc<Progress>,
    pub rx: crossbeam_channel::Receiver<DriveSummary>,
    pub done: Arc<AtomicBool>,
    pub current: Arc<Mutex<String>>,
    /// 只讀快取（啟動時回填），不做完整掃描
    pub cache_only: bool,
}

pub struct DashState {
    /// None = 全部磁碟
    pub drive: Option<char>,
    pub mode: DashMode,
    /// 圖例中隱藏的項目（key）
    pub hidden: std::collections::HashSet<String>,
    pub summaries: std::collections::HashMap<char, DriveSummary>,
    pub job: Option<DashJob>,
    pub recycle: std::collections::HashMap<char, u64>,
    pub recycle_at: Option<Instant>,
    pub hover: Option<String>,
    /// 從儀表板點類型進分析頁時，載入後要突顯的類型
    pub pending_focus: Option<Kind>,
}

// ---------------------------------------------------------------------------
// 清理頁狀態

pub struct RuleRow {
    pub rule: Rule,
    pub measured: Option<Measured>,
    pub checked: bool,
    pub expanded: bool,
}

impl RuleRow {
    pub fn bytes(&self) -> u64 {
        self.measured.as_ref().map(|m| m.bytes).unwrap_or(0)
    }
    pub fn is_command(&self) -> bool {
        matches!(self.rule.action, rules::Action::Command { .. })
    }
    /// 有東西可清：大小 > 0，或資源回收筒裡有項目（只有空檔案／空資料夾時大小是 0，但還是可以清）
    pub fn has_items(&self) -> bool {
        self.bytes() > 0
            || (matches!(self.rule.action, rules::Action::RecycleBin) && self.measured.as_ref().is_some_and(|m| m.files > 0))
    }
}

pub struct MeasureJob {
    pub cancel: Arc<AtomicBool>,
    pub results: Arc<Mutex<Vec<(usize, Measured)>>>,
    pub remaining: Arc<AtomicUsize>,
    pub total: usize,
    pub started: Instant,
}

pub struct ArtRow {
    pub art: Artifact,
    pub checked: bool,
}

/// 刪除「重複的模型」前要做的確認
enum DupCheck {
    None,
    Keep(PathBuf),
    Stale,
}

pub struct ModelRow {
    pub m: Model,
    pub checked: bool,
    /// 是「勾選重複的」或儀表板建議勾的：刪除前要確認保留的那份內容完全相同
    pub via_dup: bool,
}

pub struct ProjState {
    pub new_root: String,
    pub find: Option<Arc<FindProgress>>,
    pub started: Instant,
    pub secs: Option<f32>,
    pub items: Vec<ArtRow>,
    pub models: Vec<ModelRow>,
    pub searched: bool,
    /// 目前顯示的是快取的上次結果（秒數）
    pub cached_age: Option<u64>,
}

pub struct CleanJob {
    pub prog: Arc<DelProgress>,
    pub done: Arc<AtomicBool>,
    pub started: Instant,
    pub planned: u64,
    pub rule_idx: Vec<usize>,
    pub finished: Option<f32>,
    /// 進度視窗的垃圾桶動畫
    pub anim: crate::anim::TrashAnim,
}

pub struct CleanState {
    pub rows: Vec<RuleRow>,
    pub measure: Option<MeasureJob>,
    pub measure_secs: Option<f32>,
    /// 目前顯示的是快取的上次結果（秒數）
    pub cached_age: Option<u64>,
    pub proj: ProjState,
    pub job: Option<CleanJob>,
    pub confirm: bool,
}

// ---------------------------------------------------------------------------
// 分析頁狀態

pub struct ScanJob {
    pub prog: Arc<Progress>,
    pub rx: crossbeam_channel::Receiver<Result<ScanOutput, String>>,
    pub started: Instant,
    pub target: PathBuf,
}

pub struct Loaded {
    pub tree: Tree,
    /// 這次掃描開始的時間（FILETIME）。刪除前用來判斷掃描之後有沒有變動
    pub scanned_at: u64,
    pub origin: Origin,
    pub secs: f32,
    pub target: PathBuf,
    /// 使用者要求的路徑在樹中的節點（MFT 模式下樹是整顆磁碟）
    pub base: u32,
    pub rev: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Folders,
    Types,
    Exts,
    Largest,
}

pub struct LayoutCache {
    pub view: u32,
    pub rect: egui::Rect,
    pub rev: u64,
    pub tiles: Vec<Tile>,
}

pub struct DelRequest {
    pub node: u32,
    pub path: PathBuf,
    pub size: u64,
    pub scanned_at: u64,
    pub permanent: bool,
}

pub struct DelJob {
    pub node: u32,
    pub path: PathBuf,
    pub done: Arc<AtomicBool>,
    pub prog: Arc<DelProgress>,
}

pub struct AnalyzeState {
    pub path_input: String,
    pub job: Option<ScanJob>,
    pub data: Option<Loaded>,
    pub view: u32,
    pub history: Vec<u32>,
    pub selected: Option<u32>,
    pub hovered: Option<u32>,
    pub side: Side,
    pub kind_focus: Option<Kind>,
    pub layout: Option<LayoutCache>,
    pub stats: Option<(u32, u64, Stats)>,
    /// 目前檢視資料夾的子項目（依大小排序）
    pub kids: Option<(u32, u64, Vec<u32>)>,
    pub del: Option<DelRequest>,
    pub del_job: Option<DelJob>,
    pub error: Option<String>,
    pub message: Option<String>,
}

pub struct App {
    pub logo: egui::TextureHandle,
    pub tab: Tab,
    pub elevated: bool,
    pub drives: Vec<DriveInfo>,
    pub drives_at: Instant,
    pub settings: Settings,
    pub clean: CleanState,
    pub an: AnalyzeState,
    pub dash: DashState,
    pub boot: BootState,
}

// ---------------------------------------------------------------------------

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        ("msjh", r"C:\Windows\Fonts\msjh.ttc"),
        ("msjh", r"C:\Windows\Fonts\msjh.ttf"),
        ("noto_tc", r"C:\Windows\Fonts\NotoSansTC-VF.ttf"),
        ("mingliu", r"C:\Windows\Fonts\mingliu.ttc"),
        ("msyh", r"C:\Windows\Fonts\msyh.ttc"),
    ];
    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(bytes)));
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                f.insert(0, name.into());
            }
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                f.push(name.into());
            }
            break;
        }
    }
    // 符號與 emoji 的備援字型（正黑體沒有 ▸、🧹 這類字形）
    for (name, path) in [("segoe_sym", r"C:\Windows\Fonts\seguisym.ttf"), ("segoe_emoji", r"C:\Windows\Fonts\seguiemj.ttf")] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(bytes)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(f) = fonts.families.get_mut(&fam) {
                    f.push(name.into());
                }
            }
        }
    }
    ctx.set_fonts(fonts);
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, analyze: Option<PathBuf>) -> Self {
        setup_fonts(&cc.egui_ctx);
        crate::theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        let rows = rules::catalog()
            .into_iter()
            .map(|rule| RuleRow { rule, measured: None, checked: false, expanded: false })
            .collect();
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([128, 128], include_bytes!("../assets/icon-128.rgba")),
            egui::TextureOptions::LINEAR,
        );
        let mut app = App {
            logo,
            tab: Tab::Dash,
            elevated: win::is_elevated(),
            drives: win::drives(),
            drives_at: Instant::now(),
            settings,
            clean: CleanState {
                rows,
                measure: None,
                measure_secs: None,
                cached_age: None,
                proj: ProjState {
                    new_root: String::new(),
                    find: None,
                    started: Instant::now(),
                    secs: None,
                    items: Vec::new(),
                    models: Vec::new(),
                    searched: false,
                    cached_age: None,
                },
                job: None,
                confirm: false,
            },
            an: AnalyzeState {
                path_input: String::new(),
                job: None,
                data: None,
                view: 0,
                history: Vec::new(),
                selected: None,
                hovered: None,
                side: Side::Folders,
                kind_focus: None,
                layout: None,
                stats: None,
                kids: None,
                del: None,
                del_job: None,
                error: None,
                message: None,
            },
            dash: DashState {
                drive: None,
                mode: DashMode::Composition,
                hidden: Default::default(),
                summaries: crate::summary::load_all(),
                job: None,
                recycle: Default::default(),
                recycle_at: None,
                hover: None,
                pending_focus: None,
            },
            boot: BootState::default(),
        };
        app.load_result_caches();
        app.start_measure(&cc.egui_ctx, None);
        // 還沒有摘要、但有掃描快取的磁碟：背景讀快取回填（不重新掃描）
        let missing: Vec<char> =
            app.drives.iter().map(|d| d.letter()).filter(|l| !app.dash.summaries.contains_key(l)).collect();
        if !missing.is_empty() {
            app.start_dash_scan(&cc.egui_ctx, missing, true);
        }
        if let Some(p) = analyze {
            app.start_scan(&cc.egui_ctx, p, true);
        }
        app
    }

    pub fn refresh_drives(&mut self) {
        self.drives = win::drives();
        self.drives_at = Instant::now();
    }

    // ---------------- 規則大小計算 ----------------

    /// only=None 表示全部重新計算
    pub fn start_measure(&mut self, ctx: &egui::Context, only: Option<Vec<usize>>) {
        if let Some(j) = &self.clean.measure {
            j.cancel.store(true, Relaxed);
        }
        let idx: Vec<usize> = only.unwrap_or_else(|| (0..self.clean.rows.len()).collect());
        let rules: Vec<Rule> = idx.iter().map(|&i| self.clean.rows[i].rule.clone()).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let results = Arc::new(Mutex::new(Vec::new()));
        let remaining = Arc::new(AtomicUsize::new(rules.len()));
        self.clean.measure = Some(MeasureJob {
            cancel: cancel.clone(),
            results: results.clone(),
            remaining: remaining.clone(),
            total: rules.len(),
            started: Instant::now(),
        });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            cleaner::measure_all(&rules, &cancel, |i, m| {
                results.lock().push((idx[i], m));
                remaining.fetch_sub(1, Relaxed);
                ctx.request_repaint();
            });
        });
    }

    fn poll_measure(&mut self) {
        let Some(job) = &self.clean.measure else { return };
        let done: Vec<(usize, Measured)> = std::mem::take(&mut *job.results.lock());
        let finished = job.remaining.load(Relaxed) == 0;
        let secs = job.started.elapsed().as_secs_f32();
        for (i, m) in done {
            let elevated = self.elevated;
            let row = &mut self.clean.rows[i];
            let first = row.measured.is_none();
            row.measured = Some(m);
            if first {
                row.checked = default_checked(row, elevated);
            }
            if !row.has_items() && !matches!(row.rule.action, rules::Action::Command { .. }) {
                row.checked = false;
            }
        }
        if finished {
            self.clean.measure = None;
            self.clean.measure_secs = Some(secs);
            self.clean.cached_age = None;
            let all: Vec<(&str, &Measured)> =
                self.clean.rows.iter().filter_map(|r| r.measured.as_ref().map(|m| (r.rule.id, m))).collect();
            crate::results_cache::save_rules(&all);
        }
    }

    fn load_result_caches(&mut self) {
        if let Some((map, age)) = crate::results_cache::load_rules() {
            let elevated = self.elevated;
            for row in &mut self.clean.rows {
                if let Some(m) = map.get(row.rule.id) {
                    row.measured = Some(m.clone());
                    row.checked = default_checked(row, elevated);
                }
            }
            self.clean.cached_age = Some(age);
        }
        if let Some((arts, models, age)) = crate::results_cache::load_projects() {
            self.clean.proj.models = models.into_iter().map(|m| ModelRow { m, checked: false, via_dup: false }).collect();
            let week_ago = win::now_filetime().saturating_sub(7 * win::FILETIME_DAY);
            self.clean.proj.items = arts
                .into_iter()
                .map(|art| {
                    let checked = art.kind.risk == Risk::Safe && art.newest < week_ago;
                    ArtRow { art, checked }
                })
                .collect();
            self.clean.proj.items.sort_by(|a, b| b.art.bytes.cmp(&a.art.bytes));
            self.clean.proj.searched = true;
            self.clean.proj.cached_age = Some(age);
        }
    }

    fn save_projects(&self) {
        let v: Vec<&Artifact> = self.clean.proj.items.iter().map(|a| &a.art).collect();
        let m: Vec<&Model> = self.clean.proj.models.iter().map(|r| &r.m).collect();
        crate::results_cache::save_projects(&v, &m);
    }

    // ---------------- 專案產物搜尋 ----------------

    pub fn start_find(&mut self, ctx: &egui::Context) {
        if let Some(f) = &self.clean.proj.find {
            f.cancel.store(true, Relaxed);
        }
        let prog = Arc::new(FindProgress::default());
        self.clean.proj.find = Some(prog.clone());
        self.clean.proj.started = Instant::now();
        self.clean.proj.items.clear();
        self.clean.proj.models.clear();
        self.clean.proj.searched = true;
        self.clean.proj.secs = None;
        let roots = self.settings.roots.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            crate::projects::find(&roots, &prog);
            prog.pending.store(u64::MAX, Relaxed); // 標記完成
            ctx.request_repaint();
        });
    }

    fn poll_find(&mut self) {
        let Some(f) = &self.clean.proj.find else { return };
        let new: Vec<Artifact> = std::mem::take(&mut *f.results.lock());
        let finished = f.pending.load(Relaxed) == u64::MAX;
        let week_ago = win::now_filetime().saturating_sub(7 * win::FILETIME_DAY);
        for art in new {
            let checked = art.kind.risk == Risk::Safe && art.newest < week_ago;
            self.clean.proj.items.push(ArtRow { art, checked });
        }
        self.clean.proj.items.sort_by(|a, b| b.art.bytes.cmp(&a.art.bytes));
        let new_models: Vec<Model> = std::mem::take(&mut *f.models.lock());
        if !new_models.is_empty() {
            let checked: std::collections::HashSet<PathBuf> =
                self.clean.proj.models.iter().filter(|r| r.checked).map(|r| r.m.path.clone()).collect();
            let via_dup: std::collections::HashSet<PathBuf> =
                self.clean.proj.models.iter().filter(|r| r.via_dup).map(|r| r.m.path.clone()).collect();
            let mut all: Vec<Model> = self.clean.proj.models.drain(..).map(|r| r.m).collect();
            all.extend(new_models);
            crate::models::dedup(&mut all);
            all.sort_by(|a, b| b.bytes.cmp(&a.bytes));
            self.clean.proj.models =
                all.into_iter().map(|m| ModelRow { checked: checked.contains(&m.path), via_dup: via_dup.contains(&m.path), m }).collect();
        }
        if finished {
            self.clean.proj.secs = Some(self.clean.proj.started.elapsed().as_secs_f32());
            self.clean.proj.find = None;
            self.clean.proj.cached_age = None;
            self.save_projects();
        }
    }

    // ---------------- 清理 ----------------

    pub fn selected_bytes(&self) -> (u64, usize) {
        let mut b = 0;
        let mut n = 0;
        for r in &self.clean.rows {
            if r.checked {
                // 外部指令（Docker、DISM…）能釋放多少無法事先知道，不計入預計釋放
                if !r.is_command() {
                    b += r.bytes();
                }
                n += 1;
            }
        }
        for a in &self.clean.proj.items {
            if a.checked {
                b += a.art.bytes;
                n += 1;
            }
        }
        for m in &self.clean.proj.models {
            if m.checked {
                b += m.m.bytes;
                n += 1;
            }
        }
        (b, n)
    }

    /// 開機加速分頁：在背景讀開機記錄、啟動項目與系統設定
    pub fn start_boot_load(&mut self, ctx: &egui::Context) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.boot.job = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::boot::load());
            ctx.request_repaint();
        });
    }

    /// 只用來截圖、看動畫：模擬一次清理的進度。只改進度數字，不會刪除或碰任何檔案。
    pub fn start_demo_clean(&mut self, ctx: &egui::Context) {
        let prog = Arc::new(DelProgress::default());
        let done = Arc::new(AtomicBool::new(false));
        prog.steps.store(5, Relaxed);
        self.clean.job = Some(CleanJob {
            prog: prog.clone(),
            done: done.clone(),
            started: Instant::now(),
            planned: 9_800_000_000,
            rule_idx: Vec::new(),
            finished: None,
            anim: Default::default(),
        });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let steps = [("npm 快取", "npm-cache\\_cacache\\content-v2\\sha512"), ("使用者暫存資料夾", "Temp\\pip-build-env-x8k2"), ("uv 快取", "uv\\cache\\archive-v0\\aB3dE"), ("Chrome 快取", "Google\\Chrome\\User Data\\Default\\Cache\\Cache_Data"), ("縮圖快取", "Microsoft\\Windows\\Explorer")];
            for (k, (name, sub)) in steps.iter().enumerate() {
                for i in 0..220u64 {
                    if prog.cancel.load(Relaxed) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(9 + (k as u64 * 3)));
                    prog.files.fetch_add(3 + i % 5, Relaxed);
                    prog.freed.fetch_add(1_400_000 + (i % 7) * 900_000, Relaxed);
                    *prog.current.lock() = format!("C:\\Users\\me\\AppData\\Local\\{sub}\\{:x}\\{:04}.tmp", i * 7919, i);
                    ctx.request_repaint();
                }
                prog.log(format!("✔ {name}：釋放 {}", fmt::size(1_900_000_000 + k as u64 * 130_000_000)));
                prog.step.fetch_add(1, Relaxed);
            }
            done.store(true, Relaxed);
            ctx.request_repaint();
        });
    }

    pub fn start_clean(&mut self, ctx: &egui::Context) {
        let prog = Arc::new(DelProgress::default());
        let done = Arc::new(AtomicBool::new(false));
        let mut plan: Vec<(Rule, Vec<Found>)> = Vec::new();
        let mut rule_idx = Vec::new();
        for (i, r) in self.clean.rows.iter().enumerate() {
            if r.checked && (!r.rule.admin || self.elevated) {
                let found = r.measured.as_ref().map(|m| m.found.clone()).unwrap_or_default();
                plan.push((r.rule.clone(), found));
                rule_idx.push(i);
            }
        }
        let arts: Vec<(PathBuf, &'static str, u64)> = self
            .clean
            .proj
            .items
            .iter()
            .filter(|a| a.checked)
            .map(|a| (a.art.path.clone(), a.art.kind.id, a.art.newest))
            .collect();
        // 因為「重複」才勾的模型：刪除前要和保留的那份比對內容
        let rows = &self.clean.proj.models;
        let dups = crate::suggest::model_dups(rows);
        let models: Vec<(Model, DupCheck)> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.checked)
            .map(|(i, r)| {
                let check = if !r.via_dup || r.m.del != crate::models::ModelDel::File {
                    DupCheck::None
                } else {
                    match dups[i] {
                        Some(k) if !rows[k].checked => DupCheck::Keep(rows[k].m.path.clone()),
                        Some(_) => DupCheck::None, // 保留的那份也被手動勾了：照使用者的選擇
                        None => DupCheck::Stale,
                    }
                };
                (r.m.clone(), check)
            })
            .collect();
        let (planned, _) = self.selected_bytes();
        prog.steps.store((plan.len() + arts.len() + models.len()) as u64, Relaxed);
        self.clean.job = Some(CleanJob {
            prog: prog.clone(),
            done: done.clone(),
            started: Instant::now(),
            planned,
            rule_idx,
            finished: None,
            anim: Default::default(),
        });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            com_init();
            // 指令類（DISM 等）放最後
            plan.sort_by_key(|(r, _)| matches!(r.action, rules::Action::Command { .. }));
            for (rule, found) in &plan {
                if prog.cancel.load(Relaxed) {
                    break;
                }
                *prog.current.lock() = rule.name.clone();
                // 刪除前重新測量：畫面上可能是快取的舊結果，不能拿舊路徑直接刪
                let fresh = if matches!(rule.action, rules::Action::Delete) {
                    cleaner::measure(rule, &prog.cancel).found
                } else {
                    found.clone()
                };
                cleaner::clean_rule(rule, &fresh, &prog);
                prog.step.fetch_add(1, Relaxed);
                ctx.request_repaint();
            }
            for a in &arts {
                if prog.cancel.load(Relaxed) {
                    break;
                }
                // 刪除前重新確認它現在還是同一種建置產物（快取可能是舊的）
                if crate::projects::revalidate(&a.0).is_none_or(|k| k.id != a.1) {
                    prog.log(format!("⚠ {}：已經不是建置產物（或專案標記不見了），已略過", a.0.display()));
                    prog.failed.fetch_add(1, Relaxed);
                    prog.step.fetch_add(1, Relaxed);
                    continue;
                }
                // 掃描之後又有新的檔案（例如剛重新 build 過）：勾選時看到的「多久沒動」已經不對了
                *prog.current.lock() = format!("確認 {} 沒有變動…", a.0.display());
                let now = crate::scan::dir_size(&a.0, u64::MAX, &prog.cancel);
                if prog.cancel.load(Relaxed) {
                    break;
                }
                if now.errors > 0 {
                    prog.log(format!("⚠ {}：有 {} 個資料夾讀不到，無法確認有沒有變動，已略過", a.0.display(), now.errors));
                    prog.failed.fetch_add(1, Relaxed);
                    prog.step.fetch_add(1, Relaxed);
                    continue;
                }
                if now.newest > a.2 {
                    prog.log(format!("⚠ {}：掃描之後有新的變動（{}），已略過；請重新搜尋再決定", a.0.display(), fmt::ago(now.newest)));
                    prog.failed.fetch_add(1, Relaxed);
                    prog.step.fetch_add(1, Relaxed);
                    continue;
                }
                let before = prog.freed.load(Relaxed);
                let ok = cleaner::delete_dir(&a.0, &prog);
                let freed = prog.freed.load(Relaxed) - before;
                prog.log(format!(
                    "{} {}：釋放 {}",
                    if ok { "✔" } else { "⚠" },
                    a.0.display(),
                    fmt::size(freed)
                ));
                prog.step.fetch_add(1, Relaxed);
                ctx.request_repaint();
            }
            for (m, check) in &models {
                if prog.cancel.load(Relaxed) {
                    break;
                }
                *prog.current.lock() = m.name.clone();
                let before = prog.freed.load(Relaxed);
                let ok = match (check, &m.del) {
                    (DupCheck::Stale, _) => {
                        prog.log(format!("⚠ 模型 {}：已經不算重複了（保留的那份不在清單裡），已略過", m.name));
                        prog.failed.fetch_add(1, Relaxed);
                        false
                    }
                    // 因為「重複」才勾的：比對與刪除用同一組 handle，保留的那份全程鎖住
                    (DupCheck::Keep(keep), _) => {
                        *prog.current.lock() = format!("比對 {} 與保留的那份…", m.name);
                        match crate::models::delete_duplicate(&m.path, keep, &prog.cancel) {
                            Ok(freed) => {
                                prog.freed.fetch_add(freed, Relaxed);
                                prog.files.fetch_add(1, Relaxed);
                                true
                            }
                            Err(why) => {
                                prog.log(format!("⚠ 模型 {}：{why}，已略過", m.name));
                                prog.failed.fetch_add(1, Relaxed);
                                false
                            }
                        }
                    }
                    (DupCheck::None, crate::models::ModelDel::Dir) => cleaner::delete_dir(&m.path, &prog),
                    (DupCheck::None, crate::models::ModelDel::File) => cleaner::delete_file(&m.path, u64::MAX, &prog),
                    (DupCheck::None, crate::models::ModelDel::Ollama) => match crate::models::delete_ollama(&m.path, true) {
                        Ok((freed, failed)) => {
                            prog.freed.fetch_add(freed, Relaxed);
                            if failed > 0 {
                                prog.log(format!("   ⚠ 有 {failed} 個模型檔刪不掉（可能使用中）"));
                                prog.failed.fetch_add(failed, Relaxed);
                            }
                            failed == 0
                        }
                        Err(e) => {
                            prog.log(format!("   ⚠ {e}"));
                            prog.failed.fetch_add(1, Relaxed);
                            false
                        }
                    },
                };
                let freed = prog.freed.load(Relaxed) - before;
                prog.log(format!("{} 模型 {}（{}）：釋放 {}", if ok { "✔" } else { "⚠" }, m.name, m.source, fmt::size(freed)));
                prog.step.fetch_add(1, Relaxed);
                ctx.request_repaint();
            }
            done.store(true, Relaxed);
            ctx.request_repaint();
        });
    }

    fn poll_clean(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.clean.job else { return };
        if job.finished.is_none() && job.done.load(Relaxed) {
            job.finished = Some(job.started.elapsed().as_secs_f32());
            let idx = job.rule_idx.clone();
            // 移除已刪掉的專案產物
            self.clean.proj.items.retain(|a| !(a.checked && !a.art.path.exists()));
            for a in &mut self.clean.proj.items {
                a.checked = false;
            }
            self.clean.proj.models.retain(|m| m.m.path.exists());
            for m in &mut self.clean.proj.models {
                m.checked = false;
            }
            self.save_projects();
            self.refresh_drives();
            let _ = idx;
            // 重新計算全部規則（若清理時有全量計算在跑，只算部分會讓其他規則停在舊數字）
            self.start_measure(ctx, None);
        }
    }

    // ---------------- 磁碟分析 ----------------

    pub fn start_scan(&mut self, ctx: &egui::Context, target: PathBuf, prefer_cache: bool) {
        if let Some(j) = &self.an.job {
            j.prog.cancel.store(true, Relaxed);
        }
        let prog = Arc::new(Progress::default());
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.an.job = Some(ScanJob { prog: prog.clone(), rx, started: Instant::now(), target: target.clone() });
        self.an.error = None;
        self.an.path_input = target.to_string_lossy().into_owned();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = index::run(&target, &prog, prefer_cache);
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.tab = Tab::Analyze;
    }

    fn poll_scan(&mut self) {
        let Some(job) = &self.an.job else { return };
        let Ok(res) = job.rx.try_recv() else { return };
        let scanned_at = win::now_filetime().saturating_sub(job.started.elapsed().as_nanos() as u64 / 100);
        self.an.job = None;
        match res {
            Ok(out) => {
                // 掃的是整顆磁碟（或 MFT 整顆索引）就順便更新儀表板的摘要
                let root_name = out.tree.name(0).to_string();
                if root_name.len() == 3 && root_name.ends_with(":\\") {
                    let letter = root_name.chars().next().unwrap_or('?').to_ascii_uppercase();
                    let s = DriveSummary::from_tree(letter, &out.tree, 0);
                    self.dash.summaries.insert(letter, s);
                    crate::summary::save_all(&self.dash.summaries);
                }
                let rev = self.an.data.as_ref().map(|d| d.rev + 1).unwrap_or(1);
                self.an.view = out.view;
                self.an.history.clear();
                self.an.selected = None;
                self.an.layout = None;
                self.an.stats = None;
                self.an.kind_focus = None;
                self.an.data = Some(Loaded { tree: out.tree, scanned_at, origin: out.origin, secs: out.secs, target: out.target, base: out.view, rev });
                if let Some(k) = self.dash.pending_focus.take() {
                    self.an.kind_focus = Some(k);
                    self.an.side = Side::Types;
                }
            }
            Err(e) => self.an.error = Some(e),
        }
    }

    // ---------------- 儀表板：磁碟摘要 ----------------

    /// 依序取得各磁碟的空間組成。cache_only=true 只讀快取（啟動回填用），否則沒有快取就完整掃描。
    pub fn start_dash_scan(&mut self, ctx: &egui::Context, letters: Vec<char>, cache_only: bool) {
        if self.dash.job.is_some() {
            return;
        }
        let prog = Arc::new(Progress::default());
        let (tx, rx) = crossbeam_channel::unbounded();
        let done = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(String::new()));
        self.dash.job = Some(DashJob { prog: prog.clone(), rx, done: done.clone(), current: current.clone(), cache_only });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for letter in letters {
                if prog.cancelled() {
                    break;
                }
                *current.lock() = format!("{letter}:");
                ctx.request_repaint();
                let summary = if cache_only {
                    index::load_cached_drive(letter).map(|(tree, _)| DriveSummary::from_tree(letter, &tree, 0))
                } else {
                    // 計數歸零，讓進度顯示這顆磁碟的數字
                    for c in [&prog.files, &prog.dirs, &prog.bytes, &prog.total, &prog.done] {
                        c.store(0, Relaxed);
                    }
                    let root = PathBuf::from(format!("{letter}:\\"));
                    index::run(&root, &prog, true).ok().map(|out| {
                        let node = if out.tree.name(0).len() == 3 { 0 } else { out.view };
                        DriveSummary::from_tree(letter, &out.tree, node)
                    })
                };
                if let Some(s) = summary {
                    let _ = tx.send(s);
                }
                ctx.request_repaint();
            }
            done.store(true, Relaxed);
            ctx.request_repaint();
        });
    }

    fn poll_dash(&mut self) {
        let Some(job) = &self.dash.job else { return };
        let mut changed = false;
        while let Ok(s) = job.rx.try_recv() {
            self.dash.summaries.insert(s.letter, s);
            changed = true;
        }
        let finished = job.done.load(Relaxed);
        if changed {
            crate::summary::save_all(&self.dash.summaries);
        }
        if finished {
            self.dash.job = None;
        }
    }

    /// 各磁碟資源回收筒的大小（30 秒更新一次）
    pub fn recycle_sizes(&mut self) -> &std::collections::HashMap<char, u64> {
        if self.dash.recycle_at.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            self.dash.recycle = self
                .drives
                .iter()
                .map(|d| (d.letter(), win::recycle_bin_size(Some(&d.root)).0))
                .collect();
            self.dash.recycle_at = Some(Instant::now());
        }
        &self.dash.recycle
    }

    fn poll_del(&mut self) {
        let Some(job) = &self.an.del_job else { return };
        if !job.done.load(Relaxed) {
            return;
        }
        let job = self.an.del_job.take().unwrap();
        if let Some(d) = &mut self.an.data {
            // 用不跟隨連結的方式確認（壞掉的連結 exists() 也會回傳 false）；
            // 只有「找不到」才算刪掉，權限或 I/O 錯誤不算
            if std::fs::symlink_metadata(&job.path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
                d.tree.mark_deleted(job.node);
                d.rev += 1;
                self.an.message = Some(format!("已刪除 {}", job.path.display()));
            } else {
                let failed = job.prog.failed.load(Relaxed);
                let reason = job.prog.log.lock().last().cloned();
                self.an.message = Some(match reason {
                    Some(r) => format!("沒有刪除：{r}"),
                    None => format!(
                        "部分檔案無法刪除（{failed} 個使用中或無權限），已釋放 {}。建議重新掃描。",
                        fmt::size(job.prog.freed.load(Relaxed))
                    ),
                });
            }
            if self.an.selected == Some(job.node) {
                self.an.selected = None;
            }
        }
        self.refresh_drives();
    }

    pub fn start_delete(&mut self, ctx: &egui::Context, req: DelRequest) {
        let prog = Arc::new(DelProgress::default());
        let done = Arc::new(AtomicBool::new(false));
        self.an.del_job = Some(DelJob { node: req.node, path: req.path.clone(), done: done.clone(), prog: prog.clone() });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            com_init();
            let meta = win::stat(&req.path);
            let is_dir = meta.is_some_and(|m| m.0 & win::ATTR_DIRECTORY != 0);
            // 掃描之後被換掉或有新東西（大小不同、建立／修改／改名時間比掃描晚）就不刪，請使用者重新掃描。
            // 同大小的新檔案光比大小看不出來，所以也比時間；資料夾會檢查裡面每一個檔案。
            let changed = match cleaner::changed_since(&req.path, req.scanned_at) {
                Ok(c) => c,
                Err(_) => meta.is_some(), // 還在卻讀不到時間：不冒險
            };
            if !is_dir && meta.is_some_and(|m| m.1 != req.size && m.0 & win::ATTR_REPARSE == 0) || changed {
                prog.log("掃描之後有變動，請重新掃描再刪除");
                prog.failed.fetch_add(1, Relaxed);
            } else {
                // 永久刪除與移到資源回收筒共用：受保護位置、路徑經過連結、雲端檔（資料夾含裡面每一層）。
                // 回傳的 pins 握住上層資料夾，回收完成前路徑不會被換掉
                match cleaner::precheck(&req.path) {
                    Err(why) => {
                        prog.log(why);
                        prog.failed.fetch_add(1, Relaxed);
                    }
                    Ok(pins) => {
                        if req.permanent {
                            drop(pins); // 永久刪除自己會從頭握住每一層
                            if is_dir {
                                cleaner::delete_dir(&req.path, &prog);
                            } else {
                                cleaner::delete_file(&req.path, u64::MAX, &prog);
                            }
                        } else {
                            if !win::move_to_recycle_bin(std::slice::from_ref(&req.path)) {
                                prog.log("無法移到資源回收筒（路徑太長、檔案使用中，或取消了操作）");
                                prog.failed.fetch_add(1, Relaxed);
                            }
                            drop(pins);
                        }
                    }
                }
            }
            done.store(true, Relaxed);
            ctx.request_repaint();
        });
    }

    pub fn busy(&self) -> bool {
        self.clean.measure.is_some()
            || self.clean.proj.find.is_some()
            || self.clean.job.as_ref().is_some_and(|j| j.finished.is_none())
            || self.an.job.is_some()
            || self.an.del_job.is_some()
            || self.dash.job.is_some()
    }

    // ---------------- 上方工具列 ----------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.add(egui::Image::from_texture(&self.logo).fit_to_exact_size(egui::vec2(26.0, 26.0)));
            ui.label(RichText::new("DiskCleaner").heading().strong());
            ui.add_space(16.0);
            let tab = |ui: &mut egui::Ui, cur: &mut Tab, t: Tab, label: &str| {
                let sel = *cur == t;
                let txt = RichText::new(label).size(15.5);
                let txt = if sel { txt.strong().color(crate::theme::BLUE) } else { txt };
                if ui.selectable_label(sel, txt).clicked() {
                    *cur = t;
                }
            };
            tab(ui, &mut self.tab, Tab::Dash, "  儀表板  ");
            tab(ui, &mut self.tab, Tab::Clean, "  清理  ");
            tab(ui, &mut self.tab, Tab::Analyze, "  磁碟分析  ");
            tab(ui, &mut self.tab, Tab::Boot, "  開機加速  ");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.elevated {
                    ui.label(RichText::new("🛡 系統管理員").color(crate::theme::GREEN))
                        .on_hover_text("已啟用 MFT 極速掃描與系統項目清理");
                } else if ui
                    .button("🛡 以系統管理員重新啟動")
                    .on_hover_text("啟用 MFT 極速掃描（整顆磁碟數秒）與系統項目（Windows Update、系統暫存…）清理")
                    .clicked()
                    && win::relaunch_as_admin()
                {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}

pub fn com_init() {
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    unsafe {
        let _ = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_measure();
        self.poll_find();
        self.poll_clean(&ctx);
        self.poll_scan();
        self.poll_del();
        self.poll_dash();
        if self.drives_at.elapsed() > Duration::from_secs(20) {
            self.refresh_drives();
        }

        // 拖放資料夾 → 分析
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if let Some(p) = dropped.into_iter().next() {
            let p = if p.is_dir() { p } else { p.parent().map(|x| x.to_path_buf()).unwrap_or(p) };
            self.start_scan(&ctx, p, true);
        }

        crate::theme::paint_background(&ctx);
        egui::Panel::top("top")
            .frame(crate::theme::bar())
            .show(ui, |ui| self.top_bar(ui));

        match self.tab {
            Tab::Dash => ui_dash::show(self, ui),
            Tab::Clean => ui_clean::show(self, ui),
            Tab::Analyze => ui_analyze::show(self, ui),
            Tab::Boot => ui_boot::show(self, ui),
        }

        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }
}

/// 規則預設是否勾選：「安全」、有東西可清、權限足夠、不是外部指令。
fn default_checked(row: &RuleRow, elevated: bool) -> bool {
    crate::suggest::rule(row, elevated)
}
