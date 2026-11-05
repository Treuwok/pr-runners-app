//! 選單列／系統匣小圖：依 `Summary` 即時畫成 RGBA。純函式，不碰系統 API，所以測得到。
//!
//! 版面（macOS 選單列，高 18pt）：自家機器格子（≤ 6 台一台一格，實心＝在忙；更多時改成比例條＋`忙/在線`）→
//! `+N`（正在跑的臨時機器）→ 排隊小牌。沒有 runner 時只畫一個空心格。資料過時時整張半透明。
//! 數字用 Inter（OFL 授權，檔案與授權一起放在 `icons/tray/`），不打包 SF Pro。

// macOS 用 render_menubar、其他平台用 render_square：各平台只會用到其中一個
#![cfg_attr(not(test), allow(dead_code))]

use ab_glyph::{Font, FontRef, ScaleFont};
use pr_runners_core::view::{Severity, Summary};
use std::sync::OnceLock;
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

/// 排隊太久時小牌的顏色（唯一的顏色）。
pub const WARN_RGB: (u8, u8, u8) = (0xFF, 0x9F, 0x0A);
/// 自家機器超過這麼多台，格子改成比例條。
const MAX_CELLS: u32 = 6;
const BAR_HEIGHT_PT: f32 = 18.0;
const STALE_ALPHA: f32 = 0.5;

/// 怎麼上色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// 黑色＋alpha，交給 macOS 依選單列深淺自動配色（`template: true`）。
    /// 排隊太久時用退回方案：小牌實心＋「!」，不用顏色。
    Template,
    /// 淺色選單列：畫黑色；排隊太久時小牌是橘色。
    Light,
    /// 深色選單列：畫白色；排隊太久時小牌是橘色。
    Dark,
}

pub struct TrayArt {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 是不是 macOS 樣板圖（只看 alpha）。
    pub template: bool,
}

static FONT: OnceLock<FontRef<'static>> = OnceLock::new();

fn font() -> &'static FontRef<'static> {
    FONT.get_or_init(|| {
        FontRef::try_from_slice(include_bytes!("../icons/tray/Inter-SemiBold.ttf"))
            .expect("內建的 Inter 字型")
    })
}

/// 以「em 的像素高」換成 ab_glyph 的 `PxScale`（它的單位是 ascent－descent 的高度）。
fn px_scale(em_px: f32) -> ab_glyph::PxScale {
    let f = font();
    let ratio = f.height_unscaled() / f.units_per_em().unwrap_or(1000.0);
    ab_glyph::PxScale::from(em_px * ratio)
}

fn text_width(text: &str, em_px: f32) -> f32 {
    let scaled = font().as_scaled(px_scale(em_px));
    text.chars()
        .map(|c| scaled.h_advance(scaled.glyph_id(c)))
        .sum()
}

#[derive(Clone, Copy)]
struct Rgba(u8, u8, u8, u8);

/// 在 premultiplied 的 pixmap 上用 src-over 疊一個像素。
fn blend(pm: &mut Pixmap, x: i32, y: i32, color: Rgba, coverage: f32) {
    if x < 0 || y < 0 || x >= pm.width() as i32 || y >= pm.height() as i32 {
        return;
    }
    let sa = (f32::from(color.3) / 255.0) * coverage.clamp(0.0, 1.0);
    let idx = (y as usize * pm.width() as usize + x as usize) * 4;
    let d = &mut pm.data_mut()[idx..idx + 4];
    let inv = 1.0 - sa;
    d[0] = (f32::from(color.0) * sa + f32::from(d[0]) * inv).round() as u8;
    d[1] = (f32::from(color.1) * sa + f32::from(d[1]) * inv).round() as u8;
    d[2] = (f32::from(color.2) * sa + f32::from(d[2]) * inv).round() as u8;
    d[3] = (255.0 * sa + f32::from(d[3]) * inv).round() as u8;
}

/// 把已有像素挖空（樣板圖裡「實心小牌上的字」只能靠 alpha 表示）。
fn knock_out(pm: &mut Pixmap, x: i32, y: i32, coverage: f32) {
    if x < 0 || y < 0 || x >= pm.width() as i32 || y >= pm.height() as i32 {
        return;
    }
    let idx = (y as usize * pm.width() as usize + x as usize) * 4;
    let keep = 1.0 - coverage.clamp(0.0, 1.0);
    for b in &mut pm.data_mut()[idx..idx + 4] {
        *b = (f32::from(*b) * keep).round() as u8;
    }
}

