//! `<canvas>` 2D drawing, in pixels, in Rust: what Canard draws when a page
//! uses a 2D context. Pages call the context in `prelude.js`, which queues
//! commands; [`Context::run`] (in `script.rs`) carries them out here when the
//! page reads pixels back or a snapshot is taken. The canvas is premultiplied
//! RGBA, the size its `width` and `height` attributes say, and layout gives it
//! a box like an image's (its `src` is `canvas:N`).

pub mod path;

use crate::fonts::Font;
use crate::style::{Color, Family};
use path::{Cap, Join, Mask, Matrix, Path, StrokeStyle, Sub, IDENTITY};
use std::sync::{Arc, OnceLock};

/// A canvas's pixels, as the app and the reftest rasteriser draw them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA, row by row.
    pub pixels: Arc<Vec<u8>>,
}

/// A picture to draw: premultiplied RGBA.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl Image {
    /// From straight (not premultiplied) RGBA, as decoders give it.
    /// The picture's pixels unpremultiplied (straight RGBA).
    pub fn straight(&self) -> Vec<u8> {
        let mut out = self.rgba.to_vec();
        for p in out.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        out
    }

    pub fn from_straight(width: u32, height: u32, rgba: &[u8]) -> Image {
        let mut out = rgba.to_vec();
        for p in out.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a < 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
            }
        }
        Image { width, height, rgba: Arc::new(out) }
    }
}

/// The `src` layout gives a canvas's box.
pub fn src_for(node: u32) -> String {
    format!("canvas:{node}")
}

// ------------------------------------------------------------------ text

/// Text drawn into coverage: `alpha` is `width` x `height`, and its top left
/// is at (`left`, `top`) from the pen on the baseline, in the font's pixels.
pub struct TextMask {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
    pub left: f32,
    pub top: f32,
}

/// Fonts for canvas text: the app gives the system's; tests use the approximate widths.
pub trait TextHost: Send + Sync {
    fn width(&self, text: &str, font: &Font) -> f32;
    fn ascent(&self, font: &Font) -> f32 {
        font.size * 0.8
    }
    fn descent(&self, font: &Font) -> f32 {
        font.size * 0.2
    }
    /// The text as coverage, or None to draw the stand-in marks.
    fn mask(&self, _text: &str, _font: &Font) -> Option<TextMask> {
        None
    }
}

static TEXT: OnceLock<Box<dyn TextHost>> = OnceLock::new();

/// The app's fonts for canvas text (set once, at launch).
pub fn set_text_host(h: Box<dyn TextHost>) {
    let _ = TEXT.set(h);
}

struct Approx;
impl TextHost for Approx {
    fn width(&self, text: &str, font: &Font) -> f32 {
        crate::Fonts::width(&crate::ApproxFonts, text, font)
    }
}

pub fn text_host() -> &'static dyn TextHost {
    match TEXT.get() {
        Some(h) => h.as_ref(),
        None => &Approx,
    }
}

/// The width, ascent and descent of text in a font: from the font's own file when
/// Canard has it, else from the app's fonts.
pub fn text_metrics(text: &str, font: &Font) -> (f32, f32, f32) {
    if let Some(face) = crate::fonts::file_for(font) {
        let k = font.size / face.units_per_em as f32;
        let mut w = 0.0;
        let mut prev = None;
        for c in text.chars() {
            let g = face.glyph(c);
            if let Some(p) = prev {
                w += face.kerning(p, g) as f32;
            }
            w += face.advance(g) as f32;
            prev = Some(g);
        }
        return (w * k, face.ascent as f32 * k, -(face.descent as f32) * k);
    }
    let host = text_host();
    (host.width(text, font), host.ascent(font), host.descent(font))
}

/// Text as a path from the font file's outlines: `to` maps (pen x, y up) in px to device space.
fn glyph_path(face: &crate::sfnt::Face, text: &str, size: f32, to: impl Fn((f32, f32)) -> (f32, f32)) -> Path {
    let k = size / face.units_per_em as f32;
    let mut path = Path::default();
    let mut pen = 0.0f32;
    let mut prev = None;
    let mut cmds = Vec::new();
    for c in text.chars() {
        let g = face.glyph(c);
        if let Some(p) = prev {
            pen += face.kerning(p, g) as f32;
        }
        prev = Some(g);
        cmds.clear();
        face.outline(g, &mut cmds);
        let at = |x: f32, y: f32| to(((pen + x) * k, y * k));
        for cmd in &cmds {
            match *cmd {
                crate::sfnt::Cmd::Move(x, y) => path.move_dev(at(x, y)),
                crate::sfnt::Cmd::Line(x, y) => path.line_dev(at(x, y)),
                crate::sfnt::Cmd::Quad(cx, cy, x, y) => path.quad_dev(at(cx, cy), at(x, y)),
                crate::sfnt::Cmd::Close => path.close(),
            }
        }
        pen += face.advance(g) as f32;
    }
    path
}

