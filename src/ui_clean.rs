//! 清理頁：置中的單欄版面、右側固定的大小欄、整列 hover 底色、「建議」標籤。

use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText};

use crate::app::App;
use crate::cleaner::Mode;
use crate::rules::{Action, Cat, Risk};
pub use crate::theme::{ACCENT, DIM, GREEN, ORANGE, RED};
use crate::theme::{self, AGE_W, SIZE_W, TEXT};
use crate::{fmt, suggest, win};

fn risk_badge(ui: &mut egui::Ui, risk: Risk) {
    let (txt, col) = match risk {
        Risk::Safe => ("安全", GREEN),
        Risk::Caution => ("注意", ORANGE),
    };
    ui.label(RichText::new(txt).small().color(col));
}

/// 磁碟卡片（分析頁首頁用）
pub fn drives_strip(app: &mut App, ui: &mut egui::Ui, clickable: bool) -> Option<PathBuf> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        for d in &app.drives {
            let frac = fmt::pct(d.used(), d.total);
            let resp = theme::glass()
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}:", d.letter())).strong().size(16.0));
                            let label = if d.label.is_empty() { d.kind.to_string() } else { d.label.clone() };
                            ui.label(RichText::new(label).color(DIM));
                            if !d.fs.is_empty() {
                                ui.label(RichText::new(&d.fs).small().color(DIM));
                            }
                        });
                        theme::usage_bar(ui, frac, 190.0, 8.0);
                        ui.label(
                            RichText::new(format!("剩 {} ／ 共 {}", fmt::size(d.free), fmt::size(d.total)))
                                .small()
                                .color(if frac > 0.9 { RED } else { DIM }),
                        );
                    });
                })
                .response;
            if clickable {
                let resp = resp.interact(egui::Sense::click()).on_hover_text(format!("分析 {}", d.root));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    clicked = Some(PathBuf::from(&d.root));
                }
            }
        }
    });
    clicked
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let (sel_bytes, sel_n) = app.selected_bytes();
    let job_running = app.clean.job.as_ref().is_some_and(|j| j.finished.is_none());

    // ---------- 底部操作列 ----------
    egui::Panel::bottom("clean_bottom").frame(theme::bar()).show(ui, |ui| {
        theme::centered(ui, theme::CONTENT_W, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("已勾選 {sel_n} 項")).color(DIM));
                ui.label(RichText::new(fmt::size(sel_bytes)).strong().size(18.0).color(theme::STRONG));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let btn = egui::Button::new(RichText::new("  🧹 清除已勾選  ").size(16.0).strong()).fill(theme::with_alpha(theme::BLUE, 200));
                    if ui.add_enabled(sel_n > 0 && !job_running, btn).clicked() {
                        app.clean.confirm = true;
                    }
                    let measuring = app.clean.measure.is_some();
                    if ui.add_enabled(!measuring, egui::Button::new("🔄 重新掃描")).clicked() {
                        app.refresh_drives();
                        app.start_measure(&ctx, None);
                    }
                    let mut show_empty = app.settings.show_empty;
                    // 由右往左排：先放文字，再放勾選框（看起來是「☐ 顯示 0 B 的項目」）
                    let label_clicked = ui.add(egui::Label::new("顯示 0 B 的項目").sense(egui::Sense::click())).clicked();
                    if label_clicked {
                        show_empty = !show_empty;
                    }
                    if theme::check(ui, &mut show_empty).changed() | label_clicked {
                        app.settings.show_empty = show_empty;
                        app.settings.save();
                    }
                });
            });
        });
    });

    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            theme::centered(ui, theme::CONTENT_W, |ui| {
                ui.add_space(4.0);
                summary(app, ui);
                ui.add_space(10.0);
                categories(app, ui);
                ui.add_space(6.0);
                projects(app, ui);
                ui.add_space(6.0);
                models_section(app, ui);
                ui.add_space(20.0);
            });
        });
    });

    confirm_modal(app, &ctx);
    progress_modal(app, &ctx);
}

