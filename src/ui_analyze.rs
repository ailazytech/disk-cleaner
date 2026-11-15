//! 磁碟分析頁：treemap + 資料夾清單 + 類型／副檔名／最大檔案。

use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;

use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind};

use crate::app::{AnalyzeState, App, DelRequest, LayoutCache, Loaded, Side};
use crate::tree::{Kind, NONE, Stats, Tree, flag};
use crate::treemap::{self, HEADER_H};
use crate::theme;
use crate::ui_clean::{ACCENT, DIM, GREEN, ORANGE, RED, drives_strip};

/// 磁碟分析頁統一的留白
const PAD: i8 = 16;
use crate::{fmt, win};

enum Act {
    Enter(u32),
    Select(u32),
    Up,
    GoTo(u32),
    Reveal(u32),
    Copy(u32),
    Delete(u32, bool),
    Focus(Option<Kind>),
}

fn kind_color(k: Kind) -> Color32 {
    let [r, g, b] = k.color();
    Color32::from_rgb(r, g, b)
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();

    // ---------- 上方：路徑與磁碟 ----------
    egui::Panel::top("an_top")
        .frame(theme::bar().inner_margin(egui::Margin::symmetric(PAD, 10)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let scanning = app.an.job.is_some();
                let te = ui.add(
                    egui::TextEdit::singleline(&mut app.an.path_input)
                        .hint_text("輸入資料夾路徑，或把資料夾拖進視窗")
                        .desired_width(360.0),
                );
                let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(!scanning, egui::Button::new("📊 分析")).clicked() || enter)
                    && !app.an.path_input.trim().is_empty()
                {
                    let p = PathBuf::from(app.an.path_input.trim());
                    app.start_scan(&ctx, p, true);
                }
                if ui.add_enabled(!scanning, egui::Button::new("📂 選擇資料夾…")).clicked()
                    && let Some(p) = rfd::FileDialog::new().set_title("選擇要分析的資料夾").pick_folder()
                {
                    app.start_scan(&ctx, p, true);
                }
                ui.separator();
                let drives: Vec<_> = app.drives.clone();
                for d in &drives {
                    let frac = fmt::pct(d.used(), d.total);
                    let col = if frac > 0.9 { RED } else if frac > 0.75 { ORANGE } else { DIM };
                    let txt = RichText::new(format!("{}:  剩 {}", d.letter(), fmt::size(d.free))).color(col);
                    if ui
                        .add_enabled(!scanning, egui::Button::new(txt))
                        .on_hover_text(format!(
                            "{} {}\n已用 {} ／ 共 {}（{:.0}%）",
                            d.root,
                            d.label,
                            fmt::size(d.used()),
                            fmt::size(d.total),
                            frac * 100.0
                        ))
                        .clicked()
                    {
                        app.start_scan(&ctx, PathBuf::from(&d.root), true);
                    }
                }
            });
        });

    // ---------- 掃描中 ----------
    if let Some(job) = &app.an.job {
        let mut cancel = false;
        egui::CentralPanel::default().frame(egui::Frame::NONE.inner_margin(egui::Margin::same(PAD))).show(ui, |ui| {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.add_space(8.0);
                ui.heading(format!("正在分析 {}", job.target.display()));
                ui.label(RichText::new(job.prog.stage.lock().clone()).color(DIM));
                ui.add_space(10.0);
                let total = job.prog.total.load(Relaxed);
                if total > 0 {
                    let done = job.prog.done.load(Relaxed);
                    ui.add(
                        egui::ProgressBar::new(done as f32 / total as f32)
                            .desired_width(420.0)
                            .text(format!("MFT 紀錄 {}／{}", fmt::count(done), fmt::count(total))),
                    );
                }
                let files = job.prog.files.load(Relaxed);
                let dirs = job.prog.dirs.load(Relaxed);
                ui.label(format!(
                    "{} 個檔案{}　{}　已經過 {:.1} 秒",
                    fmt::count(files),
                    if dirs > 0 { format!("、{} 個資料夾", fmt::count(dirs)) } else { String::new() },
                    if job.prog.bytes.load(Relaxed) > 0 { fmt::size(job.prog.bytes.load(Relaxed)) } else { String::new() },
                    job.started.elapsed().as_secs_f32()
                ));
                ui.label(RichText::new(job.prog.current.lock().clone()).small().color(DIM));
                ui.add_space(10.0);
                if !app.elevated {
                    ui.label(
                        RichText::new("提示：以系統管理員身分執行可直接讀取 NTFS 的 MFT，整顆磁碟只要幾秒，之後還能用 USN 日誌增量更新。")
                            .small()
                            .color(ORANGE),
                    );
                }
                ui.add_space(10.0);
                if ui.button("⏹ 取消").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel {
            job.prog.cancel.store(true, Relaxed);
        }
        return;
    }

    // ---------- 尚未掃描 ----------
    let Some(data) = app.an.data.take() else {
        egui::CentralPanel::default().frame(egui::Frame::NONE.inner_margin(egui::Margin::same(PAD))).show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(e) = &app.an.error {
                ui.label(RichText::new(format!("⚠ {e}")).color(RED));
                ui.add_space(8.0);
            }
            ui.heading("選擇要分析的磁碟");
            ui.label(
                RichText::new(if app.elevated {
                    "已是系統管理員：NTFS 磁碟會直接讀取 MFT（數秒），之後用 USN 日誌增量更新（通常 1 秒內）。"
                } else {
                    "目前以一般權限執行：使用平行目錄列舉，掃描結果會快取，下次開啟立即顯示。"
                })
                .color(DIM),
            );
            ui.add_space(10.0);
            if let Some(p) = drives_strip(app, ui, true) {
                app.start_scan(&ctx, p, true);
            }
            ui.add_space(16.0);
            let cache_size = crate::cache::total_size();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("索引快取：{}（{}）", fmt::size(cache_size), crate::cache::cache_dir().display()))
                        .small()
                        .color(DIM),
                );
                if cache_size > 0 && ui.small_button("清除快取").clicked() {
                    crate::cache::clear_all();
                }
            });
        });
        return;
    };

    let mut acts: Vec<Act> = Vec::new();
    let mut rescan = false;
    let st = &mut app.an;
    ensure_caches(st, &data);

    // ---------- 狀態列 ----------
    egui::Panel::top("an_status")
        .frame(theme::bar().inner_margin(egui::Margin::symmetric(PAD, 8)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let t = &data.tree;
                let base = &t.nodes[data.base as usize];
                ui.label(RichText::new(data.target.to_string_lossy()).strong());
                ui.label(RichText::new(fmt::size(base.size)).strong().color(crate::theme::STRONG));
                ui.label(
                    RichText::new(format!(
                        "{} 個檔案 · 索引共 {} 個項目 · {} · {:.2} 秒",
                        fmt::count(base.files as u64),
                        fmt::count(t.nodes.len() as u64),
                        data.origin.describe(),
                        data.secs
                    ))
                    .color(DIM),
                );
                if t.errors > 0 {
                    ui.label(RichText::new(format!("⚠ {} 個資料夾無法讀取", t.errors)).color(ORANGE));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if data.origin.is_stale() { "🔄 重新掃描（快取是舊的）" } else { "🔄 重新掃描" };
                    if ui.button(label).clicked() {
                        rescan = true;
                    }
                });
            });
            if let Some(m) = st.message.clone() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(m).color(GREEN));
                    if ui.small_button("✕").clicked() {
                        st.message = None;
                    }
                });
            }
        });

    // ---------- 底部：選取項目 ----------
    egui::Panel::bottom("an_bottom")
        .frame(theme::bar().inner_margin(egui::Margin::symmetric(PAD, 10)))
        .show(ui, |ui| {
            let t = &data.tree;
            let target = st.selected.or(st.hovered);
            ui.horizontal(|ui| {
                if let Some(n) = target {
                    let node = &t.nodes[n as usize];
                    // 先從右邊放按鈕，剩下的空間給路徑（過長時截斷）
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if st.selected.is_some() {
                            let busy = st.del_job.is_some();
                            if ui.add_enabled(!busy, egui::Button::new(RichText::new("永久刪除").color(RED))).clicked() {
                                acts.push(Act::Delete(n, true));
                            }
                            if ui.add_enabled(!busy, egui::Button::new("🗑 移到資源回收筒")).clicked() {
                                acts.push(Act::Delete(n, false));
                            }
                            if ui.button("📂 在檔案總管中顯示").clicked() {
                                acts.push(Act::Reveal(n));
                            }
                            if node.is_dir() && ui.button("🔍 進入").clicked() {
                                acts.push(Act::Enter(n));
                            }
                            ui.add_space(8.0);
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let icon = if node.is_dir() { "📁" } else { "📄" };
                            ui.label(RichText::new(fmt::size(node.size)).strong().color(crate::theme::STRONG));
                            if node.is_dir() {
                                ui.label(RichText::new(format!("{} 個檔案", fmt::count(node.files as u64))).color(DIM));
                            } else {
                                ui.label(RichText::new(node.kind().label()).color(kind_color(node.kind())));
                            }
                            let path = t.path(n).display().to_string();
                            ui.add(egui::Label::new(format!("{icon} {path}")).truncate()).on_hover_text(path);
                        });
                    });
                } else {
                    ui.label(RichText::new("點一下方塊選取，雙擊進入資料夾，右鍵開啟選單；Backspace／滑鼠側鍵回上一層。").color(DIM));
                }
            });
        });

    // ---------- 左側清單 ----------
    egui::Panel::left("an_left")
        .resizable(true)
        .default_size(470.0)
        .min_size(320.0)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: PAD, right: PAD / 2, top: PAD, bottom: PAD }))
        .show(ui, |ui| {
            theme::glass().inner_margin(egui::Margin::same(PAD)).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            breadcrumb(ui, &data.tree, st.view, &mut acts);
            ui.horizontal(|ui| {
                for (s, label) in [(Side::Folders, "資料夾"), (Side::Types, "類型"), (Side::Exts, "副檔名"), (Side::Largest, "最大檔案")] {
                    if ui.selectable_label(st.side == s, label).clicked() {
                        st.side = s;
                    }
                }
            });
            ui.separator();
            match st.side {
                Side::Folders => folder_list(ui, &data.tree, st, &mut acts),
                Side::Types => types_list(ui, &data.tree, st, &mut acts),
                Side::Exts => ext_list(ui, &data.tree, st),
                Side::Largest => largest_list(ui, &data.tree, st, &mut acts),
            }
            });
        });

    // ---------- 中央 treemap ----------
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: PAD / 2, right: PAD, top: PAD, bottom: PAD }))
        .show(ui, |ui| {
            theme::glass().inner_margin(egui::Margin::same(PAD / 2)).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                draw_treemap(ui, &data, st, &mut acts);
            });
        });

    // 鍵盤／滑鼠側鍵
    let typing = ctx.egui_wants_keyboard_input();
    let back = ctx.input(|i| {
        i.pointer.button_clicked(egui::PointerButton::Extra1) || (i.key_pressed(egui::Key::Backspace) && !typing)
    });
    if back {
        acts.push(Act::Up);
    }

    // 套用動作
    for a in acts {
        apply(st, &data, &ctx, a);
    }
    let del_req = st.del.is_some();
    app.an.data = Some(data);
    if del_req {
        delete_modal(app, &ctx);
    }
    if rescan {
        let target = app.an.data.as_ref().unwrap().target.clone();
        app.start_scan(&ctx, target, false);
    }
}

