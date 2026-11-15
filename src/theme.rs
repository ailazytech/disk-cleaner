//! 磨砂 3D 圓潤主題：淺灰白方格底、霧面白卡片＋柔和陰影、粉彩霧面色票。

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Shadow, Shape, Stroke, Vec2};

// 霧面粉彩色票（參考：藍／長春花紫／粉紅／霧白）
pub const BLUE: Color32 = Color32::from_rgb(84, 136, 250);
pub const PERI: Color32 = Color32::from_rgb(118, 118, 236);
pub const PINK: Color32 = Color32::from_rgb(240, 118, 140);
pub const SKY: Color32 = Color32::from_rgb(112, 176, 252);
pub const ORANGE: Color32 = Color32::from_rgb(250, 158, 98);
pub const GREEN: Color32 = Color32::from_rgb(76, 190, 150);
pub const YELLOW: Color32 = Color32::from_rgb(240, 190, 80);
pub const RED: Color32 = Color32::from_rgb(232, 96, 106);
pub const PURPLE: Color32 = Color32::from_rgb(156, 122, 232);
pub const CYAN: Color32 = Color32::from_rgb(92, 190, 226);
pub const TEAL: Color32 = Color32::from_rgb(64, 182, 186);
pub const SLATE: Color32 = Color32::from_rgb(150, 160, 186);
pub const MIST: Color32 = Color32::from_rgb(222, 227, 246);

/// 文字
pub const TEXT: Color32 = Color32::from_rgb(52, 60, 88);
pub const STRONG: Color32 = Color32::from_rgb(26, 32, 58);
pub const DIM: Color32 = Color32::from_rgb(124, 132, 158);
pub const ACCENT: Color32 = BLUE;

pub const CARD: Color32 = Color32::from_rgb(250, 251, 254);
pub const LINE: Color32 = Color32::from_rgb(222, 226, 238);
pub const ROW_HOVER: Color32 = Color32::from_rgba_premultiplied(18, 28, 64, 22);

/// 清單右側欄寬（大小、時間）
pub const SIZE_W: f32 = 92.0;
pub const AGE_W: f32 = 76.0;
/// 內容最大寬度：視窗再寬也不會把名稱和大小拉得太遠
pub const CONTENT_W: f32 = 1080.0;

pub fn white(a: u8) -> Color32 {
    Color32::from_white_alpha(a)
}

/// 深藍墨色（淺色背景上的細線、淡底）
pub fn ink(a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(30, 40, 80, a)
}

pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// 往白色混（霧面的亮面）
pub fn tint(c: Color32, t: f32) -> Color32 {
    let m = |v: u8| (v as f32 + (255.0 - v as f32) * t).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
}

pub fn soft_shadow() -> Shadow {
    Shadow { offset: [0, 6], blur: 22, spread: 0, color: Color32::from_rgba_unmultiplied(40, 52, 100, 26) }
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.dark_mode = false;
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = CARD;
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(18);
        v.window_shadow = Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_rgba_unmultiplied(30, 40, 90, 46) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_rgba_unmultiplied(30, 40, 90, 34) };
        v.menu_corner_radius = CornerRadius::same(12);
        v.extreme_bg_color = Color32::WHITE;
        v.faint_bg_color = ink(8);
        v.code_bg_color = ink(10);
        v.hyperlink_color = BLUE;
        v.selection.bg_fill = with_alpha(BLUE, 70);
        v.selection.stroke = Stroke::new(1.0, BLUE);
        v.override_text_color = None;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = CornerRadius::same(10);
        let states = [
            (&mut w.inactive, Color32::WHITE, Color32::from_rgb(214, 220, 236), TEXT),
            (&mut w.hovered, Color32::from_rgb(240, 245, 255), Color32::from_rgb(170, 190, 245), STRONG),
            (&mut w.active, Color32::from_rgb(226, 234, 255), BLUE, STRONG),
            (&mut w.open, Color32::from_rgb(236, 241, 255), Color32::from_rgb(170, 190, 245), STRONG),
        ];
        for (st, bg, stroke, fg) in states {
            st.bg_fill = bg;
            st.weak_bg_fill = bg;
            st.bg_stroke = Stroke::new(1.0, stroke);
            st.fg_stroke = Stroke::new(1.3, fg);
            st.corner_radius = CornerRadius::same(9);
        }
        w.hovered.expansion = 0.5;

        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.interaction.tooltip_delay = 0.25;
        for (ts, f) in s.text_styles.iter_mut() {
            match ts {
                egui::TextStyle::Body | egui::TextStyle::Button => f.size = 14.5,
                egui::TextStyle::Small => f.size = 12.0,
                egui::TextStyle::Heading => f.size = 20.0,
                egui::TextStyle::Monospace => f.size = 13.5,
                _ => {}
            }
        }
    });
}