/// Reads the CSS `font` shorthand the way canvases use it (`italic bold 14px Georgia, serif`).
pub fn parse_font(spec: &str) -> Option<Font> {
    let lower = spec.trim().to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let (k, size) = words.iter().enumerate().find_map(|(k, w)| {
        let w = w.split('/').next()?;
        let num = |suffix: &str, f: f32| w.strip_suffix(suffix).and_then(|n| n.parse::<f32>().ok()).map(|n| n * f);
        num("px", 1.0).or_else(|| num("pt", 4.0 / 3.0)).or_else(|| num("rem", 16.0)).or_else(|| num("em", 10.0)).or_else(|| num("%", 0.1)).map(|s| (k, s))
    })?;
    let family_list = words.get(k + 1..).map(|w| w.join(" ")).unwrap_or_default();
    let family_list = if words[k].contains('/') || !words.get(k + 1).is_some_and(|w| w.starts_with('/')) {
        family_list
    } else {
        words.get(k + 2..).map(|w| w.join(" ")).unwrap_or_default()
    };
    if family_list.trim().is_empty() || !(size >= 0.0) {
        return None;
    }
    let before = &words[..k];
    let bold = before.iter().any(|w| *w == "bold" || *w == "bolder" || w.parse::<u32>().is_ok_and(|n| n >= 600));
    let weight = before.iter().find_map(|w| w.parse::<u16>().ok()).unwrap_or(if bold { 700 } else { 400 });
    let italic = before.iter().any(|w| *w == "italic" || *w == "oblique");
    let mut family = Family::Sans;
    for f in family_list.split(',') {
        let f = f.trim().trim_matches(|c| c == '"' || c == '\'');
        let generic = match f {
            "serif" | "georgia" | "times" | "times new roman" | "cambria" => Some(Family::Serif),
            "monospace" | "courier" | "courier new" | "menlo" | "monaco" | "consolas" => Some(Family::Mono),
            "sans-serif" | "system-ui" | "arial" | "helvetica" | "helvetica neue" | "-apple-system" | "verdana" => Some(Family::Sans),
            _ => None,
        };
        if let Some(g) = generic {
            family = g;
            break;
        }
        if crate::fonts::is_declared(f) {
            family = crate::fonts::face(f, Family::Sans);
            break;
        }
    }
    Some(Font { family, size: size.min(10_000.0), weight, italic })
}

/// The stand-in for text when there's no font system: a mark per character, as the reftest rasteriser draws.
fn approx_mask(text: &str, font: &Font) -> TextMask {
    let host = text_host();
    let width = host.width(text, font).ceil().max(1.0) as u32;
    let size = font.size;
    let height = size.ceil().max(1.0) as u32;
    let mut alpha = vec![0u8; (width * height) as usize];
    let mut pen = 0.0;
    let mut buf = [0u8; 4];
    for c in text.chars() {
        let adv = host.width(c.encode_utf8(&mut buf), font);
        if !c.is_whitespace() {
            let (gx, gw) = (pen + adv * 0.1, adv * 0.8);
            let (gy, gh) = (size * 0.2, size * 0.6);
            for y in gy as u32..((gy + gh) as u32).min(height) {
                for x in gx as u32..((gx + gw) as u32).min(width) {
                    alpha[(y * width + x) as usize] = 255;
                }
            }
        }
        pen += adv;
    }
    TextMask { width, height, alpha, left: 0.0, top: -size * 0.8 }
}

// ------------------------------------------------------------------ paint

#[derive(Debug, Clone, PartialEq)]
pub enum Gradient {
    Linear { p0: (f32, f32), p1: (f32, f32) },
    Radial { c0: (f32, f32), r0: f32, c1: (f32, f32), r1: f32 },
    Conic { c: (f32, f32), angle: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    /// Premultiplied, 0 to 1.
    Solid([f32; 4]),
    Gradient { g: Gradient, stops: Arc<Vec<(f32, [f32; 4])>> },
    Pattern { img: Arc<Image>, repeat_x: bool, repeat_y: bool, m: Matrix },
}

pub fn premul(c: Color) -> [f32; 4] {
    let a = c.a as f32 / 255.0;
    [c.r as f32 / 255.0 * a, c.g as f32 / 255.0 * a, c.b as f32 / 255.0 * a, a]
}

/// A colour the way the canvas spec gives it back: `#rrggbb`, or `rgba(r, g, b, a)` when not opaque.
pub fn serialize_color(c: Color) -> String {
    if c.a == 255 {
        return format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    }
    // The shortest decimal that comes back as the same alpha byte.
    let a = c.a as f32 / 255.0;
    let mut alpha = format!("{a}");
    for d in 1..=3 {
        let s = format!("{:.*}", d, a);
        if (s.parse::<f32>().unwrap_or(0.0) * 255.0).round() as u8 == c.a {
            alpha = s.trim_end_matches('0').trim_end_matches('.').to_string();
            if alpha.is_empty() || alpha == "-" {
                alpha = "0".into();
            }
            break;
        }
    }
    format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, alpha)
}

/// A gradient's colours along its length, 256 steps, premultiplied (as the spec interpolates).
fn ramp(stops: &[(f32, [f32; 4])]) -> Vec<[f32; 4]> {
    (0..256)
        .map(|k| {
            let t = k as f32 / 255.0;
            if stops.is_empty() {
                return [0.0; 4];
            }
            // The last stop at or before t, and the first after.
            let mut before = None;
            for (i, s) in stops.iter().enumerate() {
                if s.0 <= t {
                    before = Some(i);
                }
            }
            let Some(b) = before else { return stops[0].1 };
            let Some(after) = stops.get(b + 1) else { return stops[b].1 };
            let (o0, c0) = stops[b];
            let (o1, c1) = *after;
            if o1 <= o0 {
                return c1;
            }
            let f = (t - o0) / (o1 - o0);
            [c0[0] + (c1[0] - c0[0]) * f, c0[1] + (c1[1] - c0[1]) * f, c0[2] + (c1[2] - c0[2]) * f, c0[3] + (c1[3] - c0[3]) * f]
        })
        .collect()
}

