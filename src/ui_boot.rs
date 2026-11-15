//! 開機加速分頁：開機時間、系統設定檢查、開機自動啟動的程式、拖慢開機的項目。

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, vec2};

use crate::advice::{self, Advice, Rec};
use crate::app::{App, Tab};
use crate::boot::{self, Action, BootLog, StartupItem};
use crate::bootopt;
use crate::fmt;
use crate::theme::{self, BLUE, DIM, GREEN, ORANGE, PERI, RED, TEXT};

fn secs(ms: u32) -> String {
    format!("{:.1} 秒", ms as f32 / 1000.0)
}

fn title(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t).size(17.0).strong());
}

/// 超過 30 天前的記錄算「偏舊」
fn stale(when: u64) -> bool {
    when != 0 && crate::win::now_filetime().saturating_sub(when) > 30 * 86_400 * 10_000_000
}

/// 建議標籤：開機不啟用／延後啟用／保留；認不得的是「不確定」。滑鼠移上去說明是什麼、怎麼做。
fn rec_tag(ui: &mut egui::Ui, adv: Option<Advice>) {
    let (text, c) = match adv.map(|a| a.rec) {
        Some(Rec::Off) => ("開機不啟用", theme::TAG_ORANGE),
        Some(Rec::Later) => ("延後啟用", theme::TAG_PERI),
        Some(Rec::Keep) => ("保留", theme::TAG_GREEN),
        None => ("不確定", theme::TAG_GRAY),
    };
    theme::tag(ui, 74.0, text, c, |ui| match adv {
        Some(a) => {
            ui.label(RichText::new(format!("建議：{}", a.rec.label())).strong().color(c.0));
            ui.label(RichText::new(a.what).color(TEXT).strong());
            ui.label(a.how);
            if a.rec == Rec::Later {
                ui.label(RichText::new("這個工具不會自動幫你延後：做法是停用它的自動啟動，開機後過幾分鐘再自己打開。").small().color(c.0));
            }
            ui.label(RichText::new("這是依名稱和位置判斷的，拿不準時可以按「問 AI」再確認。").small().color(DIM));
        }
        None => {
            ui.label(RichText::new("不確定").strong().color(c.0));
            ui.label("這個工具不認得它。按右邊的「問 AI」複製一段提問，貼給 ChatGPT、Claude 或 Gemini，問它是什麼、能不能停用。");
        }
    });
}

const AI_HINT: &str = "複製一段提問到剪貼簿，貼到 ChatGPT、Claude、Gemini 等 AI，就能問它是什麼、要不要停用。\n\
路徑裡的使用者資料夾會換成 %USERPROFILE%，其他使用者名稱和 email 會遮掉，參數只留開關名稱；貼出去之前還是請看一下有沒有私人資料。";

/// 「問 AI」按鈕；剛複製過的顯示「已複製」（字數和「問 AI」差不多，按鈕不會變寬）
fn ai_button(ui: &mut egui::Ui, copied: bool) -> bool {
    let (txt, col) = if copied { ("已複製", GREEN) } else { ("問 AI", BLUE) };
    ui.add_sized([66.0, 20.0], egui::Button::new(RichText::new(txt).size(12.0).color(col))).on_hover_text(AI_HINT).clicked()
}

fn is_copied(app: &App, ui: &egui::Ui, key: &str) -> bool {
    let now = ui.input(|i| i.time);
    app.boot.copied.as_ref().is_some_and(|(k, t)| k == key && now - t < 2.0)
}

fn copy(app: &mut App, ui: &egui::Ui, key: String, text: String) {
    ui.ctx().copy_text(text);
    app.boot.copied = Some((key, ui.input(|i| i.time)));
    // 兩秒後把「已複製」換回來
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(2100));
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    if let Some(rx) = &app.boot.job {
        match rx.try_recv() {
            Ok((d, msg)) => {
                app.boot.data = Some(d);
                if msg.is_some() {
                    app.boot.msg = msg;
                }
                app.boot.job = None;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                app.boot.job = None;
                app.boot.msg = Some(("讀取時發生錯誤，請按「重新整理」再試一次。".into(), true));
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
    }
    if app.boot.data.is_none() && app.boot.job.is_none() && app.boot.msg.is_none() {
        app.start_boot_load(ui.ctx());
    }
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            theme::centered(ui, theme::CONTENT_W + 120.0, |ui| {
                ui.add_space(4.0);
                header(app, ui);
                ui.add_space(8.0);
                if app.boot.data.is_none() {
                    theme::glass().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("讀取開機記錄與啟動項目…");
                        });
                    });
                    return;
                }
                ui.columns(2, |c| {
                    boot_time_card(app, &mut c[0]);
                    sys_card(app, &mut c[1]);
                });
                ui.add_space(10.0);
                startup_card(app, ui);
                ui.add_space(10.0);
                slow_card(app, ui);
                ui.add_space(10.0);
                tasks_card(app, ui);
                ui.add_space(10.0);
                services_card(app, ui);
                ui.add_space(20.0);
            });
        });
    });
    let ctx = ui.ctx().clone();
    if let Some(which) = app.boot.open_on_start.take_if(|_| app.boot.data.is_some()) {
        match which.as_str() {
            "plan" => app.boot.plan = app.boot.data.as_ref().map(|d| build_plan(&d.items, app.elevated)),
            _ => app.boot.show_changes = true,
        }
    }
    plan_modal(app, &ctx);
    changes_modal(app, &ctx);
}

fn header(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.boot.job.is_some();
    ui.horizontal(|ui| {
        ui.label(RichText::new("開機加速").size(20.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled(!busy, egui::Button::new("⟳ 重新整理")).clicked() {
                app.boot.msg = None;
                app.start_boot_load(ui.ctx());
            }
            let done = is_copied(app, ui, "all");
            let r = ui
                .add_enabled(app.boot.data.is_some(), egui::Button::new(if done { "已複製 ✔" } else { "📋 複製完整報告問 AI" }))
                .on_hover_text(
                    "把開機時間、系統設定、開機自動啟動的程式和拖慢開機的項目整理成一段提問，貼給 ChatGPT、Claude 或 Gemini，請它排出處理順序。\n\
                     路徑裡的使用者資料夾會換成 %USERPROFILE%，其他使用者名稱和 email 會遮掉，參數只留開關名稱；貼出去之前還是請看一下有沒有私人資料。",
                );
            if r.clicked()
                && let Some(d) = &app.boot.data
            {
                let text = advice::full_report(d);
                copy(app, ui, "all".into(), text);
            }
            if busy {
                ui.spinner();
                ui.label(RichText::new("處理中…").small().color(DIM));
            }
        });
    });
    ui.label(RichText::new("找出讓開機變慢的原因。停用開機自動啟動的程式和工作管理員的「啟動」分頁相同，隨時可以再打開。").color(DIM));
    if let Some((m, err)) = &app.boot.msg {
        ui.label(RichText::new(m).color(if *err { RED } else { GREEN }));
    }
}

