//! Picks a sharp icon for a site: the home page's favourites are drawn at
//! 44 points (88 pixels on a Retina screen), and a 16-pixel `favicon.ico` is
//! a blur at that size. Sites usually offer bigger ones (`apple-touch-icon`,
//! `<link rel=icon sizes=192x192>`, SVG); this finds them, best first.

use crate::html;

/// Icons at least this many pixels across look sharp on the home page.
pub const SHARP: u32 = 96;

/// Resolves `href` against the page it was found on.
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with("data:") || href.starts_with("javascript:") {
        return None;
    }
    if href.starts_with("https://") || href.starts_with("http://") {
        return Some(href.to_string());
    }
    let scheme_end = base.find("://")? + 3;
    let scheme = &base[..scheme_end - 3];
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    let path_start = base[scheme_end..].find(['/', '?', '#']).map(|i| scheme_end + i).unwrap_or(base.len());
    let origin = &base[..path_start];
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    // Relative to the page's directory.
    let path = base[path_start..].split(['?', '#']).next().unwrap_or("");
    let dir = path.rfind('/').map(|i| &path[..=i]).unwrap_or("/");
    Some(format!("{origin}{dir}{}", href.trim_start_matches("./")))
}

/// The biggest size a `sizes` attribute declares (`any` counts as huge).
fn declared(sizes: &str) -> Option<u32> {
    sizes
        .split_ascii_whitespace()
        .filter_map(|s| if s.eq_ignore_ascii_case("any") { Some(1024) } else { s.split(['x', 'X']).next()?.parse().ok() })
        .max()
}

/// Every icon the page at `url` links to, plus the usual fallbacks, best first.
pub fn candidates(page: &str, url: &str) -> Vec<String> {
    let dom = html::parse(page);
    let base = (0..dom.nodes.len() as u32)
        .find(|&id| dom.tag(id) == Some("base"))
        .and_then(|id| dom.attr(id, "href"))
        .and_then(|h| resolve(url, h))
        .unwrap_or_else(|| url.to_string());
    let mut found: Vec<(u32, usize, String)> = Vec::new();
    for id in 0..dom.nodes.len() as u32 {
        if dom.tag(id) != Some("link") {
            continue;
        }
        let rel = dom.attr(id, "rel").unwrap_or("").to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
        let touch = rels.iter().any(|r| r.starts_with("apple-touch-icon"));
        if !touch && !rels.contains(&"icon") {
            continue; // includes mask-icon, which is a one-colour stencil
        }
        let Some(href) = dom.attr(id, "href").and_then(|h| resolve(&base, h)) else { continue };
        let svg = dom.attr(id, "type").map(|t| t.contains("svg")).unwrap_or(false) || href.split(['?', '#']).next().unwrap_or("").ends_with(".svg");
        let size = dom.attr(id, "sizes").and_then(declared).unwrap_or(if svg { 512 } else if touch { 180 } else { 32 });
        found.push((size.min(1024), found.len(), href));
    }
    // Biggest first; between equals, the page's own order.
    found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<String> = Vec::new();
    for (_, _, u) in found {
        if !out.contains(&u) {
            out.push(u);
        }
    }
    if let Some(origin) = resolve(url, "/") {
        let origin = origin.trim_end_matches('/');
        for f in ["/apple-touch-icon.png", "/favicon.ico"] {
            let u = format!("{origin}{f}");
            if !out.contains(&u) {
                out.push(u);
            }
        }
    }
    out
}

/// What kind of image these bytes are, as a MIME type.
pub fn mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF8") {
        Some("image/gif")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(&[0, 0, 1, 0]) {
        Some("image/x-icon")
    } else {
        let head = String::from_utf8_lossy(&b[..b.len().min(512)]).to_ascii_lowercase();
        (head.contains("<svg") && !head.contains("<html")).then_some("image/svg+xml")
    }
}

/// How many pixels across the image is (SVG scales to anything).
pub fn pixels(b: &[u8]) -> Option<u32> {
    let be16 = |i: usize| b.get(i..i + 2).map(|s| u16::from_be_bytes([s[0], s[1]]) as u32);
    let be32 = |i: usize| b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]));
    match mime(b)? {
        "image/png" => be32(16),
        "image/gif" => b.get(6..8).map(|s| u16::from_le_bytes([s[0], s[1]]) as u32),
        "image/x-icon" => {
            let n = b.get(4..6).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)?;
            (0..n.min(64)).filter_map(|i| b.get(6 + i * 16)).map(|&w| if w == 0 { 256 } else { w as u32 }).max()
        }
        "image/jpeg" => {
            // Walk the segments to the frame header.
            let mut i = 2;
            while i + 9 < b.len() {
                if b[i] != 0xff {
                    return None;
                }
                let marker = b[i + 1];
                if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                    return be16(i + 7);
                }
                i += 2 + be16(i + 2)? as usize;
            }
            None
        }
        "image/svg+xml" => Some(1024),
        _ => None, // WebP: fine, but we can't tell the size cheaply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_links() {
        assert_eq!(resolve("https://a.example/x/page.html?q", "icon.png").unwrap(), "https://a.example/x/icon.png");
        assert_eq!(resolve("https://a.example/x/", "/i.png").unwrap(), "https://a.example/i.png");
        assert_eq!(resolve("https://a.example", "//cdn.example/i.png").unwrap(), "https://cdn.example/i.png");
        assert_eq!(resolve("https://a.example", "./i.png").unwrap(), "https://a.example/i.png");
        assert_eq!(resolve("https://a.example/", "https://b.example/i.png").unwrap(), "https://b.example/i.png");
        assert!(resolve("https://a.example/", "data:image/png;base64,xx").is_none());
    }

    #[test]
    fn biggest_first() {
        let page = r#"<html><head>
            <link rel="icon" href="/favicon-32.png" sizes="32x32">
            <link rel="shortcut icon" href="/favicon.ico">
            <link rel="mask-icon" href="/mask.svg">
            <link rel="icon" href="https://cdn.example/s/favicon_144x144.png" sizes="144x144">
            <link rel="apple-touch-icon" href="touch.png">
            <link rel="icon" type="image/svg+xml" href="/logo.svg">
        </head><body></body></html>"#;
        let c = candidates(page, "https://www.example.com/");
        assert_eq!(
            c,
            [
                "https://www.example.com/logo.svg",
                "https://www.example.com/touch.png",
                "https://cdn.example/s/favicon_144x144.png",
                "https://www.example.com/favicon-32.png",
                "https://www.example.com/favicon.ico",
                "https://www.example.com/apple-touch-icon.png",
            ]
        );
        assert_eq!(candidates("", "https://e.example/a/b"), ["https://e.example/apple-touch-icon.png", "https://e.example/favicon.ico"]);
    }

    #[test]
    fn sizes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(180u32.to_be_bytes());
        png.extend(180u32.to_be_bytes());
        assert_eq!((mime(&png), pixels(&png)), (Some("image/png"), Some(180)));
        let mut ico = vec![0, 0, 1, 0, 2, 0];
        ico.extend([16u8; 16]);
        ico.extend([0u8; 16]);
        assert_eq!(pixels(&ico), Some(256));
        let jpg = [0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 11, 8, 0, 120, 0, 130, 3, 0, 0, 0];
        assert_eq!(pixels(&jpg), Some(130));
        assert_eq!(mime(b"<?xml version='1.0'?><svg xmlns='http://www.w3.org/2000/svg'/>"), Some("image/svg+xml"));
        assert_eq!(mime(b"<!doctype html><html><body>404</body></html>"), None);
    }
}