/// A paint ready to sample at device pixels (gradients and patterns through the transform at drawing time).
enum Source<'a> {
    Solid([f32; 4]),
    Gradient { g: &'a Gradient, ramp: Vec<[f32; 4]>, inv: Matrix },
    Pattern { img: &'a Image, rx: bool, ry: bool, inv: Matrix, smooth: bool },
    /// drawImage: the source rectangle, mapped from device pixels.
    Picture { img: &'a Image, inv: Matrix, rect: (f32, f32, f32, f32), smooth: bool },
    Nothing,
}

fn texel(img: &Image, x: i64, y: i64) -> [f32; 4] {
    let k = ((y as usize * img.width as usize) + x as usize) * 4;
    let p = &img.rgba[k..k + 4];
    [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]
}

/// Samples an image at (x, y) in its pixels, inside `bounds` (x0, y0, x1, y1), wrapping if asked.
fn sample(img: &Image, x: f32, y: f32, smooth: bool, bounds: (f32, f32, f32, f32), wrap: (bool, bool)) -> [f32; 4] {
    let (w, h) = (img.width as i64, img.height as i64);
    if w == 0 || h == 0 {
        return [0.0; 4];
    }
    let fix = |v: i64, lo: f32, hi: f32, n: i64, wrap: bool| -> Option<i64> {
        if wrap {
            Some(v.rem_euclid(n))
        } else {
            let (lo, hi) = (lo.floor() as i64, (hi.ceil() as i64 - 1).max(lo.floor() as i64));
            Some(v.clamp(lo.max(0), hi.min(n - 1)))
        }
    };
    if !wrap.0 && (x < bounds.0 || x >= bounds.2) || !wrap.1 && (y < bounds.1 || y >= bounds.3) {
        return [0.0; 4];
    }
    if !smooth {
        let (Some(ix), Some(iy)) = (fix(x.floor() as i64, bounds.0, bounds.2, w, wrap.0), fix(y.floor() as i64, bounds.1, bounds.3, h, wrap.1)) else { return [0.0; 4] };
        return texel(img, ix, iy);
    }
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let mut out = [0.0; 4];
    for (dy, wy) in [(0i64, 1.0 - ty), (1, ty)] {
        for (dx, wx) in [(0i64, 1.0 - tx), (1, tx)] {
            let wgt = wx * wy;
            if wgt == 0.0 {
                continue;
            }
            let (Some(ix), Some(iy)) = (fix(x0 as i64 + dx, bounds.0, bounds.2, w, wrap.0), fix(y0 as i64 + dy, bounds.1, bounds.3, h, wrap.1)) else { continue };
            let t = texel(img, ix, iy);
            for c in 0..4 {
                out[c] += t[c] * wgt;
            }
        }
    }
    out
}

impl Source<'_> {
    fn at(&self, x: f32, y: f32) -> [f32; 4] {
        match self {
            Source::Solid(c) => *c,
            Source::Nothing => [0.0; 4],
            Source::Gradient { g, ramp, inv } => {
                let (ux, uy) = path::apply(inv, (x, y));
                let t = match g {
                    Gradient::Linear { p0, p1 } => {
                        let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
                        ((ux - p0.0) * dx + (uy - p0.1) * dy) / (dx * dx + dy * dy)
                    }
                    Gradient::Conic { c, angle } => {
                        let a = (uy - c.1).atan2(ux - c.0) - angle;
                        (a / std::f32::consts::TAU).rem_euclid(1.0)
                    }
                    Gradient::Radial { c0, r0, c1, r1 } => {
                        let (cdx, cdy, dr) = (c1.0 - c0.0, c1.1 - c0.1, r1 - r0);
                        let (pdx, pdy) = (ux - c0.0, uy - c0.1);
                        let a = cdx * cdx + cdy * cdy - dr * dr;
                        let b = pdx * cdx + pdy * cdy + r0 * dr;
                        let c = pdx * pdx + pdy * pdy - r0 * r0;
                        let ok = |w: f32| r0 + w * dr >= 0.0;
                        if a.abs() < 1e-6 {
                            if b == 0.0 {
                                return [0.0; 4];
                            }
                            let w = c / (2.0 * b);
                            if !ok(w) {
                                return [0.0; 4];
                            }
                            w
                        } else {
                            let disc = b * b - a * c;
                            if disc < 0.0 {
                                return [0.0; 4];
                            }
                            let s = disc.sqrt();
                            let (w1, w2) = ((b + s) / a, (b - s) / a);
                            let (hi, lo) = if w1 > w2 { (w1, w2) } else { (w2, w1) };
                            if ok(hi) {
                                hi
                            } else if ok(lo) {
                                lo
                            } else {
                                return [0.0; 4];
                            }
                        }
                    }
                };
                if !t.is_finite() {
                    return [0.0; 4];
                }
                ramp[(t.clamp(0.0, 1.0) * 255.0).round() as usize]
            }
            Source::Pattern { img, rx, ry, inv, smooth } => {
                let (u, v) = path::apply(inv, (x, y));
                sample(img, u, v, *smooth, (0.0, 0.0, img.width as f32, img.height as f32), (*rx, *ry))
            }
            Source::Picture { img, inv, rect, smooth } => {
                let (u, v) = path::apply(inv, (x, y));
                sample(img, u, v, *smooth, (rect.0, rect.1, rect.0 + rect.2, rect.1 + rect.3), (false, false))
            }
        }
    }
}

// ------------------------------------------------------------- compositing

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    SourceOver,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestinationOver,
    DestinationIn,
    DestinationOut,
    DestinationAtop,
    Lighter,
    Copy,
    Xor,
    Blend(Blend),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

pub fn parse_op(s: &str) -> Option<Op> {
    use Blend::*;
    Some(match s {
        "source-over" => Op::SourceOver,
        "source-in" => Op::SourceIn,
        "source-out" => Op::SourceOut,
        "source-atop" => Op::SourceAtop,
        "destination-over" => Op::DestinationOver,
        "destination-in" => Op::DestinationIn,
        "destination-out" => Op::DestinationOut,
        "destination-atop" => Op::DestinationAtop,
        "lighter" => Op::Lighter,
        "copy" => Op::Copy,
        "xor" => Op::Xor,
        "multiply" => Op::Blend(Multiply),
        "screen" => Op::Blend(Screen),
        "overlay" => Op::Blend(Overlay),
        "darken" => Op::Blend(Darken),
        "lighten" => Op::Blend(Lighten),
        "color-dodge" => Op::Blend(ColorDodge),
        "color-burn" => Op::Blend(ColorBurn),
        "hard-light" => Op::Blend(HardLight),
        "soft-light" => Op::Blend(SoftLight),
        "difference" => Op::Blend(Difference),
        "exclusion" => Op::Blend(Exclusion),
        "hue" => Op::Blend(Hue),
        "saturation" => Op::Blend(Saturation),
        "color" => Op::Blend(Color),
        "luminosity" => Op::Blend(Luminosity),
        _ => return None,
    })
}

impl Op {
    /// Ops that change the canvas outside the shape too (the source is transparent there).
    fn unbounded(self) -> bool {
        matches!(self, Op::SourceIn | Op::SourceOut | Op::DestinationIn | Op::DestinationAtop | Op::Copy)
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let (n, x) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    let mut c = c;
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    if mx <= mn {
        return [0.0; 3];
    }
    [(c[0] - mn) * s / (mx - mn), (c[1] - mn) * s / (mx - mn), (c[2] - mn) * s / (mx - mn)]
}

fn blend(mode: Blend, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    use Blend::*;
    let sep = |f: &dyn Fn(f32, f32) -> f32| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
    let hard = |b: f32, s: f32| if s <= 0.5 { b * 2.0 * s } else { let s2 = 2.0 * s - 1.0; b + s2 - b * s2 };
    match mode {
        Multiply => sep(&|b, s| b * s),
        Screen => sep(&|b, s| b + s - b * s),
        Overlay => sep(&|b, s| hard(s, b)),
        Darken => sep(&|b, s| b.min(s)),
        Lighten => sep(&|b, s| b.max(s)),
        ColorDodge => sep(&|b, s| if b == 0.0 { 0.0 } else if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) }),
        ColorBurn => sep(&|b, s| if b >= 1.0 { 1.0 } else if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / s).min(1.0) }),
        HardLight => sep(&|b, s| hard(b, s)),
        SoftLight => sep(&|b, s| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }),
        Difference => sep(&|b, s| (b - s).abs()),
        Exclusion => sep(&|b, s| b + s - 2.0 * b * s),
        Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        Color => set_lum(cs, lum(cb)),
        Luminosity => set_lum(cb, lum(cs)),
    }
}

