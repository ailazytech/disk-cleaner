//! 儀表板：立體透明圓餅圖（空間組成／可清理，可依磁碟與類型篩選）＋建議清理清單。

use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke};

use crate::app::{App, DashMode, Tab};
use crate::pie3d::{self, Slice};
use crate::rules::Cat;
use crate::suggest::{self, drive_of};
use crate::theme::{self, BLUE, DIM, GREEN, MIST, ORANGE, PERI, PINK, RED, SKY, SLATE, TEAL, TEXT, YELLOW};
use crate::tree::{KIND_COUNT, Kind};
use crate::fmt;

#[derive(Clone, Copy)]
enum Click {
    None,
    Kind(Kind),
    Clean,
}

struct Item {
    key: String,
    label: String,
    bytes: u64,
    color: Color32,
    clear: bool,
    click: Click,
}

const FREE_COLOR: Color32 = MIST;
const DARK_SLATE: Color32 = SLATE;

fn kind_color(k: Kind) -> Color32 {
    let [r, g, b] = k.color();
    Color32::from_rgb(r, g, b)
}

fn selected_drives(app: &App) -> Vec<crate::win::DriveInfo> {
    app.drives.iter().filter(|d| app.dash.drive.is_none_or(|l| l == d.letter())).cloned().collect()
}

/// 空間組成：各檔案類型＋未分析＋可用。回傳（項目、還沒有摘要的磁碟）
fn composition(app: &App) -> (Vec<Item>, Vec<char>) {
    let mut kinds = [0u64; KIND_COUNT];
    let mut unknown = 0u64;
    let mut free = 0u64;
    let mut missing = Vec::new();
    for d in selected_drives(app) {
        free += d.free;
        match app.dash.summaries.get(&d.letter()) {
            Some(s) => {
                // 硬連結、稀疏檔（虛擬磁碟）會讓檔案大小加總超過實際佔用：依實際已用空間等比例換算
                let scale = if s.indexed > d.used() && s.indexed > 0 { d.used() as f64 / s.indexed as f64 } else { 1.0 };
                for (k, v) in kinds.iter_mut().zip(s.kinds.iter()) {
                    *k += (*v as f64 * scale) as u64;
                }
                unknown += d.used().saturating_sub(s.indexed);
            }
            None => {
                unknown += d.used();
                missing.push(d.letter());
            }
        }
    }
    let mut items: Vec<Item> = Kind::ALL[1..]
        .iter()
        .map(|&k| Item {
            key: format!("k{}", k as u8),
            label: k.label().to_string(),
            bytes: kinds[k as usize],
            color: kind_color(k),
            clear: false,
            click: Click::Kind(k),
        })
        .filter(|i| i.bytes > 0)
        .collect();
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    if unknown > 0 {
        items.push(Item {
            key: "unknown".into(),
            label: "未分析／系統保留".into(),
            bytes: unknown,
            color: DARK_SLATE,
            clear: false,
            click: Click::None,
        });
    }
    items.push(Item { key: "free".into(), label: "可用空間".into(), bytes: free, color: FREE_COLOR, clear: true, click: Click::None });
    (items, missing)
}

fn cat_color(c: Cat) -> Color32 {
    match c {
        Cat::DevCache => BLUE,
        Cat::SysTemp => ORANGE,
        Cat::SysUpdate => RED,
        Cat::Browser => YELLOW,
        Cat::Apps => SKY,
        Cat::Recycle => TEAL,
    }
}