// ---------------------------------------------------------------------------

fn boot_time_card(app: &mut App, ui: &mut egui::Ui) {
    let elevated = app.elevated;
    let Some(data) = &app.boot.data else { return };
    let mut relaunch = false;
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(230.0);
        title(ui, "開機時間");
        match &data.log {
            BootLog::Ok { boots, .. } if !boots.is_empty() => {
                let last = boots.last().unwrap();
                let col = match last.total_ms {
                    0..=30_000 => GREEN,
                    30_001..=60_000 => ORANGE,
                    _ => RED,
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(secs(last.total_ms)).size(34.0).strong().color(col));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(format!("上次開機（{}）", fmt::ago(last.when))).color(DIM));
                        if last.degraded {
                            ui.label(RichText::new("Windows 判定比平常慢").small().color(ORANGE));
                        }
                        if stale(last.when) {
                            ui.label(RichText::new("記錄偏舊，可能和現在不同").small().color(ORANGE)).on_hover_text(format!(
                                "Windows 不是每次開機都會寫這份開機效能記錄。最近一筆是{}，之後的開機都沒有記錄，所以這裡的數字可能和現在的狀況不同。",
                                fmt::ago(last.when)
                            ));
                        }
                    });
                });
                ui.label(
                    RichText::new(format!("到桌面出現 {} · 桌面出現後還在忙 {}", secs(last.main_ms), secs(last.post_ms))).color(TEXT),
                )
                .on_hover_text("「到桌面出現」是按下電源到看到桌面；「還在忙」是桌面出來之後，開機自動啟動的程式和服務還在載入，這段時間電腦會卡卡的。");
                let recent: Vec<_> = boots.iter().rev().take(10).collect();
                let avg = recent.iter().map(|b| b.total_ms as u64).sum::<u64>() / recent.len().max(1) as u64;
                ui.label(RichText::new(format!("最近 {} 次平均 {}", recent.len(), secs(avg as u32))).small().color(DIM));
                ui.add_space(6.0);
                boot_bars(ui, boots);
            }
            BootLog::Ok { .. } => {
                ui.label(RichText::new("還沒有開機記錄。").color(DIM));
            }
            BootLog::NeedAdmin => {
                ui.label(RichText::new("開機時間記錄只有系統管理員讀得到。").color(TEXT));
                ui.label(RichText::new("以系統管理員重新啟動後，可以看到每次開機花了幾秒、哪些程式或驅動拖慢了開機。").small().color(DIM));
                ui.add_space(6.0);
                if !elevated && ui.button("🛡 以系統管理員重新啟動").clicked() {
                    relaunch = true;
                }
            }
            BootLog::Unavailable(m) => {
                ui.label(RichText::new(m).color(DIM));
            }
        }
    });
    if relaunch && crate::win::relaunch_as_admin() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// 最近幾次開機的長條圖：深色是到桌面出現，淺色是桌面出現後還在忙
fn boot_bars(ui: &mut egui::Ui, boots: &[boot::BootRecord]) {
    let shown: Vec<_> = boots.iter().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
    let h = 80.0;
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let max = shown.iter().map(|b| b.total_ms).max().unwrap_or(1).max(1) as f32;
    let n = shown.len().max(1) as f32;
    let gap = 6.0;
    let bw = ((w - gap * (n - 1.0)) / n).clamp(2.0, 26.0);
    let p = ui.painter();
    p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, theme::LINE));
    for (i, b) in shown.iter().enumerate() {
        let x = rect.left() + i as f32 * (bw + gap);
        let total_h = b.total_ms as f32 / max * (h - 6.0);
        let main_h = (b.main_ms.min(b.total_ms)) as f32 / max * (h - 6.0);
        let bar = egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - total_h), egui::pos2(x + bw, rect.bottom()));
        let main = egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - main_h), egui::pos2(x + bw, rect.bottom()));
        let last = i + 1 == shown.len();
        p.rect_filled(bar, 5, theme::with_alpha(PERI, if last { 110 } else { 70 }));
        p.rect_filled(main, 5, if last { BLUE } else { theme::with_alpha(BLUE, 150) });
        // 上緣微亮，霧面立體感
        p.rect_filled(egui::Rect::from_min_max(bar.min, egui::pos2(bar.max.x, bar.min.y + 4.0)).shrink(1.0), 3, theme::white(70));
        let hover = ui.interact(bar.expand2(vec2(gap / 2.0, 0.0)), ui.id().with(("bootbar", i)), Sense::hover());
        hover.on_hover_text(format!(
            "{}\n總共 {}\n到桌面 {}\n之後還在忙 {}",
            fmt::ago(b.when),
            secs(b.total_ms),
            secs(b.main_ms),
            secs(b.post_ms)
        ));
    }
}

// ---------------------------------------------------------------------------

