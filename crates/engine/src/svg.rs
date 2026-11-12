//! Draws a laid-out page as SVG: a way to look at the engine's output (and
//! test it) anywhere, without a Mac.

use crate::layout::{Item, Page};
use crate::style::{Color, Family};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn css(c: Color) -> String {
    if c.a == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        format!("rgba({},{},{},{:.3})", c.r, c.g, c.b, c.a as f32 / 255.0)
    }
}

pub fn to_svg(page: &Page) -> String {
    let h = page.height.ceil().max(1.0);
    let mut out = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><rect width="{w}" height="{h}" fill="{bg}"/>"#,
        w = page.width,
        bg = css(page.background)
    );
    for item in &page.items {
        match item {
            Item::Rect { x, y, w, h, color } => out.push_str(&format!(r#"<rect x="{x:.2}" y="{y:.2}" width="{w:.2}" height="{h:.2}" fill="{}"/>"#, css(*color))),
            Item::Text { x, y, text, font, color, underline, strike } => {
                let family = match font.family {
                    Family::Sans => "Liberation Sans, Arial, Helvetica, sans-serif",
                    Family::Serif => "Liberation Serif, Times New Roman, Times, serif",
                    Family::Mono => "Liberation Mono, Menlo, monospace",
                };
                let mut deco = Vec::new();
                if *underline {
                    deco.push("underline");
                }
                if *strike {
                    deco.push("line-through");
                }
                out.push_str(&format!(
                    r#"<text x="{x:.2}" y="{y:.2}" font-family="{family}" font-size="{:.2}" font-weight="{}"{}{} fill="{}" style="white-space:pre">{}</text>"#,
                    font.size,
                    font.weight,
                    if font.italic { r#" font-style="italic""# } else { "" },
                    if deco.is_empty() { String::new() } else { format!(r#" text-decoration="{}""#, deco.join(" ")) },
                    css(*color),
                    esc(text)
                ));
            }
        }
    }
    out.push_str("</svg>\n");
    out
}
