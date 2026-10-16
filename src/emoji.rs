//! Emoji as pictures, for those on their own in a message, drawn twice their size as wassup draws them (its
//! hearts.ts, emojiPng). The system's colour emoji font holds each one either as a PNG (its CBDT table, or sbix on
//! macOS), which is taken as it is, or as layers of vector shapes (its COLR table), which are painted here with
//! tiny-skia; a sequence (a skin tone, a flag, people joined) is put together by shaping it first.
//!
//! Where the system's font hasn't the emoji, or draws it without colour, it's drawn from the Noto Color Emoji that
//! comes with wassup, the vector one (`fonts/Noto-COLRv1.ttf` from googlefonts/noto-emoji, OFL-1.1, see
//! `fonts/LICENSE`), as the Node version did. It comes in the program (5 MB), so a `cargo install` has it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use image::{DynamicImage, GenericImageView, RgbaImage};
use rustybuzz::ttf_parser::colr::{ClipBox, ColorStop, CompositeMode, GradientExtend, Paint, Painter};
use rustybuzz::ttf_parser::{self, GlyphId, RgbaColor};
use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LinearGradient, Mask, PathBuilder, Pixmap, PixmapPaint, Point,
    RadialGradient, Rect, Shader, SpreadMode, SweepGradient, Transform,
};

/// Where colour emoji fonts are, on Linux and macOS.
const FONTS: [&str; 5] = [
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto-emoji/NotoColorEmoji.ttf",
    "/System/Library/Fonts/Apple Color Emoji.ttc",
];


/// Pixels per em COLR glyphs are painted at: the size of Noto Color Emoji's bitmaps, whose glyphs are 136 pixels
/// wide, so a painted emoji comes out about as big as one taken from a bitmap font.
const PPEM: f32 = 109.0;

/// The colour of text, for the rare COLR layer drawn in it rather than in one of the font's own.
const FOREGROUND: RgbaColor = RgbaColor { red: 0, green: 0, blue: 0, alpha: 255 };

/// The system's colour emoji font: the one fontconfig gives for "emoji" when it holds its pictures in colour (as
/// bitmaps or as COLR layers), else one of the usual places.
fn font() -> Option<&'static [u8]> {
    static FONT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    FONT.get_or_init(|| {
        let asked = std::process::Command::new("fc-match").args(["-f", "%{file}", "emoji:color=true"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let colour = |data: &Vec<u8>| {
            rustybuzz::ttf_parser::Face::parse(data, 0).is_ok_and(|f| f.tables().cbdt.is_some() || f.tables().sbix.is_some() || f.tables().colr.is_some())
        };
        asked.and_then(|p| std::fs::read(p).ok()).filter(colour).or_else(|| FONTS.iter().find_map(|p| std::fs::read(p).ok()))
    })
    .as_deref()
}

/// The Noto Color Emoji that comes with wassup, in the program.
fn bundled() -> Option<&'static [u8]> {
    Some(include_bytes!("../fonts/Noto-COLRv1.ttf"))
}

/// An emoji's picture: its glyph's, cut to its ink and centred in a square a little bigger, as wassup's; None when
/// neither the system's font nor the bundled one has it as one glyph. Made once each.
pub fn picture(emoji: &str) -> Option<Arc<DynamicImage>> {
    static MADE: OnceLock<Mutex<HashMap<String, Option<Arc<DynamicImage>>>>> = OnceLock::new();
    let made = MADE.get_or_init(Default::default);
    if let Some(p) = made.lock().unwrap().get(emoji) {
        return p.clone();
    }
    let p = render(font(), emoji).map(Arc::new);
    made.lock().unwrap().insert(emoji.to_string(), p.clone());
    p
}

/// The emoji from the system's font; from the bundled one when the system's hasn't it or draws it in grey (as a
/// monochrome font does), unless the bundled one hasn't it either.
fn render(system: Option<&[u8]>, emoji: &str) -> Option<DynamicImage> {
    let from = |font: &[u8]| draw(font, emoji).and_then(|img| trim_square(&img));
    let first = system.and_then(from);
    match first {
        Some(p) if colourful(&p) => Some(p),
        _ => bundled().and_then(from).or(first),
    }
}