#[derive(Clone, Copy)]
enum TextMode {
    Paint(Rgba),
    KnockOut,
}

/// 文字畫在垂直置中的位置；`x` 是文字左緣。
fn draw_text(pm: &mut Pixmap, text: &str, x: f32, em_px: f32, mode: TextMode) {
    let scaled = font().as_scaled(px_scale(em_px));
    let baseline = pm.height() as f32 / 2.0 + 0.364 * em_px;
    let mut cx = x;
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        let glyph = id.with_scale_and_position(px_scale(em_px), ab_glyph::point(cx, baseline));
        if let Some(og) = font().outline_glyph(glyph) {
            let b = og.px_bounds();
            og.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                match mode {
                    TextMode::Paint(color) => blend(pm, px, py, color, cov),
                    TextMode::KnockOut => knock_out(pm, px, py, cov),
                }
            });
        }
        cx += scaled.h_advance(id);
    }
}

fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.0, c.1, c.2, c.3);
    p.anti_alias = true;
    p
}

fn fill(pm: &mut Pixmap, path: &Path, c: Rgba) {
    pm.fill_path(
        path,
        &paint(c),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn stroke(pm: &mut Pixmap, path: &Path, c: Rgba, width: f32) {
    let s = Stroke {
        width,
        ..Stroke::default()
    };
    pm.stroke_path(path, &paint(c), &s, Transform::identity(), None);
}

/// 一格（邊長 `size`）：實心＝在忙，空心＝閒置。
fn draw_cell(pm: &mut Pixmap, x: f32, y: f32, size: f32, stroke_w: f32, filled: bool, c: Rgba) {
    let half = stroke_w / 2.0;
    if let Some(p) = rrect(
        x + half,
        y + half,
        size - stroke_w,
        size - stroke_w,
        size * 0.25,
    ) {
        if filled {
            fill(pm, &p, c);
        }
        stroke(pm, &p, c, stroke_w);
    }
}

enum Item {
    Cells {
        online: u32,
        busy: u32,
    },
    /// 空心單格（沒有 runner、或只有離線的自家機器）。
    Placeholder,
    Bar {
        busy: u32,
        online: u32,
    },
    Text(String),
    Badge {
        text: String,
        solid: bool,
    },
}

struct Layout {
    s: f32,
}

impl Layout {
    fn cell(&self) -> f32 {
        12.0 * self.s
    }
    fn gap(&self) -> f32 {
        4.0 * self.s
    }
    fn em(&self) -> f32 {
        10.5 * self.s
    }
    fn width_of(&self, item: &Item) -> f32 {
        match item {
            Item::Cells { online, .. } => {
                *online as f32 * self.cell() + online.saturating_sub(1) as f32 * 2.0 * self.s
            }
            Item::Placeholder => self.cell(),
            Item::Bar { busy, online } => {
                26.0 * self.s + 3.0 * self.s + text_width(&format!("{busy}/{online}"), self.em())
            }
            Item::Text(t) => text_width(t, self.em()),
            Item::Badge { text, .. } => {
                (text_width(text, self.em()) + 8.0 * self.s).max(self.cell() + 2.0 * self.s)
            }
        }
    }
}

fn items_for(s: &Summary) -> Vec<Item> {
    let mut items = Vec::new();
    if s.own_online == 0 {
        // 沒有任何在線的自家機器：只有臨時機器在跑時不畫格子；否則（沒有 runner、只剩離線）畫一個空心格
        if s.pool_busy == 0 {
            items.push(Item::Placeholder);
        }
    } else if s.own_online <= MAX_CELLS {
        items.push(Item::Cells {
            online: s.own_online,
            busy: s.own_busy.min(s.own_online),
        });
    } else {
        items.push(Item::Bar {
            busy: s.own_busy.min(s.own_online),
            online: s.own_online,
        });
    }
    if s.pool_busy > 0 {
        items.push(Item::Text(format!("+{}", s.pool_busy)));
    }
    if s.waiting > 0 {
        let text = if s.waiting > 99 {
            "99+".to_string()
        } else {
            s.waiting.to_string()
        };
        items.push(Item::Badge { text, solid: false });
    }
    items
}

fn finish(pm: Pixmap, template: bool, stale: bool) -> TrayArt {
    let (width, height) = (pm.width(), pm.height());
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for px in pm.pixels() {
        let c = px.demultiply();
        let a = if stale {
            (f32::from(c.alpha()) * STALE_ALPHA) as u8
        } else {
            c.alpha()
        };
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), a]);
    }
    TrayArt {
        rgba,
        width,
        height,
        template,
    }
}

