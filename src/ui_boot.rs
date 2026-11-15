//! 開機加速分頁：開機時間、系統設定檢查、開機自動啟動的程式、拖慢開機的項目。

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, vec2};

use crate::app::{App, Tab};
use crate::boot::{self, BootLog, StartupItem};
use crate::fmt;
use crate::theme::{self, BLUE, DIM, GREEN, ORANGE, PERI, RED, TEXT};

fn secs(ms: u32) -> String {
    format!("{:.1} 秒", ms as f32 / 1000.0)
}

fn title(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t).size(17.0).strong());
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    if let Some(rx) = &app.boot.job
        && let Ok(d) = rx.try_recv()
    {
        app.boot.data = Some(d);
        app.boot.job = None;
    }
    if app.boot.data.is_none() && app.boot.job.is_none() {
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
                ui.add_space(20.0);
            });
        });
    });
}

fn header(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("開機加速").size(20.0).strong());
        ui.label(RichText::new("找出讓開機變慢的原因。停用開機自動啟動的程式和工作管理員的「啟動」分頁相同，隨時可以再打開。").color(DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let loading = app.boot.job.is_some();
            if ui.add_enabled(!loading, egui::Button::new("⟳ 重新整理")).clicked() {
                app.boot.msg = None;
                app.start_boot_load(ui.ctx());
            }
            if loading {
                ui.spinner();
            }
        });
    });
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
                            ui.label(RichText::new("比平常慢").small().color(ORANGE));
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
    let bw = ((w - gap * (n - 1.0)) / n).min(26.0);
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
    let Some(data) = &app.boot.data else { return };
    let sys = data.sys.clone();
    // 依實際的程式算（同一個指令重複登記的只算一個）
    let enabled = groups(&data.items).iter().filter(|g| data.items[g[0]].enabled).count();
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
        let fast_on = sys.fast_startup == Some(true) && sys.hibernate != Some(false);
        let fast_txt = match (sys.fast_startup, sys.hibernate) {
            (Some(true), Some(false)) => RichText::new("沒有作用（休眠已關閉）").color(ORANGE),
            (Some(true), _) => RichText::new("已開啟").color(GREEN),
            (Some(false), _) => RichText::new("已關閉").color(ORANGE),
            (None, _) => RichText::new("讀不到").color(DIM),
        };
        row(
            ui,
            "快速啟動",
            fast_txt,
            "關機時把系統核心存起來，下次開機直接載入，冷開機會快不少；「重新啟動」不受影響。清理頁的「休眠檔」會把它一起關掉。",
            &mut |ui| {
                let btn = if fast_on { "關閉" } else { "開啟" };
                let r = ui.add_enabled(elevated, egui::Button::new(btn).small());
                let r = if elevated { r } else { r.on_disabled_hover_text("需要系統管理員權限") };
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
            "系統磁碟太滿時，Windows 開機、更新和虛擬記憶體都會變慢。建議至少留 15% 或 20 GB。",
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
            Some(true) => ("SSD", GREEN, "系統裝在固態硬碟上，這一項沒問題。"),
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
        match boot::set_fast_startup(on) {
            Ok(()) => app.boot.msg = Some((format!("已{}快速啟動，下次關機再開機時生效。", if on { "開啟" } else { "關閉" }), false)),
            Err(e) => app.boot.msg = Some((e, true)),
        }
        app.start_boot_load(ui.ctx());
    }
    if go_clean {
        app.tab = Tab::Clean;
    }
}

// ---------------------------------------------------------------------------

/// 指令完全相同、來源相同的項目合成一列（例如 Edge 的 MicrosoftEdgeAutoLaunch_* 常常有十幾筆）
fn groups(items: &[StartupItem]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let key = it.command.trim().to_lowercase();
        match out.iter_mut().find(|g| {
            let f = &items[g[0]];
            f.source == it.source && f.command.trim().to_lowercase() == key
        }) {
            Some(g) => g.push(i),
            None => out.push(vec![i]),
        }
    }
    out
}