/// 可清理：各清理類別＋專案產物＋AI 模型＋其他已用＋可用
fn cleanable(app: &mut App) -> Vec<Item> {
    let drive = app.dash.drive;
    let on = |p: &std::path::Path| drive.is_none() || drive_of(p) == drive;
    let mut cats: Vec<(Cat, u64)> = Cat::ALL.iter().map(|&c| (c, 0)).collect();
    for row in &app.clean.rows {
        if row.is_command() || row.rule.cat == Cat::Recycle {
            continue;
        }
        let Some(m) = &row.measured else { continue };
        let b: u64 = m.found.iter().filter(|f| f.mode != crate::cleaner::Mode::Info && on(&f.path)).map(|f| f.bytes).sum();
        if let Some(c) = cats.iter_mut().find(|c| c.0 == row.rule.cat) {
            c.1 += b;
        }
    }
    let recycle: u64 = {
        let r = app.recycle_sizes();
        match drive {
            Some(l) => r.get(&l).copied().unwrap_or(0),
            None => r.values().sum(),
        }
    };
    if let Some(c) = cats.iter_mut().find(|c| c.0 == Cat::Recycle) {
        c.1 = recycle;
    }
    let proj: u64 = app.clean.proj.items.iter().filter(|a| on(&a.art.path)).map(|a| a.art.bytes).sum();
    let models: u64 = app.clean.proj.models.iter().filter(|m| on(&m.m.path)).map(|m| m.m.bytes).sum();

    let mut items: Vec<Item> = cats
        .iter()
        .filter(|c| c.1 > 0)
        .map(|&(c, b)| Item { key: format!("c{:?}", c), label: c.label().into(), bytes: b, color: cat_color(c), clear: false, click: Click::Clean })
        .collect();
    if proj > 0 {
        items.push(Item { key: "proj".into(), label: "專案建置產物".into(), bytes: proj, color: PERI, clear: false, click: Click::Clean });
    }
    if models > 0 {
        items.push(Item { key: "models".into(), label: "AI 模型".into(), bytes: models, color: PINK, clear: false, click: Click::Clean });
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let cleanable: u64 = items.iter().map(|i| i.bytes).sum();
    let (used, free) = selected_drives(app).iter().fold((0, 0), |a, d| (a.0 + d.used(), a.1 + d.free));
    items.push(Item {
        key: "c_other".into(),
        label: "其他已用空間".into(),
        bytes: used.saturating_sub(cleanable),
        color: DARK_SLATE,
        clear: false,
        click: Click::None,
    });
    items.push(Item { key: "c_free".into(), label: "可用空間".into(), bytes: free, color: FREE_COLOR, clear: true, click: Click::None });
    items
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            theme::centered(ui, theme::CONTENT_W + 120.0, |ui| {
                ui.add_space(4.0);
                filters(app, ui);
                ui.add_space(8.0);
                kpis(app, ui);
                ui.add_space(10.0);
                pie_card(app, ui, &ctx);
                ui.add_space(10.0);
                suggestions(app, ui);
                ui.add_space(20.0);
            });
        });
    });
}

fn filters(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        let chip = |ui: &mut egui::Ui, sel: bool, add: &mut dyn FnMut(&mut egui::Ui)| -> bool {
            let frame = theme::pill()
                .fill(if sel { Color32::from_rgb(228, 236, 255) } else { Color32::WHITE })
                .stroke(Stroke::new(if sel { 1.5 } else { 1.0 }, if sel { BLUE } else { theme::LINE }))
                .inner_margin(egui::Margin::symmetric(12, 6))
                .corner_radius(12);
            let r = frame.show(ui, |ui| add(ui)).response.interact(Sense::click());
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            r.clicked()
        };
        let total: u64 = app.drives.iter().map(|d| d.total).sum();
        let free: u64 = app.drives.iter().map(|d| d.free).sum();
        if chip(ui, app.dash.drive.is_none(), &mut |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("全部磁碟").strong());
                ui.label(RichText::new(format!("剩 {} ／ {}", fmt::size(free), fmt::size(total))).small().color(DIM));
            });
        }) {
            app.dash.drive = None;
        }
        let drives = app.drives.clone();
        for d in &drives {
            let frac = fmt::pct(d.used(), d.total);
            let analyzed = app.dash.summaries.contains_key(&d.letter());
            if chip(ui, app.dash.drive == Some(d.letter()), &mut |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}:", d.letter())).strong());
                        let label = if d.label.is_empty() { d.kind } else { d.label.as_str() };
                        ui.label(RichText::new(label).small().color(DIM));
                        if !analyzed {
                            ui.label(RichText::new("●").small().color(ORANGE)).on_hover_text("還沒有空間組成資料");
                        }
                    });
                    theme::usage_bar(ui, frac, 120.0, 6.0);
                    ui.label(RichText::new(format!("剩 {}", fmt::size(d.free))).small().color(if frac > 0.9 { RED } else { DIM }));
                });
            }) {
                app.dash.drive = Some(d.letter());
            }
        }
    });
}