fn summary(app: &mut App, ui: &mut egui::Ui) {
    let total: u64 = app.clean.rows.iter().filter(|r| !r.is_command()).map(|r| r.bytes()).sum::<u64>()
        + app.clean.proj.items.iter().map(|a| a.art.bytes).sum::<u64>()
        + app.clean.proj.models.iter().map(|m| m.m.bytes).sum::<u64>();
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("可清理空間").color(DIM));
                ui.label(RichText::new(fmt::size(total)).size(32.0).strong().color(theme::STRONG));
            });
            ui.add_space(24.0);
            ui.vertical(|ui| {
                if let Some(j) = &app.clean.measure {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let done = j.total - j.remaining.load(Relaxed);
                        match app.clean.cached_age {
                            Some(age) => ui.label(format!(
                                "先顯示上次結果（{} 前），背景更新中… {done}／{}",
                                crate::index::fmt_age(age),
                                j.total
                            )),
                            None => ui.label(format!("正在計算… {done}／{}", j.total)),
                        };
                    });
                } else if let Some(s) = app.clean.measure_secs {
                    ui.label(RichText::new(format!("✔ 已檢查 {} 個項目（{s:.1} 秒）", app.clean.rows.len())).color(GREEN));
                }
                let locked = app.clean.rows.iter().filter(|r| r.rule.admin && r.bytes() > 0).count();
                if !app.elevated && locked > 0 {
                    ui.label(
                        RichText::new(format!("🔒 有 {locked} 個系統項目需要系統管理員權限才能清除（右上角可重新啟動）"))
                            .color(ORANGE),
                    );
                }
                ui.label(
                    RichText::new("沒勾選但標著「建議」的項目，滑鼠移到「建議」上可以看為什麼可以清。「注意」項目刪了要重新下載或可能影響功能。")
                        .small()
                        .color(DIM),
                );
            });
        });
    });
}

/// 類別標題：左邊名稱、右邊總大小（和下面每列的大小對齊）。點名稱也能展開收合。
/// 每個區塊放在一張霧面白卡片裡。
fn section_header(ui: &mut egui::Ui, id_salt: &str, title: &str, total: u64, note: Option<String>, add_body: impl FnOnce(&mut egui::Ui)) {
    theme::glass().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_inner(ui, id_salt, title, total, note, add_body);
    });
    ui.add_space(10.0);
}

fn section_inner(ui: &mut egui::Ui, id_salt: &str, title: &str, total: u64, note: Option<String>, add_body: impl FnOnce(&mut egui::Ui)) {
    let id = ui.make_persistent_id(("section", id_salt));
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
    let (_, header, _) = state
        .show_header(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::right_cell(ui, SIZE_W, &fmt::size(total), FontId::proportional(16.0), theme::STRONG);
                if let Some(n) = &note {
                    ui.label(RichText::new(n).small().color(DIM));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(title).size(16.0).strong().color(TEXT)).sense(egui::Sense::click()))
                        .clicked()
                })
                .inner
            })
            .inner
        })
        .body(add_body);
    if header.inner {
        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
        st.toggle(ui);
        st.store(ui.ctx());
    }
}

fn categories(app: &mut App, ui: &mut egui::Ui) {
    let measuring = app.clean.measure.is_some();
    let elevated = app.elevated;
    let show_empty = app.settings.show_empty;

    let mut cats: Vec<(Cat, u64)> = Cat::ALL
        .iter()
        .map(|&c| (c, app.clean.rows.iter().filter(|r| r.rule.cat == c && !r.is_command()).map(|r| r.bytes()).sum()))
        .collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1));

    for (cat, total) in cats {
        let mut idx: Vec<usize> = (0..app.clean.rows.len())
            .filter(|&i| {
                let r = &app.clean.rows[i];
                r.rule.cat == cat && (show_empty || r.bytes() > 0 || r.is_command() || r.measured.is_none())
            })
            .collect();
        if idx.is_empty() {
            continue;
        }
        idx.sort_by(|&a, &b| app.clean.rows[b].bytes().cmp(&app.clean.rows[a].bytes()));
        let checked: u64 = idx.iter().filter(|&&i| app.clean.rows[i].checked).map(|&i| app.clean.rows[i].bytes()).sum();
        let note = (checked > 0).then(|| format!("已勾選 {}", fmt::size(checked)));
        let title = format!("{}  {}", cat.icon(), cat.label());
        section_header(ui, cat.label(), &title, total, note, |ui| {
            ui.horizontal(|ui| {
                if ui.small_button("勾選建議項目").clicked() {
                    for &i in &idx {
                        let r = &mut app.clean.rows[i];
                        r.checked = suggest::rule(r, elevated);
                    }
                }
                if ui.small_button("全不選").clicked() {
                    for &i in &idx {
                        app.clean.rows[i].checked = false;
                    }
                }
            });
            for &i in &idx {
                rule_row(app, ui, i, measuring);
            }
        });
        ui.add_space(2.0);
    }
}