fn ink_color(ink: Ink) -> Rgba {
    match ink {
        Ink::Template | Ink::Light => Rgba(0, 0, 0, 255),
        Ink::Dark => Rgba(255, 255, 255, 255),
    }
}

/// macOS 選單列的圖：高度 `18 * scale`，寬度依內容。
pub fn render_menubar(s: &Summary, ink: Ink, scale: f32) -> TrayArt {
    let lay = Layout { s: scale };
    let height = (BAR_HEIGHT_PT * scale).round().max(1.0);
    let mut items = items_for(s);
    let warn = s.attention == Severity::Warn;
    // 排隊太久：Template 退回「實心小牌＋!」；有顏色時橘色小牌
    if warn && let Some(Item::Badge { text, solid }) = items.last_mut() {
        *solid = true;
        if ink == Ink::Template {
            *text = "!".into();
        }
    }
    let pad = scale;
    let total: f32 = items.iter().map(|i| lay.width_of(i)).sum::<f32>()
        + lay.gap() * items.len().saturating_sub(1) as f32
        + 2.0 * pad;
    // 只有一個空心格（沒有 runner）：做成正方形，格子置中
    let lone_cell = matches!(items.as_slice(), [Item::Placeholder]);
    let width = if lone_cell {
        height
    } else {
        total.ceil().max(1.0)
    };
    let mut pm = Pixmap::new(width as u32, height as u32).expect("non-empty pixmap");
    let color = ink_color(ink);
    let (cell, stroke_w) = (lay.cell(), 1.5 * scale);
    let cy = (height - cell) / 2.0;
    let mut x = if lone_cell { (width - cell) / 2.0 } else { pad };
    for item in &items {
        match item {
            Item::Cells { online, busy } => {
                for i in 0..*online {
                    draw_cell(&mut pm, x, cy, cell, stroke_w, i < *busy, color);
                    x += cell + 2.0 * scale;
                }
                x -= 2.0 * scale;
            }
            Item::Placeholder => {
                draw_cell(&mut pm, x, cy, cell, stroke_w, false, color);
                x += cell;
            }
            Item::Bar { busy, online } => {
                let (bw, bh) = (26.0 * scale, 8.0 * scale);
                let by = (height - bh) / 2.0;
                if let Some(p) = rrect(
                    x + 0.6 * scale,
                    by + 0.6 * scale,
                    bw - 1.2 * scale,
                    bh - 1.2 * scale,
                    2.0 * scale,
                ) {
                    stroke(&mut pm, &p, color, 1.2 * scale);
                }
                let fw = (bw - 2.4 * scale) * *busy as f32 / (*online).max(1) as f32;
                if fw > 0.5
                    && let Some(p) = rrect(
                        x + 1.2 * scale,
                        by + 1.2 * scale,
                        fw,
                        bh - 2.4 * scale,
                        1.2 * scale,
                    )
                {
                    fill(&mut pm, &p, color);
                }
                let t = format!("{busy}/{online}");
                draw_text(
                    &mut pm,
                    &t,
                    x + bw + 3.0 * scale,
                    lay.em(),
                    TextMode::Paint(color),
                );
                x += lay.width_of(item);
            }
            Item::Text(t) => {
                draw_text(&mut pm, t, x, lay.em(), TextMode::Paint(color));
                x += lay.width_of(item);
            }
            Item::Badge { text, solid } => {
                let bw = lay.width_of(item);
                let bh = cell + 2.0 * scale;
                let by = (height - bh) / 2.0;
                let tx = x + (bw - text_width(text, lay.em())) / 2.0;
                let orange = Rgba(WARN_RGB.0, WARN_RGB.1, WARN_RGB.2, 255);
                if let Some(p) = rrect(x, by, bw, bh, 4.0 * scale) {
                    match (*solid, ink) {
                        // 橘色小牌（有顏色時）：橘底、深色字
                        (true, Ink::Light | Ink::Dark) => {
                            fill(&mut pm, &p, orange);
                            draw_text(
                                &mut pm,
                                text,
                                tx,
                                lay.em(),
                                TextMode::Paint(Rgba(0x1C, 0x1C, 0x1E, 255)),
                            );
                        }
                        // 樣板圖的退回方案：實心＋挖空的「!」
                        (true, Ink::Template) => {
                            fill(&mut pm, &p, color);
                            draw_text(&mut pm, text, tx, lay.em(), TextMode::KnockOut);
                        }
                        (false, _) => {
                            stroke(&mut pm, &p, color, 1.2 * scale);
                            draw_text(&mut pm, text, tx, lay.em(), TextMode::Paint(color));
                        }
                    }
                }
                x += bw;
            }
        }
        x += lay.gap();
    }
    finish(pm, ink == Ink::Template, s.stale)
}

