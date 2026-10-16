//! An emoji sent or received alone, as a message or a reaction, sends a big copy of it rising through the chat
//! panel, as the Node version's hearts.ts does (and Instagram used to, in chat): born over the emoji in its message, it grows
//! until halfway and rises, sliding towards the middle, until it's gone off the top. It's drawn in the terminal's own
//! characters, eighth and quarter blocks, in the colours of the emoji's picture (emoji.rs) blended with the
//! background, over everything else in the frame. The app keeps the ones in the air, draws them last in each frame,
//! and while there's any asks for a frame every `FRAME`.

use std::time::{Duration, Instant};

use image::{DynamicImage, RgbaImage};
use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};

/// How long one is in the air.
pub const LIFE: Duration = Duration::from_millis(4200);

/// Time between frames while any is in the air: 50 a second.
pub const FRAME: Duration = Duration::from_millis(20);

/// A colour as its red, green and blue.
pub type Rgb = [u8; 3];

/// Pixels per cell, quarters across and eighths down: in a cell twice as tall as it's wide they're square, and
/// they're what the eighth blocks (▁▂▃▄▅▆▇) and the quarter ones (▎▌▊) can draw.
const PW: i64 = 4;
const PH: i64 = 8;

/// The biggest height, in rows; the width follows the picture's proportion.
const MAX_H: i64 = 8;

/// The rows from `k` down, in a cell's mask, where bit r·4+c is the pixel at row r and column c.
const fn rows_from(k: u32) -> u32 {
    u32::MAX << (4 * k)
}

/// The columns left of `k`, on every row.
const fn cols_before(k: u32) -> u32 {
    0x1111_1111 * ((1 << k) - 1)
}

const FULL: u32 = u32::MAX;
const TOP: u32 = !rows_from(4);
const BOTTOM: u32 = rows_from(4);
const LEFT: u32 = cols_before(2);
const RIGHT: u32 = !LEFT;

/// The blocks a cell's 4×8 pixels are drawn with, each with the pixels it paints in its foreground colour: the
/// halves, the eighths down, the quarters across and the quadrants.
const BLOCKS: [(&str, u32); 24] = [
    ("█", FULL),
    ("▁", rows_from(7)),
    ("▂", rows_from(6)),
    ("▃", rows_from(5)),
    ("▄", rows_from(4)),
    ("▅", rows_from(3)),
    ("▆", rows_from(2)),
    ("▇", rows_from(1)),
    ("▔", !rows_from(1)),
    ("▀", TOP),
    ("▎", cols_before(1)),
    ("▌", LEFT),
    ("▊", cols_before(3)),
    ("▐", RIGHT),
    ("▘", TOP & LEFT),
    ("▝", TOP & RIGHT),
    ("▖", BOTTOM & LEFT),
    ("▗", BOTTOM & RIGHT),
    ("▚", TOP & LEFT | BOTTOM & RIGHT),
    ("▞", TOP & RIGHT | BOTTOM & LEFT),
    ("▛", !(BOTTOM & RIGHT)),
    ("▜", !(BOTTOM & LEFT)),
    ("▙", !(TOP & RIGHT)),
    ("▟", !(TOP & LEFT)),
];

/// A pixel of the picture at its drawn size: the colour of its ink alone, and how much of it the ink covers (0..1).
#[derive(Clone, Copy)]
struct Px {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

/// The picture at one height in pixels: its pixels row by row, None where there's no ink.
struct Shape {
    w: usize,
    h: usize,
    px: Vec<Option<Px>>,
}

/// One emoji in the air.
pub struct Animation {
    /// The picture cut down to its ink.
    glyph: RgbaImage,
    area: Rect,
    from: Position,
    start: Instant,
    /// The last size it was drawn at: it grows a pixel at a time, and then keeps its biggest.
    shape: Option<Shape>,
}

impl Animation {
    /// An emoji's rise over the chat panel `area` from `start`, out of its picture (emoji.rs), born on the cell
    /// `from`, the emoji's own in its message, or, with none, at the bottom of the panel in the middle. None when the
    /// picture has no ink. `area` and `from` are in the frame's own cells, as the buffer has them.
    pub fn new(picture: &DynamicImage, area: Rect, from: Option<Position>, start: Instant) -> Option<Self> {
        let glyph = trim(&picture.to_rgba8())?;
        let from = from.unwrap_or(Position::new(area.x + area.width / 2, area.y + area.height.saturating_sub(1)));
        Some(Animation { glyph, area, from, start, shape: None })
    }