fn sys_card(app: &mut App, ui: &mut egui::Ui) {
    let elevated = app.elevated;
    let busy = app.boot.job.is_some();
    let Some(data) = &app.boot.data else { return };
    let sys = data.sys.clone();
    // 依實際的程式算（同一個指令重複登記的只算一個）
    let enabled = groups(&data.items).iter().filter(|g| g.iter().any(|&i| data.items[i].enabled)).count();
    let mut set_fast: Option<bool> = None;
    let mut go_clean = false;
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(230.0);
        title(ui, "系統設定");
        ui.add_space(4.0);
        let row = |ui: &mut egui::Ui, label: &str, value: RichText, hover: &str, extra: &mut dyn FnMut(&mut egui::Ui)| {
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 22.0], egui::Label::new(RichText::new(label).color(DIM)));
                ui.label(value).on_hover_text(hover);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| extra(ui));
            });
        };
        // 快速啟動
        let ssd = sys.sys_ssd == Some(true);
        let fast_on = sys.fast_startup == Some(true) && sys.hibernate == Some(true);
        let fast_txt = match (sys.fast_startup, sys.hibernate) {
            (Some(true), Some(true)) => RichText::new("已開啟").color(GREEN),
            (Some(true), Some(false)) => RichText::new("沒有作用（休眠已關閉）").color(ORANGE),
            (Some(true), None) => RichText::new("已開啟（休眠狀態不明）").color(DIM),
            // SSD 上差別通常不大，不用警示色
            (Some(false), _) => RichText::new("已關閉").color(if ssd { DIM } else { ORANGE }),
            (None, _) => RichText::new("讀不到").color(DIM),
        };
        row(
            ui,
            "快速啟動",
            fast_txt,
            "關機時把系統核心存起來，下次開機直接載入，冷開機會快一些（傳統硬碟差最多，SSD 上差別通常不大）；「重新啟動」不受影響。\n雙系統、需要網路喚醒或除錯驅動時，有人會刻意關掉它。清理頁的「休眠檔」會把它一起關掉。",
            &mut |ui| {
                let btn = if fast_on { "關閉" } else { "開啟" };
                let r = ui.add_enabled(elevated && !busy, egui::Button::new(btn).small());
                let r = if elevated {
                    r.on_hover_text(if fast_on { "只關掉快速啟動，不動休眠" } else { "需要休眠：休眠原本關著的話，會建立較小的休眠檔（約記憶體的 20%）" })
                } else {
                    r.on_disabled_hover_text("需要系統管理員權限")
                };
                if r.clicked() {
                    set_fast = Some(!fast_on);
                }
            },
        );
        // 系統磁碟空間
        let frac = fmt::pct(sys.sys_free, sys.sys_total);
        let low = frac < 0.10 || sys.sys_free < 20 << 30;
        row(
            ui,
            &format!("{} 剩餘空間", sys.sys_drive),
            RichText::new(format!("{}（{:.0}%）", fmt::size(sys.sys_free), frac * 100.0)).color(if low { RED } else { GREEN }),
            "系統磁碟太滿時，Windows 開機、更新和虛擬記憶體都會變慢。建議至少留 10% 或 20 GB。",
            &mut |ui| {
                if low && ui.small_button("去清理").clicked() {
                    go_clean = true;
                }
            },
        );
        theme::usage_bar(ui, 1.0 - frac, ui.available_width(), 6.0);
        ui.add_space(2.0);
        // 磁碟類型
        let (dt, dc, dh) = match sys.sys_ssd {
            Some(true) => ("SSD", GREEN, "磁碟沒有回報尋軌延遲，通常是固態硬碟，這一項沒問題。（RAID 或虛擬磁碟有時也會這樣回報）"),
            Some(false) => ("傳統硬碟", RED, "系統裝在傳統硬碟上，這通常是開機慢的最大原因。換成 SSD 開機時間常常可以縮短一半以上。"),
            None => ("查不到", DIM, "查不到磁碟類型。"),
        };
        row(ui, "系統磁碟類型", RichText::new(dt).color(dc), dh, &mut |_| {});
        // 啟動項目數量
        let many = enabled > 12;
        row(
            ui,
            "開機自動啟動",
            RichText::new(format!("{enabled} 個")).color(if many { ORANGE } else { GREEN }),
            "每個開機自動啟動的程式都會在登入後搶 CPU 和磁碟。用不到的可以在下面停用。",
            &mut |ui| {
                if many {
                    ui.label(RichText::new("偏多").small().color(ORANGE));
                }
            },
        );
    });
    if let Some(on) = set_fast {
        app.boot.msg = None;
        app.start_boot_action(ui.ctx(), Action::FastStartup(on));
    }
    if go_clean {
        app.tab = Tab::Clean;
    }
}

// ---------------------------------------------------------------------------

/// 指令一字不差、來源相同的項目合成一列（例如 Edge 的 MicrosoftEdgeAutoLaunch_* 常常有十幾筆）。
/// 參數可能分大小寫，所以不轉小寫。
fn groups(items: &[StartupItem]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let groupable = it.store_key.is_none();
        match out.iter_mut().find(|g| {
            let f = &items[g[0]];
            groupable && f.store_key.is_none() && f.source == it.source && f.command.trim() == it.command.trim()
        }) {
            Some(g) => g.push(i),
            None => out.push(vec![i]),
        }
    }
    out
}

/// 目前所有延後項目的延遲秒數（決定下一個要排在多晚）
fn existing_delays(items: &[StartupItem]) -> Vec<u32> {
    items.iter().filter_map(|i| i.delayed.as_ref().map(|d| d.1)).collect()
}

/// 一鍵優化的一列：要處理的一組開機項目（上面清單的一列）
#[derive(Clone, Debug)]
pub struct OptRow {
    pub group: Vec<usize>,
    /// true＝延後啟動，false＝停用
    pub delay: bool,
    pub checked: bool,
    pub title: String,
    pub why: String,
    /// 不能處理的原因（例如要系統管理員）
    pub locked: Option<&'static str>,
}

/// 一鍵優化的候選：建議「開機不啟用」的（包含找不到程式的殘留）預設勾選；建議「延後啟用」的列出來但不預設勾選；
/// 「保留」和「不確定」不列入。已經延後、停用或受原則管理的不列入。
fn build_plan(items: &[StartupItem], elevated: bool) -> Vec<OptRow> {
    let mut rows = Vec::new();
    for g in groups(items) {
        let it = &items[g[0]];
        if !g.iter().any(|&i| items[i].enabled) || g.iter().any(|&i| items[i].delayed.is_some()) || it.policy {
            continue;
        }
        let locked = (it.source.needs_admin() && !elevated).then_some("這一項套用在所有使用者，要系統管理員權限才能改");
        let Some(a) = advice::for_startup(it) else { continue };
        let delay = match a.rec {
            Rec::Off => false,
            Rec::Later if bootopt::launch_spec(it).is_ok() => true,
            _ => continue,
        };
        let title = if g.len() > 1 { format!("{}（{} 筆相同）", it.display, g.len()) } else { it.display.clone() };
        rows.push(OptRow { checked: !delay && locked.is_none(), group: g, delay, title, why: a.what.to_string(), locked });
    }
    // 先停用、再延後；各自照名稱
    rows.sort_by(|a, b| a.delay.cmp(&b.delay).then(a.title.to_lowercase().cmp(&b.title.to_lowercase())));
    rows
}