fn ensure_caches(st: &mut AnalyzeState, data: &Loaded) {
    let t = &data.tree;
    if st.view as usize >= t.nodes.len() {
        st.view = 0;
    }
    if !st.kids.as_ref().is_some_and(|k| k.0 == st.view && k.1 == data.rev) {
        let mut v: Vec<u32> = t.nodes[st.view as usize]
            .children()
            .filter(|&c| t.nodes[c as usize].flags & flag::DELETED == 0)
            .collect();
        v.sort_by(|&a, &b| t.nodes[b as usize].size.cmp(&t.nodes[a as usize].size));
        st.kids = Some((st.view, data.rev, v));
    }
    if st.view != 0 && !st.stats.as_ref().is_some_and(|s| s.0 == st.view && s.1 == data.rev) {
        st.stats = Some((st.view, data.rev, t.stats_of(st.view)));
    }
}

fn view_stats<'a>(t: &'a Tree, st: &'a AnalyzeState) -> &'a Stats {
    if st.view == 0 {
        &t.stats
    } else {
        st.stats.as_ref().map(|s| &s.2).unwrap_or(&t.stats)
    }
}

fn apply(st: &mut AnalyzeState, data: &Loaded, ctx: &egui::Context, a: Act) {
    let t = &data.tree;
    match a {
        Act::Enter(n) => {
            let n = if t.nodes[n as usize].is_dir() { n } else { t.nodes[n as usize].parent };
            if n != NONE && n != st.view {
                st.history.push(st.view);
                st.view = n;
                st.selected = None;
            }
        }
        Act::GoTo(n) => {
            if n != st.view {
                st.history.push(st.view);
                st.view = n;
                st.selected = None;
            }
        }
        Act::Up => {
            let p = t.nodes[st.view as usize].parent;
            if p != NONE {
                st.selected = Some(st.view);
                st.view = p;
            }
        }
        Act::Select(n) => st.selected = Some(n),
        Act::Reveal(n) => win::reveal_in_explorer(&t.path(n)),
        Act::Copy(n) => ctx.copy_text(t.path(n).to_string_lossy().into_owned()),
        Act::Delete(n, permanent) => {
            if st.del_job.is_some() {
                st.message = Some("上一個刪除還在進行中，請稍候。".into());
            } else if data.origin.is_stale() {
                // 快取的舊資料：檔案可能已經換過，不能直接拿來刪
                st.message = Some("目前顯示的是快取的舊資料，請先按「重新掃描」再刪除。".into());
            } else if n != 0 {
                st.del = Some(DelRequest { node: n, path: t.path(n), size: t.nodes[n as usize].size, scanned_at: data.scanned_at, permanent });
            }
        }
        Act::Focus(k) => st.kind_focus = k,
    }
}