    /// Whether its time is up at `now`.
    pub fn finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= LIFE
    }

    /// Draws its frame at `now` into `buf`, over whatever is there, only on the cells with ink, its edges blended
    /// into the terminal's background `bg`; true once it's finished, and then it draws nothing.
    pub fn draw(&mut self, buf: &mut Buffer, now: Instant, bg: Rgb) -> bool {
        if self.finished(now) {
            return true;
        }
        let Some(t) = now.checked_duration_since(self.start) else { return false };
        let p = t.as_secs_f64() / LIFE.as_secs_f64();
        let (area, from) = (self.area, self.from);
        // Grows all the while until halfway, then keeps its biggest size.
        let full = PH * MAX_H;
        let shape = self.shape(round(p * 2.0 * full as f64).clamp(2, full) as usize);
        let (h, w) = (shape.h as i64, shape.w as i64);
        // Rises from the message's row, its bottom starting over the emoji, until it's wholly off the top of the
        // panel, sliding meanwhile towards the middle; it never goes off the sides or the bottom. It moves a pixel
        // at a time both ways: the picture packs into cells starting from any of them.
        let (top, left, height, width) = (area.y as i64, area.x as i64, area.height as i64, area.width as i64);
        let (x, y) = (from.x as i64, from.y as i64);
        let travel = (y - top + MAX_H + 1) * PH;
        let top_px = ((top + height) * PH - h).min((y + 1) * PH - h - round(p * travel as f64));
        let ease = 1.0 - (1.0 - p) * (1.0 - p);
        let middle = left as f64 + width as f64 / 2.0;
        let cx = (x as f64 + 0.5 + (middle - x as f64 - 0.5) * ease) * PW as f64;
        let left_px = round(cx - w as f64 / 2.0).min((left + width) * PW - w).max(left * PW);

        let clip = area.intersection(buf.area);
        let (first_row, last_row) = (top_px.div_euclid(PH), (top_px + h - 1).div_euclid(PH));
        let (first_col, last_col) = (left_px.div_euclid(PW), (left_px + w - 1).div_euclid(PW));
        let rows = first_row.max(clip.top() as i64)..=last_row.min(clip.bottom() as i64 - 1);
        let cols = first_col.max(clip.left() as i64)..=last_col.min(clip.right() as i64 - 1);
        for cy in rows {
            for cx in cols.clone() {
                // The cell's 32 pixels, and the mask of those with ink.
                let mut px = [None; 32];
                let mut mask = 0u32;
                for r in 0..PH {
                    for c in 0..PW {
                        let (pr, pc) = (cy * PH + r - top_px, cx * PW + c - left_px);
                        if (0..h).contains(&pr)
                            && (0..w).contains(&pc)
                            && let Some(q) = shape.px[(pr * w + pc) as usize]
                        {
                            let i = (r * PW + c) as usize;
                            px[i] = Some(q);
                            mask |= 1 << i;
                        }
                    }
                }
                if mask == 0 {
                    continue;
                }
                // A full cell is ▀, its top half in the foreground and its bottom in the background, which gives the
                // colours half a cell's resolution; any other, the block that misses the fewest of its pixels.
                let (ch, block) = if mask == FULL { ("▀", TOP) } else { best_block(mask) };
                let Some(fore) = ink(&px, block, bg) else { continue };
                let back = ink(&px, !block, bg);
                let at = Position::new(cx as u16, cy as u16);
                // Over the second half of a wide character, the first is blanked: drawn whole, it hides this cell.
                if at.x > buf.area.x && buf[(at.x - 1, at.y)].cell_width() > 1 {
                    buf[(at.x - 1, at.y)].set_symbol(" ");
                }
                let cell = &mut buf[at];
                cell.set_symbol(ch).set_fg(rgb(fore));
                if let Some(back) = back {
                    cell.set_bg(rgb(back));
                }
                cell.modifier = Modifier::empty();
            }
        }
        false
    }