fn startup_card(app: &mut App, ui: &mut egui::Ui) {
    let elevated = app.elevated;
    let busy = app.boot.job.is_some();
    let Some(data) = &app.boot.data else { return };
    let items = data.items.clone();
    let n_changes = data.changes.len();
    let mut gs = groups(&items);
    let any_on = |g: &Vec<usize>| g.iter().any(|&i| items[i].enabled || items[i].delayed.is_some());
    let delay = |g: &Vec<usize>| g.iter().filter_map(|&i| items[i].delay_ms).max().unwrap_or(0);
    // 會啟動的在前；有量到拖慢時間的再往前；其他照名稱
    gs.sort_by(|a, b| {
        any_on(b).cmp(&any_on(a)).then(delay(b).cmp(&delay(a))).then(items[a[0]].display.to_lowercase().cmp(&items[b[0]].display.to_lowercase()))
    });
    let enabled = gs.iter().filter(|g| g.iter().any(|&i| items[i].enabled)).count();
    let delayed_n = gs.iter().filter(|g| g.iter().any(|&i| items[i].delayed.is_some())).count();
    let total = gs.len();
    let next = bootopt::next_delay(&existing_delays(&items));
    let mut action: Option<Action> = None;
    let mut ask: Option<usize> = None;
    let mut open_plan = false;
    let mut open_changes = false;
    let copied: Vec<bool> = gs.iter().map(|g| is_copied(app, ui, &format!("s{}", g[0]))).collect();
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            title(ui, "開機自動啟動的程式");
            let mut count = format!("{enabled}／{total} 個開機就啟動");
            if delayed_n > 0 {
                count += &format!("、{delayed_n} 個延後");
            }
            ui.label(RichText::new(count).color(DIM));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let r = ui.add_enabled(!busy, egui::Button::new(format!("優化記錄（{n_changes}）"))).on_hover_text(
                    "這個工具做過的停用、延後啟動、排程工作與服務的改動，可以單筆或整批還原。",
                );
                if r.clicked() {
                    open_changes = true;
                }
                let r = ui.add_enabled(!busy, egui::Button::new(RichText::new("⚡ 一鍵優化…").color(BLUE))).on_hover_text(
                    "先列出建議處理的項目給你勾選確認，不會直接動手。建議「開機不啟用」的預設勾選，建議「延後啟用」的要你自己勾；「保留」和「不確定」不會列入。",
                );
                if r.clicked() {
                    open_plan = true;
                }
            });
        });
        ui.label(
            RichText::new(
                "取消勾選＝開機時不自動執行；「延後」＝登入後過幾分鐘才啟動（用 Windows 的工作排程器，不裝常駐程式）。程式本身不會被移除，隨時可以改回來。",
            )
            .small()
            .color(DIM),
        );
        ui.add_space(4.0);
        if items.is_empty() {
            ui.label(RichText::new("沒有找到開機自動啟動的程式。").color(DIM));
        }
        for (gi, g) in gs.iter().enumerate() {
            let it = &items[g[0]];
            let on_count = g.iter().filter(|&&i| items[i].enabled).count();
            let delayed = g.iter().find_map(|&i| items[i].delayed.clone());
            let admin_lock = it.source.needs_admin() && !elevated;
            let locked = admin_lock || it.policy || busy;
            let mut on = on_count > 0 || delayed.is_some();
            let d = delay(g);
            let mut asked = false;
            let mut row_action: Option<Action> = None;
            let group_items = || g.iter().map(|&i| items[i].clone()).collect::<Vec<_>>();
            let (_, _, clicked) = theme::list_row(
                ui,
                26.0,
                |ui| {
                    theme::right_cell(ui, 150.0, it.source.label(), FontId::proportional(12.0), DIM);
                    if d > 0 {
                        theme::right_cell(ui, 100.0, &format!("多花 {}", secs(d)), FontId::proportional(12.5), ORANGE)
                            .on_hover_text("開機效能記錄裡，這個程式讓開機比平常多花的平均時間");
                    } else {
                        theme::right_cell(ui, 100.0, "", FontId::proportional(12.5), DIM);
                    }
                    asked = ai_button(ui, copied[gi]);
                    // 延後：已延後的是一個選單（改時間／取消），可以延後的是一個按鈕
                    let spec = bootopt::launch_spec(it);
                    ui.allocate_ui_with_layout(vec2(122.0, 22.0), Layout::right_to_left(Align::Center), |ui| match &delayed {
                        Some((_, secs_now)) => {
                            ui.add_enabled_ui(!locked, |ui| {
                                ui.menu_button(RichText::new(format!("⏱ 登入後 {}", boot::delay_text(*secs_now))).size(12.0).color(PERI), |ui| {
                                    for s in bootopt::DELAY_CHOICES {
                                        let mark = if s == *secs_now { "✔ " } else { "　 " };
                                        if ui.button(format!("{mark}登入後 {}", boot::delay_text(s))).clicked() {
                                            row_action = Some(Action::Delay(group_items(), s));
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    if ui.button("取消延後，改回開機就啟動").clicked() {
                                        row_action = Some(Action::Undelay(group_items(), true));
                                        ui.close();
                                    }
                                    if ui.button("取消延後並停用").clicked() {
                                        row_action = Some(Action::Undelay(group_items(), false));
                                        ui.close();
                                    }
                                });
                            })
                            .response
                            .on_hover_text("用工作排程器在登入後才啟動它（工作排程器裡的 \\DiskCleaner\\ 資料夾）。點一下可以改時間或取消。");
                        }
                        None if on_count > 0 => {
                            // 建議保留的（防毒、輸入法、驅動程式小幫手）一登入就要在，不提供延後
                            let keep = advice::for_startup(it).is_some_and(|a| a.rec == Rec::Keep);
                            let ok = spec.is_ok() && !locked && !keep;
                            let r = ui.add_enabled(ok, egui::Button::new(RichText::new("延後").size(12.0)).min_size(vec2(56.0, 20.0)));
                            let r = match &spec {
                                Err(why) => r.on_disabled_hover_text(*why),
                                Ok(_) if keep => r.on_disabled_hover_text("建議保留：防毒、輸入法、驅動程式小幫手這類一登入就要在，不提供延後"),
                                Ok(_) if admin_lock => r.on_disabled_hover_text("這一項套用在所有使用者，要系統管理員權限才能改"),
                                Ok(_) => r.on_hover_text(format!(
                                    "改成登入後 {} 才啟動（每個延後的項目錯開 30 秒），開機時就不會跟系統搶資源。之後可以再改時間或取消。",
                                    boot::delay_text(next)
                                )),
                            };
                            if r.clicked() {
                                row_action = Some(Action::Delay(group_items(), next));
                            }
                        }
                        None => {}
                    });
                },
                |ui| {
                    let r = ui.add_enabled_ui(!locked, |ui| theme::check(ui, &mut on)).inner;
                    rec_tag(ui, advice::for_startup(it));
                    if admin_lock {
                        ui.label(RichText::new("🔒").small().color(ORANGE)).on_hover_text("這一項套用在所有使用者，要系統管理員權限才能改");
                    } else if it.policy {
                        ui.label(RichText::new("🔒").small().color(ORANGE)).on_hover_text("由系統原則管理，不能在這裡改");
                    }
                    let mut name = it.display.clone();
                    if g.len() > 1 {
                        name = if on_count == g.len() || on_count == 0 {
                            format!("{name}（{} 筆相同）", g.len())
                        } else {
                            format!("{name}（{} 筆中 {on_count} 筆啟用）", g.len())
                        };
                    }
                    ui.label(RichText::new(name).color(if on { TEXT } else { DIM }).strong());
                    if delayed.is_some() && on_count > 0 {
                        ui.label(RichText::new("會啟動兩次").small().color(RED)).on_hover_text(
                            "它已經設成延後啟動，但原本的開機自動啟動又被打開了（例如在工作管理員重新啟用），開機時和登入後都會各啟動一次。\
                             點右邊的延後選單取消延後，或在這裡取消勾選。",
                        );
                    }
                    if it.missing {
                        ui.label(RichText::new("找不到程式").small().color(RED))
                            .on_hover_text("指令指向的檔案已經不在了，通常是解除安裝後留下的殘留。確認用不到再停用。");
                    }
                    let detail = match &it.exe {
                        Some(e) if it.source == boot::Source::UserFolder || it.source == boot::Source::CommonFolder => format!("{} → {}", it.command, e.display()),
                        _ => it.command.clone(),
                    };
                    ui.add(egui::Label::new(RichText::new(&detail).small().color(DIM)).truncate()).on_hover_text(&detail);
                    r.changed()
                },
            );
            if clicked {
                // 會啟動（含延後）的點一下就停用（延後的會一併取消）；全部停用的點一下全部啟用
                action = Some(Action::Toggle(group_items(), !(on_count > 0 || delayed.is_some())));
            }
            if let Some(a) = row_action {
                action = Some(a);
            }
            if asked {
                ask = Some(gi);
            }
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "這裡列出 Run 機碼、啟動資料夾和 Microsoft Store 應用程式的啟動工作。工作排程器裡「登入時／開機時」執行的工作、設為自動啟動的服務在下面；\
                 「設定 → 帳戶 → 登入選項」裡「自動重新啟動我的應用程式」也可能讓登入變慢。",
            )
            .small()
            .color(DIM),
        );
    });
    if let Some(a) = action {
        app.boot.msg = None;
        app.start_boot_action(ui.ctx(), a);
    }
    if open_plan {
        app.boot.plan = Some(build_plan(&items, elevated));
    }
    if open_changes {
        app.boot.show_changes = true;
    }
    if let Some(gi) = ask
        && let Some(d) = &app.boot.data
    {
        let group: Vec<&StartupItem> = gs[gi].iter().filter_map(|&i| items.get(i)).collect();
        let text = advice::startup_prompt(&group, &d.sys.os);
        copy(app, ui, format!("s{}", gs[gi][0]), text);
    }
}