/// The emoji drawn from a font: its bitmap if the font has one, else its COLR layers painted; None if the font
/// hasn't it as one glyph, or has it only as a plain outline.
fn draw(font: &[u8], emoji: &str) -> Option<RgbaImage> {
    let face = rustybuzz::Face::from_slice(font, 0)?;
    let glyph = glyph(&face, emoji)?;
    match face.glyph_raster_image(glyph, u16::MAX) {
        Some(raster) => Some(image::load_from_memory(raster.data).ok()?.to_rgba8()),
        None => colr(&face, glyph),
    }
}

/// The font's glyph for the emoji, shaped: None if it takes more than one, or the font hasn't it.
fn glyph(face: &rustybuzz::Face, emoji: &str) -> Option<GlyphId> {
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(emoji);
    let shaped = rustybuzz::shape(face, &[], buf);
    let [info] = shaped.glyph_infos() else { return None };
    (info.glyph_id != 0).then_some(GlyphId(info.glyph_id as u16))
}

/// Whether any of the picture's ink has colour, as wassup's colourful: a font without the emoji in colour draws it
/// all in grey.
fn colourful(img: &DynamicImage) -> bool {
    img.pixels().any(|(_, _, p)| {
        let [r, g, b, a] = p.0;
        a > 128 && u16::from(r.abs_diff(b)) + u16::from(g.abs_diff(b)) > 30
    })
}

/// The glyph's COLR layers painted at `PPEM`, on a picture as big as its outlines reach: a first walk through them
/// finds how far that is, a second paints. None if the glyph has none.
fn colr(face: &ttf_parser::Face, glyph: GlyphId) -> Option<RgbaImage> {
    if !face.is_color_glyph(glyph) {
        return None;
    }
    let scale = PPEM / f32::from(face.units_per_em());
    // Font units go up, pixels down.
    let to_pixels = Transform::from_scale(scale, -scale);
    let mut measure = Colr::new(face, to_pixels, None);
    face.paint_color_glyph(glyph, 0, FOREGROUND, &mut measure)?;
    let reach = measure.reach?.round_out()?;
    let (w, h) = (reach.width() + 2, reach.height() + 2);
    // A glyph reaching that far is a broken one.
    if w.max(h) as f32 > 8.0 * PPEM {
        return None;
    }
    let to_pixmap = to_pixels.post_translate(1.0 - reach.left() as f32, 1.0 - reach.top() as f32);
    let mut painter = Colr::new(face, to_pixmap, Some(Pixmap::new(w, h)?));
    face.paint_color_glyph(glyph, 0, FOREGROUND, &mut painter)?;
    let (pixmap, _) = painter.layers.into_iter().next()?;
    RgbaImage::from_raw(w, h, pixmap.take_demultiplied())
}

/// Paints a COLR glyph as ttf-parser walks it: version 0's outlines, each filled in a colour, or version 1's graph
/// of paints, clips, transforms and layers blended into each other. Given no pixmap, it only measures how far the
/// outlines reach.
struct Colr<'f, 'a> {
    face: &'f ttf_parser::Face<'a>,
    /// Font units to pixels, where the walk is, and the ones it was before each transform it went into.
    transform: Transform,
    transforms: Vec<Transform>,
    /// The last outline given, in pixels, and whether a paint is to fill it: in version 0 a paint fills the outline
    /// before it; in version 1 the outline becomes a clip, and a paint fills all the clip leaves.
    outline: Option<tiny_skia::Path>,
    fill_outline: bool,
    /// The clips gone into, each narrowing the one before it; the last is the one in force.
    clips: Vec<Mask>,
    /// The glyph's pixmap, then the layers gone into, each with the mode to blend it into the one below with.
    layers: Vec<(Pixmap, BlendMode)>,
    /// How far the outlines reach, in pixels, when measuring.
    reach: Option<Rect>,
}

impl<'f, 'a> Colr<'f, 'a> {
    fn new(face: &'f ttf_parser::Face<'a>, transform: Transform, pixmap: Option<Pixmap>) -> Self {
        let layers = pixmap.map(|p| (p, BlendMode::SourceOver)).into_iter().collect();
        Colr { face, transform, transforms: Vec::new(), outline: None, fill_outline: false, clips: Vec::new(), layers, reach: None }
    }

