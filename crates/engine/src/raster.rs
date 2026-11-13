//! Draws a laid-out page into pixels with no system libraries, so a page
//! can be rendered and compared anywhere (the reftest runner does this on
//! Linux CI). It's deterministic, not pretty: edges get exact area coverage,
//! rounded corners are supersampled, and text is drawn as one mark per
//! character, filling the glyph's advance with a pattern that depends on the
//! character. Different text draws differently and the same text draws the
//! same, which is all a comparison needs. The Ahem test font, whose glyphs
//! are boxes, is drawn exactly.

use crate::fonts::{Font, Fonts};
use crate::layout::{Item, Page};
use crate::style::{Color, Family};
use std::collections::HashMap;

/// A decoded picture: `width` x `height` RGBA pixels, row by row.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An RGBA canvas.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy)]
struct Shape {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
}

impl Shape {
    /// How much of the pixel at (px, py) the shape covers, 0 to 1.
    fn coverage(&self, px: u32, py: u32) -> f32 {
        let (x0, y0) = (px as f32, py as f32);
        let ox = (self.x + self.w).min(x0 + 1.0) - self.x.max(x0);
        let oy = (self.y + self.h).min(y0 + 1.0) - self.y.max(y0);
        if ox <= 0.0 || oy <= 0.0 {
            return 0.0;
        }
        let r = self.r.min(self.w / 2.0).min(self.h / 2.0);
        // Near a rounded corner: sample a 4 x 4 grid.
        if r > 0.0 {
            let near = |v: f32, lo: f32, hi: f32| v < lo + r || v + 1.0 > hi - r;
            if near(x0, self.x, self.x + self.w) && near(y0, self.y, self.y + self.h) {
                let mut n = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        if self.contains(x0 + (sx as f32 + 0.5) / 4.0, y0 + (sy as f32 + 0.5) / 4.0, r) {
                            n += 1;
                        }
                    }
                }
                return n as f32 / 16.0;
            }
        }
        ox * oy
    }

    fn contains(&self, x: f32, y: f32, r: f32) -> bool {
        if x < self.x || y < self.y || x > self.x + self.w || y > self.y + self.h {
            return false;
        }
        let cx = x.clamp(self.x + r, self.x + self.w - r);
        let cy = y.clamp(self.y + r, self.y + self.h - r);
        (x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r
    }
}