fn tile(ui: &mut egui::Ui, title: &str, value: &str, sub: &str, color: Color32) {
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).color(DIM));
        ui.label(RichText::new(value).size(26.0).strong().color(color));
        ui.label(RichText::new(sub).small().color(DIM));
    });
}

fn kpis(app: &mut App, ui: &mut egui::Ui) {
    let drives = selected_drives(app);
    let (total, used, free) = drives.iter().fold((0, 0, 0), |a, d| (a.0 + d.total, a.1 + d.used(), a.2 + d.free));
    let clean_total: u64 = cleanable(app).iter().filter(|i| i.key != "c_other" && i.key != "c_free").map(|i| i.bytes).sum();
    let sugg = suggest::list(app, app.dash.drive);
    let sugg_total: u64 = sugg.iter().map(|s| s.bytes).sum();
    ui.columns(4, |cols| {
        tile(&mut cols[0], "已用空間", &fmt::size(used), &format!("共 {}（{:.0}%）", fmt::size(total), fmt::pct(used, total) * 100.0), theme::STRONG);
        tile(&mut cols[1], "可用空間", &fmt::size(free), &format!("{:.0}% 可用", fmt::pct(free, total) * 100.0), if fmt::pct(free, total) < 0.1 { RED } else { GREEN });
        tile(&mut cols[2], "可清理", &fmt::size(clean_total), "清理頁找到的快取、產物與模型", ORANGE);
        tile(&mut cols[3], "建議清理", &fmt::size(sugg_total), &format!("{} 項，清完可用 {}", sugg.len(), fmt::size(free + sugg_total)), BLUE);
    });
}