    /// Narrows the clip to a path, in pixels; to nothing, if there's none.
    fn clip(&mut self, path: Option<&tiny_skia::Path>) {
        let Some((pixmap, _)) = self.layers.first() else { return };
        let mask = match (self.clips.last(), path) {
            (Some(clip), Some(path)) => {
                let mut mask = clip.clone();
                mask.intersect_path(path, FillRule::Winding, true, Transform::identity());
                Some(mask)
            }
            (None, Some(path)) => Mask::new(pixmap.width(), pixmap.height()).map(|mut mask| {
                mask.fill_path(path, FillRule::Winding, true, Transform::identity());
                mask
            }),
            (_, None) => Mask::new(pixmap.width(), pixmap.height()),
        };
        self.clips.extend(mask);
    }
}

impl<'a> Painter<'a> for Colr<'_, 'a> {
    fn outline_glyph(&mut self, glyph: GlyphId) {
        let mut outline = Outline(PathBuilder::new());
        self.face.outline_glyph(glyph, &mut outline);
        self.outline = outline.0.finish().and_then(|p| p.transform(self.transform));
        self.fill_outline = true;
        if let (true, Some(path)) = (self.layers.is_empty(), &self.outline) {
            let bounds = path.bounds();
            self.reach = Some(self.reach.and_then(|r| r.join(&bounds)).unwrap_or(bounds));
        }
    }

    fn paint(&mut self, paint: Paint<'a>) {
        let Some((pixmap, _)) = self.layers.last_mut() else { return };
        let Some(shader) = shader(&paint, self.transform) else { return };
        let fill = tiny_skia::Paint { shader, ..tiny_skia::Paint::default() };
        let clip = self.clips.last();
        if self.fill_outline {
            if let Some(path) = &self.outline {
                pixmap.fill_path(path, &fill, FillRule::Winding, Transform::identity(), clip);
            }
        } else if let Some(all) = Rect::from_xywh(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32) {
            pixmap.fill_rect(all, &fill, Transform::identity(), clip);
        }
    }

    fn push_clip(&mut self) {
        self.fill_outline = false;
        let outline = self.outline.take();
        self.clip(outline.as_ref());
        self.outline = outline;
    }

    fn push_clip_box(&mut self, b: ClipBox) {
        let path = Rect::from_ltrb(b.x_min, b.y_min, b.x_max, b.y_max).and_then(|r| PathBuilder::from_rect(r).transform(self.transform));
        self.clip(path.as_ref());
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_layer(&mut self, mode: CompositeMode) {
        let Some((pixmap, _)) = self.layers.first() else { return };
        if let Some(layer) = Pixmap::new(pixmap.width(), pixmap.height()) {
            self.layers.push((layer, blend(mode)));
        }
    }

    fn pop_layer(&mut self) {
        if self.layers.len() > 1
            && let Some((layer, mode)) = self.layers.pop()
            && let Some((below, _)) = self.layers.last_mut()
        {
            let paint = PixmapPaint { blend_mode: mode, ..PixmapPaint::default() };
            below.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
        }
    }

    fn push_transform(&mut self, t: ttf_parser::Transform) {
        self.transforms.push(self.transform);
        self.transform = self.transform.pre_concat(Transform::from_row(t.a, t.b, t.c, t.d, t.e, t.f));
    }

    fn pop_transform(&mut self) {
        if let Some(t) = self.transforms.pop() {
            self.transform = t;
        }
    }
}

/// A glyph's outline as ttf-parser gives it, gathered into a tiny-skia path, in font units.
struct Outline(PathBuilder);

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}