fn node_menu(ui: &mut egui::Ui, t: &Tree, n: u32, acts: &mut Vec<Act>) {
    let node = &t.nodes[n as usize];
    ui.label(RichText::new(t.name(n)).strong());
    ui.label(RichText::new(fmt::size(node.size)).color(DIM));
    ui.separator();
    if node.is_dir() && ui.button("🔍 進入資料夾").clicked() {
        acts.push(Act::Enter(n));
        ui.close();
    }
    if ui.button("📂 在檔案總管中顯示").clicked() {
        acts.push(Act::Reveal(n));
        ui.close();
    }
    if ui.button("📋 複製路徑").clicked() {
        acts.push(Act::Copy(n));
        ui.close();
    }
    ui.separator();
    if ui.button("🗑 移到資源回收筒…").clicked() {
        acts.push(Act::Delete(n, false));
        ui.close();
    }
    if ui.button(RichText::new("永久刪除…").color(RED)).clicked() {
        acts.push(Act::Delete(n, true));
        ui.close();
    }
}

fn breadcrumb(ui: &mut egui::Ui, t: &Tree, view: u32, acts: &mut Vec<Act>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if ui.add_enabled(t.nodes[view as usize].parent != NONE, egui::Button::new("⬆").small()).on_hover_text("上一層").clicked() {
            acts.push(Act::Up);
        }
        let chain = t.ancestry(view);
        for (i, &c) in chain.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("›").color(DIM));
            }
            let name = t.name(c);
            let txt = if c == view { RichText::new(name).strong() } else { RichText::new(name) };
            if ui.add(egui::Button::new(txt).small().frame(false)).clicked() {
                acts.push(Act::GoTo(c));
            }
        }
    });
    let v = &t.nodes[view as usize];
    ui.label(RichText::new(format!("{} · {} 個檔案", fmt::size(v.size), fmt::count(v.files as u64))).color(DIM));
}

