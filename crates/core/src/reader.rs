//! Reader mode: finds the article in a page and rewrites it as clean, safe HTML.
//!
//! The approach is the one Readability popularised: score every paragraph by
//! how much prose it holds, credit that score to its containers, pick the
//! container with the most prose and the fewest links, then pull in sibling
//! blocks that look like part of the same story. The output keeps only a
//! small allow-list of tags and attributes, so nothing from the page can run.

use crate::html::{self, Data, Dom, NodeId, ROOT};

pub struct Article {
    pub title: String,
    pub byline: String,
    pub site: String,
    /// Sanitised article HTML (allow-listed tags, absolute URLs, no scripts or handlers).
    pub html: String,
    pub words: usize,
}

impl Article {
    pub fn minutes(&self) -> usize {
        (self.words / 230).max(1)
    }
}

/// Never part of an article.
const DROP: &[&str] = &[
    "script", "style", "noscript", "nav", "footer", "aside", "form", "button", "input", "select", "textarea", "iframe", "svg", "math", "object",
    "embed", "canvas", "template", "dialog", "menu", "link", "meta", "head", "label", "audio", "video",
];
const UNLIKELY: &[&str] = &[
    "banner", "breadcrumb", "combx", "comment", "community", "cover-wrap", "disqus", "extra", "foot", "header", "legends", "menu", "related",
    "remark", "replies", "rss", "shoutbox", "sidebar", "skyscraper", "social", "sponsor", "supplemental", "ad-break", "agegate", "pagination",
    "pager", "popup", "yom-remote", "share", "newsletter", "subscribe", "promo", "cookie", "consent", "modal", "outbrain", "taboola", "signup",
    "toolbar", "navbox", "infobox-", "mw-editsection", "hatnote", "noprint", "reflist", "refbegin", "mw-references", "references", "catlinks",
    "printfooter", "jump-to", "sistersitebox", "authority-control", "mw-jump",
];
/// Junk even inside content-looking containers.
const ALWAYS_JUNK: &[&str] = &["article-footer", "comments", "comment-list", "related-posts", "newsletter", "share-buttons", "social-share"];
const LIKELY: &[&str] = &["and", "article", "body", "column", "content", "main", "shadow", "story", "entry", "post", "text", "prose", "blog"];
const POSITIVE: &[&str] = &["article", "body", "content", "entry", "hentry", "h-entry", "main", "page", "pagination", "post", "text", "blog", "story", "prose"];
const NEGATIVE: &[&str] = &[
    "-ad-", "hidden", "banner", "combx", "comment", "com-", "contact", "foot", "footer", "footnote", "gdpr", "masthead", "media", "meta", "outbrain",
    "promo", "related", "scroll", "share", "shoutbox", "sidebar", "skyscraper", "sponsor", "shopping", "tags", "tool", "widget", "byline", "author",
];
const BLOCKS: &[&str] = &[
    "address", "article", "aside", "blockquote", "details", "div", "dl", "fieldset", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6",
    "header", "hr", "li", "main", "nav", "ol", "p", "pre", "section", "table", "ul",
];
/// Tags kept in the output, as themselves.
const KEEP: &[&str] = &[
    "p", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "code", "em", "strong", "b", "i", "u", "s", "sub", "sup", "a", "img",
    "figure", "figcaption", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "br", "hr", "dl", "dt", "dd", "mark", "small", "abbr",
    "q", "cite", "kbd", "var", "samp", "del", "ins", "time",
];

fn class_id(d: &Dom, id: NodeId) -> String {
    let mut s = String::new();
    if let Some(c) = d.attr(id, "class") {
        s.push_str(c);
    }
    s.push(' ');
    if let Some(i) = d.attr(id, "id") {
        s.push_str(i);
    }
    s.make_ascii_lowercase();
    s
}

fn has_any(s: &str, words: &[&str]) -> bool {
    words.iter().any(|w| s.contains(w))
}

fn class_weight(d: &Dom, id: NodeId) -> f32 {
    let ci = class_id(d, id);
    let mut w = 0.0;
    if has_any(&ci, NEGATIVE) {
        w -= 25.0;
    }
    if has_any(&ci, POSITIVE) {
        w += 25.0;
    }
    w
}

