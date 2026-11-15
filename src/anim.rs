//! 清理進度的小動畫：霧面 3D 垃圾桶，檔案小卡片沿拋物線飛進去，蓋子跟著彈；完成時蓋上並跳出勾勾。
//! 飛進去的速度跟著實際刪除速度變化。

use eframe::egui::{self, Color32, Mesh, Painter, Pos2, Sense, Shape, Stroke, Vec2, pos2, vec2};

use crate::theme;

const FLIGHT: f64 = 0.95;
const MAX_CHIPS: usize = 14;

#[derive(Clone, Copy)]
struct Chip {
    id: u64,
    born: f64,
}

/// 動畫狀態（每次開始清理時重設）
#[derive(Default)]
pub struct TrashAnim {
    chips: Vec<Chip>,
    next_id: u64,
    last_spawn: f64,
    last_files: u64,
    last_time: f64,
    rate: f32,
    lid: f32,
    kick: f32,
    done_at: Option<f64>,
}

fn rnd(id: u64, k: u64) -> f32 {
    let mut x = id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 31;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 29;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

fn mul(c: Color32, f: f32) -> Color32 {
    let f = f.max(0.0);
    Color32::from_rgba_unmultiplied(
        (c.r() as f32 * f).min(255.0) as u8,
        (c.g() as f32 * f).min(255.0) as u8,
        (c.b() as f32 * f).min(255.0) as u8,
        c.a(),
    )
}

fn rot(p: Pos2, o: Pos2, a: f32) -> Pos2 {
    let (s, c) = a.sin_cos();
    let d = p - o;
    pos2(o.x + d.x * c - d.y * s, o.y + d.x * s + d.y * c)
}

fn ellipse(c: Pos2, rx: f32, ry: f32, n: usize) -> Vec<Pos2> {
    (0..n).map(|i| {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        pos2(c.x + rx * t.cos(), c.y + ry * t.sin())
    })
    .collect()
}

/// 兩條點列之間填成一條帶子（每段兩個三角形），顏色逐點指定
fn band(painter: &Painter, top: &[Pos2], bot: &[Pos2], cols: &[Color32]) {
    let mut m = Mesh::default();
    for i in 0..top.len() {
        m.colored_vertex(top[i], cols[i]);
        m.colored_vertex(bot[i], cols[i]);
    }
    for i in 0..top.len().saturating_sub(1) {
        let a = (i * 2) as u32;
        m.add_triangle(a, a + 1, a + 2);
        m.add_triangle(a + 1, a + 3, a + 2);
    }
    painter.add(Shape::mesh(m));
}

const PALETTE: [Color32; 7] = [theme::BLUE, theme::PINK, theme::PERI, theme::SKY, theme::ORANGE, theme::GREEN, theme::YELLOW];

impl TrashAnim {
    /// 畫一格動畫。`files`：目前已刪除的檔案數；`running`：還在清理中。
    pub fn ui(&mut self, ui: &mut egui::Ui, size: Vec2, files: u64, running: bool) {
        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
        let now = ui.input(|i| i.time);
        let dt = if self.last_time == 0.0 { 0.016 } else { (now - self.last_time).clamp(0.0, 0.1) } as f32;
        self.last_time = now;

        // 刪除速度（每秒檔案數，平滑過）
        let delta = files.saturating_sub(self.last_files);
        self.last_files = files;
        if dt > 0.0 {
            let inst = delta as f32 / dt;
            self.rate += (inst - self.rate) * (dt * 3.0).min(1.0);
        }

        // 產生新的小卡片
        if running {
            let interval = if self.rate < 1.0 { 0.7 } else { (0.5 / (1.0 + self.rate / 150.0)).clamp(0.09, 0.5) } as f64;
            if now - self.last_spawn >= interval && self.chips.len() < MAX_CHIPS {
                self.chips.push(Chip { id: self.next_id, born: now });
                self.next_id += 1;
                self.last_spawn = now;
            }
        } else if self.done_at.is_none() {
            self.done_at = Some(now);
        }
        // 落進桶子的卡片：讓蓋子彈一下
        let before = self.chips.len();
        self.chips.retain(|c| now - c.born < FLIGHT);
        if self.chips.len() < before {
            self.kick = 1.0;
        }

        // 蓋子：清理中打開，完成時蓋上；落入時抖一下
        let target = if running { 1.15 } else { 0.0 };
        self.lid += (target - self.lid) * (dt * if running { 7.0 } else { 9.0 }).min(1.0);
        self.kick *= (-dt * 7.0).exp();
        let wobble = self.kick * 0.16 * (now as f32 * 28.0).sin();

        let p = ui.painter_at(r);
        let cx = r.left() + r.width() * 0.62;
        let by = r.bottom() - 18.0;
        let (tw, bw, h, ry) = (40.0f32, 31.0f32, 66.0f32, 9.0f32);
        let top_y = by - h;
        let rb = ry * bw / tw;
        let bin = Color32::from_rgb(150, 162, 244);

        // 地面陰影
        p.add(Shape::convex_polygon(ellipse(pos2(cx + 5.0, by + 7.0), bw + 20.0, 7.5, 40), Color32::from_rgba_unmultiplied(40, 50, 110, 26), Stroke::NONE));
        // 桶口（裡面暗一點）
        p.add(Shape::convex_polygon(ellipse(pos2(cx, top_y), tw - 2.0, ry - 1.5, 48), Color32::from_rgb(96, 104, 196), Stroke::NONE));

        // 飛行中的檔案卡片（畫在桶身前面之前：掉進桶口以下就被桶身擋住）
        for c in &self.chips {
            let u = ((now - c.born) / FLIGHT) as f32;
            let start = pos2(r.left() + 10.0 + rnd(c.id, 1) * 14.0, r.top() + 34.0 + rnd(c.id, 2) * 46.0);
            let end = pos2(cx - 10.0 + rnd(c.id, 3) * 20.0, top_y + 6.0);
            let peak = 34.0 + rnd(c.id, 4) * 22.0;
            let e = 1.0 - (1.0 - u).powi(2) * 0.15; // 前段稍快
            let x = start.x + (end.x - start.x) * u;
            let y = start.y + (end.y - start.y) * u * e - 4.0 * peak * u * (1.0 - u);
            let spin = (rnd(c.id, 5) - 0.5) * 2.4 * u;
            let scale = if u > 0.82 { 1.0 - (u - 0.82) / 0.18 * 0.55 } else { 0.6 + u.min(0.15) / 0.15 * 0.4 };
            let col = PALETTE[(c.id % PALETTE.len() as u64) as usize];
            chip(&p, pos2(x, y), scale, spin, col);
        }

        // 桶身前半：左上光源的柱面明暗
        let n = 28;
        let mut top = Vec::with_capacity(n + 1);
        let mut bot = Vec::with_capacity(n + 1);
        let mut cols = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let t = std::f32::consts::PI * (1.0 - i as f32 / n as f32); // π（左）→ 0（右）
            top.push(pos2(cx + tw * t.cos(), top_y + ry * t.sin()));
            bot.push(pos2(cx + bw * t.cos(), by + rb * t.sin()));
            let light = 0.80 + 0.30 * (t - 2.25).cos().max(-0.4);
            cols.push(mul(bin, light));
        }
        band(&p, &top, &bot, &cols);
        // 立體的直條紋
        for t in [2.35f32, 1.57, 0.8] {
            let a = pos2(cx + (tw - 6.0) * t.cos(), top_y + ry * t.sin() + 7.0);
            let b = pos2(cx + (bw - 4.0) * t.cos(), by + rb * t.sin() - 7.0);
            p.line_segment([a, b], Stroke::new(3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 46)));
        }
        // 桶緣
        let rim: Vec<Pos2> = (0..=n).map(|i| {
            let t = std::f32::consts::PI * (1.0 - i as f32 / n as f32);
            pos2(cx + tw * t.cos(), top_y + ry * t.sin())
        })
        .collect();
        p.add(Shape::line(rim, Stroke::new(3.0, Color32::from_rgb(208, 214, 255))));

        // 蓋子：以右側為軸往上掀
        let hinge = pos2(cx + tw + 2.0, top_y - 3.0);
        let ang = -(self.lid + wobble).max(0.0);
        let lc = pos2(cx, top_y - 4.0);
        let lid_top: Vec<Pos2> = ellipse(lc, tw + 3.0, ry + 1.5, 48).into_iter().map(|q| rot(q, hinge, ang)).collect();
        let front: Vec<Pos2> = (0..=n)
            .map(|i| {
                let t = std::f32::consts::PI * (1.0 - i as f32 / n as f32);
                pos2(lc.x + (tw + 3.0) * t.cos(), lc.y + (ry + 1.5) * t.sin())
            })
            .collect();
        let edge_top: Vec<Pos2> = front.iter().map(|&q| rot(q, hinge, ang)).collect();
        let edge_bot: Vec<Pos2> = front.iter().map(|&q| rot(q + vec2(0.0, 6.0), hinge, ang)).collect();
        let edge_cols: Vec<Color32> = (0..=n).map(|i| mul(bin, 0.78 + 0.18 * (i as f32 / n as f32 * 3.1).sin())).collect();
        band(&p, &edge_top, &edge_bot, &edge_cols);
        p.add(Shape::convex_polygon(lid_top, mul(bin, 1.12), Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 255, 255, 120))));
        // 蓋子上的把手
        let k0 = rot(pos2(lc.x - 11.0, lc.y - 5.0), hinge, ang);
        let k1 = rot(pos2(lc.x + 11.0, lc.y - 5.0), hinge, ang);
        p.line_segment([k0, k1], Stroke::new(6.0, mul(bin, 0.86)));
        p.line_segment([k0 + vec2(0.0, -1.5), k1 + vec2(0.0, -1.5)], Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 255, 255, 110)));

        // 完成：勾勾彈出來，周圍噴幾點亮光
        if let Some(t0) = self.done_at {
            let s = (now - t0) as f32;
            let scale = (1.0 - (-7.0 * s).exp() * (13.0 * s).cos()).max(0.0);
            let c = pos2(cx - 58.0, top_y + 8.0);
            let rad = 20.0 * scale;
            if rad > 0.5 {
                p.circle_filled(c + vec2(2.0, 3.0), rad, Color32::from_rgba_unmultiplied(40, 120, 80, 40));
                p.circle_filled(c, rad, theme::GREEN);
                p.circle_filled(c + vec2(-rad * 0.25, -rad * 0.3), rad * 0.55, Color32::from_rgba_unmultiplied(255, 255, 255, 40));
                let pts = [c + vec2(-0.42, 0.02) * rad, c + vec2(-0.1, 0.34) * rad, c + vec2(0.45, -0.32) * rad];
                p.add(Shape::line(pts.to_vec(), Stroke::new(3.2 * scale.min(1.0), Color32::WHITE)));
            }
            if s < 0.9 {
                for i in 0..6 {
                    let a = i as f32 / 6.0 * std::f32::consts::TAU + 0.3;
                    let d = 24.0 + s * 46.0;
                    let alpha = ((0.9 - s) / 0.9 * 220.0) as u8;
                    p.circle_filled(c + vec2(a.cos(), a.sin()) * d, 2.6, Color32::from_rgba_unmultiplied(255, 200, 90, alpha));
                }
            }
        }

        if running || self.done_at.is_some_and(|t0| now - t0 < 1.2) || self.lid > 0.01 || !self.chips.is_empty() {
            ui.ctx().request_repaint();
        }
    }
}

