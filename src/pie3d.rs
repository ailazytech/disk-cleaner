//! 霧面 3D 甜甜圈圖：每塊之間有縫、越大塊越高、滑鼠移上去的那塊會升起來，地面有柔和陰影。

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use eframe::egui::{self, Align2, Color32, FontId, Mesh, Painter, Pos2, Shape, Stroke, Vec2, pos2, vec2};

use crate::theme::{tint, white};

pub struct Slice {
    /// 0..1 的比例（會再正規化）
    pub frac: f32,
    pub color: Color32,
    /// 「可用空間」這類：用霧白色塊表示
    pub clear: bool,
}

pub struct Geometry {
    pub center: Pos2,
    pub rx: f32,
    pub ry: f32,
    /// 內圈半徑比例
    pub inner: f32,
    /// 每塊的起訖角度（已扣掉縫隙）
    pub spans: Vec<(f32, f32)>,
    /// 每塊的高度
    pub heights: Vec<f32>,
}

const LIFT: f32 = 14.0;
const TILT: f32 = 0.5;

fn steps(a0: f32, a1: f32) -> Vec<f32> {
    let n = (((a1 - a0) / (TAU / 180.0)).ceil() as usize).max(1);
    (0..=n).map(|i| a0 + (a1 - a0) * i as f32 / n as f32).collect()
}

fn clip(a0: f32, a1: f32, b0: f32, b1: f32) -> Option<(f32, f32)> {
    let s = a0.max(b0);
    let e = a1.min(b1);
    (e > s + 1e-4).then_some((s, e))
}

/// sin > 0 的部分（前半圈，看得到外牆）
fn front(a0: f32, a1: f32) -> Vec<(f32, f32)> {
    [clip(a0, a1, 0.0, PI), clip(a0, a1, TAU, TAU + PI)].into_iter().flatten().collect()
}

/// sin < 0 的部分（後半圈，透過中間的洞看得到內牆）
fn back(a0: f32, a1: f32) -> Vec<(f32, f32)> {
    [clip(a0, a1, -PI, 0.0), clip(a0, a1, PI, TAU)].into_iter().flatten().collect()
}

pub fn geometry(rect: egui::Rect, slices: &[Slice]) -> Geometry {
    let rx = (rect.width() / 2.0 - 16.0).min((rect.height() - 70.0) / (2.0 * TILT + 0.32)).max(30.0);
    let ry = rx * TILT;
    let base_h = rx * 0.17;
    let center = pos2(rect.center().x, rect.center().y + base_h * 0.55);
    let total: f32 = slices.iter().map(|s| s.frac.max(0.0)).sum::<f32>().max(1e-6);
    let max = slices.iter().map(|s| s.frac.max(0.0) / total).fold(0.0f32, f32::max).max(1e-6);
    let gap = (5.0 / rx).min(0.08);
    let mut a = -FRAC_PI_2;
    let mut spans = Vec::with_capacity(slices.len());
    let mut heights = Vec::with_capacity(slices.len());
    for s in slices {
        let f = s.frac.max(0.0) / total;
        let span = f * TAU;
        let g = if span > gap * 2.5 { gap / 2.0 } else { 0.0 };
        spans.push((a + g, a + span - g));
        heights.push(base_h * (0.72 + 0.6 * (f / max).sqrt()));
        a += span;
    }
    Geometry { center, rx, ry, inner: 0.46, spans, heights }
}

impl Geometry {
    fn pt(&self, a: f32, k: f32, h: f32) -> Pos2 {
        pos2(self.center.x + self.rx * k * a.cos(), self.center.y + self.ry * k * a.sin() - h)
    }
}