/// 淺灰白底＋淡淡的方格線（像參考圖的方格紙）。
pub fn paint_background(ctx: &egui::Context) {
    let r = ctx.viewport_rect();
    let p = ctx.layer_painter(egui::LayerId::background());
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(r.left_top(), Color32::from_rgb(242, 244, 250));
    mesh.colored_vertex(r.right_top(), Color32::from_rgb(238, 241, 249));
    mesh.colored_vertex(r.left_bottom(), Color32::from_rgb(232, 236, 245));
    mesh.colored_vertex(r.right_bottom(), Color32::from_rgb(228, 232, 243));
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    p.add(Shape::mesh(mesh));
    let step = 24.0;
    let minor = Stroke::new(1.0, ink(9));
    let mut x = r.left() + step;
    while x < r.right() {
        p.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], minor);
        x += step;
    }
    let mut y = r.top() + step;
    while y < r.bottom() {
        p.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], minor);
        y += step;
    }
}

/// 霧面白卡片＋柔和陰影
pub fn glass() -> egui::Frame {
    egui::Frame::NONE
        .fill(CARD)
        .stroke(Stroke::new(1.0, Color32::from_rgb(230, 234, 244)))
        .corner_radius(18)
        .shadow(soft_shadow())
        .inner_margin(egui::Margin::same(16))
}

/// 上下工具列
pub fn bar() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(252, 253, 255, 235))
        .shadow(Shadow { offset: [0, 2], blur: 12, spread: 0, color: Color32::from_rgba_unmultiplied(30, 40, 90, 20) })
        .inner_margin(egui::Margin::symmetric(12, 8))
}

/// 小藥丸（標籤、晶片）
pub fn pill() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(12)
        .shadow(Shadow { offset: [0, 2], blur: 6, spread: 0, color: Color32::from_rgba_unmultiplied(30, 40, 90, 16) })
        .inner_margin(egui::Margin::symmetric(9, 3))
}

/// 置中、限制最大寬度的欄位，讓清單在寬螢幕也好對照。
pub fn centered<R>(ui: &mut egui::Ui, max_w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let avail = ui.available_width();
    let w = avail.min(max_w);
    let pad = ((avail - w) / 2.0).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(w);
            ui.set_max_width(w);
            add(ui)
        })
        .inner
    })
    .inner
}

/// 一列清單：右邊是固定欄（由右往左排），左邊是其餘內容；滑鼠移上去整列有底色。
pub fn list_row<R>(
    ui: &mut egui::Ui,
    height: f32,
    right: impl FnOnce(&mut egui::Ui),
    left: impl FnOnce(&mut egui::Ui) -> R,
) -> (Rect, bool, R) {
    let bg = ui.painter().add(Shape::Noop);
    let res = ui.horizontal(|ui| {
        ui.set_min_height(height);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            right(ui);
            ui.with_layout(Layout::left_to_right(Align::Center), left).inner
        })
        .inner
    });
    let rect = res.response.rect.expand2(Vec2::new(6.0, 1.0));
    let hovered = ui.rect_contains_pointer(rect);
    if hovered {
        ui.painter().set(bg, Shape::rect_filled(rect, 9, ROW_HOVER));
    }
    (rect, hovered, res.inner)
}

/// 固定寬度、靠右對齊的文字欄。
pub fn right_cell(ui: &mut egui::Ui, width: f32, text: &str, font: FontId, color: Color32) -> egui::Response {
    let h = ui.spacing().interact_size.y;
    let (r, resp) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    ui.painter().text(Pos2::new(r.right(), r.center().y), Align2::RIGHT_CENTER, text, font, color);
    resp
}

pub fn size_cell(ui: &mut egui::Ui, bytes: u64, strong: bool) {
    let color = if strong { STRONG } else { TEXT };
    right_cell(ui, SIZE_W, &crate::fmt::size(bytes), FontId::monospace(13.5), color);
}