fn pie_card(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    let mode = app.dash.mode;
    let (items, missing) = match mode {
        DashMode::Composition => composition(app),
        DashMode::Cleanable => (cleanable(app), Vec::new()),
    };
    let mut action: Option<Click> = None;
    let mut scan_missing = false;
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        // 標題列：模式切換
        ui.horizontal(|ui| {
            ui.label(RichText::new("空間分布").size(17.0).strong());
            ui.add_space(12.0);
            for (m, label, tip) in [
                (DashMode::Composition, "空間組成", "依檔案類型：影片、圖片、程式碼…（需要該磁碟的掃描資料）"),
                (DashMode::Cleanable, "可清理", "依清理類別：開發快取、暫存、專案產物、AI 模型…"),
            ] {
                if ui.selectable_label(app.dash.mode == m, label).on_hover_text(tip).clicked() {
                    app.dash.mode = m;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !app.dash.hidden.is_empty() && ui.small_button("顯示全部類型").clicked() {
                    app.dash.hidden.clear();
                }
                ui.label(RichText::new("點圖例可隱藏／顯示該類型").small().color(DIM));
            });
        });

        // 缺少資料的提示
        if mode == DashMode::Composition {
            if let Some(job) = &app.dash.job {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let what = if job.cache_only { "讀取快取" } else { "分析" };
                    ui.label(RichText::new(format!(
                        "正在{what} {}… {}",
                        job.current.lock(),
                        if job.prog.files.load(Relaxed) > 0 { format!("{} 個檔案", fmt::count(job.prog.files.load(Relaxed))) } else { String::new() }
                    ))
                    .color(DIM));
                    if !job.cache_only && ui.small_button("取消").clicked() {
                        job.prog.cancel.store(true, Relaxed);
                    }
                });
            } else if !missing.is_empty() {
                ui.horizontal(|ui| {
                    let names: Vec<String> = missing.iter().map(|l| format!("{l}:")).collect();
                    ui.label(RichText::new(format!("{} 還沒有分析資料，目前算在「未分析」裡。", names.join("、"))).color(ORANGE));
                    if ui.button("分析這些磁碟").on_hover_text(if app.elevated { "NTFS 磁碟會直接讀 MFT，很快" } else { "一般權限用目錄列舉，大磁碟要幾十秒；結果會快取" }).clicked() {
                        scan_missing = true;
                    }
                });
            }
        }

        let hidden = app.dash.hidden.clone();
        let visible_total: u64 = items.iter().filter(|i| !hidden.contains(&i.key)).map(|i| i.bytes).sum::<u64>().max(1);

        ui.horizontal_top(|ui| {
            // 左：圓餅
            let pie_w = (ui.available_width() * 0.56).max(320.0);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(pie_w, 380.0), Sense::click());
            let slices: Vec<Slice> = items
                .iter()
                .map(|i| {
                    let target = if hidden.contains(&i.key) { 0.0 } else { i.bytes as f32 / visible_total as f32 };
                    let frac = ctx.animate_value_with_time(egui::Id::new(("pie", &i.key)), target, 0.35);
                    Slice { frac, color: i.color, clear: i.clear }
                })
                .collect();
            let g = pie3d::geometry(rect, &slices);
            let hover = resp.hover_pos().and_then(|p| pie3d::hit(&g, p)).filter(|&i| slices[i].frac > 0.0005);
            // 圖例 hover 也會讓扇形浮起來
            let hover = hover.or_else(|| app.dash.hover.as_ref().and_then(|k| items.iter().position(|i| &i.key == k)));
            pie3d::draw(&ui.painter_at(rect.expand(20.0)), &g, &slices, hover, true);
            if let Some(h) = resp.hover_pos().and_then(|p| pie3d::hit(&g, p)) {
                let it = &items[h];
                let tip = match it.click {
                    Click::Kind(_) if app.dash.drive.is_some() => "點一下：到磁碟分析看這類檔案",
                    Click::Clean => "點一下：到清理頁",
                    _ => "",
                };
                resp.clone().on_hover_ui_at_pointer(|ui| {
                    ui.label(RichText::new(&it.label).strong());
                    ui.label(format!("{}（{:.1}%）", fmt::size(it.bytes), it.bytes as f64 / visible_total as f64 * 100.0));
                    if !tip.is_empty() {
                        ui.label(RichText::new(tip).small().color(DIM));
                    }
                });
                if resp.clicked() {
                    action = Some(it.click);
                }
                if !matches!(it.click, Click::None) || resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
            // 中心下方的總量
            let total_label = match mode {
                DashMode::Composition => "已顯示",
                DashMode::Cleanable => "已顯示",
            };
            ui.painter().text(
                egui::pos2(rect.center().x, rect.bottom() - 8.0),
                egui::Align2::CENTER_BOTTOM,
                format!("{total_label} {}", fmt::size(visible_total)),
                FontId::proportional(13.0),
                DIM,
            );

            // 右：圖例
            ui.vertical(|ui| {
                ui.add_space(8.0);
                app.dash.hover = None;
                for it in &items {
                    let off = app.dash.hidden.contains(&it.key);
                    let (r, hovered, _) = theme::list_row(
                        ui,
                        24.0,
                        |ui| {
                            let pct = if off { "—".to_string() } else { format!("{:.1}%", it.bytes as f64 / visible_total as f64 * 100.0) };
                            theme::right_cell(ui, 54.0, &pct, FontId::monospace(12.5), if off { DIM } else { TEXT });
                            theme::right_cell(ui, theme::SIZE_W, &fmt::size(it.bytes), FontId::monospace(13.0), if off { DIM } else { theme::STRONG });
                        },
                        |ui| {
                            let (sw, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                            let fill = if off { theme::ink(14) } else if it.clear { MIST } else { it.color };
                            ui.painter().rect(sw, 4, fill, Stroke::new(1.0, if off { theme::ink(30) } else { theme::ink(40) }), egui::StrokeKind::Inside);
                            let label = RichText::new(&it.label);
                            ui.label(if off { label.color(DIM).strikethrough() } else { label.color(TEXT) });
                        },
                    );
                    let r = ui.interact(r, egui::Id::new(("legend", &it.key)), Sense::click());
                    if hovered && !off {
                        app.dash.hover = Some(it.key.clone());
                    }
                    if r.clicked() {
                        if off {
                            app.dash.hidden.remove(&it.key);
                        } else {
                            app.dash.hidden.insert(it.key.clone());
                        }
                    }
                }
            });
        });
    });

    if scan_missing {
        app.start_dash_scan(ctx, missing, false);
    }
    match action {
        Some(Click::Kind(k)) => {
            if let Some(l) = app.dash.drive {
                let root = PathBuf::from(format!("{l}:\\"));
                let loaded = app.an.data.as_ref().is_some_and(|d| d.tree.name(0).eq_ignore_ascii_case(&format!("{l}:\\")));
                if loaded {
                    app.an.kind_focus = Some(k);
                    app.an.side = crate::app::Side::Types;
                    app.tab = Tab::Analyze;
                } else {
                    app.dash.pending_focus = Some(k);
                    app.start_scan(ctx, root, true);
                }
            }
        }
        Some(Click::Clean) => app.tab = Tab::Clean,
        _ => {}
    }
}