// ---------------------------------------------------------------------------
// 一鍵優化、優化記錄（對話框）

fn plan_modal(app: &mut App, ctx: &egui::Context) {
    let Some(plan) = &mut app.boot.plan else { return };
    let Some(data) = &app.boot.data else { return };
    let mut close = false;
    let mut run: Option<Action> = None;
    egui::Modal::new(egui::Id::new("boot_plan")).show(ctx, |ui| {
        ui.set_width(720.0);
        ui.heading("一鍵優化");
        ui.label(
            RichText::new("勾選要處理的項目。停用＝開機時不自動執行；延後＝登入後才啟動（從 2 分鐘起、每項錯開 30 秒）。全部都能在「優化記錄」還原。")
                .small()
                .color(DIM),
        );
        ui.add_space(6.0);
        if plan.is_empty() {
            ui.label(RichText::new("目前沒有建議處理的項目。").color(DIM));
        }
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for row in plan.iter_mut() {
                theme::list_row(
                    ui,
                    26.0,
                    |ui| {
                        let (t, c) = if row.delay { ("延後啟動", PERI) } else { ("停用", ORANGE) };
                        theme::right_cell(ui, 80.0, t, FontId::proportional(13.0), c);
                    },
                    |ui| {
                        ui.add_enabled_ui(row.locked.is_none(), |ui| theme::check(ui, &mut row.checked)).response.on_disabled_hover_text(row.locked.unwrap_or(""));
                        ui.label(RichText::new(&row.title).color(TEXT).strong());
                        ui.add(egui::Label::new(RichText::new(&row.why).small().color(DIM)).truncate());
                    },
                );
            }
        });
        ui.add_space(8.0);
        let n = plan.iter().filter(|r| r.checked && r.locked.is_none()).count();
        ui.horizontal(|ui| {
            if ui.add_enabled(n > 0, egui::Button::new(RichText::new(format!("處理 {n} 項")).strong())).clicked() {
                // 延後的時間在這時才排：已經延後的之後，依序錯開
                let mut delays = existing_delays(&data.items);
                let list = plan
                    .iter()
                    .filter(|r| r.checked && r.locked.is_none())
                    .map(|r| {
                        let items: Vec<StartupItem> = r.group.iter().filter_map(|&i| data.items.get(i).cloned()).collect();
                        let d = r.delay.then(|| {
                            let s = bootopt::next_delay(&delays);
                            delays.push(s);
                            s
                        });
                        (items, d)
                    })
                    .collect();
                run = Some(Action::Optimize(list));
                close = true;
            }
            if ui.button("取消").clicked() {
                close = true;
            }
        });
    });
    if close {
        app.boot.plan = None;
    }
    if let Some(a) = run {
        app.boot.msg = None;
        app.start_boot_action(ctx, a);
    }
}