/// Source (premultiplied) composited onto destination (premultiplied).
fn composite(op: Op, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let pd = |fa: f32, fb: f32| [s[0] * fa + d[0] * fb, s[1] * fa + d[1] * fb, s[2] * fa + d[2] * fb, sa * fa + da * fb];
    match op {
        Op::SourceOver => pd(1.0, 1.0 - sa),
        Op::SourceIn => pd(da, 0.0),
        Op::SourceOut => pd(1.0 - da, 0.0),
        Op::SourceAtop => pd(da, 1.0 - sa),
        Op::DestinationOver => pd(1.0 - da, 1.0),
        Op::DestinationIn => pd(0.0, sa),
        Op::DestinationOut => pd(0.0, 1.0 - sa),
        Op::DestinationAtop => pd(1.0 - da, sa),
        Op::Xor => pd(1.0 - da, 1.0 - sa),
        Op::Copy => s,
        Op::Lighter => [(s[0] + d[0]).min(1.0), (s[1] + d[1]).min(1.0), (s[2] + d[2]).min(1.0), (sa + da).min(1.0)],
        Op::Blend(mode) => {
            if sa == 0.0 {
                return d;
            }
            let un = |c: &[f32; 4]| if c[3] > 0.0 { [c[0] / c[3], c[1] / c[3], c[2] / c[3]] } else { [0.0; 3] };
            let b = blend(mode, un(&d), un(&s));
            let ra = sa + da - sa * da;
            let mut out = [0.0, 0.0, 0.0, ra];
            for c in 0..3 {
                out[c] = s[c] * (1.0 - da) + d[c] * (1.0 - sa) + sa * da * b[c];
            }
            out
        }
    }
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Baseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

#[derive(Debug, Clone)]
pub struct State {
    pub m: Matrix,
    pub fill: Paint,
    pub stroke: Paint,
    pub line_width: f32,
    pub cap: Cap,
    pub join: Join,
    pub miter: f32,
    pub dash: Vec<f32>,
    pub dash_offset: f32,
    pub alpha: f32,
    pub op: Op,
    pub font: Font,
    pub align: TextAlign,
    pub baseline: Baseline,
    pub rtl: bool,
    pub shadow: [f32; 4],
    pub shadow_blur: f32,
    pub shadow_offset: (f32, f32),
    pub smoothing: bool,
    /// Coverage of the clip region over the whole canvas, if there's a clip.
    pub clip: Option<Arc<Vec<f32>>>,
}

impl Default for State {
    fn default() -> State {
        State {
            m: IDENTITY,
            fill: Paint::Solid([0.0, 0.0, 0.0, 1.0]),
            stroke: Paint::Solid([0.0, 0.0, 0.0, 1.0]),
            line_width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            alpha: 1.0,
            op: Op::SourceOver,
            font: Font { family: Family::Sans, size: 10.0, weight: 400, italic: false },
            align: TextAlign::Start,
            baseline: Baseline::Alphabetic,
            rtl: false,
            shadow: [0.0; 4],
            shadow_blur: 0.0,
            shadow_offset: (0.0, 0.0),
            smoothing: true,
            clip: None,
        }
    }
}

/// A 2D context's canvas, state and current path.
pub struct Context {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA (shared with snapshots until the next change).
    pixels: Arc<Vec<u8>>,
    pub state: State,
    stack: Vec<State>,
    pub path: Path,
}

impl Context {
    pub fn new(width: u32, height: u32) -> Context {
        // Too big to keep: a canvas with no pixels (as browsers do past their limits).
        let (width, height) = if width as u64 * height as u64 > 32 << 20 { (0, 0) } else { (width, height) };
        let n = width as usize * height as usize * 4;
        Context { width, height, pixels: Arc::new(vec![0; n]), state: State::default(), stack: Vec::new(), path: Path::default() }
    }

    /// Back to a blank canvas and the default state (resizing does this).
    pub fn reset(&mut self, width: u32, height: u32) {
        *self = Context::new(width, height);
    }

    pub fn snapshot(&self) -> Canvas {
        Canvas { width: self.width, height: self.height, pixels: self.pixels.clone() }
    }

    pub fn image(&self) -> Image {
        Image { width: self.width, height: self.height, rgba: self.pixels.clone() }
    }

    pub fn save(&mut self) {
        if self.stack.len() < 1024 {
            self.stack.push(self.state.clone());
        }
    }

    pub fn restore(&mut self) {
        if let Some(s) = self.stack.pop() {
            self.state = s;
        }
    }

    pub fn transform(&mut self, n: &Matrix) {
        if n.iter().all(|v| v.is_finite()) {
            self.state.m = path::multiply(&self.state.m, n);
        }
    }

    pub fn set_transform(&mut self, n: &Matrix) {
        if n.iter().all(|v| v.is_finite()) {
            self.state.m = *n;
        }
    }

    // ------------------------------------------------------------ drawing

    fn source<'a>(&self, paint: &'a Paint) -> Source<'a> {
        match paint {
            Paint::Solid(c) => Source::Solid(*c),
            Paint::Gradient { g, stops } => {
                let Some(inv) = path::invert(&self.state.m) else { return Source::Nothing };
                let degenerate = match g {
                    Gradient::Linear { p0, p1 } => p0 == p1,
                    Gradient::Radial { c0, r0, c1, r1 } => c0 == c1 && r0 == r1,
                    Gradient::Conic { .. } => false,
                };
                if degenerate || stops.is_empty() {
                    return Source::Nothing;
                }
                Source::Gradient { g, ramp: ramp(stops), inv }
            }
            Paint::Pattern { img, repeat_x, repeat_y, m } => {
                let Some(inv) = path::invert(&path::multiply(&self.state.m, m)) else { return Source::Nothing };
                Source::Pattern { img, rx: *repeat_x, ry: *repeat_y, inv, smooth: self.state.smoothing }
            }
        }
    }

    /// Paints `src` through `mask` with the current alpha, compositing, clip and shadow.
    fn paint(&mut self, mask: Option<&Mask>, src: &Source) {
        let op = self.state.op;
        if self.shadowed() {
            if let Some(m) = mask {
                self.shadow(m, src);
            }
        }
        self.composite(mask, src, op, 1.0);
    }

    fn composite(&mut self, mask: Option<&Mask>, src: &Source, op: Op, extra: f32) {
        let (w, h) = (self.width as i32, self.height as i32);
        if w == 0 || h == 0 {
            return;
        }
        let (x0, y0, x1, y1) = match (op.unbounded(), mask) {
            (false, Some(m)) => (m.x, m.y, m.x + m.w as i32, m.y + m.h as i32),
            (false, None) => return,
            (true, _) => (0, 0, w, h),
        };
        let alpha = self.state.alpha * extra;
        let clip = self.state.clip.clone();
        let px = Arc::make_mut(&mut self.pixels);
        for y in y0.max(0)..y1.min(h) {
            for x in x0.max(0)..x1.min(w) {
                let k = (y * w + x) as usize;
                let clip_cov = clip.as_ref().map_or(1.0, |c| c[k]);
                if clip_cov <= 0.0 {
                    continue;
                }
                let cov = mask.map_or(0.0, |m| m.at(x, y));
                if cov <= 0.0 && !op.unbounded() {
                    continue;
                }
                let s = if cov > 0.0 { src.at(x as f32 + 0.5, y as f32 + 0.5) } else { [0.0; 4] };
                let f = cov * alpha;
                let s = [s[0] * f, s[1] * f, s[2] * f, s[3] * f];
                let p = &mut px[k * 4..k * 4 + 4];
                let d = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0];
                let mut r = composite(op, s, d);
                if clip_cov < 1.0 {
                    for c in 0..4 {
                        r[c] = d[c] + (r[c] - d[c]) * clip_cov;
                    }
                }
                for c in 0..4 {
                    p[c] = (r[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
    }

    /// The shadow of what `mask` and `src` will draw: their alpha, offset and blurred, in the shadow colour.
    fn shadow(&mut self, mask: &Mask, src: &Source) {
        let blur = self.state.shadow_blur;
        let sigma = blur / 2.0;
        let pad = (sigma * 3.0).ceil() as i32;
        let (ox, oy) = self.state.shadow_offset;
        let (w, h) = (mask.w as i32 + 2 * pad, mask.h as i32 + 2 * pad);
        if w <= 0 || h <= 0 || (w as i64 * h as i64) > 64 << 20 {
            return;
        }
        let mut a = vec![0f32; (w * h) as usize];
        for y in 0..mask.h as i32 {
            for x in 0..mask.w as i32 {
                let cov = mask.cov[(y as usize) * mask.w + x as usize];
                if cov > 0.0 {
                    let s = src.at((mask.x + x) as f32 + 0.5, (mask.y + y) as f32 + 0.5);
                    a[((y + pad) * w + x + pad) as usize] = cov * s[3];
                }
            }
        }
        if sigma > 0.0 {
            box_blur(&mut a, w as usize, h as usize, sigma);
        }
        let (ix, iy) = (ox.round() as i32, oy.round() as i32);
        let shadow = Mask { x: mask.x - pad + ix, y: mask.y - pad + iy, w: w as usize, h: h as usize, cov: a };
        let c = self.state.shadow;
        self.composite(Some(&shadow), &Source::Solid(c), self.state.op, 1.0);
    }

    /// Coverage of polygons for drawing: the canvas, and when there's a shadow, wherever
    /// a shape could be for its shadow to fall on the canvas.
    fn cover(&self, subs: &[Sub], evenodd: bool) -> Option<Mask> {
        let (w, h) = (self.width as i32, self.height as i32);
        if !self.shadowed() {
            return path::fill_in(subs, evenodd, (0, 0, w, h));
        }
        let pad = (self.state.shadow_blur / 2.0 * 3.0).ceil() as i32 + 1;
        let (ix, iy) = (self.state.shadow_offset.0.round() as i32, self.state.shadow_offset.1.round() as i32);
        path::fill_in(subs, evenodd, ((-ix - pad).min(0), (-iy - pad).min(0), (w - ix + pad).max(w), (h - iy + pad).max(h)))
    }

    fn shadowed(&self) -> bool {
        self.state.shadow[3] > 0.0 && (self.state.shadow_blur > 0.0 || self.state.shadow_offset != (0.0, 0.0))
    }

    pub fn fill_path(&mut self, p: &Path, evenodd: bool) {
        let Some(mask) = self.cover(&p.polys(), evenodd) else {
            if self.state.op.unbounded() {
                self.composite(None, &Source::Nothing, self.state.op, 1.0);
            }
            return;
        };
        let paint = self.state.fill.clone();
        let src = self.source(&paint);
        self.paint(Some(&mask), &src);
    }

    /// The outline of a stroke of `p` with the current style, in device pixels.
    pub fn stroke_outline(&self, p: &Path) -> Vec<Sub> {
        let m = self.state.m;
        let Some(inv) = path::invert(&m) else { return Vec::new() };
        let user = p.mapped(&inv);
        let st = StrokeStyle { width: self.state.line_width, cap: self.state.cap, join: self.state.join, miter: self.state.miter, dash: &self.state.dash, dash_offset: self.state.dash_offset };
        let polys = path::stroke(&user, &st, path::max_scale(&m));
        polys.into_iter().map(|s| Sub { pts: s.pts.iter().map(|q| path::apply(&m, *q)).collect(), closed: true }).collect()
    }

    pub fn stroke_path(&mut self, p: &Path) {
        let outline = self.stroke_outline(p);
        let Some(mask) = self.cover(&outline, false) else { return };
        let paint = self.state.stroke.clone();
        let src = self.source(&paint);
        self.paint(Some(&mask), &src);
    }

    pub fn clip_path(&mut self, p: &Path, evenodd: bool) {
        let n = self.width as usize * self.height as usize;
        let mut cov = vec![0f32; n];
        if let Some(mask) = path::fill(&p.polys(), evenodd, self.width, self.height) {
            for y in 0..mask.h {
                for x in 0..mask.w {
                    let k = (mask.y as usize + y) * self.width as usize + mask.x as usize + x;
                    cov[k] = mask.cov[y * mask.w + x];
                }
            }
        }
        if let Some(old) = &self.state.clip {
            for (c, o) in cov.iter_mut().zip(old.iter()) {
                *c *= o;
            }
        }
        self.state.clip = Some(Arc::new(cov));
    }

    fn rect_path(&self, x: f32, y: f32, w: f32, h: f32) -> Path {
        let mut p = Path::default();
        let m = self.state.m;
        p.move_to(&m, x, y);
        p.line_to(&m, x + w, y);
        p.line_to(&m, x + w, y + h);
        p.line_to(&m, x, y + h);
        p.close();
        p
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) || w == 0.0 || h == 0.0 {
            return;
        }
        let p = self.rect_path(x, y, w, h);
        self.fill_path(&p, false);
    }

    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) {
            return;
        }
        if w == 0.0 && h == 0.0 {
            return;
        }
        let mut p = Path::default();
        let m = self.state.m;
        if w == 0.0 || h == 0.0 {
            // A line, not a box.
            p.move_to(&m, x, y);
            p.line_to(&m, x + w, y + h);
        } else {
            p = self.rect_path(x, y, w, h);
        }
        self.stroke_path(&p);
    }

    pub fn clear_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) {
            return;
        }
        let p = self.rect_path(x, y, w, h);
        let Some(mask) = path::fill(&p.polys(), false, self.width, self.height) else { return };
        let alpha = self.state.alpha;
        self.state.alpha = 1.0;
        // Transparent black over the rectangle only (inside the clip).
        self.composite(Some(&mask), &Source::Solid([0.0, 0.0, 0.0, 1.0]), Op::DestinationOut, 1.0);
        self.state.alpha = alpha;
    }

    /// `drawImage(img, sx, sy, sw, sh, dx, dy, dw, dh)`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(&mut self, img: &Image, sx: f32, sy: f32, sw: f32, sh: f32, dx: f32, dy: f32, dw: f32, dh: f32) {
        if ![sx, sy, sw, sh, dx, dy, dw, dh].iter().all(|v| v.is_finite()) || sw == 0.0 || sh == 0.0 || dw == 0.0 || dh == 0.0 {
            return;
        }
        // Negative sizes flip the rectangle round.
        let (sx, sw) = if sw < 0.0 { (sx + sw, -sw) } else { (sx, sw) };
        let (sy, sh) = if sh < 0.0 { (sy + sh, -sh) } else { (sy, sh) };
        let (dx, dw) = if dw < 0.0 { (dx + dw, -dw) } else { (dx, dw) };
        let (dy, dh) = if dh < 0.0 { (dy + dh, -dh) } else { (dy, dh) };
        // The part of the source inside the picture, and the destination shrunk to match.
        let (cx0, cy0) = (sx.max(0.0), sy.max(0.0));
        let (cx1, cy1) = ((sx + sw).min(img.width as f32), (sy + sh).min(img.height as f32));
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }
        let (kx, ky) = (dw / sw, dh / sh);
        let (dx, dy, dw, dh) = (dx + (cx0 - sx) * kx, dy + (cy0 - sy) * ky, (cx1 - cx0) * kx, (cy1 - cy0) * ky);
        let (sx, sy, sw, sh) = (cx0, cy0, cx1 - cx0, cy1 - cy0);
        let p = self.rect_path(dx, dy, dw, dh);
        let Some(mask) = self.cover(&p.polys(), false) else { return };
        // Device pixels to the image's: back through the transform, then the rectangles.
        let Some(inv) = path::invert(&self.state.m) else { return };
        let to_img: Matrix = [sw / dw, 0.0, 0.0, sh / dh, sx - dx * sw / dw, sy - dy * sh / dh];
        let inv = path::multiply(&to_img, &inv);
        let src = Source::Picture { img, inv, rect: (sx, sy, sw, sh), smooth: self.state.smoothing };
        self.paint(Some(&mask), &src);
    }

    /// The text's width and where it starts, for `fillText` and `measureText`.
    pub fn measure(&self, text: &str) -> f32 {
        text_metrics(text, &self.state.font).0
    }

    /// `fillText` / `strokeText`.
    pub fn text(&mut self, text: &str, x: f32, y: f32, max_width: Option<f32>, stroke: bool) {
        if !x.is_finite() || !y.is_finite() || max_width.is_some_and(|w| !(w > 0.0)) {
            return;
        }
        // Whitespace in canvas text is all spaces.
        let text: String = text.chars().map(|c| if matches!(c, '\t' | '\n' | '\x0c' | '\r') { ' ' } else { c }).collect();
        let m = self.state.m;
        let scale = path::max_scale(&m);
        let host = text_host();
        let mut font = self.state.font;
        let (width, asc, desc) = text_metrics(&text, &font);
        let squeeze = match max_width {
            Some(mw) if width > mw && width > 0.0 => mw / width,
            _ => 1.0,
        };
        let shift = match self.state.align {
            TextAlign::Left => 0.0,
            TextAlign::Start if !self.state.rtl => 0.0,
            TextAlign::End if self.state.rtl => 0.0,
            TextAlign::Center => -width * squeeze / 2.0,
            _ => -width * squeeze,
        };
        let base = match self.state.baseline {
            Baseline::Top => asc,
            Baseline::Hanging => asc * 0.8,
            Baseline::Middle => (asc - desc) / 2.0,
            Baseline::Alphabetic => 0.0,
            Baseline::Ideographic | Baseline::Bottom => -desc,
        };
        // A font Canard has the file for: its outlines, filled (or stroked) like any path.
        if let Some(face) = crate::fonts::file_for(&font).filter(|f| f.has_outlines()) {
            let (ox, oy) = (x + shift, y + base);
            let p = glyph_path(&face, &text, font.size, |(u, v)| path::apply(&m, (ox + u * squeeze, oy - v)));
            if stroke {
                self.stroke_path(&p);
            } else {
                self.fill_path(&p, false);
            }
            return;
        }
        // Set at the size it will be on the canvas, so it's sharp.
        font.size *= scale;
        let mask = host.mask(&text, &font).unwrap_or_else(|| approx_mask(&text, &font));
        if mask.width == 0 || mask.height == 0 {
            return;
        }
        // Mask pixels to user space: (x + shift + (left + i) / scale * squeeze, y + base + (top + j) / scale).
        let to_user: Matrix = [squeeze / scale, 0.0, 0.0, 1.0 / scale, x + shift + mask.left * squeeze / scale, y + base + mask.top / scale];
        let to_dev = path::multiply(&m, &to_user);
        let Some(inv) = path::invert(&to_dev) else { return };
        let corners = [(0.0, 0.0), (mask.width as f32, 0.0), (mask.width as f32, mask.height as f32), (0.0, mask.height as f32)];
        let quad: Vec<(f32, f32)> = corners.iter().map(|c| path::apply(&to_dev, *c)).collect();
        let Some(area) = self.cover(&[Sub { pts: quad, closed: true }], false) else { return };
        let alpha = Image { width: mask.width, height: mask.height, rgba: Arc::new(mask.alpha.iter().flat_map(|a| [*a, *a, *a, *a]).collect()) };
        let mut cov = area;
        for y in 0..cov.h {
            for x in 0..cov.w {
                let (u, v) = path::apply(&inv, ((cov.x + x as i32) as f32 + 0.5, (cov.y + y as i32) as f32 + 0.5));
                let a = sample(&alpha, u, v, true, (0.0, 0.0, mask.width as f32, mask.height as f32), (false, false))[3];
                cov.cov[y * cov.w + x] *= a;
            }
        }
        let paint = if stroke { self.state.stroke.clone() } else { self.state.fill.clone() };
        let src = self.source(&paint);
        self.paint(Some(&cov), &src);
    }

    // ------------------------------------------------------------- pixels

    /// `getImageData`: straight (not premultiplied) RGBA for a rectangle; outside the canvas is transparent.
    pub fn get_image_data(&self, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; w as usize * h as usize * 4];
        for row in 0..h as i32 {
            let sy = y + row;
            if sy < 0 || sy >= self.height as i32 {
                continue;
            }
            for col in 0..w as i32 {
                let sx = x + col;
                if sx < 0 || sx >= self.width as i32 {
                    continue;
                }
                let k = (sy as usize * self.width as usize + sx as usize) * 4;
                let o = (row as usize * w as usize + col as usize) * 4;
                let p = &self.pixels[k..k + 4];
                let a = p[3] as u32;
                if a == 0 {
                    continue;
                }
                for c in 0..3 {
                    out[o + c] = ((p[c] as u32 * 255 + a / 2) / a).min(255) as u8;
                }
                out[o + 3] = p[3];
            }
        }
        out
    }

    /// `putImageData`: copies straight RGBA in, as is (no compositing, transform or clip).
    #[allow(clippy::too_many_arguments)]
    pub fn put_image_data(&mut self, data: &[u8], dw: u32, dh: u32, x: i32, y: i32, dirty: (i32, i32, i32, i32)) {
        let (mut dx0, mut dy0, mut dx1, mut dy1) = (dirty.0, dirty.1, dirty.0 + dirty.2, dirty.1 + dirty.3);
        if dx0 > dx1 {
            std::mem::swap(&mut dx0, &mut dx1);
        }
        if dy0 > dy1 {
            std::mem::swap(&mut dy0, &mut dy1);
        }
        let (dx0, dy0, dx1, dy1) = (dx0.max(0), dy0.max(0), dx1.min(dw as i32), dy1.min(dh as i32));
        let (w, h) = (self.width as i32, self.height as i32);
        let px = Arc::make_mut(&mut self.pixels);
        for sy in dy0..dy1 {
            let ty = y + sy;
            if ty < 0 || ty >= h {
                continue;
            }
            for sx in dx0..dx1 {
                let tx = x + sx;
                if tx < 0 || tx >= w {
                    continue;
                }
                let s = ((sy * dw as i32 + sx) * 4) as usize;
                let Some(p) = data.get(s..s + 4) else { continue };
                let a = p[3] as u32;
                let k = ((ty * w + tx) * 4) as usize;
                for c in 0..3 {
                    px[k + c] = ((p[c] as u32 * a + 127) / 255) as u8;
                }
                px[k + 3] = p[3];
            }
        }
    }

    /// The canvas as a PNG.
    pub fn png(&self) -> Vec<u8> {
        let straight = self.get_image_data(0, 0, self.width, self.height);
        volant_core::png::encode(self.width, self.height, &straight)
    }
}

/// Three box blurs, which together are close to a Gaussian of `sigma`.
fn box_blur(a: &mut [f32], w: usize, h: usize, sigma: f32) {
    // Three passes of width 2r + 1 have variance 3((2r + 1)^2 - 1) / 12.
    let r = ((((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round() as usize).max(1);
    let mut tmp = vec![0f32; a.len()];
    for _ in 0..3 {
        for y in 0..h {
            let row = &a[y * w..(y + 1) * w];
            let mut sum: f32 = row[..r.min(w)].iter().sum();
            for x in 0..w {
                if x + r < w {
                    sum += row[x + r];
                }
                if x > r {
                    sum -= row[x - r - 1];
                }
                tmp[y * w + x] = sum / (2 * r + 1) as f32;
            }
        }
        for x in 0..w {
            let mut sum = 0.0;
            for y in 0..r.min(h) {
                sum += tmp[y * w + x];
            }
            for y in 0..h {
                if y + r < h {
                    sum += tmp[(y + r) * w + x];
                }
                if y > r {
                    sum -= tmp[(y - r - 1) * w + x];
                }
                a[y * w + x] = sum / (2 * r + 1) as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests;