fn suggestions(app: &mut App, ui: &mut egui::Ui) {
    let list = suggest::list(app, app.dash.drive);
    let total: u64 = list.iter().map(|s| s.bytes).sum();
    let checked: u64 = list.iter().filter(|s| suggest::is_checked(app, &s.r)).map(|s| s.bytes).sum();
    let mut go_clean = false;
    let mut check_all = false;
    let mut toggles: Vec<(usize, bool)> = Vec::new();
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::list_row(
            ui,
            28.0,
            |ui| {
                theme::right_cell(ui, theme::SIZE_W, &fmt::size(total), FontId::monospace(15.0), theme::STRONG);
            },
            |ui| {
                ui.label(RichText::new("建議清理").size(17.0).strong());
                ui.label(RichText::new(format!("{} 項", list.len())).color(DIM));
                if ui.small_button("全部勾選").clicked() {
                    check_all = true;
                }
                if ui.add(egui::Button::new(RichText::new(format!("前往清理（已勾選 {}）", fmt::size(checked))).strong()).fill(theme::with_alpha(BLUE, 170))).clicked() {
                    go_clean = true;
                }
            },
        );
        ui.label(
            RichText::new("只列出「安全」的快取、7 天以上沒動的專案產物，以及重複的模型檔。hover 每一列可看為什麼可以清。")
                .small()
                .color(DIM),
        );
        ui.add_space(4.0);
        if list.is_empty() {
            let msg = if app.clean.measure.is_some() || app.clean.proj.find.is_some() {
                "清理頁還在計算中…"
            } else if app.clean.proj.items.is_empty() && app.clean.proj.models.is_empty() {
                "目前沒有建議項目。到清理頁按「搜尋專案產物與 AI 模型」可以找到更多。"
            } else {
                "目前沒有建議項目。"
            };
            ui.label(RichText::new(msg).color(DIM));
        }
        for (i, s) in list.iter().enumerate().take(20) {
            let mut on = suggest::is_checked(app, &s.r);
            let (_, _, changed) = theme::list_row(
                ui,
                26.0,
                |ui| {
                    theme::size_cell(ui, s.bytes, s.bytes >= 1 << 30);
                    let d = s.drive.map(|l| format!("{l}:")).unwrap_or_else(|| "多顆".into());
                    theme::right_cell(ui, 40.0, &d, FontId::proportional(12.0), DIM);
                },
                |ui| {
                    let c = theme::check(ui, &mut on).changed();
                    ui.add(egui::Label::new(RichText::new(&s.title).color(TEXT)).truncate()).on_hover_ui(|ui| {
                        ui.set_max_width(380.0);
                        ui.label(RichText::new("為什麼可以清").strong());
                        ui.label(&s.why);
                    });
                    c
                },
            );
            if changed {
                toggles.push((i, on));
            }
        }
        if list.len() > 20 {
            ui.label(RichText::new(format!("…還有 {} 項，請到清理頁查看", list.len() - 20)).small().color(DIM));
        }
    });
    for (i, v) in toggles {
        suggest::set_checked(app, &list[i].r, v);
    }
    if check_all {
        for s in &list {
            suggest::set_checked(app, &s.r, true);
        }
    }
    if go_clean {
        app.tab = Tab::Clean;
    }
}