/// 一張檔案小卡片：右上角摺起來，裡面兩條「文字」
fn chip(p: &Painter, c: Pos2, scale: f32, angle: f32, col: Color32) {
    let (w, h, f) = (15.0 * scale, 19.0 * scale, 5.0 * scale);
    let pts = [vec2(-w / 2.0, -h / 2.0), vec2(w / 2.0 - f, -h / 2.0), vec2(w / 2.0, -h / 2.0 + f), vec2(w / 2.0, h / 2.0), vec2(-w / 2.0, h / 2.0)];
    let poly: Vec<Pos2> = pts.iter().map(|&v| rot(c + v, c, angle)).collect();
    p.add(Shape::convex_polygon(poly, col, Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 150))));
    let fold = [vec2(w / 2.0 - f, -h / 2.0), vec2(w / 2.0 - f, -h / 2.0 + f), vec2(w / 2.0, -h / 2.0 + f)];
    p.add(Shape::convex_polygon(fold.iter().map(|&v| rot(c + v, c, angle)).collect(), mul(col, 1.3), Stroke::NONE));
    for (y, len) in [(-1.0f32, 0.55f32), (3.5, 0.4)] {
        let a = rot(c + vec2(-w * 0.3, y * scale), c, angle);
        let b = rot(c + vec2(-w * 0.3 + w * len, y * scale), c, angle);
        p.line_segment([a, b], Stroke::new(1.4 * scale, Color32::from_rgba_unmultiplied(255, 255, 255, 170)));
    }
}

/// 把很長的路徑縮成「開頭…結尾」，最多 max 個字元（保留檔名那一端比較多）。
pub fn elide_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 8 {
        return s.to_string();
    }
    let head = max / 3;
    let tail = max - head - 1;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}