fn changes_modal(app: &mut App, ctx: &egui::Context) {
    if !app.boot.show_changes {
        return;
    }
    let Some(data) = &app.boot.data else { return };
    let elevated = app.elevated;
    let busy = app.boot.job.is_some();
    let mut close = false;
    let mut run: Option<Action> = None;
    egui::Modal::new(egui::Id::new("boot_changes")).show(ctx, |ui| {
        ui.set_width(720.0);
        ui.heading("優化記錄");
        ui.label(
            RichText::new("這個工具做過、目前還有效的改動。「還原」＝改回原本的樣子：重新啟用、刪掉延後啟動的排程工作、恢復排程工作、服務改回「自動」。")
                .small()
                .color(DIM),
        );
        ui.add_space(6.0);
        if data.changes.is_empty() {
            ui.label(RichText::new("沒有改動記錄。").color(DIM));
        }
        let needs_admin = |c: &bootopt::Change| {
            matches!(c.kind, bootopt::ChangeKind::TaskDisabled | bootopt::ChangeKind::ServiceDelayed)
                || matches!(c.target.as_str(), "MachineRun" | "MachineRun32" | "CommonFolder")
        };
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (c, gone) in &data.changes {
                theme::list_row(
                    ui,
                    26.0,
                    |ui| {
                        theme::right_cell(ui, 90.0, &fmt::ago(c.when), FontId::proportional(12.0), DIM);
                        let lock = busy || (needs_admin(c) && !elevated);
                        let r = ui.add_enabled(!lock, egui::Button::new(RichText::new("還原").size(12.0)).min_size(vec2(56.0, 20.0)));
                        let r = if needs_admin(c) && !elevated { r.on_disabled_hover_text("要系統管理員權限才能還原") } else { r };
                        if r.clicked() {
                            run = Some(Action::Restore(vec![c.clone()]));
                        }
                    },
                    |ui| {
                        let col = match c.kind {
                            bootopt::ChangeKind::Delayed | bootopt::ChangeKind::ServiceDelayed => PERI,
                            _ => ORANGE,
                        };
                        ui.add_sized([118.0, 18.0], egui::Label::new(RichText::new(c.kind.label()).small().color(col)));
                        ui.label(RichText::new(&c.label).color(TEXT).strong());
                        let mut extra = String::new();
                        if c.kind == bootopt::ChangeKind::Delayed {
                            extra = format!("登入後 {}", boot::delay_text(c.delay));
                        }
                        if *gone {
                            extra += "（延後的排程工作已經不在了，還原只會重新啟用）";
                        }
                        if !extra.is_empty() {
                            ui.add(egui::Label::new(RichText::new(extra).small().color(if *gone { ORANGE } else { DIM })).truncate());
                        }
                    },
                );
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let all: Vec<bootopt::Change> = data.changes.iter().map(|(c, _)| c.clone()).filter(|c| elevated || !needs_admin(c)).collect();
            let r = ui.add_enabled(!busy && !all.is_empty(), egui::Button::new(RichText::new(format!("全部還原（{} 項）", all.len())).strong()));
            let r = if !elevated && all.len() < data.changes.len() { r.on_hover_text("要系統管理員權限的項目不包含在內") } else { r };
            if r.clicked() {
                run = Some(Action::Restore(all));
            }
            if ui.button("關閉").clicked() {
                close = true;
            }
        });
    });
    if close {
        app.boot.show_changes = false;
    }
    if let Some(a) = run {
        app.boot.msg = None;
        app.start_boot_action(ctx, a);
    }
}

// ---------------------------------------------------------------------------
// 第三方排程工作、服務（系統管理員）