fn rule_row(app: &mut App, ui: &mut egui::Ui, i: usize, measuring: bool) {
    let elevated = app.elevated;
    let suggested = suggest::rule(&app.clean.rows[i], elevated);
    let row = &mut app.clean.rows[i];
    let locked = row.rule.admin && !elevated;
    let bytes = row.bytes();
    let is_cmd = row.is_command();
    let can = !locked && (bytes > 0 || is_cmd) && row.measured.is_some();
    let why = row.rule.why();

    // 右欄要顯示的東西先算好（左右兩個閉包不能同時借用 row）
    enum SizeCell {
        Spin,
        Text(String, Color32),
    }
    let (cell, errors) = match &row.measured {
        None if measuring => (SizeCell::Spin, 0),
        None => (SizeCell::Text("—".into(), DIM), 0),
        Some(m) if is_cmd && m.found.is_empty() => (SizeCell::Text("—".into(), DIM), m.errors),
        Some(m) if is_cmd => (SizeCell::Text(format!("約 {}", fmt::size(m.bytes)), DIM), m.errors),
        Some(m) => (SizeCell::Text(fmt::size(m.bytes), if m.bytes >= 1 << 30 { theme::STRONG } else { TEXT }), m.errors),
    };

    let (_, _, toggled) = theme::list_row(
        ui,
        26.0,
        |ui| {
            match &cell {
                SizeCell::Spin => {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(SIZE_W, 18.0), egui::Sense::hover());
                    ui.put(egui::Rect::from_center_size(egui::pos2(r.right() - 9.0, r.center().y), egui::vec2(16.0, 16.0)), egui::Spinner::new());
                }
                SizeCell::Text(t, c) => {
                    theme::right_cell(ui, SIZE_W, t, FontId::monospace(13.5), *c);
                }
            }
            if errors > 0 {
                ui.label(RichText::new("⚠").color(ORANGE))
                    .on_hover_text(format!("{errors} 個資料夾無法讀取（權限不足或使用中），實際大小可能更大"));
            }
        },
        |ui| {
            ui.add_enabled_ui(can, |ui| {
                theme::check(ui, &mut row.checked);
            });
            theme::suggest_slot(ui, suggested && !row.checked, &why);
            let arrow = if row.expanded { "▾" } else { "▸" };
            let name = RichText::new(&row.rule.name).color(if can { TEXT } else { DIM });
            let a = ui.add(egui::Label::new(RichText::new(arrow).color(DIM)).sense(egui::Sense::click()));
            let n = ui.add(egui::Label::new(name).sense(egui::Sense::click())).on_hover_text(&row.rule.desc);
            risk_badge(ui, row.rule.risk);
            if locked {
                ui.label(RichText::new("🔒 需系統管理員").small().color(ORANGE));
            }
            if is_cmd {
                ui.label(RichText::new("⚙ 執行指令").small().color(ACCENT));
            }
            if row.rule.min_age_days > 0 {
                ui.label(RichText::new(format!("{} 天前的檔案", row.rule.min_age_days)).small().color(DIM));
            }
            a.clicked() || n.clicked()
        },
    );
    if toggled {
        row.expanded = !row.expanded;
    }

    if row.expanded {
        ui.indent(("rule_detail", i), |ui| {
            ui.label(RichText::new(&row.rule.desc).small().color(DIM));
            if let Action::Command { program, args } = &row.rule.action {
                ui.label(RichText::new(format!("指令：{} {}", program, args.join(" "))).small().monospace().color(DIM));
            }
            if let Some(m) = &row.measured {
                if m.found.is_empty() {
                    ui.label(RichText::new("（找不到可清理的檔案）").small().color(DIM));
                }
                let mut found: Vec<_> = m.found.iter().collect();
                found.sort_by(|a, b| b.bytes.cmp(&a.bytes));
                for f in found.iter().take(40) {
                    theme::list_row(
                        ui,
                        20.0,
                        |ui| {
                            theme::right_cell(ui, SIZE_W, &fmt::size(f.bytes), FontId::monospace(12.5), TEXT);
                            if f.files > 1 {
                                theme::right_cell(ui, AGE_W + 16.0, &format!("{} 個檔案", fmt::count(f.files)), FontId::proportional(11.5), DIM);
                            }
                        },
                        |ui| {
                            let kind = match f.mode {
                                Mode::Contents => "內容",
                                Mode::Dir => "資料夾",
                                Mode::File => "檔案",
                                Mode::Info => "資訊",
                            };
                            if ui.small_button("📂").on_hover_text("在檔案總管中開啟").clicked() {
                                if f.mode == Mode::File || f.path.is_file() {
                                    win::reveal_in_explorer(&f.path);
                                } else {
                                    win::open_in_explorer(&f.path);
                                }
                            }
                            ui.label(RichText::new(kind).small().color(DIM));
                            ui.add(egui::Label::new(RichText::new(f.path.to_string_lossy()).small().color(TEXT)).truncate());
                            if f.errors > 0 {
                                ui.label(RichText::new(format!("⚠ {} 個無法讀取", f.errors)).small().color(ORANGE));
                            }
                        },
                    );
                }
                if found.len() > 40 {
                    ui.label(RichText::new(format!("…還有 {} 個位置", found.len() - 40)).small().color(DIM));
                }
            }
        });
    }
}