fn bar_row(ui: &mut egui::Ui, frac: f32, color: Color32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 10.0), Sense::hover());
    ui.painter().rect_filled(rect, 2, Color32::from_rgb(226, 230, 242));
    let mut f = rect;
    f.set_width((rect.width() * frac).max(1.0));
    ui.painter().rect_filled(f, 2, color);
}

fn folder_list(ui: &mut egui::Ui, t: &Tree, st: &mut AnalyzeState, acts: &mut Vec<Act>) {
    let kids = st.kids.as_ref().map(|k| k.2.clone()).unwrap_or_default();
    let parent_size = t.nodes[st.view as usize].size.max(1);
    let row_h = 22.0;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, kids.len(), |ui, range| {
        for i in range {
            let c = kids[i];
            let n = &t.nodes[c as usize];
            let sel = st.selected == Some(c);
            let resp = ui
                .horizontal(|ui| {
                    ui.set_height(row_h);
                    let frac = n.size as f32 / parent_size as f32;
                    let col = if n.is_dir() { ACCENT } else { kind_color(n.kind()) };
                    bar_row(ui, frac, col, 50.0);
                    ui.label(RichText::new(format!("{:>5.1}%", frac * 100.0)).small().monospace().color(DIM));
                    ui.label(RichText::new(format!("{:>9}", fmt::size(n.size))).monospace());
                    let icon = if n.flags & flag::REPARSE != 0 {
                        "🔗"
                    } else if n.is_dir() {
                        "📁"
                    } else {
                        "📄"
                    };
                    let name = RichText::new(format!("{icon} {}", t.name(c)));
                    let name = if sel { name.strong().color(crate::theme::STRONG) } else { name };
                    ui.add(egui::Label::new(name).truncate());
                })
                .response
                .interact(Sense::click());
            if sel {
                ui.painter().rect_stroke(resp.rect, 2, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
            } else if resp.hovered() {
                ui.painter().rect_filled(resp.rect, 2, crate::theme::ROW_HOVER);
            }
            if resp.hovered() {
                st.hovered = Some(c);
            }
            if resp.double_clicked() && n.is_dir() {
                acts.push(Act::Enter(c));
            } else if resp.clicked() {
                acts.push(Act::Select(c));
            }
            resp.context_menu(|ui| node_menu(ui, t, c, acts));
        }
    });
}