/// 滑鼠在哪一塊上（頂面或前側外牆），由前往後測試。
pub fn hit(g: &Geometry, p: Pos2) -> Option<usize> {
    let mut order: Vec<usize> = (0..g.spans.len()).filter(|&i| g.spans[i].1 - g.spans[i].0 > 1e-4).collect();
    order.sort_by(|&a, &b| depth_key(g, b).partial_cmp(&depth_key(g, a)).unwrap_or(std::cmp::Ordering::Equal));
    let norm = |mut a: f32| {
        if a < -FRAC_PI_2 {
            a += TAU;
        }
        a
    };
    for i in order {
        let (a0, a1) = g.spans[i];
        let h = g.heights[i];
        // 頂面
        let dx = (p.x - g.center.x) / g.rx;
        let dy = (p.y - (g.center.y - h)) / g.ry;
        let r2 = dx * dx + dy * dy;
        if r2 <= 1.0 && r2 >= g.inner * g.inner {
            let a = norm(dy.atan2(dx));
            if a >= a0 && a < a1 {
                return Some(i);
            }
        }
        // 前側外牆
        if dx.abs() <= 1.0 {
            let s = (1.0 - dx * dx).sqrt();
            let y_top = g.center.y - h + g.ry * s;
            let y_bot = g.center.y + g.ry * s;
            if p.y >= y_top && p.y <= y_bot {
                let a = dx.clamp(-1.0, 1.0).acos();
                if a >= a0 && a < a1 {
                    return Some(i);
                }
            }
        }
    }
    None
}

fn depth_key(g: &Geometry, i: usize) -> f32 {
    let (a0, a1) = g.spans[i];
    // 這塊最靠前的點（sin 最大）決定前後
    steps(a0, a1).iter().map(|a| a.sin()).fold(-2.0, f32::max)
}

/// 霧面明暗：外牆依法線與光線方向（右前上方）
fn lambert(a: f32) -> f32 {
    let (nx, nz) = (a.cos(), a.sin());
    let (lx, lz) = (0.55, 0.83);
    0.66 + 0.3 * (nx * lx + nz * lz).max(0.0)
}

fn mul(c: Color32, f: f32) -> Color32 {
    let m = |v: u8| ((v as f32 * f).clamp(0.0, 255.0)) as u8;
    Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
}