fn projects(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let total: u64 = app.clean.proj.items.iter().map(|a| a.art.bytes).sum();
    let n_items = app.clean.proj.items.len();
    let checked: u64 = app.clean.proj.items.iter().filter(|a| a.checked).map(|a| a.art.bytes).sum();
    let note = if n_items == 0 {
        None
    } else if checked > 0 {
        Some(format!("{n_items} 個 · 已勾選 {}", fmt::size(checked)))
    } else {
        Some(format!("{n_items} 個"))
    };
    section_header(ui, "projects", "📁  專案建置產物", total, note, |ui| {
        ui.label(
            RichText::new("在下列位置搜尋 node_modules、Rust target、Python .venv、Gradle／Flutter／.NET 建置輸出等。只有偵測到專案標記檔（package.json、Cargo.toml…）才會列出。")
                .small()
                .color(DIM),
        );
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for (i, r) in app.settings.roots.iter().enumerate() {
                theme::pill().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(r.to_string_lossy()).small());
                        if ui.small_button("✕").on_hover_text("移除").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
        });
        if let Some(i) = remove {
            app.settings.roots.remove(i);
            app.settings.save();
        }
        let finding = app.clean.proj.find.is_some();
        ui.horizontal(|ui| {
            let te = ui.add(egui::TextEdit::singleline(&mut app.clean.proj.new_root).hint_text("新增搜尋位置，例如 D:\\code").desired_width(240.0));
            let add = ui.button("新增").clicked() || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if add && !app.clean.proj.new_root.trim().is_empty() {
                let p = PathBuf::from(app.clean.proj.new_root.trim());
                if p.is_dir() && !app.settings.roots.contains(&p) {
                    app.settings.roots.push(p);
                    app.settings.save();
                }
                app.clean.proj.new_root.clear();
            }
            if ui.button("📂 選擇資料夾…").clicked()
                && let Some(p) = rfd::FileDialog::new().set_title("選擇要搜尋專案的資料夾").pick_folder()
                && !app.settings.roots.contains(&p)
            {
                app.settings.roots.push(p);
                app.settings.save();
            }
            if ui.small_button("還原預設").clicked() {
                app.settings.roots = crate::projects::default_roots();
                app.settings.save();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if finding {
                    if ui.button("⏹ 停止").clicked()
                        && let Some(f) = &app.clean.proj.find
                    {
                        f.cancel.store(true, Relaxed);
                    }
                } else if ui
                    .add_enabled(
                        !app.settings.roots.is_empty(),
                        egui::Button::new(RichText::new("🔍 搜尋專案產物與 AI 模型").strong()).fill(theme::with_alpha(theme::BLUE, 150)),
                    )
                    .clicked()
                {
                    app.start_find(&ctx);
                }
            });
        });
        if let Some(f) = &app.clean.proj.find {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "已搜尋 {} 個資料夾，找到 {} 個產物、{} 個模型…",
                    fmt::count(f.dirs.load(Relaxed)),
                    app.clean.proj.items.len(),
                    app.clean.proj.models.len()
                ));
            });
            ui.add(egui::Label::new(RichText::new(f.current.lock().clone()).small().color(DIM)).truncate());
        } else if let Some(s) = app.clean.proj.secs {
            ui.label(RichText::new(format!("✔ 搜尋完成（{s:.1} 秒），找到 {} 個", app.clean.proj.items.len())).color(GREEN));
        } else if let Some(age) = app.clean.proj.cached_age {
            ui.label(
                RichText::new(format!("顯示上次搜尋結果（{} 前，已排除不存在的），按「搜尋」更新。", crate::index::fmt_age(age))).color(DIM),
            );
        } else if !app.clean.proj.searched {
            ui.label(RichText::new("按「搜尋專案產物與 AI 模型」開始。").color(DIM));
        }

        if app.clean.proj.items.is_empty() {
            return;
        }
        // 類型摘要
        let mut kinds: Vec<(&'static str, &'static str, u64, usize, bool)> = Vec::new();
        for a in &app.clean.proj.items {
            if let Some(k) = kinds.iter_mut().find(|k| k.0 == a.art.kind.id) {
                k.2 += a.art.bytes;
                k.3 += 1;
                k.4 &= a.checked;
            } else {
                kinds.push((a.art.kind.id, a.art.kind.label, a.art.bytes, 1, a.checked));
            }
        }
        kinds.sort_by(|a, b| b.2.cmp(&a.2));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (id, label, bytes, n, all) in &kinds {
                let txt = format!("{label}  {}（{n}）", fmt::size(*bytes));
                if ui.selectable_label(*all, txt).on_hover_text("點一下全選／取消這一類").clicked() {
                    for a in &mut app.clean.proj.items {
                        if a.art.kind.id == *id {
                            a.checked = !*all;
                        }
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.small_button("勾選建議項目").clicked() {
                for a in &mut app.clean.proj.items {
                    a.checked = suggest::artifact(&a.art);
                }
            }
            if ui.small_button("全選").clicked() {
                for a in &mut app.clean.proj.items {
                    a.checked = true;
                }
            }
            if ui.small_button("全不選").clicked() {
                for a in &mut app.clean.proj.items {
                    a.checked = false;
                }
            }
            if ui.small_button("只選 30 天沒動的").clicked() {
                let cut = win::now_filetime().saturating_sub(30 * win::FILETIME_DAY);
                for a in &mut app.clean.proj.items {
                    a.checked = a.art.newest < cut;
                }
            }
        });
        ui.add_space(4.0);
        let row_h = 26.0;
        let n = app.clean.proj.items.len();
        egui::ScrollArea::vertical().id_salt("arts").max_height(440.0).auto_shrink([false, true]).show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let a = &mut app.clean.proj.items[i];
                let sugg = suggest::artifact(&a.art);
                let why = suggest::artifact_why(&a.art);
                let (size, age, files) = (a.art.bytes, fmt::ago(a.art.newest), a.art.files);
                theme::list_row(
                    ui,
                    row_h - 6.0,
                    |ui| {
                        theme::size_cell(ui, size, size >= 1 << 30);
                        theme::right_cell(ui, AGE_W, &age, FontId::proportional(12.0), DIM).on_hover_text("裡面最新檔案的修改時間");
                    },
                    |ui| {
                        theme::check(ui, &mut a.checked);
                        theme::suggest_slot(ui, sugg && !a.checked, &why);
                        if ui.small_button("📂").on_hover_text("在檔案總管中顯示").clicked() {
                            win::reveal_in_explorer(&a.art.path);
                        }
                        let col = if a.art.kind.risk == Risk::Safe { GREEN } else { ORANGE };
                        ui.label(RichText::new(a.art.kind.label).small().color(col)).on_hover_text(a.art.kind.hint);
                        ui.add(egui::Label::new(RichText::new(a.art.path.to_string_lossy()).color(TEXT)).truncate())
                            .on_hover_text(format!("{}\n{} 個檔案", a.art.path.display(), fmt::count(files)));
                    },
                );
            }
        });
    });
}

