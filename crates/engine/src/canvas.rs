//! `<canvas>` 2D drawing: what a page drew, as a display list in the
//! canvas's own pixels. Pages draw through a JavaScript context (in
//! `prelude.js`) that records shapes already transformed, with curves made
//! into short lines. Layout gives the canvas a box like an image (its `src` is
//! `canvas:N`), and the app draws the list into it, scaled to fit.

use crate::style::Color;

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Filled and/or stroked subpaths: each a list of points, and whether it's closed.
    Path { subpaths: Vec<(bool, Vec<(f32, f32)>)>, fill: Option<Color>, stroke: Option<Color>, line_width: f32 },
    /// `clearRect` over part of the canvas: paints the page background there.
    Clear { points: Vec<(f32, f32)> },
    /// `y` is where the text's baseline goes.
    Text { x: f32, y: f32, text: String, size: f32, bold: bool, serif: bool, mono: bool, color: Color, align: Align },
    Image { src: String, x: f32, y: f32, w: f32, h: f32, alpha: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A canvas's drawing: its size in canvas pixels and what's on it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Canvas {
    pub width: f32,
    pub height: f32,
    pub ops: Vec<Op>,
}

/// The `src` layout gives a canvas's box.
pub fn src_for(node: u32) -> String {
    format!("canvas:{node}")
}

/// A colour with the context's `globalAlpha` applied.
pub fn color(css: &str, alpha: f32) -> Option<Color> {
    let mut c = crate::style::parse_color(css)?;
    c.a = (c.a as f32 * alpha.clamp(0.0, 1.0)).round() as u8;
    (c.a > 0).then_some(c)
}

/// Reads a CSS font shorthand the way canvases use it (`bold 14px Georgia, serif`):
/// (size in px, bold, serif, monospace).
pub fn font(spec: &str) -> (f32, bool, bool, bool) {
    let lower = spec.to_ascii_lowercase();
    let size = lower
        .split_whitespace()
        .find_map(|w| w.split('/').next().and_then(|w| w.strip_suffix("px")).and_then(|n| n.parse::<f32>().ok()))
        .or_else(|| lower.split_whitespace().find_map(|w| w.strip_suffix("pt").and_then(|n| n.parse::<f32>().ok()).map(|p| p * 4.0 / 3.0)))
        .or_else(|| lower.split_whitespace().find_map(|w| w.strip_suffix("em").and_then(|n| n.parse::<f32>().ok()).map(|e| e * 16.0)))
        .unwrap_or(10.0);
    let bold = lower.split_whitespace().any(|w| w == "bold" || w == "bolder" || w.parse::<u32>().map(|n| n >= 600).unwrap_or(false));
    let mono = lower.contains("mono") || lower.contains("courier");
    let serif = !mono && (lower.contains("serif") && !lower.contains("sans-serif") || lower.contains("georgia") || lower.contains("times"));
    (size.clamp(1.0, 500.0), bold, serif, mono)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_and_colors() {
        assert_eq!(font("bold 14px Georgia, serif"), (14.0, true, true, false));
        assert_eq!(font("12pt monospace"), (16.0, false, false, true));
        assert_eq!(font("italic 600 20px/1.2 sans-serif"), (20.0, true, false, false));
        assert_eq!(color("#ff0000", 0.5).map(|c| c.a), Some(128));
        assert_eq!(color("transparent", 1.0), None);
    }
}