struct Info {
    /// Visible characters inside, after collapsing whitespace.
    text: Vec<u32>,
    /// Of those, characters inside links.
    link: Vec<u32>,
    dropped: Vec<bool>,
}

fn is_hidden(d: &Dom, id: NodeId) -> bool {
    if d.attr(id, "hidden").is_some() || d.attr(id, "aria-hidden") == Some("true") {
        return true;
    }
    d.attr(id, "style").map(|s| {
        let s = s.to_ascii_lowercase().replace(' ', "");
        s.contains("display:none") || s.contains("visibility:hidden")
    }) == Some(true)
}

fn analyse(d: &Dom, strip_unlikely: bool) -> Info {
    let n = d.nodes.len();
    // Containers of the page's main content are never junk, whatever their class says.
    let mut holds_main = vec![false; n];
    for id in (1..n).rev() {
        let main = matches!(d.tag(id as NodeId), Some("main" | "article")) || d.attr(id as NodeId, "role") == Some("main");
        if main || holds_main[id] {
            holds_main[d.nodes[id].parent as usize] = true;
        }
    }
    let mut dropped = vec![false; n];
    // Top-down: a node is dropped if it or any ancestor is junk.
    for id in 1..n as NodeId {
        let p = d.parent(id) as usize;
        if dropped[p] {
            dropped[id as usize] = true;
            continue;
        }
        if let Some(tag) = d.tag(id) {
            if DROP.contains(&tag) || is_hidden(d, id) {
                dropped[id as usize] = true;
                continue;
            }
            if !strip_unlikely || holds_main[id as usize] || matches!(tag, "body" | "html" | "article" | "main" | "a") {
                continue;
            }
            let ci = class_id(d, id);
            if ci.len() > 1 && (has_any(&ci, ALWAYS_JUNK) || has_any(&ci, UNLIKELY) && !has_any(&ci, LIKELY)) {
                dropped[id as usize] = true;
            } else if d.attr(id, "role").map(|r| matches!(r, "navigation" | "complementary" | "banner" | "dialog" | "alert" | "menu")).unwrap_or(false) {
                dropped[id as usize] = true;
            }
        }
    }
    // Bottom-up: text and link lengths (children always come after parents).
    let mut text = vec![0u32; n];
    let mut link = vec![0u32; n];
    for id in (1..n).rev() {
        if dropped[id] {
            continue;
        }
        if let Data::Text(t) = &d.nodes[id].data {
            text[id] = visible_len(t);
        }
        if d.tag(id as NodeId) == Some("a") {
            link[id] = text[id];
        }
        let p = d.nodes[id].parent as usize;
        text[p] += text[id];
        link[p] += link[id];
    }
    Info { text, link, dropped }
}

fn visible_len(t: &str) -> u32 {
    let mut n = 0u32;
    let mut space = true;
    for c in t.chars() {
        if c.is_whitespace() {
            if !space {
                n += 1;
                space = true;
            }
        } else {
            n += 1;
            space = false;
        }
    }
    n
}

fn link_density(info: &Info, id: NodeId) -> f32 {
    let t = info.text[id as usize];
    if t == 0 {
        0.0
    } else {
        info.link[id as usize] as f32 / t as f32
    }
}

/// A div holding only inline content reads as a paragraph.
fn is_paragraph(d: &Dom, info: &Info, id: NodeId) -> bool {
    match d.tag(id) {
        Some("p" | "pre" | "td" | "blockquote") => true,
        Some("div" | "section") => !d.children(id).iter().any(|&c| !info.dropped[c as usize] && d.tag(c).map(|t| BLOCKS.contains(&t)).unwrap_or(false)),
        _ => false,
    }
}