pub fn draw(painter: &Painter, g: &Geometry, slices: &[Slice], hover: Option<usize>, labels: bool) {
    let n = slices.len().min(g.spans.len());
    let heights: Vec<f32> = (0..n).map(|i| g.heights[i] + if Some(i) == hover { LIFT } else { 0.0 }).collect();
    let base = |i: usize| -> Color32 {
        if slices[i].clear { Color32::from_rgb(226, 230, 246) } else { slices[i].color }
    };

    // 1. 地面陰影（光從右上來，影子往左下）
    let sc = g.center + vec2(-g.rx * 0.1, g.ry * 0.16);
    for (grow, a) in [(1.16, 10u8), (1.08, 14), (1.0, 18), (0.9, 18)] {
        painter.add(Shape::ellipse_filled(sc, vec2(g.rx * grow, g.ry * grow), Color32::from_rgba_unmultiplied(40, 50, 100, a)));
    }

    let mut order: Vec<usize> = (0..n).filter(|&i| g.spans[i].1 - g.spans[i].0 > 1e-4).collect();
    order.sort_by(|&a, &b| depth_key(g, a).partial_cmp(&depth_key(g, b)).unwrap_or(std::cmp::Ordering::Equal));

    // 洞的地板：淺色，只留一點環狀陰影
    painter.add(Shape::ellipse_filled(g.center, vec2(g.rx * g.inner, g.ry * g.inner), Color32::from_rgb(236, 239, 248)));
    painter.add(Shape::ellipse_stroke(
        g.center,
        vec2(g.rx * g.inner - 3.0, g.ry * g.inner - 1.5),
        Stroke::new(5.0, Color32::from_rgba_unmultiplied(40, 50, 100, 16)),
    ));

    // 2. 內牆（透過中間的洞看到的後半圈）
    for &i in &order {
        let (a0, a1) = g.spans[i];
        let h = heights[i];
        let c = base(i);
        for (b0, b1) in back(a0, a1) {
            let mut m = Mesh::default();
            let angs = steps(b0, b1);
            for &a in &angs {
                let shade = 0.6 + 0.18 * (-a.cos()).max(0.0);
                m.colored_vertex(g.pt(a, g.inner, h), mul(c, shade + 0.08));
                m.colored_vertex(g.pt(a, g.inner, 0.0), mul(c, shade - 0.06));
            }
            quad_strip(&mut m, angs.len());
            painter.add(Shape::mesh(m));
        }
    }

    // 3. 每一塊：端面 → 前側外牆 → 頂面，由後往前畫
    for &i in &order {
        let (a0, a1) = g.spans[i];
        let h = heights[i];
        let c = base(i);
        let mut m = Mesh::default();
        // 端面（面向觀看者才畫）
        for (a, visible) in [(a0, a0.cos() < 0.0), (a1, a1.cos() > 0.0)] {
            if !visible {
                continue;
            }
            let s = m.vertices.len() as u32;
            let col_top = mul(c, 0.86);
            let col_bot = mul(c, 0.74);
            m.colored_vertex(g.pt(a, g.inner, h), col_top);
            m.colored_vertex(g.pt(a, 1.0, h), col_top);
            m.colored_vertex(g.pt(a, 1.0, 0.0), col_bot);
            m.colored_vertex(g.pt(a, g.inner, 0.0), col_bot);
            m.add_triangle(s, s + 1, s + 2);
            m.add_triangle(s, s + 2, s + 3);
        }
        // 前側外牆
        for (f0, f1) in front(a0, a1) {
            let angs = steps(f0, f1);
            let s = m.vertices.len() as u32;
            for &a in &angs {
                let l = lambert(a);
                m.colored_vertex(g.pt(a, 1.0, h), mul(c, l + 0.05));
                m.colored_vertex(g.pt(a, 1.0, 0.0), mul(c, l - 0.1));
            }
            for k in 0..angs.len() as u32 - 1 {
                let v = s + k * 2;
                m.add_triangle(v, v + 1, v + 2);
                m.add_triangle(v + 1, v + 3, v + 2);
            }
        }
        // 頂面（霧面：外緣稍亮）
        let angs = steps(a0, a1);
        let s = m.vertices.len() as u32;
        for &a in &angs {
            m.colored_vertex(g.pt(a, 1.0, h), tint(c, 0.14));
            m.colored_vertex(g.pt(a, g.inner, h), tint(c, 0.04));
        }
        for k in 0..angs.len() as u32 - 1 {
            let v = s + k * 2;
            m.add_triangle(v, v + 1, v + 2);
            m.add_triangle(v + 1, v + 3, v + 2);
        }
        painter.add(Shape::mesh(m));

        // 圓潤感：頂面外緣一道柔光、內緣淡淡的光
        let outer: Vec<Pos2> = angs.iter().map(|&a| g.pt(a, 1.0, h)).collect();
        painter.add(Shape::line(outer, Stroke::new(1.6, white(150))));
        let inner: Vec<Pos2> = angs.iter().map(|&a| g.pt(a, g.inner, h)).collect();
        painter.add(Shape::line(inner, Stroke::new(1.0, white(80))));
        if Some(i) == hover {
            let mut edge: Vec<Pos2> = angs.iter().map(|&a| g.pt(a, 1.0, h)).collect();
            edge.extend(angs.iter().rev().map(|&a| g.pt(a, g.inner, h)));
            painter.add(Shape::closed_line(edge, Stroke::new(1.8, white(230))));
        }
    }

    // 4. 百分比
    if labels {
        for &i in &order {
            let (a0, a1) = g.spans[i];
            let frac = (a1 - a0) / TAU;
            if frac < 0.035 {
                continue;
            }
            let mid = (a0 + a1) / 2.0;
            let p = g.pt(mid, (1.0 + g.inner) / 2.0, heights[i]);
            let txt = format!("{:.0}%", frac * 100.0);
            let font = FontId::proportional(if frac > 0.12 { 16.5 } else { 13.0 });
            let ink = if slices[i].clear { Color32::from_rgb(90, 100, 140) } else { Color32::WHITE };
            if !slices[i].clear {
                painter.text(p + Vec2::new(0.0, 1.0), Align2::CENTER_CENTER, &txt, font.clone(), Color32::from_rgba_unmultiplied(20, 30, 70, 90));
            }
            painter.text(p, Align2::CENTER_CENTER, &txt, font, ink);
        }
    }
}

fn quad_strip(m: &mut Mesh, n: usize) {
    for k in 0..n as u32 - 1 {
        let v = k * 2;
        m.add_triangle(v, v + 1, v + 2);
        m.add_triangle(v + 1, v + 3, v + 2);
    }
}