fn types_list(ui: &mut egui::Ui, t: &Tree, st: &AnalyzeState, acts: &mut Vec<Act>) {
    let s = view_stats(t, st);
    let total: u64 = s.kinds.iter().map(|k| k.0).sum::<u64>().max(1);
    let mut ks: Vec<(Kind, u64, u64)> = Kind::ALL.iter().map(|&k| (k, s.kinds[k as usize].0, s.kinds[k as usize].1)).filter(|x| x.1 > 0).collect();
    ks.sort_by(|a, b| b.1.cmp(&a.1));
    ui.label(RichText::new(format!("共 {} 個檔案。點一下類型可在右邊圖上突顯", fmt::count(s.files))).small().color(DIM));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (k, size, count) in ks {
            let on = st.kind_focus == Some(k);
            let resp = ui
                .horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                    ui.painter().rect_filled(r, 2, kind_color(k));
                    let label = RichText::new(k.label());
                    ui.label(if on { label.strong().color(crate::theme::STRONG) } else { label });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("{} 個", fmt::count(count))).small().color(DIM));
                        ui.label(RichText::new(format!("{:>9}", fmt::size(size))).monospace());
                        bar_row(ui, size as f32 / total as f32, kind_color(k), 90.0);
                    });
                })
                .response
                .interact(Sense::click());
            if on {
                ui.painter().rect_stroke(resp.rect, 2, Stroke::new(1.0, kind_color(k)), StrokeKind::Inside);
            }
            if resp.clicked() {
                acts.push(Act::Focus(if on { None } else { Some(k) }));
            }
        }
    });
}

fn ext_list(ui: &mut egui::Ui, t: &Tree, st: &AnalyzeState) {
    let s = view_stats(t, st);
    let total: u64 = s.kinds.iter().map(|k| k.0).sum::<u64>().max(1);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 20.0, s.exts.len().min(300), |ui, range| {
        for e in &s.exts[range] {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, 2, kind_color(e.kind));
                ui.add_sized([90.0, 18.0], egui::Label::new(RichText::new(&e.ext).monospace()).truncate());
                ui.label(RichText::new(format!("{:>9}", fmt::size(e.size))).monospace());
                bar_row(ui, e.size as f32 / total as f32, kind_color(e.kind), 70.0);
                ui.label(RichText::new(format!("{} 個", fmt::count(e.count))).small().color(DIM));
            });
        }
    });
}

