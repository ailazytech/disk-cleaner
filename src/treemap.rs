//! Squarified treemap 版面配置（Bruls、Huizing、van Wijk）。

use eframe::egui::{Pos2, Rect, pos2};

use crate::tree::{NONE, Tree, flag};

#[derive(Clone, Copy)]
pub struct Tile {
    pub rect: Rect,
    pub node: u32,
    pub depth: u16,
    /// 資料夾是否有空間畫標題列
    pub header: bool,
}

pub const HEADER_H: f32 = 15.0;
const MIN_AREA: f64 = 9.0;

/// 把 values（已由大到小排序）依面積比例切進 rect。
pub fn squarify(values: &[f64], rect: Rect, out: &mut Vec<Rect>) {
    let n = values.len();
    let total: f64 = values.iter().sum();
    let (mut x, mut y) = (rect.min.x as f64, rect.min.y as f64);
    let (mut w, mut h) = (rect.width() as f64, rect.height() as f64);
    if n == 0 || total <= 0.0 || w <= 0.0 || h <= 0.0 {
        out.extend(std::iter::repeat_n(Rect::NOTHING, n));
        return;
    }
    let scale = (w * h) / total;
    let worst = |max: f64, min: f64, sum: f64, short: f64| {
        let s2 = sum * sum;
        let w2 = short * short;
        (w2 * max / s2).max(s2 / (w2 * min))
    };
    let mut i = 0;
    while i < n {
        let short = w.min(h).max(1e-9);
        let first = values[i] * scale;
        let mut end = i + 1;
        let mut sum = first;
        let mut best = worst(first, first, sum, short);
        while end < n {
            let a = values[end] * scale;
            if a <= 0.0 {
                break;
            }
            let s2 = sum + a;
            let w2 = worst(first, a, s2, short);
            if w2 > best {
                break;
            }
            best = w2;
            sum = s2;
            end += 1;
        }
        if w >= h {
            // 左側直條
            let cw = if h > 0.0 { sum / h } else { 0.0 };
            let mut cy = y;
            for v in &values[i..end] {
                let ch = if cw > 0.0 { v * scale / cw } else { 0.0 };
                out.push(Rect::from_min_max(pos2(x as f32, cy as f32), pos2((x + cw) as f32, (cy + ch) as f32)));
                cy += ch;
            }
            x += cw;
            w -= cw;
        } else {
            // 上方橫條
            let ch = if w > 0.0 { sum / w } else { 0.0 };
            let mut cx = x;
            for v in &values[i..end] {
                let cw = if ch > 0.0 { v * scale / ch } else { 0.0 };
                out.push(Rect::from_min_max(pos2(cx as f32, y as f32), pos2((cx + cw) as f32, (y + ch) as f32)));
                cx += cw;
            }
            y += ch;
            h -= ch;
        }
        i = end;
    }
    // 數值為 0 的項目
    while out.len() < n {
        out.push(Rect::NOTHING);
    }
}

pub fn layout(tree: &Tree, root: u32, rect: Rect, budget: usize) -> Vec<Tile> {
    let mut out = Vec::with_capacity(4096);
    lay(tree, root, rect, 0, &mut out, budget);
    out
}

fn lay(tree: &Tree, node: u32, rect: Rect, depth: u16, out: &mut Vec<Tile>, budget: usize) {
    let n = &tree.nodes[node as usize];
    let is_dir = n.is_dir();
    let header = is_dir && depth > 0 && rect.height() > HEADER_H * 2.2 && rect.width() > 48.0;
    out.push(Tile { rect, node, depth, header });
    if !is_dir || n.child_count == 0 || n.size == 0 || out.len() >= budget {
        return;
    }
    let pad = if depth == 0 { 0.0 } else { 1.5 };
    let mut inner = rect.shrink(pad);
    if header {
        inner.min.y += HEADER_H - pad;
    }
    if inner.width() < 3.0 || inner.height() < 3.0 {
        return;
    }
    let area = (inner.width() * inner.height()) as f64;
    let total = n.size as f64;
    let mut kids: Vec<(u32, f64)> = n
        .children()
        .filter(|&c| {
            let cn = &tree.nodes[c as usize];
            cn.size > 0 && cn.flags & flag::DELETED == 0
        })
        .map(|c| (c, tree.nodes[c as usize].size as f64))
        .collect();
    kids.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    // 面積太小的合併成「其餘」，保持比例
    let mut shown = 0usize;
    let mut sum = 0f64;
    for (_, s) in &kids {
        if s / total * area < MIN_AREA {
            break;
        }
        shown += 1;
        sum += s;
    }
    kids.truncate(shown);
    let rest = total - sum;
    let mut values: Vec<f64> = kids.iter().map(|k| k.1).collect();
    if rest > 0.0 {
        values.push(rest);
    }
    let mut rects = Vec::with_capacity(values.len());
    squarify(&values, inner, &mut rects);
    for (i, (c, _)) in kids.iter().enumerate() {
        if out.len() >= budget {
            return;
        }
        let r = rects[i];
        if r.width() >= 1.0 && r.height() >= 1.0 {
            lay(tree, *c, r, depth + 1, out, budget);
        }
    }
}

/// 找出滑鼠下最深的方塊。
pub fn hit(tiles: &[Tile], p: Pos2) -> Option<u32> {
    tiles.iter().rev().find(|t| t.rect.contains(p)).map(|t| t.node).filter(|&n| n != NONE)
}
