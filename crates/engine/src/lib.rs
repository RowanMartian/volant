//! volant-engine: Volant's own web engine, written from scratch in Rust and
//! built up chunk by chunk.
//!
//! The pipeline is the classic one:
//!
//! 1. **Parse** HTML into a DOM (volant-core's `html`) and CSS into rules ([`css`]).
//! 2. **Style**: match selectors, run the cascade, inherit ([`style`]).
//! 3. **Layout**: blocks and lines ([`layout`]), measuring text through [`fonts::Fonts`].
//! 4. **Paint**: a display list of rectangles and text that the app draws
//!    natively (or [`svg`] draws anywhere).
//!
//! What it does today is listed in each module: CSS, the cascade, blocks,
//! lines, inline boxes, images, floats and tables. No JavaScript, flexbox or
//! grid yet: those are later chunks. Volant uses it where that's enough (an
//! opt-in Reader), while web pages stay on WebKit.

pub mod css;
pub mod fonts;
pub mod layout;
pub mod reader;
pub mod style;
pub mod svg;

pub use fonts::{ApproxFonts, Font, Fonts};
pub use layout::{Images, Item, Link, NoImages, Page};
pub use style::{Color, Family};

/// The `<style>` sheets in a document, in order.
pub fn style_sheets(dom: &volant_core::html::Dom) -> Vec<String> {
    (0..dom.nodes.len() as u32).filter(|&id| dom.tag(id) == Some("style")).map(|id| dom.text(id)).collect()
}

/// Parses `html`, styles it with its own `<style>` sheets plus `extra_css`
/// (applied last, like a user stylesheet), and lays it out `width` px wide,
/// sizing images from `images` where they're known.
pub fn render(html: &str, extra_css: &str, width: f32, fonts: &dyn Fonts, images: &dyn Images) -> Page {
    let dom = volant_core::html::parse(html);
    let mut sheets = style_sheets(&dom);
    if !extra_css.is_empty() {
        sheets.push(extra_css.to_string());
    }
    let styles = style::compute(&dom, &sheets);
    layout::layout(&dom, &styles, width, fonts, images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_document() {
        let html = "<!doctype html><html><head><title>t</title><style>h1 { color: #c00 }</style></head>\
                    <body><h1>Hello</h1><p>From <a href='https://volant.app/'>Volant</a>'s engine.</p></body></html>";
        let page = render(html, "body { font-family: Georgia, serif }", 600.0, &ApproxFonts, &NoImages);
        let svg = svg::to_svg(&page);
        assert!(svg.contains(r##"fill="#cc0000" style="white-space:pre">Hello</text>"##), "{svg}");
        assert!(svg.contains("Liberation Serif"));
        assert!(!svg.contains(">t</text>"), "the title isn't drawn");
        assert_eq!(page.links[0].href, "https://volant.app/");
        assert!(page.height > 60.0);
    }
}