fn largest_list(ui: &mut egui::Ui, t: &Tree, st: &mut AnalyzeState, acts: &mut Vec<Act>) {
    let list: Vec<u32> = view_stats(t, st).largest.clone();
    let base = t.path(st.view);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 36.0, list.len(), |ui, range| {
        for &n in &list[range] {
            let node = &t.nodes[n as usize];
            if node.flags & flag::DELETED != 0 {
                continue;
            }
            let sel = st.selected == Some(n);
            let resp = ui
                .vertical(|ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                        ui.painter().rect_filled(r, 2, kind_color(node.kind()));
                        ui.label(RichText::new(format!("{:>9}", fmt::size(node.size))).monospace());
                        let name = RichText::new(t.name(n));
                        ui.add(egui::Label::new(if sel { name.strong().color(crate::theme::STRONG) } else { name }).truncate());
                    });
                    let p = t.path(node.parent);
                    let rel = p.strip_prefix(&base).map(|r| r.to_path_buf()).unwrap_or(p);
                    ui.add(egui::Label::new(RichText::new(format!("    {}", rel.display())).small().color(DIM)).truncate());
                })
                .response
                .interact(Sense::click());
            if sel {
                ui.painter().rect_stroke(resp.rect, 2, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
            }
            if resp.hovered() {
                st.hovered = Some(n);
            }
            if resp.double_clicked() {
                acts.push(Act::Enter(node.parent));
                acts.push(Act::Select(n));
            } else if resp.clicked() {
                acts.push(Act::Select(n));
            }
            resp.context_menu(|ui| node_menu(ui, t, n, acts));
        }
    });
}

fn draw_treemap(ui: &mut egui::Ui, data: &Loaded, st: &mut AnalyzeState, acts: &mut Vec<Act>) {
    let t = &data.tree;
    let rect = ui.available_rect_before_wrap().shrink(2.0);
    let resp = ui.allocate_rect(rect, Sense::click());
    let valid = st.layout.as_ref().is_some_and(|l| l.view == st.view && l.rev == data.rev && l.rect == rect);
    if !valid {
        let tiles = treemap::layout(t, st.view, rect, 40_000);
        st.layout = Some(LayoutCache { view: st.view, rect, rev: data.rev, tiles });
    }
    let tiles = &st.layout.as_ref().unwrap().tiles;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 14, Color32::from_rgb(246, 248, 253));

    let focus = st.kind_focus;
    let font = FontId::proportional(11.5);
    let mut labels = 0;
    for tile in tiles {
        let n = &t.nodes[tile.node as usize];
        let r = tile.rect;
        if n.is_dir() {
            if tile.depth == 0 {
                continue;
            }
            // 資料夾：越深越淡的霧面底
            let d = tile.depth.min(8) as u8;
            let rad = if r.width() > 30.0 && r.height() > 30.0 { 6 } else { 2 };
            painter.rect_filled(r, rad, Color32::from_rgb(228 - d * 3, 232 - d * 3, 244 - d * 2));
            painter.rect_stroke(r, rad, Stroke::new(1.0, Color32::from_rgb(208, 214, 232)), StrokeKind::Inside);
            if tile.header && labels < 1500 {
                labels += 1;
                let hr = Rect::from_min_max(r.min, Pos2::new(r.max.x, r.min.y + HEADER_H));
                painter.with_clip_rect(hr.shrink(1.0)).text(
                    hr.left_center() + egui::vec2(5.0, 0.0),
                    Align2::LEFT_CENTER,
                    format!("{}  {}", t.name(tile.node), fmt::size(n.size)),
                    font.clone(),
                    crate::theme::TEXT,
                );
            }
        } else {
            let mut c = kind_color(n.kind());
            if let Some(k) = focus
                && k != n.kind()
            {
                c = crate::theme::tint(c, 0.82);
            }
            // 霧面圓潤色塊：圓角＋左上柔光＋右下陰影
            let big = r.width() > 8.0 && r.height() > 8.0;
            let rad = if r.width() > 24.0 && r.height() > 24.0 { 5 } else if big { 2 } else { 0 };
            let inner = if big { r.shrink(0.8) } else { r };
            painter.rect_filled(inner, rad, c);
            if big {
                let hl = Rect::from_min_max(inner.min, Pos2::new(inner.max.x, inner.min.y + (inner.height() * 0.45).min(14.0)));
                painter.rect_filled(hl.shrink(1.0), rad, Color32::from_white_alpha(46));
                painter.line_segment(
                    [Pos2::new(inner.left() + 2.0, inner.bottom() - 0.5), Pos2::new(inner.right() - 1.0, inner.bottom() - 0.5)],
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(20, 30, 70, 46)),
                );
            }
            if r.width() > 54.0 && r.height() > 18.0 && labels < 1500 {
                labels += 1;
                let cp = painter.with_clip_rect(r.shrink(2.0));
                cp.text(r.left_top() + egui::vec2(6.0, 4.0), Align2::LEFT_TOP, t.name(tile.node), font.clone(), Color32::WHITE);
                if r.height() > 34.0 {
                    cp.text(r.left_top() + egui::vec2(6.0, 18.0), Align2::LEFT_TOP, fmt::size(n.size), font.clone(), Color32::from_white_alpha(210));
                }
            }
        }
    }

    // 選取外框
    if let Some(sel) = st.selected
        && let Some(tile) = tiles.iter().find(|x| x.node == sel)
    {
        painter.rect_stroke(tile.rect, 6, Stroke::new(3.0, crate::theme::STRONG), StrokeKind::Inside);
    }

    // 滑鼠
    let hover = resp.hover_pos().and_then(|p| treemap::hit(tiles, p));
    if let Some(h) = hover {
        st.hovered = Some(h);
        if let Some(tile) = tiles.iter().rev().find(|x| x.node == h) {
            painter.rect_stroke(tile.rect, 6, Stroke::new(2.0, crate::theme::BLUE), StrokeKind::Inside);
        }
    } else if resp.hovered() {
        st.hovered = None;
    }
    let resp = if let Some(h) = hover {
        let n = &t.nodes[h as usize];
        let view_size = t.nodes[st.view as usize].size.max(1);
        resp.on_hover_ui_at_pointer(|ui| {
            ui.label(RichText::new(t.name(h)).strong());
            ui.label(format!("{}（{:.1}%）", fmt::size(n.size), n.size as f64 / view_size as f64 * 100.0));
            if n.is_dir() {
                ui.label(format!("{} 個檔案", fmt::count(n.files as u64)));
            } else {
                ui.label(RichText::new(n.kind().label()).color(kind_color(n.kind())));
            }
            ui.label(RichText::new(t.path(h).to_string_lossy()).small().color(DIM));
        })
    } else {
        resp
    };
    if resp.secondary_clicked()
        && let Some(h) = hover
    {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("tm_menu"), h));
        acts.push(Act::Select(h));
    }
    if resp.double_clicked() {
        if let Some(h) = hover {
            acts.push(Act::Enter(h));
        }
    } else if resp.clicked()
        && let Some(h) = hover
    {
        acts.push(Act::Select(h));
    }
    resp.context_menu(|ui| {
        let n: Option<u32> = ui.ctx().data(|d| d.get_temp(egui::Id::new("tm_menu")));
        if let Some(n) = n {
            node_menu(ui, t, n, acts);
        }
    });
}