    /// The picture `h` pixels high, made again only when the height changes.
    fn shape(&mut self, h: usize) -> &Shape {
        if self.shape.as_ref().is_some_and(|s| s.h != h) {
            self.shape = None;
        }
        self.shape.get_or_insert_with(|| shape(&self.glyph, h))
    }
}

/// The picture cut down to the box of its ink (alpha above a hair), or None if it has none.
fn trim(img: &RgbaImage) -> Option<RgbaImage> {
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 8 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 < x0 || y1 < y0 {
        return None;
    }
    Some(image::imageops::crop_imm(img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image())
}

/// The picture `h` pixels high and wide in proportion, each pixel the average of the stretch of the picture it
/// covers: ink when that's at least half covered, in the colour of the ink alone, its opacity left to blend it into
/// the background with.
fn shape(g: &RgbaImage, h: usize) -> Shape {
    let (gw, gh) = (g.width() as usize, g.height() as usize);
    let w = round((h * gw) as f64 / gh as f64).max(1) as usize;
    let mut px = Vec::with_capacity(w * h);
    for r in 0..h {
        let (y0, y1) = (r * gh / h, ((r + 1) * gh / h).max(r * gh / h + 1));
        for c in 0..w {
            let (x0, x1) = (c * gw / w, ((c + 1) * gw / w).max(c * gw / w + 1));
            let (mut red, mut green, mut blue, mut alpha) = (0.0, 0.0, 0.0, 0.0);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let [pr, pg, pb, pa] = g.get_pixel(sx as u32, sy as u32).0;
                    let a = pa as f64 / 255.0;
                    red += pr as f64 * a;
                    green += pg as f64 * a;
                    blue += pb as f64 * a;
                    alpha += a;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f64;
            px.push((alpha / n >= 0.5).then(|| Px { r: red / alpha, g: green / alpha, b: blue / alpha, a: alpha / n }));
        }
    }
    Shape { w, h, px }
}

/// The block that misses the fewest of a cell's pixels with ink, `mask`. Those it leaves out are painted in the
/// cell's background, covering what was there; with none, the background stays. Pixels without ink inside the
/// block, or outside it when the background gets painted, are the misses; a tie goes to the block covering more ink.
fn best_block(mask: u32) -> (&'static str, u32) {
    let (mut best, mut best_miss, mut best_hit) = (BLOCKS[0], u32::MAX, 0);
    for b in BLOCKS {
        let outside = mask & !b.1;
        let miss = (b.1 & !mask).count_ones() + if outside != 0 { (!mask & !b.1).count_ones() } else { 0 };
        let hit = (b.1 & mask).count_ones();
        if miss < best_miss || (miss == best_miss && hit > best_hit) {
            (best, best_miss, best_hit) = (b, miss, hit);
        }
    }
    best
}

/// The colour of the cell's pixels in `mask`: their ink's average, weighted by opacity, blended into the
/// background `bg` by as much as their average opacity leaves out, which softens the edges. None when it has none.
fn ink(px: &[Option<Px>; 32], mask: u32, bg: Rgb) -> Option<Rgb> {
    let (mut r, mut g, mut b, mut a, mut n) = (0.0, 0.0, 0.0, 0.0, 0);
    for q in px.iter().enumerate().filter(|(i, _)| mask >> i & 1 == 1).filter_map(|(_, q)| *q) {
        r += q.r * q.a;
        g += q.g * q.a;
        b += q.b * q.a;
        a += q.a;
        n += 1;
    }
    (n > 0).then(|| mix([r / a, g / a, b / a], bg, 1.0 - a / n as f64))
}

/// From `a` to `b` in a straight line, `t` from 0 (only `a`) to 1 (only `b`), as the Node version's rainbow.ts mix.
fn mix(a: [f64; 3], b: Rgb, t: f64) -> Rgb {
    std::array::from_fn(|i| round(a[i] + (b[i] as f64 - a[i]) * t).clamp(0, 255) as u8)
}

/// Rounded as JavaScript rounds, halves upwards, so the pixels fall where wassup's do.
fn round(v: f64) -> i64 {
    (v + 0.5).floor() as i64
}

fn rgb([r, g, b]: Rgb) -> Color {
    Color::Rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Cell;

    const BG: Rgb = [16, 16, 16];

    /// An orange disc on nothing, a square picture as emoji.rs gives.
    fn disc() -> DynamicImage {
        let img = RgbaImage::from_fn(100, 100, |x, y| {
            let (dx, dy) = (x as f64 - 49.5, y as f64 - 49.5);
            if dx * dx + dy * dy <= 40.0 * 40.0 { image::Rgba([240, 120, 30, 255]) } else { image::Rgba([0, 0, 0, 0]) }
        });
        DynamicImage::ImageRgba8(img)
    }

    /// The cells drawn on, in a frame that was all "·".
    fn drawn(buf: &Buffer) -> Vec<Position> {
        buf.area.positions().filter(|&p| buf[p].symbol() != "·").collect()
    }

    /// Born on its emoji's cell as a sliver, at its full size in the panel halfway, gone off the top before its
    /// time's up and finished at 4.2 s, never drawing outside the panel.
    #[test]
    fn rises_within_the_panel() {
        let screen = Rect::new(0, 0, 40, 34);
        let area = Rect::new(2, 1, 30, 30);
        let start = Instant::now();
        let mut a = Animation::new(&disc(), area, Some(Position::new(16, 28)), start).unwrap();
        let frame = |a: &mut Animation, ms: u64| {
            let mut buf = Buffer::filled(screen, Cell::new("·"));
            let done = a.draw(&mut buf, start + Duration::from_millis(ms), BG);
            let cells = drawn(&buf);
            assert!(cells.iter().all(|&p| area.contains(p)), "drawn outside the panel at {ms} ms");
            (done, cells, buf)
        };

        let (done, cells, buf) = frame(&mut a, 0);
        assert!(!done);
        assert_eq!(cells, [Position::new(16, 28)]);
        assert_eq!(buf[(16, 28)].symbol(), "▂");

        // Halfway, at its biggest: 64 pixels across and down, sixteen columns by eight rows.
        let (done, cells, buf) = frame(&mut a, 2100);
        assert!(!done);
        let (xs, ys) = (cells.iter().map(|p| p.x), cells.iter().map(|p| p.y));
        assert_eq!((xs.clone().min(), xs.max()), (Some(9), Some(24)));
        assert_eq!((ys.clone().min(), ys.max()), (Some(3), Some(10)));
        let solid = &buf[(16, 6)];
        assert_eq!((solid.symbol(), solid.fg, solid.bg), ("▀", Color::Rgb(240, 120, 30), Color::Rgb(240, 120, 30)));

        let (done, cells, _) = frame(&mut a, 4190);
        assert!(!done && cells.is_empty());
        assert!(!a.finished(start + Duration::from_millis(4199)));
        let (done, cells, _) = frame(&mut a, 4200);
        assert!(done && cells.is_empty());
        assert!(a.finished(start + LIFE));
    }

    /// With no cell to be born on it starts at the bottom of the panel, in the middle; a picture with no ink has none.
    #[test]
    fn born_at_the_bottom_without_a_cell() {
        let area = Rect::new(0, 0, 20, 10);
        let start = Instant::now();
        let mut a = Animation::new(&disc(), area, None, start).unwrap();
        let mut buf = Buffer::filled(area, Cell::new("·"));
        assert!(!a.draw(&mut buf, start, BG));
        assert_eq!(drawn(&buf), [Position::new(10, 9)]);
        let blank = DynamicImage::ImageRgba8(RgbaImage::new(8, 8));
        assert!(Animation::new(&blank, area, None, start).is_none());
    }

    /// The ink's colour weighted by opacity, then blended towards the background by what the opacity leaves out.
    #[test]
    fn blending() {
        assert_eq!(mix([200.0, 0.0, 100.0], [0, 0, 0], 0.5), [100, 0, 50]);
        assert_eq!(mix([10.0, 20.0, 30.0], [255, 255, 255], 0.0), [10, 20, 30]);
        // Halves go up, as in JavaScript.
        assert_eq!(mix([0.0, 0.0, 0.0], [255, 255, 255], 0.5), [128, 128, 128]);

        let mut px = [None; 32];
        px[0] = Some(Px { r: 255.0, g: 0.0, b: 0.0, a: 1.0 });
        px[1] = Some(Px { r: 0.0, g: 0.0, b: 255.0, a: 0.5 });
        // Red 2/3 and blue 1/3 is (170, 0, 85); three quarters opaque, a quarter of white goes in.
        assert_eq!(ink(&px, FULL, [255, 255, 255]), Some([191, 64, 128]));
        assert_eq!(ink(&px, 1, [0, 0, 0]), Some([255, 0, 0]));
        assert_eq!(ink(&px, !3, [0, 0, 0]), None);
    }

    /// A pixel is ink when at least half of what it covers is; its colour is the ink's alone.
    #[test]
    fn shrinking_keeps_the_ink_colour() {
        let img = RgbaImage::from_fn(4, 2, |x, _| match x {
            0 => image::Rgba([0, 200, 0, 255]),
            1 => image::Rgba([0, 0, 0, 0]),
            2 => image::Rgba([0, 200, 0, 255]),
            _ => image::Rgba([0, 0, 0, 64]),
        });
        let s = shape(&img, 1);
        assert_eq!((s.w, s.h), (2, 1));
        let left = s.px[0].unwrap();
        assert_eq!((left.g, left.a), (200.0, 0.5));
        assert!(s.px[1].is_some_and(|p| (p.a - (1.0 + 64.0 / 255.0) / 2.0).abs() < 1e-9));
    }

    /// The masks are the pictures of their blocks, and each block's own pixels pick it.
    #[test]
    fn blocks() {
        let m = |f: &dyn Fn(u32, u32) -> bool| (0..32).filter(|i| f(i / 4, i % 4)).fold(0u32, |v, i| v | 1 << i);
        let want = [
            m(&|_, _| true),
            m(&|r, _| r >= 7),
            m(&|r, _| r >= 6),
            m(&|r, _| r >= 5),
            m(&|r, _| r >= 4),
            m(&|r, _| r >= 3),
            m(&|r, _| r >= 2),
            m(&|r, _| r >= 1),
            m(&|r, _| r < 1),
            m(&|r, _| r < 4),
            m(&|_, c| c < 1),
            m(&|_, c| c < 2),
            m(&|_, c| c < 3),
            m(&|_, c| c >= 2),
            m(&|r, c| r < 4 && c < 2),
            m(&|r, c| r < 4 && c >= 2),
            m(&|r, c| r >= 4 && c < 2),
            m(&|r, c| r >= 4 && c >= 2),
            m(&|r, c| (r < 4) == (c < 2)),
            m(&|r, c| (r < 4) != (c < 2)),
            m(&|r, c| !(r >= 4 && c >= 2)),
            m(&|r, c| !(r >= 4 && c < 2)),
            m(&|r, c| !(r < 4 && c >= 2)),
            m(&|r, c| !(r < 4 && c < 2)),
        ];
        for ((ch, mask), want) in BLOCKS.iter().zip(want) {
            assert_eq!(*mask, want, "{ch}");
            assert_eq!(best_block(*mask).0, *ch);
        }
        // The bottom row and half the one over it: ▂, missing two pixels, rather than ▁ and painting the background.
        assert_eq!(best_block(rows_from(7) | (rows_from(6) & LEFT)).0, "▂");
    }
}