fn source_color(src: &str) -> Color32 {
    match src {
        "Ollama" => Color32::from_rgb(214, 224, 244),
        "ComfyUI" => theme::ORANGE,
        "LM Studio" => theme::PURPLE,
        s if s.starts_with("Hugging Face") => theme::YELLOW,
        "模型檔" => DIM,
        _ => theme::CYAN,
    }
}

fn models_section(app: &mut App, ui: &mut egui::Ui) {
    let dups = suggest::model_dups(&app.clean.proj.models);
    let total: u64 = app.clean.proj.models.iter().map(|m| m.m.bytes).sum();
    let n_models = app.clean.proj.models.len();
    let checked: u64 = app.clean.proj.models.iter().filter(|m| m.checked).map(|m| m.m.bytes).sum();
    let note = if n_models == 0 {
        None
    } else if checked > 0 {
        Some(format!("{n_models} 個 · 已勾選 {}", fmt::size(checked)))
    } else {
        Some(format!("{n_models} 個"))
    };
    let finding = app.clean.proj.find.is_some();
    section_header(ui, "models", "🧠  AI 模型", total, note, |ui| {
        ui.label(
            RichText::new("Hugging Face、Ollama、LM Studio、ComfyUI 等模型庫，以及搜尋位置中 50 MB 以上的 .safetensors／.gguf／.ckpt／.pt／.onnx 檔。模型刪除後要重新下載，預設不勾選；Ollama 模型只會刪掉沒有其他模型共用的檔案。")
                .small()
                .color(DIM),
        );
        let models = &mut app.clean.proj.models;
        if models.is_empty() {
            ui.label(RichText::new(if finding { "搜尋中…" } else { "按上面的「搜尋專案產物與 AI 模型」開始。" }).color(DIM));
            return;
        }
        let dup_bytes: u64 = models.iter().zip(&dups).filter(|(_, d)| d.is_some()).map(|(m, _)| m.m.bytes).sum();

        // 來源摘要
        let mut srcs: Vec<(&'static str, u64, usize, bool)> = Vec::new();
        for m in models.iter() {
            if let Some(s) = srcs.iter_mut().find(|s| s.0 == m.m.source) {
                s.1 += m.m.bytes;
                s.2 += 1;
                s.3 &= m.checked;
            } else {
                srcs.push((m.m.source, m.m.bytes, 1, m.checked));
            }
        }
        srcs.sort_by(|a, b| b.1.cmp(&a.1));
        ui.horizontal_wrapped(|ui| {
            for (src, bytes, n, all) in &srcs {
                let txt = RichText::new(format!("{src}  {}（{n}）", fmt::size(*bytes))).color(source_color(src));
                if ui.selectable_label(*all, txt).on_hover_text("點一下全選／取消這個來源").clicked() {
                    for m in models.iter_mut() {
                        if m.m.source == *src {
                            m.checked = !*all;
                        }
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            if dup_bytes > 0 && ui.small_button(format!("勾選重複的（每組保留最新一份，可省 {}）", fmt::size(dup_bytes))).clicked() {
                for (m, d) in models.iter_mut().zip(&dups) {
                    m.checked = d.is_some();
                }
            }
            if ui.small_button("全不選").clicked() {
                for m in models.iter_mut() {
                    m.checked = false;
                }
            }
        });
        ui.add_space(4.0);
        let row_h = 26.0;
        let n = models.len();
        let kept_paths: Vec<Option<PathBuf>> = dups.iter().map(|d| d.map(|k| models[k].m.path.clone())).collect();
        egui::ScrollArea::vertical().id_salt("models_list").max_height(440.0).auto_shrink([false, true]).show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let m = &mut models[i];
                let why = kept_paths[i].as_ref().map(|p| suggest::model_why(p)).unwrap_or_default();
                let (size, age) = (m.m.bytes, fmt::ago(m.m.newest));
                let is_dup = kept_paths[i].is_some();
                theme::list_row(
                    ui,
                    row_h - 6.0,
                    |ui| {
                        theme::size_cell(ui, size, size >= 1 << 30);
                        theme::right_cell(ui, AGE_W, &age, FontId::proportional(12.0), DIM);
                    },
                    |ui| {
                        theme::check(ui, &mut m.checked);
                        theme::suggest_slot(ui, is_dup && !m.checked, &why);
                        if ui.small_button("📂").on_hover_text("在檔案總管中顯示").clicked() {
                            win::reveal_in_explorer(&m.m.path);
                        }
                        ui.label(RichText::new(m.m.source).small().color(source_color(m.m.source)));
                        if is_dup {
                            ui.label(RichText::new("重複").small().color(ORANGE));
                        }
                        ui.label(RichText::new(&m.m.name).strong().color(TEXT));
                        ui.add(egui::Label::new(RichText::new(m.m.path.to_string_lossy()).small().color(DIM)).truncate());
                    },
                );
            }
        });
    });
}

fn confirm_modal(app: &mut App, ctx: &egui::Context) {
    if !app.clean.confirm {
        return;
    }
    let (bytes, n) = app.selected_bytes();
    let mut go = false;
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::new("confirm_clean")).show(ctx, |ui| {
        ui.set_width(580.0);
        ui.heading("確認清除");
        ui.add_space(6.0);
        ui.label(format!("即將清除 {n} 個項目，預計釋放約 {}。", fmt::size(bytes)));
        ui.label(RichText::new("檔案會直接刪除，不會經過資源回收筒。使用中的檔案會自動略過。").color(ORANGE));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            let mut sel: Vec<_> = app.clean.rows.iter().filter(|r| r.checked).collect();
            sel.sort_by(|a, b| b.bytes().cmp(&a.bytes()));
            let line = |ui: &mut egui::Ui, size: u64, name: &str, tag: Option<(&str, Color32)>| {
                theme::list_row(
                    ui,
                    20.0,
                    |ui| theme::size_cell(ui, size, false),
                    |ui| {
                        ui.label(name);
                        if let Some((t, c)) = tag {
                            ui.label(RichText::new(t).small().color(c));
                        }
                    },
                );
            };
            for r in sel {
                let tag = if r.is_command() {
                    Some(("⚙ 指令", ACCENT))
                } else if r.rule.risk == Risk::Caution {
                    Some(("注意", ORANGE))
                } else {
                    None
                };
                line(ui, r.bytes(), &r.rule.name, tag);
            }
            let arts: Vec<_> = app.clean.proj.items.iter().filter(|a| a.checked).collect();
            if !arts.is_empty() {
                line(ui, arts.iter().map(|a| a.art.bytes).sum(), &format!("專案建置產物 {} 個", arts.len()), None);
            }
            let ms: Vec<_> = app.clean.proj.models.iter().filter(|m| m.checked).collect();
            if !ms.is_empty() {
                line(ui, ms.iter().map(|m| m.m.bytes).sum(), &format!("AI 模型 {} 個", ms.len()), Some(("注意", ORANGE)));
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("  確定清除  ").strong()).fill(theme::with_alpha(RED, 220))).clicked() {
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
        app.clean.confirm = false;
        app.start_clean(ctx);
    } else if close {
        app.clean.confirm = false;
    }
}

fn progress_modal(app: &mut App, ctx: &egui::Context) {
    let Some(job) = &app.clean.job else { return };
    let mut close = false;
    egui::Modal::new(egui::Id::new("clean_progress")).show(ctx, |ui| {
        ui.set_width(620.0);
        let p = &job.prog;
        let freed = p.freed.load(Relaxed);
        match job.finished {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.heading("清理中…");
                });
            }
            Some(s) => {
                ui.heading(RichText::new(format!("✔ 清理完成（{s:.1} 秒）")).color(GREEN));
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("已釋放");
            ui.label(RichText::new(fmt::size(freed)).size(28.0).strong().color(theme::STRONG));
            ui.label(RichText::new(format!("（預計 {}）", fmt::size(job.planned))).color(DIM));
        });
        let steps = p.steps.load(Relaxed).max(1);
        let step = p.step.load(Relaxed);
        ui.add(egui::ProgressBar::new(step as f32 / steps as f32).fill(theme::BLUE).text(format!("{step}／{steps}")));
        ui.label(format!(
            "已刪除 {} 個檔案{}",
            fmt::count(p.files.load(Relaxed)),
            match p.failed.load(Relaxed) {
                0 => String::new(),
                f => format!("，{f} 個使用中或無權限已略過"),
            }
        ));
        if job.finished.is_none() {
            ui.label(RichText::new(p.current.lock().clone()).small().color(DIM));
        }
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
            for l in p.log.lock().iter() {
                ui.label(RichText::new(l).small());
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if job.finished.is_none() {
                if ui.button("⏹ 停止").clicked() {
                    p.cancel.store(true, Relaxed);
                }
            } else if ui.button("  關閉  ").clicked() {
                close = true;
            }
        });
    });
    if close {
        app.clean.job = None;
    }
}