fn delete_modal(app: &mut App, ctx: &egui::Context) {
    let Some(req) = &app.an.del else { return };
    let mut go = false;
    let mut close = false;
    let protected = crate::cleaner::is_protected(&req.path);
    let resp = egui::Modal::new(egui::Id::new("an_delete")).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.heading(if req.permanent { "永久刪除" } else { "移到資源回收筒" });
        ui.add_space(6.0);
        ui.label(RichText::new(req.path.to_string_lossy()).strong());
        ui.label(format!("大小：{}", fmt::size(req.size)));
        ui.add_space(6.0);
        if protected {
            ui.label(RichText::new("⛔ 這是系統或使用者的重要資料夾，不允許刪除。").color(RED));
        } else if req.permanent {
            ui.label(RichText::new("刪除後無法復原。").color(RED));
        } else {
            ui.label(RichText::new("移到資源回收筒可以復原，但要清空回收筒才會真正釋放空間。").color(ORANGE));
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let label = if req.permanent { "  永久刪除  " } else { "  移到資源回收筒  " };
            if ui.add_enabled(!protected, egui::Button::new(RichText::new(label).strong()).fill(Color32::from_rgb(190, 60, 50))).clicked() {
                go = true;
            }
            if ui.button("取消").clicked() {
                close = true;
            }
        });
    });
    if resp.should_close() {
        close = true;
    }
    if go {
        let req = app.an.del.take().unwrap();
        app.start_delete(ctx, req);
    } else if close {
        app.an.del = None;
    }
}