fn tasks_card(app: &mut App, ui: &mut egui::Ui) {
    let elevated = app.elevated;
    let busy = app.boot.job.is_some();
    if app.boot.scroll_to_tasks && app.boot.data.is_some() {
        app.boot.scroll_to_tasks = false;
        ui.scroll_to_cursor(Some(Align::TOP));
    }
    let Some(data) = &app.boot.data else { return };
    let mut action: Option<Action> = None;
    let mut relaunch = false;
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "開機／登入時執行的排程工作");
        ui.label(
            RichText::new("工作排程器裡設成「開機時」或「登入時」執行的第三方工作（多半是自動更新）。這裡只能停用或恢復，不會刪除；Microsoft 的工作不列出。")
                .small()
                .color(DIM),
        );
        ui.add_space(4.0);
        match &data.tasks {
            bootopt::TaskList::NeedAdmin => {
                ui.label(RichText::new("別的程式建立的排程工作，只有系統管理員讀得到。").color(DIM));
                if !elevated && ui.button("🛡 以系統管理員重新啟動").clicked() {
                    relaunch = true;
                }
            }
            bootopt::TaskList::Err(e) => {
                ui.label(RichText::new(e).color(DIM));
            }
            bootopt::TaskList::Ok(list) if list.is_empty() => {
                ui.label(RichText::new("沒有開機或登入時執行的第三方排程工作。").color(DIM));
            }
            bootopt::TaskList::Ok(list) => {
                for t in list {
                    let adv = advice::for_task(t);
                    let keep = adv.is_some_and(|a| a.rec == Rec::Keep);
                    theme::list_row(
                        ui,
                        26.0,
                        |ui| {
                            let when = match (t.boot, t.logon) {
                                (true, true) => "開機、登入時",
                                (true, false) => "開機時",
                                _ => "登入時",
                            };
                            theme::right_cell(ui, 90.0, when, FontId::proportional(12.0), DIM);
                            let (label, target) = if t.enabled { ("停用", false) } else { ("恢復", true) };
                            let r = ui.add_enabled(!busy && !keep && elevated, egui::Button::new(RichText::new(label).size(12.0)).min_size(vec2(56.0, 20.0)));
                            let r = if !elevated { r.on_disabled_hover_text("要系統管理員權限才能修改排程工作") } else if keep { r.on_disabled_hover_text("建議保留，這裡不提供停用") } else { r };
                            if r.clicked() {
                                action = Some(Action::TaskEnabled(t.clone(), target));
                            }
                        },
                        |ui| {
                            rec_tag(ui, adv);
                            let name = t.path.rsplit('\\').next().unwrap_or(&t.path);
                            let who = if t.author.is_empty() { String::new() } else { format!("\n建立者：{}", t.author) };
                            ui.label(RichText::new(name).color(if t.enabled { TEXT } else { DIM }).strong()).on_hover_text(format!("{}{who}", t.path));
                            if !t.enabled {
                                ui.label(RichText::new("已停用").small().color(DIM));
                            }
                            let detail = format!("{} {}", t.command, t.args);
                            ui.add(egui::Label::new(RichText::new(detail.trim()).small().color(DIM)).truncate()).on_hover_text(detail.trim());
                        },
                    );
                }
            }
        }
    });
    if let Some(a) = action {
        app.boot.msg = None;
        app.start_boot_action(ui.ctx(), a);
    }
    if relaunch && crate::win::relaunch_as_admin() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn services_card(app: &mut App, ui: &mut egui::Ui) {
    let elevated = app.elevated;
    let busy = app.boot.job.is_some();
    let Some(data) = &app.boot.data else { return };
    let mut action: Option<Action> = None;
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "自動啟動的第三方服務");
        ui.label(
            RichText::new(
                "設成「自動」的第三方服務會在開機時一起啟動。改成「自動（延遲開始）」是 Windows 內建的延後方式：開機完成後過一陣子才啟動，功能照常。\
                 這裡不提供停用或改成手動；Windows 與 Microsoft 的服務不列出。",
            )
            .small()
            .color(DIM),
        );
        ui.add_space(4.0);
        match &data.services {
            Err(e) => {
                ui.label(RichText::new(e).color(DIM));
            }
            Ok(list) if list.is_empty() => {
                ui.label(RichText::new("沒有設成自動啟動的第三方服務。").color(DIM));
            }
            Ok(list) => {
                for s in list {
                    let adv = advice::for_service(s);
                    let keep = adv.is_some_and(|a| a.rec == Rec::Keep);
                    let grouped = !s.group.is_empty();
                    theme::list_row(
                        ui,
                        26.0,
                        |ui| {
                            theme::right_cell(ui, 70.0, if s.running { "執行中" } else { "" }, FontId::proportional(12.0), DIM);
                            let (label, target) = if s.delayed { ("改回自動", false) } else { ("改成延遲開始", true) };
                            let lock = busy || !elevated || keep || (grouped && target);
                            let r = ui.add_enabled(!lock, egui::Button::new(RichText::new(label).size(12.0)).min_size(vec2(104.0, 20.0)));
                            let r = if !elevated {
                                r.on_disabled_hover_text("要系統管理員權限才能修改服務")
                            } else if keep {
                                r.on_disabled_hover_text("建議保留原本的設定（防毒、驅動相關的服務要一開機就在）")
                            } else if grouped && target {
                                r.on_disabled_hover_text(format!("屬於載入順序群組「{}」的服務，Windows 不允許設成延遲開始", s.group))
                            } else {
                                r.on_hover_text(if target { "開機完成後過一陣子才啟動（Windows 決定時間，通常 1～2 分鐘），功能照常。" } else { "改回開機就啟動。" })
                            };
                            if r.clicked() {
                                action = Some(Action::ServiceDelayed(s.name.clone(), s.display.clone(), target));
                            }
                        },
                        |ui| {
                            rec_tag(ui, adv);
                            let maker = if s.company.is_empty() { String::new() } else { format!("\n廠商：{}", s.company) };
                            ui.label(RichText::new(&s.display).color(TEXT).strong()).on_hover_text(format!("服務名稱：{}\n{}{maker}", s.name, s.exe));
                            if s.delayed {
                                ui.label(RichText::new("已延遲開始").small().color(PERI));
                            }
                            ui.add(egui::Label::new(RichText::new(&s.exe).small().color(DIM)).truncate());
                        },
                    );
                }
            }
        }
    });
    if let Some(a) = action {
        app.boot.msg = None;
        app.start_boot_action(ui.ctx(), a);
    }
}


// ---------------------------------------------------------------------------