fn base_score(d: &Dom, id: NodeId) -> f32 {
    let tag_score = match d.tag(id) {
        Some("div" | "article" | "main") => 5.0,
        Some("pre" | "td" | "blockquote" | "section") => 3.0,
        Some("address" | "ol" | "ul" | "dl" | "dd" | "dt" | "li" | "form") => -3.0,
        Some("h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "th") => -5.0,
        _ => 0.0,
    };
    tag_score + class_weight(d, id)
}

/// Extracts the article from a page. Returns `None` for pages that aren't articles.
pub fn extract(page: &str, url: &str) -> Option<Article> {
    let d = html::parse(page);
    // First with junk filtering by class name; if that leaves too little, the
    // filter probably hit the content itself (it happens), so try again without it.
    extract_from(&d, url, true).or_else(|| extract_from(&d, url, false))
}

fn extract_from(d: &Dom, url: &str, strip_unlikely: bool) -> Option<Article> {
    let d = d;
    let info = analyse(d, strip_unlikely);
    let n = d.nodes.len();

    // Score paragraphs and credit their ancestors.
    let mut score: Vec<f32> = vec![0.0; n];
    let mut is_candidate = vec![false; n];
    for id in 1..n as NodeId {
        if info.dropped[id as usize] || !is_paragraph(d, &info, id) {
            continue;
        }
        let len = info.text[id as usize];
        if len < 25 {
            continue;
        }
        let commas = d.text(id).matches([',', '，', '、']).count() as f32;
        let s = 1.0 + commas + (len as f32 / 100.0).min(3.0);
        let mut anc = d.parent(id);
        for level in 0..3 {
            if anc == ROOT {
                break;
            }
            if !is_candidate[anc as usize] {
                is_candidate[anc as usize] = true;
                score[anc as usize] = base_score(d, anc);
            }
            let div = match level {
                0 => 1.0,
                1 => 2.0,
                _ => level as f32 * 3.0,
            };
            score[anc as usize] += s / div;
            anc = d.parent(anc);
        }
    }
    let mut best: Option<(f32, NodeId)> = None;
    for id in 1..n {
        if is_candidate[id] {
            let s = score[id] * (1.0 - link_density(&info, id as NodeId));
            score[id] = s;
            if best.map(|(b, _)| s > b).unwrap_or(true) {
                best = Some((s, id as NodeId));
            }
        }
    }
    let (top_score, mut top) = best?;
    // If the winner is one of several similar siblings (a split article), climb to their parent.
    let parent = d.parent(top);
    if parent != ROOT && d.tag(parent) != Some("body") {
        let close = d.children(parent).iter().filter(|&&c| is_candidate[c as usize] && score[c as usize] >= top_score * 0.75).count();
        if close >= 3 {
            top = parent;
        }
    }

    // Gather the winner and siblings that look like part of the same story.
    let threshold = (top_score * 0.2).max(10.0);
    let parent = d.parent(top);
    let mut parts: Vec<NodeId> = Vec::new();
    let top_class = d.attr(top, "class").unwrap_or("");
    for &sib in d.children(parent) {
        if info.dropped[sib as usize] || d.tag(sib).is_none() {
            continue;
        }
        let mut take = sib == top;
        if !take {
            let bonus = if !top_class.is_empty() && d.attr(sib, "class") == Some(top_class) { top_score * 0.2 } else { 0.0 };
            if is_candidate[sib as usize] && score[sib as usize] + bonus >= threshold {
                take = true;
            } else if d.tag(sib) == Some("p") {
                let len = info.text[sib as usize];
                let ld = link_density(&info, sib);
                let text = d.text(sib);
                take = (len > 80 && ld < 0.25) || (len > 0 && ld == 0.0 && text.contains(". "));
            }
        }
        if take {
            parts.push(sib);
        }
    }

    let parts_text = parts.clone();
    let _ = &parts_text;
    let mut out = String::with_capacity(info.text[top as usize] as usize * 2);
    let mut words = 0usize;
    let title = title_of(d);
    for p in parts {
        serialize(d, &info, p, url, &title, &mut out, &mut words);
    }
    if words < 80 {
        return None;
    }
    trim_back_matter(&mut out);
    // Mostly links (a front page or an index): not an article.
    let (text, links) = parts_text.iter().fold((0u32, 0u32), |(t, l), &p| (t + info.text[p as usize], l + info.link[p as usize]));
    if std::env::var_os("VOLANT_READER_DEBUG").is_some() {
        eprintln!("link density {:.2} (strip_unlikely {strip_unlikely})", links as f32 / text.max(1) as f32);
    }
    if text == 0 || links as f32 / text as f32 > 0.6 {
        return None;
    }
    Some(Article { title, byline: byline_of(d, &info), site: meta(d, "og:site_name").unwrap_or_default(), html: out, words })
}

fn meta(d: &Dom, key: &str) -> Option<String> {
    for id in 0..d.nodes.len() as NodeId {
        if d.tag(id) == Some("meta") && (d.attr(id, "property") == Some(key) || d.attr(id, "name") == Some(key)) {
            if let Some(c) = d.attr(id, "content").map(str::trim).filter(|c| !c.is_empty()) {
                return Some(c.to_string());
            }
        }
    }
    None
}

fn title_of(d: &Dom) -> String {
    let og = meta(d, "og:title").unwrap_or_default();
    let doc = d.find("title").map(|t| d.text(t)).unwrap_or_default();
    let doc = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    // The page's own heading, when the titles are that heading plus the site name.
    if let Some(h1) = d.find("h1").map(|h| d.text(h).split_whitespace().collect::<Vec<_>>().join(" ")) {
        if !h1.is_empty() && h1.len() < 300 && (og.contains(&h1) || doc.contains(&h1)) {
            return h1;
        }
    }
    let doc = if og.is_empty() { doc } else { og };
    // "Article name | Site" → "Article name"
    for sep in [" | ", " – ", " — ", " - ", " :: "] {
        if let Some((a, _)) = doc.rsplit_once(sep) {
            if a.split_whitespace().count() >= 3 {
                return a.to_string();
            }
        }
    }
    if doc.is_empty() {
        d.find("h1").map(|h| d.text(h).trim().to_string()).unwrap_or_default()
    } else {
        doc
    }
}

fn byline_of(d: &Dom, info: &Info) -> String {
    if let Some(a) = meta(d, "author").filter(|a| !a.starts_with("http")) {
        return a;
    }
    for id in 1..d.nodes.len() as NodeId {
        if info.dropped[id as usize] {
            continue;
        }
        let rel_author = d.attr(id, "rel") == Some("author") || d.attr(id, "itemprop").map(|p| p.contains("author")).unwrap_or(false);
        if rel_author || class_id(d, id).contains("byline") {
            let t = d.text(id).split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() && t.len() < 100 {
                return t;
            }
        }
    }
    String::new()
}

/// Resolves `href` against the page address.
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("vbscript:") || lower.starts_with("data:text") {
        return None;
    }
    if let Some(colon) = href.find(':') {
        let scheme = &href[..colon];
        if !scheme.is_empty() && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) {
            return match scheme.to_ascii_lowercase().as_str() {
                "http" | "https" | "mailto" | "data" => Some(href.to_string()),
                _ => None,
            };
        }
    }
    let (scheme, rest) = base.split_once("://")?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..host_end]);
    if let Some(r) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{r}"));
    }
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    let path_full = &rest[host_end..];
    let path = path_full.split(['?', '#']).next().unwrap_or("");
    if href.starts_with('#') {
        return Some(format!("{origin}{}{href}", path_full.split('#').next().unwrap_or("")));
    }
    if href.starts_with('?') {
        return Some(format!("{origin}{path}{href}"));
    }
    let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
    let mut segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let (rel, tail) = match href.find(['?', '#']) {
        Some(i) => (&href[..i], &href[i..]),
        None => (href, ""),
    };
    let rel_segs: Vec<&str> = rel.split('/').collect();
    for (i, s) in rel_segs.iter().enumerate() {
        match *s {
            "." => {}
            ".." => {
                segs.pop();
            }
            s if s.is_empty() && i + 1 < rel_segs.len() => {}
            s => segs.push(s),
        }
    }
    let mut out = origin;
    for s in &segs {
        out.push('/');
        out.push_str(s);
    }
    if rel.ends_with('/') || segs.is_empty() {
        out.push('/');
    }
    out.push_str(tail);
    Some(out)
}