/// 圓角方形勾選框（egui 內建的會跟著按鈕圓角變成圓形）。
pub fn check(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(18.0, 18.0);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(22.0, 20.0), Sense::click());
    let r = Rect::from_center_size(rect.center(), size);
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let enabled = ui.is_enabled();
    let hot = resp.hovered() && enabled;
    let p = ui.painter();
    if *on {
        let fill = if enabled { BLUE } else { Color32::from_rgb(180, 190, 215) };
        p.rect_filled(r, 5, if hot { tint(fill, 0.12) } else { fill });
        // 上緣微亮，霧面立體感
        p.rect_filled(Rect::from_min_max(r.min, Pos2::new(r.max.x, r.min.y + 6.0)).shrink(1.0), 4, white(40));
        let pts = [Pos2::new(r.left() + 4.5, r.center().y + 0.5), Pos2::new(r.left() + 7.8, r.bottom() - 4.8), Pos2::new(r.right() - 4.2, r.top() + 5.0)];
        p.add(Shape::line(pts.to_vec(), Stroke::new(2.0, Color32::WHITE)));
    } else {
        p.rect(
            r,
            5,
            if enabled { Color32::WHITE } else { Color32::from_rgb(240, 242, 248) },
            Stroke::new(1.3, if hot { BLUE } else { Color32::from_rgb(196, 204, 226) }),
            egui::StrokeKind::Inside,
        );
    }
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 勾選框後面的「建議」淺色標籤；hover 說明為什麼可以清。沒有建議時保留同寬空位，讓名稱對齊。
pub fn suggest_slot(ui: &mut egui::Ui, show: bool, why: &str) {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(36.0, 19.0), Sense::hover());
    if !show {
        return;
    }
    let hot = resp.hovered();
    ui.painter().rect(
        r,
        10,
        if hot { Color32::from_rgb(220, 230, 255) } else { Color32::from_rgb(236, 241, 255) },
        Stroke::new(1.0, if hot { with_alpha(BLUE, 160) } else { Color32::from_rgb(206, 218, 250) }),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        "建議",
        FontId::proportional(11.5),
        if hot { BLUE } else { Color32::from_rgb(128, 150, 214) },
    );
    resp.on_hover_ui(|ui| {
        ui.set_max_width(360.0);
        ui.label(egui::RichText::new("建議清除").strong().color(BLUE));
        ui.label(why);
    });
}

/// 標籤的配色：（文字, 底色, 滑鼠移上去的底色, 外框）
pub type TagColors = (Color32, Color32, Color32, Color32);
pub const TAG_ORANGE: TagColors =
    (Color32::from_rgb(214, 128, 52), Color32::from_rgb(255, 243, 230), Color32::from_rgb(255, 232, 208), Color32::from_rgb(246, 210, 172));
pub const TAG_GREEN: TagColors =
    (Color32::from_rgb(64, 160, 110), Color32::from_rgb(232, 247, 238), Color32::from_rgb(214, 241, 225), Color32::from_rgb(190, 228, 206));
pub const TAG_PERI: TagColors =
    (Color32::from_rgb(96, 98, 210), Color32::from_rgb(240, 240, 255), Color32::from_rgb(228, 228, 255), Color32::from_rgb(208, 208, 246));
pub const TAG_GRAY: TagColors =
    (Color32::from_rgb(124, 132, 158), Color32::from_rgb(243, 245, 250), Color32::from_rgb(232, 235, 244), Color32::from_rgb(218, 222, 234));

/// 固定寬度的圓角小標籤（同一欄才會對齊）；滑鼠移上去顯示 hover 的內容。
pub fn tag(ui: &mut egui::Ui, width: f32, text: &str, c: TagColors, hover: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let (fg, bg, bg_hot, line) = c;
    let (r, resp) = ui.allocate_exact_size(Vec2::new(width, 19.0), Sense::hover());
    let hot = resp.hovered();
    ui.painter().rect(r, 10, if hot { bg_hot } else { bg }, Stroke::new(1.0, if hot { with_alpha(fg, 160) } else { line }), egui::StrokeKind::Inside);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, text, FontId::proportional(11.5), fg);
    resp.on_hover_ui(|ui| {
        ui.set_max_width(380.0);
        hover(ui);
    })
}

/// 「安全／注意」小標籤。滑鼠移上去說明為什麼安全、或要注意什麼。
pub fn risk_tag(ui: &mut egui::Ui, caution: bool, why: &str) {
    let (txt, title, c) = if caution { ("注意", "要注意的地方", TAG_ORANGE) } else { ("安全", "為什麼安全", TAG_GREEN) };
    tag(ui, 36.0, txt, c, |ui| {
        ui.label(egui::RichText::new(title).strong().color(c.0));
        ui.label(why);
    });
}

/// 使用率長條（霧面、圓角、上緣微亮）
pub fn usage_bar(ui: &mut egui::Ui, frac: f32, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let p = ui.painter();
    let r = height / 2.0;
    p.rect_filled(rect, r, Color32::from_rgb(226, 230, 242));
    let col = if frac > 0.9 {
        RED
    } else if frac > 0.75 {
        ORANGE
    } else {
        BLUE
    };
    let mut fill = rect;
    fill.set_width((rect.width() * frac.clamp(0.0, 1.0)).max(height));
    p.rect_filled(fill, r, col);
    let mut hl = fill.shrink2(Vec2::new(r * 0.6, 0.0));
    hl.set_height(height * 0.4);
    hl = hl.translate(Vec2::new(0.0, height * 0.12));
    p.rect_filled(hl, r, white(70));
}
