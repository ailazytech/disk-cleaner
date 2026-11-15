//! 產生 App 圖示：圓角方塊 + treemap 色塊 + 「乾淨」的閃光。
//! Generates the app icon: rounded tile + treemap blocks + a "clean" sparkle.
//!
//! cargo run --example gen_icon
//!
//! 輸出 / Outputs:
//!   assets/icon.ico        多尺寸圖示（嵌入 exe）/ multi-size icon embedded into the exe
//!   assets/icon-256.png    README 用 / for the README
//!   assets/icon-128.rgba   視窗圖示的原始 RGBA / raw RGBA for the window icon

use std::path::Path;

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, SpreadMode, Transform,
};

/// 設計稿以 256×256 為座標系
const DESIGN: f32 = 256.0;

fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> tiny_skia::Path {
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();
    pb.finish().unwrap()
}

/// 四角閃光（凹邊星形）
fn sparkle(cx: f32, cy: f32, r: f32) -> tiny_skia::Path {
    let mut pb = PathBuilder::new();
    pb.move_to(cx, cy - r);
    pb.quad_to(cx + r * 0.12, cy - r * 0.12, cx + r, cy);
    pb.quad_to(cx + r * 0.12, cy + r * 0.12, cx, cy + r);
    pb.quad_to(cx - r * 0.12, cy + r * 0.12, cx - r, cy);
    pb.quad_to(cx - r * 0.12, cy - r * 0.12, cx, cy - r);
    pb.close();
    pb.finish().unwrap()
}

fn solid(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, a);
    p.anti_alias = true;
    p
}

fn render(size: u32) -> Pixmap {
    let mut pm = Pixmap::new(size, size).unwrap();
    let s = size as f32 / DESIGN;
    let t = Transform::from_scale(s, s);
    // 小尺寸時留少一點邊、間隙加寬，避免糊成一片
    let small = size <= 24;
    let (m, gap, rad) = if small { (8.0, 10.0, 36.0) } else { (12.0, 7.0, 52.0) };

    // 底：深藍漸層圓角方塊
    let bg = rounded_rect(m, m, DESIGN - m, DESIGN - m, rad);
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.shader = LinearGradient::new(
        Point::from_xy(0.0, m),
        Point::from_xy(0.0, DESIGN - m),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(44, 62, 102, 255)),
            GradientStop::new(1.0, Color::from_rgba8(18, 26, 46, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    pm.fill_path(&bg, &paint, FillRule::Winding, t, None);

    // treemap 色塊（程式裡的檔案類型配色）
    let (a0, a1) = if small { (40.0, 216.0) } else { (44.0, 212.0) };
    let w = a1 - a0;
    let split_x = a0 + w * 0.54;
    let split_y = a0 + w * 0.46;
    let split_y2 = a0 + w * 0.73;
    let split_x2 = split_x + (a1 - split_x) * 0.5;
    let r = if small { 8.0 } else { 10.0 };
    let tiles: Vec<([f32; 4], [u8; 3])> = if small {
        vec![
            ([a0, a0, split_x - gap / 2.0, a1], [91, 143, 249]),
            ([split_x + gap / 2.0, a0, a1, split_y - gap / 2.0], [255, 157, 77]),
            ([split_x + gap / 2.0, split_y + gap / 2.0, a1, a1], [90, 216, 166]),
        ]
    } else {
        vec![
            ([a0, a0, split_x - gap / 2.0, a1], [91, 143, 249]),
            ([split_x + gap / 2.0, a0, a1, split_y - gap / 2.0], [255, 157, 77]),
            ([split_x + gap / 2.0, split_y + gap / 2.0, a1, split_y2 - gap / 2.0], [90, 216, 166]),
            ([split_x + gap / 2.0, split_y2 + gap / 2.0, split_x2 - gap / 2.0, a1], [246, 189, 22]),
            ([split_x2 + gap / 2.0, split_y2 + gap / 2.0, a1, a1], [232, 104, 74]),
        ]
    };
    for ([x0, y0, x1, y1], [cr, cg, cb]) in tiles {
        let path = rounded_rect(x0, y0, x1, y1, r);
        pm.fill_path(&path, &solid(cr, cg, cb, 255), FillRule::Winding, t, None);
        // 上緣一點亮光，增加立體感
        if !small {
            let hl = rounded_rect(x0, y0, x1, (y0 + 14.0).min(y1), r);
            pm.fill_path(&hl, &solid(255, 255, 255, 38), FillRule::Winding, t, None);
        }
    }

    // 閃光：先畫深色外框再畫白色，壓在藍色塊上也看得清楚
    let (cx, cy, sr) = if small { (88.0, 100.0, 60.0) } else { (92.0, 104.0, 58.0) };
    pm.fill_path(&sparkle(cx, cy, sr + 10.0), &solid(18, 26, 46, 150), FillRule::Winding, t, None);
    pm.fill_path(&sparkle(cx, cy, sr), &solid(255, 255, 255, 255), FillRule::Winding, t, None);
    if !small {
        pm.fill_path(&sparkle(176.0, 70.0, 22.0), &solid(18, 26, 46, 120), FillRule::Winding, t, None);
        pm.fill_path(&sparkle(176.0, 70.0, 17.0), &solid(255, 255, 255, 240), FillRule::Winding, t, None);
    }
    pm
}

fn rgba(pm: &Pixmap) -> Vec<u8> {
    pm.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    std::fs::create_dir_all(&dir).unwrap();

    // ICO：每個尺寸都存成 PNG 區塊（Windows Vista 起支援）
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| render(s).encode_png().unwrap()).collect();
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, png) in sizes.iter().zip(&pngs) {
        let d = if *s >= 256 { 0 } else { *s as u8 };
        ico.extend_from_slice(&[d, d, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &pngs {
        ico.extend_from_slice(png);
    }
    std::fs::write(dir.join("icon.ico"), ico).unwrap();
    std::fs::write(dir.join("icon-256.png"), &pngs[sizes.len() - 1]).unwrap();
    std::fs::write(dir.join("icon-128.rgba"), rgba(&render(128))).unwrap();
    // 預覽用的小尺寸並排圖（不提交）/ preview strip (not committed)
    let mut strip = Pixmap::new(16 + 32 + 48 + 64 + 256 + 60, 256).unwrap();
    let mut x = 0;
    for s in [16u32, 32, 48, 64, 256] {
        let p = render(s);
        strip.draw_pixmap(x, 0, p.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
        x += s as i32 + 12;
    }
    std::fs::write(std::env::temp_dir().join("dc_icon_preview.png"), strip.encode_png().unwrap()).unwrap();
    println!("icon written to {}", dir.display());
}