/// Windows 系統匣：只能放正方形圖。一個格子，有人在忙時實心；排隊太久時右下角一個橘點。
/// 用中灰色畫（工作列有淺有深，灰色兩邊都看得到）；數字放在 tooltip。
pub fn render_square(s: &Summary, scale: f32) -> TrayArt {
    let size = (16.0 * scale).round().max(1.0);
    let mut pm = Pixmap::new(size as u32, size as u32).expect("non-empty pixmap");
    let gray = Rgba(0x8E, 0x8E, 0x93, 255);
    let cell = 11.0 * scale;
    let off = (size - cell) / 2.0 - 0.5 * scale;
    draw_cell(
        &mut pm,
        off,
        off,
        cell,
        1.6 * scale,
        s.own_busy + s.pool_busy > 0,
        gray,
    );
    if s.attention == Severity::Warn {
        let r = 3.2 * scale;
        let mut pb = PathBuilder::new();
        pb.push_circle(size - r - 0.5 * scale, size - r - 0.5 * scale, r);
        if let Some(p) = pb.finish() {
            fill(&mut pm, &p, Rgba(WARN_RGB.0, WARN_RGB.1, WARN_RGB.2, 255));
        }
    }
    finish(pm, false, s.stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> Summary {
        Summary {
            own_busy: 0,
            own_online: 0,
            own_offline: 0,
            pool_busy: 0,
            waiting: 0,
            attention: Severity::Ok,
            stale: false,
        }
    }

    fn px(art: &TrayArt) -> impl Iterator<Item = [u8; 4]> + '_ {
        art.rgba.as_chunks::<4>().0.iter().copied()
    }

    #[test]
    fn no_runners_is_a_single_hollow_cell() {
        let a = render_menubar(&summary(), Ink::Template, 2.0);
        assert_eq!(a.width, a.height, "只有一格");
        assert_eq!(a.height, 36);
        assert!(px(&a).any(|p| p[3] > 0), "有畫東西");
    }

    #[test]
    fn pool_count_and_queue_badge_widen_the_image() {
        let cells_only = Summary {
            own_online: 4,
            own_busy: 2,
            ..summary()
        };
        let full = Summary {
            own_online: 4,
            own_busy: 2,
            pool_busy: 9,
            waiting: 1,
            ..summary()
        };
        let a = render_menubar(&cells_only, Ink::Template, 2.0);
        let b = render_menubar(&full, Ink::Template, 2.0);
        assert!(b.width > a.width);
        assert!(b.template);
    }

    #[test]
    fn a_long_queue_is_orange_only_when_the_ink_allows_colour() {
        let warn = Summary {
            own_online: 2,
            own_busy: 1,
            waiting: 3,
            attention: Severity::Warn,
            ..summary()
        };
        let t = render_menubar(&warn, Ink::Template, 2.0);
        assert!(t.template);
        assert!(
            px(&t).all(|p| p[0] == p[1] && p[1] == p[2]),
            "樣板圖不能有顏色，退回「實心＋!」"
        );
        let d = render_menubar(&warn, Ink::Dark, 2.0);
        assert!(!d.template);
        assert!(
            px(&d).any(|p| p == [0xFF, 0x9F, 0x0A, 255]),
            "深色選單列：橘色小牌"
        );
        let l = render_menubar(&warn, Ink::Light, 2.0);
        assert!(
            px(&l).any(|p| p == [0xFF, 0x9F, 0x0A, 255]),
            "淺色選單列：橘色小牌"
        );
    }

    #[test]
    fn the_template_fallback_differs_from_the_normal_badge() {
        let ok = Summary {
            own_online: 2,
            own_busy: 1,
            waiting: 3,
            ..summary()
        };
        let warn = Summary {
            attention: Severity::Warn,
            ..ok.clone()
        };
        let a = render_menubar(&ok, Ink::Template, 2.0);
        let b = render_menubar(&warn, Ink::Template, 2.0);
        assert_ne!(a.rgba, b.rgba, "排隊太久要看得出來（實心小牌）");
    }

    #[test]
    fn two_digit_counts_stay_within_120pt() {
        let big = Summary {
            own_online: 12,
            own_busy: 11,
            pool_busy: 10,
            waiting: 12,
            attention: Severity::Warn,
            ..summary()
        };
        for ink in [Ink::Template, Ink::Light, Ink::Dark] {
            for scale in [1.0, 2.0] {
                let a = render_menubar(&big, ink, scale);
                assert!(
                    a.width as f32 <= 120.0 * scale,
                    "{} > {}",
                    a.width,
                    120.0 * scale
                );
            }
        }
    }

    #[test]
    fn stale_data_is_drawn_translucent() {
        let s = Summary {
            own_online: 3,
            own_busy: 3,
            pool_busy: 2,
            waiting: 1,
            stale: true,
            ..summary()
        };
        let a = render_menubar(&s, Ink::Template, 2.0);
        assert!(px(&a).filter(|p| p[3] > 0).all(|p| p[3] <= 128));
        let fresh = render_menubar(&Summary { stale: false, ..s }, Ink::Template, 2.0);
        assert!(px(&fresh).any(|p| p[3] > 200), "沒過時時有不透明的像素");
    }

    #[test]
    fn the_square_icon_is_16_points_and_marks_trouble_with_an_orange_dot() {
        let a = render_square(&summary(), 2.0);
        assert_eq!((a.width, a.height), (32, 32));
        let warn = Summary {
            own_busy: 1,
            own_online: 1,
            attention: Severity::Warn,
            ..summary()
        };
        let b = render_square(&warn, 1.0);
        assert_eq!((b.width, b.height), (16, 16));
        assert!(px(&b).any(|p| p == [0xFF, 0x9F, 0x0A, 255]));
        assert!(!px(&a).any(|p| p == [0xFF, 0x9F, 0x0A, 255]));
    }

    /// 人工看圖：`cargo test -p pr-runners-app tray_icon -- --ignored` 把幾種狀態輸出成 PNG
    /// 到 `target/tray-preview/`（淺底、深底各一張；樣板圖在深底上反白，模擬 macOS 的配色）。
    #[test]
    #[ignore = "輸出預覽圖給人看，不是斷言"]
    fn write_previews() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tray-preview");
        std::fs::create_dir_all(&dir).unwrap();
        let cases: Vec<(&str, Summary)> = vec![
            ("empty", summary()),
            (
                "idle",
                Summary {
                    own_online: 4,
                    ..summary()
                },
            ),
            (
                "busy-queue",
                Summary {
                    own_online: 4,
                    own_busy: 2,
                    pool_busy: 9,
                    waiting: 1,
                    ..summary()
                },
            ),
            (
                "queue-too-long",
                Summary {
                    own_online: 4,
                    own_busy: 2,
                    pool_busy: 9,
                    waiting: 3,
                    attention: Severity::Warn,
                    ..summary()
                },
            ),
            (
                "many",
                Summary {
                    own_online: 12,
                    own_busy: 11,
                    pool_busy: 10,
                    waiting: 12,
                    ..summary()
                },
            ),
            (
                "stale",
                Summary {
                    own_online: 4,
                    own_busy: 2,
                    stale: true,
                    ..summary()
                },
            ),
        ];
        for (name, s) in &cases {
            for (bg, bg_name, inks) in [
                ((245u8, 245u8, 247u8), "light", [Ink::Template, Ink::Light]),
                ((38, 38, 40), "dark", [Ink::Template, Ink::Dark]),
            ] {
                for ink in inks {
                    let art = render_menubar(s, ink, 4.0);
                    let mut pm = Pixmap::new(art.width + 40, art.height + 40).unwrap();
                    for p in pm.data_mut().as_chunks_mut::<4>().0 {
                        p.copy_from_slice(&[bg.0, bg.1, bg.2, 255]);
                    }
                    for (i, c) in art.rgba.as_chunks::<4>().0.iter().enumerate() {
                        let (x, y) = (i as u32 % art.width + 20, i as u32 / art.width + 20);
                        // 樣板圖在深底上要反白
                        let src = if art.template && bg_name == "dark" {
                            [255, 255, 255, c[3]]
                        } else {
                            [c[0], c[1], c[2], c[3]]
                        };
                        let a = f32::from(src[3]) / 255.0;
                        let idx = ((y * pm.width() + x) * 4) as usize;
                        let d = &mut pm.data_mut()[idx..idx + 4];
                        for k in 0..3 {
                            d[k] = (f32::from(src[k]) * a + f32::from(d[k]) * (1.0 - a)) as u8;
                        }
                    }
                    pm.save_png(dir.join(format!("{name}-{bg_name}-{ink:?}.png")))
                        .unwrap();
                }
            }
        }
    }
}