fn slow_card(app: &mut App, ui: &mut egui::Ui) {
    let elevated = app.elevated;
    let busy = app.boot.job.is_some();
    let show_all = app.boot.show_all_slow;
    if app.boot.scroll_to_slow && app.boot.data.is_some() {
        app.boot.scroll_to_slow = false;
        ui.scroll_to_cursor(Some(Align::TOP));
    }
    let Some(data) = &app.boot.data else { return };
    let n_slow = match &data.log {
        BootLog::Ok { slow, .. } => slow.len(),
        _ => 0,
    };
    let copied: Vec<bool> = (0..n_slow).map(|i| is_copied(app, ui, &format!("w{i}"))).collect();
    let mut action: Option<Action> = None;
    let mut ask: Option<usize> = None;
    let mut flip_all = false;
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "拖慢開機的項目");
        ui.label(
            RichText::new("Windows 自己的開機效能記錄：這些程式、驅動、服務或裝置讓開機比平常慢，數字是平均每次多花的時間。標籤是建議的處理方式，滑鼠移上去看說明。")
                .small()
                .color(DIM),
        );
        ui.add_space(4.0);
        match &data.log {
            BootLog::Ok { slow, note, .. } => {
                if let Some(n) = note {
                    ui.label(RichText::new(n).small().color(ORANGE));
                }
                if slow.is_empty() {
                    ui.label(RichText::new("最近的開機記錄裡沒有特別拖慢的項目。").color(DIM));
                }
                let first = slow.iter().map(|s| s.first).filter(|&t| t != 0).min();
                let last = slow.iter().map(|s| s.last).max().filter(|&t| t != 0);
                if let (Some(f), Some(l)) = (first, last) {
                    let mut t = format!("記錄期間：{} ～ {}", fmt::ago(f), fmt::ago(l));
                    let col = if stale(l) {
                        t += "。之後的開機都沒有新記錄，這些可能是比較舊的狀況。";
                        ORANGE
                    } else {
                        DIM
                    };
                    ui.label(RichText::new(t).small().color(col));
                }
                let shown = if show_all { slow.len() } else { slow.len().min(15) };
                for (i, s) in slow.iter().enumerate().take(shown) {
                    let linked: Vec<&StartupItem> = s.startup.iter().filter_map(|&k| data.items.get(k)).collect();
                    let mut asked = false;
                    let mut stop = false;
                    theme::list_row(
                        ui,
                        26.0,
                        |ui| {
                            theme::right_cell(ui, 56.0, &format!("{} 次", s.count), FontId::proportional(12.0), DIM);
                            let (txt, col) = if s.avg_degr() > 0 {
                                (format!("多花 {}", secs(s.avg_degr())), ORANGE)
                            } else {
                                (format!("啟動 {}", secs(s.avg_total())), DIM)
                            };
                            let mut hover = format!(
                                "啟動平均花 {}，比平常多 {}；最多一次多花 {}",
                                secs(s.avg_total()),
                                secs(s.avg_degr()),
                                secs(s.max_degr_ms)
                            );
                            if s.last != 0 {
                                hover += &format!("\n最近一次記錄：{}", fmt::ago(s.last));
                            }
                            theme::right_cell(ui, 100.0, &txt, FontId::proportional(13.0), col).on_hover_text(hover);
                            asked = ai_button(ui, copied[i]);
                            // 有登記成開機自動啟動的，直接在這裡停用（和上面清單取消勾選相同）。
                            // 只有全部對應到「同一個指令」（上面清單的同一列）才給按鈕；對到好幾個不同的項目時，請使用者到上面分別處理
                            let on: Vec<&&StartupItem> = linked.iter().filter(|it| it.enabled).collect();
                            let one_row = linked.first().is_some_and(|f| {
                                linked.iter().all(|it| it.source == f.source && it.store_key == f.store_key && it.command.trim() == f.command.trim())
                            });
                            let names = linked.iter().map(|it| format!("・{}（{}）", it.display, it.source.label())).collect::<Vec<_>>().join("\n");
                            if linked.is_empty() {
                                ui.allocate_exact_size(vec2(104.0, 20.0), Sense::hover());
                            } else if !one_row {
                                theme::right_cell(ui, 104.0, &format!("對應 {} 個開機項目", linked.len()), FontId::proportional(12.0), DIM)
                                    .on_hover_text(format!("同一個程式登記成好幾個不同的開機項目，請在上面的清單分別決定：\n{names}"));
                            } else if on.is_empty() {
                                theme::right_cell(ui, 104.0, "已停用自動啟動", FontId::proportional(12.0), GREEN);
                            } else {
                                let lock = busy || on.iter().any(|it| it.policy || (it.source.needs_admin() && !elevated));
                                let r = ui.add_enabled(!lock, egui::Button::new(RichText::new("停用自動啟動").size(12.0)).min_size(vec2(104.0, 20.0)));
                                let what = if on.len() > 1 { format!("這 {} 筆（同一個指令重複登記）", on.len()) } else { "它".to_string() };
                                let r = r.on_hover_text(format!(
                                    "對應到上面清單的開機項目：\n{names}\n停用{what}之後，開機時不會自動執行（和上面清單取消勾選相同），程式本身不會被移除，隨時可以再打開。",
                                ));
                                if r.clicked() {
                                    stop = true;
                                }
                            }
                        },
                        |ui| {
                            let col = match s.kind {
                                "程式" => BLUE,
                                "驅動程式" | "裝置" => Color32::from_rgb(214, 128, 52),
                                "服務" => PERI,
                                _ => DIM,
                            };
                            ui.add_sized([60.0, 18.0], egui::Label::new(RichText::new(s.kind).small().color(col)));
                            rec_tag(ui, advice::for_slow(s));
                            let detail = if s.path.is_empty() { s.raw.clone() } else { s.path.clone() };
                            // 名稱後面接檔名（同名的不同程式，例如防毒的好幾個元件，靠檔名分辨）；放在同一個標籤裡，
                            // 太長時從尾巴截斷，檔名不會被名稱整個擠掉
                            let file = if !s.raw.is_empty() {
                                s.raw.clone()
                            } else {
                                std::path::Path::new(&s.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
                            };
                            let mut job = egui::text::LayoutJob::default();
                            let body = egui::TextStyle::Body.resolve(ui.style());
                            job.append(&s.name, 0.0, egui::TextFormat { font_id: body, color: TEXT, ..Default::default() });
                            if !file.is_empty() && !file.eq_ignore_ascii_case(&s.name) {
                                job.append(&file, 8.0, egui::TextFormat { font_id: FontId::proportional(12.0), color: DIM, valign: Align::Center, ..Default::default() });
                            }
                            ui.add(egui::Label::new(job).truncate()).on_hover_text(if detail.is_empty() { s.name.clone() } else { detail });
                        },
                    );
                    if stop {
                        let on: Vec<StartupItem> = linked.iter().filter(|it| it.enabled).map(|it| (*it).clone()).collect();
                        action = Some(Action::Toggle(on, false));
                    }
                    if asked {
                        ask = Some(i);
                    }
                }
                if slow.len() > 15 {
                    let t = if show_all { "只顯示前 15 項".to_string() } else { format!("顯示全部 {} 項", slow.len()) };
                    if ui.small_button(t).clicked() {
                        flip_all = true;
                    }
                }
                if !slow.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "怎麼處理：有登記成開機自動啟動的程式，可以直接按「停用自動啟動」。沒有登記的，通常是你開機後自己打開的、登入時被自動重新打開的，\
                             或由服務、工作排程器帶起來的，滑鼠移到標籤上看做法。「服務」可以在「服務」（services.msc）把用不到的改成「自動（延遲開始）」或「手動」；\
                             「驅動程式」和「裝置」通常要更新驅動；「群組原則」是公司網域的登入設定，要找 IT 處理。",
                        )
                        .small()
                        .color(DIM),
                    );
                }
            }
            BootLog::NeedAdmin => {
                ui.label(RichText::new("需要系統管理員權限才讀得到。").color(DIM));
            }
            BootLog::Unavailable(m) => {
                ui.label(RichText::new(m).color(DIM));
            }
        }
    });
    if flip_all {
        app.boot.show_all_slow = !show_all;
    }
    if let Some(a) = action {
        app.boot.msg = None;
        app.start_boot_action(ui.ctx(), a);
    }
    if let Some(i) = ask
        && let Some(d) = &app.boot.data
        && let BootLog::Ok { slow, .. } = &d.log
        && let Some(s) = slow.get(i)
    {
        let text = advice::slow_prompt(s, &d.items, &d.sys.os);
        copy(app, ui, format!("w{i}"), text);
    }
}
