//! Reader view as a document for the engine: the article volant-core's
//! `reader` extracted, set with Volant's Reader styles (the same measure,
//! sizes and colours as the WebKit Reader page), in light or dark.

use volant_core::reader::Article;

pub struct Options {
    pub dark: bool,
    /// Body text size in px (14–30).
    pub size: f32,
    pub serif: bool,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// (html, css) for the engine.
pub fn document(a: &Article, host: &str, o: &Options) -> (String, String) {
    let mut meta = Vec::new();
    for part in [a.site.as_str(), host, a.byline.as_str()] {
        if !part.is_empty() && !meta.iter().any(|m: &String| m.eq_ignore_ascii_case(part)) {
            meta.push(esc(part));
        }
    }
    meta.push(format!("{} min read", a.minutes()));
    let html = format!(
        "<!doctype html><html><body><article><p class=\"meta\">{}</p><h1 class=\"title\">{}</h1><div class=\"content\">{}</div></article>\
         <p class=\"end\">Drawn by Volant's own engine</p></body></html>",
        meta.join(" · "),
        esc(&a.title),
        a.html
    );
    let (bg, text, text2, text3, fill, accent) =
        if o.dark { ("#1c1c1e", "#f5f5f7", "#a1a1a6", "#6e6e73", "#262628", "#1f95ff") } else { ("#ffffff", "#1d1d1f", "#6e6e73", "#a1a1a6", "#f5f5f7", "#0078f0") };
    let face = if o.serif { "ui-serif, Georgia, serif" } else { "-apple-system, system-ui, sans-serif" };
    let size = o.size.clamp(14.0, 30.0);
    let css = format!(
        "html, body {{ background: {bg}; color: {text} }}
body {{ margin: 0; padding: 0 24px 96px; font-family: {face} }}
article, .end {{ max-width: 680px; margin: 28px auto 0 }}
.meta {{ color: {text2}; font: 13px/1.4 -apple-system, sans-serif; margin: 0 0 14px }}
h1.title {{ font: 700 {title}px/1.15 {face}; margin: 0 0 28px; color: {text} }}
.content {{ font: 400 {size}px/1.65 {face}; color: {text} }}
.content p {{ margin: 0 0 1.1em }}
.content h2, .content h3, .content h4, .content h5, .content h6 {{ font-family: -apple-system, sans-serif; line-height: 1.25; margin: 1.8em 0 .6em }}
.content h2 {{ font-size: 1.3em }}
.content h3 {{ font-size: 1.12em }}
.content h4, .content h5, .content h6 {{ font-size: 1em }}
.content a {{ color: {accent}; text-decoration: none }}
.content figure {{ margin: 1.6em 0 }}
.content figcaption {{ font: 13px/1.45 -apple-system, sans-serif; color: {text2}; margin-top: 8px; text-align: center }}
.content blockquote {{ margin: 1.4em 0; padding: 16px 20px; background: {fill} }}
.content blockquote p:last-child {{ margin-bottom: 0 }}
.content pre {{ font: 13.5px/1.55 ui-monospace, Menlo, monospace; background: {fill}; padding: 14px 16px; white-space: pre-wrap }}
.content code {{ font-family: ui-monospace, Menlo, monospace; font-size: .85em; background: {fill} }}
.content pre code {{ background: transparent; font-size: 13.5px }}
.content ul, .content ol {{ padding-left: 1.4em; margin: 0 0 1.1em }}
.content li {{ margin: .3em 0 }}
.content hr {{ border-top: 1px solid {fill}; margin: 2em 0 }}
.content table {{ font: 14px/1.45 -apple-system, sans-serif; margin: 1.4em 0 }}
.content th {{ background: {fill}; font-weight: 600 }}
.content th, .content td {{ padding: 8px 10px }}
.content sup {{ font-size: .65em }}
.end {{ margin-top: 48px; color: {text3}; font-size: 12px }}
",
        title = (size * 1.8).round(),
    );
    (html, css)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_document_renders() {
        let a = Article { title: "How jets fly".into(), byline: "Ada Wright".into(), site: "Sky Weekly".into(), html: "<p>A jet engine pulls in air.</p><h2>Wings</h2><ul><li>Turbofans</li></ul><pre><code>thrust = m * v</code></pre>".into(), words: 600 };
        let (html, css) = document(&a, "sky.example", &Options { dark: true, size: 20.0, serif: true });
        let page = crate::render(&html, &css, 900.0, &crate::ApproxFonts);
        assert_eq!(page.background, crate::Color::rgb(0x1c, 0x1c, 0x1e));
        let svg = crate::svg::to_svg(&page);
        for want in ["How jets fly", "Sky Weekly · sky.example · Ada Wright · 2 min read", "Wings", "Turbofans", "thrust = m * v", "•"] {
            assert!(svg.contains(want), "{want}");
        }
        // Centred to the 680 px measure: text starts at (900 - 680) / 2.
        let title_x = page.items.iter().find_map(|i| match i {
            crate::Item::Text { x, text, .. } if text.starts_with("How") => Some(*x),
            _ => None,
        });
        assert_eq!(title_x, Some(110.0));
    }
}