impl Canvas {
    pub fn new(width: u32, height: u32, bg: Color) -> Canvas {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            rgba.extend_from_slice(&[bg.r, bg.g, bg.b, 255]);
        }
        Canvas { width, height, rgba }
    }

    /// Blends `color` over the pixel with this much coverage.
    fn blend(&mut self, px: u32, py: u32, color: [u8; 4], cover: f32) {
        let a = color[3] as f32 / 255.0 * cover;
        if a <= 0.0 {
            return;
        }
        let k = ((py * self.width + px) * 4) as usize;
        for c in 0..3 {
            let d = self.rgba[k + c] as f32;
            self.rgba[k + c] = (d + (color[c] as f32 - d) * a).round() as u8;
        }
    }

    /// Fills a shape, clipped by `clips`.
    fn fill(&mut self, s: Shape, color: [u8; 4], clips: &[Shape]) {
        let Some((x0, y0, x1, y1)) = self.span(s, clips) else { return };
        for py in y0..y1 {
            for px in x0..x1 {
                let mut c = s.coverage(px, py);
                for k in clips {
                    if c <= 0.0 {
                        break;
                    }
                    c *= k.coverage(px, py);
                }
                if c > 0.0 {
                    self.blend(px, py, color, c);
                }
            }
        }
    }

    /// The pixels a shape can touch, inside the canvas and the clips.
    fn span(&self, s: Shape, clips: &[Shape]) -> Option<(u32, u32, u32, u32)> {
        let (mut l, mut t, mut r, mut b) = (s.x, s.y, s.x + s.w, s.y + s.h);
        for k in clips {
            l = l.max(k.x);
            t = t.max(k.y);
            r = r.min(k.x + k.w);
            b = b.min(k.y + k.h);
        }
        let x0 = l.floor().max(0.0) as u32;
        let y0 = t.floor().max(0.0) as u32;
        let x1 = (r.ceil().max(0.0) as u32).min(self.width);
        let y1 = (b.ceil().max(0.0) as u32).min(self.height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// Draws a picture scaled to fill the box (nearest pixel), clipped.
    fn image(&mut self, s: Shape, bmp: &Bitmap, clips: &[Shape]) {
        if bmp.width == 0 || bmp.height == 0 || s.w <= 0.0 || s.h <= 0.0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.span(s, clips) else { return };
        for py in y0..y1 {
            for px in x0..x1 {
                let mut c = s.coverage(px, py);
                for k in clips {
                    c *= k.coverage(px, py);
                }
                if c <= 0.0 {
                    continue;
                }
                let u = (((px as f32 + 0.5 - s.x) / s.w * bmp.width as f32) as u32).min(bmp.width - 1);
                let v = (((py as f32 + 0.5 - s.y) / s.h * bmp.height as f32) as u32).min(bmp.height - 1);
                let k = ((v * bmp.width + u) * 4) as usize;
                let p = [bmp.rgba[k], bmp.rgba[k + 1], bmp.rgba[k + 2], bmp.rgba[k + 3]];
                self.blend(px, py, p, c);
            }
        }
    }
}

/// Whether text in this font is set in Ahem (the box font CSS tests use).
pub fn is_ahem(font: &Font) -> bool {
    matches!(font.family, Family::Face(i) if crate::fonts::face_info(i).is_some_and(|(n, _)| n == "ahem"))
}

fn rgba(c: Color) -> [u8; 4] {
    [c.r, c.g, c.b, c.a]
}

/// A small, stable hash of a character, for its mark's pattern.
fn mark_bits(c: char) -> u32 {
    let mut h = (c as u32).wrapping_mul(0x9E37_79B1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA77);
    h ^= h >> 13;
    // Never blank: a printing character always leaves a mark.
    h | 1
}

fn text(cv: &mut Canvas, x: f32, base: f32, s: &str, font: &Font, color: Color, fonts: &dyn Fonts, clips: &[Shape]) {
    let col = rgba(color);
    let size = font.size;
    let ahem = is_ahem(font);
    let mut pen = x;
    let mut buf = [0u8; 4];
    for c in s.chars() {
        let adv = fonts.width(c.encode_utf8(&mut buf), font);
        if !c.is_whitespace() && !matches!(c, '\u{200B}' | '\u{FEFF}' | '\u{AD}') {
            if ahem {
                // Ahem: 'p' is the descent's box, 'É' the ascent's, everything else the whole em.
                let (top, h) = match c {
                    'p' => (base, size * 0.2),
                    'É' => (base - size * 0.8, size * 0.8),
                    _ => (base - size * 0.8, size),
                };
                cv.fill(Shape { x: pen, y: top, w: adv, h, r: 0.0 }, col, clips);
            } else {
                // A 3 x 4 grid of cells over the glyph's advance and x-height-ish band.
                let bits = mark_bits(c);
                let (gw, gh) = (adv * 0.8, size * 0.6);
                let (gx, gy) = (pen + adv * 0.1, base - gh);
                for cell in 0..12 {
                    if bits >> cell & 1 == 1 {
                        let (cx, cy) = ((cell % 3) as f32, (cell / 3) as f32);
                        cv.fill(Shape { x: gx + cx * gw / 3.0, y: gy + cy * gh / 4.0, w: gw / 3.0, h: gh / 4.0, r: 0.0 }, col, clips);
                    }
                }
            }
        }
        pen += adv;
    }
}

/// Draws `page` scrolled to the top into a `width` x `height` canvas
/// (fixed and sticky boxes last, as they're on top).
pub fn draw(page: &Page, width: u32, height: u32, fonts: &dyn Fonts, images: &HashMap<String, Bitmap>) -> Canvas {
    let bg = if page.background.a == 0 { Color { r: 255, g: 255, b: 255, a: 255 } } else { page.background };
    let mut cv = Canvas::new(width, height, bg);
    let view_h = height as f32;
    let mut order: Vec<(usize, f32)> = (0..page.items.len()).filter(|&i| !page.in_layer(i)).map(|i| (i, 0.0)).collect();
    for l in &page.layers {
        let dy = page.layer_offset(l, 0.0, view_h);
        order.extend((l.items.0..l.items.1).map(|i| (i, dy)));
    }
    let mut clips: Vec<Shape> = Vec::new();
    for (i, dy) in order {
        match &page.items[i] {
            Item::Rect { x, y, w, h, color, radius } => cv.fill(Shape { x: *x, y: y + dy, w: *w, h: *h, r: *radius }, rgba(*color), &clips),
            Item::Image { x, y, w, h, src, radius } => {
                if let Some(b) = images.get(src) {
                    cv.image(Shape { x: *x, y: y + dy, w: *w, h: *h, r: *radius }, b, &clips);
                }
            }
            Item::Text { x, y, text: t, font, color, underline, strike, .. } => {
                text(&mut cv, *x, y + dy, t, font, *color, fonts, &clips);
                let w = fonts.width(t, font);
                let th = (font.size / 14.0).max(1.0);
                if *underline {
                    cv.fill(Shape { x: *x, y: y + dy + font.size * 0.1, w, h: th, r: 0.0 }, rgba(*color), &clips);
                }
                if *strike {
                    cv.fill(Shape { x: *x, y: y + dy - font.size * 0.3, w, h: th, r: 0.0 }, rgba(*color), &clips);
                }
            }
            Item::Clip { x, y, w, h, radius } => clips.push(Shape { x: *x, y: y + dy, w: *w, h: *h, r: *radius }),
            Item::Unclip => {
                clips.pop();
            }
        }
    }
    cv
}

/// How two canvases differ: (pixels that differ, the largest channel difference).
pub fn diff(a: &Canvas, b: &Canvas) -> (u64, u8) {
    let (mut n, mut most) = (0u64, 0u8);
    for (p, q) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        let d = (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0);
        if d > 0 {
            n += 1;
            most = most.max(d);
        }
    }
    (n, most)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApproxFonts, NoImages};

    fn at(cv: &Canvas, x: u32, y: u32) -> [u8; 3] {
        let k = ((y * cv.width + x) * 4) as usize;
        [cv.rgba[k], cv.rgba[k + 1], cv.rgba[k + 2]]
    }

    #[test]
    fn draws_boxes_clips_and_text() {
        let html = "<body style='margin:0'><div style='width:50px;height:20px;background:#00ff00'></div>\
                    <div style='width:30px;height:30px;overflow:hidden'><div style='width:60px;height:60px;background:#0000ff'></div></div>\
                    <p style='margin:0;font:20px Ahem;color:#000'>X</p>";
        let page = crate::render(html, "", 100.0, &ApproxFonts, &NoImages);
        let cv = draw(&page, 100, 100, &ApproxFonts, &HashMap::new());
        assert_eq!(at(&cv, 10, 10), [0, 255, 0]);
        assert_eq!(at(&cv, 60, 10), [255, 255, 255]);
        assert_eq!(at(&cv, 20, 40), [0, 0, 255]);
        assert_eq!(at(&cv, 40, 40), [255, 255, 255], "the overflow clips");
        // Ahem's X is a solid 20px box.
        assert_eq!(at(&cv, 5, 55), [0, 0, 0]);
        assert_eq!(at(&cv, 25, 55), [255, 255, 255]);
        let same = draw(&page, 100, 100, &ApproxFonts, &HashMap::new());
        assert_eq!(diff(&cv, &same), (0, 0));
        let other = crate::render(&html.replace(">X<", ">Y<").replace("Ahem", "serif"), "", 100.0, &ApproxFonts, &NoImages);
        let ab = crate::render(&html.replace("Ahem", "serif"), "", 100.0, &ApproxFonts, &NoImages);
        assert!(diff(&draw(&other, 100, 100, &ApproxFonts, &HashMap::new()), &draw(&ab, 100, 100, &ApproxFonts, &HashMap::new())).0 > 0, "different text draws differently");
    }
}