fn startup_card(app: &mut App, ui: &mut egui::Ui) {
    let elevated = app.elevated;
    let Some(data) = &app.boot.data else { return };
    let items = data.items.clone();
    let mut gs = groups(&items);
    // 啟用的在前；有量到拖慢時間的再往前；其他照名稱
    gs.sort_by(|a, b| {
        let (x, y) = (&items[a[0]], &items[b[0]]);
        y.enabled.cmp(&x.enabled).then(y.delay_ms.unwrap_or(0).cmp(&x.delay_ms.unwrap_or(0))).then(x.name.to_lowercase().cmp(&y.name.to_lowercase()))
    });
    let enabled = gs.iter().filter(|g| items[g[0]].enabled).count();
    let total = gs.len();
    let mut toggles: Vec<(Vec<usize>, bool)> = Vec::new();
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            title(ui, "開機自動啟動的程式");
            ui.label(RichText::new(format!("{enabled}／{total} 個啟用")).color(DIM));
        });
        ui.label(
            RichText::new("取消勾選＝開機時不自動執行。只是不讓它自己啟動，程式本身不會被移除，需要時照樣可以手動打開，也隨時可以再勾回來。")
                .small()
                .color(DIM),
        );
        ui.add_space(4.0);
        if items.is_empty() {
            ui.label(RichText::new("沒有找到開機自動啟動的程式。").color(DIM));
        }
        for g in &gs {
            let it = &items[g[0]];
            let locked = it.source.needs_admin() && !elevated;
            let mut on = it.enabled;
            let (_, _, clicked) = theme::list_row(
                ui,
                26.0,
                |ui| {
                    theme::right_cell(ui, 130.0, it.source.label(), FontId::proportional(12.0), DIM);
                    match it.delay_ms {
                        Some(ms) => theme::right_cell(ui, 110.0, &format!("拖慢 {}", secs(ms)), FontId::proportional(12.5), ORANGE)
                            .on_hover_text("開機效能記錄裡，這個程式讓開機變慢的平均時間"),
                        None => theme::right_cell(ui, 110.0, "", FontId::proportional(12.5), DIM),
                    };
                },
                |ui| {
                    let r = ui.add_enabled_ui(!locked, |ui| theme::check(ui, &mut on)).inner;
                    if locked {
                        ui.label(RichText::new("🔒").small().color(ORANGE)).on_hover_text("這一項套用在所有使用者，要系統管理員權限才能改");
                    }
                    let name = if g.len() > 1 { format!("{}（{} 筆相同）", display_name(it), g.len()) } else { display_name(it) };
                    ui.label(RichText::new(name).color(if it.enabled { TEXT } else { DIM }).strong());
                    if it.missing {
                        ui.label(RichText::new("找不到程式").small().color(RED))
                            .on_hover_text("執行檔已經不在了，通常是解除安裝後留下的殘留項目，停用不會有任何影響。");
                    }
                    ui.add(egui::Label::new(RichText::new(&it.command).small().color(DIM)).truncate()).on_hover_text(&it.command);
                    r.changed()
                },
            );
            if clicked {
                toggles.push((g.clone(), on));
            }
        }
    });
    for (idx, on) in toggles {
        let mut err = None;
        let mut done = Vec::new();
        for &i in &idx {
            match boot::set_enabled(&items[i], on) {
                Ok(()) => done.push(i),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        // 成功改到的才更新畫面（一組裡可能前幾筆成功、後面失敗）
        if let Some(d) = &mut app.boot.data {
            for &i in &done {
                d.items[i].enabled = on;
            }
        }
        app.boot.msg = Some(match err {
            Some(e) => (format!("{}：{e}", display_name(&items[idx[0]])), true),
            None => (format!("已{} {}，下次登入生效。", if on { "啟用" } else { "停用" }, display_name(&items[idx[0]])), false),
        });
    }
}

/// 讓名稱好讀一點：Edge 的一長串代碼、Electron 的 electron.app.xxx、捷徑的 .lnk
fn display_name(it: &StartupItem) -> String {
    let n = it.name.as_str();
    if n.starts_with("MicrosoftEdgeAutoLaunch_") {
        return "Microsoft Edge（開機時預先載入）".into();
    }
    let n = n.strip_prefix("electron.app.").unwrap_or(n);
    let n = n.strip_suffix(".lnk").unwrap_or(n);
    n.to_string()
}

// ---------------------------------------------------------------------------

fn slow_card(app: &mut App, ui: &mut egui::Ui) {
    let Some(data) = &app.boot.data else { return };
    theme::glass().show(ui, |ui| {
        ui.set_width(ui.available_width());
        title(ui, "拖慢開機的項目");
        ui.label(
            RichText::new("Windows 自己記錄的：這些程式、驅動、服務或裝置讓開機比平常慢。數字是平均每次拖慢的時間。").small().color(DIM),
        );
        ui.add_space(4.0);
        match &data.log {
            BootLog::Ok { slow, .. } if !slow.is_empty() => {
                for s in slow.iter().take(15) {
                    theme::list_row(
                        ui,
                        24.0,
                        |ui| {
                            theme::right_cell(ui, 90.0, &format!("{} 次", s.count), FontId::proportional(12.0), DIM);
                            theme::right_cell(ui, 110.0, &format!("平均 {}", secs(s.avg_ms())), FontId::proportional(13.0), ORANGE)
                                .on_hover_text(format!("最慢一次 {}", secs(s.max_ms)));
                        },
                        |ui| {
                            let col = match s.kind {
                                "程式" => BLUE,
                                "驅動程式" | "裝置" => Color32::from_rgb(214, 128, 52),
                                "服務" => PERI,
                                _ => DIM,
                            };
                            ui.add_sized([72.0, 18.0], egui::Label::new(RichText::new(s.kind).small().color(col)));
                            ui.add(egui::Label::new(RichText::new(&s.name).color(TEXT)).truncate()).on_hover_text(if s.raw.is_empty() { &s.name } else { &s.raw });
                        },
                    );
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("「程式」類可以在上面停用它的開機自動啟動；驅動程式和裝置通常要更新驅動，或到裝置管理員停用用不到的裝置。")
                        .small()
                        .color(DIM),
                );
            }
            BootLog::Ok { .. } => {
                ui.label(RichText::new("最近幾次開機沒有記錄到特別拖慢的項目。").color(DIM));
            }
            BootLog::NeedAdmin => {
                ui.label(RichText::new("需要系統管理員權限才讀得到。").color(DIM));
            }
            BootLog::Unavailable(m) => {
                ui.label(RichText::new(m).color(DIM));
            }
        }
    });
}
