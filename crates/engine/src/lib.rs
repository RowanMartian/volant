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
//! 5. **Script** ([`script`]): the page's JavaScript, run by volant-js against
//!    a live DOM, with events, forms and the web platform's APIs.
//!
//! What it does today is listed in each module. Volant uses it for an opt-in
//! Reader and for Simple Page, which loads, runs and draws pages with nothing
//! but Volant; ordinary browsing stays on WebKit for now.

pub mod canvas;
pub mod css;
pub mod fonts;
pub mod forms;
pub mod layout;
mod punct;
pub mod raster;
pub mod reader;
pub mod script;
pub mod select;
pub mod style;
pub mod svg;

pub use fonts::{ApproxFonts, Font, Fonts};
pub use layout::{After, Hit, Images, Item, Link, NoImages, Page};
pub use css::Env;
pub use style::{Color, Family};

/// The `<style>` sheets in a document, in order.
pub fn style_sheets(dom: &volant_core::html::Dom) -> Vec<String> {
    // (A sheet a script turned off, `style.disabled = true`, doesn't apply.)
    (0..dom.nodes.len() as u32).filter(|&id| dom.tag(id) == Some("style") && dom.attr(id, "disabled").is_none()).map(|id| dom.text(id)).collect()
}

/// Parses `html`, styles it with its own `<style>` sheets plus `extra_css`
/// (applied last, like a user stylesheet), and lays it out `width` px wide,
/// sizing images from `images` where they're known.
pub fn render(html: &str, extra_css: &str, width: f32, fonts: &dyn Fonts, images: &dyn Images) -> Page {
    render_in(html, &[extra_css.to_string()], css::Env { width, height: 768.0, dark: false }, fonts, images)
}

/// Like [`render`], with any number of extra style sheets (a page's linked
/// sheets, in order) and the viewport's `@media` facts (width, dark mode).
pub fn render_in(html: &str, sheets: &[String], env: css::Env, fonts: &dyn Fonts, images: &dyn Images) -> Page {
    let mut dom = volant_core::html::parse(html);
    forms::prepare(&mut dom);
    let mut all = style_sheets(&dom);
    all.extend(sheets.iter().filter(|s| !s.is_empty()).cloned());
    let styled = style::compute(&dom, &all, &env);
    layout::layout(&dom, &styled, env.width, env.height, fonts, images)
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