/// A COLR paint as a tiny-skia shader, `transform` taking its coordinates (font units) to pixels. tiny-skia has all
/// three of COLR's gradients, Skia's: linear, two-point conical (radial) and sweep.
fn shader(paint: &Paint<'_>, transform: Transform) -> Option<Shader<'static>> {
    let spread = |extend| match extend {
        GradientExtend::Pad => SpreadMode::Pad,
        GradientExtend::Repeat => SpreadMode::Repeat,
        GradientExtend::Reflect => SpreadMode::Reflect,
    };
    match paint {
        Paint::Solid(c) => Some(Shader::SolidColor(colour(*c))),
        Paint::LinearGradient(g) => {
            let (stops, lo, hi) = colour_line(g.stops(0, &[]))?;
            // The colours run from p0 towards p1, but square to p0→p2, along which each colour stays the same: p1
            // brought onto the line through p0 at a right angle to p0→p2.
            let (nx, ny) = (g.y2 - g.y0, g.x0 - g.x2);
            let n = nx * nx + ny * ny;
            let (x1, y1) = if n > 0.0 {
                let k = ((g.x1 - g.x0) * nx + (g.y1 - g.y0) * ny) / n;
                (g.x0 + k * nx, g.y0 + k * ny)
            } else {
                (g.x1, g.y1)
            };
            let at = |t: f32| Point::from_xy(g.x0 + (x1 - g.x0) * t, g.y0 + (y1 - g.y0) * t);
            LinearGradient::new(at(lo), at(hi), stops, spread(g.extend), transform)
        }
        Paint::RadialGradient(g) => {
            let (stops, lo, hi) = colour_line(g.stops(0, &[]))?;
            let at = |t: f32| (Point::from_xy(g.x0 + (g.x1 - g.x0) * t, g.y0 + (g.y1 - g.y0) * t), (g.r0 + (g.r1 - g.r0) * t).max(0.0));
            let ((p0, r0), (p1, r1)) = (at(lo), at(hi));
            RadialGradient::new(p0, r0, p1, r1, stops, spread(g.extend), transform)
        }
        Paint::SweepGradient(g) => {
            // tiny-skia wants the angles in increasing order: one going the other way is the same sweep with them
            // swapped and its colour line turned round.
            let back = g.end_angle < g.start_angle;
            let (start, end) = if back { (g.end_angle, g.start_angle) } else { (g.start_angle, g.end_angle) };
            let line = g.stops(0, &[]).map(|s| if back { ColorStop { stop_offset: 1.0 - s.stop_offset, ..s } } else { s });
            let (stops, lo, hi) = colour_line(line)?;
            // Half turns from 180°, as OpenType 1.9.1 has them (Skia and skrifa read them so), in degrees.
            let at = |t: f32| (start + (end - start) * t) * 180.0 + 180.0;
            SweepGradient::new(Point::from_xy(g.center_x, g.center_y), at(lo), at(hi), stops, spread(g.extend), transform)
        }
    }
}

/// A colour line's stops in order and spread over 0..1, as tiny-skia has them, and where on the line the first and
/// the last were: the gradient is stretched to match, as Skia does, since COLR's may lie anywhere. None if it has
/// none.
fn colour_line(line: impl Iterator<Item = ColorStop>) -> Option<(Vec<GradientStop>, f32, f32)> {
    let mut line: Vec<ColorStop> = line.collect();
    line.sort_by(|a, b| a.stop_offset.total_cmp(&b.stop_offset));
    let lo = line.first()?.stop_offset;
    let span = line.last()?.stop_offset - lo;
    let span = if span > 0.0 { span } else { 1.0 };
    let stops = line.iter().map(|s| GradientStop::new((s.stop_offset - lo) / span, colour(s.color))).collect();
    Some((stops, lo, lo + span))
}

/// A COLR colour as tiny-skia's.
fn colour(c: RgbaColor) -> Color {
    Color::from_rgba8(c.red, c.green, c.blue, c.alpha)
}

/// A COLR composite mode as tiny-skia's blend mode: both are Skia's, one for one.
fn blend(mode: CompositeMode) -> BlendMode {
    match mode {
        CompositeMode::Clear => BlendMode::Clear,
        CompositeMode::Source => BlendMode::Source,
        CompositeMode::Destination => BlendMode::Destination,
        CompositeMode::SourceOver => BlendMode::SourceOver,
        CompositeMode::DestinationOver => BlendMode::DestinationOver,
        CompositeMode::SourceIn => BlendMode::SourceIn,
        CompositeMode::DestinationIn => BlendMode::DestinationIn,
        CompositeMode::SourceOut => BlendMode::SourceOut,
        CompositeMode::DestinationOut => BlendMode::DestinationOut,
        CompositeMode::SourceAtop => BlendMode::SourceAtop,
        CompositeMode::DestinationAtop => BlendMode::DestinationAtop,
        CompositeMode::Xor => BlendMode::Xor,
        CompositeMode::Plus => BlendMode::Plus,
        CompositeMode::Screen => BlendMode::Screen,
        CompositeMode::Overlay => BlendMode::Overlay,
        CompositeMode::Darken => BlendMode::Darken,
        CompositeMode::Lighten => BlendMode::Lighten,
        CompositeMode::ColorDodge => BlendMode::ColorDodge,
        CompositeMode::ColorBurn => BlendMode::ColorBurn,
        CompositeMode::HardLight => BlendMode::HardLight,
        CompositeMode::SoftLight => BlendMode::SoftLight,
        CompositeMode::Difference => BlendMode::Difference,
        CompositeMode::Exclusion => BlendMode::Exclusion,
        CompositeMode::Multiply => BlendMode::Multiply,
        CompositeMode::Hue => BlendMode::Hue,
        CompositeMode::Saturation => BlendMode::Saturation,
        CompositeMode::Color => BlendMode::Color,
        CompositeMode::Luminosity => BlendMode::Luminosity,
    }
}

/// The picture cut down to the box of its ink (alpha above a hair), in a square 8% bigger, centred.
fn trim_square(img: &RgbaImage) -> Option<DynamicImage> {
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
    if x1 < x0 {
        return None;
    }
    let ink = DynamicImage::ImageRgba8(img.clone()).crop_imm(x0, y0, x1 - x0 + 1, y1 - y0 + 1);
    let size = ((ink.width().max(ink.height()) as f32) * 1.08).round() as u32;
    let mut square = RgbaImage::new(size, size);
    let (ox, oy) = ((size - ink.width()) / 2, (size - ink.height()) / 2);
    for (x, y, p) in ink.pixels() {
        square.put_pixel(x + ox, y + oy, p);
    }
    Some(DynamicImage::ImageRgba8(square))
}

#[cfg(test)]
mod tests {
    use super::{bundled, colourful, colr, draw, font, glyph, picture, render, trim_square};

    /// The emoji the tests draw: a plain one, one with its variation selector, one with a skin tone and a flag.
    const EMOJI: [&str; 4] = ["😀", "❤️", "👍🏽", "🇵🇹"];

    fn bundled_font() -> &'static [u8] {
        bundled().expect("fonts/Noto-COLRv1.ttf isn't there")
    }

    /// Each one glyph, a square.
    #[test]
    fn sequences_come_out_whole() {
        if font().is_none() {
            return;
        }
        for e in EMOJI {
            let p = picture(e).unwrap_or_else(|| panic!("{e} has no picture"));
            assert_eq!(p.width(), p.height(), "{e} isn't square");
            assert!(p.width() > 64, "{e} is too small: {}", p.width());
        }
    }

    /// With no colour font on the system, they come from the bundled one, in colour.
    #[test]
    fn bundled_font_draws_in_colour() {
        bundled_font();
        for e in EMOJI {
            let p = render(None, e).unwrap_or_else(|| panic!("{e} has no picture"));
            assert!(colourful(&p), "{e} came out grey");
            assert_eq!(p.width(), p.height(), "{e} isn't square");
            assert!(p.width() > 64, "{e} is too small: {}", p.width());
        }
    }

    /// Painted from their COLR layers, bitmaps or not, they come out in colour and about as big as the system's
    /// bitmaps of them, where it has any.
    #[test]
    fn colr_painted_when_forced() {
        let face = rustybuzz::Face::from_slice(bundled_font(), 0).expect("the bundled font doesn't parse");
        for e in EMOJI {
            let g = glyph(&face, e).unwrap_or_else(|| panic!("{e} isn't one glyph"));
            let img = colr(&face, g).unwrap_or_else(|| panic!("{e} has no COLR layers"));
            let p = trim_square(&img).unwrap_or_else(|| panic!("{e} came out empty"));
            assert!(colourful(&p), "{e} came out grey");
            assert_eq!(p.width(), p.height(), "{e} isn't square");
            assert!(p.width() > 64, "{e} is too small: {}", p.width());
            if let Some(bitmap) = font().and_then(|f| draw(f, e)).and_then(|img| trim_square(&img)) {
                let ratio = p.width() as f32 / bitmap.width() as f32;
                assert!((0.75..=1.33).contains(&ratio), "{e} painted is {} wide, its bitmap {}", p.width(), bitmap.width());
            }
        }
    }
}