fn image_src(d: &Dom, id: NodeId) -> Option<&str> {
    // Lazy-loading sites keep the real address in a data attribute.
    for k in ["data-src", "data-lazy-src", "data-original", "data-url", "src"] {
        if let Some(v) = d.attr(id, k).filter(|v| !v.trim().is_empty() && !v.starts_with("data:image/gif")) {
            return Some(v);
        }
    }
    d.attr(id, "srcset").and_then(|s| s.split(',').next_back()).and_then(|c| c.split_whitespace().next())
}

fn serialize(d: &Dom, info: &Info, id: NodeId, url: &str, title: &str, out: &mut String, words: &mut usize) {
    if info.dropped[id as usize] {
        return;
    }
    let node = &d.nodes[id as usize];
    match &node.data {
        Data::Text(t) => {
            *words += t.split_whitespace().count();
            html::escape_into(t, out, false);
        }
        Data::Document => {}
        Data::Element { name, .. } => {
            let name: &str = name;
            if name == "img" {
                if let Some(src) = image_src(d, id).and_then(|s| resolve(url, s)) {
                    out.push_str("<img src=\"");
                    html::escape_into(&src, out, true);
                    out.push_str("\" alt=\"");
                    html::escape_into(d.attr(id, "alt").unwrap_or(""), out, true);
                    out.push_str("\" loading=\"lazy\">");
                }
                return;
            }
            // Nothing left to read inside (and no pictures): skip it.
            if info.text[id as usize] == 0 && !matches!(name, "br" | "hr") && !has_image(d, info, id) {
                return;
            }
            // Link-heavy lists and tables inside an article are usually navigation.
            if matches!(name, "ul" | "ol" | "table") && link_density(info, id) > 0.6 && info.text[id as usize] < 600 {
                return;
            }
            // A heading that repeats the title.
            if matches!(name, "h1" | "h2") && d.text(id).trim() == title.trim() {
                return;
            }
            let out_name = match name {
                "h1" => Some("h2"),
                n if KEEP.contains(&n) => Some(n),
                _ => None,
            };
            if let Some(tag) = out_name {
                out.push('<');
                out.push_str(tag);
                if tag == "a" {
                    if let Some(href) = d.attr(id, "href").and_then(|h| resolve(url, h)) {
                        out.push_str(" href=\"");
                        html::escape_into(&href, out, true);
                        out.push('"');
                    }
                }
                if matches!(tag, "td" | "th") {
                    for k in ["colspan", "rowspan"] {
                        if let Some(v) = d.attr(id, k).filter(|v| v.bytes().all(|b| b.is_ascii_digit())) {
                            out.push_str(&format!(" {k}=\"{v}\""));
                        }
                    }
                }
                out.push('>');
                if matches!(tag, "br" | "hr") {
                    return;
                }
            } else if matches!(name, "div" | "section" | "article" | "main" | "header") && is_paragraph(d, info, id) && info.text[id as usize] > 0 {
                // An inline-only div becomes a paragraph so the text keeps its spacing.
                out.push_str("<p>");
                for &c in &node.children {
                    serialize(d, info, c, url, title, out, words);
                }
                out.push_str("</p>");
                return;
            }
            for &c in &node.children {
                serialize(d, info, c, url, title, out, words);
            }
            if let Some(tag) = out_name {
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
}

/// Cuts reference lists, link lists and "see also" sections off the end.
fn trim_back_matter(out: &mut String) {
    const BACK: &[&str] = &["references", "external links", "further reading", "see also", "notes", "bibliography", "citations", "sources", "footnotes", "related articles", "read more"];
    let min = out.len() * 3 / 5;
    let mut cut: Option<usize> = None;
    for tag in ["<h2>", "<h3>"] {
        let mut from = min;
        while let Some(p) = out[from..].find(tag).map(|p| p + from) {
            let end = out[p..].find("</h2>").or_else(|| out[p..].find("</h3>")).map(|e| e + p).unwrap_or(out.len());
            // Heading text without tags (headings often wrap a self-link).
            let mut heading = String::new();
            let mut in_tag = false;
            for c in out[p + 4..end].chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    c if !in_tag => heading.push(c.to_ascii_lowercase()),
                    _ => {}
                }
            }
            let heading = heading.trim().trim_end_matches(':');
            if BACK.iter().any(|b| heading == *b || heading.starts_with(&format!("{b} "))) {
                cut = Some(cut.map_or(p, |c: usize| c.min(p)));
                break;
            }
            from = p + 4;
        }
    }
    if let Some(c) = cut {
        out.truncate(c);
    }
}

fn has_image(d: &Dom, info: &Info, id: NodeId) -> bool {
    d.children(id).iter().any(|&c| !info.dropped[c as usize] && (d.tag(c) == Some("img") || has_image(d, info, c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<!doctype html><html><head><title>How jets fly | Sky Weekly</title>
      <meta name="author" content="Ada Wright"><meta property="og:site_name" content="Sky Weekly"></head>
      <body><nav><a href="/">Home</a><a href="/news">News</a></nav>
      <div class="sidebar"><p>Subscribe to our newsletter, it is great, really great, trust us, sign up today.</p></div>
      <article class="post-body">
        <h1>How jets fly</h1>
        <p>A jet engine pulls in air, squeezes it, mixes it with fuel, and burns it. The hot exhaust rushes out of the back, and the engine is pushed forward.</p>
        <p>Wings do the rest. Air moving over the curved top of a wing, and the angle the wing meets it at, pushes the aircraft up. Pilots trim, climb, and turn by changing that angle.</p>
        <img data-src="/img/jet.jpg" src="data:image/gif;base64,R0l" alt="A jet">
        <p>At cruising height the air is thin, so engines burn less fuel, and the aircraft can travel fast, far, and smoothly above most of the weather below it.</p>
        <p onclick="evil()">Click <a href="javascript:alert(1)">here</a> or <a href="../more.html">read more</a>, about turbofans, ramjets, and scramjets, which push the idea further and faster than ever.</p>
        <script>alert(1)</script>
      </article>
      <div class="comments"><p>First! This is a comment, with commas, lots, of, them, that should never show up in reader mode at all.</p></div>
      <footer>© Sky Weekly</footer></body></html>"#;

    #[test]
    fn finds_the_article() {
        let a = extract(PAGE, "https://sky.example/2026/jets/how.html").expect("article");
        assert_eq!(a.title, "How jets fly");
        assert_eq!(a.byline, "Ada Wright");
        assert_eq!(a.site, "Sky Weekly");
        assert!(a.html.contains("A jet engine pulls in air"));
        assert!(a.html.contains("<img src=\"https://sky.example/img/jet.jpg\""), "{}", a.html);
        assert!(a.html.contains("href=\"https://sky.example/2026/more.html\""), "{}", a.html);
        for bad in ["newsletter", "First!", "Home", "onclick", "javascript", "<script", "alert", "©"] {
            assert!(!a.html.contains(bad), "{bad} leaked into {}", a.html);
        }
        assert!(!a.html.contains("<h2>How jets fly"), "title repeated");
    }

    #[test]
    fn non_articles_are_rejected() {
        assert!(extract("<html><body><nav><a href=/a>A</a></nav><p>Hi.</p></body></html>", "https://x.com/").is_none());
    }

    #[test]
    fn resolves_urls() {
        let b = "https://a.com/x/y/page.html?q=1#f";
        assert_eq!(resolve(b, "z.png").unwrap(), "https://a.com/x/y/z.png");
        assert_eq!(resolve(b, "../z.png").unwrap(), "https://a.com/x/z.png");
        assert_eq!(resolve(b, "/z").unwrap(), "https://a.com/z");
        assert_eq!(resolve(b, "//cdn.com/z").unwrap(), "https://cdn.com/z");
        assert_eq!(resolve(b, "#top").unwrap(), "https://a.com/x/y/page.html?q=1#top");
        assert_eq!(resolve(b, "?p=2").unwrap(), "https://a.com/x/y/page.html?p=2");
        assert_eq!(resolve(b, "https://b.com").unwrap(), "https://b.com");
        assert!(resolve(b, "javascript:alert(1)").is_none());
        assert!(resolve(b, " JavaScript:alert(1)").is_none());
        assert!(resolve(b, "file:///etc/passwd").is_none());
    }
}
